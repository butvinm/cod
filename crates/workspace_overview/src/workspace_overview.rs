//! Cod (#26): the workspace overview. One shortcut opens a floating picker that lists every
//! workspace of the window, most urgent first, each with its folders, sessions and pages under
//! it. Typing filters the rows (a workspace row stays while one of its rows matches), Up and Down
//! move one row, Left and Right jump between workspace rows, Enter shows the row's workspace and
//! focuses the row's item, Escape or a click outside closes it.
//!
//! The rows are read when the picker opens: workspaces and their folders from the window, the
//! sessions and pages from the status store (`workspace::CodStatusStore`).

use std::{
    cmp::Reverse,
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use fuzzy_nucleo::{Case, LengthPenalty, StringMatchCandidate, match_strings};
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, Render, Task,
    WeakEntity, Window, actions, rems,
};
use picker::{Picker, PickerDelegate};
use project_panel::ProjectPanel;
use title_bar::cod_workspace_tabs::{split_workspace_name, tab_display_name};
use ui::{HighlightedLabel, Indicator, ListItem, ListItemSpacing, prelude::*};
use util::paths::PathExt as _;
use workspace::{
    CodEntry, CodEntryKind, CodEntryOpen, CodMember, CodStatus, CodStatusStore, ModalView,
    MultiWorkspace, Workspace, cod_rollup,
};

/// The width of the overview.
const PANEL_WIDTH_REMS: f32 = 34.;

/// The placeholder of the overview's query.
pub const PLACEHOLDER: &str = "Go to session...";

actions!(
    workspace_overview,
    [
        /// Opens the workspace overview; while it is open, selects the next row.
        Toggle,
        /// Selects the next workspace row of the overview.
        SelectNextWorkspace,
        /// Selects the previous workspace row of the overview.
        SelectPreviousWorkspace,
    ]
);

/// Registers the overview on every workspace.
pub fn init(cx: &mut App) {
    cx.observe_new(WorkspaceOverview::register).detach();
}

/// What a row stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    Workspace,
    Folder,
    Session,
    Page,
}

impl RowKind {
    /// The name used by the end-to-end driver.
    pub fn name(self) -> &'static str {
        match self {
            RowKind::Workspace => "workspace",
            RowKind::Folder => "folder",
            RowKind::Session => "session",
            RowKind::Page => "page",
        }
    }
}

/// What Enter does on a row, after it showed the row's workspace.
#[derive(Clone)]
pub enum RowTarget {
    /// Nothing more: the workspace's last focused item has the focus.
    Workspace,
    /// Focuses the root of this folder in the project panel.
    Folder(PathBuf),
    /// Runs the entry's open callback, when it has one.
    Entry(Option<CodEntryOpen>),
}

/// One row of the overview: a name and a muted second line.
#[derive(Clone)]
pub struct OverviewRow {
    pub kind: RowKind,
    /// The index of the row's workspace in the window's members.
    pub group: usize,
    pub title: String,
    pub detail: String,
    /// The dot shows when the status is not idle.
    pub status: CodStatus,
    pub target: RowTarget,
}

/// What the overview needs to know about one workspace.
pub struct OverviewGroup {
    pub name: String,
    pub folders: Vec<PathBuf>,
    /// When the workspace was last shown; None when it never was in this run.
    pub last_activated: Option<u64>,
    pub entries: Vec<CodEntry>,
}

/// A row that the query keeps, with the matched character positions of its two lines.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RowMatch {
    pub row: usize,
    pub title_positions: Vec<usize>,
    pub detail_positions: Vec<usize>,
}

/// The second line of a folder, session or page: "[ID] - context", or whichever of the two
/// there is.
pub fn with_id(id: Option<&str>, context: &str) -> String {
    match (id, context.is_empty()) {
        (Some(id), true) => id.to_string(),
        (Some(id), false) => format!("{id} - {context}"),
        (None, _) => context.to_string(),
    }
}

fn display_path(path: &Path) -> String {
    path.compact().to_string_lossy().into_owned()
}

fn folder_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| display_path(path))
}

/// The rows of all workspaces: workspaces in order of urgency (the most urgent status of their
/// entries, then the most recently shown first; equal ones keep window order), each followed by
/// its folders, then its sessions and pages in the same order of urgency (then the most recently
/// updated first).
pub fn build_rows(groups: &[OverviewGroup]) -> Vec<OverviewRow> {
    let group_status =
        |group: &OverviewGroup| cod_rollup(group.entries.iter().map(|entry| entry.status));
    let mut order: Vec<usize> = (0..groups.len()).collect();
    order.sort_by_key(|&index| {
        let group = &groups[index];
        (group_status(group), Reverse(group.last_activated))
    });

    let mut rows = Vec::new();
    for index in order {
        let group = &groups[index];
        let (id, _) = split_workspace_name(&group.name);
        rows.push(OverviewRow {
            kind: RowKind::Workspace,
            group: index,
            title: group.name.clone(),
            detail: group
                .folders
                .first()
                .map(|folder| display_path(folder))
                .unwrap_or_default(),
            status: group_status(group),
            target: RowTarget::Workspace,
        });
        for folder in &group.folders {
            rows.push(OverviewRow {
                kind: RowKind::Folder,
                group: index,
                title: folder_name(folder),
                detail: with_id(id, &display_path(folder)),
                status: CodStatus::Idle,
                target: RowTarget::Folder(folder.clone()),
            });
        }
        let mut entries: Vec<&CodEntry> = group.entries.iter().collect();
        entries.sort_by_key(|entry| (entry.status, Reverse(entry.updated_at)));
        for entry in entries {
            rows.push(OverviewRow {
                kind: match entry.kind {
                    CodEntryKind::Session => RowKind::Session,
                    CodEntryKind::Page => RowKind::Page,
                },
                group: index,
                title: entry.title.to_string(),
                detail: with_id(id, &entry.context),
                status: entry.status,
                target: RowTarget::Entry(entry.open.clone()),
            });
        }
    }
    rows
}

fn split_positions(rows: &[OverviewRow], row: usize, positions: Vec<usize>) -> RowMatch {
    let title_len = rows[row].title.len();
    let (title_positions, detail_positions) = positions
        .into_iter()
        .filter(|position| *position != title_len)
        .partition::<Vec<usize>, _>(|position| *position < title_len);
    RowMatch {
        row,
        title_positions,
        detail_positions: detail_positions
            .into_iter()
            .map(|position| position - title_len - 1)
            .collect(),
    }
}

/// The rows the query keeps, in list order: a fuzzy match on the name and the second line (which
/// holds the ID and the context). A workspace row stays while it or one of its rows matches; an
/// empty query keeps every row.
pub fn filter_rows(rows: &[OverviewRow], query: &str) -> Vec<RowMatch> {
    let query = query.trim();
    if query.is_empty() {
        return (0..rows.len())
            .map(|row| RowMatch {
                row,
                ..RowMatch::default()
            })
            .collect();
    }
    let candidates: Vec<StringMatchCandidate> = rows
        .iter()
        .enumerate()
        .map(|(index, row)| StringMatchCandidate::new(index, format!("{} {}", row.title, row.detail)))
        .collect();
    let mut matched: HashMap<usize, Vec<usize>> = match_strings(
        &candidates,
        query,
        Case::smart_if_uppercase_in(query),
        LengthPenalty::On,
        rows.len().max(1),
    )
    .into_iter()
    .map(|found| (found.candidate_id, found.positions))
    .collect();

    let mut kept = Vec::new();
    let mut start = 0;
    while start < rows.len() {
        let end = rows[start + 1..]
            .iter()
            .position(|row| row.kind == RowKind::Workspace)
            .map_or(rows.len(), |offset| start + 1 + offset);
        let children: Vec<RowMatch> = (start + 1..end)
            .filter_map(|row| {
                matched
                    .remove(&row)
                    .map(|positions| split_positions(rows, row, positions))
            })
            .collect();
        let head = matched.remove(&start);
        if head.is_some() || !children.is_empty() {
            kept.push(split_positions(rows, start, head.unwrap_or_default()));
            kept.extend(children);
        }
        start = end;
    }
    kept
}

/// The list index of the first workspace row after `selected`.
pub fn next_workspace_row(
    rows: &[OverviewRow],
    matches: &[RowMatch],
    selected: usize,
) -> Option<usize> {
    matches
        .iter()
        .enumerate()
        .skip(selected + 1)
        .find(|(_, found)| rows[found.row].kind == RowKind::Workspace)
        .map(|(index, _)| index)
}

/// The list index of the last workspace row before `selected`.
pub fn previous_workspace_row(
    rows: &[OverviewRow],
    matches: &[RowMatch],
    selected: usize,
) -> Option<usize> {
    matches[..selected.min(matches.len())]
        .iter()
        .rposition(|found| rows[found.row].kind == RowKind::Workspace)
}

/// The members of the window and what the overview shows of each, in window order.
fn gather(
    multi_workspace: &Entity<MultiWorkspace>,
    cx: &App,
) -> (Vec<CodMember>, Vec<OverviewGroup>) {
    let multi_workspace = multi_workspace.read(cx);
    let store = CodStatusStore::try_global(cx);
    let members = multi_workspace.cod_shown_members(cx);
    let groups = members
        .iter()
        .map(|member| {
            let (name, folders) = match member {
                CodMember::Live(workspace) => {
                    let workspace = workspace.read(cx);
                    let folders: Vec<PathBuf> = workspace
                        .visible_worktrees(cx)
                        .map(|worktree| worktree.read(cx).abs_path().to_path_buf())
                        .collect();
                    (workspace::cod_workspace_name(workspace, cx), folders)
                }
                CodMember::Pending(id) => (
                    workspace::cod_workspace_name_by_id(*id, cx),
                    multi_workspace
                        .cod_pending_paths(*id)
                        .unwrap_or_default()
                        .to_vec(),
                ),
            };
            let folder_paths: Vec<&Path> = folders.iter().map(|folder| folder.as_path()).collect();
            let name = tab_display_name(name, &folder_paths);
            let id = MultiWorkspace::cod_member_id(member, cx);
            let entries = match (id, &store) {
                (Some(id), Some(store)) => store.read(cx).entries(id).to_vec(),
                _ => Vec::new(),
            };
            let last_activated = id.and_then(|id| multi_workspace.cod_last_activated(id, cx));
            OverviewGroup {
                name,
                folders,
                last_activated,
                entries,
            }
        })
        .collect();
    (members, groups)
}

/// Shows the member, then focuses what the row stands for.
fn activate(
    multi_workspace: WeakEntity<MultiWorkspace>,
    member: CodMember,
    target: RowTarget,
    window: &mut Window,
    cx: &mut App,
) {
    if multi_workspace
        .update(cx, |multi_workspace, cx| {
            multi_workspace.cod_show_member(member.clone(), window, cx)
        })
        .is_err()
    {
        return;
    }
    match target {
        RowTarget::Workspace | RowTarget::Entry(None) => {}
        RowTarget::Entry(Some(open)) => open(window, cx),
        RowTarget::Folder(path) => {
            // A pending workspace loads in the background; its folder rows only show it.
            if let CodMember::Live(workspace) = member {
                focus_folder_root(&workspace, &path, window, cx);
            }
        }
    }
}

/// Focuses the project panel and selects the root of the folder there.
fn focus_folder_root(
    workspace: &Entity<Workspace>,
    path: &Path,
    window: &mut Window,
    cx: &mut App,
) {
    workspace.update(cx, |workspace, cx| {
        let project = workspace.project().clone();
        let root = project
            .read(cx)
            .visible_worktrees(cx)
            .find(|worktree| {
                let abs_path = worktree.read(cx).abs_path();
                let abs_path: &Path = &abs_path;
                abs_path == path
            })
            .and_then(|worktree| worktree.read(cx).root_entry().map(|entry| entry.id));
        workspace.focus_panel::<ProjectPanel>(window, cx);
        if let Some(root) = root {
            project.update(cx, |_, cx| {
                cx.emit(project::Event::RevealInProjectPanel(root))
            });
        }
    });
}

/// The overview modal.
pub struct WorkspaceOverview {
    picker: Entity<Picker<OverviewDelegate>>,
}

impl ModalView for WorkspaceOverview {}

impl EventEmitter<DismissEvent> for WorkspaceOverview {}

impl Focusable for WorkspaceOverview {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.picker.focus_handle(cx)
    }
}

impl WorkspaceOverview {
    fn register(
        workspace: &mut Workspace,
        _window: Option<&mut Window>,
        _cx: &mut Context<Workspace>,
    ) {
        workspace.register_action(|workspace, _: &Toggle, window, cx| {
            if let Some(overview) = workspace.active_modal::<Self>(cx) {
                overview.update(cx, |overview, cx| {
                    overview
                        .picker
                        .update(cx, |picker, cx| picker.cycle_selection(window, cx))
                });
                return;
            }
            // The rows read every workspace of the window, this one included, so they are
            // read once this workspace is no longer being updated.
            let workspace = cx.entity();
            window.defer(cx, move |window, cx| Self::open(workspace, window, cx));
        });
    }

    fn open(workspace: Entity<Workspace>, window: &mut Window, cx: &mut App) {
        let Some(multi_workspace) = window.root::<MultiWorkspace>().flatten() else {
            return;
        };
        let (members, groups) = gather(&multi_workspace, cx);
        let rows = build_rows(&groups);
        let multi_workspace = multi_workspace.downgrade();
        workspace.update(cx, |workspace, cx| {
            workspace.toggle_modal(window, cx, move |window, cx| {
                let delegate = OverviewDelegate {
                    overview: cx.entity().downgrade(),
                    multi_workspace,
                    members,
                    rows,
                    matches: Vec::new(),
                    selected_index: 0,
                };
                let picker = cx.new(|cx| {
                    Picker::list(delegate, window, cx).initial_width(rems(PANEL_WIDTH_REMS))
                });
                WorkspaceOverview { picker }
            })
        });
    }

    /// The picker, read by the end-to-end driver.
    pub fn picker(&self) -> &Entity<Picker<OverviewDelegate>> {
        &self.picker
    }

    fn select_next_workspace(
        &mut self,
        _: &SelectNextWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.picker.update(cx, |picker, cx| {
            let delegate = &picker.delegate;
            if let Some(index) =
                next_workspace_row(&delegate.rows, &delegate.matches, delegate.selected_index)
            {
                picker.set_selected_index(index, None, true, window, cx);
                cx.notify();
            }
        });
    }

    fn select_previous_workspace(
        &mut self,
        _: &SelectPreviousWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.picker.update(cx, |picker, cx| {
            let delegate = &picker.delegate;
            if let Some(index) =
                previous_workspace_row(&delegate.rows, &delegate.matches, delegate.selected_index)
            {
                picker.set_selected_index(index, None, true, window, cx);
                cx.notify();
            }
        });
    }
}

impl Render for WorkspaceOverview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("WorkspaceOverview")
            .w(rems(PANEL_WIDTH_REMS))
            .on_action(cx.listener(Self::select_next_workspace))
            .on_action(cx.listener(Self::select_previous_workspace))
            .child(self.picker.clone())
    }
}

/// The picker side of the overview.
pub struct OverviewDelegate {
    overview: WeakEntity<WorkspaceOverview>,
    multi_workspace: WeakEntity<MultiWorkspace>,
    members: Vec<CodMember>,
    rows: Vec<OverviewRow>,
    matches: Vec<RowMatch>,
    selected_index: usize,
}

impl OverviewDelegate {
    /// The rows the list shows, in order, read by the end-to-end driver.
    pub fn listed_rows(&self) -> Vec<&OverviewRow> {
        self.matches
            .iter()
            .filter_map(|found| self.rows.get(found.row))
            .collect()
    }
}

impl PickerDelegate for OverviewDelegate {
    type ListItem = ListItem;

    fn name() -> &'static str {
        "workspace overview"
    }

    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
        PLACEHOLDER.into()
    }

    fn no_matches_text(&self, _window: &mut Window, _cx: &mut App) -> Option<SharedString> {
        Some("No matches".into())
    }

    fn match_count(&self) -> usize {
        self.matches.len()
    }

    fn selected_index(&self) -> usize {
        self.selected_index
    }

    fn set_selected_index(
        &mut self,
        ix: usize,
        _window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) {
        self.selected_index = ix;
        cx.notify();
    }

    fn update_matches(
        &mut self,
        query: String,
        _window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        self.matches = filter_rows(&self.rows, &query);
        self.selected_index = 0;
        cx.notify();
        Task::ready(())
    }

    fn confirm(&mut self, _secondary: bool, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some(row) = self
            .matches
            .get(self.selected_index)
            .and_then(|found| self.rows.get(found.row))
            .cloned()
        else {
            return;
        };
        let Some(member) = self.members.get(row.group).cloned() else {
            return;
        };
        let multi_workspace = self.multi_workspace.clone();
        // The overview closes first, so the focus it gives back is replaced by the row's item.
        self.overview
            .update(cx, |_, cx| cx.emit(DismissEvent))
            .ok();
        window.defer(cx, move |window, cx| {
            activate(multi_workspace, member, row.target, window, cx)
        });
    }

    fn dismissed(&mut self, _window: &mut Window, cx: &mut Context<Picker<Self>>) {
        self.overview
            .update(cx, |_, cx| cx.emit(DismissEvent))
            .ok();
    }

    fn render_match(
        &self,
        ix: usize,
        selected: bool,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> Option<Self::ListItem> {
        let found = self.matches.get(ix)?;
        let row = self.rows.get(found.row)?;
        let icon = match row.kind {
            RowKind::Workspace => IconName::Folder,
            RowKind::Folder => IconName::FileTree,
            RowKind::Session => IconName::ZedAssistant,
            RowKind::Page => IconName::Public,
        };
        let dot = row
            .status
            .color()
            .map(|color| div().flex_none().child(Indicator::dot().color(color)));
        let lines = v_flex()
            .min_w_0()
            .child(HighlightedLabel::new(row.title.clone(), found.title_positions.clone()).truncate())
            .when(!row.detail.is_empty(), |lines| {
                lines.child(
                    HighlightedLabel::new(row.detail.clone(), found.detail_positions.clone())
                        .color(Color::Muted)
                        .size(LabelSize::Small)
                        .truncate(),
                )
            });
        Some(
            ListItem::new(ix)
                .spacing(ListItemSpacing::Sparse)
                .inset(true)
                .toggle_state(selected)
                .when(row.kind != RowKind::Workspace, |item| item.indent_level(1))
                .start_slot::<Icon>(Icon::new(icon).color(Color::Muted))
                .child(h_flex().w_full().min_w_0().overflow_hidden().child(lines))
                .end_slot::<Div>(dot),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::*;

    fn entry(key: &str, title: &str, status: CodStatus, context: &str, age: u64) -> CodEntry {
        CodEntry {
            key: key.to_string().into(),
            kind: CodEntryKind::Session,
            title: title.to_string().into(),
            status,
            context: context.to_string().into(),
            updated_at: SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 - age),
            open: None,
        }
    }

    fn fixtures() -> Vec<OverviewGroup> {
        vec![
            OverviewGroup {
                name: "[DOC-208] (docs-site) Move the API section".into(),
                folders: vec![],
                last_activated: Some(3),
                entries: vec![],
            },
            OverviewGroup {
                name: "[OPS-77] (infra) Upgrade Postgres to 17".into(),
                folders: vec![],
                last_activated: Some(2),
                entries: vec![entry(
                    "o1",
                    "Codex: plan Postgres 17 upgrade",
                    CodStatus::Error,
                    "failed",
                    5,
                )],
            },
            OverviewGroup {
                name: "[PAY-1234] (billing-api) Fix invoice rounding".into(),
                folders: vec![PathBuf::from("/srv/work/billing-api")],
                last_activated: Some(1),
                entries: vec![
                    entry("p3", "Claude: add tax tests", CodStatus::Done, "done, 6 tests added", 3),
                    entry("p2", "Codex: review migration", CodStatus::Working, "", 2),
                    entry(
                        "p1",
                        "Claude: fix invoice rounding",
                        CodStatus::NeedsInput,
                        "waiting for you",
                        1,
                    ),
                ],
            },
        ]
    }

    fn titles(rows: &[OverviewRow]) -> Vec<&str> {
        rows.iter().map(|row| row.title.as_str()).collect()
    }

    #[test]
    fn workspaces_and_entries_are_ordered_by_urgency() {
        let rows = build_rows(&fixtures());
        assert_eq!(
            titles(&rows),
            [
                "[PAY-1234] (billing-api) Fix invoice rounding",
                "billing-api",
                "Claude: fix invoice rounding",
                "Codex: review migration",
                "Claude: add tax tests",
                "[OPS-77] (infra) Upgrade Postgres to 17",
                "Codex: plan Postgres 17 upgrade",
                "[DOC-208] (docs-site) Move the API section",
            ]
        );
        assert_eq!(rows[0].detail, "/srv/work/billing-api");
        assert_eq!(rows[1].detail, "[PAY-1234] - /srv/work/billing-api");
        assert_eq!(rows[2].detail, "[PAY-1234] - waiting for you");
        assert_eq!(rows[3].detail, "[PAY-1234]");
        assert_eq!(rows[0].status, CodStatus::NeedsInput);
        assert_eq!(rows[7].status, CodStatus::Idle);
    }

    #[test]
    fn idle_workspaces_follow_the_last_shown_first() {
        let mut groups = fixtures();
        for group in &mut groups {
            group.entries.clear();
        }
        groups[0].last_activated = None;
        let rows = build_rows(&groups);
        let workspaces: Vec<&str> = rows
            .iter()
            .filter(|row| row.kind == RowKind::Workspace)
            .map(|row| row.title.as_str())
            .collect();
        assert_eq!(
            workspaces,
            [
                "[OPS-77] (infra) Upgrade Postgres to 17",
                "[PAY-1234] (billing-api) Fix invoice rounding",
                "[DOC-208] (docs-site) Move the API section",
            ]
        );
    }

    #[test]
    fn a_filter_keeps_matches_under_their_workspace() {
        let rows = build_rows(&fixtures());
        let kept: Vec<&str> = filter_rows(&rows, "postgres")
            .iter()
            .map(|found| rows[found.row].title.as_str())
            .collect();
        assert_eq!(
            kept,
            [
                "[OPS-77] (infra) Upgrade Postgres to 17",
                "Codex: plan Postgres 17 upgrade",
            ]
        );
        let kept: Vec<&str> = filter_rows(&rows, "tax tests")
            .iter()
            .map(|found| rows[found.row].title.as_str())
            .collect();
        assert_eq!(
            kept,
            ["[PAY-1234] (billing-api) Fix invoice rounding", "Claude: add tax tests"]
        );
        assert_eq!(filter_rows(&rows, " ").len(), rows.len());
    }

    #[test]
    fn left_and_right_jump_between_workspace_rows() {
        let rows = build_rows(&fixtures());
        let matches = filter_rows(&rows, "");
        assert_eq!(next_workspace_row(&rows, &matches, 0), Some(5));
        assert_eq!(next_workspace_row(&rows, &matches, 2), Some(5));
        assert_eq!(next_workspace_row(&rows, &matches, 5), Some(7));
        assert_eq!(next_workspace_row(&rows, &matches, 7), None);
        assert_eq!(previous_workspace_row(&rows, &matches, 7), Some(5));
        assert_eq!(previous_workspace_row(&rows, &matches, 6), Some(5));
        assert_eq!(previous_workspace_row(&rows, &matches, 5), Some(0));
        assert_eq!(previous_workspace_row(&rows, &matches, 0), None);
    }

    #[test]
    fn the_second_line_joins_the_id_and_the_context() {
        assert_eq!(with_id(Some("[OPS-77]"), "failed"), "[OPS-77] - failed");
        assert_eq!(with_id(Some("[OPS-77]"), ""), "[OPS-77]");
        assert_eq!(with_id(None, "waiting for you"), "waiting for you");
    }
}
