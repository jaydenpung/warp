//! "Assign PRs" modal: manually add/remove the GitHub PR links attributed to a
//! tab's focused terminal session. Writes are applied live to
//! `~/.warp-claude-prs/<session_uuid_hex>` (the same file the `gh pr create` hook
//! writes), so the per-tab PR chips update immediately via the directory watcher.
//!
//! The workspace wraps this body view in a `Modal<AssignPrsModal>` and drives it
//! via `on_open`/`on_close` (see `build_assign_prs_modal` in workspace/view.rs).

use warpui::elements::{
    Border, ChildView, ConstrainedBox, Container, CornerRadius, CrossAxisAlignment, Element,
    Fill as ElementFill, Flex, Hoverable, MainAxisAlignment, MainAxisSize, MouseStateHandle,
    Padding, ParentElement, Radius, Shrinkable, Text,
};
use warpui::fonts::{Properties, Weight};
use warpui::keymap::FixedBinding;
use warpui::platform::Cursor;
use warpui::ui_components::button::ButtonVariant;
use warpui::ui_components::components::{Coords, UiComponent, UiComponentStyles};
use warpui::{
    AppContext, Entity, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle,
};

use crate::appearance::Appearance;
use crate::claude_pr_attribution::{add_recorded_pr, remove_recorded_pr, ClaudePrAttributionModel};
use crate::context_chips::github_pr_number_from_url;
use crate::editor::{EditorView, Event as EditorEvent, SingleLineEditorOptions};
use crate::modal::ModalAction;

const CONTENT_HORIZONTAL_PADDING: f32 = 24.;
const HEADER_PADDING_TOP: f32 = 24.;
const HEADER_PADDING_BOTTOM: f32 = 12.;
const HEADER_TITLE_FONT_SIZE: f32 = 16.;
const BODY_BOTTOM_PADDING: f32 = 16.;
const FOOTER_VERTICAL_PADDING: f32 = 12.;
const FOOTER_BUTTON_HEIGHT: f32 = 32.;
const CLOSE_ICON_SIZE: f32 = 14.;
const ERROR_FONT_SIZE: f32 = 12.;
const ROW_GAP: f32 = 8.;
const INVALID_PR_URL_ERROR: &str = "Enter a valid GitHub pull request URL (…/pull/<number>)";

/// Registers the modal's ESC-to-close binding.
pub fn init(app: &mut AppContext) {
    use warpui::keymap::macros::*;
    app.register_fixed_bindings(vec![FixedBinding::new(
        "escape",
        AssignPrsModalAction::Close,
        id!("AssignPrsModal"),
    )]);
}

pub enum AssignPrsModalEvent {
    Close,
}

#[derive(Clone, Copy, Debug)]
pub enum AssignPrsModalAction {
    AddPr,
    RemovePr(usize),
    Close,
}

pub struct AssignPrsModal {
    pr_input: ViewHandle<EditorView>,
    /// Working copy of the tab's PR URLs (kept in sync with the on-disk file).
    pr_urls: Vec<String>,
    session_uuid_hex: String,
    error: Option<String>,
    remove_button_states: Vec<MouseStateHandle>,
    add_button_mouse_state: MouseStateHandle,
    done_button_mouse_state: MouseStateHandle,
    close_button_mouse_state: MouseStateHandle,
}

impl AssignPrsModal {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let pr_input = ctx.add_typed_action_view(|ctx| {
            let mut editor = EditorView::single_line(SingleLineEditorOptions::default(), ctx);
            editor.set_placeholder_text("Paste a GitHub PR URL", ctx);
            editor
        });
        ctx.subscribe_to_view(&pr_input, |me, _, event, ctx| match event {
            EditorEvent::Enter => me.try_add_pr(ctx),
            EditorEvent::Escape => ctx.emit(AssignPrsModalEvent::Close),
            EditorEvent::Edited(_) => {
                me.error = None;
                ctx.notify();
            }
            _ => {}
        });

        Self {
            pr_input,
            pr_urls: Vec::new(),
            session_uuid_hex: String::new(),
            error: None,
            remove_button_states: Vec::new(),
            add_button_mouse_state: Default::default(),
            done_button_mouse_state: Default::default(),
            close_button_mouse_state: Default::default(),
        }
    }

    /// Called by the workspace before showing the modal.
    pub fn on_open(
        &mut self,
        session_uuid_hex: String,
        current_pr_urls: Vec<String>,
        ctx: &mut ViewContext<Self>,
    ) {
        self.session_uuid_hex = session_uuid_hex;
        self.remove_button_states = current_pr_urls
            .iter()
            .map(|_| MouseStateHandle::default())
            .collect();
        self.pr_urls = current_pr_urls;
        self.error = None;
        self.pr_input.update(ctx, |e, ctx| {
            e.clear_buffer_and_reset_undo_stack(ctx);
        });
        ctx.focus(&self.pr_input);
        ctx.notify();
    }

    /// Called by the workspace when the modal is dismissed.
    pub fn on_close(&mut self, ctx: &mut ViewContext<Self>) {
        self.pr_urls.clear();
        self.remove_button_states.clear();
        self.session_uuid_hex.clear();
        self.error = None;
        ctx.notify();
    }

    fn try_add_pr(&mut self, ctx: &mut ViewContext<Self>) {
        let url = self.pr_input.as_ref(ctx).buffer_text(ctx).trim().to_string();
        if url.is_empty() {
            return;
        }
        if github_pr_number_from_url(&url).is_none() {
            self.error = Some(INVALID_PR_URL_ERROR.to_string());
            ctx.notify();
            return;
        }
        self.error = None;
        if !self.pr_urls.iter().any(|u| u == &url) {
            add_recorded_pr(&self.session_uuid_hex, &url);
            self.pr_urls.push(url);
            self.remove_button_states.push(MouseStateHandle::default());
            self.sync_model(ctx);
        }
        self.pr_input.update(ctx, |e, ctx| {
            e.clear_buffer_and_reset_undo_stack(ctx);
        });
        ctx.notify();
    }

    fn remove_pr(&mut self, index: usize, ctx: &mut ViewContext<Self>) {
        if index >= self.pr_urls.len() {
            return;
        }
        let url = self.pr_urls.remove(index);
        if index < self.remove_button_states.len() {
            self.remove_button_states.remove(index);
        }
        remove_recorded_pr(&self.session_uuid_hex, &url);
        self.sync_model(ctx);
        ctx.notify();
    }

    /// Pushes the current PR list straight into the shared attribution model so
    /// the tab's chips update immediately, instead of waiting for the filesystem
    /// watcher (which is only reliable right after startup). The file write
    /// already happened; this keeps the in-memory model in lock-step.
    fn sync_model(&self, ctx: &mut ViewContext<Self>) {
        let uuid = self.session_uuid_hex.clone();
        let urls = self.pr_urls.clone();
        ClaudePrAttributionModel::handle(ctx).update(ctx, |model, ctx| {
            model.set_prs_for_hex(&uuid, urls, ctx);
        });
    }

    fn text_button(
        &self,
        label: &str,
        mouse_state: MouseStateHandle,
        action: AssignPrsModalAction,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        appearance
            .ui_builder()
            .button(ButtonVariant::Text, mouse_state)
            .with_text_label(label.to_string())
            .with_style(UiComponentStyles {
                font_size: Some(appearance.ui_font_size()),
                font_weight: Some(Weight::Semibold),
                height: Some(FOOTER_BUTTON_HEIGHT),
                padding: Some(Coords::uniform(0.).left(12.).right(12.)),
                background: Some(ElementFill::None),
                border_width: Some(0.),
                border_radius: Some(CornerRadius::with_all(Radius::Pixels(4.))),
                font_color: Some(theme.main_text_color(theme.background()).into()),
                ..Default::default()
            })
            .build()
            .on_click(move |ctx, _, _| {
                ctx.dispatch_typed_action(action.clone());
            })
            .finish()
    }

    fn render_pr_row(
        &self,
        index: usize,
        url: &str,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let url_text = Text::new_inline(url.to_string(), appearance.ui_font_family(), 12.)
            .with_clip(warpui::text_layout::ClipConfig::ellipsis())
            .with_color(theme.main_text_color(theme.background()).into())
            .finish();
        let remove_state = self
            .remove_button_states
            .get(index)
            .cloned()
            .unwrap_or_default();
        let remove_button =
            self.text_button("Remove", remove_state, AssignPrsModalAction::RemovePr(index), appearance);
        Container::new(
            Flex::row()
                .with_main_axis_size(MainAxisSize::Max)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                .with_child(Shrinkable::new(1., url_text).finish())
                .with_child(remove_button)
                .finish(),
        )
        .with_border(Border::bottom(1.).with_border_fill(theme.outline()))
        .with_vertical_padding(4.)
        .finish()
    }
}

impl Entity for AssignPrsModal {
    type Event = AssignPrsModalEvent;
}

impl View for AssignPrsModal {
    fn ui_name() -> &'static str {
        "AssignPrsModal"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();

        // ── Header (title + X close) ───────────────────────────────────────
        let header = {
            let title = Text::new_inline(
                "Assign PRs".to_string(),
                appearance.ui_font_family(),
                HEADER_TITLE_FONT_SIZE,
            )
            .with_color(theme.active_ui_text_color().into())
            .with_style(Properties::default().weight(Weight::Bold))
            .finish();

            let close_icon = ConstrainedBox::new(
                warp_core::ui::Icon::X
                    .to_warpui_icon(theme.sub_text_color(theme.background()))
                    .finish(),
            )
            .with_width(CLOSE_ICON_SIZE)
            .with_height(CLOSE_ICON_SIZE)
            .finish();
            let close_button =
                Hoverable::new(self.close_button_mouse_state.clone(), move |_| close_icon)
                    .on_click(|ctx, _, _| {
                        ctx.dispatch_typed_action(ModalAction::Close);
                    })
                    .with_cursor(Cursor::PointingHand)
                    .finish();

            Container::new(
                Flex::row()
                    .with_main_axis_size(MainAxisSize::Max)
                    .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_child(Shrinkable::new(1., title).finish())
                    .with_child(close_button)
                    .finish(),
            )
            .with_padding(
                Padding::uniform(0.)
                    .with_top(HEADER_PADDING_TOP)
                    .with_bottom(HEADER_PADDING_BOTTOM)
                    .with_left(CONTENT_HORIZONTAL_PADDING)
                    .with_right(CONTENT_HORIZONTAL_PADDING),
            )
            .finish()
        };

        // ── Body: current PR list + add row + error ────────────────────────
        let mut body = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch);

        if self.pr_urls.is_empty() {
            body.add_child(
                Text::new_inline(
                    "No PRs assigned to this tab yet.".to_string(),
                    appearance.ui_font_family(),
                    12.,
                )
                .with_color(theme.sub_text_color(theme.background()).into())
                .finish(),
            );
        } else {
            for (index, url) in self.pr_urls.iter().enumerate() {
                body.add_child(self.render_pr_row(index, url, appearance));
            }
        }

        // Add row: input + Add button.
        let add_row = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(ROW_GAP)
            .with_child(Shrinkable::new(1., ChildView::new(&self.pr_input).finish()).finish())
            .with_child(self.text_button(
                "Add",
                self.add_button_mouse_state.clone(),
                AssignPrsModalAction::AddPr,
                appearance,
            ))
            .finish();
        body.add_child(Container::new(add_row).with_margin_top(ROW_GAP * 1.5).finish());

        if let Some(error) = &self.error {
            body.add_child(
                Container::new(
                    Text::new_inline(error.clone(), appearance.ui_font_family(), ERROR_FONT_SIZE)
                        .with_color(theme.ui_error_color())
                        .finish(),
                )
                .with_margin_top(4.)
                .finish(),
            );
        }

        let body_container = Container::new(body.finish())
            .with_padding(
                Padding::uniform(0.)
                    .with_left(CONTENT_HORIZONTAL_PADDING)
                    .with_right(CONTENT_HORIZONTAL_PADDING)
                    .with_bottom(BODY_BOTTOM_PADDING),
            )
            .finish();

        // ── Footer: Done ───────────────────────────────────────────────────
        let footer = Container::new(
            Container::new(
                Flex::row()
                    .with_main_axis_size(MainAxisSize::Max)
                    .with_main_axis_alignment(MainAxisAlignment::End)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_child(self.text_button(
                        "Done",
                        self.done_button_mouse_state.clone(),
                        AssignPrsModalAction::Close,
                        appearance,
                    ))
                    .finish(),
            )
            .with_padding(
                Padding::uniform(FOOTER_VERTICAL_PADDING)
                    .with_left(CONTENT_HORIZONTAL_PADDING)
                    .with_right(CONTENT_HORIZONTAL_PADDING),
            )
            .finish(),
        )
        .with_border(Border::top(1.).with_border_fill(theme.outline()))
        .finish();

        Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(header)
            .with_child(body_container)
            .with_child(footer)
            .finish()
    }
}

impl TypedActionView for AssignPrsModal {
    type Action = AssignPrsModalAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            AssignPrsModalAction::AddPr => self.try_add_pr(ctx),
            AssignPrsModalAction::RemovePr(index) => self.remove_pr(*index, ctx),
            AssignPrsModalAction::Close => ctx.emit(AssignPrsModalEvent::Close),
        }
    }
}
