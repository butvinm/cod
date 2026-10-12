//! Spike-only driver, enabled with COD_SPIKE_DRIVE=1 together with COD_SPIKE_URL.
//!
//! It proves that input reaches the OS web view through AppKit without touching the user's keyboard or mouse: synthetic NSEvents are sent to the app's own NSWindow with `sendEvent:`, never posted to the system event stream.
//! Page state is read back with JavaScript and logged, and WKWebView snapshots are saved as PNG files under /tmp/cod-spike.

use std::{cell::Cell, rc::Rc, sync::Mutex, time::Duration};

use anyhow::{Context as _, Result, bail};
use futures::channel::oneshot;
use gpui::{AsyncWindowContext, Context, Entity, WeakEntity, Window};
use objc2::rc::Retained;
use objc2_app_kit::{
    NSBitmapImageFileType, NSBitmapImageRep, NSEvent, NSEventModifierFlags, NSEventType, NSImage,
    NSView, NSWindow,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGDisplayBounds, CGEvent, CGEventField, CGMainDisplayID, CGScrollEventUnit,
};
use objc2_foundation::{NSDictionary, NSError, NSString};
use workspace::Workspace;
use wry::WebViewExtMacOS as _;

use crate::web_view_item::{NativeWebView, WebView, open_url};

const LOCAL_PAGE_URL: &str = "file:///tmp/cod-spike/textarea.html";
const SNAPSHOT_DIRECTORY: &str = "/tmp/cod-spike";

const STATE_SCRIPT: &str = r#"JSON.stringify({
  url: location.href,
  title: document.title,
  viewport: innerWidth + 'x' + innerHeight,
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
    log::info!(
        "spike driver: survey {}",
        evaluate(&gitlab_native, SURVEY_SCRIPT).await?
    );
    snapshot(&gitlab_native, "gitlab-mr.png").await?;

    log_state(&gitlab_native, "before scroll").await?;
    scroll(&gitlab_native, -600)?;
    sleep(cx, 2).await;
    log_state(&gitlab_native, "after scroll").await?;

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
    for event_type in [NSEventType::LeftMouseDown, NSEventType::LeftMouseUp] {
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
    log::info!(
        "spike driver: clicked {:?} at page ({:.0}, {:.0}) = window ({:.0}, {:.0})",
        target.label,
        target.x,
        target.y,
        location.x,
        location.y
    );
    Ok(())
}

fn scroll(native: &NativeWebView, delta_y: i32) -> Result<()> {
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
    window.sendEvent(&event);
    Ok(())
}

fn type_text(native: &NativeWebView, text: &str) -> Result<()> {
    let (_, window) = native_window(native)?;
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
    let first_responder = window
        .firstResponder()
        .map(|responder| responder.class().name().to_string_lossy().into_owned());
    log::info!("spike driver: typed {text:?}, first responder {first_responder:?}");
    Ok(())
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
