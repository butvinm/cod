//! A thin bridge from `codctl_server`'s `codctl page` and `codctl open <url>` commands (#38)
//! to the page registry of #31 and #32.
//!
//! Page tabs exist only on Linux, behind the off-screen browser backend (`pages`,
//! `#[cfg(target_os = "linux")]`). Every function here has the same signature on every
//! platform, so `codctl_server` never needs its own `#[cfg(target_os = "linux")]`: elsewhere
//! it simply reports that page tabs are not available, the same way a missing file or pane
//! would.

use anyhow::{Context as _, Result, bail};
use gpui::{App, Context, WeakEntity, Window};
use serde_json::{Value, json};
use workspace::Workspace;

/// Opens `url` as a page tab in the active pane of the workspace and returns its page ID (#38 AC3).
#[cfg(target_os = "linux")]
pub fn open(
    workspace: &mut Workspace,
    url: String,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Result<String> {
    Ok(crate::pages::open(workspace, url, crate::pages::PageKind::External, window, cx).to_string())
}

#[cfg(not(target_os = "linux"))]
pub fn open(
    _workspace: &mut Workspace,
    _url: String,
    _window: &mut Window,
    _cx: &mut Context<Workspace>,
) -> Result<String> {
    bail!("page tabs need Cod's Linux off-screen browser")
}

/// The page's current URL, title and state, as `codctl page get` reports them (#38 AC4).
#[cfg(target_os = "linux")]
pub fn get(page: &str, cx: &App) -> Result<Value> {
    let id = crate::pages::PageId(page.to_owned().into());
    let info = crate::pages::get(&id, cx).with_context(|| format!("unknown page {page}"))?;
    Ok(json!({
        "id": info.id.to_string(),
        "url": info.url.to_string(),
        "title": info.title.map(|title| title.to_string()),
        "state": info.state.as_str(),
    }))
}

#[cfg(not(target_os = "linux"))]
pub fn get(page: &str, _cx: &App) -> Result<Value> {
    let _ = page;
    bail!("page tabs need Cod's Linux off-screen browser")
}

/// Reloads the page tab (#38 AC5). An unknown page ID is an error.
#[cfg(target_os = "linux")]
pub fn reload(page: &str, cx: &mut App) -> Result<()> {
    navigate(page, cx, crate::pages::reload)
}

#[cfg(not(target_os = "linux"))]
pub fn reload(page: &str, _cx: &mut App) -> Result<()> {
    let _ = page;
    bail!("page tabs need Cod's Linux off-screen browser")
}

/// Goes back in the page tab's history (#38 AC5). An unknown page ID is an error.
#[cfg(target_os = "linux")]
pub fn back(page: &str, cx: &mut App) -> Result<()> {
    navigate(page, cx, crate::pages::back)
}

#[cfg(not(target_os = "linux"))]
pub fn back(page: &str, _cx: &mut App) -> Result<()> {
    let _ = page;
    bail!("page tabs need Cod's Linux off-screen browser")
}

/// Goes forward in the page tab's history (#38 AC5). An unknown page ID is an error.
#[cfg(target_os = "linux")]
pub fn forward(page: &str, cx: &mut App) -> Result<()> {
    navigate(page, cx, crate::pages::forward)
}

#[cfg(not(target_os = "linux"))]
pub fn forward(page: &str, _cx: &mut App) -> Result<()> {
    let _ = page;
    bail!("page tabs need Cod's Linux off-screen browser")
}

#[cfg(target_os = "linux")]
fn navigate(
    page: &str,
    cx: &mut App,
    action: fn(&crate::pages::PageId, &mut App) -> bool,
) -> Result<()> {
    let id = crate::pages::PageId(page.to_owned().into());
    if action(&id, cx) {
        Ok(())
    } else {
        bail!("unknown page {page}")
    }
}

/// The workspace that owns the page tab, so the caller can find its window before closing it.
#[cfg(target_os = "linux")]
pub fn workspace_of(page: &str, cx: &App) -> Option<WeakEntity<Workspace>> {
    let id = crate::pages::PageId(page.to_owned().into());
    crate::pages::find(&id, cx)?.read(cx).workspace().cloned()
}

#[cfg(not(target_os = "linux"))]
pub fn workspace_of(page: &str, _cx: &App) -> Option<WeakEntity<Workspace>> {
    let _ = page;
    None
}

/// Closes the page tab (#38 AC5). The caller must pass the window of the page's own workspace
/// (see `workspace_of`). An unknown page ID is an error.
#[cfg(target_os = "linux")]
pub fn close(page: &str, window: &mut Window, cx: &mut App) -> Result<()> {
    let id = crate::pages::PageId(page.to_owned().into());
    if crate::pages::close(&id, window, cx) {
        Ok(())
    } else {
        bail!("unknown page {page}")
    }
}

#[cfg(not(target_os = "linux"))]
pub fn close(page: &str, _window: &mut Window, _cx: &mut App) -> Result<()> {
    let _ = page;
    bail!("page tabs need Cod's Linux off-screen browser")
}
