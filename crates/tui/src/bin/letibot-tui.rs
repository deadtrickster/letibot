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

use letibot_sessionlog::client::{ClientError, HeadClient, pump};
use letibot_sessionlog::event::Envelope;
use letibot_sessionlog::protocol::{Caps, ServerFrame};
use letibot_sessionlog::server::default_socket_path;
use letibot_sessionlog::{SessionBrief, testing};

use letibot_tui::app::App;
use letibot_tui::driver::tick;
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

    if let Err(e) = live(&args, cfg) {
        eprintln!("letibot-tui: {e}");
        std::process::exit(1);
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
    rx: &std::sync::mpsc::Receiver<ServerFrame>,
    deadline: Instant,
) -> Result<Vec<SessionBrief>, Box<dyn std::error::Error>> {
    client.list_sessions()?;
    loop {
        if Instant::now() >= deadline {
            return Err("timed out waiting for the session list".into());
        }
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(ServerFrame::Sessions { sessions, .. }) => return Ok(sessions),
            Ok(_) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err("the daemon closed the connection".into());
            }
        }
    }
}

fn live(args: &Args, cfg: RenderConfig) -> Result<(), Box<dyn std::error::Error>> {
    let (mut client, hello, reader) = HeadClient::attach(
        &args.socket,
        &args.session,
        args.since,
        "tui",
        &args.identity,
        Caps::default(),
    )?;
    let (tx, rx) = std::sync::mpsc::channel();
    let pump_thread = std::thread::spawn(move || pump(reader, tx));

    let mut app = App::new(cfg);
    app.load_prefs();
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

    let term = if args.no_tty {
        None
    } else {
        Terminal::enter().ok()
    };
    match term {
        None => {
            // One frame to stdout. Useful in a pipeline and in CI, and it is what
            // makes "does it render" answerable without a pty.
            let mut sink = |lines: &[String], _cursor| {
                for l in lines {
                    println!("{l}");
                }
            };
            tick(&mut app, &rx, &mut client, (100, 40), &[], &mut sink)?;
        }
        Some(term) => {
            let mut draw = |lines: &[String], cursor| term.draw_with_cursor(lines, cursor);
            while !app.should_quit() {
                let keys = term.keys();
                let size = term.size();
                if app.take_redraw() {
                    term.invalidate();
                }
                tick(&mut app, &rx, &mut client, size, &keys, &mut draw)?;
            }
        }
    }
    let _ = client.detach();
    drop(client);
    let _ = pump_thread.join();
    Ok(())
}
