//! Spike-only driver, enabled with COD_SPIKE_DRIVE=1 together with COD_SPIKE_URL.
//!
//! It proves that input reaches the OS web view through AppKit without touching the user's keyboard or mouse: synthetic NSEvents are sent to the app's own NSWindow with `sendEvent:`, never posted to the system event stream.
//! Page state is read back with JavaScript and logged, and WKWebView snapshots are saved as PNG files under /tmp/cod-spike.

use std::{cell::Cell, path::PathBuf, rc::Rc, sync::Mutex, time::Duration};

use anyhow::{Context as _, Result, bail};
use command_palette::CommandPalette;
use editor::Editor;
use file_finder::FileFinder;
use futures::channel::oneshot;
use gpui::{AsyncWindowContext, Context, Entity, Focusable as _, WeakEntity, Window};
use objc2::{MainThreadMarker, rc::Retained, runtime::ProtocolObject};
use objc2_app_kit::{
    NSApplication, NSBitmapImageFileType, NSBitmapImageRep, NSEvent, NSEventModifierFlags,
    NSEventType, NSImage, NSPasteboard, NSPasteboardItem, NSPasteboardTypeString,
    NSPasteboardWriting, NSView, NSWindow,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGDisplayBounds, CGEvent, CGEventField, CGMainDisplayID, CGScrollEventUnit,
};
use objc2_foundation::{NSArray, NSData, NSDictionary, NSError, NSString};
use workspace::{OpenOptions, SplitDirection, Workspace};
use wry::WebViewExtMacOS as _;

use crate::web_view_item::{NativeWebView, WebView, open_url, open_url_in_split};

const LOCAL_PAGE_URL: &str = "file:///tmp/cod-spike/textarea.html";
const SNAPSHOT_DIRECTORY: &str = "/tmp/cod-spike";

const STATE_SCRIPT: &str = r#"JSON.stringify({
  url: location.href,
  title: document.title,
  userAgent: navigator.userAgent,
  viewport: innerWidth + 'x' + innerHeight,
  scrollHeight: document.scrollingElement ? document.scrollingElement.scrollHeight : null,
  scrollY: Math.round(window.scrollY),
  scrolledElements: [...document.querySelectorAll('*')]
    .filter(element => element.scrollTop > 0)
    .map(element => element.tagName + '.' + [...element.classList].slice(0, 2).join('.') + '=' + Math.round(element.scrollTop))
    .slice(0, 5),
  activeTab: [...document.querySelectorAll('.merge-request-tabs .active, [role=tab][aria-selected=true]')]
    .map(element => element.textContent.trim().replace(/\s+/g, ' '))
    .join(' | '),
  activeElement: document.activeElement
    ? document.activeElement.tagName + (document.activeElement.id ? '#' + document.activeElement.id : '')
    : null,
  activeValue: document.activeElement && 'value' in document.activeElement ? document.activeElement.value : null,
})"#;

const SCROLL_POSITION_SCRIPT: &str = r#"JSON.stringify([window.scrollY, ...[...document.querySelectorAll('*')].map(element => element.scrollTop)].join(','))"#;

const SURVEY_SCRIPT: &str = r#"JSON.stringify({
  tabs: [...document.querySelectorAll('.merge-request-tabs a, [role=tab]')]
    .map(element => element.textContent.trim().replace(/\s+/g, ' '))
    .slice(0, 10),
  controls: [...document.querySelectorAll('input, textarea, button[aria-label]')]
    .filter(element => element.offsetParent)
    .map(element => element.tagName + ' ' + (element.type || '') + ' ' + (element.getAttribute('aria-label') || element.placeholder || element.id || ''))
    .slice(0, 30),
})"#;

const COMMITS_TAB_SCRIPT: &str = r#"(() => {
  const tab = [...document.querySelectorAll('.merge-request-tabs a, [role=tab]')]
    .find(element => /^Commits/.test(element.textContent.trim()));
  if (!tab) return 'null';
  let rect = tab.getBoundingClientRect();
  if (rect.top < 0 || rect.bottom > innerHeight) {
    tab.scrollIntoView({ block: 'center' });
    rect = tab.getBoundingClientRect();
  }
  return JSON.stringify({ x: rect.x + rect.width / 2, y: rect.y + rect.height / 2, label: tab.textContent.trim().replace(/\s+/g, ' ') });
})()"#;

const SEARCH_BUTTON_SCRIPT: &str = r#"(() => {
  const button = [...document.querySelectorAll('button')]
    .find(element => element.offsetParent && /search/i.test((element.getAttribute('aria-label') || '') + ' ' + element.textContent));
  if (!button) return 'null';
  const rect = button.getBoundingClientRect();
  return JSON.stringify({ x: rect.x + rect.width / 2, y: rect.y + rect.height / 2, label: (button.getAttribute('aria-label') || button.textContent).trim().replace(/\s+/g, ' ') });
})()"#;

const TEXT_FIELD_SCRIPT: &str = r#"(() => {
  const field = [...document.querySelectorAll('textarea, input[type=search], input[type=text], input:not([type])')]
    .find(element => {
      const rect = element.getBoundingClientRect();
      return rect.width > 0 && rect.height > 0 && rect.top >= 0 && rect.bottom <= innerHeight;
    });
  if (!field) return 'null';
  const rect = field.getBoundingClientRect();
  return JSON.stringify({ x: rect.x + rect.width / 2, y: rect.y + rect.height / 2, label: field.tagName + ' ' + (field.placeholder || field.getAttribute('aria-label') || field.id) });
})()"#;

pub(crate) fn start(page: Entity<WebView>, window: &mut Window, cx: &mut Context<Workspace>) {
    cx.spawn_in(window, async move |workspace, cx| {
        match run(workspace, page, cx).await {
            Ok(()) => log::info!("spike driver: done"),
            Err(error) => log::error!("spike driver: {error:#}"),
        }
    })
    .detach();
}

async fn run(
    workspace: WeakEntity<Workspace>,
    gitlab: Entity<WebView>,
    cx: &mut AsyncWindowContext,
) -> Result<()> {
    let gitlab_native = wait_for_load(&gitlab, cx).await?;
    sleep(cx, 5).await;
    // gitlab.com sits behind a Cloudflare check. The driver never clicks its checkbox, it only waits for the check to pass on its own.
    for _ in 0..30 {
        let title = evaluate(&gitlab_native, "JSON.stringify(document.title)").await?;
        if title != "Just a moment..." {
            break;
        }
        sleep(cx, 1).await;
    }
    log::info!(
        "spike driver: survey {}",
        evaluate(&gitlab_native, SURVEY_SCRIPT).await?
    );
    snapshot(&gitlab_native, "gitlab-mr.png").await?;

    scroll_and_check(&gitlab_native, "gitlab", cx).await?;
    snapshot(&gitlab_native, "gitlab-scrolled.png").await?;

    match locate(&gitlab_native, COMMITS_TAB_SCRIPT).await? {
        Some(target) => {
            log_state(&gitlab_native, "before tab click").await?;
            click(&gitlab_native, &target)?;
            sleep(cx, 4).await;
            log_state(&gitlab_native, "after tab click").await?;
            snapshot(&gitlab_native, "gitlab-commits-tab.png").await?;
        }
        None => log::warn!("spike driver: no Commits tab found"),
    }

    if let Some(target) = locate(&gitlab_native, SEARCH_BUTTON_SCRIPT).await? {
        click(&gitlab_native, &target)?;
        sleep(cx, 2).await;
    }
    match locate(&gitlab_native, TEXT_FIELD_SCRIPT).await? {
        Some(target) => {
            click(&gitlab_native, &target)?;
            sleep(cx, 1).await;
            log_state(&gitlab_native, "before typing").await?;
            type_text(&gitlab_native, "runner")?;
            sleep(cx, 2).await;
            log_state(&gitlab_native, "after typing").await?;
            snapshot(&gitlab_native, "gitlab-search.png").await?;
        }
        None => log::warn!("spike driver: no text field found on the GitLab page"),
    }

    let local = workspace.update_in(cx, |workspace, window, cx| {
        open_url(workspace, LOCAL_PAGE_URL.to_string(), window, cx)
    })??;
    let local_native = wait_for_load(&local, cx).await?;
    sleep(cx, 1).await;
    log_visibility("gitlab after opening a second tab", &gitlab_native);
    log_visibility("local page", &local_native);

    let target = locate(&local_native, TEXT_FIELD_SCRIPT)
        .await?
        .context("no textarea on the local page")?;
    click(&local_native, &target)?;
    sleep(cx, 1).await;
    type_text(&local_native, "hello from cod")?;
    sleep(cx, 1).await;
    log_state(&local_native, "local page after typing").await?;
    snapshot(&local_native, "textarea.png").await?;
    scroll_and_check(&local_native, "local page", cx).await?;

    resize_window(&local_native, -240.0, -120.0)?;
    sleep(cx, 1).await;
    log_visibility("local page after window resize", &local_native);

    cx.update(|window, cx| window.dispatch_action(Box::new(workspace::ToggleLeftDock), cx))?;
    sleep(cx, 1).await;
    log_visibility("local page after toggling the left dock", &local_native);

    workspace.update_in(cx, |workspace, window, cx| {
        workspace.activate_item(&gitlab, true, true, window, cx);
    })?;
    sleep(cx, 2).await;
    log_visibility("gitlab after switching back", &gitlab_native);
    log_visibility("local page after switching back", &local_native);
    log_state(&gitlab_native, "gitlab after switching back").await?;
    snapshot(&gitlab_native, "gitlab-back.png").await?;
    Ok(())
}

async fn sleep(cx: &mut AsyncWindowContext, seconds: u64) {
    cx.background_executor()
        .timer(Duration::from_secs(seconds))
        .await;
}

async fn wait_for_load(
    page: &Entity<WebView>,
    cx: &mut AsyncWindowContext,
) -> Result<Rc<NativeWebView>> {
    for _ in 0..120 {
        let loaded = page.read_with(cx, |page, _| {
            page.loaded_url().is_some().then(|| page.native())
        });
        if let Some(native) = loaded {
            return Ok(native);
        }
        cx.background_executor()
            .timer(Duration::from_millis(500))
            .await;
    }
    bail!("the page did not finish loading in 60 seconds")
}

async fn evaluate(native: &NativeWebView, script: &str) -> Result<String> {
    let (sender, receiver) = oneshot::channel();
    let sender = Mutex::new(Some(sender));
    native
        .webview
        .evaluate_script_with_callback(script, move |result| {
            let sender = sender.lock().ok().and_then(|mut sender| sender.take());
            if let Some(sender) = sender
                && sender.send(result).is_err()
            {
                log::warn!("spike driver: script result arrived after the driver stopped");
            }
        })?;
    let result = receiver.await?;
    // Scripts return JSON.stringify(...), which wry hands back JSON-encoded once more.
    Ok(serde_json::from_str::<String>(&result).unwrap_or(result))
}

async fn log_state(native: &NativeWebView, label: &str) -> Result<()> {
    log::info!(
        "spike driver: {label}: {}",
        evaluate(native, STATE_SCRIPT).await?
    );
    Ok(())
}

fn log_visibility(label: &str, native: &NativeWebView) {
    let webview = native.webview.webview();
    let frame = webview.frame();
    log::info!(
        "spike driver: {label}: hidden={} ns_frame=({}, {}, {}, {}) gpui_bounds={:?}",
        webview.isHidden(),
        frame.origin.x,
        frame.origin.y,
        frame.size.width,
        frame.size.height,
        native.frame(),
    );
}

struct Target {
    x: f64,
    y: f64,
    label: String,
}

async fn locate(native: &NativeWebView, script: &str) -> Result<Option<Target>> {
    let result = evaluate(native, script).await?;
    let value: serde_json::Value = serde_json::from_str(&result)
        .with_context(|| format!("parsing locate result {result:?}"))?;
    if value.is_null() {
        return Ok(None);
    }
    Ok(Some(Target {
        x: value["x"].as_f64().context("missing x")?,
        y: value["y"].as_f64().context("missing y")?,
        label: value["label"].as_str().unwrap_or_default().to_string(),
    }))
}

fn native_window(native: &NativeWebView) -> Result<(Retained<NSView>, Retained<NSWindow>)> {
    let webview = native.webview.webview();
    let window = webview.window().context("the web view has no window")?;
    Ok((Retained::into_super(Retained::into_super(webview)), window))
}

/// Converts a point in page (CSS) pixels to the window's base coordinates.
fn point_in_window(view: &NSView, x: f64, y: f64) -> CGPoint {
    let y = if view.isFlipped() {
        y
    } else {
        view.frame().size.height - y
    };
    view.convertPoint_toView(CGPoint::new(x, y), None)
}

fn click(native: &NativeWebView, target: &Target) -> Result<()> {
    let (view, window) = native_window(native)?;
    let location = point_in_window(&view, target.x, target.y);
    log::info!(
        "spike driver: clicking {:?} at page ({:.0}, {:.0})",
        target.label,
        target.x,
        target.y
    );
    click_at(&window, location)
}

/// Clicks a point in the window's base coordinates, through the window like AppKit does for a real click.
fn click_at(window: &NSWindow, location: CGPoint) -> Result<()> {
    // A real click comes after the pointer moved there.
    for event_type in [
        NSEventType::MouseMoved,
        NSEventType::LeftMouseDown,
        NSEventType::LeftMouseUp,
    ] {
        let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            event_type,
            location,
            NSEventModifierFlags::empty(),
            0.0,
            window.windowNumber(),
            None,
            0,
            1,
            1.0,
        )
        .context("creating a mouse event")?;
        window.sendEvent(&event);
    }
    let hit_view = window
        .contentView()
        .and_then(|content_view| {
            let point = content_view.convertPoint_fromView(location, None);
            content_view.hitTest(point)
        })
        .map(|view| view.class().name().to_string_lossy().into_owned());
    log::info!(
        "spike driver: clicked window ({:.0}, {:.0}); key_window={} hit_view={hit_view:?} first_responder={:?}",
        location.x,
        location.y,
        window.isKeyWindow(),
        first_responder_name(&window),
    );
    Ok(())
}

/// Scrolls through the window first, and if the page did not move, through the hit-tested view directly, logging which route worked.
async fn scroll_and_check(
    native: &NativeWebView,
    label: &str,
    cx: &mut AsyncWindowContext,
) -> Result<()> {
    log_state(native, &format!("{label} before scroll")).await?;
    let before = evaluate(native, SCROLL_POSITION_SCRIPT).await?;
    for route in [ScrollRoute::Window, ScrollRoute::HitView] {
        scroll(native, -600, route)?;
        sleep(cx, 2).await;
        log_state(native, &format!("{label} after scroll via {route:?}")).await?;
        if evaluate(native, SCROLL_POSITION_SCRIPT).await? != before {
            return Ok(());
        }
    }
    log::warn!("spike driver: {label} did not scroll");
    Ok(())
}

#[derive(Clone, Copy, Debug)]
enum ScrollRoute {
    Window,
    HitView,
}

fn scroll(native: &NativeWebView, delta_y: i32, route: ScrollRoute) -> Result<()> {
    let (view, window) = native_window(native)?;
    let size = view.frame().size;
    let location = point_in_window(&view, size.width / 2.0, size.height / 2.0);
    let on_screen = window.convertPointToScreen(location);
    let display = CGDisplayBounds(CGMainDisplayID());

    // AppKit only creates scroll wheel NSEvents from a CGEvent. The CGEvent is never posted,
    // it only carries the scroll delta and is handed straight to this window.
    let event = CGEvent::new_scroll_wheel_event2(None, CGScrollEventUnit::Pixel, 1, delta_y, 0, 0)
        .context("creating a scroll event")?;
    CGEvent::set_location(
        Some(&event),
        CGPoint::new(on_screen.x, display.size.height - on_screen.y),
    );
    for field in [
        CGEventField::MouseEventWindowUnderMousePointer,
        CGEventField::MouseEventWindowUnderMousePointerThatCanHandleThisEvent,
    ] {
        CGEvent::set_integer_value_field(Some(&event), field, window.windowNumber() as i64);
    }
    let event = NSEvent::eventWithCGEvent(&event).context("converting the scroll event")?;
    log::info!(
        "spike driver: scroll delta={delta_y} event_window={} location_in_window=({:.0}, {:.0}) target=({:.0}, {:.0})",
        event.windowNumber(),
        event.locationInWindow().x,
        event.locationInWindow().y,
        location.x,
        location.y,
    );
    match route {
        ScrollRoute::Window => window.sendEvent(&event),
        ScrollRoute::HitView => {
            let content_view = window.contentView().context("no content view")?;
            let point = content_view.convertPoint_fromView(location, None);
            let hit_view = content_view.hitTest(point).context("nothing under the point")?;
            hit_view.scrollWheel(&event);
        }
    }
    Ok(())
}

fn type_text(native: &NativeWebView, text: &str) -> Result<()> {
    let (_, window) = native_window(native)?;
    type_into(&window, text)
}

/// Sends plain key presses through the window, which hands them to its first responder.
fn type_into(window: &NSWindow, text: &str) -> Result<()> {
    for character in text.chars() {
        let characters = NSString::from_str(&character.to_string());
        for event_type in [NSEventType::KeyDown, NSEventType::KeyUp] {
            let event = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
                event_type,
                CGPoint::ZERO,
                NSEventModifierFlags::empty(),
                0.0,
                window.windowNumber(),
                None,
                &characters,
                &characters,
                false,
                key_code(character),
            )
            .context("creating a key event")?;
            window.sendEvent(&event);
        }
    }
    log::info!(
        "spike driver: typed {text:?}, first responder {:?}",
        first_responder_name(&window)
    );
    Ok(())
}

fn first_responder_name(window: &NSWindow) -> Option<String> {
    window
        .firstResponder()
        .map(|responder| responder.class().name().to_string_lossy().into_owned())
}

/// macOS virtual key codes for the US layout, enough for the driver's lowercase text.
fn key_code(character: char) -> u16 {
    match character {
        'a' => 0,
        's' => 1,
        'd' => 2,
        'f' => 3,
        'h' => 4,
        'g' => 5,
        'z' => 6,
        'x' => 7,
        'c' => 8,
        'v' => 9,
        'b' => 11,
        'q' => 12,
        'w' => 13,
        'e' => 14,
        'r' => 15,
        'y' => 16,
        't' => 17,
        'o' => 31,
        'u' => 32,
        'i' => 34,
        'p' => 35,
        'l' => 37,
        'j' => 38,
        'k' => 40,
        'n' => 45,
        'm' => 46,
        _ => 49,
    }
}

fn resize_window(native: &NativeWebView, delta_width: f64, delta_height: f64) -> Result<()> {
    let (_, window) = native_window(native)?;
    let frame = window.frame();
    let resized = CGRect::new(
        CGPoint::new(frame.origin.x, frame.origin.y - delta_height),
        CGSize::new(
            frame.size.width + delta_width,
            frame.size.height + delta_height,
        ),
    );
    window.setFrame_display(resized, true);
    log::info!(
        "spike driver: window resized from {}x{} to {}x{}",
        frame.size.width,
        frame.size.height,
        resized.size.width,
        resized.size.height
    );
    Ok(())
}

async fn snapshot(native: &NativeWebView, file_name: &str) -> Result<()> {
    let path = format!("{SNAPSHOT_DIRECTORY}/{file_name}");
    let (sender, receiver) = oneshot::channel::<Result<()>>();
    let sender = Cell::new(Some(sender));
    let completion = {
        let path = path.clone();
        block2::RcBlock::new(move |image: *mut NSImage, error: *mut NSError| {
            // SAFETY: WebKit passes either a valid image or a valid error, both alive for the call.
            let result = match unsafe { (image.as_ref(), error.as_ref()) } {
                (Some(image), _) => save_png(image, &path),
                (None, Some(error)) => Err(anyhow::anyhow!("snapshot failed: {error}")),
                (None, None) => Err(anyhow::anyhow!("snapshot returned nothing")),
            };
            if let Some(sender) = sender.take()
                && sender.send(result).is_err()
            {
                log::warn!("spike driver: snapshot finished after the driver stopped");
            }
        })
    };
    // SAFETY: the completion block is retained by WebKit until it is called on the main thread.
    unsafe {
        native
            .webview
            .webview()
            .takeSnapshotWithConfiguration_completionHandler(None, &completion);
    }
    receiver.await??;
    log::info!("spike driver: snapshot saved {path}");
    Ok(())
}

fn save_png(image: &NSImage, path: &str) -> Result<()> {
    let tiff = image.TIFFRepresentation().context("no TIFF data")?;
    let bitmap = NSBitmapImageRep::imageRepWithData(&tiff).context("no bitmap")?;
    // SAFETY: an empty properties dictionary is valid for PNG output.
    let png = unsafe {
        bitmap.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())
    }
    .context("no PNG data")?;
    std::fs::write(path, png.to_vec()).with_context(|| format!("writing {path}"))?;
    Ok(())
}

// Check 3 (COD_SPIKE_DRIVE=keys): keys and focus between an editor and the page.

const SAMPLE_FILE: &str = "/tmp/cod-spike/sample/README.md";
const EDITOR_TEXT: &str = "cod typed this ";
const ESCAPE: &str = "\u{1b}";
const KEY_CODE_ESCAPE: u16 = 53;

const SELECT_FIRST_WORD_SCRIPT: &str = r#"(() => {
  const field = document.getElementById('notes');
  field.setSelectionRange(0, 5);
  return JSON.stringify(field.value.slice(field.selectionStart, field.selectionEnd));
})()"#;

const CARET_TO_END_SCRIPT: &str = r#"(() => {
  const field = document.getElementById('notes');
  field.setSelectionRange(field.value.length, field.value.length);
  return JSON.stringify(field.value.length);
})()"#;

const SELECTION_SCRIPT: &str = r#"(() => {
  const field = document.getElementById('notes');
  return JSON.stringify(field.selectionStart + '..' + field.selectionEnd + ' of ' + field.value.length);
})()"#;

/// With COD_SPIKE_SHOTS=1 the driver pauses until someone outside the app saves a screenshot of the window to the named file (screencapture -l <window number>), for at most 90 seconds.
async fn window_shot(window: &NSWindow, name: &str, cx: &mut AsyncWindowContext) {
    if !std::env::var("COD_SPIKE_SHOTS").is_ok_and(|value| value == "1") {
        return;
    }
    let path = format!("{SNAPSHOT_DIRECTORY}/{name}");
    log::info!(
        "spike driver: SHOT window={} path={path}",
        window.windowNumber()
    );
    for _ in 0..180 {
        if std::path::Path::new(&path).exists() {
            sleep_millis(cx, 500).await;
            log::info!("spike driver: SHOT taken {path}");
            return;
        }
        sleep_millis(cx, 500).await;
    }
    log::warn!("spike driver: SHOT not taken {path}");
}

async fn sleep_millis(cx: &mut AsyncWindowContext, millis: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(millis))
        .await;
}

pub(crate) fn start_keys(url: String, window: &mut Window, cx: &mut Context<Workspace>) {
    cx.spawn_in(window, async move |workspace, cx| {
        match run_keys(workspace, url, cx).await {
            Ok(()) => log::info!("spike driver: keys done"),
            Err(error) => log::error!("spike driver: keys: {error:#}"),
        }
    })
    .detach();
}

/// Logs where GPUI focus is, which modal is open and AppKit's first responder.
struct Probe {
    workspace: WeakEntity<Workspace>,
    page: WeakEntity<WebView>,
    editor: Entity<Editor>,
    window: Retained<NSWindow>,
}

impl Probe {
    fn log(&self, cx: &mut AsyncWindowContext, label: &str) -> Result<()> {
        let line = self.workspace.update_in(cx, |workspace, window, cx| {
            let page = self
                .page
                .upgrade()
                .map(|page| page.read(cx).focus_handle(cx).is_focused(window));
            let editor = self
                .editor
                .read(cx)
                .focus_handle(cx)
                .contains_focused(window, cx);
            let file_finder = workspace.active_modal::<FileFinder>(cx).is_some();
            let palette = workspace.active_modal::<CommandPalette>(cx).is_some();
            let pages = workspace.items_of_type::<WebView>(cx).count();
            let prompt = window.has_active_prompt();
            let any_modal = workspace.has_active_modal(window, cx);
            format!(
                "gpui_focus page={page:?} editor={editor}; modal any={any_modal} file_finder={file_finder} command_palette={palette} gpui_prompt={prompt}; web_view_items={pages}"
            )
        })?;
        let sheet = self.window.attachedSheet().is_some();
        if sheet {
            log::warn!("spike driver: {label}: the window has an attached sheet");
        }
        log::info!(
            "spike driver: {label}: {line}; first_responder={:?}",
            first_responder_name(&self.window)
        );
        Ok(())
    }

    fn log_editor_line(&self, cx: &mut AsyncWindowContext, label: &str) -> Result<()> {
        let text = cx.update(|_, cx| self.editor.read(cx).text(cx))?;
        let line = text.lines().find(|line| line.contains(EDITOR_TEXT.trim()));
        log::info!("spike driver: {label}: editor buffer line with the typed text: {line:?}");
        Ok(())
    }
}

async fn run_keys(
    workspace: WeakEntity<Workspace>,
    url: String,
    cx: &mut AsyncWindowContext,
) -> Result<()> {
    let open = workspace.update_in(cx, |workspace, window, cx| {
        workspace.open_abs_path(
            PathBuf::from(SAMPLE_FILE),
            OpenOptions::default(),
            window,
            cx,
        )
    })?;
    let item = open.await?;
    let editor = cx
        .update(|_, cx| item.act_as::<Editor>(cx))?
        .context("the sample file did not open in an editor")?;
    let page = workspace.update_in(cx, |workspace, window, cx| {
        open_url_in_split(workspace, url, SplitDirection::Right, window, cx)
    })??;
    let native = wait_for_load(&page, cx).await?;
    sleep(cx, 2).await;
    let (native_view, window) = native_window(&native)?;
    let probe = Probe {
        workspace: workspace.clone(),
        page: page.downgrade(),
        editor: editor.clone(),
        window: window.clone(),
    };
    probe.log(cx, "page opened in a split")?;
    let _keystrokes = cx.update(|_, cx| {
        cx.observe_keystrokes(|event, _, _| {
            log::info!(
                "spike driver: GPUI keystroke {} -> action {:?} contexts={:?}",
                event.keystroke.unparse(),
                event.action.as_ref().map(|action| action.name()),
                event
                    .context_stack
                    .iter()
                    .map(|context| format!("{context:?}"))
                    .collect::<Vec<_>>(),
            );
        })
    })?;

    // AC1: a click in the page moves GPUI focus to the item.
    cx.update(|window, cx| {
        let handle = editor.read(cx).focus_handle(cx);
        handle.focus(window, cx);
    })?;
    sleep(cx, 1).await;
    probe.log(cx, "AC1 before clicking the page (editor focused first)")?;
    let field = locate(&native, TEXT_FIELD_SCRIPT)
        .await?
        .context("no textarea on the page")?;
    click(&native, &field)?;
    sleep(cx, 1).await;
    probe.log(cx, "AC1 after clicking the page")?;
    type_into(&window, "hello page")?;
    sleep(cx, 1).await;
    log_state(&native, "AC1 page after typing").await?;
    probe.log(cx, "AC1 after typing")?;
    window_shot(&window, "t3-page-next-to-editor.png", cx).await;

    // AC2: a click into the editor's text takes the keys back.
    let bounds = cx
        .update(|_, cx| editor.read(cx).last_bounds().copied())?
        .context("the editor has not been laid out")?;
    let content_view = window.contentView().context("no content view")?;
    let point = point_in_window(
        &content_view,
        f64::from(bounds.origin.x) + 120.0,
        f64::from(bounds.origin.y) + 30.0,
    );
    log::info!("spike driver: AC2 clicking the editor text, editor bounds {bounds:?}");
    click_at(&window, point)?;
    sleep(cx, 1).await;
    probe.log(cx, "AC2 after clicking the editor")?;
    type_into(&window, EDITOR_TEXT)?;
    sleep(cx, 1).await;
    probe.log_editor_line(cx, "AC2")?;
    log_state(&native, "AC2 page after typing in the editor").await?;

    // AC3: Zed shortcuts work in the editor.
    send_shortcut(&window, "p", 35, NSEventModifierFlags::Command, "cmd-p")?;
    sleep(cx, 1).await;
    probe.log(cx, "AC3 after cmd-p in the editor")?;
    send_shortcut(&window, ESCAPE, KEY_CODE_ESCAPE, NSEventModifierFlags::empty(), "escape")?;
    sleep(cx, 1).await;
    probe.log(cx, "AC3 after escape")?;
    send_shortcut(&window, "s", 1, NSEventModifierFlags::Command, "cmd-s")?;
    sleep(cx, 1).await;
    probe.log(cx, "AC3 after cmd-s in the editor")?;
    let saved = std::fs::read_to_string(SAMPLE_FILE)?;
    log::info!(
        "spike driver: AC3 file on disk contains the typed text after cmd-s: {}",
        saved.contains(EDITOR_TEXT.trim())
    );

    // AC5: edit shortcuts reach the page, Zed shortcuts still reach Zed.
    let saved_pasteboard = save_pasteboard();
    log::info!(
        "spike driver: AC5 saved the user's pasteboard ({} items) to restore it afterwards",
        saved_pasteboard.len()
    );
    let result = edit_shortcuts(&native, &window, &probe, cx).await;
    restore_pasteboard(&saved_pasteboard);
    log::info!("spike driver: AC5 restored the user's pasteboard");
    result?;

    send_shortcut(&window, "p", 35, NSEventModifierFlags::Command, "cmd-p")?;
    sleep(cx, 1).await;
    probe.log(cx, "AC5 after cmd-p in the page")?;
    log_visibility("AC5 page while the file finder is open", &native);
    send_shortcut(&window, ESCAPE, KEY_CODE_ESCAPE, NSEventModifierFlags::empty(), "escape")?;
    sleep(cx, 1).await;
    probe.log(cx, "AC5 after escape")?;
    log_visibility("AC5 page after the file finder closed", &native);

    // AC6: the command palette over a visible page.
    send_shortcut(
        &window,
        "P",
        35,
        NSEventModifierFlags::Command | NSEventModifierFlags::Shift,
        "cmd-shift-p",
    )?;
    sleep(cx, 1).await;
    probe.log(cx, "AC6 after cmd-shift-p in the page")?;
    log_visibility("AC6 page while the command palette is open", &native);
    let shot = if std::env::var("COD_SPIKE_KEEP_PAGE_OVER_MODALS").is_ok_and(|value| value == "1") {
        "t3-palette-over-page-unfixed.png"
    } else {
        "t3-palette-over-page.png"
    };
    window_shot(&window, shot, cx).await;
    send_shortcut(&window, ESCAPE, KEY_CODE_ESCAPE, NSEventModifierFlags::empty(), "escape")?;
    sleep(cx, 1).await;
    probe.log(cx, "AC6 after escape")?;
    log_visibility("AC6 page after the command palette closed", &native);

    // AC4: Cmd-W in the page closes the tab and removes the native view.
    click(&native, &field)?;
    sleep(cx, 1).await;
    probe.log(cx, "AC4 before cmd-w")?;
    log::info!(
        "spike driver: AC4 native view superview before cmd-w: {:?}",
        superview_name(&native_view)
    );
    drop(native);
    drop(page);
    send_shortcut(&window, "w", 13, NSEventModifierFlags::Command, "cmd-w")?;
    sleep(cx, 2).await;
    probe.log(cx, "AC4 after cmd-w")?;
    log::info!(
        "spike driver: AC4 native view superview after cmd-w: {:?}",
        superview_name(&native_view)
    );
    probe.log_editor_line(cx, "AC4")?;
    Ok(())
}

async fn edit_shortcuts(
    native: &NativeWebView,
    window: &NSWindow,
    probe: &Probe,
    cx: &mut AsyncWindowContext,
) -> Result<()> {
    let field = locate(native, TEXT_FIELD_SCRIPT)
        .await?
        .context("no textarea on the page")?;
    click(native, &field)?;
    sleep(cx, 1).await;
    probe.log(cx, "AC5 after clicking the page again")?;
    log::info!(
        "spike driver: AC5 selected {}",
        evaluate(native, SELECT_FIRST_WORD_SCRIPT).await?
    );
    send_shortcut(window, "c", 8, NSEventModifierFlags::Command, "cmd-c")?;
    sleep(cx, 1).await;
    log::info!(
        "spike driver: AC5 pasteboard after cmd-c: {:?}",
        pasteboard_string()
    );
    evaluate(native, CARET_TO_END_SCRIPT).await?;
    send_shortcut(window, "v", 9, NSEventModifierFlags::Command, "cmd-v")?;
    sleep(cx, 1).await;
    log_state(native, "AC5 page after cmd-v").await?;
    send_shortcut(window, "a", 0, NSEventModifierFlags::Command, "cmd-a")?;
    sleep(cx, 1).await;
    log::info!(
        "spike driver: AC5 selection after cmd-a: {}",
        evaluate(native, SELECTION_SCRIPT).await?
    );
    probe.log(cx, "AC5 after the edit shortcuts")?;
    Ok(())
}

/// Sends a key press the way AppKit routes it for the key window: first as a key equivalent through the view tree (where GPUI matches Zed's keymap), then through the main menu, and only then as keyDown to the first responder.
/// The app runs in the background so it never takes the user's keyboard, which leaves AppKit without a key window; in that case the same order is followed by hand.
fn send_shortcut(
    window: &NSWindow,
    characters: &str,
    key_code: u16,
    modifiers: NSEventModifierFlags,
    label: &str,
) -> Result<()> {
    let mtm = MainThreadMarker::new().context("not on the main thread")?;
    let app = NSApplication::sharedApplication(mtm);
    let characters = NSString::from_str(characters);
    let key_event = |event_type| {
        NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            event_type,
            CGPoint::ZERO,
            modifiers,
            0.0,
            window.windowNumber(),
            None,
            &characters,
            &characters,
            false,
            key_code,
        )
        .context("creating a key event")
    };
    let key_down = key_event(NSEventType::KeyDown)?;
    let route = if window.isKeyWindow() {
        app.sendEvent(&key_down);
        "NSApp sendEvent"
    } else {
        let content_view = window.contentView().context("no content view")?;
        if content_view.performKeyEquivalent(&key_down) {
            "key equivalent, handled by the view tree"
        } else if app
            .mainMenu()
            .is_some_and(|menu| menu.performKeyEquivalent(&key_down))
        {
            "key equivalent, handled by the main menu"
        } else {
            window.sendEvent(&key_down);
            "keyDown to the first responder"
        }
    };
    let key_up = key_event(NSEventType::KeyUp)?;
    window.sendEvent(&key_up);
    log::info!(
        "spike driver: sent {label} ({route}); first_responder={:?}",
        first_responder_name(window)
    );
    Ok(())
}

fn superview_name(view: &NSView) -> Option<String> {
    // SAFETY: called on the main thread; the superview is only read for its class name.
    unsafe { view.superview() }
        .map(|view| view.class().name().to_string_lossy().into_owned())
}

type SavedPasteboard = Vec<Vec<(Retained<NSString>, Retained<NSData>)>>;

/// The page's copy writes to the system pasteboard, so the driver keeps the user's content and puts it back.
fn save_pasteboard() -> SavedPasteboard {
    let pasteboard = NSPasteboard::generalPasteboard();
    let Some(items) = pasteboard.pasteboardItems() else {
        return Vec::new();
    };
    items
        .iter()
        .map(|item| {
            item.types()
                .iter()
                .filter_map(|data_type| {
                    let data = item.dataForType(&data_type)?;
                    Some((data_type, data))
                })
                .collect()
        })
        .collect()
}

fn restore_pasteboard(saved: &SavedPasteboard) {
    let pasteboard = NSPasteboard::generalPasteboard();
    pasteboard.clearContents();
    if saved.is_empty() {
        return;
    }
    let items: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = saved
        .iter()
        .map(|entries| {
            let item = NSPasteboardItem::new();
            for (data_type, data) in entries {
                item.setData_forType(data, data_type);
            }
            ProtocolObject::from_retained(item)
        })
        .collect();
    pasteboard.writeObjects(&NSArray::from_retained_slice(&items));
}

fn pasteboard_string() -> Option<String> {
    // SAFETY: NSPasteboardTypeString is a constant provided by AppKit.
    let string_type = unsafe { NSPasteboardTypeString };
    NSPasteboard::generalPasteboard()
        .stringForType(string_type)
        .map(|string| string.to_string())
}
