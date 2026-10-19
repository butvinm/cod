//! The "web: open url" prompt (issue #31): a one-line editor in a modal. Enter opens the URL as a page tab in the active pane; text that is not a URL shows an inline error and opens nothing.

use editor::Editor;
use gpui::{AppContext as _, DismissEvent, Entity, EventEmitter, Focusable, Styled, WeakEntity};
use ui::{
    ActiveTheme, App, Color, Context, FluentBuilder, InteractiveElement, IntoElement, Label,
    LabelCommon, LabelSize, ParentElement, Render, SharedString, StyledExt, Window, div, h_flex,
    v_flex,
};
use workspace::{ModalView, Workspace};

use crate::page_url::normalize_page_url;

pub struct OpenPageModal {
    editor: Entity<Editor>,
    workspace: WeakEntity<Workspace>,
    last_error: Option<SharedString>,
}

impl EventEmitter<DismissEvent> for OpenPageModal {}
impl ModalView for OpenPageModal {}

impl Focusable for OpenPageModal {
    fn focus_handle(&self, cx: &App) -> gpui::FocusHandle {
        self.editor.focus_handle(cx)
    }
}

impl OpenPageModal {
    pub fn toggle(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
        let weak = workspace.weak_handle();
        workspace.toggle_modal(window, cx, move |window, cx| Self::new(weak, window, cx));
    }

    fn new(workspace: WeakEntity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("https://...", window, cx);
            editor
        });
        // A new edit clears the old error, so the hint comes back while typing.
        cx.subscribe(&editor, |this, _, event: &editor::EditorEvent, cx| {
            if matches!(event, editor::EditorEvent::BufferEdited) && this.last_error.take().is_some()
            {
                cx.notify();
            }
        })
        .detach();
        Self {
            editor,
            workspace,
            last_error: None,
        }
    }

    pub fn query(&self, cx: &App) -> String {
        self.editor.read(cx).text(cx)
    }

    pub fn error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    fn cancel(&mut self, _: &menu::Cancel, _window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn confirm(&mut self, _: &menu::Confirm, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.editor.read(cx).text(cx);
        match normalize_page_url(&text) {
            Ok(url) => {
                log::info!("web_view: opening {url} from the prompt");
                cx.emit(DismissEvent);
                self.workspace
                    .update(cx, |workspace, cx| {
                        crate::offscreen::open_page_in_active_pane(
                            workspace,
                            url,
                            crate::pages::PageKind::External,
                            window,
                            cx,
                        );
                    })
                    .ok();
            }
            Err(error) => {
                log::info!("web_view: the prompt refused {text:?}: {error}");
                self.last_error = Some(error.into());
                cx.notify();
            }
        }
    }
}

impl Render for OpenPageModal {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();

        v_flex()
            .key_context("OpenPageModal")
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::confirm))
            .elevation_3(cx)
            .w_96()
            .overflow_hidden()
            .child(
                div()
                    .p_2()
                    .border_b_1()
                    .border_color(theme.colors().border_variant)
                    .child(self.editor.clone()),
            )
            .child(
                h_flex()
                    .bg(theme.colors().editor_background)
                    .rounded_b_sm()
                    .w_full()
                    .p_2()
                    .gap_1()
                    .when_some(self.last_error.clone(), |this, error| {
                        this.child(Label::new(error).size(LabelSize::Small).color(Color::Error))
                    })
                    .when(self.last_error.is_none(), |this| {
                        this.child(
                            Label::new("Type a URL to open it as a page tab.")
                                .color(Color::Muted)
                                .size(LabelSize::Small),
                        )
                    }),
            )
    }
}
