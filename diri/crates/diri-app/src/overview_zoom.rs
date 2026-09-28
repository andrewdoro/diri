//! Safari's pinch into Tab Overview, as presentation state only.
//!
//! One number describes the whole transition: `progress`, 0 with the session
//! filling the workbench and 1 with it sitting in its overview thumbnail.
//! While fingers are down the page scale is the truth -- it is the product of
//! every magnification delta, so the page tracks the fingers 1:1 -- and
//! progress is derived from it against the thumbnail's size. After release,
//! progress itself travels on the shared settle curve. Nothing here schedules
//! frames, reads a clock, or touches the store; the surface owns all of that.
use diri_ui::Motion;
use std::time::{Duration, Instant};

/// Where a release lands when the fingers carry no velocity. Safari commits
/// at roughly half way; so does this.
pub(crate) const COMMIT: f32 = 0.5;
/// How far (in seconds of travel) a flick is projected before deciding.
const FLICK_PROJECTION: f32 = 0.12;
/// The most a pinch-in past the thumbnail may shrink it, as progress.
const RUBBER_BAND: f32 = 0.06;
/// Thumbnail-to-page width. Never 1, or progress would divide by zero.
const MIN_SLOT_SCALE: f32 = 0.02;
const MAX_SLOT_SCALE: f32 = 0.95;
/// Reduce Motion replaces the flight with a cross-fade of overlay length.
const FADE: Duration = Duration::from_millis((Motion::OVERLAY_FADE * 1000.0) as u64);
const FLIGHT: Duration = Duration::from_millis((Motion::SETTLE.response * 1000.0) as u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Landing {
    Page,
    Overview,
}

impl Landing {
    fn progress(self) -> f32 {
        match self {
            Self::Page => 0.0,
            Self::Overview => 1.0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Tracking {
    /// Page width over workbench width, straight from the fingers.
    scale: f32,
    started_from: Landing,
    velocity: f32,
    last_progress: f32,
    last_at: Instant,
    ticked: bool,
}

#[derive(Clone, Copy, Debug)]
struct Flight {
    from: f32,
    to: Landing,
    started: Instant,
    duration: Duration,
    /// Released by the fingers, so the grip hands over along the flight.
    from_fingers: bool,
    eased: f32,
}

impl Flight {
    fn sample(&mut self, now: Instant) -> (f32, bool) {
        let t = (now.saturating_duration_since(self.started).as_secs_f32()
            / self.duration.as_secs_f32())
        .clamp(0.0, 1.0);
        self.eased = Motion::SETTLE.settle(t);
        let to = self.to.progress();
        (self.from + (to - self.from) * self.eased, t >= 1.0)
    }
}

#[derive(Debug)]
pub(crate) struct OverviewZoom {
    progress: f32,
    slot_scale: f32,
    tracking: Option<Tracking>,
    flight: Option<Flight>,
    feedback_pending: bool,
}

impl Default for OverviewZoom {
    fn default() -> Self {
        Self {
            progress: 0.0,
            slot_scale: 0.3,
            tracking: None,
            flight: None,
            feedback_pending: false,
        }
    }
}

impl OverviewZoom {
    /// The thumbnail width over the page width. A pinch keeps its scale when
    /// this changes (the fingers own the scale); only derived progress moves.
    pub(crate) fn set_slot_scale(&mut self, slot_scale: f32) {
        if slot_scale.is_finite() {
            self.slot_scale = slot_scale.clamp(MIN_SLOT_SCALE, MAX_SLOT_SCALE);
        }
    }

    /// Grab the presentation where it is, including mid-flight, so a second
    /// pinch never jumps. `at_rest` is where the surface sits when idle.
    pub(crate) fn begin(&mut self, at_rest: Landing, now: Instant) {
        self.advance(now);
        let started_from = self.flight.map_or(at_rest, |flight| flight.to);
        let progress = if self.is_active() {
            self.progress
        } else {
            at_rest.progress()
        };
        self.flight = None;
        self.progress = progress;
        self.tracking = Some(Tracking {
            scale: 1.0 - progress * (1.0 - self.slot_scale),
            started_from,
            velocity: 0.0,
            last_progress: progress,
            last_at: now,
            ticked: false,
        });
    }

    /// One `magnifyWithEvent:` delta. Positive spreads the fingers.
    pub(crate) fn pinch(&mut self, magnification: f32, now: Instant) {
        let slot_scale = self.slot_scale;
        let Some(tracking) = self.tracking.as_mut() else {
            return;
        };
        if !magnification.is_finite() {
            return;
        }
        // Spreading past the full page does nothing: Diri has no page zoom
        // to hand over to, so the page simply stops at its full size.
        tracking.scale = (tracking.scale * (1.0 + magnification)).clamp(0.0, 1.0);
        let raw = (1.0 - tracking.scale) / (1.0 - slot_scale);
        let progress = if raw > 1.0 {
            // Pinching in past the thumbnail only gives way a little.
            1.0 + RUBBER_BAND * (1.0 - 1.0 / (1.0 + (raw - 1.0) * 3.0))
        } else {
            raw
        };
        let dt = now
            .saturating_duration_since(tracking.last_at)
            .as_secs_f32();
        if dt > 0.001 {
            let instant = (progress - tracking.last_progress) / dt;
            tracking.velocity = tracking.velocity * 0.3 + instant * 0.7;
            tracking.last_progress = progress;
            tracking.last_at = now;
        }
        let crossed = (self.progress < COMMIT) != (progress < COMMIT);
        if crossed && !tracking.ticked {
            // One tick per gesture, however often the fingers cross back.
            tracking.ticked = true;
            self.feedback_pending = true;
        }
        self.progress = progress;
    }

    /// Fingers lifted: pick a side from position plus projected velocity and
    /// settle there from exactly where the page is.
    pub(crate) fn release(&mut self, now: Instant, reduced_motion: bool) -> Option<Landing> {
        let tracking = self.tracking.take()?;
        // A pinch that is still moving decides with its velocity, the way
        // Safari lets a quick flick commit before halfway.
        let still = now.saturating_duration_since(tracking.last_at) > Duration::from_millis(80);
        let velocity = if still { 0.0 } else { tracking.velocity };
        let projected = self.progress + velocity * FLICK_PROJECTION;
        let landing = if projected >= COMMIT {
            Landing::Overview
        } else {
            Landing::Page
        };
        self.fly_to(landing, now, reduced_motion, true);
        Some(landing)
    }

    /// The system took the gesture away: go back where it started.
    pub(crate) fn cancel(&mut self, now: Instant, reduced_motion: bool) -> Option<Landing> {
        let tracking = self.tracking.take()?;
        self.fly_to(tracking.started_from, now, reduced_motion, true);
        Some(tracking.started_from)
    }

    /// The non-interactive version of the same transition (keyboard, menus,
    /// clicks). Starts from the current pose so it can interrupt a flight.
    pub(crate) fn animate_to(&mut self, landing: Landing, now: Instant, reduced_motion: bool) {
        self.advance(now);
        self.tracking = None;
        self.fly_to(landing, now, reduced_motion, false);
    }

    fn fly_to(&mut self, landing: Landing, now: Instant, reduced_motion: bool, from_fingers: bool) {
        let from = self.progress;
        self.flight = (from != landing.progress()).then_some(Flight {
            from,
            to: landing,
            started: now,
            duration: if reduced_motion { FADE } else { FLIGHT },
            from_fingers,
            eased: 0.0,
        });
        if self.flight.is_none() {
            self.progress = landing.progress();
        }
    }

    /// Stop everything and rest at `landing` with no animation.
    pub(crate) fn reset(&mut self, landing: Landing) {
        self.tracking = None;
        self.flight = None;
        self.progress = landing.progress();
    }

    /// Advances a flight. Returns where it landed on the frame it finishes.
    pub(crate) fn advance(&mut self, now: Instant) -> Option<Landing> {
        let flight = self.flight.as_mut()?;
        let (progress, done) = flight.sample(now);
        let flight = *flight;
        self.progress = progress;
        if done {
            self.flight = None;
            self.progress = flight.to.progress();
            return Some(flight.to);
        }
        None
    }

    pub(crate) fn take_feedback(&mut self) -> bool {
        std::mem::take(&mut self.feedback_pending)
    }

    pub(crate) fn is_tracking(&self) -> bool {
        self.tracking.is_some()
    }

    pub(crate) fn is_flying(&self) -> bool {
        self.flight.is_some()
    }

    /// Tracking or flying: the surface must paint the transition.
    pub(crate) fn is_active(&self) -> bool {
        self.tracking.is_some() || self.flight.is_some()
    }

    /// Where the current flight is heading, if any.
    pub(crate) fn heading(&self) -> Option<Landing> {
        self.flight.map(|flight| flight.to)
    }

    /// 0 at the page, 1 in the grid, slightly above 1 while rubber-banding.
    pub(crate) fn progress(&self) -> f32 {
        self.progress
    }

    /// How much the fingers still own the page's position: all of it while
    /// they are down, handing over to the flight's own path after release.
    pub(crate) fn finger_weight(&self) -> f32 {
        if self.tracking.is_some() {
            return 1.0;
        }
        self.flight
            .filter(|flight| flight.from_fingers)
            .map_or(0.0, |flight| 1.0 - flight.eased)
    }

    /// Page width over workbench width for the current progress.
    pub(crate) fn page_scale(&self) -> f32 {
        1.0 - self.progress.max(0.0) * (1.0 - self.slot_scale)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct ZoomRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl ZoomRect {
    pub(crate) fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x <= self.x + self.width && y >= self.y && y <= self.y + self.height
    }

    fn valid(&self) -> bool {
        [self.x, self.y, self.width, self.height]
            .iter()
            .all(|value| value.is_finite())
            && self.width > 1.0
            && self.height > 1.0
    }
}

/// The point of the page the fingers took hold of, as a fraction of the page,
/// and where the fingers are now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Grip {
    pub fraction: (f32, f32),
    pub fingers: (f32, f32),
}

impl Grip {
    /// Take hold of whatever frame is on screen, so grabbing never jumps.
    pub(crate) fn take(frame: ZoomRect, fingers: (f32, f32)) -> Self {
        let fraction = |at: f32, origin: f32, size: f32| {
            if size > 1.0 {
                ((at - origin) / size).clamp(0.0, 1.0)
            } else {
                0.5
            }
        };
        Self {
            fraction: (
                fraction(fingers.0, frame.x, frame.width),
                fraction(fingers.1, frame.y, frame.height),
            ),
            fingers,
        }
    }
}

/// Where the shrinking page is painted.
///
/// Width is exactly `page_scale` of the workbench, so content never stretches.
/// Height morphs from the workbench aspect to the thumbnail's crop, keeping the
/// top of the page, which is where Safari crops too. The gripped point of the
/// page sits under the fingers while they own it (`finger_weight` 1) and on
/// its natural straight path between workbench and slot otherwise, so a
/// release hands the page from the fingers to the flight without a jump and
/// progress 0 and 1 are the workbench and the slot exactly.
pub(crate) fn page_frame(
    page: ZoomRect,
    slot: ZoomRect,
    grip: Option<Grip>,
    finger_weight: f32,
    progress: f32,
    page_scale: f32,
) -> ZoomRect {
    if !page.valid() || !slot.valid() {
        return page;
    }
    let p = progress.clamp(0.0, 1.0);
    let width = page.width * page_scale;
    let height = if progress > 1.0 {
        // Rubber band: keep the slot's crop while it gives way.
        slot.height * width / slot.width
    } else {
        page.height + (slot.height - page.height) * p
    };
    let (u, v) = grip.map_or((0.5, 0.5), |grip| grip.fraction);
    let natural = (
        page.x + u * page.width + (slot.x + u * slot.width - page.x - u * page.width) * p,
        page.y + v * page.height + (slot.y + v * slot.height - page.y - v * page.height) * p,
    );
    let weight = if grip.is_some() {
        finger_weight.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let fingers = grip.map_or(natural, |grip| grip.fingers);
    let held = (
        natural.0 + (fingers.0 - natural.0) * weight,
        natural.1 + (fingers.1 - natural.1) * weight,
    );
    ZoomRect {
        x: held.0 - u * width,
        y: held.1 - v * height,
        width,
        height,
    }
}

/// Before the grid has been laid out once there is no measured thumbnail.
/// This mirrors the gallery's layout closely enough for the first frame of a
/// pinch; every later frame uses the painted slot.
pub(crate) fn estimated_slot(index: usize, columns: usize, viewport_width: f32) -> ZoomRect {
    const EDGE: f32 = 24.0;
    const GAP: f32 = 16.0;
    // Titlebar inset, header, search, filters and hairline above the gallery.
    const GALLERY_TOP: f32 = 42.0 + 64.0 + 50.0 + 38.0 + 1.0;
    // Card padding plus border around the thumbnail.
    const CARD_INSET: f32 = 9.0;
    const THUMBNAIL: f32 = 178.0;
    const ROW_PITCH: f32 = THUMBNAIL + 2.0 * CARD_INSET + 52.0 + GAP;
    let columns = columns.max(1);
    let inner = (viewport_width - 2.0 * EDGE).max(1.0);
    let card = ((inner - GAP * (columns - 1) as f32) / columns as f32).max(1.0);
    ZoomRect {
        x: EDGE + (index % columns) as f32 * (card + GAP) + CARD_INSET,
        y: GALLERY_TOP + EDGE + CARD_INSET + (index / columns) as f32 * ROW_PITCH,
        width: (card - 2.0 * CARD_INSET).max(1.0),
        height: THUMBNAIL,
    }
}

/// How opaque the grid around the page is. It leads the page slightly so the
/// other thumbnails are readable before the page lands, as in Safari.
pub(crate) fn grid_alpha(progress: f32) -> f32 {
    Motion::SETTLE.settle((progress * 1.25).clamp(0.0, 1.0))
}

/// The live terminal only carries the first moment of a pinch, while the
/// page is still nearly full size; by a third of the way it has handed over
/// entirely to the overview card preview, which is what shrinks and lands.
pub(crate) fn live_page_alpha(progress: f32) -> f32 {
    let t = (progress / 0.3).clamp(0.0, 1.0);
    1.0 - t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> Instant {
        Instant::now()
    }

    fn zoom(slot_scale: f32) -> OverviewZoom {
        let mut zoom = OverviewZoom::default();
        zoom.set_slot_scale(slot_scale);
        zoom
    }

    #[test]
    fn page_scale_is_the_product_of_the_fingers_deltas() {
        let t = now();
        let mut zoom = zoom(0.3);
        zoom.begin(Landing::Page, t);
        zoom.pinch(-0.1, t + Duration::from_millis(8));
        zoom.pinch(-0.1, t + Duration::from_millis(16));
        assert!(
            (zoom.page_scale() - 0.81).abs() < 1e-4,
            "{}",
            zoom.page_scale()
        );
        // Spreading back returns along the same path.
        zoom.pinch(1.0 / 0.9 - 1.0, t + Duration::from_millis(24));
        assert!((zoom.page_scale() - 0.9).abs() < 1e-4);
    }

    #[test]
    fn spreading_on_the_page_does_nothing() {
        let t = now();
        let mut zoom = zoom(0.3);
        zoom.begin(Landing::Page, t);
        zoom.pinch(0.5, t + Duration::from_millis(8));
        assert_eq!(zoom.progress(), 0.0);
        assert_eq!(
            zoom.release(t + Duration::from_millis(200), false),
            Some(Landing::Page)
        );
        assert!(!zoom.is_active());
    }

    #[test]
    fn a_slow_release_decides_by_position_alone() {
        let t = now();
        let mut zoom = zoom(0.3);
        zoom.begin(Landing::Page, t);
        // Scale 0.7 is progress 3/7 < 0.5: springs back.
        zoom.pinch(-0.3, t + Duration::from_millis(10));
        let late = t + Duration::from_millis(400);
        assert_eq!(zoom.release(late, false), Some(Landing::Page));
        assert!(zoom.is_flying());
        assert_eq!(zoom.advance(late + FLIGHT), Some(Landing::Page));
        assert_eq!(zoom.progress(), 0.0);

        zoom.begin(Landing::Page, late + FLIGHT);
        zoom.pinch(-0.5, late + FLIGHT + Duration::from_millis(10));
        assert_eq!(
            zoom.release(late + FLIGHT + Duration::from_millis(400), false),
            Some(Landing::Overview)
        );
    }

    #[test]
    fn a_quick_flick_commits_before_halfway() {
        let t = now();
        let mut zoom = zoom(0.3);
        zoom.begin(Landing::Page, t);
        for step in 1..=4 {
            zoom.pinch(-0.06, t + Duration::from_millis(8 * step));
        }
        assert!(zoom.progress() < COMMIT, "{}", zoom.progress());
        assert_eq!(
            zoom.release(t + Duration::from_millis(40), false),
            Some(Landing::Overview)
        );
    }

    #[test]
    fn release_flies_from_the_exact_pose_without_a_jump() {
        let t = now();
        let mut zoom = zoom(0.3);
        zoom.begin(Landing::Page, t);
        zoom.pinch(-0.5, t + Duration::from_millis(10));
        let pose = zoom.progress();
        let released = t + Duration::from_millis(300);
        zoom.release(released, false);
        zoom.advance(released);
        assert!((zoom.progress() - pose).abs() < 1e-6);
        zoom.advance(released + FLIGHT / 2);
        assert!(zoom.progress() > pose && zoom.progress() < 1.0);
    }

    #[test]
    fn a_new_pinch_grabs_a_flight_in_place() {
        let t = now();
        let mut zoom = zoom(0.3);
        zoom.animate_to(Landing::Overview, t, false);
        let mid = t + FLIGHT / 3;
        zoom.advance(mid);
        let pose = zoom.progress();
        zoom.begin(Landing::Page, mid);
        assert!(zoom.is_tracking() && !zoom.is_flying());
        assert!((zoom.progress() - pose).abs() < 1e-6);
        assert!((zoom.page_scale() - (1.0 - pose * 0.7)).abs() < 1e-5);
        // A cancel returns to where the interrupted flight was heading.
        assert_eq!(zoom.cancel(mid, false), Some(Landing::Overview));
    }

    #[test]
    fn pinching_out_of_the_grid_grows_from_the_thumbnail() {
        let t = now();
        let mut zoom = zoom(0.25);
        zoom.reset(Landing::Overview);
        zoom.begin(Landing::Overview, t);
        assert!((zoom.page_scale() - 0.25).abs() < 1e-6);
        zoom.pinch(2.0, t + Duration::from_millis(10));
        assert!((zoom.page_scale() - 0.75).abs() < 1e-5);
        assert_eq!(
            zoom.release(t + Duration::from_millis(400), false),
            Some(Landing::Page)
        );
    }

    #[test]
    fn pinching_in_on_the_grid_only_rubber_bands() {
        let t = now();
        let mut zoom = zoom(0.3);
        zoom.reset(Landing::Overview);
        zoom.begin(Landing::Overview, t);
        for step in 1..=40 {
            zoom.pinch(-0.2, t + Duration::from_millis(8 * step));
        }
        assert!(zoom.progress() > 1.0 && zoom.progress() <= 1.0 + RUBBER_BAND);
        assert_eq!(
            zoom.release(t + Duration::from_millis(900), false),
            Some(Landing::Overview)
        );
        zoom.advance(t + Duration::from_millis(900) + FLIGHT);
        assert_eq!(zoom.progress(), 1.0);
    }

    #[test]
    fn the_threshold_ticks_once_per_gesture() {
        let t = now();
        let mut zoom = zoom(0.3);
        zoom.begin(Landing::Page, t);
        assert!(!zoom.take_feedback(), "starting a pinch is not a threshold");
        let mut at = t;
        for _ in 0..3 {
            at += Duration::from_millis(10);
            zoom.pinch(-0.5, at);
            at += Duration::from_millis(10);
            zoom.pinch(1.0, at);
        }
        assert!(zoom.take_feedback());
        assert!(!zoom.take_feedback());
    }

    #[test]
    fn reduced_motion_uses_the_short_fade() {
        let t = now();
        let mut zoom = zoom(0.3);
        zoom.animate_to(Landing::Overview, t, true);
        assert_eq!(zoom.advance(t + FADE), Some(Landing::Overview));
    }

    const PAGE: ZoomRect = ZoomRect {
        x: 240.0,
        y: 0.0,
        width: 1000.0,
        height: 800.0,
    };
    const SLOT: ZoomRect = ZoomRect {
        x: 300.0,
        y: 240.0,
        width: 300.0,
        height: 178.0,
    };

    fn close(a: ZoomRect, b: ZoomRect) -> bool {
        [
            (a.x, b.x),
            (a.y, b.y),
            (a.width, b.width),
            (a.height, b.height),
        ]
        .iter()
        .all(|(a, b)| (a - b).abs() < 1e-3)
    }

    #[test]
    fn the_frame_starts_at_the_page_and_ends_in_the_slot() {
        let grip = Some(Grip::take(PAGE, (700.0, 400.0)));
        assert!(close(page_frame(PAGE, SLOT, grip, 0.0, 0.0, 1.0), PAGE));
        assert!(close(page_frame(PAGE, SLOT, grip, 0.0, 1.0, 0.3), SLOT));
        assert!(close(page_frame(PAGE, SLOT, None, 1.0, 1.0, 0.3), SLOT));
    }

    #[test]
    fn the_gripped_point_stays_under_the_fingers_as_they_move() {
        let grip = Grip::take(PAGE, (840.0, 500.0));
        for (scale, fingers) in [(0.95, (840.0, 500.0)), (0.6, (800.0, 460.0))] {
            let progress = (1.0 - scale) / 0.7;
            let grip = Grip { fingers, ..grip };
            let frame = page_frame(PAGE, SLOT, Some(grip), 1.0, progress, scale);
            let under = (
                (fingers.0 - frame.x) / frame.width,
                (fingers.1 - frame.y) / frame.height,
            );
            assert!((under.0 - grip.fraction.0).abs() < 1e-4, "{under:?}");
            assert!((under.1 - grip.fraction.1).abs() < 1e-4, "{under:?}");
            assert!((frame.width - PAGE.width * scale).abs() < 1e-3);
        }
    }

    #[test]
    fn a_thumbnail_grows_about_the_fingers_that_grabbed_it() {
        let grip = Grip::take(SLOT, (500.0, 300.0));
        let frame = page_frame(PAGE, SLOT, Some(grip), 1.0, 1.0, 0.3);
        assert!(close(frame, SLOT), "grabbing must not move the thumbnail");
        let frame = page_frame(PAGE, SLOT, Some(grip), 1.0, 0.5, 0.65);
        assert!(((500.0 - frame.x) / frame.width - grip.fraction.0).abs() < 1e-4);
    }

    #[test]
    fn rubber_band_shrinks_about_the_grip() {
        let grip = Grip::take(SLOT, (SLOT.x + 150.0, SLOT.y + 89.0));
        let frame = page_frame(PAGE, SLOT, Some(grip), 1.0, 1.05, 0.28);
        assert!(frame.width < SLOT.width);
        let centre = |r: ZoomRect| (r.x + r.width / 2.0, r.y + r.height / 2.0);
        let (a, b) = (centre(frame), centre(SLOT));
        assert!((a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3);
    }

    #[test]
    fn the_grip_hands_over_to_the_flight_without_a_jump() {
        let t = now();
        let mut zoom = zoom(0.3);
        zoom.begin(Landing::Page, t);
        zoom.pinch(-0.5, t + Duration::from_millis(10));
        assert_eq!(zoom.finger_weight(), 1.0);
        let released = t + Duration::from_millis(400);
        zoom.release(released, false);
        zoom.advance(released);
        assert!((zoom.finger_weight() - 1.0).abs() < 1e-6);
        zoom.advance(released + FLIGHT);
        assert_eq!(zoom.finger_weight(), 0.0);
        zoom.animate_to(Landing::Page, released + FLIGHT, false);
        assert_eq!(
            zoom.finger_weight(),
            0.0,
            "keyboard flights follow the path"
        );
    }

    #[test]
    fn the_estimate_lands_inside_the_gallery_columns() {
        let first = estimated_slot(0, 3, 1100.0);
        let third = estimated_slot(2, 3, 1100.0);
        let fourth = estimated_slot(3, 3, 1100.0);
        assert!(first.x > 24.0 && third.x + third.width < 1100.0 - 24.0);
        assert_eq!(fourth.x, first.x);
        assert!(fourth.y > first.y + first.height);
    }

    #[test]
    fn the_live_terminal_hands_over_to_the_card_early() {
        assert_eq!(live_page_alpha(0.0), 1.0);
        assert_eq!(live_page_alpha(0.3), 0.0);
        assert_eq!(live_page_alpha(1.0), 0.0, "only the card ever lands");
        assert!(live_page_alpha(0.15) > 0.0 && live_page_alpha(0.15) < 1.0);
    }

    #[test]
    fn grid_alpha_is_monotonic_and_complete() {
        assert_eq!(grid_alpha(0.0), 0.0);
        assert_eq!(grid_alpha(1.0), 1.0);
        let mut previous = 0.0;
        for step in 0..=100 {
            let value = grid_alpha(step as f32 / 100.0);
            assert!(value >= previous);
            previous = value;
        }
    }
}
