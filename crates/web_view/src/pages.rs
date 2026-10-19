//! The registry of open page tabs (issue #31), for link routing (#32) and `codctl page` (#38, #43).
//!
//! Every page tab has a `PageId` such as `page-3`. IDs come from a counter kept in the key-value store, so they never repeat, and a restored page keeps the ID it had before the restart.

use std::collections::BTreeMap;

use db::kvp::KeyValueStore;
use gpui::{App, AppContext as _, Entity, Global, TaskExt as _, WeakEntity, Window};
use ui::SharedString;
use workspace::Workspace;

use crate::{
    browser_client::protocol,
    offscreen::{OffscreenView, PageState},
};

const NEXT_PAGE_KEY: &str = "cod_next_page_id";

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PageId(pub SharedString);

impl PageId {
    pub fn number(&self) -> Option<u64> {
        self.0.strip_prefix("page-")?.parse().ok()
    }
}

impl std::fmt::Display for PageId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Who made the page: the user or a link (`External`), or an agent through `codctl open --html` (#43).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageKind {
    External,
    Agent,
}

impl PageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::External => "external",
            Self::Agent => "agent",
        }
    }

    pub fn parse(text: &str) -> Self {
        match text {
            "agent" => Self::Agent,
            _ => Self::External,
        }
    }
}

/// What a script can read about a page (#38 `codctl page get`).
#[derive(Clone, Debug)]
pub struct PageInfo {
    pub id: PageId,
    pub url: SharedString,
    pub title: Option<SharedString>,
    pub state: PageState,
    pub kind: PageKind,
    pub workspace: Option<WeakEntity<Workspace>>,
}

#[derive(Default)]
struct Pages {
    next: Option<u64>,
    open: BTreeMap<PageId, WeakEntity<OffscreenView>>,
}

impl Global for Pages {}

fn pages(cx: &mut App) -> &mut Pages {
    cx.default_global::<Pages>()
}

/// A new page ID, never given out before on this machine.
pub(crate) fn allocate_page_id(cx: &mut App) -> PageId {
    let saved_next = || {
        let stored = KeyValueStore::global(cx)
            .read_kvp(NEXT_PAGE_KEY)
            .ok()
            .flatten()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(1);
        let highest_saved = crate::page_db::CodPagesDb::global(cx)
            .all_pages()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(_, _, page_id, _)| PageId(page_id.into()).number())
            .max()
            .unwrap_or(0);
        stored.max(highest_saved + 1)
    };
    let next = match cx.try_global::<Pages>().and_then(|pages| pages.next) {
        Some(next) => next,
        None => saved_next(),
    };
    pages(cx).next = Some(next + 1);
    let store = KeyValueStore::global(cx);
    cx.background_spawn(async move {
        store
            .write_kvp(NEXT_PAGE_KEY.to_string(), (next + 1).to_string())
            .await
    })
    .detach_and_log_err(cx);
    PageId(format!("page-{next}").into())
}

/// Records a page tab under its ID; a restored page passes the ID it was saved with.
pub(crate) fn register(id: PageId, view: &Entity<OffscreenView>, cx: &mut App) {
    if let Some(number) = id.number() {
        let pages = pages(cx);
        if pages.next.is_some_and(|next| next <= number) {
            pages.next = Some(number + 1);
        }
    }
    pages(cx).open.insert(id, view.downgrade());
}

pub(crate) fn unregister(id: &PageId, cx: &mut App) {
    if let Some(pages) = cx.try_global::<Pages>() {
        if !pages.open.contains_key(id) {
            return;
        }
    } else {
        return;
    }
    pages(cx).open.remove(id);
}

/// The open page with this ID.
pub fn find(id: &PageId, cx: &App) -> Option<Entity<OffscreenView>> {
    cx.try_global::<Pages>()?.open.get(id)?.upgrade()
}

/// Every open page tab, in ID order.
pub fn all(cx: &App) -> Vec<Entity<OffscreenView>> {
    cx.try_global::<Pages>()
        .map(|pages| pages.open.values().filter_map(|view| view.upgrade()).collect())
        .unwrap_or_default()
}

pub fn get(id: &PageId, cx: &App) -> Option<PageInfo> {
    Some(find(id, cx)?.read(cx).info())
}

/// The page tab of `workspace` that shows `url` now, for "focus the tab instead of opening a second one" (#32 AC4).
pub fn find_by_url(
    workspace: &Entity<Workspace>,
    url: &str,
    cx: &App,
) -> Option<Entity<OffscreenView>> {
    let workspace = workspace.downgrade();
    all(cx).into_iter().find(|view| {
        let view = view.read(cx);
        view.workspace() == Some(&workspace) && same_url(view.current_url(), url)
    })
}

/// URLs are compared after parsing, so `http://host` and `http://host/` match.
fn same_url(left: &str, right: &str) -> bool {
    match (url::Url::parse(left), url::Url::parse(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

/// Opens `url` as a page tab in the active pane of `workspace` and returns its ID.
pub fn open(
    workspace: &mut Workspace,
    url: String,
    kind: PageKind,
    window: &mut Window,
    cx: &mut gpui::Context<Workspace>,
) -> PageId {
    crate::offscreen::open_page_in_active_pane(workspace, url, kind, window, cx)
        .read(cx)
        .page_id()
        .clone()
}

/// Closes the page's tab. Returns false for an unknown ID.
pub fn close(id: &PageId, window: &mut Window, cx: &mut App) -> bool {
    let Some(view) = find(id, cx) else {
        return false;
    };
    let Some(workspace) = view.read(cx).workspace().and_then(|workspace| workspace.upgrade())
    else {
        return false;
    };
    let item_id = view.entity_id();
    workspace.update(cx, |workspace, cx| {
        for pane in workspace.panes().to_vec() {
            pane.update(cx, |pane, cx| {
                if pane.items().any(|item| item.item_id() == item_id) {
                    pane.close_item_by_id(
                        item_id,
                        workspace::SaveIntent::Skip,
                        window,
                        cx,
                    )
                    .detach_and_log_err(cx);
                }
            });
        }
    });
    true
}

fn navigate(id: &PageId, action: u8, cx: &mut App) -> bool {
    let Some(view) = find(id, cx) else {
        return false;
    };
    view.update(cx, |view, cx| view.navigate(action, cx));
    true
}

pub fn reload(id: &PageId, cx: &mut App) -> bool {
    navigate(id, protocol::NAVIGATE_RELOAD, cx)
}

pub fn back(id: &PageId, cx: &mut App) -> bool {
    navigate(id, protocol::NAVIGATE_BACK, cx)
}

pub fn forward(id: &PageId, cx: &mut App) -> bool {
    navigate(id, protocol::NAVIGATE_FORWARD, cx)
}

/// Saved page rows (item, workspace, page ID, URL), including pages of workspaces not restored yet.
pub fn saved_pages(cx: &App) -> Vec<(workspace::ItemId, i64, String, String)> {
    crate::page_db::CodPagesDb::global(cx)
        .all_pages()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_ids_carry_their_number() {
        assert_eq!(PageId("page-3".into()).number(), Some(3));
        assert_eq!(PageId("page-x".into()).number(), None);
        assert_eq!(PageId("3".into()).number(), None);
    }

    #[test]
    fn urls_match_after_parsing() {
        assert!(same_url("http://127.0.0.1:8000", "http://127.0.0.1:8000/"));
        assert!(!same_url(
            "http://127.0.0.1:8000/tracker/PAY-1234",
            "http://127.0.0.1:8000/tracker/PAY-1235"
        ));
    }

    #[test]
    fn page_kinds_round_trip() {
        assert_eq!(PageKind::parse(PageKind::Agent.as_str()), PageKind::Agent);
        assert_eq!(PageKind::parse(PageKind::External.as_str()), PageKind::External);
        assert_eq!(PageKind::parse("unknown"), PageKind::External);
    }
}
