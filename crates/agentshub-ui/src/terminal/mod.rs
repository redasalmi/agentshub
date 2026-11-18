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
use std::sync::{Arc, Mutex, PoisonError, mpsc};
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

mod frame;
mod pty;
mod run;
mod scope;
mod selection;

pub use scope::{Scope, set as set_scope};

pub use frame::{Colors, Frame, GridSize, Run};
use frame::{build_frame, set_colors};
pub use pty::{Pty, wait_reaped};
use run::run;
use selection::{Selecting, encoder_size, selection_text, viewport_cell, whole_pixel_position};

#[cfg(test)]
use agentshub_core::process::KILL_GRACE;
use agentshub_core::process::{Pid, kill_session};
use smol::future::FutureExt as _;

use agentshub_core::status::{self, Report, Status};

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
    Exited(bool),
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
    Exited(bool),
}

pub struct Terminal {
    tx: mpsc::Sender<Msg>,
    frame: Option<Arc<Frame>>,
    size: Option<GridSize>,
    exited: bool,
    pid: Pid,
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

pub struct Exited {
    /// Whether the CLI exited with status 0, rather than failing or being
    /// killed.
    pub success: bool,
}

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
                let mut exited = None;
                for update in
                    std::iter::once(update).chain(std::iter::from_fn(|| updates.try_recv().ok()))
                {
                    match update {
                        Update::Frame(next) => frame = Some(next),
                        Update::Status(status) => statuses.push(status),
                        Update::Copy { text, primary } => copies.push((text, primary)),
                        Update::Exited(success) => exited = Some(success),
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
                    if let Some(success) = exited
                        && !this.exited
                    {
                        this.exited = true;
                        cx.emit(Exited { success });
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
            pid: Pid::from_shared(pty.pid),
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

    pub fn pid(&self) -> Pid {
        self.pid.clone()
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

    /// Kills the CLI. The returned channel closes once it has been reaped, and
    /// nothing is left in its scope, which `wait_reaped` waits for.
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

#[cfg(test)]
mod tests;
