//! Painting a terminal frame, its cursor, selection and scrollbar.

use super::*;

/// JetBrains Mono's underline thickness, in ems, which Ghostty draws
/// underlines and box lines as thick as.
const UNDERLINE_THICKNESS: f32 = 0.05;

/// Where JetBrains Mono puts the tops of underlines below the baseline and of
/// strikethroughs above it, in ems.
const UNDERLINE_POSITION: f32 = 0.155;
const STRIKETHROUGH_POSITION: f32 = 0.32;

/// Faint text, as Ghostty draws it by default.
const FAINT_OPACITY: f32 = 0.5;

/// The terminals' text size, which the user zooms.
fn font_size(cx: &App) -> Pixels {
    px(cx.global::<Settings>().terminal_font_size)
}

/// In whole device pixels, at least one.
fn device_pixels(length: Pixels, scale: f32) -> f32 {
    (f32::from(length) * scale).round().max(1.)
}

fn snap(length: Pixels, scale: f32) -> Pixels {
    px((f32::from(length) * scale).round() / scale)
}

/// A cell is the font's advance wide and its ascent and descent tall, both
/// rounded to whole device pixels as Ghostty sizes its cells, so the grid
/// lands on pixel boundaries.
pub(super) fn cell_size(window: &Window, cx: &App) -> Size<Pixels> {
    let font_size = font_size(cx);
    let scale = window.scale_factor();
    let text_system = window.text_system();
    let font_id = text_system.resolve_font(&terminal_font());
    let width = text_system
        .advance(font_id, font_size, 'm')
        .map_or(font_size * 0.6, |advance| advance.width);
    let height = px(f32::from(text_system.ascent(font_id, font_size))
        + f32::from(text_system.descent(font_id, font_size)).abs());
    size(
        px(device_pixels(width, scale) / scale),
        px(device_pixels(height, scale) / scale),
    )
}

/// How text and sprites sit in the cells.
struct Grid {
    cell: Size<Pixels>,
    scale: f32,
    /// From the top of a cell, on a whole device pixel.
    baseline: Pixels,
    /// The tops of underlines and strikethroughs, from the top of a cell.
    underline: Pixels,
    strikethrough: Pixels,
    sprite: sprite::Metrics,
}

impl Grid {
    fn snap(&self, origin: Point<Pixels>) -> Point<Pixels> {
        point(snap(origin.x, self.scale), snap(origin.y, self.scale))
    }

    /// Underlines and strikethroughs, as thick as box lines.
    fn line_thickness(&self) -> Pixels {
        px(self.sprite.thickness as f32 / self.scale)
    }

    /// Where to paint a shaped line so its baseline is the grid's. GPUI
    /// centers a line by the tallest font it used, so a run drawn in a
    /// fallback font would otherwise sit higher or lower than the rest.
    fn line_origin(&self, line: &ShapedLine, origin: Point<Pixels>) -> Point<Pixels> {
        let centered = (self.cell.height - line.ascent - line.descent) / 2. + line.ascent;
        point(origin.x, origin.y + self.baseline - centered)
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "device pixel sizes of one cell"
    )]
    fn new(cell: Size<Pixels>, window: &Window, cx: &App) -> Self {
        let font_size = font_size(cx);
        let scale = window.scale_factor();
        let text_system = window.text_system();
        let font_id = text_system.resolve_font(&terminal_font());
        let ascent = f32::from(text_system.ascent(font_id, font_size)) * scale;
        let descent = f32::from(text_system.descent(font_id, font_size)).abs() * scale;
        let height = device_pixels(cell.height, scale);
        // Ghostty centers the font in the cell and rounds the baseline to a
        // pixel, so text is as crisp as the sprites.
        let baseline = height - (descent - (height - ascent - descent) / 2.).round();
        let thickness = (f32::from(font_size) * scale * UNDERLINE_THICKNESS)
            .ceil()
            .max(1.);
        let em = f32::from(font_size) * scale;
        Self {
            cell,
            scale,
            baseline: px(baseline / scale),
            underline: px((baseline + UNDERLINE_POSITION * em).round() / scale),
            strikethrough: px((baseline - STRIKETHROUGH_POSITION * em).round() / scale),
            sprite: sprite::Metrics {
                width: device_pixels(cell.width, scale) as i32,
                height: height as i32,
                thickness: thickness as i32,
            },
        }
    }
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

/// The text of the cursor's cell, which a double-width character's second
/// column shares with its first.
fn cursor_text(run: &Run, col: u16) -> Option<&str> {
    let cell = if run.wide { 0 } else { col - run.col };
    run.cell_texts().nth(cell as usize)
}

/// A cell's text drawn as a sprite rather than by the font.
fn sprite_char(text: &str) -> Option<char> {
    let mut chars = text.chars();
    chars
        .next()
        .filter(|ch| chars.next().is_none() && sprite::covers(*ch))
}

#[expect(
    clippy::too_many_lines,
    reason = "paints everything a frame holds on the grid it sets up"
)]
pub(super) fn paint_frame(
    frame: &Frame,
    origin: Point<Pixels>,
    cell: Size<Pixels>,
    focused: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let grid = Grid::new(cell, window, cx);
    let origin = grid.snap(origin);
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
        let chars = row_chars(runs, frame.cols);
        for run in runs.iter().filter(|run| run.paints_text()) {
            let origin = cell_origin(run.col, row);
            let text = paint_cells(run, origin, &chars, &grid, window, cx);
            paint_text(text, run, origin, &grid, None, window, cx);
            paint_decorations(run, origin, run.cells, &grid, text_color(run, None), window);

            // A solid cursor covers its cell, so that character is drawn
            // again on top of it in the background color.
            let under_cursor = solid_cursor.filter(|cursor| {
                cursor.row as usize == row && (run.col..run.col + run.cells).contains(&cursor.col)
            });
            if let Some(cursor) = under_cursor
                && let Some(text) = cursor_text(run, cursor.col)
            {
                let fg = Some(color(run.bg.unwrap_or(frame.background)));
                let color = text_color(run, fg);
                let col = if run.wide { run.col } else { cursor.col };
                let origin = cell_origin(col, row);
                let cells = if run.wide { run.cells } else { 1 };
                // Only what's inside the cursor takes its color; a symbol
                // spreading into the next cell keeps its own there.
                let cursor_cell = Bounds::new(origin, size(cell.width * cells as f32, cell.height));
                window.with_content_mask(
                    Some(ContentMask {
                        bounds: cursor_cell,
                    }),
                    |window| {
                        paint_decorations(run, origin, cells, &grid, color, window);
                        if let Some(ch) = sprite_char(text) {
                            sprite::paint(ch, origin, grid.sprite, grid.scale, color, window);
                        } else if starts_symbol(text) {
                            let cells = symbol_cells(run, &chars, col);
                            paint_symbol(
                                text,
                                run,
                                symbol_bounds(origin, cells, &grid),
                                &grid,
                                color,
                                window,
                                cx,
                            );
                        } else {
                            paint_text(text.to_string().into(), run, origin, &grid, fg, window, cx);
                        }
                    },
                );
            }
        }
    }
}

/// The first character of each cell in a row, a space where nothing is
/// drawn.
fn row_chars(runs: &[Run], cols: u16) -> Vec<char> {
    let mut chars = vec![' '; cols as usize];
    for run in runs {
        for (ix, cell) in run.cell_texts().enumerate() {
            if let Some(slot) = chars.get_mut(run.col as usize + ix) {
                *slot = cell.chars().next().unwrap_or(' ');
            }
        }
    }
    chars
}

fn symbol_bounds(origin: Point<Pixels>, cells: u16, grid: &Grid) -> Bounds<Pixels> {
    Bounds::new(
        origin,
        size(grid.cell.width * cells as f32, grid.cell.height),
    )
}

fn starts_symbol(text: &str) -> bool {
    text.chars().next().is_some_and(symbol::is_symbol)
}

/// How many cells a symbol at `col` may cover: a double-width one its own
/// two, others as Ghostty decides.
fn symbol_cells(run: &Run, chars: &[char], col: u16) -> u16 {
    if run.wide {
        run.cells
    } else {
        symbol::constraint_width(chars, col)
    }
}

/// Draws the run's sprites, and its symbols fitted to the cells they may
/// cover, and returns the rest of its text with a space in each of their
/// cells, for the font to draw.
#[expect(
    clippy::cast_possible_truncation,
    reason = "a column within one terminal row"
)]
fn paint_cells(
    run: &Run,
    origin: Point<Pixels>,
    chars: &[char],
    grid: &Grid,
    window: &mut Window,
    cx: &mut App,
) -> SharedString {
    let color = text_color(run, None);
    if run.wide {
        if starts_symbol(&run.text) {
            let cells = symbol_cells(run, chars, run.col);
            paint_symbol(
                &run.text,
                run,
                symbol_bounds(origin, cells, grid),
                grid,
                color,
                window,
                cx,
            );
            return SharedString::default();
        }
        return run.text.clone();
    }
    if !run
        .text
        .chars()
        .any(|ch| sprite::covers(ch) || symbol::is_symbol(ch))
    {
        return run.text.clone();
    }
    let mut text = String::with_capacity(run.text.len());
    for (ix, cell) in run.cell_texts().enumerate() {
        let origin = point(origin.x + grid.cell.width * ix as f32, origin.y);
        if let Some(ch) = sprite_char(cell) {
            sprite::paint(ch, origin, grid.sprite, grid.scale, color, window);
            text.push(' ');
        } else if starts_symbol(cell) {
            let cells = symbol_cells(run, chars, run.col + ix as u16);
            paint_symbol(
                cell,
                run,
                symbol_bounds(origin, cells, grid),
                grid,
                color,
                window,
                cx,
            );
            text.push(' ');
        } else {
            text.push_str(cell);
        }
    }
    text.into()
}

/// A symbol, scaled down when it's wider or taller than the `cells` it may
/// cover, about its center and kept inside them, as Ghostty fits symbols.
fn paint_symbol(
    text: &str,
    run: &Run,
    cells: Bounds<Pixels>,
    grid: &Grid,
    color: Hsla,
    window: &mut Window,
    cx: &mut App,
) {
    let font = run_font(run);
    let shape = |size: Pixels, window: &mut Window| {
        let text_run = TextRun {
            len: text.len(),
            font: font.clone(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        window
            .text_system()
            .shape_line(text.to_string().into(), size, &[text_run], None)
    };
    let font_size = font_size(cx);
    let line = shape(font_size, window);
    let origin = cells.origin;
    let span = f32::from(cells.size.width);
    let width = f32::from(line.width);
    let height = f32::from(line.ascent + line.descent);
    let scale = (span / width)
        .min(f32::from(grid.cell.height) / height)
        .min(1.);
    if scale >= 1. || scale.is_nan() {
        let origin = grid.line_origin(&line, origin);
        line.paint(origin, grid.cell.height, TextAlign::Left, None, window, cx)
            .ok();
        return;
    }
    let small = shape(font_size * scale, window);
    let left = f32::from(origin.x) + (width - f32::from(small.width)) / 2.;
    let left = left.min(f32::from(origin.x) + span - f32::from(small.width));
    let left = left.max(f32::from(origin.x));
    // The scaled glyph keeps the vertical center of the full-size one.
    let center = origin.y + grid.baseline - (line.ascent - line.descent) / 2.;
    let baseline = center + (small.ascent - small.descent) / 2.;
    let top = baseline - ((grid.cell.height - small.ascent - small.descent) / 2. + small.ascent);
    small
        .paint(
            point(px(left), top),
            grid.cell.height,
            TextAlign::Left,
            None,
            window,
            cx,
        )
        .ok();
}

fn run_font(run: &Run) -> Font {
    let mut font = if run.wide {
        wide_font()
    } else {
        terminal_font()
    };
    if run.attrs.bold {
        font = font.bold();
    }
    if run.attrs.italic {
        font = font.italic();
    }
    font
}

fn text_color(run: &Run, fg: Option<Hsla>) -> Hsla {
    let color = fg.unwrap_or_else(|| color(run.fg));
    if run.attrs.faint {
        color.opacity(FAINT_OPACITY)
    } else {
        color
    }
}

/// Text an input method is composing, over the cursor's cell.
pub(super) fn paint_marked_text(
    text: String,
    frame: &Frame,
    (col, row): (u16, u16),
    origin: Point<Pixels>,
    cell: Size<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let grid = Grid::new(cell, window, cx);
    let origin = grid.snap(origin) + point(cell.width * col as f32, cell.height * row as f32);
    let text_color = cx.theme().foreground;
    let text_run = TextRun {
        len: text.len(),
        font: terminal_font(),
        color: text_color,
        background_color: None,
        underline: Some(UnderlineStyle {
            thickness: grid.line_thickness(),
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
    line.paint(
        grid.line_origin(&line, origin),
        cell.height,
        TextAlign::Left,
        None,
        window,
        cx,
    )
    .ok();
}

fn paint_text(
    text: SharedString,
    run: &Run,
    origin: Point<Pixels>,
    grid: &Grid,
    fg: Option<Hsla>,
    window: &mut Window,
    cx: &mut App,
) {
    if text.trim().is_empty() {
        return;
    }
    let text_color = text_color(run, fg);
    let text_run = TextRun {
        len: text.len(),
        font: run_font(run),
        color: text_color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    // Forcing every glyph to the cell width keeps text on the grid even when
    // a fallback font with different metrics renders some characters.
    let force_width = (!run.wide).then_some(grid.cell.width);
    let line = window
        .text_system()
        .shape_line(text, font_size(cx), &[text_run], force_width);
    line.paint(
        grid.line_origin(&line, origin),
        grid.cell.height,
        TextAlign::Left,
        None,
        window,
        cx,
    )
    .ok();
}

/// Underlines and strikethroughs across the run's cells. GPUI would draw
/// them to the width the font shaped, which the grid's rounded cells don't
/// match.
fn paint_decorations(
    run: &Run,
    origin: Point<Pixels>,
    cells: u16,
    grid: &Grid,
    color: Hsla,
    window: &mut Window,
) {
    let width = grid.cell.width * cells as f32;
    let thickness = grid.line_thickness();
    if run.attrs.underline && run.attrs.curly_underline {
        let wave = Bounds::new(origin, size(width, grid.cell.height));
        let amplitude = thickness.max(px(1.));
        let y = origin.y + grid.underline + thickness / 2.;
        let half = grid.cell.width / 2.;
        let mut path = PathBuilder::stroke(thickness);
        path.move_to(point(origin.x, y));
        for ix in 0..cells * 2 {
            let x = origin.x + half * ix as f32;
            let peak = if ix % 2 == 0 { -amplitude } else { amplitude };
            path.curve_to(point(x + half, y), point(x + half / 2., y + peak * 2.));
        }
        if let Ok(path) = path.build() {
            window.with_content_mask(Some(ContentMask { bounds: wave }), |window| {
                window.paint_path(path, color)
            });
        }
    } else if run.attrs.underline {
        window.paint_quad(fill(
            Bounds::new(
                point(origin.x, origin.y + grid.underline),
                size(width, thickness),
            ),
            color,
        ));
    }
    if run.attrs.strikethrough {
        window.paint_quad(fill(
            Bounds::new(
                point(origin.x, origin.y + grid.strikethrough),
                size(width, thickness),
            ),
            color,
        ));
    }
}
