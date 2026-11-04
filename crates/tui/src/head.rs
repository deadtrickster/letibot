//! `letibot-tui` as a function, so more than one binary can be it: the head is the role a person meets and the one
//! a substitute speaks for. The launcher hands it a socket, an identity and at most a session to resume or a title,
//! and everything else it learns from the daemon.

//! The TUI head.
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
//! `--replay` and `--demo` need no daemon, model or socket, so the whole head is buildable against a recorded log.

use std::io::BufRead;
use std::time::{Duration, Instant};

use letibot_metrics::now_ms;
use letibot_sessionlog::client::{ClientError, HeadClient, Inbound, pump};
use letibot_sessionlog::event::Envelope;
use letibot_sessionlog::protocol::{Caps, ServerFrame};
use letibot_sessionlog::server::default_socket_path;
use letibot_sessionlog::{SessionBrief, SessionEvent, testing};

use crate::app::App;
use crate::driver::Link;
use crate::ui::render::{Budget, RenderConfig};
use rano::term::Terminal;

struct Args {
    socket: std::path::PathBuf,
    session: String,
    /// A session to resume out of the store and switch to, once attached.
    resume: String,
    /// A session to create and switch to, once attached. `Some("")` is an untitled
    /// one - distinct from `None`, which is "do not make one".
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
    /// Print the version and exit: an installer has to be able to prove the binary it just copied runs, and the
    /// cheapest proof is a question answered without a socket, a model or a store.
    version: bool,
    theme_template: bool,
    /// How long `--interrupt-all` waits for the turns to end.
    wait: u64,
    /// What the interrupt is recorded as (`--reason`): the command that sent it, so a plain
    /// `letibot --stop` is not written into the transcript as a forced one.
    reason: String,
    /// Answer "is there a daemon here, and does it speak this build's protocol" and
    /// exit. No screen, no snapshot, no frame - see [`probe`].
    probe: bool,
    /// `--models NAME`: the daemon's own `/models NAME`, its answer, and exit. A one-shot
    /// like `--interrupt-all`, because a script that moves a session's model has no screen.
    models: Option<String>,
}

/// `args` is a parameter, and that is the point of the move: the first version left `parse()` reading the process's
/// own arguments, so `run(args)` received them and then ignored them.
fn parse(args: &[String]) -> Result<Args, String> {
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
        version: false,
        theme_template: false,
        wait: 30,
        reason: "letibot-tui --interrupt-all".into(),
        probe: false,
        models: None,
    };
    let mut it = args.iter().cloned();
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
            // The buffer size is configurable rather than hardcoded.
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
            // The name is the whole of the flag: a daemon sent a bare `/models` answers a
            // LISTING, which is not a switch, so an empty one never reaches the socket.
            "--models" => {
                let name = next()?;
                if name.trim().is_empty() {
                    return Err("--models: the name is empty".into());
                }
                a.models = Some(name)
            }
            "--reason" => a.reason = next()?,
            "-V" | "--version" => a.version = true,
            // Every role with its default look, as a theme file reads them: the starting point
            // for `~/.config/letibot/themes/NAME.toml`. See `rano::theme`.
            "--theme-template" => a.theme_template = true,
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
     \x20           [--interrupt-all [--wait SECS] [--reason TEXT]] [--list-sessions]\n\
     \x20           [--models NAME] [--theme-template]"
        .into()
}

/// Run the head over `args`, returning the process exit code. `args` is what followed the program name, so the
/// launcher and `letibot tui` arrive here identically.
pub fn run(args: &[String]) -> i32 {
    run_with(args, None)
}

/// [`run`], with a reader of the other daemons' packages for the sessions list.
pub fn run_with(args: &[String], fleet: Option<letibot_sessionlog::FleetReader>) -> i32 {
    let args = match parse(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    let cfg = RenderConfig {
        budget: args.budget,
        ..RenderConfig::default()
    };

    if args.demo || args.replay.is_some() {
        replay(&args, cfg);
        return 0;
    }

    if args.version {
        println!("letibot-tui {}", env!("CARGO_PKG_VERSION"));
        return 0;
    }

    if args.theme_template {
        print!("{}", rano::theme::Theme::template());
        return 0;
    }

    if args.list_sessions {
        if let Err(e) = list_sessions(&args) {
            eprintln!("letibot-tui: {e}");
            return 1;
        }
        return 0;
    }
    if args.interrupt_all {
        if let Err(e) = interrupt_all(&args) {
            eprintln!("letibot-tui: {e}");
            return 1;
        }
        return 0;
    }

    if let Some(name) = args.models.as_deref() {
        return models(&args, name, &mut std::io::stdout(), &mut std::io::stderr());
    }

    if args.probe {
        return probe(&args);
    }

    // The id, on stdout, as the last thing this head says: the launcher execs the head, so this stream is the
    // launcher's, and it is the one place an exit can leave something for the next command to read.
    match live(&args, cfg, fleet) {
        Ok(served) => {
            if !args.no_tty
                && let Err(e) = print_exit_session(&mut std::io::stdout(), &served)
            {
                eprintln!("letibot-tui: {e}");
                return 1;
            }
            0
        }
        Err(e) => {
            eprintln!("letibot-tui: {e}");
            1
        }
    }
}

/// The id this head was serving, as the one line an exit leaves on stdout. The head prints it because the launcher
/// execs it, so a head's stdout is the terminal the person is sitting at, where the daemon's is not.
fn print_exit_session<W: std::io::Write>(out: &mut W, served: &str) -> std::io::Result<()> {
    if served.is_empty() {
        return Ok(());
    }
    writeln!(out, "{served}")
}

/// How long the head waits for the daemon's `Hello` before giving up: the `Hello` carries the whole snapshot, so
/// this allows for a big session on a busy daemon, and it is bounded because a daemon that accepted a connection
/// and never answered is what the person gets stuck on.
const ATTACH_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// Is there a daemon here, and can this build attach to it? `0` when there is and it answers, `1` when there is and
/// it refuses, `2` when there is not, and a version skew is not a refusal from a current daemon.
fn probe(args: &Args) -> i32 {
    match HeadClient::attach(
        &args.socket,
        &args.session,
        0,
        "probe",
        &args.identity,
        Caps::drawing_rows(),
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
        // A daemon that is here and cannot be talked to is exit 1 and not 2: the launcher reads 2 as *no daemon* and
        // starts one, which unlinks the socket of the daemon that is there, which is the hazard the probe exists for.
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
        Terminal::enter_with(any_motion()).map_err(|_| ())
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
            app.set_features(term.features());
            // Paced, so the demo shows the streaming behaviour rather than the
            // finished document.
            for env in envelopes {
                app.apply(ServerFrame::Event(env));
                let (w, h) = term.size();
                if app.take_redraw() {
                    term.invalidate();
                }
                let frame = app.screen(w, h);
                outside_the_frame(&term, &mut app);
                term.draw_with_cursor(&frame, app.cursor());
                for k in read_routed(&term, &mut app).0 {
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
                outside_the_frame(&term, &mut app);
                term.draw_with_cursor(&frame, app.cursor());
                for k in read_routed(&term, &mut app).0 {
                    app.key(k);
                }
                if app.should_quit() {
                    return;
                }
            }
        }
    }
}

/// This daemon's sessions, one TSV line each, for `--ls`. It reuses `discover`, which learns the ids from the
/// daemon's refusal of a bogus one, so no session id is needed from the caller.
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
        Caps::drawing_rows(),
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
            if s.status.model.is_empty() {
                "-"
            } else {
                &s.status.model
            },
        );
    }
    Ok(())
}

/// A session title as one TSV field: a title is the first words of a prompt, so it can hold tabs and newlines, and
/// a reader splitting on tabs would see one title as three columns.
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

/// `--interrupt-all`: [`interrupt_turns`], said on stdout the way `scripts/letibot` has
/// always read it.
fn interrupt_all(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    match interrupt_turns(
        &args.socket,
        Duration::from_secs(args.wait.max(1)),
        &args.reason,
        |id| println!("  {id} · interrupted"),
    )? {
        Interrupted::NoSessions => println!("the daemon holds no sessions"),
        Interrupted::NoneRunning => println!("no turns in flight"),
        Interrupted::Ended(_) => println!("all turns stopped"),
    }
    Ok(())
}

/// How long a `/models NAME` sent from the command line may take: the daemon rewrites
/// `providers.toml` and may have to ask a provider about its key, and neither is this head's
/// clock to measure.
const MODELS_WAIT: Duration = Duration::from_secs(30);

/// What the daemon said to a `/models NAME` this head sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Switched {
    /// The verb's answer, in the daemon's own words.
    pub said: String,
    /// Whether the switch was made: the event's code, never a reading of the prose.
    pub ok: bool,
}

/// `--models NAME`: a one-shot in `--interrupt-all`'s shape. The daemon's own sentence is
/// stdout's and a refusal is stderr's, so a script reads an answer or nothing.
fn models<W: std::io::Write, E: std::io::Write>(
    args: &Args,
    name: &str,
    out: &mut W,
    err: &mut E,
) -> i32 {
    // `--resume` first: it names a session out of the store, and it is the one the caller
    // asked about when both flags are there.
    let session = if args.resume.is_empty() {
        args.session.as_str()
    } else {
        args.resume.as_str()
    };
    match switch_model(&args.socket, session, name, MODELS_WAIT) {
        Ok(Switched { said, ok: true }) => {
            let _ = writeln!(out, "{said}");
            0
        }
        Ok(Switched { said, ok: false }) => {
            let _ = writeln!(err, "{said}");
            1
        }
        Err(e) => {
            let _ = writeln!(err, "letibot-tui: {e}");
            1
        }
    }
}

/// Send `models NAME` over the frame a typed `/models` uses, and wait for the daemon's answer
/// under one of the two codes its slash door publishes.
pub fn switch_model(
    socket: &std::path::Path,
    session: &str,
    name: &str,
    wait: Duration,
) -> Result<Switched, Box<dyn std::error::Error>> {
    let line = format!("models {name}");
    let deadline = Instant::now() + wait;
    let (mut client, _hello, reader) =
        HeadClient::attach(socket, session, 0, "remote", "models", Caps::default())?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || pump(reader, tx));
    client.slash(0, &line)?;
    let out = loop {
        if Instant::now() >= deadline {
            return Err(format!("no answer to `/{line}` within {}s", wait.as_secs()).into());
        }
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(Inbound::Frame(ServerFrame::Event(env))) => {
                let SessionEvent::Warning { code, detail, .. } = env.event else {
                    continue;
                };
                if code != "slash" && code != "slash_refused" {
                    continue;
                }
                // Another head's verb lands on this stream too, and the echo is the only thing
                // that says the answer is the one this command asked for.
                let (echo, said) = detail.split_once('\n').unwrap_or((&detail, ""));
                if echo.trim().trim_start_matches('/') != line {
                    continue;
                }
                break Switched {
                    said: said.trim().to_string(),
                    ok: code == "slash",
                };
            }
            // A frame this head cannot read is not a reason to lose the round trip: the
            // answer may be the very next one.
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
    };
    let _ = client.detach();
    Ok(out)
}

/// How long one ask for the session list may take, on its own clock (see [`interrupt_turns`]).
const SESSION_LIST_WAIT: Duration = Duration::from_secs(5);

/// What [`interrupt_turns`] found and did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interrupted {
    /// The daemon holds no sessions at all.
    NoSessions,
    /// It holds some and none had a turn running.
    NoneRunning,
    /// This many turns were interrupted, and every one has ended.
    Ended(usize),
}

/// Interrupt every session whose turn is running, through the same frame an Esc-Esc sends, and wait for the turns
/// to end. This is `--stop --force`'s first half and the head's own force path's.
pub fn interrupt_turns(
    socket: &std::path::Path,
    wait: Duration,
    reason: &str,
    mut each: impl FnMut(&str),
) -> Result<Interrupted, Box<dyn std::error::Error>> {
    let deadline = Instant::now() + wait;
    // Each question gets its own time to be answered: the list was asked with the OVERALL deadline, so an ask after
    // it had passed timed out at once and the count of turns still running could never be printed.
    let ask_by = || Instant::now() + SESSION_LIST_WAIT;
    let held = discover(socket)?;
    if held.is_empty() {
        return Ok(Interrupted::NoSessions);
    }
    // One seat, moved across the sessions that need the interrupt: `Switch`
    // exists so a connection can change sessions, and an interrupt is scoped to
    // the seat's session, so the moving is how one client covers many.
    let (mut client, _hello, reader) =
        HeadClient::attach(socket, &held[0], 0, "remote", "stop-force", Caps::default())?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || pump(reader, tx));

    let sessions = ask_sessions(&mut client, &rx, ask_by())?;
    let running: Vec<SessionBrief> = sessions.into_iter().filter(|s| s.status.running).collect();
    if running.is_empty() {
        return Ok(Interrupted::NoneRunning);
    }
    for s in &running {
        client.switch(&s.session_id, 0)?;
        // `expected_seq = 0` is "no expectation" at the hub - never stale  -
        // which is what a client that is not following the stream gets to say.
        client.interrupt(0, reason)?;
        each(&s.session_id);
    }
    loop {
        if Instant::now() >= deadline {
            let left = ask_sessions(&mut client, &rx, ask_by())?
                .into_iter()
                .filter(|s| s.status.running)
                .count();
            return Err(format!("{left} turn(s) still running after {}s", wait.as_secs()).into());
        }
        std::thread::sleep(Duration::from_millis(400));
        if !ask_sessions(&mut client, &rx, ask_by())?
            .into_iter()
            .any(|s| s.status.running)
        {
            return Ok(Interrupted::Ended(running.len()));
        }
    }
}

/// How the force path sends a signal: `kill(pid, sig)`. A parameter rather than a
/// call so a test can stand in a process that survives everything - the one case the
/// head's last sentence is for - without needing a process the kernel cannot kill.
pub type Signaller = std::sync::Arc<dyn Fn(i32, i32) -> std::io::Result<()> + Send + Sync>;

/// The real one: `kill(2)`.
pub fn kill_signaller() -> Signaller {
    std::sync::Arc::new(|pid, sig| {
        if unsafe { libc::kill(pid, sig) } == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    })
}

/// The daemon the force path may signal: the one this head attached to and nothing else.
#[derive(Debug, Clone)]
pub struct ForceTarget {
    /// The daemon's socket, for the interrupt half.
    pub socket: std::path::PathBuf,
    /// `App::daemon_pid` - the process at the other end of this head's socket, from
    /// `SO_PEERCRED`. `None` and no signal is sent at all.
    pub pid: Option<i32>,
    /// When that process started, read when the stop began and re-read before every signal: a pid that now names a
    /// process that started at a different time is not the daemon this head attached to, and it is not signalled.
    pub started: Option<u64>,
}

/// How long the force path waits for the interrupt half to answer at all. A daemon that
/// accepted the connection and then says nothing is exactly the wedge being forced, so the
/// interrupt is not allowed to hold the signals up past this.
pub const FORCE_INTERRUPT_WAIT: Duration = Duration::from_secs(3);

/// How long the force path waits for the process after SIGKILL - the wrapper's
/// `wait_gone "$p" 4`, four half-second steps.
pub const FORCE_KILL_WAIT: Duration = Duration::from_secs(2);

/// `--stop --force`, in process: what the head runs itself when the daemon is an older build, or when an ordinary
/// stop did not finish. It is the head's own act, made only after the deadline passed with the process still there.
pub fn force_stop(t: &ForceTarget, signal: &Signaller) -> crate::app::ForceOutcome {
    use crate::driver::{process_gone, process_started};
    let mut said: Vec<String> = Vec::new();
    // ---- The interrupt, bounded. On its own thread because a wedged daemon accepts the
    // connection and never answers it, and `HeadClient::attach` has no deadline of its own.
    let (tx, rx) = std::sync::mpsc::channel();
    let socket = t.socket.clone();
    std::thread::spawn(move || {
        let r = interrupt_turns(
            &socket,
            FORCE_INTERRUPT_WAIT,
            "the head's exit-and-stop (force)",
            |_| {},
        )
        .map_err(|e| e.to_string());
        let _ = tx.send(r);
    });
    said.push(
        match rx.recv_timeout(FORCE_INTERRUPT_WAIT + SESSION_LIST_WAIT + Duration::from_secs(1)) {
            Ok(Ok(Interrupted::NoSessions)) => "it held no sessions to interrupt".into(),
            Ok(Ok(Interrupted::NoneRunning)) => "no turns were in flight".into(),
            Ok(Ok(Interrupted::Ended(n))) => format!("{n} turn(s) interrupted and ended"),
            Ok(Err(e)) => format!("the interrupt did not go through ({e})"),
            Err(_) => format!(
                "the daemon did not answer the interrupt within {}s",
                FORCE_INTERRUPT_WAIT.as_secs()
            ),
        },
    );
    let gone = |pid| process_gone(pid) == Some(true);
    let finish = |said: Vec<String>, gone: bool| crate::app::ForceOutcome {
        gone,
        said: said.join("; "),
    };
    let Some(pid) = t.pid else {
        said.push("no signal sent: the kernel did not name the daemon's pid".into());
        return finish(said, false);
    };
    if gone(pid) {
        said.push(format!("it went without a signal (pid {pid})"));
        return finish(said, true);
    }
    // Never this process. A head whose daemon runs in its own process (a test's
    // in-process server is the only such case) must not be the one it kills.
    if pid == std::process::id() as i32 {
        said.push(format!("no signal sent: pid {pid} is this head itself"));
        return finish(said, false);
    }
    let steps = [
        (
            libc::SIGTERM,
            "SIGTERM",
            Duration::from_millis(crate::app::FORCE_TERM_GRACE_MS),
        ),
        (libc::SIGKILL, "SIGKILL", FORCE_KILL_WAIT),
    ];
    for (sig, name, grace) in steps {
        // Still the same process? Read immediately before the signal, so the window in
        // which the pid could be handed on is a syscall wide.
        if process_started(pid) != t.started || t.started.is_none() {
            said.push(format!(
                "no {name} sent: pid {pid} is no longer the process this head attached to"
            ));
            return finish(said, gone(pid));
        }
        if let Err(e) = signal(pid, sig) {
            if e.raw_os_error() == Some(libc::ESRCH) {
                said.push(format!("it went before {name} (pid {pid})"));
                return finish(said, true);
            }
            said.push(format!("{name} could not be sent to pid {pid} ({e})"));
            return finish(said, false);
        }
        let until = Instant::now() + grace;
        while Instant::now() < until {
            if gone(pid) {
                said.push(format!("{name} ended it (pid {pid})"));
                return finish(said, true);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        said.push(format!("{name} did not end it within {}s", grace.as_secs()));
    }
    let g = gone(pid);
    finish(said, g)
}

/// Which sessions does the daemon hold? No frame lists them without a seat, but the ATTACH refusal for an unknown
/// id is a `Bye` that lists what is held, so the refusal is the directory and a probe costs one round trip.
fn discover(path: &std::path::Path) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    match HeadClient::attach(path, "\0probe", 0, "remote", "stop-force", Caps::default()) {
        // A daemon that accepted the probe id would be one that mints sessions
        // on attach, which none does; treat it as "held nothing useful".
        Ok(_) => Ok(Vec::new()),
        Err(ClientError::Refused(reason)) => match reason.split_once("it holds ") {
            Some((_, list)) => Ok(list.split(", ").map(str::to_string).collect()),
            // "this daemon holds none yet" - nothing held, nothing to interrupt.
            None => Ok(Vec::new()),
        },
        Err(e) => Err(e.into()),
    }
}

/// One `ListSessions` round trip: send it, then read frames until the reply
/// arrives, discarding everything else - the seat's event stream keeps
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

/// Run the head until it quits, and return the session it was serving. The id is returned rather than printed here
/// so the caller can write it after the terminal is restored. See [`print_exit_session`].
fn live(
    args: &Args,
    cfg: RenderConfig,
    fleet: Option<letibot_sessionlog::FleetReader>,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut app = App::new(cfg);
    app.load_prefs();
    if let Some(reader) = fleet {
        app.set_fleet_reader(reader);
    }

    // Take the screen and draw chrome before the round trip: `attach` blocks until the `Hello` arrives, and that
    // carries the whole snapshot, so a head that waited drew nothing at all for the length of a big session's.
    let term = if args.no_tty {
        None
    } else {
        Terminal::enter_with(any_motion()).ok()
    };
    // The handshake goes to a thread and the cat walks while it waits: a `Hello` carries the whole snapshot, so on
    // a big session this is a real wait. The reader thread starts inside `Link::open` and its first frame is the
    // `Hello`.
    let mut link = Link::open(
        &args.socket,
        &args.session,
        args.since,
        "tui",
        &args.identity,
    )?;

    if let Some(t) = &term {
        app.set_features(t.features());
        app.begin_attach_at(now_ms());
        let (w, h) = t.size();
        let frame = app.screen(w, h);
        outside_the_frame(t, &mut app);
        t.draw_with_cursor(&frame, app.cursor());
    }

    // Wait for the `Hello`: with a screen, draw the cat as the clock moves, and without one block properly rather
    // than sleeping in 120 ms steps for a frame nobody sees. The wait has to be escapable, which is why the keys are
    // read while it runs.
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
                        // Keys first: two Ctrl-Cs on an empty composer quit, which is what the hint bar under this
                        // frame has promised since before there was a frame.
                        for k in read_keys(t) {
                            let _ = app.key(k);
                        }
                        if app.should_quit() {
                            return Ok(app.session_id().to_string());
                        }
                        app.clock(now_ms());
                        let (w, h) = t.size();
                        let frame = app.screen(w, h);
                        outside_the_frame(t, &mut app);
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
    // A first frame this build cannot read is a skew, and it is named.
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
    // Which process that is, read off the socket this head just connected to.
    app.set_daemon_pid(link.daemon_pid());

    app.apply(hello);
    // After the `Hello`, so the head knows what the daemon holds before it asks for
    // something else - a resume of a session that is already live is then a switch
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
            link.tick(&mut app, (100, 40), &[], &[], &mut sink);
        }
        Some(term) => {
            let mut draw = |lines: &[String], cursor| term.draw_with_cursor(lines, cursor);
            while !app.should_quit() {
                // The read, as rano's events and routed: the editor pane takes every key while it has the keyboard
                // and every mouse report inside it, and what comes back is this head's keys.
                let (keys, raw) = read_routed(&term, &mut app);
                let size = term.size();
                if app.take_redraw() {
                    term.invalidate();
                }
                link.tick(&mut app, size, &keys, &raw, &mut draw);
                // The window title, after the tick: `draw` holds the terminal while `tick` holds
                // the app, so it is read here - once a tick, written only when it changed.
                outside_the_frame(&term, &mut app);
                // Getting back is this loop's job, because the socket is this layer's: the head knows that the link
                // is down and how long, and only here is there a path to open and a `Link` to replace.
                if app.should_reconnect() {
                    match link.reconnect(&args.socket, app.seq) {
                        // The attempt is out, so the head waits for its answer: the `ATTACH` is on a live socket and
                        // the `Hello` a moment away, and asking again would open a second socket and close the first.
                        Ok(()) => {
                            app.reconnect_sent();
                            // A reconnect is a new connection, so the daemon at the far end can be a different
                            // process; the pid is re-read rather than letting `/status` show the old one.
                            app.set_daemon_pid(link.daemon_pid());
                        }
                        // Said and counted, then tried again after the backoff. A
                        // `Err` here is the socket nobody is listening on yet.
                        Err(e) => app.reconnect_failed(&e.to_string()),
                    }
                }
            }
        }
    }
    // A detach is for a head leaving a daemon it is not stopping: while a stop is in flight the head has already
    // asked, and a `Detach` here would be a second, contradictory instruction racing the first.
    if app.wants_detach() {
        link.detach();
    }
    // After the terminal is back, because this is the one message that must outlive the screen: the alternate
    // screen is torn down by now, so a reason said into the transcript is gone and the head looks like it crashed.
    if let Some(said) = app.stop_farewell() {
        eprintln!("letibot: {said}");
    }
    if let Some(reason) = app.farewell() {
        eprintln!("letibot: the daemon ended this head — {reason}");
    }
    // The id, read last and read live: a head that switched with ctrl-s, or was switched by a `--resume` at
    // attach, served something other than the daemon's first session, and the next command needs the one it was in.
    Ok(app.session_id().to_string())
}

#[cfg(test)]
mod exit_tests {
    use super::*;

    /// The exit line is the id and nothing else, so it is asserted on the bytes rather than described: the next
    /// command is `letibot` in a shell, and it reads this stream.
    #[test]
    fn the_exit_prints_the_id_it_was_serving_and_nothing_else() {
        let mut out: Vec<u8> = Vec::new();
        print_exit_session(&mut out, "s-1788987496351498881").expect("writing");
        assert_eq!(
            String::from_utf8(out).expect("utf8"),
            "s-1788987496351498881\n"
        );
    }

    /// A head that never seated a session prints nothing at all. An empty line is still a
    /// line: a script reading stdout would take it for an id, and the operator would see a blank
    /// where the answer should be - which is how a refusal comes to look like a bug in the exit.
    #[test]
    fn a_head_that_never_seated_a_session_prints_nothing() {
        let mut out: Vec<u8> = Vec::new();
        print_exit_session(&mut out, "").expect("writing");
        assert!(out.is_empty(), "{:?}", String::from_utf8_lossy(&out));
    }
}

#[cfg(test)]
mod probe_tests {
    use super::*;
    use letibot_sessionlog::Hub;
    use letibot_sessionlog::server::{self, ServerHandle};

    fn socket_path(tag: &str) -> std::path::PathBuf {
        // `scratch_root`, not `temp_dir`: a socket path must fit `sun_path`, and a long
        // `TMPDIR` made this one too long to bind (see `scratch_root`).
        let mut p = letibot_sessionlog::testing::scratch_root();
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
            version: false,
            theme_template: false,
            wait: 30,
            reason: "test".into(),
            probe: true,
            models: None,
        }
    }

    /// A turn that does not end in time is said as one, under the command that asked: the deadline
    /// must not already be past, or this answers *timed out waiting for the session list*.
    #[test]
    fn a_turn_still_running_at_the_deadline_is_counted_and_the_reason_is_the_callers() {
        let (hub, h) = start("running");
        hub.publish(letibot_sessionlog::SessionEvent::TurnStarted {
            turn_id: "t1".into(),
            model: "m".into(),
            ledger_head: String::new(),
            began_ms: None,
        });
        let mut told = Vec::new();
        let r = interrupt_turns(h.path(), Duration::from_secs(1), "letibot --stop", |id| {
            told.push(id.to_string())
        });
        let e = r.expect_err("nothing here ends the turn").to_string();
        assert_eq!(e, "1 turn(s) still running after 1s");
        assert_eq!(told, vec!["s".to_string()]);
        let mut reasons = Vec::new();
        while let Some(c) = hub.try_command() {
            if let letibot_sessionlog::CommandKind::Interrupt { reason } = c.kind {
                reasons.push(reason);
            }
        }
        assert_eq!(reasons, vec!["letibot --stop".to_string()]);
        h.shutdown();
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

    /// A peer that answers with a frame this build cannot read is a daemon that is here and is not this build, which
    /// is a `1` and must never be a `2`: the launcher reads `2` as no daemon and starts one over the live socket.
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

#[cfg(test)]
mod models_tests {
    use super::*;
    use letibot_sessionlog::server;
    use letibot_sessionlog::{CommandKind, Hub, SessionEvent};

    fn socket_path(tag: &str) -> std::path::PathBuf {
        let mut p = letibot_sessionlog::testing::scratch_root();
        p.push(format!("letibot-models-{tag}-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    /// The daemon's half of `/models`: take the queued command and publish the answer under the
    /// codes its own slash door uses. Returns the line the head sent, or `None` if none came.
    fn daemon(
        hub: &std::sync::Arc<Hub>,
        ok: bool,
        said: &'static str,
    ) -> std::thread::JoinHandle<Option<String>> {
        let hub = hub.clone();
        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                let Some(cmd) = hub.try_command() else {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                };
                let CommandKind::Slash { line } = cmd.kind else {
                    continue;
                };
                hub.publish(SessionEvent::Warning {
                    code: if ok { "slash" } else { "slash_refused" }.into(),
                    detail: format!("/{line}\n{said}"),
                    compaction: None,
                });
                return Some(line);
            }
            None
        })
    }

    fn args_with(socket: &std::path::Path, models: &str) -> Args {
        let argv: Vec<String> = vec![
            "--socket".into(),
            socket.display().to_string(),
            "--session".into(),
            "s".into(),
            "--models".into(),
            models.into(),
        ];
        parse(&argv).expect("a parsable command line")
    }

    /// The flag is the daemon's own verb: the line it sends is `models NAME`, the answer is
    /// stdout's, and the exit code is the daemon's verdict rather than this head's reading of it.
    #[test]
    fn the_models_flag_moves_a_running_session_and_says_so() {
        let hub = Hub::new("s");
        let h = server::serve(hub.clone(), socket_path("moves")).expect("bind");
        let said = "now answering on dense78 (deepseek); the next turn uses it";
        let worker = daemon(&hub, true, said);
        let args = args_with(h.path(), "dense78");
        let name = args.models.as_deref().expect("the flag's value");
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = models(&args, name, &mut out, &mut err);
        assert_eq!(worker.join().unwrap().as_deref(), Some("models dense78"));
        h.shutdown();
        assert_eq!(String::from_utf8(out).unwrap(), format!("{said}\n"));
        assert_eq!(String::from_utf8(err).unwrap(), "");
        assert_eq!(code, 0, "the daemon accepted the switch");
    }

    /// A refusal is a `1` and its words are stderr's, so a script that reads stdout gets an answer
    /// or nothing and never a refusal it would take for one.
    #[test]
    fn a_refused_models_switch_is_a_one_and_the_refusal_is_on_stderr() {
        let hub = Hub::new("s");
        let h = server::serve(hub.clone(), socket_path("refused")).expect("bind");
        let said = "no provider named \"nosuch\" in providers.toml; /models lists what is there";
        let worker = daemon(&hub, false, said);
        let args = args_with(h.path(), "nosuch");
        let name = args.models.as_deref().expect("the flag's value");
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = models(&args, name, &mut out, &mut err);
        assert_eq!(worker.join().unwrap().as_deref(), Some("models nosuch"));
        h.shutdown();
        assert_eq!(String::from_utf8(out).unwrap(), "");
        assert_eq!(String::from_utf8(err).unwrap(), format!("{said}\n"));
        assert_eq!(code, 1, "the daemon refused the switch");
    }

    /// An empty name is refused by the flag's own name and before a socket is opened: a daemon
    /// sent `/models` with nothing after it answers a LISTING, which is not what was asked.
    #[test]
    fn an_empty_models_name_is_refused_before_anything_is_sent() {
        let e = parse(&["--models".to_string(), "  ".to_string()])
            .err()
            .expect("refused");
        assert!(e.contains("--models"), "{e}");
    }
}

/// The terminal's modes for this head: every motion reported, not only the buttons' - what
/// the pointer is over is drawn (`app::pointer`), and that needs motion with no button held.
fn any_motion() -> rano::term::Options {
    rano::term::Options {
        mouse: rano::term::Mouse::Any,
        ..Default::default()
    }
}

/// Everything the head writes to the terminal that is not the frame: the window title, the tab's progress bar, a
/// notification, a clipboard copy. Once a tick, after it, and only what changed.
fn outside_the_frame(term: &Terminal, app: &mut App) {
    term.set_title(&app.window_title());
    term.set_progress(app.progress());
    if let Some(n) = app.take_notification() {
        term.notify(&n);
    }
    if let Some(text) = app.take_clipboard() {
        term.copy(&text);
    }
    // The hand over what a click would act on, the default off it (OSC 22). A terminal that
    // does not speak it ignores it; the underline says the same thing everywhere.
    if let Some(hand) = app.take_pointer() {
        term.write_raw(if hand {
            b"\x1b]22;pointer\x1b\\"
        } else {
            b"\x1b]22;default\x1b\\"
        });
    }
    for upload in app.take_image_uploads() {
        term.write_raw(&upload);
    }
}

/// One read of the terminal, as this head's keys: whatever `Terminal::events` returns  -
/// at most ~100 ms of quiet, the `VMIN=0 VTIME=1` read the loops above are paced by - mapped
/// through [`crate::app::key_of`]. The bytes that read consumed are `Terminal::raw_input`.
fn read_keys(term: &Terminal) -> Vec<crate::app::Key> {
    term.events()
        .into_iter()
        .filter_map(crate::app::key_of)
        .collect()
}

/// One read of the terminal with the editor pane's share taken out, then rano's work between events: what comes
/// back is this head's keys and the raw bytes a `!term` pane reads. The live loop and `--replay` both read here.
fn read_routed(term: &Terminal, app: &mut App) -> (Vec<crate::app::Key>, Vec<u8>) {
    // Keys a click typed come first, and stand for this read: a click on `esc closes` is the
    // Esc key, by the route a keypress takes (see `app::keyhints`), and the terminal is read
    // again on the next pass.
    let injected = app.take_injected();
    if !injected.is_empty() {
        let (keys, _) = app.route(rano::term::decode(&injected));
        app.editor_tick(Instant::now());
        return (keys, Vec::new());
    }
    let (events, raw) = match app.editor_wait() {
        Some(wait) if wait < crate::app::READ_PACE && !term.poll(wait) => (Vec::new(), Vec::new()),
        // The same read as bytes: the pane's program reads what the person's terminal sent and not this head's
        // decoding of it, and `ctrl-\` is a byte `key_of` maps to nothing, so this head never sees it.
        _ => {
            let events = term.events();
            (events, term.raw_input())
        }
    };
    let (keys, rano_took) = app.route(events);
    // Bytes rano read are not a `!term` pane's: `ctrl-\` is rano's replace, and a byte the editor consumed must not
    // reach the pane's way-out scan as though the terminal had sent it at a pane.
    let raw = if rano_took { Vec::new() } else { raw };
    // rano's work between events, before the frame - rano's rule for a host.
    app.editor_tick(Instant::now());
    (keys, raw)
}
