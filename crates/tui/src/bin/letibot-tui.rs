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
use std::time::Duration;

use letibot_sessionlog::client::{HeadClient, pump};
use letibot_sessionlog::event::Envelope;
use letibot_sessionlog::protocol::{Caps, ServerFrame};
use letibot_sessionlog::server::default_socket_path;
use letibot_sessionlog::testing;

use letibot_tui::app::App;
use letibot_tui::driver::tick;
use letibot_tui::render::{Budget, RenderConfig};
use letibot_tui::term::Terminal;

struct Args {
    socket: std::path::PathBuf,
    session: String,
    since: u64,
    replay: Option<String>,
    demo: bool,
    no_tty: bool,
    budget: Budget,
    identity: String,
}

fn parse() -> Result<Args, String> {
    let mut a = Args {
        socket: default_socket_path(),
        session: String::new(),
        since: 0,
        replay: None,
        demo: false,
        no_tty: false,
        budget: Budget::default(),
        identity: std::env::var("USER").unwrap_or_else(|_| "operator".into()),
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut next = || it.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--socket" => a.socket = next()?.into(),
            "--session" => a.session = next()?,
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
            "-h" | "--help" => return Err(usage()),
            other => return Err(format!("unknown argument {other}\n\n{}", usage())),
        }
    }
    Ok(a)
}

fn usage() -> String {
    "letibot-tui [--socket PATH] [--session ID] [--since SEQ] [--identity NAME]\n\
     \x20           [--replay FILE.jsonl] [--demo] [--no-tty]\n\
     \x20           [--body-lines N] [--reasoning-lines N]"
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

    if let Err(e) = live(&args, cfg) {
        eprintln!("letibot-tui: {e}");
        std::process::exit(1);
    }
}

/// Render a recorded log. No daemon, no model, no socket.
fn replay(args: &Args, cfg: RenderConfig) {
    let mut app = App::new(cfg);
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
    app.apply(hello);

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
