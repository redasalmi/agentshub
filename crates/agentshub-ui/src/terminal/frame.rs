//! Snapshots of the terminal grid for the view to paint.

use super::*;

pub struct Frame {
    pub rows: Vec<Vec<Run>>,
    /// The selected cells of each row.
    pub selection: Vec<Option<RowSelection>>,
    pub cursor: Option<Cursor>,
    pub background: RgbColor,
    pub scrollbar: Scrollbar,
    /// How many columns each row has.
    pub cols: u16,
}

/// Neighbouring cells of one row that share a style.
pub struct Run {
    pub col: u16,
    pub cells: u16,
    pub text: SharedString,
    /// Where each cell's text starts in `text`. A cell's text can join the
    /// next one's into a grapheme, so graphemes don't count cells.
    pub offsets: Vec<u32>,
    pub fg: RgbColor,
    pub bg: Option<RgbColor>,
    pub attrs: Attrs,
    /// A single double-width character, drawn without forcing it into one cell.
    pub wide: bool,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Attrs {
    pub bold: bool,
    pub italic: bool,
    pub faint: bool,
    pub underline: bool,
    pub curly_underline: bool,
    pub strikethrough: bool,
}

impl Run {
    /// Each cell's text, from the run's first column. A double-width
    /// character is one, though it covers two columns.
    pub fn cell_texts(&self) -> impl Iterator<Item = &str> {
        self.offsets.iter().enumerate().map(|(ix, start)| {
            let end = self
                .offsets
                .get(ix + 1)
                .map_or(self.text.len(), |end| *end as usize);
            self.text.get(*start as usize..end).unwrap_or_default()
        })
    }

    /// Blank cells still paint when they carry a decoration.
    pub fn paints_text(&self) -> bool {
        !self.text.trim().is_empty() || self.attrs.underline || self.attrs.strikethrough
    }
}

pub struct Cursor {
    pub col: u16,
    pub row: u16,
    pub style: CursorVisualStyle,
    pub color: RgbColor,
}

#[derive(Clone, Copy, PartialEq)]
pub struct GridSize {
    pub cols: u16,
    pub rows: u16,
    pub cell_width: f32,
    pub cell_height: f32,
}

pub use crate::theme::Colors;

pub(super) fn set_colors(terminal: &mut VtTerminal<'_, '_>, colors: Colors) -> Result<()> {
    let mut palette = terminal.default_color_palette()?;
    palette.0[..colors.ansi.len()].copy_from_slice(&colors.ansi);
    terminal
        .set_default_bg_color(Some(colors.background))?
        .set_default_fg_color(Some(colors.foreground))?
        .set_default_color_palette(Some(palette))?;
    Ok(())
}

pub(super) fn build_frame<'alloc>(
    render_state: &mut RenderState<'alloc>,
    rows: &mut RowIterator<'alloc>,
    cells: &mut CellIterator<'alloc>,
    terminal: &VtTerminal<'alloc, '_>,
) -> Result<Frame> {
    let snapshot = render_state.update(terminal)?;
    let colors = snapshot.colors()?;

    let cursor = match snapshot.cursor_viewport()? {
        Some(position) if snapshot.cursor_visible()? => Some(Cursor {
            col: position.x,
            row: position.y,
            style: snapshot.cursor_visual_style()?,
            color: colors.cursor.unwrap_or(colors.foreground),
        }),
        _ => None,
    };

    let mut frame_rows = Vec::new();
    let mut selection = Vec::new();
    let mut text = String::new();
    let mut cols = 0;
    let mut row_iter = rows.update(&snapshot)?;
    while let Some(row) = row_iter.next() {
        selection.push(row.selection()?);
        let mut runs: Vec<Run> = Vec::new();
        let mut run_text = String::new();
        let mut run_offsets = Vec::new();
        let mut cell_iter = cells.update(row)?;
        let mut col = 0;
        while let Some(cell) = cell_iter.next() {
            let x = col;
            col += 1;

            let width = cell.raw_cell()?.wide()?;
            if width == CellWide::SpacerTail {
                // The second half of the wide character in the previous run.
                if let Some(run) = runs.last_mut() {
                    run.cells += 1;
                }
                continue;
            }

            text.clear();
            if width != CellWide::SpacerHead && cell.graphemes_len()? > 0 {
                cell.graphemes_utf8(&mut text)?;
            } else {
                text.push(' ');
            }

            let mut fg = cell.fg_color()?.unwrap_or(colors.foreground);
            let mut bg = cell.bg_color()?;
            let mut attrs = Attrs::default();
            if cell.has_styling()? {
                let style = cell.style()?;
                if style.inverse {
                    (fg, bg) = (bg.unwrap_or(colors.background), Some(fg));
                }
                if style.invisible {
                    text.clear();
                    text.push(' ');
                }
                attrs = Attrs {
                    bold: style.bold,
                    italic: style.italic,
                    faint: style.faint,
                    underline: style.underline != Underline::None,
                    curly_underline: style.underline == Underline::Curly,
                    strikethrough: style.strikethrough,
                };
            }

            let wide = width == CellWide::Wide;
            if let Some(run) = runs.last_mut().filter(|run| {
                !wide && !run.wide && run.fg == fg && run.bg == bg && run.attrs == attrs
            }) {
                run_offsets.push(run_text.len() as u32);
                run_text.push_str(&text);
                run.cells += 1;
            } else {
                if let Some(run) = runs.last_mut() {
                    run.text = std::mem::take(&mut run_text).into();
                    run.offsets = std::mem::take(&mut run_offsets);
                }
                run_offsets.push(0);
                run_text.push_str(&text);
                runs.push(Run {
                    col: x,
                    cells: 1,
                    text: SharedString::default(),
                    offsets: Vec::new(),
                    fg,
                    bg,
                    attrs,
                    wide,
                });
            }
        }
        if let Some(run) = runs.last_mut() {
            run.text = run_text.into();
            run.offsets = run_offsets;
        }
        cols = cols.max(col);
        runs.retain(|run| run.bg.is_some() || run.paints_text());
        frame_rows.push(runs);
    }

    Ok(Frame {
        rows: frame_rows,
        selection,
        cursor,
        background: colors.background,
        scrollbar: terminal.scrollbar()?,
        cols,
    })
}
