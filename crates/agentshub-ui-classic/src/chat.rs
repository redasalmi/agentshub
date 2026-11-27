//! The chat sheet's header, the toolbar and the footer.

use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::assets::IconName as ExtraIcon;
use gpui_kit::component::TitleBar;
use gpui_kit::component::button::{Button, ButtonVariants as _, DropdownButton};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::{
    ActiveTheme as _, IconName, Selectable as _, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use agentshub_core::agent::Agent;
use agentshub_core::format::{Turn, capitalized, duration};

use agentshub_ui_kit::actions::{
    ArchiveChat, DeleteChat, NextNeedingYou, OpenInEditor, OpenSearch, RenameChat, ShowRunning,
    ShowShortcuts, ToggleHistory, TogglePin, ToggleShell,
};
use agentshub_ui_kit::chat_body::{ChatBody, ChatIntent};
use agentshub_ui_kit::editor::{EditorChoice, SetEditor, editor_icon, editor_menu};
use agentshub_ui_kit::status::{Tone, badge, dot, live_dot};
use agentshub_ui_kit::theme as appearance;
use agentshub_ui_kit::widgets::{heading, path_line};

type On = Rc<dyn Fn(ChatIntent, &mut Window, &mut App)>;

/// What the chat sheet shows and the intents it emits.
pub struct ChatSheet {
    pub title: SharedString,
    pub pinned: bool,
    pub agent: Agent,
    pub cwd: PathBuf,
    pub tone: Tone,
    pub turn: Option<Turn>,
    pub running: bool,
    pub hidden: bool,
    pub earlier: usize,
    pub history_open: bool,
    /// The editor the chat opens in, and every installed one.
    pub editors: Option<(EditorChoice, Vec<EditorChoice>)>,
    pub focus: FocusHandle,
    /// What shows under the header.
    pub body: ChatBody,
}

impl ChatSheet {
    pub fn render(&self, window: &Window, cx: &App) -> AnyElement {
        // A spinner only earns its frames while the window is on screen.
        let spin = window.is_visible() && !self.hidden;
        v_flex()
            .size_full()
            .overflow_hidden()
            .track_focus(&self.focus)
            .child(self.render_header(spin, cx))
            .child(self.body.render(window, cx))
            .into_any_element()
    }
}

impl ChatSheet {
    #[expect(
        clippy::too_many_lines,
        reason = "the header's element tree and its buttons"
    )]
    fn render_header(&self, spin: bool, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let focus = self.focus.clone();
        let on = Rc::clone(&self.body.on);
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
                            .child(path_line(&self.cwd, cx)),
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
                                    .selected(self.body.shell.is_some())
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

/// Opens the chat's folder in the default editor; its menu changes which one
/// that is. A filled surface, the one ghost buttons take on hover, tells it
/// apart from the header's plain buttons as a control with a menu.
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
            agentshub_ui_kit::palette::shortcut(action, window)
                .map(|keys| key_hint(keys, label, cx))
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
                                            .child(heading("Recent", cx))
                                            .children(
                                                agentshub_ui_kit::palette::shortcut(
                                                    &OpenSearch,
                                                    window,
                                                )
                                                .map(|keys| {
                                                    div()
                                                        .text_xs()
                                                        .text_color(theme.muted_foreground)
                                                        .child(format!("{keys} to search them all"))
                                                }),
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
            (0, open) => Some(badge(open, false, working, cx)),
            (needing, _) => Some(badge(needing, true, false, cx)),
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
                        agentshub_ui_kit::palette::shortcut(&OpenSearch, window)
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
