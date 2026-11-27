//! The shown chat's header, which the window is dragged by.

use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::assets::IconName as ExtraIcon;
use gpui_kit::component::button::{Button, ButtonVariants as _, DropdownButton};
use gpui_kit::component::menu::DropdownMenu as _;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Selectable as _, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use agentshub_core::agent::Agent;
use agentshub_core::format::{Turn, capitalized, duration};
use agentshub_ui_kit::actions::{
    ArchiveChat, DeleteChat, OpenInEditor, RenameChat, ToggleHistory, TogglePin, ToggleShell,
};
use agentshub_ui_kit::chat_body::ChatIntent;
use agentshub_ui_kit::editor::{EditorChoice, SetEditor, editor_icon, editor_menu};
use agentshub_ui_kit::status::{Tone, dot, live_dot};
use agentshub_ui_kit::theme::{self as appearance, Tint};
use agentshub_ui_kit::widgets::path_line;

use crate::list::project_mark;

type On = Rc<dyn Fn(ChatIntent, &mut Window, &mut App)>;

/// The widest the project's name and the branch get in the header; the
/// folder gives way first.
const MAX_NAME_WIDTH: Pixels = px(200.);

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

/// Opens the chat's folder in the default editor; its menu changes which one
/// that is.
fn editor_button(current: &EditorChoice, all: Vec<EditorChoice>, on: On) -> impl IntoElement {
    let editor = current.editor;
    let set: SetEditor = {
        let on = Rc::clone(&on);
        Rc::new(move |editor, window, cx| on(ChatIntent::SetEditor(editor), window, cx))
    };
    let open = format!("Open in {}", editor.name());
    DropdownButton::new("editor")
        .secondary()
        .small()
        .button(
            Button::new("open-editor")
                .small()
                .child(editor_icon(current.icon.as_ref()))
                .accessibility_label(open.clone())
                .tooltip_with_action(open, &OpenInEditor, None)
                .on_click(move |_, window, cx| on(ChatIntent::OpenEditor(editor), window, cx)),
        )
        .menu_accessibility_label("Choose the default editor")
        .dropdown_menu(move |menu, _, _| editor_menu(menu, editor, &all, &set))
}

/// What the agent is doing, with how long it has been doing it.
fn status_pill(tone: Tone, turn: Option<Turn>, spin: bool, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let palette = appearance::palette(cx);
    let label = capitalized(tone.label());
    let (label, time) = match turn {
        Some(Turn::Since(start)) => (format!("{label} ·"), Some(duration(start.elapsed()))),
        Some(Turn::Took(took)) => (format!("{label} · took"), Some(duration(took))),
        None => (label, None),
    };
    let (background, foreground) = tone
        .tint(cx)
        .map(|tint| (tint.soft, tint.text))
        .unwrap_or((palette.strong, palette.secondary_foreground));
    h_flex()
        .flex_none()
        .h(px(26.))
        .px_2p5()
        .gap_2()
        .rounded_full()
        .bg(background)
        .text_color(foreground)
        .text_xs()
        .font_medium()
        .child(if spin {
            live_dot(tone, cx)
        } else {
            dot(tone, cx)
        })
        .child(h_flex().gap_1().child(label).when_some(time, |this, time| {
            this.child(
                div()
                    .font_family(theme.mono_font_family.clone())
                    .child(time),
            )
        }))
}

/// Keeps a press on the header's controls from moving the window.
fn stay_put(element: Div) -> Div {
    element
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
}
