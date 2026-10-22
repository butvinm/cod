//! Cod (#25): the workspaces of the window as quiet tabs in the title bar, in window order, each
//! with a status dot only when something is happening, followed by a "+" that opens a new
//! workspace. Clicking a tab shows its workspace (a pending one loads first).
//!
//! Names follow "[ISSUE-ID] title" (D27): the leading "[ID]" is always shown whole and only the
//! rest is cut with "...". The number of a numbered default name ("New workspace 2") is likewise
//! always shown whole after the cut part (#81), so crowded default tabs stay distinguishable. The
//! full name is the tab's tooltip.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
    time::Duration,
};

use gpui::{
    Animation, AnimationExt as _, Bounds, ElementId, Entity, FocusHandle, MouseButton,
    ScrollHandle, WeakEntity, pulsating_between,
};
use ui::{IconButtonShape, Tooltip, prelude::*};
use workspace::{CodMember, CodStatus, MultiWorkspace, NewWorkspace};
// The names moved to the workspace crate (#81) so a close prompt names a workspace the way its
// tab does; kept reachable here for the overview.
pub use workspace::{
    COD_UNNAMED_WORKSPACE, is_unnamed_default, member_name_and_folders, tab_display_name,
};

/// The widest a tab gets, in logical pixels (#25 open question 1).
pub const COD_TAB_MAX_WIDTH: f32 = 242.;
const COD_TAB_MIN_WIDTH: f32 = 114.;

#[derive(Default)]
pub struct CodTabsState {
    scroll: ScrollHandle,
    focus: HashMap<ElementId, (FocusHandle, FocusHandle)>,
    revealed: Option<(ElementId, Pixels, usize)>,
    focused: Option<ElementId>,
    viewport_width: Rc<Cell<Option<Pixels>>>,
}

/// One tab as it was last drawn, for the end-to-end driver.
#[derive(Clone, Debug, Default)]
pub struct CodTabInfo {
    /// The name the tab stands for (the user's name, else its folders, else "New workspace").
    pub name: String,
    pub active: bool,
    pub focused: bool,
    pub close_focused: bool,
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
    /// it got. Tabs keep their minimum width and scroll when the title bar is crowded.
    pub cut: bool,
    /// The label as the last frame drew it (issue #81): the "[ID]", the part of the rest of the
    /// name that fit (ending in an ellipsis when cut) and the number of a numbered default name,
    /// joined by spaces. Empty before the tab was drawn.
    pub visible: String,
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
    pub tabs_viewport: Option<Bounds<Pixels>>,
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

/// Builds the status dot, if the status calls for one, and wires it so the end-to-end driver
/// reads back what was actually drawn rather than the status that was computed for it: the
/// color is written into `layout` only from the dot's own prepaint callback, and the pulsing
/// flag only from inside its animation's own frame callback.
fn status_dot(
    status: CodStatus,
    index: usize,
    layout: &Rc<RefCell<CodTabsLayout>>,
    cx: &App,
) -> Option<AnyElement> {
    let color = status.color()?;
    let color_name = status.dot_name()?;
    let drawn_layout = layout.clone();
    let dot = div()
        .flex_none()
        .child(div().size(px(5.)).rounded_full().bg(color.color(cx)))
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
    state: &mut CodTabsState,
    window: &Window,
    cx: &mut App,
) -> AnyElement {
    let weak: WeakEntity<MultiWorkspace> = multi_workspace.downgrade();
    let multi_workspace = multi_workspace.read(cx);
    let active = multi_workspace.workspace().clone();
    let members = multi_workspace.cod_shown_names(cx);
    let colors = cx.theme().colors();
    let (active_text, active_bg) = (colors.text, colors.tab_active_background);
    let (inactive_text, hover_bg) = (colors.text_muted, colors.ghost_element_hover);
    let border = colors.border_variant;
    let focus_border = colors.border_focused;
    let active_index = members.iter().position(
        |(member, _)| matches!(member, CodMember::Live(workspace) if *workspace == active),
    );
    let member_count = members.len();
    let mut identities = Vec::with_capacity(member_count);
    let mut focused = None;

    let mut infos = Vec::with_capacity(members.len());
    let mut tabs: Vec<AnyElement> = Vec::with_capacity(members.len() + 1);
    // #77: the second and later unnamed, folderless workspaces read "New workspace 2", "New
    // workspace 3", and so on (`MultiWorkspace::cod_shown_names`).
    for (index, (member, shown)) in members.into_iter().enumerate() {
        let name = shown.name;
        let number_text = shown.number.map(|number| number.to_string());
        let is_active = matches!(&member, CodMember::Live(workspace) if *workspace == active);
        let tab_id: ElementId = match &member {
            CodMember::Live(workspace) => ("cod-workspace-tab", workspace.entity_id()).into(),
            CodMember::Pending(id) => {
                ("cod-workspace-tab-pending", i64::from(*id).unsigned_abs()).into()
            }
        };
        identities.push(tab_id.clone());
        let (tab_focus, close_focus) = state
            .focus
            .entry(tab_id.clone())
            .or_insert_with(|| (cx.focus_handle(), cx.focus_handle()));
        let tab_focus = tab_focus.clone();
        let close_focus = close_focus.clone();
        let has_focus = tab_focus.contains_focused(window, cx);
        if has_focus {
            focused = Some(tab_id.clone());
            if state.focused.as_ref() != Some(&tab_id) {
                state.scroll.scroll_to_item(index);
            }
        }
        if is_active {
            let reveal = (tab_id.clone(), window.viewport_size().width, member_count);
            if state.revealed.as_ref() != Some(&reveal) {
                state.scroll.scroll_to_item(index);
                state.revealed = Some(reveal);
            }
        }
        let status = MultiWorkspace::cod_member_id(&member, cx)
            .map(|id| workspace::cod_workspace_status(id, cx))
            .unwrap_or(CodStatus::Idle);
        infos.push(CodTabInfo {
            name: name.clone(),
            active: is_active,
            focused: tab_focus.is_focused(window),
            close_focused: close_focus.is_focused(window),
            dot: None,
            pulsing: false,
            tooltip: name.clone(),
            bounds: None,
            cut: false,
            visible: String::new(),
            close_bounds: None,
        });

        let (id, rest) = split_workspace_name(&name);
        // #81 (QA #92): the number of a numbered default name is drawn by itself after the rest,
        // never cut, so six "New workspace N" tabs in a full bar read "New w... 2", "New w... 3"
        // and so on rather than six identical "New w...". The first default one has no number.
        let rest = match &number_text {
            Some(number) => rest
                .strip_suffix(number.as_str())
                .map(str::trim_end)
                .unwrap_or(rest),
            None => rest,
        };
        let text_color = if is_active {
            active_text
        } else {
            inactive_text
        };
        let weak = weak.clone();
        // The dot, the "[ID]", the rest of the name and the number. After layout the rest is
        // measured against the room it got, to report whether the name was cut and what was drawn.
        let rest_text: SharedString = rest.to_string().into();
        let id_text = id.map(str::to_string);
        let drawn_number = number_text.clone();
        let measured_layout = layout.clone();
        let dot = status_dot(status, index, &layout, cx);
        // The place of the rest among the drawn children: after the dot and the "[ID]".
        let rest_index = usize::from(dot.is_some()) + usize::from(id.is_some());
        // Only the name shrinks inside a tab; its identity and default number stay whole.
        let content = h_flex()
            .min_w_0()
            .flex_grow_1()
            .gap(px(8.))
            .children(dot)
            .when_some(id, |content, id| {
                content.child(
                    div().flex_none().child(
                        Label::new(id.to_string())
                            .size(LabelSize::Small)
                            .when(is_active, |label| label.weight(gpui::FontWeight::SEMIBOLD))
                            .color(Color::Custom(text_color)),
                    ),
                )
            })
            .when(!rest.is_empty(), |content| {
                content.child(
                    div().min_w_0().overflow_x_hidden().child(
                        Label::new(rest_text.clone())
                            .size(LabelSize::Small)
                            .when(is_active, |label| label.weight(gpui::FontWeight::SEMIBOLD))
                            .color(Color::Custom(text_color))
                            .truncate(),
                    ),
                )
            })
            .when_some(number_text, |content, number| {
                content.child(
                    div().flex_none().child(
                        Label::new(number)
                            .size(LabelSize::Small)
                            .when(is_active, |label| label.weight(gpui::FontWeight::SEMIBOLD))
                            .color(Color::Custom(text_color)),
                    ),
                )
            })
            .on_children_prepainted(move |bounds, window, cx| {
                let mut visible: Vec<String> = id_text.iter().cloned().collect();
                let mut cut = false;
                if let Some(room) = bounds.get(rest_index).filter(|_| !rest_text.is_empty()) {
                    // Match the small label's size and selected weight in the drawn-text probe.
                    let mut font = theme::theme_settings(cx).ui_font(cx).clone();
                    if is_active {
                        font.weight = gpui::FontWeight::SEMIBOLD;
                    }
                    let font_size = TextSize::Small.rems(cx).to_pixels(window.rem_size());
                    let runs = [gpui::TextRun {
                        len: rest_text.len(),
                        font: font.clone(),
                        ..Default::default()
                    }];
                    let text_width = window
                        .text_system()
                        .shape_line(rest_text.clone(), font_size, &runs, None)
                        .width();
                    cut = is_cut(text_width, room.size.width);
                    // What the truncating label draws: the whole text when it fits, else
                    // gpui's own end truncation with an ellipsis (gpui's text element).
                    let drawn = if text_width <= room.size.width {
                        rest_text.clone()
                    } else {
                        window
                            .text_system()
                            .line_wrapper(font, font_size)
                            .truncate_line(
                                rest_text.clone(),
                                room.size.width,
                                "\u{2026}",
                                &runs,
                                gpui::TruncateFrom::End,
                            )
                            .0
                    };
                    visible.push(drawn.to_string());
                }
                visible.extend(drawn_number.iter().cloned());
                if let Some(tab) = measured_layout.borrow_mut().tabs.get_mut(index) {
                    tab.cut = cut;
                    tab.visible = visible.join(" ");
                }
            });
        // Keep the close slot even while hidden or not yet loaded so labels do not move on hover.
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
                let button = IconButton::new(
                    ("cod-workspace-tab-close", workspace.entity_id()),
                    IconName::Close,
                )
                .shape(IconButtonShape::Square)
                .size(ButtonSize::None)
                .icon_size(IconSize::XSmall)
                .width(px(22.))
                .tab_index(0isize)
                .track_focus(&close_focus)
                .tooltip(Tooltip::text("Close Workspace"))
                .when(!has_focus, |button| {
                    button.visible_on_hover("cod-workspace-tab")
                })
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
                        .flex_none()
                        .w(px(22.))
                        .h(px(22.))
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
        // `on_children_prepainted` is only on the plain `Div` gpui gives back from `h_flex()`,
        // not on the `Stateful<Div>` that `.id(...)` turns it into below (needed for `tooltip`,
        // `on_mouse_down` and `on_mouse_up`), so it is attached first.
        let tab = h_flex()
            .when(has_close, |tab| {
                tab.on_children_prepainted(move |bounds, _window, _cx| {
                    if let Some(tab_info) = close_layout.borrow_mut().tabs.get_mut(index) {
                        tab_info.close_bounds = bounds.get(1).copied();
                    }
                })
            })
            .id(tab_id)
            .group("cod-workspace-tab")
            .tab_index(0isize)
            .track_focus(&tab_focus)
            .flex_none()
            .h(px(32.))
            .min_w(px(COD_TAB_MIN_WIDTH))
            .max_w(px(COD_TAB_MAX_WIDTH))
            .pl(px(12.))
            .pr(px(5.))
            .gap(px(8.))
            .border_t_1()
            .border_x_1()
            .border_color(gpui::transparent_black())
            .rounded_t_sm()
            .text_color(text_color)
            .when(is_active, |tab| tab.bg(active_bg).border_color(border))
            .when(!is_active, |tab| tab.hover(move |style| style.bg(hover_bg)))
            .focus_visible(move |style| style.border_color(focus_border))
            .cursor_pointer()
            .child(content)
            .children(close_button)
            .when(!has_close, |tab| tab.child(div().flex_none().size(px(22.))))
            .when(
                !is_active && active_index != Some(index + 1) && index + 1 < member_count,
                |tab| {
                    tab.child(
                        div()
                            .absolute()
                            .right_0()
                            .top(px(8.))
                            .w(px(1.))
                            .h(px(14.))
                            .bg(border),
                    )
                },
            )
            .tooltip(Tooltip::text(name.clone()))
            .on_key_down({
                let weak = weak.clone();
                let member = member.clone();
                move |event, window, cx| {
                    if tab_focus.is_focused(window)
                        && !event.keystroke.modifiers.modified()
                        && matches!(event.keystroke.key.as_str(), "enter" | "space")
                    {
                        cx.stop_propagation();
                        weak.update(cx, |multi_workspace, cx| {
                            multi_workspace.cod_press_tab(&member, cx);
                            multi_workspace.cod_release_tab(&member, window, cx);
                        })
                        .ok();
                    }
                }
            })
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
    state
        .focus
        .retain(|identity, _| identities.contains(identity));
    state.focused = focused;
    *layout.borrow_mut() = CodTabsLayout {
        tabs: infos,
        tabs_viewport: None,
        add_button: None,
    };

    // A release anywhere but over the pressed tab ends the press without a click. The row's
    // own release listener runs after the tab's (children first), so a release over a tab
    // gets its chance first; a release outside the row is seen here too.
    let cancel = move |_: &gpui::MouseUpEvent, _: &mut Window, cx: &mut App| {
        weak.update(cx, |multi_workspace, _| {
            multi_workspace.cod_cancel_tab_press()
        })
        .ok();
    };
    let tab_layout = layout.clone();
    let drawn_scroll = state.scroll.clone();
    let resized_scroll = state.scroll.clone();
    let viewport_width = state.viewport_width.clone();
    h_flex()
        .min_w_0()
        .h_full()
        .on_mouse_up(MouseButton::Left, cancel.clone())
        .on_mouse_up_out(MouseButton::Left, cancel)
        .on_children_prepainted(move |bounds, window, _cx| {
            let mut layout = layout.borrow_mut();
            layout.tabs_viewport = bounds.first().copied();
            layout.add_button = bounds.get(1).copied();
            if let Some(viewport) = layout.tabs_viewport
                && viewport_width.replace(Some(viewport.size.width)) != Some(viewport.size.width)
                && let Some(active_index) = active_index
            {
                // The scroll handle learns the resized viewport during prepaint, after its
                // reveal pass. Repeat once with those actual bounds instead of the old width.
                resized_scroll.scroll_to_item(active_index);
                window.refresh();
            }
        })
        .child(
            h_flex()
                .min_w_0()
                .h_full()
                .items_end()
                .on_children_prepainted(move |_, _window, _cx| {
                    // Tracked scroll containers keep child bounds on their handle instead of
                    // passing them to this callback. Include the painted scroll offset.
                    let offset = drawn_scroll.offset();
                    for (index, tab) in tab_layout.borrow_mut().tabs.iter_mut().enumerate() {
                        tab.bounds = drawn_scroll.bounds_for_item(index).map(|mut bounds| {
                            bounds.origin += offset;
                            bounds
                        });
                    }
                })
                .id("cod-workspace-tabs-scroll")
                .overflow_x_scroll()
                .track_scroll(&state.scroll)
                .children(tabs),
        )
        .child(
            h_flex()
                .flex_none()
                .w(px(36.))
                .h_full()
                .justify_center()
                .child(
                    IconButton::new("cod-new-workspace", IconName::Plus)
                        .width(px(26.))
                        .tab_index(0isize)
                        .icon_size(IconSize::Small)
                        .tooltip(Tooltip::text("New Workspace"))
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(NewWorkspace), cx)
                        }),
                ),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

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
            tab_display_name(
                None,
                &[Path::new("/w/billing-api"), Path::new("/w/infra")],
                1
            ),
            "billing-api, infra"
        );
        assert_eq!(
            tab_display_name(Some("  ".into()), &[], 1),
            COD_UNNAMED_WORKSPACE
        );
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
