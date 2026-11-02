//! What a daemon needs to know before it can open a session.
//!
//! Every field is a fact the daemon cannot invent, and there is deliberately no `Default`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use letibot_turn::Endpoint;
use serde_json::{Value, json};

use crate::dialect::Dialect;

/// Where a project's reviewers run - `[queue] reviewers` in `leticode.toml`, `--reviewers`
/// on the daemon's command line, and [`Config::reviewers`] in a session. Defined in
/// `mergequeue::settings`, because the project file is where a PROJECT says it.
pub use crate::mergequeue::settings::Reviewers;

/// §8.3 clause 5's budget, as configuration. `Unset` is the default and it is correct, per
/// D6: nothing spills until somebody configures a budget, and `harnessd --spill-inline N` is
/// where that gap stops being invisible.
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
    /// In this process. Dies with the daemon, which makes `read_spill` a lie across a restart -
    /// tolerable only while the budget is `Unset` and nothing can spill.
    Memory,
    /// Under a directory, one subdirectory per session.
    Dir(PathBuf),
}

/// Which role a session seats, and therefore what it can reach. Nothing widens by default
/// [`Seat::Orchestrator`] is what a `letibot` invocation already got. The role picks the tool list
/// the list's `Access` picks the backend, and that decides whether an adjudicator is required.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Seat {
    /// §8.4's `orchestrator`: `read`, `grep`, `glob`, `ask_code`, `ask_corpus`
    /// `read_spill`. Read-only backend, no gate traffic. The default.
    #[default]
    Orchestrator,
    /// D9's plan mode: read, search, the intent list, and the two things that make a plan a plan - `write_plan` (scoped to plan documents by taking a *name*, not a path) and `say`. Needs
    /// a writable backend and an adjudicator.
    Planner,
    /// §8.4's `researcher` with the web instead of `search_corpus`. Four of its
    /// seven seats refuse today for want of a backend; the two network ones still
    /// declare `Access::Network`, so it needs an adjudicator to be honest.
    Researcher,
    /// §8.4's `coder` without `bash`: `read`, `write`, `edit`, `grep`, `glob`
    /// `read_spill`. Writable backend.
    Coder,
    /// The only role that can run a command: the job verbs, `monitor`, and - only
    /// behind [`Config::allow_bash`] - `bash`. Confined backend, never
    /// `HostBackend::executable`.
    Runner,
    /// leticode's seat: opencode's tool union by opencode's names, `task` included, plus `bash`
    /// behind [`Config::allow_bash`]. Unconfined, on purpose - opencode has no workspace
    /// boundary, so the permission ruleset and the mode, not a jail, are the gate.
    Leticode,
    /// The merge queue's reviewer ([`letibot_tools::runtime::roles::gatekeeper`]): the read
    /// tools and, behind [`Config::allow_bash`], `bash` for `git diff`. Nothing that can author
    /// code, and never a seat a person picks - the daemon starts it (`mergequeue::GatekeeperDoor`).
    Gatekeeper,
    /// The standing notes' keeper ([`letibot_tools::runtime::roles::notes_keeper`]): the read
    /// tools and `notes`, the one writer that reaches a note. Never a seat a person picks - the
    /// daemon starts it, and the per-workspace clock that will is still deferred.
    NotesKeeper,
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
            Seat::Gatekeeper => letibot_sessionlog::GATEKEEPER_SEAT,
            Seat::NotesKeeper => "notes_keeper",
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
            letibot_sessionlog::GATEKEEPER_SEAT => Ok(Seat::Gatekeeper),
            "notes_keeper" | "keeper" => Ok(Seat::NotesKeeper),
            other => Err(format!(
                "unknown role `{other}`; this build has orchestrator, planner, \
                 researcher, coder, runner, leticode, gatekeeper, notes_keeper"
            )),
        }
    }

    /// Whether this seat needs a backend that can change the operator's tree. Read off the seat
    /// so the two cannot disagree; the *authoritative* answer is still the seated schemas, and
    /// this decides which constructor to call before they exist.
    pub fn needs_writable_backend(self) -> bool {
        matches!(
            self,
            // `NotesKeeper` is here for its one writer: `notes` declares `Access::Write`, and a
            // keeper that cannot repair only complains. Its SEAT keeps it from authoring code.
            Seat::Planner | Seat::Coder | Seat::Runner | Seat::Leticode | Seat::NotesKeeper
        )
    }

    /// Whether this seat needs a backend that can start processes, and therefore
    /// the boundary that goes around one.
    pub fn needs_exec_backend(self) -> bool {
        // Coder joined Runner on 2026-09-11: a role that writes code and cannot run it leaves
        // the loop open where it would have paid. `HostBackend::confined` is writable + EXEC, so this
        // adds a project-rooted boundary, not file access - and it does NOT seat `bash`.
        matches!(self, Seat::Runner | Seat::Coder | Seat::Gatekeeper)
    }

    /// The read-only grants this seat needs beyond the project. Empty here and filled from
    /// `--grant-ro`: a grant is the operator's decision, and `Grant` carries a `why` so it can be
    /// explained and revoked.
    pub fn default_grants(self) -> Vec<std::path::PathBuf> {
        Vec::new()
    }
}

/// How the daemon reaches the flowy fabric. `None` is no flowy at all - the mode
/// `docs/tool-design-brief.md` §3b keeps - and it is the default. What is missing is looked up
/// along the usual path, and the banner says where each value came from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FlowyConfig {
    /// The seat's name. Omitted: `$FLOWY_AGENT`, else the only seat on the box.
    pub seat: Option<String>,
    pub addr: Option<String>,
    pub token_file: Option<PathBuf>,
    /// Declare the inbox reader at the head of the log before listening. OFF by default and
    /// never implied: a reader that silently appears is a typo producing a permanently empty
    /// inbox - see `letibot_flowy::client::NodeError`.
    pub new_reader: bool,
}

/// A cloud provider for the turns - D10's mode 3. `None` is the local server
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
    /// This provider's context window, from the catalogue - the same models.dev number
    /// `Harness::retune_window` moves to on `/models`, so a session that STARTS on a provider plans
    /// against the same wall. `None` when the catalogue has no number, which is honest, not large.
    pub fn catalogue_window(&self, cat: &letibot_provider::catalogue::Catalogue) -> Option<u64> {
        letibot_provider::Preset::parse(&self.name)
            .ok()?
            .window(self.model.as_deref(), cat)
    }
}

/// Where the model that answers came from, as the startup disclosure says it: four levels
/// plus the file and any fault found. A value on the config rather than a sentence composed
/// where the model is set, so the disclosure cannot go missing exactly when it was surprising.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ModelSource {
    /// The level that won. [`crate::leticode_config::Level::Builtin`] is the
    /// default, which is the honest answer for a daemon nothing spoke for.
    pub level: crate::leticode_config::Level,
    /// The name that level wrote, when a level wrote one. `None` for the built-in
    /// default, which is not a name somebody typed - it is the model the daemon
    /// was launched on.
    pub value: Option<String>,
    /// The file, when a file is where it came from.
    pub file: Option<PathBuf>,
    /// What the level was, in a phrase: `--provider deepseek`, `[default] in
    /// providers.toml`, `the local server this daemon was launched against`. The
    /// short form of the same fact the level names, for the sentence.
    pub origin: String,
    /// A fault found at startup: a value naming a model this box cannot reach, or an alias
    /// the command line's binding outranks. `None` is the quiet case. Carried here so the banner
    /// says it before the first turn rather than a 400 discovering it.
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

    /// The precedence, in the words a reader gets, so the rule is on the
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
    /// The skills a session is STARTED with - their bodies preloaded into its prompt on top
    /// of the index every session gets. Declared, never inferred: a `task {skills}` child's
    /// spec fills it, and a child that names none inherits its parent's list at the spawn.
    pub skills: Vec<String>,
    /// The reviewer's declared list, from `[queue] reviewer_skills`: read by the daemon and
    /// handed to the review queue's session as its `skills`, so every reviewer it starts carries
    /// them. Empty is the ordinary case.
    pub reviewer_skills: Vec<String>,
    /// The alias the server reports. Recorded in `turn_metrics` and in the store
    /// several models may share one dialect.
    pub model: String,
    pub endpoint: Endpoint,
    /// The server's context window, in tokens - the wall compaction exists for. From `/props` at
    /// startup, overridable with `--context-window`; `None` when nothing said, because a window
    /// that is not known must not be invented.
    pub context_window: Option<u64>,
    /// The token an image goes where it sits, from `/props` (`serving::served_media_marker`).
    /// Fetched because the server randomises it per process, so it is a fact about the running
    /// server like `n_ctx` - and `None` here means no images on this endpoint, never a guess.
    pub media_marker: Option<String>,
    /// Compact automatically when a turn leaves less than [`Config::headroom`] of the window free -
    /// *compact when you must, as late as possible*. It gates the PRE-EMPTIVE question only: a
    /// refusal is answered through [`Config::at_the_wall`], deliberately without it.
    pub auto_compact: bool,
    /// What this session does at its wall - the wall controller's state and the last count a
    /// provider measured (`crate::wall`). Replaced `auto_compact_stood_down`, the latch that
    /// judged a fold on an estimate and never released (`s-1789919514688401228`).
    pub wall: crate::wall::WallControl,
    /// The operator's pause of this session's idle plan-check (`/nag pause`), or `None`. On
    /// the config because the store row is written from it and read back at open. Never
    /// inherited: the open overwrites it from the child's OWN row.
    pub nags_paused: Option<letibot_sessionlog::NagPause>,
    /// The GGUF the vocabulary is read from - the first shard for a split model. `None` is a
    /// daemon for a cloud provider, whose ledger uses the byte vocabulary
    /// (`letibot_tokencore::Vocab::bytes`).
    pub vocab_gguf: Option<PathBuf>,
    /// The root every read-only tool is confined to.
    pub workspace: PathBuf,
    pub socket: PathBuf,
    /// `None` keeps the transcript in memory only - usable, and honest about it.
    pub store: Option<PathBuf>,
    pub session_id: String,
    /// The session that spawned this one as a subagent, or `None` for a top-level
    /// session. Recorded into the store so a subagent tree is a fact on disk, not an
    /// id convention a picker has to reverse-engineer.
    pub parent_session_id: Option<String>,
    /// How deep in a subagent tree this session sits - zero for a root. A config fact, not a
    /// seat fact, because the seat table can express two depths and the ruling wants a configured
    /// number between. Incremented once, at spawn, and read where `task` is seated.
    pub depth: u32,
    /// Where an HTTP head listens, or `None` for the ordinary socket-only
    /// daemon. `--http HOST:PORT`. See [`crate::httphead`].
    pub http: Option<String>,
    /// What the last metered turn cost in each side's tokens, as `(ledger, provider)`. The
    /// ledger counts a rendered prompt with its reasoning blocks; a messages provider is sent
    /// neither, so the ledger ran 1.4-1.8x high - the more a model thinks, the earlier it compacted.
    pub ledger_scale: Option<(u64, u64)>,
    /// The old pair, kept across a switch as an ESTIMATE - `None` until a session moves off
    /// the model it measured on, and cleared by any round on the new one. See [`CarriedScale`].
    pub carried_scale: Option<CarriedScale>,
    /// Whether this session's backend is rooted at `/` - opencode parity: the permission ruleset
    /// not a jail, is the gate. Set for [`Seat::Leticode`] and inherited by its subagents
    /// which re-seat to [`Seat::Coder`] without being re-confined.
    pub unconfined: bool,
    /// A human name for the session, or empty - and then a head shows the id. Deliberately
    /// not derived from the first prompt: a title guessed from content renames itself under
    /// you, and a picker whose rows rename themselves is a picker you cannot learn.
    pub title: String,
    pub owner: String,
    /// The bootstrap system prompt - message 0 of the stable prefix, and nothing volatile belongs
    /// in it (§5.2). Composed once, at session open ([`Config::compose_for_seat`]); only a fork
    /// re-reads the notes files, because a fork is already a cold prefill.
    pub system: String,
    /// Compose [`Config::system`] at open, for the tools this session seats. `Some` for every
    /// session the daemon opens; `None` keeps `system` exactly as given, which is a test's fixture
    /// or the operator's `--system`.
    pub seat_prompt: Option<SeatPrompt>,
    /// What the prompt was composed from, set at open from the seated tools -
    /// `None` before. The fabric's later refreshes render for the same doors.
    pub seat_facts: Option<SeatFacts>,
    /// The operator's `prompts.toml`, loaded once at startup. `Default` (no
    /// overrides) when the file is absent or was refused - which is also the
    /// byte-identical case. See [`Prompts`].
    pub prompts: Prompts,
    /// The project's `leticode.toml`, discovered by walking up from the workspace, with the user
    /// file already merged under it. Models are configuration; permissions are not: this carries
    /// model names and nothing else, so a file cannot widen a capability.
    pub leticode: crate::leticode_config::LeticodeConfig,
    /// The user file alone, `~/.config/letibot/leticode.toml` - because [`Config::leticode`]
    /// is the merge, and a disclosure naming only the merge could not say which file set a key.
    /// This one is for the screen; the session reads the merge.
    pub leticode_user: crate::leticode_config::LeticodeConfig,
    /// Whether a session reads its project's `leticode.toml` - `false` under
    /// `--no-project-config`. Carried because one key is read per SESSION, `[memory]`, whose
    /// project is the session's own workspace ([`crate::leticode_config::memory_policy`]).
    pub read_project_config: bool,
    /// The box's own directories: where its instruction file and its box-wide notes live. Read
    /// once from `$HOME` by [`Config::read_the_box`] and empty in a fixture, so a test's prompt is
    /// a function of its own files and not of whose home the suite runs in.
    pub box_dirs: crate::leticode_config::BoxDirs,
    /// The box's own skill roots (the operator's, the shared install, the data dir a model
    /// writes into), filled by [`Config::read_the_box`] and empty in a fixture. The project's two
    /// conventions are the session's workspace instead: see [`Self::skill_dirs`].
    pub box_skills: letibot_tools::builtins::skill::SkillDirs,
    /// Where the model that answers came from - the level, the file, and any fault found. An
    /// answer that lives in a file the operator has to go and read is not an answer. See
    /// [`ModelSource`].
    pub model_source: ModelSource,
    /// `low` / `medium` / `high` / `xhigh`, interpreted per dialect. It is prefix
    /// bytes, so changing it mid-session re-prefills everything.
    pub effort: Option<String>,
    pub sampling: Value,
    /// Where this project sits, as a named point ([`letibot_tools::mode::Mode`]). Read from
    /// the per-project store, never chosen here. A separate field from [`Config::seat`] because a
    /// role says which tools are seated and a mode says what each one costs to approve.
    pub mode: letibot_tools::mode::Mode,
    /// The permission ruleset (allow/deny/ask per tool and pattern): the shipped list, then
    /// `~/.config/letibot/permission.json`, then `LETIBOT_PERMISSION`. A subagent inherits it, so
    /// its calls are governed by the same rules as its parent's.
    pub permission: letibot_tools::permission::Ruleset,
    /// The flowy seat this daemon holds, when asked to. See [`FlowyConfig`].
    pub flowy: Option<FlowyConfig>,
    /// What this session is denied below its role - a subagent's downgrade
    /// inherited by every subagent it spawns and only ever added to. `none` for
    /// a session the operator opened. See `letibot_tools::schema::Downgrade`.
    pub downgrade: letibot_tools::schema::Downgrade,
    /// Which search provider is behind `web_search`, by name (`brave`), or `None` for the tool
    /// that refuses. The key is deliberately not here: it is resolved at attach, so a secret
    /// never rides in a struct that derives `Debug`.
    pub web_search: Option<String>,
    /// Whether `curl` egress is attached behind `web_fetch`. Off by default: a search provider is
    /// opted into by a key the operator placed in a file, while a page fetch reads whatever the
    /// address names - the wider door - so it waits for the flag.
    pub web_fetch: bool,
    /// Where this session's tools run: the host (its own boundary), or a firecode VM - a
    /// subagent's `where`. `Host` for every session the operator opened.
    pub placement: letibot_tools::builtins::task::Placement,
    /// Extra arguments for `firecode up` when the placement is a VM - the
    /// operator's `--vm-arg`, verbatim, inherited by every subagent placed in a
    /// VM under this session.
    pub vm_args: Vec<String>,
    /// The tree a `firecode` placement copies, when the caller that KNOWS names one - the ONE
    /// repository the work is in. `None` is [`Config::vm_copy_source`]'s resolution, because a session
    /// is seated where a person put it, which is not always a repository.
    pub vm_source: Option<PathBuf>,
    /// The VM this session runs in when it is not its own - the project of an ancestor's
    /// firecode VM, because *"children live in the same vm"*. Set by the spawn; `Harness::open`
    /// attaches rather than boots (`FirecodeBackend::attach`).
    pub vm_attach: Option<PathBuf>,
    /// The acceptance check this session's VM runs when it is brought down -
    /// [`letibot_tools::builtins::task::TaskSpec::vm_verify`], applied at the spawn. Not inherited
    /// it is set from each spawn's own spec, so a child's gate is never its parent's by accident.
    pub vm_verify: Option<String>,
    /// A VM session works in its workspace itself, mounted read-write (`firecode up
    /// --mount-rw`) - on a Mac, where firecode can mount. Set by `--where firecode`; `--vm-copy`
    /// turns it off. Never inherited by a child that gets a VM of its own (the spawn clears it).
    pub vm_mount: bool,
    /// Pairs every command this session runs carries, set at a spawn and read into the exec host's
    /// standing environment. Today one: a worktree child's `CARGO_TARGET_DIR`, so it shares the main
    /// tree's build cache - an env var, where a written `.cargo/config.toml` dirtied the tree.
    pub build_env: Vec<(String, String)>,
    /// Where the review queue's reviewers run - the one child whose placement is decided FOR it
    /// (`crate::sessions::gatekeeper::reviewer_placement`). `Host` is the default and the operator's
    /// ruling: a fixture must ask by name ([`Config::allow_vm_reviewers`]) to get a VM.
    pub reviewers: Reviewers,
    /// Where this daemon's merge gate runs - `[queue] gate_host`, `--gate-host`, and
    /// [`crate::mergequeue::gate::gate_host`]. `None` is this machine, which is what a fixture must
    /// get; the daemon is the one caller that reads the file.
    pub gate_host: Option<String>,
    /// The cloud provider the turns go to, when not the local server.
    pub provider: Option<ProviderConfig>,
    /// What to try when the model this session is on is OUT - `[fallback] models` in
    /// `providers.toml`, in the order written; empty is the old behaviour. Read once at startup
    /// like `[default]`, so an edit takes effect on the next daemon.
    pub fallback: Vec<String>,
    /// What the fabric block in the system prompt is, said by whoever composed
    /// it (`Sessions`): live, cached with its age, or unreachable. `None` when
    /// there is no seat, and then there is no block.
    pub fabric: Option<String>,
    /// Paths bound READ-ONLY into the confined view, from `--grant-ro`. The boundary is hermetic, so a
    /// toolchain under `$HOME` is ABSENT rather than denied - which is why `cargo` needs one of these.
    /// Read-only means readable into the transcript, so each is a decision with that consequence.
    pub grants_ro: Vec<std::path::PathBuf>,
    /// Where the merge queue's landing worktrees live - the daemon's own
    /// (`mergequeue::git::landing_root`); `None` in every fixture built with
    /// [`Config::for_this_box`], so no test reaches the box's real `~/.local/share/letibot`.
    pub landing_root: Option<std::path::PathBuf>,
    /// Where a review's own checkout is made - `<temp>/letibot-review-clones` on the daemon, a
    /// fixture's own directory when it drives reviews. One tree per ATTEMPT, so a re-seat can never
    /// touch the tree its previous reviewer is reading. `None` in every other fixture.
    pub review_clones: Option<std::path::PathBuf>,
    /// Whether this daemon runs the merge and review queues - `true` unless `--no-queue`. Wrong
    /// for a package worker: on 2026-10-26 one took its repository's lease, found `main` red
    /// and put an EMERGENCY in front of a model that had never been asked about that tree.
    pub serve_queues: bool,
    /// This daemon is the box's ROOM COORDINATOR (`--room`) - a whole `harnessd` whose only
    /// job is the room: its own socket, data directory, no project, no store, and one session
    /// (`s-room-<host key>`) that arbitrates the queues. Set by `--room` and by nothing else.
    pub room: bool,
    /// Whether this daemon's sessions may join the box's room. `true` on the daemon's own
    /// path, `false` in every fixture - the same rule `landing_root` follows, and for the same
    /// reason: a fixture on a box where the operator runs a coordinator must not post into it.
    pub room_link: bool,
    /// Whether this daemon may ask the room's supervisor to restart the coordinator. `true` on
    /// the daemon's own path and `false` on the coordinator's, because the arbiter's pane is
    /// drawn by the process a restart would kill. `false` in every fixture too.
    pub restart_room: bool,
    /// Where the daemon's own logs live - `<data dir>/logs`, one directory per session. Set by
    /// [`Config::for_the_daemon`]; `None` in every fixture. They were in `<workspace>/.letibot/`
    /// and a child's workspace is its worktree, so every finished child's tree looked dirty.
    pub logs_root: Option<std::path::PathBuf>,
    /// Which role this session seats. [`Seat::Orchestrator`] by default, which
    /// is what every invocation gets today. See [`Seat`].
    pub seat: Seat,
    /// `bash` stays off even behind [`Seat::Runner`], and this is the flag. A named hole, not
    /// caution: §3's invariant is that secret bytes never enter the transcript, the choke point
    /// that would enforce it does not exist yet, and `bash`'s result is an arbitrary byte stream.
    pub allow_bash: bool,
    /// How deep a subagent tree may go. The operator's ruling, 2026-10-03: *"subagents are
    /// absolutely allowed to spawn subagents up to configured nesting level."* Default 3. The seat
    /// names `task`; the limit is enforced at the call and refused by name, not by hiding the tool.
    pub max_subagent_depth: u32,
    /// How long a finished subagent with nothing left to do keeps its thread - ; `None`
    /// keeps it for the daemon's life. Parked as store-only rather than reaped, because a child is
    /// a session and must stay answerable: ~12 MB and two threads each, reopened by the next prompt.
    pub child_park_after: Option<std::time::Duration>,
    /// The starvation cap on held results : a result nothing in the plan waits on is held
    /// for the next turn boundary, and a turn that runs longer than this gets the batch at its next
    /// round boundary anyway. Default [`crate::delivery::HELD_RESULTS_CAP`], the operator's number.
    pub held_results_cap: std::time::Duration,
    /// How long a daemon may have been down for a session's work to be restored on its own
    /// (`crate::restart`). Under it, subagents are resumed and monitors re-armed; over it, the model is
    /// reminded instead. Default [`crate::restart::RESTORE_WITHIN`], one hour.
    pub restore_within: std::time::Duration,
    /// How long a daemon with nothing to do stays up - no head, no turn, no job, no live subagent
    /// no monitor, no queue work - and then it exits (`Sessions::idle_exit_due`). The operator found
    /// seven of them, days old, each holding a replaced build. Default one hour; `0` keeps it up.
    pub idle_exit_after: Option<std::time::Duration>,
    /// Whether this daemon is a one-shot's - started for `letibot "a question"`. It changes one
    /// thing: `bash` refuses `background: true`, because no later head and no daemon would be watching
    /// when the result arrives. `cli.rs` sets it from the steps it was handed.
    pub one_shot: bool,
    /// Who decides a gated call. See [`AdjudicatorChoice`].
    pub adjudicator: AdjudicatorChoice,
    /// Where layer B lives, `HOST:PORT` speaking llama.cpp's `/completion`.
    /// Required by [`AdjudicatorChoice::Model`] and meaningless without it.
    pub oracle: Option<Endpoint>,
    /// Which model answers at that endpoint, when it is not the one doing the work.
    /// `[gatekeeper] model` in providers.toml. `None` means *the session's own model*, which stopped
    /// being right the moment the guard moved to another box: the banner announced the wrong name.
    pub oracle_model: Option<String>,
    /// The authority the operator has declared for their guard, from
    /// `[gatekeeper] intents` / `max_scope` in providers.toml. `None` is the
    /// built-in floor, which is what a box that has said nothing gets.
    pub oracle_scope: Option<letibot_tools::authorise::OracleScope>,
    /// The memory cap a model's tool call runs under, and the most it may take without the
    /// operator - `providers.toml`'s `[memory]`. Built-in: 64 MiB, and half of
    /// what is free.
    pub memory: letibot_tools::exec::memory::MemoryPolicy,
    /// Layer B's latency budget. The trait's default is 400ms and
    /// `ModelAdjudicator` abandons an oracle that overruns, so this is the knob
    /// that decides whether a given model can hold the seat at all.
    pub oracle_budget: std::time::Duration,
    /// How many output tokens the guard may spend on its answer. `None` is
    /// [`crate::oracle::DEFAULT_MAX_TOKENS`]. It exists for the reply cut off before a verdict
    /// (`UnsureKind::OutOfRoom`), which says to raise this - and raising it costs latency.
    pub oracle_max_tokens: Option<usize>,
    /// What the guard is asked: a verdict, or two scores the thresholds turn
    /// into one. See `crate::oracle::Question`.
    pub oracle_question: crate::oracle::Question,
    /// Start with the guard model consulted on every gated call. Only a starting value
    /// `/supervise` moves it at run time, which is why it is a flag on the gate rather than a mode.
    /// Meaningless without `oracle`, and the gate says so by name rather than pretending.
    pub supervise: bool,
    /// Whether the intent check reads the assistant's prose as well as its tools. Off by default
    /// the tool half cannot fire unless a role seating `todo`/`goal` is in use, while the prose half
    /// fires on any turn that ran no tools - the false positive that turns the check off.
    pub intent_prose: bool,
    pub spill: SpillPolicy,
    pub spill_storage: SpillStorage,
    /// The backstop, and only the backstop. How many rounds one user turn may take before the
    /// daemon stops and says so; `0` is unbounded, and it is the default, because a round count
    /// measures effort and not progress. [`Config::stall_rounds`] is the closed-loop half.
    pub max_tool_rounds: usize,
    /// How many times a round is RE-attempted when the model endpoint fails, after the first try
    /// `0` is none. The count is the ladder's length, not its rule - *what* is retryable lives in
    /// `letibot_http::retry::Policy`, so raising this is patience. Retries, not attempts.
    pub http_retries: u32,
    /// How many consecutive rounds may produce nothing new before the turn stops. A round produces
    /// nothing new when no call returned an `Ok` result this turn had not already seen
    /// ([`crate::progress`]); `0` turns the check off and leaves the round count as the only stop.
    pub stall_rounds: usize,
}

/// Who decides a gated call. A gate that says "allowed" because nothing is wired is worse than no
/// gate, and one that refuses everything because nobody attached fails every call - so there is no
/// value that means nobody. See [`AdjudicatorChoice::parse`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AdjudicatorChoice {
    /// [`crate::oracle::HttpOracle`] behind [`letibot_tools::ModelAdjudicator`]. The only choice
    /// that can satisfy [`letibot_tools::mode::Prereq::Oracle`], and therefore the only one under
    /// which `automode` opens. It needs `--oracle HOST:PORT`, and `parse` refuses by name without it.
    Model,
    /// [`crate::answers::HeadAdjudicator`] over the session socket - the default for any seat that
    /// can reach the gate, because a daemon in the background with a head attached is the shape people
    /// run. A session with no head refuses at once, naming that fact, not sitting out a deadline.
    #[default]
    Head,
    /// [`letibot_tools::ConsoleAdjudicator`] over the daemon's own stdin/stderr. Right for
    /// `harnessd --prompt …` and for a daemon run in a foreground terminal, and only those: a
    /// head attached over the socket cannot answer it, which is D25.
    Console,
}

impl AdjudicatorChoice {
    /// The two values this refuses are the interesting part. `none` would be a session that seats
    /// `write` and refuses every call, spelled `--role orchestrator` instead; `model` has the
    /// concept and no oracle behind it, which is the fact worth saying.
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
            // Wired as of the HttpOracle. It still needs `--oracle HOST:PORT`
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

/// The base system prompt, composed per seat. Every sentence is true for the seat that
/// reads it: a section is a function of [`SeatFacts`], and one with nothing true to say is left out.
/// Nothing volatile beyond the scratch path - one added `<env>` line once cost a full re-prefill.
pub const SECTION_NAMES: &[&str] = &[
    "identity",
    "language",
    "tree",
    "find_and_read",
    "edit_files",
    "scratch",
    "notes",
    "working",
    "tone",
];

/// What a session's base prompt is composed from - the facts of its seat that the prompt has
/// sentences about. Read at open (`Harness::open_with_registry`), after the role is resolved and
/// the downgrade applied, so `tools` is what the model is actually given.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeatFacts {
    /// The names of the seated tools.
    pub tools: std::collections::BTreeSet<String>,
    /// The scratch directory the backend reaches, when it has one.
    pub scratch: Option<String>,
    /// A subagent: its last reply is its parent's answer, not the operator's.
    pub subagent: bool,
    /// The workspace is a linked git worktree, whose branch the merge queue lands.
    pub worktree: bool,
}

impl SeatFacts {
    /// Facts naming these tools and nothing else.
    pub fn with_tools<I, S>(tools: I) -> SeatFacts
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        SeatFacts {
            tools: tools.into_iter().map(Into::into).collect(),
            ..SeatFacts::default()
        }
    }

    /// Whether `tool` is seated.
    pub fn has(&self, tool: &str) -> bool {
        self.tools.contains(tool)
    }

    /// The doors the fabric block may name for this seat.
    pub fn fabric_reach(&self) -> letibot_flowy::FabricReach {
        letibot_flowy::FabricReach {
            skill: self.has("skill"),
            bash: self.has("bash"),
        }
    }
}

/// `a`, `a or b`, `a, b or c` - the seated subset of a list, spelled for a sentence.
fn either(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => (*one).to_string(),
        [init @ .., last] => format!("{} or {last}", init.join(", ")),
    }
}

/// The built-in text of one section for one seat, or `None` when the section has
/// nothing true to say there. See [`SECTION_NAMES`].
pub fn default_section(name: &str, f: &SeatFacts) -> Option<String> {
    let any_tool = !f.tools.is_empty();
    match name {
        "identity" => Some(
            "You are a careful software engineering assistant working in a checked-out source \
             tree."
                .into(),
        ),
        "language" => Some(
            "Answer in English unless the user writes in another language, in which case answer \
             in theirs."
                .into(),
        ),
        "tree" => {
            let mut s = String::new();
            if f.has("read") || f.has("grep") || f.has("glob") {
                s.push_str(
                    "Use your tools for questions about **this tree** — its files, their \
                     contents, where something is defined — rather than guessing: a file you \
                     have not read is a file you do not know. ",
                );
            }
            if f.has("transcript") {
                s.push_str(
                    "If the user refers to something as already settled and you have no record \
                     of it, read it with `transcript` before asking them to repeat it: a \
                     compaction replaced those turns with a summary, and the turns are still in \
                     the store. ",
                );
            }
            let web: Vec<&str> = ["`web_search`", "`web_fetch`"]
                .into_iter()
                .filter(|t| f.has(t.trim_matches('`')))
                .collect();
            if web.is_empty() {
                s.push_str(
                    "Answer questions about the world, definitions and arithmetic directly, \
                     without a tool.",
                );
            } else {
                s.push_str(&format!(
                    "Answer questions about the world, definitions and arithmetic directly; use \
                     {} when the answer depends on current facts.",
                    either(&web)
                ));
            }
            if any_tool {
                s.push_str(" When a tool finds nothing, say so; do not fill the gap from memory.");
            }
            Some(s)
        }
        "find_and_read" => (f.has("grep") && f.has("read")).then(|| {
            format!(
                "Find with `grep` and read with `read`{}. `grep` takes a `context` count and \
                 returns the numbered region around each match; `read` takes a `ranges` list for \
                 several windows of one file. To show the user lines of a file, cite them rather \
                 than copying them: an empty code fence labelled with the path and the line range \
                 (```src/main.rs:12-30, then the closing ```, nothing between) is filled with \
                 those lines, numbered as the file numbers them.",
                if f.has("bash") {
                    ", not by piping `grep -n` into `sed`"
                } else {
                    ""
                }
            )
        }),
        "edit_files" => f.has("edit").then(|| {
            format!(
                "Change files with `edit`{}{}. `edit` takes an `edits` list: several changes to \
                 one file, applied in order, all or nothing.",
                if f.has("write") { " and `write`" } else { "" },
                if f.has("bash") {
                    ", never with a script piped into a shell, which leaves the operator no diff"
                } else {
                    ""
                }
            )
        }),
        "scratch" => match &f.scratch {
            Some(path) if f.has("write") => Some(format!(
                "Your scratch directory is `{path}`. Put working files there — a generated \
                 script, a fetched page, intermediate output — rather than in the workspace or a \
                 directory you make under /tmp. Creating, writing and deleting inside it need no \
                 permission."
            )),
            _ => None,
        },
        "notes" => f.has("notes").then(|| {
            "Keep standing notes with the `notes` tool: markdown in `.letibot/notes/` that the \
             harness reads into this prompt, so a note outlives a compaction. Write one for \
             something interesting, remarkable or surprising that you would not want to \
             rediscover — not as a summary of what you did. `AGENTS.md` and the box-wide notes \
             are the operator's: read them, do not edit them. A note says what was true or \
             intended when it was written: check it against the tree before taking it as a fact \
             about now, and replace one that has gone stale."
                .to_string()
        }),
        "working" => Some(working(f)),
        "tone" => Some("Be direct. Prefer the shortest answer that is complete.".into()),
        _ => None,
    }
}

/// The `working` section: how results arrive, asking, context, stopping, the queue.
fn working(f: &SeatFacts) -> String {
    let mut rules: Vec<String> = Vec::new();
    // A background job is a thing the seat can follow only with the job verbs: `bash`
    // without `job_output` (a coder child, the gatekeeper) has nothing to wait for.
    let jobs = f.has("bash") && f.has("job_output");
    let children = f.has("task") || f.has("task_start");
    if jobs || children {
        let what = match (jobs, children) {
            (true, true) => "A background job's or a subagent's",
            (true, false) => "A background job's",
            _ => "A subagent's",
        };
        let at_once = if f.has("todo_write") {
            "if a todo row needs it or wake is \"now\""
        } else {
            "if wake is \"now\""
        };
        rules.push(format!(
            "{what} completion reaches you automatically — when your current turn ends, or at \
             once {at_once}. Do not poll or sleep waiting for it."
        ));
    }
    match (f.has("ask_user_question"), f.has("todo_write")) {
        (true, true) => rules.push(
            "`ask_user_question` blocks until the operator answers: use it when nothing else can \
             go on without them. When only one step waits on their decision, put a `wait_on` \
             with an `operator` question on that `todo_write` row and carry on with the rest of \
             the plan."
                .into(),
        ),
        (true, false) => rules.push(
            "`ask_user_question` blocks until the operator answers: use it when you cannot go on \
             without their decision."
                .into(),
        ),
        (false, true) => rules.push(
            "When a step waits on a decision only the operator can make, put a `wait_on` with an \
             `operator` question on that `todo_write` row and carry on with the rest of the plan."
                .into(),
        ),
        (false, false) => {}
    }
    rules.push(
        "The harness manages your context and compacts it when it must. Do not size, split or \
         stop work by how much context you think is left."
            .into(),
    );
    if f.subagent {
        rules.push(
            "You are a subagent: your last reply is the answer the session that started you \
             receives. Keep working until the task is done or you are blocked, then reply with \
             what you did and found, or what blocks you."
                .into(),
        );
    } else {
        let mut waits: Vec<&str> = Vec::new();
        if jobs {
            waits.push("a job");
        }
        if children {
            waits.push("a subagent");
        }
        waits.push("the operator");
        rules.push(format!(
            "Keep working while there is something you can do. When everything left waits on {}, \
             end your turn and say in one line what it waits on. Do not restate status in place \
             of acting.",
            either(&waits)
        ));
    }
    if f.worktree {
        rules.push(
            "You work on a worktree branch: commit your work there. The merge queue lands it; \
             never push or merge it yourself."
                .into(),
        );
    } else if f.has("task_start") {
        rules.push(
            "A worktree branch is landed by the merge queue; never push or merge one yourself."
                .into(),
        );
    }
    let mut s = String::from("How work runs here:");
    for r in rules {
        s.push_str("\n- ");
        s.push_str(&r);
    }
    s
}

/// The built-in prompt for one seat, with no `prompts.toml`. What
/// [`Prompts::compose`] returns when nothing overrides anything.
pub fn default_system(f: &SeatFacts) -> String {
    Prompts::default().compose("", f)
}

/// What only some sessions are told, composed at open with the rest - see
/// [`Config::seat_prompt`].
#[derive(Debug, Clone, Default)]
pub struct SeatPrompt {
    /// The paragraph only this session is given (the review queue's), appended after
    /// the standing notes. `None` for every session the operator started.
    pub own: Option<String>,
    /// The fabric block, for a root session holding a seat - rendered at open, when
    /// the session's tools are known, so it names only the doors it has.
    pub fabric: Option<FabricPart>,
}

/// The fabric as it was read for a session, before it is rendered for the session's tools.
#[derive(Debug, Clone)]
pub enum FabricPart {
    /// Read from the node, or from the cached copy - `source` says which.
    Read {
        ctx: letibot_flowy::FabricContext,
        source: letibot_flowy::FabricSource,
    },
    /// No node and no copy: the block that says so, which names no door.
    Unreachable(String),
}

impl FabricPart {
    /// The block, naming only the doors in `reach`.
    pub fn render(&self, reach: letibot_flowy::FabricReach) -> String {
        match self {
            FabricPart::Read { ctx, source } => ctx.render(source, reach),
            FabricPart::Unreachable(block) => block.clone(),
        }
    }
}

/// The per-model system-prompt overrides, from `prompts.toml` - a separate file from
/// `providers.toml` (mode 600, rewritten by `/models NAME`) because prompt text is not a secret. Its
/// keys are [`SECTION_NAMES`], each replacing that one section in place; `system_extra` adds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Prompts {
    /// The `[base]` section overrides, keyed by section name. Empty overrides are dropped on
    /// load: an empty string would replace a section with nothing, and keeping it would let an
    /// empty exact block shadow the provider glob it should fall through to.
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

/// The file's shape, as parsed. `deny_unknown_fields` on the top level is the report for a
/// section this daemon does not know - a `[foo]` is a parse error, not a silently ignored line.
/// The keys INSIDE a layer are captured by `flatten` and checked in [`Prompts::load`].
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PromptsFile {
    #[serde(default)]
    base: Option<BaseSection>,
    #[serde(default)]
    model: BTreeMap<String, ModelSection>,
}

/// The `[base]` layer: section overrides plus the `system_extra` appended for every model.
/// `flatten` captures the section keys so [`Prompts::load`] can refuse an unknown name by name
/// naming `system_extra` is what stops it being mistaken for one.
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

    /// Load `prompts.toml`. A missing file is `Ok` with no overrides - the byte-identical case, not
    /// an error. A file that does not parse, or that names a section this daemon does not know, is
    /// `Err` with the parser's own message (or the section's name) and the path.
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

    /// The composed system prompt for one model on one seat: the sections that have something true to
    /// say for `seat`, each replaced in place by the model's override, else the provider glob's, else
    /// `[base]`'s, then `system_extra` last. With no file it is `default_system` byte for byte.
    pub fn compose(&self, model_name: &str, seat: &SeatFacts) -> String {
        // The exact block and the provider-glob block, if present: the exact name wins per
        // section, the glob is the fallback. One lookup for the sections and the `system_extra`
        // alike - two matchers for one rule is the drift this tree deletes.
        let exact = self.models.get(model_name);
        let glob = model_name
            .split_once('/')
            .map(|(provider, _)| format!("{provider}/*"))
            .and_then(|g| self.models.get(&g));

        let sections: Vec<String> = SECTION_NAMES
            .iter()
            .filter_map(|name| {
                let default = default_section(name, seat)?;
                Some(
                    exact
                        .and_then(|m| m.sections.get(*name))
                        .or_else(|| glob.and_then(|m| m.sections.get(*name)))
                        .or_else(|| self.base.get(*name))
                        .cloned()
                        .unwrap_or(default),
                )
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

/// Whether `name` is a section of the base prompt, per [`SECTION_NAMES`].
fn is_known_section(name: &str) -> bool {
    SECTION_NAMES.contains(&name)
}

/// A sixteenth of `w`, floored at 2k and capped at a quarter - see
/// [`Config::headroom`] for why each bound is there. One spelling, because the
/// planning window and the shown one must reserve by the same rule.
fn headroom_of(w: u64) -> u64 {
    (w / 16).max(2048).min(w / 4)
}

/// The last ratio this session measured, on a model it has since switched away from. A switch
/// clears `Config::ledger_scale`, rightly, but the cleared count then refused a turn before it left
/// the machine. Kept as an estimate, named as one, and never a pre-emptive fold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CarriedScale {
    /// The model the pair was measured on, in `/models`' own spelling -
    /// `deepseek/deepseek-flash`. Named in every line that uses it.
    pub model: String,
    /// `(ledger tokens, that model's prompt tokens)` for ONE prompt.
    pub ledger: u64,
    pub provider: u64,
}

/// What a ledger count's relation to the answering model's count is known from - the provenance
/// a wall decision turns on, in one value rather than three predicates that have to agree. What
/// matters at the wall is not *is it a measurement* but *is it a measurement of THIS model*.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WallBasis<'a> {
    /// The ledger IS the prompt (a local session), or a pair was measured on the model
    /// answering now.
    Measured,
    /// The pair was measured on `model`, which this session has switched away from -
    /// a ratio carried across the switch.
    Carried(&'a str),
    /// Nothing has measured anything: a fresh metered provider, or a session whose stored
    /// pair predates `context_ledger`.
    Unmeasured,
}

/// What still stops at `mode`, said after the mode's own words - the always-ask list and layer A's
/// `Blocked` tier are not the mode's to move, and this must agree with the summary it follows: on
/// 2026-10-26 one paragraph said both *"the list included"* and *"reaches you at every one"*.
fn mode_beyond_the_mode(mode: &letibot_tools::mode::Mode) -> &'static str {
    use letibot_tools::mode::{Boundary, Disposition};
    match mode.boundary {
        // `allow-all` in a confined VM: the list does not ask, because nothing behind it is the
        // operator's box.
        Boundary::Structural => {
            "The always-ask list does not ask here: the boundary is structural and nothing inside \
             it reaches this box. A secret leaving the boundary is still refused."
        }
        // The same absent list on a box with no boundary at all, and the reason it is absent is a
        // person's answer rather than a confinement - which is exactly the difference the two
        // coordinates exist to keep visible.
        Boundary::OperatorConsented => {
            "The always-ask list does not ask here either — you consented to that, once, in \
             advance, for this box — and nothing confines the session. What still stops is a \
             secret leaving it: that is refused before any mode is consulted."
        }
        // `read-only`: there is nothing for the list to ask about, because the tools are ABSENT
        // rather than refused - so saying it *reaches you at every one of them* would describe
        // actions this point cannot take.
        _ if mode.write == Disposition::Absent
            && mode.exec == Disposition::Absent
            && mode.network == Disposition::Absent =>
        {
            "No write, no command and nothing that leaves the box is seated here, so the \
             always-ask list has nothing to ask about — and nothing at this mode promotes a \
             blocked action."
        }
        Boundary::Operator => {
            "The always-ask list still reaches you at every action on it, whatever this mode \
             admits elsewhere — and nothing at this mode promotes an action the boundary blocks."
        }
    }
}

impl Config {
    /// How much of the window must stay free for a turn to be safe to start. A compaction is itself a
    /// turn, so firing it with no room left fails like the turn it was meant to prevent. The session
    /// that died was at 93% and the 500 arrived on the NEXT turn.
    pub fn headroom(&self) -> u64 {
        // A sixteenth of the window, floored at 2k so a summary has room, and CAPPED AT A QUARTER so
        // it cannot swallow the window it is reserving in: the first version was `(w/16).max(8192)`
        // which made the reserve larger than the window for any window of 8192 or less.
        headroom_of(self.planning_window().unwrap_or(0))
    }

    /// The window and its reserve as a line prints them - in the units the window was given in
    /// which on a metered session are the provider's. `shown_tokens(planning_window)` was the old
    /// spelling, and a round trip through a ratio is not the identity.
    pub fn shown_wall(&self) -> (u64, u64) {
        let w = self.context_window.unwrap_or(0);
        (w, headroom_of(w))
    }

    /// The window, in the units the caller is counting in. Every caller measures `resident` off the
    /// ledger, so the window must be in ledger tokens too - `context_window` is the PROVIDER's number.
    /// The ratio is measured, not assumed, and clamped to [1/4, 16] so one odd turn cannot move it.
    pub fn planning_window(&self) -> Option<u64> {
        let w = self.context_window?;
        let Some((ledger, provider)) = self.scale() else {
            return Some(w);
        };
        if ledger == 0 || provider == 0 {
            return Some(w);
        }
        let scaled = (w as u128 * ledger as u128) / provider as u128;
        Some((scaled.clamp((w / 4) as u128, (w as u128) * 16)) as u64)
    }

    /// The pair every conversion uses - the measurement on the model answering now, else the one
    /// carried across the switch. The carried pair is only consulted for a METERED session: on a local
    /// one the ledger IS the prompt, and a cloud ratio applied to it converts one unit into itself.
    pub fn scale(&self) -> Option<(u64, u64)> {
        if self.provider.is_none() {
            return self.ledger_scale;
        }
        self.ledger_scale
            .or_else(|| self.carried_scale.as_ref().map(|c| (c.ledger, c.provider)))
    }

    /// The model whose measurement is standing in for this one's, when that is what is happening -
    /// `None` once a round on the current model has measured. Every line that prints a converted count
    /// asks this: an estimate in a measurement's clothes is this tree's most expensive kind of wrong.
    pub fn carried_note(&self) -> Option<&str> {
        if self.provider.is_none() || self.ledger_scale.is_some() {
            return None;
        }
        self.carried_scale.as_ref().map(|c| c.model.as_str())
    }

    /// Where this session's count comes from - see [`WallBasis`]. One answer, read by
    /// the wall's decision and by the lines that explain it.
    pub fn wall_basis(&self) -> WallBasis<'_> {
        if self.provider.is_none() || self.ledger_scale.is_some() {
            return WallBasis::Measured;
        }
        match self.carried_note() {
            Some(model) => WallBasis::Carried(model),
            None => WallBasis::Unmeasured,
        }
    }

    /// A ledger count, in the units the operator's header shows. The ledger holds the reasoning rows
    /// a messages provider is never sent, so it over-counts: *"1.35 is a lie - that top was shown as
    /// 900+"* was the other unit. Falls back to the ledger's figure when there is nothing to convert.
    pub fn shown_tokens(&self, ledger_tokens: u64) -> u64 {
        self.provider_tokens(ledger_tokens).unwrap_or(ledger_tokens)
    }

    /// True when [`Config::shown_tokens`] is saying something different from the
    /// ledger, so a message can name the other number once rather than per figure.
    pub fn tokens_are_converted(&self) -> bool {
        self.provider_tokens(1_000_000)
            .is_some_and(|v| v != 1_000_000)
    }

    /// A metered model with no measurement of the ratio yet - the window between a switch and the
    /// first round on the new model. Not the same question as `!tokens_are_converted`: a local
    /// session's two counts agree, while an unconverted metered count must be said to be unscaled.
    pub fn tokens_are_unscaled(&self) -> bool {
        self.provider.is_some() && self.scale().is_none()
    }

    /// Ledger tokens as the provider would count them, or `None` if nobody has measured the
    /// ratio yet. `None` is load-bearing: a caller must not substitute the ledger's own figure
    /// which over-counts by a measured 49% - the direction that refuses work which would have fitted.
    pub fn provider_tokens(&self, ledger_tokens: u64) -> Option<u64> {
        let (ledger, provider) = self.scale()?;
        if ledger == 0 || provider == 0 {
            return None;
        }
        Some(((ledger_tokens as u128 * provider as u128) / ledger as u128) as u64)
    }

    /// Is the ledger AT THE WALL - within a headroom of the window? [`Config::should_compact`]'s
    /// threshold half, shared by *would the next turn fit* and *a turn was just refused - is there a
    /// way forward?* The second reads it with the flag absent: a stood-down compaction freed nothing.
    pub fn at_the_wall(&self, resident_tokens: u64) -> bool {
        let Some(w) = self.planning_window() else {
            return false;
        };
        resident_tokens + self.headroom() >= w
    }

    /// The PRE-EMPTIVE question: would the next turn not fit, so should something be tidied
    /// first? [`Config::auto_compact`]'s own gate, and the one the no-progress guard stands down.
    /// A refusal that has already happened is the other question - see [`Config::at_the_wall`].
    pub fn should_compact(&self, resident_tokens: u64) -> bool {
        self.auto_compact && self.at_the_wall(resident_tokens)
    }

    /// The pre-emptive door's question, for a session that may be metered.
    /// [`Config::should_compact`], except that a metered session with no measured scale is never at the
    /// wall: its count is LEDGER bytes against a provider's window, which folded a real session.
    pub fn pre_emptive_compact(&self, resident_tokens: u64, metered: bool) -> bool {
        if metered && self.ledger_scale.is_none() {
            return false;
        }
        self.should_compact(resident_tokens)
    }

    /// Would a turn starting at `resident` tokens fit, with the headroom reserved? The gate the wall
    /// continuation reads: *"is there room"*, where [`Config::should_compact`] folds in `auto_compact` - a flag the guard turns off without freeing anything. Measured off the LEDGER.
    pub fn room_for_next_turn(&self, resident_tokens: u64) -> bool {
        match self.planning_window() {
            Some(w) => resident_tokens + self.headroom() < w,
            None => false,
        }
    }

    /// Whether this session's VM mounts its workspace read-write instead of copying a tree. A
    /// session placed with `--vm` on a Mac, booting its own VM: not one attached to an ancestor's
    /// (that is the ancestor's mount), and not one whose caller named the tree to copy.
    pub fn vm_mounts_workspace(&self) -> bool {
        cfg!(target_os = "macos")
            && self.vm_mount
            && self.placement == letibot_tools::builtins::task::Placement::Firecode
            && self.vm_attach.is_none()
            && self.vm_source.is_none()
    }

    /// The tree a `firecode` placement copies, as one answer - [`Config::vm_source`] when the caller
    /// that knows named one, the REPOSITORY its workspace is about when it did not, and the workspace
    /// itself when there is no repository to name. The repository, because a folder weighed 72.8 GiB.
    pub fn vm_copy_source(&self) -> PathBuf {
        self.vm_source
            .clone()
            .or_else(|| crate::mergequeue::repo_of_workspace(&self.workspace))
            .unwrap_or_else(|| self.workspace.clone())
    }
}

/// Which search provider `web_search` gets, when the operator has not said. A configured key IS
/// the opt-in - *"i dont want to do leticode --web-search brave"* - and `--web-search none` turns
/// it off. No key is still no tool, so an unconfigured box is byte-identical to before this existed.
fn default_web_search() -> Option<String> {
    letibot_websearch::resolve_key(None)
        .ok()
        .map(|_| "brave".to_string())
}

/// The permission ruleset, two layers in precedence order (last match wins)
/// `~/.config/letibot/permission.json` - installed from the repository's seed, and where an *Always
/// allow* answer appends - then `LETIBOT_PERMISSION`. An unparsable file is reported and skipped.
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
    /// A session against the box this repository is developed on - named so because the endpoint, the
    /// model alias and the GGUF path are this machine's, and a `Default` would let them travel silently.
    /// Every fixture builds on it, so `reviewers` is [`Reviewers::Host`].
    pub fn for_this_box(workspace: impl Into<PathBuf>) -> Config {
        Config {
            dialect: Dialect::Qwen,
            model: "qwen-3.8-flash-next".into(),
            endpoint: Endpoint::new("127.0.0.1", 8080),
            // No default GGUF. This was the development box's own path, which on any
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
            carried_scale: None,
            unconfined: false,
            title: String::new(),
            owner: std::env::var("USER").unwrap_or_else(|_| "operator".into()),
            // A seat with no tools: nothing in it claims a tool. Every session the daemon
            // opens is composed for its seat at open instead (`seat_prompt`).
            system: default_system(&SeatFacts::default()),
            seat_prompt: None,
            seat_facts: None,
            // No fallback until `run` reads `[fallback]`: an empty list is the old behaviour
            // exactly, so a test that wants the fallback path sets this field by hand.
            fallback: Vec::new(),
            // No overrides until `run` loads the operator's `prompts.toml`. `Default`
            // is the byte-identical case: a daemon that never reads the file composes
            // the built-in prompt for every seat.
            prompts: Prompts::default(),
            // No project models until `run` discovers and loads the project's
            // `leticode.toml`. `Default` is the byte-identical case: a daemon with
            // no project file runs on its own models and discloses nothing from it.
            leticode: crate::leticode_config::LeticodeConfig::default(),
            // And no user file either, until `run` reads the fixed path beside
            // `providers.toml`. Same byte-identical case at the other level.
            leticode_user: crate::leticode_config::LeticodeConfig::default(),
            read_project_config: true,
            // An empty box: no instruction file, no box-wide notes, no box skills. A fixture
            // that read this laptop's home would assert about the laptop. The daemon fills these
            // (`for_the_daemon`), and a test that IS about the box passes its own.
            box_dirs: crate::leticode_config::BoxDirs::default(),
            box_skills: letibot_tools::builtins::skill::SkillDirs::default(),
            // Nothing has spoken for the model yet, and `Builtin` is the honest answer: the model
            // is the one this daemon was started on, and the disclosure says exactly that.
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
            landing_root: None,
            // `None` is the daemon's own `<temp>/letibot-review-clones`; a fixture that serves
            // reviews sets its own directory. See the field.
            review_clones: None,
            serve_queues: true,
            room: false,
            room_link: false,
            restart_room: false,
            logs_root: None,
            mode: letibot_tools::mode::UNSEEN_PROJECT,
            permission: parse_permission(),
            flowy: None,
            downgrade: letibot_tools::schema::Downgrade::none(),
            web_search: default_web_search(),
            web_fetch: false,
            context_window: None,
            media_marker: None,
            auto_compact: true,
            wall: crate::wall::WallControl::default(),
            nags_paused: None,
            placement: letibot_tools::builtins::task::Placement::Host,
            vm_args: Vec::new(),
            vm_source: None,
            vm_attach: None,
            vm_verify: None,
            vm_mount: false,
            build_env: Vec::new(),
            // The safe end: a fixture must not be able to boot a microVM by forgetting a line.
            // `Host` is also the operator's own ruling of 2026-10-29; the daemon reads the project
            // file in `for_the_daemon`. See the field.
            reviewers: Reviewers::Host,
            // The gate runs here unless the daemon reads otherwise. A fixture must not find
            // its gate on another machine because a file on the operator's box said so - see the
            // field, and `Config::for_the_daemon` for the one caller that reads it.
            gate_host: None,
            // A fixture starts no session with a skill it did not name. The reviewer's list is
            // the daemon's (`for_the_daemon` reads `[queue] reviewer_skills`), and a child's is its
            // spec's - see the fields.
            skills: Vec::new(),
            reviewer_skills: Vec::new(),
            provider: None,
            fabric: None,
            allow_bash: false,
            max_subagent_depth: 3,
            child_park_after: Some(std::time::Duration::from_secs(600)),
            held_results_cap: crate::delivery::HELD_RESULTS_CAP,
            restore_within: crate::restart::RESTORE_WITHIN,
            idle_exit_after: Some(std::time::Duration::from_secs(3600)),
            one_shot: false,
            adjudicator: AdjudicatorChoice::default(),
            oracle: None,
            oracle_model: None,
            oracle_scope: None,
            memory: Default::default(),
            oracle_budget: std::time::Duration::from_millis(400),
            oracle_max_tokens: None,
            oracle_question: crate::oracle::Question::Verdict,
            supervise: false,
            intent_prose: false,
            spill: SpillPolicy::Unset,
            spill_storage: SpillStorage::Memory,
            // Unbounded on purpose: `stall_rounds` is the stop that measures progress, and a round
            // count never could. See the field.
            max_tool_rounds: 0,
            http_retries: crate::harness::MAX_HTTP_RETRIES,
            stall_rounds: 5,
        }
    }

    /// The daemon's own configuration: [`Config::for_this_box`], with the project's own answer to
    /// *where do my reviewers run*. The file is read HERE and `--reviewers` is applied by `cli::run`
    /// afterwards; a file that will not parse means `host`, not a stopped daemon.
    pub fn for_the_daemon(workspace: impl Into<PathBuf>) -> Config {
        let mut cfg = Config::for_this_box(workspace);
        // The box's directories, resolved HERE and once. The harness, the notes reader and
        // the skills index all take them off this config afterwards, so an in-process fixture
        // cannot inherit the operator's `~/.claude` and a daemon reads it exactly as before.
        cfg.read_the_box();
        // One read of the project's file for both answers: where its reviewers run and where its
        // gate runs are the same file, and two calls could disagree about which file that is.
        let queue = crate::mergequeue::settings::for_repository(&cfg.workspace).unwrap_or_default();
        cfg.reviewers = queue.reviewers;
        cfg.reviewer_skills = queue.reviewer_skills.clone();
        cfg.gate_host = queue.gate_host;
        cfg.landing_root = crate::mergequeue::git::landing_root().ok();
        // The reviewers' own directory, beside the landing root and for the same reason: it is
        // the daemon's, not a session's, and a fixture that serves reviews names its own.
        cfg.review_clones = Some(std::env::temp_dir().join(crate::mergequeue::review::CLONES_DIR));
        // `$LETIBOT_LOGS_ROOT` first: what a test that spawns a daemon sets, as it sets
        // `$LETIBOT_LANDING_ROOT`, so it never writes into the box's real data directory.
        cfg.logs_root = std::env::var_os("LETIBOT_LOGS_ROOT")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| crate::mergequeue::git::letibot_data_dir().map(|d| d.join("logs")));
        // And this daemon's sessions may join the box's room (the room's slice 2). Set HERE and
        // not in `for_this_box`, which every fixture builds on: a test's session must not post into
        // the operator's queue, and the socket existing is not consent.
        cfg.room_link = true;
        // And this daemon may ask the launcher to restart the coordinator (slice 4). Armed HERE
        // for the reason above, one degree sharper: a fixture that pressed restart would stop the
        // operator's room. The coordinator's own daemon clears it.
        cfg.restart_room = true;
        cfg
    }

    /// Read this box's own directories, once: its instruction file, its box-wide notes and
    /// its skill roots. The one place `$HOME` is read for any of them, so everything below takes
    /// them off a config and a caller that never calls this reads an empty box.
    pub fn read_the_box(&mut self) {
        self.box_dirs = crate::leticode_config::BoxDirs::for_this_box();
        self.box_skills = letibot_tools::builtins::skill::SkillDirs::for_this_box(
            None,
            crate::mergequeue::git::letibot_data_dir().as_deref(),
        );
    }

    /// The box-wide standing notes: the config dir's `notes/`, beside `providers.toml`, which
    /// is the rule `Prompts::path` already uses. Empty when the config names no box, and the
    /// readers treat that as no notes rather than as a relative path into the working directory.
    pub fn global_notes(&self) -> PathBuf {
        match self.box_dirs.config.as_os_str().is_empty() {
            true => PathBuf::new(),
            false => self.box_dirs.config.join("notes"),
        }
    }

    /// The directories this session's skills are read from: the box's roots from the config
    /// and the project's two conventions off this session's own workspace, so a child opened in
    /// another tree reads that tree's skills.
    pub fn skill_dirs(&self) -> letibot_tools::builtins::skill::SkillDirs {
        self.box_skills.in_workspace(&self.workspace)
    }

    /// The skills the operator pinned, `[skills] pin` from the project file and their own: the only
    /// rows the prompt's index carries, and the one list a pin that answers to nothing is found in.
    /// Both levels apply and `--no-project-config` leaves only the operator's, which is the merge.
    pub fn skills_pin(&self) -> &[String] {
        &self.leticode.skills.pin
    }

    /// This session's logs directory: `<logs_root>/<session id>/`, or - with no logs root, as
    /// in a fixture - `<workspace>/.letibot/`, where they were before.
    pub fn daemon_logs(&self) -> PathBuf {
        match &self.logs_root {
            Some(root) => root.join(&self.session_id),
            None => self.workspace.join(".letibot"),
        }
    }

    /// Arm this session's review-queue reviewers for a VM - the one named way the value behind
    /// [`Config::reviewers`] becomes [`Reviewers::Vm`]. Two callers, and both mean it: `cli::run` for
    /// `--reviewers vm`, and a test that MEANS to exercise a VM placement.
    pub fn allow_vm_reviewers(&mut self) {
        self.reviewers = Reviewers::Vm;
    }

    /// The model name the prompt lookup runs on: `provider/model` for a metered session, the bare
    /// alias for a local one - the same name `/models` and the header show, so a `prompts.toml` written
    /// against the screen matches. A preset's default is resolved, or no glob would match.
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

    /// Compose `system` for this session's seat. Once, at session open. The sections from
    /// `prompts.toml`, then the standing notes, then the session's own paragraph and fabric block. After
    /// it, `system` is message 0 for the session's whole life; `/reseat` is the remedy.
    pub fn compose_for_seat(&mut self, seat: &SeatFacts, prompt: &SeatPrompt, notes: Option<&str>) {
        let name = self.prompt_model_name();
        let mut system = crate::standing_notes::replace(&self.prompts.compose(&name, seat), notes);
        if let Some(own) = &prompt.own {
            system.push_str(own);
        }
        if let Some(fabric) = &prompt.fabric {
            system = format!(
                "{}\n\n{}",
                system.trim_end(),
                fabric.render(seat.fabric_reach())
            );
        }
        self.system = system;
    }

    /// Every setting this session runs under, one row each, for the head's config pane (`/config`)
    /// `value` rendered and never a secret, `source` where this struct knows it (empty otherwise), and
    /// `editable` the verb that changes the row now - empty for the rows that take a restart.
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
        // What changes now, first: it is what the pane is for. The mode's choices travel with it
        // - `Mode::NAMED` is the one list, given to the head rather than copied, because the copy it
        // kept drifted: it offered `supervised`, which is not a mode.
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
        // What answers the turns, with the models this box can actually reach. The choices
        // travel with the row for the `mode` row's reason: a head that kept its own copy of a list
        // got it wrong, and this list is not even fixed at build time - it comes from the catalogue.
        {
            let cat = letibot_provider::catalogue::Catalogue::load();
            let mut names = vec!["local".to_string()];
            // The local models this fleet declares, right after `local`. This row is what the head's
            // model PICKER draws, and the first cut built it from the compiled-in presets only - so bare
            // `/models` knew no `dense78` the daemon had working. Above the presets: no key, no meter.
            for m in letibot_provider::keys::local_models(None) {
                names.push(m.name);
            }
            for p in letibot_provider::presets::ALL {
                names.push(format!("{}/{}", p.name, p.default_model(&cat)));
            }
            let now = match &self.provider {
                // The local alias is kept in the value, and named when it is a declared local
                // model, so the picker shows the row the operator is on. Matched on the address
                // because that is what the switch moved: two blocks can name one alias.
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
        // The idle plan-check, and whether the operator has paused it - `on`, or `paused until
        // 14:30`. A row because a pause must be findable where every other runtime switch is: a
        // silent board is otherwise indistinguishable from a broken clock. See `nag_pause`.
        out.push(row(
            "nags",
            crate::nag_pause::setting_value(
                self.nags_paused,
                letibot_sessionlog::registry::now_ms(),
            ),
            if self.nags_paused.is_some() {
                "/nag pause, this session"
            } else {
                ""
            },
            "/nag pause [30m|2h] · /nag resume",
        ));
        // And which of those rows this box can actually use - *"model peeker should green models we
        // have keys for"*. Only the daemon can answer it: a head that guessed would green a row that
        // refuses at the first turn. `local` is absent on purpose - it needs no credential.
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
        // And the rows that need no credential at all, which is the other half of the same
        // question and cannot live in the row above - see `MODEL_KEYLESS_KEY`. The operator
        // *"dense78 needs a key this box does not hold"*, about a box on the LAN that wants none.
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
        // The names an operator may run themselves - part two, decision 3. Carried on the wire
        // rather than compiled into a head: a list a head holds drifts, and the daemon is the side that
        // ENFORCES this one. `value` stays the comma-joined names; `tools` is the same list described.
        let mut tools_row = row(
            letibot_sessionlog::HEAD_RUN_TOOLS_KEY,
            letibot_sessionlog::HEAD_RUN_TOOLS.join(","),
            "default",
            "",
        );
        tools_row.tools = door_tools.to_vec();
        out.push(tools_row);
        // The verbs this daemon answers -, and the same argument: a head completes
        // `/`-commands from a table and forwards a verb it does not know, so this half of the
        // namespace is the daemon's to publish. Neither enumerates the other's.
        out.push(row(
            letibot_sessionlog::protocol::DAEMON_VERBS_KEY,
            crate::slash::VERBS.join(","),
            "default",
            "",
        ));
        // The sections a compaction's record has -, and the same argument: a list a head
        // holds drifts, and this one is the daemon's own template. An absent row means a daemon older
        // than this, and the head then draws the warning's `detail` sentence as it did before.
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
        // Where the model may work without asking. The base prompt names it too, for a seat
        // that can write there; this row is the operator's copy, for the config pane.
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
        // The `model` row is up with the changeable ones now: read-only here, it said only the
        // local alias - not what answers the turns once a provider is set, and nothing a head
        // could act on.
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
                None => "none — the byte vocabulary: a cloud provider's, or no model chosen yet"
                    .to_string(),
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
        // The row a stood-down compaction is found by. The value carries the pair that turned it
        // off, because `off` alone cannot say WHICH state it is: a `--no-auto-compact` the operator
        // typed, or the wall controller acting on its own - a choice versus a finding.
        out.push(row(
            "auto-compact",
            match (self.auto_compact, &self.wall.state) {
                (false, _) => "off".into(),
                (true, crate::wall::WallState::Stuck { before }) => format!(
                    "off — the wall controller is stuck: the sliced fold of {before} was \
                     measured still within {h} of the {w} window (last measured {last}). A \
                     measured count under {release} re-arms it, as do `/compact` and \
                     `/auto-compact on`; a turn the provider refuses is still folded",
                    h = self.shown_wall().1,
                    w = self.shown_wall().0,
                    last = self.wall.last_measured.unwrap_or(0),
                    release = self
                        .shown_wall()
                        .0
                        .saturating_sub(crate::wall::RELEASE_HEADROOMS * self.shown_wall().1),
                ),
                (true, _) => "on".into(),
            },
            if self.wall.is_stuck() {
                "the wall controller, after every fold was measured still at the wall"
            } else {
                ""
            },
            "",
        ));
        // The controller's state and the last measured count, always - the `auto-compact` row
        // above keys the head's resident line on `off -`, and this one is the state behind it in
        // every case, including the ones that are not news.
        out.push(row(
            "wall",
            self.wall.row("tokens"),
            "the wall controller (measured counts only)",
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
            // The ceiling. Shown rather than implied: a reply cut off at it is a *budget*
            // rather than an unreadable answer, and this is the number to raise - a number an
            // operator cannot see is a number they cannot act on.
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
        out.push(row("placement", self.placement.as_str(), "", ""));
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

    /// The things that are off, and why - a daemon that does not say *spill is unset* is one whose
    /// operator finds out when a 40 MB grep result lands in the prompt. It takes [`GateWiring`]
    /// rather than deciding, because a disclosure that guesses is what this list exists to prevent.
    pub fn disclosures(&self, wiring: &GateWiring) -> Vec<Disclosure> {
        let mut out = Vec::new();
        // Which models the project file set, said at startup - a file that changes which model
        // answers is disclosed by name, so a session on a project's models does not read as one on the
        // daemon's. Two levels, two lines, because the operator needs to see which one spoke.
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
        // Which model answers, and which level said so. *"why is my session on deepseek"*, with
        // the answer in a file they had to go and find: here it is one line, and the precedence is
        // the same line rather than a thing to look up.
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
        // The stop condition, said out loud. A turn that can only be stopped by a
        // round count is the state that cut two working sessions, and an operator who
        // does not know which of the two guards is armed cannot read a stop.
        let backstop = match self.max_tool_rounds {
            0 => "unlimited".to_string(),
            n => format!("{n} rounds"),
        };
        match (self.stall_rounds, self.max_tool_rounds) {
            // Both off: nothing but the model ends a turn. Not a refusal - the operator asked
            // for the backstop to come off and the progress check is theirs to arm - but it is a
            // different setting from either one alone and has to read as one.
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
        // , found live 2026-09-10: GLM's end-of-turn token is also the *name* of a thing an agent
        // may be asked to write, so a turn can stop mid-thought with a normal `stop`. Disclosed as on
        // because a list of only the optional checks implies the unconditional ones are absent.
        out.push(Disclosure::on(
            "mid-reasoning check",
            "a turn that stops inside an unterminated reasoning block with no \
             assistant content fails as UnfinishedReasoning and the loop asks the \
             model to continue",
        ));
        // Retrieval, the web tools, the forge and MCP: computed from what the session attached and
        // what it seated, never asserted. This line used to be a constant sentence about `ask_code`
        // which was true and unchecked - the same defect `GateWiring` exists to make unwriteable.
        for row in wiring.external.startup_disclosures() {
            out.push(Disclosure {
                subject: row.subject.into(),
                state: row.state.into(),
                detail: row.detail,
                active: row.active,
            });
        }
        // Where this project sits. First of the three, because it is the one an operator changes
        // and the other two are consequences of it: the role says which tools exist, the adjudicator
        // says who answers, and this says how much any of it costs. It was the missing line.
        out.push(Disclosure {
            subject: "mode".into(),
            state: String::new(),
            detail: format!(
                "{}. {}",
                self.mode.describe(),
                mode_beyond_the_mode(&self.mode)
            ),
            active: true,
        });
        // The preapproved list. What never asks, by count and by source, and where an *Always
        // allow* answer lands - so an operator who is asked about `git status` can see that the list
        // did not cover it rather than wonder whether the list exists.
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
        // The scratch directory, to the MODEL. The settings row is for the head's config pane
        // this is the disclosure the `harness` tool reads, and the model is the reader that needs the
        // path - one that invented `/tmp/scratch` would be writing to shared temp space.
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
        // The seat, read from the resolved registry rather than from `--role`.
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
        // The backend quotes itself. `HostBackend::describe` used to answer
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
        // Plan mode, and the half of it this build cannot do. Seating `planner` puts the session in
        // plan mode for real; what it cannot do is the *widening* - a role's tool list is stable-prefix
        // bytes, so re-seating `write` costs a full cold re-prefill (§5.3, 179k tokens).
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
            // The classes travel, rather than a bool that means "write". The gate is reachable from
            // any class that is not unattended, so a bool would call a planner (`say`) unattended - and
            // `true` produced *"Write tools are callable"* about a session with no write tools.
            &gated_classes(wiring),
            wiring.denials_surfaced,
        );
        out.push(Disclosure {
            subject: "adjudication".into(),
            state: state.into(),
            detail,
            active,
        });
        // The authorisation trail. Only worth a line where something can reach
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
        // The corpus. Only worth a line where something can reach the gate, for
        // the trail's reason: a read-only seat decides nothing to record.
        if wiring.has_write_tools || wiring.has_exec_tools || wiring.has_network_tools {
            match wiring.corpus {
                Some(c) => out.push(Disclosure::on(
                    "corpus",
                    format!(
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
        // The intent diff, T21.3's error signal.
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
        // The monitor wake. A poll and a wake are different facts, and T24 says
        // so in the entry that left this gap open. Three states, because *nobody armed one* and
        // *the thread would not start* are the same word to a reader and different facts to
        // somebody deciding what to do next.
        if wiring.seated.iter().any(|t| t == "monitor") {
            match wiring.monitor_wake {
                MonitorWake::Armed => out.push(Disclosure::on(
                    "monitor wake",
                    "a monitor that fires WAKES this session between turns; it does \
                     not wait to be asked.",
                )),
                MonitorWake::Unarmed => out.push(Disclosure::off(
                    "monitor wake",
                    "POLL ONLY",
                    "a fired monitor reaches the model only when something calls \
                     `job_list`. That is a poll, not a wake, and a condition that \
                     fires while nothing is running is a condition nobody acts on. \
                     No daemon armed a wake for this session — a `Harness` driven \
                     directly by a test or by `letibot-m1` never has one.",
                )),
                MonitorWake::Failed => out.push(Disclosure::off(
                    "monitor wake",
                    "POLL ONLY",
                    "a fired monitor reaches the model only when something calls \
                     `job_list`. This is NOT the same as a session nobody armed: the \
                     daemon asked for the waiter and the thread could not be started, \
                     and the `monitor_wake_not_armed` warning on this session's log \
                     says why. Nothing is watching what a firing was meant to wake.",
                )),
            }
        }
        // A subagent's downgrade, said as what is gone: the tools of those
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
        // The fabric block. What the model was told exists on the shelf and
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
        // What `task(where: firecode)` would get on this host, said where a subagent can be
        // started - FIRECODE-NOTES item 4: the placement's preconditions used to be found out
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
                        "a subagent can be placed in a VM: firecode at {}{}. Inside, the guest sees \
                         a copy of this workspace and nothing else of the host — no host filesystem, \
                         no host processes, no credentials — so permission checks are off there: \
                         nothing is asked and there is nothing to approve. The guest root is a layer \
                         stack, so what a VM installs and saves is still there next run rather than \
                         reinstalled per call, and a project's toolchains live in its own layer. Use \
                         it for work that must not spend the operator's attention — a reviewer, a \
                         long build, a probe — and for anything you want to be unable to reach this \
                         box.",
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
                format!(
                    "a firecode VM holding a copy of {}: the VM is the boundary, the mode inside \
                     is allow-all, and what this session writes lands in a sibling directory when \
                     it ends",
                    self.vm_copy_source().display()
                ),
            ));
        }
        // The banner half of a remote placement (`docs/remote-hosts.md`). `ON` and not `off`: it
        // changes where every command lands, and it says the other half too - that this is NOT a
        // boundary - because a list of only the guarantees reads as a claim about the rest.
        if let Some(host) = self.placement.host() {
            out.push(Disclosure::on(
                "placement",
                format!(
                    "an ssh host, `{host}`: this session's TOOLS run there — bash, read, write, \
                     edit, jobs — while its model, store and gate stay here. NOT a boundary: \
                     `{host}` is the operator's machine with the operator's files, so the gate \
                     is this box's own gate and every card names the host. The tree is a \
                     checkout of {}, and its commits come back as the branch \
                     `remote/{host}/<short id>` when the session ends",
                    self.vm_copy_source().display()
                ),
            ));
        }
        // The room. Whether anybody said anywhere reaches this session, and as what. The seat's
        // own line - listening, stalled, stopped - is printed by the daemon from the seat, not from
        // here, because a config cannot know whether a listener is actually attached.
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

/// The access classes this session seated that can reach the gate, in prompt order. Read off the
/// wiring's booleans, which were themselves read off the seated schemas. `Read` and `Session` are
/// absent because they are unattended - a read-only tool has no code path to a question (clause 4).
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

/// What the session actually wired, read from it rather than asserted about it - a type that makes a
/// defect unwriteable: the banner was a hard-coded sentence, true when written and checked by nothing
/// so a seat with `write` was told no boundary existed while everything could prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateWiring {
    /// How the adjudicator identifies itself, or a string beginning `none`.
    pub adjudicator: String,
    /// Whether the execution backend was opened with a writable constructor.
    pub backend_writable: bool,
    /// Whether any seated tool declares `Access::Write`.
    pub has_write_tools: bool,
    /// What is behind the tools that need infrastructure this box does not run
    /// read off the seams themselves. The same argument as the three fields above
    /// applied to four more things that can silently be absent.
    pub external: letibot_tools::ExternalWiring,

    // ---- the seams wired on top, every one of them read rather than asserted ---- The role
    // that was actually seated, taken from the resolved registry - not the role that was
    // *asked for*.
    pub role: String,
    /// Every seated tool name, in prompt order. The evidence behind every boolean
    /// below, so a disclosure can be checked rather than believed.
    pub seated: Vec<String>,
    /// Whether any seated tool declares `Access::Exec`. Read off the schemas, so a
    /// build that seats `bash` under some other name still says so.
    pub has_exec_tools: bool,
    /// Whether any seated tool declares `Access::Network`.
    pub has_network_tools: bool,
    /// How the execution backend describes itself - `HostBackend::describe`, which says `read-only`
    /// `writable`, `NOT CONFINED` or names the boundary. It exists because `describe` used to answer
    /// `read-only` for every backend: the banner now *quotes* the backend rather than paraphrasing it.
    pub backend: String,
    /// Whether a [`letibot_tools::DenialSink`] is attached, so a refusal reaches the operator at the
    /// moment it is decided (`docs/boundary-and-adjudication.md` §4b: denials that go nowhere are not
    /// a state to be in silently).
    pub denials_surfaced: bool,
    /// Whether an authorisation trail source is installed, so the adjudicator sees the operator's own
    /// words rather than deciding blind. `false` means every trail is `NotCollected` - *nobody
    /// looked*, which is a different fact from an empty trail, and why `ModelAdjudicator` refuses.
    pub trail_installed: bool,
    /// Whether the intent encoder is attached, so `close_the_turn` can diff what
    /// the turn *said* it would do against what ran. `false` makes every completion
    /// `Verification::NoEncoder` - not complete, and the reason is ours.
    pub intent_encoder: bool,
    /// Whether a fired monitor WAKES this session between turns, or only waits to be asked, and
    /// when it waits, WHICH of the two waits it is.
    ///
    /// Three states and not a bool: *no daemon armed one* and *the waiter thread failed to start*
    /// render the same line from a bool, and they are different facts - the second already
    /// published a `Warning` naming the error, and a reader told only *POLL ONLY* goes looking
    /// for a knob that would not have helped.
    pub monitor_wake: MonitorWake,
    /// Whether the gate's decisions are written somewhere durable, and what is already there. `None`
    /// means the corpus is discarded: the rows die with this daemon, and nothing about a session
    /// that is dropping them looks different from one that is keeping them.
    pub corpus: Option<letibot_tokencore::store::CorpusCounts>,
    /// The firecode a subagent would be placed in a VM with, asked once at start - `None`
    /// when this host has none, so `task(where: firecode)` would be refused.
    pub firecode: Option<letibot_tools::firecode::Installed>,
}

/// What the monitor wake is, as the banner has to say it: armed, never armed, or asked for and
/// failed. [`GateWiring::monitor_wake`]'s three states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MonitorWake {
    /// Nothing armed a wake. A `Harness` a test or `letibot-m1` drives directly reports this
    /// honestly: there was no daemon and no thread to start.
    #[default]
    Unarmed,
    /// The daemon's waiter is running, so a firing wakes the session between turns.
    Armed,
    /// The daemon asked for the waiter and the thread could NOT be started
    /// ([`crate::turn_tail::spawn_monitor_wake`]). Different from [`MonitorWake::Unarmed`] because
    /// somebody tried: the error is already a `Warning` on the session's own log.
    Failed,
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
            monitor_wake: MonitorWake::Unarmed,
            corpus: None,
            firecode: None,
        }
    }
}

/// One thing the operator has to know about this session before they trust an answer from it.
/// Structured rather than a sentence because the banner had become a wall of correct prose with the
/// two words that decide whether to trust it - the subject, and whether it is on - buried mid-line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disclosure {
    /// `spill`, `store`, `retrieval`, `adjudication`.
    pub subject: String,
    /// The state, in one word. `UNSET`, `MEMORY`, `INERT`, `NONE` - or empty when
    /// the thing is configured and there is nothing alarming to name.
    pub state: String,
    /// What it means, in full. Never abbreviated for the banner.
    pub detail: String,
    /// Whether the subject is doing anything. `false` is the case that has to be
    /// impossible to miss.
    pub active: bool,
}

impl Disclosure {
    /// Public because the daemon has disclosures the config cannot compute - the
    /// count of stored sessions needs the store open - and a second Disclosure
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
mod tests;

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

    /// An unscaled count never opens the pre-emptive door on a metered session - it is
    /// ledger tokens against the provider's window. A local one has no scale to wait for, and a
    /// measured scale opens it as `should_compact` would.
    #[test]
    fn an_unmeasured_metered_session_is_not_folded_before_it_is_measured() {
        let mut cfg = Config::for_this_box("/tmp");
        cfg.auto_compact = true;
        cfg.context_window = Some(1_000_000);
        cfg.ledger_scale = None;
        let ledger = 2_431_232;
        assert!(
            cfg.should_compact(ledger),
            "the premise: the raw count is over the window"
        );
        assert!(
            !cfg.pre_emptive_compact(ledger, true),
            "a metered session with no measured scale was folded on its ledger bytes"
        );
        assert!(
            cfg.pre_emptive_compact(ledger, false),
            "a local session's count is its own"
        );
        // Measured at about four ledger bytes a token: 2.43M bytes is ~600k of a 1M window.
        cfg.ledger_scale = Some((2_400_000, 600_000));
        assert!(
            !cfg.pre_emptive_compact(ledger, true),
            "~600k of 1M is not the wall"
        );
        assert!(cfg.pre_emptive_compact(3_990_000, true), "~997k of 1M is");
    }

    /// The wall-continuation gate measures the window, not the flag. `should_compact` folds in
    /// `auto_compact`, and the guard turns that flag off WITHOUT freeing anything - so a gate reading
    /// the flag would see room that is not there. The headroom boundary is exclusive.
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
