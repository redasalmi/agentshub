use std::collections::HashMap;
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
use crate::status::{Need, Status};

/// How often the ages of the chats are brought up to date.
const AGE_INTERVAL: Duration = Duration::from_secs(60);

const RAIL_WIDTH: Pixels = px(60.);
const TILE_SIZE: Pixels = px(40.);
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

    fn render_rail(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .w(RAIL_WIDTH)
            .h_full()
            .flex_none()
            .items_center()
            .py_3()
            .gap_2()
            .border_r_1()
            .border_color(theme.sidebar_border)
            .child(
                v_flex()
                    .id("project-tiles")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .pt_1()
                    .overflow_y_scroll()
                    .children(
                        self.projects
                            .iter()
                            .enumerate()
                            .map(|(index, project)| self.render_tile(index, project, cx)),
                    )
                    .child(
                        div()
                            .id("add-project")
                            .flex_none()
                            .size(TILE_SIZE)
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(10.))
                            .border_1()
                            .border_color(appearance::palette(cx).border_strong)
                            .text_color(theme.muted_foreground)
                            .cursor_pointer()
                            .hover(|style| style.bg(theme.sidebar_accent))
                            .child(Icon::new(IconName::Plus).small())
                            .tooltip(|window, cx| Tooltip::new("Add a project").build(window, cx))
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(SidebarEvent::AddProject))),
                    ),
            )
            .child(
                Button::new("settings")
                    .ghost()
                    .icon(ExtraIcon::Settings)
                    .tooltip("Settings")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(SidebarEvent::OpenSettings))),
            )
    }

    /// A project's tile in the rail, with how many of its chats need the user.
    fn render_tile(&self, index: usize, project: &Project, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let palette = appearance::palette(cx);
        let id = project.id;
        let selected = self.selected == Some(id);
        let tones: Vec<Tone> = project
            .chats
            .iter()
            .map(|chat| Tone::of(self.running(chat.id)))
            .collect();
        let need_you = tones.iter().filter(|tone| tone.needs_you()).count();
        let failed = tones.contains(&Tone::Error);
        let name = SharedString::from(project.name.clone());
        let hint = if index < NUMBERED {
            format!("{name}  ·  Ctrl+{}", index + 1)
        } else {
            name.to_string()
        };
        let dragged = DraggedProject {
            id,
            initials: initials(&project.name).into(),
        };
        let drop_color = theme.drop_target;

        div()
            .id(("project", id as u64))
            .relative()
            .flex_none()
            .size(TILE_SIZE)
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(10.))
            .text_sm()
            .font_medium()
            .font_family(theme.mono_font_family.clone())
            .cursor_pointer()
            .map(|this| {
                if selected {
                    this.bg(palette.accent_soft)
                        .text_color(palette.accent)
                        .border_1()
                        .border_color(palette.accent)
                } else {
                    this.bg(palette.strong)
                        .text_color(palette.secondary_foreground)
                        .hover(|style| style.text_color(theme.foreground))
                }
            })
            .child(initials(&project.name))
            .when(need_you > 0, |this| {
                let tint = if failed {
                    palette.danger
                } else {
                    palette.warning
                };
                this.child(
                    div()
                        .absolute()
                        .top(px(-4.))
                        .right(px(-4.))
                        .min_w(px(18.))
                        .h(px(18.))
                        .px_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .border_2()
                        .border_color(theme.sidebar)
                        .bg(tint.text)
                        .text_color(palette.on_accent)
                        .text_size(px(10.))
                        .child(need_you.to_string()),
                )
            })
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

/// The two letters a project's tile shows: the first of its first two words,
/// or its first two letters.
pub fn initials(name: &str) -> String {
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

/// A project being dragged, and the tile that follows the pointer meanwhile.
#[derive(Clone)]
struct DraggedProject {
    id: i64,
    initials: SharedString,
}

impl Render for DraggedProject {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let palette = appearance::palette(cx);
        div()
            .size(TILE_SIZE)
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(10.))
            .bg(palette.strong)
            .text_color(theme.foreground)
            .text_sm()
            .font_medium()
            .font_family(theme.mono_font_family.clone())
            .shadow_md()
            .child(self.initials.clone())
    }
}

impl Render for Sidebar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
            .child(self.render_rail(cx))
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
    use super::{initials, summary};
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
}
