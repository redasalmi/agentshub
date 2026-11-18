//! Painting a terminal frame, its cursor, selection and scrollbar.

use super::*;

/// Built once, since every run of every frame needs it.
static TERMINAL_FONT: LazyLock<Font> = LazyLock::new(|| Font {
    fallbacks: Some(FontFallbacks::from_fonts(
        FONT_FALLBACKS.iter().map(|name| name.to_string()).collect(),
    )),
    ..font(FONT_FAMILY)
});

pub(super) fn terminal_font() -> Font {
    TERMINAL_FONT.clone()
}

/// The terminals' text size, which the user zooms.
fn font_size(cx: &App) -> Pixels {
    px(cx.global::<Settings>().terminal_font_size)
}

pub(super) fn cell_size(window: &Window, cx: &App) -> Size<Pixels> {
    let font_size = font_size(cx);
    let text_system = window.text_system();
    let font_id = text_system.resolve_font(&terminal_font());
    let width = text_system
        .advance(font_id, font_size, 'm')
        .map_or(font_size * 0.6, |advance| advance.width);
    size(width, (font_size * LINE_HEIGHT).round())
}

/// Where the scrollbar thumb sits, in the padding right of the grid, when
/// there is scrollback to scroll through.
pub(super) fn thumb_bounds(scrollbar: &Scrollbar, grid: Bounds<Pixels>) -> Option<Bounds<Pixels>> {
    let Scrollbar { total, offset, len } = *scrollbar;
    if total <= len {
        return None;
    }
    let track = grid.size.height;
    let height = (track * (len as f32 / total as f32)).max(MIN_THUMB_HEIGHT.min(track));
    let top = (track - height) * (offset as f32 / (total - len) as f32);
    Some(Bounds::new(
        point(grid.right() + px(3.), grid.top() + top),
        size(SCROLLBAR_WIDTH, height),
    ))
}

/// The complete grapheme under the cursor's cell. A grapheme can span several
/// chars, and a combining mark takes up no cell of its own.
fn cursor_grapheme(run: &Run, col: u16) -> Option<&str> {
    if run.wide {
        Some(run.text.as_str())
    } else {
        run.text.graphemes(true).nth((col - run.col) as usize)
    }
}

pub(super) fn paint_frame(
    frame: &Frame,
    origin: Point<Pixels>,
    cell: Size<Pixels>,
    focused: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let cell_origin = |col: u16, row: usize| {
        point(
            origin.x + cell.width * col as f32,
            origin.y + cell.height * row as f32,
        )
    };

    for (row, runs) in frame.rows.iter().enumerate() {
        for run in runs {
            if let Some(bg) = run.bg {
                let size = size(cell.width * run.cells as f32, cell.height);
                window.paint_quad(fill(
                    Bounds::new(cell_origin(run.col, row), size),
                    color(bg),
                ));
            }
        }
    }

    let selection_color = cx.theme().selection;
    for (row, selection) in frame.selection.iter().enumerate() {
        if let Some(selection) = selection {
            let cells = selection.end_x - selection.start_x + 1;
            window.paint_quad(fill(
                Bounds::new(
                    cell_origin(selection.start_x, row),
                    size(cell.width * cells as f32, cell.height),
                ),
                selection_color,
            ));
        }
    }

    // An unfocused block cursor is drawn hollow.
    let solid_cursor = frame
        .cursor
        .as_ref()
        .filter(|cursor| focused && cursor.style == CursorVisualStyle::Block);
    if let Some(cursor) = &frame.cursor {
        let origin = cell_origin(cursor.col, cursor.row as usize);
        let cursor_color = color(cursor.color);
        let quad = match cursor.style {
            CursorVisualStyle::Bar => {
                fill(Bounds::new(origin, size(px(2.), cell.height)), cursor_color)
            }
            CursorVisualStyle::Underline => fill(
                Bounds::new(
                    point(origin.x, origin.y + cell.height - px(2.)),
                    size(cell.width, px(2.)),
                ),
                cursor_color,
            ),
            _ if solid_cursor.is_some() => fill(Bounds::new(origin, cell), cursor_color),
            _ => outline(Bounds::new(origin, cell), cursor_color, BorderStyle::Solid),
        };
        window.paint_quad(quad);
    }

    for (row, runs) in frame.rows.iter().enumerate() {
        for run in runs.iter().filter(|run| run.paints_text()) {
            paint_text(
                run.text.clone(),
                run,
                cell_origin(run.col, row),
                cell,
                None,
                window,
                cx,
            );

            // A solid cursor covers its cell, so that character is drawn
            // again on top of it in the background color.
            let under_cursor = solid_cursor.filter(|cursor| {
                cursor.row as usize == row && (run.col..run.col + run.cells).contains(&cursor.col)
            });
            if let Some(cursor) = under_cursor
                && let Some(text) = cursor_grapheme(run, cursor.col)
            {
                let fg = Some(color(run.bg.unwrap_or(frame.background)));
                let origin = cell_origin(if run.wide { run.col } else { cursor.col }, row);
                paint_text(text.to_string().into(), run, origin, cell, fg, window, cx);
            }
        }
    }
}

pub(super) fn paint_marked_text(
    text: String,
    frame: &Frame,
    origin: Point<Pixels>,
    cell: Size<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let text_color = cx.theme().foreground;
    let text_run = TextRun {
        len: text.len(),
        font: terminal_font(),
        color: text_color,
        background_color: None,
        underline: Some(UnderlineStyle {
            thickness: px(1.),
            color: Some(text_color),
            wavy: false,
        }),
        strikethrough: None,
    };
    let line = window
        .text_system()
        .shape_line(text.into(), font_size(cx), &[text_run], None);
    window.paint_quad(fill(
        Bounds::new(origin, size(line.width(), cell.height)),
        color(frame.background),
    ));
    line.paint(origin, cell.height, TextAlign::Left, None, window, cx)
        .ok();
}

fn paint_text(
    text: SharedString,
    run: &Run,
    origin: Point<Pixels>,
    cell: Size<Pixels>,
    fg: Option<Hsla>,
    window: &mut Window,
    cx: &mut App,
) {
    let mut text_color = fg.unwrap_or_else(|| color(run.fg));
    if run.attrs.faint {
        text_color = text_color.opacity(0.6);
    }
    let mut font = terminal_font();
    if run.attrs.bold {
        font = font.bold();
    }
    if run.attrs.italic {
        font = font.italic();
    }
    let text_run = TextRun {
        len: text.len(),
        font,
        color: text_color,
        background_color: None,
        underline: run.attrs.underline.then_some(UnderlineStyle {
            thickness: px(1.),
            color: Some(text_color),
            wavy: false,
        }),
        strikethrough: run.attrs.strikethrough.then_some(StrikethroughStyle {
            thickness: px(1.),
            color: Some(text_color),
        }),
    };
    // Forcing every glyph to the cell width keeps text on the grid even when
    // a fallback font with different metrics renders some characters.
    let force_width = (!run.wide).then_some(cell.width);
    let line = window
        .text_system()
        .shape_line(text, font_size(cx), &[text_run], force_width);
    line.paint(origin, cell.height, TextAlign::Left, None, window, cx)
        .ok();
}
