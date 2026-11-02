use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui_kit::assets::IconName as ExtraIcon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::agent::Agent;
use crate::appearance::{self, Tint};
use crate::db::{self, Chat, Project};
use crate::settings::Settings;
use crate::status::{Need, Status};

/// How often the ages of the chats are brought up to date.
const AGE_INTERVAL: Duration = Duration::from_secs(60);

/// The rail's width with the projects' marks alone.
pub const RAIL_WIDTH: Pixels = px(60.);
/// The narrowest and widest the rail can be dragged while it names the
/// projects. Dragged narrower than `FOLD_RAIL_BELOW`, it shows their marks
/// alone.
pub const MIN_NAMED_RAIL_WIDTH: Pixels = px(120.);
pub const MAX_RAIL_WIDTH: Pixels = px(300.);
pub const FOLD_RAIL_BELOW: Pixels = px(100.);
const TILE_SIZE: Pixels = px(40.);
/// A project's row in the rail while it names them, and its mark there.
const ROW_HEIGHT: Pixels = px(34.);
const MARK_SIZE: Pixels = px(22.);
/// The bar beside the selected project, on the rail's edge.
const BAR_WIDTH: Pixels = px(3.);
const DOT_SIZE: Pixels = px(8.);

/// How many chats and projects have a shortcut of their own.
pub const NUMBERED: usize = 9;

#[derive(Clone)]
pub enum SidebarEvent {
    AddProject,
    /// Files and folders dropped on the sidebar from outside the app; the
    /// folders among them become projects.
    DropPaths(Vec<PathBuf>),
    RemoveProject(i64),
    /// Move a project to where another one is.
    MoveProject(i64, i64),
    /// Start a chat in a project.
    NewChat(i64),
    OpenChat(i64),
    /// Ask for a chat's new title, starting from its current one.
    RenameChat(i64, SharedString),
    PinChat(i64, bool),
    /// Another project was selected, or none.
    Selected,
    /// Move a chat to where another one is.
    MoveChat(i64, i64),
    DeleteChat(i64),
    OpenSettings,
    /// Name the projects in the rail, or show only their marks.
    ShowProjectNames(bool),
}

/// Which of the selected project's chats the list shows.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub enum Filter {
    #[default]
    All,
    /// The ones whose agent waits on the user, finished or failed.
    NeedsYou,
    /// The ones whose agent runs.
    Running,
}

impl Filter {
    pub const ALL: [Self; 3] = [Self::All, Self::NeedsYou, Self::Running];

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::NeedsYou => "Needs you",
            Self::Running => "Running",
        }
    }

    fn admits(self, running: Option<Option<Status>>) -> bool {
        match self {
            Self::All => true,
            Self::NeedsYou => Tone::of(running).needs_you(),
            Self::Running => running.is_some(),
        }
    }
}

/// What a chat's dot says about its agent.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tone {
    /// Not running: a saved session.
    Saved,
    /// Running without saying what it does.
    Running,
    Idle,
    Working,
    NeedsYou(Option<Need>),
    Done,
    Error,
}

impl Tone {
    /// `running` is `None` for a chat whose agent doesn't run, and
    /// `Some(None)` for one that runs but hasn't said what it's doing.
    pub fn of(running: Option<Option<Status>>) -> Self {
        match running {
            None => Self::Saved,
            Some(None) => Self::Running,
            Some(Some(Status::Idle)) => Self::Idle,
            Some(Some(Status::Working)) => Self::Working,
            Some(Some(Status::Blocked(need))) => Self::NeedsYou(need),
            Some(Some(Status::Done)) => Self::Done,
            Some(Some(Status::Error)) => Self::Error,
        }
    }

    pub fn needs_you(self) -> bool {
        matches!(self, Self::NeedsYou(_) | Self::Done | Self::Error)
    }

    /// What the agent is doing, in a few words.
    pub fn label(self) -> &'static str {
        match self {
            Self::Saved => "saved",
            Self::Running => "running",
            Self::Idle => "idle",
            Self::Working => "working",
            Self::NeedsYou(Some(Need::Permission)) => "needs permission",
            Self::NeedsYou(Some(Need::Question)) => "has a question",
            Self::NeedsYou(Some(Need::Auth)) => "needs you to sign in",
            Self::NeedsYou(None) => "waiting for you",
            Self::Done => "done",
            Self::Error => "stopped on an error",
        }
    }

    /// The status color, for the tones that have one.
    pub fn tint(self, cx: &App) -> Option<Tint> {
        let palette = appearance::palette(cx);
        match self {
            Self::Working => Some(palette.info),
            Self::NeedsYou(_) => Some(palette.warning),
            Self::Done => Some(palette.success),
            Self::Error => Some(palette.danger),
            Self::Saved | Self::Running | Self::Idle => None,
        }
    }

    /// The color of a label about this tone.
    pub fn text_color(self, cx: &App) -> Hsla {
        self.tint(cx)
            .map(|tint| tint.text)
            .unwrap_or(cx.theme().muted_foreground)
    }
}

/// A chat's status dot: filled in its status color, or a ring for an agent at
/// rest, hollow when it doesn't run.
pub fn dot(tone: Tone, cx: &App) -> Div {
    let theme = cx.theme();
    let palette = appearance::palette(cx);
    let dot = div().size(DOT_SIZE).flex_none().rounded_full();
    match tone {
        Tone::Saved => dot.border_1().border_color(palette.border_strong),
        Tone::Running | Tone::Idle => dot
            .border_1()
            .border_color(theme.muted_foreground)
            .bg(theme.muted_foreground.opacity(0.35)),
        Tone::Working => {
            let tint = palette.info;
            // A halo marks the agents at work.
            dot.bg(tint.base)
                .border_2()
                .border_color(tint.soft)
                .size(DOT_SIZE + px(4.))
        }
        tone => dot.bg(tone.tint(cx).map(|tint| tint.base).unwrap_or_default()),
    }
}

pub struct Sidebar {
    projects: Vec<Project>,
    /// The chats whose agent runs, with what it reports doing, if it reports
    /// that.
    running: HashMap<i64, Option<Status>>,
    /// The chat that is shown.
    active: Option<i64>,
    /// The project whose chats the list shows.
    selected: Option<i64>,
    filter: Filter,
    _age: Task<()>,
}

impl EventEmitter<SidebarEvent> for Sidebar {}

impl Sidebar {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let age = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(AGE_INTERVAL).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });

        Self {
            projects: Vec::new(),
            running: HashMap::new(),
            active: None,
            selected: None,
            filter: Filter::default(),
            _age: age,
        }
    }

    pub fn set_projects(&mut self, projects: Vec<Project>, cx: &mut Context<Self>) {
        self.projects = projects;
        if !self
            .selected
            .is_some_and(|id| self.projects.iter().any(|project| project.id == id))
        {
            let selected = self
                .active
                .and_then(|id| self.project_of(id))
                .or_else(|| self.projects.first().map(|project| project.id));
            if selected != self.selected {
                self.selected = selected;
                cx.emit(SidebarEvent::Selected);
            }
        }
        cx.notify();
    }

    /// Marks what each running chat is doing and which one is shown. The
    /// selected project stays as the user left it.
    pub fn set_chats(
        &mut self,
        running: HashMap<i64, Option<Status>>,
        active: Option<i64>,
        cx: &mut Context<Self>,
    ) {
        self.running = running;
        self.active = active;
        cx.notify();
    }

    /// Selects the project of chat `chat_id`, which is being shown.
    pub fn reveal(&mut self, chat_id: i64, cx: &mut Context<Self>) {
        if let Some(project) = self.project_of(chat_id) {
            self.select(project, cx);
        }
    }

    pub fn projects(&self) -> &[Project] {
        &self.projects
    }

    /// What chat `id`'s agent is doing, `None` when it doesn't run.
    pub fn running(&self, id: i64) -> Option<Option<Status>> {
        self.running.get(&id).copied()
    }

    /// Which chat `id` is, for telling the user about it away from the
    /// sidebar: its title and its project's name.
    pub fn describe(&self, id: i64) -> Option<String> {
        self.projects.iter().find_map(|project| {
            let chat = project.chats.iter().find(|chat| chat.id == id)?;
            Some(match &chat.title {
                Some(title) => format!("{title} · {}", project.name),
                None => format!("New chat in {}", project.name),
            })
        })
    }

    fn project_of(&self, chat_id: i64) -> Option<i64> {
        self.projects
            .iter()
            .find(|project| project.chats.iter().any(|chat| chat.id == chat_id))
            .map(|project| project.id)
    }

    pub fn selected(&self) -> Option<&Project> {
        self.projects
            .iter()
            .find(|project| Some(project.id) == self.selected)
    }

    pub fn select(&mut self, project_id: i64, cx: &mut Context<Self>) {
        if self.selected != Some(project_id) {
            self.selected = Some(project_id);
            cx.emit(SidebarEvent::Selected);
        }
        cx.notify();
    }

    /// Selects the `index`th project of the rail, counting from zero.
    pub fn select_at(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(project) = self.projects.get(index) {
            self.select(project.id, cx);
        }
    }

    pub fn filter(&self) -> Filter {
        self.filter
    }

    pub fn set_filter(&mut self, filter: Filter, cx: &mut Context<Self>) {
        self.filter = filter;
        cx.notify();
    }

    /// The selected project's chats the filter lets through, pinned first.
    fn shown(&self) -> Vec<&Chat> {
        self.selected()
            .into_iter()
            .flat_map(|project| &project.chats)
            .filter(|chat| self.filter.admits(self.running(chat.id)))
            .collect()
    }

    /// The `index`th chat of the list, counting from zero.
    pub fn chat_at(&self, index: usize) -> Option<i64> {
        self.shown().get(index).map(|chat| chat.id)
    }

    fn count(&self, project: &Project, filter: Filter) -> usize {
        project
            .chats
            .iter()
            .filter(|chat| filter.admits(self.running(chat.id)))
            .count()
    }

    fn render_rail(&self, window_width: Pixels, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (named, width) = rail(window_width, cx);
        let marks = marks(self.projects.iter().map(|project| project.name.as_str()));
        let settings = Button::new("settings")
            .ghost()
            .icon(ExtraIcon::Settings)
            .tooltip("Settings")
            .on_click(cx.listener(|_, _, _, cx| cx.emit(SidebarEvent::OpenSettings)));
        let toggle = Button::new("project-names")
            .ghost()
            .icon(if named {
                IconName::PanelLeftClose
            } else {
                IconName::PanelLeftOpen
            })
            .tooltip(if named {
                "Hide the projects' names"
            } else {
                "Show the projects' names"
            })
            .on_click(
                cx.listener(move |_, _, _, cx| cx.emit(SidebarEvent::ShowProjectNames(!named))),
            );

        v_flex()
            .w(width)
            .h_full()
            .flex_none()
            .py_3()
            .gap_2()
            .border_r_1()
            .border_color(theme.sidebar_border)
            .when(named, |this| {
                this.child(
                    div()
                        .px_3()
                        .pt_1()
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(theme.muted_foreground)
                        .child("PROJECTS"),
                )
            })
            .child(
                v_flex()
                    .id("project-tiles")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scroll()
                    .map(|this| {
                        if named {
                            this.px(px(6.)).gap_px()
                        } else {
                            // Room for the badges over the tiles' corners.
                            this.items_center().gap_2().pt_1()
                        }
                    })
                    .children(self.projects.iter().zip(marks).enumerate().map(
                        |(index, (project, mark))| {
                            self.render_project(index, project, mark.into(), named, cx)
                        },
                    ))
                    .child(self.render_add_project(named, cx)),
            )
            .child(if named {
                h_flex()
                    .px_2()
                    .justify_between()
                    .child(settings)
                    .child(toggle)
            } else {
                v_flex()
                    .items_center()
                    .gap_1()
                    .child(toggle)
                    .child(settings)
            })
    }

    /// A project in the rail, with how many of its chats need the user: a
    /// row with its mark and its name, or a tile with its mark alone.
    fn render_project(
        &self,
        index: usize,
        project: &Project,
        mark: SharedString,
        named: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let palette = appearance::palette(cx);
        let id = project.id;
        let selected = self.selected == Some(id);
        let tint = appearance::project_tint(id, cx);
        let tones: Vec<Tone> = project
            .chats
            .iter()
            .map(|chat| Tone::of(self.running(chat.id)))
            .collect();
        let need_you = tones.iter().filter(|tone| tone.needs_you()).count();
        // The badge is red when one of them failed.
        let alert = (need_you > 0).then(|| {
            if tones.contains(&Tone::Error) {
                palette.danger
            } else {
                palette.warning
            }
        });
        let name = SharedString::from(project.name.clone());
        // The row already shows the name, so its hint tells where it is.
        let hint = if named {
            home_relative(&project.path)
        } else {
            name.to_string()
        };
        let hint = if index < NUMBERED {
            format!("{hint}  ·  Ctrl+{}", index + 1)
        } else {
            hint
        };
        let dragged = DraggedProject {
            id,
            mark: mark.clone(),
            name: name.clone(),
        };
        let drop_color = theme.drop_target;
        let badge = |count: usize, alert: Tint| {
            div()
                .flex_none()
                .min_w(px(18.))
                .h(px(18.))
                .px_1()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(alert.text)
                .text_color(palette.on_accent)
                .text_size(px(10.))
                .font_medium()
                .child(count.to_string())
        };
        let bar = div()
            .absolute()
            .w(BAR_WIDTH)
            .rounded_r(px(2.))
            .bg(palette.accent);

        let item = if named {
            h_flex()
                .id(("project", id as u64))
                .relative()
                .flex_none()
                .h(ROW_HEIGHT)
                .px_2()
                .gap_2()
                .rounded(theme.radius)
                .map(|this| {
                    if selected {
                        this.bg(palette.accent_soft)
                    } else {
                        this.hover(|style| style.bg(theme.sidebar_accent))
                    }
                })
                .child(project_mark(mark, tint, cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .truncate()
                        .map(|this| {
                            if selected {
                                this.font_medium().text_color(theme.foreground)
                            } else {
                                this.text_color(palette.secondary_foreground)
                            }
                        })
                        .child(name),
                )
                .when_some(alert, |this, alert| this.child(badge(need_you, alert)))
                // On the rail's edge, past the list's padding.
                .when(selected, |this| {
                    this.child(bar.left(px(-6.)).top(px(9.)).bottom(px(9.)))
                })
        } else {
            div()
                .id(("project", id as u64))
                .relative()
                .flex_none()
                .size(TILE_SIZE)
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(10.))
                .border_1()
                .bg(tint.soft)
                .text_color(tint.text)
                .text_sm()
                .font_medium()
                .font_family(theme.mono_font_family.clone())
                .map(|this| {
                    if selected {
                        this.border_color(palette.accent)
                    } else {
                        this.border_color(transparent_black())
                            .hover(|style| style.border_color(palette.border_strong))
                    }
                })
                .child(mark)
                .when_some(alert, |this, alert| {
                    this.child(
                        badge(need_you, alert)
                            .absolute()
                            .top(px(-5.))
                            .right(px(-5.))
                            .border_2()
                            .border_color(theme.sidebar),
                    )
                })
                // On the rail's edge, as the tile is centered in it.
                .when(selected, |this| {
                    this.child(bar.left(px(-9.)).top(px(10.)).bottom(px(10.)))
                })
        };

        item.cursor_pointer()
            .tooltip(move |window, cx| Tooltip::new(hint.clone()).build(window, cx))
            // Projects are reordered by dropping one on another.
            .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            .drag_over::<DraggedProject>(move |style, dragged, _, _| {
                if dragged.id != id {
                    style.bg(drop_color)
                } else {
                    style
                }
            })
            .on_drop(cx.listener(move |_, dragged: &DraggedProject, _, cx| {
                if dragged.id != id {
                    cx.emit(SidebarEvent::MoveProject(dragged.id, id));
                }
            }))
            .on_click(cx.listener(move |this, _, _, cx| this.select(id, cx)))
    }

    fn render_add_project(&self, named: bool, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let palette = appearance::palette(cx);
        let item = if named {
            h_flex()
                .id("add-project")
                .flex_none()
                .h(ROW_HEIGHT)
                .px_2()
                .gap_2()
                .rounded(theme.radius)
                .text_sm()
                .text_color(theme.muted_foreground)
                .hover(|style| style.bg(theme.sidebar_accent).text_color(theme.foreground))
                .child(
                    div()
                        .size(MARK_SIZE)
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(6.))
                        .border_1()
                        .border_color(palette.border_strong)
                        .child(Icon::new(IconName::Plus).xsmall()),
                )
                .child("Add project")
        } else {
            div()
                .id("add-project")
                .flex_none()
                .size(TILE_SIZE)
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(10.))
                .border_1()
                .border_color(palette.border_strong)
                .text_color(theme.muted_foreground)
                .hover(|style| style.bg(theme.sidebar_accent))
                .child(Icon::new(IconName::Plus).small())
                .tooltip(|window, cx| Tooltip::new("Add a project").build(window, cx))
        };
        item.cursor_pointer()
            .on_click(cx.listener(|_, _, _, cx| cx.emit(SidebarEvent::AddProject)))
    }

    fn render_list(&self, project: &Project, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let id = project.id;
        let shown = self.shown();
        let (pinned, others): (Vec<_>, Vec<_>) =
            shown.iter().enumerate().partition(|(_, chat)| chat.pinned);

        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(
                v_flex()
                    .flex_none()
                    .px_3()
                    .pt_4()
                    .pb_3()
                    .gap_3()
                    .child(
                        h_flex()
                            .group("project-header")
                            .items_start()
                            .gap_2()
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .pl_1()
                                    .child(
                                        div()
                                            .font_semibold()
                                            .truncate()
                                            .child(project.name.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_family(theme.mono_font_family.clone())
                                            .text_color(theme.muted_foreground)
                                            .truncate()
                                            .child(home_relative(&project.path)),
                                    ),
                            )
                            .child(
                                div()
                                    .invisible()
                                    .group_hover("project-header", |style| style.visible())
                                    .child(
                                        Button::new("remove-project")
                                            .ghost()
                                            .small()
                                            .icon(IconName::Delete)
                                            .tooltip("Remove this project")
                                            .on_click(cx.listener(move |_, _, _, cx| {
                                                cx.emit(SidebarEvent::RemoveProject(id))
                                            })),
                                    ),
                            )
                            .child(
                                Button::new("new-chat")
                                    .primary()
                                    .small()
                                    .icon(IconName::Plus)
                                    .tooltip_with_action(
                                        "Start an agent here",
                                        &crate::workspace::NewChat,
                                        None,
                                    )
                                    .on_click(cx.listener(move |_, _, _, cx| {
                                        cx.emit(SidebarEvent::NewChat(id))
                                    })),
                            ),
                    )
                    .child(self.render_filter(project, cx)),
            )
            .child(
                v_flex()
                    .id("chats")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_2()
                    .pb_3()
                    .gap_px()
                    .when(project.chats.is_empty(), |this| {
                        this.child(self.render_none(
                            "No chats yet. Start an agent in this folder with New chat.",
                            cx,
                        ))
                    })
                    .when(!project.chats.is_empty() && shown.is_empty(), |this| {
                        this.child(self.render_none(
                            match self.filter {
                                Filter::NeedsYou => "No agent here needs you.",
                                _ => "No agent runs here.",
                            },
                            cx,
                        ))
                    })
                    .when(!pinned.is_empty(), |this| {
                        this.child(section("Pinned", cx)).children(
                            pinned
                                .iter()
                                .map(|(index, chat)| self.render_chat(*index, chat, cx)),
                        )
                    })
                    .when(!others.is_empty(), |this| {
                        this.when(!pinned.is_empty(), |this| this.child(section("Chats", cx)))
                            .children(
                                others
                                    .iter()
                                    .map(|(index, chat)| self.render_chat(*index, chat, cx)),
                            )
                    }),
            )
    }

    fn render_filter(&self, project: &Project, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let palette = appearance::palette(cx);
        h_flex()
            .p(px(3.))
            .gap(px(2.))
            .rounded(theme.radius)
            .bg(palette.strong)
            .children(Filter::ALL.map(|filter| {
                let active = self.filter == filter;
                div()
                    .id(filter.label())
                    .flex_1()
                    .h(px(26.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_1()
                    .rounded(theme.radius - px(2.))
                    .text_xs()
                    .cursor_pointer()
                    .map(|this| {
                        if active {
                            this.bg(theme.background)
                                .text_color(theme.foreground)
                                .font_medium()
                                .shadow_xs()
                        } else {
                            this.text_color(palette.secondary_foreground)
                                .hover(|style| style.text_color(theme.foreground))
                        }
                    })
                    .child(filter.label())
                    .map(|this| match self.count(project, filter) {
                        0 => this,
                        count => this.child(
                            div()
                                .font_family(theme.mono_font_family.clone())
                                .text_color(theme.muted_foreground)
                                .child(count.to_string()),
                        ),
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.set_filter(filter, cx)))
            }))
    }

    fn render_none(&self, text: &'static str, cx: &Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_4()
            .text_sm()
            .line_height(rems(1.4))
            .text_color(cx.theme().muted_foreground)
            .child(text)
    }

    /// A chat in the list, `index` being its place in it.
    fn render_chat(&self, index: usize, chat: &Chat, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let palette = appearance::palette(cx);
        let id = chat.id;
        let active = self.active == Some(id);
        let tone = Tone::of(self.running(id));
        let group = SharedString::from(format!("chat-{id}"));
        let title = SharedString::from(chat.title.clone().unwrap_or_else(|| "New chat".into()));
        let current_title = SharedString::from(chat.title.clone().unwrap_or_default());
        let pinned = chat.pinned;
        let order_group = (chat.project_id, pinned);
        let dragged = DraggedChat {
            id,
            group: order_group,
            title: title.clone(),
        };
        let drop_color = theme.drop_target;
        let row_bg = if active {
            palette.accent_soft
        } else {
            theme.sidebar_accent
        };

        h_flex()
            .id(group.clone())
            .group(group.clone())
            .relative()
            .flex_none()
            .px_2()
            .py(px(7.))
            .gap(px(10.))
            .rounded(theme.radius)
            .cursor_pointer()
            .map(|this| {
                if active {
                    this.bg(row_bg)
                } else {
                    this.hover(|style| style.bg(row_bg))
                }
            })
            .child(
                div()
                    .w(DOT_SIZE + px(4.))
                    .flex()
                    .justify_center()
                    .child(dot(tone, cx)),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_sm()
                            .truncate()
                            .when(active, |this| this.font_medium())
                            .child(title),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(tone.text_color(cx))
                            .truncate()
                            .child(subtitle(chat, tone)),
                    ),
            )
            .when(index < NUMBERED, |this| {
                this.child(
                    div()
                        .flex_none()
                        .px_1p5()
                        .rounded(px(4.))
                        .border_1()
                        .border_color(theme.border)
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(theme.muted_foreground)
                        .group_hover(group.clone(), |style| style.invisible())
                        .child((index + 1).to_string()),
                )
            })
            .child(
                h_flex()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right_1()
                    .pl_2()
                    .items_center()
                    .bg(row_bg)
                    .invisible()
                    .group_hover(group, |style| style.visible())
                    .child(
                        Button::new("pin-chat")
                            .ghost()
                            .xsmall()
                            .icon(if pinned {
                                ExtraIcon::PinOff
                            } else {
                                ExtraIcon::Pin
                            })
                            .tooltip(if pinned {
                                "Unpin this chat"
                            } else {
                                "Pin this chat to the top"
                            })
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.stop_propagation();
                                cx.emit(SidebarEvent::PinChat(id, !pinned));
                            })),
                    )
                    .child(
                        Button::new("rename-chat")
                            .ghost()
                            .xsmall()
                            .icon(ExtraIcon::Pencil)
                            .tooltip("Rename this chat")
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.stop_propagation();
                                cx.emit(SidebarEvent::RenameChat(id, current_title.clone()));
                            })),
                    )
                    .child(
                        Button::new("delete-chat")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Delete)
                            .tooltip("Delete this chat")
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.stop_propagation();
                                cx.emit(SidebarEvent::DeleteChat(id));
                            })),
                    ),
            )
            // Chats are reordered by dropping one on another of the same
            // project, pinned or not as it is.
            .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            .drag_over::<DraggedChat>(move |style, dragged, _, _| {
                if dragged.group == order_group && dragged.id != id {
                    style.bg(drop_color)
                } else {
                    style
                }
            })
            .on_drop(cx.listener(move |_, dragged: &DraggedChat, _, cx| {
                if dragged.group == order_group && dragged.id != id {
                    cx.emit(SidebarEvent::MoveChat(dragged.id, id));
                }
            }))
            .on_click(cx.listener(move |_, _, _, cx| cx.emit(SidebarEvent::OpenChat(id))))
    }
}

fn section(label: &'static str, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .px_2()
        .pt_3()
        .pb_1()
        .text_xs()
        .font_family(theme.mono_font_family.clone())
        .text_color(theme.muted_foreground)
        .child(label.to_uppercase())
}

/// A chat's agent, what it's doing while it runs, and how long ago it last
/// changed.
fn subtitle(chat: &Chat, tone: Tone) -> String {
    let agent = chat.agent.label();
    match tone {
        Tone::Saved => format!("{agent} · {}", age(chat.updated_at)),
        tone => format!("{agent} · {} · {}", tone.label(), age(chat.updated_at)),
    }
}

/// Whether the rail names the projects in a window `window_width` wide, and
/// how wide it is there. The settings keep the width the user wants, which a
/// narrower window only shows narrower.
pub fn rail(window_width: Pixels, cx: &App) -> (bool, Pixels) {
    let settings = cx.global::<Settings>();
    fit_rail(
        settings.project_names,
        px(settings.rail_width),
        crate::workspace::rail_room(window_width),
    )
}

/// The rail the user wants, `named` and `wanted` wide, fit in `room`: as
/// narrow as it must be, and folded to the projects' marks when even its
/// narrowest with their names doesn't fit.
fn fit_rail(named: bool, wanted: Pixels, room: Pixels) -> (bool, Pixels) {
    let room = room.min(MAX_RAIL_WIDTH);
    if named && room >= MIN_NAMED_RAIL_WIDTH {
        (true, wanted.clamp(MIN_NAMED_RAIL_WIDTH, room))
    } else {
        (false, RAIL_WIDTH)
    }
}

/// How much the rail adds to the sidebar's width beyond its marks alone in a
/// window `window_width` wide, which is all the width the user drags the
/// sidebar's edge accounts for.
pub fn names_width(window_width: Pixels, cx: &App) -> Pixels {
    rail(window_width, cx).1 - RAIL_WIDTH
}

/// A project's mark, drawn in its color.
fn project_mark(mark: SharedString, tint: Tint, cx: &App) -> Div {
    div()
        .size(MARK_SIZE)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .bg(tint.soft)
        .text_color(tint.text)
        .text_size(px(11.))
        .font_medium()
        .font_family(cx.theme().mono_font_family.clone())
        .child(mark)
}

/// The marks of projects named `names`, in the rail's order: each one's
/// initials, unless an earlier project has them. It then gets the first pair
/// of its letters starting with its first that no earlier one has, or else
/// such a three.
pub fn marks<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut taken = HashSet::new();
    names
        .into_iter()
        .map(|name| {
            let mark = candidates(name)
                .into_iter()
                .find(|mark| !taken.contains(mark))
                .unwrap_or_else(|| initials(name));
            taken.insert(mark.clone());
            mark
        })
        .collect()
}

/// The marks project `name` could have, the likeliest first.
fn candidates(name: &str) -> Vec<String> {
    let letters: Vec<char> = name
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    let mut candidates = vec![initials(name)];
    if let [first, second, rest @ ..] = letters.as_slice() {
        let pairs = std::iter::once(second).chain(rest);
        candidates.extend(pairs.map(|other| [*first, *other].iter().collect::<String>()));
        candidates.extend(
            rest.iter()
                .map(|other| [*first, *second, *other].iter().collect()),
        );
    }
    candidates
}

/// The two letters a project's mark shows unless another project has them:
/// the first of its first two words, or its first two letters.
fn initials(name: &str) -> String {
    let words: Vec<&str> = name
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    let letters: String = match words.as_slice() {
        [] => "?".into(),
        [word] => word.chars().take(2).collect(),
        [first, second, ..] => first
            .chars()
            .take(1)
            .chain(second.chars().take(1))
            .collect(),
    };
    letters.to_lowercase()
}

/// `path`, starting with `~` when it is in the home folder.
pub fn home_relative(path: &Path) -> String {
    match std::env::home_dir().and_then(|home| path.strip_prefix(home).ok().map(Path::to_owned)) {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// A chat being dragged, and what follows the pointer meanwhile.
#[derive(Clone)]
struct DraggedChat {
    id: i64,
    /// Its project, and whether it is pinned.
    group: (i64, bool),
    title: SharedString,
}

impl Render for DraggedChat {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .max_w(px(240.))
            .px_2()
            .py_1()
            .rounded(theme.radius)
            .bg(theme.sidebar_accent)
            .text_color(theme.sidebar_accent_foreground)
            .text_sm()
            .truncate()
            .shadow_md()
            .child(self.title.clone())
    }
}

/// A project being dragged, and the row that follows the pointer meanwhile.
#[derive(Clone)]
struct DraggedProject {
    id: i64,
    mark: SharedString,
    name: SharedString,
}

impl Render for DraggedProject {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        h_flex()
            .max_w(px(240.))
            .h(ROW_HEIGHT)
            .px_2()
            .gap_2()
            .rounded(theme.radius)
            .bg(theme.sidebar_accent)
            .text_color(theme.sidebar_accent_foreground)
            .text_sm()
            .shadow_md()
            .child(project_mark(
                self.mark.clone(),
                appearance::project_tint(self.id, cx),
                cx,
            ))
            .child(div().min_w_0().truncate().child(self.name.clone()))
    }
}

impl Render for Sidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let drop_color = appearance::palette(cx).accent_soft;
        h_flex()
            .size_full()
            .bg(theme.sidebar)
            .text_color(theme.sidebar_foreground)
            // Folders dragged in from a file manager are added as projects.
            // The platform only says what is dragged on the drop, so files
            // light it up too, and are turned away then.
            .drag_over::<ExternalPaths>(move |style, _, _, _| style.bg(drop_color))
            .on_drop(cx.listener(|_, dropped: &ExternalPaths, _, cx| {
                cx.emit(SidebarEvent::DropPaths(dropped.paths().to_vec()))
            }))
            .child(self.render_rail(window.viewport_size().width, cx))
            .map(|this| match self.selected() {
                Some(project) => this.child(self.render_list(project, cx)),
                None => {
                    this.child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .p_4()
                            .gap_3()
                            .child(div().font_semibold().child("No projects yet"))
                            .child(
                                div()
                                    .text_sm()
                                    .line_height(rems(1.4))
                                    .text_color(theme.muted_foreground)
                                    .child("Add a folder you work in to start agents there."),
                            )
                            .child(
                                h_flex().child(
                                    Button::new("add-first-project")
                                        .primary()
                                        .small()
                                        .icon(IconName::FolderOpen)
                                        .label("Add project")
                                        .on_click(cx.listener(|_, _, _, cx| {
                                            cx.emit(SidebarEvent::AddProject)
                                        })),
                                ),
                            ),
                    )
                }
            })
    }
}

/// What a chat's agent is doing, or just its name while it doesn't run.
/// `Some(None)` is an agent that runs but hasn't said what it's doing.
pub fn activity(agent: Agent, running: Option<Option<Status>>) -> String {
    let name = agent.display_name();
    match running {
        None => name.to_string(),
        Some(None) => format!("{name} is running"),
        Some(Some(Status::Idle)) => format!("{name} is idle"),
        Some(Some(Status::Working)) => format!("{name} is working"),
        Some(Some(Status::Blocked(Some(Need::Permission)))) => {
            format!("{name} needs your permission")
        }
        Some(Some(Status::Blocked(Some(Need::Question)))) => format!("{name} has a question"),
        Some(Some(Status::Blocked(Some(Need::Auth)))) => format!("{name} needs you to sign in"),
        Some(Some(Status::Blocked(None))) => format!("{name} is waiting for you"),
        Some(Some(Status::Done)) => format!("{name} is done"),
        Some(Some(Status::Error)) => format!("{name} stopped on an error"),
    }
}

/// How long ago something happened, given in seconds since the Unix epoch,
/// in its largest whole unit.
pub fn age(at: i64) -> String {
    let minutes = (db::now() - at).max(0) / 60;
    match minutes {
        0 => "now".to_string(),
        1..60 => format!("{minutes}m"),
        60..1440 => format!("{}h", minutes / 60),
        1440..10080 => format!("{}d", minutes / 1440),
        10080..525600 => format!("{}w", minutes / 10080),
        _ => format!("{}y", minutes / 525600),
    }
}

/// How many agents work, need the user, sit idle, or haven't said.
pub fn summary<'a>(running: impl Iterator<Item = &'a Option<Status>>) -> String {
    let (mut working, mut need_you, mut idle, mut unknown) = (0, 0, 0, 0);
    for status in running {
        match status {
            Some(Status::Working) => working += 1,
            Some(Status::Blocked(_) | Status::Done | Status::Error) => need_you += 1,
            Some(Status::Idle) => idle += 1,
            None => unknown += 1,
        }
    }
    let mut parts = Vec::new();
    if working > 0 {
        parts.push(format!("{working} working"));
    }
    match need_you {
        0 => {}
        1 => parts.push("1 needs you".to_string()),
        n => parts.push(format!("{n} need you")),
    }
    if idle > 0 {
        parts.push(format!("{idle} idle"));
    }
    if unknown > 0 {
        parts.push(format!("{unknown} running"));
    }
    if parts.is_empty() {
        "No agents running".to_string()
    } else {
        parts.join(" · ")
    }
}

impl Sidebar {
    /// How many agents work, need the user, sit idle, or haven't said.
    pub fn summary(&self) -> String {
        summary(self.running.values())
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::px;

    use super::{RAIL_WIDTH, fit_rail, initials, marks, summary};
    use crate::status::Status;

    #[test]
    fn sums_up_what_agents_do() {
        let sum = |statuses: &[Option<Status>]| summary(statuses.iter());
        assert_eq!(sum(&[]), "No agents running");
        assert_eq!(
            sum(&[
                Some(Status::Working),
                Some(Status::Done),
                Some(Status::Idle),
                None,
                Some(Status::Blocked(None)),
            ]),
            "1 working · 2 need you · 1 idle · 1 running"
        );
        // A CLI that doesn't report may well be working, so it isn't idle.
        assert_eq!(sum(&[None, None]), "2 running");
    }

    #[test]
    fn names_projects_with_two_letters() {
        assert_eq!(initials("agentshub"), "ag");
        assert_eq!(initials("simple-base"), "sb");
        assert_eq!(initials("My_Blog"), "mb");
        assert_eq!(initials("x"), "x");
        assert_eq!(initials("--"), "?");
    }

    #[test]
    fn fits_the_rail_in_the_window() {
        // Room enough: as the user wants it, within its limits.
        assert_eq!(fit_rail(true, px(300.), px(1000.)), (true, px(300.)));
        assert_eq!(fit_rail(true, px(50.), px(1000.)), (true, px(120.)));
        assert_eq!(fit_rail(true, px(900.), px(1000.)), (true, px(300.)));
        // A narrower window shows it narrower.
        assert_eq!(fit_rail(true, px(300.), px(200.)), (true, px(200.)));
        // Too narrow for the names, it folds.
        assert_eq!(fit_rail(true, px(300.), px(100.)), (false, RAIL_WIDTH));
        assert_eq!(fit_rail(false, px(300.), px(1000.)), (false, RAIL_WIDTH));
    }

    #[test]
    fn tells_projects_with_the_same_initials_apart() {
        assert_eq!(marks(["test", "terraform", "frontend"]), ["te", "tr", "fr"]);
        // A project added twice from different folders.
        assert_eq!(marks(["notes", "notes"]), ["no", "nt"]);
        // Out of pairs, it takes three letters.
        assert_eq!(marks(["ab", "ac", "abc"]), ["ab", "ac", "abc"]);
        // Out of letters, it keeps its initials.
        assert_eq!(marks(["ab", "a-b"]), ["ab", "ab"]);
    }
}
