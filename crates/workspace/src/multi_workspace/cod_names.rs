//! Cod (#25, #77, #81): the name a workspace's tab shows. It lives here, next to the members, so
//! the title bar, the overview and the window's own prompts (closing a workspace) all name a
//! workspace the same way.

use std::path::{Path, PathBuf};

use gpui::{App, Entity};

use super::{CodMember, MultiWorkspace, cod_workspace_name, cod_workspace_name_by_id};
use crate::Workspace;

/// The name of a workspace without a name and without folders.
pub const COD_UNNAMED_WORKSPACE: &str = "New workspace";

/// Whether a workspace with this name and these folders falls back to the numbered "New
/// workspace" name (issue #77: a user must be able to tell two freshly-opened workspaces
/// apart). A workspace the user named, or one with a folder, is never in the numbering: renaming
/// it or adding a folder takes it out, and a name the user typed is never changed.
pub fn is_unnamed_default(name: Option<&str>, folders: &[&Path]) -> bool {
    name.map(str::trim).unwrap_or("").is_empty() && folders.is_empty()
}

/// The name a tab shows: the user's name, else the folder names joined by ", ", else "New
/// workspace" (issue #77: "New workspace 2", "New workspace 3", ... when several workspaces are
/// all unnamed and folderless, so they can be told apart; `unnamed_rank` is this workspace's
/// 1-based position among those, in tab order, found with `is_unnamed_default` and a running
/// count over the shown members, oldest (leftmost tab) first. Ignored when the workspace is
/// named or has a folder).
pub fn tab_display_name(name: Option<String>, folders: &[&Path], unnamed_rank: usize) -> String {
    if let Some(name) = name.filter(|name| !name.trim().is_empty()) {
        return name;
    }
    let folder_names: Vec<String> = folders
        .iter()
        .filter_map(|folder| folder.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .collect();
    if folder_names.is_empty() {
        if unnamed_rank <= 1 {
            COD_UNNAMED_WORKSPACE.to_string()
        } else {
            format!("{COD_UNNAMED_WORKSPACE} {unnamed_rank}")
        }
    } else {
        folder_names.join(", ")
    }
}

/// The raw stored name and folders of a member, before `tab_display_name` turns them into what a
/// tab shows.
pub fn member_name_and_folders(
    member: &CodMember,
    multi_workspace: &MultiWorkspace,
    cx: &App,
) -> (Option<String>, Vec<PathBuf>) {
    match member {
        CodMember::Live(workspace) => {
            let workspace = workspace.read(cx);
            let folders: Vec<PathBuf> = workspace
                .visible_worktrees(cx)
                .map(|worktree| worktree.read(cx).abs_path().to_path_buf())
                .collect();
            (cod_workspace_name(workspace, cx), folders)
        }
        CodMember::Pending(id) => {
            let folders = multi_workspace
                .cod_pending_paths(*id)
                .unwrap_or_default()
                .to_vec();
            (cod_workspace_name_by_id(*id, cx), folders)
        }
    }
}

/// What the tab of one member shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodShownName {
    /// The whole name, for example "[PAY-1234] Fix rounding" or "New workspace 2".
    pub name: String,
    /// The number of a numbered default name (2 for "New workspace 2"), None for every other
    /// name, including the first default one ("New workspace").
    pub number: Option<usize>,
}

impl MultiWorkspace {
    /// The names the tabs show for the shown members (`cod_shown_members`), in tab order. The
    /// unnamed, folderless members are numbered from the second one on.
    pub fn cod_shown_names(&self, cx: &App) -> Vec<(CodMember, CodShownName)> {
        let mut unnamed_count = 0usize;
        self.cod_shown_members(cx)
            .into_iter()
            .map(|member| {
                let (name, folders) = member_name_and_folders(&member, self, cx);
                let folder_refs: Vec<&Path> =
                    folders.iter().map(|folder| folder.as_path()).collect();
                let unnamed = is_unnamed_default(name.as_deref(), &folder_refs);
                if unnamed {
                    unnamed_count += 1;
                }
                let shown = CodShownName {
                    name: tab_display_name(name, &folder_refs, unnamed_count),
                    number: (unnamed && unnamed_count > 1).then_some(unnamed_count),
                };
                (member, shown)
            })
            .collect()
    }

    /// The name the tab of this loaded workspace shows, None when it has no tab.
    pub fn cod_shown_name(&self, workspace: &Entity<Workspace>, cx: &App) -> Option<String> {
        self.cod_shown_names(cx)
            .into_iter()
            .find_map(|(member, shown)| match member {
                CodMember::Live(live) if live == *workspace => Some(shown.name),
                _ => None,
            })
    }
}
