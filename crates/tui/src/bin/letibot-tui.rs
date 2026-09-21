//! The TUI head (§13.4, row one).
//!
//! ```text
//!   letibot-tui                        attach to $XDG_RUNTIME_DIR/harnessd.sock
//!   letibot-tui --socket PATH          attach to a specific daemon
//!   letibot-tui --since SEQ            resume rather than take a snapshot
//!   letibot-tui --replay FILE.jsonl    render a recorded event log, no daemon
//!   letibot-tui --demo                 render the built-in recorded session
//!   letibot-tui --no-tty               render one frame to stdout and exit
//! ```
//!
//! `--replay` and `--demo` are the reason this head is a leaf strand: they need no
//! daemon, no model and no socket, so the whole of §13.3 is buildable and
//! demonstrable against a recorded log.

use std::io::BufRead;
use std::time::{Duration, Instant};

use letibot_sessionlog::client::{ClientError, HeadClient, Inbound, pump};
use letibot_sessionlog::event::Envelope;
use letibot_sessionlog::protocol::{Caps, ServerFrame};
use letibot_sessionlog::server::default_socket_path;
use letibot_sessionlog::{SessionBrief, testing};

use letibot_tui::app::App;
use letibot_tui::driver::Link;
use letibot_tui::render::{Budget, RenderConfig};
use letibot_tui::term::Terminal;

struct Args {
    socket: std::path::PathBuf,
    session: String,
    /// A session to resume out of the store and switch to, once attached.
    resume: String,
    /// A session to create and switch to, once attached. `Some("")` is an untitled
    /// one — distinct from `None`, which is "do not make one".
    new_session: Option<String>,
    since: u64,
    replay: Option<String>,
    demo: bool,
    no_tty: bool,
    budget: Budget,
    identity: String,
    /// Headless shutdown helper: interrupt every session whose turn is running,
    /// wait for the turns to end, exit. `letibot --stop --force` runs this
    /// before it pkills.
    interrupt_all: bool,
    list_sessions: bool,
    /// How long `--interrupt-all` waits for the turns to end.
    wait: u64,
    /// Answer "is there a daemon here, and does it speak this build's protocol" and
    /// exit. No screen, no snapshot, no frame — see [`probe`].
    probe: bool,
}

fn parse() -> Result<Args, String> {
    let mut a = Args {
        socket: default_socket_path(),
        session: String::new(),
        resume: String::new(),
        new_session: None,
        since: 0,
        replay: None,
        demo: false,
        no_tty: false,
        budget: Budget::default(),
        identity: std::env::var("USER").unwrap_or_else(|_| "operator".into()),
        interrupt_all: false,
        list_sessions: false,
        wait: 30,
        probe: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut next = || it.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--socket" => a.socket = next()?.into(),
            "--session" => a.session = next()?,
            "--resume" => a.resume = next()?,
            "--new-session" => a.new_session = Some(next()?),
            "--since" => a.since = next()?.parse().map_err(|e| format!("--since: {e}"))?,
            "--replay" => a.replay = Some(next()?),
            "--identity" => a.identity = next()?,
            // §13.3: the buffer size is configurable rather than hardcoded.
            "--body-lines" => {
                a.budget.body_lines = next()?.parse().map_err(|e| format!("--body-lines: {e}"))?
            }
            "--reasoning-lines" => {
                a.budget.reasoning_lines = next()?
                    .parse()
                    .map_err(|e| format!("--reasoning-lines: {e}"))?
            }
            "--demo" => a.demo = true,
            "--no-tty" => a.no_tty = true,
            "--probe" => a.probe = true,
            "--interrupt-all" => a.interrupt_all = true,
            // Print this daemon's live sessions, one per line, and exit. For
            // `letibot --ls`, which draws the byobu-shaped view across folders.
            "--list-sessions" => a.list_sessions = true,
            "--wait" => a.wait = next()?.parse().map_err(|e| format!("--wait: {e}"))?,
            "-h" | "--help" => return Err(usage()),
            other => return Err(format!("unknown argument {other}\n\n{}", usage())),
        }
    }
    Ok(a)
}

fn usage() -> String {
    "letibot-tui [--socket PATH] [--session ID] [--resume ID] [--new-session TITLE]\n\
     \x20           [--since SEQ] [--identity NAME]\n\
     \x20           [--replay FILE.jsonl] [--demo] [--no-tty]\n\
     \x20           [--body-lines N] [--reasoning-lines N]\n\
     \x20           [--interrupt-all [--wait SECS]] [--list-sessions]"
        .into()
}

fn main() {
    let args = match parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let cfg = RenderConfig {
        budget: args.budget,
        ..RenderConfig::default()
    };

    if args.demo || args.replay.is_some() {
        replay(&args, cfg);
        return;
    }

    if args.list_sessions {
        if let Err(e) = list_sessions(&args) {
            eprintln!("letibot-tui: {e}");
            std::process::exit(1);
        }
        return;
    }
    if args.interrupt_all {
        if let Err(e) = interrupt_all(&args) {
            eprintln!("letibot-tui: {e}");
            std::process::exit(1);
        }
        return;
    }

    if args.probe {
        std::process::exit(probe(&args));
    }

    if let Err(e) = live(&args, cfg) {
        eprintln!("letibot-tui: {e}");
        std::process::exit(1);
    }
}

/// How long the head waits for the daemon's `Hello` before giving up.
///
/// The `Hello` carries the whole snapshot, so this has to allow for a big session on a
/// busy daemon — but it is bounded because the alternative is what the operator hit: a
/// daemon that accepts the connection and never answers, and a head that waits for ever
/// with no screen telling them so. Long enough for a slow snapshot, short enough that a
/// hung daemon is reported rather than endured.
const ATTACH_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// **Is there a daemon here, and does it speak this build's protocol?**
///
/// `0` when there is and it does, `1` when there is and it does not (with the
/// daemon's own refusal on stderr), `2` when there is not.
///
/// # Why this is not `--no-tty`
///
/// `~/bin/letibot` asks this on every start, to tell "no daemon, start one" from
/// "a daemon is here but a different build is, so starting one would unlink its
/// socket" — the second of which is a live daemon holding every session and must
/// not be routed around. It did that with `--no-tty --since 0`, which is a whole
/// head: it builds the config, reads the prefs, applies a transcript of thousands of
/// rows and renders a frame, none of which the question is about. **Measured
/// 2026-09-20 on a session of 1397 rows: 0.14 s that way, 0.02 s as a probe**, on the
/// path that runs before anything is on the screen.
///
/// So: attach, drop the connection, report. What is left is the handshake, which is
/// the only part that answers the question.
///
/// # What this does *not* save, measured rather than assumed
///
/// The daemon still builds and sends a snapshot, and `since_seq` does not change
/// that: a `since` past the end is a **resync**, and `Hub::attach` answers a resync
/// with `view.snapshot` — the same `Hello` a fresh attach gets, `resumed_from` and
/// all. Measured by asserting it, not by reading the code: the first version of this
/// probe asked for `u64::MAX` *and* a test claimed the snapshot was skipped. Both
/// were wrong, so the constant is gone and the `since` is the plain 0 the launcher
/// always used.
///
/// **The whole 0.12 s is the client side** — building the config, reading the prefs,
/// applying a transcript of thousands of rows and drawing a frame that nothing
/// reads. Skipping the snapshot on the wire as well would be a protocol change (a
/// cap meaning "I only want the version"), and it is not worth one: what is left
/// after this is the handshake, which is the part that answers the question.
fn probe(args: &Args) -> i32 {
    match HeadClient::attach(
        &args.socket,
        &args.session,
        0,
        "probe",
        &args.identity,
        Caps::default(),
    ) {
        Ok((mut client, _hello, _reader)) => {
            let _ = client.detach();
            0
        }
        Err(ClientError::Refused(reason)) => {
            // The same line `live` prints, because the launcher greps for it.
            eprintln!("letibot-tui: the daemon refused the attach: {reason}");
            1
        }
        // **A daemon that is here and cannot be talked to is exit 1, not 2.** The
        // launcher reads 2 as *no daemon* and starts one, which unlinks the socket of
        // the daemon that is very much there — which is the hazard this whole probe
        // exists for, and a version skew is exactly the shape of it. A `Hello` this
        // build cannot parse, or an answer that is not a `Hello` at all, both mean
        // "somebody is on this socket and it is not this build".
        Err(e @ ClientError::Protocol(_)) => {
            eprintln!("letibot-tui: {e}");
            1
        }
        Err(e) => {
            eprintln!("letibot-tui: {e}");
            2
        }
    }
}

/// Render a recorded log. No daemon, no model, no socket.
fn replay(args: &Args, cfg: RenderConfig) {
    let mut app = App::new(cfg);
    app.load_prefs();
    let envelopes: Vec<Envelope> = if args.demo {
        testing::recorded_session()
            .into_iter()
            .enumerate()
            .map(|(i, event)| Envelope {
                session_id: "demo".into(),
                seq: i as u64 + 1,
                ts: 0,
                event,
            })
            .collect()
    } else {
        let path = args.replay.as_ref().unwrap();
        let f = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("letibot-tui: {path}: {e}");
                std::process::exit(1);
            }
        };
        std::io::BufReader::new(f)
            .lines()
            .map_while(Result::ok)
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str::<Envelope>(&l).ok())
            .collect()
    };

    let term = if args.no_tty {
        Err(())
    } else {
        Terminal::enter().map_err(|_| ())
    };
    match term {
        Err(()) => {
            for env in envelopes {
                app.apply(ServerFrame::Event(env));
            }
            if args.demo {
                for (id, item) in testing::recorded_items() {
                    app.record_item(&id, item);
                }
            }
            for line in app.screen(100, 40) {
                println!("{line}");
            }
        }
        Ok(term) => {
            // Paced, so the demo shows the streaming behaviour rather than the
            // finished document.
            for env in envelopes {
                app.apply(ServerFrame::Event(env));
                let (w, h) = term.size();
                if app.take_redraw() {
                    term.invalidate();
                }
                let frame = app.screen(w, h);
                term.draw_with_cursor(&frame, app.cursor());
                for k in term.keys() {
                    if app.key(k).is_some() {
                        break;
                    }
                }
                if app.should_quit() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(8));
            }
            if args.demo {
                for (id, item) in testing::recorded_items() {
                    app.record_item(&id, item);
                }
            }
            loop {
                // The composer's double-tap windows are measured against a clock
                // the head is told about, so a replay that never tells it one has
                // an Esc from ten minutes ago still armed.
                app.clock(now_ms());
                let (w, h) = term.size();
                if app.take_redraw() {
                    term.invalidate();
                }
                let frame = app.screen(w, h);
                term.draw_with_cursor(&frame, app.cursor());
                for k in term.keys() {
                    app.key(k);
                }
                if app.should_quit() {
                    return;
                }
            }
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Interrupt every session whose turn is running, through the same frame an
/// Esc-Esc sends, and wait for the turns to end.
///
/// This is `letibot --stop --force`'s first half. The daemon's own signal
/// handler deliberately has no force path — its comment calls a kill "a way to
/// lose the last turn's rows" — and this is the shape that loses none: each
/// running turn is asked to stop over the protocol, the abort is recorded like
/// any other, and the daemon is left to a plain SIGTERM, which it can now take
/// promptly because nothing is mid-turn.
/// This daemon's sessions, one TSV line each, for `letibot --ls`.
///
/// Reuses `discover` — which learns the ids from the daemon's own refusal of a
/// bogus one — so it needs no session id from the caller, and `ask_sessions`,
/// which is the same round trip `--interrupt-all` already makes.
///
/// TSV rather than a drawn table: the caller composes this with every other
/// daemon's answer and does the aligning, and a format a shell can cut is worth
/// more here than one a person can read.
fn list_sessions(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let held = discover(&args.socket)?;
    let Some(first) = held.first() else {
        return Ok(());
    };
    let (mut client, _hello, reader) = HeadClient::attach(
        &args.socket,
        first,
        u64::MAX,
        "remote",
        "ls",
        Caps::default(),
    )?;
    let (tx, rx) = std::sync::mpsc::channel();
    let pump_thread = std::thread::spawn(move || pump(reader, tx));
    let out = ask_sessions(&mut client, &rx, Instant::now() + Duration::from_secs(5));
    let _ = client.detach();
    let _ = pump_thread.join();
    for s in out? {
        // id, title, live, running, heads, rows, model
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}",
            s.session_id,
            one_line(&s.title),
            s.live,
            s.status.running,
            s.status.heads,
            s.status.items,
            if s.status.model.is_empty() { "-" } else { &s.status.model },
        );
    }
    Ok(())
}

/// A session title as ONE TSV field.
///
/// A title is the first words of a prompt, so it is arbitrary text: it can hold
/// tabs and newlines, and a reader splitting on tabs then sees a title as three
/// columns and the next session's row as a continuation of this one. Measured
/// the first time `letibot --ls` was run against this box — seventy sessions
/// rendered as shredded half-lines.
///
/// The EMITTER guarantees the format. A consumer cannot un-split a field, and
/// asking every caller to quote correctly is how one of them does not.
fn one_line(title: &str) -> String {
    if title.trim().is_empty() {
        return "-".into();
    }
    let flat: String = title
        .chars()
        .map(|c| if c.is_control() || c == '\t' { ' ' } else { c })
        .collect();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > 60 {
        flat.chars().take(59).collect::<String>() + "…"
    } else {
        flat
    }
}

fn interrupt_all(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = Instant::now() + Duration::from_secs(args.wait.max(1));
    let held = discover(&args.socket)?;
    if held.is_empty() {
        println!("the daemon holds no sessions");
        return Ok(());
    }
    // One seat, moved across the sessions that need the interrupt: `Switch`
    // exists so a connection can change sessions, and an interrupt is scoped to
    // the seat's session, so the moving is how one client covers many.
    let (mut client, _hello, reader) = HeadClient::attach(
        &args.socket,
        &held[0],
        0,
        "remote",
        "stop-force",
        Caps::default(),
    )?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || pump(reader, tx));

    let sessions = ask_sessions(&mut client, &rx, deadline)?;
    let running: Vec<SessionBrief> = sessions
        .into_iter()
        .filter(|s| s.status.running)
        .collect();
    if running.is_empty() {
        println!("no turns in flight");
        return Ok(());
    }
    for s in &running {
        client.switch(&s.session_id, 0)?;
        // `expected_seq = 0` is "no expectation" at the hub — never stale —
        // which is what a client that is not following the stream gets to say.
        client.interrupt(0, "letibot --stop --force")?;
        println!("  {} · interrupted", s.session_id);
    }
    loop {
        if Instant::now() >= deadline {
            let left = ask_sessions(&mut client, &rx, deadline)?
                .into_iter()
                .filter(|s| s.status.running)
                .count();
            return Err(format!("{left} turn(s) still running after {}s", args.wait).into());
        }
        std::thread::sleep(Duration::from_millis(400));
        if !ask_sessions(&mut client, &rx, deadline)?
            .into_iter()
            .any(|s| s.status.running)
        {
            println!("all turns stopped");
            return Ok(());
        }
    }
}

/// Which sessions does the daemon hold?
///
/// No frame lists sessions without a seat — `ListSessions` reads the seat's
/// registry — but the ATTACH refusal for an unknown id is a **Bye that lists
/// what is held**, so the refusal is the directory. A probe attach costs one
/// round trip and names no session anybody wanted.
fn discover(path: &std::path::Path) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    match HeadClient::attach(path, "\0probe", 0, "remote", "stop-force", Caps::default()) {
        // A daemon that accepted the probe id would be one that mints sessions
        // on attach, which none does; treat it as "held nothing useful".
        Ok(_) => Ok(Vec::new()),
        Err(ClientError::Refused(reason)) => match reason.split_once("it holds ") {
            Some((_, list)) => Ok(list.split(", ").map(str::to_string).collect()),
            // "this daemon holds none yet" — nothing held, nothing to interrupt.
            None => Ok(Vec::new()),
        },
        Err(e) => Err(e.into()),
    }
}

/// One `ListSessions` round trip: send it, then read frames until the reply
/// arrives, discarding everything else — the seat's event stream keeps
/// arriving on the same channel, and an interrupt in progress makes it loud.
fn ask_sessions(
    client: &mut HeadClient,
    rx: &std::sync::mpsc::Receiver<Inbound>,
    deadline: Instant,
) -> Result<Vec<SessionBrief>, Box<dyn std::error::Error>> {
    client.list_sessions()?;
    loop {
        if Instant::now() >= deadline {
            return Err("timed out waiting for the session list".into());
        }
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(Inbound::Frame(ServerFrame::Sessions { sessions, .. })) => return Ok(sessions),
            // A frame this head cannot read is not a reason to lose the round trip:
            // the answer may be the very next one. Said and counted like any other.
            Ok(Inbound::Unreadable(u)) => {
                eprintln!("letibot-tui: {}", u.said());
                continue;
            }
            Ok(Inbound::Frame(_)) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err("the daemon closed the connection".into());
            }
        }
    }
}

fn live(args: &Args, cfg: RenderConfig) -> Result<(), Box<dyn std::error::Error>> {
    let mut app = App::new(cfg);
    app.load_prefs();

    // **Take the screen and draw chrome before the round trip.**
    //
    // `HeadClient::attach` blocks until the daemon's `Hello` arrives, and the `Hello`
    // carries the **whole snapshot** — so the head used to spend the entire attach
    // accumulating a transcript it could not yet draw, on a terminal it had not yet
    // taken over, leaving the operator's previous screen up for the duration. Measured
    // on this workspace's own daemon: 0.17 s of attach and 0.08 s of first frame, of
    // which the 0.01 s the process takes to start is the only part that was ever
    // visible as *letibot*.
    //
    // What a head knows before it asks is its own composer, its key bindings and its
    // layout, and drawing those is the difference between "it started" and "it did
    // nothing". The body is left blank rather than showing the empty-transcript
    // banner, which would be claiming the session has said nothing when the truth is
    // that nobody has told this head yet — `App::begin_attach` is that distinction.
    //
    // The terminal is entered before the attach, so a failed attach has to leave the
    // screen as it found it: `Terminal`'s `Drop` restores the termios and leaves the
    // alternate screen, and it does so *before* printing its own reports, so the error
    // from a refused attach lands on a terminal that is already restored.
    let term = if args.no_tty {
        None
    } else {
        Terminal::enter().ok()
    };
    // **The handshake goes to a thread, and the cat walks while it waits.**
    //
    // A `Hello` carries the whole snapshot, so on a big session this is a real wait.
    // The reader thread starts inside `Link::open` and the first frame it hands over is
    // the `Hello`, so the wait below is an ordinary read on an ordinary channel — which
    // is also what makes a *re*-attach look like the first one: same frame, same path.
    //
    // The Attach is sent here rather than on the thread, so a refused attach — the
    // protocol-skew case the launcher refuses to route around — is known before
    // anything is drawn, and a head that cannot attach never claims the screen. A
    // *socket* error is still only discoverable on the connect, so this reports one.
    let mut link = Link::open(
        &args.socket,
        &args.session,
        args.since,
        "tui",
        &args.identity,
    )?;

    if let Some(t) = &term {
        app.begin_attach_at(now_ms());
        let (w, h) = t.size();
        let frame = app.screen(w, h);
        t.draw_with_cursor(&frame, app.cursor());
    }

    // Wait for the `Hello`. **With a screen, draw the cat as the clock moves**; without
    // one — the `--no-tty` path — block properly rather than sleeping in 120 ms steps
    // for a frame nobody will see.
    //
    // # The wait has to be escapable, and it was not
    //
    // This loop draws the cat while `HeadClient` waits for a `Hello`. The first version
    // read nothing from the terminal, so **keys went nowhere**: the main loop — the only
    // place `term.keys()` was called — had not started, and the terminal is in raw mode,
    // so Ctrl-C is a key event rather than a signal. The operator, on a daemon that
    // accepted the connection and then died without answering:
    //
    //     asking the daemon for this session
    //                                    3m09s
    //
    // *"stuck waiting for daemon and no way to exit"*. Three things were wrong and all
    // three are fixed here: keys are read and handled, a wait that outlives its budget
    // gives up, and the frame says so once the wait has gone on long enough to be worth
    // a sentence.
    let hello: Option<Inbound> = match &term {
        None => link.frames().recv().ok(),
        Some(t) => {
            let started = Instant::now();
            let deadline = started + ATTACH_WAIT;
            let mut got = None;
            loop {
                match link.frames().recv_timeout(Duration::from_millis(120)) {
                    Ok(f) => {
                        got = Some(f);
                        break;
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        // **Keys first.** Two Ctrl-Cs on an empty composer quit, which is
                        // what the hint bar under this frame has promised the operator
                        // since before there was a frame: a hint that names a key the
                        // wait does not read is a hint that lies.
                        for k in t.keys() {
                            let _ = app.key(k);
                        }
                        if app.should_quit() {
                            return Ok(());
                        }
                        app.clock(now_ms());
                        let (w, h) = t.size();
                        let frame = app.screen(w, h);
                        t.draw_with_cursor(&frame, app.cursor());
                        if Instant::now() >= deadline {
                            return Err(format!(
                                "the daemon did not answer within {}s. It accepted the \
                                 connection and sent no `Hello`, which is a hung daemon \
                                 rather than an absent one — `letibot --status` says what \
                                 is on the socket, and `letibot --stop` stops it.",
                                ATTACH_WAIT.as_secs()
                            )
                            .into());
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            got
        }
    };
    // **A first frame this build cannot read is a skew, and it is named.**
    let hello = match hello {
        Some(Inbound::Frame(f)) => f,
        Some(Inbound::Unreadable(u)) => return Err(u.said().into()),
        None => return Err("the daemon closed the connection before answering".into()),
    };
    // A refusal arrives as a `Bye`, exactly as `HeadClient::attach` treats it.
    if let ServerFrame::Bye { reason } = &hello {
        return Err(ClientError::Refused(reason.clone()).into());
    }
    link.seated_by(&hello);

    app.apply(hello);
    // After the `Hello`, so the head knows what the daemon holds before it asks for
    // something else — a resume of a session that is already live is then a switch
    // rather than a round trip through the store.
    if !args.resume.is_empty() {
        app.request_session(&args.resume);
    }
    if let Some(title) = &args.new_session {
        app.request_new_session(title);
    }

    match term {
        None => {
            // One frame to stdout. Useful in a pipeline and in CI, and it is what
            // makes "does it render" answerable without a pty.
            let mut sink = |lines: &[String], _cursor| {
                for l in lines {
                    println!("{l}");
                }
            };
            link.tick(&mut app, (100, 40), &[], &mut sink);
        }
        Some(term) => {
            let mut draw = |lines: &[String], cursor| term.draw_with_cursor(lines, cursor);
            while !app.should_quit() {
                let keys = term.keys();
                let size = term.size();
                if app.take_redraw() {
                    term.invalidate();
                }
                link.tick(&mut app, size, &keys, &mut draw);
                // **Getting back is this loop's job, because the socket is this
                // layer's.** The head knows *that* the link is down and how long it
                // has been; only here is there a socket path to open and a `Link` to
                // replace. `app.seq` is the read mark, which is what makes the new
                // `ATTACH` a resume: the daemon answers the gap with events, or with a
                // `Resync` when the gap is larger than it still holds.
                //
                // The attempt is not waited on: the `ATTACH` goes out here and the
                // `Hello` arrives on the pump like any other frame, so the head keeps
                // drawing — and keeps taking keys — through the whole recovery.
                if app.should_reconnect() {
                    match link.reconnect(&args.socket, app.seq) {
                        // **The attempt is out, so the head waits for its answer.** The
                        // `ATTACH` is on a live socket and the `Hello` is a moment away;
                        // asking again in the meantime would open a second socket and
                        // close the first, which is the one about to be answered.
                        Ok(()) => app.reconnect_sent(),
                        // Said and counted, then tried again after the backoff. A
                        // `Err` here is the socket nobody is listening on yet.
                        Err(e) => app.reconnect_failed(&e.to_string()),
                    }
                }
            }
        }
    }
    link.detach();
    // **After the terminal is back**, because this is the one message that must
    // outlive the screen: the alternate screen has been torn down by now, so a
    // reason said into the transcript is gone and the head looks like it crashed.
    if let Some(reason) = app.farewell() {
        eprintln!("letibot: the daemon ended this head — {reason}");
    }
    Ok(())
}

#[cfg(test)]
mod probe_tests {
    use super::*;
    use letibot_sessionlog::server::{self, ServerHandle};
    use letibot_sessionlog::Hub;

    fn socket_path(tag: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("letibot-probe-{tag}-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn start(tag: &str) -> (std::sync::Arc<Hub>, ServerHandle) {
        let hub = Hub::new("s");
        let h = server::serve(hub.clone(), socket_path(tag)).expect("bind");
        (hub, h)
    }

    fn args_for(socket: std::path::PathBuf) -> Args {
        Args {
            socket,
            session: "s".into(),
            resume: String::new(),
            new_session: None,
            since: 0,
            replay: None,
            demo: false,
            no_tty: false,
            budget: Budget::default(),
            identity: "probe-test".into(),
            interrupt_all: false,
            list_sessions: false,
            wait: 30,
            probe: true,
        }
    }

    /// A live daemon is a `0`, which is what the launcher reads as "there is one
    /// here, do not start another over it".
    #[test]
    fn a_live_daemon_probes_yes() {
        let (_hub, h) = start("live");
        assert_eq!(probe(&args_for(h.path().to_path_buf())), 0);
        h.shutdown();
    }

    /// Nothing there is a `2`, and it says so on stderr rather than silently.
    #[test]
    fn nothing_there_probes_no() {
        let mut p = std::env::temp_dir();
        p.push(format!("letibot-probe-absent-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&p);
        assert_eq!(probe(&args_for(p)), 2);
    }

    /// **A peer that answers with a frame this build cannot read is a daemon that is
    /// here and is not this build — which is a `1`, and it must never be a `2`.**
    ///
    /// The launcher reads `2` as *no daemon* and starts one, which unlinks the socket
    /// the live daemon is listening on. A version skew is exactly the shape of
    /// anyway-there, so the exit code is the load-bearing part here; the sentence on
    /// stderr is `Unreadable::said`, which names both builds.
    #[test]
    fn a_hello_this_build_cannot_read_is_a_one_not_a_two() {
        let path = socket_path("skew");
        let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
        let peer = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().expect("accept");
            // Read the Attach line, then answer in a dialect from the future.
            let mut r = std::io::BufReader::new(s.try_clone().unwrap());
            let mut line = String::new();
            std::io::BufRead::read_line(&mut r, &mut line).expect("the Attach");
            assert!(line.contains("attach"), "{line}");
            use std::io::Write;
            s.write_all(b"{\"frame\":\"hello_v2\",\"session_id\":\"s\"}\n")
                .expect("write");
        });
        let code = probe(&args_for(path.clone()));
        peer.join().unwrap();
        assert_eq!(code, 1, "the launcher must not start a second daemon here");
        let _ = std::fs::remove_file(&path);
    }
}
