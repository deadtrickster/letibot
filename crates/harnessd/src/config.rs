//! What a daemon needs to know before it can open a session.
//!
//! Every field is a fact the daemon cannot invent. There is no `Default` for the
//! whole struct on purpose: a default endpoint, a default model and a default
//! workspace root together describe a session against somebody else's box, and the
//! failure would be a running daemon rather than an error.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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
    /// **The merge queue's reviewer** ([`letibot_tools::runtime::roles::gatekeeper`]): `read`,
    /// `grep`, `glob`, `read_spill`, and — behind [`Config::allow_bash`], like the runner's —
    /// `bash` for `git diff` and `git log`. Nothing that can author code, and never a seat a
    /// person picks: the daemon starts it as a hidden child of the session whose branch it
    /// judges (`mergequeue::GatekeeperDoor`).
    Gatekeeper,
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
            Seat::Gatekeeper => "gatekeeper",
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
            "gatekeeper" => Ok(Seat::Gatekeeper),
            other => Err(format!(
                "unknown role `{other}`; this build has orchestrator, planner, \
                 researcher, coder, runner, leticode, gatekeeper"
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
        matches!(
            self,
            Seat::Planner | Seat::Coder | Seat::Runner | Seat::Leticode
        )
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
        matches!(self, Seat::Runner | Seat::Coder | Seat::Gatekeeper)
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

impl ProviderConfig {
    /// **This provider's context window, from the catalogue.**
    ///
    /// The same models.dev number `Harness::retune_window` moves to when the
    /// operator runs `/models`, so a session that STARTS on a provider plans
    /// against the same wall as one that switches to it. Before this they did
    /// not: the start path skipped the lookup entirely and left the window
    /// `None`, and `None` is not a large window — it is no wall at all, because
    /// every check that would compact is written `let Some(window) = …`.
    ///
    /// `None` when the preset or the model is not in the catalogue, which is the
    /// honest answer and leaves the old behaviour exactly where it was for a
    /// model nobody has a number for.
    pub fn catalogue_window(&self, cat: &letibot_provider::catalogue::Catalogue) -> Option<u64> {
        letibot_provider::Preset::parse(&self.name)
            .ok()?
            .window(self.model.as_deref(), cat)
    }
}

/// **Where the model that answers came from**, as the startup disclosure says it.
///
/// Four levels, one word each — `flag`, `project`, `user`, `built-in` — plus the
/// file when a file is where it came from and any fault found while working it
/// out. The operator's own evening is the reason this is a value on the config
/// rather than a sentence composed where the model is set: a disclosure that can
/// only be produced by the code path that chose the model is a disclosure that
/// goes missing exactly when the choice was surprising.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ModelSource {
    /// The level that won. [`crate::leticode_config::Level::Builtin`] is the
    /// default, which is the honest answer for a daemon nothing spoke for.
    pub level: crate::leticode_config::Level,
    /// The name that level wrote, when a level wrote one. `None` for the built-in
    /// default, which is not a name somebody typed — it is the model the daemon
    /// was launched on.
    pub value: Option<String>,
    /// The file, when a file is where it came from.
    pub file: Option<PathBuf>,
    /// What the level was, in a phrase: `--provider deepseek`, `[default] in
    /// providers.toml`, `the local server this daemon was launched against`. The
    /// short form of the same fact the level names, for the sentence.
    pub origin: String,
    /// **A fault found at startup**, when there is one: a value that names a
    /// model this box cannot reach, or a file's alias that the command line's
    /// binding outranks. `None` is the quiet case. Carried here so the banner says
    /// it — the whole point is that it is read before the first turn rather than
    /// discovered as a 400.
    pub note: Option<String>,
}

impl ModelSource {
    /// The level and where it was, as one clause: `the user level
    /// (~/.config/letibot/leticode.toml)`.
    pub fn describe(&self) -> String {
        let level = self.level.describe();
        match (&self.file, self.origin.is_empty()) {
            (Some(p), _) => format!("{level} ({})", p.display()),
            (None, true) => level.to_string(),
            (None, false) => format!("{level} — {}", self.origin),
        }
    }

    /// **The precedence, in the words a reader gets**, so the rule is on the
    /// screen next to the value it decided rather than in a file beside it.
    pub const ORDER: &'static str = "the command line beats the project file, the project file \
         beats the user file, the user file beats the built-in default";

    /// The whole sentence for the `session model` disclosure.
    pub fn render(&self, model: &str) -> String {
        let mut out = format!("main {model} — from {}. ", self.describe());
        out.push_str(&format!(
            "The order is: {}. The built-in default is `[default]` in \
             ~/.config/letibot/providers.toml (what `/models NAME` writes), else the local \
             server this daemon was launched against.",
            Self::ORDER
        ));
        if let Some(note) = &self.note {
            out.push(' ');
            out.push_str(note);
        }
        out
    }
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
    /// **The token an image goes where it sits**, read from `/props` at startup
    /// (`serving::served_media_marker`), and `None` when the endpoint says nothing —
    /// every metered provider, and any local server without `mtmd`.
    ///
    /// **Fetched rather than known, because the server randomises it.** MEASURED 2026-09-27: the live
    /// value is `<__media_d2QxA7RJNGYqiEAoAPVLPCHm6CPADiRe__>`, it changes with the process unless
    /// `LLAMA_MEDIA_MARKER` pins it, and the model's OWN vision tokens
    /// (`<|vision_start|><|image_pad|><|vision_end|>`) are answered `Failed to tokenize prompt`. So
    /// this is a fact about the running server, like `n_ctx` and `total_slots`, and `None` here means
    /// **no images on this endpoint** — disclosed, never guessed at.
    pub media_marker: Option<String>,
    /// Compact automatically when a turn leaves less than [`Config::headroom`]
    /// of the window free. On by default where the window is known.
    ///
    /// `docs/compaction.md` §1: the trigger is `n_ctx` and memory pressure, and
    /// NOT quality — depth was measured not to hurt (1.000 at 60k against 0.829
    /// at zero). So the policy is *compact when you must, as late as possible*,
    /// and this is the "must".
    ///
    /// **It gates the PRE-EMPTIVE question only** — *would the next turn fit, so
    /// should something be tidied first?* ([`Config::should_compact`]). The other
    /// question, *a turn was just refused for context length, is there a way
    /// forward?*, is answered through [`Config::at_the_wall`] with this flag
    /// deliberately absent: the no-progress guard turns this flag off without
    /// freeing anything, and a refusal that met a stood-down compaction would be
    /// the wedge of 2026-10-09 (session `s-1789919514688401228`: one compaction,
    /// then thirty provider 400s in a row, none of them recovered) all over
    /// again. See `Sessions::compact_because_refused`.
    pub auto_compact: bool,
    /// **The pair that stood automatic compaction down** — `(resident, after)`,
    /// both in LEDGER tokens: the conversation's size going into the compaction
    /// that made no room, and its size coming out still within a headroom of the
    /// window. `Some` ⟺ the no-progress guard has fired for this session.
    ///
    /// Carried on the config because the config is the per-session object that
    /// survives being handed around (the store row is written from it at
    /// [`crate::harness::Harness::stand_down_auto_compact`] and read back at
    /// [`crate::harness::Harness::restore_auto_compact`]), so a daemon that comes
    /// back after a restart holds the same fact the row does — measured on
    /// 2026-10-09: the guard's old `auto_compact = false` lived in memory only, so
    /// the restart was the one thing that cleared it, silently, in both directions.
    /// Nothing clears it but a fresh session or a raised window: a manual
    /// `/compact` does not re-arm the automatic one, and never has.
    pub auto_compact_stood_down: Option<(u64, u64)>,
    /// The GGUF the vocabulary is read from. For a split model, the first shard.
    ///
    /// `None` is a daemon for a cloud provider: its ledger uses the byte vocabulary
    /// (`letibot_tokencore::Vocab::bytes`), and a local model cannot be selected on it.
    pub vocab_gguf: Option<PathBuf>,
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
    /// **How deep in a subagent tree this session sits.** Zero for a root, one for its
    /// child, and so on — a config fact rather than a seat fact, because the seat table
    /// (*"does this role name `task`"*) can express exactly two depths, zero and
    /// unlimited, and the ruling wants a *configured* number in between.
    ///
    /// Incremented once, at spawn (`harness.rs`'s `sub_cfg`), and read where `task` is
    /// seated and where a call past [`Config::max_subagent_depth`] is refused by name.
    pub depth: u32,
    /// Whether this session's backend is rooted at `/` — opencode parity: `read`
    /// reaches the whole host and the permission ruleset, not a jail, is the gate.
    ///
    /// Set for [`Seat::Leticode`], and **inherited by its subagents**: a subagent
    /// re-seats to [`Seat::Coder`] for its tools but must not be re-confined to a
    /// project its parent already left. A subagent of a coder session stays confined,
    /// because its parent is.
    /// **What the last metered turn cost in each side's tokens**, as
    /// `(ledger, provider)`.
    ///
    /// The ledger counts this conversation as a LOCAL rendered prompt, in the
    /// session's own vocabulary and with its reasoning blocks in it. A messages
    /// provider is sent neither: `letibot_provider::messages` drops `Reasoning`
    /// on purpose, because DeepSeek documents that `reasoning_content` must not
    /// be sent back. So the two numbers measure different things, and every
    /// compaction decision was comparing one against the other.
    ///
    /// Measured in the operator's own store, 2026-09-20:
    ///
    /// | transcript | ledger    | reasoning       | ledger − reasoning | provider  |
    /// |------------|-----------|-----------------|--------------------|-----------|
    /// | letibot    | 1,072,176 | 363,111 (33.9%) | 709,065            | ~607,000  |
    /// | leticl     | 1,495,702 | 400,788 (26.8%) | 1,094,914          | 1,048,607 |
    ///
    /// Take the reasoning out and the counts agree to 4-17%, which is ordinary
    /// variance between two tokenizers; leave it in and the ledger runs 1.4-1.8x
    /// high, by a factor that is not a constant — it is however much the model
    /// thought. So the session that thinks more compacts earlier, which is the
    /// opposite of what anybody wants. `letibot` compacted with 39% of its
    /// window free.
    ///
    /// `None` until a metered turn has reported, and on a local session, where
    /// the ledger IS the prompt and the two are the same number by construction.
    /// **Where an HTTP head listens**, or `None` for the ordinary socket-only
    /// daemon. `--http HOST:PORT`. See [`crate::httphead`].
    pub http: Option<String>,
    pub ledger_scale: Option<(u64, u64)>,
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
    ///
    /// Built as [`DEFAULT_SYSTEM`] in [`Config::for_this_box`] and **composed once,
    /// at session open**, from [`Prompts`] (the operator's `prompts.toml`), this
    /// session's model, and — through [`Config::compose_system_with_notes`] — the
    /// standing-notes files on disk. After that it is message 0 and is never
    /// rewritten **mid-session on a local model**: a session that started under
    /// one prompt keeps it until a base rebuild. The standing-notes section is
    /// the one deliberate exception to "keeps it for its whole life": every fork
    /// re-reads the files (`Harness::reseat_target`), because a compaction is
    /// already a cold prefill and moving the head there costs nothing extra.
    /// `prompts.toml` itself stays a fact about NEW sessions — it is parsed once
    /// and never re-read.
    pub system: String,
    /// The operator's `prompts.toml`, loaded once at startup. `Default` (no
    /// overrides) when the file is absent or was refused — which is also the
    /// byte-identical case. See [`Prompts`].
    pub prompts: Prompts,
    /// The project's `leticode.toml`, discovered by walking up from the workspace
    /// and loaded once at startup — **with the user file already merged under
    /// it**, so this is the two file levels as the session reads them. `Default`
    /// (no models) when neither file is there or both were refused — which is
    /// also the byte-identical case: a daemon with no file runs on its own
    /// models and discloses nothing from a file. See
    /// [`crate::leticode_config::LeticodeConfig`].
    ///
    /// **Models are configuration; permissions are not.** This field carries model
    /// names and nothing else — no seat, no tool list, no access narrowing — so a
    /// file cannot widen a capability. The precedence that turns these into the
    /// session's models is stated once, in
    /// [`crate::leticode_config::precedence`]: command line beats the project
    /// file, the project file beats the user file, the user file beats the
    /// built-in default, and an unset key falls through.
    pub leticode: crate::leticode_config::LeticodeConfig,
    /// **The user file alone**, `~/.config/letibot/leticode.toml`, as its own
    /// value — because [`Config::leticode`] is the merge and a disclosure that
    /// named only the merged value could not say which of the two files set a
    /// key. This one is for the screen; the session reads the merge.
    pub leticode_user: crate::leticode_config::LeticodeConfig,
    /// **Where the model that answers came from.** The level, the file when a
    /// file is where it came from, and any fault found while working it out —
    /// because *"why is my session on deepseek"* was a whole evening, and an
    /// answer that lives in a file the operator has to go and read is not an
    /// answer. See [`ModelSource`].
    pub model_source: ModelSource,
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
    /// **How deep a subagent tree may go.** The operator's ruling, 2026-10-03:
    /// *"subagents are absolutely allowed to spawn subagents up to configured nesting
    /// level."* Default 3.
    ///
    /// It rides the config rather than the seat table for the reason above `depth`: a
    /// seat that names `task` says *unlimited*, and one that does not says *never*, and
    /// neither can carry a number. `task` is therefore **listed on the seat and the
    /// limit enforced at the call**, refused by name rather than by the tool's absence —
    /// a capability that exists but is hidden manufactures the workaround (see
    /// `m2_coder`'s own note on `todo`).
    pub max_subagent_depth: u32,
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
    /// **How many output tokens the guard may spend on its answer** (R12).
    ///
    /// `None` is [`crate::oracle::DEFAULT_MAX_TOKENS`], which is the same value this
    /// shipped with. It exists because of the one case where the answer is a *budget* and
    /// not a person: a reply cut off at the ceiling before it reached a verdict is
    /// `UnsureKind::OutOfRoom`, it says so on the card and on the corpus row, and the
    /// thing to do about it is to raise this. Without the knob the sentence would be
    /// telling the operator to turn a wheel that is welded on.
    ///
    /// Raising it costs latency on every gated call that uses the words, so it is typed
    /// rather than inferred — `--oracle-budget-ms`'s rule, one knob along.
    pub oracle_max_tokens: Option<usize>,
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
    ///
    /// **`0` is unbounded, and it is the default.** The operator: *"make 200 tool
    /// calls limit configurable and set it to infinity"*. The argument for it is
    /// the one this field's own history already makes — a round count measures
    /// effort and cannot tell a model that is working from one that is stuck, so
    /// as a stop it was only ever wrong in one direction, and 200 is a number
    /// nobody derived. `stall_rounds` is the guard that reads progress, and it is
    /// the one that should be doing this job.
    ///
    /// What follows, and is disclosed rather than hidden: with this off AND
    /// `stall_rounds` off, **nothing ends a turn but the model** — see
    /// [`Config::disclosures`], which says exactly that when both are zero. One
    /// off is a choice; both off is a different setting and reads as one.
    pub max_tool_rounds: usize,
    /// **How many times a round is RE-attempted when the model endpoint fails**,
    /// after the first try. `0` is none. See `harness::MAX_HTTP_RETRIES`, which is
    /// the default: six, doubling from a second, about a minute of waiting.
    ///
    /// Retries and not attempts, because the first version of this field was
    /// called `http_attempts` and `1` still retried once — the guard reads
    /// `attempt >= budget` with `attempt` starting at zero. A field whose name
    /// says one thing and whose arithmetic says another is a bug waiting for
    /// somebody to set it to what the name promises; its own test caught it.
    ///
    /// Configurable because the right answer depends on something the daemon
    /// cannot see — whether the endpoint is a server that might be reloading or
    /// one that is simply not there. A caller that already knows sets `1` and is
    /// told at once. Measured, 2026-09-20: `compact.rs` spent 63.7 seconds of wall
    /// clock on 3.5 seconds of CPU, all of it one test waiting out this ladder
    /// against a port its own comment called "a dead port, so the attempt fails
    /// fast".
    pub http_retries: u32,
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
Find with `grep` and read with `read`, not by piping `grep -n` into `sed`. `grep` takes a \
`context` count and returns the region around each match, numbered — that is the find and the \
look in one call. `read` takes a `ranges` list for several windows of one file at once. A `sed` \
slice has no line numbers, does not say where the file continues, and prints nothing for a range \
that is wrong, which reads exactly like a range that was right.\n\n\
Change files with `edit` and `write`, never by piping a script into a shell. `edit` takes an \
`edits` list: several changes to one file, applied in order and all-or-nothing, each one either \
`old_string`+`new_string` or `insert_before`/`insert_after`+`new_string`. That is what a heredoc \
was for, and it is one call instead of four. A shell that rewrites a file also produces no diff \
for the operator to read and no record of what changed.\n\n\
You have a scratch directory of your own — the `scratch` row of `harness status` names the \
exact path. Put working files there: a generated script, a downloaded page, intermediate \
output, anything you need on disk that the operator did not ask for. It is outside their tree, \
so nothing you leave in it touches their work, and creating, writing and deleting inside it \
need no permission.\n\n\
Use that path and no other. A directory you invent under /tmp is shared temp space: deleting \
there is a decision somebody has to make, and the name may already be another process's. Do \
not scatter temporary files through the workspace either.\n\n\
You can keep standing notes: markdown files the harness reads into this prompt and re-reads \
after every compaction, so a note survives a context you cannot. Write one with the `notes` \
tool when you find something worth keeping — interesting, remarkable or surprising, \
something you would not want to rediscover — not as a summary of what you did. The tool \
writes the workspace's `.letibot/notes/` only; `AGENTS.md` and the box-wide notes are the \
operator's, and you read them rather than edit them. A note says what was true or intended \
when it was written: verify against the tree before treating one as a fact about now, and \
replace a note that has gone stale rather than repeat it.\n\n\
Be direct. Prefer the shortest answer that is complete.";

/// **The named sections of [`DEFAULT_SYSTEM`], in the order they appear.**
///
/// Joined by `\n\n` they are [`DEFAULT_SYSTEM`] byte for byte — a test asserts it.
/// That is what makes the split owned rather than invented: an edit to a paragraph
/// that forgets the table fails the join test, and so does a table whose boundaries
/// moved. The names are the keys the operator uses in `prompts.toml` to replace a
/// section in place, leaving the other sections and their order alone.
pub const SYSTEM_SECTIONS: &[(&str, &str)] = &[
    (
        "identity",
        "You are a careful software engineering assistant working in a checked-out source tree.",
    ),
    (
        "language",
        "Answer in English unless the user writes in another language, in which case answer in theirs.",
    ),
    (
        "read_only_tools",
        "You have read-only tools. Use them for questions about **this tree** — its files, their contents, where something is defined — rather than guessing: a file you have not read is a file you do not know. The same holds for **this conversation**: if the user refers to something as already settled and you have no record of it, read it with `transcript` before asking them to repeat it — a compaction replaces earlier turns with a summary, and the turns themselves are still in the store. Do not call a tool for a question about the world, about a definition, or about arithmetic; answer those directly. When a tool reports that it found nothing, say so — do not fill the gap from memory.",
    ),
    (
        "find_and_read",
        "Find with `grep` and read with `read`, not by piping `grep -n` into `sed`. `grep` takes a `context` count and returns the region around each match, numbered — that is the find and the look in one call. `read` takes a `ranges` list for several windows of one file at once. A `sed` slice has no line numbers, does not say where the file continues, and prints nothing for a range that is wrong, which reads exactly like a range that was right.",
    ),
    (
        "edit_files",
        "Change files with `edit` and `write`, never by piping a script into a shell. `edit` takes an `edits` list: several changes to one file, applied in order and all-or-nothing, each one either `old_string`+`new_string` or `insert_before`/`insert_after`+`new_string`. That is what a heredoc was for, and it is one call instead of four. A shell that rewrites a file also produces no diff for the operator to read and no record of what changed.",
    ),
    (
        "scratch",
        "You have a scratch directory of your own — the `scratch` row of `harness status` names the exact path. Put working files there: a generated script, a downloaded page, intermediate output, anything you need on disk that the operator did not ask for. It is outside their tree, so nothing you leave in it touches their work, and creating, writing and deleting inside it need no permission.",
    ),
    (
        "scratch_path",
        "Use that path and no other. A directory you invent under /tmp is shared temp space: deleting there is a decision somebody has to make, and the name may already be another process's. Do not scatter temporary files through the workspace either.",
    ),
    (
        "notes",
        "You can keep standing notes: markdown files the harness reads into this prompt and re-reads after every compaction, so a note survives a context you cannot. Write one with the `notes` tool when you find something worth keeping — interesting, remarkable or surprising, something you would not want to rediscover — not as a summary of what you did. The tool writes the workspace's `.letibot/notes/` only; `AGENTS.md` and the box-wide notes are the operator's, and you read them rather than edit them. A note says what was true or intended when it was written: verify against the tree before treating one as a fact about now, and replace a note that has gone stale rather than repeat it.",
    ),
    (
        "tone",
        "Be direct. Prefer the shortest answer that is complete.",
    ),
];

/// **The per-model system-prompt overrides, from `prompts.toml`.**
///
/// A file the operator edits, beside `providers.toml` in the same config dir. It
/// answers a question `providers.toml` must not: what the model is TOLD, as opposed
/// to how it is sampled and where its key lives.
///
/// # Why a separate file, and a real parser
///
/// `providers.toml` is mode 600 because it holds provider keys, and
/// `/models NAME --key K` WRITES it. Prompt text is not a secret, and hand-written
/// multi-line prompt text living in a file a program rewrites by hand-rolled
/// parsing is exactly the mangled-prompt defect this avoids. So: one file for keys,
/// one for prompts, and a real TOML parser (`toml`) for the one file that carries
/// arbitrary prompt bytes. The tree hand-parses the flat `key = "value"` subset
/// elsewhere; that is a different class of content and stays that way.
///
/// # The keys, and why they are these
///
/// The keys inside a layer are the **section names** of [`SYSTEM_SECTIONS`] —
/// `identity`, `language`, `read_only_tools`, `find_and_read`, `edit_files`,
/// `scratch`, `scratch_path`, `notes`, `tone`. A section key replaces that one section in
/// place, leaving the other sections and their order alone. That is the whole
/// feature: the operator can override one section for one model without rewriting
/// the prompt.
///
/// * `[base] <section> = "…"` — a change for every model. The section is replaced
///   for every session, and a model's own section still overrides it.
/// * `[model."NAME"] <section> = "…"` — the same section, replaced for one model.
///   `NAME` is the model as the daemon names it: `provider/model` for a metered
///   session, the bare alias for a local one. A `*` in the provider position
///   (`deepseek/*`) is a glob over that provider's models.
/// * `<layer> system_extra = "…"` — text appended to the composed prompt, after
///   every section. It may be set in either layer: in `[base]` it is appended for
///   every model, and a model's own `system_extra` is appended after it. It ADDS,
///   it does not replace.
///
/// There is no `[base] system` any more. The wholesale replacement it was is the
/// shape that could not override one section for one model, and the sections are
/// the unit now: a prompt the operator wants changed in one place is changed in one
/// place, and the rest of [`DEFAULT_SYSTEM`] — and its order — is left alone. A
/// section name this daemon does not know is refused by name, with the file's path,
/// and the session runs on the un-overridden composition.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Prompts {
    /// The `[base]` section overrides, keyed by section name. Empty overrides are
    /// dropped on load: an empty string would replace a section with nothing, and
    /// keeping it would let an empty exact block shadow the provider glob it should
    /// fall through to.
    base: BTreeMap<String, String>,
    /// The `[base] system_extra`, appended after the sections for every model.
    /// `None` when unset.
    base_extra: Option<String>,
    /// Per-model overrides, keyed by model name. A model with neither a section
    /// override nor a `system_extra` is dropped on load, for the same shadowing
    /// reason.
    models: BTreeMap<String, ModelOverrides>,
}

/// One model's overrides: the sections it replaces (in place) and the
/// `system_extra` it appends (after the base's, when there is one).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ModelOverrides {
    /// Section overrides, keyed by section name.
    sections: BTreeMap<String, String>,
    /// The `system_extra` text, appended last. `None` when unset.
    system_extra: Option<String>,
}

/// The file's shape, as parsed. `deny_unknown_fields` on the top level is the
/// report for a section this daemon does not know: a `[foo]` is a parse error
/// carrying the parser's own message, not a silently ignored line. The keys INSIDE
/// `[base]` and `[model."NAME"]` are section names, captured by `flatten` and
/// checked against [`SYSTEM_SECTIONS`] in [`Prompts::load`] — a name the daemon
/// does not know is refused there, by name, with the path.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PromptsFile {
    #[serde(default)]
    base: Option<BaseSection>,
    #[serde(default)]
    model: BTreeMap<String, ModelSection>,
}

/// The `[base]` layer: section overrides plus the `system_extra` appended for every
/// model. `flatten` captures the section keys so [`Prompts::load`] can refuse an
/// unknown section name by name; `system_extra` is named so it is not mistaken for
/// a section.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
struct BaseSection {
    #[serde(flatten)]
    sections: BTreeMap<String, String>,
    #[serde(default)]
    system_extra: Option<String>,
}

/// The `[model."NAME"]` layer: section overrides plus the `system_extra` appended
/// last. The same shape as [`BaseSection`], scoped to one model.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
struct ModelSection {
    #[serde(flatten)]
    sections: BTreeMap<String, String>,
    #[serde(default)]
    system_extra: Option<String>,
}

impl Prompts {
    /// The path beside `providers.toml`: the same config dir, one file over.
    pub fn path() -> PathBuf {
        letibot_provider::keys::config_file()
            .parent()
            .map(|p| p.join("prompts.toml"))
            .unwrap_or_else(|| PathBuf::from("prompts.toml"))
    }

    /// Load `prompts.toml`. A missing file is `Ok` with no overrides — the
    /// byte-identical case, not an error. A file that does not parse, or that names
    /// a section this daemon does not know, is `Err` with the parser's own message
    /// (or the section's name) and the path, so the operator can see which file
    /// said what.
    pub fn load(path: &Path) -> Result<Prompts, String> {
        if !path.is_file() {
            return Ok(Prompts::default());
        }
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let file: PromptsFile =
            toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;

        // The `[base]` layer. An unknown section name is refused by name; an empty
        // one is dropped (it would shadow the provider glob it should fall through
        // to).
        let mut base = BTreeMap::new();
        let mut base_extra = None;
        if let Some(b) = file.base {
            for (name, value) in b.sections {
                if !is_known_section(&name) {
                    return Err(format!(
                        "{}: unknown section name `{name}` in [base]",
                        path.display()
                    ));
                }
                if !value.is_empty() {
                    base.insert(name, value);
                }
            }
            base_extra = b.system_extra.filter(|e| !e.is_empty());
        }

        // The per-model layers, the same rules per block.
        let mut models = BTreeMap::new();
        for (name, m) in file.model {
            let mut sections = BTreeMap::new();
            for (section, value) in m.sections {
                if !is_known_section(&section) {
                    return Err(format!(
                        "{}: unknown section name `{section}` in [model.\"{name}\"]",
                        path.display()
                    ));
                }
                if !value.is_empty() {
                    sections.insert(section, value);
                }
            }
            let system_extra = m.system_extra.filter(|e| !e.is_empty());
            if !sections.is_empty() || system_extra.is_some() {
                models.insert(
                    name,
                    ModelOverrides {
                        sections,
                        system_extra,
                    },
                );
            }
        }

        Ok(Prompts {
            base,
            base_extra,
            models,
        })
    }

    /// The composed system prompt for one model: the sections of [`DEFAULT_SYSTEM`]
    /// in their order, each replaced in place by the model's override, else the
    /// provider glob's, else `[base]`'s, else the default — plus the `system_extra`
    /// (the base's, then the model's), appended last, when there is one.
    ///
    /// **The safety property:** with no file, or a file that overrides nothing, this
    /// returns [`DEFAULT_SYSTEM`] byte for byte. That is what makes the feature safe
    /// to land — the default is unchanged, and a test asserts it.
    pub fn compose(&self, model_name: &str) -> String {
        // The exact block and the provider-glob block, if present. The exact name
        // is a more specific instruction than a provider-wide one, so it wins per
        // section; the glob is the fallback for a section the exact block did not
        // set. One lookup, used for the sections and the `system_extra` alike —
        // two matchers for one rule is the drift this tree deletes.
        let exact = self.models.get(model_name);
        let glob = model_name
            .split_once('/')
            .map(|(provider, _)| format!("{provider}/*"))
            .and_then(|g| self.models.get(&g));

        let sections: Vec<&str> = SYSTEM_SECTIONS
            .iter()
            .map(|(name, default)| {
                exact
                    .and_then(|m| m.sections.get(*name))
                    .or_else(|| glob.and_then(|m| m.sections.get(*name)))
                    .or_else(|| self.base.get(*name))
                    .map(|s| s.as_str())
                    .unwrap_or(*default)
            })
            .collect();
        let mut result = sections.join("\n\n");

        // The `[base] system_extra`, appended for every model.
        if let Some(extra) = self.base_extra.as_deref() {
            result.push_str("\n\n");
            result.push_str(extra);
        }
        // The model's `system_extra`, appended last: the exact block's, else the
        // glob's.
        let extra = exact
            .and_then(|m| m.system_extra.as_deref())
            .or_else(|| glob.and_then(|m| m.system_extra.as_deref()));
        if let Some(extra) = extra {
            result.push_str("\n\n");
            result.push_str(extra);
        }

        result
    }
}

/// Whether `name` is a section of [`DEFAULT_SYSTEM`], per [`SYSTEM_SECTIONS`].
fn is_known_section(name: &str) -> bool {
    SYSTEM_SECTIONS.iter().any(|(n, _)| *n == name)
}

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
        let w = self.planning_window().unwrap_or(0);
        (w / 16).max(2048).min(w / 4)
    }

    /// Is this turn's resident size close enough to the wall to compact first?
    /// `false` whenever the window is unknown — not knowing is not a reason to
    /// act, and an invented number here would compact conversations that had room.
    /// **The window, in the units the caller is counting in.**
    ///
    /// Every caller measures `resident` off the ledger, so the window has to be
    /// in ledger tokens too. [`Config::context_window`] is the PROVIDER's number
    /// — it is what the provider will refuse at, and it is what the banner
    /// should say — so planning against it directly compares two different
    /// counts. See [`Config::ledger_scale`] for the measurement and what it cost.
    ///
    /// The ratio is measured, not assumed, because it is not a constant: it is
    /// how much of the conversation is reasoning, which varies per session and
    /// over the life of one. Clamped to [1/4, 4] so a single odd turn cannot
    /// move the wall somewhere absurd, and `None` stays `None` — an unknown
    /// window is not a large one.
    pub fn planning_window(&self) -> Option<u64> {
        let w = self.context_window?;
        let Some((ledger, provider)) = self.ledger_scale else {
            return Some(w);
        };
        if ledger == 0 || provider == 0 {
            return Some(w);
        }
        let scaled = (w as u128 * ledger as u128) / provider as u128;
        Some((scaled.clamp((w / 4) as u128, (w as u128) * 4)) as u64)
    }

    /// **Ledger tokens as the provider would count them**, or `None` if nobody
    /// has measured the ratio yet.
    ///
    /// The inverse of what [`Config::planning_window`] does to the window, and it
    /// exists because one caller genuinely needs the conversion in this
    /// direction: a refusal the operator reads has to quote a number they can
    /// find on their own screen, and the screen shows the PROVIDER's count.
    ///
    /// `None` is the load-bearing case. It means "this session has not taken a
    /// metered turn yet, so the ratio is unmeasured", and a caller must not
    /// substitute the ledger's own figure for it. On a metered provider the
    /// ledger always over-counts — reasoning rows are in it and are dropped from
    /// what is sent — so treating ledger tokens as provider tokens does not fail
    /// safe, it fails by a measured 49% in the direction of refusing work that
    /// would have fitted. Measured on the operator's own session, 2026-09-20:
    /// 991,596 ledger tokens against 671,280 the provider counted, and a
    /// `/reseat` refused against a window it would have sat inside.
    /// **A ledger count, in the units the operator's header shows.**
    ///
    /// Every number the harness plans with is counted off the ledger, and on a
    /// metered provider that is not what the header says: the ledger holds the
    /// reasoning rows a messages provider is never sent, so it over-counts. The
    /// operator, reading a context-wall notice that said `1350750 of 1440006`
    /// beside a header reading 900k: *"1.35 is a lie - that top was shown as
    /// 900+"*. It was not a lie, it was the other unit — which is the same thing
    /// from where they were sitting.
    ///
    /// So a message a person reads converts first. Falls back to the ledger's own
    /// figure where there is nothing to convert with, which is honest: that IS
    /// the number in that case, because a local endpoint counts with the
    /// vocabulary the ledger uses.
    pub fn shown_tokens(&self, ledger_tokens: u64) -> u64 {
        self.provider_tokens(ledger_tokens).unwrap_or(ledger_tokens)
    }

    /// True when [`Config::shown_tokens`] is saying something different from the
    /// ledger, so a message can name the other number once rather than per figure.
    pub fn tokens_are_converted(&self) -> bool {
        self.provider_tokens(1_000_000)
            .is_some_and(|v| v != 1_000_000)
    }

    /// **A metered model with no measurement of the ratio yet** — the window between a
    /// switch and the first round on the new model.
    ///
    /// **Not the same question as `!tokens_are_converted()`**, which is why it is its own
    /// predicate: a local session's two counts agree by construction, so an unconverted
    /// number there is the whole truth, while an unconverted number on a metered model is a
    /// ledger figure standing in for a provider's — up to the clamp's factor of four out, in
    /// the direction that compacts early.
    ///
    /// The ruling's own words: *"until then the unscaled count stands and anything that reads
    /// it must be able to say it is unscaled rather than silently treating it as
    /// calibrated."*
    pub fn tokens_are_unscaled(&self) -> bool {
        self.provider.is_some() && self.ledger_scale.is_none()
    }

    pub fn provider_tokens(&self, ledger_tokens: u64) -> Option<u64> {
        let (ledger, provider) = self.ledger_scale?;
        if ledger == 0 || provider == 0 {
            return None;
        }
        Some(((ledger_tokens as u128 * provider as u128) / ledger as u128) as u64)
    }

    /// **Is the ledger AT THE WALL — within a headroom of the window?**
    ///
    /// This is [`Config::should_compact`]'s threshold half, factored out because
    /// two questions share it and only one of them is the flag's business:
    ///
    /// * *would the next turn fit — should something be tidied pre-emptively?* is
    ///   [`Config::should_compact`], and `auto_compact` answers it. The no-progress
    ///   guard stands the flag down precisely to stop that door compacting once
    ///   per turn for ever.
    /// * *a turn was just refused for context length — is there a way forward?*
    ///   reads THIS method with the flag deliberately absent, because the guard
    ///   turns the flag off **without freeing anything** and a refusal recovery
    ///   gated on it would refuse the only lever it has. MEASURED 2026-10-09,
    ///   session `s-1789919514688401228`: one automatic compaction made no room,
    ///   the guard switched compaction off, and the next thirty provider refusals
    ///   (1,048,624 / 1,049,070 / 1,049,191 tokens against a 1,048,576 limit) met
    ///   a recovery that would not act — a wedge with no door out.
    ///
    /// `false` whenever the window is unknown, for the same reason
    /// [`Config::should_compact`] says false: not knowing is not a reason to act,
    /// and an invented number here would compact conversations that had room.
    pub fn at_the_wall(&self, resident_tokens: u64) -> bool {
        let Some(w) = self.planning_window() else {
            return false;
        };
        resident_tokens + self.headroom() >= w
    }

    /// **The PRE-EMPTIVE question: would the next turn not fit, so should
    /// something be tidied first?** [`Config::auto_compact`]'s own gate, and the
    /// one the no-progress guard stands down. A refusal that has already happened
    /// is the other question — see [`Config::at_the_wall`].
    pub fn should_compact(&self, resident_tokens: u64) -> bool {
        self.auto_compact && self.at_the_wall(resident_tokens)
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
    /// `resident_tokens` is counted off the LEDGER, like every other caller's,
    /// so the window it is measured against is [`Config::planning_window`] and
    /// not the provider's raw number. Reading `context_window` here compared two
    /// different counts and, on a metered provider where the ledger over-counts,
    /// closed the gate on turns that had room. See `planning_window`.
    pub fn room_for_next_turn(&self, resident_tokens: u64) -> bool {
        match self.planning_window() {
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
    letibot_websearch::resolve_key(None)
        .ok()
        .map(|_| "brave".to_string())
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
            // **No default GGUF.** This was the development box's own path, which on any
            // other machine is a file that is not there; a provider needs none, and a
            // local model is named by `--vocab` (the launcher always passes one).
            vocab_gguf: None,
            workspace: workspace.into(),
            socket: letibot_sessionlog::server::default_socket_path(),
            store: None,
            session_id: format!("s-{}", now_ns()),
            parent_session_id: None,
            depth: 0,
            http: None,
            ledger_scale: None,
            unconfined: false,
            title: String::new(),
            owner: std::env::var("USER").unwrap_or_else(|_| "operator".into()),
            system: DEFAULT_SYSTEM.into(),
            // No overrides until `run` loads the operator's `prompts.toml`. `Default`
            // is the byte-identical case: a daemon that never reads the file composes
            // `DEFAULT_SYSTEM` for every session.
            prompts: Prompts::default(),
            // No project models until `run` discovers and loads the project's
            // `leticode.toml`. `Default` is the byte-identical case: a daemon with
            // no project file runs on its own models and discloses nothing from it.
            leticode: crate::leticode_config::LeticodeConfig::default(),
            // And no user file either, until `run` reads the fixed path beside
            // `providers.toml`. Same byte-identical case at the other level.
            leticode_user: crate::leticode_config::LeticodeConfig::default(),
            // Nothing has spoken for the model yet. `Builtin` is the honest
            // starting value and the honest final one for a daemon launched with
            // no flag and no file: the model is the one this daemon was started
            // on, and the disclosure says exactly that rather than naming a file
            // that did not speak.
            model_source: ModelSource::default(),
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
            media_marker: None,
            auto_compact: true,
            auto_compact_stood_down: None,
            placement: letibot_tools::builtins::task::Placement::Host,
            vm_args: Vec::new(),
            provider: None,
            fabric: None,
            allow_bash: false,
            max_subagent_depth: 3,
            adjudicator: AdjudicatorChoice::default(),
            oracle: None,
            oracle_model: None,
            oracle_scope: None,
            oracle_budget: std::time::Duration::from_millis(400),
            oracle_max_tokens: None,
            oracle_question: crate::oracle::Question::Verdict,
            supervise: false,
            intent_prose: false,
            spill: SpillPolicy::Unset,
            spill_storage: SpillStorage::Memory,
            // Far out on purpose: the progress detector is the stop, and this is the
            // thing that catches a turn which never stops making new results.
            // Unbounded: see the field. `stall_rounds` is the stop that measures
            // progress, and a round count never could.
            max_tool_rounds: 0,
            http_retries: crate::harness::MAX_HTTP_RETRIES,
            stall_rounds: 5,
        }
    }

    /// **The model name the prompt lookup runs on**: `provider/model` for a metered
    /// session, the bare alias for a local one.
    ///
    /// It is the same name the operator sees in `/models` and the header, so a
    /// `prompts.toml` written against what is on screen is a `prompts.toml` that
    /// matches. When the operator named no model (`--provider deepseek`), the
    /// preset's default is resolved from the catalogue rather than left off: a
    /// `deepseek/*` glob must match the model the session actually runs on, and
    /// `deepseek` with no model part would match nothing.
    pub fn prompt_model_name(&self) -> String {
        match &self.provider {
            Some(pc) => {
                let model = pc.model.clone().unwrap_or_else(|| {
                    letibot_provider::Preset::parse(&pc.name)
                        .ok()
                        .map(|p| p.default_model(&letibot_provider::catalogue::Catalogue::load()))
                        .unwrap_or_default()
                });
                if model.is_empty() {
                    pc.name.clone()
                } else {
                    format!("{}/{}", pc.name, model)
                }
            }
            None => self.model.clone(),
        }
    }

    /// **Compose `system` from `prompts.toml` and this session's model. Once, at
    /// session open.**
    ///
    /// This is the only place the prompt is built from the file, and it runs before
    /// the system message is written into the transcript. After it, `system` is
    /// message 0 and is never rewritten: a session that started under one prompt
    /// keeps it for its whole life, even if the model is switched or the file is
    /// edited. That is the rule the operator asked for, and it is what makes an
    /// edit to `prompts.toml` a fact about NEW sessions rather than a rewrite of
    /// old ones.
    pub fn compose_system(&mut self) {
        let name = self.prompt_model_name();
        self.system = self.prompts.compose(&name);
    }

    /// [`Config::compose_system`], then the standing notes read off disk.
    ///
    /// The one input to the prompt that is NOT session-frozen: `AGENTS.md` and the
    /// notes directories are read here, every time this runs, and the assembled
    /// section (verbatim under a token budget, a digest with line references over
    /// it — see [`crate::standing_notes`]) is appended after `system_extra`, so it
    /// is the newest thing the model reads before the conversation. Called at
    /// session open with the vocabulary the session will count with; every base
    /// rebuild re-reads through `Harness::reseat_target` rather than here.
    ///
    /// Takes the [`Vocab`] rather than holding one: `Config` is built before any
    /// engine exists, and the encoder is the session's, not the daemon's.
    pub fn compose_system_with_notes(&mut self, vocab: &letibot_tokencore::Vocab) {
        self.compose_system();
        let section = crate::standing_notes::section(
            &self.workspace,
            &crate::standing_notes::global_dir(),
            vocab,
        );
        self.system = crate::standing_notes::replace(&self.system, section.as_deref());
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
    pub fn settings(
        &self,
        mode_source: &str,
        supervising: bool,
        door_tools: &[letibot_sessionlog::protocol::HeadRunTool],
    ) -> Vec<letibot_sessionlog::protocol::SettingRow> {
        use letibot_sessionlog::protocol::SettingRow;
        let row = |key: &str, value: String, source: &str, editable: &str| SettingRow {
            key: key.into(),
            value,
            source: source.into(),
            editable: editable.into(),
            choices: Vec::new(),
            tools: Vec::new(),
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
            row(
                "mode",
                self.mode.name.to_string(),
                mode_source,
                "/mode NAME",
            ),
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
            // **The local models this fleet declares, right after `local`.**
            //
            // This row is what the head's model PICKER draws, and it was the half the
            // first cut missed: `models_choice` resolved a declared name and
            // `models_listing` printed one, so `/models dense78` worked and the text
            // listing showed it — while bare `/models`, which opens the picker, built
            // its rows from here and knew only the compiled-in presets. The operator:
            // *"there is no dense78"*, on a daemon that had the block, the binary and
            // the name all working.
            //
            // That is this row's own warning coming true from the other side — *"a head
            // that kept its own copy of a list got it wrong"* — except the stale list
            // was the daemon's, and a feature reachable only by typing its name exactly
            // is a feature nobody discovers.
            //
            // Above the presets because they cost nothing to run: no key, no meter.
            for m in letibot_provider::keys::local_models(None) {
                names.push(m.name);
            }
            for p in letibot_provider::presets::ALL {
                names.push(format!("{}/{}", p.name, p.default_model(&cat)));
            }
            let now = match &self.provider {
                // The local alias is kept in the value, because this row replaced
                // the read-only one that carried it and a pane that stopped
                // naming the model the server is running would be a worse row.
                // The first word is what a picker matches on, so it stays `local`.
                // **Named when it is a declared local model**, so the picker shows the
                // row the operator is on rather than the bare word `local`. Matched on
                // the address, because that is what the switch actually moved: two
                // blocks can name one alias and only the endpoint says which is live.
                None => match letibot_provider::keys::local_models(None)
                    .into_iter()
                    .find(|m| {
                        crate::harness::local_url_authority(&m.url).as_deref()
                            == Some(self.endpoint.authority().as_str())
                            && m.model == self.model
                    }) {
                    Some(m) => format!("{} ({} at {})", m.name, m.model, m.url),
                    None => format!("local ({})", self.model),
                },
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
            let source = if self.provider.is_none() {
                "--model"
            } else {
                ""
            };
            let mut r = row("model", now, source, "/models PROVIDER/MODEL");
            r.choices = names;
            out.push(r);
        }
        out.push(choices(
            row(
                "supervise",
                if supervising {
                    "on — the guard model answers".into()
                } else {
                    "off".into()
                },
                "",
                "/supervise on|off",
            ),
            &["on", "off"],
        ));
        // **And which of those rows this box can actually use** — the operator's ask of
        // 2026-10-04: *"model peeker should green models we have keys for. — if i choose a model
        // without key picker should ask for the key"*.
        //
        // **Only the daemon can answer it, and that is why it is a row.** Whether a preset
        // resolves a key is a fact about this box — an environment variable, a stored key, or the
        // key opencode filed under its own provider id — and a head that guessed would green a row
        // that refuses at the first turn. It travels as a row for the reason `daemon.verbs` does:
        // the half that owns the fact publishes it, and the half that draws it reads it rather
        // than keeping a copy that drifts.
        //
        // `local` is absent on purpose: it needs no credential, so its presence in this list
        // would be a claim about a key rather than about reachability.
        {
            let keyed: Vec<&str> = letibot_provider::presets::ALL
                .iter()
                .filter(|p| letibot_provider::keys::resolve(p, None, None).is_ok())
                .map(|p| p.name)
                .collect();
            out.push(row(
                letibot_sessionlog::protocol::MODEL_KEYS_KEY,
                keyed.join(","),
                "each preset's own file, variable or opencode's auth.json",
                "/models PROVIDER/MODEL --key PASTE",
            ));
        }
        // **And the rows that need no credential at all**, which is the other half of
        // the same question and cannot live in the row above — see
        // `MODEL_KEYLESS_KEY`. The operator: *"dense78 needs a key this box does not
        // hold"*, about a box on the LAN that has no key and wants none.
        {
            let mut keyless = vec!["local".to_string()];
            for m in letibot_provider::keys::local_models(None) {
                keyless.push(m.name);
            }
            out.push(row(
                letibot_sessionlog::protocol::MODEL_KEYLESS_KEY,
                keyless.join(","),
                "this daemon's own server, and the [model.\"...\"] blocks with a url",
                "",
            ));
        }
        // **The names an operator may run themselves** — R24 part two, decision 3.
        //
        // Carried on the wire rather than compiled into a head, and that is the whole
        // requirement: a list a head holds is a list that drifts, and the daemon is the side
        // that ENFORCES this one — see `ClientFrame::OperatorCall`'s handler, which refuses
        // anything else by name. `choices` is empty because this is not a closed set of values
        // for the *setting*; it is the list, and a head reads `value`.
        // **The list, and beside it each name's bare form** — R31 and R32.
        //
        // `value` stays the comma-joined names because that is what it has always been and a
        // head reading it keeps working; `tools` is the same list described, so a head can
        // turn `/web_search blabla` into the JSON the wire wants and complete a path for
        // `/read` while knowing nothing about either tool. See
        // [`letibot_sessionlog::protocol::HeadRunTool`].
        let mut tools_row = row(
            letibot_sessionlog::HEAD_RUN_TOOLS_KEY,
            letibot_sessionlog::HEAD_RUN_TOOLS.join(","),
            "default",
            "",
        );
        tools_row.tools = door_tools.to_vec();
        out.push(tools_row);
        // **The verbs this daemon answers** — R32, and the same argument as the tool list
        // above: a head completes `/`-commands from a table, a head that does not recognise
        // a verb forwards it, so this half of the namespace is the daemon's to publish. The
        // head's own verbs come from its own dispatcher; neither enumerates the other's.
        out.push(row(
            letibot_sessionlog::protocol::DAEMON_VERBS_KEY,
            crate::slash::VERBS.join(","),
            "default",
            "",
        ));
        // **The sections a compaction's record has** — R27, and the same argument as the
        // row above it: a list a head holds is a list that drifts, and this one is the
        // daemon's own template. A head reads `value` and splits on `,`, and an absent
        // row means a daemon older than this — *no structure to draw*, so the sentence in
        // the warning's `detail` is all there is and it draws exactly what it drew
        // before. `choices` is empty for the reason `head-run.tools`' is: this is not a
        // closed set of values for a setting, it is the list.
        out.push(row(
            letibot_sessionlog::COMPACTION_SECTIONS_KEY,
            letibot_turn::WIRE_SECTIONS.join(","),
            "default",
            "",
        ));
        // What the session is.
        out.push(row("session", self.session_id.clone(), "", ""));
        out.push(row("seat", self.seat.as_str().to_string(), "--role", ""));
        out.push(row(
            "workspace",
            self.workspace.display().to_string(),
            "",
            "",
        ));
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
        out.push(row(
            "dialect",
            self.dialect.name().to_string(),
            "--dialect",
            "",
        ));
        out.push(row("endpoint", self.endpoint.authority(), "--endpoint", ""));
        out.push(row(
            "vocab",
            match &self.vocab_gguf {
                Some(p) => p.display().to_string(),
                None => "none — the byte vocabulary, for a cloud provider".to_string(),
            },
            "--vocab",
            "",
        ));
        out.push(row(
            "context",
            match self.context_window {
                Some(n) => format!("{n} tokens"),
                None => "asked of the server".into(),
            },
            "",
            "",
        ));
        // **The row a stood-down compaction is found by.** The value carries the
        // pair that turned it off (shown in the operator's units, like every other
        // number the pane renders) because `off` alone cannot tell the reader
        // WHICH of the two states it is: a `--no-auto-compact` the operator typed,
        // or the no-progress guard acting on its own after a summary that gained
        // no room. One is a choice, the other is a finding, and a head that draws
        // a resident line for the second must not draw it for the first. The
        // spelling `off — …` is what the head keys on; see its `auto-compact` arm.
        out.push(row(
            "auto-compact",
            match (self.auto_compact, self.auto_compact_stood_down) {
                (true, _) => "on".into(),
                (false, Some((resident, after))) => format!(
                    "off — the no-progress guard stood it down: compacted {} to {} \
                     tokens and still no room for the next turn",
                    self.shown_tokens(resident),
                    self.shown_tokens(after)
                ),
                (false, None) => "off".into(),
            },
            if self.auto_compact_stood_down.is_some() {
                "the no-progress guard, after a compaction that gained no room"
            } else {
                ""
            },
            "",
        ));
        out.push(row(
            "effort",
            self.effort.clone().unwrap_or_else(|| "default".into()),
            "--effort",
            "",
        ));
        if let Some(p) = &self.provider {
            out.push(row(
                "provider",
                format!(
                    "{}{}",
                    p.name,
                    p.model
                        .as_ref()
                        .map(|m| format!(" ({m})"))
                        .unwrap_or_default()
                ),
                "providers.toml",
                "",
            ));
        }
        // The gate.
        out.push(row(
            "adjudicator",
            self.adjudicator.as_str().to_string(),
            "--adjudicator",
            "",
        ));
        out.push(row(
            "oracle",
            self.oracle
                .as_ref()
                .map(|e| e.authority())
                .unwrap_or_else(|| "none".into()),
            "--oracle",
            "/supervise HOST:PORT",
        ));
        out.push(row(
            "oracle.model",
            self.oracle_model
                .clone()
                .unwrap_or_else(|| "server's default".into()),
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
            "oracle.max_tokens",
            // **The ceiling** (R12). Shown rather than implied, because a reply cut off at
            // it is a *budget* rather than an unreadable answer and this is the number to
            // raise — and a number an operator cannot see is a number they cannot act on.
            match self.oracle_max_tokens {
                None => format!("{} (default)", crate::oracle::DEFAULT_MAX_TOKENS),
                Some(n) => n.to_string(),
            },
            "--oracle-max-tokens",
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
                    if s.is_declared() {
                        " (declared)"
                    } else {
                        " (earned)"
                    }
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
                self.downgrade
                    .deny
                    .iter()
                    .map(|a| a.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            },
            "",
            "",
        ));
        out.push(row(
            "bash",
            self.allow_bash.to_string(),
            "--bash / --no-bash",
            "",
        ));
        out.push(row("unconfined", self.unconfined.to_string(), "", ""));
        out.push(row(
            "grants.ro",
            if self.grants_ro.is_empty() {
                "none".into()
            } else {
                self.grants_ro
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
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
        out.push(row(
            "max-tool-rounds",
            match self.max_tool_rounds {
                0 => "unlimited".into(),
                n => n.to_string(),
            },
            "",
            "",
        ));
        out.push(row("stall-rounds", self.stall_rounds.to_string(), "", ""));
        out.push(row("intent.prose", self.intent_prose.to_string(), "", ""));
        // The fabric.
        out.push(row(
            "flowy",
            if self.flowy.is_some() {
                "configured".into()
            } else {
                "off".into()
            },
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
            if self.web_search.is_some() {
                "configured".into()
            } else {
                "off".into()
            },
            "$BRAVE_API_KEY / providers.toml",
            "",
        ));
        out.push(row(
            "web_fetch",
            if self.web_fetch {
                "curl".into()
            } else {
                "off".into()
            },
            "--web-fetch",
            "",
        ));
        // The plumbing.
        out.push(row(
            "socket",
            self.socket.display().to_string(),
            "--socket",
            "",
        ));
        out.push(row(
            "store",
            self.store
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "none".into()),
            "--store",
            "",
        ));
        out
    }

    pub fn disclosures(&self, wiring: &GateWiring) -> Vec<Disclosure> {
        let mut out = Vec::new();
        // **Which models the project file set, said at startup.** The operator's
        // ask, and the half of the loud-failure rule that answers "where did this
        // model come from" from the screen: a file that changes which model answers
        // is disclosed by name, so a session that runs on a project's models does not
        // read as one that runs on the daemon's. Absent file, or a file that set
        // nothing, discloses nothing — the byte-identical case, and a banner that
        // named a file that set nothing would be a disclosure that is a guess.
        //
        // **Two levels, two lines**, because they are two files and the operator
        // who wrote one of them needs to see which one spoke. The project line is
        // the merge the session actually reads, so it names the keys in force.
        if let Some(path) = &self.leticode.path {
            let set = self.leticode.set_models();
            if !set.is_empty() {
                out.push(Disclosure::on(
                    "project models",
                    format!(
                        "{} sets {} — command line beats this file, this file beats the user \
                         file, the user file beats the built-in default, and a key it does not \
                         set falls through",
                        path.display(),
                        set.join(", ")
                    ),
                ));
            }
        }
        if let Some(path) = &self.leticode_user.path {
            let set = self.leticode_user.set_models();
            if !set.is_empty() {
                out.push(Disclosure::on(
                    "user models",
                    format!(
                        "{} sets {} — the box-wide level, one file for every project; the \
                         nearest project `leticode.toml` overrides it key by key",
                        path.display(),
                        set.join(", ")
                    ),
                ));
            }
        }
        // **Which model answers, and which level said so.** The operator's
        // question of the evening: *"why is my session on deepseek"*, with the
        // answer in a file they had to go and find. Here it is on the screen, one
        // line, and the precedence is the same line rather than a thing to look
        // up.
        out.push(Disclosure::on(
            "session model",
            self.model_source.render(&self.prompt_model_name()),
        ));
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
        let backstop = match self.max_tool_rounds {
            0 => "unlimited".to_string(),
            n => format!("{n} rounds"),
        };
        match (self.stall_rounds, self.max_tool_rounds) {
            // **Both off: nothing but the model ends a turn.** Not a refusal — the
            // operator asked for the backstop to come off and the progress check is
            // theirs to arm — but it is a different setting from either one alone
            // and has to read as one.
            (0, 0) => out.push(Disclosure::off(
                "progress check",
                "NOTHING STOPS A TURN",
                "no progress check and no round backstop: a turn ends when the model \
                 stops calling tools, when it meets the context wall, or when you \
                 interrupt it — and nothing else. A turn that loops forever will loop \
                 forever. Pass --stall-rounds N to arm the check that reads progress, \
                 or --max-tool-rounds N for a count of effort.",
            )),
            (0, _) => out.push(Disclosure::off(
                "progress check",
                "OFF",
                &format!(
                    "nothing measures whether a turn is getting anywhere; the only \
                     stop is the {backstop} backstop, which counts effort rather than \
                     progress. Pass --stall-rounds N."
                ),
            )),
            _ => out.push(Disclosure::on(
                "progress check",
                format!(
                    "stops after {} consecutive rounds producing nothing new; the \
                     round backstop is {backstop}",
                    self.stall_rounds
                ),
            )),
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
                    if env {
                        ", plus $LETIBOT_PERMISSION"
                    } else {
                        ""
                    }
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
        let (state, detail, active) = letibot_tools::adjudicate::startup_disclosure_for(
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
                         {} decisions recorded: {} you answered yourself, {} where an \
                         oracle was actually consulted, {} where you and it differ, \
                         {} decided by a model. \
                         `/gate recent` shows them; `/gate ok|grant|revoke ID [note]` \
                         rules on one after the fact.",
                        c.total,
                        c.decided_by_operator,
                        c.measured,
                        c.disagreements,
                        c.model_decided
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
        // **What `task(where: firecode)` would get on this host**, said where a subagent can be
        // started — FIRECODE-NOTES item 4: the placement's preconditions used to be found out
        // at spawn, one refusal per attempt.
        if wiring
            .seated
            .iter()
            .any(|t| t == "task" || t == "task_start")
        {
            match &wiring.firecode {
                Some(fc) => out.push(Disclosure::on(
                    "firecode",
                    format!(
                        "a subagent can be placed in a VM: firecode at {}{}",
                        fc.path.display(),
                        if fc.inherit {
                            ", and a VM's copy inherits its source's layers"
                        } else {
                            " — no layer inheritance (this firecode has no `layer inherit`): \
                             a VM boots on base-image toolchains"
                        }
                    ),
                )),
                None => out.push(Disclosure::off(
                    "firecode",
                    "NOT FOUND",
                    "no firecode on the daemon's PATH and no $FIRECODE_BIN, so \
                     `task(where: firecode)` is refused at the call; subagents run in this \
                     session's own boundary",
                )),
            }
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
    /// The firecode a subagent would be placed in a VM with, asked once at start — `None`
    /// when this host has none, so `task(where: firecode)` would be refused.
    pub firecode: Option<letibot_tools::firecode::Installed>,
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
            firecode: None,
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

    /// **Every declared local model reaches the picker's row.**
    ///
    /// The regression this closes was live and the operator found it: `/models dense78`
    /// worked, `models_listing` printed it, and bare `/models` — which opens the picker,
    /// and the picker draws THIS row's `choices` — showed only the compiled-in presets.
    /// A model reachable solely by typing its exact name is a model nobody discovers.
    ///
    /// **Honest about its own reach**: `Config::settings` reads the real
    /// `providers.toml`, with no seam to point it elsewhere, so on a box that declares
    /// nothing this test passes vacuously. It is still worth keeping — it fails loudly
    /// on any box that HAS a declaration, which is every box the feature is for, and it
    /// records what the row is supposed to contain. Threading a config path through
    /// `settings` would make it unconditional and is the better fix if this ever breaks
    /// again.
    #[test]
    fn the_model_row_offers_every_declared_local_model() {
        let cfg = Config::for_this_box("/tmp");
        let rows = cfg.settings("", false, &[]);
        let model = rows.iter().find(|r| r.key == "model").expect("a model row");
        for m in letibot_provider::keys::local_models(None) {
            assert!(
                model.choices.contains(&m.name),
                "`{}` is declared in providers.toml but the picker does not offer it: {:?}",
                m.name,
                model.choices
            );
        }
        assert!(
            model.choices.iter().any(|c| c == "local"),
            "the daemon's own server stays first: {:?}",
            model.choices
        );
    }

    /// **The picker's greening comes from a row, and the row names the presets this box can
    /// actually authenticate.**
    ///
    /// The operator, 2026-10-04: *"model peeker should green models we have keys for."* Whether a
    /// preset resolves a key is a fact about this box — a file, a variable, or what opencode filed
    /// under its own provider id — so only the daemon can answer it, and the answer travels as a
    /// row rather than as a head's guess.
    ///
    /// **The assertion is the agreement**, because that is the property that matters and not a
    /// list: every name in the row resolves a key, every preset NOT named does not, and no name is
    /// anything but a preset `ALL` carries. A row that disagreed with the check the daemon makes
    /// when it refuses a switch would green exactly the rows that fail at the first turn.
    #[test]
    fn the_box_publishes_which_providers_it_holds_a_key_for() {
        let cfg = Config::for_this_box("/tmp");
        let rows = cfg.settings("default", false, &[]);
        let row = rows
            .iter()
            .find(|r| r.key == letibot_sessionlog::protocol::MODEL_KEYS_KEY)
            .expect("the keyed-providers row is published");
        assert!(
            !row.value.contains(' '),
            "the list is comma-joined with no spaces, so a head splits on one thing: {:?}",
            row.value
        );
        let named: Vec<&str> = if row.value.is_empty() {
            Vec::new()
        } else {
            row.value.split(',').collect()
        };
        for n in &named {
            assert!(
                letibot_provider::presets::ALL.iter().any(|p| p.name == *n),
                "`{n}` is not a preset this build knows"
            );
        }
        // **`local` is absent on purpose**: it needs no credential, and its presence here would
        // be a claim about a key rather than about reachability.
        assert!(!named.contains(&"local"), "{named:?}");
        for p in letibot_provider::presets::ALL {
            let resolves = letibot_provider::keys::resolve(p, None, None).is_ok();
            assert_eq!(
                resolves,
                named.contains(&p.name),
                "`{}` disagrees with the answer `keys::resolve` gives it",
                p.name
            );
        }
        // A plain value row: a head COLOURS with it, it does not choose from it.
        assert!(row.choices.is_empty(), "{row:?}");
    }

    /// **The consented point describes itself, not the one that was asked for.**
    ///
    /// `Mode::ALLOW_ALL`'s summary says "the VM is the boundary and nothing
    /// inside it reaches this box", which is true of a firecode placement and
    /// false of the operator's laptop. Printing it for a session that landed on
    /// `ALLOW_ALL_HERE` put that sentence two lines above "the confinement
    /// prerequisite is what refuses this point on a bare host" — on 2026-09-20,
    /// in one card, on their screen.
    #[test]
    fn the_two_allow_all_points_do_not_borrow_each_others_sentences() {
        use letibot_tools::mode::Mode;
        assert!(
            Mode::ALLOW_ALL.summary.contains("the VM is the boundary"),
            "the confined point still claims a VM"
        );
        assert!(
            !Mode::ALLOW_ALL_HERE.summary.contains("VM"),
            "the consented point must not claim a boundary it does not have: {}",
            Mode::ALLOW_ALL_HERE.summary
        );
        assert!(
            Mode::ALLOW_ALL_HERE.summary.contains("operator confirmed"),
            "and must say what it DOES stand on: {}",
            Mode::ALLOW_ALL_HERE.summary
        );
        // The names differ too, so a card naming the applied point cannot read as
        // the confined one.
        assert_ne!(Mode::ALLOW_ALL.name, Mode::ALLOW_ALL_HERE.name);
    }

    /// **A carry is quoted in the units the operator can check.**
    ///
    /// `/reseat` refused on `991596 token(s) in front of a 1000000-token window`
    /// while the header said 671k, and both numbers were right: 991,596 is
    /// `s-1789462738453908838#t20`'s ledger sum and 671,280 is what DeepSeek
    /// counted for the same 2,702 rows. The refusal compared the first against a
    /// window expressed in the second.
    ///
    /// Three claims, and the first is the premise the other two need: the two
    /// counts really do differ here; the conversion lands on the number the
    /// header shows; and the converted carry FITS, so the refusal was wrong.
    #[test]
    fn a_carry_is_converted_into_the_providers_units_before_it_is_judged() {
        let mut cfg = Config::for_this_box("/tmp");
        cfg.context_window = Some(1_000_000);
        // Unmeasured: there is nothing to convert with, and saying so is the
        // point — a caller must not substitute the ledger's own figure.
        assert_eq!(cfg.provider_tokens(991_596), None);

        cfg.ledger_scale = Some((1_000_699, 671_280));
        let carried = cfg.provider_tokens(991_596).expect("a conversion");
        assert!(
            (660_000..680_000).contains(&carried),
            "the ledger's 991596 is about 665k to the provider, which is what the \
             operator's header showed; got {carried}"
        );
        assert!(
            carried + cfg.headroom() < 1_000_000,
            "and it FITS — {carried} plus {} headroom against 1000000. The refusal \
             the operator saw was the unconverted comparison.",
            cfg.headroom()
        );
        // The unconverted comparison, kept here so the regression is named rather
        // than merely absent: this is what used to be asked, and it says no.
        assert!(
            991_596 + cfg.headroom() >= 1_000_000,
            "the premise: the LEDGER figure does not fit, which is why the units \
             mattered"
        );
    }

    /// The wall continuation asked the same question with the same mistake.
    ///
    /// `room_for_next_turn` is handed `ledger_len`, so its window has to be the
    /// ledger's too. Reading `context_window` closed the gate on a conversation
    /// the provider had 330k tokens of room for.
    #[test]
    fn room_for_the_next_turn_is_measured_in_ledger_tokens() {
        let mut cfg = Config::for_this_box("/tmp");
        cfg.context_window = Some(1_000_000);
        cfg.ledger_scale = Some((1_000_699, 671_280));
        assert!(
            cfg.room_for_next_turn(991_596),
            "991596 ledger tokens is ~665k to the provider and has room; the raw \
             window said no"
        );
        // And it still says no when there genuinely is none, so the fix is not
        // "always true".
        assert!(!cfg.room_for_next_turn(cfg.planning_window().unwrap()));
    }

    /// **The window is planned in the units the ledger counts in.**
    ///
    /// The ledger counts a LOCAL rendered prompt with reasoning in it; a
    /// messages provider is sent neither, because `letibot_provider::messages`
    /// drops `Reasoning` (DeepSeek documents that `reasoning_content` must not
    /// be sent back). Comparing one against the other made a conversation that
    /// thinks a lot compact while most of its window was free.
    ///
    /// The numbers below are the operator's own, 2026-09-20.
    #[test]
    fn the_planning_window_follows_the_measured_ledger_to_provider_ratio() {
        let mut cfg = Config::for_this_box("/tmp");
        cfg.context_window = Some(1_000_000);
        cfg.auto_compact = true;

        // Before any metered turn there is nothing to convert with, so the
        // provider's number stands.
        assert_eq!(cfg.planning_window(), Some(1_000_000));

        // letibot #t18: the ledger called it 1,072,176; deepseek called it
        // ~607,000. Planning against 1,000,000 ledger tokens compacted a
        // conversation with 39% of its window free.
        cfg.ledger_scale = Some((1_072_176, 607_000));
        let w = cfg.planning_window().expect("a window");
        assert!(
            (1_700_000..1_800_000).contains(&w),
            "a 1M provider window is ~1.77M ledger tokens here, got {w}"
        );
        assert!(
            !cfg.should_compact(1_072_176),
            "this is the turn that compacted early and must not any more"
        );
        // And it still compacts when the conversation really is at the wall.
        assert!(cfg.should_compact(w));

        // leticl #t11 thought less, so its ledger runs closer to the provider's
        // count — the ratio is a measurement, not a constant.
        cfg.ledger_scale = Some((1_495_702, 1_048_607));
        let w2 = cfg.planning_window().expect("a window");
        assert!((1_400_000..1_500_000).contains(&w2), "got {w2}");

        // A nonsense measurement cannot move the wall somewhere absurd.
        cfg.ledger_scale = Some((1, 1_000_000));
        assert_eq!(cfg.planning_window(), Some(250_000), "clamped at a quarter");
        cfg.ledger_scale = Some((1_000_000, 1));
        assert_eq!(
            cfg.planning_window(),
            Some(4_000_000),
            "clamped at four times"
        );

        // An unknown window is still not a large one.
        cfg.context_window = None;
        assert_eq!(cfg.planning_window(), None);
        assert!(!cfg.should_compact(u64::MAX / 2));
    }

    /// **A session that starts on a metered provider gets a wall.**
    ///
    /// `harnessd` read the window off `/props` only when there was no provider,
    /// so `[default] provider = deepseek` in the operator's providers.toml left
    /// `context_window` at `None` — and `None` is not a big window, it is no
    /// window: every compaction check reads `let Some(window) = …` and is
    /// skipped. Measured 2026-09-20: their session reached 1,023,545 resident
    /// tokens against a model the catalogue puts at 1,000,000, having never
    /// compacted once. *"leticl session shows 1m context and doesnt compact"*.
    #[test]
    fn a_metered_provider_takes_its_window_from_the_catalogue() {
        let dir = std::env::temp_dir().join(format!("letibot-cat-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("models.json");
        std::fs::write(
            &path,
            r#"{"deepseek":{"id":"deepseek","models":{
                 "deepseek-flash":{"id":"deepseek-flash","name":"f",
                   "limit":{"context":1000000,"output":384000},
                   "cost":{"input":0.28,"output":0.42}}}}}"#,
        )
        .expect("fixture");
        let cat = letibot_provider::catalogue::Catalogue::read(&path).expect("catalogue");

        let pc = |model: Option<&str>| ProviderConfig {
            name: "deepseek".into(),
            model: model.map(str::to_string),
            api_key: None,
            thinking: false,
        };
        assert_eq!(
            pc(Some("deepseek-flash")).catalogue_window(&cat),
            Some(1_000_000),
            "the wall the operator's session never had"
        );
        // A model the catalogue does not carry stays `None` — the old behaviour,
        // for the case the old comment was actually about.
        assert_eq!(pc(Some("no-such-model")).catalogue_window(&cat), None);
        // And a preset this build does not know.
        assert_eq!(
            ProviderConfig {
                name: "nope".into(),
                ..pc(None)
            }
            .catalogue_window(&cat),
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
    use super::*;

    // --- prompts.toml: per-model system-prompt composition -------------------

    /// A temp dir for a `prompts.toml` fixture, unique per test so parallel tests
    /// do not share a file. The caller removes it.
    fn prompts_dir(test: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("letibot-prompts-{}-{}", std::process::id(), test));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    /// (a) **No file: the composed prompt is `DEFAULT_SYSTEM` byte for byte.**
    ///
    /// The safety property that makes the feature safe to land: a daemon with no
    /// `prompts.toml` composes the built-in prompt, unchanged, for every model.
    #[test]
    fn no_prompts_file_composes_the_default_byte_for_byte() {
        let dir = prompts_dir("no_file");
        let path = dir.join("prompts.toml");
        let prompts = Prompts::load(&path).expect("a missing file is Ok, not an error");
        assert_eq!(prompts.compose("deepseek/deepseek-flash"), DEFAULT_SYSTEM);
        assert_eq!(prompts.compose("qwen-3.8-flash-next"), DEFAULT_SYSTEM);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// (b) **An empty file: the composed prompt is `DEFAULT_SYSTEM` byte for byte.**
    ///
    /// A file that overrides nothing is the same as no file: the default is
    /// unchanged.
    #[test]
    fn an_empty_prompts_file_composes_the_default_byte_for_byte() {
        let dir = prompts_dir("empty");
        let path = dir.join("prompts.toml");
        std::fs::write(&path, "").expect("fixture");
        let prompts = Prompts::load(&path).expect("an empty file is Ok");
        assert_eq!(prompts.compose("deepseek/deepseek-flash"), DEFAULT_SYSTEM);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// (c) **A `[model."NAME"]` override applies to that model's session and not to
    /// another's.**
    ///
    /// The named model gets the extra, appended to the base; every other model does
    /// not. The override is scoped to the name, not global.
    #[test]
    fn a_model_override_applies_to_its_model_and_not_another() {
        let dir = prompts_dir("model_override");
        let path = dir.join("prompts.toml");
        std::fs::write(
            &path,
            r#"
[model."deepseek/deepseek-flash"]
system_extra = "You are a deepseek specialist."
"#,
        )
        .expect("fixture");
        let prompts = Prompts::load(&path).expect("parse");
        // The named model gets the extra, appended to the base.
        assert_eq!(
            prompts.compose("deepseek/deepseek-flash"),
            format!("{DEFAULT_SYSTEM}\n\nYou are a deepseek specialist.")
        );
        // Another model under the same provider does not.
        assert_eq!(prompts.compose("deepseek/deepseek-chat"), DEFAULT_SYSTEM);
        // A model under a different provider does not.
        assert_eq!(prompts.compose("glm-coding/glm-5.3"), DEFAULT_SYSTEM);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A provider glob matches every model under that provider, and the exact
    /// name wins when both are present.**
    ///
    /// `deepseek/*` is the operator's way of saying "any deepseek model" without
    /// naming each one. An exact name is a more specific instruction, so it wins
    /// when both are present for the same model.
    #[test]
    fn a_provider_glob_matches_its_models_and_the_exact_name_wins() {
        let dir = prompts_dir("glob");
        let path = dir.join("prompts.toml");
        std::fs::write(
            &path,
            r#"
[model."deepseek/*"]
system_extra = "DeepSeek house style."

[model."deepseek/deepseek-flash"]
system_extra = "Flash is special."
"#,
        )
        .expect("fixture");
        let prompts = Prompts::load(&path).expect("parse");
        // The exact name wins for the model it names.
        assert_eq!(
            prompts.compose("deepseek/deepseek-flash"),
            format!("{DEFAULT_SYSTEM}\n\nFlash is special.")
        );
        // The glob covers the rest of the provider's models.
        assert_eq!(
            prompts.compose("deepseek/deepseek-chat"),
            format!("{DEFAULT_SYSTEM}\n\nDeepSeek house style.")
        );
        // Another provider is untouched.
        assert_eq!(prompts.compose("glm-coding/glm-5.3"), DEFAULT_SYSTEM);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// (d) **The `[base]` section replaces that section for every model.**
    ///
    /// Present, the base section is the operator's text, not the built-in, for every
    /// model. The other sections are left alone, and a model extra still appends.
    /// (There is no `[base] system` any more: the wholesale replacement it was is
    /// the shape that could not override one section for one model.)
    #[test]
    fn the_base_section_replaces_that_section_for_every_model() {
        let dir = prompts_dir("base");
        let path = dir.join("prompts.toml");
        std::fs::write(
            &path,
            r#"
[base]
tone = "A wholly different tone."

[model."deepseek/deepseek-flash"]
system_extra = "And a model extra on top."
"#,
        )
        .expect("fixture");
        let prompts = Prompts::load(&path).expect("parse");
        // The base tone replaces the default tone for a model with no override.
        let composed = prompts.compose("glm-coding/glm-5.3");
        assert!(composed.contains("A wholly different tone."));
        assert!(!composed.contains("Be direct. Prefer the shortest answer that is complete."));
        // The other sections are still the default.
        assert!(composed.contains("You are a careful software engineering assistant"));
        // A model extra appends to the composed base.
        let composed = prompts.compose("deepseek/deepseek-flash");
        assert!(composed.contains("A wholly different tone."));
        assert!(composed.ends_with("And a model extra on top."));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// (e) **A malformed file is reported with the path and the parser's message,
    /// and the session still runs on `DEFAULT_SYSTEM`.**
    ///
    /// The report is the parser's own message, prefixed with the path, so the
    /// operator can see which file said what. The "session still runs" half is the
    /// `Err` case in `run`: a refused file leaves `Config.prompts` at `Default`,
    /// which composes `DEFAULT_SYSTEM` — asserted here directly.
    #[test]
    fn a_malformed_prompts_file_is_reported_and_the_session_runs_on_the_default() {
        let dir = prompts_dir("malformed");
        let path = dir.join("prompts.toml");
        std::fs::write(&path, "this is not [valid toml").expect("fixture");
        let err = Prompts::load(&path).expect_err("a malformed file is Err");
        // The report names the path.
        let path_str = path.to_str().expect("utf-8 path");
        assert!(err.contains(path_str), "the path is in the report: {err}");
        // And it carries the parser's own message, not a bare "failed".
        assert!(
            err.len() > path_str.len() + 4,
            "the parser's message is in the report: {err}"
        );
        // The session still runs: a refused file leaves the prompts at `Default`,
        // which composes `DEFAULT_SYSTEM` byte for byte.
        let cfg = Config::for_this_box("/tmp");
        assert_eq!(
            cfg.prompts.compose("deepseek/deepseek-flash"),
            DEFAULT_SYSTEM
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A top-level section the daemon does not know is a parse error, not a
    /// silent ignore.**
    ///
    /// `deny_unknown_fields` on the top level is the report: a `[foo]` section is
    /// refused with the parser's own message rather than dropped. (A section NAME
    /// this daemon does not know, inside `[base]` or `[model."NAME"]`, is refused
    /// by [`Prompts::load`] — see the tests below.)
    #[test]
    fn an_unknown_section_is_refused_not_ignored() {
        let dir = prompts_dir("unknown_section");
        let path = dir.join("prompts.toml");
        std::fs::write(&path, "[foo]\nbar = \"baz\"\n").expect("fixture");
        let err = Prompts::load(&path).expect_err("an unknown section is Err");
        let path_str = path.to_str().expect("utf-8 path");
        assert!(err.contains(path_str), "the path is in the report: {err}");
        assert!(err.contains("foo"), "the unknown section is named: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **`SYSTEM_SECTIONS` joins to `DEFAULT_SYSTEM` byte for byte.**
    ///
    /// The test that makes the split owned rather than invented: the table is not
    /// "a concatenation nobody owns", it is the constant, checked. An edit to a
    /// paragraph that forgets the table fails here, and so does a table whose
    /// boundaries moved.
    #[test]
    fn the_system_sections_join_to_the_default_byte_for_byte() {
        let joined = SYSTEM_SECTIONS
            .iter()
            .map(|(_, text)| *text)
            .collect::<Vec<_>>()
            .join("\n\n");
        assert_eq!(joined, DEFAULT_SYSTEM);
    }

    /// **The notes convention is instruction in the base, not data in a block.**
    ///
    /// The injected standing-notes block is DATA — what the files on disk say
    /// right now — and its envelope frames what the block is. The convention —
    /// that notes exist, what belongs in one, where it lands, and that a note
    /// is not ground truth — is INSTRUCTION, and instruction belongs in
    /// [`DEFAULT_SYSTEM`] so it holds even for a session whose notes are empty.
    /// The two must not restate each other: a duplicated sentence is one that
    /// drifts, so this also asserts the paragraph does not carry the
    /// envelope's own framing sentence.
    #[test]
    fn the_default_prompt_carries_the_notes_convention_and_not_the_envelope() {
        let (name, paragraph) = SYSTEM_SECTIONS
            .iter()
            .find(|(n, _)| *n == "notes")
            .expect("a `notes` section");
        assert_eq!(*name, "notes");
        // It exists, it is read into the prompt, it comes back after a
        // compaction.
        assert!(paragraph.contains("reads into this prompt"));
        assert!(paragraph.contains("compaction"));
        // The purpose the operator named, in their words: keep what is worth
        // keeping — not a running summary.
        assert!(paragraph.contains("interesting, remarkable or surprising"));
        assert!(paragraph.contains("not want to rediscover"));
        assert!(paragraph.contains("not as a summary of what you did"));
        // Where they live and how to write one.
        assert!(paragraph.contains("`notes` tool"));
        assert!(paragraph.contains(".letibot/notes/"));
        // The caveat, so a note is not read as ground truth.
        assert!(paragraph.contains("what was true or intended when it was written"));
        // And not the injected block's framing — that sentence is the
        // envelope's job and is asserted there.
        assert!(!paragraph.contains("one in force"));
        assert!(!paragraph.contains("[standing-notes"));
    }

    /// **A section replaced by `[model."NAME"]` changes only that section and keeps
    /// the order.**
    ///
    /// The named model's `read_only_tools` is the operator's text, not the built-in;
    /// every other section is the default, in the order it appears in
    /// [`DEFAULT_SYSTEM`].
    #[test]
    fn a_model_section_replaces_only_that_section_and_keeps_the_order() {
        let dir = prompts_dir("model_section");
        let path = dir.join("prompts.toml");
        std::fs::write(
            &path,
            r#"
[model."qwen-3.8-27b"]
read_only_tools = "You have read-only tools, and you use them."
"#,
        )
        .expect("fixture");
        let prompts = Prompts::load(&path).expect("parse");
        let composed = prompts.compose("qwen-3.8-27b");
        // The replaced section is the operator's text, not the default.
        assert!(composed.contains("You have read-only tools, and you use them."));
        assert!(!composed.contains("Do not call a tool for a question about the world"));
        // Every other section is the default, in the order it appears.
        let identity = SYSTEM_SECTIONS[0].1;
        let language = SYSTEM_SECTIONS[1].1;
        let find_and_read = SYSTEM_SECTIONS[3].1;
        let edit_files = SYSTEM_SECTIONS[4].1;
        let scratch = SYSTEM_SECTIONS[5].1;
        let scratch_path = SYSTEM_SECTIONS[6].1;
        let notes = SYSTEM_SECTIONS[7].1;
        let tone = SYSTEM_SECTIONS[8].1;
        let expected = [
            identity,
            language,
            "You have read-only tools, and you use them.",
            find_and_read,
            edit_files,
            scratch,
            scratch_path,
            notes,
            tone,
        ]
        .join("\n\n");
        assert_eq!(composed, expected);
        // Another model is untouched.
        assert_eq!(prompts.compose("deepseek/deepseek-flash"), DEFAULT_SYSTEM);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A `[base]` section is overridden by a model one, in place.**
    ///
    /// The base section is the operator's text for every model, but a model's own
    /// section replaces it for that model — the base is the fallback, not the
    /// ceiling.
    #[test]
    fn a_base_section_is_overridden_by_a_model_one() {
        let dir = prompts_dir("base_overridden");
        let path = dir.join("prompts.toml");
        std::fs::write(
            &path,
            r#"
[base]
tone = "House tone, for every model."

[model."deepseek/deepseek-flash"]
tone = "Flash has its own tone."
"#,
        )
        .expect("fixture");
        let prompts = Prompts::load(&path).expect("parse");
        // The named model gets its own tone, not the base one.
        let composed = prompts.compose("deepseek/deepseek-flash");
        assert!(composed.contains("Flash has its own tone."));
        assert!(!composed.contains("House tone, for every model."));
        // Another model gets the base tone.
        let composed = prompts.compose("deepseek/deepseek-chat");
        assert!(composed.contains("House tone, for every model."));
        assert!(!composed.contains("Flash has its own tone."));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **`system_extra` is appended last, and not appended when unset.**
    ///
    /// When a layer sets `system_extra`, it is appended after every section. When no
    /// layer sets it, nothing is appended and the composition is the sections alone.
    #[test]
    fn system_extra_is_appended_last_and_not_when_unset() {
        let dir = prompts_dir("system_extra");
        let path = dir.join("prompts.toml");
        std::fs::write(
            &path,
            r#"
[model."deepseek/deepseek-flash"]
system_extra = "One short tool call beats a long plan."
"#,
        )
        .expect("fixture");
        let prompts = Prompts::load(&path).expect("parse");
        // Set: appended after every section.
        let composed = prompts.compose("deepseek/deepseek-flash");
        assert!(composed.ends_with("One short tool call beats a long plan."));
        assert_eq!(
            composed,
            format!("{DEFAULT_SYSTEM}\n\nOne short tool call beats a long plan.")
        );
        // Unset for another model: nothing appended, the default byte for byte.
        assert_eq!(prompts.compose("deepseek/deepseek-chat"), DEFAULT_SYSTEM);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **An unknown section key is refused by name, in each layer.**
    ///
    /// A section name the daemon does not know is a load error naming the key and
    /// the file's path, in `[base]` and in `[model."NAME"]` alike. The session runs
    /// on the un-overridden composition: a refused file leaves the prompts at
    /// `Default`, which composes `DEFAULT_SYSTEM`.
    #[test]
    fn an_unknown_section_key_is_refused_by_name_in_each_layer() {
        // In `[base]`.
        let dir = prompts_dir("unknown_base_key");
        let path = dir.join("prompts.toml");
        std::fs::write(&path, "[base]\nread_only_tool = \"typo\"\n").expect("fixture");
        let err = Prompts::load(&path).expect_err("an unknown base key is Err");
        let path_str = path.to_str().expect("utf-8 path");
        assert!(err.contains(path_str), "the path is in the report: {err}");
        assert!(err.contains("read_only_tool"), "the key is named: {err}");
        assert!(err.contains("[base]"), "the layer is named: {err}");
        let _ = std::fs::remove_dir_all(&dir);

        // In `[model."NAME"]`.
        let dir = prompts_dir("unknown_model_key");
        let path = dir.join("prompts.toml");
        std::fs::write(
            &path,
            "[model.\"deepseek/deepseek-flash\"]\ntonee = \"typo\"\n",
        )
        .expect("fixture");
        let err = Prompts::load(&path).expect_err("an unknown model key is Err");
        let path_str = path.to_str().expect("utf-8 path");
        assert!(err.contains(path_str), "the path is in the report: {err}");
        assert!(err.contains("tonee"), "the key is named: {err}");
        assert!(
            err.contains("deepseek/deepseek-flash"),
            "the model is named: {err}"
        );
        let _ = std::fs::remove_dir_all(&dir);

        // The session still runs: a refused file leaves the prompts at `Default`,
        // which composes `DEFAULT_SYSTEM` byte for byte.
        let cfg = Config::for_this_box("/tmp");
        assert_eq!(
            cfg.prompts.compose("deepseek/deepseek-flash"),
            DEFAULT_SYSTEM
        );
    }

    /// **The operator's excerpt, end to end.**
    ///
    /// A small utility model seated with `web_search` is told, by the default
    /// `read_only_tools`, not to call a tool for a question about the world — so a
    /// model whose whole job is to look things up is discouraged from looking them
    /// up. The operator's fix: replace `read_only_tools` for that one model, in
    /// place, and append a `system_extra`. The composed prompt for that model has
    /// the new `read_only_tools` and not the default one; a session on another model
    /// has the default one.
    #[test]
    fn the_operators_excerpt_composes_the_new_read_only_tools_for_that_model() {
        let dir = prompts_dir("operator_excerpt");
        let path = dir.join("prompts.toml");
        std::fs::write(
            &path,
            r#"
[base]
tone = "Short. Direct. No preamble."

[model."qwen-3.8-27b"]
read_only_tools = "You have read-only tools, and a web_search. For a fact about the world — a version, a date, a definition, something that changed last week — call `web_search` first and answer from what comes back. Do not answer from memory, and do not guess."
system_extra = "One short tool call beats a long plan."
"#,
        )
        .expect("fixture");
        let prompts = Prompts::load(&path).expect("parse");

        // The named model: the new read_only_tools, not the default one.
        let composed = prompts.compose("qwen-3.8-27b");
        assert!(composed.contains("call `web_search` first and answer from what comes back"));
        assert!(!composed.contains("Do not call a tool for a question about the world"));
        // The base tone is in place, and the model extra is appended last.
        assert!(composed.contains("Short. Direct. No preamble."));
        assert!(composed.ends_with("One short tool call beats a long plan."));
        // The other sections are the default, in order.
        assert!(composed.contains("You are a careful software engineering assistant"));
        assert!(composed.contains("Find with `grep` and read with `read`"));

        // A session on another model: the default read_only_tools, the base tone,
        // and no model extra.
        let composed = prompts.compose("deepseek/deepseek-flash");
        assert!(composed.contains("Do not call a tool for a question about the world"));
        assert!(!composed.contains("call `web_search` first and answer from what comes back"));
        assert!(composed.contains("Short. Direct. No preamble."));
        assert!(!composed.ends_with("One short tool call beats a long plan."));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **`Config::compose_system` composes from the file and the session's model,
    /// and `prompt_model_name` names the model the way the operator sees it.**
    ///
    /// This is the wiring: the composition runs on the session's config, using the
    /// model name the operator would write in `prompts.toml`.
    #[test]
    fn compose_system_uses_the_session_model_name() {
        // A local session: the model name is the bare alias.
        let mut cfg = Config::for_this_box("/tmp");
        cfg.model = "qwen-3.8-flash-next".into();
        assert_eq!(cfg.prompt_model_name(), "qwen-3.8-flash-next");
        cfg.compose_system();
        assert_eq!(cfg.system, DEFAULT_SYSTEM);

        // A metered session: the model name is `provider/model`.
        let mut cfg = Config::for_this_box("/tmp");
        cfg.provider = Some(ProviderConfig {
            name: "deepseek".into(),
            model: Some("deepseek-flash".into()),
            ..Default::default()
        });
        assert_eq!(cfg.prompt_model_name(), "deepseek/deepseek-flash");
        cfg.compose_system();
        assert_eq!(cfg.system, DEFAULT_SYSTEM);
    }

    /// **The model row names the provider when there is one.**
    ///
    /// This is the daemon's half of R20.1: the operator's turns went to deepseek while
    /// the head's header said `qwen-3.8-27b`. The header prefers this row and falls back
    /// to `Hello`'s `wiring.model`, which is the daemon's `--model` — so if this row says
    /// `local (qwen-3.8-27b)` when a provider is configured, the daemon is what is wrong
    /// and the head is faithfully drawing it.
    #[test]
    fn the_model_row_names_the_provider_when_one_is_configured() {
        let mut cfg = Config::for_this_box("/tmp/x");
        assert!(
            cfg.provider.is_none(),
            "a fresh config is the local server, or this test says nothing"
        );
        let local = cfg
            .settings("--model", false, &[])
            .into_iter()
            .find(|r| r.key == "model")
            .expect("model");
        assert!(
            local.value.starts_with("local ("),
            "no provider must name the local alias: {}",
            local.value
        );

        // Now a provider, which is what `--provider deepseek` resolves to before any
        // session exists.
        cfg.provider = Some(ProviderConfig {
            name: "deepseek".into(),
            model: None,
            ..Default::default()
        });
        let row = cfg
            .settings("", false, &[])
            .into_iter()
            .find(|r| r.key == "model")
            .expect("model");
        assert!(
            row.value.starts_with("deepseek/"),
            "a configured provider must be named by the model row, or every head goes on \
             showing the local alias: {}",
            row.value
        );
        assert!(
            !row.value.contains("qwen"),
            "the local alias must not survive a provider: {}",
            row.value
        );
        // And the source is empty rather than `--model`: the model is the provider's,
        // not the flag's.
        assert_eq!(row.source, "", "{}", row.source);
    }

    /// **`compose_system_with_notes` puts the standing notes after
    /// `system_extra`, and only there.**
    ///
    /// The brief's placement rule: the newest thing the model reads before the
    /// conversation. Asserted by position against a `system_extra` the
    /// fixture sets, and the section is read back with `carried` — the same
    /// extraction a base rebuild uses — so the test also pins that what is
    /// appended is what is swapped later. Assertions are made against the
    /// workspace's file with a nonce, so a box whose global notes directory
    /// has files of its own still passes: those belong in the section too.
    #[test]
    fn compose_system_with_notes_appends_the_section_after_system_extra() {
        let ws = std::env::temp_dir().join(format!("letibot-notes-order-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&ws);
        std::fs::create_dir_all(&ws).expect("temp dir");
        std::fs::write(ws.join("AGENTS.md"), "nonce-notes-order\n").expect("fixture");
        let mut cfg = Config::for_this_box(&ws);
        cfg.prompts = Prompts {
            base_extra: Some("extra goes before the notes".into()),
            ..Prompts::default()
        };
        cfg.compose_system_with_notes(&letibot_tokencore::Vocab::bytes([], []));
        assert!(cfg.system.contains("nonce-notes-order"), "{}", cfg.system);
        let extra = cfg
            .system
            .find("extra goes before the notes")
            .expect("extra");
        let notes = cfg
            .system
            .find("[standing-notes-begin]")
            .expect("the section is in the composed prompt");
        assert!(
            extra < notes,
            "`system_extra` first, the notes after it: {}",
            cfg.system
        );
        // What was appended is what a rebuild would swap out — same envelope,
        // markers included.
        let carried = crate::standing_notes::carried(&cfg.system).expect("carried");
        assert!(carried.contains("nonce-notes-order"), "{carried}");
        // And the composition without notes is untouched: `compose_system`'s
        // own byte-for-byte test still rules that half.
        let mut bare = Config::for_this_box(&ws);
        bare.prompts = cfg.prompts.clone();
        bare.compose_system();
        assert_eq!(
            crate::standing_notes::replace(&cfg.system, None),
            bare.system,
            "stripping the section gives back the plain composition"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    /// The pane's contract: the two rows that change now come first and name
    /// their verb; every key is unique (a duplicate would be two rows that
    /// disagree); nothing that takes a restart claims a verb.
    #[test]
    fn settings_rows_lead_with_what_changes_now_and_never_repeat_a_key() {
        let cfg = Config::for_this_box("/tmp/x");
        let rows = cfg.settings("project store (modes.tsv)", false, &[]);
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
        let sup = rows
            .iter()
            .find(|r| r.key == "supervise")
            .expect("supervise");
        assert!(sup.value.starts_with("off"));
        // The model row carries its own choices, so a head can draw the picker
        // without keeping a list of its own.
        let model = rows.iter().find(|r| r.key == "model").expect("model");
        assert!(
            model.value.starts_with("local ("),
            "no provider is the local server, and the row still names its alias: {}",
            model.value
        );
        assert!(
            model.choices.contains(&"local".to_string()),
            "{:?}",
            model.choices
        );
        assert!(
            model.choices.iter().any(|c| c.starts_with("glm/")),
            "{:?}",
            model.choices
        );
        let mut seen = std::collections::HashSet::new();
        for r in &rows {
            assert!(seen.insert(r.key.clone()), "duplicate key {}", r.key);
        }
        assert!(
            rows.iter()
                .any(|r| r.key == "oracle.budget" && r.editable.is_empty())
        );
        assert!(
            rows.iter()
                .any(|r| r.key == "workspace" && r.value == "/tmp/x")
        );
        // Supervision on reads as on.
        let on = cfg.settings("x", true, &[]);
        let sup = on.iter().find(|r| r.key == "supervise").expect("supervise");
        assert!(sup.value.starts_with("on"));
    }

    /// **The round count is a backstop now, not the stop.**
    ///
    /// The defect this replaced: `max_tool_rounds: 12` was the only thing that could
    /// end a runaway turn, so it also ended two working ones. What is asserted is the
    /// *relationship* — a progress check is armed, and the round count is not a
    /// thing a real investigation can run into — rather than either number, because
    /// a test pinning the number would have to be edited to change it and would then
    /// be pinning nothing.
    ///
    /// The count is now `0`, unbounded, at the operator's word: *"make 200 tool
    /// calls limit configurable and set it to infinity"*. The relationship is
    /// unchanged and so is this test's point — a count of rounds measures effort,
    /// and the guard that reads progress is the one that has to be armed.
    #[test]
    fn the_stop_is_the_progress_check_and_the_round_count_is_the_backstop() {
        let c = Config::for_this_box("/tmp");
        assert!(
            c.stall_rounds > 0,
            "a session with no progress check is the defect"
        );
        assert!(
            c.max_tool_rounds == 0 || c.max_tool_rounds >= 100,
            "a real investigation is dozens of rounds, so the backstop is either \
             unbounded or far outside that range; {} is inside it and would cut one",
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

    /// **Both guards off is its own state, and says so.**
    ///
    /// The round backstop is unbounded by default now, so the progress check is
    /// the only thing measuring anything. Turning that off as well leaves nothing
    /// between a looping turn and forever — not a refusal, because the operator
    /// asked for the backstop off and the check is theirs to arm, but it must not
    /// read as either one alone. A disclosure that said "the only stop is the
    /// 0-round backstop" would be worse than silence: it names a guard that is
    /// not there.
    #[test]
    fn both_guards_off_is_disclosed_as_its_own_state() {
        let mut c = Config::for_this_box("/tmp");
        assert_eq!(c.max_tool_rounds, 0, "the premise: unbounded by default");
        c.stall_rounds = 0;
        let line = c
            .disclosures(&GateWiring::read_only())
            .into_iter()
            .find(|d| d.subject == "progress check")
            .expect("especially when there is nothing left")
            .to_string();
        assert!(
            line.contains("NOTHING STOPS A TURN"),
            "it has to read as its own state: {line}"
        );
        assert!(
            !line.contains("0-round"),
            "and must never name a backstop that is not armed: {line}"
        );
        // Both ways back are named, because either one alone re-arms something.
        assert!(line.contains("--stall-rounds"), "{line}");
        assert!(line.contains("--max-tool-rounds"), "{line}");
    }

    /// **The disclosure says which level won.** The operator's own evening — *"why is
    /// my session on deepseek"* — was answered by a file they had to go and read, and
    /// the fix is that the answer is on the screen: the model, the level by name, and
    /// the file when a file is where it came from. A disclosure that named only the
    /// model would be the same defect one line shorter.
    #[test]
    fn the_session_model_disclosure_names_the_level_that_won() {
        use crate::config::ModelSource;
        use crate::leticode_config::Level;
        let mut c = Config::for_this_box("/tmp");

        // No flag, no file: the built-in default, and it says so — the fourth of the
        // four levels rather than a sentence about nothing.
        c.model_source = ModelSource::default();
        let line = c
            .disclosures(&GateWiring::read_only())
            .into_iter()
            .find(|d| d.subject == "session model")
            .expect("the model is always disclosed, and always with its level")
            .to_string();
        assert!(line.contains("built-in"), "{line}");
        assert!(
            line.contains("the command line beats the project file"),
            "and the precedence is on the same line rather than in a file: {line}"
        );

        // Each of the four levels names itself, and a file level names the file.
        for (level, path, wanted) in [
            (Level::Flag, None, "the command line"),
            (
                Level::Project,
                Some(PathBuf::from("/w/leticode.toml")),
                "/w/leticode.toml",
            ),
            (
                Level::User,
                Some(PathBuf::from("/h/.config/letibot/leticode.toml")),
                "/h/.config/letibot/leticode.toml",
            ),
            (Level::Builtin, None, "the built-in default"),
        ] {
            c.model_source = ModelSource {
                level,
                value: Some("glm-5.3-flash".into()),
                file: path,
                origin: "--provider deepseek".into(),
                note: None,
            };
            let line = c
                .disclosures(&GateWiring::read_only())
                .into_iter()
                .find(|d| d.subject == "session model")
                .expect("session model")
                .to_string();
            assert!(
                line.contains(wanted),
                "{level:?} must name {wanted}: {line}"
            );
            assert!(
                line.contains(level.as_str()),
                "and its word is stable: {line}"
            );
        }

        // A fault found at startup rides the same line, because the screen is where
        // the operator asked for it.
        c.model_source = ModelSource {
            level: Level::User,
            value: Some("glm-5.3-flash".into()),
            file: Some(PathBuf::from("/h/.config/letibot/leticode.toml")),
            origin: String::new(),
            note: Some("`glm-5.3-flash` is a local alias, and the server is not serving it".into()),
        };
        let line = c
            .disclosures(&GateWiring::read_only())
            .into_iter()
            .find(|d| d.subject == "session model")
            .expect("session model")
            .to_string();
        assert!(line.contains("is not serving it"), "{line}");
    }

    /// **The sentence, verbatim.** The deliverable of requirement 3 is a sentence on
    /// the screen, so it is pinned here rather than described: a change to it is a
    /// change to what the operator reads, and that is a thing to be deliberate about
    /// rather than a thing to notice later.
    #[test]
    fn the_session_model_sentence_is_this_one() {
        use crate::leticode_config::Level;
        let user = ModelSource {
            level: Level::User,
            value: Some("glm-5.3-flash".into()),
            file: Some(PathBuf::from("/home/dead/.config/letibot/leticode.toml")),
            origin: String::new(),
            note: None,
        };
        assert_eq!(
            user.render("glm-5.3-flash"),
            "main glm-5.3-flash — from the user level \
             (/home/dead/.config/letibot/leticode.toml). The order is: the command line beats \
             the project file, the project file beats the user file, the user file beats the \
             built-in default. The built-in default is `[default]` in \
             ~/.config/letibot/providers.toml (what `/models NAME` writes), else the local \
             server this daemon was launched against."
        );

        // The built-in case says which built-in it is: `[default]` when that is what
        // spoke, and the launch line when nothing did.
        let default_block = ModelSource {
            level: Level::Builtin,
            value: Some("deepseek/deepseek-flash".into()),
            file: Some(PathBuf::from("/home/dead/.config/letibot/providers.toml")),
            origin: "[default] in /home/dead/.config/letibot/providers.toml".into(),
            note: None,
        };
        let line = default_block.render("deepseek/deepseek-flash");
        assert!(
            line.starts_with(
                "main deepseek/deepseek-flash — from the built-in level \
                 (/home/dead/.config/letibot/providers.toml)."
            ),
            "{line}"
        );

        // A fault is appended after the precedence, so the rule is read before the
        // exception to it.
        let faulty = ModelSource {
            note: Some("and `no-such-alias` is not one this daemon can reach.".into()),
            ..user.clone()
        };
        let line = faulty.render("glm-5.3-flash");
        assert!(
            line.ends_with("and `no-such-alias` is not one this daemon can reach."),
            "{line}"
        );
    }

    /// **No file at all is the built-in default, and says so.** The fresh config is
    /// exactly the daemon that read no `leticode.toml` at either level and was given
    /// no `--provider`: the fourth level, named, rather than a silence the operator
    /// has to interpret.
    #[test]
    fn no_file_at_all_is_the_builtin_default_and_says_so() {
        let c = Config::for_this_box("/tmp");
        assert!(c.leticode.path.is_none(), "the premise: no file was read");
        assert!(c.leticode_user.path.is_none());
        let line = c
            .disclosures(&GateWiring::read_only())
            .into_iter()
            .find(|d| d.subject == "session model")
            .expect("session model")
            .to_string();
        assert!(
            line.contains("the built-in default"),
            "nothing spoke, and the screen says which level that is: {line}"
        );
        assert!(
            line.contains(&c.prompt_model_name()),
            "and it names the model, so the line is about this session: {line}"
        );
    }

    /// **The user file is disclosed as its own level, and the project's over it.**
    /// Two files, two lines, because the operator who wrote one of them needs to see
    /// which one spoke — and the project line names the keys in force after the merge.
    #[test]
    fn the_user_file_is_disclosed_as_its_own_level() {
        let mut c = Config::for_this_box("/tmp");
        let subjects: Vec<String> = c
            .disclosures(&GateWiring::read_only())
            .into_iter()
            .map(|d| d.subject)
            .collect();
        assert!(
            !subjects.iter().any(|s| s == "user models"),
            "a file that is not there discloses nothing: {subjects:?}"
        );

        c.leticode_user = crate::leticode_config::LeticodeConfig {
            main_model: Some("glm-5.3-flash".into()),
            path: Some(PathBuf::from("/h/.config/letibot/leticode.toml")),
            level: crate::leticode_config::Level::User,
            ..Default::default()
        };
        // The merge the session reads: the user level as the base, nothing over it.
        c.leticode = c.leticode_user.clone();
        let line = c
            .disclosures(&GateWiring::read_only())
            .into_iter()
            .find(|d| d.subject == "user models")
            .expect("the user file speaks, so it is named")
            .to_string();
        assert!(line.contains("/h/.config/letibot/leticode.toml"), "{line}");
        assert!(line.contains("main glm-5.3-flash"), "{line}");
        assert!(
            line.contains("overrides it key by key"),
            "and it says what can overrule it: {line}"
        );
    }

    /// **The settings row says `unlimited`, not `0`.** An operator reading a table of
    /// numbers reads `0` as "zero rounds allowed", which is the opposite.
    #[test]
    fn an_unbounded_backstop_reads_as_unlimited_not_as_zero() {
        let c = Config::for_this_box("/tmp");
        let rows = c.settings("x", false, &[]);
        let r = rows
            .iter()
            .find(|r| r.key == "max-tool-rounds")
            .expect("the row");
        assert_eq!(r.value, "unlimited");
        let mut bounded = c.clone();
        bounded.max_tool_rounds = 12;
        assert_eq!(
            bounded
                .settings("x", false, &[])
                .iter()
                .find(|r| r.key == "max-tool-rounds")
                .expect("the row")
                .value,
            "12",
            "and a real bound still reads as its number"
        );
    }

    /// Off is reachable and is **said out loud**, because a refusal that can only be
    /// routed around teaches people to route around refusals.
    #[test]
    fn turning_the_progress_check_off_is_a_declared_state() {
        let mut c = Config::for_this_box("/tmp");
        c.stall_rounds = 0;
        // With a backstop still armed. Both off is a DIFFERENT state and has its
        // own disclosure and its own test below — reading them as one is how an
        // operator comes to believe something is still catching runaway turns.
        c.max_tool_rounds = 200;
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
        assert!(
            w.active,
            "an attached adjudicator over a writable backend is on"
        );
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
        for known in [
            "orchestrator",
            "planner",
            "researcher",
            "coder",
            "runner",
            "leticode",
        ] {
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

    /// **Whether a subagent can be placed in a VM is said at start, not found out at spawn** —
    /// FIRECODE-NOTES item 4: *"Resolve the binary and probe capabilities at daemon start, and
    /// publish them in the banner."* Only where a subagent can be started at all.
    #[test]
    fn the_banner_says_whether_a_subagent_can_be_placed_in_a_vm() {
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
        let base = GateWiring {
            seated: vec!["task".into()],
            ..GateWiring::read_only()
        };
        let found = GateWiring {
            firecode: Some(letibot_tools::firecode::Installed {
                path: "/opt/homebrew/bin/firecode".into(),
                inherit: false,
            }),
            ..base.clone()
        };
        let line = detail(&found, "firecode");
        assert!(line.contains("/opt/homebrew/bin/firecode"), "{line}");
        assert!(line.contains("no layer inheritance"), "{line}");
        let missing = detail(&base, "firecode");
        assert!(missing.contains("refused"), "{missing}");
        assert!(missing.contains("FIRECODE_BIN"), "{missing}");
        // And no line at all where no subagent can be started.
        assert!(
            !subjects(&GateWiring::read_only())
                .iter()
                .any(|s| s == "firecode")
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
            assert!(
                all.contains(expected),
                "{expected} is not disclosed:\n{all}"
            );
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
        assert!(
            DEFAULT_SYSTEM.contains("Answer in English"),
            "§5.2: state the language"
        );
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
            assert!(
                h < w,
                "window {w}: headroom {h} is not smaller than the window"
            );
            assert!(
                h <= w / 4,
                "window {w}: headroom {h} is more than a quarter"
            );
            // An empty conversation must never be at the wall.
            assert!(
                !cfg.should_compact(0),
                "window {w} compacts an empty session"
            );
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
