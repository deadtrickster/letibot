//! The daemon's command line, as a function rather than a `main`.
//!
//! # Why this is the cleanest of the five
//!
//! `harnessd`'s binary already had its body in `fn run() -> Result<i32, String>` with a
//! four-line `main` dispatching on it — so the move is a lift rather than an untangling.
//! What changed is only what being a library forces: the arguments arrive as a
//! parameter instead of `std::env::args()`, and the caller decides the process's fate
//! from the returned `Result`. The `harnessd: {e}` prefix a person sees is unchanged,
//! because it is the ROLE's name and this is still the daemon role.
//!
//! # What was checked
//!
//! Five non-daemon paths, captured from the binary before the move and compared after:
//! `--help` 0, an unknown argument 1, a store that cannot be opened 1, `--rename` with
//! no value 1, and `--latest-session` against the same bad store 1. The paths that
//! START a daemon are deliberately not in that list — a test that launches one is a
//! test that leaves one running.

//! `harnessd` — the daemon.
//!
//! ```text
//! harnessd [--workspace DIR] [--socket PATH] [--store PATH]
//!          [--dialect glm|qwen] [--model ALIAS] [--endpoint HOST:PORT]
//!          [--vocab GGUF] [--system TEXT] [--system-file FILE]
//!          [--effort low|medium|high|xhigh]
//!          [--spill-inline BYTES] [--spill-dir DIR]
//!          [--max-tool-rounds N|off] [--stall-rounds N] [--session ID] [--title NAME]
//!          [--prompt TEXT ...]        run these, print the answers, exit
//!          [--slash VERB ...]         a head's slash verb, in order with --prompt:
//!                                     `--prompt hi --slash "models deepseek" --prompt again`
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

use crate::config::{AdjudicatorChoice, Config, Disclosure, Seat, SpillPolicy, SpillStorage};
use crate::{Daemon, Dialect, Outcome, Parts, Sessions};
use letibot_sessionlog::registry::Registry;
use letibot_turn::Endpoint;

/// Resolve the credentials and open the seat. Every refusal in here names what
/// was looked at, because "no flowy" and "the wrong flowy" must not read alike.
fn open_seat(f: &crate::config::FlowyConfig) -> Result<letibot_flowy::Seat, String> {
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
        let r = seat
            .declare_reader()
            .map_err(|e| format!("declaring the reader: {e}"))?;
        eprintln!(
            "harnessd: declared inbox reader `{}` at cursor {}",
            r.reader, r.cursor
        );
    }
    Ok(seat)
}

/// **Apply a `main_model` from the project file to the session's config.**
///
/// The project file's `main_model` is a model name, the same shape the operator
/// types for `--model` and the names `/models` offers: a `provider/model` for a
/// metered session, a bare alias for a local one. This is the one place that
/// turns that name into the `cfg` fields the session runs on, and it is the
/// project-file half of the precedence — called only when the command line did
/// not name a model, so a `main_model` the operator typed is never overridden by
/// one the project set.
///
/// A `provider/model` name sets the metered provider, and a bare alias sets the
/// local model and clears the provider, because the two are the two ways a
/// session's turns go and a name is one or the other, not both. The provider's
/// key is not set here: it is resolved along the usual path (`$PROVIDER_API_KEY`,
/// then `providers.toml`), and a project file that names a model but not a key is
/// a model the operator has a key for, not a secret the project carries.
fn apply_main_model(cfg: &mut Config, model: &str, bound: Option<&str>) -> Option<String> {
    if let Some((provider, m)) = model.split_once('/') {
        cfg.provider = Some(crate::config::ProviderConfig {
            name: provider.to_string(),
            model: Some(m.to_string()),
            api_key: None,
            thinking: false,
        });
    } else {
        // `bound` is unused at this commit: the file levels are not yet wired to
        // `cli_binding`, and this keeps the pre-level behaviour exactly.
        let _ = bound;
        cfg.model = model.to_string();
        cfg.provider = None;
    }
    None
}

fn usage() -> String {
    "harnessd [--workspace DIR] [--socket PATH] [--store PATH]\n\
     \x20        [--dialect glm|qwen] [--model ALIAS] [--endpoint HOST:PORT]\n\
     \x20        [--vocab GGUF] [--system TEXT|--system-file FILE]\n\
     \x20        [--effort low|medium|high|xhigh]\n\
     \x20        [--spill-inline BYTES] [--spill-dir DIR]\n\
     \x20        [--max-tool-rounds N|off] [--stall-rounds N] [--session ID] [--title NAME] [--prompt TEXT ...] [--slash VERB ...]\n\
     \n\
     what this session may do — every one of these is off unless you pass it:\n\
     \x20 --role NAME               orchestrator (default, read-only) | planner |\n\
     \x20                           researcher | coder (write+edit) | runner (exec)\n\
     \x20 --bash                    seat `bash` under --role runner. OFF even behind\n\
     \x20                           the role: the transcript choke point does not\n\
     \x20                           exist yet, so the boundary keeps secret bytes out\n\
     \x20                           of the VIEW and nothing stops a tool result\n\
     \x20                           carrying them into the transcript\n\
     \x20 --max-subagent-depth 3    how deep a subagent tree may go, in levels below\n\
     \x20                           the root. `task` is seated on every subagent, so\n\
     \x20                           a call past this depth is refused BY NAME rather\n\
     \x20                           than withdrawn -- a capability that exists but is\n\
     \x20                           hidden manufactures the workaround. 0 refuses\n\
     \x20                           every `task` call\n\
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
     \x20 --oracle-question verdict what the guard is asked: `verdict` (ALLOW/DENY/\n\
     \x20                           UNSURE) or `scores` (FIT and CLAIM 0-10, the\n\
     \x20                           thresholds derive the verdict; TraceGuard §4)\n\
     \x20 --oracle-max-tokens 120   how many output tokens the guard may spend on its\n\
     \x20                           answer. A reply stopped at this ceiling before its\n\
     \x20                           verdict is reported as *ran out of room* and NOT as\n\
     \x20                           an unreadable answer -- it is the one unsure whose\n\
     \x20                           response is this number\n\
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
     \x20                           local server: deepseek | glm | glm-coding |\n\
     \x20                           glm-coding-cn | grok. --model names\n\
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
     \x20 --web-fetch             attach `curl` egress behind `web_fetch`: the page\n\
     \x20                           is reader-mode extracted and rendered as\n\
     \x20                           markdown before it reaches the model\n\
     \x20 --no-project-config      ignore the project's own `leticode.toml`: its\n\
     \x20                           `main_model`, `subagent_model`,\n\
     \x20                           `gatekeeper_model` and `[roles]` overrides are\n\
     \x20                           not read, and the session runs on the daemon's\n\
     \x20                           own. Otherwise the nearest file at or above\n\
     \x20                           --workspace is found by walking up, read, and\n\
     \x20                           disclosed at startup; a file that does not parse\n\
     \x20                           is reported and the session still starts\n\
     \n\
     store queries (no socket, no model):\n\
     \x20 --list-sessions [--tsv]   what is on disk: id, title, workspace, age, rows\n\
     \x20 --latest-session          the id of the newest one, scoped by --workspace\n\
     \x20 --rename ID TITLE         name a session, or clear it with an empty title\n\
     \x20 --delete ID               remove a session that has no rows"
        .into()
}

/// One scripted step, in the order the flags were given.
enum Step {
    Prompt(String),
    Slash(String),
}

pub fn run(args: &[String]) -> Result<i32, String> {
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
        // The guard's own model, when it is not the one doing the work. Read here
        // beside the endpoint because they describe the same server, and a file that
        // names a model nobody reads is a setting the operator will reasonably
        // believe took effect.
        if let Some(m) = gk.model.filter(|m| !m.trim().is_empty()) {
            cfg.oracle_model = Some(m);
        }
        // **The authority the operator declares for their own guard.** Refused by
        // name rather than narrowed silently: a `[gatekeeper] intents` line with a
        // typo in it would otherwise leave the guard at the floor while the
        // operator believes they widened it, which is the shape of every other
        // "the banner says one thing and the session does another" defect this
        // tree refuses.
        if !gk.intents.is_empty() || gk.max_scope.is_some() || !gk.tools.is_empty() {
            match letibot_tools::authorise::OracleScope::declared(
                &gk.intents,
                gk.max_scope.as_deref(),
                &gk.tools,
            ) {
                Ok(scope) => cfg.oracle_scope = Some(scope),
                Err(why) => {
                    return Err(format!(
                        "[gatekeeper] in {}: {why}",
                        letibot_provider::keys::config_file().display()
                    ));
                }
            }
        }
        if let Some(ms) = gk.budget_ms {
            cfg.oracle_budget = std::time::Duration::from_millis(ms);
        }
    }
    // **Prompts and slash verbs in one ordered list.** `--prompt` could run turns
    // headlessly and nothing could change the session between them, so the one
    // thing worth testing about a mid-session model switch — that the turn AFTER
    // it works — could only be done by hand in a terminal. The operator: *"check
    // deepseek <-> qwen switch actually works midsession yourself. if there is not
    // enough tools in the letibot-tui to drive it programmatically - add"*.
    //
    // Order is the whole point, so they share a list rather than being two.
    let mut steps: Vec<Step> = Vec::new();
    // `--model` under `--provider` names the provider's model, not the local
    // alias; resolved after the flags, because either may come first.
    let mut model_given: Option<String> = None;
    // **Whether the command line named the main model.** `--model` or `--provider`
    // is the CLI half of the project file's precedence: command line beats the
    // project file, so a `main_model` the operator typed must not be overridden by
    // one the project set. Tracked rather than read off `cfg`, because `cfg.model`
    // and `cfg.provider` are also set by the user config (`[default]`), and the two
    // must not be told apart by a field they share.
    let mut cli_main_model = false;
    // **The explicit "ignore the project file".** See the flag's own note.
    let mut no_project_config = false;

    let mut query: Option<Query> = None;
    let mut tsv = false;
    // `--workspace` defaults to the process's cwd for a daemon, and to *nothing* for
    // a listing: "every session" and "every session under this directory" are
    // different questions, and defaulting the second one silently would make
    // `--list-sessions` hide rows without saying it had.
    let mut scope: Option<PathBuf> = None;

    let mut it = args.iter().cloned();
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
            // Replay the operator-answered corpus against the guard and print
            // what it earns. `--calibrate-write` also records it where the
            // daemon reads it at startup. See `crate::calibrate`.
            // Re-render any stable prefix stored with no tokens, so a transcript
            // hanging off one can be resumed again. See `Query::RepairPrefixes`.
            "--repair-prefixes" => query = Some(Query::RepairPrefixes),
            // Run every arm over the same rows and print them side by side. A
            // prompt change to this seam is a hypothesis until this says otherwise.
            "--compare" => query = Some(Query::Compare),
            // Print the bytes the guard is actually handed for a call it could not
            // answer. "Look at what it is shown" — the only way to tell a prompt
            // problem from a evidence problem.
            "--show-brief" => query = Some(Query::ShowBrief),
            // Recover shapes for decisions the operator already made, so the shape
            // cache does not start cold on a store full of their own approvals.
            // Re-derived with today's classifier, never reinstated from the row.
            // Cross every command in the etalon with layer A as it is now: what
            // would prompt, what would be caught, what cannot be read. Plan §2.
            "--etalon" => query = Some(Query::Etalon(PathBuf::from(next()?))),
            // The guard itself over an etalon-shaped JSONL — an attack set, with
            // the main task as the operator's ask. Plan §1 (BashArena). Needs
            // --oracle. Rows are seated in a scratch copy of the store.
            "--etalon-oracle" => query = Some(Query::EtalonOracle(PathBuf::from(next()?))),
            // Every command's tree-sitter shape, aggregated and laid out on a
            // self-organising map; writes the JSON the map page draws. Plan §7.
            "--etalon-map" => {
                let file = PathBuf::from(next()?);
                let out = PathBuf::from(next()?);
                query = Some(Query::EtalonMap(file, out));
            }
            "--backfill-shapes" => query = Some(Query::Backfill { write: false }),
            "--backfill-shapes-write" => query = Some(Query::Backfill { write: true }),
            "--calibrate" => query = Some(Query::Calibrate { write: false }),
            "--calibrate-write" => query = Some(Query::Calibrate { write: true }),
            // **An empty scope is a missing argument, not "everywhere".**
            // `Path::new("/anything").starts_with("")` is true in Rust, so an
            // empty `--scope` matched every session in the store and `--continue`
            // handed back the newest one on the box — the operator's report:
            // *"--continue seems to be continuing the last global, not the last in
            // project"*. The launcher computes the scope from `git rev-parse
            // --show-toplevel` with `$PWD` as the fallback, and both can come back
            // empty if the directory is gone (a pruned worktree does it).
            //
            // Refused by name rather than defaulted, because both defaults are
            // wrong: "everywhere" continues somebody else's conversation, and
            // "nowhere" reads as resume being broken.
            "--scope" => {
                let v = next()?;
                if v.trim().is_empty() {
                    return Err("--scope was given an empty path. It scopes a store query \
                                to one workspace, and an empty one would match every \
                                session on this box — which is how `--continue` would \
                                reopen a conversation from another project. Pass a \
                                directory, or leave --scope off to search the whole store \
                                deliberately."
                        .into());
                }
                scope = Some(PathBuf::from(v));
            }
            "--workspace" => cfg.workspace = PathBuf::from(next()?),
            "--socket" => cfg.socket = PathBuf::from(next()?),
            "--store" => cfg.store = Some(PathBuf::from(next()?)),
            "--session" => cfg.session_id = next()?,
            "--title" => cfg.title = next()?,
            "--model" => {
                let m = next()?;
                model_given = Some(m.clone());
                cfg.model = m;
                cli_main_model = true;
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
            // **How deep a subagent tree may go.** Zero refuses every `task` call by
            // name; the default is the operator's ruling (3). A flag rather than a
            // hidden constant because it bounds a whole tree and is the one number an
            // operator reaches for when a run should not fan out.
            "--max-subagent-depth" => {
                cfg.max_subagent_depth = next()?
                    .parse()
                    .map_err(|e| format!("--max-subagent-depth: {e}"))?
            }
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
            // What the guard is asked: `verdict` (its own ALLOW/DENY/UNSURE) or
            // `scores` (FIT and CLAIM 0-10, TraceGuard arXiv 2604.03968 §4; the
            // thresholds derive the verdict). Plan §3; `--compare` measures both.
            // **The guard's output ceiling** (R12). One of the four readings of an unsure
            // answer is a *budget*: a reply stopped at `max_tokens` before it reached a
            // verdict. That is the one whose response is a knob rather than a person, and
            // this is the knob.
            "--oracle-max-tokens" => {
                let v = next()?;
                cfg.oracle_max_tokens =
                    Some(v.parse().map_err(|e| format!("--oracle-max-tokens: {e}"))?);
            }
            "--oracle-question" => {
                let v = next()?;
                cfg.oracle_question = crate::oracle::Question::parse(&v)
                    .ok_or_else(|| format!("--oracle-question: `{v}` is not verdict or scores"))?;
            }
            "--intent-prose" => cfg.intent_prose = true,
            "--provider" => {
                cfg.provider.get_or_insert_with(Default::default).name = next()?;
                cli_main_model = true;
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
            // The wall, when the operator knows it and the server will not say —
            // a metered provider, or a proxy that does not serve /props.
            "--context-window" => {
                cfg.context_window = Some(
                    next()?
                        .parse()
                        .map_err(|e| format!("--context-window: {e}"))?,
                )
            }
            "--no-auto-compact" => cfg.auto_compact = false,
            // **The explicit "ignore the project file".** The walk-up finds the
            // nearest `leticode.toml` at or above the workspace, and this is how an
            // operator says "not for this daemon" without deleting the file: a
            // one-off against a project whose models do not apply, or a daemon that
            // should run on its own standing choice. Off by default — the file is
            // found and read unless this says otherwise, the way `.env` is read
            // unless `--no-env` says otherwise.
            "--no-project-config" => no_project_config = true,
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
            // What is behind `web_fetch`: the curl subprocess the webfetch
            // crate wraps. No key and no account — the flag is the whole
            // opt-in, and the tool's refusal names it.
            "--web-fetch" => cfg.web_fetch = true,
            "--prompt" => steps.push(Step::Prompt(next()?)),
            // The slash verbs a head can send, from a script: `--slash "models
            // deepseek"`, `--slash compact`, `--slash tools`. Written without the
            // leading `/`, like the wire carries them.
            "--slash" => steps.push(Step::Slash(next()?)),
            // The round backstop, which is unbounded by default. `off`, `none`
            // and `0` all say so, because somebody turning a limit off types the
            // word before they think to type the number.
            "--max-tool-rounds" => {
                let v = next()?;
                cfg.max_tool_rounds = match v.as_str() {
                    "off" | "none" | "unlimited" | "infinity" | "inf" => 0,
                    other => other.parse().map_err(|e| format!("{arg}: {e}"))?,
                }
            }
            // How many times a round is taken again when the endpoint fails. `1`
            // means "do not retry", for an endpoint known not to be there.
            "--http-retries" => {
                cfg.http_retries = next()?.parse().map_err(|e| format!("{arg}: {e}"))?
            }
            // The progress check's tolerance band. `0` turns it off, and the daemon
            // says so at startup — see `Config::disclosures`.
            "--stall-rounds" => {
                cfg.stall_rounds = next()?.parse().map_err(|e| format!("{arg}: {e}"))?
            }
            "--spill-inline" => {
                cfg.spill = SpillPolicy::Inline(next()?.parse().map_err(|e| format!("{arg}: {e}"))?)
            }
            "--spill-dir" => cfg.spill_storage = SpillStorage::Dir(PathBuf::from(next()?)),
            "--dialect" => {
                let n = next()?;
                cfg.dialect = Dialect::parse(&n).ok_or_else(|| format!("unknown dialect {n:?}"))?;
            }
            "--endpoint" => {
                let v = next()?;
                let (h, p) = v.rsplit_once(':').ok_or("--endpoint wants HOST:PORT")?;
                cfg.endpoint = Endpoint::new(h, p.parse().map_err(|e| format!("port: {e}"))?);
            }
            // **`--system` is the prompt; `--system-file` is where to read one
            // from.** It used to be the file alone, undocumented as such beyond
            // one word in the usage line, and the operator hit it the obvious
            // way: *"wtf is --system FILE? I also want a string"*.
            //
            // A value that names an existing file is REFUSED rather than guessed
            // at. Taking a path as the prompt silently would make a scripted
            // `--system prompt.txt` start a session whose entire system prompt is
            // the ten characters `prompt.txt` — the failure nobody checks for,
            // because the daemon starts and answers.
            "--system" => {
                let v = next()?;
                if std::path::Path::new(&v).is_file() {
                    return Err(format!(
                        "`--system` takes the prompt ITSELF, and {v:?} is a file that \
                         exists. If you meant its contents: --system-file {v}. If you \
                         really meant that text, it cannot be told apart from the path, \
                         so say it a different way."
                    ));
                }
                cfg.system = v;
            }
            // An HTTP head beside the socket, for a daemon whose job is to answer
            // prompts rather than to hold a conversation — the guard being the
            // case it was built for. See `crate::httphead`.
            "--http" => {
                cfg.http = Some(next()?);
            }
            "--system-file" => {
                let path = next()?;
                cfg.system = std::fs::read_to_string(&path)
                    .map_err(|e| format!("--system-file {path}: {e}"))?;
            }
            "-h" | "--help" => {
                println!("{}", usage());
                return Ok(0);
            }
            // **The version, because an INSTALLER has to be able to prove the binary runs.**
            //
            // The one-line installer copies a binary it downloaded and then says what it got —
            // `rano`'s own installer: *"a binary that cannot run is a failure this script would
            // otherwise report as success"* — and the cheapest way to prove a binary runs is to ask
            // it something it can answer without a socket, a model or a store. `--help` would do it
            // and prints forty lines; this prints one.
            //
            // **`CARGO_PKG_VERSION` is the WORKSPACE's version**, which is the number the release
            // tag has to agree with — see the root manifest. So a stale binary is visible from the
            // same command the installer uses to check it landed.
            "-V" | "--version" => {
                println!("harnessd {}", env!("CARGO_PKG_VERSION"));
                return Ok(0);
            }
            other => return Err(format!("unknown argument {other}\n\n{}", usage())),
        }
    }

    if let Some(p) = cfg.provider.as_mut() {
        if p.name.is_empty() {
            return Err(
                "--api-key / --thinking need --provider NAME (deepseek | glm | glm-coding | \
                 glm-coding-cn | grok)"
                    .into(),
            );
        }
        p.model = model_given.clone();
    }

    // Before the vocabulary, before the socket, before the model. See the module
    // header for why that ordering is load-bearing rather than tidy.
    if let Some(q) = query {
        return run_query(&cfg, q, scope.as_deref(), tsv);
    }

    // **What a corpus replay measured**, written beside the store by
    // `--calibrate-write`. Read here, after `--store` is known, and
    // absent/malformed is silently the declared scope, the same fail-open a
    // startup read must have.
    if let Some(earned) = crate::calibrate::read_calibration(&cfg) {
        // **A measurement ADDS to a declaration; it never shrinks one.**
        //
        // Replacing outright was wrong, and the way it was wrong is the way that
        // matters: a replay recommends only what it has rows for, so on this box it
        // earned five intents and a reach of `host_project` — while the operator
        // had set `host_other` in providers.toml, which is what their work outside the project
        // needs. Taking the earned scope whole would have widened the intents and
        // silently revoked the reach, in the name of evidence that never said
        // anything about it.
        //
        // Both halves are the operator's own authority: one they typed, one
        // measured from calls they answered themselves. Neither gets to quietly
        // undo the other, so the intents are the union and the reach is the further
        // of the two, and the evidence says it is both.
        cfg.oracle_scope = Some(match cfg.oracle_scope.take() {
            None => earned,
            Some(declared) => letibot_tools::authorise::OracleScope::earned(
                declared.intents.union(&earned.intents).copied().collect(),
                declared.max_scope.max(earned.max_scope),
                format!(
                    "{} — combined with what the operator declared in providers.toml,                      which a measurement adds to and never shrinks",
                    earned.evidence
                ),
            ),
        });
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
    // **The operator's standing choice, resolved once, here.** Explicit
    // `--provider` wins; else `[default]` in providers.toml, which
    // `/default-model` writes; else the local server this daemon was launched
    // against. Resolved in the binary because it reads the operator's own config
    // file, and a `Harness` that reached for that file made every test inherit it.
    // The Err is said, not swallowed: a daemon that comes up local while the file
    // says deepseek is a fault the operator cannot find from the outside. The line
    // follows the one prompts.toml already gets — the file, the parser's own
    // message, and what happens instead.
    if cfg.provider.is_none() {
        match letibot_provider::keys::default_choice(None) {
            Ok(Some(d)) => {
                cfg.provider = Some(crate::config::ProviderConfig {
                    name: d.provider,
                    model: d.model,
                    api_key: None,
                    thinking: false,
                });
            }
            Ok(None) => {}
            Err(u) => eprintln!(
                "  {u} — the standing choice could not be read, so new sessions \
                 start on the local server this daemon was launched against"
            ),
        }
    }
    if let Some(src) = crate::sessions::StoreSessions::open(&cfg) {
        // **One store, two questions** (R19.2b): the same instance lists sessions the
        // registry is not holding, and answers `FetchRow` for a row its bounded view has
        // trimmed. Both are set here because this is the one place that knows there is a
        // store at all.
        // **One store, three questions** now: the same instance lists sessions, answers
        // `FetchRow`, and serves R11's locator for the oracle's brief and reply.
        registry.set_diagnostic_source(src.clone());
        registry.set_row_source(src.clone());
        registry.set_source(src);
    }
    // **The smart `!`'s model half, attached whenever there is a local model to ask.**
    //
    // The endpoint is `cfg.oracle` — the `[gatekeeper]` endpoint read at startup, the
    // LOCAL model — and the suggester is installed on exactly that condition, the same
    // one the guard is attached on. A daemon with no local endpoint installs no
    // suggester, and a `SuggestShell` then answers with an empty list: a suggestion
    // must not cost money per keystroke, so the fallback is *nothing*, never a metered
    // provider.
    //
    // **The name is the operator's `[gatekeeper] model` when they named one, and
    // `local` otherwise — deliberately NOT the session's model.** The guard falls back
    // to `cfg.model` (`harness.rs`), and it can afford to: its own body hardcodes
    // `"model": "guard"` on the wire, so the name it was constructed with is only ever
    // read by `describe`. This one puts its name in the request, and a name is a thing a
    // proxy in front of the endpoint may ROUTE BY — an operator whose session model is a
    // metered provider's and whose local box sits behind such a proxy would be billed per
    // keystroke by exactly the line that exists to prevent it.
    if let Some(ep) = cfg.oracle.clone() {
        let model = cfg
            .oracle_model
            .clone()
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(|| "local".to_string());
        registry.set_suggester(std::sync::Arc::new(crate::suggest::LocalSuggester::new(
            ep, model,
        )));
    }
    // **The pane's pty, owned by this daemon.** (`!term`.)
    //
    // Installed unconditionally, and **deliberately not behind a capability check**: unlike
    // the suggester, a pane needs no model, no endpoint and no configuration — it needs a
    // pty, which is either there or is not, and a box without one says so in the pane's own
    // ending rather than at startup. A driver that is absent is a `!term` that answers *no
    // pty here*; a driver that is present and cannot open one answers the same sentence a
    // moment later, and the difference is a `libc` call.
    //
    // `Weak`, because the registry stores this driver and the driver reads the registry's
    // wiring for a session's workspace — see `Terminals`' own note.
    registry.set_terminal(std::sync::Arc::new(crate::term::Terminals::new(
        std::sync::Arc::downgrade(&registry),
    )));
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

    // **The merge queue, before the worker loop takes this thread.** Its own thread, its own
    // connection to the session store, and the gate CI runs as its check — see `mergequeue`'s
    // module docs for why each of those is a thread of its own. It is inert until an entry
    // exists, and the entry comes from `task_start`'s child finishing (see
    // `HarnessTaskRunner::finished`); a daemon that is not in a git repo (or has no `--store`)
    // is told it has no queue rather than pretending to serve one.
    let merge_stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    // **The door the queue reviews through, and the one thing about it that is NOT built.**
    //
    // `SessionReviewer` is the daemon's half of a wake: it writes the request into the session
    // store and rings the bell that starts the reviewer's turn. What it does NOT do is start a
    // reviewer, because there is no reviewer session to start — and the reason is a capability
    // question rather than an omission.
    //
    // `roles::gatekeeper()` seats `bash`, `bash` is `Access::Exec`, and a seat that can reach
    // the gate must have an adjudicator that reaches somebody: `Harness::open` REFUSES to open
    // a session with an exec tool and an adjudicator whose own account of itself begins `none`.
    // A daemon-owned reviewer has no head, so the default `Head` adjudicator is exactly that,
    // and the alternatives are a `Console` reader (a background daemon has none) or a model
    // adjudicator over `[gatekeeper] endpoint` (a real answer, and a decision about who rules
    // on a reviewer's own commands that belongs with the gatekeeper's seat rather than with
    // this wiring).
    //
    // So the door is wired and it REFUSES BY NAME when no reviewer session is live — which is
    // the honest state of this build, and the safe direction: the queue asks, is told nobody
    // can be asked, and lands nothing. Nothing lands unreviewed either way; what is missing is
    // the review that would let it land at all.
    let reviewer: Box<dyn letibot_tools::gatekeeper::Reviewer + Send> = match cfg.store.as_ref() {
        None => Box::new(letibot_tools::gatekeeper::NoReviewer),
        Some(path) => match letibot_tokencore::store::Store::open(path) {
            Ok(store) => Box::new(crate::mergequeue::SessionReviewer::new(
                store,
                registry.bell().clone(),
                registry.clone(),
                crate::mergequeue::REVIEWER_SESSION_ID.to_string(),
            )),
            Err(e) => {
                eprintln!(
                    "  merge queue: the reviewer cannot write requests — {}: {e}",
                    path.display()
                );
                Box::new(letibot_tools::gatekeeper::NoReviewer)
            }
        },
    };
    let merge_queue = crate::mergequeue::spawn_for(
        &cfg,
        merge_stop.clone(),
        reviewer,
        // **Where the queue's moves go: EVERY session's log.** See
        // `letibot_sessionlog::registry::Registry::broadcast` for the decision and its cost —
        // in short, the queue is daemon-level and any head can open the pane, so the events
        // that keep that pane current have to reach a log wherever the head is attached.
        Box::new({
            let registry = registry.clone();
            move |event| {
                registry.broadcast(event);
            }
        }),
    );

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

    // **The per-model profile out of `providers.toml`** — `[model.<family>]` for the
    // dialect's effort, `[model."<alias>"]` for this model's sampling.
    //
    // This is the wire that was missing. The file has documented the two blocks, the
    // precedence and a worked example since 2026-09-19, and nothing in the tree parsed
    // either: `cfg.sampling` was assigned nowhere, so every daemon ran the
    // `Config::default` greedy literal and every number written in that file did
    // nothing. A documented knob that is silently ignored is worse than an absent one,
    // because the operator has no way to tell the difference from the outside.
    //
    // **Flag beats file beats built-in**, as the file says: `--effort` is read before
    // this and keeps its value, and sampling has no flag yet, so a block wins over the
    // built-in whenever it says anything at all.
    //
    // **A block replaces the built-in rather than merging onto it.** Merging would
    // leave `temperature = 0.0` in force under a block that set only
    // `presence_penalty`, which is the built-in deciding the most consequential knob
    // in a profile the operator wrote — a surprise in the one direction nobody would
    // look for.
    {
        let profile = letibot_provider::keys::model_profile(cfg.dialect.family(), &cfg.model, None);
        if !profile.sampling.is_empty() {
            cfg.sampling = serde_json::Value::Object(profile.sampling.clone());
        }
        if cfg.effort.is_none() {
            cfg.effort = profile.effort.clone();
        }
        // **Said, not swallowed.** The defect above was a number nobody read; a
        // misspelled key is the same defect one line lower, so it gets a line.
        for u in &profile.unknown {
            eprintln!(
                "  providers.toml: [model] carries `{u}`, which nothing here reads. \
                 Sampling keys are temperature, top_p, top_k, min_p, presence_penalty, \
                 frequency_penalty, repeat_penalty, seed, max_tokens, thinking_budget_tokens."
            );
        }
    }

    // **The operator's `prompts.toml`, read once, here.** The per-model system-prompt
    // overrides, beside `providers.toml` in the same config dir.
    //
    // Read at startup rather than per session because the report belongs at startup:
    // a file that does not parse, or that names a section this daemon does not know,
    // is said once, with the parser's own message and the path, and the daemon runs
    // on `DEFAULT_SYSTEM` for every session. Silently ignoring an unreadable prompt
    // file is the failure this feature exists to forbid — somebody edits a prompt,
    // sees no change, and cannot tell whether they were ignored.
    {
        let path = crate::config::Prompts::path();
        match crate::config::Prompts::load(&path) {
            Ok(prompts) => cfg.prompts = prompts,
            Err(why) => {
                eprintln!("  prompts.toml: {why} — the session runs on the built-in prompt")
            }
        }
    }

    // **The project's `leticode.toml`, discovered by walking up from the workspace
    // and read once, here.** The per-project models, beside the daemon's own
    // standing choice in `~/.config/letibot/`.
    //
    // Read after the user config (`[default]`, `[gatekeeper]`) rather than before,
    // because the precedence is command line beats the project file, the project
    // file beats `~/.config/letibot/`, and an unset key falls through — and the
    // user config is already in `cfg` by the time this runs, so applying the
    // project file now is what makes it beat the user config. The command line is
    // checked rather than re-applied: a `main_model` the operator typed must not be
    // overridden by one the project set, so the project's `main_model` is applied
    // only when the command line did not name one.
    //
    // **A file that does not parse does not take the session down.** The daemon
    // runs on its own defaults and says so — the file, the parser's own message,
    // and what happens instead — the way `prompts.toml` already does. Silently
    // ignoring an unreadable project file is the failure this feature exists to
    // forbid: somebody sets a model, sees no change, and cannot tell whether they
    // were ignored.
    if no_project_config {
        eprintln!(
            "  leticode.toml: --no-project-config, so the project file is not read \
             and the session runs on the daemon's own models"
        );
    } else {
        match crate::leticode_config::LeticodeConfig::discover(&cfg.workspace) {
            None => {
                // No project file at or above the workspace: the daemon's own
                // models, and nothing to name. Said rather than silent, because a
                // project that expects a `leticode.toml` and does not find one is a
                // setting the operator will reasonably believe took effect.
                eprintln!(
                    "  leticode.toml: none found at or above {} — the session runs \
                     on the daemon's own models",
                    cfg.workspace.display()
                );
            }
            Some(path) => match crate::leticode_config::LeticodeConfig::load(&path) {
                Ok(project) => {
                    // **The precedence, applied.** Command line beats the project
                    // file, the project file beats `~/.config/letibot/`, and an
                    // unset key falls through. The user config is already in `cfg`,
                    // so applying the project file now is what makes it beat the
                    // user config; the command line is checked rather than
                    // re-applied.
                    //
                    // **This is the one level `precedence` itself is not called
                    // for**, and the reason is in `apply_main_model`: its argument
                    // is not a name but a whole `provider` block, and re-applying
                    // the USER config's model through it would drop the key
                    // resolved for it (the block it builds carries `api_key: None`).
                    // So the two levels that can win are spelled out here, and the
                    // third is left where `cfg` already has it.
                    if !cli_main_model {
                        if let Some(m) = &project.main_model {
                            // The `bound` argument is the alias `--model` set; this
                            // intermediate state has no reader for it yet, and passing
                            // `None` is the same behaviour as before the level landed.
                            apply_main_model(&mut cfg, m, None);
                        }
                    }
                    // **The guard's model, and the adjudicator's — two words for the ONE oracle
                    // this build has**, which `load` has already refused to see disagree. The
                    // precedence goes through the one function that states it rather than being
                    // spelled out again here: there is no command-line flag for the guard's
                    // model, and `[gatekeeper] model` from providers.toml is already in
                    // `cfg.oracle_model` above — so the project's word wins over the user's,
                    // and an unset key leaves the user's where it was.
                    if let Some((m, _)) = crate::leticode_config::precedence(
                        None,
                        project
                            .gatekeeper_model
                            .as_deref()
                            .or(project.judge_model.as_deref()),
                        cfg.oracle_model.as_deref(),
                    ) {
                        cfg.oracle_model = Some(m);
                    }
                    // **`subagent_model` and `[roles]` do not ride the config for a path to
                    // read later** — the spawn reads them off `cfg.leticode` at the moment it
                    // seats a child (`HarnessTaskRunner::run_to_completion`), which is the only
                    // place a child's model and its samplers are decided. A key carried "for
                    // the path that reads it" and read by no path is the defect this feature
                    // is written against, and both were exactly that until now.
                    cfg.leticode = project;
                    let set = cfg.leticode.set_models();
                    if set.is_empty() {
                        eprintln!(
                            "  leticode.toml: {} sets no models — the session runs \
                             on the daemon's own models",
                            path.display()
                        );
                    } else {
                        eprintln!(
                            "  leticode.toml: {} sets {}",
                            path.display(),
                            set.join(", ")
                        );
                    }
                }
                Err(why) => {
                    eprintln!(
                        "  leticode.toml: {why} — the session runs on the daemon's \
                         own models"
                    );
                }
            },
        }
    }

    // **The image marker, read off the server for the same reason the window is.**
    //
    // It is a per-instance random value published on `/props`, so `None` means *this endpoint takes
    // no images* — a metered provider, or a local server without `mtmd`. Nothing is assumed: the
    // model's own vision tokens are the wrong answer and are refused by the server, which is the
    // measurement `serving::served_media_marker` records.
    if cfg.provider.is_none() {
        cfg.media_marker = letibot_turn::serving::served_media_marker(&cfg.endpoint);
    }

    // **The wall, read off the server rather than assumed.** Only for a local
    // endpoint: a metered provider has no `/props`, and its window stays `None`
    // unless the operator states it, because a guessed window would either
    // compact a conversation that had room or fail to compact one that did not.
    if cfg.context_window.is_none() {
        cfg.context_window = match &cfg.provider {
            // A local endpoint states its own window, and `/props` is the only
            // place that number is true.
            None => letibot_turn::serving::served_ctx(&cfg.endpoint),
            // **A metered provider's window comes from the catalogue.**
            //
            // This used to be skipped entirely for a provider, on the reasoning
            // that "a guessed window would either compact a conversation that had
            // room or fail to compact one that did not". That was right when
            // nothing here knew a cloud model's size — and it stopped being right
            // when `catalogue` landed, which is the same models.dev number
            // `Harness::retune_window` already switches to when the operator runs
            // `/models`. So a session that switched mid-conversation planned
            // against a real window and a session that STARTED on the provider
            // planned against none at all.
            //
            // `None` is not a large window, it is no wall: every check that would
            // compact reads `let Some(window) = self.cfg.context_window` and is
            // skipped. Measured 2026-09-20 on the operator's box — `[default]
            // provider = deepseek` in providers.toml, so this branch was taken at
            // every start, and their session reached 1,023,545 resident tokens
            // against a model whose catalogue limit is 1,000,000, having never
            // once compacted. "leticl session shows 1m context and doesnt compact".
            //
            // A model the catalogue does not carry still yields `None`, which is
            // the old behaviour for exactly the case the old comment was about.
            Some(pc) => pc.catalogue_window(&letibot_provider::catalogue::Catalogue::load()),
        };
    }

    // Captured before `cfg` and `parts` are handed to the worker: the HTTP head
    // needs the same vocabulary and dialect and none of the session state.
    let cfg_http = cfg.http.clone();
    let http_parts = parts.clone();
    let http_cfg = cfg.clone();

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
            .set_shelf(std::sync::Arc::new(letibot_flowy::FabricShelf::new(
                seat.clone(),
            )));
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
    {
        // Say the wall and what happens at it. A session that will compact itself
        // without warning is a session doing something the operator did not see
        // coming, and one that will NOT is worth knowing before it dies at 500.
        let c = sessions.harness_of(&session_id).map(|h| h.config().clone());
        if let Some(c) = c {
            match (c.context_window, c.auto_compact) {
                (Some(w), true) => eprintln!(
                    "  context  {w} tokens; compacts automatically with {} left, as one \
                     more message so the prefix stays cached",
                    c.headroom()
                ),
                (Some(w), false) => eprintln!(
                    "  context  {w} tokens; --no-auto-compact, so `/compact` is the only \
                     way and the wall is a 500"
                ),
                (None, _) => eprintln!(
                    "  context  UNKNOWN (the endpoint did not say and --context-window was \
                     not given), so nothing compacts on its own and the wall is a 500"
                ),
            }
        }
    }
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
            Err(e) => {
                eprintln!("           node not answering yet ({e}); the listener will keep trying")
            }
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
    if !steps.is_empty() {
        for step in &steps {
            let p = match step {
                Step::Slash(line) => {
                    let reply = sessions.slash(&session_id, line);
                    for l in &reply.lines {
                        println!("{l}");
                    }
                    // A refused verb is a failure of the run, the same as a failed
                    // prompt: a script that switched model and carried on against
                    // the old one would report a pass for the wrong thing.
                    if !reply.ok {
                        failed += 1;
                    }
                    continue;
                }
                Step::Prompt(p) => p,
            };
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
                            keeps
                                .iter()
                                .map(|k| format!("{k:.4}"))
                                .collect::<Vec<_>>()
                                .join(" ")
                        )
                    };
                    eprintln!(
                        "  [{} round(s), {} tool call(s), {tail}]",
                        reply.rounds, reply.tool_calls
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
        if let Some(seat) = &seat {
            seat.stop();
        }
        return Ok(if failed > 0 { 1 } else { 0 });
    }

    // **The HTTP head, before the worker loop takes this thread.** Its own
    // thread, its own engine, its own scratch transcripts: it shares the
    // vocabulary and the dialect and touches no session, so nothing it does can
    // reach the conversations this daemon is holding.
    if let Some(addr) = cfg_http.clone() {
        match std::net::TcpListener::bind(&addr) {
            Ok(l) => {
                let (p, c) = (http_parts, http_cfg);
                std::thread::Builder::new()
                    .name("http-head".into())
                    .spawn(move || crate::httphead::serve(l, p, c))
                    .map_err(|e| format!("starting the http head: {e}"))?;
            }
            Err(e) => return Err(format!("--http {addr}: {e}")),
        }
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
        // A wake handed to the session's own thread; nothing ran on this one. See
        // `Sessions::wake` — the arm exists because the outcome is the worker's vocabulary.
        Outcome::HandedOn => {}
    });
    // **The queue is told to stop, and it is deliberately NOT joined.** A gate in flight is a
    // `cargo test --workspace`, so waiting here would hold the daemon's exit for minutes — and
    // nothing is lost by not waiting: the row on disk says `Taken`, and the next daemon's
    // `recover` moves it to `Stale` with its reason, which is the designed answer to a merge
    // interrupted mid-flight. Dropping the handle detaches the thread; the process is exiting.
    merge_stop.store(true, std::sync::atomic::Ordering::Relaxed);
    drop(merge_queue);

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
    Calibrate { write: bool },
    Backfill { write: bool },
    Etalon(PathBuf),
    EtalonOracle(PathBuf),
    EtalonMap(PathBuf, PathBuf),
    RepairPrefixes,
    Compare,
    ShowBrief,
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
        // **Re-render a prefix that was stored without its tokens.**
        //
        // A `/reseat` fork wrote its new prefix row with an empty token list and a
        // zero `h_init`, so every transcript opened on it verifies as a broken
        // chain at row 0 and refuses to resume — with the conversation's items all
        // present and correct behind it, because the LIVE session rendered the
        // prefix properly and only the store row was wrong.
        //
        // The row is content-addressed by its system text and tool schemas, so the
        // tokens can be recomputed exactly: render them again through this
        // daemon's own renderer and fill the row in. A row that already has tokens
        // is never touched.
        Query::RepairPrefixes => {
            let empty: Vec<(String, String, String, String)> = store
                .connection()
                .prepare("SELECT id, system, tools_json, dialect_sha FROM stable_prefix WHERE n_tokens = 0")
                .and_then(|mut st| {
                    st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                        .and_then(|rows| rows.collect())
                })
                .map_err(|e| e.to_string())?;
            if empty.is_empty() {
                println!("every stored prefix has its tokens; nothing to repair.");
                return Ok(0);
            }
            let parts = Parts::load(cfg).map_err(|e| e.to_string())?;
            let engine = crate::harness::engine_for(&parts, cfg).map_err(|e| e.to_string())?;
            let mut fixed = 0;
            for (id, system, tools_json, dialect_sha) in empty {
                let tools: Vec<String> =
                    serde_json::from_str(&tools_json).map_err(|e| e.to_string())?;
                let prefix = letibot_dialect::StablePrefix {
                    system,
                    tools_json: tools,
                };
                let opened = engine
                    .open(&format!("repair-{id}"), &prefix)
                    .map_err(|e| format!("re-rendering {id}: {e}"))?;
                let rec = letibot_tokencore::store::StablePrefixRecord {
                    // The id hashes this too, so the stored one is what makes the
                    // recomputed row the SAME row rather than a new prefix.
                    dialect_sha,
                    system: prefix.system.clone(),
                    tools_json: prefix.tools_json.clone(),
                    tokens: opened.ledger.prefix_tokens().to_vec(),
                    h_init: opened.ledger.h_init(),
                    vocab_source: cfg.vocab_gguf.display().to_string(),
                };
                // Content-addressed: the id is the hash of the system text and the
                // schemas, so this is the same row and the write fills it in.
                let wrote = store.put_stable_prefix(&rec).map_err(|e| e.to_string())?;
                if wrote != id {
                    println!(
                        "  {id}: re-rendered to a DIFFERENT prefix ({wrote}) — not the \
                         same prompt, so the row was left alone"
                    );
                    continue;
                }
                println!("  {id}: {} tokens", rec.tokens.len());
                fixed += 1;
            }
            println!("repaired {fixed} prefix row(s); the transcripts on them resume again.");
            Ok(0)
        }
        Query::ShowBrief => {
            drop(store);
            let r = crate::calibrate::replay(cfg, path, 10_000)?;
            let pick = r
                .rows
                .iter()
                .find(|x| !x.guard_allowed && x.operator_admitted && x.brief.is_some())
                .or_else(|| r.rows.iter().find(|x| x.brief.is_some()));
            match pick {
                Some(row) => {
                    println!(
                        "a call YOU allowed and the guard would not, and everything it \
                         had to go on:\n\n  verdict: {}\n\n{}",
                        row.guard_said,
                        row.brief.as_deref().unwrap_or("")
                    );
                }
                None => println!("no row was replayed with a brief to show."),
            }
            Ok(0)
        }
        Query::Compare => {
            drop(store);
            println!(
                "replaying the same operator-answered calls under each brief, so a \
                 difference belongs to one change.\n"
            );
            println!(
                "  {:<46} {:>7} {:>7} {:>13}",
                "arm", "agreed", "asks", "FALSE ALLOWS"
            );
            // One snapshot, every arm. The store is live.
            let rows = crate::calibrate::rows_to_replay(path, 10_000)?;
            for arm in crate::calibrate::ARMS {
                let r = crate::calibrate::replay_rows(cfg, path, rows.clone(), *arm)?;
                let agreed = r.rows.iter().filter(|x| x.is_saved_prompt()).count();
                let bad = r.rows.iter().filter(|x| x.is_false_allow()).count();
                let asks = r
                    .rows
                    .iter()
                    .filter(|x| !x.guard_allowed && x.operator_admitted)
                    .count();
                println!("  {:<46} {agreed:>7} {asks:>7} {bad:>13}", arm.name);
            }
            println!(
                "\nagreed: prompts this arm would have saved you. asks: times it would \
                 still have come to you. FALSE ALLOWS: times it would have admitted \
                 what you refused — the only column that is a fault, and the one a \
                 wording change must not raise."
            );
            Ok(0)
        }
        Query::Etalon(file) => {
            drop(store);
            // The transcripts' commands ran under a fixed shell — Claude Code's,
            // opencode's, letibot's own — so layer A reads them as it reads a
            // live leticode session, with the shell declared. Left undeclared,
            // 88% of the corpus is refused at "bare name under an unknown
            // shell" (measured 2026-09-16) and the table says nothing.
            let env = crate::harness::surroundings_for(cfg)
                .with_pinned_shell("the etalon: the harness that recorded it ran a fixed shell");
            let r = crate::etalon::report(&file, &env, 1_000_000)?;
            print!("{}", r.render());
            Ok(0)
        }
        Query::EtalonOracle(file) => {
            drop(store);
            // The best arm `--compare` found: the fullest brief, the verdict
            // question. Measured 2026-09-16 at 52/11 on the store's rows.
            let arm = crate::calibrate::ARMS
                .iter()
                .find(|a| {
                    a.claim && a.examples && matches!(a.question, crate::oracle::Question::Verdict)
                })
                .copied()
                .ok_or("no claim arm in ARMS")?;
            let o = crate::etalon_oracle::measure(cfg, path, &file, arm, 100_000)?;
            print!("{}", crate::etalon_oracle::render(&o, &arm));
            Ok(0)
        }
        Query::EtalonMap(file, out) => {
            drop(store);
            let env = crate::harness::surroundings_for(cfg)
                .with_pinned_shell("the etalon: the harness that recorded it ran a fixed shell");
            let t = std::time::Instant::now();
            let map = crate::etalon_map::build(&file, &env, 1_000_000, 4000)?;
            let json = serde_json::to_string(&map).map_err(|e| e.to_string())?;
            std::fs::write(&out, json).map_err(|e| format!("{}: {e}", out.display()))?;
            eprintln!(
                "etalon map: {} rows, {} commands, {} unique shapes, {}x{} cells, {} epochs, {:.1}s → {}",
                map.rows,
                map.commands,
                map.unique_shapes,
                map.side,
                map.side,
                map.epochs,
                t.elapsed().as_secs_f64(),
                out.display()
            );
            Ok(0)
        }
        Query::Backfill { write } => {
            // Same reason as the replay below: the backfill opens its own handle.
            drop(store);
            let mut report = crate::backfill::plan(path)?;
            if write {
                crate::backfill::apply(path, &mut report)?;
            }
            print!("{}", report.render());
            Ok(0)
        }
        Query::Calibrate { write } => {
            // The store connection above is dropped by the replay opening its own;
            // two handles on one WAL database is the store's declared shape.
            drop(store);
            let report = crate::calibrate::replay(cfg, path, 10_000)?;
            print!("{}", report.render());
            match (write, report.earned_scope()) {
                (true, Some(scope)) => {
                    let file = crate::calibrate::write_calibration(cfg, &scope)?;
                    println!(
                        "\nrecorded in {} — from the next daemon start the guard may \
                         answer about this much, and the banner cites the numbers \
                         above when it says why. Delete the file to undo it; whatever \
                         `[gatekeeper]` sets in providers.toml still applies either \
                         way, and this only ever adds to it.",
                        file.display()
                    );
                }
                (true, None) => println!(
                    "\nnothing recorded: no intent reached {} rows with zero false \
                     allows, so there is no scope this evidence supports.",
                    crate::calibrate::MIN_ROWS
                ),
                (false, _) => println!(
                    "\n(a dry run — `--calibrate-write` records the scope above so the \
                     daemon reads it at startup)"
                ),
            }
            Ok(0)
        }
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
    // The empty path is refused at the flag, and refused again here: `starts_with("")`
    // is true for every path, so a scope that reached this function empty would
    // widen the query to the whole box while looking like it had narrowed it. Two
    // guards for one mistake, because the failure is silent and the blast radius
    // is continuing somebody else's conversation.
    if root.as_os_str().is_empty() {
        return Err(
            "a store query was scoped to the empty path, which matches every \
                    session on this box rather than none. Nothing was searched."
                .into(),
        );
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_tokencore::store::{SessionRecord, Store};

    fn store_with(workspaces: &[(&str, &str)]) -> (Store, TempDir) {
        let d = TempDir::new();
        let store = Store::open(&d.0.join("sessions.db")).expect("opening");
        for (id, root) in workspaces {
            store
                .put_session(&SessionRecord {
                    id: (*id).into(),
                    title: None,
                    model_id: "m".into(),
                    dialect_sha: "d".into(),
                    workspace_root: (*root).into(),
                    owner: "dead".into(),
                    approvers: vec![],
                    role: None,
                    parent_session_id: None,
                })
                .expect("session");
        }
        (store, d)
    }

    struct TempDir(std::path::PathBuf);
    impl TempDir {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "harnessd-scope-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&p).expect("scratch");
            TempDir(p)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// **A `main_model` from the project file lands on the right `cfg` field.** A
    /// `provider/model` name sets the metered provider, and a bare alias sets the
    /// local model and clears the provider, because the two are the two ways a
    /// session's turns go and a name is one or the other, not both. A regression
    /// here would be a project that set a model the session did not run on, which
    /// is the defect this feature exists to forbid.
    #[test]
    fn a_project_main_model_lands_on_the_right_config_field() {
        // A `provider/model` name sets the metered provider.
        let mut cfg = Config::for_this_box("/tmp");
        apply_main_model(&mut cfg, "deepseek/deepseek-chat", None);
        let pc = cfg
            .provider
            .expect("a provider/model name sets the provider");
        assert_eq!(pc.name, "deepseek");
        assert_eq!(pc.model.as_deref(), Some("deepseek-chat"));
        assert!(
            pc.api_key.is_none(),
            "the key is resolved along the usual path"
        );

        // A bare alias sets the local model and clears the provider.
        let mut cfg = Config::for_this_box("/tmp");
        cfg.provider = Some(crate::config::ProviderConfig {
            name: "deepseek".into(),
            model: Some("deepseek-chat".into()),
            api_key: None,
            thinking: false,
        });
        apply_main_model(&mut cfg, "qwen-3.8-flash-next", None);
        assert_eq!(cfg.model, "qwen-3.8-flash-next");
        assert!(cfg.provider.is_none(), "a bare alias is a local session");
    }

    /// **The empty scope matched everything.** `Path::new("/a").starts_with("")` is
    /// true, so a store query scoped to the empty path silently widened to the whole
    /// box — and `--continue` then reopened the newest conversation anywhere, which
    /// is what the operator saw: *"--continue seems to be continuing the last
    /// global, not the last in project"*.
    #[test]
    fn an_empty_scope_is_refused_rather_than_matching_every_session() {
        let (store, _d) = store_with(&[
            ("s-a", "/home/dead/Projects/leticl"),
            ("s-b", "/home/dead/Projects/rano"),
        ]);
        let e = scoped(&store, Some(std::path::Path::new("")))
            .expect_err("an empty scope is not a scope");
        assert!(e.contains("matches every session"), "{e}");

        // A real scope still narrows, and no scope at all still means the whole
        // store — the deliberate case, which this must not break.
        assert_eq!(
            scoped(
                &store,
                Some(std::path::Path::new("/home/dead/Projects/rano"))
            )
            .expect("scoping")
            .len(),
            1
        );
        assert_eq!(scoped(&store, None).expect("unscoped").len(), 2);
    }

    /// The prefix match reaches DOWNWARDS only: a session opened in a subdirectory
    /// belongs to the tree above it. This is the behaviour `scoped`'s own comment
    /// describes, pinned so the empty-path guard above cannot quietly change it.
    #[test]
    fn a_scope_finds_sessions_opened_beneath_it() {
        let (store, _d) = store_with(&[("s-a", "/home/dead/Projects/rano/crates/ui")]);
        assert_eq!(
            scoped(
                &store,
                Some(std::path::Path::new("/home/dead/Projects/rano"))
            )
            .expect("scoping")
            .len(),
            1,
            "a session in a subdirectory belongs to the tree"
        );
        // And not sideways into a sibling.
        assert!(
            scoped(
                &store,
                Some(std::path::Path::new("/home/dead/Projects/leticl"))
            )
            .expect("scoping")
            .is_empty()
        );
    }
}
