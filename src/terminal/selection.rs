//! Turning pointer gestures into a terminal selection.

use super::*;

/// Turns left-button presses, drags and releases into the terminal's
/// selection: a drag selects cells, a double click a word, a triple click a
/// line.
pub(super) struct Selecting {
    gesture: Gesture<'static>,
    press: gesture::PressEvent<'static>,
    drag: gesture::DragEvent<'static>,
    release: gesture::ReleaseEvent<'static>,
    tick: gesture::AutoscrollTickEvent<'static>,
    /// The left button went down to select and hasn't been released.
    pub(super) active: bool,
    /// Where the pointer last was, in whole pixels, maybe past the grid.
    pointer: (f32, f32),
    /// When to next scroll towards a pointer dragged past the grid's top or
    /// bottom.
    pub(super) next_tick: Option<Instant>,
    /// Presses are timed from here, which tells double clicks apart.
    epoch: Instant,
}

impl Selecting {
    pub(super) fn new() -> Result<Self> {
        let mut press = gesture::PressEvent::new()?;
        // Both default to zero, which makes every click a single click.
        press
            .set_repeat_interval(REPEAT_CLICK_INTERVAL)?
            .set_repeat_distance(REPEAT_CLICK_DISTANCE)?;
        Ok(Self {
            gesture: Gesture::new()?,
            press,
            drag: gesture::DragEvent::new()?,
            release: gesture::ReleaseEvent::new()?,
            tick: gesture::AutoscrollTickEvent::new()?,
            active: false,
            pointer: (0., 0.),
            next_tick: None,
            epoch: Instant::now(),
        })
    }

    /// Returns the selected text once the button is released. `x` and `y`
    /// are in whole pixels.
    pub(super) fn mouse(
        &mut self,
        terminal: &VtTerminal<'_, '_>,
        action: mouse::Action,
        x: f32,
        y: f32,
        size: GridSize,
    ) -> Result<Option<String>> {
        let grid_ref = terminal.grid_ref(Point::Viewport(viewport_cell(size, x, y)))?;
        self.pointer = (x, y);
        let (x, y) = (x.into(), y.into());
        let selection = match action {
            mouse::Action::Press => {
                self.active = true;
                self.press
                    .set_position(x, y)?
                    .set_time(self.epoch.elapsed())?
                    .apply(&mut self.gesture, terminal, grid_ref)?
            }
            mouse::Action::Release => {
                self.active = false;
                self.next_tick = None;
                self.release
                    .apply(&mut self.gesture, terminal, Some(grid_ref))?;
                return Ok(selection_text(terminal));
            }
            _ => {
                let selection = self.drag.set_position(x, y)?.apply(
                    &mut self.gesture,
                    terminal,
                    grid_ref,
                    geometry(size),
                )?;
                self.schedule(terminal)?;
                selection
            }
        };
        terminal.set_selection(selection.as_ref())?;
        Ok(None)
    }

    /// Scrolls a row towards the pointer past the grid's top or bottom, and
    /// selects up to the row it brought under the pointer.
    pub(super) fn autoscroll(
        &mut self,
        terminal: &VtTerminal<'_, '_>,
        size: GridSize,
    ) -> Result<()> {
        self.next_tick = None;
        let (x, y) = self.pointer;
        let selection = self.tick.set_position(x.into(), y.into())?.apply(
            &mut self.gesture,
            terminal,
            viewport_cell(size, x, y),
            geometry(size),
        )?;
        self.schedule(terminal)?;
        // None means the press is gone, which leaves the selection be.
        if let Some(selection) = selection {
            terminal.set_selection(Some(&selection))?;
        }
        Ok(())
    }

    /// Ticks while the gesture asks to scroll, keeping a tick already due.
    fn schedule(&mut self, terminal: &VtTerminal<'_, '_>) -> Result<()> {
        let scrolling = self.gesture.autoscroll(terminal)? != gesture::Autoscroll::None;
        self.next_tick = scrolling.then(|| {
            self.next_tick
                .unwrap_or_else(|| Instant::now() + AUTOSCROLL_INTERVAL)
        });
        Ok(())
    }
}

/// The selection as plain text, laid out the way Ghostty copies it.
pub(super) fn selection_text(terminal: &VtTerminal<'_, '_>) -> Option<String> {
    let options = FormatOptions::new()
        .with_emit_format(Format::Plain)
        .with_unwrap(true)
        .with_trim(true);
    let bytes = terminal.format_selection_alloc(None, options).ok()??;
    Some(String::from_utf8_lossy(&bytes).into_owned()).filter(|text| !text.is_empty())
}

/// Ghostty measures the mouse in whole-pixel cells, so a position over cells
/// of a fractional size is scaled onto cells of the truncated size.
pub(super) fn whole_pixel_position(size: GridSize, x: f32, y: f32) -> (f32, f32) {
    (
        x / size.cell_width * size.cell_width.trunc(),
        y / size.cell_height * size.cell_height.trunc(),
    )
}

/// The viewport cell at a whole-pixel position, or the nearest one when it's
/// past the grid's edges.
pub(super) fn viewport_cell(size: GridSize, x: f32, y: f32) -> PointCoordinate {
    PointCoordinate {
        x: ((x / size.cell_width.trunc()) as u16).min(size.cols.saturating_sub(1)),
        y: ((y / size.cell_height.trunc()) as u32).min(size.rows.saturating_sub(1).into()),
    }
}

pub(super) fn geometry(size: GridSize) -> gesture::Geometry {
    gesture::Geometry {
        columns: size.cols.into(),
        cell_width: size.cell_width as u32,
        padding_left: 0,
        screen_height: size.rows as u32 * size.cell_height as u32,
    }
}

pub(super) fn encoder_size(size: GridSize) -> mouse::EncoderSize {
    mouse::EncoderSize {
        screen_width: size.cols as u32 * size.cell_width as u32,
        screen_height: size.rows as u32 * size.cell_height as u32,
        cell_width: size.cell_width as u32,
        cell_height: size.cell_height as u32,
        padding_top: 0,
        padding_bottom: 0,
        padding_right: 0,
        padding_left: 0,
    }
}
