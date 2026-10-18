//! Cod: keeps the expanded folders and the selected entry of each workspace's project panel across restarts.
//!
//! Entry ids do not survive a restart, so the state is stored as (worktree absolute path, path below the worktree) pairs in a scoped key-value store, keyed by the workspace's database id.
//! It is written with a short debounce after it changes and applied once when the panel of a restored workspace first lists its worktrees.
//! Saved entries that never appear before the worktrees finish their initial scan are dropped silently.

use std::{path::PathBuf, time::Duration};

use collections::HashMap;
use db::kvp::KeyValueStore;
use gpui::{App, Context, Task, Window};
use project::{Project, ProjectEntryId, WorktreeId};
use serde::{Deserialize, Serialize};
use util::{ResultExt as _, rel_path::RelPath};
use workspace::SelectedEntry;

use crate::ProjectPanel;

const COD_PANEL_STATE_SCOPE: &str = "CodProjectPanelState";
const SAVE_DEBOUNCE: Duration = Duration::from_millis(100);
const MAX_WORKTREE_WAITS: usize = 40;
const WORKTREE_WAIT: Duration = Duration::from_millis(250);

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct CodPanelState {
    expanded: Vec<CodPanelPath>,
    selected: Option<CodPanelPath>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct CodPanelPath {
    worktree: PathBuf,
    path: String,
}

#[derive(Default)]
pub(crate) enum CodRestore {
    /// The workspace has no database id yet, or the saved state was not read yet.
    #[default]
    Unloaded,
    /// Saved entries not applied yet. `scanned` is set once the worktrees finished their initial scan; the next pass drops what is still missing.
    Pending {
        state: CodPanelState,
        scanned: bool,
    },
    /// Restored (or nothing to restore): changes are saved from now on.
    Done,
}

/// What the panel last saved, to notice changes cheaply.
type RawPanelState = (
    Option<SelectedEntry>,
    HashMap<WorktreeId, Vec<ProjectEntryId>>,
);

#[derive(Default)]
pub(crate) struct CodPanelPersistence {
    restore: CodRestore,
    workspace_key: Option<String>,
    last_saved: Option<RawPanelState>,
    save_task: Option<Task<()>>,
    scan_task: Option<Task<()>>,
}

fn resolve(
    project: &Project,
    saved: &CodPanelPath,
    cx: &App,
) -> Option<(WorktreeId, ProjectEntryId, bool)> {
    let worktree_entity = project
        .visible_worktrees(cx)
        .find(|worktree| &*worktree.read(cx).abs_path() == saved.worktree.as_path())?;
    let worktree = worktree_entity.read(cx);
    let path = RelPath::from_unix_str(saved.path.as_str()).ok()?;
    let entry = worktree.entry_for_path(path)?;
    Some((worktree.id(), entry.id, entry.is_dir()))
}

fn describe(
    project: &Project,
    worktree_id: WorktreeId,
    entry_id: ProjectEntryId,
    cx: &App,
) -> Option<CodPanelPath> {
    let worktree_entity = project.worktree_for_id(worktree_id, cx)?;
    let worktree = worktree_entity.read(cx);
    let entry = worktree.entry_for_id(entry_id)?;
    Some(CodPanelPath {
        worktree: worktree.abs_path().to_path_buf(),
        path: entry.path.as_unix_str().to_string(),
    })
}

impl ProjectPanel {
    /// Called each time the visible entries were recomputed: reads the saved state once the workspace has a database id, then applies what the worktrees already list.
    pub(crate) fn cod_after_entries_update(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.cod_persistence.restore, CodRestore::Unloaded) {
            let Some(database_id) = self
                .workspace
                .upgrade()
                .and_then(|workspace| workspace.read(cx).database_id())
            else {
                return;
            };
            let key = i64::from(database_id).to_string();
            let saved = KeyValueStore::global(cx)
                .scoped(COD_PANEL_STATE_SCOPE)
                .read(&key)
                .log_err()
                .flatten()
                .and_then(|text| serde_json::from_str::<CodPanelState>(&text).log_err());
            self.cod_persistence.workspace_key = Some(key);
            match saved {
                Some(state) if state.selected.is_some() || !state.expanded.is_empty() => {
                    self.cod_persistence.restore = CodRestore::Pending {
                        state,
                        scanned: false,
                    };
                    self.cod_wait_for_scans(window, cx);
                }
                _ => {
                    self.cod_persistence.restore = CodRestore::Done;
                    return;
                }
            }
        }

        let (pending, scanned) = match &mut self.cod_persistence.restore {
            CodRestore::Pending { state, scanned } => (std::mem::take(state), *scanned),
            _ => return,
        };

        let mut to_expand = Vec::new();
        let mut still_missing = Vec::new();
        let mut selected = pending.selected;
        let mut new_selection = None;
        {
            let project = self.project.read(cx);
            for saved in pending.expanded {
                match resolve(project, &saved, cx) {
                    Some((worktree_id, entry_id, true))
                        if self.state.expanded_dir_ids.contains_key(&worktree_id) =>
                    {
                        to_expand.push((worktree_id, entry_id));
                    }
                    Some((_, _, false)) => {}
                    _ => still_missing.push(saved),
                }
            }
            if let Some(saved) = &selected
                && let Some((worktree_id, entry_id, _)) = resolve(project, saved, cx)
            {
                new_selection = Some(SelectedEntry {
                    worktree_id,
                    entry_id,
                });
            }
        }
        if new_selection.is_some() {
            self.selection = new_selection;
            selected = None;
        }

        let mut expanded_any = false;
        for (worktree_id, entry_id) in to_expand {
            let already = self
                .state
                .expanded_dir_ids
                .get(&worktree_id)
                .is_some_and(|ids| ids.binary_search(&entry_id).is_ok());
            if !already {
                self.expand_entry(worktree_id, entry_id, cx);
                expanded_any = true;
            }
        }

        self.cod_persistence.restore = if scanned || (still_missing.is_empty() && selected.is_none())
        {
            self.cod_persistence.scan_task = None;
            CodRestore::Done
        } else {
            CodRestore::Pending {
                state: CodPanelState {
                    expanded: still_missing,
                    selected,
                },
                scanned,
            }
        };

        if expanded_any {
            cx.defer_in(window, |this, window, cx| {
                this.update_visible_entries(None, false, false, window, cx);
                cx.notify();
            });
        }
        cx.notify();
    }

    /// Waits until every saved worktree is in the project and has finished its initial scan, then runs a last restore pass that drops what is still missing.
    fn cod_wait_for_scans(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cod_persistence.scan_task = Some(cx.spawn_in(window, async move |this, cx| {
            for _ in 0..MAX_WORKTREE_WAITS {
                let Ok((scans, all_present)) = this.update(cx, |this, cx| {
                    let project = this.project.read(cx);
                    let scans = project
                        .visible_worktrees(cx)
                        .filter_map(|worktree| {
                            worktree.read(cx).as_local().map(|local| local.scan_complete())
                        })
                        .collect::<Vec<_>>();
                    let all_present = match &this.cod_persistence.restore {
                        CodRestore::Pending { state, .. } => state
                            .expanded
                            .iter()
                            .chain(state.selected.iter())
                            .all(|saved| {
                                project.visible_worktrees(cx).any(|worktree| {
                                    &*worktree.read(cx).abs_path() == saved.worktree.as_path()
                                })
                            }),
                        _ => true,
                    };
                    (scans, all_present)
                }) else {
                    return;
                };
                futures::future::join_all(scans).await;
                if all_present {
                    break;
                }
                cx.background_executor().timer(WORKTREE_WAIT).await;
            }
            this.update_in(cx, |this, window, cx| {
                if let CodRestore::Pending { scanned, .. } = &mut this.cod_persistence.restore {
                    *scanned = true;
                    this.update_visible_entries(None, false, false, window, cx);
                }
            })
            .ok();
        }));
    }

    /// Called on render: when the expanded folders or the selection changed since the last save, writes them after a short debounce. The write runs on the database thread.
    pub(crate) fn cod_save_if_changed(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.cod_persistence.restore, CodRestore::Done) {
            return;
        }
        let Some(key) = self.cod_persistence.workspace_key.clone() else {
            return;
        };
        if self.state.expanded_dir_ids.is_empty() {
            return;
        }
        let raw: RawPanelState = (self.selection, self.state.expanded_dir_ids.clone());
        if self.cod_persistence.last_saved.as_ref() == Some(&raw) {
            return;
        }

        let state = {
            let project = self.project.read(cx);
            let mut expanded = Vec::new();
            for (worktree_id, entry_ids) in &raw.1 {
                for entry_id in entry_ids {
                    if let Some(path) = describe(project, *worktree_id, *entry_id, cx) {
                        expanded.push(path);
                    }
                }
            }
            expanded.sort_by(|a, b| (&a.worktree, &a.path).cmp(&(&b.worktree, &b.path)));
            let selected = raw
                .0
                .and_then(|selection| describe(project, selection.worktree_id, selection.entry_id, cx));
            CodPanelState { expanded, selected }
        };
        self.cod_persistence.last_saved = Some(raw);
        let Some(text) = serde_json::to_string(&state).log_err() else {
            return;
        };
        let kvp = KeyValueStore::global(cx);
        self.cod_persistence.save_task = Some(cx.spawn(async move |_, cx| {
            cx.background_executor().timer(SAVE_DEBOUNCE).await;
            kvp.scoped(COD_PANEL_STATE_SCOPE)
                .write(key, text)
                .await
                .log_err();
        }));
    }
}
