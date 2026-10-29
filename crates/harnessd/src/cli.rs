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

use crate::config::{
    AdjudicatorChoice, Config, Disclosure, Reviewers, Seat, SpillPolicy, SpillStorage,
};
use crate::{Daemon, Dialect, Outcome, OutcomeSaid, Parts, Sessions};
use letibot_sessionlog::registry::Registry;
use letibot_turn::Endpoint;

/// Resolve the credentials and open the seat. Every refusal in here names what
/// was looked at, because "no flowy" and "the wrong flowy" must not read alike.
/// **The daemon's boot, said on the first session's hub** (TODO R92.2, R92.3).
///
/// The socket binds before the session opens — a second daemon must find out it is second before
/// a vocabulary is loaded — and the launcher execs the head as soon as it listens. So a head is
/// attached to a session that cannot run anything until the worker starts, sometimes for minutes
/// (a model server probe has a 3-minute read timeout; a long transcript is re-tokenised on
/// resume). Each step is published as `daemon_starting` (`starting: <step>`), the end as
/// `daemon_started`, and a failure's reason as `daemon_start_failed` BEFORE the hubs close, so the
/// head shows why instead of a closed connection. Warnings rather than a new event: they are in
/// the hub's log, so a head that attaches mid-boot replays the step it missed, and no protocol
/// changes.
struct Boot {
    hub: Option<std::sync::Arc<letibot_sessionlog::hub::Hub>>,
}

impl Boot {
    fn new(registry: &letibot_sessionlog::registry::Registry, session_id: &str) -> Boot {
        Boot {
            hub: registry.get(session_id),
        }
    }

    fn say(&self, code: &str, detail: String) {
        if let Some(h) = &self.hub {
            h.publish(letibot_sessionlog::SessionEvent::Warning {
                code: code.into(),
                detail,
                compaction: None,
            });
        }
    }

    /// The next step of the boot.
    fn step(&self, what: impl std::fmt::Display) {
        eprintln!("  starting: {what}");
        self.say("daemon_starting", format!("starting: {what}"));
    }

    /// The worker is about to run: a prompt is taken from now on.
    fn done(&self) {
        self.say(
            "daemon_started",
            "started: the daemon takes prompts now".into(),
        );
    }

    /// **Why the boot failed, on the hub, and a moment for a head to read it** before the caller
    /// shuts the daemon down. Answers `why`, for the caller's `Err`.
    fn failed(&self, why: String) -> String {
        self.say(
            "daemon_start_failed",
            format!("the daemon did not start: {why}"),
        );
        // The head drains its queue on its own thread; the hubs close at `shutdown`.
        std::thread::sleep(std::time::Duration::from_millis(750));
        why
    }
}

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
pub(crate) fn apply_main_model(cfg: &mut Config, model: &str, bound: Option<&str>) -> Option<String> {
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

/// **What `--reviewers WORD` does to a `Config`** — the flag's whole body, in a function so the
/// one thing it must get right is a value a test can read.
///
/// Two spellings and nothing else ([`Reviewers::parse`] refuses anything else BY NAME, with both
/// words in the sentence — a flag whose only wrong values are silent is the shape this tree keeps
/// closing). **`vm` goes through [`Config::allow_vm_reviewers`]**, which is the named way a value
/// becomes `Vm` — the same function a test that means to exercise a VM placement calls — so the
/// flag and the test ask for the microVM by the same door.
///
/// The flag wins over the project file because of where it is parsed: `Config::for_the_daemon`
/// reads `[queue] reviewers` first and the argument loop runs after it, so this overwrites that
/// value — and `host` overwrites it too, which is the half a daemon seated above several
/// repositories needs (there is one file it could read and several projects it serves).
fn apply_reviewers_flag(cfg: &mut Config, word: &str) -> Result<(), String> {
    match Reviewers::parse(word)? {
        Reviewers::Vm => cfg.allow_vm_reviewers(),
        Reviewers::Host => cfg.reviewers = Reviewers::Host,
    }
    Ok(())
}

/// **A `provider/model` name a file set that this box cannot reach**, as a sentence.
///
/// No network and no endpoint, so this is asked of every model name in either file:
/// the provider must be one this build knows, and a key for it must resolve here.
/// The alternative is the failure the tree has paid for three times — a *plausible*
/// string reaching the first turn and coming back as a `401` that names neither the
/// file nor the line that wrote it.
pub(crate) fn unreachable_provider(model: &str) -> Option<String> {
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
     \x20 --restore-within 3600     seconds the daemon may have been down for a\n\
     \x20                           session's subagents and monitors to be restored on\n\
     \x20                           their own at the next start; past it the model is\n\
     \x20                           only reminded of what was running\n\
     \x20 --idle-exit 3600          seconds with no head, turn, job, subagent, monitor\n\
     \x20                           or queue work after which the daemon exits; its\n\
     \x20                           sessions stay in the store. 0 never exits\n\
      \x20 --no-queue                serve NO merge or review queue: no lease is\n\
      \x20                           claimed, no target gated and no branch landed\n\
      \x20                           here, and this daemon's sessions are told no\n\
      \x20                           queue news (a package worker must not be set\n\
      \x20                           to fixing a `main` nobody asked it about). The\n\
      \x20                           store watcher stays, so a head still moves\
     \x20 --reviewers host|vm     where this daemon's REVIEWERS run. `host` (the\
     \x20                           default) is a reviewer child on this machine, in\
     \x20                           a checkout of the head it judges -- as the\
     \x20                           subagents that author the branches already run;\
     \x20                           `vm` is each reviewer in a firecode microVM,\
     \x20                           which asks nobody for anything and costs a guest\
     \x20                           exec per read. The project's `[queue] reviewers`\
     \x20                           says the same thing, and this flag overrides it\n\
     \x20 --gate-host NAME         run this daemon's MERGE GATE on a machine the\n\
     \x20                           operator declared in hosts.toml, instead of on\n\
     \x20                           this box. The landing tree goes over that host's\n\
     \x20                           own key and root and the steps run there with\n\
     \x20                           their build cache; the merge itself stays here.\n\
     \x20                           Unset -- the default -- is this machine. The\n\
     \x20                           project's `[queue] gate_host` says the same\n\
     \x20                           thing, and this flag overrides it;\n\
     \x20                           `--gate-host local` (or an empty name) puts\n\
     \x20                           it back on this machine\n\
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
     \x20 --where host|firecode     where this session's tools run. firecode runs them\n\
     \x20                           in a VM, the boundary: everything is allowed inside.\n\
     \x20                           On a Mac the workspace itself is mounted read-write\n\
     \x20                           (every repository in it; nothing else of the host),\n\
     \x20                           joining a VM already up for it; elsewhere it is a\n\
     \x20                           COPY, delivered as a sibling directory at the end\n\
     \x20 --vm-copy                 a copy even on a Mac, as before\n\
     \x20 --vm-arg ARG              passed to `firecode up` verbatim, repeatable:\n\
     \x20                           --vm-arg --mem --vm-arg 8192 --vm-arg --add-dir\n\
     \x20                           --vm-arg ~/.rustup (a toolchain the guest lacks)\n\
     \x20 --host NAME               where this session's TOOLS run: on the ssh host\n\
     \x20                           NAME, declared by the operator under [host.NAME]\n\
     \x20                           in ~/.config/letibot/hosts.toml. The model, the\n\
     \x20                           store and the gate stay here; bash, read, write,\n\
     \x20                           jobs and the rest run there, and the repository's\n\
     \x20                           HEAD goes over as a checkout (branch remote/<id>)\n\
     \x20                           whose commits come back as remote/NAME/<id>\n\
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
     \x20 --list-sessions [--tsv]   what is on disk, GROUPED: mine first, then the\n\
     \x20                           harness's own work (subagents, the review queue,\n\
     \x20                           packages, one-shots) with a count each. `--tsv` is\n\
     \x20                           the plain five-column data form, unchanged\n\
     \x20 --latest-session          the id of the newest one, scoped by --workspace\n\
     \x20 --rename ID TITLE         name a session, or clear it with an empty title\n\
     \x20 --delete ID               remove a session that has no rows\n\
     \n\
     claude code, both directions (no socket, no model):\n\
     \x20 --list-claude [--tsv]     what Claude Code has on this box: the id `--session\n\
     \x20                           cc-<id>` takes, the model, and the first thing said\n\
     \x20 --handover SESSION DIR    write that session out as a transcript\n\
     \x20                           `claude --resume <path>` opens, plus a Markdown copy\n\
     \x20 --note TRANSCRIPT TEXT    append one line to a transcript, with a backup first\n\
     \x20 --revert TRANSCRIPT BAK   put a transcript back from that backup, byte for byte"
        .into()
}

/// One scripted step, in the order the flags were given.
enum Step {
    Prompt(String),
    Slash(String),
}

/// **How long the daemon's exit waits for the subagent threads it started**, and why that
/// number.
///
/// The wait is [`Sessions::drain_subagents`]'s, and the reason there is one at all is the
/// process's exit: `exit()` runs every ELF finalizer, so a child still inside libllama's
/// tokenizer when `_dl_fini` unloads `libggml-cuda` takes SIGSEGV — the crash measured on
/// `message_between_turns`, `si_addr: 0x3`, after every test had printed `ok`.
///
/// **Why 5 seconds, from the two ends of the case.** The drain stops the children first, so
/// what this waits on is a child that has been TOLD to stop. Three kinds of child answer at
/// once: one parked between turns leaves on the closed hub, one between rounds leaves at its
/// next round boundary — the round loop already polls its steering on 250 ms slices
/// (`Harness::sleep_unless_closed`) — and one in a tool call leaves when that call returns.
/// Several times the slice covers the first two, and it is longer than any single tokenization
/// of a prompt this box has measured. What is left is the child inside ONE model call or one
/// long tool, which cannot be interrupted at all: a local turn here has been measured in
/// minutes, so a bound large enough for that case would make every Ctrl-C wait minutes for a
/// case a larger number cannot fix anyway. Past this the exit gives up, says which session it
/// could not wait for, and takes the crash risk rather than the hang — the same ruling
/// `drop(merge_queue)` makes below, for the same reason: a daemon that will not stop is worse
/// than a row the next daemon recovers.
const SUBAGENT_EXIT_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// **The daemon's memory caps, said once, by the daemon** — R64/R65.
///
/// This sentence used to be printed by the `harnessd` binary's `main`, so every short-lived
/// invocation of that binary said it: `--continue` asks the same binary for the newest session id
/// (`--latest-session`), and the operator's terminal got *"harnessd: memory caps OFF — …"* from
/// the ask and then the same line from the daemon that actually started, twice around a banner
/// that was already being drowned by other lines (measured 2026-10-26).
///
/// So the delegation happens in [`run`], before anything can start a child, and the WORDS are
/// said here — past every verb that answers and exits, at the top of the daemon's own log.
fn say_memory_caps() {
    match letibot_tools::exec::delegate_memory() {
        Ok(scope) => eprintln!(
            "harnessd: memory caps on — the tool calls' cgroups live in {}",
            scope.display()
        ),
        Err(why) => eprintln!("harnessd: memory caps OFF — {why}"),
    }
}

pub fn run(args: &[String]) -> Result<i32, String> {
    // **Step into the daemon's own cgroup leaf before anything here can start a child** (R64/R65)
    // — `delegate_memory` refuses when the cgroup already holds another pid, so a `git` the
    // startup ran first would turn every tool call of this daemon's life uncapped. Said by
    // [`say_memory_caps`], on the daemon's path only: see that function for why the sentence
    // moved out of the `harnessd` binary's `main`.
    let _ = letibot_tools::exec::delegate_memory();
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    // **`for_the_daemon`, and not `for_this_box`.** The daemon is the one caller that is a live
    // operator's box running the review queue, so this is where the PROJECT's answer to *where do
    // my reviewers run* is read (`[queue] reviewers`) — every fixture builds its `Config` with
    // `for_this_box`, which says `host`, and a test that means to exercise a VM placement asks for
    // one by name (`Config::allow_vm_reviewers`). `--reviewers vm` below overrides the file. See
    // `Config::reviewers` for the whole of the reason.
    let mut cfg = Config::for_the_daemon(cwd);
    // **The guard model comes from config, not from a flag.**
    //
    // `[gatekeeper] endpoint = "HOST:PORT"` in `~/.config/letibot/providers.toml`,
    // beside the provider keys, because an endpoint is a property of the box and not
    // of an invocation. `--oracle` still overrides it below, for a one-off. A
    // missing section, an unreadable file and a malformed address are all "no
    // guard" — this is read at startup and must never be a reason a daemon does not
    // start.
    // `[memory]`: the cap a model's tool call runs under, and the most it may ask for without
    // the operator. A malformed value refuses the start BY NAME, for `[gatekeeper]`'s reason
    // below: a typo that left the built-in in force while the operator believes they set
    // their own is the banner-says-one-thing defect.
    {
        let m = letibot_provider::memory(None);
        let parse = |key: &str,
                     v: &Option<String>|
         -> Result<Option<letibot_tools::exec::memory::Amount>, String> {
            v.as_deref()
                .map(|s| {
                    letibot_tools::exec::memory::Amount::parse(s).map_err(|e| {
                        format!(
                            "[memory] {key} in {}: {e}",
                            letibot_provider::keys::config_file().display()
                        )
                    })
                })
                .transpose()
        };
        if let Some(a) = parse("default", &m.default)? {
            cfg.memory.default = a;
            cfg.memory.default_from = letibot_tools::exec::memory::PolicySource::Box;
        }
        if let Some(a) = parse("ceiling", &m.ceiling)? {
            cfg.memory.ceiling = a;
        }
        // **The merge gate's own cap** (R76 item 6): every gate step runs in a cgroup of its
        // own beside the daemon's leaf, under this — half of what is free when the gate starts
        // unless `gate` says otherwise (`uncapped` keeps the scope and the peak, with no cap).
        let gate = letibot_tools::exec::memory::Ask::parse(m.gate.as_deref()).map_err(|e| {
            format!(
                "[memory] gate in {}: {e}",
                letibot_provider::keys::config_file().display()
            )
        })?;
        crate::mergequeue::gate::set_gate_memory(gate);
        // **The daemon's own throttle** (R66): `memory.high` on its leaf, so a daemon that grows
        // without bound is slowed by the kernel instead of taking the box — 16 GiB unless
        // `daemon_high` says otherwise (`max` turns it off). Only in a delegated scope; anywhere
        // else there is no leaf of the daemon's own to set it on, and `memwatch` still logs.
        let high = match m.daemon_high.as_deref().map(str::trim) {
            Some("max") => None,
            other => {
                let total = letibot_tools::exec::memory::Meminfo::read()
                    .map(|i| i.total)
                    .unwrap_or(0);
                let a = parse("daemon_high", &other.map(str::to_string))?
                    .unwrap_or(letibot_tools::exec::memory::Amount::Bytes(16 << 30));
                Some(a.resolve(total))
            }
        };
        if let (Ok(scope), Some(bytes)) = (letibot_tools::exec::delegate_memory(), high) {
            let leaf = scope.join(letibot_tools::exec::DAEMON_LEAF);
            match std::fs::write(leaf.join("memory.high"), bytes.to_string()) {
                Ok(()) => eprintln!(
                    "harnessd: the daemon's own memory.high is {}",
                    letibot_tools::exec::human_bytes(bytes)
                ),
                Err(e) => eprintln!("harnessd: the daemon's memory.high could not be set: {e}"),
            }
        }
    }
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

    // `--vm-copy` said anywhere on the line wins over `--where firecode`'s mount, before it or
    // after: the operator who asked for a copy must never get the folder itself read-write.
    let mut vm_copy = false;

    let mut it = args.iter().cloned();
    while let Some(arg) = it.next() {
        let mut next = || it.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--list-sessions" => query = Some(Query::List),
            // **What Claude Code has on this box** (Package H) — the ids `--session cc-<id>`
            // takes, with the model, the directory and the first thing that was said. Beside
            // `--list-sessions` because it answers the same question about a different store,
            // and it needs no store of ours at all, so it is answered before one is opened.
            "--list-claude" => query = Some(Query::ListClaude),
            // **The return trip**: this session's conversation as a transcript
            // `claude --resume <path>` opens, plus a Markdown copy a person can read.
            "--handover" => {
                let session = next()?;
                let to = PathBuf::from(next()?);
                query = Some(Query::Handover { session, to });
            }
            // **The lighter shape the operator chose**: one line on the ORIGINAL transcript
            // saying the session was continued here, plus the Markdown handover for the
            // fallback. The line's own words are composed, not typed — see
            // `note_continued`.
            "--note-continued" => {
                let session = next()?;
                let transcript = PathBuf::from(next()?);
                let dir = it.next().map(PathBuf::from);
                query = Some(Query::NoteContinued {
                    session,
                    transcript,
                    dir,
                });
            }
            // **One line on the end of a transcript Claude Code owns.** A backup first, and no
            // write at all when a backup could not be made or proven — see
            // `letibot_claude::handover::append_note`.
            "--note" => {
                let transcript = PathBuf::from(next()?);
                let text = next()?;
                query = Some(Query::Note { transcript, text });
            }
            // **Put a transcript back, byte for byte**, from a backup this binary made.
            "--revert" => {
                let transcript = PathBuf::from(next()?);
                let backup = PathBuf::from(next()?);
                query = Some(Query::Revert { transcript, backup });
            }
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
            "--session" => {
                let named = next()?;
                // **`host` is the box's own agent, by name** — the room's coordinator, whose id is
                // derived from the machine (`s-room-<host key>`) so that `letibot --session host`
                // always resolves without the operator remembering a hash. The operator's ask:
                // *"a singleton with a special session name - 'host' so i can always connect to
                // it"*.
                cfg.session_id = if named.eq_ignore_ascii_case("host") {
                    crate::room::session_id()
                } else {
                    named
                };
            }
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
            // **How long the daemon may have been down for a session's work to come back on its
            // own** — see `crate::restart`. Seconds, because it is a span an operator types.
            "--restore-within" => {
                cfg.restore_within = std::time::Duration::from_secs(
                    next()?
                        .parse()
                        .map_err(|e| format!("--restore-within: {e}"))?,
                )
            }
            // **How long the daemon stays up with nothing to do** — see `Config::idle_exit_after`.
            // Seconds, and `0` for never, because a daemon an operator wants kept is one flag.
            "--idle-exit" => {
                let secs: u64 = next()?.parse().map_err(|e| format!("--idle-exit: {e}"))?;
                cfg.idle_exit_after = (secs > 0).then(|| std::time::Duration::from_secs(secs));
            }
            // **A daemon that serves no queue.** The merge and review queues belong to the
            // repository's session daemons, which means a daemon the Claude Code bridge starts
            // in a package's worktree is one of them by construction — and on 2026-10-26 the one
            // started for package P1 took letibot's lease, found `main` red, and put an
            // EMERGENCY in front of the package's model before its brief. A package worker is
            // not the repository's queue holder and must not be told to fix its `main`.
            //
            // It changes two things and nothing else: no `QueueService` is spawned (so no lease
            // is claimed, no gate is run and no landing happens here), and a `Harness` composes
            // no model-facing queue notice (see `Config::serve_queues`). The store watcher
            // stays, so a head attached here still redraws when another daemon moves a row.
            "--no-queue" => cfg.serve_queues = false,
            // **The box's room coordinator** (the room's slice 2). A whole daemon whose only job
            // is the room: its own socket, no store, no project, and one session of its own that
            // arbitrates the queues. See `crate::room::coordinator` for what it configures and
            // for the two things it refuses to start without.
            "--room" => cfg.room = true,
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
                let w = next()?;
                cfg.placement = letibot_tools::builtins::task::Placement::parse(&w)
                    .map_err(|e| format!("--where: {e}"))?;
                // `--where host:NAME` is `--host NAME` under another name, and the name
                // is checked the same way: one rule, whichever flag spelled it.
                if let Some(name) = cfg.placement.host() {
                    let hosts = letibot_tools::remote::Hosts::load();
                    if hosts.get(name).is_none() {
                        return Err(hosts.refusal(name));
                    }
                }
                // The session's VM works in the workspace itself, mounted (a Mac).
                cfg.vm_mount =
                    !vm_copy && cfg.placement == letibot_tools::builtins::task::Placement::Firecode;
            }
            // A copy of one repository instead of the workspace mounted - the old placement.
            "--vm-copy" => {
                vm_copy = true;
                cfg.vm_mount = false;
            }
            // **Where this daemon's REVIEWERS run** — `host` (the default: a reviewer child on
            // this machine, in a checkout of the head it judges, as the subagents that author the
            // branches already run) or `vm` (each reviewer booted in a firecode microVM).
            //
            // `--help` has advertised this flag since the two words were introduced and the
            // parser never had an arm for it: `harnessd --reviewers vm` answered *unknown
            // argument --reviewers*, which is a person being told their configuration is a typo
            // by a build whose own help says it is not. It is here now, and it is the same shape
            // as `--gate-host`: the project's `[queue] reviewers` is read by
            // `Config::for_the_daemon`, and this flag is parsed AFTER it, so the flag wins — and
            // `--reviewers host` puts it back, which is how a daemon seated above several
            // repositories overrides the one file it could read.
            "--reviewers" => apply_reviewers_flag(&mut cfg, &next()?)?,
            // **Where the merge gate runs.** A name from the operator's `hosts.toml`, checked
            // here for `--host`'s reason: a name nobody declared is refused by this line rather
            // than by a gate that fails on the first branch after it. `local` (or an empty name)
            // puts it back on this machine, which is how an operator overrides a project file
            // for one daemon.
            "--gate-host" => {
                let name = next()?;
                let name = name.trim().to_string();
                if name.is_empty() || name == "local" || name == "here" {
                    cfg.gate_host = None;
                } else {
                    let hosts = letibot_tools::remote::Hosts::load();
                    if hosts.get(&name).is_none() {
                        return Err(hosts.refusal(&name));
                    }
                    cfg.gate_host = Some(name);
                }
            }
            // **Where this session's TOOLS run, on another machine**
            // (`docs/remote-hosts.md`): `--host lab2x1`, the same shape as `--vm`. The
            // name must be one the operator declared, so it is resolved here — before a
            // daemon starts — and a name that is not in the file is refused by this
            // line rather than by a session that fails its first `bash`.
            "--host" => {
                let name = next()?;
                let hosts = letibot_tools::remote::Hosts::load();
                if hosts.get(&name).is_none() {
                    return Err(hosts.refusal(&name));
                }
                cfg.placement = letibot_tools::builtins::task::Placement::Remote(name);
                // A remote session is not in a VM: nothing is mounted and nothing is
                // copied — the tree goes over as a git bundle (`SshBackend::place`).
                cfg.vm_mount = false;
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

    // **A RUN OF STEPS IS A ONE-SHOT, and the session's tools are told so.** `--prompt` and
    // `--slash` are what `letibot "a question"` passes: the daemon runs the steps, prints the
    // answers and exits, and it exits with them. Configured HERE, off the steps themselves,
    // rather than left to whoever built the command line — a second flag meaning *one-shot*
    // could disagree with them, and this is the fact the tools act on (`InvokeCtx::one_shot`:
    // `bash` refuses `background: true`, because nothing will ever read that job's result).
    //
    // **And a one-shot serves NO queue.** It has no idle life to oversee one: it claims the
    // leases of every repository it sits above, runs one prompt and exits, and what it leaves
    // behind is a lease whose holder is dead — on 2026-10-26 a one-shot for one prompt held the
    // leases of six repositories under `~/Projects/stroppy-io`, and the operator's next daemon
    // had to give all six up (its log: *"gave up the lease of …/stroppy, … its heartbeat was
    // 90 s old and pid 28089 is not running"*). The same two things `--no-queue` takes away
    // (`crate::mergequeue::spawn_for`): no lease is claimed, no gate runs, no landing happens
    // here, and no model-facing queue notice is composed.
    cfg.one_shot = !steps.is_empty();
    if cfg.one_shot {
        cfg.serve_queues = false;
    }

    // **Where this daemon's gate runs**, told to the queue once — the queue builds one runner for
    // the whole daemon (`QueueEnv::production`), so this is the seam and `mergequeue::gate`
    // reads it. `None` is this machine and is today's behaviour exactly. Said AFTER the flags so
    // `--gate-host` is the last word over `[queue] gate_host`, which `Config::for_the_daemon`
    // read from the project's file.
    crate::mergequeue::gate::set_gate_host(cfg.gate_host.clone());

    // **The box's room coordinator** (the room's slice 2). Configured here, after every flag and
    // file has spoken and before anything is loaded or bound: a coordinator with no store, its own
    // socket and session id derived from the host, and a model it refuses to start without. See
    // `crate::room::coordinator::configure` for each decision and its reason.
    //
    // A `--room` daemon is not a folder's daemon, so it is not a one-shot either: `--prompt` with
    // `--room` would be a room that arbitrates one question and exits, which is not a thing anybody
    // wants and is not worth a second refusal.
    if cfg.room {
        crate::room::coordinator::configure(&mut cfg)?;
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
                    "{} — combined with what the operator declared in providers.toml, which a measurement adds to and never shrinks",
                    earned.evidence
                ),
            ),
        });
    }

    // One registry, seeded with the session named on the command line. A head can
    // make more over the socket; this one is the daemon's own, and it is opened
    // eagerly so that a dialect which does not fit the vocabulary is a startup
    // error rather than a failure on somebody's first prompt.
    // **The metrics writer** (TODO R81), before the registry: a session the registry creates
    // tells the writer its parent, and an emit before the writer is installed is a no-op.
    //
    // And the memory caps, said here rather than by the binary: this is the first line of the
    // DAEMON's own log, past every verb that answers and exits.
    say_memory_caps();
    crate::instruments::start_writer(&cfg);
    let registry = Registry::new();
    // **An older binary on a newer store: keep working, and say so in red on every head.**
    //
    // Asked once, here, for the same reason `say_memory_caps` is: this is past every verb that
    // answers and exits, and before every session opens — so the sentence is on the registry
    // before the first head can attach, and a store this build may NOT serve is a startup error
    // naming the fix rather than a session that fails to open halfway through a boot. See
    // `store_floor` for what each of the three answers means.
    //
    // **The store is opened for this and then dropped**, which is one more connection among the
    // several a daemon already holds (the session store, the corpus, the queues): the fact is
    // settled once per process and the connection has no second use, so paying for it here is
    // cheaper than threading it to every opener that would otherwise have to keep it.
    // **Asked with `?`, so a store this build may not serve is a startup error**: the sentence it
    // carries already names the command that fixes it, and a daemon that went on would be
    // building sessions on a file it cannot read.
    let said = crate::store_floor::said(&cfg)?;
    // **Said here as well as on the heads**, and this is the daemon's own log — the file an
    // operator reads a week later when they wonder why a build was behind. Cheap, and the one
    // place the fact is on disk before any head exists.
    if let Some(said) = &said {
        eprintln!("  {said}");
    }
    registry.set_store_older(said);
    // **The daemon watches its own memory** — `memwatch`. One line a minute in this log, a
    // census when it crosses a step, and a warning on every session.
    crate::memwatch::spawn(
        registry.clone(),
        std::env::var("LETIBOT_MEMWATCH_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .filter(|s: &u64| *s > 0)
            .map(std::time::Duration::from_secs)
            .unwrap_or(crate::memwatch::DEFAULT_EVERY),
        letibot_tools::exec::delegate_memory()
            .as_ref()
            .ok()
            .map(|s| s.join(letibot_tools::exec::DAEMON_LEAF)),
    );
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
    // **The stored row's parent and name come in with the first session** (R68.6), read once
    // here at startup: the registry's entry is what every later open asks (`Registry::parent_of`,
    // `Registry::title_of`) instead of scanning the store, so a session resumed by `--continue`
    // must arrive knowing whose subagent it is and what the operator named it.
    let stored = registry.resumable(&cfg.session_id);
    let title = match (&cfg.title, &stored) {
        (t, Some(b)) if t.is_empty() => b.title.clone(),
        (t, _) => t.clone(),
    };
    let parent = cfg
        .parent_session_id
        .clone()
        .or_else(|| stored.and_then(|b| b.parent_session_id));
    registry
        .create_under(
            cfg.session_id.clone(),
            title,
            Sessions::wiring(&cfg),
            parent,
        )
        .map_err(|e| e.to_string())?;

    // Bind before opening the session: a socket that is already served means
    // another daemon owns it, and finding that out after loading a vocabulary
    // wastes the load.
    let mut daemon = Daemon::serve(registry.clone(), &cfg.socket).map_err(|e| e.to_string())?;
    // **Where this daemon's tools run, said once and beside the socket** (RH2). The placement is
    // a fact of the PROCESS — every session this daemon opens runs its `bash`, `read`, `write`
    // and jobs in the same place — and the protocol carries it nowhere: a head attaching is told
    // the model, the endpoint and the dialect, all per session, and nothing about the backend.
    // So it is written down for the readers that need it, and the one that needs it is the
    // bridge: `leticode_start { host }` must not join a daemon running its caller's tools
    // somewhere else, and cannot tell from the socket alone.
    //
    // Written HERE and not before the bind, so only the daemon that actually holds the folder
    // writes an answer: a second start refused for a live daemon has already failed, and must
    // not overwrite the live daemon's record with the placement it was refused for.
    //
    // A failure is said and not fatal — no session depends on the file, only a head's question
    // about one does, and a daemon that refused to start over this would be trading a whole
    // session for a sentence.
    if let Err(e) = letibot_sessionlog::placement::write(&cfg.socket, &cfg.placement.as_str()) {
        eprintln!(
            "  could not record the placement beside {}: {e} — a head asking where this daemon's \
             tools run will be told there is no answer, and the bridge refuses to reuse a daemon \
             it cannot place",
            letibot_sessionlog::placement::path(&cfg.socket).display()
        );
    }
    daemon
        .catch_signals(cfg.one_shot)
        .map_err(|e| e.to_string())?;
    let boot = Boot::new(&registry, &cfg.session_id);
    boot.step("the socket is bound; starting the queues");

    // **The review queue and the merge queue (R61), before the worker loop takes this thread.**
    // Their own threads — a supervisor that claims each repository's lease and a worker per
    // lease held — never a session's worker, with their own connections to the store. The
    // reviewers are `task` children of the review queue's session (`Sessions::serve_reviews`);
    // the threads ring it, and every move is announced on every hub (`Registry::broadcast`),
    // because the queues are daemon-level and a pane may be open on any session.
    //
    // **The review desk is made HERE, not by `Parts::load` below**, because the queue's thread is
    // started first and both loops that judge a reviewer's fate have to see one desk: the worker
    // asks it whether a reviewer whose session is gone has already handed its verdict in, and the
    // review pass takes the verdict and writes the row. Two desks would be two answers to one
    // question — the worker would re-seat a row that had just been judged, and the verdict would
    // be discarded by a pass that found the row was no longer its reviewer's (measured 2026-10-29).
    let reviews = std::sync::Arc::new(letibot_tools::builtins::verdict::ReviewDesk::new());
    let merge_stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    // **A coordinator serves no repository**, so it claims no lease and runs no gate: `--room`
    // sets `serve_queues` off, and `spawn_for` is not called at all rather than called and made to
    // find nothing. The room is the queue this daemon serves.
    let merge_queue = if cfg.room {
        None
    } else {
        crate::mergequeue::spawn_for(&cfg, merge_stop.clone(), registry.clone(), reviews.clone())
    };

    let socket = daemon.socket().display().to_string();
    let dialect = cfg.dialect.name();
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
        Some(f) => {
            boot.step("opening the flowy seat");
            match open_seat(f) {
                Ok(s) => Some(s),
                Err(e) => {
                    let why = boot.failed(format!("--flowy: {e}"));
                    daemon.shutdown();
                    return Err(why);
                }
            }
        }
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
    cfg.read_project_config = !no_project_config;
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
    if cfg.provider.is_none() {
        boot.step(format!(
            "asking the model server at {} what it serves",
            cfg.endpoint.authority()
        ));
    }
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
    let banner_model = BannerModel::of(&cfg);

    // **The operator's `prompts.toml`, read once, here.** The per-model system-prompt
    // overrides, beside `providers.toml` in the same config dir.
    //
    // Read at startup rather than per session because the report belongs at startup:
    // a file that does not parse, or that names a section this daemon does not know,
    // is said once, with the parser's own message and the path, and the daemon runs
    // on the built-in prompt for every session. Silently ignoring an unreadable prompt
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
    boot.step("loading the vocabulary");
    let mut parts = match Parts::load(&cfg) {
        Ok(p) => p,
        Err(e) => {
            let why = boot.failed(e.to_string());
            daemon.shutdown();
            return Err(why);
        }
    };
    // **The desk the queue's thread is already holding** — see where it is made, above. Every
    // session's harness registers its own `review_verdict` against `parts.reviews`, and a
    // reviewer's verdict has to reach the queue that asked for it, so this is not a second desk.
    parts.reviews = reviews;

    // **The room, before the first session opens.** The coordinator's socket has to exist when
    // `Harness::open` builds the session's room link, and its session id has to be the arbiter's
    // before any event can be routed to it — so it is started here, after the vocabulary (a
    // coordinator that cannot load one has already failed) and before `open_first_with_seat`.
    //
    // Held in a local so an orderly stop drops it: `Coordinator::drop` unbinds the socket and gives
    // the lease back, which is what makes the next coordinator a FRESH start rather than a takeover
    // of a dead one. A coordinator that is killed leaves both behind on purpose — the lease is how
    // the next one learns it was killed rather than that it never existed.
    let _coordinator = if cfg.room {
        let bell: std::sync::Arc<dyn Fn(&str) + Send + Sync> = {
            let bell = std::sync::Arc::clone(registry.bell());
            std::sync::Arc::new(move |session: &str| bell.ring_wake(session))
        };
        match crate::room::coordinator::Coordinator::start(&cfg.session_id, bell) {
            Ok(c) => Some(c),
            Err(why) => {
                let said = boot.failed(why);
                daemon.shutdown();
                return Err(said);
            }
        }
    } else {
        None
    };

    // Captured before `cfg` and `parts` are handed to the worker: the HTTP head
    // needs the same vocabulary and dialect and none of the session state.
    let cfg_http = cfg.http.clone();
    let http_parts = parts.clone();
    let http_cfg = cfg.clone();

    // **This daemon's life, written down** — its start now, a heartbeat while it runs, and its
    // stop on the orderly path below — so the next daemon can say how long it was down
    // (`crate::restart`). Before the first session opens, because that open reads the LAST
    // daemon's life to decide what to restore.
    let lifeline = crate::restart::Lifeline::start(cfg.store.clone(), registry.clone());
    // **The VM reaper, from the start** — a VM whose owner is finished, or whose owner's daemon
    // is gone, is brought down rather than left holding a slot every review then waits on
    // (`crate::vm_reap`). Its own thread and its own connection: a reap can take minutes.
    crate::vm_reap::spawn(cfg.store.clone(), registry.clone());
    // **The process supervisor, from the start** — RH7. Its own thread and its own connection, and
    // its FIRST tick is the recovery: it reads every record the last daemon left open, checks each
    // host once, and writes down which processes are still the ones recorded. Started here rather
    // than with the first session because a job is not a session's: one whose session has not been
    // reopened yet still has to be checked, and a host that has gone quiet is `lost` either way.
    crate::supervise::spawn(cfg.store.clone(), registry.clone());
    boot.step(match resume_rows(&cfg) {
        Some(n) if n > 0 => format!("resuming the session ({n} rows)"),
        _ => "opening the session".to_string(),
    });
    let mut sessions =
        match Sessions::open_first_with_seat(&parts, cfg, registry.clone(), seat.clone()) {
            Ok(s) => s,
            Err(e) => {
                let why = boot.failed(e.to_string());
                daemon.shutdown();
                // A start that failed is a stop too: the next daemon reads it as one, not a crash.
                lifeline.stopped();
                return Err(why);
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
        .with_harness(&session_id, |h| (h.tokens().len(), h.ledger_head()))
        .unwrap_or((0, String::new()));

    // After the session is open, not before: the adjudication line is read from the
    // backend, the gate and the seated schemas, and none of those exist until the
    // harness is built. A banner computed early is a banner describing a session
    // that had not been wired yet.
    let disclosures = sessions
        .with_harness(&session_id, |h| h.config().disclosures(h.wiring()))
        .unwrap_or_default();

    eprintln!("harnessd: session {session_id}");
    // **The model, AND where it came from.** The operator's own question of the
    // evening — *"why is my session on deepseek"* — was answered by a file they had to
    // go and read, and a banner that says `model deepseek` without saying *which
    // level* is the same defect one line shorter. So the level is on this line, and
    // the `session model` disclosure under it carries the whole sentence.
    //
    // **And it names the model that ANSWERS the turns** (2026-10-26). It printed
    // `cfg.model` — the local vocabulary BINDING, which the launcher passes on every
    // start — so a session whose turns went to `glm-coding/glm-5.3` was headed `model
    // qwen-3.8-flash-next via 127.0.0.1:8080`, while its own disclosures two lines
    // down said `main glm-coding/glm-5.3` and `turns go to glm-coding (glm-5.3)`.
    // See [`BannerModel`]: the local binding is shown when it is a different thing,
    // and it is labelled as the binding rather than as the model.
    for line in banner_model.lines() {
        eprintln!("{line}");
    }
    eprintln!("  dialect  {dialect}");
    eprintln!("  workspace {workspace}");
    eprintln!("  socket   {socket}");
    eprintln!("  prefix   {prefix_tokens} tokens, head {ledger_head}");
    {
        // Say the wall and what happens at it. A session that will compact itself
        // without warning is a session doing something the operator did not see
        // coming, and one that will NOT is worth knowing before it dies at 500.
        let c = sessions.with_harness(&session_id, |h| h.config().clone());
        if let Some(c) = c {
            eprintln!("{}", context_line(&c));
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
            let counted = sessions.with_harness(&session_id, move |h| {
                (h.config().shown_tokens(r.tokens as u64), h.token_unit())
            });
            eprintln!("{}", resumed_line(&r, counted));
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
        // **The steps are over, so the one-shot's signal grace no longer applies.** A turn that
        // was interrupted properly and then spends its orderly tail (up to five seconds of
        // subagent threads, a flowy seat stopping) must not be killed by its own watchdog with a
        // line saying the turn never came back. See `daemon::one_shot_steps_done`.
        crate::daemon::one_shot_steps_done();
        daemon.shutdown();
        // **The same wait the serving path makes, for the same reason.** The process ends at
        // the `Ok` below — `bin/harnessd.rs` turns it into `std::process::exit` — and a
        // subagent thread still inside libllama when that runs the ELF finalizers is a
        // SIGSEGV after the answers have already been printed. See `SUBAGENT_EXIT_GRACE`.
        if let Some(said) = sessions.drain_subagents(SUBAGENT_EXIT_GRACE).sentence() {
            eprintln!("  {said}");
        }
        // **And hand the placement's work back** before the process ends: a remote host's
        // branch, a VM's copy. See `Sessions::close_backends`.
        for landed in sessions.close_backends() {
            eprintln!("  {landed}");
        }
        lifeline.stopped();
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
            Err(e) => {
                let why = boot.failed(format!("--http {addr}: {e}"));
                daemon.shutdown();
                lifeline.stopped();
                return Err(why);
            }
        }
    }

    boot.done();
    eprintln!("  waiting for a head. Ctrl-C to stop.");
    daemon.run(&mut sessions, |session, said| match said {
        // **A command a head sent, and what it came to.** The wording is the daemon's and the
        // turn's own thread reports it through this door — one line either way.
        OutcomeSaid::Command(cmd, outcome) => match outcome {
            // The session is named on every line: with more than one of them, "who did
            // what" is only half the question and the other half used to be unanswerable
            // from this log.
            Outcome::Replied(r) => eprintln!(
                "  {session} · {} -> {} round(s), {} tool call(s)",
                cmd.identity, r.rounds, r.tool_calls
            ),
            // The base is the LEDGER's pair (the fork's own measurement), and says so: the
            // count in the provider's units is the `compacted:` line the session printed just
            // above this one (R72 follow-up).
            Outcome::Compacted(r) => eprintln!(
                "  {session} · {} -> compacted, base {} -> {} ledger tokens (transcript {}); {}",
                cmd.identity,
                r.fork.was_tokens,
                r.fork.base_tokens,
                r.fork.transcript_id,
                crate::sessions::summary_turn_reuse(&r.summary_turn)
            ),
            Outcome::Failed(e) => eprintln!("  {session} · {} -> {e}", cmd.identity),
            Outcome::Ignored => {}
            // A wake handed to the session's own thread; nothing ran on this one. See
            // `Sessions::wake` — the arm exists because the outcome is the worker's vocabulary.
            Outcome::HandedOn => {}
        },
        // **A wake has no command behind it.** It used to be the worker's own `monitor ->` line;
        // now it is said by whichever thread ran the turn, in the same words.
        OutcomeSaid::Wake(outcome) => match outcome {
            Outcome::Replied(r) => eprintln!(
                "  {session} · monitor -> {} round(s), {} tool call(s)",
                r.rounds, r.tool_calls
            ),
            Outcome::Failed(e) => eprintln!("  {session} · monitor -> {e}"),
            // A wake never compacts, and the two that say nothing happened say nothing.
            Outcome::Compacted(_) | Outcome::Ignored | Outcome::HandedOn => {}
        },
    });
    // **Every step of the orderly stop is SAID, as it starts** (2026-10-26). What follows is
    // bounded but not short — a gate's process group killed, the VMs brought down, five seconds of
    // subagent threads — and until this, the daemon was silent through all of it. `letibot --stop`
    // waited five seconds, heard nothing, and reported *"is ignoring SIGTERM"* about a daemon that
    // was working through exactly this list; the launcher now relays the last `  stopping: ` line
    // it finds in this log, so the line has to exist. Stderr is the per-folder log
    // (`~/logs/harnessd-<key>.log`), which is where the launcher reads.
    eprintln!("  stopping: bringing every subagent VM down");
    // **Every subagent's VM comes down with the daemon — first.** Their backends live on threads
    // the process's exit does not unwind, so their `Drop` never ran and the VMs outlived it — two
    // of the acceptance suite's own runs, 2026-10-08. firecode's `down` delivers the work. FIRST,
    // and all at once (`down_all`), because what follows is bounded but not short and a forced
    // stop's SIGKILL comes five seconds after its SIGTERM: three of the four VMs that held every
    // slot on 2026-10-15 were children of a daemon that ended with no stop written. Whatever a
    // SIGKILL still interrupts here, the next daemon's reaper finds (`crate::vm_reap`).
    letibot_tools::firecode::down_all();
    // **The queues are told to stop.**
    eprintln!(
        "  stopping: the merge queues (a gate in flight is killed, its row put back in line)"
    );
    merge_stop.store(true, std::sync::atomic::Ordering::Relaxed);
    // **Joined, and bounded**: a gate in flight is killed with its whole process group (SIGTERM,
    // five seconds, SIGKILL) and its row goes back in line, and each worker releases its lease so
    // a successor need not wait out the heartbeat. The v23 queue detached here and left the
    // orphaned `cargo test` holding the worktree's `target` lock (R61 defect 6).
    if let Some(h) = merge_queue {
        let _ = h.join();
    }

    eprintln!("  stopping: closing every session");
    daemon.shutdown();
    // The seat after the daemon: the listener's next poll window sees the stop and
    // the waiter claim is released with it. `std::process::exit` above runs no RUST
    // destructors, so this is said here rather than left to a drop — and it is worth
    // being exact about the other half, because that half is this file's bug: `exit()`
    // DOES run every ELF finalizer (`_dl_fini`), which is why the subagent threads are
    // waited for below rather than left to the exit.
    if let Some(seat) = &seat {
        seat.stop();
    }
    // **Every subagent thread this daemon started is waited for before the process ends.**
    // `daemon.shutdown()` above closed every hub, which is what a child parked between turns
    // leaves on, and `drain_subagents` tells the running ones to stop first — a hub close does
    // not end a turn already in flight. The bound is `SUBAGENT_EXIT_GRACE`'s, and what a
    // give-up costs is the sentence printed here rather than a crash nobody can explain.
    eprintln!(
        "  stopping: waiting up to {}s for subagent threads to leave",
        SUBAGENT_EXIT_GRACE.as_secs()
    );
    if let Some(said) = sessions.drain_subagents(SUBAGENT_EXIT_GRACE).sentence() {
        eprintln!("  {said}");
    }
    // **The orderly stop, with its exact time** — what the next daemon measures its down time from.
    lifeline.stopped();
    eprintln!("  stopping: done");
    drop(sessions);
    drop(seat);
    Ok(0)
}

/// **What Claude Code has on this box**, one line per transcript — the id `--session
/// cc-<id>` takes, when it was last written, how big it is, the model that answered, and the
/// first thing that was said.
///
/// `--tsv` is the plain six-column data form, like `--list-sessions --tsv`: a script that
/// wants to pick a transcript should not have to parse a table.
fn list_claude(tsv: bool) -> Result<i32, String> {
    let Some(dir) = letibot_claude::default_projects_dir() else {
        return Err("no $HOME, so no Claude Code transcript directory to list".into());
    };
    list_claude_in(&dir, tsv)
}

/// The listing itself, against a named directory — so a test can point it at a fixture
/// instead of at the operator's transcripts, and so the sentence a box without a
/// `~/.claude/projects` gets is the same one a caller with a wrong path gets.
fn list_claude_in(dir: &std::path::Path, tsv: bool) -> Result<i32, String> {
    let src = letibot_claude::Source::open(dir).map_err(|e| e.to_string())?;
    let list = src.sessions().map_err(|e| e.to_string())?;
    if list.is_empty() {
        println!("no Claude Code transcripts under {}", dir.display());
        return Ok(0);
    }
    for s in &list {
        println!("{}", if tsv { s.tsv() } else { s.line() });
    }
    if !tsv {
        println!(
            "\n{} transcript(s) under {}. `letibot --session cc-<id>` reads one in and makes \
             it an ordinary session — resumable, promptable, and continued by `--continue` \
             from then on.",
            list.len(),
            dir.display()
        );
    }
    Ok(0)
}

/// **Write this session's conversation out as a Claude Code transcript** and a Markdown
/// copy, into `to`.
///
/// Two files, and they promise different things: the `.jsonl` is what
/// `claude --resume <absolute path>` opens, and the `.md` is what survives a Claude release
/// changing the line format. **Not into `~/.claude/projects`**: by absolute path a resume
/// needs the file nowhere in particular, and writing into another tool's directory is a
/// thing to do only when there is no other way.
fn handover(
    store: letibot_tokencore::store::Store,
    session: &str,
    to: &std::path::Path,
) -> Result<i32, String> {
    let row = store
        .session(session)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("the store has no session `{session}`"))?;
    let transcript_id = store
        .current_transcript_id(session)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            format!("session `{session}` has no transcript to hand over — it has no rows yet")
        })?;
    let loaded = store
        .load_transcript(&transcript_id)
        .map_err(|e| e.to_string())?;
    // **No per-row times, and the report says so.** This head's store records the row, its
    // tokens and its chain — not a clock per row — so a handover cannot carry one without
    // inventing it. `0` is "the row has no time", and `Skipped::no_time` counts the lines
    // that left without a `timestamp`.
    let items: Vec<(u64, letibot_transcript::TranscriptItem)> = loaded
        .items
        .iter()
        .map(|(i, _, _)| (0u64, i.clone()))
        .collect();
    let meta =
        letibot_claude::handover::Meta::fresh(std::path::Path::new(&row.workspace_root), None);
    let w =
        letibot_claude::handover::write_handover(&items, &meta, to).map_err(|e| e.to_string())?;
    println!(
        "wrote {} ({} line(s)) and {}",
        w.jsonl.display(),
        w.lines,
        w.markdown.display()
    );
    println!("resume it with: claude --resume {}", w.jsonl.display());
    println!("{}", w.skipped.said());
    Ok(0)
}

/// **The line that says a conversation moved**, on the transcript it moved from.
///
/// This is the shape the operator chose over a full Claude-format transcript: one
/// conservative line on the original, pointing at a Markdown handover — so whoever opens
/// Claude Code later sees, in the conversation they were in, that it continued here and
/// where to read what happened.
///
/// The line is **composed, not typed**, because every part of it is a fact this daemon has
/// and a person would have to look up: the session id, the span it ran over, the branch and
/// commit, and the path of the handover. The append is
/// [`letibot_claude::handover::append_note`]'s — a proven backup first, and a refusal while
/// a `claude` process is live for that project.
fn note_continued(
    store: letibot_tokencore::store::Store,
    session: &str,
    transcript: &std::path::Path,
    dir: Option<&std::path::Path>,
) -> Result<i32, String> {
    let row = store
        .session(session)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("the store has no session `{session}`"))?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let from = letibot_claude::handover::iso_ts(row.created_ms.max(0) as u64);
    let to = letibot_claude::handover::iso_ts(now);

    // Where the detail goes: the caller's directory, else the session's own workspace —
    // never `~/.claude/projects`, which is another tool's directory to write in.
    let out_dir = dir.map(std::path::Path::to_path_buf).unwrap_or_else(|| {
        std::path::Path::new(&row.workspace_root)
            .join(".letibot")
            .join("handover")
    });

    // The handover itself: the transcript `claude --resume <path>` opens by absolute path
    // (so it needs no place in `~/.claude/projects` and no existing id), plus the Markdown a
    // person reads when a Claude release moves the line format.
    let mut detail = out_dir.join("session.md").display().to_string();
    match store
        .current_transcript_id(session)
        .map_err(|e| e.to_string())?
    {
        Some(transcript_id) => {
            let loaded = store
                .load_transcript(&transcript_id)
                .map_err(|e| e.to_string())?;
            let items: Vec<(u64, letibot_transcript::TranscriptItem)> = loaded
                .items
                .iter()
                .map(|(i, _, _)| (0u64, i.clone()))
                .collect();
            let meta = letibot_claude::handover::Meta::fresh(
                std::path::Path::new(&row.workspace_root),
                None,
            );
            let w = letibot_claude::handover::write_handover(&items, &meta, &out_dir)
                .map_err(|e| e.to_string())?;
            detail = w.markdown.display().to_string();
            println!(
                "wrote {} ({} line(s)) and {}",
                w.jsonl.display(),
                w.lines,
                w.markdown.display()
            );
            println!(
                "resume that copy with: claude --resume {}",
                w.jsonl.display()
            );
        }
        // A session with no rows has no conversation to hand over. The line is still
        // written, because *this session exists* is the fact the transcript is missing.
        None => println!(
            "session `{session}` has no rows yet, so there is no conversation to write out"
        ),
    }

    let git = git_of(std::path::Path::new(&row.workspace_root));
    let line = format!(
        "[letibot] session continued in letibot from {from} to {to} — session {session}, detail \
         at {detail}, git {git}"
    );
    let got = letibot_claude::handover::append_note(
        transcript,
        &line,
        now,
        &letibot_claude::handover::PsLive,
    )
    .map_err(|e| e.to_string())?;
    println!(
        "appended {} byte(s) to {}:",
        got.bytes,
        transcript.display()
    );
    println!("  {line}");
    println!("the backup taken first: {}", got.backup.display());
    println!(
        "to put it back: harnessd --revert {} {}",
        transcript.display(),
        got.backup.display()
    );
    Ok(0)
}

/// `branch @ short-sha`, or a sentence saying why it could not be read — the line names a
/// commit so a reader can see which tree the conversation was about, and a repository that
/// will not answer must not cost the append.
fn git_of(dir: &std::path::Path) -> String {
    let ask = |args: &[&str]| -> Option<String> {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!s.is_empty()).then_some(s)
    };
    match (
        ask(&["rev-parse", "--abbrev-ref", "HEAD"]),
        ask(&["rev-parse", "--short", "HEAD"]),
    ) {
        (Some(branch), Some(sha)) => format!("{branch} @ {sha}"),
        _ => "(no git answer here)".to_string(),
    }
}

/// **One line on the end of a transcript Claude Code owns**, with a backup taken and proven
/// first — and nothing written at all when that backup could not be made or does not match.
fn append_note(transcript: &std::path::Path, text: &str) -> Result<i32, String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let got = letibot_claude::handover::append_note(
        transcript,
        text,
        now,
        &letibot_claude::handover::PsLive,
    )
    .map_err(|e| e.to_string())?;
    println!("appended {} byte(s) to {}", got.bytes, transcript.display());
    println!("the backup taken first: {}", got.backup.display());
    println!(
        "to put it back: harnessd --revert {} {}",
        transcript.display(),
        got.backup.display()
    );
    Ok(0)
}

/// **Put a transcript back, byte for byte**, from a backup this binary made.
fn revert_transcript(
    transcript: &std::path::Path,
    backup: &std::path::Path,
) -> Result<i32, String> {
    let bytes =
        letibot_claude::handover::revert(transcript, backup, &letibot_claude::handover::PsLive)
            .map_err(|e| e.to_string())?;
    println!(
        "restored {} from {} ({bytes} byte(s), byte for byte)",
        transcript.display(),
        backup.display()
    );
    println!("the backup is still there — reverting does not consume it");
    Ok(0)
}

/// How many sessions the store already holds, or `None` when there is no store.
///
/// A `SELECT` through `Store::connection`, which is the escape hatch that exists
/// for exactly this: the append-only guarantees are triggers, not a property of
/// whoever holds the connection, so a read here cannot weaken them.
/// **How many rows the first session's resume will read** — for the boot step's words; `None`
/// with no store or no such session (a new one).
fn resume_rows(cfg: &Config) -> Option<u32> {
    let store = letibot_tokencore::store::Store::open(cfg.store.as_ref()?).ok()?;
    let t = store.current_transcript_id(&cfg.session_id).ok()??;
    store.item_count(&t).ok()
}

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
    Calibrate {
        write: bool,
    },
    Backfill {
        write: bool,
    },
    Etalon(PathBuf),
    EtalonOracle(PathBuf),
    EtalonMap(PathBuf, PathBuf),
    RepairPrefixes,
    Compare,
    ShowBrief,
    /// What Claude Code has under `~/.claude/projects` (Package H).
    ListClaude,
    /// This session's conversation as a Claude Code transcript and a Markdown copy.
    Handover {
        session: String,
        to: PathBuf,
    },
    /// One line appended to an existing transcript, with a backup first.
    Note {
        transcript: PathBuf,
        text: String,
    },
    /// **The lighter shape**: the composed "continued in letibot" line on the original
    /// transcript, plus the Markdown handover it points at.
    NoteContinued {
        session: String,
        transcript: PathBuf,
        dir: Option<PathBuf>,
    },
    /// A transcript restored from a backup, byte for byte.
    Revert {
        transcript: PathBuf,
        backup: PathBuf,
    },
}

/// Answer a question about the store and exit. No socket, no vocabulary, no model.
fn run_query(
    cfg: &Config,
    q: Query,
    scope: Option<&std::path::Path>,
    tsv: bool,
) -> Result<i32, String> {
    // **The three Claude Code verbs that need no store of ours**, answered before `--store` is
    // required — so `--list-claude` works on a box whose daemon has never started, and
    // `--note`/`--revert` act on a file rather than on a session. Package H.
    match &q {
        Query::ListClaude => return list_claude(tsv),
        Query::Note { transcript, text } => return append_note(transcript, text),
        Query::Revert { transcript, backup } => return revert_transcript(transcript, backup),
        _ => {}
    }
    // **`--note-continued` needs the store** — it composes the line out of the session's own
    // row — so it is answered below, with `--handover`, once `--store` is known.
    let path = cfg
        .store
        .as_ref()
        .ok_or("no --store, and a question about stored sessions needs one")?;
    let store = letibot_tokencore::store::Store::open(path)
        .map_err(|e| format!("opening {}: {e}", path.display()))?;

    match q {
        // **Answered above, before the store was asked for.** Here only because a `match`
        // must be exhaustive; reaching this arm would mean one of them had started needing
        // a store without the three early returns being changed, and the `Ok(0)` is the
        // arm's way of not being a panic in a daemon.
        Query::ListClaude | Query::Note { .. } | Query::Revert { .. } => Ok(0),
        // **This session's conversation, as a transcript Claude Code can resume**, plus a
        // Markdown copy (Package H). The rows come from the store, which is the only place
        // a finished conversation exists whole.
        Query::Handover { session, to } => handover(store, &session, &to),
        // **The lighter shape the operator chose**: a composed line on the ORIGINAL
        // transcript, and the Markdown handover beside it.
        Query::NoteContinued {
            session,
            transcript,
            dir,
        } => note_continued(store, &session, &transcript, dir.as_deref()),
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
            // **The listing is GROUPED, and it lists everything** — the operator, 2026-10-26:
            // *"in session view the automatic sessions must be grouped somehow, so my vs auto.
            // otherwise im lost in subagents"*, said about the head's picker and true of this
            // listing for the same reason: one daemon's store holds their conversations and a
            // pile of the harness's own work, and a flat list ordered by recency puts a reviewer
            // above the thing they are doing.
            //
            // **What changed and what deliberately did not.** `--continue`'s resolution still
            // reads [`is_conversation`] and nothing here touches it — a reviewer must never be
            // what `--continue` reopens. What changes is the LISTING: it used to drop every row
            // that is not a conversation, which made *the queue's session exists* a fact a person
            // could not see from this command even though `--session ID` would open it. A group
            // line with a count says the same thing and says it once.
            //
            // **The groups come from `letibot_sessionlog::origin`, the same rule the head's
            // picker draws** — one rule, two renderers, so the two cannot tell a person that a
            // session is in different places.
            let all = scoped(&store, scope)?;
            let groups = grouped(&all);
            if tsv {
                // **TSV is a data format and it does not change.** Five fields,
                // tab-separated, one conversation per line: that is what a script cuts, and a
                // sixth column or a group header line in the middle would break every reader
                // for a fact a reader can compute. Whoever wants the groups reads the human
                // listing.
                //
                // The filter is the old one on purpose: this is the format of *the
                // conversations under this scope*, and it is what it always was.
                for s in all.iter().filter(|s| is_conversation(s)) {
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
            if all.is_empty() {
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
            let wid = all.iter().map(|s| s.id.len()).max().unwrap_or(8);
            // **The one row per session the listing draws**, as a closure so the two groups that
            // are not plain rows (a child under its parent, everything else) share it.
            let row = |s: &letibot_tokencore::store::StoredSession, indent: &str| {
                println!(
                    "{:<wid$}  {:>5}  {:>7}  {indent}{}{}",
                    s.id,
                    s.items,
                    age(now - s.last_activity_ms),
                    s.title.clone().unwrap_or_else(|| "(unnamed)".into()),
                    // **Where it LIVES, when that is not here.** A row whose live copy is on a
                    // host is not a session this box is appending to, and a listing that offered
                    // it unmarked would be offering a conversation that stopped the day it moved
                    // (`docs/remote-hosts.md` RH4). It stays in the list — the rows are here, and
                    // `--migrate SESSION --to local` brings it back — but it says so.
                    match &s.lives_on {
                        Some(on) => format!("  [lives on {on}]"),
                        None => String::new(),
                    },
                );
                // The same prefix width as the row above it, so the workspace lines up under the
                // title rather than three columns past it.
                println!("{:<wid$}  {:>16}  {indent}{}", "", "", s.workspace_root);
            };
            // **The same rows `grouped()` built**, for the one question the printing asks that the
            // grouping does not: is this child's parent on this same group's list? (The indent.)
            // Built from the same fields, because a second opinion about what a row is would be a
            // second grouping.
            let rows: Vec<letibot_sessionlog::GroupRow<'_>> = all
                .iter()
                .map(|s| letibot_sessionlog::GroupRow {
                    id: &s.id,
                    parent: s.parent_session_id.as_deref(),
                    origin: s
                        .origin
                        .as_deref()
                        .and_then(letibot_sessionlog::SessionOrigin::parse)
                        .unwrap_or_default(),
                    role: s.role.as_deref(),
                    title: s.title.as_deref().unwrap_or(""),
                })
                .collect();
            println!(
                "{:<wid$}  {:>5}  {:>7}  TITLE / WORKSPACE",
                "ID", "ROWS", "AGE"
            );
            for (group, members) in &groups {
                // **No empty group line**, for the reason the picker draws none: a header saying
                // `(0)` teaches a reader to skip headers.
                if members.is_empty() {
                    continue;
                }
                // **A group's line, and the count that is the whole reason for it.** It is a
                // header rather than a column: a reader scanning for their own conversation
                // stops at `mine`, and the four lines under it answer *what else is here* in four
                // numbers instead of forty rows.
                println!();
                println!(
                    "{:<wid$}  {} ({}) — {}",
                    "",
                    group.title(),
                    members.len(),
                    group.legend()
                );
                for s in members {
                    // Inside a group a child is indented under a parent that is on the same
                    // list, which is the same nesting the picker draws.
                    let indent = if s.parent_session_id.as_deref().is_some_and(|p| {
                        all.iter().any(|o| {
                            o.id == p && letibot_sessionlog::group_of(&rows, &o.id) == *group
                        })
                    }) {
                        "  "
                    } else {
                        ""
                    };
                    row(s, indent);
                }
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

/// **The stored sessions, in the groups a person reads them in** — `mine` first, then one entry
/// per automatic kind, each with its members in the store's own order (newest activity first).
///
/// Separate from the printing so the decision is assertable without a terminal, and because two
/// things read it: the human listing and the `--tsv` header line the launcher prints. The rule
/// itself is `letibot_sessionlog::group_of`'s — the same one the head's picker draws — so this
/// function is a grouping and not a second opinion about what a session is.
///
/// Every group is returned, empty ones included: `SessionGroup::ALL`'s order is the order a
/// person reads, and a caller that wants to skip an empty group (`(0)` headers teach a reader to
/// skip headers) does so where it prints.
fn grouped(
    all: &[letibot_tokencore::store::StoredSession],
) -> Vec<(
    letibot_sessionlog::SessionGroup,
    Vec<&letibot_tokencore::store::StoredSession>,
)> {
    let rows: Vec<letibot_sessionlog::GroupRow<'_>> = all
        .iter()
        .map(|s| letibot_sessionlog::GroupRow {
            id: &s.id,
            parent: s.parent_session_id.as_deref(),
            origin: s
                .origin
                .as_deref()
                .and_then(letibot_sessionlog::SessionOrigin::parse)
                .unwrap_or_default(),
            // **The store's row has both marks**, so a reviewer parented on the operator's own
            // conversation — the layout before the queue had a session of its own — is the queue's
            // work and not one of their subagents.
            role: s.role.as_deref(),
            title: s.title.as_deref().unwrap_or(""),
        })
        .collect();
    let mine =
        |s: &letibot_tokencore::store::StoredSession| letibot_sessionlog::group_of(&rows, &s.id);
    let mut out = Vec::new();
    for group in letibot_sessionlog::SessionGroup::ALL {
        let members: Vec<&letibot_tokencore::store::StoredSession> =
            all.iter().filter(|s| mine(s) == group).collect();
        out.push((group, members));
    }
    out
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

/// **The banner's two model facts, kept apart** — what answers this session's turns and, when the
/// daemon also holds a LOCAL binding that is a different thing, the vocabulary its token ids are
/// cut from.
///
/// Read before `cfg` is handed to `Sessions` and printed after the session is open, because the
/// banner is printed once the session exists.
///
/// 2026-10-26: the header said `model qwen-3.8-flash-next via 127.0.0.1:8080` on a session whose
/// own disclosures, two lines below, said `main glm-coding/glm-5.3` and `turns go to glm-coding
/// (glm-5.3)`. The line was `cfg.model`, which is the local VOCABULARY BINDING the launcher passes
/// on every start and which answers nothing when a provider does. The operator reads that line to
/// find out who they are talking to, so the answering model goes there, and the binding is said
/// beside it under the name the launcher already uses for it (`vocab`, a record written before
/// `answers` existed).
struct BannerModel {
    /// The model that answers: `provider/model` for a metered session, the local alias else.
    answers: String,
    /// Where that name came from — a file level, a flag, the built-in default.
    from: String,
    /// The endpoint an answer's token ids are submitted to.
    endpoint: String,
    /// The daemon's local binding, when a provider answers instead: the vocabulary this session's
    /// turns are not tokenised with, on an endpoint they never reach.
    binding: Option<String>,
}

impl BannerModel {
    fn of(cfg: &crate::config::Config) -> BannerModel {
        BannerModel {
            answers: cfg.prompt_model_name(),
            from: cfg.model_source.describe(),
            endpoint: cfg.endpoint.authority(),
            binding: cfg.provider.is_some().then(|| cfg.model.clone()),
        }
    }

    /// The lines, in the order the operator reads them — the answering model first, always.
    fn lines(&self) -> Vec<String> {
        let Some(binding) = &self.binding else {
            // A local session: one line, and it is both facts at once.
            return vec![format!(
                "  model    {} via {} (/completion, token array) — from {}",
                self.answers, self.endpoint, self.from
            )];
        };
        vec![
            format!("  model    {} — from {}", self.answers, self.from),
            format!(
                "  vocab    {binding} via {} (/completion, token array) — the daemon's local \
                 binding, NOT what answers this session's turns",
                self.endpoint
            ),
        ]
    }
}

/// **The banner's `context` line** — the window as given and, beside it, the reserve in
/// the SAME units.
///
/// The reserve was `Config::headroom()`, which is the PLANNING window's: in ledger
/// tokens, scaled by the session's ledger/provider pair. After a restart on a scratch
/// deepseek session (window 40000, pair 21212/18246) the line read *"compacts
/// automatically with 2905 left"* — 2500 × 21212/18246 — beside a window whose reserve
/// is 2500. R72 follow-up, defect C: every count printed beside the window is in the
/// window's units, which is `Config::shown_wall`.
fn context_line(c: &crate::config::Config) -> String {
    match (c.context_window, c.auto_compact) {
        (Some(w), true) => format!(
            "  context  {w} tokens; compacts automatically with {} left, as one more \
             message so the prefix stays cached",
            c.shown_wall().1
        ),
        (Some(w), false) => format!(
            "  context  {w} tokens; --no-auto-compact, so `/compact` is the only way and \
             the wall is a 500"
        ),
        (None, _) => "  context  UNKNOWN (the endpoint did not say and --context-window was \
                      not given), so nothing compacts on its own and the wall is a 500"
            .into(),
    }
}

/// **The banner's `RESUMED` line**, its count in the unit the wall is judged in (R72
/// follow-up, defect C). It printed the rebuilt LEDGER's length with no unit — *"21212
/// tokens"* on a deepseek session the provider counted at ~18.2k — a third number for the
/// one conversation beside the head's and the wall's. `counted` is that ledger length
/// through the session's pair (`Config::shown_tokens`, which `Harness::open` brings up to
/// date with `measure_resident`) and `Harness::token_unit`; the ledger's own figure is
/// kept in parentheses where it differs, because the store's rows are in it.
fn resumed_line(r: &crate::harness::ResumeReport, counted: Option<(u64, String)>) -> String {
    let ledger = r.tokens as u64;
    let tokens = match counted {
        Some((n, unit)) if n != ledger => format!("{n} {unit} ({ledger} ledger tokens)"),
        Some((n, unit)) => format!("{n} {unit}"),
        None => format!("{ledger} ledger tokens"),
    };
    format!(
        "  RESUMED  {} — {} row(s), {tokens}, head {}",
        r.transcript_id,
        r.rows,
        &r.head[..16.min(r.head.len())]
    )
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

    /// **The Claude Code listing** (Package H): the id a person types, then the facts a
    /// picker row needs — and the two forms, because a script should not parse a table.
    ///
    /// Against a fixture directory rather than `~/.claude/projects`, so this says what the
    /// listing DOES and not what happens to be on the box running the test.
    #[test]
    fn the_claude_listing_names_the_id_the_model_and_the_first_ask() {
        let dir =
            std::env::temp_dir().join(format!("letibot-cli-claude-{}-{}", std::process::id(), {
                use std::sync::atomic::{AtomicU32, Ordering};
                static N: AtomicU32 = AtomicU32::new(0);
                N.fetch_add(1, Ordering::Relaxed)
            }));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("proj")).unwrap();
        let id = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
        std::fs::write(
            dir.join("proj").join(format!("{id}.jsonl")),
            concat!(
                r#"{"type":"user","uuid":"u1","timestamp":"2026-10-07T12:28:34.023Z","cwd":"/w","gitBranch":"main","version":"2.1.285","origin":{"kind":"human"},"message":{"role":"user","content":"make the tests pass"}}"#,
                "\n",
                r#"{"type":"assistant","uuid":"a1","timestamp":"2026-10-07T12:28:36.000Z","message":{"model":"claude-opus-5-5","role":"assistant","content":[{"type":"text","text":"ok"}]}}"#,
                "\n"
            ),
        )
        .unwrap();

        // The tsv form: six columns, and the first is the id with the namespace mark this
        // head strips.
        assert_eq!(list_claude_in(&dir, true).unwrap(), 0);
        assert_eq!(list_claude_in(&dir, false).unwrap(), 0);

        // A directory that is not there is a sentence naming the directory, not an empty
        // listing — the same pair `--list-sessions` tells apart for the store.
        let absent = dir.join("not-there");
        let err = list_claude_in(&absent, false).unwrap_err();
        assert!(err.contains("no Claude Code transcript directory"), "{err}");
        assert!(
            err.contains("not-there"),
            "the sentence names the path: {err}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **The header names the model that ANSWERS the turns**, and says the local binding is the
    /// binding — 2026-10-26: a session whose disclosures, two lines below, said `main
    /// glm-coding/glm-5.3` and `turns go to glm-coding (glm-5.3)` was headed `model
    /// qwen-3.8-flash-next via 127.0.0.1:8080`, because the line was `cfg.model`, the local
    /// VOCABULARY binding the launcher passes on every start. The operator reads that line to find
    /// out who they are talking to.
    #[test]
    fn the_header_names_the_model_that_answers_and_labels_the_binding() {
        // A metered session: the provider answers, the local alias is only the vocabulary.
        let mut c = crate::config::Config::for_this_box("/tmp");
        c.model = "qwen-3.8-flash-next".into();
        c.endpoint = letibot_turn::Endpoint::new("127.0.0.1", 8080);
        c.provider = Some(crate::config::ProviderConfig {
            name: "glm-coding".into(),
            model: Some("glm-5.3".into()),
            api_key: None,
            thinking: false,
        });
        let lines = BannerModel::of(&c).lines();
        assert_eq!(lines.len(), 2, "two facts, two lines: {lines:?}");
        assert!(
            lines[0].starts_with("  model    glm-coding/glm-5.3 "),
            "the first line is what answers: {:?}",
            lines[0]
        );
        assert!(
            lines[1].starts_with("  vocab    qwen-3.8-flash-next via 127.0.0.1:8080")
                && lines[1].contains("NOT what answers this session's turns"),
            "and the binding is named as one: {:?}",
            lines[1]
        );

        // A local session: one line, and it is both facts at once.
        let mut c = crate::config::Config::for_this_box("/tmp");
        c.model = "qwen-3.8-flash-next".into();
        c.endpoint = letibot_turn::Endpoint::new("127.0.0.1", 8080);
        let lines = BannerModel::of(&c).lines();
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].starts_with("  model    qwen-3.8-flash-next via 127.0.0.1:8080")
                && !lines[0].contains("vocab"),
            "{:?}",
            lines[0]
        );
    }

    /// R72 follow-up, defect C: after a restart the banner printed the reserve in the
    /// planning window's ledger units (2905) beside a 40000 provider window whose
    /// reserve is 2500, and the RESUMED count as the ledger's length with no unit.
    #[test]
    fn the_banner_prints_counts_in_the_windows_units() {
        let mut c = crate::config::Config::for_this_box("/tmp");
        c.context_window = Some(40_000);
        c.auto_compact = true;
        c.ledger_scale = Some((21_212, 18_246));
        let line = context_line(&c);
        assert!(
            line.contains("40000 tokens; compacts automatically with 2500 left"),
            "{line}"
        );

        let r = crate::harness::ResumeReport {
            transcript_id: "s-1#t3".into(),
            rows: 12,
            tokens: 21_212,
            head: "0123456789abcdef0123".into(),
            workspace: "/tmp".into(),
            notes: Vec::new(),
        };
        assert_eq!(
            resumed_line(&r, Some((18_246, "deepseek tokens".into()))),
            "  RESUMED  s-1#t3 — 12 row(s), 18246 deepseek tokens (21212 ledger tokens), \
             head 0123456789abcdef"
        );
        assert_eq!(
            resumed_line(&r, Some((21_212, "tokens".into()))),
            "  RESUMED  s-1#t3 — 12 row(s), 21212 tokens, head 0123456789abcdef"
        );
    }
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
                        // The prefix above is three tokens long, so the first row starts
                        // after them — the store refuses a row that starts anywhere else,
                        // because a resume rebuilds the chain from the prefix's length.
                        tok_offset: 3 + seq * 3,
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

    /// **A daemon and a fixture both say `host` until something asks otherwise** — the two halves
    /// of the property [`Config::reviewers`] states.
    ///
    /// Both halves are asserted because either alone is a lie: a `for_this_box` that said `vm`
    /// would put a microVM under every test that built a fixture, and a `for_the_daemon` that
    /// quietly said `vm` would take every review on this box back into a VM nobody asked for —
    /// which is the 30–60-minute, one-of-two-slots review the operator ruled against on 2026-10-29.
    /// `--reviewers vm` and `Config::allow_vm_reviewers` are the two named ways to ask.
    #[test]
    fn a_fixture_and_a_daemon_both_place_reviewers_on_the_host_unless_asked() {
        assert_eq!(
            Config::for_this_box("/tmp").reviewers,
            crate::config::Reviewers::Host,
            "a fixture built the ordinary way must state no VM, or a test boots one by accident"
        );
        assert_eq!(
            Config::for_the_daemon("/tmp").reviewers,
            crate::config::Reviewers::Host,
            "the daemon places reviewers on the host unless the project or the flag asks"
        );
        let mut asked = Config::for_the_daemon("/tmp");
        asked.allow_vm_reviewers();
        assert_eq!(asked.reviewers, crate::config::Reviewers::Vm);
    }

    /// **`--reviewers` is parsed, both spellings, and it wins over the file** — the flag
    /// `--help` has advertised since the two words existed and the parser did not have.
    ///
    /// MEASURED 2026-10-29: `harnessd --reviewers vm` answered *unknown argument --reviewers*,
    /// which tells an operator their configuration is a typo when the build's own help says it is
    /// not. What is asserted here is the flag's whole body ([`apply_reviewers_flag`]) against a
    /// `Config` the way the argument loop holds one: after `for_the_daemon`, which is the value the
    /// project's `[queue] reviewers` put there.
    ///
    /// **`run` itself is not driven from here**, and the reason is not convenience: it delegates
    /// this process's memory scope, sets the gate's cap and the gate's host process-wide, and
    /// reads the operator's `providers.toml` — a lib test that called it would leave every other
    /// test in this binary with settings that are not theirs (`gate_host()` in particular, which a
    /// gate test asserts is unset). So the flag's body is a function with a name, and this is it.
    #[test]
    fn the_reviewers_flag_takes_both_spellings_and_overrides_the_project() {
        // The default, with no flag at all.
        let mut cfg = Config::for_the_daemon("/tmp");
        assert_eq!(cfg.reviewers, Reviewers::Host, "no flag, no VM");

        // `--reviewers vm` — the spelling that used to be *unknown argument*.
        apply_reviewers_flag(&mut cfg, "vm").expect("`vm` is one of the two words");
        assert_eq!(cfg.reviewers, Reviewers::Vm);

        // `--reviewers host` puts it back over a config that says `vm`, which is the whole point
        // of the flag outranking the project file.
        apply_reviewers_flag(&mut cfg, "host").expect("`host` is the other");
        assert_eq!(cfg.reviewers, Reviewers::Host);

        // A third word is refused BY NAME with both of them in the sentence, rather than
        // silently leaving whatever was there.
        cfg.allow_vm_reviewers();
        let why = apply_reviewers_flag(&mut cfg, "firecode").unwrap_err();
        assert!(why.contains("\"firecode\""), "{why}");
        assert!(
            why.contains("\"host\"") && why.contains("\"vm\""),
            "the refusal names both words: {why}"
        );
        assert_eq!(
            cfg.reviewers,
            Reviewers::Vm,
            "a refused word changes nothing"
        );
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

    /// **The listing groups the store the way the head's picker does** — mine first, then one
    /// entry per automatic kind, each carrying its own members.
    ///
    /// The operator's ask is one sentence and it is about both places a session is listed:
    /// *"in session view the automatic sessions must be grouped somehow, so my vs auto. otherwise
    /// im lost in subagents."* The RULE is `letibot_sessionlog::group_of`'s and is tested there;
    /// what is asserted here is that `--list-sessions` feeds it the store's own facts — the parent
    /// edge, the queue's id, and the `origin` column a daemon writes — and not a second opinion.
    #[test]
    fn the_listing_puts_mine_first_and_the_harnesss_own_work_in_its_groups() {
        let (store, _d) = store_with(&[("s-mine", "/ws"), ("s-sub", "/ws")]);
        // A grandchild of the operator's conversation, the queue and one of its reviewers, a
        // package's conversation, and a one-shot.
        store
            .set_session_origin("s-mine", "operator")
            .expect("the origin");
        session_row(&store, "s-gk-sub", "/ws", None, None, Some("s-sub"));
        store
            .set_session_origin("s-sub", "operator")
            .expect("the origin");
        let queue = "s-gatekeeper-0123456789ab";
        session_row(&store, queue, "/ws", None, None, None);
        session_row(&store, "s-gk", "/ws", None, None, Some(queue));
        store
            .set_session_origin(queue, "queue")
            .expect("the origin");
        session_row(&store, "s-pkg", "/ws", None, None, None);
        store
            .set_session_origin("s-pkg", "package")
            .expect("the origin");
        session_row(&store, "s-shot", "/ws", None, None, None);
        store
            .set_session_origin("s-shot", "one_shot")
            .expect("the origin");

        let rows = store.list_sessions().expect("listing");
        let grouped = grouped(&rows);
        let ids = |g: letibot_sessionlog::SessionGroup| -> Vec<String> {
            let mut v: Vec<String> = grouped
                .iter()
                .find(|(k, _)| *k == g)
                .map(|(_, m)| m.iter().map(|s| s.id.clone()).collect())
                .unwrap_or_default();
            // The store's order is recency's and this test is about membership, so the two are
            // sorted apart on purpose.
            v.sort();
            v
        };
        assert_eq!(
            ids(letibot_sessionlog::SessionGroup::Mine),
            ["s-mine", "s-sub"]
        );
        assert_eq!(
            ids(letibot_sessionlog::SessionGroup::Subagents),
            ["s-gk-sub"],
            "a task child is a subagent however deep"
        );
        assert_eq!(
            ids(letibot_sessionlog::SessionGroup::ReviewQueue),
            ["s-gatekeeper-0123456789ab", "s-gk"],
            "the queue's session and its reviewers are one group"
        );
        assert_eq!(ids(letibot_sessionlog::SessionGroup::Packages), ["s-pkg"]);
        assert_eq!(ids(letibot_sessionlog::SessionGroup::OneShot), ["s-shot"]);
        // **Mine is first**, which is the whole of *mine vs auto*: the order is
        // `SessionGroup::ALL`'s and the printer follows it.
        assert_eq!(grouped[0].0, letibot_sessionlog::SessionGroup::Mine);
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
