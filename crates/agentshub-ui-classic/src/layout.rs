//! The Classic layout: the rail and the chat list beside the chat's sheet,
//! under the toolbar, with the footer at the sheet's foot.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::base::InteractiveElementExt as _;
use gpui_kit::component::menu::PopupMenu;
use gpui_kit::component::resizable::{HandleEdge, resize_handle};
use gpui_kit::component::{ActiveTheme as _, IconName, h_flex, v_flex};
use gpui_kit::*;

use crate::chat::{ChatSheet, Footer, Toolbar};
use crate::sidebar::{
    self, MAX_SIDEBAR_WIDTH, MIN_MAIN_WIDTH, MIN_SIDEBAR_WIDTH, Sidebar, SidebarEvent, rail_room,
};
use crate::workspace::Workspace;
use agentshub_app::settings::{DEFAULT_RAIL_WIDTH, DEFAULT_SIDEBAR_WIDTH, Settings};
use agentshub_runtime::{Intent, Runtime};
use agentshub_ui_kit::layout::{Body, Held, LayoutView, NavEvent, Parts, ShownChat};
use agentshub_ui_kit::palette::{Entry, Lead, Pick};
use agentshub_ui_kit::status::{Filter, Tone};

/// The Classic layout, which lists the selected project's chats in the
/// sidebar.
pub struct ClassicLayout {
    runtime: Entity<Runtime>,
    sidebar: Entity<Sidebar>,
    workspace: WeakEntity<Workspace>,
    /// How wide the user wants the sidebar, saved once a drag of its edge
    /// ends. A window too narrow for it shows it narrower.
    sidebar_width: Rc<Cell<Pixels>>,
    _sidebar_events: [Subscription; 2],
}

/// What dragging the sidebar's edge carries, and draws: nothing, since the
/// sidebar itself follows the pointer.
#[derive(Clone)]
struct ResizeSidebar;

impl Render for ResizeSidebar {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// What dragging the rail's edge carries, and draws: nothing, as for the
/// sidebar's.
#[derive(Clone)]
struct ResizeRail;

impl Render for ResizeRail {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// `width` kept within the sidebar's limits, and narrow enough to leave the
/// chat room in a window `window_width` wide. The width and the limits leave
/// out `names`, the room the projects' names take in the rail, which the
/// sidebar has on top.
fn clamp_sidebar_width(width: Pixels, window_width: Pixels, names: Pixels) -> Pixels {
    let max = MAX_SIDEBAR_WIDTH
        .min(window_width - MIN_MAIN_WIDTH - names)
        .max(MIN_SIDEBAR_WIDTH);
    width.clamp(MIN_SIDEBAR_WIDTH, max)
}

impl ClassicLayout {
    /// The layout of `workspace`, which hears what its sidebar asks for.
    pub fn new(
        runtime: Entity<Runtime>,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Self {
        let sidebar = cx.new(Sidebar::new);
        let nav = cx.subscribe_in(&sidebar, window, |this, _, event: &NavEvent, window, cx| {
            this.navigate(event, window, cx)
        });
        let own = cx.subscribe_in(
            &sidebar,
            window,
            |this, _, event: &SidebarEvent, window, cx| match event {
                SidebarEvent::ShowProjectNames(show) => this.show_project_names(*show, window, cx),
            },
        );
        Self {
            runtime,
            sidebar,
            workspace,
            sidebar_width: Rc::new(Cell::new(px(cx.global::<Settings>().sidebar_width))),
            _sidebar_events: [nav, own],
        }
    }

    /// The shown chat's sheet: its header over its body.
    fn render_chat(&self, shown: ShownChat, window: &Window, cx: &App) -> AnyElement {
        let runtime = self.runtime.read(cx);
        let id = shown.body.id;
        let Some(chat) = runtime.chat(id) else {
            return Empty.into_any_element();
        };
        let (title, pinned) = runtime
            .projects()
            .iter()
            .flat_map(|project| &project.chats)
            .find(|chat| chat.id == id)
            .map(|entry| {
                (
                    entry.title.clone().unwrap_or_else(|| "New chat".into()),
                    entry.pinned,
                )
            })
            .unwrap_or_else(|| ("New chat".into(), false));
        ChatSheet {
            title: title.into(),
            pinned,
            agent: chat.agent,
            cwd: chat.cwd.to_path_buf(),
            tone: Tone::of(self.sidebar.read(cx).running(id)),
            turn: chat.turn,
            running: chat.running,
            hidden: runtime.hidden(),
            earlier: shown.earlier,
            history_open: shown.history_open,
            editors: agentshub_ui_kit::editor::editor_choices(cx),
            focus: shown.focus,
            body: shown.body,
        }
        .render(window, cx)
    }

    /// The search, which opens the search modal, and the menu of the running
    /// chats, over the `width` the sidebar leaves.
    fn render_toolbar(
        &self,
        width: Pixels,
        menu: Option<Entity<PopupMenu>>,
        window: &Window,
        cx: &App,
    ) -> AnyElement {
        let running = self
            .sidebar
            .read(cx)
            .running_chats()
            .into_iter()
            .map(|(_, _, tone)| tone)
            .collect();
        let on = {
            let workspace = self.workspace.clone();
            Rc::new(move |event: NavEvent, window: &mut Window, cx: &mut App| {
                workspace
                    .update(cx, |this, cx| this.navigate(&event, window, cx))
                    .ok();
            })
        };
        Toolbar {
            width,
            running,
            menu,
            on,
        }
        .render(window, cx)
    }

    fn render_footer(&self, width: Pixels, window: &Window, cx: &App) -> AnyElement {
        let sidebar = self.sidebar.read(cx);
        let summary = sidebar.summary();
        let needing = sidebar.next_needing_you(None).is_some();
        Footer {
            summary: &summary,
            needing,
            width,
        }
        .render(window, cx)
    }

    /// The sidebar's right edge, which resizes it when dragged and restores
    /// its default width when double clicked.
    fn render_sidebar_edge(&self) -> impl IntoElement {
        let runtime = self.runtime.downgrade();
        let width = Rc::clone(&self.sidebar_width);
        let workspace = self.workspace.clone();
        let reset = {
            let runtime = runtime.clone();
            let width = Rc::clone(&width);
            move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                width.set(px(DEFAULT_SIDEBAR_WIDTH));
                save_sidebar_width(&runtime, width.get(), cx);
                workspace.update(cx, |_, cx| cx.notify()).ok();
            }
        };
        resize_handle("sidebar-edge", Axis::Horizontal)
            .inside(HandleEdge::Trailing)
            .on_drag(ResizeSidebar, move |drag, _, _, cx| {
                cx.stop_propagation();
                let runtime = runtime.clone();
                let width = Rc::clone(&width);
                cx.new(|cx| {
                    // The drag lets go of what it carries once it ends, which
                    // is when the width the user settled on is saved.
                    cx.on_release(move |_, cx| save_sidebar_width(&runtime, width.get(), cx))
                        .detach();
                    drag.clone()
                })
            })
            .on_double_click(reset)
    }

    /// The rail's right edge, which resizes the rail when dragged, folds it
    /// to the projects' marks when dragged narrow enough, and restores its
    /// default width when double clicked. The sidebar grows or shrinks with
    /// it, so its list keeps its width.
    fn render_rail_edge(&self, window_width: Pixels, cx: &App) -> impl IntoElement {
        let runtime = self.runtime.downgrade();
        let reset = {
            let runtime = runtime.clone();
            let sidebar = self.sidebar.clone();
            let workspace = self.workspace.clone();
            move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                resize_rail(true, DEFAULT_RAIL_WIDTH, &sidebar, &workspace, cx);
                save_rail(&runtime, cx);
            }
        };
        div()
            .absolute()
            .top_0()
            .bottom_0()
            .left_0()
            .w(sidebar::rail(window_width, cx).1)
            .child(
                resize_handle("rail-edge", Axis::Horizontal)
                    .inside(HandleEdge::Trailing)
                    .on_drag(ResizeRail, move |drag, _, _, cx| {
                        cx.stop_propagation();
                        let runtime = runtime.clone();
                        cx.new(|cx| {
                            // Saved once the drag ends, as the sidebar's width.
                            cx.on_release(move |_, cx| save_rail(&runtime, cx)).detach();
                            drag.clone()
                        })
                    })
                    .on_double_click(reset),
            )
    }
}

/// Saves the sidebar's `width`, unless it is the one saved.
fn save_sidebar_width(runtime: &WeakEntity<Runtime>, width: Pixels, cx: &mut App) {
    let width = f32::from(width);
    if cx.global::<Settings>().sidebar_width == width {
        return;
    }
    let saved = Settings::set_sidebar_width(width, cx);
    runtime
        .update(cx, |runtime, cx| {
            runtime.apply(Intent::SaveSettings(vec![saved]), cx)
        })
        .ok();
}

/// Shows the rail `width` wide, naming the projects, or with their marks
/// alone, without saving it.
fn resize_rail(
    named: bool,
    width: f32,
    sidebar: &Entity<Sidebar>,
    workspace: &WeakEntity<Workspace>,
    cx: &mut App,
) {
    let settings = *cx.global::<Settings>();
    if settings.project_names == named && settings.rail_width == width {
        return;
    }
    cx.set_global(Settings {
        project_names: named,
        rail_width: width,
        ..settings
    });
    sidebar.update(cx, |_, cx| cx.notify());
    workspace.update(cx, |_, cx| cx.notify()).ok();
}

fn save_rail(runtime: &WeakEntity<Runtime>, cx: &mut App) {
    let settings = *cx.global::<Settings>();
    let width = Settings::set_rail_width(settings.rail_width, cx);
    let names = Settings::set_project_names(settings.project_names, cx);
    runtime
        .update(cx, |runtime, cx| {
            runtime.apply(Intent::SaveSettings(vec![width, names]), cx)
        })
        .ok();
}

impl LayoutView for ClassicLayout {
    fn selected_project(&self, cx: &App) -> Option<i64> {
        self.sidebar.read(cx).selected().map(|project| project.id)
    }

    fn select_project(&self, project: i64, cx: &mut App) {
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.select(project, cx));
    }

    fn select_project_at(&self, index: usize, cx: &mut App) {
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.select_at(index, cx));
    }

    fn chat_at(&self, index: usize, cx: &App) -> Option<i64> {
        self.sidebar.read(cx).chat_at(index)
    }

    fn next_needing_you(&self, after: Option<i64>, cx: &App) -> Option<i64> {
        self.sidebar.read(cx).next_needing_you(after)
    }

    fn refresh(&self, cx: &mut App) {
        let runtime = self.runtime.read(cx);
        let projects = runtime.projects().to_vec();
        let running = agentshub_ui_kit::layout::running(runtime);
        let active = runtime.active();
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.set_projects(projects, cx);
            sidebar.set_chats(running, active, cx);
        });
    }

    fn reveal(&self, chat: i64, cx: &mut App) {
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.reveal(chat, cx));
    }

    fn hold_numbers(&self, held: Option<Held>, cx: &mut App) {
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.set_held(held, cx));
    }

    fn commands(&self, cx: &App) -> Vec<Entry> {
        let filter = self.sidebar.read(cx).filter();
        Filter::ALL
            .into_iter()
            .filter(|each| *each != filter)
            .map(|each| {
                let label = match each {
                    Filter::All => "Show all the chats",
                    Filter::NeedsYou => "Show the chats that need you",
                    Filter::Running => "Show the running chats",
                };
                Entry::new(
                    label,
                    Lead::Icon(IconName::Search.into()),
                    Pick::Filter(each),
                )
            })
            .collect()
    }

    fn pick(&self, pick: Pick, cx: &mut App) {
        if let Pick::Filter(filter) = pick {
            self.sidebar
                .update(cx, |sidebar, cx| sidebar.set_filter(filter, cx));
        }
    }

    fn render(&self, parts: Parts, window: &mut Window, cx: &mut App) -> AnyElement {
        let window_width = window.viewport_size().width;
        let names = sidebar::names_width(window_width, cx);
        let sidebar_width = clamp_sidebar_width(self.sidebar_width.get(), window_width, names);
        let room = window_width - sidebar_width - names;
        let body = match parts.body {
            Body::Settings(view) => view
                .update(cx, |view, cx| view.render(room, window, cx))
                .into_any_element(),
            Body::Chat(shown) => self.render_chat(*shown, window, cx),
            Body::Empty(empty) => empty,
        };
        let toolbar = self.render_toolbar(room, parts.running_menu, window, cx);
        let footer = self.render_footer(room, window, cx);
        let theme = cx.theme();
        let resize_sidebar = {
            let width = Rc::clone(&self.sidebar_width);
            let workspace = self.workspace.clone();
            move |event: &DragMoveEvent<ResizeSidebar>, window: &mut Window, cx: &mut App| {
                let window_width = window.viewport_size().width;
                let names = sidebar::names_width(window_width, cx);
                let dragged = clamp_sidebar_width(
                    event.event.position.x - event.bounds.left() - names,
                    window_width,
                    names,
                );
                if dragged != width.get() {
                    width.set(dragged);
                    workspace.update(cx, |_, cx| cx.notify()).ok();
                }
            }
        };
        let resize = {
            let sidebar = self.sidebar.clone();
            let workspace = self.workspace.clone();
            move |event: &DragMoveEvent<ResizeRail>, window: &mut Window, cx: &mut App| {
                let width = event.event.position.x - event.bounds.left();
                let rail_width = cx.global::<Settings>().rail_width;
                if width < sidebar::FOLD_RAIL_BELOW {
                    // Folded, it keeps the width to unfold to.
                    resize_rail(false, rail_width, &sidebar, &workspace, cx);
                } else {
                    let max = rail_room(window.viewport_size().width)
                        .min(sidebar::MAX_RAIL_WIDTH)
                        .max(sidebar::MIN_NAMED_RAIL_WIDTH);
                    let width = width.clamp(sidebar::MIN_NAMED_RAIL_WIDTH, max);
                    resize_rail(true, f32::from(width), &sidebar, &workspace, cx);
                }
            }
        };

        h_flex()
            .size_full()
            // Follows the pointer anywhere in the window while the sidebar's
            // edge is dragged, since it leaves the edge's thin band at once.
            .on_drag_move(resize_sidebar)
            // The rail's edge likewise. The rail starts where the window
            // does.
            .on_drag_move(resize)
            .child(
                div()
                    .relative()
                    .w(sidebar_width + names)
                    .h_full()
                    .flex_none()
                    // Terminal output re-renders the window many times a
                    // second; the sidebar and the history only need to render
                    // again when their own state changes.
                    .child(
                        self.sidebar
                            .clone()
                            .cached(StyleRefinement::default().size_full()),
                    )
                    .child(self.render_rail_edge(window_width, cx))
                    .child(self.render_sidebar_edge()),
            )
            // The sheet the chat is drawn on, set on the sidebar's ground
            // under the toolbar.
            .child(
                v_flex().flex_1().min_w_0().h_full().child(toolbar).child(
                    v_flex().flex_1().min_h_0().px_2().pb_2().child(
                        v_flex()
                            .flex_1()
                            .min_h_0()
                            .w_full()
                            .bg(theme.background)
                            .border_1()
                            .border_color(theme.border)
                            .rounded(theme.radius_lg)
                            .shadow_xs()
                            .child(v_flex().flex_1().min_h_0().child(body))
                            .child(footer),
                    ),
                ),
            )
            .into_any_element()
    }
}
