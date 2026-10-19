//! Cod (#25): the workspaces of the window as quiet tabs in the title bar, in window order, each
//! with a status dot only when something is happening, followed by a "+" that opens a new
//! workspace. Clicking a tab shows its workspace (a pending one loads first).
//!
//! Names follow "[ISSUE-ID] title" (D27): the leading "[ID]" is always shown whole and only the
//! rest is cut with "...". The full name is the tab's tooltip.

use std::{cell::RefCell, path::Path, rc::Rc, time::Duration};

use gpui::{Animation, AnimationExt as _, Bounds, Entity, WeakEntity, pulsating_between};
use ui::{Indicator, Tooltip, prelude::*};
use workspace::{CodMember, CodStatus, MultiWorkspace, NewWorkspace};

/// The widest a tab gets, in logical pixels (#25 open question 1).
pub const COD_TAB_MAX_WIDTH: f32 = 280.;

/// The name of a workspace without a name and without folders.
pub const COD_UNNAMED_WORKSPACE: &str = "New workspace";

/// One tab as it was last drawn, for the end-to-end driver.
#[derive(Clone, Debug, Default)]
pub struct CodTabInfo {
    /// The name the tab stands for (the user's name, else its folders, else "New workspace").
    pub name: String,
    pub active: bool,
    /// The status of the workspace; the dot shows when it is not idle.
    pub status: Option<CodStatus>,
    /// The text of the tab's tooltip: the full name.
    pub tooltip: String,
    /// Where the tab was drawn, None before it was drawn.
    pub bounds: Option<Bounds<Pixels>>,
}

impl CodTabInfo {
    /// Whether the name is cut: the tab is as wide as a tab gets.
    pub fn truncated(&self) -> bool {
        self.bounds
            .is_some_and(|bounds| f32::from(bounds.size.width) >= COD_TAB_MAX_WIDTH - 0.5)
    }
}

/// The tabs and the "+" button as they were last drawn.
#[derive(Clone, Debug, Default)]
pub struct CodTabsLayout {
    pub tabs: Vec<CodTabInfo>,
    pub add_button: Option<Bounds<Pixels>>,
}

/// Splits a name into its leading "[ID]" and the rest (D27). Names without a leading
/// "[...]" have no ID.
pub fn split_workspace_name(name: &str) -> (Option<&str>, &str) {
    match name.find(']').filter(|_| name.starts_with('[')) {
        Some(end) => {
            let (id, rest) = name.split_at(end + 1);
            (Some(id), rest.trim_start())
        }
        None => (None, name),
    }
}

/// The name a tab shows: the user's name, else the folder names joined by ", ", else
/// "New workspace".
pub fn tab_display_name(name: Option<String>, folders: &[&Path]) -> String {
    if let Some(name) = name.filter(|name| !name.trim().is_empty()) {
        return name;
    }
    let folders: Vec<String> = folders
        .iter()
        .filter_map(|folder| folder.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .collect();
    if folders.is_empty() {
        COD_UNNAMED_WORKSPACE.to_string()
    } else {
        folders.join(", ")
    }
}

fn member_name(member: &CodMember, multi_workspace: &MultiWorkspace, cx: &App) -> String {
    match member {
        CodMember::Live(workspace) => {
            let workspace = workspace.read(cx);
            let folders: Vec<std::sync::Arc<Path>> = workspace
                .visible_worktrees(cx)
                .map(|worktree| worktree.read(cx).abs_path())
                .collect();
            let folders: Vec<&Path> = folders.iter().map(|folder| folder.as_ref()).collect();
            tab_display_name(workspace::cod_workspace_name(workspace, cx), &folders)
        }
        CodMember::Pending(id) => {
            let folders: Vec<&Path> = multi_workspace
                .cod_pending_paths(*id)
                .unwrap_or_default()
                .iter()
                .map(|path| path.as_path())
                .collect();
            tab_display_name(workspace::cod_workspace_name_by_id(*id, cx), &folders)
        }
    }
}

fn status_dot(status: CodStatus, index: usize) -> Option<AnyElement> {
    let color = status.color()?;
    let dot = div().flex_none().child(Indicator::dot().color(color));
    Some(if status.pulses() {
        dot.with_animation(
            ("cod-workspace-tab-dot", index),
            Animation::new(Duration::from_secs(2))
                .repeat()
                .with_easing(pulsating_between(0.3, 1.0)),
            |dot, delta| dot.opacity(delta),
        )
        .into_any_element()
    } else {
        dot.into_any_element()
    })
}

/// The tabs of the window's workspaces and the "+" button. `layout` receives what was drawn.
pub fn render_cod_workspace_tabs(
    multi_workspace: &Entity<MultiWorkspace>,
    layout: Rc<RefCell<CodTabsLayout>>,
    cx: &mut App,
) -> AnyElement {
    let weak: WeakEntity<MultiWorkspace> = multi_workspace.downgrade();
    let multi_workspace = multi_workspace.read(cx);
    let active = multi_workspace.workspace().clone();
    let members = multi_workspace.cod_shown_members(cx);
    let colors = cx.theme().colors();
    let (active_text, active_bg, border) = (
        colors.text,
        colors.tab_active_background,
        colors.border,
    );
    let (inactive_text, hover_bg) = (colors.text_muted, colors.ghost_element_hover);

    let mut infos = Vec::with_capacity(members.len());
    let mut tabs: Vec<AnyElement> = Vec::with_capacity(members.len() + 1);
    for (index, member) in members.into_iter().enumerate() {
        let name = member_name(&member, multi_workspace, cx);
        let is_active = matches!(&member, CodMember::Live(workspace) if *workspace == active);
        let status = MultiWorkspace::cod_member_id(&member, cx)
            .map(|id| workspace::cod_workspace_status(id, cx))
            .unwrap_or(CodStatus::Idle);
        infos.push(CodTabInfo {
            name: name.clone(),
            active: is_active,
            status: Some(status),
            tooltip: name.clone(),
            bounds: None,
        });

        let (id, rest) = split_workspace_name(&name);
        let text_color = if is_active { active_text } else { inactive_text };
        let weak = weak.clone();
        let tab = h_flex()
            .id(("cod-workspace-tab", index))
            .h_full()
            .min_w_0()
            .max_w(px(COD_TAB_MAX_WIDTH))
            .px_2()
            .gap_1()
            .border_x_1()
            .text_color(text_color)
            .when(is_active, |tab| tab.bg(active_bg).border_color(border))
            .when(!is_active, |tab| {
                tab.border_color(gpui::transparent_black())
                    .hover(move |style| style.bg(hover_bg))
            })
            .cursor_pointer()
            .children(status_dot(status, index))
            .when_some(id, |tab, id| {
                tab.child(
                    div()
                        .flex_none()
                        .child(Label::new(id.to_string()).color(Color::Custom(text_color))),
                )
            })
            .when(!rest.is_empty(), |tab| {
                tab.child(
                    div().min_w_0().overflow_x_hidden().child(
                        Label::new(rest.to_string())
                            .color(Color::Custom(text_color))
                            .truncate(),
                    ),
                )
            })
            .tooltip(Tooltip::text(name.clone()))
            .on_click(move |_, window, cx| {
                weak.update(cx, |multi_workspace, cx| {
                    multi_workspace.cod_show_member(member.clone(), window, cx)
                })
                .ok();
            });
        tabs.push(tab.into_any_element());
    }
    tabs.push(
        IconButton::new("cod-new-workspace", IconName::Plus)
            .icon_size(IconSize::Small)
            .tooltip(Tooltip::text("New Workspace"))
            .on_click(|_, window, cx| window.dispatch_action(Box::new(NewWorkspace), cx))
            .into_any_element(),
    );
    *layout.borrow_mut() = CodTabsLayout {
        tabs: infos,
        add_button: None,
    };

    h_flex()
        .h_full()
        .min_w_0()
        .overflow_x_hidden()
        .on_children_prepainted(move |bounds, _window, _cx| {
            let mut layout = layout.borrow_mut();
            let tab_count = layout.tabs.len();
            for (tab, bounds) in layout.tabs.iter_mut().zip(bounds.iter()) {
                tab.bounds = Some(*bounds);
            }
            layout.add_button = bounds.get(tab_count).copied();
        })
        .children(tabs)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_leading_id_is_split_off() {
        assert_eq!(
            split_workspace_name("[PAY-1234] (billing-api) Fix invoice rounding"),
            (Some("[PAY-1234]"), "(billing-api) Fix invoice rounding")
        );
        assert_eq!(split_workspace_name("[OPS-77]"), (Some("[OPS-77]"), ""));
        assert_eq!(
            split_workspace_name("Fix [PAY-1] later"),
            (None, "Fix [PAY-1] later")
        );
        assert_eq!(split_workspace_name("[open"), (None, "[open"));
    }

    #[test]
    fn unnamed_tabs_fall_back_to_folders_then_new_workspace() {
        assert_eq!(
            tab_display_name(Some("[DOC-208] Move".into()), &[Path::new("/w/docs")]),
            "[DOC-208] Move"
        );
        assert_eq!(
            tab_display_name(None, &[Path::new("/w/billing-api"), Path::new("/w/infra")]),
            "billing-api, infra"
        );
        assert_eq!(tab_display_name(Some("  ".into()), &[]), COD_UNNAMED_WORKSPACE);
        assert_eq!(tab_display_name(None, &[]), COD_UNNAMED_WORKSPACE);
    }
}
