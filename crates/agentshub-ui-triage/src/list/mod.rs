//! Triage's sidebar: the project filter, the search, the chats of every
//! project grouped by what needs the user, and the footer.

use std::cmp::Reverse;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::Duration;

use gpui_kit::assets::IconName as ExtraIcon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use agentshub_core::status::Status;
use agentshub_core::store::Project;
use agentshub_ui_kit::actions::OpenSearch;
use agentshub_ui_kit::layout::{Held, NavEvent};
use agentshub_ui_kit::status::Tone;
use agentshub_ui_kit::theme::{self as appearance, Tint};

use crate::groups::{Group, Kept, Row, group_of, rows};

mod card;
mod filter;

/// How wide the sidebar is.
pub(crate) const SIDEBAR_WIDTH: Pixels = px(336.);
/// How often the ages of the chats are brought up to date.
const AGE_INTERVAL: Duration = Duration::from_secs(60);

/// The chats of every project not archived, grouped by what needs the user,
/// with the project they are filtered to, if any.
pub(crate) struct TriageList {
    projects: Vec<Project>,
    /// The chats whose agent runs, with what it reports doing, if it reports
    /// that.
    running: HashMap<i64, Option<Status>>,
    /// The chats with something the user hasn't seen.
    unseen: HashSet<i64>,
    /// The chat that is shown.
    active: Option<i64>,
    /// The project the runtime has selected, which "All projects" leaves as
    /// it was.
    selected: Option<i64>,
    /// The project whose chats the list shows, or `None` for every project's.
    filter: Option<i64>,
    /// The shown chat, kept in the group it was selected in.
    kept: Option<Kept>,
    held: Option<Held>,
    /// The folded groups the user opened.
    unfolded: BTreeSet<Group>,
    _age: Task<()>,
}

impl EventEmitter<NavEvent> for TriageList {}

impl TriageList {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
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
            unseen: HashSet::new(),
            active: None,
            selected: None,
            filter: None,
            kept: None,
            held: None,
            unfolded: BTreeSet::new(),
            _age: age,
        }
    }

    /// Takes in the projects and what their chats do. A project that leaves
    /// the list leaves the filter and the selection.
    pub(crate) fn set_chats(
        &mut self,
        projects: Vec<Project>,
        running: HashMap<i64, Option<Status>>,
        unseen: HashSet<i64>,
        active: Option<i64>,
        cx: &mut Context<Self>,
    ) {
        if active != self.active {
            // Read before the chat is seen, which moves a finished one out
            // of the group it was picked in.
            let was = active.and_then(|chat| self.group(chat));
            self.kept = active.and_then(|chat| keep(chat, was));
        }
        self.projects = projects;
        self.running = running;
        self.unseen = unseen;
        self.active = active;
        if !self.filter.is_some_and(|id| self.is_listed(id)) {
            self.filter = None;
        }
        if !self.selected.is_some_and(|id| self.is_listed(id)) {
            let selected = self
                .active
                .and_then(|id| self.project_of(id))
                .filter(|id| self.is_listed(*id))
                .or_else(|| self.listed().next().map(|project| project.id));
            if selected != self.selected {
                self.selected = selected;
                cx.emit(NavEvent::Selected);
            }
        }
        cx.notify();
    }

    /// The projects the list shows the chats of: the ones not archived.
    fn listed(&self) -> impl Iterator<Item = &Project> {
        self.projects
            .iter()
            .filter(|project| project.archived_at.is_none())
    }

    fn is_listed(&self, id: i64) -> bool {
        self.listed().any(|project| project.id == id)
    }

    fn project_of(&self, chat: i64) -> Option<i64> {
        self.projects
            .iter()
            .find(|project| project.chats.iter().any(|each| each.id == chat))
            .map(|project| project.id)
    }

    /// What chat `chat`'s agent is doing, `None` when it doesn't run.
    fn running(&self, chat: i64) -> Option<Option<Status>> {
        self.running.get(&chat).copied()
    }

    /// The group chat `chat` is listed in by what its agent does, if it is
    /// listed.
    fn group(&self, chat: i64) -> Option<Group> {
        self.listed()
            .flat_map(|project| &project.chats)
            .find(|each| each.id == chat)
            .map(|each| group_of(Tone::of(self.running(chat)), each.archived_at.is_some()))
    }

    /// Selects project `project` in the runtime, unless it is.
    fn select(&mut self, project: i64, cx: &mut Context<Self>) {
        if self.selected != Some(project) {
            self.selected = Some(project);
            cx.emit(NavEvent::Selected);
        }
        cx.notify();
    }

    /// Selects project `project`, which the list then shows the chats of if
    /// it shows one project's.
    pub(crate) fn select_project(&mut self, project: i64, cx: &mut Context<Self>) {
        if self.filter.is_some() {
            self.filter = Some(project);
        }
        self.select(project, cx);
    }

    /// Shows the chats of project `filter`, selecting it, or every project's,
    /// leaving the selection as it is.
    pub(crate) fn set_filter(&mut self, filter: Option<i64>, cx: &mut Context<Self>) {
        self.filter = filter;
        match filter {
            Some(project) => self.select(project, cx),
            None => cx.notify(),
        }
    }

    /// Shows the chats of the `index`th project, counting from zero.
    pub(crate) fn filter_at(&mut self, index: usize, cx: &mut Context<Self>) {
        let id = self.listed().nth(index).map(|project| project.id);
        if let Some(id) = id {
            self.set_filter(Some(id), cx);
        }
    }

    /// Selects the project of chat `chat`, which is being shown, and lists it
    /// if the list shows another project's chats.
    pub(crate) fn reveal(&mut self, chat: i64, cx: &mut Context<Self>) {
        let Some(project) = self.project_of(chat).filter(|id| self.is_listed(*id)) else {
            return;
        };
        if self.filter.is_some() {
            self.filter = Some(project);
        }
        self.select(project, cx);
    }

    pub(crate) fn set_held(&mut self, held: Option<Held>, cx: &mut Context<Self>) {
        if self.held != held {
            self.held = held;
            cx.notify();
        }
    }

    /// The project the runtime has selected.
    pub(crate) fn selected_project(&self) -> Option<i64> {
        self.selected
            .filter(|id| self.projects.iter().any(|project| project.id == *id))
    }

    /// The project a new chat starts in: the shown chat's, else the one the
    /// list shows, else the selected one, else the first, leaving out the
    /// archived ones.
    pub(crate) fn new_chat_project(&self) -> Option<i64> {
        let listed: Vec<i64> = self.listed().map(|project| project.id).collect();
        first_listed(
            [
                self.active.and_then(|chat| self.project_of(chat)),
                self.filter,
                self.selected,
            ],
            &listed,
        )
    }

    /// The listed chats, as `rows` orders them: each listed project's chats in
    /// its order, the archived ones last archived first.
    fn rows(&self) -> Vec<Row> {
        let mut chats = Vec::new();
        let mut archived = Vec::new();
        for project in self.listed() {
            for chat in &project.chats {
                let row = (
                    chat.id,
                    Tone::of(self.running(chat.id)),
                    chat.pinned,
                    chat.archived_at.is_some(),
                    project.id,
                );
                match chat.archived_at {
                    Some(at) => archived.push((at, row)),
                    None => chats.push(row),
                }
            }
        }
        archived.sort_by_key(|(at, _)| Reverse(*at));
        chats.extend(archived.into_iter().map(|(_, row)| row));
        rows(&chats, self.filter, self.kept)
    }

    /// The `index`th chat on show, counting from zero.
    pub(crate) fn chat_at(&self, index: usize) -> Option<i64> {
        shown(self.rows(), &self.unfolded)
            .get(index)
            .map(|row| row.chat)
    }

    /// The chat to go to next among the ones that need the user: the one
    /// after chat `after` in the list, or the first.
    pub(crate) fn next_needing_you(&self, after: Option<i64>) -> Option<i64> {
        let needing: Vec<i64> = self
            .rows()
            .into_iter()
            .filter(|row| row.group == Group::NeedsYou)
            .map(|row| row.chat)
            .collect();
        next_of(&needing, after)
    }

    /// How many of project §project§'s chats need the user and how many are
    /// open, and whether one of them works.
    fn counts(&self, project: &Project) -> (usize, usize, bool) {
        let running: Vec<Option<Status>> = project
            .chats
            .iter()
            .filter_map(|chat| self.running(chat.id))
            .collect();
        let needing = running
            .iter()
            .filter(|status| Tone::of(Some(**status)).needs_you())
            .count();
        let working = running.contains(&Some(Status::Working));
        (needing, running.len(), working)
    }

    fn toggle(&mut self, group: Group, cx: &mut Context<Self>) {
        if !self.unfolded.remove(&group) {
            self.unfolded.insert(group);
        }
        cx.notify();
    }
}

/// The shown chat kept in group `was`, the one it was in when it was
/// selected, if that was Ready to review: it would leave it once seen.
fn keep(chat: i64, was: Option<Group>) -> Option<Kept> {
    (was == Some(Group::ReadyToReview)).then_some(Kept {
        chat,
        group: Group::ReadyToReview,
    })
}

/// The first of `candidates` that is `listed`, else the first listed.
fn first_listed(candidates: [Option<i64>; 3], listed: &[i64]) -> Option<i64> {
    candidates
        .into_iter()
        .flatten()
        .find(|id| listed.contains(id))
        .or_else(|| listed.first().copied())
}

/// The rows on show: the folded groups' only once `unfolded`.
fn shown(rows: Vec<Row>, unfolded: &BTreeSet<Group>) -> Vec<Row> {
    rows.into_iter()
        .filter(|row| !folds(row.group) || unfolded.contains(&row.group))
        .collect()
}

/// Whether group `group` starts folded.
fn folds(group: Group) -> bool {
    matches!(group, Group::Closed | Group::Archived)
}

/// The one after `after` in `chats`, going round, or the first.
fn next_of(chats: &[i64], after: Option<i64>) -> Option<i64> {
    let next = after
        .and_then(|id| chats.iter().position(|each| *each == id))
        .map_or(0, |at| at + 1);
    chats.get(next).or(chats.first()).copied()
}

/// What a group's heading says.
fn label(group: Group) -> &'static str {
    match group {
        Group::NeedsYou => "Needs you",
        Group::ReadyToReview => "Ready to review",
        Group::Working => "Working",
        Group::Idle => "Idle",
        Group::Closed => "Closed",
        Group::Archived => "Archived",
    }
}

/// A click handler that sends `event` from the list.
fn emit(
    list: &Entity<TriageList>,
    event: NavEvent,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    let list = list.clone();
    move |_, _, cx| list.update(cx, |_, cx| cx.emit(event.clone()))
}

/// A project's mark, `size` wide, in its color.
fn project_mark(mark: SharedString, tint: Tint, size: Pixels, cx: &App) -> Div {
    div()
        .size(size)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(size * 0.28)
        .bg(tint.soft)
        .text_color(tint.text)
        .text_size(size * 0.5)
        .font_medium()
        .font_family(cx.theme().mono_font_family.clone())
        .child(mark)
}

impl Render for TriageList {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let drop_color = appearance::palette(cx).accent_soft;
        v_flex()
            .size_full()
            .bg(theme.sidebar)
            .text_color(theme.sidebar_foreground)
            .border_r_1()
            .border_color(theme.sidebar_border)
            // Folders dragged in from a file manager are added as projects,
            // as in the other layouts.
            .drag_over::<ExternalPaths>(move |style, _, _, _| style.bg(drop_color))
            .on_drop(cx.listener(|_, dropped: &ExternalPaths, _, cx| {
                cx.emit(NavEvent::DropPaths(dropped.paths().to_vec()))
            }))
            .child(self.render_top(cx))
            .child(self.render_search(window, cx))
            .child(self.render_chats(cx))
            .child(self.render_footer(cx))
    }
}

impl TriageList {
    /// The button that opens the search.
    fn render_search(&self, window: &Window, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let palette = appearance::palette(cx);
        div().flex_none().px(px(10.)).pt_1().pb(px(6.)).child(
            Button::new("search")
                .outline()
                .w_full()
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
                        .child(div().flex_1().min_w_0().truncate().child("Search"))
                        .children(
                            agentshub_ui_kit::palette::shortcut(&OpenSearch, window).map(|keys| {
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
                .on_click(cx.listener(|_, _, _, cx| cx.emit(NavEvent::OpenSearch))),
        )
    }

    /// The chats, under the heading of their group.
    fn render_chats(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let rows = self.rows();
        let listed: Vec<&Project> = self.listed().collect();
        let mut list = v_flex()
            .id("triage-chats")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_2()
            .pb(px(14.));
        if rows.is_empty() {
            let shown = self
                .filter
                .and_then(|id| listed.iter().find(|project| project.id == id));
            let (title, text) = match shown {
                _ if listed.is_empty() => (
                    Some("No projects yet"),
                    SharedString::from("Add a folder you work in to start agents there."),
                ),
                Some(project) => (None, format!("No chats in {} yet.", project.name).into()),
                None => (None, "No chats in any project yet.".into()),
            };
            return list.child(
                v_flex()
                    .px_2()
                    .py_4()
                    .gap_2()
                    .when_some(title, |this, title| {
                        this.child(div().font_semibold().child(title))
                    })
                    .child(
                        div()
                            .text_sm()
                            .line_height(rems(1.4))
                            .text_color(theme.muted_foreground)
                            .child(text),
                    ),
            );
        }
        let mut index = 0;
        let mut at = 0;
        while at < rows.len() {
            let group = rows[at].group;
            let count = rows[at..]
                .iter()
                .take_while(|row| row.group == group)
                .count();
            let open = !folds(group) || self.unfolded.contains(&group);
            list = list.child(self.render_heading(group, count, open, cx));
            if open {
                for row in &rows[at..at + count] {
                    let found = listed.iter().find_map(|project| {
                        project
                            .chats
                            .iter()
                            .find(|chat| chat.id == row.chat)
                            .map(|chat| (*project, chat))
                    });
                    let Some((project, chat)) = found else {
                        continue;
                    };
                    list = list.child(self.render_card(index, project, chat, cx));
                    index += 1;
                }
            }
            at += count;
        }
        list
    }

    /// A group's heading, with how many chats it has. The folding ones open
    /// and close on a click.
    fn render_heading(
        &self,
        group: Group,
        count: usize,
        open: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let color = if group == Group::NeedsYou {
            Tone::NeedsYou(None).text_color(cx)
        } else {
            theme.muted_foreground
        };
        h_flex()
            .id(label(group))
            .flex_none()
            .px_2()
            .pt(px(14.))
            .pb(px(6.))
            .gap_2()
            .text_xs()
            .font_semibold()
            .text_color(color)
            .when(folds(group), |this| {
                this.cursor_pointer()
                    .hover(|style| style.text_color(theme.foreground))
                    .child(
                        Icon::new(if open {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .xsmall(),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle(group, cx)))
            })
            .child(label(group))
            .child(div().font_medium().child(count.to_string()))
    }

    /// Adding a project, and the settings.
    fn render_footer(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        h_flex()
            .flex_none()
            .p_2()
            .justify_between()
            .border_t_1()
            .border_color(theme.sidebar_border)
            .child(
                Button::new("add-project")
                    .ghost()
                    .small()
                    .icon(IconName::FolderOpen)
                    .label("Add project")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(NavEvent::AddProject))),
            )
            .child(
                Button::new("settings")
                    .ghost()
                    .icon(ExtraIcon::Settings)
                    .tooltip("Settings")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(NavEvent::OpenSettings))),
            )
    }
}

#[cfg(test)]
mod tests {
    use agentshub_core::agent::Agent;
    use agentshub_core::status::{Need, Status};

    use super::card::card_text;
    use super::{first_listed, keep, next_of, shown};
    use crate::groups::{Group, Kept, Row};

    fn row(chat: i64, group: Group) -> Row {
        Row {
            chat,
            group,
            pinned: false,
        }
    }

    #[test]
    fn a_new_chat_starts_in_the_shown_chats_project_first() {
        let listed = [1, 2, 3];
        assert_eq!(first_listed([Some(1), Some(2), Some(3)], &listed), Some(1));
        assert_eq!(first_listed([None, Some(2), Some(3)], &listed), Some(2));
        assert_eq!(first_listed([None, None, Some(3)], &listed), Some(3));
        assert_eq!(first_listed([None, None, None], &listed), Some(1));
        // An archived project is passed over.
        assert_eq!(first_listed([Some(9), None, Some(3)], &listed), Some(3));
        assert_eq!(first_listed([Some(9), None, None], &[]), None);
    }

    #[test]
    fn folded_groups_hide_their_chats_from_the_numbers() {
        let rows = vec![
            row(1, Group::NeedsYou),
            row(2, Group::Idle),
            row(3, Group::Closed),
            row(4, Group::Archived),
        ];
        let ids = |unfolded: &[Group]| -> Vec<i64> {
            shown(rows.clone(), &unfolded.iter().copied().collect())
                .into_iter()
                .map(|row| row.chat)
                .collect()
        };
        assert_eq!(ids(&[]), [1, 2]);
        assert_eq!(ids(&[Group::Archived]), [1, 2, 4]);
        assert_eq!(ids(&[Group::Closed, Group::Archived]), [1, 2, 3, 4]);
    }

    #[test]
    fn goes_round_the_chats_that_need_the_user() {
        assert_eq!(next_of(&[4, 7, 9], None), Some(4));
        assert_eq!(next_of(&[4, 7, 9], Some(4)), Some(7));
        assert_eq!(next_of(&[4, 7, 9], Some(9)), Some(4));
        // From a chat that doesn't need the user, the first.
        assert_eq!(next_of(&[4, 7, 9], Some(5)), Some(4));
        assert_eq!(next_of(&[], Some(5)), None);
    }

    #[test]
    fn only_a_chat_ready_to_review_keeps_its_group() {
        assert_eq!(
            keep(7, Some(Group::ReadyToReview)),
            Some(Kept {
                chat: 7,
                group: Group::ReadyToReview
            })
        );
        assert_eq!(keep(7, Some(Group::Working)), None);
        assert_eq!(keep(7, None), None);
    }

    #[test]
    fn a_card_says_what_the_agent_needs_or_does() {
        let text = |status| card_text(Agent::Claude, Some(Some(status)));
        assert_eq!(
            text(Status::Blocked(Some(Need::Permission))),
            "Needs permission"
        );
        assert_eq!(
            text(Status::Blocked(Some(Need::Question))),
            "Has a question"
        );
        assert_eq!(
            text(Status::Blocked(Some(Need::Auth))),
            "Needs you to sign in"
        );
        assert_eq!(
            text(Status::Blocked(None)),
            "Claude Code is waiting for you"
        );
        assert_eq!(text(Status::Working), "Claude Code is working");
        assert_eq!(text(Status::Error), "Claude Code stopped on an error");
        assert_eq!(card_text(Agent::Codex, None), "Codex isn't running");
    }
}
