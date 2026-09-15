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

/// Resolve the credentials and open the seat. Every refusal in here names what
/// was looked at, because "no flowy" and "the wrong flowy" must not read alike.
fn open_seat(f: &letibot_harnessd::config::FlowyConfig) -> Result<letibot_flowy::Seat, String> {
    let onboarding = letibot_flowy::Onboarding {
        addr: f.addr.clone(),
        agent: f.seat.clone(),
        token: None,
        token_file: f.token_file.clone(),
        config_dir: None,
        read_env: true,
    };
    let creds = letibot_flowy::creds::discover(&onboarding).map_err(|e| e.to_string())?;
    let seat = letibot_flowy::Seat::open(creds, None, None).map_err(|e| e.to_string())?;
    if f.new_reader {
        let r = seat.declare_reader().map_err(|e| format!("declaring the reader: {e}"))?;
        eprintln!("harnessd: declared inbox reader `{}` at cursor {}", r.reader, r.cursor);
    }
    Ok(seat)
}

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
     \x20 --oracle HOST:PORT        layer B: a llama.cpp /completion endpoint that\n\
     \x20                           answers `did the operator ask for this`. Its own\n\
     \x20                           flag, not --endpoint: the guard need not be the\n\
     \x20                           model doing the work. Required by\n\
     \x20                           --adjudicator model and by /mode supervised;\n\
     \x20                           both refuse by name without it\n\
     \x20 --supervise               start with the guard model consulted on every\n\
     \x20                           gated call: it answers, then you do, and both\n\
     \x20                           verdicts land on one corpus row. `/supervise`\n\
     \x20                           turns it on and off mid-session -- this is only\n\
     \x20                           the starting value. Needs --oracle\n\
     \x20 --oracle-budget-ms 400    how long the gate waits for that answer before\n\
     \x20                           giving up and failing closed. 400 was measured\n\
     \x20                           against a 4B on THIS box's CPU and says nothing\n\
     \x20                           about an oracle across the LAN -- measure before\n\
     \x20                           raising it, and know that every gated call blocks\n\
     \x20                           for up to this long\n\
     \x20 --intent-prose            also read the assistant's prose for commitments\n\
     \x20                           it did not act on. The tool-declared half is\n\
     \x20                           always on; this half has false positives\n\
     \x20 --flowy                   hold a flowy seat: messages for it wake the\n\
     \x20                           session as firings of the `flowy` monitor, and\n\
     \x20                           the `flowy` tool speaks as it. The seat comes\n\
     \x20                           from the usual path ($FLOWY_AGENT, else the only\n\
     \x20                           token under ~/.config/flowy/agents/) unless\n\
     \x20                           --flowy-seat NAME says which. Never the\n\
     \x20                           operator's own ~/.config/flowy/token\n\
     \x20 --flowy-seat NAME         which seat (implies --flowy)\n\
     \x20 --flowy-addr URL          the node, else $FLOWY_ADDR, else the seat's env\n\
     \x20                           file, else http://127.0.0.1:8787\n\
     \x20 --flowy-token-file PATH   the token, else the seat's file\n\
     \x20 --provider NAME           send the turns to a cloud provider instead of the\n\
     \x20                           local server: deepseek | glm | grok. --model names\n\
     \x20                           the provider's model (default: the preset's).\n\
     \x20                           The key: $DEEPSEEK_API_KEY / $ZHIPUAI_API_KEY /\n\
     \x20                           $XAI_API_KEY, --api-key, or [NAME] key= in\n\
     \x20                           ~/.config/letibot/providers.toml, which also\n\
     \x20                           prices models. METERED; the prefix check skips\n\
     \x20 --api-key KEY             the provider's key, for a one-off\n\
     \x20 --thinking                ask the provider to think out loud where it has\n\
     \x20                           a switch (GLM)\n\
     \x20 --flowy-new-reader        declare the inbox reader at the head of the log.\n\
     \x20                           Never implied: `no inbox reader` is also what a\n\
     \x20                           SWITCHED token says, and re-declaring there loses\n\
     \x20                           every message since the switch\n\
     \x20 --where host|firecode     where this session's tools run. firecode boots a\n\
     \x20                           VM on a COPY of the workspace; the VM is the\n\
     \x20                           boundary, the mode inside is allow-all, and the\n\
     \x20                           writes land in a sibling directory when it ends\n\
     \x20 --vm-arg ARG              passed to `firecode up` verbatim, repeatable:\n\
     \x20                           --vm-arg --mem --vm-arg 8192 --vm-arg --add-dir\n\
     \x20                           --vm-arg ~/.rustup (a toolchain the guest lacks)\n\
     \x20 --web-search brave        attach a provider behind `web_search`. Without\n\
     \x20                           it the tool refuses and says so\n\
     \x20 --brave-key KEY           for this run; else $BRAVE_API_KEY, else\n\
     \x20                           [brave] key= in ~/.config/letibot/providers.toml\n\
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
    // **The guard model comes from config, not from a flag.**
    //
    // `[gatekeeper] endpoint = "HOST:PORT"` in `~/.config/letibot/providers.toml`,
    // beside the provider keys, because an endpoint is a property of the box and not
    // of an invocation. `--oracle` still overrides it below, for a one-off. A
    // missing section, an unreadable file and a malformed address are all "no
    // guard" — this is read at startup and must never be a reason a daemon does not
    // start.
    {
        let gk = letibot_provider::gatekeeper(None);
        if let Some(ep) = gk.endpoint.as_deref().and_then(|s| Endpoint::parse(s).ok()) {
            cfg.oracle = Some(ep);
        }
        if let Some(ms) = gk.budget_ms {
            cfg.oracle_budget = std::time::Duration::from_millis(ms);
        }
    }
    let mut prompts: Vec<String> = Vec::new();
    // `--model` under `--provider` names the provider's model, not the local
    // alias; resolved after the flags, because either may come first.
    let mut model_given: Option<String> = None;

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
            "--model" => {
                let m = next()?;
                model_given = Some(m.clone());
                cfg.model = m;
            }
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
            // **Layer B's endpoint.** Its own, not `--endpoint`: the guard does not
            // have to be the model doing the work, and on this fleet it should not
            // be — a 4B on the CPU beside a 27B on a GPU, or a second box entirely.
            // One flag for both would make the guard follow every model change.
            // Start supervised. Equivalent to typing `/supervise` as the first thing
            // in the session, and there so a script does not have to.
                    "--supervise" => cfg.supervise = true,
            "--oracle" => {
                let v = next()?;
                let (h, p) = v.rsplit_once(':').ok_or("--oracle wants HOST:PORT")?;
                cfg.oracle = Some(Endpoint::new(
                    h,
                    p.parse().map_err(|e| format!("oracle port: {e}"))?,
                ));
            }
            // The budget `ModelAdjudicator` enforces. A knob because it decides which
            // models can hold the seat at all, and the answer is different for an
            // oracle on this box and one across the LAN: the default 400 ms was
            // measured against a local 4B on CPU and says nothing about a remote one.
            //
            // Raising it is a real decision with a cost — the gate blocks for this
            // long on every gated call — so it is typed rather than inferred from
            // whether the endpoint looks remote.
            "--oracle-budget-ms" => {
                let v = next()?;
                cfg.oracle_budget = std::time::Duration::from_millis(
                    v.parse().map_err(|e| format!("--oracle-budget-ms: {e}"))?,
                );
            }
            "--intent-prose" => cfg.intent_prose = true,
            "--provider" => {
                cfg.provider.get_or_insert_with(Default::default).name = next()?;
            }
            "--api-key" => {
                cfg.provider.get_or_insert_with(Default::default).api_key = Some(next()?);
            }
            "--thinking" => {
                cfg.provider.get_or_insert_with(Default::default).thinking = true;
            }
            "--flowy" => {
                cfg.flowy.get_or_insert_with(Default::default);
            }
            "--flowy-seat" => cfg.flowy.get_or_insert_with(Default::default).seat = Some(next()?),
            "--flowy-addr" => cfg.flowy.get_or_insert_with(Default::default).addr = Some(next()?),
            "--flowy-token-file" => {
                cfg.flowy.get_or_insert_with(Default::default).token_file =
                    Some(PathBuf::from(next()?));
            }
            "--flowy-new-reader" => {
                cfg.flowy.get_or_insert_with(Default::default).new_reader = true;
            }
            // Where THIS session's tools run. `firecode` boots a VM on a copy of
            // the workspace, the VM is the boundary and the mode inside is
            // allow-all — the same placement a subagent gets from `where`.
            "--where" => {
                cfg.placement = match next()?.as_str() {
                    "host" => letibot_tools::builtins::task::Placement::Host,
                    "firecode" | "vm" => letibot_tools::builtins::task::Placement::Firecode,
                    other => return Err(format!("--where {other}: host or firecode")),
                }
            }
            // Passed to `firecode up` verbatim, repeatable: `--vm-arg --mem --vm-arg 8192`.
            "--vm-arg" => cfg.vm_args.push(next()?),
            // What is behind `web_search`. Without it the tool refuses and names
            // this flag, which is the state every session has had until now.
            "--web-search" => {
                cfg.web_search = match next()?.as_str() {
                    // `none` is how an operator with a key in providers.toml
                    // turns the tool off for one daemon without deleting it.
                    "none" | "off" => None,
                    other => Some(other.to_string()),
                }
            }
            // Held by the websearch crate, not by `Config` — which derives
            // `Debug`, and a secret in a struct that can be `{:?}`-printed is a
            // secret one `eprintln!` away from a log.
            "--brave-key" => letibot_websearch::set_flag_key(next()?),
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

    if let Some(p) = cfg.provider.as_mut() {
        if p.name.is_empty() {
            return Err("--api-key / --thinking need --provider NAME (deepseek | glm | grok)".into());
        }
        p.model = model_given.clone();
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

    // The seat, before the first session: a `--flowy` that cannot be honoured is
    // a startup error, not a session that quietly hears nothing. Opening takes
    // the local waiter claim and the spool; the node is not touched until the
    // loop starts, so a node that is away right now is a stall the banner
    // reports rather than a refusal to start.
    let seat = match &cfg.flowy {
        None => None,
        Some(f) => match open_seat(f) {
            Ok(s) => Some(s),
            Err(e) => {
                daemon.shutdown();
                return Err(format!("--flowy: {e}"));
            }
        },
    };

    let mut sessions =
        match Sessions::open_first_with_seat(&parts, cfg, registry.clone(), seat.clone()) {
            Ok(s) => s,
            Err(e) => {
                daemon.shutdown();
                return Err(e.to_string());
            }
        };
    if let Some(seat) = &seat {
        seat.start();
        // The fabric's skills, through the same `skill` tool as the disk's. One
        // shelf per daemon, because one seat per daemon.
        parts
            .skills
            .set_shelf(std::sync::Arc::new(letibot_flowy::FabricShelf::new(seat.clone())));
    }
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
    if let Some(seat) = &seat {
        // From the seat, not from the config: what it is, where the credential came
        // from, and where the reader stands — a reading, not a claim.
        eprintln!("  flowy    {}", seat.credentials().describe());
        match seat.reader() {
            Ok(Some(r)) => eprintln!(
                "           reader `{}` at cursor {} — listening as this process (pid {})",
                r.reader,
                r.cursor,
                std::process::id()
            ),
            Ok(None) => eprintln!(
                "           reader `{}` is NOT DECLARED on the node; the listener will stop \
                 on its first poll. Pass --flowy-new-reader if this seat has never \
                 listened — and read the refusal first if it has",
                seat.name()
            ),
            Err(e) => eprintln!("           node not answering yet ({e}); the listener will keep trying"),
        }
    }
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
        None => {
            eprintln!("  new session — nothing in the store to resume under this id");
            for note in sessions.open_notes(&session_id) {
                for line in wrap(&note, term_cols().clamp(48, 100).saturating_sub(11)) {
                    eprintln!("           {line}");
                }
            }
        }
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
                    // Under a metered provider there is no f_keep to read — the
                    // cache figures are the provider's — so the footer says what it
                    // does have: tokens and, when the model is priced, the cost.
                    let tail = if keeps.is_empty() {
                        let prompt: u64 = reply.metrics.iter().map(|m| m.prompt_tokens).sum();
                        let cached: u64 = reply.metrics.iter().map(|m| m.cached_tokens).sum();
                        let out: u64 = reply.metrics.iter().map(|m| m.predicted_tokens).sum();
                        let micros: Option<u64> = reply
                            .metrics
                            .iter()
                            .map(|m| m.cost.micros_usd)
                            .try_fold(0u64, |acc, m| m.map(|m| acc + m));
                        format!(
                            "{prompt} prompt tokens ({cached} cached), {out} out, cost {}",
                            match micros {
                                Some(m) => format!("${:.6}", m as f64 / 1_000_000.0),
                                None => "unpriced".into(),
                            }
                        )
                    } else {
                        format!(
                            "f_keep {}",
                            keeps.iter().map(|k| format!("{k:.4}")).collect::<Vec<_>>().join(" ")
                        )
                    };
                    eprintln!("  [{} round(s), {} tool call(s), {tail}]", reply.rounds, reply.tool_calls);
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
        if let Some(seat) = &seat {
            seat.stop();
        }
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
            "  {session} · {} -> compacted, base {} -> {} tokens (transcript {}); \
             summary turn reused {} of {} carryable tokens",
            cmd.identity,
            r.fork.was_tokens,
            r.fork.base_tokens,
            r.fork.transcript_id,
            r.summary_turn.cached_tokens,
            r.summary_turn.reusable
        ),
        Outcome::Failed(e) => eprintln!("  {session} · {} -> {e}", cmd.identity),
        Outcome::Ignored => {}
    });
    daemon.shutdown();
    // The seat after the daemon: the listener's next poll window sees the stop and
    // the waiter claim is released with it. `std::process::exit` above runs no
    // destructors, so this is said here rather than left to a drop.
    if let Some(seat) = &seat {
        seat.stop();
    }
    drop(sessions);
    drop(seat);
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
            // Subagents are children of a session, not sessions a picker lists —
            // they live in the subagent tree, not the flat list.
            let rows: Vec<_> = scoped(&store, scope)?
                .into_iter()
                .filter(|s| s.parent_session_id.is_none())
                .collect();
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
