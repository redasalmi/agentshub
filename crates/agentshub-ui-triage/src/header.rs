//! The shown chat's header, which the window is dragged by, and the banner
//! under it while the chat needs the user.

use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::assets::IconName as ExtraIcon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::DropdownMenu as _;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Selectable as _, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use agentshub_core::agent::Agent;
use agentshub_core::format::{Turn, activity};
use agentshub_core::status::Status;
use agentshub_ui_kit::actions::{
    ArchiveChat, DeleteChat, NextNeedingYou, RenameChat, ToggleHistory, TogglePin, ToggleShell,
};
use agentshub_ui_kit::chat_body::{ChatIntent, On};
use agentshub_ui_kit::editor::{EditorChoice, editor_button};
use agentshub_ui_kit::status::{Tone, dot, status_pill};
use agentshub_ui_kit::theme::{self as appearance, Tint};
use agentshub_ui_kit::widgets::{path_line, stay_put};

use crate::list::project_mark;

/// The widest the project's name and the branch get in the header; the
/// folder gives way first.
const MAX_NAME_WIDTH: Pixels = px(200.);

/// What the banner says after what the chat needs.
const ANSWER: &str = "Answer it in the terminal below.";

/// What the header shows of the chat and the intents its buttons emit.
pub(crate) struct Header {
    pub title: SharedString,
    pub pinned: bool,
    /// The chat's project: its name, its mark and the mark's color.
    pub project: Option<(SharedString, SharedString, Tint)>,
    pub agent: Agent,
    /// The branch of the chat's worktree, when it has one.
    pub branch: Option<SharedString>,
    pub cwd: PathBuf,
    pub tone: Tone,
    pub turn: Option<Turn>,
    pub running: bool,
    /// Whether the status glyph may spin.
    pub spin: bool,
    pub earlier: usize,
    pub history_open: bool,
    pub shell: bool,
    /// The editor the chat opens in, and every installed one.
    pub editors: Option<(EditorChoice, Vec<EditorChoice>)>,
    pub focus: FocusHandle,
    pub on: On,
}

impl Header {
    /// The title over where the chat is, and the chat's status and buttons.
    pub(crate) fn render(self, cx: &App) -> AnyElement {
        h_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .gap_4()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .text_size(px(15.))
                            .font_semibold()
                            .child(self.title.clone()),
                    )
                    .child(self.render_place(cx)),
            )
            .child(
                stay_put(h_flex().flex_none().gap_4())
                    .when(self.running, |this| {
                        this.child(status_pill(self.tone, self.turn, self.spin, cx))
                    })
                    .child(self.render_buttons()),
            )
            .into_any_element()
    }

    /// The project, the agent, the worktree's branch and the folder.
    fn render_place(&self, cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        let separator = || div().flex_none().opacity(0.5).child("·");
        h_flex()
            .mt(px(3.))
            .min_w_0()
            .gap(px(6.))
            .overflow_hidden()
            .whitespace_nowrap()
            .text_xs()
            .text_color(theme.muted_foreground)
            .when_some(self.project.clone(), |this, (name, mark, tint)| {
                this.child(project_mark(mark, tint, px(14.), cx))
                    .child(
                        div()
                            .flex_none()
                            .max_w(MAX_NAME_WIDTH)
                            .truncate()
                            .child(name),
                    )
                    .child(separator())
            })
            .child(div().flex_none().child(self.agent.label()))
            .when_some(self.branch.clone(), |this, branch| {
                this.child(separator())
                    .child(Icon::new(ExtraIcon::GitBranch).xsmall())
                    .child(
                        div()
                            .flex_none()
                            .max_w(MAX_NAME_WIDTH)
                            .truncate()
                            .font_family(theme.mono_font_family.clone())
                            .child(branch),
                    )
            })
            .child(separator())
            .child(path_line(&self.cwd, cx))
    }

    /// The earlier sessions, the editor, the shell and the chat's menu.
    fn render_buttons(&self) -> impl IntoElement {
        let on = Rc::clone(&self.on);
        let focus = self.focus.clone();
        let pinned = self.pinned;
        h_flex()
            .flex_none()
            .gap_1()
            .when(self.earlier > 0, |this| {
                let on = Rc::clone(&on);
                this.child(
                    Button::new("history")
                        .ghost()
                        .small()
                        .icon(ExtraIcon::Clock)
                        .label(format!("{} earlier", self.earlier))
                        .selected(self.history_open)
                        .tooltip_with_action(
                            "Show or hide the earlier sessions",
                            &ToggleHistory,
                            None,
                        )
                        .on_click(move |_, window, cx| {
                            on(ChatIntent::ToggleHistory, window, cx);
                        }),
                )
            })
            .when_some(self.editors.clone(), |this, (current, all)| {
                this.child(editor_button(&current, all, Rc::clone(&on)))
            })
            .child({
                let on = Rc::clone(&on);
                Button::new("shell")
                    .ghost()
                    .small()
                    .icon(IconName::SquareTerminal)
                    .label("Shell")
                    .selected(self.shell)
                    .tooltip_with_action("Show or hide the project's shell", &ToggleShell, None)
                    .on_click(move |_, window, cx| {
                        on(ChatIntent::ToggleShell, window, cx);
                    })
            })
            .child(
                Button::new("chat-menu")
                    .ghost()
                    .small()
                    .icon(IconName::Ellipsis)
                    .tooltip("More")
                    .dropdown_menu(move |menu, _, _| {
                        menu.action_context(focus.clone())
                            .menu("Rename", Box::new(RenameChat))
                            .menu(
                                if pinned { "Unpin" } else { "Pin to the top" },
                                Box::new(TogglePin),
                            )
                            .separator()
                            .menu("Archive", Box::new(ArchiveChat))
                            .menu("Delete", Box::new(DeleteChat))
                    }),
            )
    }
}

/// What the banner says the chat needs, naming its agent, while the agent
/// waits on the user: `running` is what it is doing, as `Tone::of` takes it.
fn need(agent: Agent, running: Option<Option<Status>>) -> Option<String> {
    matches!(Tone::of(running), Tone::NeedsYou(_)).then(|| format!("{}.", activity(agent, running)))
}

/// The banner under the header while the chat needs the user.
pub(crate) struct Banner {
    /// What the chat needs.
    need: String,
    tone: Tone,
    /// The title of the next chat that needs the user, when another does.
    next: Option<SharedString>,
    focus: FocusHandle,
}

impl Banner {
    /// The banner of a chat of `agent` doing `running`, if it needs the
    /// user. `next` and `focus` are as the fields.
    pub(crate) fn new(
        agent: Agent,
        running: Option<Option<Status>>,
        next: Option<SharedString>,
        focus: FocusHandle,
    ) -> Option<Self> {
        Some(Self {
            need: need(agent, running)?,
            tone: Tone::of(running),
            next,
            focus,
        })
    }

    pub(crate) fn render(self, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let tint = appearance::palette(cx).warning;
        let focus = self.focus;
        h_flex()
            .flex_none()
            .px_6()
            .py_2()
            .gap_3()
            .border_b_1()
            .border_color(theme.border)
            .bg(tint.soft)
            .text_sm()
            .text_color(tint.text)
            .child(dot(self.tone, cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(div().flex_none().font_semibold().child(self.need))
                    .child(div().min_w_0().truncate().child(ANSWER)),
            )
            .when_some(self.next, |this, next| {
                this.child(
                    Button::new("next-needing-you")
                        .small()
                        .max_w(px(280.))
                        .label(format!("Next: {next}"))
                        .child(Icon::new(ExtraIcon::ArrowRight).xsmall())
                        .tooltip_with_action(
                            "Open the next chat that needs you",
                            &NextNeedingYou,
                            None,
                        )
                        .on_click(move |_, window, cx| {
                            focus.dispatch_action(&NextNeedingYou, window, cx);
                        }),
                )
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use agentshub_core::agent::Agent;
    use agentshub_core::status::{Need, Status};

    use super::{ANSWER, need};

    fn banner(agent: Agent, running: Option<Option<Status>>) -> Option<String> {
        need(agent, running).map(|need| format!("{need} {ANSWER}"))
    }

    #[test]
    fn the_banner_names_the_agent_and_what_it_needs() {
        let blocked = |need| Some(Some(Status::Blocked(need)));
        assert_eq!(
            banner(Agent::Codex, blocked(Some(Need::Permission))).as_deref(),
            Some("Codex needs your permission. Answer it in the terminal below.")
        );
        assert_eq!(
            banner(Agent::Claude, blocked(Some(Need::Question))).as_deref(),
            Some("Claude Code has a question. Answer it in the terminal below.")
        );
        assert_eq!(
            banner(Agent::Pi, blocked(Some(Need::Auth))).as_deref(),
            Some("Pi needs you to sign in. Answer it in the terminal below.")
        );
        assert_eq!(
            banner(Agent::Claude, blocked(None)).as_deref(),
            Some("Claude Code is waiting for you. Answer it in the terminal below.")
        );
    }

    #[test]
    fn only_a_chat_that_waits_on_the_user_has_a_banner() {
        for running in [
            None,
            Some(None),
            Some(Some(Status::Idle)),
            Some(Some(Status::Working)),
            Some(Some(Status::Done)),
            Some(Some(Status::Error)),
        ] {
            assert_eq!(banner(Agent::Claude, running), None, "{running:?}");
        }
    }
}
