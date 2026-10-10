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

/// **Apply a `main_model` from a file to the session's config.**
///
/// A `main_model` is a model name, the same shape the operator types for `--model`
/// and the names `/models` offers: a `provider/model` for a metered session, a bare
/// alias for a local one. This is the one place that turns that name into the `cfg`
/// fields the session runs on, and it is the file half of the precedence — called
/// for the user level and then for the project level, each over the last, and never
/// when the command line chose who answers.
///
/// A `provider/model` name sets the metered provider; a bare alias sets the local
/// model and **clears the provider**, because the two are the two ways a session's
/// turns go and a name is one or the other, not both. The clearing is the half that
/// gives the operator their local model back: `[default] provider = deepseek` in
/// `providers.toml` is read into `cfg.provider` below this, and a file that says
/// `main_model = "glm-5.3-flash"` is a file saying *answer on this box*. The
/// provider's key is not set here: it is resolved along the usual path
/// (`$PROVIDER_API_KEY`, then `providers.toml`), and a file that names a model but
/// not a key is a model the operator has a key for, not a secret the file carries.
///
/// # `bound`: the alias `--model` bound, and why it is not overwritten
///
/// The launcher passes `--model` on every start, and what it names is the LOCAL
/// alias — the vocabulary this daemon tokenises with (`scripts/letibot` labels the
/// two facts apart in its own banner: `model` is the binding, `answers` is what
/// answers). So `--model` is not a choice of who answers and does not suppress a
/// file's `main_model`; but it IS the binding, and a file that re-bound it would
/// point the daemon's GGUF at weights the server is not serving. That failure is
/// `400 Prompt contains invalid tokens` at the first turn, which names nothing.
///
/// So a file's bare alias clears the provider — the half that decides who answers —
/// and does not overwrite a binding the command line set. When the two disagree the
/// disagreement is RETURNED and the caller puts it on the screen, because a file
/// the operator wrote that silently did not take effect is the defect this whole
/// feature is against.
fn apply_main_model(cfg: &mut Config, model: &str, bound: Option<&str>) -> Option<String> {
    if let Some((provider, m)) = model.split_once('/') {
        cfg.provider = Some(crate::config::ProviderConfig {
            name: provider.to_string(),
            model: Some(m.to_string()),
            api_key: None,
            thinking: false,
        });
        return None;
    }
    // A bare alias: the file is saying *answer locally, on this model*.
    cfg.provider = None;
    match bound {
        Some(b) if b != model => Some(format!(
            "main_model = \"{model}\" names a local alias, and the command line bound \
             `--model {b}`. This session tokenises with {b}'s vocabulary, so {b} is the model \
             it runs on and the file's alias is not applied — the server is serving one set \
             of weights and ids from another are valid numbers that mean other words. The \
             file's alias still cleared the provider, which is the half that decides who \
             answers."
        )),
        _ => {
            cfg.model = model.to_string();
            None
        }
    }
}

/// **A `provider/model` name a file set that this box cannot reach**, as a sentence.
///
/// No network and no endpoint, so this is asked of every model name in either file:
/// the provider must be one this build knows, and a key for it must resolve here.
/// The alternative is the failure the tree has paid for three times — a *plausible*
/// string reaching the first turn and coming back as a `401` that names neither the
/// file nor the line that wrote it.
fn unreachable_provider(model: &str) -> Option<String> {
    let (provider, m) = model.split_once('/')?;
    let Ok(preset) = letibot_provider::Preset::parse(provider) else {
        let known: Vec<&str> = letibot_provider::presets::ALL
            .iter()
            .map(|p| p.name)
            .collect();
        return Some(format!(
            "`{model}` names provider `{provider}`, which this build does not know — the \
             presets are {}. Nothing refuses this at startup, so the first turn would fail \
             on a name nothing here can resolve.",
            known.join(", ")
        ));
    };
    if m.trim().is_empty() {
        return Some(format!(
            "`{model}` names no model on provider `{provider}` — write `{provider}/MODEL`, \
             the way `/models` prints it"
        ));
    }
    if let Err(why) = letibot_provider::keys::resolve(preset, None, None) {
        return Some(format!(
            "`{model}` names provider `{provider}`, and no key for it resolves on this box: \
             {why}. The first turn would fail as an authorization error that names none of \
             this."
        ));
    }
    None
}

/// **A bare local alias that names a model this daemon cannot reach**, as a sentence.
///
/// Asked only of the name that decides THIS daemon's own turns — `main_model` — and
/// `served` is the alias the local server reports off `/props`. A `subagent_model` is
/// decided per spawn, and the guard's model answers at the `[gatekeeper]` endpoint,
/// which is a different server on this fleet: comparing either against this daemon's
/// would be a false alarm, and a warning that cries wolf is worse than none.
///
/// `local` is reachable by definition — it is the word the file's own documentation
/// uses for *this daemon's server* (`LeticodeConfig::spawn_model`) — and so is a
/// `[model."..."]` block in `providers.toml` that carries an address, because that is
/// a model `/models NAME` can switch to.
fn unreachable_local(cfg: &Config, model: &str, served: Option<&str>) -> Option<String> {
    if model.eq_ignore_ascii_case("local") {
        return None;
    }
    let declared = letibot_provider::keys::local_models(None);
    if declared
        .iter()
        .any(|m| m.name == model || m.model == model || m.name.eq_ignore_ascii_case(model))
    {
        return None;
    }
    match served {
        Some(s) if letibot_turn::serving::matches(s, model) => None,
        Some(s) => Some(format!(
            "`{model}` is a local alias, and the server at {} is serving `{s}` — not it — \
             and providers.toml declares no `[model.\"{model}\"]` block with a url. A daemon \
             submits token ids, so the first turn would come back `400 Prompt contains \
             invalid tokens`, which names nothing.",
            cfg.endpoint.authority()
        )),
        None => Some(format!(
            "`{model}` is a local alias, nothing answers `/props` at {} to confirm it, and \
             providers.toml declares no `[model.\"{model}\"]` block with a url — so this \
             name is not one this daemon can reach.",
            cfg.endpoint.authority()
        )),
    }
}

/// **Every model name either file set, with the faults this box can see.**
///
/// The provider-shaped half is asked of all of them; the local half only of
/// `main_model`, which is the one that decides this daemon's own turns.
fn unreachable_models(
    cfg: &Config,
    files: &crate::leticode_config::LeticodeConfig,
    served: Option<&str>,
) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for m in [
        files.subagent_model.as_deref(),
        files.gatekeeper_model.as_deref(),
        files.judge_model.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        names.push(m.to_string());
    }
    for role in files.roles.values() {
        if let Some(m) = &role.model {
            names.push(m.clone());
        }
    }
    let mut out: Vec<String> = names
        .iter()
        .filter_map(|m| unreachable_provider(m))
        .collect();
    if let Some(m) = &files.main_model {
        if let Some(why) = unreachable_provider(m) {
            out.push(why);
        } else if let Some(why) = unreachable_local(cfg, m, served) {
            out.push(why);
        }
    }
    out
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
     \x20 the model, and which level decided it:\n\
     \x20                           command line > project `leticode.toml` > user\n\
     \x20                           `~/.config/letibot/leticode.toml` > built-in. The\n\
     \x20                           user file is the same keys as the project's, read\n\
     \x20                           once for the whole box, so a `main_model` line\n\
     \x20                           there decides what a session runs on without a\n\
     \x20                           flag. `--provider` is the flag that\n\
     \x20                           outranks both; `--model` alone names the LOCAL\n\
     \x20                           alias the daemon tokenises with, and does not\n\
     \x20                           suppress the files. Startup names the level that won,\n\
     \x20                           and names a model this box cannot reach\n\
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
    // **Whether the command line chose WHO ANSWERS.** That is `--provider`, and it
    // is the flag level of the precedence: `--provider deepseek` beats the project
    // file, the project file beats the user file, and the user file beats the
    // built-in default.
    //
    // **`--model` alone is deliberately not this**, and the reason is the tree's own
    // vocabulary rather than a convenience. `--model` names the LOCAL alias — the
    // vocabulary this daemon tokenises with, which `scripts/letibot` labels apart
    // from `answers` in its own banner — and the launcher passes it on EVERY start,
    // including a start where the operator said nothing about a model at all. A
    // `--model` that suppressed the files would make both of them unreachable for
    // every launcher-started daemon, which is the defect this level exists to fix.
    // Tracked rather than read off `cfg`, because `cfg.model` and `cfg.provider` are
    // also set by the files, and the two must not be told apart by a field they
    // share.
    let mut cli_main_model = false;
    // **The local alias `--model` bound**, when it named one. The binding and the
    // choice of who answers are two facts and this is the other one: a file's
    // `main_model` may clear the provider without re-binding the vocabulary — see
    // `apply_main_model`.
    let mut cli_binding: Option<String> = None;
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
                cfg.model = m.clone();
                // The binding, not the choice of who answers — see `cli_main_model`.
                cli_binding = Some(m);
            }
            "--vocab" => cfg.vocab_gguf = Some(PathBuf::from(next()?)),
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
                // A provider answers the turns, so there is no local binding left to
                // keep: the file levels do not speak at all from here down.
                cli_binding = None;
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

    // One registry, seeded with the session named on the command line. A head can
    // make more over the socket; this one is the daemon's own, and it is opened
    // eagerly so that a dialect which does not fit the vocabulary is a startup
    // error rather than a failure on somebody's first prompt.
    let registry = Registry::new();
    // What is on disk, so a head's picker can show sessions from daemons that are no
    // longer running and `ResumeSession` can find them. A daemon with no `--store`
    // sets no source and lists only what it holds, which is what it always did.
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

    // **The door the queue reviews through: a gatekeeper SUBAGENT.**
    //
    // A reviewer the daemon owned by itself could not be opened — the gatekeeper seats `bash`,
    // and `Harness::open` refuses an exec seat whose adjudicator reaches nobody, which is what a
    // headless daemon session has. A subagent's asks go up to its root's head, so the review
    // runs as a hidden child of the root of the session that enqueued the entry (resumed from
    // the store when it is not live): `GatekeeperDoor` writes the request naming that host and
    // rings it, and the host spawns the child and writes the verdict (`Harness::serve_reviews`).
    // **The row is what the review depends on and the ring is only the latency**: a ring is a
    // bell and a bell is per-daemon, so a daemon serves the reviews ITS sessions host from the
    // store on its own clock (`Sessions::serve_reviews`). With no store there is no row to write,
    // and the door refuses by name.
    let reviewer: Box<dyn letibot_tools::gatekeeper::Reviewer + Send> = match cfg.store.as_ref() {
        None => Box::new(letibot_tools::gatekeeper::NoReviewer),
        Some(path) => match letibot_tokencore::store::Store::open(path) {
            Ok(store) => Box::new(crate::mergequeue::GatekeeperDoor::new(
                store,
                registry.bell().clone(),
                registry.clone(),
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

    // **The user's `leticode.toml`, at one fixed path beside `providers.toml`.**
    //
    // The box-wide level: `main_model = "glm-5.3-flash"` here decides what a session
    // runs on **without a flag**, for every project on this box — which is the thing a
    // *project's* file cannot say and the thing the operator asked for. Same name,
    // same keys, same parser as the project file (`load_user` is `load` at this path);
    // the discovery is the whole difference, because a project file is FOUND by
    // walking up and this one is AT AN ADDRESS.
    //
    // Read before the project file, because the project file overrides it, and before
    // `[default]`, because `[default]` is the level below both.
    //
    // **A file that does not parse does not take the daemon down.** The file, the
    // line the parser stopped on, and what happens instead — the same sentence
    // `prompts.toml` already gets, and the same one the project file gets below. A
    // user file that took every daemon on the box down would be a worse failure than
    // the one it reports.
    let user_level = match crate::leticode_config::load_user() {
        Ok(u) => u,
        Err(why) => {
            eprintln!(
                "  ~/.config/letibot/leticode.toml: {why} — the session runs without the \
                 user level"
            );
            crate::leticode_config::LeticodeConfig::default()
        }
    };
    if let Some(path) = &user_level.path {
        let set = user_level.set_models();
        if set.is_empty() {
            eprintln!("  leticode.toml (user): {} sets no models", path.display());
        } else {
            eprintln!(
                "  leticode.toml (user): {} sets {}",
                path.display(),
                set.join(", ")
            );
        }
    }

    // **The project's `leticode.toml`, discovered by walking up from the workspace.**
    //
    // Read after the user file rather than before, because the precedence is command
    // line beats the project file, the project file beats the user file, the user file
    // beats the built-in default, and an unset key falls through — and the user file is
    // already loaded by the time this runs, so merging the project's keys over it now
    // is what makes the project level win. The command line is checked rather than
    // re-applied: a model the operator's own `--provider` named must not be overridden
    // by one a file set.
    let project_level = if no_project_config {
        eprintln!(
            "  leticode.toml: --no-project-config, so the project file is not read \
             and the session runs on the daemon's own models"
        );
        crate::leticode_config::LeticodeConfig::default()
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
                crate::leticode_config::LeticodeConfig::default()
            }
            Some(path) => match crate::leticode_config::LeticodeConfig::load(&path) {
                Ok(project) => {
                    let set = project.set_models();
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
                    project
                }
                Err(why) => {
                    eprintln!(
                        "  leticode.toml: {why} — the session runs on the daemon's \
                         own models"
                    );
                    crate::leticode_config::LeticodeConfig::default()
                }
            },
        }
    };

    // **The two file levels as one value, the project's over the user's.** This is
    // what the spawn reads (`cfg.leticode`) and what the disclosure names, and
    // merging here rather than at each read is what makes a project that sets one
    // key keep the user's others.
    //
    // **`subagent_model` and `[roles]` do not ride the config for a path to read
    // later** — the spawn reads them off `cfg.leticode` at the moment it seats a child
    // (`HarnessTaskRunner::run_to_completion`), which is the only place a child's
    // model and its samplers are decided. A key carried "for the path that reads it"
    // and read by no path is the defect this feature is written against.
    cfg.leticode = user_level.overridden_by(&project_level);
    cfg.leticode_user = user_level.clone();

    // **`main_model`, through the one function that states the precedence.**
    //
    // The command line is the flag level when it chose WHO ANSWERS — `--provider`,
    // and not `--model`, which is the local BINDING the launcher passes on every
    // start; see `cli_main_model` and `apply_main_model`. `[default]` is not a level
    // here at all: it is the built-in, applied below and only if none of the three
    // above spoke.
    if !cli_main_model {
        if let Some((model, level)) = crate::leticode_config::precedence(
            None,
            project_level.main_model.as_deref(),
            user_level.main_model.as_deref(),
        ) {
            let note = apply_main_model(&mut cfg, &model, cli_binding.as_deref());
            let file = match level {
                crate::leticode_config::Level::Project => project_level.path.clone(),
                crate::leticode_config::Level::User => user_level.path.clone(),
                _ => None,
            };
            cfg.model_source = crate::config::ModelSource {
                level,
                value: Some(model),
                file,
                origin: String::new(),
                note,
            };
        }
    } else if cfg.provider.is_some() {
        // The flag level, and `Level::describe` already names `--provider` — so there
        // is nothing to add and nothing that could disagree with it. The provider and
        // its model are on the front of the same sentence (`main deepseek/…`).
        cfg.model_source = crate::config::ModelSource {
            level: crate::leticode_config::Level::Flag,
            value: Some(cfg.prompt_model_name()),
            file: None,
            origin: String::new(),
            note: None,
        };
    }

    // **The built-in default, and it is BELOW the two files.**
    //
    // `[default]` in providers.toml — what `/models NAME` writes — is the daemon's
    // standing choice, and it is read here rather than before the files so that a
    // line the operator wrote in `leticode.toml` outranks it. That ordering is the
    // asymmetry this level exists to fix, and the shape of the defect is worth
    // keeping in view: `[default]` is read into `cfg.provider` only when
    // `cfg.provider.is_none()`, and `--model` (the launcher's binding) leaves it
    // `None` — so a FILE outranked a COMMAND LINE, and `letibot --glm` answered on
    // deepseek while its own banner said `glm-5.3-flash`.
    //
    // The Err is said, not swallowed: a daemon that comes up local while the file
    // says deepseek is a fault the operator cannot find from the outside. The line
    // follows the one prompts.toml already gets — the file, the parser's own message,
    // and what happens instead.
    if cfg.model_source.level == crate::leticode_config::Level::Builtin {
        match letibot_provider::keys::default_choice(None) {
            Ok(Some(d)) => {
                let origin = format!(
                    "[default] in {}",
                    letibot_provider::keys::config_file().display()
                );
                let model = Some(match &d.model {
                    Some(m) => format!("{}/{m}", d.provider),
                    None => d.provider.clone(),
                });
                cfg.provider = Some(crate::config::ProviderConfig {
                    name: d.provider,
                    model: d.model,
                    api_key: None,
                    thinking: false,
                });
                cfg.model_source = crate::config::ModelSource {
                    level: crate::leticode_config::Level::Builtin,
                    value: model,
                    file: Some(letibot_provider::keys::config_file()),
                    origin,
                    note: None,
                };
            }
            Ok(None) => {
                cfg.model_source = crate::config::ModelSource {
                    level: crate::leticode_config::Level::Builtin,
                    value: Some(cfg.model.clone()),
                    file: None,
                    origin: format!(
                        "the local server this daemon was launched against, {}",
                        cfg.endpoint.authority()
                    ),
                    note: None,
                };
            }
            Err(u) => {
                eprintln!(
                    "  {u} — the standing choice could not be read, so new sessions \
                     start on the local server this daemon was launched against"
                );
                cfg.model_source = crate::config::ModelSource {
                    level: crate::leticode_config::Level::Builtin,
                    value: Some(cfg.model.clone()),
                    file: None,
                    origin: format!(
                        "the local server this daemon was launched against, {} — the \
                         `[default]` block could not be read",
                        cfg.endpoint.authority()
                    ),
                    note: None,
                };
            }
        }
    }

    // **The guard's model, and the adjudicator's — two words for the ONE oracle this
    // build has**, which `load` has already refused to see disagree. The precedence
    // goes through the one function that states it rather than being spelled out
    // again here: there is no command-line flag for the guard's model, and
    // `[gatekeeper] model` from providers.toml is already in `cfg.oracle_model` — so
    // the merged file's word wins over the user's, and an unset key leaves the user's
    // where it was.
    if let Some((m, _)) = crate::leticode_config::precedence(
        None,
        cfg.leticode
            .gatekeeper_model
            .as_deref()
            .or(cfg.leticode.judge_model.as_deref()),
        cfg.oracle_model.as_deref(),
    ) {
        cfg.oracle_model = Some(m);
    }

    // **A model a file named that this box cannot reach, said at startup.**
    //
    // The third loud failure, and the one the tree has paid for three times: a name
    // that is a *plausible* string reaches the first turn and comes back `401` or
    // `400 Prompt contains invalid tokens`, neither of which names the file or the
    // line that wrote it. `/props` is asked once, and only when a file set a bare
    // local alias — a start where no file spoke pays nothing for this.
    {
        let wants_served = cfg.provider.is_none()
            && cfg
                .leticode
                .main_model
                .as_deref()
                .is_some_and(|m| !m.contains('/'));
        let served = if wants_served {
            letibot_turn::serving::served_model(&cfg.endpoint).ok()
        } else {
            None
        };
        for why in unreachable_models(&cfg, &cfg.leticode.clone(), served.as_deref()) {
            eprintln!("  leticode.toml: {why}");
            // The one about the model that decides this session also rides the
            // disclosure, because the screen is where the operator asked for it.
            if cfg
                .leticode
                .main_model
                .as_deref()
                .is_some_and(|m| why.contains(&format!("`{m}`")))
            {
                cfg.model_source.note = Some(why);
            }
        }
    }

    // **The per-model profile out of `providers.toml`** — `[model.<family>]` for the
    // dialect's effort, `[model."<alias>"]` for this model's sampling.
    //
    // Read HERE, after the levels rather than before them, because the profile is
    // looked up BY the alias the session runs on: a file that set `main_model` had its
    // `[model."…"]` sampling block skipped entirely while this ran earlier, so a
    // project or user file could name a model and get the built-in greedy literal
    // instead of the numbers the operator wrote for those weights.
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

    // **What to try when the model this session is on is OUT** — `[fallback] models`
    // in the same file, read once here rather than at the moment of need.
    //
    // A model that cannot answer is the one failure a person cannot fix by waiting
    // (MEASURED 2026-10-12: a weekly quota exhausted, 63 s of retries, the turn
    // recorded as FAILED), and until now there was nowhere in the file to say what
    // should answer instead. The list is theirs, in their order; this only reads it.
    //
    // **Said at startup**, because a fallback nobody knows is configured is a model
    // change nobody authorised: the first notice of one should not be a turn that
    // silently went somewhere else. A file that will not parse is a fault and says
    // so, the same way `[default]` does — the two readers of one file must not
    // disagree about it.
    match letibot_provider::keys::fallback_models(None) {
        Ok(list) if !list.is_empty() => {
            eprintln!(
                "  fallback: {} — from [fallback] in {}",
                list.join(", "),
                letibot_provider::keys::config_file().display()
            );
            cfg.fallback = list;
        }
        Ok(_) => {}
        Err(why) => eprintln!("  providers.toml: {why} — no fallback is in force"),
    }

    // **Captured here rather than before the levels**, because this is the banner's
    // copy of the model and the levels are what decide it: a file that set
    // `main_model` would otherwise be disclosed by the daemon's own launch line.
    // The source goes with it, because `cfg` is handed to `Sessions` a few lines
    // down and the banner is printed after that.
    let model = cfg.model.clone();
    let model_from = cfg.model_source.describe();

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

    // **Loaded here, after every source of the provider has spoken** — the flag,
    // `main_model` in leticode.toml, `[default]` in providers.toml — because which
    // vocabulary a daemon needs depends on who answers: a cloud provider with no
    // `--vocab` is the byte vocabulary, and that is only knowable once the provider is.
    let parts = Parts::load(&cfg).map_err(|e| e.to_string())?;

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
    // **The model, AND where it came from.** The operator's own question of the
    // evening — *"why is my session on deepseek"* — was answered by a file they had to
    // go and read, and a banner that says `model deepseek` without saying *which
    // level* is the same defect one line shorter. So the level is on this line, and
    // the `session model` disclosure under it carries the whole sentence.
    eprintln!("  model    {model} via {endpoint} (/completion, token array) — from {model_from}");
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
    // **Every subagent's VM comes down with the daemon.** Their backends live on threads the
    // process's exit does not unwind, so their `Drop` never ran and the VMs outlived it —
    // two of the acceptance suite's own runs, 2026-10-08. firecode's `down` delivers the work.
    letibot_tools::firecode::down_all();

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
                    vocab_source: parts.vocab.source().to_string(),
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
            let latest = newest_conversation(scoped(&store, scope)?, scope);
            // **What the pick walked past is said, not silently dropped.** The launcher
            // deliberately does not capture this stream: it is the announcement of which
            // conversation the next thing typed lands in, and a pick that skipped ten rows
            // to reach it should say so.
            if latest.hidden > 0 {
                eprintln!(
                    "skipped {} session(s) that are not conversations: a task child and the \
                     merge queue's reviewer are sessions, and neither is a conversation to \
                     continue",
                    latest.hidden
                );
            }
            if latest.empty > 0 {
                eprintln!(
                    "skipped {} session(s) with no rows: nothing was ever said in them",
                    latest.empty
                );
            }

            match &latest.pick {
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
                    eprintln!("{}", nothing_to_continue(&latest, scope));
                    Ok(1)
                }
            }
        }
        Query::List => {
            // **The same predicate the resolution reads**, and that is the point of it: the
            // listing and `--latest-session` are two questions about one store, and when they
            // disagreed the operator was handed a reviewer by `--continue` that this list would
            // never have offered him. Subagents are children of a session, not sessions a picker
            // lists — they live in the subagent tree, not the flat list — and the queue's
            // reviewer is not a conversation wherever it sits.
            let rows: Vec<_> = scoped(&store, scope)?
                .into_iter()
                .filter(is_conversation)
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

/// **Is this row a conversation a person can be brought back to?**
///
/// `--continue` promises to reopen *your conversation*, and a store holds three kinds of row that
/// are sessions without being one: a sub-session, the merge queue's reviewer, and a conversation
/// nobody ever said anything in. The first two are refused here; the third cannot be, because
/// *has anything been said in it* is the caller's question ([`newest_conversation`]).
///
/// * **A sub-session** — a `task` child, or the merge queue's reviewer. The relation is
///   `parent_session_id`, the same field the head's picker nests by (its `session_rows`), and
///   `--list-sessions` reads this same predicate — so the listing and the resolution cannot
///   disagree about a row again, which is exactly what they did.
/// * **The queue's gatekeeper**, by the seat its own row recorded (its `role`, which is
///   `Seat::Gatekeeper.as_str()`) or by the title cut from the review brief
///   ([`letibot_sessionlog::GATEKEEPER_TITLE_PREFIX`]).
///
/// # Why the gatekeeper has a mark of its own when it is already a child
///
/// The operator's report is what the mark is for: *"restart pg-noop and was brought to one of
/// the gatekeeper sessions"*. Ten reviewers spawned into one workspace by the merge queue made
/// the newest row in it a reviewer, and `--continue` — which asked only for the newest row —
/// handed back a review. The parent field alone would have skipped those ten; the seat and the
/// title are here because a reviewer is not a conversation *wherever it sits*: a row the
/// operator renamed keeps its seat, and a head rebuilding a row off the wire has no role to
/// read and only the title, which is why the brief is required to begin with the mark.
///
/// Nothing else is filtered. A root session is a conversation however it was started, whatever
/// its model, and whether or not it has a title.
fn is_conversation(s: &letibot_tokencore::store::StoredSession) -> bool {
    s.parent_session_id.is_none()
        && s.role.as_deref() != Some(crate::config::Seat::Gatekeeper.as_str())
        && !s
            .title
            .as_deref()
            .is_some_and(|t| t.starts_with(letibot_sessionlog::GATEKEEPER_TITLE_PREFIX))
}

/// What `--latest-session` resolved to, and what it walked past on the way.
struct Latest {
    /// The conversation to reopen, or `None` — and the three counts below are what tells the
    /// three ways of having none apart.
    pick: Option<letibot_tokencore::store::StoredSession>,
    /// Rows in scope before anything was skipped. The difference between *nothing is here* and
    /// *nothing but children is here*, which are two different sentences to a person.
    in_scope: usize,
    /// Rows that are sessions and not conversations: a task child, or the queue's reviewer.
    hidden: usize,
    /// Conversations nobody ever said anything in.
    empty: usize,
}

/// **The newest session that is a CONVERSATION** — the rule `--continue` implements.
///
/// # The rule
///
/// Of the sessions stored under the scope, take the ones that are conversations
/// ([`is_conversation`]) and have at least one row, and return the newest. "Newest" is
/// `last_activity_ms`, which is the last row's time — the order `Store::list_sessions` already
/// returns — and an **exact** workspace match beats a descendant of it, so standing in `~` and
/// asking to continue hands back the `~` conversation rather than the one in
/// `~/Projects/rano`. The sort is stable, so within each of those two groups recency stands.
///
/// # The edges, named
///
/// * **A root with no rows is skipped, not returned.** A daemon writes the session row at
///   startup and nobody may ever prompt into it; those rows are often the newest in the table,
///   and picking one answers `--continue` with an empty screen — indistinguishable from resume
///   being broken. This is the rule that was already here; it is unchanged.
/// * **A child that is newer than every root does not win, and does not lift its parent's
///   time either.** The pick is the newest root's own last utterance. Promoting a root by its
///   children's activity is a second notion of recency that would let a reviewer's clock decide
///   where the person lands — which is the defect being fixed, one step removed.
/// * **A child whose parent is gone is still skipped.** An orphan row is a sub-session that
///   lost its parent, not a conversation that grew one; it is reachable by id (`--session ID`,
///   and the picker nests it under nothing) and is never what `--continue` opens.
/// * **A workspace holding nothing but children has nothing to continue**, and the sentence for
///   it says exactly that rather than "no stored session" — see [`nothing_to_continue`].
fn newest_conversation(
    mut all: Vec<letibot_tokencore::store::StoredSession>,
    scope: Option<&std::path::Path>,
) -> Latest {
    // An **exact** workspace match beats a descendant of it. Standing in
    // `~` and asking to continue should not hand back the conversation you
    // were having in `~/Projects/rano` while a `~` conversation exists; the
    // subtree match is the fallback that makes `--continue` work from a
    // subdirectory, not a licence to reach downwards past a closer answer.
    if let Some(root) = scope {
        all.sort_by_key(|s| std::path::Path::new(&s.workspace_root) != root);
    }
    let in_scope = all.len();
    let mut hidden = 0usize;
    let mut empty = 0usize;
    let mut rows = Vec::with_capacity(all.len());
    for s in all {
        if !is_conversation(&s) {
            hidden += 1;
        } else if s.items == 0 {
            empty += 1;
        } else {
            rows.push(s);
        }
    }
    Latest {
        pick: rows.into_iter().next(),
        in_scope,
        hidden,
        empty,
    }
}

/// **Why there is nothing to continue**, as a sentence a person can act on.
///
/// Three stores produce the same `None` and only one of them means *there is nothing here*, so
/// the answer names which one this is. The remedy matters more than the diagnosis: a store
/// whose every row is a sub-session is a store where `--sessions` shows nothing and a bare
/// `letibot` starts a conversation, and a person told only "no stored session" would go looking
/// for a fault in the store that is not there.
fn nothing_to_continue(latest: &Latest, scope: Option<&std::path::Path>) -> String {
    let here = scope
        .map(|p| format!(" under {}", p.display()))
        .unwrap_or_default();
    if latest.in_scope == 0 {
        return format!("no stored session{here}");
    }
    if latest.hidden == latest.in_scope {
        return format!(
            "the {} session(s){here} are all sub-sessions — task children and the merge \
             queue's reviewers — and a sub-session is not a conversation to continue. \
             `letibot` starts a new one here, `--sessions` lists the conversations on disk, \
             and `--session ID` opens a sub-session by id.",
            latest.in_scope
        );
    }
    format!(
        "every conversation{here} is empty: {} session(s) with no rows, nothing ever said \
         in them. `letibot` starts a new one, and `--sessions` shows what is on disk.",
        latest.empty
    )
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

    /// A session row with the four fields the resolution reads: where it was opened, what it is
    /// called, the seat it was recorded in, and the session that spawned it.
    fn session_row(
        store: &Store,
        id: &str,
        workspace: &str,
        title: Option<&str>,
        role: Option<&str>,
        parent: Option<&str>,
    ) {
        store
            .put_session(&SessionRecord {
                id: id.into(),
                title: title.map(str::to_string),
                model_id: "m".into(),
                dialect_sha: "d".into(),
                workspace_root: workspace.into(),
                owner: "dead".into(),
                approvers: vec![],
                role: role.map(str::to_string),
                parent_session_id: parent.map(str::to_string),
            })
            .expect("session");
    }

    /// **Give a session rows, and make sure the store's clock has passed `after_ms` FIRST.**
    ///
    /// Returns the `last_activity_ms` the store then reports, which is what the resolution orders
    /// by.
    ///
    /// The wait comes *before* the rows, and that is the whole of the correctness here.
    /// `created_at` is stamped once, when a row is appended, so a fixture that waits afterwards
    /// is waiting for a number that cannot change: two sessions written inside the same
    /// millisecond then tie, and a tie is broken by the order the session rows were inserted —
    /// which is the order a fixture writes them in. A test that means *the child is NEWER than
    /// its parent* would therefore pass with the rule removed. (It did more than that the first
    /// time this was written: the wait was after the append, the reviewer landed 0.3 ms behind
    /// its host, the stamps came out equal and the loop spun for ever.)
    fn rows_for(store: &Store, id: &str, rows: u32, after_ms: i64) -> i64 {
        wait_past(after_ms);
        let prefix = store
            .put_stable_prefix(&letibot_tokencore::store::StablePrefixRecord {
                dialect_sha: "d".into(),
                system: format!("system of {id}"),
                tools_json: vec![],
                tokens: vec![1, 2, 3],
                h_init: [0u8; 32],
                vocab_source: "test".into(),
            })
            .expect("prefix");
        let transcript = format!("{id}#t0");
        store
            .put_transcript(&transcript, id, &prefix)
            .expect("transcript");
        for seq in 0..rows {
            let tokens: Vec<u32> = vec![7, 8, 9];
            store
                .append_item(
                    &transcript,
                    seq,
                    &letibot_transcript::TranscriptItem::User {
                        speaker: Default::default(),
                        parts: vec![letibot_transcript::UserPart::Text {
                            text: format!("row {seq} of {id}"),
                        }],
                    },
                    &letibot_tokencore::ledger::LedgerRow {
                        item_id: format!("{transcript}.{seq}"),
                        tok_offset: seq * 3,
                        tok_len: 3,
                        h_k: [0u8; 32],
                    },
                    &tokens,
                )
                .expect("a row");
        }
        // Read back what the store says rather than what this function believes: the stamp is
        // the store's, and it is the number the resolution orders by.
        store
            .session(id)
            .expect("reading")
            .expect("the row just written")
            .last_activity_ms
    }

    /// Wait until the store's own clock has passed `ms` — for a fixture whose time is stamped at
    /// write time and cannot be moved afterwards.
    fn wait_past(ms: i64) {
        let now = || {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0)
        };
        while now() <= ms {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    struct TempDir(std::path::PathBuf);
    impl TempDir {
        /// **A counter in the name, not only the clock.** The tests in this module run in
        /// parallel in one process, and macOS's `SystemTime` has microsecond resolution: two
        /// that started in the same microsecond shared a directory, and the first to finish
        /// removed the other's store under it — `an_empty_scope_is_refused…` failed `opening`
        /// one workspace run in a few. MEASURED 2026-10-08.
        fn new() -> Self {
            static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let p = std::env::temp_dir().join(format!(
                "harnessd-scope-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
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

    /// **A `main_model` from a file lands on the right `cfg` field.** A
    /// `provider/model` name sets the metered provider, and a bare alias sets the
    /// local model and clears the provider, because the two are the two ways a
    /// session's turns go and a name is one or the other, not both. A regression
    /// here would be a file that set a model the session did not run on, which is
    /// the defect this feature exists to forbid.
    #[test]
    fn a_file_main_model_lands_on_the_right_config_field() {
        // A `provider/model` name sets the metered provider.
        let mut cfg = Config::for_this_box("/tmp");
        assert_eq!(
            apply_main_model(&mut cfg, "deepseek/deepseek-chat", None),
            None
        );
        let pc = cfg
            .provider
            .expect("a provider/model name sets the provider");
        assert_eq!(pc.name, "deepseek");
        assert_eq!(pc.model.as_deref(), Some("deepseek-chat"));
        assert!(
            pc.api_key.is_none(),
            "the key is resolved along the usual path"
        );

        // A bare alias sets the local model and clears the provider. **The clearing
        // is the whole point**: this is the line that takes a session off
        // `[default] provider = deepseek` and back onto the box's own model.
        let mut cfg = Config::for_this_box("/tmp");
        cfg.provider = Some(crate::config::ProviderConfig {
            name: "deepseek".into(),
            model: Some("deepseek-chat".into()),
            api_key: None,
            thinking: false,
        });
        assert_eq!(
            apply_main_model(&mut cfg, "qwen-3.8-flash-next", None),
            None
        );
        assert_eq!(cfg.model, "qwen-3.8-flash-next");
        assert!(cfg.provider.is_none(), "a bare alias is a local session");
    }

    /// **A file's alias does not overwrite the binding `--model` set, and says so.**
    ///
    /// The launcher passes `--model` on every start, and what it names is the
    /// vocabulary this daemon tokenises with. A file that re-bound it would point the
    /// GGUF at weights the server is not serving — `400 Prompt contains invalid
    /// tokens` at the first turn — so the binding stands, the provider is still
    /// cleared (the half that decides who answers), and the disagreement is returned
    /// for the banner rather than resolved in silence.
    #[test]
    fn a_file_alias_does_not_rebind_the_command_line_and_says_so() {
        let mut cfg = Config::for_this_box("/tmp");
        cfg.provider = Some(crate::config::ProviderConfig {
            name: "deepseek".into(),
            model: Some("deepseek-chat".into()),
            api_key: None,
            thinking: false,
        });
        let note = apply_main_model(&mut cfg, "glm-5.3-flash", Some("qwen-3.8-flash-next"))
            .expect("a disagreement is said, not swallowed");
        assert!(
            note.contains("glm-5.3-flash") && note.contains("qwen-3.8-flash-next"),
            "the sentence names both: {note}"
        );
        assert_eq!(
            cfg.model, "qwen-3.8-flash-next",
            "the binding is the command line's"
        );
        assert!(
            cfg.provider.is_none(),
            "and the file still cleared the provider"
        );

        // Agreeing is not a disagreement: the file's alias IS the binding, and it
        // applies with nothing to say.
        let mut cfg = Config::for_this_box("/tmp");
        assert_eq!(
            apply_main_model(&mut cfg, "glm-5.3-flash", Some("glm-5.3-flash")),
            None
        );
        assert_eq!(cfg.model, "glm-5.3-flash");
    }

    /// **A model a file named that this box cannot reach is named at startup.**
    ///
    /// The three shapes the tree has paid for: a provider this build does not know, a
    /// provider with no key here, and a local alias the server is not serving. Each is
    /// a sentence naming the value; the reachable cases are silent, because a warning
    /// that cries wolf is worse than none.
    #[test]
    fn a_model_a_file_named_that_cannot_be_reached_is_named() {
        let cfg = Config::for_this_box("/tmp");

        // A provider this build does not know.
        let why = unreachable_provider("notaprovider/x").expect("an unknown provider is named");
        assert!(why.contains("notaprovider"), "{why}");

        // A provider this build knows, with no key on this box. `grok` is the one
        // preset whose key this tree documents as absent on the operator's box — the
        // opencode entry is `type: oauth`, and an oauth entry is deliberately not read
        // as a key. The test asserts the SHAPE rather than the key: whatever this box
        // resolves, the answer is either `None` (a key is here) or a sentence naming
        // the provider (no key is).
        if let Some(why) = unreachable_provider("grok/grok-4-fast") {
            assert!(why.contains("grok"), "{why}");
        }

        // A local alias the server is not serving, with no block to switch to.
        let why = unreachable_local(&cfg, "no-such-alias", Some("qwen-3.8-flash-next"))
            .expect("a local alias nothing serves is named");
        assert!(why.contains("no-such-alias"), "{why}");
        assert!(
            why.contains("400"),
            "and it says what the turn would be: {why}"
        );

        // The reachable cases are silent: the word `local`, an alias the server is
        // serving, a `[model."..."]` block with an address, and any local name at all
        // when the file did not set `main_model`.
        assert_eq!(unreachable_local(&cfg, "local", None), None);
        assert_eq!(
            unreachable_local(&cfg, "qwen-3.8-flash-next", Some("qwen-3.8-flash-next")),
            None,
            "the alias the server reports"
        );
        assert_eq!(unreachable_provider("glm-5.3-flash"), None);
    }

    /// **`unreachable_models` asks the local half only of `main_model`.** The guard's
    /// model answers at the `[gatekeeper]` endpoint and a subagent's is decided per
    /// spawn, so comparing either against this daemon's own server would be a false
    /// alarm — and a banner that cries wolf is a banner nobody reads.
    #[test]
    fn only_main_model_is_checked_against_this_daemons_own_server() {
        use crate::leticode_config::LeticodeConfig;
        let cfg = Config::for_this_box("/tmp");
        let mut files = LeticodeConfig::default();
        files.main_model = Some("no-such-alias".into());
        files.subagent_model = Some("also-not-served".into());
        files.gatekeeper_model = Some("qwen-3.8-27b".into());
        let found = unreachable_models(&cfg, &files, Some("qwen-3.8-flash-next"));
        assert_eq!(
            found.len(),
            1,
            "one fault, and it is main_model's: {found:?}"
        );
        assert!(found[0].contains("no-such-alias"), "{found:?}");
        assert!(
            !found.iter().any(|w| w.contains("also-not-served")),
            "a subagent's model is decided per spawn, not here: {found:?}"
        );
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

    /// **The operator's report, as a test.** Ten reviewers spawned into one workspace by the
    /// merge queue made the newest row in it a reviewer, and `--continue` — which asked only for
    /// the newest row — reopened a review instead of the conversation that enqueued the work:
    /// *"restart pg-noop and was brought to one of the gatekeeper sessions"*.
    #[test]
    fn a_reviewer_is_never_the_conversation_continue_reopens() {
        let (store, _d) = store_with(&[]);
        session_row(
            &store,
            "s-mine",
            "/ws",
            Some("the conversation"),
            None,
            None,
        );
        let mine = rows_for(&store, "s-mine", 3, 0);

        // The queue's reviewer: a child of that conversation, seated `gatekeeper`, its title cut
        // from the review brief — and NEWER than anything the person said.
        session_row(
            &store,
            "s-gk",
            "/ws",
            Some(&format!(
                "{} a subagent has finished work on a branch",
                letibot_sessionlog::GATEKEEPER_TITLE_PREFIX
            )),
            Some("gatekeeper"),
            Some("s-mine"),
        );
        let reviewer = rows_for(&store, "s-gk", 40, mine);
        assert!(
            reviewer > mine,
            "the fixture's reviewer has to be the newer row, or this proves nothing"
        );

        let latest = newest_conversation(
            store.list_sessions().expect("listing"),
            Some(std::path::Path::new("/ws")),
        );
        assert_eq!(
            latest.pick.as_ref().map(|s| s.id.as_str()),
            Some("s-mine"),
            "a reviewer is a session and not a conversation"
        );
        assert_eq!(
            latest.hidden, 1,
            "and it is counted as skipped, not quietly dropped"
        );
    }

    /// **The seat carries the mark where the title cannot.** A reviewer the person renamed keeps
    /// the seat its row recorded, and a head rebuilding a row from the wire has no role to read
    /// at all — only the brief's first words. Both marks are therefore applied, and this is the
    /// half the title alone cannot cover.
    #[test]
    fn a_reviewer_whose_title_was_changed_is_still_not_a_conversation() {
        let (store, _d) = store_with(&[]);
        session_row(
            &store,
            "s-mine",
            "/ws",
            Some("the conversation"),
            None,
            None,
        );
        let mine = rows_for(&store, "s-mine", 2, 0);
        // No parent and a title of the operator's own: nothing but the seat says what this is.
        session_row(
            &store,
            "s-gk",
            "/ws",
            Some("review of agent/idle-clock"),
            Some("gatekeeper"),
            None,
        );
        let reviewer = rows_for(&store, "s-gk", 9, mine);
        assert!(reviewer > mine, "the reviewer is the newer row");

        let latest = newest_conversation(
            store.list_sessions().expect("listing"),
            Some(std::path::Path::new("/ws")),
        );
        assert_eq!(
            latest.pick.as_ref().map(|s| s.id.as_str()),
            Some("s-mine"),
            "the seat is the mark that survives a rename"
        );
    }

    /// **A task child that is newer than its parent is not the answer**, and it does not lift
    /// its parent's time either: the pick is the newest conversation's own last utterance, and
    /// promoting a root by its children's activity would let a reviewer's clock decide where the
    /// person lands.
    #[test]
    fn a_task_child_newer_than_its_parent_is_not_the_conversation() {
        let (store, _d) = store_with(&[]);
        session_row(
            &store,
            "s-mine",
            "/ws",
            Some("the conversation"),
            None,
            None,
        );
        let mine = rows_for(&store, "s-mine", 2, 0);
        // A plain `task` child: no gatekeeper mark anywhere, a parent and nothing else.
        session_row(
            &store,
            "s-task",
            "/ws",
            Some("survey the crate"),
            Some("coder"),
            Some("s-mine"),
        );
        let child = rows_for(&store, "s-task", 12, mine);
        assert!(child > mine, "the child is the newer row");

        let latest = newest_conversation(
            store.list_sessions().expect("listing"),
            Some(std::path::Path::new("/ws")),
        );
        assert_eq!(
            latest.pick.as_ref().map(|s| s.id.as_str()),
            Some("s-mine"),
            "the parent is where a person continues, however recently the child worked"
        );
        assert_eq!(latest.hidden, 1);
    }

    /// **A child whose parent is gone is still a child**, and a workspace holding nothing else
    /// has nothing to continue. What matters here is the sentence: it names the case and gives a
    /// remedy, because the alternative — `no stored session` over a store that is not empty —
    /// sends a person looking for a fault that is not there.
    #[test]
    fn a_workspace_holding_nothing_but_children_says_so() {
        let (store, _d) = store_with(&[]);
        // The parent row was deleted (`--delete` takes a session with no rows) or lives in
        // another store; either way this row is a sub-session with nobody above it.
        session_row(
            &store,
            "s-orphan",
            "/ws",
            Some("a child of a session that is gone"),
            Some("coder"),
            Some("s-parent-nobody-has"),
        );
        rows_for(&store, "s-orphan", 4, 0);

        let latest = newest_conversation(
            store.list_sessions().expect("listing"),
            Some(std::path::Path::new("/ws")),
        );
        assert!(latest.pick.is_none(), "an orphan is not a conversation");
        let said = nothing_to_continue(&latest, Some(std::path::Path::new("/ws")));
        assert!(
            said.contains("sub-session"),
            "it names what is there: {said}"
        );
        assert!(said.contains("/ws"), "and where: {said}");
        assert!(
            said.contains("--session ID") && said.contains("--sessions"),
            "and what a person can do about it: {said}"
        );

        // A store with nothing in it at all is a different sentence, and the old one.
        let empty = newest_conversation(Vec::new(), Some(std::path::Path::new("/ws")));
        assert!(empty.pick.is_none());
        assert_eq!(
            nothing_to_continue(&empty, Some(std::path::Path::new("/ws"))),
            "no stored session under /ws"
        );
    }

    /// **A conversation nobody ever said anything in is skipped** — the rule that was here
    /// before the reviewer was, pinned so the narrowing above cannot quietly drop it. These rows
    /// are often the NEWEST in a store: a daemon writes one at startup, and picking it answers
    /// `--continue` with an empty screen.
    #[test]
    fn a_conversation_with_no_rows_is_not_continued() {
        let (store, _d) = store_with(&[]);
        session_row(&store, "s-old", "/ws", Some("something I said"), None, None);
        let said = rows_for(&store, "s-old", 5, 0);
        // Opened after it, and never prompted into: a row whose time is its creation time, so
        // the clock has to have moved before it is written.
        wait_past(said);
        session_row(&store, "s-blank", "/ws", None, None, None);
        assert!(
            store
                .session("s-blank")
                .expect("reading")
                .expect("the row")
                .last_activity_ms
                > said,
            "the empty conversation has to be the newer row"
        );

        let latest = newest_conversation(
            store.list_sessions().expect("listing"),
            Some(std::path::Path::new("/ws")),
        );
        assert_eq!(latest.pick.as_ref().map(|s| s.id.as_str()), Some("s-old"));
        assert_eq!(latest.empty, 1);
    }

    /// The exact workspace still beats a descendant of it — asserted here because that sort
    /// moved into [`newest_conversation`] and an untested move is a rule nobody is holding.
    #[test]
    fn an_exact_workspace_beats_a_descendant_of_it() {
        let (store, _d) = store_with(&[]);
        session_row(&store, "s-here", "/ws", Some("here"), None, None);
        let here = rows_for(&store, "s-here", 2, 0);
        session_row(
            &store,
            "s-below",
            "/ws/crates/ui",
            Some("below"),
            None,
            None,
        );
        let below = rows_for(&store, "s-below", 2, here);
        assert!(below > here, "the descendant is the newer row");

        let latest = newest_conversation(
            store.list_sessions().expect("listing"),
            Some(std::path::Path::new("/ws")),
        );
        assert_eq!(
            latest.pick.as_ref().map(|s| s.id.as_str()),
            Some("s-here"),
            "the subtree match is a fallback, not a licence to reach past a closer answer"
        );
    }
}
