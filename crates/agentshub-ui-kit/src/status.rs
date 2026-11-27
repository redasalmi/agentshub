//! Status glyphs, badges and the tones and filters they stand for.

use std::time::Duration;

use gpui_kit::component::{ActiveTheme as _, Icon, IconName, StyledExt as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use agentshub_core::format::{Turn, age, capitalized, duration};
use agentshub_core::status::{Need, Status};
use agentshub_core::store::Chat;

use crate::theme::{self as appearance, Tint};

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

    pub fn admits(self, running: Option<Option<Status>>) -> bool {
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

    /// Where the running list puts a chat: an agent that can't go on without
    /// the user first, one at rest last.
    pub fn urgency(self) -> u8 {
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
const WORKING_GLYPH: &[u8] = include_bytes!("../../../assets/working.svg");

/// How long a status glyph that has just come to need the user pops for, and
/// how much bigger it gets at its fullest.
pub const POP: Duration = Duration::from_millis(420);
const POP_GROWTH: f32 = 0.35;

/// A chat's status glyph. Each status has a shape of its own as well as a
/// color, so they tell apart at a glance and without telling colors apart:
/// a mark in a filled circle for the ones that need the user, a ring with a
/// bright head for an agent at work, a plain ring for one at rest, and a
/// small dot for a chat that doesn't run.
pub fn dot(tone: Tone, cx: &App) -> Div {
    status_glyph(tone, false, None, cx)
}

/// A status glyph that spins while its agent works, where it says what a
/// chat is doing now. Only the header's status pill should use this: a tick
/// re-renders the view that holds it, and rebuilding the sidebar for a glyph
/// costs more than the whole animation is worth.
pub fn live_dot(tone: Tone, cx: &App) -> Div {
    status_glyph(tone, true, None, cx)
}

/// What the agent is doing, with how long it has been doing it.
pub fn status_pill(tone: Tone, turn: Option<Turn>, spin: bool, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let palette = appearance::palette(cx);
    let label = capitalized(tone.label());
    let (label, time) = match turn {
        Some(Turn::Since(start)) => (format!("{label} ·"), Some(duration(start.elapsed()))),
        Some(Turn::Took(took)) => (format!("{label} · took"), Some(duration(took))),
        None => (label, None),
    };
    let (background, foreground) = tone
        .tint(cx)
        .map(|tint| (tint.soft, tint.text))
        .unwrap_or((palette.strong, palette.secondary_foreground));
    h_flex()
        .flex_none()
        .h(px(26.))
        .px_2p5()
        .gap_2()
        .rounded_full()
        .bg(background)
        .text_color(foreground)
        .text_xs()
        .font_medium()
        .child(if spin {
            live_dot(tone, cx)
        } else {
            dot(tone, cx)
        })
        .child(h_flex().gap_1().child(label).when_some(time, |this, time| {
            this.child(
                div()
                    .font_family(theme.mono_font_family.clone())
                    .child(time),
            )
        }))
}

/// `pop` names the swell a glyph that needs the user gives when it has just
/// come to, if it has.
pub fn status_glyph(tone: Tone, animate: bool, pop: Option<u64>, cx: &App) -> Div {
    let theme = cx.theme();
    let palette = appearance::palette(cx);
    let glyph = div()
        .size(GLYPH_SIZE)
        .flex_none()
        .flex()
        .items_center()
        .justify_center();
    // A glyph that needs the user: its circle, swelling if it pops. It
    // outgrows the glyph's room rather than moving what's beside it.
    let swell = |circle: Div| match pop {
        Some(key) => circle
            .with_animation(("pop", key), Animation::new(POP), |circle, delta| {
                // One hump up and back, so nothing flashes.
                let grow = (delta * std::f32::consts::PI).sin() * POP_GROWTH;
                circle.size(GLYPH_SIZE * (1. + grow)).flex_none()
            })
            .into_any_element(),
        None => circle.into_any_element(),
    };
    let filled = |background: Hsla, foreground: Hsla, icon: IconName| {
        swell(
            div()
                .size(GLYPH_SIZE)
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(background)
                .child(Icon::new(icon).size(px(10.)).text_color(foreground)),
        )
    };
    match tone {
        Tone::Saved => glyph.child(div().size(px(5.)).rounded_full().bg(palette.border_strong)),
        Tone::Idle => glyph.child(
            div()
                .size(px(10.))
                .rounded_full()
                .border_2()
                .border_color(theme.muted_foreground),
        ),
        Tone::Running => glyph.child(
            div()
                .size(px(10.))
                .rounded_full()
                .border_2()
                .border_color(theme.muted_foreground)
                .bg(theme.muted_foreground.opacity(0.35)),
        ),
        Tone::Working => {
            let icon = Icon::default()
                .data(WORKING_GLYPH)
                .size(GLYPH_SIZE)
                .text_color(palette.info.base);
            if animate && cx.reduce_motion() {
                // Reduced motion stills the animation, which would leave a
                // working agent looking stuck. The pill redraws every second
                // while a turn is under way, so the head steps round slowly.
                let step = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |since| since.as_secs() % REDUCED_SPIN_STEPS);
                glyph.child(icon.transform(Transformation::rotate(radians(
                    std::f32::consts::TAU * step as f32 / REDUCED_SPIN_STEPS as f32,
                ))))
            } else if animate {
                glyph.child(
                    icon.with_animation(
                        "working",
                        Animation::new(SPIN_PERIOD)
                            .repeat_synced()
                            .with_max_fps(SPIN_FPS),
                        |icon, delta| icon.transform(Transformation::rotate(percentage(delta))),
                    ),
                )
            } else {
                glyph.child(icon)
            }
        }
        Tone::NeedsYou(_) => glyph.child(swell(
            div()
                .size(GLYPH_SIZE)
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(theme.warning)
                .text_color(theme.warning_foreground)
                .text_size(px(11.))
                .font_semibold()
                .line_height(px(11.))
                .child("!"),
        )),
        Tone::Done => glyph.child(filled(
            theme.success,
            theme.success_foreground,
            IconName::Check,
        )),
        Tone::Error => glyph.child(filled(
            theme.danger,
            theme.danger_foreground,
            IconName::Close,
        )),
    }
}

/// What a chat's row says under its title: what its agent does, where the
/// chat is (`project`, else its agent) and when it was last worked in.
pub fn subtitle(chat: &Chat, tone: Tone, project: Option<&str>) -> String {
    let source = project.unwrap_or(chat.agent.label());
    match tone {
        Tone::Saved => format!("{source} · {}", age(chat.updated_at)),
        tone => format!("{} · {source} · {}", tone.label(), age(chat.updated_at)),
    }
}

/// A count in the rail: filled when it counts chats that need the user, else
/// a quiet outline, in the working color while one of them works.
pub fn badge(count: usize, urgent: bool, working: bool, cx: &App) -> Div {
    let theme = cx.theme();
    let palette = appearance::palette(cx);
    let badge = div()
        .flex_none()
        .min_w(px(18.))
        .h(px(18.))
        .px_1()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .border_1()
        .text_size(px(10.))
        .font_medium()
        .child(count.to_string());
    if urgent {
        badge
            .bg(theme.warning)
            .border_color(theme.warning)
            .text_color(theme.warning_foreground)
            .font_semibold()
    } else if working {
        badge
            .bg(theme.sidebar)
            .border_color(palette.info.base)
            .text_color(palette.info.text)
    } else {
        badge
            .bg(theme.sidebar)
            .border_color(palette.border_strong)
            .text_color(palette.secondary_foreground)
    }
}
