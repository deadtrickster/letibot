//! `letibot-tui` as a FUNCTION, so more than one binary can be it.
//!
//! The head is the role a person meets, and it is also the one a SUBSTITUTE speaks
//! for: `scripts/letibot` hands the head `--socket S --identity U [--resume ID |
//! --new-session TITLE]`, and leticl's Common Lisp head is a head that speaks that
//! same contract. So this function's argument handling is the contract, and it is
//! moved rather than rewritten — the flags parse exactly as they did.
//!
//! # What being a function changed
//!
//! The arguments arrive as a parameter, and `main`'s own `std::process::exit(n)`
//! became `return n` so the caller decides whether the process ends.
//!
//! **One exit IS left inside a helper** — `replay`'s, for a demo that cannot read its
//! input. It is named here rather than hidden: the alternative is threading a code
//! through the replay path for no observable difference, and the exit code and the
//! message are the interface.
//!
//! The non-interactive paths are what a dispatcher can get wrong, and they were
//! captured from the binary before this move and compared after: `--version` 0,
//! `--help` 2, an unknown flag 2, a missing socket on `--list-sessions` 1 and on
//! `--probe` 2, a flag with no value 2.

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
    /// **Print the version and exit.** An installer has to be able to prove the binary it just copied
    /// runs, and the cheapest proof is a question it can answer without a socket, a model or a store
    /// — see `harnessd`'s arm for the full reason. `CARGO_PKG_VERSION` is the WORKSPACE's version,
    /// which is the number the release tag must agree with.
    version: bool,
    theme_template: bool,
    /// How long `--interrupt-all` waits for the turns to end.
    wait: u64,
    /// **What the interrupt is recorded as** (`--reason`): the command that sent it, so a plain
    /// `letibot --stop` is not written into the transcript as a forced one (TODO R92.21).
    reason: String,
    /// Answer "is there a daemon here, and does it speak this build's protocol" and
    /// exit. No screen, no snapshot, no frame — see [`probe`].
    probe: bool,
}

/// **`args` is a parameter, and that is the whole point of the move.**
///
/// The first version of this left `parse()` reading `std::env::args().skip(1)` —
/// so `run(args)` received the arguments and then ignored them. MEASURED, and it is
/// why the check has TWO arms: `letibot-render`-style calls worked (the process's
/// argv was right by luck, since the symlink is invoked with no role word), while
/// `letibot tui --version` answered `unknown argument tui` — because `parse()` read
/// the process's argv, which still began with the role word.
///
/// A function that takes its arguments and then reaches past them for the real ones
/// is the defect the whole extraction exists to avoid: the binary works and the
/// dispatcher cannot.
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
     \x20           [--theme-template]"
        .into()
}

/// Run the head over `args`, returning the process exit code.
///
/// `args` is what followed the program name — the launcher and `letibot tui`
/// both arrive here identically, which is the point.
pub fn run(args: &[String]) -> i32 {
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

    if args.probe {
        return probe(&args);
    }

    // **The id, on stdout, as the last thing this head says.** The launcher `exec`s the head,
    // so this stream IS the launcher's stdout — the one place an exit can leave something for
    // the next command to read. What else an exit says (a farewell, a stopped daemon's
    // apology) goes to stderr and stays there.
    //
    // `--no-tty` is the one head that leaves no line: its stdout is documented as ONE frame,
    // and an id appended to a screen is a line a pipeline would read as part of the render.
    // The id is still available there by the route a script should be using anyway —
    // `harnessd --latest-session`.
    match live(&args, cfg) {
        Ok(served) => {
            if !args.no_tty {
                if let Err(e) = print_exit_session(&mut std::io::stdout(), &served) {
                    eprintln!("letibot-tui: {e}");
                    return 1;
                }
            }
            0
        }
        Err(e) => {
            eprintln!("letibot-tui: {e}");
            1
        }
    }
}

/// **The id this head was serving, as the one line an exit leaves on stdout.**
///
/// # Why the head prints it and not the daemon
///
/// The launcher `exec`s this head, so a head's stdout is the launcher's stdout — the terminal
/// the person is sitting at. The daemon's stdout is not: `scripts/letibot` starts it detached
/// with `>>"$LOG"`, so an id printed there lands in a log file nobody reads, and the operator's
/// ask — *"on exit you have to print main session id to stdout too"* — would be met on paper and
/// nowhere else.
///
/// # Why exactly one line, and why it is the session it was SERVING
///
/// The id is the thing the next command needs: `letibot --session <id>` reopens it when the
/// automatic pick is wrong, and a person reads it off the screen and pastes it. So it goes out
/// alone, on stdout, with nothing beside it — everything else an exit says is a sentence on
/// stderr, which a pipeline ignores.
///
/// It is the session the head was **in** at exit, not the daemon's first session and not its
/// root: the operator may have switched with ctrl-s, and resuming the child they were actually
/// looking at is a thing `--session` supports. A head that never got a `Hello` — the wait was
/// interrupted — has no id and says nothing at all, because an empty line is a line a script
/// would take for one.
fn print_exit_session<W: std::io::Write>(out: &mut W, served: &str) -> std::io::Result<()> {
    if served.is_empty() {
        return Ok(());
    }
    writeln!(out, "{served}")
}

/// How long the head waits for the daemon's `Hello` before giving up.
///
/// The `Hello` carries the whole snapshot, so this has to allow for a big session on a
/// busy daemon — but it is bounded because the alternative is what the operator hit: a
/// daemon that accepts the connection and never answers, and a head that waits for ever
/// with no screen telling them so. Long enough for a slow snapshot, short enough that a
/// hung daemon is reported rather than endured.
const ATTACH_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// **Is there a daemon here, and can this build attach to it?**
///
/// `0` when there is and it answers, `1` when there is and it refuses (with the
/// daemon's own refusal on stderr), `2` when there is not. A version skew is no
/// longer a refusal from a current daemon — it accepts the attach and states its own
/// number in the `Hello`, so a skewed probe is exit `0` and the launcher routes to
/// the head, which seats read-only against an older daemon and says so. Exit `1` on
/// a version skew is now the mark of a daemon built **before** that acceptance: its
/// bare `Bye` is exactly what this arm prints.
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

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

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
            if s.status.model.is_empty() {
                "-"
            } else {
                &s.status.model
            },
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

/// Interrupt every session whose turn is running, through the same frame an
/// Esc-Esc sends, and wait for the turns to end.
///
/// This is `letibot --stop --force`'s first half — and the head's own force path's
/// ([`force_stop`]). The daemon's own signal handler deliberately has no force path —
/// its comment calls a kill "a way to lose the last turn's rows" — and this is the
/// shape that loses none: each running turn is asked to stop over the protocol, the
/// abort is recorded like any other, and the daemon is left to a plain SIGTERM, which
/// it can now take promptly because nothing is mid-turn.
///
/// `each` is told every session id as its interrupt goes out. `reason` is what the abort is
/// recorded as — the command that asked. An `Err` is a daemon that could not be asked, or turns
/// still running when `wait` ran out.
pub fn interrupt_turns(
    socket: &std::path::Path,
    wait: Duration,
    reason: &str,
    mut each: impl FnMut(&str),
) -> Result<Interrupted, Box<dyn std::error::Error>> {
    let deadline = Instant::now() + wait;
    // **Each question gets its own time to be answered** (R92.21). The list was asked with the
    // OVERALL deadline, so the ask after it had passed timed out at once and the intended
    // "N turn(s) still running" could never be printed — the operator read "timed out waiting for
    // the session list" instead.
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
        // `expected_seq = 0` is "no expectation" at the hub — never stale —
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

/// **How the force path sends a signal**: `kill(pid, sig)`. A parameter rather than a
/// call so a test can stand in a process that survives everything — the one case the
/// head's last sentence is for — without needing a process the kernel cannot kill.
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

/// **The daemon the force path may signal**: the one this head attached to and nothing else.
#[derive(Debug, Clone)]
pub struct ForceTarget {
    /// The daemon's socket, for the interrupt half.
    pub socket: std::path::PathBuf,
    /// `App::daemon_pid` — the process at the other end of this head's socket, from
    /// `SO_PEERCRED`. `None` and no signal is sent at all.
    pub pid: Option<i32>,
    /// **When that process started**, read when the stop began. Re-read before every signal:
    /// a pid that now names a process that started at a different time is not the daemon
    /// this head attached to — the daemon went and the number was handed on — and it is not
    /// signalled.
    pub started: Option<u64>,
}

/// How long the force path waits for the interrupt half to answer at all. A daemon that
/// accepted the connection and then says nothing is exactly the wedge being forced, so the
/// interrupt is not allowed to hold the signals up past this.
pub const FORCE_INTERRUPT_WAIT: Duration = Duration::from_secs(3);

/// How long the force path waits for the process after SIGKILL — the wrapper's
/// `wait_gone "$p" 4`, four half-second steps.
pub const FORCE_KILL_WAIT: Duration = Duration::from_secs(2);

/// **`letibot --stop --force`, in process** — what the head runs itself when the daemon is
/// an older build, or when an ordinary stop did not finish.
///
/// The operator: *"yeah when i exited letidb and chose to stop daemon too - it couldnt. had
/// to --stop --force"*; asked whether the head should detect an older daemon and take the
/// force path itself with a line saying why: *"yes"*.
///
/// The same steps as `scripts/letibot --stop --force`, in the same order and with the same
/// waits: [`interrupt_turns`] (the protocol half, so no turn's rows are lost), then SIGTERM —
/// which `harnessd` takes as `Registry::close` — then, if the process is still there
/// [`FORCE_TERM_GRACE_MS`](crate::app::FORCE_TERM_GRACE_MS) later, SIGKILL.
///
/// **It signals only [`ForceTarget::pid`], and only while that pid is still the process it
/// was** (its start time, re-read before each signal), never this process itself, and never
/// a process that is already gone. Blocking: the driver runs it on its own thread so the
/// screen stays live.
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
    // **Never this process.** A head whose daemon runs in its own process (a test's
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
        // **Still the same process?** Read immediately before the signal, so the window in
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

/// **Run the head until it quits, and return the session it was serving.**
///
/// The id is the exit's one piece of output — see [`print_exit_session`], which the caller
/// writes it with once this function has returned. It is returned rather than printed here
/// because returning is what drops the `Terminal`: the alternate screen is torn down as this
/// function's locals go out of scope, and a line printed before that lands on a screen that is
/// about to be erased.
///
/// Empty means *this head never seated a session* — the attach was refused, the `Hello` never
/// came, or the person quit during the wait. There is no id to give then, and the caller says
/// nothing rather than an empty line.
fn live(args: &Args, cfg: RenderConfig) -> Result<String, Box<dyn std::error::Error>> {
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
        Terminal::enter_with(any_motion()).ok()
    };
    // **The handshake goes to a thread, and the cat walks while it waits.**
    //
    // A `Hello` carries the whole snapshot, so on a big session this is a real wait.
    // The reader thread starts inside `Link::open` and the first frame it hands over is
    // the `Hello`, so the wait below is an ordinary read on an ordinary channel — which
    // is also what makes a *re*-attach look like the first one: same frame, same path.
    //
    // The Attach is sent here rather than on the thread, so a refused attach — a
    // daemon built before the acceptance change refusing on the version, or an
    // unknown session — is known before anything is drawn, and a head that cannot
    // attach never claims the screen. A *socket* error is still only discoverable
    // on the connect, so this reports one.
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

    // Wait for the `Hello`. **With a screen, draw the cat as the clock moves**; without
    // one — the `--no-tty` path — block properly rather than sleeping in 120 ms steps
    // for a frame nobody will see.
    //
    // # The wait has to be escapable, and it was not
    //
    // This loop draws the cat while `HeadClient` waits for a `Hello`. The first version
    // read nothing from the terminal, so **keys went nowhere**: the main loop — the only
    // place the terminal was read — had not started, and the terminal is in raw mode,
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
    // **Which process that is** (R30), read off the socket this head just connected to.
    app.set_daemon_pid(link.daemon_pid());

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
            link.tick(&mut app, (100, 40), &[], &[], &mut sink);
        }
        Some(term) => {
            let mut draw = |lines: &[String], cursor| term.draw_with_cursor(lines, cursor);
            while !app.should_quit() {
                // **The read, as rano's events, and routed.** The editor pane takes its share —
                // every key while it has the keyboard, every mouse report inside it — and what
                // comes back is this head's keys, exactly as `read_keys` would have made them.
                //
                // **The wait is the editor's when the editor needs it sooner.** `events` waits
                // ~100 ms for a key, which is the head's pace and too slow for a file streaming
                // into rano or a prefix card coming due; so with a pane open the loop polls for
                // rano's `next_wakeup` first and reads only when something is there.
                let (keys, raw) = read_routed(&term, &mut app);
                let size = term.size();
                if app.take_redraw() {
                    term.invalidate();
                }
                link.tick(&mut app, size, &keys, &raw, &mut draw);
                // The window title, after the tick: `draw` holds the terminal while `tick` holds
                // the app, so it is read here — once a tick, written only when it changed.
                outside_the_frame(&term, &mut app);
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
                        Ok(()) => {
                            app.reconnect_sent();
                            // A reconnect is a new connection, so the daemon at the far
                            // end can be a different process — a restart, or a different
                            // build entirely. Re-read rather than letting `/status` show
                            // the pid of the daemon that just went away.
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
    // **A detach is for a head that is leaving a daemon it is not stopping.** R30: while a
    // stop is in flight the daemon is on its way out and this head has already asked; a
    // `Detach` here would be a second, contradictory instruction racing the first, and the
    // connection closing is itself one of the facts the wait is reading.
    if app.wants_detach() {
        link.detach();
    }
    // **After the terminal is back**, because this is the one message that must
    // outlive the screen: the alternate screen has been torn down by now, so a
    // reason said into the transcript is gone and the head looks like it crashed.
    //
    // **R30's third part, and the reason it is on stderr and not on the screen.** The
    // operator chose *stop the daemon*, and if it did not stop they have to learn that from
    // this head rather than from `ps` a day later — which is exactly how the orphan this
    // rule exists for was found. It cannot be a note in the transcript: the transcript goes
    // down with the alternate screen, and the head is about to be gone. So it is said here,
    // once, on the terminal the operator is looking at.
    if let Some(said) = app.stop_farewell() {
        eprintln!("letibot: {said}");
    }
    if let Some(reason) = app.farewell() {
        eprintln!("letibot: the daemon ended this head — {reason}");
    }
    // **The id, read last and read live.** A head that switched with ctrl-s — or that was
    // switched by a `--resume` at attach — was serving something other than the daemon's first
    // session, and the id the next command needs is the one it was actually in. The terminal is
    // already back (the `Some(term)` arm above dropped it), so this is a line on the operator's
    // own screen rather than a row of a frame that is about to be erased.
    Ok(app.session_id().to_string())
}

#[cfg(test)]
mod exit_tests {
    use super::*;

    /// **The exit line is the id and nothing else.**
    ///
    /// This is the whole of the operator's second ask — *"on exit you have to print main
    /// session id to stdout too"* — so it is asserted on the bytes rather than described: the
    /// next command is `letibot --session <id>`, and a person copies the line off the terminal.
    /// A sentence on the same line would make it a line to edit rather than one to paste.
    #[test]
    fn the_exit_prints_the_id_it_was_serving_and_nothing_else() {
        let mut out: Vec<u8> = Vec::new();
        print_exit_session(&mut out, "s-1788987496351498881").expect("writing");
        assert_eq!(
            String::from_utf8(out).expect("utf8"),
            "s-1788987496351498881\n"
        );
    }

    /// **A head that never seated a session prints nothing at all.** An empty line is still a
    /// line: a script reading stdout would take it for an id, and the operator would see a blank
    /// where the answer should be — which is how a refusal comes to look like a bug in the exit.
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
        }
    }

    /// **A turn that does not end in time is said as one, under the command that asked**
    /// (TODO R92.21). The list used to be asked with the deadline already past, so this answered
    /// "timed out waiting for the session list"; and every interrupt was recorded as
    /// `letibot --stop --force`.
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

/// **The terminal's modes for this head: every motion reported**, not only the buttons' — what
/// the pointer is over is drawn (`app::pointer`), and that needs motion with no button held.
fn any_motion() -> rano::term::Options {
    rano::term::Options {
        mouse: rano::term::Mouse::Any,
        ..Default::default()
    }
}

/// **Everything the head writes to the terminal that is not the frame**: the window title, the
/// tab's progress bar, a desktop notification, a clipboard copy. Once a tick, after the tick —
/// the app decides what each should be, and the terminal writes only what changed and only
/// what it speaks (see `rano::term::features`).
fn outside_the_frame(term: &Terminal, app: &mut App) {
    term.set_title(&app.window_title());
    term.set_progress(app.progress());
    if let Some(n) = app.take_notification() {
        term.notify(&n);
    }
    if let Some(text) = app.take_clipboard() {
        term.copy(&text);
    }
    // **The hand over what a click would act on**, the default off it (OSC 22). A terminal that
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

/// **One read of the terminal, as this head's keys**: whatever `Terminal::events` returns —
/// at most ~100 ms of quiet, the `VMIN=0 VTIME=1` read the loops above are paced by — mapped
/// through [`crate::app::key_of`]. The bytes that read consumed are `Terminal::raw_input`.
fn read_keys(term: &Terminal) -> Vec<crate::app::Key> {
    term.events()
        .into_iter()
        .filter_map(crate::app::key_of)
        .collect()
}

/// **One read of the terminal with the editor pane's share taken out**, then rano's work
/// between events: what comes back is this head's keys, exactly as [`read_keys`] would have
/// made them, and the raw bytes a `!term` pane reads. The live loop and `--replay` both read
/// through here, so the acceptance suite (`tests/acceptance/`) drives the pane the way a
/// session does.
///
/// **The wait is the editor's when the editor needs it sooner.** `events` waits ~100 ms for a
/// key, which is the head's pace and too slow for a file streaming into rano or a prefix card
/// coming due; so with a pane open this polls for rano's `next_wakeup` first and reads only
/// when something is there.
fn read_routed(term: &Terminal, app: &mut App) -> (Vec<crate::app::Key>, Vec<u8>) {
    // **Keys a click typed come first**, and stand for this read: a click on `esc closes` is the
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
        // **The same read, as bytes.** The pane is a terminal and its program reads the bytes
        // the operator's terminal sent, not this head's decoding of them — and `ctrl-\`, the
        // pane's one way out, is a byte `key_of` maps to nothing, so this head never sees it
        // as a `Key`. See `Terminal::raw_input`.
        _ => {
            let events = term.events();
            (events, term.raw_input())
        }
    };
    let (keys, rano_took) = app.route(events);
    // **Bytes rano read are not a `!term` pane's.** `ctrl-\` is rano's replace, and a byte
    // the editor consumed must not reach the pane's way-out scan as though the operator's
    // terminal had sent it at a pane (see `Link::tick`, where the scan lives and where a
    // read the composer consumed has no way out in it at all).
    let raw = if rano_took { Vec::new() } else { raw };
    // rano's work between events, before the frame — rano's rule for a host.
    app.editor_tick(Instant::now());
    (keys, raw)
}
