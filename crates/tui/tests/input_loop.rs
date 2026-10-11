//! **The terminal's bytes, through the driver's own loop, to what they do** (R69).
//!
//! `app`'s unit tests hand `App::key` a `Key` they built. What sits between the operator's
//! terminal and that call is the driver: the bytes one read consumed, rano's decoding of them,
//! `key_of`'s reading of the decoding, and `Link::tick` handing the keys to the head — or the
//! raw bytes to a `!term` pane — in order, then drawing, then sending what the keys asked for.
//! The review of 2026-10-11 found no test of that seam about input at all: `driver.rs`'s four
//! tests are about processes. These drive it with real bytes and assert on what the socket
//! carried, because *did the key do its act* is, for the acts that leave the head, a fact
//! about the socket.

use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use letibot_sessionlog::ScrubReport;
use letibot_sessionlog::protocol::{ClientFrame, PROTOCOL_VERSION, ServerFrame};
use letibot_sessionlog::registry::SessionWiring;
use letibot_sessionlog::wire::{FrameReader, FrameWriter};

use letibot_tui::app::App;
use letibot_tui::app::{Key, key_of};
use letibot_tui::driver::Link;
use letibot_tui::ui::render::RenderConfig;

const SESSION: &str = "s-input";
const SIZE: (usize, usize) = (100, 30);

fn socket_path(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("letibot-input-{tag}-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

/// **A daemon that says hello and records what it is sent**, answering a `TermOpen` with a
/// screen so a pane has something to draw.
fn recording_daemon(path: PathBuf) -> Receiver<ClientFrame> {
    let (tx, rx) = std::sync::mpsc::channel();
    let listener = UnixListener::bind(&path).expect("the socket binds");
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("the attach");
        let mut r = FrameReader::new(stream.try_clone().unwrap());
        let mut w = FrameWriter::new(stream);
        w.write(&ServerFrame::Hello {
            protocol_version: PROTOCOL_VERSION,
            session_id: SESSION.into(),
            head_id: "h1".into(),
            dropped: 0,
            snapshot: None,
            resumed_from: None,
            scrubbed: ScrubReport::default(),
            wiring: SessionWiring::default(),
            sessions: Vec::new(),
        })
        .unwrap();
        while let Ok(f) = r.read::<ClientFrame>() {
            if let ClientFrame::TermOpen { .. } = &f {
                // The pane's number first, as the real daemon sends it (protocol 44).
                w.write(&ServerFrame::TermAttached {
                    pane: 1,
                    command: "the program".into(),
                    opened: true,
                })
                .unwrap();
                w.write(&ServerFrame::TermOutput {
                    pane: 1,
                    bytes: b"\x1b[2J\x1b[Hthe program\r\n".to_vec(),
                })
                .unwrap();
            }
            let _ = tx.send(f);
        }
    });
    rx
}

fn app() -> App {
    App::new(RenderConfig {
        width: SIZE.0,
        color: false,
        ..RenderConfig::default()
    })
}

/// One pass of the driver's loop over one read: the keys are rano's decoding of `raw`, read
/// through [`key_of`], exactly as `head::read_routed` makes them.
fn step(link: &mut Link, a: &mut App, raw: &[u8]) {
    let keys: Vec<Key> = rano::term::decode(raw)
        .into_iter()
        .filter_map(key_of)
        .collect();
    let mut sink = |_lines: &[String], _cursor| {};
    link.tick(a, SIZE, &keys, raw, &mut sink);
}

fn settle(link: &mut Link, a: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut quiet = 0;
    while Instant::now() < deadline && quiet < 4 {
        let before = a.seq;
        step(link, a, &[]);
        quiet = if a.seq == before { quiet + 1 } else { 0 };
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn sent(frames: &Receiver<ClientFrame>) -> Vec<ClientFrame> {
    let mut out = Vec::new();
    while let Ok(f) = frames.recv_timeout(Duration::from_millis(200)) {
        out.push(f);
    }
    out
}

/// An SGR left press at 0-based screen cell `(x, y)`, as the terminal sends it.
fn press(x: usize, y: usize) -> String {
    format!("\x1b[<0;{};{}M", x + 1, y + 1)
}

/// **Two chords in one read are two acts, in order, and the second pane is the one in front.**
/// `ctrl-q` then `ctrl-t` arriving as one read asks the daemon for both lists, and the frame the
/// tick draws is the todos pane — the pane the keys now reach — with the jobs pane behind it.
#[test]
fn two_chords_in_one_read_ask_for_both_and_leave_the_second_in_front() {
    let path = socket_path("chords");
    let frames = recording_daemon(path.clone());
    let mut a = app();
    let mut link = Link::open(&path, SESSION, 0, "tui", "input-chords").expect("attach");
    settle(&mut link, &mut a);
    let _ = sent(&frames);

    step(&mut link, &mut a, b"\x11\x14");
    let asked = sent(&frames);
    assert!(
        asked.iter().any(|f| matches!(f, ClientFrame::ListJobs)),
        "ctrl-q's read reached the daemon: {asked:?}"
    );
    assert!(
        asked.iter().any(|f| matches!(f, ClientFrame::ListTodos)),
        "ctrl-t's read reached the daemon: {asked:?}"
    );
    let frame = a.screen(SIZE.0, SIZE.1).join("\n");
    assert!(
        frame.contains("todos — "),
        "the todos pane is in front:\n{frame}"
    );
    assert!(
        !frame.contains("background jobs"),
        "the jobs pane is drawn over the todos pane the keys reach:\n{frame}"
    );
}

/// **A click (press and release) in a read reaches the card the last frame drew, and the Enter after it in
/// the same read answers what the click selected.** The second ctrl-c opens the quit card; a
/// press on its second choice selects it; Enter takes it — so the STOP leaves the head, which
/// is the socket's fact. The click is measured against the frame the tick before drew.
#[test]
fn a_click_report_then_enter_in_one_read_answers_the_row_clicked() {
    let path = socket_path("quit");
    let frames = recording_daemon(path.clone());
    let mut a = app();
    let mut link = Link::open(&path, SESSION, 0, "tui", "input-quit").expect("attach");
    settle(&mut link, &mut a);

    step(&mut link, &mut a, b"\x03\x03");
    let frame = a.screen(SIZE.0, SIZE.1);
    let y = frame
        .iter()
        .position(|l| l.contains("leave and stop the daemon"))
        .unwrap_or_else(|| panic!("the quit card is drawn: {frame:#?}"));
    // A click is the press AND the release: a press alone may yet become a drag (a selection).
    let read = format!("{}{}\r", press(10, y), press(10, y).replace('M', "m"));
    step(&mut link, &mut a, read.as_bytes());
    let out = sent(&frames);
    assert!(
        out.iter().any(|f| matches!(f, ClientFrame::Stop { .. })),
        "the clicked choice was not the one Enter took: {out:?}"
    );
}

/// **A `!term` pane is never handed the operator's mouse**, through the loop: while a pane is
/// drawn, the read's raw bytes are the program's, and a wheel report and a click in them are
/// stripped before anything is forwarded — the keys around them still go (R67 item 6, at the
/// driver's seam rather than at `App::pane_keys`).
#[test]
fn the_pane_gets_the_keys_of_a_read_and_none_of_its_mouse_reports() {
    let path = socket_path("pane-mouse");
    let frames = recording_daemon(path.clone());
    let mut a = app();
    let mut link = Link::open(&path, SESSION, 0, "tui", "input-pane-mouse").expect("attach");
    settle(&mut link, &mut a);

    step(&mut link, &mut a, b"!term top\r");
    settle(&mut link, &mut a);
    assert!(a.pane_open(), "the pane is up");
    let _ = sent(&frames);

    let read = format!("q\x1b[<64;5;5M{}x", press(3, 3));
    step(&mut link, &mut a, read.as_bytes());
    let typed: Vec<u8> = sent(&frames)
        .into_iter()
        .filter_map(|f| match f {
            ClientFrame::TermInput { bytes, .. } => Some(bytes),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(typed, b"qx", "the program was handed a mouse report");
}
