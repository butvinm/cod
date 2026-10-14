//! Spike-only driver for the off-screen web view, enabled with COD_SPIKE_DRIVE=offscreen together with COD_SPIKE_URL (a file URL of `test_fixtures/offscreen.html`).
//!
//! Headless Weston has no input seat, so the driver feeds synthetic GPUI events into the window with `Window::dispatch_event`, exactly where the platform layer would, and reads the page's state back with JavaScript (`console.log` lines that the engine reports).
//! Every acceptance criterion logs one `driver: ACn PASS|FAIL ...` line.

use std::{
    sync::atomic::{AtomicU32, Ordering},
    time::{Duration, Instant},
};

use gpui::{
    Axis, AsyncWindowContext, Context, Entity, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PlatformInput, ScrollDelta,
    ScrollWheelEvent, TouchPhase, WeakEntity, Window, point, px,
};
use serde_json::Value;
use util::ResultExt as _;
use workspace::Workspace;

use crate::{
    browser_client,
    offscreen::{OffscreenView, open_url_in_active_pane},
};

const SECOND_TITLE: &str = "Cod off-screen second page";
const FIRST_TITLE: &str = "Cod off-screen test page";

pub(crate) fn start(page: Entity<OffscreenView>, window: &mut Window, cx: &mut Context<Workspace>) {
    cx.spawn_in(window, async move |workspace, cx| {
        run(workspace, page, cx).await;
    })
    .detach();
}

fn verdict(name: &str, passed: bool, detail: String) {
    let word = if passed { "PASS" } else { "FAIL" };
    log::info!("driver: {name} {word} {detail}");
}

async fn pause(cx: &AsyncWindowContext, millis: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(millis))
        .await;
}

fn read<R>(
    cx: &mut AsyncWindowContext,
    page: &Entity<OffscreenView>,
    read: impl FnOnce(&OffscreenView) -> R,
) -> Option<R> {
    page.read_with(cx, |view, _| read(view)).ok()
}

fn send(cx: &mut AsyncWindowContext, event: PlatformInput) {
    cx.update(|window, cx| {
        window.dispatch_event(event, cx);
    })
    .log_err();
}

async fn wait_until(
    cx: &mut AsyncWindowContext,
    timeout_millis: u64,
    mut condition: impl FnMut(&mut AsyncWindowContext) -> bool,
) -> Option<Duration> {
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(timeout_millis) {
        if condition(cx) {
            return Some(started.elapsed());
        }
        pause(cx, 25).await;
    }
    None
}

fn run_script(cx: &mut AsyncWindowContext, page: &Entity<OffscreenView>, script: &str) {
    if let Some(Some(engine)) = read(cx, page, |view| view.page()) {
        engine.execute_javascript(script);
    }
}

async fn read_state(cx: &mut AsyncWindowContext, page: &Entity<OffscreenView>) -> Option<Value> {
    static SEQUENCE: AtomicU32 = AtomicU32::new(0);
    let id = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let prefix = format!("COD_STATE#{id} ");
    run_script(
        cx,
        page,
        &format!("console.log('COD_STATE#{id} ' + window.codState())"),
    );
    let mut json = None;
    wait_until(cx, 3000, |cx| {
        json = read(cx, page, |view| {
            view.console()
                .iter()
                .find_map(|line| line.strip_prefix(prefix.as_str()).map(str::to_owned))
        })
        .flatten();
        json.is_some()
    })
    .await?;
    serde_json::from_str(&json?).ok()
}

fn number(state: &Option<Value>, key: &str) -> f64 {
    state
        .as_ref()
        .and_then(|state| state.get(key))
        .and_then(Value::as_f64)
        .unwrap_or(f64::NAN)
}

fn target(state: &Option<Value>, key: &str) -> (f32, f32) {
    let spot = state.as_ref().and_then(|state| state.get(key));
    let coordinate = |axis: &str| {
        spot.and_then(|spot| spot.get(axis))
            .and_then(Value::as_f64)
            .unwrap_or(0.0) as f32
    };
    (coordinate("x"), coordinate("y"))
}

fn pane_origin(cx: &mut AsyncWindowContext, page: &Entity<OffscreenView>) -> gpui::Point<gpui::Pixels> {
    read(cx, page, |view| view.pane_bounds().origin).unwrap_or_default()
}

fn move_to(cx: &mut AsyncWindowContext, position: gpui::Point<gpui::Pixels>) {
    send(
        cx,
        PlatformInput::MouseMove(MouseMoveEvent {
            position,
            pressed_button: None,
            modifiers: Modifiers::default(),
        }),
    );
}

fn click_at(cx: &mut AsyncWindowContext, position: gpui::Point<gpui::Pixels>) {
    move_to(cx, position);
    send(
        cx,
        PlatformInput::MouseDown(MouseDownEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        }),
    );
    send(
        cx,
        PlatformInput::MouseUp(MouseUpEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::default(),
            click_count: 1,
        }),
    );
}

fn wheel(cx: &mut AsyncWindowContext, position: gpui::Point<gpui::Pixels>, delta_y: f32) {
    send(
        cx,
        PlatformInput::ScrollWheel(ScrollWheelEvent {
            position,
            delta: ScrollDelta::Pixels(point(px(0.0), px(delta_y))),
            modifiers: Modifiers::default(),
            touch_phase: TouchPhase::Moved,
        }),
    );
}

fn press(cx: &mut AsyncWindowContext, key: &str, text: Option<&str>, modifiers: Modifiers) {
    let keystroke = Keystroke {
        modifiers,
        key: key.into(),
        key_char: text.map(str::to_owned),
    };
    send(
        cx,
        PlatformInput::KeyDown(KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        }),
    );
    send(cx, PlatformInput::KeyUp(KeyUpEvent { keystroke }));
}

async fn wait_for_title(
    cx: &mut AsyncWindowContext,
    page: &Entity<OffscreenView>,
    title: &str,
    timeout_millis: u64,
) -> Option<Duration> {
    wait_until(cx, timeout_millis, |cx| {
        read(cx, page, |view| {
            view.title() == Some(title) && !view.is_loading()
        })
        .unwrap_or(false)
    })
    .await
}

fn cpu_seconds() -> f64 {
    let Ok(stat) = std::fs::read_to_string("/proc/self/stat") else {
        return 0.0;
    };
    let Some(rest) = stat.rsplit_once(')').map(|(_, rest)| rest) else {
        return 0.0;
    };
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // utime and stime are fields 14 and 15 of the file, 12 and 13 after the command name.
    let ticks = |index: usize| {
        fields
            .get(index)
            .and_then(|field| field.parse::<f64>().ok())
            .unwrap_or(0.0)
    };
    (ticks(11) + ticks(12)) / 100.0
}

async fn run(
    workspace: WeakEntity<Workspace>,
    page: Entity<OffscreenView>,
    cx: &mut AsyncWindowContext,
) {
    let Some(loaded) = wait_for_title(cx, &page, FIRST_TITLE, 30_000).await else {
        verdict("AC1", false, "the fixture page did not load".into());
        return;
    };
    pause(cx, 500).await;
    let scale = cx.update(|window, _| window.scale_factor()).unwrap_or(1.0);
    let (frames, frame_size, bounds) = read(cx, &page, |view| {
        (view.frames_painted, view.last_frame_size, view.pane_bounds())
    })
    .unwrap_or_default();
    let expected = (
        (f32::from(bounds.size.width) * scale) as u32,
        (f32::from(bounds.size.height) * scale) as u32,
    );
    verdict(
        "AC1",
        frames > 0 && frame_size == expected,
        format!(
            "page loaded in {loaded:?}, {frames} frames drawn as GPUI images, frame {}x{} px, pane {}x{} px, scale {scale}",
            frame_size.0, frame_size.1, expected.0, expected.1
        ),
    );

    // AC2: the wheel scrolls the page.
    let state = read_state(cx, &page).await;
    let origin = pane_origin(cx, &page);
    let center = origin + point(px(200.0), px(300.0));
    let before = number(&state, "scrollY");
    move_to(cx, center);
    for _ in 0..3 {
        wheel(cx, center, -100.0);
        pause(cx, 30).await;
    }
    pause(cx, 700).await;
    let state = read_state(cx, &page).await;
    let after = number(&state, "scrollY");
    verdict(
        "AC2 wheel",
        after > before,
        format!("scrollY {before} -> {after}"),
    );
    for _ in 0..3 {
        wheel(cx, center, 200.0);
        pause(cx, 30).await;
    }
    pause(cx, 700).await;

    // AC2: a click on a link navigates and the tab title changes.
    let state = read_state(cx, &page).await;
    let (link_x, link_y) = target(&state, "link");
    click_at(cx, origin + point(px(link_x), px(link_y)));
    let navigated = wait_for_title(cx, &page, SECOND_TITLE, 5000).await;
    let title = read(cx, &page, |view| view.title().map(str::to_owned)).flatten();
    verdict(
        "AC2 link",
        navigated.is_some(),
        format!("tab title {title:?} after {navigated:?}"),
    );

    // Back to the test page for the typing check.
    let fixture = read(cx, &page, |view| view.current_url().replace("offscreen_next.html", "offscreen.html"))
        .unwrap_or_default();
    if let Some(Some(engine)) = read(cx, &page, |view| view.page()) {
        engine.load_url(&fixture);
    }
    wait_for_title(cx, &page, FIRST_TITLE, 10_000).await;
    pause(cx, 500).await;

    // AC3: click the text field, type, Backspace, arrow, type again.
    let state = read_state(cx, &page).await;
    let (field_x, field_y) = target(&state, "field");
    click_at(cx, origin + point(px(field_x), px(field_y)));
    pause(cx, 300).await;
    for letter in ["h", "e", "l", "l", "o"] {
        press(cx, letter, Some(letter), Modifiers::default());
        pause(cx, 20).await;
    }
    press(cx, "backspace", None, Modifiers::default());
    press(cx, "left", None, Modifiers::default());
    press(cx, "x", Some("X"), Modifiers::shift());
    pause(cx, 400).await;
    let state = read_state(cx, &page).await;
    let value = state
        .as_ref()
        .and_then(|state| state.get("value"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    verdict(
        "AC3",
        value == "helXl",
        format!(
            "field value {value:?} (expected \"helXl\"), caret {}, active {:?}",
            number(&state, "caret"),
            state.as_ref().and_then(|state| state.get("active"))
        ),
    );

    // AC4: resize the split; the page re-lays out within a second and the frame matches the pane.
    let width_before = number(&state, "innerWidth");
    let pane_before = read(cx, &page, |view| view.pane_bounds().size.width).unwrap_or_default();
    let resized_at = Instant::now();
    workspace
        .update_in(cx, |workspace, window, cx| {
            workspace.resize_pane(Axis::Horizontal, px(-250.0), window, cx);
        })
        .log_err();
    let mut width_after = width_before;
    let mut relaid_in = None;
    while resized_at.elapsed() < Duration::from_secs(4) {
        let state = read_state(cx, &page).await;
        width_after = number(&state, "innerWidth");
        if (width_after - width_before).abs() > 50.0 {
            relaid_in = Some(resized_at.elapsed());
            break;
        }
    }
    pause(cx, 400).await;
    let (frame_size, bounds) = read(cx, &page, |view| (view.last_frame_size, view.pane_bounds()))
        .unwrap_or_default();
    let matches = (frame_size.0 as f32 / scale - f32::from(bounds.size.width)).abs() < 1.5
        && (frame_size.1 as f32 / scale - f32::from(bounds.size.height)).abs() < 1.5;
    verdict(
        "AC4",
        relaid_in.is_some_and(|elapsed| elapsed < Duration::from_secs(1)) && matches,
        format!(
            "pane width {}px -> {}px, page innerWidth {width_before} -> {width_after} after {relaid_in:?}, latest frame {}x{} px for a pane of {}x{} px",
            f32::from(pane_before),
            f32::from(bounds.size.width),
            frame_size.0,
            frame_size.1,
            f32::from(bounds.size.width) * scale,
            f32::from(bounds.size.height) * scale,
        ),
    );

    // AC5: another tab hides the page, coming back shows it, Ctrl-W closes and frees it.
    let live_before = browser_client::live_browsers();
    let second = workspace
        .update_in(cx, |workspace, window, cx| {
            open_url_in_active_pane(workspace, fixture.replace("offscreen.html", "offscreen_next.html"), window, cx).log_err()
        })
        .ok()
        .flatten();
    pause(cx, 1500).await;
    let hidden = read(cx, &page, |view| view.is_hidden()).unwrap_or(false);
    let live_two = browser_client::live_browsers();
    let pane = workspace
        .read_with(cx, |workspace, _| workspace.active_pane().clone())
        .ok();
    if let Some(pane) = &pane {
        pane.update_in(cx, |pane, window, cx| {
            pane.activate_previous_item(&Default::default(), window, cx);
        })
        .log_err();
    }
    pause(cx, 1200).await;
    let shown = !read(cx, &page, |view| view.is_hidden()).unwrap_or(true);
    let frames_before_close = read(cx, &page, |view| view.frames_painted).unwrap_or(0);
    verdict(
        "AC5 hide/show",
        hidden && shown && second.is_some(),
        format!("after opening another tab hidden={hidden}, after switching back shown={shown}, live browser views {live_before} -> {live_two}, frames so far {frames_before_close}"),
    );
    press(
        cx,
        "w",
        None,
        Modifiers {
            control: true,
            ..Default::default()
        },
    );
    let closed = wait_until(cx, 5000, |_| browser_client::live_browsers() < live_two).await;
    verdict(
        "AC5 close",
        closed.is_some(),
        format!(
            "Ctrl-W: live browser views {live_two} -> {} after {closed:?}",
            browser_client::live_browsers()
        ),
    );

    // Frame cost: scroll the remaining page continuously and report frames, copy cost and CPU.
    if let Some(second) = second {
        let origin = pane_origin(cx, &second);
        let center = origin + point(px(200.0), px(300.0));
        let (frames_a, micros_a) = read(cx, &second, |view| (view.frames_painted, view.image_micros)).unwrap_or_default();
        let cpu_a = cpu_seconds();
        let started = Instant::now();
        run_script(
            cx,
            &second,
            "document.body.style.height='20000px'; (function tick(){ document.body.style.background = 'hsl(' + (performance.now() / 10 % 360) + ',60%,85%)'; requestAnimationFrame(tick); })();",
        );
        while started.elapsed() < Duration::from_secs(6) {
            wheel(cx, center, -40.0);
            pause(cx, 16).await;
        }
        let elapsed = started.elapsed().as_secs_f64();
        let (frames_b, micros_b) = read(cx, &second, |view| (view.frames_painted, view.image_micros)).unwrap_or_default();
        let cpu_b = cpu_seconds();
        let frames = frames_b - frames_a;
        let size = read(cx, &second, |view| view.last_frame_size).unwrap_or_default();
        log::info!(
            "driver: COST pane frame {}x{} px: {frames} frames in {elapsed:.1}s = {:.1} fps; image build {} us/frame; Zed process CPU {:.0}% of one core over the run (the cod-browser processes are measured separately)",
            size.0,
            size.1,
            frames as f64 / elapsed,
            if frames > 0 { (micros_b - micros_a) / frames } else { 0 },
            (cpu_b - cpu_a) / elapsed * 100.0,
        );
    }
    log::info!("driver: done");
}
