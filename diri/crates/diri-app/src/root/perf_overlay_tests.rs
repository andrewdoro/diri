//! The developer performance overlay and render counters in a real window:
//! off by default, toggled by key and command, saved, and never keeping the
//! window drawing on its own.

use std::time::Duration;

use gpui::{TestAppContext, VisualTestContext, size};

use super::tests::test_services;
use super::*;
use crate::sidebar::SidebarPreviewFixture;

fn open(cx: &mut TestAppContext) -> (Entity<RootView>, &mut VisualTestContext) {
    cx.update(|cx| {
        cx.set_reduce_motion(true);
        commands::bind_keys(cx, &Default::default());
    });
    let services = test_services();
    {
        let fixture = SidebarPreviewFixture::make(PreviewScenario::Typical);
        let mut store = services.store.store.write().unwrap();
        store.hydrate(fixture.list);
        store.select(fixture.selected_session_id.unwrap());
        store
            .update_preferences(|prefs| prefs.sidebar_visible = true)
            .unwrap();
    }
    let (root, cx) = cx.add_window_view(move |window, cx| {
        RootView::new(services, false, PreviewScenario::Empty, window, cx)
    });
    cx.simulate_resize(size(px(1200.0), px(800.0)));
    root.update(cx, |root, cx| {
        root.preview = false;
        cx.notify();
    });
    cx.run_until_parked();
    (root, cx)
}

fn saved(root: &Entity<RootView>, cx: &mut VisualTestContext) -> (bool, bool) {
    root.read_with(cx, |root, _| {
        let store = root.services.store.store.read().unwrap();
        let prefs = store.preferences();
        (prefs.perf_overlay, prefs.render_counters)
    })
}

fn focus_terminal(root: &Entity<RootView>, cx: &mut VisualTestContext) {
    root.update_in(cx, |root, window, cx| {
        if let Some(terminal) = root.active_terminal(cx) {
            terminal.update(cx, |terminal, cx| terminal.focus(window, cx));
        }
    });
    cx.run_until_parked();
}

#[gpui::test]
fn the_overlay_is_off_until_its_key_and_the_choice_is_saved(cx: &mut TestAppContext) {
    let (root, cx) = open(cx);
    assert_eq!(saved(&root, cx), (false, false), "off by default");
    assert!(cx.debug_bounds("perf-overlay").is_none());
    assert!(cx.debug_bounds("render-badge-sidebar").is_none());

    // From inside the terminal, where most keys belong to the agent.
    focus_terminal(&root, cx);
    cx.simulate_keystrokes(&commands::test_chords("cmd-alt-p"));
    cx.run_until_parked();
    assert_eq!(saved(&root, cx), (true, false));
    assert!(crate::perf_overlay::overlay_enabled());
    assert!(cx.debug_bounds("perf-overlay").is_some());
    // Without render counters the overlay has no render list.
    assert!(cx.debug_bounds("perf-renders-root").is_none());

    // Its close button turns it off again, and that is saved too.
    let close = cx.debug_bounds("perf-overlay-close").expect("close button");
    cx.simulate_click(close.center(), gpui::Modifiers::none());
    cx.run_until_parked();
    assert_eq!(saved(&root, cx), (false, false));
    assert!(!crate::perf_overlay::overlay_enabled());
    assert!(cx.debug_bounds("perf-overlay").is_none());
}

#[gpui::test]
fn render_counters_badge_the_views_and_list_them_in_the_overlay(cx: &mut TestAppContext) {
    let (root, cx) = open(cx);
    focus_terminal(&root, cx);
    cx.simulate_keystrokes(&commands::test_chords("cmd-alt-r"));
    cx.run_until_parked();
    assert_eq!(saved(&root, cx), (false, true));
    assert!(
        cx.debug_bounds("render-badge-sidebar").is_some(),
        "the cached sidebar renders again to wear its badge"
    );
    assert!(cx.debug_bounds("render-badge-terminal").is_some());
    assert!(
        cx.debug_bounds("perf-overlay").is_none(),
        "only the counters"
    );

    root.update_in(cx, |root, window, cx| {
        root.run_command(CommandId::TogglePerfOverlay, window, cx);
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("perf-renders-root").is_some());
    assert!(cx.debug_bounds("perf-renders-sidebar").is_some());

    // Reset clears the list until something renders again.
    let reset = cx.debug_bounds("perf-overlay-reset").expect("reset button");
    cx.simulate_click(reset.center(), gpui::Modifiers::none());
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("perf-renders-sidebar").is_none(),
        "the sidebar has not rendered since the reset"
    );
    assert!(cx.debug_bounds("perf-renders-root").is_some());

    root.update_in(cx, |root, window, cx| {
        root.run_command(CommandId::ToggleRenderCounters, window, cx);
        root.run_command(CommandId::TogglePerfOverlay, window, cx);
    });
    cx.run_until_parked();
    assert_eq!(saved(&root, cx), (false, false));
    assert!(cx.debug_bounds("render-badge-sidebar").is_none());
    assert!(cx.debug_bounds("perf-overlay").is_none());
}

/// The overlay reads frames the window draws anyway. Once they stop it
/// repaints itself once to say so, leaves that frame out, and then the
/// window stays still.
#[gpui::test]
fn the_overlay_goes_idle_instead_of_keeping_the_window_drawing(cx: &mut TestAppContext) {
    let (root, cx) = open(cx);
    // Let launch timers (title settle, notices) run out first: their frames
    // are real and would be counted.
    cx.executor().advance_clock(Duration::from_secs(30));
    cx.run_until_parked();
    root.update_in(cx, |root, window, cx| {
        root.run_command(CommandId::TogglePerfOverlay, window, cx);
    });
    cx.run_until_parked();
    let window = root.read_with(cx, |_, cx| {
        cx.windows()
            .first()
            .map(|window| window.window_id().as_u64())
            .unwrap()
    });
    let frames = |cx: &mut VisualTestContext| {
        cx.update(|_, cx| {
            crate::perf_overlay::since_last_frame(window, cx.background_executor().now())
        })
    };
    assert!(
        frames(cx).is_some(),
        "the frame that showed the overlay was counted"
    );
    // The overlay draws before its frame is counted, so it reads the frame
    // before: any real redraw now shows a rate.
    root.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    assert!(cx.debug_bounds("perf-fps-idle").is_none());
    assert!(cx.debug_bounds("perf-fps-1").is_some());

    cx.executor().advance_clock(Duration::from_millis(1500));
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("perf-fps-idle").is_some(),
        "a quiet second reads as idle"
    );
    let quiet_since = frames(cx).expect("a last frame");
    assert!(
        quiet_since >= crate::perf_overlay::IDLE_AFTER,
        "the idle repaint is not counted as a frame"
    );
    assert!(root.read_with(cx, |root, _| root.perf_idle_repaint.is_none()));

    // Nothing else draws: the overlay schedules nothing more.
    cx.executor().advance_clock(Duration::from_secs(10));
    cx.run_until_parked();
    assert!(root.read_with(cx, |root, _| root.perf_idle_repaint.is_none()));
    assert!(frames(cx).unwrap() >= quiet_since + Duration::from_secs(10));

    root.update_in(cx, |root, window, cx| {
        root.run_command(CommandId::TogglePerfOverlay, window, cx);
    });
    cx.run_until_parked();
}

/// A headless picture of the overlay with render counters on, for the PR.
/// `DIRI_VISUAL_OUTPUT` is the PNG path; `DIRI_VISUAL_THEME` a theme id.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "writes the performance overlay screenshot artifact"]
fn render_perf_overlay_screenshot() {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Instant;

    use crate::perf_overlay::{
        Corner, FrameLog, FrameSample, Memory, OverlayActions, OverlayModel, RenderCounts,
    };
    use gpui::HeadlessAppContext;

    let output = std::env::var_os("DIRI_VISUAL_OUTPUT")
        .map(PathBuf::from)
        .expect("set DIRI_VISUAL_OUTPUT to the target PNG path");
    let theme = std::env::var("DIRI_VISUAL_THEME").unwrap_or_else(|_| "dirijor-dark".into());
    let colors = crate::app_theme::colors(&theme);

    // A plausible minute: a burst of 60 Hz frames with two hitches.
    let start = Instant::now();
    let mut log = FrameLog::default();
    let mut at = start;
    for n in 0..90u64 {
        let gap = match n {
            40 => 50,
            71 => 34,
            _ => 16,
        };
        at += Duration::from_millis(gap);
        let cost = match n {
            40 => 31_000,
            71 => 19_500,
            _ => 2_400 + (n * 997 % 4_100),
        };
        log.record(FrameSample {
            end: at,
            cost: Duration::from_micros(cost),
            views_rendered: 3,
            views_reused: 14,
            terminal_paints: 1,
        });
    }
    let now = at + Duration::from_millis(8);
    let mut renders = RenderCounts::default();
    let wall = Instant::now();
    for (name, total, recent) in [
        ("root", 1_284u64, 58u64),
        ("terminal", 1_190, 57),
        ("sidebar", 214, 4),
        ("sidebar row", 96, 2),
        ("inspector", 31, 0),
        ("palette", 12, 0),
    ] {
        for _ in 0..(total - recent) {
            renders.hit(name, wall - Duration::from_secs(5));
        }
        for _ in 0..recent {
            renders.hit(name, wall);
        }
    }
    let model = OverlayModel {
        frames: log.readout(now),
        memory: Some(Memory {
            footprint_bytes: 412 * 1024 * 1024,
            open_fds: 118,
        }),
        renders: Some(renders.rows(wall)),
        corner: Corner::TopLeft,
    };

    struct Harness {
        model: OverlayModel,
        colors: diri_ui::SemanticColors,
    }
    impl Render for Harness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let noop = || -> crate::perf_overlay::Handler { Box::new(|_, _, _| {}) };
            div()
                .size_full()
                .relative()
                .bg(self.colors.work_surface())
                .child(crate::perf_overlay::overlay(
                    self.model.clone(),
                    self.colors,
                    OverlayActions {
                        move_corner: noop(),
                        reset: noop(),
                        close: noop(),
                    },
                ))
                .child(crate::perf_overlay::render_badge("sidebar", 214))
        }
    }

    let platform = gpui_platform::current_platform(true);
    let mut cx = HeadlessAppContext::with_platform(
        platform.text_system(),
        Arc::new(diri_ui::IconAssets),
        gpui_platform::current_headless_renderer,
    );
    cx.update(|cx| crate::fonts::init(cx));
    let window = cx
        .open_window(size(px(260.0), px(500.0)), move |_, cx| {
            cx.new(|_| Harness { model, colors })
        })
        .expect("open headless overlay window");
    cx.run_until_parked();
    let screenshot = cx
        .capture_screenshot(window.into())
        .expect("capture overlay screenshot");
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).expect("create screenshot directory");
    }
    screenshot.save(output).expect("save overlay screenshot");
}
