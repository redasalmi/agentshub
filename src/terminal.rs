//! A CLI running in a PTY, with its screen state kept by libghostty-vt.
//!
//! libghostty's `Terminal` is `!Send`, so every terminal gets a thread that
//! owns it. PTY output, key encoding, scrolling and resizes all go through
//! that thread, and the UI only ever receives owned `Frame` snapshots.

use std::cell::{Cell, RefCell};
use std::ffi::OsStr;
use std::io::{Read, Write};
use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use gpui_kit::{ClipboardItem, Context, EventEmitter, SharedString, Task};
use libghostty_vt::{
    Terminal as VtTerminal, TerminalOptions,
    fmt::Format,
    key, mouse, paste,
    render::{CellIterator, CursorVisualStyle, RenderState, RowIterator, RowSelection},
    screen::CellWide,
    selection::{
        FormatOptions,
        gesture::{self, Gesture},
    },
    style::{RgbColor, Underline},
    terminal::{
        ConformanceLevel, DeviceAttributeFeature, DeviceAttributes, DeviceType, Mode, Point,
        PointCoordinate, PrimaryDeviceAttributes, ScrollViewport, Scrollbar,
        SecondaryDeviceAttributes,
    },
};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use smol::future::FutureExt as _;

use crate::status::{self, Report, Status};

/// Output bursts are coalesced into at most one frame per interval, which is
/// about what a display takes. The first frame after a quiet period is sent
/// right away so typing stays instant.
const FRAME_INTERVAL: Duration = Duration::from_millis(16);
const SCROLLBACK_LINES: usize = 10_000;

/// How many updates may wait for the UI. Frames are dropped rather than queued
/// past this: the next one carries everything they left out, and a stalled UI
/// must not make the terminal thread grow memory.
const UPDATE_QUEUE: usize = 8;

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

/// How soon, and how close in pixels, a click must follow the last one to
/// count towards a double or triple click.
const REPEAT_CLICK_INTERVAL: Duration = Duration::from_millis(500);
const REPEAT_CLICK_DISTANCE: f64 = 10.;

/// How often a selection dragged past the grid's top or bottom scrolls a row.
const AUTOSCROLL_INTERVAL: Duration = Duration::from_millis(15);

/// What the UI draws: one list of styled runs per visible row.
pub struct Frame {
    pub rows: Vec<Vec<Run>>,
    /// The selected cells of each row.
    pub selection: Vec<Option<RowSelection>>,
    pub cursor: Option<Cursor>,
    pub background: RgbColor,
    pub scrollbar: Scrollbar,
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

/// The colors that output without colors of its own is drawn in.
#[derive(Clone, Copy)]
pub struct Colors {
    pub background: RgbColor,
    pub foreground: RgbColor,
    /// The 16 ANSI colors, which CLIs pick from by index.
    pub ansi: [RgbColor; 16],
}

pub struct KeyInput {
    pub key: key::Key,
    pub mods: key::Mods,
    pub consumed_mods: key::Mods,
    pub text: Option<String>,
    pub unshifted: char,
}

pub struct MouseInput {
    pub action: mouse::Action,
    /// The button pressed or released, or held during a motion.
    pub button: Option<mouse::Button>,
    pub mods: key::Mods,
    /// Relative to the grid's origin.
    pub x: f32,
    pub y: f32,
}

enum Msg {
    Output(Vec<u8>),
    Key(KeyInput),
    Text(String),
    Paste(String),
    Scroll { lines: isize, x: f32, y: f32 },
    ScrollTo(usize),
    Mouse(MouseInput),
    Copy,
    Resize(GridSize),
    Visible(bool),
    Colors(Colors),
    Exited,
    Shutdown,
}

enum Update {
    Frame(Frame),
    Status(Option<Status>),
    /// Text for the clipboard, or for the primary selection on Linux.
    Copy {
        text: String,
        primary: bool,
    },
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
        colors: Colors,
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
        let (updates_tx, updates) = smol::channel::bounded(UPDATE_QUEUE);

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
    input: Cell<Input>,
    status: Option<Status>,
    _updates: Task<()>,
}

/// What the user last typed into the CLI. A draft lives only in the CLI, so the
/// CLI can't be stopped without losing it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Input {
    /// Nothing since the CLI started.
    Untouched,
    /// Keys that may have left a draft. Any key counts, since one that types
    /// nothing can still bring text back, as history and yank do.
    Draft,
    /// A draft and then Enter while the CLI was at rest, at this many seconds
    /// since the Unix epoch.
    Entered(i64),
}

impl Input {
    /// Enter while the CLI works can queue the draft behind prompts queued
    /// before it, which reach the transcript after it, so the draft stays one
    /// until it's entered at rest, when nothing is queued.
    fn key(self, enter: bool, at_rest: bool, now: i64) -> Self {
        match (self, enter) {
            (Input::Draft, true) if at_rest => Input::Entered(now),
            (input, true) => input,
            (_, false) => Input::Draft,
        }
    }

    /// Whether what was typed has been sent, given when the session's last
    /// prompt was. Enter doesn't always send it, as after a trailing backslash
    /// in pi, and the CLI can start working without it, as pi does when it
    /// compacts; only the prompt reaching the transcript shows it was sent.
    /// shortcut: transcript times are in seconds, so a draft entered in the
    /// same second as the prompt before it counts as sent.
    pub fn sent(self, last_prompt: impl FnOnce() -> Option<i64>) -> bool {
        match self {
            Input::Untouched => true,
            Input::Draft => false,
            Input::Entered(at) => last_prompt().is_some_and(|prompt| prompt >= at),
        }
    }
}

pub struct Exited;

impl EventEmitter<Exited> for Terminal {}

/// The CLI reported what it's doing. `None` when it cleared its status.
pub struct StatusChanged(pub Option<Status>);

impl EventEmitter<StatusChanged> for Terminal {}

impl Terminal {
    pub fn new(pty: Pty, cx: &mut Context<Self>) -> Self {
        let updates = pty.updates;
        let task = cx.spawn(async move |this, cx| {
            while let Ok(update) = updates.recv().await {
                // Only the newest frame matters, so skip any that queued up.
                let mut frame = None;
                let mut statuses = Vec::new();
                let mut copies = Vec::new();
                let mut exited = false;
                for update in
                    std::iter::once(update).chain(std::iter::from_fn(|| updates.try_recv().ok()))
                {
                    match update {
                        Update::Frame(next) => frame = Some(next),
                        Update::Status(status) => statuses.push(status),
                        Update::Copy { text, primary } => copies.push((text, primary)),
                        Update::Exited => exited = true,
                    }
                }
                let updated = this.update(cx, |this, cx| {
                    if let Some(frame) = frame {
                        this.frame = Some(Arc::new(frame));
                    }
                    for status in statuses {
                        this.status = status;
                        cx.emit(StatusChanged(status));
                    }
                    for (text, primary) in copies {
                        let item = ClipboardItem::new_string(text);
                        if primary {
                            #[cfg(any(target_os = "linux", target_os = "freebsd"))]
                            cx.write_to_primary(item);
                        } else {
                            cx.write_to_clipboard(item);
                        }
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
            input: Cell::new(Input::Untouched),
            status: None,
            _updates: task,
        }
    }

    pub fn frame(&self) -> Option<Arc<Frame>> {
        self.frame.clone()
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

    pub fn input(&self) -> Input {
        self.input.get()
    }

    pub fn key(&self, input: KeyInput) {
        let enter = input.key == key::Key::Enter && input.mods.is_empty();
        let at_rest = matches!(self.status, Some(Status::Idle | Status::Done));
        let now = jiff::Timestamp::now().as_second();
        self.input.set(self.input.get().key(enter, at_rest, now));
        self.tx.send(Msg::Key(input)).ok();
    }

    /// Text typed through an input method, sent as is.
    pub fn text(&self, text: String) {
        self.input.set(Input::Draft);
        self.tx.send(Msg::Text(text)).ok();
    }

    pub fn paste(&self, text: String) {
        self.input.set(Input::Draft);
        self.tx.send(Msg::Paste(text)).ok();
    }

    /// Negative `lines` scroll up. `x`/`y` are relative to the grid's origin.
    pub fn scroll(&self, lines: isize, x: f32, y: f32) {
        self.tx.send(Msg::Scroll { lines, x, y }).ok();
    }

    /// Scrolls to `row` rows below the top of the scrollback.
    pub fn scroll_to(&self, row: usize) {
        self.tx.send(Msg::ScrollTo(row)).ok();
    }

    pub fn mouse(&self, input: MouseInput) {
        self.tx.send(Msg::Mouse(input)).ok();
    }

    /// Copies the selection to the clipboard.
    pub fn copy(&self) {
        self.tx.send(Msg::Copy).ok();
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

    pub fn set_colors(&self, colors: Colors) {
        self.tx.send(Msg::Colors(colors)).ok();
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
    let mut exiting = false;
    let mut last_frame = Instant::now() - FRAME_INTERVAL;

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
                let to_px = |cells: u16, size: f32| {
                    (cells as u32 * size as u32).min(u16::MAX as u32) as u16
                };
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
            Some(Msg::Exited) => {
                // What the CLI printed last is part of what it did, so it goes
                // out before the news that it exited.
                exiting = true;
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

        if exiting {
            if dirty && visible {
                let frame = build_frame(&mut render_state, &mut rows, &mut cells, &terminal)?;
                if updates.send_blocking(Update::Frame(frame)).is_err() {
                    break;
                }
                dirty = false;
                last_frame = Instant::now();
            }
            exiting = false;
            updates.send_blocking(Update::Exited).ok();
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

fn set_colors(terminal: &mut VtTerminal<'_, '_>, colors: Colors) -> Result<()> {
    let mut palette = terminal.default_color_palette()?;
    palette.0[..colors.ansi.len()].copy_from_slice(&colors.ansi);
    terminal
        .set_default_bg_color(Some(colors.background))?
        .set_default_fg_color(Some(colors.foreground))?
        .set_default_color_palette(Some(palette))?;
    Ok(())
}

/// Turns left-button presses, drags and releases into the terminal's
/// selection: a drag selects cells, a double click a word, a triple click a
/// line.
struct Selecting {
    gesture: Gesture<'static>,
    press: gesture::PressEvent<'static>,
    drag: gesture::DragEvent<'static>,
    release: gesture::ReleaseEvent<'static>,
    tick: gesture::AutoscrollTickEvent<'static>,
    /// The left button went down to select and hasn't been released.
    active: bool,
    /// Where the pointer last was, in whole pixels, maybe past the grid.
    pointer: (f32, f32),
    /// When to next scroll towards a pointer dragged past the grid's top or
    /// bottom.
    next_tick: Option<Instant>,
    /// Presses are timed from here, which tells double clicks apart.
    epoch: Instant,
}

impl Selecting {
    fn new() -> Result<Self> {
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
    fn mouse(
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
    fn autoscroll(&mut self, terminal: &VtTerminal<'_, '_>, size: GridSize) -> Result<()> {
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
fn selection_text(terminal: &VtTerminal<'_, '_>) -> Option<String> {
    let options = FormatOptions::new()
        .with_emit_format(Format::Plain)
        .with_unwrap(true)
        .with_trim(true);
    let bytes = terminal.format_selection_alloc(None, options).ok()??;
    Some(String::from_utf8_lossy(&bytes).into_owned()).filter(|text| !text.is_empty())
}

/// Ghostty measures the mouse in whole-pixel cells, so a position over cells
/// of a fractional size is scaled onto cells of the truncated size.
fn whole_pixel_position(size: GridSize, x: f32, y: f32) -> (f32, f32) {
    (
        x / size.cell_width * size.cell_width.trunc(),
        y / size.cell_height * size.cell_height.trunc(),
    )
}

/// The viewport cell at a whole-pixel position, or the nearest one when it's
/// past the grid's edges.
fn viewport_cell(size: GridSize, x: f32, y: f32) -> PointCoordinate {
    PointCoordinate {
        x: ((x / size.cell_width.trunc()) as u16).min(size.cols.saturating_sub(1)),
        y: ((y / size.cell_height.trunc()) as u32).min(size.rows.saturating_sub(1).into()),
    }
}

fn geometry(size: GridSize) -> gesture::Geometry {
    gesture::Geometry {
        columns: size.cols.into(),
        cell_width: size.cell_width as u32,
        padding_left: 0,
        screen_height: size.rows as u32 * size.cell_height as u32,
    }
}

fn encoder_size(size: GridSize) -> mouse::EncoderSize {
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
    let mut selection = Vec::new();
    let mut text = String::new();
    let mut row_iter = rows.update(&snapshot)?;
    while let Some(row) = row_iter.next() {
        selection.push(row.selection()?);
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
        selection,
        cursor,
        background: colors.background,
        scrollbar: terminal.scrollbar()?,
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

    fn colors() -> Colors {
        Colors {
            background: RgbColor::default(),
            foreground: white(),
            ansi: crate::appearance::DARK_ANSI,
        }
    }

    /// The first row of a frame as text, as the tests' scripts print it.
    fn row_text(frame: &Frame) -> String {
        frame.rows[0].iter().map(|run| run.text.as_str()).collect()
    }

    #[test]
    fn keeps_a_draft_until_its_prompt_is_in_the_transcript() {
        assert!(Input::Untouched.sent(|| None));
        assert!(Input::Untouched.key(true, true, 10).sent(|| None));
        // Keys that type nothing can bring text back, like Up and Ctrl+Y.
        assert!(!Input::Untouched.key(false, true, 10).sent(|| Some(10)));
        // Entered while the CLI works, a prompt queued before it can reach
        // the transcript later.
        assert_eq!(Input::Draft.key(true, false, 12), Input::Draft);
        let entered = Input::Draft.key(true, true, 12);
        assert!(entered.sent(|| Some(12)));
        // Enter after a trailing backslash in pi, or on a menu, sends nothing,
        // whatever the CLI does next.
        assert!(!entered.sent(|| Some(11)));
        assert!(!entered.sent(|| None));
        // A second Enter on what was sent adds nothing to lose.
        assert_eq!(entered.key(true, true, 20), entered);
        assert_eq!(entered.key(false, true, 20), Input::Draft);
    }

    #[test]
    fn renders_pty_output_into_styled_runs() {
        let script = r"printf 'hello \033[1;31mred\033[0m'; sleep 1";
        let pty = Pty::spawn(
            "sh",
            &["-c".into(), script.into()],
            &[],
            Path::new("/"),
            colors(),
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
    fn reports_status_and_answers_queries_in_order() {
        // What the terminal answers comes back as input, made visible by `tr`.
        let script = r"stty raw -echo
            printf '\033]7501;state=working\033\\\033[c\033]7501;?\033\\\033[c'
            head -c 36 | tr '\033' E; sleep 1";
        let pty = Pty::spawn(
            "sh",
            &["-c".into(), script.into()],
            &[],
            Path::new("/"),
            colors(),
        )
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        let (mut status, mut answers) = (None, None);
        while status.is_none() || answers.is_none() {
            assert!(Instant::now() < deadline, "no status, or no answers");
            match smol::block_on(pty.updates.recv()).unwrap() {
                Update::Status(reported) => status = Some(reported),
                Update::Frame(frame) => {
                    let text = row_text(&frame);
                    if !text.trim().is_empty() {
                        answers = Some(text.trim().to_string());
                    }
                }
                Update::Copy { .. } => {}
                Update::Exited => panic!("exited early"),
            }
        }
        assert_eq!(status, Some(Some(Status::Working)));
        assert_eq!(
            answers.as_deref(),
            Some(r"E[?62;1;6;22cE]7501;?E\E[?62;1;6;22c")
        );
    }

    const GRID: GridSize = GridSize {
        cols: 80,
        rows: 24,
        cell_width: 10.,
        cell_height: 20.,
    };

    /// A left button event at `x` on the first row.
    fn mouse(action: mouse::Action, x: f32, mods: key::Mods) -> Msg {
        Msg::Mouse(MouseInput {
            action,
            button: Some(mouse::Button::Left),
            mods,
            x,
            y: 5.,
        })
    }

    fn pointer(action: mouse::Action, button: Option<mouse::Button>, x: f32, y: f32) -> Msg {
        Msg::Mouse(MouseInput {
            action,
            button,
            mods: key::Mods::empty(),
            x,
            y,
        })
    }

    /// The text copied once the button is released.
    fn wait_for_copy(pty: &Pty) -> String {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(Instant::now() < deadline, "nothing was selected");
            if let Update::Copy { text, .. } = next_update(pty, deadline) {
                return text;
            }
        }
    }

    /// The next update, failing the test once `deadline` passes without one.
    fn next_update(pty: &Pty, deadline: Instant) -> Update {
        let timeout = async {
            smol::Timer::at(deadline).await;
            None
        };
        smol::block_on(async { pty.updates.recv().await.ok() }.or(timeout))
            .expect("no update in time")
    }

    /// Waits for a frame whose `row` contains `text`.
    fn wait_for_row(pty: &Pty, row: usize, text: &str) -> Frame {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(Instant::now() < deadline, "no frame with {text:?}");
            if let Update::Frame(frame) = next_update(pty, deadline)
                && frame.rows[row]
                    .iter()
                    .map(|run| run.text.as_str())
                    .collect::<String>()
                    .contains(text)
            {
                return frame;
            }
        }
    }

    #[test]
    fn selects_dragged_text_and_copies_it() {
        let pty = Pty::spawn(
            "sh",
            &["-c".into(), "printf 'hello world'; sleep 2".into()],
            &[],
            Path::new("/"),
            colors(),
        )
        .unwrap();
        pty.tx.send(Msg::Resize(GRID)).unwrap();
        wait_for_row(&pty, 0, "hello world");

        pty.tx
            .send(mouse(mouse::Action::Press, 2., key::Mods::empty()))
            .unwrap();
        pty.tx
            .send(mouse(mouse::Action::Motion, 48., key::Mods::empty()))
            .unwrap();
        pty.tx
            .send(mouse(mouse::Action::Release, 48., key::Mods::empty()))
            .unwrap();
        pty.tx.send(Msg::Copy).unwrap();

        let mut copies = Vec::new();
        let mut selected = None;
        let deadline = Instant::now() + Duration::from_secs(5);
        while copies.len() < 2 || selected.is_none() {
            assert!(Instant::now() < deadline, "{copies:?}, {selected:?}");
            match next_update(&pty, deadline) {
                Update::Copy { text, primary } => copies.push((text, primary)),
                Update::Frame(frame) => {
                    selected = selected.or(frame.selection[0].map(|row| (row.start_x, row.end_x)));
                }
                _ => {}
            }
        }
        assert_eq!(
            copies,
            [("hello".to_string(), true), ("hello".to_string(), false)]
        );
        assert_eq!(selected, Some((0, 4)));
    }

    #[test]
    fn double_click_selects_a_word() {
        let pty = Pty::spawn(
            "sh",
            &["-c".into(), "printf 'hello world'; sleep 2".into()],
            &[],
            Path::new("/"),
            colors(),
        )
        .unwrap();
        pty.tx.send(Msg::Resize(GRID)).unwrap();
        wait_for_row(&pty, 0, "hello world");

        for _ in 0..2 {
            for action in [mouse::Action::Press, mouse::Action::Release] {
                pty.tx.send(mouse(action, 72., key::Mods::empty())).unwrap();
            }
        }

        assert_eq!(wait_for_copy(&pty), "world");
    }

    #[test]
    fn selects_by_the_cells_drawn_when_they_are_fractional() {
        let pty = Pty::spawn(
            "sh",
            &["-c".into(), "printf 'ABCDEFGHIJKLMNOP'; sleep 2".into()],
            &[],
            Path::new("/"),
            colors(),
        )
        .unwrap();
        pty.tx
            .send(Msg::Resize(GridSize {
                cell_width: 8.4,
                ..GRID
            }))
            .unwrap();
        wait_for_row(&pty, 0, "ABCDEFGHIJKLMNOP");

        // From the left edge of K to well past the middle of L.
        pty.tx
            .send(mouse(mouse::Action::Press, 84.1, key::Mods::empty()))
            .unwrap();
        pty.tx
            .send(mouse(mouse::Action::Motion, 99.7, key::Mods::empty()))
            .unwrap();
        pty.tx
            .send(mouse(mouse::Action::Release, 99.7, key::Mods::empty()))
            .unwrap();
        assert_eq!(wait_for_copy(&pty), "KL");
    }

    #[test]
    fn scrolls_while_selecting_above_the_grid() {
        let pty = Pty::spawn(
            "sh",
            &["-c".into(), "seq 100; sleep 5".into()],
            &[],
            Path::new("/"),
            colors(),
        )
        .unwrap();
        pty.tx.send(Msg::Resize(GRID)).unwrap();
        let offset = wait_for_row(&pty, 22, "100").scrollbar.offset;

        // Past the end of 83, so all of it is selected.
        let left = Some(mouse::Button::Left);
        pty.tx
            .send(pointer(mouse::Action::Press, left, 25., 100.))
            .unwrap();
        pty.tx
            .send(pointer(mouse::Action::Motion, left, 5., -30.))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(Instant::now() < deadline, "the viewport never scrolled");
            if let Update::Frame(frame) = next_update(&pty, deadline)
                && frame.scrollbar.offset + 3 <= offset
            {
                break;
            }
        }
        pty.tx
            .send(pointer(mouse::Action::Release, left, 5., -30.))
            .unwrap();
        // 77 was in the scrollback when the drag began.
        let text = wait_for_copy(&pty);
        assert!(
            text.contains("\n77\n") && text.ends_with("\n83"),
            "{text:?}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn forwards_the_mouse_to_a_cli_that_tracks_it() {
        // What the terminal sends comes back as input, made visible by `tr`.
        let script = r"stty raw -echo
            printf '\033[?1000h\033[?1006hready\r\n'
            head -c 18 | tr '\033' E; sleep 1";
        let pty = Pty::spawn(
            "sh",
            &["-c".into(), script.into()],
            &[],
            Path::new("/"),
            colors(),
        )
        .unwrap();
        pty.tx.send(Msg::Resize(GRID)).unwrap();
        wait_for_row(&pty, 0, "ready");

        // Shift selects instead, so the CLI never sees this drag.
        for (action, x) in [
            (mouse::Action::Press, 5.),
            (mouse::Action::Motion, 48.),
            (mouse::Action::Release, 48.),
        ] {
            pty.tx.send(mouse(action, x, key::Mods::SHIFT)).unwrap();
        }
        assert_eq!(wait_for_copy(&pty), "ready");
        // A press the CLI got has its release go there too, Shift or not.
        pty.tx
            .send(mouse(mouse::Action::Press, 25., key::Mods::empty()))
            .unwrap();
        pty.tx
            .send(mouse(mouse::Action::Release, 25., key::Mods::SHIFT))
            .unwrap();
        let frame = wait_for_row(&pty, 1, "E[<0;3;1ME[<4;3;1m");
        // The CLI's click drops the selection.
        assert!(frame.selection[0].is_none());
    }

    #[test]
    fn stops_scrolling_a_selection_once_hidden() {
        let pty = Pty::spawn(
            "sh",
            &["-c".into(), "seq 100; sleep 5".into()],
            &[],
            Path::new("/"),
            colors(),
        )
        .unwrap();
        pty.tx.send(Msg::Resize(GRID)).unwrap();
        let offset = wait_for_row(&pty, 22, "100").scrollbar.offset;

        let left = Some(mouse::Button::Left);
        pty.tx
            .send(pointer(mouse::Action::Press, left, 25., 100.))
            .unwrap();
        pty.tx
            .send(pointer(mouse::Action::Motion, left, 5., -30.))
            .unwrap();
        pty.tx.send(Msg::Visible(false)).unwrap();
        thread::sleep(Duration::from_millis(250));
        pty.tx.send(Msg::Visible(true)).unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Update::Frame(frame) = next_update(&pty, deadline) {
                assert_eq!(frame.scrollbar.offset, offset);
                break;
            }
        }
    }

    #[test]
    fn keeps_clicks_in_the_padding_from_a_cli() {
        // What the terminal sends comes back as input, made visible by `tr`.
        let script = r"stty raw -echo
            printf '\033[?1002h\033[?1006hready\r\n'
            head -c 18 | tr '\033' E; sleep 1";
        let pty = Pty::spawn(
            "sh",
            &["-c".into(), script.into()],
            &[],
            Path::new("/"),
            colors(),
        )
        .unwrap();
        pty.tx.send(Msg::Resize(GRID)).unwrap();
        wait_for_row(&pty, 0, "ready");

        let left = Some(mouse::Button::Left);
        // A drag from the padding into the grid, then a click in it.
        for (action, x, y) in [
            (mouse::Action::Press, -5., -5.),
            (mouse::Action::Motion, 15., 5.),
            (mouse::Action::Release, 15., 5.),
            (mouse::Action::Press, 25., 5.),
            (mouse::Action::Release, 25., 5.),
        ] {
            pty.tx.send(pointer(action, left, x, y)).unwrap();
        }
        wait_for_row(&pty, 1, "E[<0;3;1ME[<0;3;1m");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn reports_motion_once_per_cell() {
        // What the terminal sends comes back as input, made visible by `tr`.
        let script = r"stty raw -echo
            printf '\033[?1003h\033[?1006hready\r\n'
            head -c 30 | tr '\033' E; sleep 1";
        let pty = Pty::spawn(
            "sh",
            &["-c".into(), script.into()],
            &[],
            Path::new("/"),
            colors(),
        )
        .unwrap();
        pty.tx.send(Msg::Resize(GRID)).unwrap();
        wait_for_row(&pty, 0, "ready");

        // Hovering the padding isn't hovering the first cell.
        for (x, y) in [(2., 5.), (5., 5.), (25., 5.), (-5., -5.), (35., 5.)] {
            pty.tx
                .send(pointer(mouse::Action::Motion, None, x, y))
                .unwrap();
        }
        wait_for_row(&pty, 1, "E[<35;1;1ME[<35;3;1ME[<35;4;1M");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn reaps_the_child_once_it_exits() {
        let pty = Pty::spawn(
            "sh",
            &["-c".into(), "echo $$".into()],
            &[],
            Path::new("/"),
            colors(),
        )
        .unwrap();

        let mut pid = None;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(Instant::now() < deadline, "the child never exited");
            match smol::block_on(pty.updates.recv()).unwrap() {
                Update::Frame(frame) => {
                    pid = pid.or(row_text(&frame).trim().parse::<u32>().ok());
                }
                Update::Status(_) | Update::Copy { .. } => {}
                Update::Exited => break,
            }
        }
        // The output may reach the terminal after the reaper noticed the exit.
        while pid.is_none() && Instant::now() < deadline {
            match pty.updates.try_recv() {
                Ok(Update::Frame(frame)) => {
                    pid = row_text(&frame).trim().parse::<u32>().ok();
                }
                Ok(_) => {}
                Err(smol::channel::TryRecvError::Empty) => thread::sleep(Duration::from_millis(5)),
                Err(_) => break,
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
            colors(),
        )
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        let pid = loop {
            assert!(Instant::now() < deadline, "no frame with the pid");
            let Ok(Update::Frame(frame)) = smol::block_on(pty.updates.recv()) else {
                continue;
            };
            if let Ok(pid) = row_text(&frame).trim().parse::<u32>() {
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
            colors(),
        )
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        let pid = loop {
            assert!(Instant::now() < deadline, "no frame with the pid");
            let Ok(Update::Frame(frame)) = smol::block_on(pty.updates.recv()) else {
                continue;
            };
            if let Ok(pid) = row_text(&frame).trim().parse::<u32>() {
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
            colors(),
        )
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(Instant::now() < deadline, "no frame with the prompt");
            let Ok(Update::Frame(frame)) = smol::block_on(pty.updates.recv()) else {
                continue;
            };
            if row_text(&frame).contains("ready") {
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
