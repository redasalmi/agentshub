//! The chat sheet: its header, history, terminal and shell.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::assets::IconName as ExtraIcon;
use gpui_kit::base::{InteractiveElementExt as _, ResizeHandleRenderer, ResizeHandleState};
use gpui_kit::component::TitleBar;
use gpui_kit::component::button::{Button, ButtonVariants as _, DropdownButton};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::resizable::{HandleEdge, resize_handle, resize_handle_appearance};
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Selectable as _, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use agentshub_app::editors::Installed;
use agentshub_app::settings::{DEFAULT_SHELL_SHARE, Settings};
use agentshub_core::agent::Agent;
use agentshub_core::editors::{self, Editor, Found};
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
        ShowRunning,
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
    /// Opens the chat's folder in that editor.
    OpenEditor(Editor),
    /// Makes that editor the one chats open in, and opens the chat's folder.
    PickEditor(Editor),
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
    /// The editor the chat opens in, and every installed one.
    pub editors: Option<(EditorChoice, Vec<EditorChoice>)>,
    pub focus: FocusHandle,
    pub on: On,
    /// Saves the shell's height once a drag ends.
    pub save: Save,
}

impl ChatSheet {
    #[expect(
        clippy::too_many_lines,
        reason = "the chat's element tree: header, terminal, history and shell"
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

        // A spinner only earns its frames while the window is on screen.
        let spin = window.is_visible() && !self.hidden;
        let on = Rc::clone(&self.on);
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
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(sidebar::path_line(&shell.cwd, cx)),
                    )
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

impl ChatSheet {
    #[expect(
        clippy::too_many_lines,
        reason = "the header's element tree and its buttons"
    )]
    fn render_header(&self, spin: bool, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let focus = self.focus.clone();
        let on = Rc::clone(&self.on);
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
                                    .selected(self.shell.is_some())
                                    .tooltip_with_action(
                                        "Show or hide the project's shell",
                                        &ToggleShell,
                                        None,
                                    )
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
                            ),
                    ),
            )
            .into_any_element()
    }
}

/// An installed editor, with the icon its desktop entry names.
#[derive(Clone)]
pub struct EditorChoice {
    pub editor: Editor,
    pub icon: Option<PathBuf>,
}

/// The editor chats open in, and every installed one, once they are known.
pub fn editor_choices(cx: &App) -> Option<(EditorChoice, Vec<EditorChoice>)> {
    let installed = &cx.try_global::<Installed>()?.0;
    let choice = |found: &Found| EditorChoice {
        editor: found.editor,
        icon: found.icon.clone(),
    };
    let current = editors::pick(installed, cx.global::<Settings>().editor)?;
    Some((choice(current), installed.iter().map(choice).collect()))
}

/// An editor's icon, or a folder of code when none was found.
pub fn editor_icon(icon: Option<&Path>) -> AnyElement {
    match icon {
        Some(path) => img(path.to_path_buf())
            .flex_none()
            .size_4()
            .into_any_element(),
        None => Icon::new(ExtraIcon::FolderCode).small().into_any_element(),
    }
}

pub type PickEditor = Rc<dyn Fn(Editor, &mut Window, &mut App)>;

/// The installed editors with their icons, `current` checked.
pub fn editor_menu(
    menu: PopupMenu,
    current: Editor,
    all: &[EditorChoice],
    pick: &PickEditor,
) -> PopupMenu {
    all.iter().fold(menu, |menu, choice| {
        let (editor, icon) = (choice.editor, choice.icon.clone());
        let pick = Rc::clone(pick);
        menu.item(
            PopupMenuItem::element(move |_, _| {
                h_flex()
                    .gap_2()
                    .child(editor_icon(icon.as_deref()))
                    .child(editor.name())
            })
            .checked(editor == current)
            .on_click(move |_, window, cx| pick(editor, window, cx)),
        )
    })
}

/// Opens the chat's folder in the current editor, or picks another.
fn editor_button(current: &EditorChoice, all: Vec<EditorChoice>, on: On) -> impl IntoElement {
    let editor = current.editor;
    let pick: PickEditor = {
        let on = Rc::clone(&on);
        Rc::new(move |editor, window, cx| on(ChatIntent::PickEditor(editor), window, cx))
    };
    DropdownButton::new("editor")
        .ghost()
        .small()
        .button(
            Button::new("open-editor")
                .ghost()
                .small()
                .child(editor_icon(current.icon.as_deref()))
                .tooltip(format!("Open in {}", editor.name()))
                .on_click(move |_, window, cx| on(ChatIntent::OpenEditor(editor), window, cx)),
        )
        .menu_accessibility_label("Choose the editor")
        .dropdown_menu(move |menu, _, _| editor_menu(menu, editor, &all, &pick))
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

/// What the empty state asks the workspace to do.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EmptyIntent {
    StartAgent(i64, Agent),
    NewChat,
    AddProject,
    OpenChat(i64),
}

type EmptyOn = Rc<dyn Fn(EmptyIntent, &mut Window, &mut App)>;

/// A chat last worked in, as the empty state lists it.
pub struct Recent {
    pub id: i64,
    pub title: String,
    pub about: String,
    pub tone: Tone,
}

/// What shows while no chat is open.
pub struct EmptyState {
    pub selected: Option<SharedString>,
    pub agents: Option<(i64, Vec<Agent>)>,
    pub recent: Vec<Recent>,
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
                                    match &self.selected {
                                        Some(_) => "Start an agent",
                                        None => "Add a project to begin",
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
                            (Some(_), _) => {
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
                            (None, _) => {
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
                                            .child(sidebar::heading("Recent", cx))
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
                                            .child(sidebar::dot(recent.tone, cx))
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

/// What the toolbar asks the workspace to do.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ToolbarIntent {
    OpenPalette,
    RunningMenu(bool),
    CloseWindow,
}

type ToolbarOn = Rc<dyn Fn(ToolbarIntent, &mut Window, &mut App)>;

/// The search, the running chats and the window's controls.
pub struct Toolbar {
    pub width: Pixels,
    pub running: Vec<Tone>,
    pub menu: Option<Entity<PopupMenu>>,
    pub on: ToolbarOn,
}

impl Toolbar {
    pub fn render(&self, window: &Window, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let palette = appearance::palette(cx);
        let needing = self.running.iter().filter(|tone| tone.needs_you()).count();
        let working = self.running.contains(&Tone::Working);
        let count = match (needing, self.running.len()) {
            (0, 0) => None,
            (0, open) => Some(sidebar::badge(open, false, working, cx)),
            (needing, _) => Some(sidebar::badge(needing, true, false, cx)),
        };
        let menu = self.menu.clone();
        let on = Rc::clone(&self.on);
        let search_on = Rc::clone(&on);

        let search = Button::new("search")
            .outline()
            .flex_1()
            .min_w_0()
            .h_8()
            .bg(theme.background)
            .icon(IconName::Search)
            .accessibility_label("Search chats, messages, projects and commands")
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .text_color(theme.muted_foreground)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child("Search chats, messages, projects and commands"),
                    )
                    .children(
                        crate::palette::shortcut(&OpenSearch, window)
                            .filter(|_| self.width >= MIN_SEARCH_KEYS_WIDTH)
                            .map(|keys| {
                                div()
                                    .flex_none()
                                    .px_1p5()
                                    .rounded(px(4.))
                                    .bg(palette.strong)
                                    .text_xs()
                                    .font_family(theme.mono_font_family.clone())
                                    .text_color(palette.secondary_foreground)
                                    .child(keys)
                            }),
                    ),
            )
            .on_click(move |_, window, cx| {
                (search_on)(ToolbarIntent::OpenPalette, window, cx);
            });

        let running = Button::new("running")
            .ghost()
            .h_8()
            .icon(ExtraIcon::Activity)
            .label("Running")
            .children(count)
            .dropdown_caret(true)
            .tooltip_with_action("The running chats of every project", &ShowRunning, None);

        let close_on = Rc::clone(&on);
        let menu_on = on;
        TitleBar::new()
            .h(TITLE_BAR_HEIGHT)
            .pl_2()
            .bg(transparent_black())
            .border_b_0()
            .on_close_window(move |_, window, cx| {
                (close_on)(ToolbarIntent::CloseWindow, window, cx);
            })
            .child(
                h_flex()
                    .absolute()
                    .inset_0()
                    .gap_4()
                    .pr_2()
                    .justify_between()
                    .child(stay_put(h_flex().flex_1().min_w_0().max_w(px(480.))).child(search))
                    .child(
                        stay_put(div()).child(
                            Popover::new("running-menu")
                                .anchor(Anchor::TopRight)
                                .appearance(false)
                                .overlay_closable(false)
                                .open(menu.is_some())
                                .when_some(menu.as_ref(), |this, menu| {
                                    this.track_focus(&menu.focus_handle(cx))
                                })
                                .on_open_change(move |open, window, cx| {
                                    (menu_on)(ToolbarIntent::RunningMenu(*open), window, cx);
                                })
                                .trigger(running)
                                .content(move |_, _, _| match &menu {
                                    Some(menu) => menu.clone().into_any_element(),
                                    None => div().into_any_element(),
                                }),
                        ),
                    ),
            )
            .into_any_element()
    }
}

/// Keeps a press on the title bar's controls from moving the window.
fn stay_put(element: Div) -> Div {
    element
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
}

const MIN_SEARCH_KEYS_WIDTH: Pixels = px(600.);
const TITLE_BAR_HEIGHT: Pixels = px(48.);
