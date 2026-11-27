//! What shows while no chat is open: the agents to start and the chats last
//! worked in.

use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, IconName, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use agentshub_core::agent::Agent;

use crate::actions::OpenSearch;
use crate::status::{Tone, dot};
use crate::theme as appearance;
use crate::widgets::heading;

/// What the empty state asks the workspace to do.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EmptyIntent {
    /// Start that agent in a new chat of that project.
    StartAgent(i64, Agent),
    /// Open the New Chat dialog for the selected project.
    NewChat,
    AddProject,
    /// Go back to a chat last worked in.
    OpenChat(i64),
}

type EmptyOn = Rc<dyn Fn(EmptyIntent, &mut Window, &mut App)>;

/// A chat last worked in, as the empty state lists it.
pub struct Recent {
    pub id: i64,
    pub title: String,
    /// What its agent is doing, or when it was last worked in, and its
    /// project.
    pub about: String,
    /// What its agent is doing, for its status dot.
    pub tone: Tone,
}

/// What shows while no chat is open.
pub struct EmptyState {
    /// The name of the selected project, if there is one.
    pub selected: Option<SharedString>,
    /// Whether there is a project to start a chat in, selected or not.
    pub projects: bool,
    /// The agents the selected project can start, once they have been looked
    /// up, with the project's id.
    pub agents: Option<(i64, Vec<Agent>)>,
    /// The chats last worked in, the last one first.
    pub recent: Vec<Recent>,
    /// Asks the workspace for what the buttons and rows do.
    pub on: EmptyOn,
}

impl EmptyState {
    #[expect(
        clippy::too_many_lines,
        reason = "one element tree, read top to bottom as the layout"
    )]
    pub fn render(&self, window: &Window, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let palette = appearance::palette(cx);
        let on = Rc::clone(&self.on);
        div()
            .id("empty")
            .size_full()
            .overflow_y_scroll()
            .child(
                h_flex()
                    .min_h_full()
                    .items_center()
                    .justify_center()
                    .p_8()
                    .child(
                    v_flex()
                        .w_full()
                        .max_w(px(520.))
                        .gap_6()
                        .child(
                            v_flex()
                                .gap_2()
                                .child(div().font_family(appearance::DISPLAY).text_3xl().child(
                                    if self.projects {
                                        "Start an agent"
                                    } else {
                                        "Add a project to begin"
                                    },
                                ))
                                .child(
                                    div()
                                        .text_sm()
                                        .line_height(rems(1.45))
                                        .text_color(theme.muted_foreground)
                                        .child(match &self.selected {
                                            Some(name) => format!(
                                                "In {name}. Agents keep running while you look at \
                                                     another chat."
                                            ),
                                            None if self.projects => "Pick the project in New \
                                                     chat. Agents keep running while you look at \
                                                     another chat."
                                                .to_string(),
                                            None => "A project is a folder you work in. Add one, \
                                                     then start Claude Code, Codex or Pi there."
                                                .to_string(),
                                        }),
                                ),
                        )
                        .child(match (&self.selected, &self.agents) {
                            (Some(_), Some((project, agents))) if !agents.is_empty() => {
                                let start = Rc::clone(&on);
                                h_flex()
                                    .flex_wrap()
                                    .gap_2()
                                    .children(agents.iter().copied().enumerate().map(
                                        |(index, agent)| {
                                            let start = Rc::clone(&start);
                                            let project = *project;
                                            Button::new(("empty-agent", index as u64))
                                                .map(|button| {
                                                    if index == 0 {
                                                        button.primary()
                                                    } else {
                                                        button.outline()
                                                    }
                                                })
                                                .icon(IconName::Plus)
                                                .label(agent.display_name())
                                                .child(
                                                    div()
                                                        .px_1p5()
                                                        .rounded(px(4.))
                                                        .bg(palette.strong)
                                                        .text_xs()
                                                        .font_family(theme.mono_font_family.clone())
                                                        .text_color(palette.secondary_foreground)
                                                        .child((index + 1).to_string()),
                                                )
                                                .tooltip(format!(
                                                    "Start {} here, or press {}",
                                                    agent.display_name(),
                                                    index + 1
                                                ))
                                                .on_click(move |_, window, cx| {
                                                    (start)(
                                                        EmptyIntent::StartAgent(project, agent),
                                                        window,
                                                        cx,
                                                    );
                                                })
                                        },
                                    ))
                                    .child({
                                        let start = Rc::clone(&on);
                                        Button::new("empty-new-chat")
                                            .ghost()
                                            .label("More options…")
                                            .on_click(move |_, window, cx| {
                                                (start)(EmptyIntent::NewChat, window, cx);
                                            })
                                    })
                                    .into_any_element()
                            }
                            _ if self.projects => {
                                let start = Rc::clone(&on);
                                h_flex()
                                    .child(
                                        Button::new("empty-new-chat")
                                            .primary()
                                            .icon(IconName::Plus)
                                            .label("New chat")
                                            .on_click(move |_, window, cx| {
                                                (start)(EmptyIntent::NewChat, window, cx);
                                            }),
                                    )
                                    .into_any_element()
                            }
                            _ => {
                                let start = Rc::clone(&on);
                                h_flex()
                                    .child(
                                        Button::new("empty-add-project")
                                            .primary()
                                            .icon(IconName::FolderOpen)
                                            .label("Add project")
                                            .on_click(move |_, window, cx| {
                                                (start)(EmptyIntent::AddProject, window, cx);
                                            }),
                                    )
                                    .into_any_element()
                            }
                        })
                        .when(!self.recent.is_empty(), |this| {
                            this.child(
                                v_flex()
                                    .gap_1()
                                    .pt_5()
                                    .border_t_1()
                                    .border_color(theme.border)
                                    .child(
                                        h_flex()
                                            .justify_between()
                                            .pb_1()
                                            .child(heading("Recent", cx))
                                            .children(
                                                crate::palette::shortcut(&OpenSearch, window).map(
                                                    |keys| {
                                                        div()
                                                            .text_xs()
                                                            .text_color(theme.muted_foreground)
                                                            .child(format!(
                                                                "{keys} to search them all"
                                                            ))
                                                    },
                                                ),
                                            ),
                                    )
                                    .children(self.recent.iter().map(|recent| {
                                        let on = Rc::clone(&on);
                                        let id = recent.id;
                                        h_flex()
                                            .id(("recent", id.cast_unsigned()))
                                            .px_2()
                                            .py(px(7.))
                                            .mx(px(-8.))
                                            .gap(px(10.))
                                            .rounded(theme.radius)
                                            .cursor_pointer()
                                            .hover(|style| style.bg(theme.accent))
                                            .child(dot(recent.tone, cx))
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .text_sm()
                                                    .truncate()
                                                    .child(recent.title.clone()),
                                            )
                                            .child(
                                                div()
                                                    .flex_none()
                                                    .max_w(px(240.))
                                                    .truncate()
                                                    .text_xs()
                                                    .font_family(theme.mono_font_family.clone())
                                                    .text_color(recent.tone.text_color(cx))
                                                    .child(recent.about.clone()),
                                            )
                                            .on_click(move |_, window, cx| {
                                                (on)(EmptyIntent::OpenChat(id), window, cx);
                                            })
                                    })),
                            )
                        }),
                ),
            )
            .into_any_element()
    }
}
