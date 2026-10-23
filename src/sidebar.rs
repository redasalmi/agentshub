use std::collections::{HashMap, HashSet};
use std::time::Duration;

use gpui_kit::assets::IconName as ExtraIcon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::agent::Agent;
use crate::appearance;
use crate::db::{self, Chat, Project};
use crate::status::{Need, Status};

/// How often the ages of the chats are brought up to date.
const AGE_INTERVAL: Duration = Duration::from_secs(60);

const LAMP_SIZE: Pixels = px(10.);
/// The space between a chat's lamp and its title.
const LAMP_GAP: Pixels = px(8.);

#[derive(Clone)]
pub enum SidebarEvent {
    AddProject,
    RemoveProject(i64),
    /// Start a chat in a project.
    NewChat(i64),
    OpenChat(i64),
    /// Ask for a chat's new title, starting from its current one.
    RenameChat(i64, SharedString),
    PinChat(i64, bool),
    /// Move a chat to where another one is.
    MoveChat(i64, i64),
    DeleteChat(i64),
    /// Write a copy of the database where the user asks.
    Export,
}

pub struct Sidebar {
    projects: Vec<Project>,
    /// The chats whose agent runs, with what it reports doing, if it reports
    /// that.
    running: HashMap<i64, Option<Status>>,
    collapsed: HashSet<i64>,
    /// The chat that is shown.
    active: Option<i64>,
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
            collapsed: HashSet::new(),
            active: None,
            _age: age,
        }
    }

    pub fn set_projects(&mut self, projects: Vec<Project>, cx: &mut Context<Self>) {
        self.projects = projects;
        cx.notify();
    }

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

    /// Shows a project's chats.
    pub fn expand(&mut self, project_id: i64, cx: &mut Context<Self>) {
        self.collapsed.remove(&project_id);
        cx.notify();
    }

    fn toggle_project(&mut self, project_id: i64, cx: &mut Context<Self>) {
        if !self.collapsed.remove(&project_id) {
            self.collapsed.insert(project_id);
        }
        cx.notify();
    }

    fn render_project(&self, project: &Project, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let id = project.id;
        let expanded = !self.collapsed.contains(&id);
        let group = SharedString::from(format!("project-{id}"));

        v_flex()
            .child(
                h_flex()
                    .id(group.clone())
                    .group(group.clone())
                    .h_8()
                    .pl_1p5()
                    .pr_1()
                    .gap_1p5()
                    .rounded(theme.radius)
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.sidebar_accent.opacity(0.6)))
                    .child(
                        Icon::new(if expanded {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .xsmall()
                        .text_color(theme.muted_foreground),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_semibold()
                            .child(project.name.clone()),
                    )
                    .child(
                        div()
                            .invisible()
                            .group_hover(group, |style| style.visible())
                            .child(
                                Button::new("remove-project")
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::Delete)
                                    .tooltip("Remove this project")
                                    .on_click(cx.listener(move |_, _, _, cx| {
                                        cx.stop_propagation();
                                        cx.emit(SidebarEvent::RemoveProject(id));
                                    })),
                            ),
                    )
                    .child(
                        Button::new("new-chat")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Plus)
                            .tooltip("Start an agent in this project")
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.stop_propagation();
                                cx.emit(SidebarEvent::NewChat(id));
                            })),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_project(id, cx))),
            )
            .when(expanded, |this| {
                this.children(project.chats.iter().map(|chat| self.render_chat(chat, cx)))
            })
    }

    /// A chat's lamp sits under its project's chevron. It's dim while its
    /// agent runs, and lit while that agent works.
    fn render_chat(&self, chat: &Chat, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let id = chat.id;
        let active = self.active == Some(id);
        let running = self.running.get(&id).copied();
        let color = appearance::agent_color(chat.agent, cx);
        let lamp_state = match running {
            None => Lamp::Off,
            Some(Some(Status::Working)) => Lamp::Lit,
            Some(_) => Lamp::Dim,
        };
        // What needs the user stands out.
        let activity_color = match running {
            Some(Some(Status::Blocked(_))) => theme.warning,
            Some(Some(Status::Done)) => theme.success,
            Some(Some(Status::Error)) => theme.danger,
            _ => theme.muted_foreground,
        };
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

        v_flex()
            .id(group.clone())
            .group(group.clone())
            .pl_1p5()
            .pr_2()
            .py_1p5()
            .gap_0p5()
            .rounded(theme.radius)
            .cursor_pointer()
            .map(|this| {
                if active {
                    this.bg(theme.sidebar_accent)
                        .text_color(theme.sidebar_accent_foreground)
                } else {
                    this.hover(|style| style.bg(theme.sidebar_accent.opacity(0.6)))
                }
            })
            .child(
                h_flex()
                    .gap(LAMP_GAP)
                    .child(lamp(color, lamp_state))
                    .child(div().flex_1().min_w_0().text_sm().truncate().child(title))
                    .when(pinned, |this| {
                        this.child(
                            Icon::new(ExtraIcon::Pin)
                                .xsmall()
                                .text_color(theme.muted_foreground),
                        )
                    })
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .group_hover(group.clone(), |style| style.invisible())
                            .child(age(chat.updated_at)),
                    ),
            )
            .child(
                h_flex()
                    .pl(LAMP_SIZE + LAMP_GAP)
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(activity_color)
                            .child(activity(chat.agent, running)),
                    )
                    .child(
                        h_flex()
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
                                        cx.emit(SidebarEvent::RenameChat(
                                            id,
                                            current_title.clone(),
                                        ));
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

impl Render for Sidebar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let summary = summary(self.running.values());
        let dark = theme.is_dark();

        v_flex()
            .size_full()
            .bg(theme.sidebar)
            .text_color(theme.sidebar_foreground)
            .border_r_1()
            .border_color(theme.sidebar_border)
            .child(
                h_flex()
                    .h_12()
                    .flex_none()
                    .pl_4()
                    .pr_2()
                    .gap_2()
                    .justify_between()
                    .text_xs()
                    .font_medium()
                    .text_color(theme.muted_foreground)
                    .child(summary)
                    .child(
                        Button::new("theme")
                            .ghost()
                            .small()
                            .icon(if dark { IconName::Sun } else { IconName::Moon })
                            .tooltip(if dark {
                                "Use light theme"
                            } else {
                                "Use dark theme"
                            })
                            .on_click(|_, window, cx| appearance::toggle(window, cx)),
                    ),
            )
            .child(
                v_flex()
                    .id("projects")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_2()
                    .pb_3()
                    .gap_1()
                    .children(
                        self.projects
                            .iter()
                            .map(|project| self.render_project(project, cx)),
                    ),
            )
            .child(
                h_flex()
                    .flex_none()
                    .p_2()
                    .gap_1()
                    .border_t_1()
                    .border_color(theme.sidebar_border)
                    .child(
                        Button::new("add-project")
                            .ghost()
                            .small()
                            .flex_1()
                            .icon(IconName::FolderOpen)
                            .label("Add project")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(SidebarEvent::AddProject))),
                    )
                    .child(
                        Button::new("export-database")
                            .ghost()
                            .small()
                            .icon(IconName::HardDrive)
                            .tooltip("Export a copy of the database")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(SidebarEvent::Export))),
                    ),
            )
    }
}

#[derive(Clone, Copy)]
pub enum Lamp {
    /// A ring, for a saved session.
    Off,
    /// Half filled, for an agent that runs but doesn't work.
    Dim,
    /// Filled, for an agent at work.
    Lit,
}

/// An agent's mark, in its color.
pub fn lamp(color: Hsla, state: Lamp) -> Div {
    let lamp = div()
        .size(LAMP_SIZE)
        .flex_none()
        .rounded_full()
        .border_2()
        .border_color(color);
    match state {
        Lamp::Off => lamp,
        Lamp::Dim => lamp.bg(color.opacity(0.35)),
        Lamp::Lit => lamp.bg(color),
    }
}

/// What a chat's agent is doing, or just its name while it doesn't run.
/// `Some(None)` is an agent that runs but hasn't said what it's doing.
fn activity(agent: Agent, running: Option<Option<Status>>) -> String {
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
fn summary<'a>(running: impl Iterator<Item = &'a Option<Status>>) -> String {
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

#[cfg(test)]
mod tests {
    use super::summary;
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
}
