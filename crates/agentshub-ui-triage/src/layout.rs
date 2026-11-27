//! The Triage layout: one list of every project's chats, grouped by what
//! needs the user, beside the shown chat.

use std::rc::Rc;

use gpui_kit::component::TitleBar;
use gpui_kit::component::menu::PopupMenu;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::{ActiveTheme as _, Selectable, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use agentshub_runtime::Runtime;
use agentshub_ui_kit::layout::{Body, Commands, Held, LayoutView, NavEvent, Parts, ShownChat};
use agentshub_ui_kit::palette::Pick;
use agentshub_ui_kit::workspace::Workspace;

use crate::list::{SIDEBAR_WIDTH, TriageList};

/// The title bar's height, which the window is dragged by.
const TITLE_BAR_HEIGHT: Pixels = px(48.);

/// Where the running chats' menu opens from: a corner of the title bar,
/// which takes no room.
#[derive(IntoElement)]
struct MenuCorner;

impl RenderOnce for MenuCorner {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        div().size_0()
    }
}

impl Selectable for MenuCorner {
    fn selected(self, _: bool) -> Self {
        self
    }

    fn is_selected(&self) -> bool {
        false
    }
}

/// The Triage layout, which lists the chats of every project, or of the one
/// it is filtered to, by what needs the user.
pub struct TriageLayout {
    runtime: Entity<Runtime>,
    list: Entity<TriageList>,
    workspace: WeakEntity<Workspace>,
    _list_events: Subscription,
}

impl TriageLayout {
    /// The layout of `workspace`, which hears what its list asks for.
    pub fn new(
        runtime: Entity<Runtime>,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Self {
        let list = cx.new(TriageList::new);
        let events = cx.subscribe_in(&list, window, |this, _, event: &NavEvent, window, cx| {
            this.navigate(event, window, cx)
        });
        Self {
            runtime,
            list,
            workspace,
            _list_events: events,
        }
    }

    /// The title bar over the chat: what it shows, the window's controls,
    /// and the running chats' menu while it is open.
    fn render_title_bar(
        &self,
        title: Option<SharedString>,
        menu: Option<Entity<PopupMenu>>,
        cx: &App,
    ) -> AnyElement {
        let on = {
            let workspace = self.workspace.clone();
            Rc::new(move |event: NavEvent, window: &mut Window, cx: &mut App| {
                workspace
                    .update(cx, |this, cx| this.navigate(&event, window, cx))
                    .ok();
            })
        };
        let close_on = Rc::clone(&on);
        TitleBar::new()
            .h(TITLE_BAR_HEIGHT)
            .pl(px(22.))
            .bg(transparent_black())
            .border_b_1()
            .border_color(cx.theme().border)
            .on_close_window(move |_, window, cx| {
                (close_on)(NavEvent::CloseWindow, window, cx);
            })
            .child(
                h_flex()
                    .absolute()
                    .inset_0()
                    .pr_2()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(15.))
                            .font_semibold()
                            .children(title),
                    )
                    // The running chats' menu opens from the corner, as no
                    // button of the header opens it.
                    .child(
                        div().flex_none().child(
                            Popover::new("running-menu")
                                .anchor(Anchor::TopRight)
                                .appearance(false)
                                .overlay_closable(false)
                                .open(menu.is_some())
                                .when_some(menu.as_ref(), |this, menu| {
                                    this.track_focus(&menu.focus_handle(cx))
                                })
                                .trigger(MenuCorner)
                                .on_open_change(move |open, window, cx| {
                                    (on)(NavEvent::RunningMenu(*open), window, cx);
                                })
                                .content(move |_, _, _| match &menu {
                                    Some(menu) => menu.clone().into_any_element(),
                                    None => div().into_any_element(),
                                }),
                        ),
                    ),
            )
            .into_any_element()
    }

    /// The shown chat's body, which tracks the keyboard for the workspace.
    fn render_chat(shown: &ShownChat, window: &Window, cx: &App) -> AnyElement {
        v_flex()
            .size_full()
            .overflow_hidden()
            .track_focus(&shown.focus)
            .child(shown.body.render(window, cx))
            .into_any_element()
    }

    /// What the title bar says: the shown chat's title, else the selected
    /// project's name.
    fn title(&self, body: &Body, cx: &App) -> Option<SharedString> {
        let runtime = self.runtime.read(cx);
        let chats = || runtime.projects().iter().flat_map(|project| &project.chats);
        match body {
            Body::Settings(_) => None,
            Body::Chat(shown) => Some(
                chats()
                    .find(|chat| chat.id == shown.body.id)
                    .and_then(|chat| chat.title.clone())
                    .unwrap_or_else(|| "New chat".into())
                    .into(),
            ),
            Body::Empty(_) => {
                let selected = self.list.read(cx).selected_project();
                runtime
                    .projects()
                    .iter()
                    .find(|project| Some(project.id) == selected)
                    .map(|project| project.name.clone().into())
            }
        }
    }
}

impl LayoutView for TriageLayout {
    fn new_chat_projects(&self, _: &App) -> bool {
        true
    }

    fn selected_project(&self, cx: &App) -> Option<i64> {
        self.list.read(cx).selected_project()
    }

    fn new_chat_project(&self, cx: &App) -> Option<i64> {
        self.list.read(cx).new_chat_project()
    }

    fn select_project(&self, project: i64, cx: &mut App) {
        self.list
            .update(cx, |list, cx| list.select_project(project, cx));
    }

    fn select_project_at(&self, index: usize, cx: &mut App) {
        self.list.update(cx, |list, cx| list.filter_at(index, cx));
    }

    fn chat_at(&self, index: usize, cx: &App) -> Option<i64> {
        self.list.read(cx).chat_at(index)
    }

    fn next_needing_you(&self, after: Option<i64>, cx: &App) -> Option<i64> {
        self.list.read(cx).next_needing_you(after)
    }

    fn refresh(&self, cx: &mut App) {
        let runtime = self.runtime.read(cx);
        let projects = runtime.projects().to_vec();
        let running = agentshub_ui_kit::layout::running(runtime);
        let unseen = runtime
            .open_chats()
            .filter(|chat| chat.unseen)
            .map(|chat| chat.id)
            .collect();
        let active = runtime.active();
        self.list.update(cx, |list, cx| {
            list.set_chats(projects, running, unseen, active, cx)
        });
    }

    fn reveal(&self, chat: i64, cx: &mut App) {
        self.list.update(cx, |list, cx| list.reveal(chat, cx));
    }

    fn hold_numbers(&self, held: Option<Held>, cx: &mut App) {
        self.list.update(cx, |list, cx| list.set_held(held, cx));
    }

    fn commands(&self, _: &App) -> Commands {
        Commands::default()
    }

    fn pick(&self, _: Pick, _: &mut App) {}

    fn handle_action(&self, _: &dyn Action, _: &mut Window, _: &mut App) -> bool {
        false
    }

    fn render(&self, parts: Parts, window: &mut Window, cx: &mut App) -> AnyElement {
        let room = window.viewport_size().width - SIDEBAR_WIDTH;
        let title = self.title(&parts.body, cx);
        let body = match parts.body {
            Body::Settings(view) => view
                .update(cx, |view, cx| view.render(room, window, cx))
                .into_any_element(),
            Body::Chat(shown) => Self::render_chat(&shown, window, cx),
            Body::Empty(empty) => empty,
        };
        let title_bar = self.render_title_bar(title, parts.running_menu, cx);
        h_flex()
            .size_full()
            .child(
                div()
                    .w(SIDEBAR_WIDTH)
                    .h_full()
                    .flex_none()
                    // Terminal output re-renders the window many times a
                    // second; the list only needs to render again when its
                    // own state changes.
                    .child(
                        self.list
                            .clone()
                            .cached(StyleRefinement::default().size_full()),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .bg(cx.theme().background)
                    .child(title_bar)
                    .child(v_flex().flex_1().min_h_0().child(body)),
            )
            .into_any_element()
    }
}
