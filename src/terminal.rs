//! A CLI running in a PTY, with its screen state kept by libghostty-vt.
//!
//! libghostty's `Terminal` is `!Send`, so every terminal gets a thread that
//! owns it. PTY output, key encoding, scrolling and resizes all go through
//! that thread, and the UI only ever receives owned `Frame` snapshots.

use std::cell::RefCell;
use std::ffi::OsStr;
use std::io::{Read, Write};
use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use gpui_kit::{Context, EventEmitter, SharedString, Task};
use libghostty_vt::{
    Terminal as VtTerminal, TerminalOptions, key, mouse, paste,
    render::{CellIterator, CursorVisualStyle, RenderState, RowIterator},
    screen::CellWide,
    style::{RgbColor, Underline},
    terminal::{
        ConformanceLevel, DeviceAttributeFeature, DeviceAttributes, DeviceType, Mode,
        PrimaryDeviceAttributes, ScrollViewport, SecondaryDeviceAttributes,
    },
};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use smol::future::FutureExt as _;

/// Output bursts are coalesced into at most one frame per interval. The first
/// frame after a quiet period is sent right away so typing stays instant.
const FRAME_INTERVAL: Duration = Duration::from_millis(8);
const SCROLLBACK_LINES: usize = 10_000;

/// How often the child's own exit is checked for. Short enough that an exited
/// CLI is marked dead before anyone notices the delay.
const EXIT_POLL: Duration = Duration::from_millis(25);

/// How long the CLI's processes get to handle SIGHUP before they're killed.
#[cfg(unix)]
const KILL_GRACE: Duration = Duration::from_millis(200);

/// How often the CLI is checked for having exited during its grace period.
#[cfg(unix)]
const KILL_POLL: Duration = Duration::from_millis(5);

/// How long the app waits on quit for its CLIs to be killed, a little more
/// than their grace period. Whatever is left gets the hangup of its PTY
/// closing as the app exits.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(300);

/// Room kept free in the input buffer before encoding.
///
/// `encode_to_vec` retries a too-small buffer by reserving only the shortfall,
/// but `Vec::reserve` does nothing when the spare capacity already covers it,
/// so the retry fails with the same `OutOfSpace`. A key or mouse sequence is
/// a few dozen bytes at most, so reserving this much up front always fits.
const ENCODE_SPARE: usize = 64;

/// What the UI draws: one list of styled runs per visible row.
pub struct Frame {
    pub rows: Vec<Vec<Run>>,
    pub cursor: Option<Cursor>,
    pub background: RgbColor,
}

/// Neighbouring cells of one row that share a style.
pub struct Run {
    pub col: u16,
    pub cells: u16,
    pub text: SharedString,
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
    pub strikethrough: bool,
}

impl Run {
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

pub struct KeyInput {
    pub key: key::Key,
    pub mods: key::Mods,
    pub consumed_mods: key::Mods,
    pub text: Option<String>,
    pub unshifted: char,
}

enum Msg {
    Output(Vec<u8>),
    Key(KeyInput),
    Text(String),
    Paste(String),
    Scroll { lines: isize, x: f32, y: f32 },
    Resize(GridSize),
    Visible(bool),
    Exited,
    Shutdown,
}

enum Update {
    Frame(Frame),
    Exited,
}

/// A started CLI process, before it is attached to the UI.
pub struct Pty {
    tx: mpsc::Sender<Msg>,
    updates: smol::channel::Receiver<Update>,
    pid: Arc<Mutex<Option<u32>>>,
    /// Asks for the CLI to be killed, without going through the terminal
    /// thread, which blocks while the CLI isn't reading its input.
    stop: mpsc::Sender<()>,
    /// Closes once the CLI has been reaped.
    reaped: smol::channel::Receiver<()>,
}

impl Pty {
    pub fn spawn(
        program: &str,
        args: &[String],
        env: &[(&str, &OsStr)],
        cwd: &Path,
        colors: (RgbColor, RgbColor),
    ) -> Result<Self> {
        let pair = native_pty_system().openpty(PtySize::default())?;
        let mut command = CommandBuilder::new(program);
        command.args(args);
        command.cwd(cwd);
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        for (key, value) in env {
            command.env(key, value);
        }
        let child = pair.slave.spawn_command(command)?;
        let pid = Arc::new(Mutex::new(child.process_id()));
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let (tx, rx) = mpsc::channel();
        let (updates_tx, updates) = smol::channel::unbounded();

        let output_tx = tx.clone();
        thread::spawn(move || {
            let mut buf = vec![0; 64 * 1024];
            // Reads fail with EIO once every side of the PTY is closed.
            while let Ok(n @ 1..) = reader.read(&mut buf) {
                if output_tx.send(Msg::Output(buf[..n].to_vec())).is_err() {
                    return;
                }
            }
        });

        // Reaps the child on its own instead of waiting for the reader: a
        // descendant holding inherited PTY descriptors keeps the reader from
        // reaching EOF long after the CLI itself has exited. Killing happens
        // only here, so the pid, which may get reused once it is reaped, is
        // never signalled. The shared pid is cleared, under its lock, before
        // the child is reaped, so nobody else uses it after that either.
        // The terminal asks for the kill when it goes away, and so does its
        // thread when it ends.
        let (stop_tx, stop_rx) = mpsc::channel();
        let (reaped_tx, reaped) = smol::channel::bounded::<()>(1);
        let exit_tx = tx.clone();
        let reaper_pid = pid.clone();
        thread::spawn(move || {
            let _reaped_tx = reaped_tx;
            let mut child = child;
            loop {
                {
                    let mut pid = reaper_pid.lock().unwrap();
                    if !matches!(child.try_wait(), Ok(None)) {
                        *pid = None;
                        break;
                    }
                }
                match stop_rx.recv_timeout(EXIT_POLL) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                        match reaper_pid.lock().unwrap().take() {
                            #[cfg(unix)]
                            Some(pid) => kill_group(pid),
                            _ => {
                                child.kill().ok();
                            }
                        }
                        child.wait().ok();
                        break;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
            }
            exit_tx.send(Msg::Exited).ok();
        });

        let failed_tx = updates_tx.clone();
        let run_stop_tx = stop_tx.clone();
        thread::spawn(move || {
            if let Err(err) = run(rx, pair.master, writer, colors, updates_tx) {
                eprintln!("terminal thread failed: {err:#}");
                // The CLI gets killed below and nothing reports it anymore.
                failed_tx.send_blocking(Update::Exited).ok();
            }
            run_stop_tx.send(()).ok();
        });

        Ok(Self {
            tx,
            updates,
            pid,
            stop: stop_tx,
            reaped,
        })
    }
}

/// Waits for CLIs that were shut down to be reaped, for at most
/// `SHUTDOWN_TIMEOUT`.
pub fn wait_reaped(reaped: impl IntoIterator<Item = smol::channel::Receiver<()>>) {
    let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
    smol::block_on(async {
        for reaped in reaped {
            let timeout = async {
                smol::Timer::at(deadline).await;
                Ok(())
            };
            reaped.recv().or(timeout).await.ok();
        }
    });
}

/// Hangs up the CLI's process group, then kills whatever is left of it once
/// the CLI has exited or its grace period is over, so processes it started,
/// like the native binary behind an npm launcher or anything that ignores
/// SIGHUP, don't outlive it. The CLI leads its own session, so the group id is
/// its pid. The CLI isn't reaped before the last signal, which keeps that id
/// from being reused by another group in the meantime.
#[cfg(unix)]
fn kill_group(pid: u32) {
    unsafe { libc::killpg(pid as libc::pid_t, libc::SIGHUP) };
    let deadline = Instant::now() + KILL_GRACE;
    while !has_exited(pid) && Instant::now() < deadline {
        thread::sleep(KILL_POLL);
    }
    unsafe { libc::killpg(pid as libc::pid_t, libc::SIGKILL) };
}

/// Whether the child has exited, leaving it to be reaped.
#[cfg(unix)]
fn has_exited(pid: u32) -> bool {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    // A child that is still running leaves `info` zeroed.
    result != 0 || unsafe { info.si_pid() } != 0
}

pub struct Terminal {
    tx: mpsc::Sender<Msg>,
    frame: Option<Arc<Frame>>,
    size: Option<GridSize>,
    exited: bool,
    pid: Arc<Mutex<Option<u32>>>,
    stop: mpsc::Sender<()>,
    reaped: smol::channel::Receiver<()>,
    _updates: Task<()>,
}

pub struct Exited;

impl EventEmitter<Exited> for Terminal {}

impl Terminal {
    pub fn new(pty: Pty, cx: &mut Context<Self>) -> Self {
        let updates = pty.updates;
        let task = cx.spawn(async move |this, cx| {
            while let Ok(update) = updates.recv().await {
                // Only the newest frame matters, so skip any that queued up.
                let mut frame = None;
                let mut exited = false;
                for update in
                    std::iter::once(update).chain(std::iter::from_fn(|| updates.try_recv().ok()))
                {
                    match update {
                        Update::Frame(next) => frame = Some(next),
                        Update::Exited => exited = true,
                    }
                }
                let updated = this.update(cx, |this, cx| {
                    if let Some(frame) = frame {
                        this.frame = Some(Arc::new(frame));
                    }
                    if exited && !this.exited {
                        this.exited = true;
                        cx.emit(Exited);
                    }
                    cx.notify();
                });
                if updated.is_err() {
                    break;
                }
            }
        });

        Self {
            tx: pty.tx,
            frame: None,
            size: None,
            exited: false,
            pid: pty.pid,
            stop: pty.stop,
            reaped: pty.reaped,
            _updates: task,
        }
    }

    pub fn frame(&self) -> Option<Arc<Frame>> {
        self.frame.clone()
    }

    pub fn exited(&self) -> bool {
        self.exited
    }

    /// Calls `f` with the CLI's pid, unless it has been reaped. The pid can't
    /// be reaped, and so can't belong to another process, until `f` returns.
    pub fn with_pid<T>(&self, f: impl FnOnce(u32) -> Option<T>) -> Option<T> {
        self.pid.lock().unwrap().and_then(f)
    }

    pub fn resize(&mut self, size: GridSize) {
        if self.size != Some(size) {
            self.size = Some(size);
            self.tx.send(Msg::Resize(size)).ok();
        }
    }

    pub fn key(&self, input: KeyInput) {
        self.tx.send(Msg::Key(input)).ok();
    }

    /// Text typed through an input method, sent as is.
    pub fn text(&self, text: String) {
        self.tx.send(Msg::Text(text)).ok();
    }

    pub fn paste(&self, text: String) {
        self.tx.send(Msg::Paste(text)).ok();
    }

    /// Negative `lines` scroll up. `x`/`y` are relative to the grid's origin.
    pub fn scroll(&self, lines: isize, x: f32, y: f32) {
        self.tx.send(Msg::Scroll { lines, x, y }).ok();
    }

    /// Kills the CLI. The returned channel closes once it has been reaped,
    /// which `wait_reaped` waits for.
    pub fn shutdown(&self) -> smol::channel::Receiver<()> {
        self.tx.send(Msg::Shutdown).ok();
        self.stop.send(()).ok();
        self.reaped.clone()
    }

    /// Frames are only built while the terminal is shown. Output still gets
    /// parsed in the meantime.
    pub fn set_visible(&self, visible: bool) {
        self.tx.send(Msg::Visible(visible)).ok();
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run(
    rx: mpsc::Receiver<Msg>,
    master: Box<dyn MasterPty + Send>,
    mut writer: Box<dyn Write + Send>,
    (background, foreground): (RgbColor, RgbColor),
    updates: smol::channel::Sender<Update>,
) -> Result<()> {
    let mut terminal = VtTerminal::new(TerminalOptions {
        cols: 80,
        rows: 24,
        max_scrollback: SCROLLBACK_LINES,
    })?;
    terminal
        .set_default_bg_color(Some(background))?
        .set_default_fg_color(Some(foreground))?;

    // Replies to terminal queries are collected while parsing and written to
    // the PTY afterwards. The CLIs probe device attributes at startup to
    // detect keyboard protocol support, and wait for an answer.
    let replies = Rc::new(RefCell::new(Vec::new()));
    terminal
        .on_pty_write({
            let replies = replies.clone();
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

    let mut render_state = RenderState::new()?;
    let mut rows = RowIterator::new()?;
    let mut cells = CellIterator::new()?;
    let mut key_encoder = key::Encoder::new()?;
    let mut key_event = key::Event::new()?;
    let mut mouse_encoder = mouse::Encoder::new()?;
    let mut mouse_event = mouse::Event::new()?;

    let mut size = None;
    let mut input = Vec::new();
    let mut dirty = false;
    let mut visible = true;
    let mut last_frame = Instant::now() - FRAME_INTERVAL;

    loop {
        let msg = if dirty && visible {
            match rx.recv_timeout(FRAME_INTERVAL.saturating_sub(last_frame.elapsed())) {
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
                terminal.vt_write(&bytes);
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
                dirty = true;
            }
            Some(Msg::Text(text)) => {
                input.extend_from_slice(text.as_bytes());
                terminal.scroll_viewport(ScrollViewport::Bottom);
                dirty = true;
            }
            Some(Msg::Paste(text)) => {
                let bracketed = terminal.mode(Mode::BRACKETED_PASTE)?;
                let mut data = text.into_bytes();
                let mut encoded = vec![0; data.len() + 16];
                let len = paste::encode(&mut data, bracketed, &mut encoded)?;
                input.extend_from_slice(&encoded[..len]);
                terminal.scroll_viewport(ScrollViewport::Bottom);
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
                    mouse_encoder
                        .set_options_from_terminal(&terminal)
                        .set_size(encoder_size(size));
                    // Wheel buttons have no release, and CLIs read a release
                    // packet as a second scroll of the same button.
                    mouse_event
                        .set_action(mouse::Action::Press)
                        .set_position(mouse::Position { x, y })
                        .set_button(Some(button));
                    for _ in 0..lines.unsigned_abs() {
                        input.reserve(ENCODE_SPARE);
                        mouse_encoder.encode_to_vec(&mouse_event, &mut input)?;
                    }
                } else {
                    terminal.scroll_viewport(ScrollViewport::Delta(lines));
                    dirty = true;
                }
            }
            Some(Msg::Resize(new_size)) => {
                let (cell_width, cell_height) =
                    (new_size.cell_width as u32, new_size.cell_height as u32);
                terminal.resize(new_size.cols, new_size.rows, cell_width, cell_height)?;
                master.resize(PtySize {
                    rows: new_size.rows,
                    cols: new_size.cols,
                    pixel_width: new_size.cols * cell_width as u16,
                    pixel_height: new_size.rows * cell_height as u16,
                })?;
                size = Some(new_size);
                dirty = true;
            }
            Some(Msg::Visible(shown)) => {
                visible = shown;
                dirty = true;
            }
            Some(Msg::Exited) => {
                updates.send_blocking(Update::Exited).ok();
            }
            Some(Msg::Shutdown) => break,
            None => {}
        }

        if !input.is_empty() {
            // Fails only once the CLI has exited, which the reaper reports.
            writer.write_all(&input).ok();
            input.clear();
        }

        if dirty && visible && last_frame.elapsed() >= FRAME_INTERVAL {
            let frame = build_frame(&mut render_state, &mut rows, &mut cells, &terminal)?;
            if updates.send_blocking(Update::Frame(frame)).is_err() {
                break;
            }
            dirty = false;
            last_frame = Instant::now();
        }
    }

    Ok(())
}

fn encoder_size(size: GridSize) -> mouse::EncoderSize {
    mouse::EncoderSize {
        screen_width: (size.cols as f32 * size.cell_width) as u32,
        screen_height: (size.rows as f32 * size.cell_height) as u32,
        cell_width: size.cell_width as u32,
        cell_height: size.cell_height as u32,
        padding_top: 0,
        padding_bottom: 0,
        padding_right: 0,
        padding_left: 0,
    }
}

fn build_frame<'alloc>(
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
    let mut text = String::new();
    let mut row_iter = rows.update(&snapshot)?;
    while let Some(row) = row_iter.next() {
        let mut runs: Vec<Run> = Vec::new();
        let mut run_text = String::new();
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
                    strikethrough: style.strikethrough,
                };
            }

            let wide = width == CellWide::Wide;
            let extends_run = runs.last().is_some_and(|run| {
                !wide && !run.wide && run.fg == fg && run.bg == bg && run.attrs == attrs
            });
            if extends_run {
                run_text.push_str(&text);
                runs.last_mut().unwrap().cells += 1;
            } else {
                if let Some(run) = runs.last_mut() {
                    run.text = std::mem::take(&mut run_text).into();
                }
                run_text.push_str(&text);
                runs.push(Run {
                    col: x,
                    cells: 1,
                    text: SharedString::default(),
                    fg,
                    bg,
                    attrs,
                    wide,
                });
            }
        }
        if let Some(run) = runs.last_mut() {
            run.text = run_text.into();
        }
        runs.retain(|run| run.bg.is_some() || run.paints_text());
        frame_rows.push(runs);
    }

    Ok(Frame {
        rows: frame_rows,
        cursor,
        background: colors.background,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn white() -> RgbColor {
        RgbColor {
            r: 255,
            g: 255,
            b: 255,
        }
    }

    #[test]
    fn renders_pty_output_into_styled_runs() {
        let script = r"printf 'hello \033[1;31mred\033[0m'; sleep 1";
        let pty = Pty::spawn(
            "sh",
            &["-c".into(), script.into()],
            &[],
            Path::new("/"),
            (RgbColor::default(), white()),
        )
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        let (red, bold) = loop {
            assert!(Instant::now() < deadline, "no frame with the output");
            let Ok(Update::Frame(frame)) = smol::block_on(pty.updates.recv()) else {
                continue;
            };
            let row = &frame.rows[0];
            if let Some(red) = row.iter().find(|run| run.text.as_str() == "red") {
                assert_eq!(row[0].text.as_str(), "hello ");
                assert_eq!(row[0].fg, white());
                break (red.fg, red.attrs.bold);
            }
        };
        assert!(red.r > red.g && red.r > red.b, "{red:?} isn't red");
        assert!(bold);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn reaps_the_child_once_it_exits() {
        let pty = Pty::spawn(
            "sh",
            &["-c".into(), "echo $$".into()],
            &[],
            Path::new("/"),
            (RgbColor::default(), white()),
        )
        .unwrap();

        let mut pid = None;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(Instant::now() < deadline, "the child never exited");
            match smol::block_on(pty.updates.recv()).unwrap() {
                Update::Frame(frame) => {
                    let text = frame.rows[0]
                        .iter()
                        .map(|run| run.text.as_str())
                        .collect::<String>();
                    pid = pid.or(text.trim().parse::<u32>().ok());
                }
                Update::Exited => break,
            }
        }
        let pid = pid.expect("no frame with the pid");
        assert!(
            !Path::new(&format!("/proc/{pid}")).exists(),
            "{pid} wasn't reaped"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn kills_what_the_child_started_on_shutdown() {
        let pty = Pty::spawn(
            "sh",
            &["-c".into(), "trap '' HUP; sleep 100 & echo $!; wait".into()],
            &[],
            Path::new("/"),
            (RgbColor::default(), white()),
        )
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        let pid = loop {
            assert!(Instant::now() < deadline, "no frame with the pid");
            let Ok(Update::Frame(frame)) = smol::block_on(pty.updates.recv()) else {
                continue;
            };
            let text = frame.rows[0]
                .iter()
                .map(|run| run.text.as_str())
                .collect::<String>();
            if let Ok(pid) = text.trim().parse::<u32>() {
                break pid;
            }
        };
        pty.tx.send(Msg::Shutdown).unwrap();
        while Path::new(&format!("/proc/{pid}")).exists() {
            assert!(Instant::now() < deadline, "{pid} outlived the terminal");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn kills_what_ignores_hangups_once_the_child_exits() {
        let pty = Pty::spawn(
            "sh",
            &[
                "-c".into(),
                "(trap '' HUP; exec sleep 100) & echo $!; wait".into(),
            ],
            &[],
            Path::new("/"),
            (RgbColor::default(), white()),
        )
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        let pid = loop {
            assert!(Instant::now() < deadline, "no frame with the pid");
            let Ok(Update::Frame(frame)) = smol::block_on(pty.updates.recv()) else {
                continue;
            };
            let text = frame.rows[0]
                .iter()
                .map(|run| run.text.as_str())
                .collect::<String>();
            if let Ok(pid) = text.trim().parse::<u32>() {
                break pid;
            }
        };
        let start = Instant::now();
        pty.tx.send(Msg::Shutdown).unwrap();
        pty.reaped.recv_blocking().unwrap_err();
        // The child exits on the hangup, so the grace period is cut short.
        assert!(start.elapsed() < KILL_GRACE, "{:?}", start.elapsed());
        // Gone, or a zombie left for its new parent to reap.
        while fs::read_to_string(format!("/proc/{pid}/stat"))
            .is_ok_and(|stat| !stat.contains(") Z "))
        {
            assert!(Instant::now() < deadline, "{pid} outlived the terminal");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn shuts_down_while_input_is_stuck() {
        let pty = Pty::spawn(
            "sh",
            &[
                "-c".into(),
                "stty -icanon; echo ready; exec sleep 30".into(),
            ],
            &[],
            Path::new("/"),
            (RgbColor::default(), white()),
        )
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(Instant::now() < deadline, "no frame with the prompt");
            let Ok(Update::Frame(frame)) = smol::block_on(pty.updates.recv()) else {
                continue;
            };
            if frame.rows[0].iter().any(|run| run.text.contains("ready")) {
                break;
            }
        }
        // `sleep` never reads it, so writing it blocks the terminal thread.
        pty.tx.send(Msg::Paste("x".repeat(1 << 20))).unwrap();
        thread::sleep(Duration::from_millis(100));

        let start = Instant::now();
        pty.tx.send(Msg::Shutdown).unwrap();
        pty.stop.send(()).unwrap();
        wait_reaped([pty.reaped.clone()]);
        assert!(pty.reaped.is_closed(), "the CLI wasn't reaped");
        assert!(start.elapsed() < SHUTDOWN_TIMEOUT, "{:?}", start.elapsed());
    }
}
