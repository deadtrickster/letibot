//! `harnessd` — the daemon.
//!
//! ```text
//! harnessd [--workspace DIR] [--socket PATH] [--store PATH]
//!          [--dialect glm|qwen] [--model ALIAS] [--endpoint HOST:PORT]
//!          [--vocab GGUF] [--system FILE] [--effort low|medium|high|xhigh]
//!          [--spill-inline BYTES] [--spill-dir DIR]
//!          [--max-tool-rounds N] [--stall-rounds N] [--session ID] [--title NAME]
//!          [--prompt TEXT ...]        run these, print the answers, exit
//!
//! store queries — no socket, no vocabulary, no model:
//! harnessd --store PATH --list-sessions [--workspace DIR] [--tsv]
//! harnessd --store PATH --latest-session [--workspace DIR]
//! harnessd --store PATH --rename ID TITLE
//! harnessd --store PATH --delete ID
//! ```
//!
//! With no `--prompt` it serves the socket and waits. Attach a head with
//! `letibot-tui --socket PATH`.
//!
//! # The store queries answer before anything is loaded
//!
//! `--list-sessions` and its three siblings return **before** `Parts::load`, before
//! the endpoint health check and before the socket is bound. That is not an
//! optimisation: `letibot --sessions` has to work when the model server is down, and
//! a listing that first insisted on a 0.6-second GGUF load and a live `/health`
//! would be a listing you cannot use to find out what went wrong.
//!
//! With one or more `--prompt` it runs them in order and exits — which is what
//! makes a scripted session a shell command rather than a program, and it is how
//! `letibot-m1` drives its own measurement.

use std::path::PathBuf;

use letibot_harnessd::config::{
    AdjudicatorChoice, Config, Disclosure, Seat, SpillPolicy, SpillStorage,
};
use letibot_harnessd::{Daemon, Dialect, Outcome, Parts, Sessions};
use letibot_sessionlog::registry::Registry;
use letibot_turn::Endpoint;

fn usage() -> String {
    "harnessd [--workspace DIR] [--socket PATH] [--store PATH]\n\
     \x20        [--dialect glm|qwen] [--model ALIAS] [--endpoint HOST:PORT]\n\
     \x20        [--vocab GGUF] [--system FILE] [--effort low|medium|high|xhigh]\n\
     \x20        [--spill-inline BYTES] [--spill-dir DIR]\n\
     \x20        [--max-tool-rounds N] [--stall-rounds N] [--session ID] [--title NAME] [--prompt TEXT ...]\n\
     \n\
     what this session may do — every one of these is off unless you pass it:\n\
     \x20 --role NAME               orchestrator (default, read-only) | planner |\n\
     \x20                           researcher | coder (write+edit) | runner (exec)\n\
     \x20 --bash                    seat `bash` under --role runner. OFF even behind\n\
     \x20                           the role: the transcript choke point does not\n\
     \x20                           exist yet, so the boundary keeps secret bytes out\n\
     \x20                           of the VIEW and nothing stops a tool result\n\
     \x20                           carrying them into the transcript\n\
     \x20 --adjudicator console     who decides a gated call, and the default for any\n\
     \x20                           role that can reach the gate. Reads this daemon's\n\
     \x20                           own stdin, so it works in the foreground and an\n\
     \x20                           attached head cannot answer it (T25/D10). There is\n\
     \x20                           no `none`: a role with nobody to decide refuses to\n\
     \x20                           start, because --role orchestrator is the honest\n\
     \x20                           spelling of a session that cannot write\n\
     \x20 --intent-prose            also read the assistant's prose for commitments\n\
     \x20                           it did not act on. The tool-declared half is\n\
     \x20                           always on; this half has false positives\n\
     \n\
     store queries (no socket, no model):\n\
     \x20 --list-sessions [--tsv]   what is on disk: id, title, workspace, age, rows\n\
     \x20 --latest-session          the id of the newest one, scoped by --workspace\n\
     \x20 --rename ID TITLE         name a session, or clear it with an empty title\n\
     \x20 --delete ID               remove a session that has no rows"
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

    let mut query: Option<Query> = None;
    let mut tsv = false;
    // `--workspace` defaults to the process's cwd for a daemon, and to *nothing* for
    // a listing: "every session" and "every session under this directory" are
    // different questions, and defaulting the second one silently would make
    // `--list-sessions` hide rows without saying it had.
    let mut scope: Option<PathBuf> = None;

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut next = || it.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--list-sessions" => query = Some(Query::List),
            "--latest-session" => query = Some(Query::Latest),
            "--tsv" => tsv = true,
            "--rename" => {
                let id = next()?;
                let title = it.next().unwrap_or_default();
                query = Some(Query::Rename(id, title));
            }
            "--delete" => query = Some(Query::Delete(next()?)),
            "--scope" => scope = Some(PathBuf::from(next()?)),
            "--workspace" => cfg.workspace = PathBuf::from(next()?),
            "--socket" => cfg.socket = PathBuf::from(next()?),
            "--store" => cfg.store = Some(PathBuf::from(next()?)),
            "--session" => cfg.session_id = next()?,
            "--title" => cfg.title = next()?,
            "--model" => cfg.model = next()?,
            "--vocab" => cfg.vocab_gguf = PathBuf::from(next()?),
            "--effort" => cfg.effort = Some(next()?),
            // **The four flags that make anything reachable, and all four are
            // opt-in.** Nothing here changes what an invocation without them gets.
            "--role" => cfg.seat = Seat::parse(&next()?)?,
            // **The approval policy, separate from the role.** `Config::mode` has
            // existed and been disclosed at startup since roles and policy were split
            // -- its own doc says `--role coder` used to mean both -- but nothing ever
            // set it, so every session ran at the `always-ask` default and the other
            // three named points were unreachable.
            //
            // This is why `allow_session` did not stick: `always-ask` is
            // `GrantScope::Once`, so a session grant is never recorded. `writes allowed`
            // is `GrantScope::Session` and is what an operator who wants to approve
            // edits once per session is asking for.
            "--mode" => cfg.mode = letibot_tools::mode::Mode::parse(&next()?)?,
            // Bind one path read-only into the confined view. Repeatable.
            //
            // The boundary is hermetic: `$HOME` inside is a fresh tmpfs, so `~/.cargo`
            // and `~/.rustup` are absent and `cargo` cannot run. This is how they come
            // back, and it is a flag rather than a default because a grant is readable
            // into the transcript and that is the operator's call to make.
            "--grant-ro" => cfg.grants_ro.push(PathBuf::from(next()?)),
            "--bash" => cfg.allow_bash = true,
            "--adjudicator" => cfg.adjudicator = AdjudicatorChoice::parse(&next()?)?,
            "--intent-prose" => cfg.intent_prose = true,
            "--prompt" => prompts.push(next()?),
            "--max-tool-rounds" => {
                cfg.max_tool_rounds = next()?.parse().map_err(|e| format!("{arg}: {e}"))?
            }
            // The progress check's tolerance band. `0` turns it off, and the daemon
            // says so at startup — see `Config::disclosures`.
            "--stall-rounds" => {
                cfg.stall_rounds = next()?.parse().map_err(|e| format!("{arg}: {e}"))?
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

    // Before the vocabulary, before the socket, before the model. See the module
    // header for why that ordering is load-bearing rather than tidy.
    if let Some(q) = query {
        return run_query(&cfg, q, scope.as_deref(), tsv);
    }

    let parts = Parts::load(&cfg).map_err(|e| e.to_string())?;

    // One registry, seeded with the session named on the command line. A head can
    // make more over the socket; this one is the daemon's own, and it is opened
    // eagerly so that a dialect which does not fit the vocabulary is a startup
    // error rather than a failure on somebody's first prompt.
    let registry = Registry::new();
    // What is on disk, so a head's picker can show sessions from daemons that are no
    // longer running and `ResumeSession` can find them. A daemon with no `--store`
    // sets no source and lists only what it holds, which is what it always did.
    if let Some(src) = letibot_harnessd::sessions::StoreSessions::open(&cfg) {
        registry.set_source(src);
    }
    registry
        .create(
            cfg.session_id.clone(),
            cfg.title.clone(),
            Sessions::wiring(&cfg),
        )
        .map_err(|e| e.to_string())?;

    // Bind before opening the session: a socket that is already served means
    // another daemon owns it, and finding that out after loading a vocabulary
    // wastes the load.
    let mut daemon = Daemon::serve(registry.clone(), &cfg.socket).map_err(|e| e.to_string())?;
    daemon.catch_signals().map_err(|e| e.to_string())?;

    let socket = daemon.socket().display().to_string();
    let dialect = cfg.dialect.name();
    let model = cfg.model.clone();
    let endpoint = cfg.endpoint.authority();
    let workspace = cfg.workspace.display().to_string();
    let stored = stored_sessions(&cfg);
    let session_id = cfg.session_id.clone();
    let store_path = cfg.store.clone();

    let mut sessions = match Sessions::open_first(&parts, cfg, registry.clone()) {
        Ok(s) => s,
        Err(e) => {
            daemon.shutdown();
            return Err(e.to_string());
        }
    };
    let (prefix_tokens, ledger_head) = sessions
        .harness_of(&session_id)
        .map(|h| (h.tokens().len(), h.ledger_head()))
        .unwrap_or((0, String::new()));

    // After the session is open, not before: the adjudication line is read from the
    // backend, the gate and the seated schemas, and none of those exist until the
    // harness is built. A banner computed early is a banner describing a session
    // that had not been wired yet.
    let disclosures = sessions
        .harness_of(&session_id)
        .map(|h| h.config().disclosures(h.wiring()))
        .unwrap_or_default();

    eprintln!("harnessd: session {session_id}");
    eprintln!("  model    {model} via {endpoint} (/completion, token array)");
    eprintln!("  dialect  {dialect}");
    eprintln!("  workspace {workspace}");
    eprintln!("  socket   {socket}");
    eprintln!("  prefix   {prefix_tokens} tokens, head {ledger_head}");
    eprintln!("  sessions 1 open — a head can list them, switch, and make more");
    // What this session actually is: rebuilt from the store, or new. Printed as
    // numbers, because "resumed" on its own is the claim and the row count, the token
    // count and the chain head are the evidence.
    match sessions.resume_report(&session_id) {
        Some(r) => {
            eprintln!(
                "  RESUMED  {} — {} row(s), {} tokens, head {}",
                r.transcript_id,
                r.rows,
                r.tokens,
                &r.head[..16.min(r.head.len())]
            );
            for note in &r.notes {
                for line in wrap(note, term_cols().clamp(48, 100).saturating_sub(11)) {
                    eprintln!("           {line}");
                }
            }
        }
        None => eprintln!("  new session — nothing in the store to resume under this id"),
    }
    eprintln!();
    for line in banner(&disclosures, term_cols()) {
        eprintln!("{line}");
    }
    // Measured rather than assumed. A count that is silently absent reads as
    // "there is nothing there", which for a store that has been collecting
    // transcripts for weeks is the opposite of true.
    if let Some(n) = stored {
        for line in banner(&[resume_disclosure(n, store_path.as_deref())], term_cols())
            .into_iter()
            .skip(1)
        {
            eprintln!("{line}");
        }
    }
    eprintln!();

    let mut failed = 0;
    if !prompts.is_empty() {
        for p in &prompts {
            match sessions.submit(&session_id, p) {
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
    daemon.run(&mut sessions, |session, cmd, outcome| match outcome {
        // The session is named on every line: with more than one of them, "who did
        // what" is only half the question and the other half used to be unanswerable
        // from this log.
        Outcome::Replied(r) => eprintln!(
            "  {session} · {} -> {} round(s), {} tool call(s)",
            cmd.identity, r.rounds, r.tool_calls
        ),
        Outcome::Compacted(r) => eprintln!(
            "  {session} · {} -> compacted, base {} -> {} tokens (transcript {})",
            cmd.identity, r.was_tokens, r.base_tokens, r.transcript_id
        ),
        Outcome::Failed(e) => eprintln!("  {session} · {} -> {e}", cmd.identity),
        Outcome::Ignored => {}
    });
    daemon.shutdown();
    Ok(0)
}

/// How many sessions the store already holds, or `None` when there is no store.
///
/// A `SELECT` through `Store::connection`, which is the escape hatch that exists
/// for exactly this: the append-only guarantees are triggers, not a property of
/// whoever holds the connection, so a read here cannot weaken them.
fn stored_sessions(cfg: &Config) -> Option<u64> {
    let path = cfg.store.as_ref()?;
    let store = letibot_tokencore::store::Store::open(path).ok()?;
    store
        .connection()
        .query_row("SELECT COUNT(*) FROM session", [], |r| r.get::<_, i64>(0))
        .ok()
        .map(|n| n as u64)
}

/// The disclosure for what else is in the store.
///
/// **This used to say resume was impossible, and it was right at the time.** The
/// sentence it carried — that the transcript, the ledger rows and the token blobs
/// were all persisted, that `Store::load_transcript` plus `TokenLedger::restore`
/// would rebuild them, and that what was missing was a constructor for
/// `letibot_turn::Session` from a restored ledger — was an accurate description of a
/// hole. `letibot_turn::resume` is that constructor, and it takes the assistant rows'
/// tokens from `transcript_item.tokens` rather than re-rendering them, which is the
/// one thing the old note said must not happen.
///
/// So the count stays and the refusal goes. The count is the part that was never
/// decoration: a store that has been collecting transcripts for weeks and says
/// nothing reads as an empty one.
fn resume_disclosure(n: u64, store: Option<&std::path::Path>) -> Disclosure {
    Disclosure {
        subject: "resume".into(),
        state: format!("{n} ON DISK"),
        detail: format!(
            "every one of them can be brought back: `letibot --sessions` lists them, \
             `letibot --continue` reopens the newest in this workspace, and ctrl-s in \
             the head shows them alongside the live ones. A resumed session replays \
             the stored TOKENS — the assistant rows were cut from the ids the server \
             streamed and are never re-rendered — and the hash chain is verified twice \
             on the way in, so a session whose rows do not rebuild refuses by name \
             instead of continuing approximately.{}",
            store
                .map(|p| format!(" Store: {}.", p.display()))
                .unwrap_or_default()
        ),
        active: true,
    }
}

/// What a store query was asked for.
enum Query {
    List,
    Latest,
    Rename(String, String),
    Delete(String),
}

/// Answer a question about the store and exit. No socket, no vocabulary, no model.
fn run_query(
    cfg: &Config,
    q: Query,
    scope: Option<&std::path::Path>,
    tsv: bool,
) -> Result<i32, String> {
    let path = cfg
        .store
        .as_ref()
        .ok_or("no --store, and a question about stored sessions needs one")?;
    let store = letibot_tokencore::store::Store::open(path)
        .map_err(|e| format!("opening {}: {e}", path.display()))?;

    match q {
        Query::Rename(id, title) => {
            store
                .set_title(&id, &title)
                .map_err(|e| format!("renaming {id}: {e}"))?;
            if title.is_empty() {
                println!("{id} has no name now");
            } else {
                println!("{id} is now {title:?}");
            }
            // A daemon holding this session has its own copy of the name in the
            // registry and will not see this write. Said rather than papered over:
            // the rename is durable and the running head's header is stale, which is
            // a different thing from the rename not having happened.
            eprintln!(
                "note: a running daemon keeps its own copy of the name. Use /rename in \
                 the head to change it live."
            );
            Ok(0)
        }
        Query::Delete(id) => match store.delete_empty_session(&id) {
            Ok(()) => {
                println!("{id} deleted");
                Ok(0)
            }
            Err(e) => Err(e.to_string()),
        },
        Query::Latest => {
            let mut all = scoped(&store, scope)?;
            // An **exact** workspace match beats a descendant of it. Standing in
            // `~` and asking to continue should not hand back the conversation you
            // were having in `~/Projects/rano` while a `~` conversation exists; the
            // subtree match is the fallback that makes `--continue` work from a
            // subdirectory, not a licence to reach downwards past a closer answer.
            if let Some(root) = scope {
                all.sort_by_key(|s| std::path::Path::new(&s.workspace_root) != root);
            }
            // **A session with no rows is not a conversation to continue.** Two of
            // the five sessions in this box's store are exactly that: a daemon wrote
            // the row at startup and nobody ever prompted into it. They are the
            // *newest* rows in the table, so picking by time alone answers
            // `--continue` with an empty screen — which is indistinguishable from
            // resume being broken, and is how this would have been reported as still
            // not working.
            let skipped = all.iter().filter(|s| s.items == 0).count();
            let rows: Vec<_> = all.into_iter().filter(|s| s.items > 0).collect();
            if skipped > 0 {
                eprintln!(
                    "skipped {skipped} session(s) with no rows: nothing was ever said in them"
                );
            }
            match rows.first() {
                // Printed on stdout alone, so `$(harnessd --latest-session)` is the
                // id and nothing else. Which one it picked, and why, goes to stderr —
                // where the launcher can echo it and a pipeline ignores it.
                Some(s) => {
                    println!("{}", s.id);
                    eprintln!(
                        "{} — {} · {} row(s) · {}",
                        s.id,
                        s.title.clone().unwrap_or_else(|| "(unnamed)".into()),
                        s.items,
                        s.workspace_root
                    );
                    Ok(0)
                }
                None => {
                    eprintln!(
                        "no stored session{}",
                        scope
                            .map(|p| format!(" under {}", p.display()))
                            .unwrap_or_default()
                    );
                    Ok(1)
                }
            }
        }
        Query::List => {
            let rows = scoped(&store, scope)?;
            if tsv {
                // id, title, workspace, rows, last-activity-ms. Tab-separated and
                // unpadded: this is what `~/bin/letibot` reads, and a column layout
                // that a terminal width can change is not a data format.
                for s in &rows {
                    println!(
                        "{}\t{}\t{}\t{}\t{}",
                        s.id,
                        s.title.clone().unwrap_or_default(),
                        s.workspace_root,
                        s.items,
                        s.last_activity_ms
                    );
                }
                return Ok(0);
            }
            if rows.is_empty() {
                println!(
                    "no sessions{}",
                    scope
                        .map(|p| format!(" under {}", p.display()))
                        .unwrap_or_default()
                );
                return Ok(0);
            }
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            let wid = rows.iter().map(|s| s.id.len()).max().unwrap_or(8);
            println!(
                "{:<wid$}  {:>5}  {:>7}  TITLE / WORKSPACE",
                "ID", "ROWS", "AGE"
            );
            for s in &rows {
                println!(
                    "{:<wid$}  {:>5}  {:>7}  {}",
                    s.id,
                    s.items,
                    age(now - s.last_activity_ms),
                    s.title.clone().unwrap_or_else(|| "(unnamed)".into()),
                );
                // The same prefix width as the row above it — `wid + 2 + 5 + 2 + 7 + 2`
                // — so the workspace lines up under the title rather than three
                // columns past it.
                println!("{:<wid$}  {:>16}{}", "", "", s.workspace_root);
            }
            Ok(0)
        }
    }
}

/// Every stored session, optionally only those rooted at `scope`.
///
/// The scope test is a **path prefix**, not equality: a session opened in
/// `~/Projects/rano/crates` belongs to `~/Projects/rano` for the purpose of "what was
/// I doing here". Equality would make `--continue` miss the session you had open in
/// a subdirectory ten minutes ago and silently resume something older.
fn scoped(
    store: &letibot_tokencore::store::Store,
    scope: Option<&std::path::Path>,
) -> Result<Vec<letibot_tokencore::store::StoredSession>, String> {
    let all = store.list_sessions().map_err(|e| e.to_string())?;
    let Some(root) = scope else { return Ok(all) };
    Ok(all
        .into_iter()
        .filter(|s| std::path::Path::new(&s.workspace_root).starts_with(root))
        .collect())
}

/// A duration, in the largest unit that is still a small number.
fn age(ms: i64) -> String {
    let s = (ms / 1000).max(0);
    if s < 90 {
        return format!("{s}s");
    }
    let m = s / 60;
    if m < 90 {
        return format!("{m}m");
    }
    let h = m / 60;
    if h < 48 {
        return format!("{h}h");
    }
    format!("{}d", h / 24)
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
