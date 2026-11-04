//! The search modal: one field over the chats of every project, the agents
//! that can start in the selected one, the projects, and the commands.

use std::cell::Cell;
use std::rc::Rc;

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

/// Opens the modal over `sections`, each a heading and its entries. Empty
/// sections are left out.
pub fn open(
    sections: Vec<(SharedString, Vec<Entry>)>,
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
        let mut group = CommandGroup::new().label(heading);
        let mut group_picks = Vec::new();
        for entry in entries {
            let hint = match &entry.pick {
                Pick::Run(action) => shortcut(action.as_ref(), window),
                _ => None,
            };
            let mut keywords = entry.keywords.clone();
            keywords.extend(entry.meta.clone());
            group = group.item(
                CommandItem::new()
                    .label(entry.label.clone())
                    .keywords(keywords)
                    .child(row(entry.label, entry.meta, entry.lead, hint)),
            );
            group_picks.push(entry.pick);
        }
        groups.push(group);
        picks.push(group_picks);
    }

    let state = cx.new(|cx| CommandState::new(window, cx));
    let picks = Rc::new(picks);
    let on_pick = Rc::new(on_pick);
    let initial_focus = Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        if initial_focus.replace(false) {
            state.update(cx, |state, cx| state.focus(window, cx));
        }
        let (picks, on_pick) = (picks.clone(), on_pick.clone());
        let command = groups
            .iter()
            .cloned()
            .fold(Command::new(&state), Command::group)
            .bordered(false)
            .max_h(MAX_HEIGHT)
            .placeholder("Search chats, projects and commands")
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
