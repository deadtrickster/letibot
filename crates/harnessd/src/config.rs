//! What a daemon needs to know before it can open a session.
//!
//! Every field is a fact the daemon cannot invent. There is no `Default` for the
//! whole struct on purpose: a default endpoint, a default model and a default
//! workspace root together describe a session against somebody else's box, and the
//! failure would be a running daemon rather than an error.

use std::path::PathBuf;

use letibot_turn::Endpoint;
use serde_json::{Value, json};

use crate::dialect::Dialect;

/// §8.3 clause 5's budget, as configuration.
///
/// **`Unset` is the default and it is correct**, per D6: an unset budget is a
/// genuine no-op, not a hidden constant. Nothing spills until somebody configures
/// one. That is a session-config gap rather than a code one, it is invisible unless
/// said, and `harnessd --spill-inline N` is where it stops being invisible.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SpillPolicy {
    /// No budget. Every payload reaches the model whole. The daemon says so at
    /// startup rather than letting a reader assume spill is on.
    #[default]
    Unset,
    /// One inline ceiling for every tool, in bytes.
    Inline(usize),
}

/// Where a spilled payload is kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpillStorage {
    /// In this process. Dies with the daemon, which makes `read_spill` a lie
    /// across a restart — so it is only the default while the budget is `Unset`
    /// and nothing can spill.
    Memory,
    /// Under a directory, one subdirectory per session.
    Dir(PathBuf),
}

/// **Which role a session seats, and therefore what it can reach.**
///
/// Every capability in this harness was built behind a role and none of them was
/// constructible: `harnessd` seated [`letibot_tools::roles::m1_orchestrator`] as a
/// constant, so `write`, `edit`, `bash`, the job verbs, `monitor`, `todo`, `goal`,
/// `write_plan` and `say` existed, were tested, and could not be called by anything.
/// This enum is the flag that makes them reachable.
///
/// # Nothing widens by default
///
/// [`Seat::Orchestrator`] is the default and is exactly what a `letibot` invocation
/// gets today: read-only tools, a read-only backend, no adjudicator, and no code
/// path from a tool to a question (clause 4). An operator who passes no `--role`
/// gets no new capability from this file existing. That is the property, and it is
/// checked by a test rather than asserted here.
///
/// # The seat decides three things at once, and they are not independent
///
/// The role picks the tool list; the tool list's declared [`letibot_tools::Access`]
/// picks the backend constructor and decides whether an adjudicator is *required*.
/// Reading the second and third off the first is what stops a session from being
/// seated with `edit` over a read-only backend, or with `bash` and nobody to ask —
/// both of which start cleanly and then fail every call, which wastes a turn to
/// discover.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Seat {
    /// §8.4's `orchestrator`: `read`, `grep`, `glob`, `ask_code`, `ask_corpus`,
    /// `read_spill`. Read-only backend, no gate traffic. **The default.**
    #[default]
    Orchestrator,
    /// D9's plan mode: read, search, the intent list, and the two things that make
    /// a plan a plan — `write_plan` (a write scoped to plan documents by taking a
    /// *name* rather than a path) and `say`. Needs a writable backend for the plan
    /// file and an adjudicator for `write_plan` and `say`.
    Planner,
    /// §8.4's `researcher` with the web instead of `search_corpus`. Four of its
    /// seven seats refuse today for want of a backend; the two network ones still
    /// declare `Access::Network`, so it needs an adjudicator to be honest.
    Researcher,
    /// §8.4's `coder` without `bash`: `read`, `write`, `edit`, `grep`, `glob`,
    /// `read_spill`. Writable backend.
    Coder,
    /// The only role that can run a command: the job verbs, `monitor`, and — only
    /// behind [`Config::allow_bash`] — `bash`. **Confined backend, never
    /// `HostBackend::executable`.**
    Runner,
    /// leticode's seat: opencode's tool union by opencode's names — `read`, `write`,
    /// `edit`, `grep`, `glob`, `todo_write`, `skill`, `lsp`, `task`, plus `bash`
    /// behind [`Config::allow_bash`]. This is the seat that can spawn a subagent
    /// (`task`).
    ///
    /// **Unconfined, on purpose.** opencode has no workspace boundary: its `read`
    /// reaches the whole host and its permission model — not a jail — decides what a
    /// write or a command may do. leticode is the port of that model, so it roots its
    /// backend at `/` and leaves gating to the permission ruleset and the mode,
    /// rather than confining to the project like `coder` and `runner`.
    Leticode,
}

impl Seat {
    pub fn as_str(&self) -> &'static str {
        match self {
            Seat::Orchestrator => "orchestrator",
            Seat::Planner => "planner",
            Seat::Researcher => "researcher",
            Seat::Coder => "coder",
            Seat::Runner => "runner",
            Seat::Leticode => "leticode",
        }
    }

    /// Parse a `--role` value. Every unknown value names the five rather than
    /// falling back to the default: a typo that silently seated the read-only role
    /// would be an operator who thinks they have `edit` and does not.
    pub fn parse(s: &str) -> Result<Seat, String> {
        match s {
            "orchestrator" | "m1" => Ok(Seat::Orchestrator),
            "planner" | "plan" => Ok(Seat::Planner),
            "researcher" => Ok(Seat::Researcher),
            "coder" => Ok(Seat::Coder),
            "runner" => Ok(Seat::Runner),
            "leticode" | "opencode" => Ok(Seat::Leticode),
            other => Err(format!(
                "unknown role `{other}`; this build has orchestrator, planner, \
                 researcher, coder, runner, leticode"
            )),
        }
    }

    /// Whether this seat needs a backend that can change the operator's tree.
    ///
    /// Read off the seat rather than off a flag so the two cannot disagree. The
    /// *authoritative* answer is still the seated schemas — `GateWiring` reads
    /// those — and this is what decides which constructor to call before the
    /// schemas exist.
    pub fn needs_writable_backend(self) -> bool {
        matches!(self, Seat::Planner | Seat::Coder | Seat::Runner | Seat::Leticode)
    }

    /// Whether this seat needs a backend that can start processes, and therefore
    /// the boundary that goes around one.
    pub fn needs_exec_backend(self) -> bool {
        // **Coder joined Runner on 2026-09-11**, because a role that writes code and
        // cannot run it leaves the loop open at the point where it would have paid —
        // the model wrote a regression test and had no way to execute it.
        //
        // This costs nothing in file access: `HostBackend::confined` is `writable +
        // EXEC`, so a confined coder still edits its tree. What it adds is a boundary
        // the writable backend does not have — a project-rooted view where a secret
        // outside it is ABSENT rather than denied.
        //
        // It does NOT seat `bash`. That is still behind `--bash` for both seats, so
        // the capability arrives because somebody typed it.
        matches!(self, Seat::Runner | Seat::Coder)
    }

    /// The read-only grants this seat needs to be useful, beyond the project.
    ///
    /// Empty by default and filled from `--grant-ro`, never from a table here: a grant
    /// is the operator's decision and `Grant` carries a `why` so it can be explained
    /// and therefore revoked.
    pub fn default_grants(self) -> Vec<std::path::PathBuf> {
        Vec::new()
    }
}

/// How the daemon reaches the flowy fabric. `None` is no flowy at all — the
/// no-flowy mode `docs/tool-design-brief.md` §3b keeps — and it is the default.
///
/// Every field optional: what is missing is looked up along the usual path
/// (`$FLOWY_*`, `~/.config/flowy/env-<seat>`, `~/.config/flowy/agents/<seat>`),
/// and the daemon's banner says where each value came from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FlowyConfig {
    /// The seat's name. Omitted: `$FLOWY_AGENT`, else the only seat on the box.
    pub seat: Option<String>,
    pub addr: Option<String>,
    pub token_file: Option<PathBuf>,
    /// Declare the inbox reader at the head of the log before listening. OFF by
    /// default and never implied: a reader that silently appears is a typo that
    /// produces an inbox which is permanently empty, and the same refusal appears
    /// when the token has been switched — see `letibot_flowy::client::NodeError`.
    pub new_reader: bool,
}

/// A cloud provider for the turns — D10's mode 3. `None` is the local server,
/// which is the default and what every invocation got before this existed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderConfig {
    /// `deepseek` | `glm` | `grok`.
    pub name: String,
    /// The model id on the wire; the preset's default when `None`.
    pub model: Option<String>,
    /// `--api-key`, for a one-off. Otherwise the environment or
    /// `~/.config/letibot/providers.toml`.
    pub api_key: Option<String>,
    /// Ask the provider to think out loud (`thinking` on GLM).
    pub thinking: bool,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub dialect: Dialect,
    /// The alias the server reports. Recorded in `turn_metrics` and in the store;
    /// several models may share one dialect.
    pub model: String,
    pub endpoint: Endpoint,
    /// The server's context window, in tokens — the wall compaction exists for.
    /// Read from `/props` at startup (`serving::served_ctx`), overridable with
    /// `--context-window`, and `None` when nothing said: a metered provider has
    /// no `/props`, and a window that is not known must not be invented.
    pub context_window: Option<u64>,
    /// Compact automatically when a turn leaves less than [`Config::headroom`]
    /// of the window free. On by default where the window is known.
    ///
    /// `docs/compaction.md` §1: the trigger is `n_ctx` and memory pressure, and
    /// NOT quality — depth was measured not to hurt (1.000 at 60k against 0.829
    /// at zero). So the policy is *compact when you must, as late as possible*,
    /// and this is the "must".
    pub auto_compact: bool,
    /// The GGUF the vocabulary is read from. For a split model, the first shard.
    pub vocab_gguf: PathBuf,
    /// The root every read-only tool is confined to.
    pub workspace: PathBuf,
    pub socket: PathBuf,
    /// `None` keeps the transcript in memory only — usable, and honest about it.
    pub store: Option<PathBuf>,
    pub session_id: String,
    /// The session that spawned this one as a subagent, or `None` for a top-level
    /// session. Recorded into the store so a subagent tree is a fact on disk, not an
    /// id convention a picker has to reverse-engineer.
    pub parent_session_id: Option<String>,
    /// Whether this session's backend is rooted at `/` — opencode parity: `read`
    /// reaches the whole host and the permission ruleset, not a jail, is the gate.
    ///
    /// Set for [`Seat::Leticode`], and **inherited by its subagents**: a subagent
    /// re-seats to [`Seat::Coder`] for its tools but must not be re-confined to a
    /// project its parent already left. A subagent of a coder session stays confined,
    /// because its parent is.
    pub unconfined: bool,
    /// A human name for the session, or empty.
    ///
    /// Empty is the default and a head then shows the id. Deliberately **not**
    /// derived from the first prompt: a title guessed from content is a title that
    /// changes under you, and a session picker whose rows rename themselves is a
    /// picker you cannot learn.
    pub title: String,
    pub owner: String,
    /// The bootstrap system prompt. Part of the stable prefix; nothing volatile
    /// belongs in it (§5.2, and the operator paid for that rule).
    pub system: String,
    /// `low` / `medium` / `high` / `xhigh`, interpreted per dialect. It is prefix
    /// bytes, so changing it mid-session re-prefills everything.
    pub effort: Option<String>,
    pub sampling: Value,
    /// **Where this project sits**, as a named point. See
    /// [`letibot_tools::mode::Mode`].
    ///
    /// Read from the per-project store by the daemon, never chosen here: the default
    /// is [`letibot_tools::mode::UNSEEN_PROJECT`], which is *always-ask* — nothing that
    /// is not a read happens without the operator.
    ///
    /// It is a separate field from [`Config::seat`] because they answer different
    /// questions and conflating them is the defect this whole strand exists to undo: a
    /// **role** says which tools are seated, a **mode** says how much approval each one
    /// costs. `--role coder` used to mean both, so an operator who wanted to edit had
    /// to pick a role and thereby also picked an approval policy they were never shown.
    pub mode: letibot_tools::mode::Mode,
    /// The permission ruleset (allow/deny/ask per tool and pattern): the shipped
    /// preapproved list, then `~/.config/letibot/permission.json`, then
    /// `LETIBOT_PERMISSION` — see [`parse_permission`]. A subagent inherits this,
    /// so its calls are governed by the same rules as the session it came from.
    pub permission: letibot_tools::permission::Ruleset,
    /// The flowy seat this daemon holds, when asked to. See [`FlowyConfig`].
    pub flowy: Option<FlowyConfig>,
    /// What this session is denied below its role — a subagent's downgrade,
    /// inherited by every subagent it spawns and only ever added to. `none` for
    /// a session the operator opened. See `letibot_tools::schema::Downgrade`.
    pub downgrade: letibot_tools::schema::Downgrade,
    /// Where this session's tools run: the host (its own boundary), or a
    /// firecode VM — a subagent's `where`. `Host` for every session the operator
    /// opened.
    /// Which search provider is behind `web_search`, by name (`brave`), or `None`
    /// for the tool that refuses. **The key is deliberately not here**: it is
    /// resolved at attach from `$BRAVE_API_KEY` or `providers.toml`, so a secret
    /// never rides in a struct that derives `Debug`.
    pub web_search: Option<String>,
    /// Whether `curl` egress is attached behind `web_fetch`. A bool, not a
    /// provider name: this build has exactly one fetcher and it takes no
    /// credential, so there is nothing to choose. Off by default — a search
    /// provider is opted into by a key the operator placed in a file; a page
    /// fetch is the wider door (it reads whatever the address names), so it
    /// waits for the flag.
    pub web_fetch: bool,
    pub placement: letibot_tools::builtins::task::Placement,
    /// Extra arguments for `firecode up` when the placement is a VM — the
    /// operator's `--vm-arg`, verbatim, inherited by every subagent placed in a
    /// VM under this session.
    pub vm_args: Vec<String>,
    /// The cloud provider the turns go to, when not the local server.
    pub provider: Option<ProviderConfig>,
    /// What the fabric block in the system prompt is, said by whoever composed
    /// it (`Sessions`): live, cached with its age, or unreachable. `None` when
    /// there is no seat, and then there is no block.
    pub fabric: Option<String>,
    /// Paths bound READ-ONLY into the confined view, from `--grant-ro`.
    ///
    /// The boundary is hermetic by design — `$HOME` is a fresh tmpfs, so a toolchain
    /// under it is ABSENT rather than denied, which is the same property that keeps a
    /// secret out. That is correct and it is why `cargo` cannot run inside without one
    /// of these.
    ///
    /// **Read-only means readable into the transcript.** §3's second half is not
    /// enforced by the confinement module, so each of these is a decision with that
    /// consequence, printed next to the grant by `Boundary::describe`.
    pub grants_ro: Vec<std::path::PathBuf>,
    /// **Which role this session seats.** [`Seat::Orchestrator`] by default, which
    /// is what every invocation gets today. See [`Seat`].
    pub seat: Seat,
    /// **`bash` stays off even behind [`Seat::Runner`], and this is the flag.**
    ///
    /// Not caution for its own sake — a named, still-open hole.
    /// `docs/boundary-and-adjudication.md` §5, *"where the transcript edge is
    /// enforced"*: §3's invariant is that secret bytes may be consumed inside the
    /// boundary but may never enter the transcript, and the **single choke point
    /// every tool result passes through does not exist yet**. Today each tool
    /// builds its own body and `Baseline::of_paths` covers the path-shaped tools by
    /// *reproducing* the rule rather than by sharing an edge.
    ///
    /// So [`crate::harness::Harness`]'s confined backend keeps secret bytes out of
    /// the process's **view** — `~/.ssh` is absent from the mount namespace, not
    /// merely denied — and nothing yet stops a tool result carrying bytes from
    /// *inside* the view into the transcript. `bash` is the tool whose result is an
    /// arbitrary byte stream, so it is the one that turns that gap from theoretical
    /// into reachable. The job verbs, `monitor`, `read`, `grep` and `read_spill` do
    /// not: their outputs are shaped by the tool.
    ///
    /// The disclosure says exactly this, so an operator who passes the flag is
    /// making a decision rather than accepting a default.
    pub allow_bash: bool,
    /// Who decides a gated call. See [`AdjudicatorChoice`].
    pub adjudicator: AdjudicatorChoice,
    /// Where layer B lives, `HOST:PORT` speaking llama.cpp's `/completion`.
    /// Required by [`AdjudicatorChoice::Model`] and meaningless without it.
    pub oracle: Option<Endpoint>,
    /// **Which model answers at that endpoint**, when it is not the one doing the
    /// work. `[gatekeeper] model` in the operator's providers.toml.
    ///
    /// `None` means *the session's own model*, which is the right default for a
    /// guard on the same server. It stopped being right the moment the operator
    /// put the guard on another box: the request named `glm-5.3-flash` to a server
    /// holding a 27B, and the banner announced the guard by the wrong name — a
    /// disclosure that is a guess about which model is guarding.
    pub oracle_model: Option<String>,
    /// **The authority the operator has declared for their guard**, from
    /// `[gatekeeper] intents` / `max_scope` in providers.toml. `None` is the
    /// built-in floor, which is what a box that has said nothing gets.
    pub oracle_scope: Option<letibot_tools::authorise::OracleScope>,
    /// Layer B's latency budget. The trait's default is 400ms and
    /// `ModelAdjudicator` abandons an oracle that overruns, so this is the knob
    /// that decides whether a given model can hold the seat at all.
    pub oracle_budget: std::time::Duration,
    /// What the guard is asked: a verdict, or two scores the thresholds turn
    /// into one. See `crate::oracle::Question`.
    pub oracle_question: crate::oracle::Question,
    /// **Start with the guard model consulted on every gated call.**
    ///
    /// Only a starting value: `/supervise` moves it at run time, which is the point
    /// of it being a flag on the gate rather than a mode. Meaningless without
    /// `oracle`, and the gate says so by name rather than pretending.
    pub supervise: bool,
    /// **Whether the intent check reads the assistant's prose as well as its tools.**
    ///
    /// Off by default, and the asymmetry is deliberate. The tool-declared half of
    /// the diff — an item declared with `todo` and a turn that ran nothing — cannot
    /// fire unless a role that seats `todo` or `goal` is in use, so it costs an
    /// existing session nothing and is always on. The prose half
    /// (`intent::commitments`) fires on any turn that ran no tools, which for a
    /// plain answer is the normal case, and *"you said you would X"* landing in a
    /// conversation that was working is the false positive that makes people turn
    /// the whole check off.
    ///
    /// It is the more useful half. It is also the one with a false-positive rate
    /// nobody has measured, so it is a flag rather than a default, and the cost of
    /// each choice is written here rather than discovered.
    pub intent_prose: bool,
    pub spill: SpillPolicy,
    pub spill_storage: SpillStorage,
    /// **The backstop, and only the backstop.** How many times one user turn may go
    /// round the tool loop before the daemon stops and says so.
    ///
    /// This used to be the *only* stop, at 12, and it cut two legitimate sessions —
    /// one asked to *"look at the project and suggest improvements"* and stopped
    /// mid-investigation, one two rounds from finishing with every round doing new
    /// work. A count of rounds measures **effort**, not progress: it is an open-loop
    /// guard in `docs/closed-loop.md`'s terms, and it cannot tell a model that is
    /// working hard from one that is stuck.
    ///
    /// [`Config::stall_rounds`] is the closed-loop half and is what actually stops a
    /// looping turn now, so this is set far out — a real investigation of a codebase
    /// is dozens of rounds and the old value was inside that range. What is left for
    /// this number to catch is a turn that keeps producing genuinely new results
    /// forever, which is a different failure and wants a different sentence.
    pub max_tool_rounds: usize,
    /// **How many consecutive rounds may produce nothing new before the turn stops.**
    ///
    /// A round counts as producing nothing new when none of its calls returned `Ok`
    /// with a result this turn had not already seen — see [`crate::progress`]. One
    /// round before this the model is told what the harness sees, once, so a turn
    /// that was in fact working can say so and carry on.
    ///
    /// `0` turns the check off and leaves [`Config::max_tool_rounds`] as the only
    /// stop, which is the state that cost the two sessions. It is reachable because
    /// a refusal that can only be routed around teaches people to route around
    /// refusals, and it is **disclosed** rather than silent.
    pub stall_rounds: usize,
}

/// **Who decides a gated call.**
///
/// `docs/tool-design-brief.md` §2.5: a gate that says "allowed" because nothing is
/// wired is worse than no gate. The inverse is also true and is what this enum is
/// for — a gate that refuses everything because nobody attached anything is not a
/// safety property, it is a session that starts cleanly and then fails every call.
/// The operator finds out one wasted turn later.
///
/// So a seat whose tools declare `Write`, `Exec` or `Network` gets
/// [`AdjudicatorChoice::Console`], and there is **no value that means nobody**.
/// That is not an oversight; see [`AdjudicatorChoice::parse`].
///
/// One variant today, and it is still an enum: this is the seam a second one lands
/// on, and [`AdjudicatorChoice::parse`] is where the two refusals live with their
/// reasons, which is the part that has to exist whether or not there is a choice
/// to make yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AdjudicatorChoice {
    /// [`crate::oracle::HttpOracle`] behind [`letibot_tools::ModelAdjudicator`].
    ///
    /// The only choice that can satisfy [`letibot_tools::mode::Prereq::Oracle`],
    /// and therefore the only one under which `automode` opens. It needs
    /// `--oracle HOST:PORT`; without one it is a choice that cannot be built,
    /// and `parse` says so rather than falling back to a person.
    Model,
    /// [`crate::answers::HeadAdjudicator`] over the session socket.
    ///
    /// **The default for any seat that can reach the gate**, and the reason is which
    /// shape people actually run: a daemon in the background with `letibot-tui`
    /// attached. It posts `DecisionRequested`, which the head already renders, and
    /// waits on the answer frame the head already has — see [`crate::answers`] for
    /// why that answer cannot come in through the command queue.
    ///
    /// A session with no head that can answer refuses **at once**, naming that fact,
    /// rather than sitting out a deadline.
    #[default]
    Head,
    /// [`letibot_tools::ConsoleAdjudicator`] over the daemon's own stdin/stderr.
    ///
    /// Right for `harnessd --prompt …` and for a daemon run in a foreground
    /// terminal, and **only** those: a head attached over the socket cannot answer
    /// it, which is D25. It is no longer the default and it is still selectable,
    /// because a foreground daemon with no head is a real way to run this and the
    /// console is the only thing that can reach a person there.
    Console,
}

impl AdjudicatorChoice {
    /// The two values this refuses are the interesting part.
    ///
    /// **`none`** would be a session with `write` or `bash` seated and nothing that
    /// can approve a call: every gated call returns `NotRun`, the model is told
    /// nobody decided, and the operator has a harness that looks wired and does
    /// nothing. There is no use for that state — a session that wants no writes is
    /// spelled `--role orchestrator`, which does not seat the tools in the first
    /// place, so the model is not told it has a capability it cannot use. The two
    /// spellings are not equivalent and the difference is which one lies to the
    /// model.
    ///
    /// **`model`** is the one somebody reaches for after reading
    /// `docs/boundary-and-adjudication.md`, and "unknown adjudicator" would read as
    /// this build not having the concept. It has the concept and no oracle behind
    /// it, which is a different fact and the one worth saying.
    pub fn parse(s: &str) -> Result<AdjudicatorChoice, String> {
        match s {
            "head" => Ok(AdjudicatorChoice::Head),
            "console" => Ok(AdjudicatorChoice::Console),
            "none" => Err(
                "there is no `none`. A seat with write, exec or network tools and \
                 nobody to decide is a session that starts, prints a banner, and \
                 refuses every call with `not_run` — and the model is meanwhile \
                 carrying tool definitions for capabilities it does not have. If you \
                 want a session that cannot write, pass `--role orchestrator`: it \
                 does not seat the tools, so nothing is claimed and nothing refuses."
                    .into(),
            ),
            // Wired as of the HttpOracle. It still needs `--oracle HOST:PORT`;
            // `Harness::open` refuses by name when the choice is made without
            // one, rather than silently falling back to a person.
            "model" => Ok(AdjudicatorChoice::Model),
            other => Err(format!(
                "unknown adjudicator `{other}`; this build has head, console"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            AdjudicatorChoice::Head => "head",
            AdjudicatorChoice::Console => "console",
            AdjudicatorChoice::Model => "model",
        }
    }
}

/// The system prompt M1 ships.
///
/// Two rules from §5.2 are visible in it, and both cost the operator real time:
/// it says how to use the tools and never what the data contains, and it **states
/// the answer language** — oracle's prompt did not, and a Chinese-trained model
/// drifted into Chinese mid-sentence under Russian input.
///
/// Nothing volatile: no timestamp, no cwd listing, no git branch. Adding one line
/// to an `<env>` block once cost a full cold re-prefill of a 179k conversation.
///
/// # The scratch sentence names the concept, never the path
///
/// The path carries this daemon's pid, so putting it here would give every
/// process its own stable prefix and a cold prefill with it — the exact cost the
/// paragraph above this one is about. The prompt says the directory exists and is
/// the model's to use; `harness status`'s `scratch` row says where. Same split as
/// the mode: the rule is in the prompt, the value is a row.
///
/// # The `transcript` sentence, and why it earned its re-prefill
///
/// It is one of the few additions worth that cost, because the behaviour it
/// changes is one nothing else can reach. A tool's own description is read when
/// the model is choosing among tools — but the moment this is about is the moment
/// BEFORE that, when the model has decided it does not know something and is
/// composing a question back to the operator. Nothing in a schema is consulted
/// there.
///
/// The trigger is narrow on purpose. "Something you do not know" is most things,
/// and a model that reaches for a tool about all of them is worse than one that
/// says so. The tell is specific: the operator refers to something **as already
/// settled**, in the tone of a thing you both know, and there is no record of it.
/// That is what a compaction leaves behind, and it is also the one case where
/// asking them to repeat themselves is asking for something they already said.
///
/// The sentence rides the read-only-tools paragraph rather than opening its own,
/// because it is the same rule about a different subject — *a file you have not
/// read is a file you do not know* — and two paragraphs would read as two rules.
pub const DEFAULT_SYSTEM: &str = "You are a careful software engineering assistant working in a \
checked-out source tree.\n\n\
Answer in English unless the user writes in another language, in which case answer in theirs.\n\n\
You have read-only tools. Use them for questions about **this tree** — its files, their contents, \
where something is defined — rather than guessing: a file you have not read is a file you do not \
know. The same holds for **this conversation**: if the user refers to something as already settled \
and you have no record of it, read it with `transcript` before asking them to repeat it — a \
compaction replaces earlier turns with a summary, and the turns themselves are still in the store. \
Do not call a tool for a question about the world, about a definition, or about arithmetic; \
answer those directly. When a tool reports that it found nothing, say so — do not fill the gap \
from memory.\n\n\
You have a scratch directory of your own — the `scratch` row of `harness status` names the \
exact path. Put working files there: a generated script, a downloaded page, intermediate \
output, anything you need on disk that the operator did not ask for. It is outside their tree, \
so nothing you leave in it touches their work, and creating, writing and deleting inside it \
need no permission.\n\n\
Use that path and no other. A directory you invent under /tmp is shared temp space: deleting \
there is a decision somebody has to make, and the name may already be another process's. Do \
not scatter temporary files through the workspace either.\n\n\
Be direct. Prefer the shortest answer that is complete.";

impl Config {
    /// How much of the window must stay free for a turn to be safe to start.
    ///
    /// A compaction is itself a turn: it re-sends the resident prompt and
    /// generates a summary, so firing it with no room left fails exactly like the
    /// turn it was meant to prevent. The reserve is the output budget plus a
    /// margin for the instruction and the next user message.
    ///
    /// Measured 2026-09-15: the session that died was at ~244k of 262144 — 93% —
    /// and the 500 arrived on the NEXT turn. A reserve of a sixteenth of the
    /// window (16k of 262144) would have fired the compaction two turns earlier,
    /// with room to summarise.
    pub fn headroom(&self) -> u64 {
        // A sixteenth of the window, floored at 2k so a summary has room, and
        // CAPPED AT A QUARTER so it cannot swallow the window it is reserving in.
        //
        // The cap is not hypothetical: the first version was `(w/16).max(8192)`,
        // which for any window of 8192 or less makes the reserve larger than the
        // window, `should_compact` true on every turn, and compaction a loop that
        // never lets a conversation start. Caught by asking what the formula does
        // at the edges rather than at 262144.
        let w = self.context_window.unwrap_or(0);
        (w / 16).max(2048).min(w / 4)
    }

    /// Is this turn's resident size close enough to the wall to compact first?
    /// `false` whenever the window is unknown — not knowing is not a reason to
    /// act, and an invented number here would compact conversations that had room.
    pub fn should_compact(&self, resident_tokens: u64) -> bool {
        let Some(w) = self.context_window else {
            return false;
        };
        self.auto_compact && resident_tokens + self.headroom() >= w
    }

    /// Would a turn starting at `resident` tokens fit, with the headroom reserved?
    ///
    /// The gate the wall continuation reads (`Sessions::after_turn`), and the
    /// difference from [`Config::should_compact`] is the point: `should_compact`
    /// answers *"must something be tidied first"* and folds in the `auto_compact`
    /// flag, while this answers *"is there room"* as a measurement of the window
    /// alone. The no-progress guard turns `auto_compact` off **without freeing
    /// anything**, so a gate that read the flag would see room that is not there
    /// and continue straight into a second wall. Unknown window: no room, for the
    /// same reason `should_compact` says false — an invented number here would
    /// continue turns that cannot fit.
    pub fn room_for_next_turn(&self, resident_tokens: u64) -> bool {
        match self.context_window {
            Some(w) => resident_tokens + self.headroom() < w,
            None => false,
        }
    }
}

/// Which search provider `web_search` gets, when the operator has not said.
///
/// **A configured key IS the opt-in.** The operator's rule, 2026-09-15: *"i dont
/// want to do leticode --web-search brave"*. Putting a key under `[brave]` in
/// `providers.toml` is already a deliberate act, and requiring a flag on top of
/// it means the capability is off in exactly the sessions whose operator went to
/// the trouble of configuring it.
///
/// So: a resolvable key attaches the provider, and `--web-search none` turns it
/// off. No key is still no tool — an unconfigured box is byte-identical to one
/// from before this existed.
///
/// The cost, stated because it is real and one-time: seating a tool changes the
/// stable prefix, so the first session after a key is added re-prefills. That is
/// the price of turning a capability on, and it is paid once per project.
fn default_web_search() -> Option<String> {
    letibot_websearch::resolve_key(None).ok().map(|_| "brave".to_string())
}

/// The permission ruleset, two layers in precedence order (last match wins):
/// `~/.config/letibot/permission.json` — the preapproved list, installed from
/// the repository's `config/permission.json` the first time no file is there
/// and the operator's from then on; the file an *Always allow* answer appends
/// to — then `LETIBOT_PERMISSION` (opencode's JSON object). A file that does
/// not parse is reported on stderr and skipped, never silently emptied.
fn parse_permission() -> letibot_tools::permission::Ruleset {
    use letibot_tools::permission;
    let mut rules = Vec::new();
    if let Some(path) = permission::file_path() {
        match permission::install_seed(&path) {
            Ok(true) => eprintln!(
                "letibot: installed the preapproved list at {} — it is yours to edit",
                path.display()
            ),
            Ok(false) => {}
            Err(e) => eprintln!("letibot: the preapproved list was not installed: {e}"),
        }
        match permission::load_file(&path) {
            Ok(mut r) => rules.append(&mut r),
            Err(e) => eprintln!("letibot: permission file not read: {e}"),
        }
    }
    if let Some(raw) = std::env::var("LETIBOT_PERMISSION").ok()
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw)
        && let Some(obj) = value.as_object()
    {
        rules.append(&mut permission::config_to_ruleset(obj).unwrap_or_default());
    }
    rules
}

impl Config {
    /// A session against the box this repository is developed on.
    ///
    /// Named `for_this_box` rather than `default` because that is what it is: the
    /// endpoint, the model alias and the GGUF path are this machine's, and a
    /// `Default` impl would let them travel silently.
    pub fn for_this_box(workspace: impl Into<PathBuf>) -> Config {
        Config {
            dialect: Dialect::Qwen,
            model: "qwen-3.8-flash-next".into(),
            endpoint: Endpoint::new("127.0.0.1", 8080),
            vocab_gguf: PathBuf::from(
                "/home/dead/models/qwen3.8-flash-next/\
                 Qwen3.8-Flash-Next-UD-Q6_K_XL-00001-of-00006.gguf",
            ),
            workspace: workspace.into(),
            socket: letibot_sessionlog::server::default_socket_path(),
            store: None,
            session_id: format!("s-{}", now_ns()),
            parent_session_id: None,
            unconfined: false,
            title: String::new(),
            owner: std::env::var("USER").unwrap_or_else(|_| "operator".into()),
            system: DEFAULT_SYSTEM.into(),
            effort: None,
            // Deterministic by default: a harness whose own measurements move
            // between runs cannot tell a regression from a sample.
            sampling: json!({"temperature": 0.0, "top_k": 1, "seed": 7}),
            // Nothing widens by default: this is exactly what an invocation got
            // before roles were reachable. `the_default_seat_is_what_shipped_before`
            // is the test that keeps it true.
            seat: Seat::default(),
            grants_ro: Vec::new(),
            mode: letibot_tools::mode::UNSEEN_PROJECT,
            permission: parse_permission(),
            flowy: None,
            downgrade: letibot_tools::schema::Downgrade::none(),
            web_search: default_web_search(),
            web_fetch: false,
            context_window: None,
            auto_compact: true,
            placement: letibot_tools::builtins::task::Placement::Host,
            vm_args: Vec::new(),
            provider: None,
            fabric: None,
            allow_bash: false,
            adjudicator: AdjudicatorChoice::default(),
            oracle: None,
            oracle_model: None,
            oracle_scope: None,
            oracle_budget: std::time::Duration::from_millis(400),
            oracle_question: crate::oracle::Question::Verdict,
            supervise: false,
            intent_prose: false,
            spill: SpillPolicy::Unset,
            spill_storage: SpillStorage::Memory,
            // Far out on purpose: the progress detector is the stop, and this is the
            // thing that catches a turn which never stops making new results.
            max_tool_rounds: 200,
            stall_rounds: 5,
        }
    }

    /// The things that are off, and why.
    ///
    /// A daemon that does not say "spill is unset" is a daemon whose operator finds
    /// out when a 40 MB grep result lands in the prompt.
    ///
    /// Takes [`GateWiring`] rather than deciding for itself, because the config does
    /// not know which tools were seated or how the backend was opened, and a
    /// disclosure that guesses is the thing this list exists to prevent.
    /// **Every setting this session runs under, one row each**, for the head's
    /// config pane (`/config`). The operator's ask: *"a config pane with all
    /// the configs and the one that can be changed at runtime editable"*.
    ///
    /// Three columns beside the key. `value` is rendered, never a secret.
    /// `source` is where the value came from **when this struct knows** — the
    /// mode carries its own provenance, the permission rules are a file, the
    /// provider key names its own source — and is empty otherwise, because the
    /// flags are parsed straight into fields and "flag" would be a guess for
    /// most of them. `editable` names the verb that changes the row now, and
    /// is empty for the rows that take a restart, which is most of them: a
    /// dialect, a vocabulary or a socket is what the process *is*.
    ///
    /// `mode_source` is the daemon's note about where the mode came from —
    /// the project store or the flag — since the struct itself does not keep it.
    pub fn settings(&self, mode_source: &str, supervising: bool) -> Vec<letibot_sessionlog::protocol::SettingRow> {
        use letibot_sessionlog::protocol::SettingRow;
        let row = |key: &str, value: String, source: &str, editable: &str| SettingRow {
            key: key.into(),
            value,
            source: source.into(),
            editable: editable.into(),
            choices: Vec::new(),
        };
        let choices = |mut r: SettingRow, of: &[&str]| -> SettingRow {
            r.choices = of.iter().map(|s| (*s).to_string()).collect();
            r
        };
        let mut out = Vec::new();
        // What changes now, first: it is what the pane is for.
        //
        // **The mode's choices travel with it.** `Mode::NAMED` is the one list
        // and the head is given it rather than keeping a copy — which it did,
        // and which drifted: it offered `supervised`, which is not a mode, and
        // not `automode-edits`, which is.
        out.push(choices(
            row("mode", self.mode.name.to_string(), mode_source, "/mode NAME"),
            &letibot_tools::mode::Mode::NAMED
                .iter()
                .map(|m| m.name)
                .collect::<Vec<_>>(),
        ));
        // **What answers the turns**, with the models this box can actually reach.
        // The choices travel with the row for the `mode` row's reason: a head that
        // kept its own copy of a list got it wrong, and this list is not even
        // fixed at build time — it comes from the catalogue.
        {
            let cat = letibot_provider::catalogue::Catalogue::load();
            let mut names = vec!["local".to_string()];
            for p in letibot_provider::presets::ALL {
                names.push(format!("{}/{}", p.name, p.default_model(&cat)));
            }
            let now = match &self.provider {
                // The local alias is kept in the value, because this row replaced
                // the read-only one that carried it and a pane that stopped
                // naming the model the server is running would be a worse row.
                // The first word is what a picker matches on, so it stays `local`.
                None => format!("local ({})", self.model),
                Some(pc) => match letibot_provider::Preset::parse(&pc.name) {
                    Ok(preset) => format!(
                        "{}/{}",
                        preset.name,
                        pc.model
                            .clone()
                            .unwrap_or_else(|| preset.default_model(&cat))
                    ),
                    Err(_) => pc.name.clone(),
                },
            };
            // A model the operator named that is not one of the defaults is still
            // where this conversation is, so it joins the list rather than being
            // silently absent from a menu that claims to show what answers now.
            if !names.contains(&now) {
                names.insert(1, now.clone());
            }
            let source = if self.provider.is_none() { "--model" } else { "" };
            let mut r = row("model", now, source, "/models PROVIDER/MODEL");
            r.choices = names;
            out.push(r);
        }
        out.push(choices(
            row(
                "supervise",
                if supervising { "on — the guard model answers".into() } else { "off".into() },
                "",
                "/supervise on|off",
            ),
            &["on", "off"],
        ));
        // What the session is.
        out.push(row("session", self.session_id.clone(), "", ""));
        out.push(row("seat", self.seat.as_str().to_string(), "--role", ""));
        out.push(row("workspace", self.workspace.display().to_string(), "", ""));
        // **Where the model may work without asking.** The path cannot go in the
        // system prompt — it carries this daemon's pid, and a prompt that differs
        // per process gives every process its own stable prefix and a cold
        // prefill with it. So the prompt names the concept and this row names the
        // place, which is the same split `/mode` uses for the mode.
        out.push(row(
            "scratch",
            crate::harness::scratch_dir().display().to_string(),
            "",
            "",
        ));
        out.push(row("owner", self.owner.clone(), "", ""));
        if let Some(p) = &self.parent_session_id {
            out.push(row("parent", p.clone(), "", ""));
        }
        // The `model` row is up with the changeable ones now: it used to sit here,
        // read-only, saying only the local alias — which is not what answers the
        // turns once a provider is set, and not something a head could act on.
        out.push(row("dialect", self.dialect.name().to_string(), "--dialect", ""));
        out.push(row("endpoint", self.endpoint.authority(), "--endpoint", ""));
        out.push(row("vocab", self.vocab_gguf.display().to_string(), "--vocab", ""));
        out.push(row(
            "context",
            match self.context_window {
                Some(n) => format!("{n} tokens"),
                None => "asked of the server".into(),
            },
            "",
            "",
        ));
        out.push(row("auto-compact", self.auto_compact.to_string(), "", ""));
        out.push(row("effort", self.effort.clone().unwrap_or_else(|| "default".into()), "--effort", ""));
        if let Some(p) = &self.provider {
            out.push(row(
                "provider",
                format!("{}{}", p.name, p.model.as_ref().map(|m| format!(" ({m})")).unwrap_or_default()),
                "providers.toml",
                "",
            ));
        }
        // The gate.
        out.push(row("adjudicator", self.adjudicator.as_str().to_string(), "--adjudicator", ""));
        out.push(row(
            "oracle",
            self.oracle.as_ref().map(|e| e.authority()).unwrap_or_else(|| "none".into()),
            "--oracle",
            "/supervise HOST:PORT",
        ));
        out.push(row(
            "oracle.model",
            self.oracle_model.clone().unwrap_or_else(|| "server's default".into()),
            "",
            "",
        ));
        out.push(row(
            "oracle.budget",
            format!("{:.1}s", self.oracle_budget.as_secs_f64()),
            "--oracle-budget",
            "",
        ));
        out.push(row(
            "oracle.question",
            self.oracle_question.as_str().to_string(),
            "--oracle-question",
            "",
        ));
        out.push(row(
            "oracle.scope",
            match &self.oracle_scope {
                None => "none".into(),
                Some(s) => format!(
                    "{} intent(s) up to {}{}",
                    s.intents.len(),
                    s.max_scope.as_str(),
                    if s.is_declared() { " (declared)" } else { " (earned)" }
                ),
            },
            "providers.toml [gatekeeper] / calibration",
            "",
        ));
        out.push(row(
            "permission",
            format!("{} rule(s)", self.permission.len()),
            "permission.json",
            "Always allow, from a prompt",
        ));
        out.push(row(
            "downgrade",
            if self.downgrade.deny.is_empty() {
                "none".into()
            } else {
                self.downgrade.deny.iter().map(|a| a.as_str()).collect::<Vec<_>>().join(",")
            },
            "",
            "",
        ));
        out.push(row("bash", self.allow_bash.to_string(), "--bash / --no-bash", ""));
        out.push(row("unconfined", self.unconfined.to_string(), "", ""));
        out.push(row(
            "grants.ro",
            if self.grants_ro.is_empty() {
                "none".into()
            } else {
                self.grants_ro.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
            },
            "--grant-ro",
            "",
        ));
        out.push(row(
            "placement",
            format!("{:?}", self.placement).to_lowercase(),
            "",
            "",
        ));
        if !self.vm_args.is_empty() {
            out.push(row("vm.args", self.vm_args.join(" "), "", ""));
        }
        // Tool results.
        out.push(row(
            "spill",
            match self.spill {
                SpillPolicy::Unset => "unset — nothing spills".into(),
                SpillPolicy::Inline(n) => format!("inline budget {n} bytes"),
            },
            "--spill-inline",
            "",
        ));
        out.push(row(
            "spill.storage",
            match &self.spill_storage {
                SpillStorage::Memory => "memory".into(),
                SpillStorage::Dir(d) => d.display().to_string(),
            },
            "",
            "",
        ));
        out.push(row("max-tool-rounds", self.max_tool_rounds.to_string(), "", ""));
        out.push(row("stall-rounds", self.stall_rounds.to_string(), "", ""));
        out.push(row("intent.prose", self.intent_prose.to_string(), "", ""));
        // The fabric.
        out.push(row(
            "flowy",
            if self.flowy.is_some() { "configured".into() } else { "off".into() },
            "",
            "",
        ));
        out.push(row(
            "fabric",
            self.fabric.clone().unwrap_or_else(|| "none".into()),
            "",
            "",
        ));
        out.push(row(
            "web_search",
            if self.web_search.is_some() { "configured".into() } else { "off".into() },
            "$BRAVE_API_KEY / providers.toml",
            "",
        ));
        out.push(row(
            "web_fetch",
            if self.web_fetch { "curl".into() } else { "off".into() },
            "--web-fetch",
            "",
        ));
        // The plumbing.
        out.push(row("socket", self.socket.display().to_string(), "--socket", ""));
        out.push(row(
            "store",
            self.store.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "none".into()),
            "--store",
            "",
        ));
        out
    }

    pub fn disclosures(&self, wiring: &GateWiring) -> Vec<Disclosure> {
        let mut out = Vec::new();
        match &self.spill {
            SpillPolicy::Unset => out.push(Disclosure::off(
                "spill",
                "UNSET",
                "no inline budget is configured, so no tool result will ever spill and \
                 every payload reaches the model whole (D6: unset is a genuine no-op). \
                 Pass --spill-inline BYTES to enable it.",
            )),
            SpillPolicy::Inline(n) => {
                out.push(Disclosure::on(
                    "spill",
                    format!("inline budget {n} bytes, all tools"),
                ));
                if self.spill_storage == SpillStorage::Memory {
                    out.push(Disclosure::off(
                        "spill store",
                        "MEMORY",
                        "a spilled payload does not survive a restart, so `read_spill` \
                         fails after one. Pass --spill-dir.",
                    ));
                }
            }
        }
        // **The stop condition, said out loud.** A turn that can only be stopped by a
        // round count is the state that cut two working sessions, and an operator who
        // does not know which of the two guards is armed cannot read a stop.
        if self.stall_rounds == 0 {
            out.push(Disclosure::off(
                "progress check",
                "OFF",
                &format!(
                    "nothing measures whether a turn is getting anywhere; the only \
                     stop is the {}-round backstop, which counts effort rather than \
                     progress. Pass --stall-rounds N.",
                    self.max_tool_rounds
                ),
            ));
        } else {
            out.push(Disclosure::on(
                "progress check",
                format!(
                    "stops after {} consecutive rounds producing nothing new; the \
                     round backstop is {}",
                    self.stall_rounds, self.max_tool_rounds
                ),
            ));
        }
        if self.store.is_none() {
            out.push(Disclosure::off(
                "store",
                "MEMORY",
                "the transcript is not persisted; the session ends with the process. \
                 Pass --store PATH.",
            ));
        }
        // R7, found live 2026-09-10: GLM's end-of-turn token is also the *name* of
        // a thing an agent may be asked to write, so a turn can stop mid-thought
        // with a normal `stop` and no content. The engine owns the parse and the
        // commitment, so the check is unconditional — which is why it is disclosed
        // as on rather than left out: a list of only the optional checks implies
        // the unconditional ones are absent.
        out.push(Disclosure::on(
            "mid-reasoning check",
            "a turn that stops inside an unterminated reasoning block with no \
             assistant content fails as UnfinishedReasoning and the loop asks the \
             model to continue",
        ));
        // Retrieval, the web tools, the forge and MCP: computed from what the
        // session attached and what it seated, never asserted. This line used to be
        // a constant sentence about `ask_code`, which was true and unchecked — the
        // same defect `GateWiring` exists to make unwriteable, one subject over.
        for row in wiring.external.startup_disclosures() {
            out.push(Disclosure {
                subject: row.subject.into(),
                state: row.state.into(),
                detail: row.detail,
                active: row.active,
            });
        }
        // **Where this project sits.** First of the three, because it is the one an
        // operator changes and the other two are consequences of it: the role says
        // which tools exist, the adjudicator says who answers, and this says how much
        // any of it costs. It was the missing line — an operator could read which role
        // was seated and could not read what that role would ask them.
        out.push(Disclosure {
            subject: "mode".into(),
            state: String::new(),
            detail: if self.mode.boundary == letibot_tools::mode::Boundary::Structural {
                format!(
                    "{}. The always-ask list does not ask here: the boundary is \
                     structural and nothing inside it reaches this box. A secret \
                     leaving the boundary is still refused.",
                    self.mode.describe()
                )
            } else {
                format!(
                    "{}. Nothing at this mode reaches an blocked action, and the \
                     always-ask list reaches you at every one of them.",
                    self.mode.describe()
                )
            },
            active: true,
        });
        // **The preapproved list.** What never asks, by count and by source, and
        // where an *Always allow* answer lands — so an operator who is asked about
        // `git status` can see that the list did not cover it rather than wonder
        // whether the list exists.
        {
            use letibot_tools::permission;
            let file = permission::file_path();
            let from_file = file
                .as_ref()
                .and_then(|p| permission::load_file(p).ok())
                .map(|r| r.len())
                .unwrap_or(0);
            let env = std::env::var_os("LETIBOT_PERMISSION").is_some();
            out.push(Disclosure {
                subject: "preapproved".into(),
                state: format!("{} RULES", self.permission.len()),
                detail: format!(
                    "{from_file} in {}{} — the list is that file (installed once from the \
                     repository's config/permission.json: read-only git and gh, cargo/go/npm/\
                     pytest build and test verbs, the shell's read-only utilities; yours from \
                     then on). A `bash` command is tested one simple command at a time — every \
                     segment must match, and a substitution, a redirection to a file or a \
                     group is never matched — and `Always allow` on a prompt appends the \
                     program and its verb to the file. `deny` rows outrank everything.",
                    file.as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "no file (no $HOME)".into()),
                    if env { ", plus $LETIBOT_PERMISSION" } else { "" }
                ),
                active: true,
            });
        }
        // **The scratch directory, to the MODEL.** The settings row beside the
        // workspace one is for the head's config pane; this is the disclosure the
        // `harness` tool reads, and the model is the reader that needs the path —
        // the prompt tells it the directory is its own and this says where.
        //
        // One place, spelled out, so there is nothing to guess: a model that
        // invented `/tmp/scratch` would be writing to shared temp space, where
        // deleting is an ask and another process may already own the name.
        out.push(Disclosure {
            subject: "scratch".into(),
            state: String::new(),
            detail: format!(
                "{} — this session's own working directory. Put generated scripts, \
                 fetched pages and intermediate output here rather than in the \
                 workspace. It is outside the operator's tree, and creating, writing \
                 and DELETING inside it need no permission. This exact path: a \
                 different directory under /tmp is shared temp space, where a delete \
                 is an ask and the name may already be somebody else's.",
                crate::harness::scratch_dir().display()
            ),
            active: true,
        });
        // **The seat, read from the resolved registry rather than from `--role`.**
        // A role that resolved to fewer tools than its name suggests is exactly the
        // thing an operator should be able to see, so the names travel with it.
        out.push(Disclosure {
            subject: "role".into(),
            state: String::new(),
            detail: format!(
                "seated as `{}`: {} tool(s) — {}. Access classes present: {}.",
                wiring.role,
                wiring.seated.len(),
                if wiring.seated.is_empty() {
                    "none".to_string()
                } else {
                    wiring.seated.join(", ")
                },
                {
                    let mut c = vec!["read"];
                    if wiring.has_write_tools {
                        c.push("write");
                    }
                    if wiring.has_exec_tools {
                        c.push("exec");
                    }
                    if wiring.has_network_tools {
                        c.push("network");
                    }
                    c.join(" + ")
                }
            ),
            active: true,
        });
        // The backend **quotes itself**. `HostBackend::describe` used to answer
        // `read-only` for every backend including the writable one, and a banner
        // that paraphrases a wiring is a banner that can be wrong about it.
        out.push(Disclosure {
            subject: "backend".into(),
            state: if wiring.backend_writable {
                "WRITABLE"
            } else {
                ""
            }
            .into(),
            detail: wiring.backend.clone(),
            active: true,
        });
        // **Plan mode, and the half of it this build cannot do.**
        //
        // Seating `planner` puts the session in plan mode for real — the state is
        // active, so `write_plan` and the ledger mean what they say and
        // `exit_plan_mode` is not a seat that can only refuse. What it cannot do is
        // the *widening*: a role's tool list is `tools_json`, which is stable-prefix
        // bytes, so re-seating `write` and `edit` mid-session rewrites message 0 and
        // costs a full cold re-prefill of the whole conversation. That is the one
        // thing this harness is built not to do (§5.3, measured at 179k tokens).
        //
        // So leaving plan mode records the plan and does not hand over the tools,
        // and an operator who reads this knows to open a coder session against the
        // plan rather than discovering it from a `write` that is not there.
        if self.seat == Seat::Planner {
            out.push(Disclosure::off(
                "plan mode",
                "NO HANDOVER",
                "this session is in plan mode and can record a plan (`write_plan`) and \
                 talk to the fabric (`say`). `exit_plan_mode` commits the plan to the \
                 intent list; it does NOT seat `write` and `edit`, because a role's \
                 tool list is stable-prefix bytes and re-seating mid-session is a full \
                 cold re-prefill of the conversation. Open a `--role coder` session \
                 against the plan to execute it.",
            ));
        }
        // `bash`, and why it is off. Only interesting where it could have been on.
        if self.seat == Seat::Runner && !wiring.seated.iter().any(|t| t == "bash") {
            out.push(Disclosure::off(
                "bash",
                "OFF",
                "the runner role is seated without `bash`, which is its own flag \
                 (--bash) even behind the role. The reason is a named hole, not \
                 caution: docs/boundary-and-adjudication.md §5 — the single choke \
                 point every tool result passes through does not exist, so the \
                 boundary keeps secret bytes out of the process's VIEW and nothing \
                 yet stops a tool result carrying bytes from inside that view into \
                 the transcript. `bash` is the tool whose result is an arbitrary \
                 byte stream. The job verbs and `monitor` are seated and shaped.",
            ));
        }
        let (state, detail, active) =
            letibot_tools::adjudicate::startup_disclosure_for(
                &wiring.adjudicator,
                wiring.backend_writable,
                // **The classes travel, rather than a bool that means "write".**
                // The gate is reachable from any class that is not unattended, so a
                // disclosure counting only writes would call a planner (`say`) or a
                // runner (the job verbs) unattended. Passing `true` for "something
                // is gated" fixed the on/off logic and produced a sentence saying
                // *"Write tools are callable"* about a session with no write tools —
                // correct about the boundary and wrong about the session, which is
                // this defect rather than a smaller version of it.
                &gated_classes(wiring),
                wiring.denials_surfaced,
            );
        out.push(Disclosure {
            subject: "adjudication".into(),
            state: state.into(),
            detail,
            active,
        });
        // **The authorisation trail.** Only worth a line where something can reach
        // the gate; on a read-only seat there is nothing to authorise.
        if wiring.has_write_tools || wiring.has_exec_tools || wiring.has_network_tools {
            if wiring.trail_installed {
                out.push(Disclosure::on(
                    "auth trail",
                    "the adjudicator is shown the operator's own words from this \
                     transcript, with their distance in turns and in seconds. \
                     §2: the same command is authorised or not depending on what \
                     was just said, so a decision without the trail is a decision \
                     about a different question.",
                ));
            } else {
                out.push(Disclosure::off(
                    "auth trail",
                    "NOT COLLECTED",
                    "no trail source is installed, so every adjudication sees \
                     `NotCollected` — nobody looked, which is NOT evidence that the \
                     operator said nothing. A model adjudicator refuses on it \
                     rather than deciding blind, and a human one is asked to \
                     approve a command with no idea what asked for it.",
                ));
            }
        }
        // **The corpus.** Only worth a line where something can reach the gate, for
        // the trail's reason: a read-only seat decides nothing to record.
        if wiring.has_write_tools || wiring.has_exec_tools || wiring.has_network_tools {
            match wiring.corpus {
                Some(c) => out.push(Disclosure::on(
                    "corpus",
                    &format!(
                        "every decision this gate makes is written to the session \
                         store, with the model's verdict and the operator's ruling in \
                         separate columns so a disagreement survives as a label. \
                         {} decisions recorded: {} you answered yourself, {} measured \
                         against a model, {} where the two differ. \
                         `/gate recent` shows them; `/gate ok|grant|revoke ID [note]` \
                         rules on one after the fact.",
                        c.total, c.decided_by_operator, c.measured, c.disagreements
                    ),
                )),
                None => out.push(Disclosure::off(
                    "corpus",
                    "DISCARDED",
                    "decisions are kept in memory and lost when this daemon exits. \
                     Nothing distinguishes this from a session that is keeping them, \
                     which is why it is said out loud. A labelled decision can only \
                     be collected as a side effect of working, so a run without a \
                     store is a run whose evidence cannot be recovered afterwards.",
                )),
            }
        }
        // **The intent diff**, T21.3's error signal.
        if wiring.intent_encoder {
            out.push(Disclosure::on(
                "intent check",
                "at every turn boundary, what the turn SAID it would do is diffed \
                 against what actually ran, and a mismatch is injected as steering \
                 at the next step boundary. Only an `ok` tool result counts as an \
                 effect; an abstention is an attempt.",
            ));
        } else {
            out.push(Disclosure::off(
                "intent check",
                "NO ENCODER",
                "nothing is measuring what this session's turns actually did, so \
                 'I will start X' and 'I started X' are indistinguishable here. \
                 That is `Verification::NoEncoder` — not complete, and the reason \
                 is ours rather than the model's.",
            ));
        }
        // **The monitor wake.** A poll and a wake are different facts, and T24 says
        // so in the entry that left this gap open.
        if wiring.seated.iter().any(|t| t == "monitor") {
            if wiring.monitor_wake {
                out.push(Disclosure::on(
                    "monitor wake",
                    "a monitor that fires WAKES this session between turns; it does \
                     not wait to be asked.",
                ));
            } else {
                out.push(Disclosure::off(
                    "monitor wake",
                    "POLL ONLY",
                    "a fired monitor reaches the model only when something calls \
                     `job_list`. That is a poll, not a wake, and a condition that \
                     fires while nothing is running is a condition nobody acts on.",
                ));
            }
        }
        // **A subagent's downgrade**, said as what is gone: the tools of those
        // classes are not seated, the backend is opened without them, and the
        // ruleset denies them by name. Three readers of one fact, disclosed once.
        if !self.downgrade.is_none() {
            out.push(Disclosure::on(
                "downgrade",
                format!(
                    "{} — below the role's own permissions, inherited from the spawning \
                     session and never widened",
                    self.downgrade.describe()
                ),
            ));
        }
        // **The fabric block.** What the model was told exists on the shelf and
        // in the memories, and where that reading came from.
        match (&self.flowy, &self.fabric) {
            (None, _) => {}
            (Some(_), None) => out.push(Disclosure::off(
                "fabric",
                "OFF",
                "the seat is held but no fabric block was composed for this session — a \
                 subagent, or a session opened before the seat was up.",
            )),
            (Some(_), Some(line)) => out.push(Disclosure::on("fabric", line.clone())),
        }
        match &self.provider {
            None => {}
            Some(p) => out.push(Disclosure::on(
                "provider",
                format!(
                    "turns go to {} ({}), METERED: every token costs money, the cache figures \
                     are the provider's coarse ones, and the structural prefix check does not \
                     run (D10 — a skip is said, never counted as a pass). The token ledger is \
                     kept as the local record with this session's own vocabulary and is never \
                     sent. Cost per turn is reported only for models priced in \
                     ~/.config/letibot/providers.toml; otherwise it is unpriced, not free",
                    p.name,
                    p.model.as_deref().unwrap_or("the preset's default model")
                ),
            )),
        }
        if self.placement == letibot_tools::builtins::task::Placement::Firecode {
            out.push(Disclosure::on(
                "placement",
                "a firecode VM holding a copy of the workspace: the VM is the boundary, the \
                 mode inside is allow-all, and what this session writes lands in a sibling \
                 directory when it ends",
            ));
        }
        // **The room.** Whether anybody said anywhere reaches this session, and as
        // what. `--flowy` is the whole switch; the seat's own line — listening,
        // stalled, stopped — is printed by the daemon from the seat, not from here,
        // because a config cannot know whether a listener is actually attached.
        match (&self.flowy, wiring.seated.iter().any(|t| t == "flowy")) {
            (None, true) => out.push(Disclosure::off(
                "flowy",
                "NO SEAT",
                "the `flowy` tool is seated as a door and nothing is behind it: no room is \
                 heard, nothing is said. `/flowy login` in the head attaches a seat to the \
                 running daemon; --flowy at start does the same.",
            )),
            (None, false) => out.push(Disclosure::off(
                "flowy",
                "OFF",
                "no seat and no `flowy` tool: a subagent, or a role with no spare seat.",
            )),
            (Some(_), true) => out.push(Disclosure::on(
                "flowy",
                "a message for the seat — AND a change on its board — arrives as a \
                 firing of the `flowy` monitor; the two are one stream and one \
                 watcher, so a row assigned to you wakes this session exactly as \
                 something said to you does. The board is sampled beside each inbox \
                 poll and delivered on its EDGES: a line when a row arrives in one of \
                 your buckets and one when a bucket empties, silence while the level \
                 holds. `flowy nag` reads the level itself at any time. The `flowy` \
                 tool sets attention per room, subscribes to rows and threads, and \
                 speaks as the seat.",
            )),
            (Some(_), false) => out.push(Disclosure::off(
                "flowy",
                "NOT SEATED",
                "--flowy was given, but this session has no `flowy` tool: a subagent \
                 (which hears the room through its parent, by design), or a role with \
                 no spare seat (runner). Nothing said on the fabric reaches it directly.",
            )),
        }
        out
    }
}

/// The access classes this session seated that can reach the gate, in prompt order.
///
/// Read off the wiring's booleans, which were themselves read off the seated schemas.
/// `Read` and `Session` are absent because they are unattended — a read-only tool has
/// no code path to a question (clause 4).
fn gated_classes(wiring: &GateWiring) -> Vec<&'static str> {
    let mut out = Vec::new();
    if wiring.has_write_tools {
        out.push("write");
    }
    if wiring.has_exec_tools {
        out.push("exec");
    }
    if wiring.has_network_tools {
        out.push("network");
    }
    out
}

/// What the session actually wired, read from it rather than asserted about it.
///
/// This type exists because of a defect it now makes unwriteable. The adjudication
/// disclosure was a hard-coded sentence — *"M1 is read-only tools, which never
/// prompt (clause 4). There is no boundary and no human in the loop."* It was true
/// when it was written and **nothing checked it**. The moment a session seats a
/// role containing `write` or `edit`, that banner tells the operator there is no
/// boundary while there is one, and that nothing can prompt while everything can.
///
/// A banner whose job is to say what is off, and which says it from memory instead
/// of from the wiring, is worse than no banner: it is trusted. Every field here is
/// read at open time — [`ExecBackend::is_writable`], [`Gate::describe`], and the
/// registry's own schemas — so the sentence cannot drift away from the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateWiring {
    /// How the adjudicator identifies itself, or a string beginning `none`.
    pub adjudicator: String,
    /// Whether the execution backend was opened with a writable constructor.
    pub backend_writable: bool,
    /// Whether any seated tool declares `Access::Write`.
    pub has_write_tools: bool,
    /// What is behind the tools that need infrastructure this box does not run,
    /// read off the seams themselves. The same argument as the three fields above,
    /// applied to four more things that can silently be absent.
    pub external: letibot_tools::ExternalWiring,

    // ---- the seams wired on top, every one of them read rather than asserted ----
    /// The role that was **actually seated**, taken from the resolved registry.
    /// Not the role that was *asked for*: a role that failed to resolve does not
    /// open a session at all, and one that resolved to fewer tools than its name
    /// suggests is a fact an operator should be able to see.
    pub role: String,
    /// Every seated tool name, in prompt order. The evidence behind every boolean
    /// below, so a disclosure can be checked rather than believed.
    pub seated: Vec<String>,
    /// Whether any seated tool declares `Access::Exec`. Read off the schemas, so a
    /// build that seats `bash` under some other name still says so.
    pub has_exec_tools: bool,
    /// Whether any seated tool declares `Access::Network`.
    pub has_network_tools: bool,
    /// How the execution backend describes itself — `HostBackend::describe`, which
    /// says `read-only`, `writable`, `NOT CONFINED` or names the boundary. This
    /// field exists because `describe` used to return `read-only` for every backend
    /// including the writable one: four instances of one defect in one night, and
    /// the fix is that the banner *quotes* the backend rather than paraphrasing it.
    pub backend: String,
    /// Whether a [`letibot_tools::DenialSink`] is attached, so a refusal reaches
    /// the operator at the moment it is decided.
    ///
    /// `docs/boundary-and-adjudication.md` §4b: a session whose denials go nowhere
    /// has the defect the operator named, and it is not a state to be in silently.
    pub denials_surfaced: bool,
    /// Whether an authorisation trail source is installed, so the adjudicator sees
    /// the operator's own words rather than deciding blind. `false` means every
    /// trail is `NotCollected` — *nobody looked*, which is a different fact from an
    /// empty trail and is why `ModelAdjudicator` refuses on one.
    pub trail_installed: bool,
    /// Whether the intent encoder is attached, so `close_the_turn` can diff what
    /// the turn *said* it would do against what ran. `false` makes every completion
    /// `Verification::NoEncoder` — not complete, and the reason is ours.
    pub intent_encoder: bool,
    /// Whether a fired monitor **wakes** the loop rather than waiting to be asked.
    /// `false` means monitors are polled: `job_list` shows a firing, with why, and
    /// nothing acts on it until the model happens to look.
    pub monitor_wake: bool,
    /// Whether the gate's decisions are written somewhere durable, and what is
    /// already there.
    ///
    /// `None` means the corpus is being **discarded**: the rows live in a `Vec` on
    /// the gate and die with this daemon. That was the state for every run this
    /// harness has ever made, and it is not a state to be in silently — the labelled
    /// rows are the expensive ones, they can only be collected as a side effect of
    /// working, and nothing about a session that is dropping them looks different
    /// from one that is keeping them.
    pub corpus: Option<letibot_tokencore::store::CorpusCounts>,
}

impl GateWiring {
    /// The read-only wiring: no adjudicator, no writable backend, no write tools.
    /// Used by callers that have not opened a session yet.
    pub fn read_only() -> GateWiring {
        GateWiring {
            adjudicator: "none (no adjudicator attached)".into(),
            backend_writable: false,
            has_write_tools: false,
            external: letibot_tools::ExternalWiring::none(),
            role: "orchestrator".into(),
            seated: Vec::new(),
            has_exec_tools: false,
            has_network_tools: false,
            backend: "not opened".into(),
            denials_surfaced: false,
            trail_installed: false,
            intent_encoder: false,
            monitor_wake: false,
            corpus: None,
        }
    }
}

/// One thing the operator has to know about this session before they trust an
/// answer from it.
///
/// Structured rather than a sentence because the banner had become a wall: five
/// paragraphs of correct prose, in which the two words that decide whether you can
/// trust the answer — the subject, and whether it is on — were buried mid-line.
/// The prose is not the problem and none of it is cut; what it needed was a shape
/// that can be *scanned*, with the sentence still under it for whoever wants to
/// know why. [`Display`](std::fmt::Display) still renders the original one-line
/// form, which is what a log line and `letibot-m1`'s header want.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disclosure {
    /// `spill`, `store`, `retrieval`, `adjudication`.
    pub subject: String,
    /// The state, in one word. `UNSET`, `MEMORY`, `INERT`, `NONE` — or empty when
    /// the thing is configured and there is nothing alarming to name.
    pub state: String,
    /// What it means, in full. Never abbreviated for the banner.
    pub detail: String,
    /// Whether the subject is doing anything. `false` is the case that has to be
    /// impossible to miss.
    pub active: bool,
}

impl Disclosure {
    /// Public because the daemon has disclosures the config cannot compute — the
    /// count of stored sessions needs the store open — and a second Disclosure
    /// constructor in the binary is a second way for the banner to be shaped.
    pub fn off(subject: &str, state: &str, detail: &str) -> Disclosure {
        Disclosure {
            subject: subject.into(),
            state: state.into(),
            detail: detail.into(),
            active: false,
        }
    }

    fn on(subject: &str, detail: impl Into<String>) -> Disclosure {
        Disclosure {
            subject: subject.into(),
            state: String::new(),
            detail: detail.into(),
            active: true,
        }
    }
}

impl std::fmt::Display for Disclosure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.state.is_empty() {
            write!(f, "{}: {}", self.subject, self.detail)
        } else {
            write!(f, "{}: {} — {}", self.subject, self.state, self.detail)
        }
    }
}

pub fn now_ns() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pane's contract: the two rows that change now come first and name
    /// their verb; every key is unique (a duplicate would be two rows that
    /// disagree); nothing that takes a restart claims a verb.
    #[test]
    fn settings_rows_lead_with_what_changes_now_and_never_repeat_a_key() {
        let cfg = Config::for_this_box("/tmp/x");
        let rows = cfg.settings("project store (modes.tsv)", false);
        assert_eq!(rows[0].key, "mode");
        assert_eq!(rows[0].editable, "/mode NAME");
        assert_eq!(rows[0].source, "project store (modes.tsv)");
        // **The changeable rows lead**, and the assertion is that rather than a
        // position: `model` joined them and shifted `supervise` down, which is the
        // list doing its job and not a regression. Indexing by number made adding
        // a row look like breaking one.
        let leading: Vec<&str> = rows.iter().take(3).map(|r| r.key.as_str()).collect();
        assert!(leading.contains(&"supervise"), "{leading:?}");
        assert!(leading.contains(&"model"), "{leading:?}");
        let sup = rows.iter().find(|r| r.key == "supervise").expect("supervise");
        assert!(sup.value.starts_with("off"));
        // The model row carries its own choices, so a head can draw the picker
        // without keeping a list of its own.
        let model = rows.iter().find(|r| r.key == "model").expect("model");
        assert!(
            model.value.starts_with("local ("),
            "no provider is the local server, and the row still names its alias: {}",
            model.value
        );
        assert!(model.choices.contains(&"local".to_string()), "{:?}", model.choices);
        assert!(
            model.choices.iter().any(|c| c.starts_with("glm/")),
            "{:?}",
            model.choices
        );
        let mut seen = std::collections::HashSet::new();
        for r in &rows {
            assert!(seen.insert(r.key.clone()), "duplicate key {}", r.key);
        }
        assert!(rows.iter().any(|r| r.key == "oracle.budget" && r.editable.is_empty()));
        assert!(rows.iter().any(|r| r.key == "workspace" && r.value == "/tmp/x"));
        // Supervision on reads as on.
        let on = cfg.settings("x", true);
        let sup = on.iter().find(|r| r.key == "supervise").expect("supervise");
        assert!(sup.value.starts_with("on"));
    }


    /// **The round count is a backstop now, not the stop.**
    ///
    /// The defect this replaced: `max_tool_rounds: 12` was the only thing that could
    /// end a runaway turn, so it also ended two working ones. What is asserted is the
    /// *relationship* — a progress check is armed, and the count sits far enough out
    /// that a real investigation of a codebase does not reach it — rather than either
    /// number, because a test pinning 200 would have to be edited to change it and
    /// would then be pinning nothing.
    #[test]
    fn the_stop_is_the_progress_check_and_the_round_count_is_the_backstop() {
        let c = Config::for_this_box("/tmp");
        assert!(c.stall_rounds > 0, "a session with no progress check is the defect");
        assert!(
            c.max_tool_rounds >= 100,
            "a real investigation is dozens of rounds; {} is inside that range and \
             would cut one",
            c.max_tool_rounds
        );
        let d = c
            .disclosures(&GateWiring::read_only())
            .into_iter()
            .find(|d| d.subject == "progress check")
            .expect("which guard is armed is not something an operator should guess");
        assert!(d.active);
        assert!(d.to_string().contains(&c.stall_rounds.to_string()));
    }

    /// Off is reachable and is **said out loud**, because a refusal that can only be
    /// routed around teaches people to route around refusals.
    #[test]
    fn turning_the_progress_check_off_is_a_declared_state() {
        let mut c = Config::for_this_box("/tmp");
        c.stall_rounds = 0;
        let d = c
            .disclosures(&GateWiring::read_only())
            .into_iter()
            .find(|d| d.subject == "progress check")
            .expect("still disclosed when off — especially when off");
        assert!(!d.active);
        let line = d.to_string();
        assert!(line.contains("OFF"), "{line}");
        assert!(
            line.contains("--stall-rounds"),
            "the disclosure carries the way back on: {line}"
        );
    }

    #[test]
    fn the_default_is_no_spill_and_it_says_so() {
        let c = Config::for_this_box("/tmp");
        assert_eq!(c.spill, SpillPolicy::Unset);
        assert!(
            c.disclosures(&GateWiring::read_only())
                .iter()
                .any(|d| d.to_string().contains("spill: UNSET")),
            "a daemon that does not disclose an unset budget is the defect"
        );
    }

    /// The defect this replaced: the adjudication line was a sentence asserting
    /// "M1 is read-only tools, which never prompt", printed regardless of what the
    /// session had seated. Nothing here checks that a *particular* wording is
    /// produced — what it checks is that the wording MOVES when the wiring does.
    /// A banner that says the same thing about a read-only session and a session
    /// with `write` and `edit` in it is not a disclosure.
    #[test]
    fn the_adjudication_line_reads_the_wiring_rather_than_asserting_it() {
        let c = Config::for_this_box("/tmp");
        let line = |w: &GateWiring| {
            c.disclosures(w)
                .into_iter()
                .find(|d| d.subject == "adjudication")
                .expect("adjudication is always disclosed")
        };

        let ro = line(&GateWiring::read_only());
        assert!(!ro.active);
        assert!(
            ro.detail.contains("read-only"),
            "a read-only session should say so: {}",
            ro.detail
        );

        // Write tools seated, nobody to ask. This is the case the old sentence got
        // exactly backwards: it reported no boundary while the gate was refusing
        // every call.
        let unattended = GateWiring {
            backend_writable: true,
            has_write_tools: true,
            ..GateWiring::read_only()
        };
        let u = line(&unattended);
        assert_ne!(
            u.detail, ro.detail,
            "seating write tools must change the disclosure; it did not"
        );
        assert!(
            u.detail.contains("WRITE TOOLS"),
            "an operator must be able to see write tools are present: {}",
            u.detail
        );
        assert!(!u.active, "no adjudicator is not an active boundary");

        // Fully wired: an admitted call reaches the disk, and that is a third
        // distinct thing to say.
        let wired = GateWiring {
            adjudicator: "console adjudicator".into(),
            backend_writable: true,
            has_write_tools: true,
            denials_surfaced: true,
            ..GateWiring::read_only()
        };
        let w = line(&wired);
        assert!(w.active, "an attached adjudicator over a writable backend is on");
        assert_ne!(w.detail, u.detail);

        // **A fourth thing, and it is the one §4b is about.** An adjudicator over a
        // writable backend whose denials go nowhere is not a boundary an operator
        // has: they see a task that stopped and never the decision that stopped it.
        // The disclosure must therefore differ from the fully wired one AND must
        // not read as on.
        let blind = GateWiring {
            denials_surfaced: false,
            ..wired.clone()
        };
        let b = line(&blind);
        assert!(
            !b.active,
            "an adjudicator whose denials reach nobody is not an active boundary"
        );
        assert_ne!(
            b.detail, w.detail,
            "losing the denial sink must change the disclosure; it did not"
        );
    }

    /// **Nothing widens by default**, as a test rather than as a sentence at the
    /// top of the file.
    ///
    /// The whole of this strand is capabilities becoming reachable, and the rule it
    /// is under is that an invocation which passes no flag gets exactly what it got
    /// before. Four fields decide that and all four are checked here, because the
    /// way this regresses is somebody making one of them "sensible" in isolation.
    #[test]
    fn the_default_seat_is_what_shipped_before() {
        let c = Config::for_this_box("/tmp");
        assert_eq!(c.seat, Seat::Orchestrator);
        assert!(!c.allow_bash, "`bash` is off even before a role is chosen");
        assert!(
            !c.intent_prose,
            "the prose half of the intent check has unmeasured false positives"
        );
        assert!(
            !c.seat.needs_writable_backend(),
            "the default seat must get HostBackend::new"
        );
        assert!(!c.seat.needs_exec_backend());
    }

    /// A typo in `--role` names the five rather than falling back to the default.
    ///
    /// Falling back would give an operator who typed `--role codre` a read-only
    /// session that looks like the one they asked for and cannot edit anything —
    /// found out one turn later, which is the cost this whole file is written
    /// against.
    #[test]
    fn an_unknown_role_is_refused_with_the_list() {
        let e = Seat::parse("codre").unwrap_err();
        for known in ["orchestrator", "planner", "researcher", "coder", "runner", "leticode"] {
            assert!(e.contains(known), "{e}");
        }
        assert_eq!(Seat::parse("coder").unwrap(), Seat::Coder);
        assert_eq!(Seat::parse("runner").unwrap(), Seat::Runner);
        assert_eq!(Seat::parse("leticode").unwrap(), Seat::Leticode);
        assert_eq!(Seat::parse("opencode").unwrap(), Seat::Leticode);
    }

    /// The model adjudicator **parses, and refuses by name without an oracle.**
    ///
    /// It used to fail at parse time with a message about T25/D13, because there was
    /// no oracle in the build at all. There is one now (`crate::oracle::HttpOracle`),
    /// so the choice is legal and the missing piece moved one layer down: an
    /// endpoint. The refusal it produces there names `--oracle` — see
    /// `Harness::open_with_registry`. Rejecting it here again would say this build
    /// has no model adjudicator, which is no longer true.
    #[test]
    fn the_model_adjudicator_parses_now_that_an_oracle_exists() {
        assert_eq!(
            AdjudicatorChoice::parse("model").expect("model is a real choice"),
            AdjudicatorChoice::Model
        );
    }

    /// Every seam this strand wired is disclosed, and each one **moves** with the
    /// wiring rather than being a sentence.
    ///
    /// The defect this guards is the one already paid for four times in one night:
    /// a banner that said "M1 is read-only tools" about sessions with write tools,
    /// a `describe` that said `read-only` for every backend including the writable
    /// one, and a constant `writable + UNSANDBOXED EXEC`. Nothing here asserts a
    /// particular wording. What it asserts is that the wording is different when
    /// the wiring is.
    #[test]
    fn every_new_seam_is_disclosed_and_the_line_moves_with_it() {
        let c = Config::for_this_box("/tmp");
        let subjects = |w: &GateWiring| -> Vec<String> {
            c.disclosures(w).into_iter().map(|d| d.subject).collect()
        };
        let detail = |w: &GateWiring, s: &str| -> String {
            c.disclosures(w)
                .into_iter()
                .find(|d| d.subject == s)
                .map(|d| d.detail)
                .unwrap_or_default()
        };

        let ro = GateWiring::read_only();
        for expected in ["role", "backend", "adjudication", "intent check"] {
            assert!(
                subjects(&ro).iter().any(|s| s == expected),
                "{expected} is not disclosed: {:?}",
                subjects(&ro)
            );
        }
        // A read-only seat has nothing to authorise, so the trail line is absent
        // rather than saying "not collected" about a session where it could not
        // matter. Absence with a reason, not silence.
        assert!(!subjects(&ro).iter().any(|s| s == "auth trail"));

        let gated = GateWiring {
            has_write_tools: true,
            backend_writable: true,
            role: "coder".into(),
            seated: vec!["read".into(), "write".into(), "edit".into()],
            backend: "the host filesystem under /tmp, writable".into(),
            ..GateWiring::read_only()
        };
        assert!(subjects(&gated).iter().any(|s| s == "auth trail"));
        assert!(
            detail(&gated, "auth trail").contains("NOT"),
            "an uninstalled trail must not read as installed"
        );
        let with_trail = GateWiring {
            trail_installed: true,
            ..gated.clone()
        };
        assert_ne!(
            detail(&with_trail, "auth trail"),
            detail(&gated, "auth trail")
        );

        // The backend line quotes the backend. Two different backends must not
        // produce one sentence.
        let other = GateWiring {
            backend: "a project-scoped namespace over /tmp".into(),
            ..gated.clone()
        };
        assert_ne!(detail(&other, "backend"), detail(&gated, "backend"));

        // The role line names what was seated, so a role that resolved to fewer
        // tools than its name suggests is visible.
        assert!(detail(&gated, "role").contains("write"));
        assert!(detail(&gated, "role").contains("coder"));

        // The monitor line exists only where a monitor could be declared, and it
        // distinguishes a wake from a poll.
        assert!(!subjects(&gated).iter().any(|s| s == "monitor wake"));
        let watching = GateWiring {
            seated: vec!["monitor".into()],
            has_exec_tools: true,
            ..gated.clone()
        };
        assert!(detail(&watching, "monitor wake").contains("poll"));
        let woken = GateWiring {
            monitor_wake: true,
            ..watching.clone()
        };
        assert_ne!(
            detail(&woken, "monitor wake"),
            detail(&watching, "monitor wake")
        );
    }

    #[test]
    fn every_thing_that_is_off_is_disclosed() {
        let c = Config::for_this_box("/tmp");
        let all = c
            .disclosures(&GateWiring::read_only())
            .iter()
            .map(|d| d.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        for expected in ["spill", "store", "retrieval", "adjudication"] {
            assert!(all.contains(expected), "{expected} is not disclosed:\n{all}");
        }
    }

    #[test]
    fn the_system_prompt_carries_nothing_volatile() {
        // §5.2 rule 1, as a test rather than as a habit.
        for volatile in ["/home/", "20", "branch", "commit"] {
            assert!(
                !DEFAULT_SYSTEM.contains(volatile),
                "the stable prefix contains {volatile:?}, which will change and re-prefill"
            );
        }
        assert!(DEFAULT_SYSTEM.contains("Answer in English"), "§5.2: state the language");
    }
}

#[cfg(test)]
mod compaction_trigger_tests {
    use super::*;

    /// The reserve must leave a usable window at every size. The first version
    /// did not: `(w/16).max(8192)` exceeds any window of 8192 or less, which makes
    /// `should_compact` true before a conversation has said anything.
    #[test]
    fn the_headroom_never_swallows_the_window_it_reserves_in() {
        let mut cfg = Config::for_this_box(std::env::temp_dir());
        for w in [262144u64, 131072, 32768, 16000, 8192, 4096, 2048, 1024] {
            cfg.context_window = Some(w);
            let h = cfg.headroom();
            assert!(h < w, "window {w}: headroom {h} is not smaller than the window");
            assert!(h <= w / 4, "window {w}: headroom {h} is more than a quarter");
            // An empty conversation must never be at the wall.
            assert!(!cfg.should_compact(0), "window {w} compacts an empty session");
            // And a full one must be.
            assert!(cfg.should_compact(w), "window {w} never compacts");
        }
    }

    /// Not knowing the window is not a reason to act. A metered provider has no
    /// `/props`, and inventing a number would compact conversations that had room.
    #[test]
    fn an_unknown_window_never_triggers() {
        let mut cfg = Config::for_this_box(std::env::temp_dir());
        cfg.context_window = None;
        assert!(!cfg.should_compact(u64::MAX));
        // And the switch is honoured when the window IS known.
        cfg.context_window = Some(1000);
        cfg.auto_compact = false;
        assert!(!cfg.should_compact(999_999));
    }

    /// **The wall-continuation gate measures the window, not the flag.**
    ///
    /// `should_compact` folds in `auto_compact`, and the no-progress guard turns
    /// that flag off WITHOUT freeing anything — so a gate that read the flag would
    /// see room that is not there and continue straight into a second wall. The
    /// edges: the headroom boundary is exclusive (a turn that would end exactly at
    /// the wall does not start), and an unknown window is no room, for the same
    /// reason `should_compact` refuses.
    #[test]
    fn the_wall_continuation_gate_measures_the_window_not_the_flag() {
        let mut cfg = Config::for_this_box(std::env::temp_dir());
        cfg.context_window = Some(10_000);
        let h = cfg.headroom();
        assert!(h > 0 && h < 10_000, "{h}");
        assert!(
            !cfg.room_for_next_turn(10_000 - h),
            "resident + headroom == window is the wall, not room"
        );
        assert!(
            cfg.room_for_next_turn(10_000 - h - 1),
            "one token under the wall is room"
        );
        // The flag the no-progress guard flips changes nothing here: this is a
        // measurement, and the guard does not free tokens when it flips it.
        cfg.auto_compact = false;
        assert!(cfg.room_for_next_turn(10_000 - h - 1));
        // Unknown window: no room, the same refusal should_compact gives.
        cfg.context_window = None;
        assert!(!cfg.room_for_next_turn(0));
    }
}
