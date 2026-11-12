//! The daemon's command line, as a function rather than a `main`.

//! `harnessd` - the daemon.
//!
//! ```text
//! harnessd [--workspace DIR] [--socket PATH] [--store DIR] [--project KEY]
//!          [--dialect glm|qwen] [--model ALIAS] [--endpoint HOST:PORT]
//!          [--vocab GGUF] [--system TEXT] [--system-file FILE]
//!          [--effort low|medium|high|xhigh]
//!          [--spill-inline BYTES] [--spill-dir DIR]
//!          [--max-tool-rounds N|off] [--stall-rounds N] [--session ID] [--title NAME]
//!          [--prompt TEXT ...]        run these, print the answers, exit
//!          [--slash VERB ...]         a head's slash verb, in order with --prompt:
//!                                     `--prompt hi --slash "models deepseek" --prompt again`
//!
//! store queries - no socket, no vocabulary, no model:
//! harnessd --store PATH --list-sessions [--workspace DIR] [--tsv]
//! harnessd --store PATH --listen WORDS... [--workspace DIR]   (/listen, offline)
//! harnessd --store PATH --latest-session [--workspace DIR]
//! harnessd --store PATH --resolve-session NAME [--scope DIR]
//! harnessd --store PATH --find-session NAME --scope DIR
//! harnessd --store PATH --rename ID TITLE
//! harnessd --store PATH --delete ID
//! ```
//!
//! With no `--prompt` it serves the socket and waits. Attach a head with
//! `letibot-tui --socket PATH`.
//!
//! # The store queries answer before anything is loaded
//!
//! `--list-sessions` and its three siblings return before `Parts::load`, before
//! the endpoint health check and before the socket is bound. That is not an
//! optimisation: `letibot --sessions` has to work when the model server is down, and
//! a listing that first insisted on a 0.6-second GGUF load and a live `/health`
//! would be a listing you cannot use to find out what went wrong.
//!
//! With one or more `--prompt` it runs them in order and exits - which is what
//! makes a scripted session a shell command rather than a program, and it is how
//! `letibot-m1` drives its own measurement.

use std::path::PathBuf;

use crate::config::{
    AdjudicatorChoice, Config, Disclosure, Reviewers, Seat, SpillPolicy, SpillStorage,
};
use crate::{Daemon, Dialect, Outcome, OutcomeSaid, Parts, Sessions};
use letibot_sessionlog::registry::Registry;
use letibot_turn::Endpoint;

/// Resolve the credentials and open the seat. Every refusal in here names what was looked
/// at, because "no flowy" and "the wrong flowy" must not read alike. The daemon's boot
/// said on the first session's hub.
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
            h.warn(code, detail);
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

    /// Why the boot failed, on the hub, and a moment for a head to read it before the caller
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

/// Apply a `main_model` from a file to the session's config. A `main_model` is a model
/// name, the same shape the operator types for `--model` and the names `/models` offers: a
/// `provider/model` for a metered session, a bare alias for a local one.
pub(crate) fn apply_main_model(
    cfg: &mut Config,
    model: &str,
    bound: Option<&str>,
) -> Option<String> {
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

/// What `--reviewers WORD` does to a `Config` - the flag's whole body, in a function so
/// the one thing it must get right is a value a test can read.
fn apply_reviewers_flag(cfg: &mut Config, word: &str) -> Result<(), String> {
    match Reviewers::parse(word)? {
        Reviewers::Vm => cfg.allow_vm_reviewers(),
        Reviewers::Host => cfg.reviewers = Reviewers::Host,
    }
    Ok(())
}

/// Where this session's tools run, from the two asks the command line can carry -.
/// A VM and a machine are not alternatives: neither ask is dropped without a word
/// because the other came last.
fn placement_from_flags(
    vm: bool,
    host: Option<&str>,
) -> Result<letibot_tools::builtins::task::Placement, String> {
    use letibot_tools::builtins::task::Placement;
    match (vm, host) {
        (true, Some(host)) => Ok(Placement::RemoteVm(host.to_string())),
        (true, None) => Ok(Placement::Firecode),
        (false, Some(host)) => Ok(Placement::Remote(host.to_string())),
        (false, None) => Ok(Placement::Host),
    }
}

/// A `provider/model` name a file set that this box cannot reach, as a sentence. No
/// network and no endpoint, so this is asked of every model name in either file: the
/// provider must be one this build knows, and a key for it must resolve here.
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

/// A bare local alias that names a model this daemon cannot reach, as a sentence. Asked
/// only of the name that decides THIS daemon's own turns - `main_model` - and `served` is
/// the alias the local server reports off `/props`.
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

/// Every model name either file set, with the faults this box can see. The
/// provider-shaped half is asked of all of them; the local half only of `main_model`, which
/// is the one that decides this daemon's own turns.
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
    "harnessd [--workspace DIR] [--socket PATH] [--store DIR|FILE] [--project KEY]\n\
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
      \x20 --parent-seat cc@proj     this daemon's sessions are children of that\n\
      \x20                           Claude Code seat, not top-level services: they\n\
      \x20                           do not join the box's room\n\
      \x20 --no-queue                serve NO merge or review queue: no lease is\n\
      \x20                           claimed, no target gated and no branch landed\n\
      \x20                           here, and this daemon's sessions are told no\n\
      \x20                           queue news (a package worker must not be set\n\
      \x20                           to fixing a `main` nobody asked it about). The\n\
      \x20                           store watcher stays, so a head still moves\n\
     \x20 --room                    be the BOX'S ROOM COORDINATOR: a whole daemon\n\
     \x20                           whose only job is the room -- its own socket\n\
     \x20                           under the data directory, NO store, no project,\n\
     \x20                           and one session of its own (`s-room-<host>`,\n\
     \x20                           reachable as `--session host`) that arbitrates\n\
     \x20                           the queues. One per box: a second is refused by\n\
     \x20                           a lease that names the live holder, and it will\n\
     \x20                           not start without a model to arbitrate with\n\
     \x20                           (`--model`, deepseek by default)\n\
     \x20 --reviewers host|vm     where this daemon's REVIEWERS run. `host` (the\n\
     \x20                           default) is a reviewer child on this machine, in\n\
     \x20                           a checkout of the head it judges -- as the\n\
     \x20                           subagents that author the branches already run;\n\
     \x20                           `vm` is each reviewer in a firecode microVM,\n\
     \x20                           which asks nobody for anything and costs a guest\n\
     \x20                           exec per read. The project's `[queue] reviewers`\n\
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
     \x20 --reviewers-at-once N how many reviewers this daemon's queues run at ONCE.\n\
     \x20                           The default belongs to the placement: `vm` keeps\n\
     \x20                           the box's VM slots, and `host` runs as many as the\n\
     \x20                           machines it may use can take (four per machine, one\n\
     \x20                           per four spare cores). The project's\n\
     \x20                           `[queue] reviewers_at_once` says the same thing\n\
     \x20 --reviewer-hosts LIST   the machines a reviewer may be placed on, comma\n\
     \x20                           separated: names from the operator's hosts.toml,\n\
     \x20                           `local` for this machine, or `*` for every\n\
     \x20                           declared host. The default is this machine only\n\
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
     \x20                           --adjudicator model and by /supervise;\n\
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
     \x20                           whose commits come back as remote/NAME/<id>. A\n\
     \x20                           remote session is NOT in a VM unless --where\n\
     \x20                           firecode says so, and the pair is a VM THERE:\n\
     \x20                           the host's own `firecode` boots the guest over\n\
     \x20                           this session's ssh link, the guest holds a copy\n\
     \x20                           of the checkout at the same path and IS the\n\
     \x20                           boundary, and what it commits is collected onto\n\
     \x20                           the checkout when the VM stops, then fetched home\n\
     \x20                           as remote/NAME/<id>. A host with no firecode or\n\
     \x20                           no /dev/kvm is refused by name, once per host\n\
     \x20                           (the launcher spells the pair `--host NAME --vm`)\n\
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
     \x20 --find-session NAME      a session by name under --scope, else in any folder;\n\
     \x20                           prints `here` or `there` rows (see the launcher)\n\
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

/// How long the daemon's exit waits for the subagent threads it started, and why that
/// number. The wait is [`Sessions::drain_subagents`]'s, and the reason there is one at all
/// is the process's exit
const SUBAGENT_EXIT_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// The daemon's memory caps, said once, by the daemon. It is
/// not printed by the `harnessd` binary's `main`, or every short-lived invocation of that
/// binary would say it.
fn say_memory_caps() {
    match letibot_tools::exec::delegate_memory() {
        Ok(scope) => eprintln!(
            "harnessd: memory caps on — the tool calls' cgroups live in {}",
            scope.display()
        ),
        Err(why) => eprintln!("harnessd: memory caps OFF — {why}"),
    }
}

/// Nothing this process booted on another machine outlives it. A guest is booted by a
/// placement, and a `run` that then returns early (an error opening the session, a verb
/// that is not a daemon at all) never reaches a backend's close, so it comes down here
struct GuestsDown;

impl Drop for GuestsDown {
    fn drop(&mut self) {
        for said in letibot_tools::remote::guests_down() {
            eprintln!("  {said}");
        }
        // And what a placed session STARTED there. A host has no cgroup to empty, so this is the
        // same guard for the same reason: `--stop` closes no backend, and a session on its own
        // thread holds its backend where `close_backends` cannot reach it.
        for said in letibot_tools::remote::placements_down() {
            eprintln!("  {said}");
        }
    }
}

/// Where this session's turns go when its model is out. `[fallback] models` is the default and a `leticode.toml` list replaces it whole, so a project naming two does not inherit a third. The timeout budget rides with the list.
fn session_fallback(cfg: &mut Config) {
    if !cfg.leticode.fallback.is_empty() {
        eprintln!(
            "  fallback: {} — from leticode.toml ({})",
            cfg.leticode.fallback.join(", "),
            cfg.leticode
                .path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        );
        cfg.fallback = cfg.leticode.fallback.clone();
    }
    if let Some(n) = cfg.leticode.fallback_after_timeouts {
        cfg.fallback_after_timeouts = n;
    }
    // And what a usage limit does. The file's own word, else switch when a list exists and wait
    // without one - the operator's ruling is that a limit with a known reset never fails a session.
    cfg.on_limit = cfg.leticode.limit_policy();
    eprintln!(
        "  on a usage limit: {} — {}",
        cfg.on_limit.as_str(),
        match cfg.on_limit {
            crate::leticode_config::OnLimit::Switch => {
                "the session moves to the next fallback model that is not limited and moves back \
                 after the reset"
            }
            crate::leticode_config::OnLimit::Wait => {
                "the session waits for the reset and resumes the turn by itself"
            }
        }
    );
}

pub fn run(args: &[String]) -> Result<i32, String> {
    // Declared first, so it is dropped LAST: after every session, daemon and seat has had its own
    // chance to close its backend and say where its work went.
    let _guests = GuestsDown;
    // The file this process was started from, before a rebuild can replace it.
    crate::exe_stamp::take();
    // Step into the daemon's own cgroup leaf before anything here can start a child
    // - `delegate_memory` refuses when the cgroup already holds another pid, so a
    // `git` the startup ran first would turn every tool call of this daemon's life uncapped.
    let _ = letibot_tools::exec::delegate_memory();
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    // `for_the_daemon`, and not `for_this_box`. The daemon is the one caller that is a
    // live operator's box running the review queue, so this is where the PROJECT's answer to
    // *where do my reviewers run* is read (`[queue] reviewers`) - every fixture builds its
    let mut cfg = Config::for_the_daemon(cwd);
    // The guard model comes from config, not from a flag. `[gatekeeper] endpoint =
    // "HOST:PORT"` in `~/.config/letibot/providers.toml`, beside the provider keys, because an
    // endpoint is a property of the box and not of an invocation.
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
        // The merge gate's cap: each gate step runs in its own cgroup beside the daemon's leaf, under this (half of free memory unless `gate` says otherwise; `uncapped` keeps scope and peak, no cap).
        // The file is handed over, not the value: the gate re-reads `[memory] gate` every run. It is parsed now so a malformed value is refused at start, naming the file, not swallowed.
        crate::mergequeue::gate::set_gate_memory_from_file(&letibot_provider::keys::config_file())?;
        // The daemon's own throttle : `memory.high` on its leaf, so a daemon that grows
        // without bound is slowed by the kernel instead of taking the box - 16 GiB unless
        // `daemon_high` says otherwise (`max` turns it off). Only in a delegated scope
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
        // The guard's own model, when it is not the one doing the work. Read here beside the
        // endpoint because they describe the same server, and a file that names a model nobody
        // reads is a setting the operator will reasonably believe took effect.
        if let Some(m) = gk.model.filter(|m| !m.trim().is_empty()) {
            cfg.oracle_model = Some(m);
        }
        // The authority the operator declares for their own guard. Refused by name rather than
        // narrowed silently
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
    // Prompts and slash verbs in one ordered list. `--prompt` could run turns headlessly
    // and nothing could change the session between them, so the one thing worth testing about
    // a mid-session model switch - that the turn AFTER it works - could only be done by hand
    let mut steps: Vec<Step> = Vec::new();
    // `--model` under `--provider` names the provider's model, not the local
    // alias; resolved after the flags, because either may come first.
    let mut model_given: Option<String> = None;
    // Whether the command line chose WHO ANSWERS. That is `--provider`, and it is the flag
    // level of the precedence: `--provider deepseek` beats the project file, the project file
    // beats the user file, and the user file beats the built-in default.
    let mut cli_main_model = false;
    // The local alias `--model` bound, when it named one. The binding and the choice of
    // who answers are two facts and this is the other one: a file's `main_model` may clear the
    // provider without re-binding the vocabulary - see `apply_main_model`.
    let mut cli_binding: Option<String> = None;
    // The explicit "ignore the project file". See the flag's own note.
    let mut no_project_config = false;

    let mut query: Option<Query> = None;
    let mut tsv = false;
    // `--workspace` defaults to the process's cwd for a daemon, and to *nothing* for a
    // listing
    let mut scope: Option<PathBuf> = None;

    // `--vm-copy` said anywhere on the line wins over `--where firecode`'s mount, before it or
    // after: the operator who asked for a copy must never get the folder itself read-write.
    let mut vm_copy = false;

    let mut vm_asked = false;
    let mut host_asked: Option<String> = None;

    let mut it = args.iter().cloned();
    while let Some(arg) = it.next() {
        let mut next = || it.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--list-sessions" => query = Some(Query::List),
            // What Claude Code has on this box (Package H) - the ids `--session cc-<id>` takes
            // with the model, the directory and the first thing that was said.
            "--list-claude" => query = Some(Query::ListClaude),
            // The return trip: this session's conversation as a transcript
            // `claude --resume <path>` opens, plus a Markdown copy a person can read.
            "--handover" => {
                let session = next()?;
                let to = PathBuf::from(next()?);
                query = Some(Query::Handover { session, to });
            }
            // The lighter shape the operator chose: one line on the ORIGINAL transcript saying the
            // session was continued here, plus the Markdown handover for the fallback. The line's own
            // words are composed, not typed - see `note_continued`.
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
            // One line on the end of a transcript Claude Code owns. A backup first, and no
            // write at all when a backup could not be made or proven - see
            // `letibot_claude::handover::append_note`.
            "--note" => {
                let transcript = PathBuf::from(next()?);
                let text = next()?;
                query = Some(Query::Note { transcript, text });
            }
            // Put a transcript back, byte for byte, from a backup this binary made.
            "--revert" => {
                let transcript = PathBuf::from(next()?);
                let backup = PathBuf::from(next()?);
                query = Some(Query::Revert { transcript, backup });
            }
            "--latest-session" => query = Some(Query::Latest),
            "--resolve-session" => query = Some(Query::Resolve(next()?)),
            "--find-session" => query = Some(Query::Find(next()?)),
            "--tsv" => tsv = true,
            "--rename" => {
                let id = next()?;
                let title = it.next().unwrap_or_default();
                query = Some(Query::Rename(id, title));
            }
            "--delete" => query = Some(Query::Delete(next()?)),
            // Every remaining word is `/listen`'s: `-REPO`, `+REPO`, `only REPO|this`, `all`.
            "--listen" => query = Some(Query::Listen(it.by_ref().collect())),
            // Replay the operator-answered corpus against the guard and print what it earns.
            // `--calibrate-write` also records it where the daemon reads it at startup. See
            // `crate::calibrate`.
            "--repair-prefixes" => query = Some(Query::RepairPrefixes),
            // Run every arm over the same rows and print them side by side. A
            // prompt change to this seam is a hypothesis until this says otherwise.
            "--compare" => query = Some(Query::Compare),
            // Print the bytes the guard is actually handed for a call it could not
            // answer. "Look at what it is shown" - the only way to tell a prompt
            // problem from a evidence problem.
            "--show-brief" => query = Some(Query::ShowBrief),
            // Recover shapes for decisions the operator already made, so the shape cache does not
            // start cold on a store full of their own approvals. Re-derived with today's classifier
            // never reinstated from the row.
            "--etalon" => query = Some(Query::Etalon(PathBuf::from(next()?))),
            // The guard itself over an etalon-shaped JSONL - an attack set, with
            // the main task as the operator's ask, as in BashArena. Needs
            // --oracle. Rows are seated in a scratch copy of the store.
            "--etalon-oracle" => query = Some(Query::EtalonOracle(PathBuf::from(next()?))),
            // Every command's tree-sitter shape, aggregated and laid out on a
            // self-organising map; writes the JSON the map page draws.
            "--etalon-map" => {
                let file = PathBuf::from(next()?);
                let out = PathBuf::from(next()?);
                query = Some(Query::EtalonMap(file, out));
            }
            "--backfill-shapes" => query = Some(Query::Backfill { write: false }),
            "--backfill-shapes-write" => query = Some(Query::Backfill { write: true }),
            "--calibrate" => query = Some(Query::Calibrate { write: false }),
            "--calibrate-write" => query = Some(Query::Calibrate { write: true }),
            // An empty scope is a missing argument, not "everywhere".
            // `Path::new("/anything").starts_with("")` is true in Rust, so an empty `--scope` matched
            // every session in the store and `--continue` handed back the newest one on the box - the
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
            // A data directory: `shared.db` and `projects/<key>.db` under it. A path ending in
            // `.db` is one file, the pre-split shape this build keeps for a test fixture.
            "--store" => cfg.store = Some(PathBuf::from(next()?)),
            // Which project file this daemon serves, when it is not the folder's own: a worktree's
            // daemon stands for the folder it was cut from, and the key is that folder's.
            "--project" => cfg.store_key = Some(next()?),
            "--session" => {
                let named = next()?;
                // `host` is the box's own agent, by name, and `host:<tag>` is one tag's - the room's
                // coordinator is `s-room-<host key>` and a tag's agent is that plus the tag, both derived
                // from the machine so that `letibot --session host` (and `--session host:ram`) always
                cfg.session_id = crate::room::session_named(&named).unwrap_or(named);
            }
            "--title" => cfg.title = next()?,
            "--model" => {
                let m = next()?;
                model_given = Some(m.clone());
                cfg.model = m.clone();
                // The binding, not the choice of who answers - see `cli_main_model`.
                cli_binding = Some(m);
            }
            "--vocab" => cfg.vocab_gguf = Some(PathBuf::from(next()?)),
            "--effort" => cfg.effort = Some(next()?),
            // The four flags that make anything reachable, and all four are
            // opt-in. Nothing here changes what an invocation without them gets.
            "--role" => cfg.seat = Seat::parse(&next()?)?,
            // The approval policy, separate from the role. `Config::mode` has existed since the two
            // were split, but nothing set it, so every session ran at `UNSEEN_PROJECT`.
            "--mode" => cfg.mode = letibot_tools::mode::Mode::parse(&next()?)?,
            // Bind one path read-only into the confined view. Repeatable. The boundary is hermetic
            // `$HOME` inside is a fresh tmpfs, so `~/.cargo` and `~/.rustup` are absent and `cargo`
            // cannot run.
            "--grant-ro" => cfg.grants_ro.push(PathBuf::from(next()?)),
            "--bash" => cfg.allow_bash = true,
            // How deep a subagent tree may go. Zero refuses every `task` call by name; the default
            // is the operator's ruling (3). A flag rather than a hidden constant because it bounds a
            // whole tree and is the one number an operator reaches for when a run should not fan out.
            "--max-subagent-depth" => {
                cfg.max_subagent_depth = next()?
                    .parse()
                    .map_err(|e| format!("--max-subagent-depth: {e}"))?
            }
            // How long the daemon may have been down for a session's work to come back on its
            // own - see `crate::restart`. Seconds, because it is a span an operator types.
            "--restore-within" => {
                cfg.restore_within = std::time::Duration::from_secs(
                    next()?
                        .parse()
                        .map_err(|e| format!("--restore-within: {e}"))?,
                )
            }
            // How long the daemon stays up with nothing to do - see `Config::idle_exit_after`.
            // Seconds, and `0` for never, because a daemon an operator wants kept is one flag.
            "--idle-exit" => {
                let secs: u64 = next()?.parse().map_err(|e| format!("--idle-exit: {e}"))?;
                cfg.idle_exit_after = (secs > 0).then(|| std::time::Duration::from_secs(secs));
            }
            // A daemon that serves no queue. The merge and review queues belong to the repository's
            // session daemons, so a daemon the Claude Code bridge starts in a package's worktree is
            // one of them by construction, and a daemon started there must be able to opt out.
            "--no-queue" => cfg.serve_queues = false,
            // The seat that started this daemon: its sessions are that seat's children.
            "--parent-seat" => cfg.parent_seat = Some(next()?).filter(|h| !h.is_empty()),
            // The box's room coordinator (the room's slice 2). A whole daemon whose only job is
            // the room: its own socket, no store, no project, and one session of its own that
            // arbitrates the queues.
            "--room" => cfg.room = true,
            "--adjudicator" => cfg.adjudicator = AdjudicatorChoice::parse(&next()?)?,
            // Layer B's endpoint. Its own, not `--endpoint`: the guard does not have to be the
            // model doing the work, and on this fleet it should not be - a 4B on the CPU beside a 27B
            // on a GPU, or a second box entirely.
            "--supervise" => cfg.supervise = true,
            "--oracle" => {
                let v = next()?;
                let (h, p) = v.rsplit_once(':').ok_or("--oracle wants HOST:PORT")?;
                cfg.oracle = Some(Endpoint::new(
                    h,
                    p.parse().map_err(|e| format!("oracle port: {e}"))?,
                ));
            }
            // The budget `ModelAdjudicator` enforces. A knob because it decides which models can hold
            // the seat at all, and the answer is different for an oracle on this box and one across
            // the LAN
            "--oracle-budget-ms" => {
                let v = next()?;
                cfg.oracle_budget = std::time::Duration::from_millis(
                    v.parse().map_err(|e| format!("--oracle-budget-ms: {e}"))?,
                );
            }
            // What the guard is asked: `verdict` (its own ALLOW/DENY/UNSURE) or `scores` (FIT and
            // CLAIM 0-10, TraceGuard arXiv 2604.03968; the thresholds derive the verdict).
            // `--compare` measures both. The guard's output ceiling.
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
            // allow-all - the same placement a subagent gets from `where`.
            "--where" => {
                let w = next()?;
                let asked = letibot_tools::builtins::task::Placement::parse(&w)
                    .map_err(|e| format!("--where: {e}"))?;
                match asked {
                    // : the VM and the host are TWO asks, recorded here rather than
                    // resolved, so the answer does not depend on which flag came first.
                    letibot_tools::builtins::task::Placement::Firecode => vm_asked = true,
                    // `--where host:NAME` is `--host NAME` under another name, and `--where
                    // host:NAME:vm` is the pair under a third. The name is checked the same
                    // way: one rule, whichever flag spelled it.
                    letibot_tools::builtins::task::Placement::Remote(name) => {
                        let hosts = letibot_tools::remote::Hosts::load();
                        if hosts.get(&name).is_none() {
                            return Err(hosts.refusal(&name));
                        }
                        host_asked = Some(name);
                    }
                    letibot_tools::builtins::task::Placement::RemoteVm(name) => {
                        let hosts = letibot_tools::remote::Hosts::load();
                        if hosts.get(&name).is_none() {
                            return Err(hosts.refusal(&name));
                        }
                        vm_asked = true;
                        host_asked = Some(name);
                    }
                    letibot_tools::builtins::task::Placement::Host => {}
                }
            }
            // A copy of one repository instead of the workspace mounted - the old placement.
            "--vm-copy" => vm_copy = true,
            // Where this daemon's REVIEWERS run - `host` (the default: a reviewer child on this
            // machine, in a checkout of the head it judges, as the subagents that author the branches
            // already run) or `vm` (each reviewer booted in a firecode microVM).
            "--reviewers" => apply_reviewers_flag(&mut cfg, &next()?)?,
            // Where the merge gate runs. A name from the operator's `hosts.toml`, checked here for
            // `--host`'s reason: a name nobody declared is refused by this line rather than by a gate
            // that fails on the first branch after it.
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
            // Where this session's TOOLS run, on another machine (`docs/remote-hosts.md`): `--host
            // lab2x1`, the same shape as `--vm`.
            "--host" => {
                let name = next()?;
                let hosts = letibot_tools::remote::Hosts::load();
                if hosts.get(&name).is_none() {
                    return Err(hosts.refusal(&name));
                }
                host_asked = Some(name);
            }
            // Per-invocation overrides of `[queue] reviewers_at_once` and `reviewer_hosts`, for a
            // daemon above several repositories that has no one project file.
            "--reviewers-at-once" => {
                let n = next()?;
                let n: usize = n
                    .trim()
                    .parse()
                    .map_err(|_| format!("--reviewers-at-once: `{n}` is not a number"))?;
                if n == 0 {
                    return Err(
                        "--reviewers-at-once: 0 would claim no review at all, which is a queue \
                         nobody can tell from a broken one - the smallest number that means \
                         anything is 1"
                            .to_string(),
                    );
                }
                cfg.reviewers_at_once = Some(n);
            }
            "--reviewer-hosts" => {
                let list = next()?;
                let mut out: Vec<String> = Vec::new();
                for entry in list.split(',') {
                    let name = entry.trim();
                    if name.is_empty() {
                        continue;
                    }
                    if crate::mergequeue::review::hosts::is_forbidden(name) {
                        return Err(format!("--reviewer-hosts: `{name}` is never used"));
                    }
                    if name != "local" && name != "*" {
                        // The same rule `--host` follows: a name that is not declared is refused
                        // here, before a daemon starts, rather than by a review that falls back.
                        let hosts = letibot_tools::remote::Hosts::load();
                        if hosts.get(name).is_none() {
                            return Err(format!(
                                "--reviewer-hosts: {}. `local` is this machine, and `*` is every \
                                 host the file declares",
                                hosts.refusal(name)
                            ));
                        }
                    }
                    if !out.iter().any(|e| e == name) {
                        out.push(name.to_string());
                    }
                }
                if out.is_empty() {
                    return Err(
                        "--reviewer-hosts: an empty list - name the machines a reviewer may use \
                         (`local` for this one), or leave the flag out for this machine only"
                            .to_string(),
                    );
                }
                // `*` is every declared host, as in `leticode.toml`.
                cfg.reviewer_hosts = if out.iter().any(|e| e == "*") {
                    None
                } else {
                    Some(out)
                };
            }
            // Passed to `firecode up` verbatim, repeatable: `--vm-arg --mem --vm-arg 8192`.
            "--vm-arg" => cfg.vm_args.push(next()?),
            // What is behind `web_search`. Without it the tool refuses and names this flag, which is
            // the state every session has had until now. The wall, when the operator knows it and the
            // server will not say - a metered provider, or a proxy that does not serve /props.
            "--context-window" => {
                cfg.context_window = Some(
                    next()?
                        .parse()
                        .map_err(|e| format!("--context-window: {e}"))?,
                )
            }
            "--no-auto-compact" => cfg.auto_compact = false,
            // The explicit "ignore the project file". The walk-up finds the nearest
            // `leticode.toml` at or above the workspace, and this is how an operator says "not for
            // this daemon" without deleting the file
            "--no-project-config" => no_project_config = true,
            "--web-search" => {
                cfg.web_search = match next()?.as_str() {
                    // `none` is how an operator with a key in providers.toml
                    // turns the tool off for one daemon without deleting it.
                    "none" | "off" => None,
                    other => Some(other.to_string()),
                }
            }
            // Held by the websearch crate, not by `Config` - which derives
            // `Debug`, and a secret in a struct that can be `{:?}`-printed is a
            // secret one `eprintln!` away from a log.
            "--brave-key" => letibot_websearch::set_flag_key(next()?),
            // What is behind `web_fetch`: the curl subprocess the webfetch
            // crate wraps. No key and no account - the flag is the whole
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
            // says so at startup - see `Config::disclosures`.
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
            // `--system` is the prompt; `--system-file` is where to read one from. The string
            // form exists because the operator expected it: *"wtf is --system FILE? I also want a string"*.
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
            // prompts rather than to hold a conversation - the guard being the
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
            // The version, because an INSTALLER has to be able to prove the binary runs. The
            // one-line installer copies a binary it downloaded and then says what it got - `rano`'s
            // own installer
            "-V" | "--version" => {
                println!("harnessd {}", env!("CARGO_PKG_VERSION"));
                return Ok(0);
            }
            other => return Err(format!("unknown argument {other}\n\n{}", usage())),
        }
    }

    // A RUN OF STEPS IS A ONE-SHOT, and the session's tools are told so. `--prompt` and
    // `--slash` are what `letibot "a question"` passes: the daemon runs the steps, prints the
    // answers and exits, and it exits with them.
    cfg.one_shot = !steps.is_empty();
    if cfg.one_shot {
        cfg.serve_queues = false;
    }

    // Where this session's tools run - one answer from the two asks, and the pair this
    // build cannot honour is refused here rather than silently resolved to one of them.
    cfg.placement = placement_from_flags(vm_asked, host_asked.as_deref())?;
    // The session's VM works in the workspace itself, mounted (a Mac), unless the operator
    // asked for a copy - said anywhere on the line, before or after `--where`.
    cfg.vm_mount = !vm_copy && cfg.placement == letibot_tools::builtins::task::Placement::Firecode;

    // Where this daemon's gate runs, told to the queue once - the queue builds one runner
    // for the whole daemon (`QueueEnv::production`), so this is the seam and
    // `mergequeue::gate` reads it. `None` is this machine and is today's behaviour exactly.
    crate::mergequeue::gate::set_gate_host(cfg.gate_host.clone());

    // The box's room coordinator (the room's slice 2). Configured here, after every flag
    // and file has spoken and before anything is loaded or bound
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

    // What a corpus replay measured, written beside the store by `--calibrate-write`. Read
    // here, after `--store` is known, and absent/malformed is silently the declared scope, the
    // same fail-open a startup read must have.
    if let Some(earned) = crate::calibrate::read_calibration(&cfg) {
        // A measurement ADDS to a declaration; it never shrinks one. Replacing outright was
        // wrong, and the way it was wrong is the way that matters
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

    // One registry, seeded with the session named on the command line. A head can make more
    // over the socket
    say_memory_caps();
    crate::instruments::start_writer(&cfg);
    let registry = Registry::new();
    // An older binary on a newer store: keep working, and say so in red on every head.
    // Asked once, here, for the same reason `say_memory_caps` is
    let said = crate::store_floor::said(&cfg)?;
    // Said here as well as on the heads, and this is the daemon's own log - the file an
    // operator reads a week later when they wonder why a build was behind. Cheap, and the one
    // place the fact is on disk before any head exists.
    if let Some(said) = &said {
        eprintln!("  {said}");
    }
    registry.set_store_older(said);
    // The daemon watches its own memory - `memwatch`. One line a minute in this log, a
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
        // One store, two questions : the same instance lists sessions the registry is
        // not holding, and answers `FetchRow` for a row its bounded view has trimmed. Both are set
        // here because this is the one place that knows there is a store at all.
        registry.set_diagnostic_source(src.clone());
        registry.set_row_source(src.clone());
        registry.set_source(src);
    }
    // The operator's seat on the host coordinator, opened on a head's first chat request. The
    // coordinator may start later than the daemon, so nothing connects here.
    if let Some(socket) = crate::room::socket_path() {
        registry.set_room_chat(crate::room::operator::OperatorSeat::new(socket).seat());
    }
    // The smart `!`'s model half, attached whenever there is a local model to ask. The
    // endpoint is `cfg.oracle` - the `[gatekeeper]` endpoint read at startup, the LOCAL model
    // - and the suggester is installed on exactly that condition, the same one the guard is
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
    // The pane's pty, owned by this daemon. (`!term`.) Installed unconditionally, and
    // deliberately not behind a capability check. A pane is capped by the same policy a `bash`
    // call in its session gets, and it outlives its session only until the next daemon starts.
    let terminals = crate::term::Terminals::new(std::sync::Arc::downgrade(&registry)).with_memory(
        crate::term::PaneMemory::new(
            cfg.memory,
            cfg.leticode_user.clone(),
            cfg.read_project_config,
        ),
    );
    if let Some(record) = crate::term::record_path(&cfg.workspace) {
        // Before any head can ask for a pane: what the daemon this one replaced left running is
        // ended here, and this tree is recorded for whoever replaces this daemon.
        for line in terminals.begin(&record) {
            eprintln!("  {line}");
        }
    }
    registry.set_terminal(std::sync::Arc::new(terminals));
    // The stored row's parent and name come in with the first session, read once
    // here at startup
    let stored = registry.resumable(&cfg.session_id);
    let title = match (&cfg.title, &stored) {
        (t, Some(b)) if t.is_empty() => b.title.clone(),
        (t, _) => t.clone(),
    };
    // Who this session's parent is, by the tree's one rule - the caller's `--parent` when it
    // has one, else its own stored row. `Harness::open` asks the same function, so the registry's
    // entry and the session's config cannot disagree.
    let parent = registry.parent_for(
        &cfg.session_id,
        cfg.parent_session_id.clone(),
        stored.as_ref().and_then(|b| b.parent_session_id.as_deref()),
    );
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
    // Where this daemon's tools run, said once and beside the socket (RH2). The placement
    // is a fact of the PROCESS - every session this daemon opens runs its `bash`, `read`
    // `write` and jobs in the same place - and the protocol carries it nowhere
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

    // The review queue and the merge queue, before the worker loop takes this
    // thread. Their own threads - a supervisor that claims each repository's lease and a
    // worker per lease held - never a session's worker, and the process's one connection to
    let reviews = std::sync::Arc::new(letibot_tools::builtins::verdict::ReviewDesk::new());
    // And the daemon's Claude Code reviewers (`[queue.claude]`, R116): the table of processes the
    // queue's thread starts and the review pool polls. One `Arc` and both hold it, for the desk's
    // reason - a run one pass started is collected by the next.
    let claude = std::sync::Arc::new(crate::mergequeue::review::claude::Runs::new());
    let merge_stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    // A coordinator serves no repository, so it claims no lease and runs no gate: `--room`
    // sets `serve_queues` off, and `spawn_for` is not called at all rather than called and made to
    // find nothing. The room is the queue this daemon serves.
    let merge_queue = if cfg.room {
        None
    } else {
        crate::mergequeue::spawn_for(
            &cfg,
            merge_stop.clone(),
            registry.clone(),
            reviews.clone(),
            claude.clone(),
        )
    };

    let socket = daemon.socket().display().to_string();
    let dialect = cfg.dialect.name();
    let workspace = cfg.workspace.display().to_string();
    let stored = stored_sessions(&cfg);
    let session_id = cfg.session_id.clone();
    let store_path = cfg.store.clone();

    // The seat, before the first session: a `--flowy` that cannot be honoured is a startup
    // error, not a session that quietly hears nothing. Opening takes the local waiter claim
    // and the spool
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

    // The user's `leticode.toml`, at one fixed path beside `providers.toml`. The box-wide
    // level
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

    // The project's `leticode.toml`, discovered by walking up from the workspace. Read
    // after the user file rather than before, because the precedence is command line beats the
    // project file, the project file beats the user file, the user file beats the built-in
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
                // No project file at or above the workspace: the daemon's own models, and nothing to name.
                // Said rather than silent, because a project that expects a `leticode.toml` and does not
                // find one is a setting the operator will reasonably believe took effect.
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

    // The two file levels as one value, the project's over the user's. This is what the
    // spawn reads (`cfg.leticode`) and what the disclosure names, and merging here rather than
    // at each read is what makes a project that sets one key keep the user's others.
    cfg.leticode = user_level.overridden_by(&project_level);
    cfg.leticode_user = user_level.clone();
    // And the box's one embedder, here and not in `Config::for_the_daemon`: `[embeddings]` is read
    // off these two files, so it waits for them. One model, loaded once, shared by every caller.
    cfg.read_embeddings();

    // The GitHub reviewer seat (R118), installed on the registry so `letibot attach github <scope>`
    // reaches it, `/pane github/github` is drawn from it, and the idle pass polls it. `post` off is
    // the shipped state, and a file that asks to post without a token is said here and then held at
    // *off*: a daemon that refused to start over a reviewer would take the session down with it.
    {
        let home = std::env::var("HOME").unwrap_or_default();
        let config = match letibot_github::seat::Config::resolved(
            cfg.leticode.github.post.unwrap_or(false),
            cfg.leticode.github.token_path.as_deref(),
            std::path::Path::new(&home),
        ) {
            Ok(config) => config,
            Err(why) => {
                eprintln!("  github: {why} — this daemon keeps its reviews as drafts");
                letibot_github::seat::Config::default()
            }
        };
        // The panel's members are the ones this project's `[queue.claude]` names, the same opt-in
        // the merge queue reads: no table, no member, and a row that says so rather than a review
        // nobody made.
        let claude = match crate::mergequeue::settings::for_repository(&cfg.workspace) {
            Ok(settings) => settings.claude,
            Err(why) => {
                eprintln!("  github: {why} — its panel has no Claude member");
                None
            }
        };
        let root = std::path::Path::new(&home)
            .join(".local/share/letibot/github")
            .join(letibot_sessionlog::workspace_key(&cfg.workspace));
        let seat: std::sync::Arc<dyn letibot_sessionlog::registry::GithubSeat> =
            crate::github::Github::new(config)
                .production(root, claude, registry.clone())
                .install();
        registry.set_github(seat);
    }

    // `main_model`, through the one function that states the precedence. The command line
    // is the flag level when it chose WHO ANSWERS - `--provider`, and not `--model`, which is
    // the local BINDING the launcher passes on every start
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
        // The flag level, and `Level::describe` already names `--provider` - so there
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

    // The built-in default, and it is BELOW the two files. `[default]` in providers.toml -
    // what `/models NAME` writes - is the daemon's standing choice, and it is read here rather
    // than before the files so that a line the operator wrote in `leticode.toml` outranks it.
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

    // The guard's model, and the adjudicator's - two words for the ONE oracle this build
    // has, which `load` has already refused to see disagree. The precedence goes through the
    // one function that states it rather than being spelled out again here
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

    // A model a file named that this box cannot reach, said at startup. The third loud
    // failure, and the one the tree has paid for three times
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

    // The per-model profile out of `providers.toml` - `[model.<family>]` for the dialect's
    // effort, `[model."<alias>"]` for this model's sampling.
    {
        let profile = letibot_provider::keys::model_profile(cfg.dialect.family(), &cfg.model, None);
        if !profile.sampling.is_empty() {
            cfg.sampling = serde_json::Value::Object(profile.sampling.clone());
        }
        if cfg.effort.is_none() {
            cfg.effort = profile.effort.clone();
        }
        // Said, not swallowed. The defect above was a number nobody read; a
        // misspelled key is the same defect one line lower, so it gets a line.
        for u in &profile.unknown {
            eprintln!(
                "  providers.toml: [model] carries `{u}`, which nothing here reads. \
                 Sampling keys are temperature, top_p, top_k, min_p, presence_penalty, \
                 frequency_penalty, repeat_penalty, seed, max_tokens, thinking_budget_tokens."
            );
        }
    }

    // What to try when the model this session is on is OUT - `[fallback] models` in the
    // same file, read once here rather than at the moment of need. A model that cannot answer
    // is the one failure a person cannot fix by waiting.
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
    // A session's own list outranks the box's, because it is the narrower statement: a file in
    // the project says where THIS project's sessions go.
    session_fallback(&mut cfg);

    // Captured here rather than before the levels, because this is the banner's copy of
    // the model and the levels are what decide it: a file that set `main_model` would
    // otherwise be disclosed by the daemon's own launch line.
    let banner_model = BannerModel::of(&cfg);

    // The operator's `prompts.toml`, read once, here. The per-model system-prompt
    // overrides, beside `providers.toml` in the same config dir. Read at startup rather than
    // per session because the report belongs at startup
    {
        let path = crate::config::Prompts::path();
        match crate::config::Prompts::load(&path) {
            Ok(prompts) => cfg.prompts = prompts,
            Err(why) => {
                eprintln!("  prompts.toml: {why} — the session runs on the built-in prompt")
            }
        }
    }

    // The image marker, read off the server for the same reason the window is. It is a
    // per-instance random value published on `/props`, so `None` means *this endpoint takes no
    // images* - a metered provider, or a local server without `mtmd`. Nothing is assumed
    if cfg.provider.is_none() {
        cfg.media_marker = letibot_turn::serving::served_media_marker(&cfg.endpoint);
    }

    // The wall, read off the server rather than assumed. Only for a local endpoint
    if cfg.context_window.is_none() {
        cfg.context_window = match &cfg.provider {
            // A local endpoint states its own window, and `/props` is the only
            // place that number is true.
            None => letibot_turn::serving::served_ctx(&cfg.endpoint),
            // A metered provider's window comes from the catalogue. It is not skipped
            // for a provider, although "a guessed window would either compact a
            // conversation that had room or fail to compact one that did not".
            Some(pc) => pc.catalogue_window(&letibot_provider::catalogue::Catalogue::load()),
        };
    }

    // Loaded here, after every source of the provider has spoken - the flag, `main_model`
    // in leticode.toml, `[default]` in providers.toml - because which vocabulary a daemon
    // needs depends on who answers
    boot.step("loading the vocabulary");
    let mut parts = match Parts::load(&cfg) {
        Ok(p) => p,
        Err(e) => {
            let why = boot.failed(e.to_string());
            daemon.shutdown();
            return Err(why);
        }
    };
    // The desk the queue's thread is already holding - see where it is made, above. Every
    // session's harness registers its own `review_verdict` against `parts.reviews`, and a
    // reviewer's verdict has to reach the queue that asked for it, so this is not a second desk.
    parts.reviews = reviews;
    parts.claude = claude;

    // The room, before the first session opens. The coordinator's socket has to exist when
    // `Harness::open` builds the session's room link, and its session id has to be the
    // arbiter's before any event can be routed to it - so it is started here, after the
    let _coordinator = if cfg.room {
        let bell: std::sync::Arc<dyn Fn(&str) + Send + Sync> = {
            let bell = std::sync::Arc::clone(registry.bell());
            std::sync::Arc::new(move |session: &str| bell.ring_wake(session))
        };
        match crate::room::coordinator::Coordinator::start(&cfg.session_id, bell) {
            Ok(c) => {
                // And the tags' agents, on a thread of their own. The coordinator's server is what
                // knows which tags have work in them, and the registry is what knows whether a tag's agent
                // is still there - so the upkeep is handed both and no bell
                let ledger = letibot_room::AgentLedger::new();
                c.server().with_agents(std::sync::Arc::clone(&ledger));
                crate::room::agents::spawn(
                    c.server().clone(),
                    std::sync::Arc::clone(&registry),
                    letibot_room::host_key(),
                    cfg.session_id.clone(),
                    ledger,
                    c.stop_flag(),
                );
                Some(c)
            }
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

    // This daemon's life, written down - its start now, a heartbeat while it runs, and its
    // stop on the orderly path below - so the next daemon can say how long it was down
    // (`crate::restart`).
    let lifeline = crate::restart::Lifeline::start(cfg.store_at(), registry.clone());
    // The VM reaper, from the start - a VM whose owner is finished, or whose owner's
    // daemon is gone, is brought down rather than left holding a slot every review then waits
    // on (`crate::vm_reap`). Its own thread, so a reap that takes minutes is not the worker's
    crate::vm_reap::spawn(cfg.store_at(), registry.clone());
    // The process supervisor, from the start - RH7. Its own thread, and its FIRST tick is
    // the recovery: it reads every record the last daemon left open, checks each host once
    // and writes down which processes are still the ones recorded.
    crate::supervise::spawn(cfg.store_at(), registry.clone());
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

    // After the session is open, not before: the adjudication line is read from the backend
    // the gate and the seated schemas, and none of those exist until the harness is built. A
    // banner computed early is a banner describing a session that had not been wired yet.
    let disclosures = sessions
        .with_harness(&session_id, |h| h.config().disclosures(h.wiring()))
        .unwrap_or_default();

    eprintln!("harnessd: session {session_id}");
    // The model, AND where it came from. The operator's own question of the evening -
    // *"why is my session on deepseek"* - was answered by a file they had to go and read, and
    // a banner that says `model deepseek` without saying *which level* is the same defect one
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
        // from, and where the reader stands - a reading, not a claim.
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
                    // Under a metered provider there is no f_keep to read - the
                    // cache figures are the provider's - so the footer says what it
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
        // The steps are over, so the one-shot's signal grace no longer applies. A turn that
        // was interrupted properly and then spends its orderly tail (up to five seconds of
        // subagent threads, a flowy seat stopping) must not be killed by its own watchdog with a
        crate::daemon::one_shot_steps_done();
        daemon.shutdown();
        // The same wait the serving path makes, for the same reason. The process ends at the
        // `Ok` below - `bin/harnessd.rs` turns it into `std::process::exit` - and a subagent
        // thread still inside libllama when that runs the ELF finalizers is a SIGSEGV after the
        if let Some(said) = sessions.drain_subagents(SUBAGENT_EXIT_GRACE).sentence() {
            eprintln!("  {said}");
        }
        // And hand the placement's work back before the process ends: a remote host's
        // branch, a VM's copy. See `Sessions::close_backends`.
        for landed in sessions.close_backends() {
            eprintln!("  {landed}");
        }
        // And a guest whose session was never closed here. A session served on its own thread
        // holds its backend there, so `close_backends` cannot reach it, and this process is about
        // to end. Bounded, and it says where each copy-out landed.
        for said in letibot_tools::remote::guests_down() {
            eprintln!("  {said}");
        }
        lifeline.stopped();
        if let Some(seat) = &seat {
            seat.stop();
        }
        return Ok(if failed > 0 { 1 } else { 0 });
    }

    // The HTTP head, before the worker loop takes this thread. Its own thread, its own
    // engine, its own scratch transcripts
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
        // A command a head sent, and what it came to. The wording is the daemon's and the
        // turn's own thread reports it through this door - one line either way.
        OutcomeSaid::Command(cmd, outcome) => match outcome {
            // The session is named on every line: with more than one of them, "who did
            // what" is only half the question and the other half is answerable
            // from this log.
            Outcome::Replied(r) => eprintln!(
                "  {session} · {} -> {} round(s), {} tool call(s)",
                cmd.identity, r.rounds, r.tool_calls
            ),
            // The base is the LEDGER's pair (the fork's own measurement), and says so: the
            // count in the provider's units is the `compacted:` line the session printed just
            // above this one.
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
            // `Sessions::wake` - the arm exists because the outcome is the worker's vocabulary.
            Outcome::HandedOn => {}
        },
        // A wake has no command behind it. It is said by whichever thread ran the turn, in the same words as the
        // worker's own `monitor ->` line.
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
    // Every step of the orderly stop is SAID, as it starts. What follows is
    // bounded but not short - a gate's process group killed, the VMs brought down, five
    // seconds of subagent threads - and until this, the daemon was silent through all of it.
    eprintln!("  stopping: bringing every subagent VM down");
    // Every subagent's VM comes down with the daemon - first. Their backends live on
    // threads the process's exit does not unwind, so their `Drop` never ran and the VMs
    // outlived it - two of the acceptance suite's own runs.
    letibot_tools::firecode::down_all();
    // And every guest a remote session booted, whose `down` is the host's own client: the same
    // act, one machine over, and the line says where each copy-out landed.
    eprintln!("  stopping: bringing every remote session's VM down");
    for said in letibot_tools::remote::guests_down() {
        eprintln!("  {said}");
    }
    // The queues are told to stop.
    eprintln!(
        "  stopping: the merge queues (a gate in flight is killed, its row put back in line)"
    );
    merge_stop.store(true, std::sync::atomic::Ordering::Relaxed);
    // Joined, and bounded: a gate in flight is killed with its whole process group
    // (SIGTERM, five seconds, SIGKILL) and its row goes back in line, and each worker releases
    // its lease so a successor need not wait out the heartbeat.
    if let Some(h) = merge_queue {
        let _ = h.join();
    }

    eprintln!("  stopping: closing every session");
    daemon.shutdown();
    // The seat after the daemon: the listener's next poll window sees the stop and the waiter
    // claim is released with it.
    if let Some(seat) = &seat {
        seat.stop();
    }
    // A placed session's commits come home only while its backend is still reachable, and a session
    // served on its own thread keeps that backend there - so this settles BEFORE the drain below,
    // which is what ends those threads. Every line is the branch and its commit, or why not.
    eprintln!("  stopping: bringing every placed session's branch home");
    for landed in sessions.close_backends() {
        eprintln!("  {landed}");
    }
    // Placements no close could reach: a session's own thread holds its backend, so a stop gets neither sweep nor fetch.
    // Runs after the closes, since a session that did close already ended its processes and brought its branch home.
    eprintln!("  stopping: ending what the placed sessions no close reached started on its host");
    for said in letibot_tools::remote::placements_down() {
        eprintln!("  {said}");
    }
    // Every subagent thread this daemon started is waited for before the process ends.
    // `daemon.shutdown()` above closed every hub, which is what a child parked between turns
    // leaves on, and `drain_subagents` tells the running ones to stop first - a hub close does
    eprintln!(
        "  stopping: waiting up to {}s for subagent threads to leave",
        SUBAGENT_EXIT_GRACE.as_secs()
    );
    if let Some(said) = sessions.drain_subagents(SUBAGENT_EXIT_GRACE).sentence() {
        eprintln!("  {said}");
    }
    // The orderly stop, with its exact time - what the next daemon measures its down time from.
    lifeline.stopped();
    eprintln!("  stopping: done");
    drop(sessions);
    drop(seat);
    Ok(0)
}

/// What Claude Code has on this box, one line per transcript - the id `--session
/// cc-<id>` takes, when it was last written, how big it is, the model that answered, and
/// the first thing that was said.
fn list_claude(tsv: bool) -> Result<i32, String> {
    let Some(dir) = letibot_claude::default_projects_dir() else {
        return Err("no $HOME, so no Claude Code transcript directory to list".into());
    };
    list_claude_in(&dir, tsv)
}

/// The listing itself, against a named directory - so a test can point it at a fixture
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

/// Write this session's conversation out as a Claude Code transcript and a Markdown
/// copy, into `to`. Two files, and they promise different things
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
    // No per-row times, and the report says so. This head's store records the row, its
    // tokens and its chain - not a clock per row - so a handover cannot carry one without
    // inventing it.
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

/// The line that says a conversation moved, on the transcript it moved from. This is
/// the shape the operator chose over a full Claude-format transcript
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

    // Where the detail goes: the caller's directory, else the session's own workspace -
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

/// `branch @ short-sha`, or a sentence saying why it could not be read - the line names a
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

/// One line on the end of a transcript Claude Code owns, with a backup taken and proven
/// first - and nothing written at all when that backup could not be made or does not match.
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

/// Put a transcript back, byte for byte, from a backup this binary made.
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

/// How many sessions the store already holds, or `None` when there is no store. A `SELECT`
/// through `Store::connection`, which is the escape hatch that exists for exactly this
fn resume_rows(cfg: &Config) -> Option<u32> {
    let store = cfg.store_at()?.open_existing().ok()?;
    let t = store.current_transcript_id(&cfg.session_id).ok()??;
    store.item_count(&t).ok()
}

fn stored_sessions(cfg: &Config) -> Option<u64> {
    let store = cfg.store_at()?.open_existing().ok()?;
    store
        .connection_of("session")
        .query_row("SELECT COUNT(*) FROM session", [], |r| r.get::<_, i64>(0))
        .ok()
        .map(|n| n as u64)
}

/// The disclosure for what else is in the store.
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
    Resolve(String),
    Find(String),
    Rename(String, String),
    Delete(String),
    Listen(Vec<String>),
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
    /// The lighter shape: the composed "continued in letibot" line on the original
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

/// The id is the mistake, so the second line is how to find the right one. `not found: session
/// s-nope` named what was missing and left a person to guess which verb lists the ones there are.
fn no_such_session(e: impl std::fmt::Display) -> String {
    format!("{e}\n         `letibot --sessions` lists the sessions this store holds")
}

/// Answer a question about the store and exit. No socket, no vocabulary, no model.
/// `--listen WORDS`: `/listen` against the store before any daemon runs. `this` is the repository
/// the process's cwd is in.
pub(crate) fn listen_offline(
    cfg: &Config,
    store: &letibot_tokencore::store::Store,
    words: &[String],
) -> Result<i32, String> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| cfg.workspace.clone());
    let config = crate::leticode_config::LeticodeConfig::discover(&cfg.workspace)
        .and_then(|p| crate::leticode_config::LeticodeConfig::load(&p).ok())
        .unwrap_or_default();
    let config = config.queue_mute();
    let (lines, ok) = if words.is_empty() {
        (
            crate::mergequeue::listen::show(store, &cfg.workspace, config),
            true,
        )
    } else {
        let this = crate::mergequeue::listen::this_repo(&cwd);
        crate::mergequeue::listen::run(store, &cfg.workspace, config, words, this.as_deref())
    };
    for l in &lines {
        if ok {
            println!("{l}");
        } else {
            eprintln!("{l}");
        }
    }
    Ok(if ok { 0 } else { 2 })
}

/// The store a question about `session` reads: a split set opened with the key its own index row
/// records (`session.project`), or the store as configured when there is nothing to switch to.
fn session_project(at: &crate::config::StoreAt, session: &str) -> Option<crate::config::StoreAt> {
    let crate::config::StoreAt::Set { dir, .. } = at else {
        return None;
    };
    let index = at.open_existing().ok()?;
    let key: Option<String> = index
        .connection_of("session")
        .query_row(
            "SELECT project FROM session WHERE id = ?1",
            [session],
            |r| r.get(0),
        )
        .ok()
        .flatten();
    let key = key.filter(|k| !k.is_empty())?;
    Some(crate::config::StoreAt::Set {
        dir: dir.clone(),
        key,
    })
}

fn run_query(
    cfg: &Config,
    q: Query,
    scope: Option<&std::path::Path>,
    tsv: bool,
) -> Result<i32, String> {
    // The three Claude Code verbs that need no store of ours, answered before `--store` is
    // required - so `--list-claude` works on a box whose daemon has never started, and
    // `--note`/`--revert` act on a file rather than on a session. Package H.
    match &q {
        Query::ListClaude => return list_claude(tsv),
        Query::Note { transcript, text } => return append_note(transcript, text),
        Query::Revert { transcript, backup } => return revert_transcript(transcript, backup),
        _ => {}
    }
    // `--note-continued` needs the store - it composes the line out of the session's own
    // row - so it is answered below, with `--handover`, once `--store` is known.
    let at = cfg
        .store_at()
        .ok_or("no --store, and a question about stored sessions needs one")?;
    // A query that names ONE session reads that session's project file, whose key the shared index
    // records - not this folder's, which for a verb run from elsewhere is another file entirely.
    let at = match &q {
        Query::Handover { session, .. } | Query::NoteContinued { session, .. } => {
            session_project(&at, session).unwrap_or(at)
        }
        Query::Delete(id) => session_project(&at, id).unwrap_or(at),
        _ => at,
    };
    // The verbs below read one file's rows: a replay and a measurement want the history, which a
    // split set keeps in the project file - a single-file store answers its own path.
    let path = at.path("transcript");
    let path = &path;
    let store = at
        .open_existing()
        .map_err(|e| format!("opening {}: {e}", at.display()))?;

    match q {
        // Answered above, before the store was asked for. Here only because a `match` must be
        // exhaustive
        Query::ListClaude | Query::Note { .. } | Query::Revert { .. } => Ok(0),
        // This session's conversation, as a transcript Claude Code can resume, plus a
        // Markdown copy (Package H). The rows come from the store, which is the only place
        // a finished conversation exists whole.
        Query::Handover { session, to } => handover(store, &session, &to),
        // The lighter shape the operator chose: a composed line on the ORIGINAL
        // transcript, and the Markdown handover beside it.
        Query::NoteContinued {
            session,
            transcript,
            dir,
        } => note_continued(store, &session, &transcript, dir.as_deref()),
        // Re-render a prefix that was stored without its tokens. A `/reseat` fork wrote its
        // new prefix row with an empty token list and a zero `h_init`, so every transcript opened
        // on it verifies as a broken chain at row 0 and refuses to resume - with the
        Query::RepairPrefixes => {
            let empty: Vec<(String, String, String, String)> = store
                .connection_of("stable_prefix")
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
            // The transcripts' commands ran under a fixed shell - Claude Code's, opencode's, letibot's
            // own - so layer A reads them as it reads a live leticode session, with the shell
            // declared.
            let env = crate::harness::surroundings_for(cfg)
                .with_pinned_shell("the etalon: the harness that recorded it ran a fixed shell");
            let r = crate::etalon::report(&file, &env, 1_000_000)?;
            print!("{}", r.render());
            Ok(0)
        }
        Query::EtalonOracle(file) => {
            drop(store);
            // The best arm `--compare` found: the fullest brief, the verdict
            // question. Measured at 52/11 on the store's rows.
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
            // Same reason as the replay below: the backfill opens the store by path, which answers
            // this same connection.
            drop(store);
            let mut report = crate::backfill::plan(path)?;
            if write {
                crate::backfill::apply(path, &mut report)?;
            }
            print!("{}", report.render());
            Ok(0)
        }
        Query::Calibrate { write } => {
            // The store handle above is dropped before the replay opens the same file: both are
            // the process's one connection, so the drop gives back a handle and not a descriptor.
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
            store.rename_unique(&id, &title).map_err(|e| match e {
                letibot_tokencore::store::StoreError::NotFound(_) => {
                    no_such_session(format!("no such session: {id}"))
                }
                e => format!("renaming {id}: {e}"),
            })?;
            if title.is_empty() {
                println!("{id} has no name now");
            } else {
                println!("{id} is now {title:?}");
            }
            // A daemon holding this session has its own copy of the name in the registry and will not
            // see this write. Said rather than papered over: the rename is durable and the running
            // head's header is stale, which is a different thing from the rename not having happened.
            eprintln!(
                "note: a running daemon keeps its own copy of the name. Use /rename in \
                 the head to change it live."
            );
            Ok(0)
        }
        Query::Listen(words) => listen_offline(cfg, &store, &words),
        Query::Delete(id) => match store.delete_empty_session(&id) {
            Ok(()) => {
                println!("{id} deleted");
                // A deleted session is the one `DOWN`: every ask of it ends and every ask it made
                // is withdrawn, so nobody is left nagged by a board that no longer exists. The
                // room is told after the row is gone, and a box with no coordinator has no ask.
                match crate::room::link::retire(&id) {
                    Ok(said) => println!("the room was told: {said}"),
                    Err(why) if crate::room::socket_exists() => eprintln!(
                        "note: the room was not told `{id}` is gone ({why}), so asks about it stay \
                         open until one side's grace escalates"
                    ),
                    Err(_) => {}
                }
                Ok(0)
            }
            Err(letibot_tokencore::store::StoreError::NotFound(_)) => {
                Err(no_such_session(format!("no such session: {id}")))
            }
            Err(e) => Err(e.to_string()),
        },
        Query::Resolve(name) => {
            let id = resolve_session_name(&scoped(&store, scope)?, &name, scope)?;
            println!("{id}");
            Ok(0)
        }
        Query::Find(name) => {
            let Some(here) = scope else {
                return Err("--find-session needs --scope: the folder searched first".into());
            };
            let all = store.list_sessions().map_err(|e| e.to_string())?;
            match find_session(&all, &name, here)? {
                Found::Here(id) => println!("here\t{id}"),
                Found::Elsewhere(rows) => {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0);
                    for s in rows {
                        // Tabs in a title would shift the columns the launcher cuts.
                        let title = s.title.as_deref().unwrap_or_default().replace('\t', " ");
                        println!(
                            "there\t{}\t{title}\t{}\t{}",
                            s.id,
                            s.workspace_root,
                            age(now - s.last_activity_ms)
                        );
                    }
                }
            }
            Ok(0)
        }
        Query::Latest => {
            let latest = newest_conversation(scoped(&store, scope)?, scope);
            // What the pick walked past is said, not silently dropped. The launcher deliberately
            // does not capture this stream: it is the announcement of which conversation the next
            // thing typed lands in, and a pick that skipped ten rows to reach it should say so.
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
                // id and nothing else. Which one it picked, and why, goes to stderr -
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
            // The listing is GROUPED, and it lists everything - the operator: *"in
            // session view the automatic sessions must be grouped somehow, so my vs auto.
            let all = scoped(&store, scope)?;
            let groups = grouped(&all);
            if tsv {
                // TSV is a data format and it does not change. Five fields, tab-separated, one
                // conversation per line: that is what a script cuts, and a sixth column or a group header
                // line in the middle would break every reader for a fact a reader can compute.
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
            // The name leads: it is what a person types for `--session`. An unnamed session leads
            // with its id, which is its only handle; a named one keeps the id on the line below.
            let label = |s: &letibot_tokencore::store::StoredSession| -> String {
                match s.title.as_deref() {
                    Some(t) if !t.is_empty() => t.to_string(),
                    _ => s.id.clone(),
                }
            };
            let wid = all
                .iter()
                .map(|s| label(s).chars().count() + 2)
                .max()
                .unwrap_or(8);
            // The one row per session the listing draws, as a closure so the two groups that
            // are not plain rows (a child under its parent, everything else) share it.
            let row = |s: &letibot_tokencore::store::StoredSession, indent: &str| {
                println!(
                    "{:<wid$}  {:>5}  {:>7}{}",
                    format!("{indent}{}", label(s)),
                    s.items,
                    age(now - s.last_activity_ms),
                    // Where it LIVES, when that is not here. A row whose live copy is on a host is not a
                    // session this box is appending to, and a listing that offered it unmarked would be
                    // offering a conversation that stopped the day it moved (`docs/remote-hosts.md` RH4).
                    match &s.lives_on {
                        Some(on) => format!("  [lives on {on}]"),
                        None => String::new(),
                    },
                );
                // Details: the id (the handle `--session` also takes) and the workspace.
                println!("{indent}  {}  {}", s.id, s.workspace_root);
            };
            // The same rows `grouped()` built, for the one question the printing asks that the
            // grouping does not
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
                "{:<wid$}  {:>5}  {:>7}  (ID / WORKSPACE below)",
                "NAME", "ROWS", "AGE"
            );
            for (group, members) in &groups {
                // No empty group line, for the reason the picker draws none: a header saying
                // `(0)` teaches a reader to skip headers.
                if members.is_empty() {
                    continue;
                }
                // A group's line, and the count that is the whole reason for it. It is a header rather
                // than a column: a reader scanning for their own conversation stops at `mine`, and the
                // four lines under it answer *what else is here* in four numbers instead of forty rows.
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

/// Every stored session, optionally only those rooted at `scope`. The scope test is a
/// path prefix, not equality: a session opened in `~/Projects/rano/crates` belongs to
/// `~/Projects/rano` for the purpose of "what was I doing here".
fn scoped(
    store: &letibot_tokencore::store::Store,
    scope: Option<&std::path::Path>,
) -> Result<Vec<letibot_tokencore::store::StoredSession>, String> {
    let all = store.list_sessions().map_err(|e| e.to_string())?;
    let Some(root) = scope else { return Ok(all) };
    // The empty path is refused at the flag, and refused again here: `starts_with("")` is true
    // for every path, so a scope that reached this function empty would widen the query to the
    // whole box while looking like it had narrowed it.
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

/// The session a name means in this project: an id as typed, else the exact name
/// (case-insensitive) or its slug, else a unique prefix. Ambiguity and absence are refused with the
/// candidates.
fn resolve_session_name(
    all: &[letibot_tokencore::store::StoredSession],
    name: &str,
    scope: Option<&std::path::Path>,
) -> Result<String, String> {
    if let Some(s) = all.iter().find(|s| s.id == name) {
        return Ok(s.id.clone());
    }
    let named: Vec<_> = all
        .iter()
        .filter(|s| is_conversation(s))
        .filter(|s| s.title.as_deref().is_some_and(|t| !t.is_empty()))
        .collect();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let list = |rows: &[&letibot_tokencore::store::StoredSession]| {
        rows.iter()
            .map(|s| {
                format!(
                    "  {}  {}  {} ago",
                    s.title.as_deref().unwrap_or_default(),
                    s.id,
                    age(now - s.last_activity_ms)
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let hits = name_hits(&named, name);
    match hits.as_slice() {
        [one] => Ok(one.id.clone()),
        [] => {
            let here = scope
                .map(|p| format!(" under {}", p.display()))
                .unwrap_or_default();
            Err(if named.is_empty() {
                format!("no session named {name:?}{here}, and none here has a name")
            } else {
                format!(
                    "no session named {name:?}{here}. Named sessions:\n{}",
                    list(&named)
                )
            })
        }
        many => Err(format!(
            "{name:?} names {} sessions. Use an id or a longer name:\n{}",
            many.len(),
            list(many)
        )),
    }
}

/// The named rows a name means: the exact title or slug, else every title it prefixes.
fn name_hits<'a>(
    named: &[&'a letibot_tokencore::store::StoredSession],
    name: &str,
) -> Vec<&'a letibot_tokencore::store::StoredSession> {
    let want = name.to_lowercase();
    let title = |s: &letibot_tokencore::store::StoredSession| {
        s.title.as_deref().unwrap_or_default().to_lowercase()
    };
    let exact: Vec<_> = named
        .iter()
        .copied()
        .filter(|s| letibot_tokencore::store::same_name(&title(s), &want))
        .collect();
    if !exact.is_empty() {
        return exact;
    }
    named
        .iter()
        .copied()
        .filter(|s| title(s).starts_with(&want))
        .collect()
}

/// Where a name was found.
#[derive(Debug)]
enum Found {
    Here(String),
    Elsewhere(Vec<letibot_tokencore::store::StoredSession>),
}

/// A name under `here` first, as `--session` always meant. Only when nothing there matches, the
/// operator's own top-level sessions in every folder: queue, package and subagent rows never
/// match, so a name cannot open work the operator did not start.
/// A session is "here" when its folder is this one, or one of this folder's own worktrees. Not any
/// folder below it: from `~` that would be every session there is, and nothing would ever move.
fn same_folder(ws: &std::path::Path, here: &std::path::Path) -> bool {
    ws == here
        || ws.starts_with(here.join("worktrees"))
        || ws.starts_with(here.join(".claude/worktrees"))
}

fn find_session(
    all: &[letibot_tokencore::store::StoredSession],
    name: &str,
    here: &std::path::Path,
) -> Result<Found, String> {
    let local: Vec<_> = all
        .iter()
        .filter(|s| same_folder(std::path::Path::new(&s.workspace_root), here))
        .cloned()
        .collect();
    let local_named: Vec<_> = local
        .iter()
        .filter(|s| is_conversation(s))
        .filter(|s| s.title.as_deref().is_some_and(|t| !t.is_empty()))
        .collect();
    if local.iter().any(|s| s.id == name) || !name_hits(&local_named, name).is_empty() {
        return resolve_session_name(&local, name, Some(here)).map(Found::Here);
    }
    let named: Vec<_> = all
        .iter()
        .filter(|s| is_conversation(s) && is_operators(s))
        .filter(|s| s.title.as_deref().is_some_and(|t| !t.is_empty()))
        .collect();
    let hits = name_hits(&named, name);
    if hits.is_empty() {
        return match resolve_session_name(&local, name, Some(here)) {
            Ok(id) => Ok(Found::Here(id)),
            Err(e) => Err(format!("{e}\n(every folder was searched too)")),
        };
    }
    Ok(Found::Elsewhere(hits.into_iter().cloned().collect()))
}

/// Is this row a conversation a person can be brought back to? `--continue` promises to
/// reopen *your conversation*, and a store holds three kinds of row that are sessions
/// without being one
fn is_conversation(s: &letibot_tokencore::store::StoredSession) -> bool {
    // The queue's own session is known by its id, which never changes: its seat is a default one
    // and its title may be re-worded, so neither can tell it from the operator's conversation.
    s.parent_session_id.is_none()
        && !letibot_sessionlog::is_gatekeeper_session(&s.id)
        && !matches!(s.origin.as_deref(), Some("queue" | "package" | "one_shot"))
        && s.role.as_deref() != Some(crate::config::Seat::Gatekeeper.as_str())
        && !s
            .title
            .as_deref()
            .is_some_and(|t| t.starts_with(letibot_sessionlog::GATEKEEPER_TITLE_PREFIX))
}

/// Is this row the operator's own top-level session - what `--continue` reopens. A package's,
/// a one-shot's, the queue's and a subagent's are conversations to name, never to continue.
fn is_operators(s: &letibot_tokencore::store::StoredSession) -> bool {
    letibot_sessionlog::is_operator_top_level(&letibot_sessionlog::GroupRow {
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
}

/// The stored sessions, in the groups a person reads them in - `mine` first, then one
/// entry per automatic kind, each with its members in the store's own order (newest
/// activity first).
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
            // The store's row has both marks, so a reviewer parented on the operator's own
            // conversation - the layout before the queue had a session of its own - is the queue's
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
    /// The conversation to reopen, or `None` - and the three counts below are what tells the
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

/// The newest session that is a CONVERSATION - the rule `--continue` implements. # The
/// rule Of the sessions stored under the scope, take the ones that are conversations
/// ([`is_conversation`]) and have at least one row, and return the newest.
fn newest_conversation(
    mut all: Vec<letibot_tokencore::store::StoredSession>,
    scope: Option<&std::path::Path>,
) -> Latest {
    // An exact workspace match beats a descendant of it. Standing in `~` and asking to
    // continue should not hand back the conversation you were having in `~/Projects/rano`
    // while a `~` conversation exists
    if let Some(root) = scope {
        all.sort_by_key(|s| std::path::Path::new(&s.workspace_root) != root);
    }
    let in_scope = all.len();
    let mut hidden = 0usize;
    let mut empty = 0usize;
    let mut rows = Vec::with_capacity(all.len());
    for s in all {
        if !is_conversation(&s) || !is_operators(&s) {
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

/// Why there is nothing to continue, as a sentence a person can act on. Three stores
/// produce the same `None` and only one of them means *there is nothing here*, so the
/// answer names which one this is. The remedy matters more than the diagnosis
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

/// The startup disclosures, as something that can be scanned. The content is
/// `Config::disclosures()` unchanged and unabridged. What changes is the shape
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
        // A headline that can be read at a glance, then the sentence under it. Not one padded
        // column: `adjudication` is twelve characters and any column wide enough for it wastes a
        // fifth of an eighty-column terminal on every other row.
        out.push(format!("  {state}  {} ({})", d.subject, d.state));
        for line in wrap(&d.detail, width.saturating_sub(indent)) {
            out.push(format!("{:indent$}{line}", ""));
        }
    }
    out
}

/// The banner's two model facts, kept apart - what answers this session's turns and
/// when the daemon also holds a LOCAL binding that is a different thing, the vocabulary its
/// token ids are cut from.
struct BannerModel {
    /// The model that answers: `provider/model` for a metered session, the local alias else.
    answers: String,
    /// Where that name came from - a file level, a flag, the built-in default.
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

    /// The lines, in the order the operator reads them - the answering model first, always.
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

/// The banner's `context` line - the window as given and, beside it, the reserve in the
/// SAME units. The reserve was `Config::headroom()`, which is the PLANNING window's: in
/// ledger tokens, scaled by the session's ledger/provider pair.
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

/// The banner's `RESUMED` line, its count in the unit the wall is judged in (
/// follow-up, defect C).
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

/// The terminal width, when there is a terminal. Eighty when there is not - a log
/// file has no width and eighty is the width a log file is read at.
fn term_cols() -> usize {
    // SAFETY: `winsize` is plain integers, so all-zero is valid, and TIOCGWINSZ writes only into
    // the struct passed.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(2, libc::TIOCGWINSZ, &mut ws) } == 0 && ws.ws_col > 0 {
        ws.ws_col as usize
    } else {
        80
    }
}

#[cfg(test)]
#[cfg(test)]
mod tests;
