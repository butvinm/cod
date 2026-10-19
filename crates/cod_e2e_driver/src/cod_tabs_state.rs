//! The driver's part of story #25: the title bar state (workspace tabs, "+" and the Zed items it
//! renders), the status of each workspace, a mouse click at a window position, and the one test
//! fixture that writes app state, `set_entry`.
//!
//! - `{"cmd":"click","x":120.5,"y":16}` moves the mouse to the position (logical pixels from the
//!   window's top left), then presses and releases the left button there, as the platform layer
//!   delivers them (`Window::dispatch_event`). Reply `{"ok":true}`.
//! - `{"cmd":"set_entry","workspace_index":0,"key":"s1","kind":"session","title":"Claude: fix invoice rounding","status":"needs_input","context":"waiting for you"}`
//!   sets a session or page of the workspace at that index of `workspaces` in the status store
//!   (`workspace::CodStatusStore`), replacing the entry with the same key. `kind` is "session" or
//!   "page"; `status` is "needs_input", "error", "working", "done" or "idle". Reply `{"ok":true}`.
//!
//! `set_entry` is the one exception to "the driver never calls into the app": it stands in for
//! the scripted fake agent provider of #28 until that lands, and only writes the store the
//! providers write.

use std::time::{Duration, SystemTime};

use anyhow::{Context as _, Result, anyhow, bail};
use gpui::{
    App, AsyncApp, Bounds, Entity, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, PlatformInput, SharedString, point, px,
};
use serde::Deserialize;
use serde_json::{Value, json};
use title_bar::TitleBar;
use workspace::{
    CodEntry, CodEntryKind, CodMember, CodStatus, CodStatusStore, MultiWorkspace, Workspace,
};

/// The pause between the mouse events of one click, so the window draws in between as it does
/// for a person.
const CLICK_PAUSE: Duration = Duration::from_millis(30);

/// A parsed `set_entry` request.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SetEntry {
    pub workspace_index: usize,
    pub key: String,
    pub kind: String,
    pub title: String,
    pub status: String,
    #[serde(default)]
    pub context: String,
}

/// Checks a `set_entry` request: a non-empty key, a known kind and a known status.
pub fn check_set_entry(entry: &SetEntry) -> Result<()> {
    if entry.key.trim().is_empty() {
        bail!("set_entry needs a non-empty key");
    }
    entry_kind(&entry.kind)?;
    entry_status(&entry.status)?;
    Ok(())
}

fn entry_kind(kind: &str) -> Result<CodEntryKind> {
    match kind {
        "session" => Ok(CodEntryKind::Session),
        "page" => Ok(CodEntryKind::Page),
        other => Err(anyhow!("unknown entry kind {other:?}: use \"session\" or \"page\"")),
    }
}

fn entry_status(status: &str) -> Result<CodStatus> {
    CodStatus::from_name(status).ok_or_else(|| {
        anyhow!(
            "unknown status {status:?}: use \"needs_input\", \"error\", \"working\", \"done\" or \"idle\""
        )
    })
}

/// Checks the coordinates of a click: finite and not negative.
pub fn check_click(x: f32, y: f32) -> Result<()> {
    if !x.is_finite() || !y.is_finite() || x < 0. || y < 0. {
        bail!("click needs a position inside the window, got ({x}, {y})");
    }
    Ok(())
}

/// The status name of a member, as `workspaces[].status` reports it.
pub(crate) fn member_status(member: &CodMember, cx: &App) -> &'static str {
    MultiWorkspace::cod_member_id(member, cx)
        .map(|id| workspace::cod_workspace_status(id, cx))
        .unwrap_or(CodStatus::Idle)
        .name()
}

fn bounds_json(bounds: Option<Bounds<Pixels>>) -> Value {
    match bounds {
        Some(bounds) => json!({
            "x": f32::from(bounds.origin.x),
            "y": f32::from(bounds.origin.y),
            "width": f32::from(bounds.size.width),
            "height": f32::from(bounds.size.height),
        }),
        None => Value::Null,
    }
}

/// The title bar of the active workspace as it was last drawn: `{"tabs": [{"name", "active",
/// "dot", "pulsing", "tooltip", "truncated", "bounds"}], "add_button": {"bounds"}, "items": [...]}`,
/// or null when the workspace has no Cod title bar.
pub(crate) fn title_bar_state(workspace: &Entity<Workspace>, cx: &App) -> Value {
    let Some(title_bar) = workspace
        .read(cx)
        .titlebar_item()
        .and_then(|item| item.downcast::<TitleBar>().ok())
    else {
        return Value::Null;
    };
    let title_bar = title_bar.read(cx);
    let layout = title_bar.cod_tabs_layout();
    let tabs: Vec<Value> = layout
        .tabs
        .iter()
        .map(|tab| {
            let status = tab.status.unwrap_or(CodStatus::Idle);
            json!({
                "name": tab.name,
                "active": tab.active,
                "dot": status.dot_name(),
                "pulsing": status.pulses(),
                "tooltip": tab.tooltip,
                "truncated": tab.truncated(),
                "bounds": bounds_json(tab.bounds),
            })
        })
        .collect();
    json!({
        "tabs": tabs,
        "add_button": { "bounds": bounds_json(layout.add_button) },
        "items": title_bar.cod_visible_items(cx),
    })
}

/// Moves the mouse to the position, then presses and releases the left button there.
#[cfg(unix)]
pub(crate) async fn click(
    x: f32,
    y: f32,
    memory: &std::rc::Rc<std::cell::RefCell<super::Memory>>,
    cx: &mut AsyncApp,
) -> Result<()> {
    let position = point(px(x), px(y));
    let events = [
        PlatformInput::MouseMove(MouseMoveEvent {
            position,
            pressed_button: None,
            modifiers: Modifiers::default(),
        }),
        PlatformInput::MouseDown(MouseDownEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        }),
        PlatformInput::MouseUp(MouseUpEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::default(),
            click_count: 1,
        }),
    ];
    for (index, event) in events.into_iter().enumerate() {
        if index > 0 {
            cx.background_executor().timer(CLICK_PAUSE).await;
        }
        let window = cx
            .update(|cx| super::workspace_window(cx, &memory.borrow()))
            .context("Cod has no workspace window")?;
        cx.update_window(window.into(), |_, window, cx| {
            window.dispatch_event(event, cx);
        })
        .context("the workspace window closed")?;
    }
    Ok(())
}

/// Writes the entry into the status store, for the workspace at `workspace_index` of the
/// driver's `workspaces` list.
#[cfg(unix)]
pub(crate) fn set_entry(
    entry: SetEntry,
    memory: &std::rc::Rc<std::cell::RefCell<super::Memory>>,
    cx: &mut AsyncApp,
) -> Result<()> {
    let kind = entry_kind(&entry.kind)?;
    let status = entry_status(&entry.status)?;
    let window = cx
        .update(|cx| super::workspace_window(cx, &memory.borrow()))
        .context("Cod has no workspace window")?;
    let id = cx
        .update_window(window.into(), |root, _window, cx| {
            let multi_workspace = root
                .downcast::<MultiWorkspace>()
                .map_err(|_| anyhow!("the window does not show a workspace"))?;
            let multi_workspace = multi_workspace.read(cx);
            let members = multi_workspace.cod_shown_members(cx);
            let member = members.get(entry.workspace_index).ok_or_else(|| {
                anyhow!(
                    "no workspace at index {} (the window has {})",
                    entry.workspace_index,
                    members.len()
                )
            })?;
            MultiWorkspace::cod_member_id(member, cx)
                .ok_or_else(|| anyhow!("workspace {} has no id yet", entry.workspace_index))
        })
        .context("the workspace window closed")??;
    cx.update(|cx| {
        let store = CodStatusStore::global(cx);
        store.update(cx, |store, cx| {
            store.set_entry(
                id,
                CodEntry {
                    key: SharedString::from(entry.key),
                    kind,
                    title: SharedString::from(entry.title),
                    status,
                    context: SharedString::from(entry.context),
                    updated_at: SystemTime::now(),
                    open: None,
                },
                cx,
            )
        });
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(kind: &str, status: &str, key: &str) -> SetEntry {
        SetEntry {
            workspace_index: 0,
            key: key.to_string(),
            kind: kind.to_string(),
            title: "Claude: fix invoice rounding".to_string(),
            status: status.to_string(),
            context: String::new(),
        }
    }

    #[test]
    fn accepts_known_kinds_and_statuses() {
        for status in ["needs_input", "error", "working", "done", "idle"] {
            check_set_entry(&entry("session", status, "s1")).unwrap();
        }
        check_set_entry(&entry("page", "idle", "p1")).unwrap();
    }

    #[test]
    fn refuses_unknown_kinds_statuses_and_empty_keys() {
        assert!(check_set_entry(&entry("chat", "idle", "s1")).is_err());
        assert!(check_set_entry(&entry("session", "busy", "s1")).is_err());
        assert!(check_set_entry(&entry("session", "idle", " ")).is_err());
    }

    #[test]
    fn refuses_clicks_outside_the_window() {
        check_click(10., 4.5).unwrap();
        assert!(check_click(-1., 4.).is_err());
        assert!(check_click(f32::NAN, 4.).is_err());
    }
}
