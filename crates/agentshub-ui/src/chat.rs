//! The chat sheet: its header, history, terminal and shell.

use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::assets::IconName as ExtraIcon;
use gpui_kit::base::{InteractiveElementExt as _, ResizeHandleRenderer, ResizeHandleState};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::DropdownMenu as _;
use gpui_kit::component::resizable::{HandleEdge, resize_handle, resize_handle_appearance};
use gpui_kit::component::{
    ActiveTheme as _, IconName, Selectable as _, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use agentshub_app::settings::DEFAULT_SHELL_SHARE;
use agentshub_core::agent::Agent;
use agentshub_core::format::{Turn, capitalized, duration};

use crate::history::History;
use crate::settings::ShowShortcuts;
use crate::sidebar::{self, Tone};
use crate::terminal_view::TerminalView;
use crate::theme as appearance;

actions!(
    chat,
    [
        OpenSearch,
        NextNeedingYou,
        ToggleHistory,
        ToggleShell,
        RenameChat,
        TogglePin,
        DeleteChat,
        ArchiveChat,
    ]
);

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
}

type On = Rc<dyn Fn(ChatIntent, &mut Window, &mut App)>;
type Save = Rc<dyn Fn(&mut App)>;

/// The shell beside the chat, with the folder it runs in.
#[derive(Clone)]
pub struct Shell {
    pub cwd: PathBuf,
    pub view: Entity<TerminalView>,
}

/// What the chat sheet shows and the intents it emits.
pub struct ChatSheet {
    pub id: i64,
    pub title: SharedString,
    pub pinned: bool,
    pub agent: Agent,
    pub cwd: PathBuf,
    pub tone: Tone,
    pub turn: Option<Turn>,
    pub running: bool,
    pub notice: Option<SharedString>,
    pub message: Option<SharedString>,
    pub history: Entity<History>,
    pub terminal: Option<Entity<TerminalView>>,
    pub shell: Option<Shell>,
    pub shell_share: f32,
    pub hidden: bool,
    /// Whether the chat, rather than the settings, has the sheet.
    pub active: bool,
    pub earlier: usize,
    pub history_open: bool,
    pub focus: FocusHandle,
    pub on: On,
    /// Saves the shell's height once a drag ends.
    pub save: Save,
}

impl ChatSheet {
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

        // A spinner only earns its frames while the window is on screen.
        let spin = window.is_visible() && !self.hidden;
        let on = self.on.clone();
        v_flex()
            .size_full()
            .overflow_hidden()
            .track_focus(&self.focus)
            .child(self.render_header(spin, cx))
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
                let restart = on.clone();
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
                        let on = on.clone();
                        div()
                            .relative()
                            .flex_1()
                            .min_h(MIN_TERMINAL_HEIGHT)
                            .pl_5()
                            .pr_2()
                            .pt_3()
                            .when(focused, |this| this.child(focus_bar(cx)))
                            .child(terminal.clone())
                            .when(focused, |this| {
                                this.child(div().id(("focused", id as u64)).on_click(
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
                self.on.clone(),
                self.save.clone(),
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
    let drag_on = on.clone();
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
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(sidebar::path_line(&shell.cwd, cx)),
                    )
                    .child({
                        let close = on.clone();
                        Button::new("close-shell")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .tooltip_with_action("Hide the shell", &ToggleShell, None)
                            .on_click(move |_, window, cx| {
                                (close.clone())(ChatIntent::ToggleShell, window, cx);
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
                let drag_save = save.clone();
                let double_on = on.clone();
                let double_save = save.clone();
                resize_handle::<ResizeShell, ResizeShell>("shell-edge", Axis::Vertical)
                    .inside(HandleEdge::Leading)
                    .with_appearance(shell_edge_appearance(appearance::palette(cx).border_strong))
                    .on_drag(ResizeShell, move |drag, _, _, cx| {
                        cx.stop_propagation();
                        let save = drag_save.clone();
                        cx.new(|cx| {
                            cx.on_release(move |_, cx| {
                                (save)(cx);
                            })
                            .detach();
                            drag.clone()
                        })
                    })
                    .on_double_click(move |_, window, cx| {
                        (double_on.clone())(
                            ChatIntent::ResizeShell(DEFAULT_SHELL_SHARE),
                            window,
                            cx,
                        );
                        (double_save.clone())(cx);
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

impl ChatSheet {
    fn render_header(&self, spin: bool, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let focus = self.focus.clone();
        let on = self.on.clone();
        let pinned = self.pinned;
        h_flex()
            .flex_none()
            .flex_wrap()
            .gap_x_4()
            .gap_y_2()
            .pl_6()
            .pr_3()
            .py_3()
            .border_b_1()
            .border_color(theme.border)
            .child(
                v_flex()
                    .flex_1()
                    .min_w(MIN_TITLE_WIDTH)
                    .child(
                        div()
                            .font_family(appearance::DISPLAY)
                            .text_xl()
                            .truncate()
                            .child(self.title.clone()),
                    )
                    .child(
                        h_flex()
                            .min_w_0()
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(theme.muted_foreground)
                            .child(
                                div()
                                    .flex_none()
                                    .child(format!("{} · ", self.agent.label())),
                            )
                            .child(sidebar::path_line(&self.cwd, cx)),
                    ),
            )
            .child(
                h_flex()
                    .ml_auto()
                    .flex_wrap()
                    .justify_end()
                    .gap_x_4()
                    .gap_y_2()
                    .when(self.running, |this| {
                        this.child(status_pill(self.tone, self.turn, spin, cx))
                    })
                    .child(
                        h_flex()
                            .flex_none()
                            .gap_1()
                            .when(self.earlier > 0, |this| {
                                let on = on.clone();
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
                                            (on.clone())(ChatIntent::ToggleHistory, window, cx);
                                        }),
                                )
                            })
                            .child({
                                let on = on.clone();
                                Button::new("shell")
                                    .ghost()
                                    .small()
                                    .icon(IconName::SquareTerminal)
                                    .label("Shell")
                                    .selected(self.shell.is_some())
                                    .tooltip_with_action(
                                        "Show or hide the project's shell",
                                        &ToggleShell,
                                        None,
                                    )
                                    .on_click(move |_, window, cx| {
                                        (on.clone())(ChatIntent::ToggleShell, window, cx);
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
                            ),
                    ),
            )
            .into_any_element()
    }
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
            sidebar::live_dot(tone, cx)
        } else {
            sidebar::dot(tone, cx)
        })
        .child(h_flex().gap_1().child(label).when_some(time, |this, time| {
            this.child(
                div()
                    .font_family(theme.mono_font_family.clone())
                    .child(time),
            )
        }))
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

const MIN_TITLE_WIDTH: Pixels = px(200.);

/// The least room beside the sidebar in which the footer hints at the search.
const MIN_FOOTER_SEARCH_WIDTH: Pixels = px(560.);

/// The footer: what the agents do, and the shortcuts that help now.
pub struct Footer<'a> {
    pub summary: &'a str,
    pub needing: bool,
    pub width: Pixels,
}

impl Footer<'_> {
    pub fn render(&self, window: &Window, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let bound = |action: &dyn Action, label: &'static str| {
            crate::palette::shortcut(action, window).map(|keys| key_hint(keys, label, cx))
        };
        h_flex()
            .flex_none()
            .h_8()
            .px_4()
            .gap_4()
            .overflow_hidden()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.muted)
            .rounded_b(theme.radius_lg - px(1.))
            .text_xs()
            .font_family(theme.mono_font_family.clone())
            .text_color(theme.muted_foreground)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(self.summary.to_string()),
            )
            .when(self.needing, |this| {
                this.children(bound(&NextNeedingYou, "next that needs you"))
            })
            .when(self.width >= MIN_FOOTER_SEARCH_WIDTH, |this| {
                this.children(bound(&OpenSearch, "search"))
            })
            .children(bound(&ShowShortcuts, "shortcuts"))
            .into_any_element()
    }
}

/// A shortcut's keys, and what they do.
fn key_hint(keys: SharedString, label: &'static str, cx: &App) -> impl IntoElement {
    let palette = appearance::palette(cx);
    h_flex()
        .flex_none()
        .gap_1p5()
        .child(
            div()
                .px_1p5()
                .rounded(px(4.))
                .bg(palette.strong)
                .text_color(palette.secondary_foreground)
                .child(keys),
        )
        .child(label)
}
