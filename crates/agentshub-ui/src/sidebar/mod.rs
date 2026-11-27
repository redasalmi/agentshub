use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName as ExtraIcon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{
    ContextMenu, ContextMenuExt as _, DropdownMenu as _, PopupMenuItem,
};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use agentshub_app::settings::Settings;
use agentshub_core::status::{Need, Status};
use agentshub_core::store::{Chat, Project};

use crate::theme::{self as appearance, Tint};

actions!(sidebar, [NewChat]);

mod list_view;
mod rail;
mod rail_view;
mod status;
mod text;
mod widgets;

pub use agentshub_core::format::{activity, age, home_relative};
use rail::{key_number, project_mark};
pub use rail::{names_width, rail};

pub use status::{badge, dot, live_dot};
use status::{name_of, popping_dot};
use text::summary;
pub use text::{marks, path_line, subtitle};
use widgets::section;
pub use widgets::{group_heading, heading};

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

/// The narrowest the sidebar can be dragged.
pub const MIN_SIDEBAR_WIDTH: Pixels = px(300.);
/// The widest the sidebar can be dragged.
pub const MAX_SIDEBAR_WIDTH: Pixels = px(600.);
/// How much of the window the sidebar always leaves to the chat.
pub const MIN_MAIN_WIDTH: Pixels = px(360.);

/// The most room the rail has in a window `window_width` wide: what leaves
/// the rest of the sidebar and the chat their least room.
pub fn rail_room(window_width: Pixels) -> Pixels {
    window_width - MIN_MAIN_WIDTH - MIN_SIDEBAR_WIDTH + RAIL_WIDTH
}
const TILE_SIZE: Pixels = px(40.);
const TILE_GROUP: &str = "project-tile";
/// A project's row in the rail while it names them, and its mark there.
const ROW_HEIGHT: Pixels = px(34.);
const MARK_SIZE: Pixels = px(22.);
/// The bar beside the selected project, on the rail's edge.
const BAR_WIDTH: Pixels = px(3.);
/// A chat's status glyph, and the room it takes in a row.
const GLYPH_SIZE: Pixels = px(14.);
/// A working glyph's rotation. One slow turn still reads as motion when it
/// advances a step at a time, and a cap this low keeps the whole-window frame
/// it costs to a small share of a core.
const SPIN_PERIOD: Duration = Duration::from_millis(2500);
const SPIN_FPS: f32 = 8.;
/// How many seconds a working glyph takes to step round under reduced motion.
const REDUCED_SPIN_STEPS: u64 = 8;
/// The working glyph, drawn rather than taken from the icon set: a quiet ring
/// with a bright head reads better than a uniform arc when it steps.
const WORKING_GLYPH: &[u8] = include_bytes!("../../../../assets/working.svg");

/// How the selection bar and the filter's thumb travel to what was picked: a
/// quick spring that overshoots a little before it settles.
const SLIDE: SpringConfig = SpringConfig::new(420., 32., 1.);
/// How long a status glyph that has just come to need the user pops for, and
/// how much bigger it gets at its fullest.
const POP: Duration = Duration::from_millis(420);
const POP_GROWTH: f32 = 0.35;
/// How long each archived chat waits after the one above it to fade in when
/// the archive opens.
const STAGGER: Duration = Duration::from_millis(30);
const REVEAL: Duration = Duration::from_millis(180);
/// How long a chat takes to grow into the list when it joins it, and to fold
/// out of it when it's archived or deleted.
const JOIN: Duration = Duration::from_millis(220);
const LEAVE: Duration = Duration::from_millis(180);
/// At least as tall as a chat's row, which grows from nothing to this.
const CHAT_ROW_HEIGHT: Pixels = px(60.);

/// How many chats and projects have a shortcut of their own.
pub const NUMBERED: usize = 9;

#[derive(Clone)]
pub enum SidebarEvent {
    AddProject,
    /// Files and folders dropped on the sidebar from outside the app; the
    /// folders among them become projects.
    DropPaths(Vec<PathBuf>),
    RemoveProject(i64),
    /// Archive a project, which leaves the rail with its chats.
    ArchiveProject(i64),
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
    /// Stop a running chat's agent, which opening the chat again resumes.
    CloseChat(i64),
    DeleteChat(i64),
    /// Archive a chat, which leaves the list, stopping its agent.
    ArchiveChat(i64),
    /// Take a chat out of the archive, to the top of its list.
    RestoreChat(i64),
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

/// Which shortcut numbers the sidebar shows, while the modifier that opens
/// them is held: Alt for the chats, Ctrl for the projects.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Held {
    Chats,
    Projects,
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

    /// Where the running list puts a chat: an agent that can't go on without
    /// the user first, one at rest last.
    fn urgency(self) -> u8 {
        match self {
            Self::NeedsYou(_) => 0,
            Self::Error => 1,
            Self::Done => 2,
            Self::Working => 3,
            Self::Running => 4,
            Self::Idle => 5,
            Self::Saved => 6,
        }
    }

    /// The heading of the running list's group this tone is in.
    pub fn group(self) -> &'static str {
        match self {
            tone if tone.needs_you() => "Needs you",
            Self::Working => "Working",
            Self::Idle => "Idle",
            _ => "Running",
        }
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

/// `row` growing into the list from nothing, or, when it `leaves`, folding
/// out of it to nothing.
fn fold(row: impl IntoElement + 'static, name: (&'static str, u64), leaves: bool) -> AnyElement {
    let animation = if leaves {
        Animation::new(LEAVE).with_easing(ease_in_out)
    } else {
        Animation::new(JOIN).with_easing(ease_out_quint())
    };
    div()
        .child(row)
        .with_animation(name, animation, move |wrapper, delta| {
            let shown = if leaves { 1. - delta } else { delta };
            if shown < 1. {
                wrapper
                    .max_h(CHAT_ROW_HEIGHT * shown)
                    .overflow_hidden()
                    .opacity(shown)
            } else {
                wrapper
            }
        })
        .into_any_element()
}

/// A row of the chats' list: a chat, with its place in the list, or one
/// that is leaving it.
enum Row<'a> {
    Chat(usize, &'a Chat),
    Leaving(&'a Leaving),
}

impl Row<'_> {
    fn id(&self) -> i64 {
        match self {
            Row::Chat(_, chat) => chat.id,
            Row::Leaving(leaving) => leaving.chat.id,
        }
    }
}

/// A chat folding out of the list since it was archived or deleted, below
/// the chat it was `after` in its group, if any.
struct Leaving {
    chat: Chat,
    after: Option<i64>,
    since: Instant,
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
    /// Whether the selected project's archived chats are listed below the
    /// others.
    show_archived: bool,
    held: Option<Held>,
    /// When each chat's agent last came to need the user, for the pop its
    /// glyph gives then.
    popped: HashMap<i64, Instant>,
    /// When each chat joined the list, for the way it grows into it.
    joined: HashMap<i64, Instant>,
    leaving: Vec<Leaving>,
    _leave: Option<Task<()>>,
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
            show_archived: false,
            held: None,
            popped: HashMap::new(),
            joined: HashMap::new(),
            leaving: Vec::new(),
            _leave: None,
            _age: age,
        }
    }

    pub fn set_projects(&mut self, projects: Vec<Project>, cx: &mut Context<Self>) {
        let was_selected = self.selected;
        let was_shown: Vec<_> = self
            .shown()
            .iter()
            .map(|chat| (chat.id, chat.pinned))
            .collect();
        let mut old = std::mem::replace(&mut self.projects, projects);
        let listed = |id: i64| self.listed().any(|project| project.id == id);
        if !self.selected.is_some_and(listed) {
            let selected = self
                .active
                .and_then(|id| self.project_of(id))
                .filter(|id| listed(*id))
                .or_else(|| self.listed().next().map(|project| project.id));
            if selected != self.selected {
                self.selected = selected;
                cx.emit(SidebarEvent::Selected);
            }
        }

        let now = Instant::now();
        let unarchived = |projects: &[Project]| -> HashSet<i64> {
            projects
                .iter()
                .flat_map(|project| &project.chats)
                .filter(|chat| chat.archived_at.is_none())
                .map(|chat| chat.id)
                .collect()
        };
        let chats = unarchived(&self.projects);
        // The ones there from the start were already there.
        if !old.is_empty() {
            let had = unarchived(&old);
            self.joined
                .extend(chats.difference(&had).map(|id| (*id, now)));
        }
        self.joined.retain(|_, at| now - *at < JOIN);
        self.leaving
            .retain(|leaving| now - leaving.since < LEAVE && !chats.contains(&leaving.chat.id));
        if self.selected == was_selected {
            let mut above = [None, None];
            for (id, pinned) in was_shown {
                if !chats.contains(&id)
                    && let Some(chat) = old.iter_mut().find_map(|project| {
                        let at = project.chats.iter().position(|chat| chat.id == id)?;
                        Some(project.chats.swap_remove(at))
                    })
                {
                    self.leaving.push(Leaving {
                        chat,
                        after: above[pinned as usize],
                        since: now,
                    });
                }
                above[pinned as usize] = Some(id);
            }
        }
        if !self.leaving.is_empty() {
            self._leave = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(LEAVE).await;
                this.update(cx, |this, cx| {
                    this.leaving
                        .retain(|leaving| leaving.since.elapsed() < LEAVE);
                    cx.notify();
                })
                .ok();
            }));
        }
        cx.notify();
    }

    /// `chats` of one group of the list, pinned or not, with those leaving
    /// it where they were.
    fn rows<'a>(&'a self, chats: &[(usize, &'a Chat)], pinned: bool) -> Vec<Row<'a>> {
        let mut rows: Vec<_> = chats
            .iter()
            .map(|(index, chat)| Row::Chat(*index, chat))
            .collect();
        for leaving in self.leaving.iter().filter(|leaving| {
            leaving.chat.pinned == pinned && Some(leaving.chat.project_id) == self.selected
        }) {
            let at = rows
                .iter()
                .position(|row| Some(row.id()) == leaving.after)
                .map_or(0, |at| at + 1);
            rows.insert(at, Row::Leaving(leaving));
        }
        rows
    }

    fn render_row(&self, row: &Row, cx: &Context<Self>) -> AnyElement {
        match row {
            Row::Chat(index, chat) => {
                let rendered = self.render_chat(*index, chat, cx);
                match self.joined.get(&chat.id).filter(|at| at.elapsed() < JOIN) {
                    Some(at) => fold(rendered, ("chat-in", name_of(*at)), false),
                    None => rendered.into_any_element(),
                }
            }
            Row::Leaving(leaving) => fold(
                div()
                    .relative()
                    .child(self.render_chat(NUMBERED, &leaving.chat, cx))
                    // Gone from the data, it no longer answers the pointer.
                    .child(div().absolute().inset_0().occlude()),
                ("chat-out", leaving.chat.id.cast_unsigned()),
                true,
            ),
        }
    }

    /// Marks what each running chat is doing and which one is shown. The
    /// selected project stays as the user left it.
    pub fn set_chats(
        &mut self,
        running: HashMap<i64, Option<Status>>,
        active: Option<i64>,
        cx: &mut Context<Self>,
    ) {
        let now = Instant::now();
        for (id, status) in &running {
            let tone = Tone::of(Some(*status));
            let was = Tone::of(self.running.get(id).copied());
            if tone.needs_you() && tone != was {
                self.popped.insert(*id, now);
            }
        }
        self.popped.retain(|_, at| now - *at < POP);
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

    /// The running chats of every project in the rail, the most urgent first,
    /// and in the rail's order within the same urgency, so a chat only moves
    /// when what its agent does changes.
    pub fn running_chats(&self) -> Vec<(&Project, &Chat, Tone)> {
        let mut chats: Vec<_> = self
            .listed()
            .flat_map(|project| {
                project
                    .chats
                    .iter()
                    .filter(|chat| chat.archived_at.is_none())
                    .filter_map(move |chat| {
                        let running = self.running(chat.id)?;
                        Some((project, chat, Tone::of(Some(running))))
                    })
            })
            .collect();
        chats.sort_by_key(|(_, _, tone)| tone.urgency());
        chats
    }

    /// The chat to go to next among the ones that need the user: the one
    /// after chat `after` in the running list, or the first.
    pub fn next_needing_you(&self, after: Option<i64>) -> Option<i64> {
        let needing: Vec<i64> = self
            .running_chats()
            .into_iter()
            .filter(|(_, _, tone)| tone.needs_you())
            .map(|(_, chat, _)| chat.id)
            .collect();
        let next = after
            .and_then(|id| needing.iter().position(|each| *each == id))
            .map_or(0, |at| at + 1);
        needing.get(next).or(needing.first()).copied()
    }

    /// Every project, the archived ones too.
    pub fn projects(&self) -> &[Project] {
        &self.projects
    }

    /// The projects the rail shows: the ones not archived.
    fn listed(&self) -> impl Iterator<Item = &Project> {
        self.projects
            .iter()
            .filter(|project| project.archived_at.is_none())
    }

    /// What chat `id`'s agent is doing, `None` when it doesn't run.
    pub fn running(&self, id: i64) -> Option<Option<Status>> {
        self.running.get(&id).copied()
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
        let id = self.listed().nth(index).map(|project| project.id);
        if let Some(id) = id {
            self.select(id, cx);
        }
    }

    pub fn set_held(&mut self, held: Option<Held>, cx: &mut Context<Self>) {
        if self.held != held {
            self.held = held;
            cx.notify();
        }
    }

    pub fn filter(&self) -> Filter {
        self.filter
    }

    pub fn set_filter(&mut self, filter: Filter, cx: &mut Context<Self>) {
        self.filter = filter;
        cx.notify();
    }

    /// The selected project's chats the filter lets through, pinned first,
    /// leaving out the archived ones.
    fn shown(&self) -> Vec<&Chat> {
        self.selected()
            .into_iter()
            .flat_map(|project| &project.chats)
            .filter(|chat| chat.archived_at.is_none() && self.filter.admits(self.running(chat.id)))
            .collect()
    }

    /// A project's archived chats, the last one archived first.
    fn archived(project: &Project) -> Vec<&Chat> {
        let mut chats: Vec<_> = project
            .chats
            .iter()
            .filter(|chat| chat.archived_at.is_some())
            .collect();
        chats.sort_by_key(|chat| std::cmp::Reverse(chat.archived_at));
        chats
    }

    /// The `index`th chat of the list, counting from zero.
    pub fn chat_at(&self, index: usize) -> Option<i64> {
        self.shown().get(index).map(|chat| chat.id)
    }

    fn count(&self, project: &Project, filter: Filter) -> usize {
        project
            .chats
            .iter()
            .filter(|chat| chat.archived_at.is_none() && filter.admits(self.running(chat.id)))
            .count()
    }
}

/// A click handler that sends `event` from the sidebar.
fn emit(
    sidebar: &Entity<Sidebar>,
    event: SidebarEvent,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    let sidebar = sidebar.clone();
    move |_, _, cx| sidebar.update(cx, |_, cx| cx.emit(event.clone()))
}

/// A row being dragged over, with a line on the edge the dragged one will land
/// at: below the row when it comes from above, and above it otherwise. The
/// line takes the room of the gap beside it, so the rows stay put.
fn drop_line(style: StyleRefinement, after: bool, color: Hsla) -> StyleRefinement {
    let style = style.rounded_none().border_color(color);
    if after {
        style.border_b_2().mb(px(-2.))
    } else {
        style.border_t_2().mt(px(-2.))
    }
}

/// A chat being dragged, and what follows the pointer meanwhile.
#[derive(Clone)]
struct DraggedChat {
    id: i64,
    /// Where it is in the list.
    index: usize,
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
    /// Where it is in the rail.
    index: usize,
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

impl Sidebar {
    /// How many agents need the user, work, sit idle, or haven't said.
    pub fn summary(&self) -> String {
        summary(self.running.values())
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::px;

    use super::rail::fit_rail;
    use super::text::{initials, summary};
    use super::{RAIL_WIDTH, marks};
    use agentshub_core::status::Status;

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
            "2 need you · 1 working · 1 idle · 1 running"
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
