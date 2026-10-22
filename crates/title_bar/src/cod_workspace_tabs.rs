//! Cod (#25): the workspaces of the window as quiet tabs in the title bar, in window order, each
//! with a status dot only when something is happening, followed by a "+" that opens a new
//! workspace. Clicking a tab shows its workspace (a pending one loads first).
//!
//! Names follow "[ISSUE-ID] title" (D27): the leading "[ID]" is always shown whole and only the
//! rest is cut with "...". The full name is the tab's tooltip.

use std::{cell::RefCell, path::Path, rc::Rc, time::Duration};

use gpui::{
    Animation, AnimationExt as _, Bounds, Entity, MouseButton, WeakEntity, pulsating_between,
};
use ui::{IconButtonShape, Indicator, Tooltip, prelude::*};
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
    /// The dot's color name ("yellow", "red", "blue", "green"), written only when the title bar
    /// actually prepainted the dot in the last frame; None otherwise (no dot, idle or not drawn
    /// at all). Set by the dot's own `on_children_prepainted`, not derived from the workspace's
    /// status, so removing the drawn dot from the tab turns this back to None on its own.
    pub dot: Option<&'static str>,
    /// Whether the dot's pulsing animation actually ran in the last frame. Set from inside the
    /// animation's own frame callback, so removing the animation (while keeping a static dot)
    /// turns this back to false on its own.
    pub pulsing: bool,
    /// The text of the tab's tooltip: the full name.
    pub tooltip: String,
    /// Where the tab was drawn, None before it was drawn.
    pub bounds: Option<Bounds<Pixels>>,
    /// Whether the last frame cut the name: the text after the "[ID]" was wider than the room
    /// it got. A tab is cut both at the maximum width and when the title bar is too narrow for
    /// all tabs at their full width.
    pub cut: bool,
    /// Where the tab's close button was drawn (issue #77 F3), None when the tab has none (a
    /// pending, not yet loaded, member). Its space is always reserved on a loaded tab, so this is
    /// set regardless of hover or active state; whether it is currently visible to a person is a
    /// paint-time style (`visible_on_hover`), not reported here.
    pub close_bounds: Option<Bounds<Pixels>>,
}

impl CodTabInfo {
    /// Whether the name is cut in the last drawn frame.
    pub fn truncated(&self) -> bool {
        self.cut
    }
}

/// Whether text of `text_width` is cut in a box of `room` (half a pixel of rounding allowed).
fn is_cut(text_width: Pixels, room: Pixels) -> bool {
    f32::from(text_width) > f32::from(room) + 0.5
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
) -> (Option<String>, Vec<std::path::PathBuf>) {
    match member {
        CodMember::Live(workspace) => {
            let workspace = workspace.read(cx);
            let folders: Vec<std::path::PathBuf> = workspace
                .visible_worktrees(cx)
                .map(|worktree| worktree.read(cx).abs_path().to_path_buf())
                .collect();
            (workspace::cod_workspace_name(workspace, cx), folders)
        }
        CodMember::Pending(id) => {
            let folders = multi_workspace
                .cod_pending_paths(*id)
                .unwrap_or_default()
                .to_vec();
            (workspace::cod_workspace_name_by_id(*id, cx), folders)
        }
    }
}

/// The name a tab shows for `member`, numbering it among the unnamed, folderless workspaces
/// `unnamed_count` has counted so far (in tab order); bumps `unnamed_count` when `member` is one
/// of them, so the next call sees the right rank.
fn member_name(
    member: &CodMember,
    multi_workspace: &MultiWorkspace,
    cx: &App,
    unnamed_count: &mut usize,
) -> String {
    let (name, folders) = member_name_and_folders(member, multi_workspace, cx);
    let folder_refs: Vec<&Path> = folders.iter().map(|folder| folder.as_ref()).collect();
    if is_unnamed_default(name.as_deref(), &folder_refs) {
        *unnamed_count += 1;
    }
    tab_display_name(name, &folder_refs, *unnamed_count)
}

/// Builds the status dot, if the status calls for one, and wires it so the end-to-end driver
/// reads back what was actually drawn rather than the status that was computed for it: the
/// color is written into `layout` only from the dot's own prepaint callback, and the pulsing
/// flag only from inside its animation's own frame callback.
fn status_dot(status: CodStatus, index: usize, layout: &Rc<RefCell<CodTabsLayout>>) -> Option<AnyElement> {
    let color = status.color()?;
    let color_name = status.dot_name()?;
    let drawn_layout = layout.clone();
    let dot = div()
        .flex_none()
        .child(Indicator::dot().color(color))
        .on_children_prepainted(move |bounds, _window, _cx| {
            let drawn = bounds
                .first()
                .is_some_and(|bounds| bounds.size.width > px(0.) && bounds.size.height > px(0.));
            if let Some(tab) = drawn_layout.borrow_mut().tabs.get_mut(index) {
                tab.dot = drawn.then_some(color_name);
            }
        });
    Some(if status.pulses() {
        let pulse_layout = layout.clone();
        dot.with_animation(
            ("cod-workspace-tab-dot", index),
            Animation::new(Duration::from_secs(2))
                .repeat()
                .with_easing(pulsating_between(0.3, 1.0)),
            move |dot, delta| {
                if let Some(tab) = pulse_layout.borrow_mut().tabs.get_mut(index) {
                    tab.pulsing = true;
                }
                dot.opacity(delta)
            },
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
    // #77 F2: the same ghost tokens Zed's own title bar buttons use (a toggled IconButton), so
    // every theme stays coherent: ghost_element_selected for the active tab, ghost_element_hover
    // on hover, no background (the title bar shows through) otherwise, and no separate border
    // color on any tab (the previous `border` token was unrelated to either the fill or the bar,
    // and read as a third, incoherent grey against them).
    let colors = cx.theme().colors();
    let (active_text, active_bg) = (colors.text, colors.ghost_element_selected);
    let (inactive_text, hover_bg) = (colors.text_muted, colors.ghost_element_hover);

    let mut infos = Vec::with_capacity(members.len());
    let mut tabs: Vec<AnyElement> = Vec::with_capacity(members.len() + 1);
    // #77: counts the unnamed, folderless workspaces seen so far, in tab order, so the second
    // and later ones read "New workspace 2", "New workspace 3", and so on.
    let mut unnamed_count = 0usize;
    for (index, member) in members.into_iter().enumerate() {
        let name = member_name(&member, multi_workspace, cx, &mut unnamed_count);
        let is_active = matches!(&member, CodMember::Live(workspace) if *workspace == active);
        let status = MultiWorkspace::cod_member_id(&member, cx)
            .map(|id| workspace::cod_workspace_status(id, cx))
            .unwrap_or(CodStatus::Idle);
        infos.push(CodTabInfo {
            name: name.clone(),
            active: is_active,
            dot: None,
            pulsing: false,
            tooltip: name.clone(),
            bounds: None,
            cut: false,
            close_bounds: None,
        });

        let (id, rest) = split_workspace_name(&name);
        let text_color = if is_active { active_text } else { inactive_text };
        let weak = weak.clone();
        // The dot, the "[ID]" and the rest of the name. After layout the rest is measured
        // against the room it got, to report whether the name was cut.
        let rest_text: SharedString = rest.to_string().into();
        let measured_layout = layout.clone();
        // #77 F1: `content` keeps its own `min_w_0`: once `tab` below is capped at
        // COD_TAB_MAX_WIDTH, `content` (and inside it, the label) must still be able to shrink
        // to that width, or the label reports its full, unclipped natural width as the "room" it
        // got and `cut` below never becomes true for a name that really is too long. What must
        // NOT have `min_w_0` is the tabs *row* and the left group that holds it
        // (`title_bar.rs`): those feed the title bar's outer three-group layout, and having
        // `min_w_0` there made that outer layout measure their natural (max-content) width as
        // far smaller than the tab labels actually need, cutting even short names like "New
        // workspace" with most of the title bar still empty.
        let content = h_flex()
            .min_w_0()
            .gap_1()
            .children(status_dot(status, index, &layout))
            .when_some(id, |content, id| {
                content.child(
                    div()
                        .flex_none()
                        .child(Label::new(id.to_string()).color(Color::Custom(text_color))),
                )
            })
            .when(!rest.is_empty(), |content| {
                content.child(
                    div().min_w_0().overflow_x_hidden().child(
                        Label::new(rest_text.clone())
                            .color(Color::Custom(text_color))
                            .truncate(),
                    ),
                )
            })
            .on_children_prepainted(move |bounds, window, cx| {
                let Some(room) = bounds.last().filter(|_| !rest_text.is_empty()) else {
                    return;
                };
                // The label's font: the UI font at the default label size.
                let font = theme::theme_settings(cx).ui_font(cx).clone();
                let font_size = TextSize::Default.rems(cx).to_pixels(window.rem_size());
                let text_width = window
                    .text_system()
                    .shape_line(
                        rest_text.clone(),
                        font_size,
                        &[gpui::TextRun {
                            len: rest_text.len(),
                            font,
                            ..Default::default()
                        }],
                        None,
                    )
                    .width();
                if let Some(tab) = measured_layout.borrow_mut().tabs.get_mut(index) {
                    tab.cut = is_cut(text_width, room.size.width);
                }
            });
        // #77 F3: a close control, after the name, visible on the active tab and on hover; its
        // space is always reserved (`invisible()`/`visible_on_hover`, never added or removed) so
        // the tab's own width never jumps when the mouse moves over it. A pending (not yet
        // loaded) member has no live `Workspace` to close this way, so it gets none: a pending
        // tab is slightly narrower than a loaded one until it loads.
        let close_button = match &member {
            CodMember::Live(workspace) => {
                let workspace = workspace.clone();
                let weak = weak.clone();
                // The same compact shape Zed's own pane tab close button uses (pane.rs), so its
                // box is tight around the icon and centers the same way the status dot does,
                // rather than an icon button's usual (taller, padded) click target.
                // #77 C1: keyed by the workspace's own entity id, not the tab's index, so a held
                // press cannot migrate to a different workspace if the member list changes
                // (another tab closing, for example) between the mouse down and the mouse up:
                // gpui keeps pending press state by element id across frames
                // (crates/gpui/src/elements/div.rs), and an index-based id would let whichever
                // workspace now sits at that position inherit a press meant for another one.
                let button = IconButton::new(("cod-workspace-tab-close", workspace.entity_id()), IconName::Close)
                    .shape(IconButtonShape::Square)
                    .size(ButtonSize::None)
                    .icon_size(IconSize::XSmall)
                    .tooltip(Tooltip::text("Close Workspace"))
                    .when(!is_active, |button| button.visible_on_hover(""))
                    .on_click(move |_, window, cx| {
                        weak.update(cx, |multi_workspace, cx| {
                            multi_workspace.cod_close_live_workspace(workspace.clone(), window, cx)
                        })
                        .ok();
                    });
                // `IconButton`'s own click only stops propagation from inside its synthesized
                // "click" (after a matching down and up), which runs after the tab's own
                // `on_mouse_down`/`on_mouse_up` already saw the raw events and selected the tab
                // underneath it; stopping the mouse down here (the same pattern title_bar.rs
                // already uses for its own button groups) keeps it from doing that.
                Some(
                    h_flex()
                        .h_full()
                        .items_center()
                        .justify_center()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(button),
                )
            }
            CodMember::Pending(_) => None,
        };
        let has_close = close_button.is_some();
        let close_layout = layout.clone();
        // #77 C1: the tab's own id is also keyed by the workspace's identity, not its index, for
        // the same reason as the close button's id above.
        let tab_id: gpui::ElementId = match &member {
            CodMember::Live(workspace) => ("cod-workspace-tab", workspace.entity_id()).into(),
            CodMember::Pending(id) => ("cod-workspace-tab-pending", u64::from(i64::from(*id).unsigned_abs())).into(),
        };
        // `on_children_prepainted` is only on the plain `Div` gpui gives back from `h_flex()`,
        // not on the `Stateful<Div>` that `.id(...)` turns it into below (needed for `tooltip`,
        // `on_mouse_down` and `on_mouse_up`), so it is attached first.
        let tab = h_flex()
            .when(has_close, |tab| {
                tab.on_children_prepainted(move |bounds, _window, _cx| {
                    if let Some(tab_info) = close_layout.borrow_mut().tabs.get_mut(index) {
                        tab_info.close_bounds = bounds.last().copied();
                    }
                })
            })
            .id(tab_id)
            .group("")
            .h_full()
            .min_w_0()
            .max_w(px(COD_TAB_MAX_WIDTH))
            .px_2()
            .gap_1()
            .text_color(text_color)
            .when(is_active, |tab| tab.bg(active_bg))
            .when(!is_active, |tab| tab.hover(move |style| style.bg(hover_bg)))
            .cursor_pointer()
            .child(content)
            .children(close_button)
            .tooltip(Tooltip::text(name.clone()))
            // A click shows the workspace, as a click on a Zed tab does: the press and then the
            // release over the same workspace's tab (#25 AC5). The press is kept by the window's
            // `MultiWorkspace`, not by this element: each workspace draws its own title bar, so
            // when a pending workspace finishes loading and is shown between the press and the
            // release, the release lands on the new title bar's tab for the same workspace.
            // The close button above stops its own mouse down/up from reaching here (ButtonLike
            // already does, for every button in this codebase), so clicking it never also
            // selects the tab.
            .on_mouse_down(MouseButton::Left, {
                let weak = weak.clone();
                let member = member.clone();
                move |_, _window, cx| {
                    weak.update(cx, |multi_workspace, cx| {
                        multi_workspace.cod_press_tab(&member, cx)
                    })
                    .ok();
                }
            })
            .on_mouse_up(MouseButton::Left, move |_, window, cx| {
                weak.update(cx, |multi_workspace, cx| {
                    multi_workspace.cod_release_tab(&member, window, cx)
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

    // A release anywhere but over the pressed tab ends the press without a click. The row's
    // own release listener runs after the tab's (children first), so a release over a tab
    // gets its chance first; a release outside the row is seen here too.
    let cancel = move |_: &gpui::MouseUpEvent, _: &mut Window, cx: &mut App| {
        weak.update(cx, |multi_workspace, _| multi_workspace.cod_cancel_tab_press())
            .ok();
    };
    // #77 F1/C4: no `min_w_0`/`overflow_x_hidden` here (see the comment on `content` above):
    // each tab already shrinks on its own (its own `min_w_0` and `max_w`), and the title bar's
    // own outer row already clips (`platform_title_bar.rs`'s `overflow_x_hidden`); this row only
    // needs to report its true natural width when there is room for it. `max_w(relative(1.))`
    // is a hard cap at the row's own available width regardless: measured, the layout engine's
    // own "how much room do I want" pass for an ancestor and its "how much room do I actually
    // get" pass for this row do not agree without `min_w_0` once there truly is not enough room
    // for every tab at its natural width (six short-named tabs in a 640 px bar ran 120 px past
    // the title bar's own right edge without this), so the cap is what actually keeps the row,
    // and every tab inside it, inside the title bar in that case.
    h_flex()
        .h_full()
        .max_w(relative(1.))
        .on_mouse_up(MouseButton::Left, cancel.clone())
        .on_mouse_up_out(MouseButton::Left, cancel)
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
    fn text_wider_than_its_room_is_cut() {
        assert!(is_cut(px(400.), px(80.)));
        assert!(!is_cut(px(80.), px(80.)));
        assert!(!is_cut(px(80.4), px(80.)));
        assert!(is_cut(px(81.), px(80.)));
    }

    #[test]
    fn unnamed_tabs_fall_back_to_folders_then_new_workspace() {
        assert_eq!(
            tab_display_name(Some("[DOC-208] Move".into()), &[Path::new("/w/docs")], 1),
            "[DOC-208] Move"
        );
        assert_eq!(
            tab_display_name(None, &[Path::new("/w/billing-api"), Path::new("/w/infra")], 1),
            "billing-api, infra"
        );
        assert_eq!(tab_display_name(Some("  ".into()), &[], 1), COD_UNNAMED_WORKSPACE);
        assert_eq!(tab_display_name(None, &[], 1), COD_UNNAMED_WORKSPACE);
    }

    #[test]
    fn several_unnamed_folderless_tabs_are_numbered_from_the_second_one() {
        // #77: a user must be able to tell two freshly-opened workspaces apart.
        assert_eq!(tab_display_name(None, &[], 1), "New workspace");
        assert_eq!(tab_display_name(None, &[], 2), "New workspace 2");
        assert_eq!(tab_display_name(None, &[], 3), "New workspace 3");
        // A name or a folder takes a workspace out of the numbering, whatever rank it is passed.
        assert_eq!(tab_display_name(Some("Renamed".into()), &[], 5), "Renamed");
        assert_eq!(tab_display_name(None, &[Path::new("/w/docs")], 5), "docs");
    }

    #[test]
    fn is_unnamed_default_matches_what_tab_display_name_numbers() {
        assert!(is_unnamed_default(None, &[]));
        assert!(is_unnamed_default(Some("  "), &[]));
        assert!(!is_unnamed_default(Some("Renamed"), &[]));
        assert!(!is_unnamed_default(None, &[Path::new("/w/docs")]));
    }
}
