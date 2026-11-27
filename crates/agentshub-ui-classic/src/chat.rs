//! The chat sheet's header, the toolbar and the footer.

use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::assets::IconName as ExtraIcon;
use gpui_kit::component::TitleBar;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::{
    ActiveTheme as _, IconName, Selectable as _, Sizable as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use agentshub_core::agent::Agent;
use agentshub_core::format::Turn;

use agentshub_ui_kit::actions::{
    ArchiveChat, DeleteChat, NextNeedingYou, OpenSearch, RenameChat, ShowRunning, ShowShortcuts,
    ToggleHistory, TogglePin, ToggleShell,
};
use agentshub_ui_kit::chat_body::{ChatBody, ChatIntent};
use agentshub_ui_kit::editor::{EditorChoice, editor_button};
use agentshub_ui_kit::layout::NavEvent;
use agentshub_ui_kit::status::{Tone, badge, status_pill};
use agentshub_ui_kit::theme as appearance;
use agentshub_ui_kit::widgets::{path_line, stay_put};

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

type ToolbarOn = Rc<dyn Fn(NavEvent, &mut Window, &mut App)>;

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
                (search_on)(NavEvent::OpenSearch, window, cx);
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
                (close_on)(NavEvent::CloseWindow, window, cx);
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
                                    (menu_on)(NavEvent::RunningMenu(*open), window, cx);
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

const MIN_SEARCH_KEYS_WIDTH: Pixels = px(600.);
const TITLE_BAR_HEIGHT: Pixels = px(48.);
