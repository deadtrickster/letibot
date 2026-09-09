//! `harnessd` — the daemon.
//!
//! ```text
//! harnessd [--workspace DIR] [--socket PATH] [--store PATH]
//!          [--dialect glm|qwen] [--model ALIAS] [--endpoint HOST:PORT]
//!          [--vocab GGUF] [--system FILE] [--effort low|medium|high|xhigh]
//!          [--spill-inline BYTES] [--spill-dir DIR]
//!          [--max-tool-rounds N] [--session ID]
//!          [--prompt TEXT ...]        run these, print the answers, exit
//! ```
//!
//! With no `--prompt` it serves the socket and waits. Attach a head with
//! `letibot-tui --socket PATH`.
//!
//! With one or more `--prompt` it runs them in order and exits — which is what
//! makes a scripted session a shell command rather than a program, and it is how
//! `letibot-m1` drives its own measurement.

use std::path::PathBuf;

use letibot_harnessd::config::{Config, Disclosure, SpillPolicy, SpillStorage};
use letibot_harnessd::{Daemon, Dialect, Harness, Outcome, Parts};
use letibot_sessionlog::hub::Hub;
use letibot_turn::Endpoint;

fn usage() -> String {
    "harnessd [--workspace DIR] [--socket PATH] [--store PATH]\n\
     \x20        [--dialect glm|qwen] [--model ALIAS] [--endpoint HOST:PORT]\n\
     \x20        [--vocab GGUF] [--system FILE] [--effort low|medium|high|xhigh]\n\
     \x20        [--spill-inline BYTES] [--spill-dir DIR]\n\
     \x20        [--max-tool-rounds N] [--session ID] [--prompt TEXT ...]"
        .into()
}

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("harnessd: {e}");
            std::process::exit(1);
        }
    }
}

fn run() -> Result<i32, String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let mut cfg = Config::for_this_box(cwd);
    let mut prompts: Vec<String> = Vec::new();

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut next = || it.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--workspace" => cfg.workspace = PathBuf::from(next()?),
            "--socket" => cfg.socket = PathBuf::from(next()?),
            "--store" => cfg.store = Some(PathBuf::from(next()?)),
            "--session" => cfg.session_id = next()?,
            "--model" => cfg.model = next()?,
            "--vocab" => cfg.vocab_gguf = PathBuf::from(next()?),
            "--effort" => cfg.effort = Some(next()?),
            "--prompt" => prompts.push(next()?),
            "--max-tool-rounds" => {
                cfg.max_tool_rounds = next()?.parse().map_err(|e| format!("{arg}: {e}"))?
            }
            "--spill-inline" => {
                cfg.spill =
                    SpillPolicy::Inline(next()?.parse().map_err(|e| format!("{arg}: {e}"))?)
            }
            "--spill-dir" => cfg.spill_storage = SpillStorage::Dir(PathBuf::from(next()?)),
            "--dialect" => {
                let n = next()?;
                cfg.dialect =
                    Dialect::parse(&n).ok_or_else(|| format!("unknown dialect {n:?}"))?;
            }
            "--endpoint" => {
                let v = next()?;
                let (h, p) = v.rsplit_once(':').ok_or("--endpoint wants HOST:PORT")?;
                cfg.endpoint = Endpoint::new(h, p.parse().map_err(|e| format!("port: {e}"))?);
            }
            "--system" => {
                cfg.system = std::fs::read_to_string(next()?).map_err(|e| e.to_string())?;
            }
            "-h" | "--help" => {
                println!("{}", usage());
                return Ok(0);
            }
            other => return Err(format!("unknown argument {other}\n\n{}", usage())),
        }
    }

    let parts = Parts::load(&cfg).map_err(|e| e.to_string())?;
    let hub = Hub::new(cfg.session_id.clone());

    // Bind before opening the session: a socket that is already served means
    // another daemon owns this session, and finding that out after loading a
    // vocabulary wastes the load.
    let mut daemon = Daemon::serve(hub.clone(), &cfg.socket).map_err(|e| e.to_string())?;
    daemon.catch_signals().map_err(|e| e.to_string())?;

    let socket = daemon.socket().display().to_string();
    let dialect = cfg.dialect.name();
    let model = cfg.model.clone();
    let endpoint = cfg.endpoint.authority();
    let workspace = cfg.workspace.display().to_string();
    let disclosures = cfg.disclosures();

    let mut harness = match Harness::open(&parts, cfg, hub.clone()) {
        Ok(h) => h,
        Err(e) => {
            daemon.shutdown();
            return Err(e.to_string());
        }
    };

    eprintln!("harnessd: session {}", harness.config().session_id);
    eprintln!("  model    {model} via {endpoint} (/completion, token array)");
    eprintln!("  dialect  {dialect}");
    eprintln!("  workspace {workspace}");
    eprintln!("  socket   {socket}");
    eprintln!("  prefix   {} tokens, head {}", harness.tokens().len(), harness.ledger_head());
    eprintln!();
    for line in banner(&disclosures, term_cols()) {
        eprintln!("{line}");
    }
    eprintln!();

    let mut failed = 0;
    if !prompts.is_empty() {
        for p in &prompts {
            match harness.submit(p) {
                Ok(reply) => {
                    println!("{}", reply.text);
                    let keeps = reply.f_keep();
                    eprintln!(
                        "  [{} round(s), {} tool call(s), f_keep {}]",
                        reply.rounds,
                        reply.tool_calls,
                        keeps
                            .iter()
                            .map(|k| format!("{k:.4}"))
                            .collect::<Vec<_>>()
                            .join(" ")
                    );
                    if reply.truncated {
                        eprintln!("  ! the answer was cut short (finish_reason: length)");
                    }
                }
                Err(e) => {
                    eprintln!("  ! {e}");
                    failed += 1;
                    if e.is_fatal() {
                        break;
                    }
                }
            }
        }
        daemon.shutdown();
        return Ok(if failed > 0 { 1 } else { 0 });
    }

    eprintln!("  waiting for a head. Ctrl-C to stop.");
    daemon.run(&mut harness, |cmd, outcome| match outcome {
        Outcome::Replied(r) => eprintln!(
            "  {} -> {} round(s), {} tool call(s)",
            cmd.identity, r.rounds, r.tool_calls
        ),
        Outcome::Failed(e) => eprintln!("  {} -> {e}", cmd.identity),
        Outcome::Ignored => {}
    });
    daemon.shutdown();
    Ok(0)
}

/// The startup disclosures, as something that can be scanned.
///
/// The content is `Config::disclosures()` unchanged and unabridged. What changes is
/// the shape: a state word in a fixed column, the subject beside it, and the
/// sentence wrapped under them — so an operator glancing at a terminal reads
/// `OFF retrieval` in one saccade and can then choose to read why. The previous
/// form was five correct paragraphs run together, in which the words that decide
/// whether an answer can be trusted were mid-sentence.
fn banner(ds: &[Disclosure], cols: usize) -> Vec<String> {
    let width = cols.clamp(48, 100);
    let indent = 9usize;
    let mut out = vec!["  this session".to_string()];
    for d in ds {
        let state = if d.active { "on " } else { "OFF" };
        if d.state.is_empty() {
            // Configured, and the detail is short. One line.
            out.push(format!("  {state}  {} — {}", d.subject, d.detail));
            continue;
        }
        // A headline that can be read at a glance, then the sentence under it.
        // Not one padded column: `adjudication` is twelve characters and any
        // column wide enough for it wastes a fifth of an eighty-column terminal
        // on every other row.
        out.push(format!("  {state}  {} ({})", d.subject, d.state));
        for line in wrap(&d.detail, width.saturating_sub(indent)) {
            out.push(format!("{:indent$}{line}", ""));
        }
    }
    out
}

/// Word wrap. Twelve lines rather than a dependency, for one banner.
fn wrap(s: &str, width: usize) -> Vec<String> {
    let width = width.max(24);
    let mut out = Vec::new();
    let mut line = String::new();
    for word in s.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

/// The terminal width, when there is a terminal. Eighty when there is not — a log
/// file has no width and eighty is the width a log file is read at.
fn term_cols() -> usize {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(2, libc::TIOCGWINSZ, &mut ws) } == 0 && ws.ws_col > 0 {
        ws.ws_col as usize
    } else {
        80
    }
}
