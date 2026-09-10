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
}

impl Seat {
    pub fn as_str(&self) -> &'static str {
        match self {
            Seat::Orchestrator => "orchestrator",
            Seat::Planner => "planner",
            Seat::Researcher => "researcher",
            Seat::Coder => "coder",
            Seat::Runner => "runner",
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
            other => Err(format!(
                "unknown role `{other}`; this build has orchestrator, planner, \
                 researcher, coder, runner"
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
        matches!(self, Seat::Planner | Seat::Coder | Seat::Runner)
    }

    /// Whether this seat needs a backend that can start processes, and therefore
    /// the boundary that goes around one.
    pub fn needs_exec_backend(self) -> bool {
        matches!(self, Seat::Runner)
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub dialect: Dialect,
    /// The alias the server reports. Recorded in `turn_metrics` and in the store;
    /// several models may share one dialect.
    pub model: String,
    pub endpoint: Endpoint,
    /// The GGUF the vocabulary is read from. For a split model, the first shard.
    pub vocab_gguf: PathBuf,
    /// The root every read-only tool is confined to.
    pub workspace: PathBuf,
    pub socket: PathBuf,
    /// `None` keeps the transcript in memory only — usable, and honest about it.
    pub store: Option<PathBuf>,
    pub session_id: String,
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
    /// How many times one user turn may go round the tool loop before the daemon
    /// stops and says so. Not a token cap — §5.7 removed those — a *loop* bound, so
    /// a model that calls `read` on the same file forever is a reported failure
    /// rather than a session that never returns.
    pub max_tool_rounds: usize,
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
    /// [`letibot_tools::ConsoleAdjudicator`] over the daemon's own stdin/stderr.
    ///
    /// **The default for any seat that can reach the gate.** Its honest limit is
    /// where it reads from: this works for `harnessd --prompt …` and for a daemon
    /// run in a foreground terminal, and a head attached over the socket has no way
    /// to answer it — the head's answer affordance is T25/D10 and is not built. The
    /// startup disclosure says so rather than letting somebody discover it by
    /// watching a daemon block on a closed stdin.
    #[default]
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
            "model" => Err(
                "the model adjudicator is not wired: `ModelAdjudicator` takes an \
                 `AuthorisationOracle` and this build has no oracle behind it, and its \
                 always-ask list is unreviewed (TODO.md T25/D13). It would refuse every \
                 call on an uncollected trail, which is `console` with extra steps"
                    .into(),
            ),
            other => Err(format!(
                "unknown adjudicator `{other}`; this build has console"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            AdjudicatorChoice::Console => "console",
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
pub const DEFAULT_SYSTEM: &str = "You are a careful software engineering assistant working in a \
checked-out source tree.\n\n\
Answer in English unless the user writes in another language, in which case answer in theirs.\n\n\
You have read-only tools. Use them for questions about **this tree** — its files, their contents, \
where something is defined — rather than guessing: a file you have not read is a file you do not \
know. Do not call a tool for a question about the world, about a definition, or about arithmetic; \
answer those directly. When a tool reports that it found nothing, say so — do not fill the gap \
from memory.\n\n\
Be direct. Prefer the shortest answer that is complete.";

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
            allow_bash: false,
            adjudicator: AdjudicatorChoice::default(),
            intent_prose: false,
            spill: SpillPolicy::Unset,
            spill_storage: SpillStorage::Memory,
            max_tool_rounds: 12,
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
        if self.store.is_none() {
            out.push(Disclosure::off(
                "store",
                "MEMORY",
                "the transcript is not persisted; the session ends with the process. \
                 Pass --store PATH.",
            ));
        }
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
            letibot_tools::adjudicate::startup_disclosure_with_surfacing(
                &wiring.adjudicator,
                wiring.backend_writable,
                // The gate is reachable from any class that is not unattended, not
                // from `write` alone. A seat with `say` and no writes still has a
                // code path to a question, and a disclosure that only counted
                // writes would call that session unattended.
                wiring.has_write_tools || wiring.has_exec_tools || wiring.has_network_tools,
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
        out
    }
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
        for known in ["orchestrator", "planner", "researcher", "coder", "runner"] {
            assert!(e.contains(known), "{e}");
        }
        assert_eq!(Seat::parse("coder").unwrap(), Seat::Coder);
        assert_eq!(Seat::parse("runner").unwrap(), Seat::Runner);
    }

    /// The model adjudicator is **named as unwired**, not silently missing.
    ///
    /// `--adjudicator model` is the thing somebody reaches for after reading
    /// `docs/boundary-and-adjudication.md`, and "unknown adjudicator" would read as
    /// this build not having the concept. It has the concept and no oracle behind
    /// it (T25/D13), which is a different fact and the one worth saying.
    #[test]
    fn the_model_adjudicator_says_why_it_is_not_here() {
        let e = AdjudicatorChoice::parse("model").unwrap_err();
        assert!(e.contains("oracle"), "{e}");
        assert!(e.contains("D13"), "{e}");
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
