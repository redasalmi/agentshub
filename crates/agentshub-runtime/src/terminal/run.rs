//! The terminal thread: parsing output, encoding input and building frames.

use super::*;

#[expect(
    clippy::needless_pass_by_value,
    reason = "the PTY thread owns its master and channels, so they close when it returns"
)]
#[expect(
    clippy::too_many_lines,
    reason = "the terminal thread's message loop, whose arms share the terminal and its state"
)]
pub(super) fn run(
    rx: mpsc::Receiver<Msg>,
    master: Box<dyn MasterPty + Send>,
    mut writer: Box<dyn Write + Send>,
    colors: Colors,
    updates: smol::channel::Sender<Update>,
) -> Result<()> {
    let mut terminal = VtTerminal::new(TerminalOptions {
        cols: 80,
        rows: 24,
        max_scrollback: SCROLLBACK_LINES,
    })?;
    set_colors(&mut terminal, colors)?;

    // Replies to terminal queries are collected while parsing and written to
    // the PTY afterwards. The CLIs probe device attributes at startup to
    // detect keyboard protocol support, and wait for an answer.
    let replies = Rc::new(RefCell::new(Vec::new()));
    terminal
        .on_pty_write({
            let replies = Rc::clone(&replies);
            move |_, data| replies.borrow_mut().extend_from_slice(data)
        })?
        .on_device_attributes(|_| {
            Some(DeviceAttributes {
                primary: PrimaryDeviceAttributes::new(
                    ConformanceLevel::VT220,
                    &[
                        DeviceAttributeFeature::COLUMNS_132,
                        DeviceAttributeFeature::SELECTIVE_ERASE,
                        DeviceAttributeFeature::ANSI_COLOR,
                    ],
                ),
                secondary: SecondaryDeviceAttributes {
                    device_type: DeviceType::VT220,
                    firmware_version: 1,
                    rom_cartridge: 0,
                },
                tertiary: Default::default(),
            })
        })?
        .on_xtversion(|_| Some("agentshub"))?;

    let mut statuses = status::Scanner::default();
    let mut render_state = RenderState::new()?;
    let mut rows = RowIterator::new()?;
    let mut cells = CellIterator::new()?;
    let mut key_encoder = key::Encoder::new()?;
    let mut key_event = key::Event::new()?;
    let mut mouse_encoder = mouse::Encoder::new()?;
    let mut mouse_event = mouse::Event::new()?;
    // The cell of the last mouse event the CLI got.
    let mut last_cell = None;
    let mut selecting = Selecting::new()?;
    // Presses the CLI got, whose releases it gets too whatever Shift does.
    let mut forwarded = Vec::new();

    let mut size = None;
    let mut input = Vec::new();
    let mut dirty = false;
    let mut visible = true;
    let mut exiting = None;
    let mut last_frame = Instant::now()
        .checked_sub(FRAME_INTERVAL)
        .unwrap_or_else(Instant::now);

    loop {
        let timeout = [
            (dirty && visible).then(|| FRAME_INTERVAL.saturating_sub(last_frame.elapsed())),
            selecting
                .next_tick
                .map(|tick| tick.saturating_duration_since(Instant::now())),
        ]
        .into_iter()
        .flatten()
        .min();
        let msg = if let Some(timeout) = timeout {
            match rx.recv_timeout(timeout) {
                Ok(msg) => Some(msg),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match rx.recv() {
                Ok(msg) => Some(msg),
                Err(_) => break,
            }
        };

        match msg {
            Some(Msg::Output(bytes)) => {
                // Queries are answered in the order they came: CLIs follow the
                // status query with a device attributes query, and take that
                // answer coming first to mean reports aren't read.
                let mut written = 0;
                statuses.scan(&bytes, |report, end| match report {
                    Report::Query => {
                        terminal.vt_write(&bytes[written..end]);
                        written = end;
                        input.append(&mut replies.borrow_mut());
                        input.extend_from_slice(status::QUERY_REPLY);
                    }
                    Report::Status(status) => {
                        updates.send_blocking(Update::Status(status)).ok();
                    }
                });
                terminal.vt_write(&bytes[written..]);
                input.append(&mut replies.borrow_mut());
                dirty = true;
            }
            Some(Msg::Key(key_input)) => {
                key_event
                    .set_action(key::Action::Press)
                    .set_key(key_input.key)
                    .set_mods(key_input.mods)
                    .set_consumed_mods(key_input.consumed_mods)
                    .set_unshifted_codepoint(key_input.unshifted)
                    .set_utf8(key_input.text);
                input.reserve(ENCODE_SPARE);
                key_encoder
                    .set_options_from_terminal(&terminal)
                    .encode_to_vec(&key_event, &mut input)?;
                terminal.scroll_viewport(ScrollViewport::Bottom);
                terminal.set_selection(None)?;
                dirty = true;
            }
            Some(Msg::Text(text)) => {
                input.extend_from_slice(text.as_bytes());
                terminal.scroll_viewport(ScrollViewport::Bottom);
                terminal.set_selection(None)?;
                dirty = true;
            }
            Some(Msg::Paste(text)) => {
                let bracketed = terminal.mode(Mode::BRACKETED_PASTE)?;
                let mut data = text.into_bytes();
                let mut encoded = vec![0; data.len() + 16];
                let len = paste::encode(&mut data, bracketed, &mut encoded)?;
                input.extend_from_slice(&encoded[..len]);
                terminal.scroll_viewport(ScrollViewport::Bottom);
                terminal.set_selection(None)?;
                dirty = true;
            }
            Some(Msg::Scroll { lines, x, y }) => {
                if let Some(size) = size.filter(|_| terminal.is_mouse_tracking().unwrap_or(false)) {
                    // The CLI asked for mouse events, so the wheel becomes
                    // button 4/5 presses instead of scrolling our scrollback.
                    let button = if lines < 0 {
                        mouse::Button::Four
                    } else {
                        mouse::Button::Five
                    };
                    mouse_encoder.set_options_from_terminal(&terminal);
                    let (x, y) = whole_pixel_position(size, x, y);
                    // Wheel buttons have no release, and CLIs read a release
                    // packet as a second scroll of the same button.
                    mouse_event
                        .set_action(mouse::Action::Press)
                        .set_position(mouse::Position { x, y })
                        .set_button(Some(button))
                        .set_mods(key::Mods::empty());
                    for _ in 0..lines.unsigned_abs() {
                        input.reserve(ENCODE_SPARE);
                        mouse_encoder.encode_to_vec(&mouse_event, &mut input)?;
                    }
                } else {
                    terminal.scroll_viewport(ScrollViewport::Delta(lines));
                    dirty = true;
                }
            }
            Some(Msg::ScrollTo(row)) => {
                terminal.scroll_viewport(ScrollViewport::Row(row));
                dirty = true;
            }
            Some(Msg::Mouse(mouse_input)) => {
                if let Some(size) = size {
                    let (x, y) = whole_pixel_position(size, mouse_input.x, mouse_input.y);
                    let left = mouse_input.button == Some(mouse::Button::Left);
                    // A CLI that tracks the mouse gets it, except with Shift held,
                    // which selects as in other terminals.
                    let forward = !forwarded.is_empty()
                        || (!selecting.active
                            && !mouse_input.mods.contains(key::Mods::SHIFT)
                            && terminal.is_mouse_tracking().unwrap_or(false));
                    if forward {
                        let screen = encoder_size(size);
                        let in_grid = (0. ..screen.screen_width as f32).contains(&x)
                            && (0. ..screen.screen_height as f32).contains(&y);
                        // A press in the padding only focuses the terminal, a
                        // release goes only where its press went, and hovering
                        // the padding isn't hovering the grid's edge.
                        let report = match mouse_input.action {
                            mouse::Action::Press => in_grid,
                            mouse::Action::Release => forwarded
                                .iter()
                                .any(|&button| Some(button) == mouse_input.button),
                            _ => in_grid || !forwarded.is_empty(),
                        };
                        match mouse_input.action {
                            mouse::Action::Press if report => {
                                forwarded.extend(mouse_input.button);
                                // The click is the CLI's, so the old selection is gone.
                                terminal.set_selection(None)?;
                                dirty = true;
                            }
                            mouse::Action::Release => {
                                forwarded.retain(|&button| Some(button) != mouse_input.button)
                            }
                            _ => {}
                        }
                        // Motion is only reported when it reaches another cell,
                        // unless the CLI asked for pixels. Ghostty's encoder
                        // forgets the last cell whenever its options are set.
                        let cell = viewport_cell(size, x, y);
                        let report = report
                            && (mouse_input.action != mouse::Action::Motion
                                || last_cell != Some(cell)
                                || terminal.mode(Mode::SGR_PIXELS_MOUSE)?);
                        if report {
                            last_cell = Some(cell);
                            mouse_encoder
                                .set_options_from_terminal(&terminal)
                                .set_any_button_pressed(!forwarded.is_empty());
                            // Drags past the grid's edges report its edge cells.
                            mouse_event
                                .set_action(mouse_input.action)
                                .set_position(mouse::Position {
                                    x: x.clamp(0., screen.screen_width as f32 - 1.),
                                    y: y.clamp(0., screen.screen_height as f32 - 1.),
                                })
                                // A drag from the padding is hovering to the CLI.
                                .set_button(mouse_input.button.filter(|button| {
                                    mouse_input.action != mouse::Action::Motion
                                        || forwarded.contains(button)
                                }))
                                .set_mods(mouse_input.mods);
                            input.reserve(ENCODE_SPARE);
                            mouse_encoder.encode_to_vec(&mouse_event, &mut input)?;
                        }
                    } else if (selecting.active
                        && (left || mouse_input.action == mouse::Action::Motion))
                        || (left && mouse_input.action == mouse::Action::Press)
                    {
                        // A failed selection isn't worth losing the CLI over.
                        match selecting.mouse(&terminal, mouse_input.action, x, y, size) {
                            Ok(Some(text)) => {
                                updates
                                    .send_blocking(Update::Copy {
                                        text,
                                        primary: true,
                                    })
                                    .ok();
                            }
                            Ok(None) => {}
                            Err(err) => eprintln!("selecting failed: {err:#}"),
                        }
                        dirty = true;
                    }
                }
            }
            Some(Msg::Copy) => {
                if let Some(text) = selection_text(&terminal) {
                    updates
                        .send_blocking(Update::Copy {
                            text,
                            primary: false,
                        })
                        .ok();
                }
            }
            Some(Msg::Resize(new_size)) => {
                #[expect(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "pixel sizes are positive and clamped to u16"
                )]
                let to_px = |cells: u16, size: f32| {
                    (cells as u32 * size as u32).min(u16::MAX as u32) as u16
                };
                #[expect(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "cell sizes are a few positive pixels"
                )]
                let (cell_width, cell_height) =
                    (new_size.cell_width as u32, new_size.cell_height as u32);
                terminal.resize(new_size.cols, new_size.rows, cell_width, cell_height)?;
                master.resize(PtySize {
                    rows: new_size.rows,
                    cols: new_size.cols,
                    pixel_width: to_px(new_size.cols, new_size.cell_width),
                    pixel_height: to_px(new_size.rows, new_size.cell_height),
                })?;
                mouse_encoder.set_size(encoder_size(new_size));
                size = Some(new_size);
                dirty = true;
            }
            Some(Msg::Visible(shown)) => {
                visible = shown;
                // A hidden view never gets the release, so the drag ends here.
                if !shown {
                    selecting.active = false;
                    selecting.next_tick = None;
                }
                dirty = true;
            }
            Some(Msg::Colors(colors)) => {
                set_colors(&mut terminal, colors)?;
                dirty = true;
            }
            Some(Msg::Exited(success)) => {
                // What the CLI printed last is part of what it did, so it goes
                // out before the news that it exited.
                exiting = Some(success);
            }
            Some(Msg::Shutdown) => break,
            None => {}
        }

        if let Some(size) = size
            && selecting
                .next_tick
                .is_some_and(|tick| tick <= Instant::now())
        {
            if let Err(err) = selecting.autoscroll(&terminal, size) {
                eprintln!("selecting failed: {err:#}");
            }
            dirty = true;
        }

        if !input.is_empty() {
            // Fails only once the CLI has exited, which the reaper reports.
            writer.write_all(&input).ok();
            input.clear();
        }

        if let Some(success) = exiting.take() {
            if dirty && visible {
                let frame = build_frame(&mut render_state, &mut rows, &mut cells, &terminal)?;
                if updates.send_blocking(Update::Frame(frame)).is_err() {
                    break;
                }
                dirty = false;
                last_frame = Instant::now();
            }
            updates.send_blocking(Update::Exited(success)).ok();
        }

        if dirty && visible && last_frame.elapsed() >= FRAME_INTERVAL {
            let frame = build_frame(&mut render_state, &mut rows, &mut cells, &terminal)?;
            match updates.try_send(Update::Frame(frame)) {
                Ok(()) => {
                    dirty = false;
                    last_frame = Instant::now();
                }
                // The UI is behind; the frame is built again next time round.
                Err(smol::channel::TrySendError::Full(_)) => last_frame = Instant::now(),
                Err(smol::channel::TrySendError::Closed(_)) => break,
            }
        }
    }

    Ok(())
}
