//! The sidebar's status glyphs and badges.

use super::*;

/// A chat's status glyph. Each status has a shape of its own as well as a
/// color, so they tell apart at a glance and without telling colors apart:
/// a mark in a filled circle for the ones that need the user, a ring with a
/// bright head for an agent at work, a plain ring for one at rest, and a
/// small dot for a chat that doesn't run.
pub fn dot(tone: Tone, cx: &App) -> Div {
    status_glyph(tone, false, None, cx)
}

/// A chat's status glyph whose circle swells and settles back when its agent
/// has just come to need the user, `since` then.
pub(super) fn popping_dot(tone: Tone, since: Option<Instant>, cx: &App) -> Div {
    // Named after when it popped, so a later pop starts again.
    status_glyph(tone, false, since.map(name_of), cx)
}

/// An animation's name that changes with the moment it starts at.
pub(super) fn name_of(at: Instant) -> u64 {
    let mut hasher = std::hash::DefaultHasher::new();
    std::hash::Hash::hash(&at, &mut hasher);
    std::hash::Hasher::finish(&hasher)
}

/// A status glyph that spins while its agent works, where it says what a
/// chat is doing now. Only the header's status pill should use this: a tick
/// re-renders the view that holds it, and rebuilding the sidebar for a glyph
/// costs more than the whole animation is worth.
pub fn live_dot(tone: Tone, cx: &App) -> Div {
    status_glyph(tone, true, None, cx)
}

/// `pop` names the swell a glyph that needs the user gives when it has just
/// come to, if it has.
fn status_glyph(tone: Tone, animate: bool, pop: Option<u64>, cx: &App) -> Div {
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
