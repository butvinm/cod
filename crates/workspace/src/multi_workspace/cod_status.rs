//! Cod (#25, #26): what each workspace has going on. Every workspace (loaded or pending) can hold
//! entries: agent sessions and pages, each with a status. The title bar tabs show the most urgent
//! status of a workspace as a dot (#25) and the overview picker lists the entries (#26).
//!
//! The store is one global entity, keyed by workspace id, so a workspace that is not loaded yet
//! still reports its status. It notifies its observers on every change. Nothing is persisted:
//! the providers that own the sessions (#28) and pages (#31) fill it in again after a start.

use std::{collections::HashMap, rc::Rc, time::SystemTime};

use gpui::{App, AppContext as _, Context, Entity, Global, SharedString, Window};
use ui::Color;

use crate::WorkspaceId;

/// The status of one session or page. The variants are declared from the most urgent to the
/// least urgent, so the derived order sorts by urgency: the smallest value is the most urgent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CodStatus {
    /// The session waits for the user.
    NeedsInput,
    /// The session failed.
    Error,
    /// The session is working.
    Working,
    /// The session finished and the user has not looked at it yet.
    Done,
    /// Nothing is happening.
    Idle,
}

impl CodStatus {
    /// The theme color of the status dot, or None for idle (no dot).
    pub fn color(self) -> Option<Color> {
        match self {
            CodStatus::NeedsInput => Some(Color::Warning),
            CodStatus::Error => Some(Color::Error),
            CodStatus::Working => Some(Color::Info),
            CodStatus::Done => Some(Color::Success),
            CodStatus::Idle => None,
        }
    }

    /// Whether the status dot pulses.
    pub fn pulses(self) -> bool {
        self == CodStatus::Working
    }

    /// The name used by the end-to-end driver and in logs.
    pub fn name(self) -> &'static str {
        match self {
            CodStatus::NeedsInput => "needs_input",
            CodStatus::Error => "error",
            CodStatus::Working => "working",
            CodStatus::Done => "done",
            CodStatus::Idle => "idle",
        }
    }

    /// The status with this name (see `name`).
    pub fn from_name(name: &str) -> Option<Self> {
        [
            CodStatus::NeedsInput,
            CodStatus::Error,
            CodStatus::Working,
            CodStatus::Done,
            CodStatus::Idle,
        ]
        .into_iter()
        .find(|status| status.name() == name)
    }

    /// The color name of the dot, as the end-to-end driver reports it, or None for idle.
    pub fn dot_name(self) -> Option<&'static str> {
        match self {
            CodStatus::NeedsInput => Some("yellow"),
            CodStatus::Error => Some("red"),
            CodStatus::Working => Some("blue"),
            CodStatus::Done => Some("green"),
            CodStatus::Idle => None,
        }
    }
}

/// The most urgent status of these statuses; idle when there is none.
pub fn cod_rollup(statuses: impl IntoIterator<Item = CodStatus>) -> CodStatus {
    statuses.into_iter().min().unwrap_or(CodStatus::Idle)
}

/// What an entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodEntryKind {
    /// An agent session (chat).
    Session,
    /// A page (web view) of the workspace.
    Page,
}

/// Shows the entry: switches to its workspace and focuses it.
pub type CodEntryOpen = Rc<dyn Fn(&mut Window, &mut App)>;

/// One session or page of a workspace.
#[derive(Clone)]
pub struct CodEntry {
    /// The provider's session id for a session (never a made-up id), the page id for a page, or
    /// an internal key for a draft that has no id yet. Unique within a workspace.
    pub key: SharedString,
    pub kind: CodEntryKind,
    pub title: SharedString,
    pub status: CodStatus,
    /// A short line: the session's status text ("waiting for you") or the page's host and path.
    pub context: SharedString,
    pub updated_at: SystemTime,
    /// Shows the entry; None while it cannot be shown.
    pub open: Option<CodEntryOpen>,
}

/// The entries of every workspace (see the module comment).
#[derive(Default)]
pub struct CodStatusStore {
    entries: HashMap<WorkspaceId, Vec<CodEntry>>,
}

struct GlobalCodStatusStore(Entity<CodStatusStore>);

impl Global for GlobalCodStatusStore {}

impl CodStatusStore {
    /// The store, created on first use.
    pub fn global(cx: &mut App) -> Entity<Self> {
        if let Some(store) = cx.try_global::<GlobalCodStatusStore>() {
            return store.0.clone();
        }
        let store = cx.new(|_| CodStatusStore::default());
        cx.set_global(GlobalCodStatusStore(store.clone()));
        store
    }

    /// The store, None before anything created it.
    pub fn try_global(cx: &App) -> Option<Entity<Self>> {
        cx.try_global::<GlobalCodStatusStore>()
            .map(|store| store.0.clone())
    }

    /// Adds the entry to the workspace, or replaces the entry with the same key.
    pub fn set_entry(&mut self, workspace: WorkspaceId, entry: CodEntry, cx: &mut Context<Self>) {
        let entries = self.entries.entry(workspace).or_default();
        match entries.iter_mut().find(|existing| existing.key == entry.key) {
            Some(existing) => *existing = entry,
            None => entries.push(entry),
        }
        cx.notify();
    }

    /// Removes the entry with this key from the workspace.
    pub fn remove_entry(&mut self, workspace: WorkspaceId, key: &str, cx: &mut Context<Self>) {
        if let Some(entries) = self.entries.get_mut(&workspace) {
            let before = entries.len();
            entries.retain(|entry| entry.key.as_str() != key);
            if entries.len() != before {
                if entries.is_empty() {
                    self.entries.remove(&workspace);
                }
                cx.notify();
            }
        }
    }

    /// Drops every entry of a workspace that left its window for good.
    pub fn forget_workspace(&mut self, workspace: WorkspaceId, cx: &mut Context<Self>) {
        if self.entries.remove(&workspace).is_some() {
            cx.notify();
        }
    }

    /// The most urgent status of the workspace's entries; idle without entries.
    pub fn workspace_status(&self, workspace: WorkspaceId) -> CodStatus {
        cod_rollup(self.entries(workspace).iter().map(|entry| entry.status))
    }

    /// The entries of the workspace in the order they were added.
    pub fn entries(&self, workspace: WorkspaceId) -> &[CodEntry] {
        self.entries
            .get(&workspace)
            .map(|entries| entries.as_slice())
            .unwrap_or_default()
    }
}

/// The most urgent status of the workspace with this id; idle when nothing is known.
pub fn cod_workspace_status(workspace: WorkspaceId, cx: &App) -> CodStatus {
    CodStatusStore::try_global(cx)
        .map(|store| store.read(cx).workspace_status(workspace))
        .unwrap_or(CodStatus::Idle)
}

/// Drops the entries of a workspace that left its window for good (no-op before the store
/// exists).
pub(super) fn cod_forget_workspace_status(workspace: WorkspaceId, cx: &mut App) {
    if let Some(store) = CodStatusStore::try_global(cx) {
        store.update(cx, |store, cx| store.forget_workspace(workspace, cx));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_status_rolls_up_to_idle() {
        assert_eq!(cod_rollup([]), CodStatus::Idle);
        assert_eq!(cod_rollup([CodStatus::Idle]), CodStatus::Idle);
    }

    #[test]
    fn the_most_urgent_status_wins() {
        assert_eq!(
            cod_rollup([CodStatus::Done, CodStatus::Working, CodStatus::Idle]),
            CodStatus::Working
        );
        assert_eq!(
            cod_rollup([CodStatus::Working, CodStatus::Error, CodStatus::Done]),
            CodStatus::Error
        );
        assert_eq!(
            cod_rollup([CodStatus::Error, CodStatus::NeedsInput]),
            CodStatus::NeedsInput
        );
    }

    #[test]
    fn colors_follow_the_theme_status_colors() {
        assert_eq!(CodStatus::NeedsInput.color(), Some(Color::Warning));
        assert_eq!(CodStatus::Error.color(), Some(Color::Error));
        assert_eq!(CodStatus::Working.color(), Some(Color::Info));
        assert_eq!(CodStatus::Done.color(), Some(Color::Success));
        assert_eq!(CodStatus::Idle.color(), None);
        assert!(CodStatus::Working.pulses());
        assert!(!CodStatus::Done.pulses());
    }

    #[test]
    fn names_round_trip() {
        for status in [
            CodStatus::NeedsInput,
            CodStatus::Error,
            CodStatus::Working,
            CodStatus::Done,
            CodStatus::Idle,
        ] {
            assert_eq!(CodStatus::from_name(status.name()), Some(status));
        }
        assert_eq!(CodStatus::from_name("busy"), None);
    }

    #[gpui::test]
    fn the_store_rolls_up_per_workspace(cx: &mut gpui::TestAppContext) {
        let first = WorkspaceId::from_i64(1);
        let second = WorkspaceId::from_i64(2);
        let entry = |key: &str, status: CodStatus| CodEntry {
            key: SharedString::from(key.to_string()),
            kind: CodEntryKind::Session,
            title: SharedString::from(format!("Session {key}")),
            status,
            context: SharedString::default(),
            updated_at: SystemTime::UNIX_EPOCH,
            open: None,
        };
        let store = cx.update(CodStatusStore::global);
        store.update(cx, |store, cx| {
            store.set_entry(first, entry("a", CodStatus::Done), cx);
            store.set_entry(first, entry("b", CodStatus::NeedsInput), cx);
            store.set_entry(second, entry("c", CodStatus::Working), cx);
        });
        store.read_with(cx, |store, _| {
            assert_eq!(store.workspace_status(first), CodStatus::NeedsInput);
            assert_eq!(store.workspace_status(second), CodStatus::Working);
        });
        store.update(cx, |store, cx| {
            // The same key replaces the entry.
            store.set_entry(first, entry("b", CodStatus::Idle), cx);
            store.remove_entry(second, "c", cx);
        });
        store.read_with(cx, |store, _| {
            assert_eq!(store.entries(first).len(), 2);
            assert_eq!(store.workspace_status(first), CodStatus::Done);
            assert_eq!(store.workspace_status(second), CodStatus::Idle);
        });
        store.update(cx, |store, cx| store.forget_workspace(first, cx));
        store.read_with(cx, |store, _| {
            assert!(store.entries(first).is_empty());
        });
    }
}
