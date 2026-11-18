//! The rail's width and the widgets that belong to it.

use super::*;

/// Whether the rail names the projects in a window `window_width` wide, and
/// how wide it is there. The settings keep the width the user wants, which a
/// narrower window only shows narrower.
pub fn rail(window_width: Pixels, cx: &App) -> (bool, Pixels) {
    let settings = cx.global::<Settings>();
    fit_rail(
        settings.project_names,
        px(settings.rail_width),
        super::rail_room(window_width),
    )
}

/// The rail the user wants, `named` and `wanted` wide, fit in `room`: as
/// narrow as it must be, and folded to the projects' marks when even its
/// narrowest with their names doesn't fit.
pub(super) fn fit_rail(named: bool, wanted: Pixels, room: Pixels) -> (bool, Pixels) {
    let room = room.min(MAX_RAIL_WIDTH);
    if named && room >= MIN_NAMED_RAIL_WIDTH {
        (true, wanted.clamp(MIN_NAMED_RAIL_WIDTH, room))
    } else {
        (false, RAIL_WIDTH)
    }
}

/// How much the rail adds to the sidebar's width beyond its marks alone in a
/// window `window_width` wide, which is all the width the user drags the
/// sidebar's edge accounts for.
pub fn names_width(window_width: Pixels, cx: &App) -> Pixels {
    rail(window_width, cx).1 - RAIL_WIDTH
}

/// The number that opens the `index`th chat or project with its modifier.
pub(super) fn key_number(index: usize, cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .flex_none()
        .px_1p5()
        .rounded(px(4.))
        .border_1()
        .border_color(appearance::palette(cx).border_strong)
        .bg(theme.background)
        .text_xs()
        .font_family(theme.mono_font_family.clone())
        .text_color(theme.foreground)
        .child((index + 1).to_string())
}

/// A project's mark, drawn in its color.
pub(super) fn project_mark(mark: SharedString, tint: Tint, cx: &App) -> Div {
    div()
        .size(MARK_SIZE)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .bg(tint.soft)
        .text_color(tint.text)
        .text_size(px(11.))
        .font_medium()
        .font_family(cx.theme().mono_font_family.clone())
        .child(mark)
}
