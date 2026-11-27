//! The Triage layout: one list of every project's chats, grouped by what
//! needs the user, beside the shown chat.

use std::rc::Rc;

use gpui_kit::component::TitleBar;
use gpui_kit::component::menu::PopupMenu;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::{ActiveTheme as _, Selectable, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use agentshub_core::store::Project;
use agentshub_runtime::Runtime;
use agentshub_ui_kit::layout::{Body, Commands, Held, LayoutView, NavEvent, Parts, ShownChat};
use agentshub_ui_kit::marks::marks;
use agentshub_ui_kit::palette::Pick;
use agentshub_ui_kit::status::Tone;
use agentshub_ui_kit::theme::{self as appearance, Tint};
use agentshub_ui_kit::workspace::Workspace;

use crate::header::{Banner, Header};
use crate::list::{SIDEBAR_WIDTH, TriageList};

/// The title bar's height: the shown chat's header, which the window is
/// dragged by.
const TITLE_BAR_HEIGHT: Pixels = px(60.);

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

    /// The title bar over the chat: `content`, the window's controls, and
    /// the running chats' menu while it is open.
    fn render_title_bar(
        &self,
        content: AnyElement,
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
                    .child(content)
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

    /// The shown chat: its header in the title bar, the banner while it
    /// needs the user, and its body. They track the keyboard for the
    /// workspace together.
    fn render_chat(
        &self,
        shown: &ShownChat,
        menu: Option<Entity<PopupMenu>>,
        window: &Window,
        cx: &App,
    ) -> AnyElement {
        let (header, banner) = match self.header(shown, window, cx) {
            Some((header, banner)) => (header.render(cx), banner.map(|banner| banner.render(cx))),
            None => (Empty.into_any_element(), None),
        };
        v_flex()
            .size_full()
            .overflow_hidden()
            .track_focus(&shown.focus)
            .child(self.render_title_bar(header, menu, cx))
            .children(banner)
            .child(shown.body.render(window, cx))
            .into_any_element()
    }

    /// What the shown chat's header and banner show, while the runtime has
    /// the chat.
    fn header(
        &self,
        shown: &ShownChat,
        window: &Window,
        cx: &App,
    ) -> Option<(Header, Option<Banner>)> {
        let runtime = self.runtime.read(cx);
        let list = self.list.read(cx);
        let id = shown.body.id;
        let chat = runtime.chat(id)?;
        let project = runtime
            .projects()
            .iter()
            .find(|project| project.chats.iter().any(|chat| chat.id == id));
        let entry = project.and_then(|project| project.chats.iter().find(|chat| chat.id == id));
        let running = list.running(id);
        let next = list
            .next_needing_you(Some(id))
            .filter(|next| *next != id)
            .map(|next| SharedString::from(title_of(runtime, next)));
        let header = Header {
            title: title_of(runtime, id).into(),
            pinned: entry.is_some_and(|entry| entry.pinned),
            project: project.and_then(|project| place(runtime, project, cx)),
            agent: chat.agent,
            branch: entry
                .and_then(|entry| entry.worktree.as_ref())
                .map(|worktree| worktree.branch().into()),
            cwd: chat.cwd.to_path_buf(),
            tone: Tone::of(running),
            turn: chat.turn,
            running: chat.running,
            spin: window.is_visible() && !runtime.hidden(),
            earlier: shown.earlier,
            history_open: shown.history_open,
            shell: shown.body.shell.is_some(),
            editors: agentshub_ui_kit::editor::editor_choices(cx),
            focus: shown.focus.clone(),
            on: Rc::clone(&shown.body.on),
        };
        let banner = Banner::new(chat.agent, running, next, shown.focus.clone());
        Some((header, banner))
    }

    /// What the title bar says while no chat shows: the selected project's
    /// name, over the empty state.
    fn render_title(&self, body: &Body, cx: &App) -> AnyElement {
        let name = match body {
            Body::Empty(_) => {
                let selected = self.list.read(cx).selected_project();
                self.runtime
                    .read(cx)
                    .projects()
                    .iter()
                    .find(|project| Some(project.id) == selected)
                    .map(|project| SharedString::from(project.name.clone()))
            }
            Body::Settings(_) | Body::Chat(_) => None,
        };
        div()
            .flex_1()
            .min_w_0()
            .truncate()
            .text_size(px(15.))
            .font_semibold()
            .children(name)
            .into_any_element()
    }
}

/// Project `project`'s name, its mark as the list's filter shows it, and
/// the mark's color, unless it is archived.
fn place(
    runtime: &Runtime,
    project: &Project,
    cx: &App,
) -> Option<(SharedString, SharedString, Tint)> {
    let listed: Vec<&Project> = runtime
        .projects()
        .iter()
        .filter(|project| project.archived_at.is_none())
        .collect();
    let at = listed.iter().position(|each| each.id == project.id)?;
    let mark = marks(listed.iter().map(|project| project.name.as_str())).swap_remove(at);
    Some((
        project.name.clone().into(),
        mark.into(),
        appearance::project_tint(project.id, cx),
    ))
}

/// Chat `id`'s title, as the list shows it.
fn title_of(runtime: &Runtime, id: i64) -> String {
    runtime
        .projects()
        .iter()
        .flat_map(|project| &project.chats)
        .find(|chat| chat.id == id)
        .and_then(|chat| chat.title.clone())
        .unwrap_or_else(|| "New chat".into())
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
        let menu = parts.running_menu;
        let main = match parts.body {
            Body::Chat(shown) => self.render_chat(&shown, menu, window, cx),
            body => {
                let title = self.render_title(&body, cx);
                let body = match body {
                    Body::Settings(view) => view
                        .update(cx, |view, cx| view.render(room, window, cx))
                        .into_any_element(),
                    Body::Empty(empty) => empty,
                    Body::Chat(_) => Empty.into_any_element(),
                };
                v_flex()
                    .size_full()
                    .child(self.render_title_bar(title, menu, cx))
                    .child(v_flex().flex_1().min_h_0().child(body))
                    .into_any_element()
            }
        };
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
                    .child(main),
            )
            .into_any_element()
    }
}
