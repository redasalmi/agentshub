//! Headings shared by the sidebar's lists.

use super::*;

/// A heading of the running list, in the color of what its chats need.
pub fn group_heading(label: &'static str, count: usize, tone: Tone, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let color = if tone.needs_you() {
        Tone::NeedsYou(None).text_color(cx)
    } else {
        tone.text_color(cx)
    };
    h_flex()
        .pt_2()
        .pb_1()
        .gap_1p5()
        .text_xs()
        .font_medium()
        .text_color(color)
        .child(label)
        .child(
            div()
                .font_family(theme.mono_font_family.clone())
                .text_color(theme.muted_foreground)
                .child(count.to_string()),
        )
}

pub(super) fn section(label: &'static str, cx: &App) -> impl IntoElement {
    heading(label, cx).px_2().pt_3().pb_1()
}

/// A label over a group of rows: quieter than the rows, in the text face so
/// the mono face is left to paths, keys and figures.
pub fn heading(label: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .text_xs()
        .font_medium()
        .text_color(cx.theme().muted_foreground)
        .child(label.into())
}
