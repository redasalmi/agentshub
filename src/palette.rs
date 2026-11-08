//! The search modal: one field over the chats of every project, the agents
//! that can start in the selected one, the projects, the commands, and what
//! was said in the chats.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, WindowExt as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::agent::Agent;
use crate::sidebar::{self, Filter, Tone};

const WIDTH: Pixels = px(640.);
const MARGIN_TOP: Pixels = px(96.);
const MAX_HEIGHT: Pixels = px(420.);

/// How long the query has to stay the same before it is searched for.
const SEARCH_DELAY: Duration = Duration::from_millis(120);

/// Looks up what was typed, to show under the entries the modal already has.
pub type Search = Rc<dyn Fn(String, &mut App) -> Task<Vec<Entry>>>;

/// What picking an entry does.
pub enum Pick {
    OpenChat(i64),
    StartAgent(i64, Agent),
    SelectProject(i64),
    Filter(Filter),
    /// Dispatched to the workspace, as its shortcut would be.
    Run(Box<dyn Action>),
}

impl Clone for Pick {
    fn clone(&self) -> Self {
        match self {
            Self::OpenChat(id) => Self::OpenChat(*id),
            Self::StartAgent(project, agent) => Self::StartAgent(*project, *agent),
            Self::SelectProject(id) => Self::SelectProject(*id),
            Self::Filter(filter) => Self::Filter(*filter),
            Self::Run(action) => Self::Run(action.boxed_clone()),
        }
    }
}

/// How an entry starts: the status dot of a chat, or an icon.
#[derive(Clone)]
pub enum Lead {
    Tone(Tone),
    Icon(gpui_kit::assets::IconName),
}

pub struct Entry {
    pub label: SharedString,
    /// Shown dimmed after the label, and searched too.
    pub meta: Option<SharedString>,
    /// Searched, but not shown.
    pub keywords: Vec<SharedString>,
    pub lead: Lead,
    pub pick: Pick,
}

impl Entry {
    pub fn new(label: impl Into<SharedString>, lead: Lead, pick: Pick) -> Self {
        Self {
            label: label.into(),
            meta: None,
            keywords: Vec::new(),
            lead,
            pick,
        }
    }

    pub fn meta(mut self, meta: impl Into<SharedString>) -> Self {
        self.meta = Some(meta.into());
        self
    }

    pub fn keywords(mut self, keywords: impl IntoIterator<Item = impl Into<SharedString>>) -> Self {
        self.keywords.extend(keywords.into_iter().map(Into::into));
        self
    }
}

/// The shortcut bound to `action`, as the user types it.
pub fn shortcut(action: &dyn Action, window: &Window) -> Option<SharedString> {
    let binding = window.highest_precedence_binding_for_action(action)?;
    let keystroke = binding.keystrokes().first()?;
    Some(Kbd::format(keystroke.as_keystroke()).into())
}

/// Opens the modal over `sections`, each a heading and its entries, and what
/// `search` finds for the query under them, where it can't move the entry the
/// user is about to pick. Empty sections are left out.
pub fn open(
    sections: Vec<(SharedString, Vec<Entry>)>,
    search: Option<Search>,
    on_pick: impl Fn(Pick, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    let mut groups = Vec::new();
    let mut picks: Vec<Vec<Pick>> = Vec::new();
    for (heading, entries) in sections {
        if entries.is_empty() {
            continue;
        }
        let (group, group_picks) = group(heading, entries.iter(), None, window);
        groups.push(group);
        picks.push(group_picks);
    }

    let state = cx.new(|cx| CommandState::new(window, cx));
    let picks = Rc::new(picks);
    let on_pick = Rc::new(on_pick);
    let initial_focus = Cell::new(true);
    let found: Rc<RefCell<Vec<Entry>>> = Rc::default();
    let searching: Rc<Cell<Option<Task<()>>>> = Rc::default();
    window.open_dialog(cx, move |dialog, window, cx| {
        if initial_focus.replace(false) {
            state.update(cx, |state, cx| state.focus(window, cx));
        }
        let (picks, on_pick) = (picks.clone(), on_pick.clone());
        // The filter only passes what has the query in it, and a message can
        // match without having it verbatim. Keeping the query that is typed
        // now, the last results stay until the next ones replace them.
        let query = state.read(cx).query(cx);
        let (messages, message_picks) = group(
            "Messages".into(),
            found.borrow().iter(),
            Some(query),
            window,
        );
        let (found, searching, search) = (found.clone(), searching.clone(), search.clone());
        let command = groups
            .iter()
            .cloned()
            .chain((!message_picks.is_empty()).then_some(messages))
            .fold(Command::new(&state), Command::group)
            .bordered(false)
            .max_h(MAX_HEIGHT)
            .placeholder("Search chats, messages, projects and commands")
            .when_some(search, |command, search| {
                command.on_query(move |query, window, cx| {
                    let query = query.trim().to_string();
                    if query.is_empty() {
                        searching.set(None);
                        found.borrow_mut().clear();
                        window.refresh();
                        return;
                    }
                    let (found, search) = (found.clone(), search.clone());
                    // Replacing the task cancels the search it was waiting on.
                    searching.set(Some(window.spawn(cx, async move |cx| {
                        cx.background_executor().timer(SEARCH_DELAY).await;
                        let Ok(entries) = cx.update(|_, cx| search(query, cx)) else {
                            return;
                        };
                        let entries = entries.await;
                        *found.borrow_mut() = entries;
                        cx.update(|window, _| window.refresh()).ok();
                    })));
                })
            })
            .empty(|_, _, cx| {
                div()
                    .p_4()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("Nothing matches.")
            })
            .footer(|_, _, cx| {
                let theme = cx.theme();
                h_flex()
                    .px_4()
                    .py_2()
                    .gap_4()
                    .border_t_1()
                    .border_color(theme.border)
                    .bg(theme.muted)
                    .text_xs()
                    .font_family(theme.mono_font_family.clone())
                    .text_color(theme.muted_foreground)
                    .child("↑↓ move")
                    .child("↵ open")
                    .child("esc close")
            })
            .on_confirm(move |index, window, cx| {
                let Some(pick) = picks
                    .get(index.section)
                    .or((index.section == picks.len()).then_some(&message_picks))
                    .and_then(|group| group.get(index.row))
                else {
                    return;
                };
                window.close_dialog(cx);
                on_pick(pick.clone(), window, cx);
            });
        dialog
            .w(WIDTH)
            .margin_top(MARGIN_TOP)
            .close_button(false)
            .child(command)
    });
}

/// A section of the modal, and what picking each of its entries does. Besides
/// what the entries say, the filter passes them on `query`.
fn group<'a>(
    heading: SharedString,
    entries: impl Iterator<Item = &'a Entry>,
    query: Option<SharedString>,
    window: &Window,
) -> (CommandGroup, Vec<Pick>) {
    let mut group = CommandGroup::new().label(heading);
    let mut picks = Vec::new();
    for entry in entries {
        let hint = match &entry.pick {
            Pick::Run(action) => shortcut(action.as_ref(), window),
            _ => None,
        };
        let mut keywords = entry.keywords.clone();
        keywords.extend(entry.meta.clone());
        keywords.extend(query.clone());
        group = group.item(
            CommandItem::new()
                .label(entry.label.clone())
                .keywords(keywords)
                .child(row(
                    entry.label.clone(),
                    entry.meta.clone(),
                    entry.lead.clone(),
                    hint,
                )),
        );
        picks.push(entry.pick.clone());
    }
    (group, picks)
}

/// An entry's row: its dot or icon, its label, what's known about it, and its
/// shortcut.
fn row(
    label: SharedString,
    meta: Option<SharedString>,
    lead: Lead,
    hint: Option<SharedString>,
) -> impl Fn(&mut Window, &mut App) -> Div + 'static {
    move |_, cx| {
        let theme = cx.theme();
        h_flex()
            .w_full()
            .min_w_0()
            .gap_3()
            .child(
                div()
                    .w_4()
                    .flex_none()
                    .flex()
                    .justify_center()
                    .map(|this| match &lead {
                        Lead::Tone(tone) => this.child(sidebar::dot(*tone, cx)),
                        Lead::Icon(icon) => {
                            this.child(Icon::new(*icon).small().text_color(theme.muted_foreground))
                        }
                    }),
            )
            .child(div().flex_1().min_w_0().truncate().child(label.clone()))
            .when_some(meta.clone(), |this, meta| {
                this.child(
                    div()
                        .flex_none()
                        .max_w(px(260.))
                        .truncate()
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(match &lead {
                            Lead::Tone(tone) => tone.text_color(cx),
                            Lead::Icon(_) => theme.muted_foreground,
                        })
                        .child(meta),
                )
            })
            .when_some(hint.clone(), |this, hint| {
                this.child(
                    div()
                        .flex_none()
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(theme.muted_foreground)
                        .child(hint),
                )
            })
    }
}
