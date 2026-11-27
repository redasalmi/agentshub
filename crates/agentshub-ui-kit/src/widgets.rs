//! Headings and path lines shared by the layouts.

use std::path::Path;

use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, StyledExt as _, h_flex};
use gpui_kit::*;

use agentshub_core::format::home_relative;

use crate::status::Tone;

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

/// A label over a group of rows: quieter than the rows, in the text face so
/// the mono face is left to paths, keys and figures.
pub fn heading(label: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .text_xs()
        .font_medium()
        .text_color(cx.theme().muted_foreground)
        .child(label.into())
}

/// `path` on one line, cut from its start when it doesn't fit, since its end
/// says the most, and whole in a tooltip.
pub fn path_line(path: &Path, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    let shown = home_relative(path);
    let whole = SharedString::from(path.display().to_string());
    div()
        .id(SharedString::from(format!("path:{whole}")))
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis_start()
        .text_xs()
        .font_family(theme.mono_font_family.clone())
        .text_color(theme.muted_foreground)
        .tooltip(move |window, cx| Tooltip::new(whole.clone()).build(window, cx))
        .child(shown)
}
