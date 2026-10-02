//! DIRI PATCH (sprite animation): frame-swapped SVG marks that animate
//! without rendering, laying out or painting anything.
//!
//! A looping mark such as a working spinner is a fixed set of pre-rasterized
//! frames. Upstream, each frame change needs a notify, and a notify marks
//! every ancestor view dirty, so one 14px mark re-renders and re-lays out the
//! whole sidebar it sits in, several times a second.
//!
//! [`Window::paint_animated_svg`] rasterizes every frame into the sprite atlas
//! once and paints the current one. The window remembers which atlas tiles
//! belong to which animation. After each draw it records where those sprites
//! landed in the finished scene; when no view is dirty, each display refresh
//! only checks the clock, and on a frame boundary rewrites those sprites' tiles
//! in place and presents the same scene again.
//!
//! Cached views replay their sprites from the previous scene, carrying
//! whichever frame's tile was current then. The post-draw pass recognizes any
//! frame's tile, so replayed marks keep animating.

use crate::{AtlasTile, DevicePixels, Scene, SharedString, Size};
use collections::FxHashMap;
use scheduler::Instant;
use smallvec::SmallVec;
use std::time::Duration;

#[derive(Clone, PartialEq, Eq, Hash)]
struct AnimationKey {
    first_frame: SharedString,
    frame_count: usize,
    size: Size<DevicePixels>,
    interval: Duration,
}

struct SpriteAnimation {
    frames: SmallVec<[AtlasTile; 8]>,
    interval: Duration,
    /// The step last written into the rendered scene.
    shown_step: Option<u64>,
    /// Whether the rendered scene shows this animation.
    live: bool,
}

impl SpriteAnimation {
    fn step(&self, elapsed: Duration) -> u64 {
        (elapsed.as_nanos() / self.interval.as_nanos().max(1)) as u64
    }

    /// Frames may sit in different atlas textures: the scene groups sprites
    /// into per-texture batches only when it is presented.
    fn frame_at(&self, step: u64) -> AtlasTile {
        self.frames[(step % self.frames.len() as u64) as usize]
    }
}

pub(crate) struct SpriteAnimations {
    /// Shared by every animation in the window, so marks stay in phase. Set
    /// by the first registration, from the executor's clock (tests drive it).
    epoch: Option<Instant>,
    animations: Vec<SpriteAnimation>,
    by_key: FxHashMap<AnimationKey, usize>,
    /// Keyed by texture index: monochrome sprites only use monochrome textures.
    by_tile: FxHashMap<(u32, u32), usize>,
    /// `(monochrome sprite index, animation)` for the rendered scene.
    live: Vec<(usize, usize)>,
}

impl SpriteAnimations {
    pub(crate) fn new() -> Self {
        Self {
            epoch: None,
            animations: Vec::new(),
            by_key: FxHashMap::default(),
            by_tile: FxHashMap::default(),
            live: Vec::new(),
        }
    }

    pub(crate) fn lookup(
        &self,
        frames: &[SharedString],
        size: Size<DevicePixels>,
        interval: Duration,
    ) -> Option<usize> {
        self.by_key.get(&Self::key(frames, size, interval)).copied()
    }

    pub(crate) fn register(
        &mut self,
        frames: &[SharedString],
        size: Size<DevicePixels>,
        interval: Duration,
        tiles: SmallVec<[AtlasTile; 8]>,
        now: Instant,
    ) -> usize {
        self.epoch.get_or_insert(now);
        let index = self.animations.len();
        for tile in &tiles {
            self.by_tile
                .insert((tile.texture_id.index, tile.tile_id.0), index);
        }
        self.animations.push(SpriteAnimation {
            frames: tiles,
            interval,
            shown_step: None,
            live: false,
        });
        self.by_key.insert(Self::key(frames, size, interval), index);
        index
    }

    fn key(frames: &[SharedString], size: Size<DevicePixels>, interval: Duration) -> AnimationKey {
        AnimationKey {
            first_frame: frames[0].clone(),
            frame_count: frames.len(),
            size,
            interval,
        }
    }

    /// The tile `animation` shows right now.
    pub(crate) fn current_tile(&self, animation: usize, now: Instant) -> AtlasTile {
        let animation = &self.animations[animation];
        animation.frame_at(animation.step(self.elapsed(now)))
    }

    fn elapsed(&self, now: Instant) -> Duration {
        self.epoch
            .map_or(Duration::ZERO, |epoch| now.saturating_duration_since(epoch))
    }

    /// Forgets every animation, for when the atlas's tiles stop being valid
    /// (GPU device recovery). The next paint registers them again.
    pub(crate) fn clear(&mut self) {
        self.animations.clear();
        self.by_key.clear();
        self.by_tile.clear();
        self.live.clear();
    }

    /// Finds the animated sprites in a freshly drawn scene and brings them to
    /// the current step. Replayed sprites can carry any frame's tile.
    pub(crate) fn collect(&mut self, scene: &mut Scene, now: Instant) {
        self.live.clear();
        if self.animations.is_empty() {
            return;
        }
        for animation in &mut self.animations {
            animation.shown_step = None;
            animation.live = false;
        }
        for (index, sprite) in scene.monochrome_sprites.iter().enumerate() {
            if let Some(&animation) = self
                .by_tile
                .get(&(sprite.tile.texture_id.index, sprite.tile.tile_id.0))
            {
                self.live.push((index, animation));
                self.animations[animation].live = true;
            }
        }
        self.apply(scene, now);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn live_tiles(&self, scene: &Scene) -> Vec<AtlasTile> {
        self.live
            .iter()
            .filter_map(|&(index, _)| scene.monochrome_sprites.get(index))
            .map(|sprite| sprite.tile)
            .collect()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn forget_shown_steps(&mut self) {
        for animation in &mut self.animations {
            animation.shown_step = None;
        }
    }

    /// Whether the rendered scene shows any animated sprite.
    pub(crate) fn is_live(&self) -> bool {
        !self.live.is_empty()
    }

    /// Writes the current frame into every live sprite whose animation has
    /// reached a new step. Returns whether anything changed, which means the
    /// scene needs presenting again.
    pub(crate) fn apply(&mut self, scene: &mut Scene, now: Instant) -> bool {
        if self.live.is_empty() {
            return false;
        }
        let elapsed = self.elapsed(now);
        let mut changed = false;
        for animation in self
            .animations
            .iter_mut()
            .filter(|animation| animation.live)
        {
            let step = animation.step(elapsed);
            if animation.shown_step != Some(step) {
                animation.shown_step = Some(step);
                changed = true;
            }
        }
        if !changed {
            return false;
        }
        for &(index, animation) in &self.live {
            let animation = &self.animations[animation];
            let Some(step) = animation.shown_step else {
                continue;
            };
            if let Some(sprite) = scene.monochrome_sprites.get_mut(index) {
                sprite.tile = animation.frame_at(step);
            }
        }
        true
    }
}
