//! What shows under a chat's header: its notices, the history over the
//! terminal, and the project's shell under them, which the user can resize.

use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::base::{InteractiveElementExt as _, ResizeHandleRenderer, ResizeHandleState};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::resizable::{HandleEdge, resize_handle, resize_handle_appearance};
use gpui_kit::component::{
    ActiveTheme as _, IconName, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use agentshub_app::settings::DEFAULT_SHELL_SHARE;
use agentshub_core::editors::Editor;

use crate::actions::ToggleShell;
use crate::history::History;
use crate::terminal_view::TerminalView;
use crate::theme as appearance;
use crate::widgets::path_line;

/// How much of a window the earlier sessions take at most.
pub const HISTORY_SHARE: f32 = 0.45;
/// The least room the terminal keeps under them.
pub const MIN_TERMINAL_HEIGHT: Pixels = px(160.);
/// The least room the shell keeps.
pub const MIN_SHELL_HEIGHT: Pixels = px(96.);
/// The most of a window the shell takes.
pub const MAX_SHELL_SHARE: f32 = 0.75;
/// How long the shell takes to slide in.
pub const SHELL_IN: Duration = Duration::from_millis(220);

/// What the chat asks the workspace to do.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ChatIntent {
    Restart,
    ToggleHistory,
    ToggleShell,
    ResizeShell(f32),
    /// Opens the chat's folder in that editor.
    OpenEditor(Editor),
    /// Makes that editor the one chats open in.
    SetEditor(Editor),
}

/// What a chat's controls ask the workspace for.
pub type On = Rc<dyn Fn(ChatIntent, &mut Window, &mut App)>;
type Save = Rc<dyn Fn(&mut App)>;

/// The shell beside the chat, with the folder it runs in.
#[derive(Clone)]
pub struct Shell {
    pub cwd: PathBuf,
    pub view: Entity<TerminalView>,
}

/// What shows under a chat's header: its notices, the earlier sessions over
/// its terminal, and the shell.
pub struct ChatBody {
    /// The chat's id, which tells its elements apart from another chat's.
    pub id: i64,
    /// Something about the session to show above the terminal.
    pub notice: Option<SharedString>,
    /// Why the CLI didn't start or how it exited, shown with a button that
    /// restarts it.
    pub message: Option<SharedString>,
    /// The chat's earlier sessions, over its terminal.
    pub history: Entity<History>,
    /// The view of its terminal, unless its CLI didn't start.
    pub terminal: Option<Entity<TerminalView>>,
    /// The project's shell, while it shows.
    pub shell: Option<Shell>,
    /// How much of the window's height the shell takes.
    pub shell_share: f32,
    /// Whether the chat, rather than the settings, has the sheet.
    pub active: bool,
    /// Asks the workspace for what the chat's controls do.
    pub on: On,
    /// Saves the shell's height once a drag ends.
    pub save: Save,
}

impl ChatBody {
    #[expect(
        clippy::too_many_lines,
        reason = "the body's element tree: notices, history, terminal and shell"
    )]
    pub fn render(&self, window: &Window, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let id = self.id;
        let viewport = window.viewport_size().height;
        let shell_height = shell_height(self.shell_share, viewport);
        let history_max = if self.terminal.is_none() {
            viewport
                - px(140.)
                - if self.shell.is_some() {
                    shell_height
                } else {
                    px(0.)
                }
        } else {
            viewport * HISTORY_SHARE
        };

        let on = Rc::clone(&self.on);
        v_flex()
            .flex_1()
            .min_h_0()
            .when_some(self.notice.clone(), |this, notice| {
                this.child(
                    div()
                        .flex_none()
                        .px_6()
                        .py_2()
                        .border_b_1()
                        .border_color(theme.border)
                        .bg(theme.muted)
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(notice),
                )
            })
            .when_some(self.message.clone(), |this, message| {
                let tint = appearance::palette(cx).danger;
                let restart = Rc::clone(&on);
                this.child(
                    h_flex()
                        .flex_none()
                        .px_6()
                        .py_2()
                        .gap_3()
                        .border_b_1()
                        .border_color(theme.border)
                        .bg(tint.soft)
                        .child(
                            div()
                                .flex_1()
                                .text_sm()
                                .text_color(tint.text)
                                .child(message),
                        )
                        .child(
                            Button::new("restart-agent")
                                .small()
                                .label("Restart agent")
                                .on_click(move |_, window, cx| {
                                    (restart)(ChatIntent::Restart, window, cx);
                                }),
                        ),
                )
            })
            .child(over_shell(
                v_flex()
                    .size_full()
                    .children(self.history.read(cx).height(history_max).map(|height| {
                        self.history.clone().cached(
                            StyleRefinement::default()
                                .w_full()
                                .h(height)
                                .flex_shrink(1.)
                                .min_h_0(),
                        )
                    }))
                    .children(self.terminal.clone().map(|terminal| {
                        let focused =
                            self.shell.is_some() && terminal.focus_handle(cx).is_focused(window);
                        let on = Rc::clone(&on);
                        div()
                            .relative()
                            .flex_1()
                            .min_h(MIN_TERMINAL_HEIGHT)
                            .pl_5()
                            .pr_2()
                            .pt_3()
                            .when(focused, |this| this.child(focus_bar(cx)))
                            .child(terminal)
                            .when(focused, |this| {
                                this.child(div().id(("focused", id.cast_unsigned())).on_click(
                                    move |_, window, cx| {
                                        (on)(ChatIntent::ToggleShell, window, cx);
                                    },
                                ))
                            })
                    })),
                if self.terminal.is_some() {
                    MIN_TERMINAL_HEIGHT
                } else {
                    px(0.)
                },
                self.shell.clone(),
                self.shell_share,
                self.active,
                Rc::clone(&self.on),
                Rc::clone(&self.save),
                window,
                cx,
            ))
            .into_any_element()
    }
}

/// The shell under `above`, when it shows.
#[expect(
    clippy::too_many_arguments,
    reason = "the shell's layout and callbacks"
)]
pub fn over_shell(
    above: impl IntoElement,
    above_min: Pixels,
    shell: Option<Shell>,
    shell_share: f32,
    active: bool,
    on: On,
    save: Save,
    window: &Window,
    cx: &App,
) -> AnyElement {
    let drag_on = Rc::clone(&on);
    v_flex()
        .flex_1()
        .min_h_0()
        .overflow_hidden()
        .on_drag_move(move |event: &DragMoveEvent<ResizeShell>, window, cx| {
            let room = event.bounds.size.height - above_min;
            let height = (event.bounds.bottom() - event.event.position.y).min(room);
            let share = self::shell_share(height, window.viewport_size().height);
            (drag_on)(ChatIntent::ResizeShell(share), window, cx);
        })
        .child(div().flex_1().min_h(above_min).child(above))
        .children(shell_panel(
            shell,
            shell_share,
            active,
            on,
            save,
            window,
            cx,
        ))
        .into_any_element()
}

fn shell_panel(
    shell: Option<Shell>,
    shell_share: f32,
    active: bool,
    on: On,
    save: Save,
    window: &Window,
    cx: &App,
) -> Option<AnyElement> {
    let theme = cx.theme();
    let shell = shell?;
    let viewport = window.viewport_size().height;
    let height = shell_height(shell_share, viewport);
    let focused = shell.view.focus_handle(cx).is_focused(window);
    let mut hasher = std::hash::DefaultHasher::new();
    std::hash::Hash::hash(&shell.cwd, &mut hasher);
    let key = std::hash::Hasher::finish(&hasher);
    Some(
        v_flex()
            .relative()
            .flex_shrink(1.)
            .h(height)
            .min_h(MIN_SHELL_HEIGHT)
            .child(
                h_flex()
                    .flex_none()
                    .h_8()
                    .pl_5()
                    .pr_2()
                    .gap_2()
                    .bg(theme.muted)
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .font_medium()
                            .text_color(theme.muted_foreground)
                            .child("Shell"),
                    )
                    .child(div().flex_1().min_w_0().child(path_line(&shell.cwd, cx)))
                    .child({
                        let close = Rc::clone(&on);
                        Button::new("close-shell")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .tooltip_with_action("Hide the shell", &ToggleShell, None)
                            .on_click(move |_, window, cx| {
                                close(ChatIntent::ToggleShell, window, cx);
                            })
                    }),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .pl_5()
                    .pr_2()
                    .when(focused && active, |this| this.child(focus_bar(cx)))
                    .child(shell.view.clone()),
            )
            .child({
                let drag_save = Rc::clone(&save);
                let double_on = on;
                let double_save = save;
                resize_handle::<ResizeShell, ResizeShell>("shell-edge", Axis::Vertical)
                    .inside(HandleEdge::Leading)
                    .with_appearance(shell_edge_appearance(appearance::palette(cx).border_strong))
                    .on_drag(ResizeShell, move |drag, _, _, cx| {
                        cx.stop_propagation();
                        let save = Rc::clone(&drag_save);
                        cx.new(|cx| {
                            cx.on_release(move |_, cx| {
                                (save)(cx);
                            })
                            .detach();
                            drag.clone()
                        })
                    })
                    .on_double_click(move |_, window, cx| {
                        double_on(ChatIntent::ResizeShell(DEFAULT_SHELL_SHARE), window, cx);
                        double_save(cx);
                    })
            })
            .with_animation(
                ("shell-in", key),
                Animation::new(SHELL_IN).with_easing(ease_out_quint()),
                move |shell, shown| shell.top(height * (1. - shown)),
            )
            .into_any_element(),
    )
}

/// The bar on the edge of the terminal that has the keyboard.
fn focus_bar(cx: &App) -> impl IntoElement {
    div()
        .absolute()
        .left_0()
        .top_3()
        .bottom_3()
        .w(px(3.))
        .rounded_r(px(2.))
        .bg(appearance::palette(cx).accent)
}

/// The shell's height in a window `viewport` tall.
pub fn shell_height(share: f32, viewport: Pixels) -> Pixels {
    (viewport * share.min(MAX_SHELL_SHARE)).max(MIN_SHELL_HEIGHT)
}

/// The share of a window `viewport` tall that a shell dragged `height` tall
/// takes.
pub fn shell_share(height: Pixels, viewport: Pixels) -> f32 {
    if viewport <= px(0.) {
        return DEFAULT_SHELL_SHARE;
    }
    let max = (viewport * MAX_SHELL_SHARE).max(MIN_SHELL_HEIGHT);
    height.clamp(MIN_SHELL_HEIGHT, max) / viewport
}

fn shell_edge_appearance(strong: Hsla) -> ResizeHandleRenderer {
    let usual = resize_handle_appearance();
    Rc::new(move |handle, window, cx| {
        let usual = usual(handle, window, cx)?;
        Some(
            div()
                .flex_none()
                .relative()
                .w_full()
                .h(px(1.))
                .child(usual)
                .when(handle.state() == ResizeHandleState::Idle, |this| {
                    this.child(div().absolute().inset_0().bg(strong))
                })
                .into_any_element(),
        )
    })
}

#[derive(Clone)]
struct ResizeShell;

impl Render for ResizeShell {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_SHELL_SHARE, MIN_SHELL_HEIGHT, shell_height, shell_share};
    use gpui_kit::px;

    #[test]
    fn keeps_the_shell_within_its_limits() {
        // As dragged, within its limits.
        assert_eq!(shell_share(px(300.), px(1000.)), 0.3);
        assert_eq!(shell_share(px(10.), px(1000.)), 0.096);
        assert_eq!(shell_share(px(990.), px(1000.)), MAX_SHELL_SHARE);
        // Drawn the share of the window it takes, never shorter than its
        // header and a few lines.
        assert_eq!(shell_height(0.3, px(1000.)), px(300.));
        assert_eq!(shell_height(0.3, px(200.)), MIN_SHELL_HEIGHT);
        assert_eq!(shell_height(0.99, px(1000.)), px(750.));
    }
}
