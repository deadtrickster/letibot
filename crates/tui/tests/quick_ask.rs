//! A quick ask at the socket: `alt-k r` in a real pane, through the router the loop uses, and the
//! `Prompt` the daemon reads - with its `context` and the ask's answer mode on it.

use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use letibot_sessionlog::ScrubReport;
use letibot_sessionlog::protocol::{ClientFrame, PROTOCOL_VERSION, ServerFrame};
use letibot_sessionlog::registry::SessionWiring;
use letibot_sessionlog::wire::{FrameReader, FrameWriter};

use letibot_tui::app::App;
use letibot_tui::driver::Link;
use letibot_tui::ui::render::RenderConfig;

use rano::term::{Event, KeyCode, KeyEvent, Mods};

const SESSION: &str = "s-quick-ask";
const SIZE: (usize, usize) = (120, 40);

/// A workspace of one file, removed when the test ends.
struct Workspace(PathBuf);

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn workspace(tag: &str) -> Workspace {
    let mut dir = letibot_sessionlog::testing::scratch_root();
    dir.push(format!("letibot-ask-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("article.txt"),
        "The first transatlantic cable was laid in 1866.\n\
         It carried one message a minute.\n\n\
         A gap in the record remains.\n",
    )
    .unwrap();
    Workspace(dir)
}

fn socket_path(tag: &str) -> PathBuf {
    // `scratch_root`, not `temp_dir`: a socket path must fit `sun_path`.
    let mut p = letibot_sessionlog::testing::scratch_root();
    p.push(format!("letibot-ask-{tag}-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

/// A daemon that says hello with the session's workspace and records what it is sent.
fn recording_daemon(path: PathBuf, workspace: &str) -> Receiver<ClientFrame> {
    let (tx, rx) = std::sync::mpsc::channel();
    let wiring = SessionWiring {
        workspace: workspace.to_string(),
        ..SessionWiring::default()
    };
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
            wiring,
            sessions: Vec::new(),
        })
        .unwrap();
        while let Ok(f) = r.read::<ClientFrame>() {
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

/// One pass of the driver's loop over no keys: what the head queued leaves, and a frame is drawn.
fn step(link: &mut Link, a: &mut App) {
    let mut sink = |_lines: &[String], _cursor| {};
    link.tick(a, SIZE, &[], b"", &mut sink);
}

/// A read of the terminal's, routed the way `head::read_routed` routes it: the pane's share to rano,
/// then its tick, then the head's own keys - and only then does the loop send what it queued.
fn read(link: &mut Link, a: &mut App, events: Vec<Event>) {
    let (keys, _) = a.route(events);
    a.editor_tick(Instant::now());
    let mut sink = |_lines: &[String], _cursor| {};
    link.tick(a, SIZE, &keys, b"", &mut sink);
}

fn plain(c: char) -> Event {
    Event::Key(KeyEvent::new(KeyCode::Char(c), Mods::NONE))
}

fn sent(frames: &Receiver<ClientFrame>) -> Vec<ClientFrame> {
    let mut out = Vec::new();
    while let Ok(f) = frames.recv_timeout(Duration::from_millis(200)) {
        out.push(f);
    }
    out
}

/// rano loads a file on a thread of its own, so the pane is ready when its text is on the screen -
/// the title edge names the file and a row carries a word of it.
fn until_the_file_is_up(link: &mut Link, a: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        a.editor_tick(Instant::now());
        step(link, a);
        let screen = a.screen(SIZE.0, SIZE.1);
        if screen.iter().any(|row| row.contains("article.txt"))
            && screen.iter().any(|row| row.contains("1866"))
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!(
        "the pane never drew the file:\n{}",
        a.screen(SIZE.0, SIZE.1).join("\n")
    );
}

/// **`alt-k r` in the pane, and the daemon receives the ask.** The frame that leaves carries the
/// preset, the answer mode the daemon teaches that fence by, and the paragraph as an editor place -
/// there is no composer round trip anywhere in it.
#[test]
fn alt_k_r_in_the_pane_sends_a_prompt_the_daemon_reads_as_an_ask() {
    let ws = workspace("ask");
    let file = ws.0.join("article.txt");
    let path = socket_path("ask");
    let frames = recording_daemon(path.clone(), &ws.0.display().to_string());
    let mut a = app();
    let mut link = Link::open(&path, SESSION, 0, "tui", "quick-ask").expect("attach");
    let mut sink = |_lines: &[String], _cursor| {};
    link.tick(&mut a, SIZE, &[], b"", &mut sink);
    let _ = sent(&frames);

    // `ctrl-e` on the empty prompt opens the pane on rano's own open-file prompt; the path is typed
    // into that, and Enter takes it.
    link.tick(
        &mut a,
        SIZE,
        &[letibot_tui::app::Key::CtrlE],
        b"",
        &mut sink,
    );
    read(&mut link, &mut a, Vec::new());
    let typed: Vec<Event> = file
        .display()
        .to_string()
        .chars()
        .map(plain)
        .chain(std::iter::once(Event::Key(KeyEvent::plain(KeyCode::Enter))))
        .collect();
    let (keys, _) = a.route(typed);
    assert!(keys.is_empty(), "the open prompt took every key: {keys:?}");
    until_the_file_is_up(&mut link, &mut a);
    let _ = sent(&frames);

    // The ask: the prefix, then the preset's letter - a bare `r` under the `alt-k` menu, which is
    // what rano binds a preset to.
    read(
        &mut link,
        &mut a,
        vec![Event::Key(KeyEvent::new(KeyCode::Char('k'), Mods::ALT))],
    );
    read(&mut link, &mut a, vec![plain('r')]);
    step(&mut link, &mut a);
    step(&mut link, &mut a);

    let ask = sent(&frames)
        .into_iter()
        .find_map(|f| match f {
            ClientFrame::Prompt { text, context, .. } => Some((text, context)),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no Prompt reached the daemon"));
    let (text, context) = ask;
    let context = context.expect("the ask rides the prompt's context");
    let ask = context.ask.expect("and says it is an ask");
    assert_eq!(
        ask.answer,
        letibot_sessionlog::protocol::AnswerMode::Suggest,
        "the answer mode the daemon teaches the suggest fence by"
    );
    assert_eq!(ask.preset, "rewrite");
    assert!(
        text.starts_with("Rewrite the selection"),
        "the preset's own words: {text:?}"
    );
    let r = context.refs.first().expect("the place rides the ask");
    assert!(r.file.ends_with("article.txt"), "{r:?}");
    let range = r.range.expect("a range, not a bare place");
    assert_eq!(range.start.line, 1);
    assert!(
        r.text.contains("1866"),
        "the paragraph's own text: {:?}",
        r.text
    );
}
