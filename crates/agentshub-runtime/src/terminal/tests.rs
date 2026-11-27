use std::fs;

use unicode_segmentation::UnicodeSegmentation as _;

use super::*;

fn white() -> RgbColor {
    RgbColor {
        r: 255,
        g: 255,
        b: 255,
    }
}

fn colors() -> Colors {
    // Black, then red, which the scripts print in.
    let mut ansi = [RgbColor::default(); 16];
    ansi[1] = RgbColor { r: 255, g: 0, b: 0 };
    Colors {
        background: RgbColor::default(),
        foreground: white(),
        ansi,
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
            assert_eq!(
                row[0].cell_texts().collect::<Vec<_>>(),
                ["h", "e", "l", "l", "o", " "]
            );
            assert_eq!(row[0].fg, white());
            break (red.fg, red.attrs.bold);
        }
    };
    assert!(red.r > red.g && red.r > red.b, "{red:?} isn't red");
    assert!(bold);
}

#[test]
fn counts_cells_whose_text_joins_into_one_grapheme() {
    // Two regional indicators in cells of their own make one grapheme, the
    // box line after them still has a cell of its own.
    let text = "\u{1F1FA}\u{1F1F8}│";
    let run = Run {
        col: 4,
        cells: 3,
        text: text.into(),
        offsets: vec![0, 4, 8],
        fg: white(),
        bg: None,
        attrs: frame::Attrs::default(),
        wide: false,
    };
    assert_eq!(text.graphemes(true).count(), 2);
    assert_eq!(
        run.cell_texts().collect::<Vec<_>>(),
        ["\u{1F1FA}", "\u{1F1F8}", "│"]
    );
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
            Update::Exited(_) => panic!("exited early"),
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
    smol::block_on(async { pty.updates.recv().await.ok() }.or(timeout)).expect("no update in time")
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
            Update::Exited(success) => {
                assert!(success, "sh exited with an error");
                break;
            }
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
    while fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| !stat.contains(") Z ")) {
        assert!(Instant::now() < deadline, "{pid} outlived the terminal");
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn kills_jobs_in_process_groups_of_their_own() {
    // Job control gives the job a process group of its own, as an
    // interactive shell does.
    let pty = Pty::spawn(
        "sh",
        &[
            "-c".into(),
            "set -m; (trap '' HUP; exec sleep 100) & echo $!; wait".into(),
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
    pty.tx.send(Msg::Shutdown).unwrap();
    pty.reaped.recv_blocking().unwrap_err();
    // Gone, or a zombie left for its new parent to reap.
    while fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| !stat.contains(") Z ")) {
        assert!(Instant::now() < deadline, "{pid} outlived the terminal");
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn kills_what_left_the_session_on_shutdown() {
    if !scope::available() {
        eprintln!("skipped: systemd can't put the CLI in a scope");
        return;
    }
    // The pause lets the CLI be moved into its scope first.
    let pty = Pty::spawn(
        "sh",
        &[
            "-c".into(),
            "sleep 0.2; setsid sleep 100 & echo $!; wait".into(),
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
    // Long enough for `setsid` to have left the session.
    thread::sleep(Duration::from_millis(100));
    pty.tx.send(Msg::Shutdown).unwrap();
    pty.reaped.recv_blocking().unwrap_err();
    while fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| !stat.contains(") Z ")) {
        assert!(Instant::now() < deadline, "{pid} outlived the terminal");
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn kills_what_the_child_left_behind_when_it_exits() {
    if !scope::available() {
        eprintln!("skipped: systemd can't put the CLI in a scope");
        return;
    }
    let pty = Pty::spawn(
        "sh",
        &[
            "-c".into(),
            "sleep 0.2; setsid sleep 100 & echo $!; sleep 0.1".into(),
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
    pty.reaped.recv_blocking().unwrap_err();
    while fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| !stat.contains(") Z ")) {
        assert!(Instant::now() < deadline, "{pid} outlived the CLI");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn says_a_pid_in_use_may_be_running_without_waiting() {
    let pid = Pid::from_shared(Arc::new(Mutex::new(Some(1))));
    let (held_tx, held) = mpsc::channel();
    let (release_tx, release) = mpsc::channel::<()>();
    let holder = thread::spawn({
        let pid = pid.clone();
        move || {
            pid.with(|_| {
                held_tx.send(()).unwrap();
                release.recv().ok()
            })
        }
    });
    held.recv().unwrap();
    assert!(pid.may_be_running());
    release_tx.send(()).unwrap();
    holder.join().unwrap();
    pid.reaped();
    assert!(!pid.may_be_running());
}

#[test]
fn forgets_the_pid_once_the_cli_is_reaped() {
    let pty = Pty::spawn("true", &[], &[], Path::new("/"), colors()).unwrap();
    pty.reaped.recv_blocking().unwrap_err();
    assert_eq!(*pty.pid.lock().unwrap(), None);
}

/// The pid a CLI prints, which only a frame shows.
#[cfg(target_os = "linux")]
fn printed_pid(pty: &Pty) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(Instant::now() < deadline, "no frame with the pid");
        let Ok(Update::Frame(frame)) = smol::block_on(pty.updates.recv()) else {
            continue;
        };
        if let Ok(pid) = row_text(&frame).trim().parse::<u32>() {
            break pid;
        }
    }
}

#[cfg(target_os = "linux")]
fn gone(pid: u32) -> bool {
    !fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| !stat.contains(") Z "))
}

#[cfg(target_os = "linux")]
#[test]
fn counts_as_reaped_once_what_left_the_session_is_gone() {
    if !scope::available() {
        eprintln!("skipped: systemd can't put the CLI in a scope");
        return;
    }
    // What it leaves ignores SIGTERM, so it lasts until systemd kills it.
    let pty = Pty::spawn(
        "sh",
        &[
            "-c".into(),
            "sleep 0.2; setsid sh -c \"trap '' TERM; exec sleep 100\" & echo $!; sleep 0.1".into(),
        ],
        &[],
        Path::new("/"),
        colors(),
    )
    .unwrap();
    let pid = printed_pid(&pty);
    pty.reaped.recv_blocking().unwrap_err();
    assert!(gone(pid), "{pid} outlived the CLI");
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "stops every scope of the tests running with it: cargo test -- --ignored"]
fn quitting_stops_the_scope_of_a_cli_not_reaped_yet() {
    if !scope::available() {
        eprintln!("skipped: systemd can't put the CLI in a scope");
        return;
    }
    let pty = Pty::spawn(
        "sh",
        &[
            "-c".into(),
            "sleep 0.2; setsid sleep 100 & echo $!; exec sleep 100".into(),
        ],
        &[],
        Path::new("/"),
        colors(),
    )
    .unwrap();
    let pid = printed_pid(&pty);
    scope::stop_all();
    scope::wait_stopped();
    assert!(gone(pid), "{pid} outlived quitting");
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
