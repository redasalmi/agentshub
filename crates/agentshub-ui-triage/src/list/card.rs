//! A chat's card in the list.

use gpui_kit::assets::IconName as ExtraIcon;
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use agentshub_core::agent::Agent;
use agentshub_core::format::{activity, age, capitalized};
use agentshub_core::status::Status;
use agentshub_core::store::{Chat, Project};
use agentshub_ui_kit::actions::NUMBERED;
use agentshub_ui_kit::layout::{Held, NavEvent};
use agentshub_ui_kit::status::{Tone, dot};
use agentshub_ui_kit::theme as appearance;
use agentshub_ui_kit::widgets::key_number;

use super::{TriageList, emit};

const CARD_RADIUS: Pixels = px(9.);
/// The widest a project's name gets on a card; the branch beside it gives
/// way first.
const MAX_PROJECT_WIDTH: Pixels = px(160.);

/// What a card says under its title: what its agent needs or does.
pub(super) fn card_text(agent: Agent, running: Option<Option<Status>>) -> String {
    match Tone::of(running) {
        tone @ Tone::NeedsYou(Some(_)) => capitalized(tone.label()),
        Tone::Saved => format!("{} isn't running", agent.display_name()),
        _ => activity(agent, running),
    }
}

/// A small square in project `project`'s color.
pub(super) fn swatch(project: i64, cx: &App) -> Div {
    div()
        .size(px(10.))
        .flex_none()
        .rounded(px(3.))
        .bg(appearance::project_tint(project, cx).text)
}

impl TriageList {
    /// A chat's card, `index` being its place among the cards on show.
    #[expect(
        clippy::too_many_lines,
        reason = "one element tree for a chat's card, with its menu"
    )]
    pub(super) fn render_card(
        &self,
        index: usize,
        project: &Project,
        chat: &Chat,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let palette = appearance::palette(cx);
        let id = chat.id;
        let active = self.active == Some(id);
        let running = self.running(id);
        let tone = Tone::of(running);
        let archived = chat.archived_at.is_some();
        let pinned = chat.pinned;
        let open = tone != Tone::Saved;
        let title = SharedString::from(chat.title.clone().unwrap_or_else(|| "New chat".into()));
        let current_title = SharedString::from(chat.title.clone().unwrap_or_default());
        let text_color = match tone {
            Tone::NeedsYou(_) | Tone::Error => tone.text_color(cx),
            _ => theme.muted_foreground,
        };
        let list = cx.entity();
        let separator = || div().flex_none().opacity(0.5).child("·");

        h_flex()
            .id(("card", id.cast_unsigned()))
            .flex_none()
            .items_start()
            .gap(px(10.))
            .px(px(10.))
            .pt(px(9.))
            .pb(px(10.))
            .rounded(CARD_RADIUS)
            .border_1()
            .cursor_pointer()
            .map(|this| {
                if active {
                    this.bg(theme.sidebar_accent)
                        .border_color(palette.accent.opacity(0.3))
                } else {
                    this.border_color(transparent_black())
                        .hover(|style| style.bg(theme.sidebar_accent))
                }
            })
            .child(div().mt(px(2.)).child(dot(tone, cx)))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.))
                    .child(
                        h_flex()
                            .min_w_0()
                            .gap_2()
                            .when(pinned, |this| {
                                this.child(
                                    Icon::new(ExtraIcon::Pin)
                                        .xsmall()
                                        .text_color(theme.muted_foreground),
                                )
                            })
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_sm()
                                    .map(|this| {
                                        if !open {
                                            this.text_color(theme.muted_foreground)
                                        } else if self.unseen.contains(&id) {
                                            this.font_semibold()
                                        } else {
                                            this.font_medium()
                                        }
                                    })
                                    .child(title),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(age(chat.archived_at.unwrap_or(chat.updated_at))),
                            )
                            .when(index < NUMBERED && self.held == Some(Held::Chats), |this| {
                                this.child(key_number(index, cx))
                            }),
                    )
                    .child(
                        div()
                            .text_xs()
                            .truncate()
                            .text_color(text_color)
                            .child(card_text(chat.agent, running)),
                    )
                    .child(
                        h_flex()
                            .mt_1()
                            .min_w_0()
                            .gap(px(5.))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .when(self.filter.is_none(), |this| {
                                this.child(swatch(project.id, cx))
                                    .child(
                                        div()
                                            .flex_none()
                                            .max_w(MAX_PROJECT_WIDTH)
                                            .truncate()
                                            .child(project.name.clone()),
                                    )
                                    .child(separator())
                            })
                            .child(div().flex_none().child(chat.agent.label()))
                            .when_some(chat.worktree.as_ref(), |this, worktree| {
                                this.child(separator())
                                    .child(Icon::new(ExtraIcon::GitBranch).xsmall())
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .font_family(theme.mono_font_family.clone())
                                            .child(worktree.branch()),
                                    )
                            }),
                    ),
            )
            .on_click(cx.listener(move |_, _, _, cx| cx.emit(NavEvent::OpenChat(id))))
            .context_menu(move |menu, _, _| {
                let emit = |event| emit(&list, event);
                let menu = menu.item(
                    PopupMenuItem::new("Open")
                        .icon(IconName::ExternalLink)
                        .on_click(emit(NavEvent::OpenChat(id))),
                );
                if archived {
                    return menu
                        .item(
                            PopupMenuItem::new("Bring back")
                                .icon(ExtraIcon::ArchiveRestore)
                                .on_click(emit(NavEvent::RestoreChat(id))),
                        )
                        .separator()
                        .item(
                            PopupMenuItem::new("Delete")
                                .icon(IconName::Delete)
                                .on_click(emit(NavEvent::DeleteChat(id))),
                        );
                }
                menu.item(
                    PopupMenuItem::new("Rename…")
                        .icon(ExtraIcon::Pencil)
                        .on_click(emit(NavEvent::RenameChat(id, current_title.clone()))),
                )
                .item(
                    PopupMenuItem::new(if pinned { "Unpin" } else { "Pin to the top" })
                        .icon(if pinned {
                            ExtraIcon::PinOff
                        } else {
                            ExtraIcon::Pin
                        })
                        .on_click(emit(NavEvent::PinChat(id, !pinned))),
                )
                .when(open, |menu| {
                    menu.item(
                        PopupMenuItem::new("Close")
                            .icon(IconName::Close)
                            .on_click(emit(NavEvent::CloseChat(id))),
                    )
                })
                .separator()
                .item(
                    PopupMenuItem::new("Archive")
                        .icon(ExtraIcon::Archive)
                        .on_click(emit(NavEvent::ArchiveChat(id))),
                )
                .item(
                    PopupMenuItem::new("Delete")
                        .icon(IconName::Delete)
                        .on_click(emit(NavEvent::DeleteChat(id))),
                )
            })
    }
}
