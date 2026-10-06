//! The runtime: what happens between a `ToolCall` coming out of the parser and a
//! `TranscriptItem::ToolResult` going back in.
//!
//! Every clause that is not a property of one tool lives here.
//!
//! | clause | here |
//! |---|---|
//! | 2 — malformed input is salvaged | [`ToolRuntime::invoke`] calls [`crate::args::salvage`] and carries the repairs onto the result |
//! | 3 — abstention is not success | the outcome comes from the tool and is never widened; [`crate::result::propagate`] is the caller's half |
//! | 4 — read/write declared in the schema | [`ToolRuntime::invoke`] consults the [`Gate`] **only** for a non-`Read` tool |
//! | 5 — bounded and spilled | [`crate::spill::Spiller`] runs on every payload |
//! | 6 — the description is usage | [`Registry::register`] refuses a description that fails the lint |
//! | §8.4 — tool budget | [`Role`] and [`Registry::resolve_role`], a refusal and not a guideline |
//!
//! Clause 1 is the tools' own, because it is a different sentence for every tool.
//! What the runtime does about it is smaller and easy to miss: an **unknown tool
//! name** is itself a miss, and it comes back with the tool list and the nearest
//! match rather than with "unknown tool".

use letibot_transcript::{ToolCall, ToolOutcome, TranscriptItem};
use serde_json::Value;

use crate::args::{Repair, salvage};
use crate::backend::ExecBackend;
use crate::events::{ToolEvent, ToolEventSink, payload_digest};
use crate::result::ToolResult;
use crate::schema::{Access, ToolSchema, lint_description};
use crate::spill::{SpillContext, Spiller};

/// What came back from asking whether a path may enter the session's view.
///
/// Three outcomes and not two: a refusal is somebody saying no, and `NotAsked` is
/// nobody having been asked — the same distinction `not_run` draws against
/// `denied` one layer up, and for the same reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewGrant {
    Granted { writable: bool },
    Refused(String),
    NotAsked,
}

/// What a tool hands back. The outcome is the tool's decision and the runtime
/// never widens it.
#[derive(Debug, Clone, PartialEq)]
pub struct Invocation {
    pub outcome: ToolOutcome,
    pub payload: String,
    /// Clause 1's voice: what the tool did about a miss.
    pub notes: Vec<String>,
    /// Both sides of a file this call changed, for a head to draw.
    ///
    /// `None` for every tool that changed nothing, **including a write tool whose
    /// call was a no-op** — a head asked to render a diff of no change would draw
    /// an empty card, and an empty card is indistinguishable from a bug. See
    /// [`crate::edit::FileEdit`] for what the head does with it and for why it is
    /// here rather than in a [`crate::events::ToolEvent`].
    pub edit: Option<crate::edit::FileEdit>,
    /// **Paths this call could not see because they are outside the session's
    /// filesystem view.** Read off the boundary, not guessed from the output.
    ///
    /// Empty for every call that saw everything it named, and for every backend
    /// with no view at all. Non-empty is a dead end the runtime can do something
    /// about: it raises a grant decision, and until this existed the only thing the
    /// system could do was print a note telling the model that "whoever opened the
    /// session" would have to grant it — with nothing able to ask that person.
    pub needs_in_view: Vec<std::path::PathBuf>,
    /// **Bytes this call read that are not text** — an image, today; nothing else claims it.
    ///
    /// The second structured channel beside [`Invocation::edit`], and its docstring's argument —
    /// *the row is the durable artifact, so the fact lives on the row* — is the reason it is a field
    /// rather than an event. Where it DIFFERS is what earns it its own paragraph: `edit` is
    /// display-only and this one exists to be SENT (see [`crate::media`]).
    ///
    /// `None` for every tool that read no picture, which is every call but one.
    pub media: Option<crate::media::Media>,
}

impl Invocation {
    pub fn ok(payload: impl Into<String>) -> Self {
        Invocation {
            outcome: ToolOutcome::Ok,
            payload: payload.into(),
            notes: Vec::new(),
            edit: None,
            needs_in_view: Vec::new(),
            media: None,
        }
    }

    /// No result, and the body is whatever helps the caller correct itself.
    pub fn abstained(reason: impl Into<String>, payload: impl Into<String>) -> Self {
        Invocation {
            outcome: ToolOutcome::Abstained {
                reason: reason.into(),
            },
            payload: payload.into(),
            notes: Vec::new(),
            edit: None,
            needs_in_view: Vec::new(),
            media: None,
        }
    }

    pub fn failed(reason: impl Into<String>, payload: impl Into<String>) -> Self {
        Invocation {
            outcome: ToolOutcome::Failed {
                reason: reason.into(),
            },
            payload: payload.into(),
            notes: Vec::new(),
            edit: None,
            needs_in_view: Vec::new(),
            media: None,
        }
    }

    /// **The deadline passed and the command was killed.** opencode's `timeout`
    /// semantics: the process is not left running in the background, it is
    /// terminated, and the payload says how to ask for more time.
    pub fn timed_out(payload: impl Into<String>) -> Self {
        Invocation {
            outcome: ToolOutcome::Timeout,
            payload: payload.into(),
            notes: Vec::new(),
            edit: None,
            needs_in_view: Vec::new(),
            media: None,
        }
    }

    /// **Nothing ran.** Not a denial, not an abstention, not a failure — see
    /// [`crate::attach`], which is where the sentence a tool puts in `why` is
    /// built so that every tool with nothing behind it produces the same shape.
    ///
    /// This was a struct literal in `retrieval` and nowhere else, which is why it
    /// is here now: the family had three constructors and a fourth outcome, and
    /// the missing constructor is what makes a tool author reach for the nearest
    /// one that exists.
    pub fn not_run(why: impl Into<String>, payload: impl Into<String>) -> Self {
        Invocation {
            outcome: ToolOutcome::NotRun { why: why.into() },
            payload: payload.into(),
            notes: Vec::new(),
            edit: None,
            needs_in_view: Vec::new(),
            media: None,
        }
    }

    /// **Still running, and here is the handle.** Not finished and not failed.
    ///
    /// The fourth constructor exists for the same reason [`Invocation::not_run`]
    /// became one: a family with three constructors and a fourth outcome is a
    /// family whose fourth outcome gets spelled as whichever of the three is
    /// nearest, and every one of the three says something false here. A promotion
    /// the model does not notice is the same defect as a denial the operator does
    /// not see — it infers the wrong thing and acts on it.
    pub fn backgrounded(
        handle: impl Into<String>,
        ran_for: std::time::Duration,
        how: letibot_transcript::Backgrounding,
        next: impl Into<String>,
        payload: impl Into<String>,
    ) -> Self {
        Invocation {
            outcome: ToolOutcome::Backgrounded {
                handle: handle.into(),
                ran_for_ms: ran_for.as_millis() as u64,
                how,
                next: next.into(),
            },
            payload: payload.into(),
            notes: Vec::new(),
            edit: None,
            needs_in_view: Vec::new(),
            media: None,
        }
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }
}

/// Bounds a tool must respect so that one call cannot occupy a session.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// The most filesystem entries a walk may visit.
    pub max_walk_entries: usize,
    /// The most matches a search reports before it says it stopped counting.
    pub max_matches: usize,
    /// The most bytes a single file read returns before the spill policy sees it.
    pub max_file_bytes: usize,
    /// The most lines `read` returns when the call named no `limit`. A full
    /// thousand-line file at twenty thousand tokens is one call occupying a
    /// session, which is this struct's job to prevent; the tool's own note
    /// hands back the offset that continues it.
    pub max_read_lines: usize,
    /// The most bytes of numbered text `read` returns in one call, whatever the
    /// line count. The line cap cannot bound a file with six enormous lines —
    /// minified javascript is one line — so the byte budget is the guarantee
    /// and the line cap is the default shape.
    pub max_read_bytes: usize,
    /// The most characters of one line `read` will show before it clips the
    /// rest. A clipped line is named in the notes, because a model that cannot
    /// see a boundary will assume there is none.
    pub max_read_line_chars: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_walk_entries: 20_000,
            max_matches: 200,
            max_file_bytes: 4 * 1024 * 1024,
            max_read_lines: 200,
            max_read_bytes: 32 * 1024,
            max_read_line_chars: 2000,
        }
    }
}

/// What a tool is given for one call.
/// **Is the operator waiting to say something?**
///
/// A closure rather than a flag, because the answer is the hub's and it changes
/// while a call runs — which is the whole point. `crates/tools` must not know
/// what a hub is, so the daemon supplies the question already answered.
pub type OperatorWaiting = std::sync::Arc<dyn Fn() -> bool + Send + Sync>;

/// **Is this job's completion already on its way to the model?**
///
/// The daemon watches every background job and submits its settlement to the model
/// as a turn of its own, unprompted (R7, `crates/harnessd/src/jobwatch.rs`). That is
/// a promise the harness *keeps*, and until R23 the tools merely described it — so a
/// `job_wait` on a job the harness had already promised could still block the floor
/// for its whole deadline, and the model could spend a round waiting for an answer
/// that was in flight. *"A promise the harness keeps and the tools merely describe is
/// a promise the model can decline."*
///
/// Same shape as [`OperatorWaiting`] and for the same reason: the answer is the
/// daemon's and it changes while a call runs, so `crates/tools` gets the question
/// already answered and never learns what a watcher is.
///
/// A closure taking the **job id**, because unlike the operator's question this one
/// is about a particular job: the promise is per job, and two `job_wait` calls in the
/// same session must get different answers.
pub type CompletionDelivered = std::sync::Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// **What the daemon is told about the OPERATOR's own run, while it runs.**
///
/// The operator's `!` line is the one run with a **stdin the daemon holds** — see
/// [`crate::exec::SpawnRequest::tty`] and [`crate::exec::Stdin`] — and it is therefore the
/// one run a person can be **answered**. The tools layer is the half that knows the three
/// things the daemon does not: the job, the command, and the handle to write to. This is
/// the seam they travel on, and it is [`OperatorWaiting`]'s shape for the same reason:
/// **`crates/tools` must not learn what a session, a hub or a card is.** The daemon
/// supplies a closure; the tools layer calls it; what it does with the news is the
/// daemon's business.
///
/// # Why three events and not a question
///
/// The obvious design is a callback that *asks* — *"should I raise a card?"* — and it is
/// the wrong one, because the answer is not the tool's to act on: the tool cannot draw
/// anything, and the card is raised on another thread while this one keeps waiting. So the
/// tool **reports** and the daemon decides. The three reports are the whole life of the
/// run:
///
/// * [`OperatorRun::Answerable`] — it started, it has a stdin, here is the handle. **This
///   is the one that makes the manual way in work when no card is ever raised**, and it is
///   the reason a missed heuristic is a missed convenience rather than a lost command.
/// * [`OperatorRun::Waiting`] — the run is **blocked reading the answer we hold** and has
///   been quiet for a beat. See [`crate::exec::ask`]: this is a reading of the process and
///   not of its words, and the text that rides along is there to be **shown** and never to
///   be decided on.
/// * [`OperatorRun::Unreadable`] — the run has been quiet for a beat and **this daemon may
///   not look at one of its processes**, so it cannot tell whether the run is waiting. See
///   the variant: this is the `sudo` case, and the report exists because silence there is
///   what the operator read as *the daemon hangs*.
/// * [`OperatorRun::Ended`] — the run is over, so whatever card was up for it comes down.
///   Without this the card would outlive the command it was about: a person would type an
///   answer into a program that had already exited.
///
/// # What is deliberately not in it
///
/// **No secret.** [`OperatorRun`] carries a question, a command and a job — never a
/// password, and never a channel a password could travel on. A password has its own path
/// (`SUDO_ASKPASS`, an `askpass` head, `ClientFrame::Secret`), and the two must not be
/// mergeable by a later edit: a prompt card is drawn in the open, and a secret must not be.
/// `crate::exec::ask`'s own docs carry the half of that which is a *detection* decision (a
/// password prompt is not a question).
pub enum OperatorRun<'a> {
    /// **This run can be answered.** It has a stdin — a pipe this daemon holds — and this
    /// is the handle to it.
    Answerable {
        /// The job's handle, as `job_list` spells it.
        job: &'a str,
        /// The command as the operator typed it, verbatim.
        command: &'a str,
        /// **The way in.** Cloned out of the job, because the answer arrives on a thread
        /// that does not hold the job and must not have to find it.
        stdin: crate::exec::Stdin,
    },
    /// **This run is waiting for an answer**, by [`crate::exec::ask`]'s reading of the
    /// process — not of its words.
    Waiting {
        /// The job's handle.
        job: &'a str,
        /// **The last thing the program said, for the card to SHOW.** `None` when it has
        /// said nothing at all, which is a real case — a `read` that asks nothing is still
        /// waiting — and is reported as *nothing to show* rather than as an empty line.
        ///
        /// **Nothing anywhere decides anything by this string.** The card carries it
        /// because the person answering needs to see what they are answering.
        question: Option<&'a str>,
    },
    /// **This daemon cannot tell whether the run is waiting** — it has been quiet for a
    /// beat, and at least one of its processes could not be looked at.
    ///
    /// # Why this is not [`OperatorRun::Waiting`] with a `None` in it
    ///
    /// Because it is not a question and must not become a card. `ask::Waiting::Unreadable`
    /// is *"I could not look"*, which is the one thing a card must never be raised on —
    /// the whole design of [`crate::exec::ask`] is that a card is a reading of the process
    /// and not a guess about it, and a card raised here would be a guess that is *wrong*
    /// for every long quiet command that is not asking anything.
    ///
    /// # The case it exists for, which is the operator's own
    ///
    /// `! sudo apt install mc`: `bash` runs as the operator and is readable, `sudo` and then
    /// `apt` run as **root**, and `/proc/<pid>/fd/0` is `EACCES` for a uid that is not
    /// theirs. So the process that is actually waiting at `Continue? [Y/n]` is invisible to
    /// the daemon, no card can honestly be raised, and — before this report existed — the
    /// daemon said nothing at all while the run held its one worker until the deadline. The
    /// operator's report is that silence: *"the command appears queued and the daemon
    /// hangs."*
    ///
    /// **The report is a disclosure and not a question.** What the daemon does with it is
    /// say the one sentence that is true — *I cannot tell, and `!send` is the way in* —
    /// because `!send` needs no signal at all and works under every miss the card has.
    Unreadable {
        /// The job's handle.
        job: &'a str,
    },
    /// **The run is over.** Whatever card was up for it comes down.
    Ended {
        /// The job's handle.
        job: &'a str,
    },
}

/// See [`OperatorRun`]. `None` in a runtime with no daemon behind it — a harness driven
/// directly by a test, or a backend that cannot start processes — and every operator run
/// then behaves exactly as it did before the card existed.
pub type OperatorRuns = std::sync::Arc<dyn Fn(OperatorRun<'_>) + Send + Sync>;

pub struct InvokeCtx<'a> {
    pub backend: &'a dyn ExecBackend,
    pub spiller: &'a Spiller,
    /// What this session has shown the model, and therefore what it may change.
    /// See [`crate::files`]: the read-only tools write to it, the write tools
    /// read it, and it is on the context rather than on each tool so that two
    /// tools cannot end up with two ledgers.
    pub files: &'a crate::files::FileLedger,
    pub limits: Limits,
    turn_id: &'a str,
    call_id: &'a str,
    sink: &'a mut dyn ToolEventSink,
    operator_waiting: Option<&'a OperatorWaiting>,
    completion_delivered: Option<&'a CompletionDelivered>,
    /// See [`OperatorRun`]. `None` in a runtime with no daemon behind it.
    operator_runs: Option<&'a OperatorRuns>,
    /// **Whether this call is the OPERATOR's own**, which is the one thing that
    /// decides whether the command meets the world their console gives it.
    ///
    /// It is `!gated` and nothing else: [`ToolRuntime::invoke_operator`] is the
    /// ungated path, and it is the `!` line and the door's calls — *a person typed
    /// this and is looking at a screen*. A model's call is gated and reads the
    /// payload as tokens. So the flag is derived where the two paths are told apart
    /// rather than passed in by a caller who could disagree with them, which is the
    /// shape that keeps a second reader from inventing a third answer.
    ///
    /// **Three things follow from it and they follow from this one flag**: a pty on
    /// stdout and stderr ([`crate::exec::pty`]), their shell with their rc read, and
    /// their environment with pagers that cannot page ([`crate::exec::console`],
    /// which carries the decision, what it changes, and why a model's `bash` call
    /// gets none of it). One flag rather than three because it is one fact, and three
    /// flags with the same value are three things a later edit can make disagree.
    pub tty: bool,
}

impl InvokeCtx<'_> {
    /// Say that the call is still working. §8.5 requires this to count as liveness,
    /// and a tool that walks a large tree must produce it.
    pub fn progress(&mut self, note: impl Into<String>) {
        self.sink.emit(ToolEvent::Progress {
            turn_id: self.turn_id.to_string(),
            call_id: self.call_id.to_string(),
            note: note.into(),
        });
    }

    /// **The operator has typed something and is waiting for this call to end.**
    ///
    /// A tool that runs for minutes — a `digest` folding two dozen parts, a
    /// `job_wait` holding a three-minute deadline — should ask, and should stop
    /// when the answer is yes. Nothing is lost by stopping: what has been read is
    /// reported, and the operator's sentence very often makes the rest of the work
    /// pointless. The measured case, 2026-09-20: a `digest` reached part 15 of 24
    /// while *"we did it with another agent"* sat queued underneath it.
    ///
    /// `false` when nobody wired the question, so a runtime without a hub behaves
    /// exactly as it always did.
    pub fn operator_waiting(&self) -> bool {
        self.operator_waiting.map(|f| f()).unwrap_or(false)
    }

    /// **This job's completion is already being delivered to you.**
    ///
    /// See [`CompletionDelivered`]. `false` when nobody wired the question, so a
    /// runtime with no watcher behind it blocks exactly as it always did — which is
    /// the conservative direction: an unwired runtime waits, and a wait is never
    /// wrong, only slow.
    pub fn completion_delivered(&self, job: &str) -> bool {
        self.completion_delivered.map(|f| f(job)).unwrap_or(false)
    }

    /// **Tell the daemon something about the operator's own run.** See [`OperatorRun`].
    ///
    /// A no-op when nobody wired it, so a runtime with no daemon behaves exactly as it
    /// always did — which is the conservative direction and the one every other seam here
    /// takes: an unwired run is a run with no card, and a run with no card is a run a
    /// person answers with `!send`.
    pub fn operator_run(&mut self, what: OperatorRun<'_>) {
        if let Some(f) = self.operator_runs {
            f(what);
        }
    }

    /// Whether anybody is listening. **The one thing a tool may branch on**, because a
    /// tool that did work to report something nobody wired would be paying for a card it
    /// cannot raise: `bash` reads a run's output every tick to find the question to show,
    /// and that read is worth skipping when there is no daemon to show it to.
    pub fn operator_runs_wired(&self) -> bool {
        self.operator_runs.is_some()
    }

    pub fn call_id(&self) -> &str {
        self.call_id
    }

    /// The turn this call belongs to.
    ///
    /// Needed by any tool whose result is a fact *about the turn* rather than
    /// about the tree — the intent ledger keys on it, because "declared this turn
    /// and nothing ran this turn" is the whole of T21.3.
    pub fn turn_id(&self) -> &str {
        self.turn_id
    }
}

/// One tool.
pub trait Tool: Send + Sync {
    fn schema(&self) -> ToolSchema;

    /// `args` has already been salvaged against the schema, so a tool sees an
    /// object with the right key names and the right value types or nothing at
    /// all. What it must still handle is a *missing* argument, because supplying
    /// one would be inventing meaning.
    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation;

    /// **What THIS call does**, when a tool's verbs differ in kind.
    ///
    /// The schema's class is one value for the whole tool, so a tool that
    /// dispatches on an argument gets its most consequential verb's class applied
    /// to every verb. `flowy` declared `Network` for all eight of its actions, so
    /// `flowy read` — which only reads the fabric — was adjudicated exactly like
    /// `flowy say`, which puts a message in front of other people. The operator's
    /// report was that bookkeeping tools should not be reaching the guard at all.
    ///
    /// The brief already knew about this and worked around it: it pushes the `op`
    /// into the boundary facts because *"a tool that dispatches on an op is one
    /// tool to the gate and ten actions to whoever is deciding"*. This is the same
    /// observation applied one layer earlier, where it can stop the call reaching
    /// the gate at all.
    ///
    /// The default is the schema's class, so a tool that does one thing says
    /// nothing. **A tool may only narrow**, never widen: the runtime takes the more
    /// consequential of the two, so a tool cannot talk its way out of the gate by
    /// claiming a call is a read.
    fn access_for(&self, _args: &Value) -> Option<crate::schema::Access> {
        None
    }
}

/// One gated call, as the gate sees it.
///
/// W9's `admit(name, access, args)` was the smallest thing the runtime needed and
/// `TODO.md` T16.3 said W11 should absorb it rather than build a parallel seam.
/// W10 is the first caller that needs the gate to *decide* something, and three
/// of §11.2's request fields could not be filled from the old signature: the turn
/// and call this belongs to (so an audit row can be correlated), and whether the
/// target exists (so [`crate::adjudicate::ActionClass`] can say whether the action
/// is reversible). They are here rather than smuggled through `args`.
#[derive(Debug, Clone, Copy)]
pub struct GateCall<'a> {
    /// The tool as **declared**, not as the model spelled it.
    pub name: &'a str,
    pub access: Access,
    /// Already salvaged (clause 2), so the gate reads the same values the tool will.
    pub args: &'a Value,
    pub turn_id: &'a str,
    pub call_id: &'a str,
    /// What the backend calls itself. `EXPLAIN` and the adjudication brief both
    /// want the operator to see where a write would land.
    pub workspace: &'a str,
    /// Whether the `path` argument names something that exists, when there is one.
    /// `None` means the call has no path argument to stat, which is a different
    /// fact from "the path is not there".
    pub target_exists: Option<bool>,
    /// **The scripts this command will run, read from disk.**
    ///
    /// An interpreter is `Intent::ExecuteCode` and nothing more: layer A reads
    /// `python3 script.py` as *a program runs*, and the program text is a file it
    /// never opens. Measured 2026-09-20 — `python3 -c "import os;
    /// os.remove('/home/dead/.ssh/id_rsa')"` classifies as `MayApprove` while the
    /// same effect written as `rm -rf ~/.ssh` is `Tier::Blocked`, because the
    /// literal path inside the `-c` string is not a path to the classifier.
    ///
    /// The oracle CAN read code — asked with the command in the brief it denied
    /// that call, and denied it again with the path built at runtime from
    /// `pathlib.Path.home()`, which no matcher could have caught. What it cannot
    /// do is read a file nobody handed it: `python3 script.py` shows it a
    /// filename.
    ///
    /// So the file is read here, where the backend is, and travels in the brief.
    /// Read UP FRONT rather than on request: an oracle that has to ask for the
    /// script can forget to, and that failure is silent — it answers about a
    /// filename and nothing says it judged blind.
    pub scripts: &'a [ScriptSource],
}

/// One script an execution vehicle was handed, as the adjudicator will see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptSource {
    /// The path as the command spelled it.
    pub path: String,
    /// The bytes, or why there are none. **Not `Option<String>`**: "this file is
    /// 400 KB" and "this file is not there" and "these are the bytes" are three
    /// facts, and an adjudicator that cannot tell them apart is one that reads a
    /// missing file as an empty one.
    pub body: ScriptBody,
}

/// **The scripts a shell command will run, read through `read`** — R39 moved this out
/// of [`ToolRuntime`] so that it is one implementation with three callers rather than
/// three readers that can drift: the session's own gate, the classifier's example, and
/// any corpus sweep that wants the number a session would get.
///
/// **One read serves both halves of the gate.** The body is pulled here, before the
/// classifer runs, because the adjudicator's brief has to carry the program — *"judge
/// THIS, not the filename"* — and [`crate::intent::Baseline::of_command_with`] is then
/// handed the same bytes, so the half that decides the tier and the half that decides
/// permission cannot disagree about one file.
///
/// **A path in the secret store is not opened at all.** That is R39's disclosure clause,
/// and it is a real hole rather than a hypothetical: `python3 ~/.ssh/id_rsa` would
/// otherwise put the key in the brief, the transcript and the model's context, and the
/// entry here has been added to the brief *because* a reader asked what it contains. The
/// file is still named — `Unreadable` is a fact, not silence — and the command still runs
/// an interpreter on it, which layer A judges from the argv. What does not happen is the
/// read.
///
/// **The file can change between this read and the run, and the rest of this paragraph is
/// the answer to it rather than a disclaimer.** A here-document body cannot: it is in the
/// command text, so it is the same bytes at classification and at execution. A file on disk
/// is not, and a gated call can wait minutes for an answer. So: one read serves both halves
/// of the gate (they cannot disagree about one file); the finding says when the read
/// happened and carries its digest; and — the part that is a check and not a caveat —
/// [`ToolRuntime`] re-reads it after the gate answers and before the command starts, and
/// refuses if it is no longer those bytes
/// ([`ToolRuntime::script_changed_since_the_gate`]). What is still not true is that the
/// bytes are pinned: a file can change between that second read and the `exec` a
/// microsecond later, and nothing here can close that. The window that mattered — the one
/// an operator's thinking time opens — is closed.
pub fn scripts_for(
    command: &str,
    home: Option<&str>,
    read: impl Fn(&str) -> Result<Vec<u8>, String>,
) -> Vec<ScriptSource> {
    let n = letibot_code::shell::normalise(command);
    let mut out: Vec<ScriptSource> = Vec::new();
    for stage in &n.stages {
        let letibot_code::shell::Word::Literal(program) = &stage.program else {
            continue;
        };
        // **The same unwrapping the classifier does**, so `sudo python3 foo.py` has its
        // file read by the reader and scanned by layer A, rather than read by neither
        // and reported as one half's hole.
        let (program, skip) = match crate::intent::unwrap_wrapper(program, &stage.argv) {
            Some((inner, at)) => (inner, at + 1),
            None => (program.clone(), 0),
        };
        let argv = literal_argv(&stage.argv[skip.min(stage.argv.len())..]);
        let Some(path) = crate::intent::script_argument(&program, &argv) else {
            continue;
        };
        if out.iter().any(|s| s.path == path) {
            continue;
        }
        // **Before the open, not after.** A store path is named and not read; the
        // sentence says which store so that a reader of the brief is told what is there
        // and why they are not being shown it.
        let body = match crate::intent::Surroundings::secret_store_of(&path, home) {
            Some(store) => ScriptBody::Unreadable(format!(
                "it is in the {store} store, and this is not opened to find out what it \
                 contains. The command still runs an interpreter on that path"
            )),
            None => match read(&path) {
                Err(why) => ScriptBody::Unreadable(why),
                Ok(bytes) => bounded(bytes),
            },
        };
        out.push(ScriptSource { path, body });
    }
    out
}

/// A script body as the brief can carry it: UTF-8, capped, and honest about both.
///
/// The cap is its own number and larger than the 2 KiB an argument gets: an argument
/// preview exists so nobody is made to read a 40 KB file body to approve a one-line
/// edit, and this is the opposite case — the file body IS the thing being judged. Past
/// the cap the head is kept and the omission is stated, so an adjudicator knows it is
/// reading part of a program rather than all of one.
fn bounded(bytes: Vec<u8>) -> ScriptBody {
    const MAX: usize = 16_384;
    match String::from_utf8(bytes) {
        Err(_) => ScriptBody::Unreadable(
            "not UTF-8 — a binary, or text in an encoding this cannot show".into(),
        ),
        Ok(text) if text.len() <= MAX => ScriptBody::Read(text),
        Ok(text) => {
            let mut head = MAX;
            while head > 0 && !text.is_char_boundary(head) {
                head -= 1;
            }
            ScriptBody::Truncated {
                omitted: text.len() - head,
                head: text[..head].to_string(),
            }
        }
    }
}

fn literal_argv(words: &[letibot_code::shell::Word]) -> Vec<String> {
    words
        .iter()
        .filter_map(|w| w.literal())
        .map(str::to_string)
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptBody {
    Read(String),
    /// Read, but longer than the brief carries: the head, and how much went.
    Truncated {
        head: String,
        omitted: usize,
    },
    /// Named but not readable, and why — missing, a directory, not UTF-8.
    Unreadable(String),
}

impl GateCall<'_> {
    /// Whether the `path` argument stays inside the workspace, decided
    /// **lexically** and before the tool runs.
    ///
    /// Lexical on purpose: §11.4 says an action *"whose class says `in_run` but
    /// whose arguments would leave the run"* escalates rather than hard-failing,
    /// and that is a property of the argument, not of what the filesystem happens
    /// to hold. [`crate::backend::HostBackend::resolve`] does the second, stricter
    /// check (it canonicalises, so it catches a symlink out of the tree); this one
    /// exists so the *class* is known before anything is opened.
    pub fn path_is_inside(&self) -> bool {
        let Some(path) = self.args.get("path").and_then(|v| v.as_str()) else {
            return true;
        };
        if path.starts_with('/') {
            return path.starts_with(self.workspace);
        }
        let mut depth: i32 = 0;
        for seg in path.split('/') {
            match seg {
                "" | "." => {}
                ".." => {
                    depth -= 1;
                    if depth < 0 {
                        return false;
                    }
                }
                _ => depth += 1,
            }
        }
        true
    }
}

/// §11's seam, from the tool side.
///
/// Consulted only for a call whose declared access is not [`Access::Read`], which
/// is clause 4 as a control-flow fact: there is no code path from a read-only tool
/// to a question.
///
/// `Send + Sync` closes `TODO.md` T20.4 — *"`ToolRuntime` is not `Send` — `Gate`
/// lacks `Send + Sync`, alone among the runtime's traits. Blocks §13.2's
/// multi-head worker."* It was left for W11 to absorb; absorbing it means writing
/// the bound down, and every implementation in the tree already satisfies it.
pub trait Gate: Send + Sync {
    fn admit(&mut self, call: &GateCall<'_>) -> GateDecision;

    /// **May this session see `path`?** Asked when a call named something the
    /// boundary hid, so the operator gets the question instead of the model
    /// getting a dead end.
    ///
    /// The default is [`ViewGrant::NotAsked`], which is the honest answer for a
    /// gate with nobody behind it: not a refusal (nobody decided) and not silence
    /// (the caller knows it was never put to anyone). A gate that CAN reach a
    /// person overrides this.
    fn grant_view(&mut self, _path: &std::path::Path, _tool: &str) -> ViewGrant {
        ViewGrant::NotAsked
    }

    /// Who is adjudicating, for the daemon's startup disclosure.
    ///
    /// Defaults to naming the absence, because that is the case an operator most
    /// needs to see and the one a hard-coded banner gets wrong: a gate that does
    /// not identify an adjudicator does not have one.
    fn describe(&self) -> String {
        "none (no adjudicator attached)".into()
    }

    /// **Turn supervision on or off while the session is running.**
    ///
    /// > *"I want to start leticode, do /supervise, and move on."*
    ///
    /// Supervision is a property of the gate and not of the mode, which is what makes
    /// that possible: the mode decides *what asks*, and is fixed when a session opens
    /// because the tools seated under it are. This decides *whether the guard model
    /// gets a turn before the answer*, and nothing about a session's shape depends on
    /// it — so it can move without rebuilding anything.
    ///
    /// Returns what to tell the operator, and whether it took. A gate with no advisor
    /// says so rather than reporting success and supervising nothing.
    ///
    /// The default refuses, because a gate that silently accepted the request and
    /// never supervised would be the exact failure the whole feature exists to catch.
    fn set_supervision(&mut self, _on: bool) -> Result<String, String> {
        Err("this gate has no adjudicator, so there is nothing to supervise".into())
    }

    /// Whether the guard model currently gets a turn on every call.
    fn supervising(&self) -> bool {
        false
    }

    /// **Move this gate to another point in mode-space, now.**
    ///
    /// The gate reads its mode at decision time — the decider, the grant scope,
    /// what admits unasked — so moving it is a field write, and everything the
    /// session has accumulated (the audit log, the breaker, the advisor) stays.
    /// What does NOT stay is the standing grants: a grant was an answer to a
    /// question asked under the old point, and a new point is a new question. A
    /// grant kept across a tightening would leak permission; kept across a
    /// loosening it is moot.
    ///
    /// Whether the point's PREREQUISITES are met is the caller's to check — the
    /// gate does not know what backend or oracle is behind it. Returns how many
    /// grants were dropped, for the sentence the operator reads.
    ///
    /// The default refuses, for the reason `set_supervision`'s does: a gate that
    /// accepted the request and kept deciding at the old point would be the mode
    /// saying one thing and the session doing another.
    fn set_mode(&mut self, _mode: crate::mode::Mode) -> Result<usize, String> {
        Err("this gate has no mode to move".into())
    }

    /// **Attach a guard model to a session that opened without one.**
    ///
    /// So that turning supervision on never means restarting anything. The endpoint
    /// is the only expensive part of supervision and it is just an address; requiring
    /// it at daemon start made `/supervise` a lie on every session that had not
    /// thought to pass it, which is every session somebody starts by typing
    /// `leticode`.
    ///
    /// Replaces whatever was there. The default refuses, for `set_supervision`'s
    /// reason.
    fn attach_advisor(
        &mut self,
        _advisor: std::sync::Arc<dyn crate::adjudicate::Adjudicator>,
    ) -> Result<(), String> {
        Err("this gate has no adjudicator, so an advisor would have nothing to advise".into())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum GateDecision {
    Admit,
    /// The call does not happen, and this is the outcome it carries. `Denied` when
    /// somebody decided; `NotRun` when there was nobody to ask.
    Refuse {
        outcome: ToolOutcome,
        /// What the **model** is told, which is not the same as what the audit
        /// records. §11.5: *"the audit is log-only and never enters the model
        /// transcript … the model sees the derived tool outcome, not the
        /// deliberation."* An adjudicator that chose `deny_and_tell` puts its
        /// reason here; a plain `deny` leaves it empty and the reason stays in the
        /// row.
        tell: String,
    },
}

impl GateDecision {
    pub fn refuse(outcome: ToolOutcome) -> Self {
        GateDecision::Refuse {
            outcome,
            tell: String::new(),
        }
    }

    pub fn refuse_and_tell(outcome: ToolOutcome, tell: impl Into<String>) -> Self {
        GateDecision::Refuse {
            outcome,
            tell: tell.into(),
        }
    }
}

/// The gate for a session with **nothing attached**, and it refuses.
///
/// This is deliberately not a `DenyAll` that says "denied": `Denied` means a
/// decision was made, and no decision was made here. §8.2's discipline applied to
/// the outcome vocabulary itself.
///
/// [`crate::adjudicate::AdjudicatedGate::closed`] is the same fail-closed
/// behaviour reached through the full §11.2 shape, and it is what a session that
/// *could* have an adjudicator should use, because it produces an audit row. This
/// one produces none, which is right for a session that has no adjudication at
/// all.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoBoundary;

impl Gate for NoBoundary {
    fn admit(&mut self, call: &GateCall<'_>) -> GateDecision {
        GateDecision::refuse(ToolOutcome::NotRun {
            why: format!(
                "`{}` declares {} access and no adjudicator is attached to this session, \
                 so there is nobody to decide whether it may run. The gate fails closed: \
                 nothing was executed and nothing on disk changed. This is not a denial — \
                 nobody decided. Attach an adjudicator (§11) to make this callable.",
                call.name,
                call.access.as_str()
            ),
        })
    }
}

/// A role's tool set, and §8.4's ceiling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Role {
    pub name: String,
    pub tools: Vec<String>,
    pub max_tools: usize,
}

/// §8.4's default ceiling. *"Past ~5–7 MCP servers small models get worse at
/// choosing tools."* Raised 8 -> 16 (2026-09-13) for leticode, which seats the
/// opencode tool union — a dozen tools — rather than the original eight; the
/// ceiling is still a hard stop, it is just a larger one.
pub const DEFAULT_MAX_TOOLS: usize = 16;

impl Role {
    pub fn new(name: &str, tools: &[&str]) -> Self {
        Role {
            name: name.to_string(),
            tools: tools.iter().map(|t| t.to_string()).collect(),
            max_tools: DEFAULT_MAX_TOOLS,
        }
    }
}

/// §8.4's table, verbatim. Roles naming tools that do not exist yet fail to
/// resolve, loudly, which is how M2's arrival is noticed rather than assumed.
pub mod roles {
    use super::Role;

    pub fn orchestrator() -> Role {
        Role::new(
            "orchestrator",
            // `task_result` beside `task`: `task` hands back a handle now rather
            // than blocking on the child, so a seat with `task` and no way to
            // collect it would be a seat that can start work and never read it.
            // `task_message` completes the trio for the same reason one step on: a seat
            // that can read a child and not correct it can only wait for the wrong answer.
            // `task_start` is seated with `task` for the operator's ask — *"we will need
            // a new tool - task_start or what that will arrange worktree, firecode and
            // subagent"*: a seat that can start a child but not arrange its placement is
            // the seat that spawns it into the main tree and then kills it for being in
            // the wrong place, which is the two mistakes the tool exists to prevent.
            &[
                "task",
                "task_result",
                "task_message",
                "task_start",
                "read",
                "grep",
                "glob",
                "ask_code",
                "ask_corpus",
            ],
        )
    }

    pub fn coder() -> Role {
        Role::new(
            "coder",
            &[
                "read",
                "write",
                "edit",
                "grep",
                "glob",
                "bash",
                "read_spill",
                // A subagent seats this role, and a subagent is the one reader
                // that arrives with none of the conversation it is working on.
                // `transcript` is how it can be told "the operator decided this
                // earlier" and go and check, rather than asking its parent to
                // paste it. Read-only, so it costs the seat nothing else.
                "transcript",
            ],
        )
    }

    /// leticode's seat: opencode's toolset, by opencode's names, plus `skill`.
    ///
    /// Deliberately the opencode union and not the letibot extras — this is the
    /// seat an opencode-shaped agent runs under. `bash` is stripped unless the
    /// daemon was started with `--bash`, exactly as it is for `coder`; `task` and
    /// `lsp` join this list as they land.
    pub fn leticode() -> Role {
        let mut r = Role::new(
            "leticode",
            &[
                "read",
                "write",
                "edit",
                "grep",
                "glob",
                "bash",
                "todo_write",
                "skill",
                "lsp",
                "task",
                "task_result",
                "task_message",
                // `task_start` beside `task`: the operator's ask — *"we will need a new
                // tool - task_start or what that will arrange worktree, firecode and
                // subagent"*. A seat that can start a child but not arrange its placement
                // is the seat that spawns it into the main tree and then kills it for
                // being in the wrong place.
                "task_start",
                // The background-job surface, so a `bash` call that is backgrounded
                // (asked, promoted, or by the operator) can be waited, read, killed
                // and listed — and a condition can be watched across turns. Seated
                // beside `bash`, not instead of it: a background task without
                // `job_wait` is a handle the model cannot follow up on.
                "job_list",
                "job_output",
                "job_wait",
                "job_kill",
                "monitor",
                // `pkill`: find by a string, kill by pid, never itself. Seated
                // with the exec surface, because signalling a process is one.
                "pkill",
                // `ps`: the question `ps | grep -v grep` was asked 294 times to
                // answer (`PS_USE.md`), as a read-only table that never lists
                // this process. Seated with the shell, which is what it looks at.
                "ps",
                // `harness`: what this session is running inside. Read-only, and
                // it is how a model stops asking the operator to read their own
                // terminal aloud.
                "harness",
                // `transcript` and `digest`: this session's own conversation,
                // including what compaction replaced. Seated together on purpose
                // — `transcript` reports the count of what it did not show and
                // names `digest` as the way to ask about the rest, and that offer
                // is a lie on a seat where only one of them is present.
                "transcript",
                "digest",
                // `decisions`: what the gate decided and why. Read-only, and the
                // question it answers — "why was that refused" — is otherwise put
                // to the operator, who has to go and read the corpus themselves.
                "decisions",
            ],
        );
        // Eighteen: the opencode union, the room (`flowy`, seated by the daemon
        // when it holds a seat), `pkill` and `ps`. The ceiling is a guard against
        // a prompt nobody counted, and this is the count, counted. Twenty since
        // `task_result` joined `task`; twenty-two since `transcript` and `digest`,
        // which are one capability seated as two tools; twenty-three since
        // `decisions`, which is the gate's half of the same "read what was
        // already recorded instead of asking" move.
        // **Twenty-four since `task_message`, and this one takes its own seat rather than
        // trading for it** — the operator's ruling, 2026-10-06: *"in the tree all subagents
        // must be addressable by their parents. that is how live corrections delivered."*
        // The seat was already at its ceiling, so nothing was displaced to make room, and
        // that is stated rather than hidden: `task` starts a child, `task_result` reads it
        // and `job_kill` stops it, and without this a parent's only remedies when its child
        // goes down the wrong path are to wait out a wrong answer or throw the work away.
        // **Twenty-five since `task_start`**, the operator's ask — *"we will need a new
        // tool - task_start or what that will arrange worktree, firecode and subagent"* —
        // and it takes its own seat for the same reason: a seat that can start a child but
        // not arrange its placement is the seat that spawns it into the main tree and then
        // kills it for being in the wrong place.
        // Declared here, like `m2_runner`'s ninth, because a ceiling quietly raised for
        // everybody is not a ceiling.
        r.max_tools = 25;
        r
    }

    pub fn researcher() -> Role {
        Role::new(
            "researcher",
            &[
                "ask_corpus",
                "search_corpus",
                "ask_code",
                "read",
                "grep",
                "read_spill",
                // The conversation is a corpus too, and it is the one this seat
                // arrives without. `digest` seats its fold children as
                // `researcher`, so this is also the role that reads a transcript
                // on somebody else's behalf.
                "transcript",
            ],
        )
    }

    pub fn reviewer() -> Role {
        Role::new(
            "reviewer",
            &[
                "read",
                "grep",
                "glob",
                "git",
                "read_spill",
                // "What did the operator actually ask for" is a review question,
                // and the answer is in the conversation rather than the diff.
                "transcript",
            ],
        )
    }

    /// What M1 can actually seat: §8.4's `orchestrator` without `task`, which is
    /// W16's, plus `read_spill`, which is clause 5's own tool.
    ///
    /// `todo_write` is seated here and in [`m3_researcher`] because those are the
    /// two roles with spare seats against §8.4's ceiling, and the todos pane needs
    /// a writer wherever a session runs by default — which is this one. It is the
    /// operator-facing list (whole-replace, persisted to the session store,
    /// announced to heads), not the intent board's `todo`: that one stays the
    /// checked working list for the roles that already seat it, and the two
    /// schemas say so. A role without a spare seat does not get this tool by
    /// taking one from something else; the pane simply stays empty there until a
    /// seat decision says otherwise.
    pub fn m1_orchestrator() -> Role {
        Role::new(
            "orchestrator",
            &[
                "read",
                "grep",
                "glob",
                "ask_code",
                "ask_corpus",
                "read_spill",
                "todo_write",
            ],
        )
    }

    /// §8.4's `researcher` with the web instead of `search_corpus`, which no
    /// build has.
    ///
    /// Seven against a ceiling of eight, and the shape of the table's own
    /// `researcher`: ask the index, ask the web, then read and search the tree.
    /// Four of the seven refuse today — the two retrieval seats have no backend and
    /// the two network seats have no provider — and the role exists so that *"which
    /// tools would this agent have"* is a question with a written answer rather
    /// than one settled per session.
    ///
    /// `todo_write` takes the eighth seat for the same reason it is in
    /// [`m1_orchestrator`]: it is the pane's writer, the role had the room, and
    /// nothing was displaced to make it fit.
    pub fn m3_researcher() -> Role {
        Role::new(
            "researcher",
            &[
                "ask_corpus",
                "ask_code",
                "web_search",
                "web_fetch",
                "read",
                "grep",
                "read_spill",
                "todo_write",
            ],
        )
    }

    /// What a session in plan mode seats: nothing that can change **the work**.
    ///
    /// Plan mode is a **capability boundary**, so this is a role and not a flag —
    /// `write` and `edit` are absent from `tools_json` rather than refused at call
    /// time. [`crate::builtins::intent::plan::seating`] derives the same answer
    /// from an arbitrary base role; this is the named one.
    ///
    /// D9: it keeps `write_plan` (a write scoped to plan documents by taking a
    /// name rather than a path) and `say` (the fabric's chat verb), because a
    /// planner that can neither record nor discuss its plan has to carry the plan
    /// in the context it is about to hand over. Eight tools, which is exactly
    /// [`DEFAULT_MAX_TOOLS`] — this role has no spare seat, and `outline` is what
    /// it gave up to get one.
    pub fn planner() -> Role {
        Role::new(
            "planner",
            &[
                "read",
                "grep",
                "glob",
                "todo",
                "goal",
                "write_plan",
                "say",
                "exit_plan_mode",
            ],
        )
    }

    /// What M2 can seat: §8.4's `coder` without `bash`.
    ///
    /// `bash` is `Access::Exec` and this role deliberately does not name it, even
    /// now that the tool exists: a session seated as `coder` is a session that
    /// edits files, and giving it a shell as a side effect of somebody else's
    /// workstream is how a capability arrives without a decision. `bash` is seated
    /// by [`m2_runner`] and by nothing else. Six tools against a ceiling of eight.
    pub fn m2_coder() -> Role {
        Role::new(
            "coder",
            // `todo` is seated here and the reason is not convenience.
            //
            // It was seated only by `planner`, which has no `write` or `edit` — so no
            // role could both DO the work and record what it meant to do. That put the
            // error signal of `docs/closed-loop.md` §2 in the one role that produces no
            // effects to compare it against: `planner` recorded intentions it could not
            // carry out, `coder` carried out work it could not declare, and
            // `intent::ledger`'s intent-versus-effect diff had nothing to diff.
            //
            // What it cost, observed 2026-09-10 in a real session: asked for a todo
            // list, the model spent 13 tool calls and ~15,000 tokens of reasoning
            // reading `board.rs` and `ledger.rs` to EMULATE the tool it had not been
            // given, then wrote 1,926 tokens describing what it would have printed.
            // That is §4b — a capability that exists but is hidden manufactures the
            // workaround — at maximum price.
            //
            // It needs no gate change: `todo` is `Access::Session` (asserted in
            // `builtins::intent::mod`), so it is neither a read nor a write and nothing
            // the operator owns is touched. With no board mounted it records into this
            // session's own ledger — no node, no token, no network.
            //
            // Seven tools against a ceiling of eight. `goal` is deliberately NOT added
            // with it: a separate capability is a separate decision.
            // `bash` is listed and then STRIPPED unless `--bash` was passed — the same
            // shape `m2_runner` has. Listing it here is what makes the flag mean
            // something for this seat; without the entry the flag would be ignored and
            // the operator would be told a capability was on while it was not.
            //
            // Nine tools without the flag and ten with it — over §8.4's eight, and the
            // seat's own ceiling is `DEFAULT_MAX_TOOLS` (16), so both fit. `goal` is
            // still not here: a separate capability is a separate decision.
            //
            // **`task`, `task_result`, `task_message` and `task_start` are seated here for R58** —
            // *"subagents are absolutely allowed to spawn subagents up to configured nesting
            // level"* — and they are seated rather than stripped at the limit **on purpose**: the
            // depth cap is enforced where the call is made (`HarnessTaskRunner::start`,
            // refused by name against `--max-subagent-depth`), because a seat that simply
            // lacked `task` at the limit manufactures the workaround, which is exactly
            // what this role's own note on `todo` above records a model paying 13 calls
            // and ~15k tokens for. The trio goes together for `orchestrator`'s reason, and
            // `task_message` is the same rule one step on: a parent that can start a child,
            // read it and stop it but not correct it has only the wrong answer to wait for.
            // `task_start` is the operator's ask — *"we will need a new tool - task_start or
            // what that will arrange worktree, firecode and subagent"* — and it is seated with
            // `task` for the same reason: a subagent that can start a child but not arrange its
            // placement is the one that spawns it into the main tree and then kills it for
            // being in the wrong place.
            &[
                "read",
                "write",
                "edit",
                "grep",
                "glob",
                "read_spill",
                "todo",
                "bash",
                "task",
                "task_result",
                "task_message",
                "task_start",
            ],
        )
    }

    /// The only role that can run a command.
    ///
    /// **Nine tools, one over §8.4's ceiling, and the overrun is declared rather
    /// than absorbed.** The arithmetic: the exec surface is five seats (`bash`
    /// plus the four job verbs, which cannot be fewer — starting, watching,
    /// reading and stopping are four different questions and three harnesses
    /// independently found the same shape), `monitor` is the sixth, and `read`,
    /// `grep` and `read_spill` take the rest.
    ///
    /// What that gives up is `glob`, and it is given up on purpose: a session with
    /// a shell has a worse-but-real substitute for it in `ls` and `find`, and it
    /// has **no** substitute for `read_spill`, which is what makes clause 5's
    /// "bounded, never truncated" true rather than a slogan. Dropping the honest
    /// one to keep the convenient one would be trading a correctness property for
    /// a search.
    ///
    /// # Why the ninth seat is taken rather than traded for
    ///
    /// The ceiling's evidence is *"past ~5–7 MCP servers small models get worse at
    /// choosing tools"* — it is about **confusion between similar choices**. The
    /// monitor surface was cut twice against exactly that before it was allowed to
    /// cost a seat:
    ///
    /// - declaring, renewing and retiring are **one** tool taking an `action`,
    ///   because all three act on the same named handle;
    /// - listing monitors is **not a tool at all**. It is in `job_list`, next to
    ///   the jobs, the scopes, the promotions and the reap log, because "what is
    ///   running and what is watching" is one question.
    ///
    /// What is left cannot be folded into `job_wait` without making the model's
    /// worst mistake here spellable: `job_wait` blocks **inside** the turn and a
    /// monitor watches **across** turns, and a flag that switched between them
    /// would let a model believe it had waited when it had not. T24 names them as
    /// two primitives for that reason.
    ///
    /// So `max_tools` is 9 here and [`DEFAULT_MAX_TOOLS`] everywhere else. A
    /// ceiling that is quietly raised for everybody is not a ceiling; one role
    /// declaring its own number, with the trade written down, is a decision
    /// somebody can reverse.
    pub fn m2_runner() -> Role {
        let mut r = Role::new(
            "runner",
            &[
                "read",
                "grep",
                "read_spill",
                "bash",
                "job_list",
                "job_output",
                "job_wait",
                "job_kill",
                "monitor",
            ],
        );
        r.max_tools = 9;
        r
    }

    /// The gatekeeper's seat: read-only by construction, and blind to the child's
    /// report.
    ///
    /// The seat is the capability half of the gatekeeper ([`crate::gatekeeper`]):
    /// what a reviewer may reach. It may read, grep, glob, and run `bash` for
    /// `git diff` and `git log` — the artifact is the diff, and the history is the
    /// context. It has **no** `write`, no `edit`, no `task`, and nothing else that
    /// can author code. A reviewer that can edit becomes a second author and then
    /// nobody reviewed.
    ///
    /// It is also deliberately blind to the child's report. `transcript` is the one
    /// read-only tool that would break that blindness — the child's final answer is
    /// in the conversation, and a reviewer that reads it reviews the story rather
    /// than the artifact — so it is excluded on purpose, for the same reason the
    /// request has no field for a report. See [`crate::gatekeeper`].
    ///
    /// Five tools against a ceiling of sixteen: `read`, `grep`, `glob`, `bash` and
    /// `read_spill`. `read_spill` is seated beside `bash` for the same reason
    /// [`m2_runner`] seats it — a `git diff` on a large branch spills, and a seat
    /// that cannot read back its own spilled output is a seat that cannot review a
    /// large change.
    pub fn gatekeeper() -> Role {
        Role::new(
            "gatekeeper",
            &["read", "grep", "glob", "bash", "read_spill"],
        )
    }
}

#[derive(Debug)]
pub enum RegisterError {
    Duplicate(String),
    /// Clause 6, refused at registration rather than in review.
    Description {
        tool: String,
        findings: Vec<crate::schema::DescriptionFinding>,
    },
}

impl std::fmt::Display for RegisterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegisterError::Duplicate(n) => write!(f, "a tool called `{n}` is already registered"),
            RegisterError::Description { tool, findings } => write!(
                f,
                "`{tool}`'s description says what the data contains, which clause 6 forbids: {}",
                findings
                    .iter()
                    .map(|x| x.to_string())
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
        }
    }
}

impl std::error::Error for RegisterError {}

#[derive(Debug)]
pub enum RoleError {
    /// The resolved tool count exceeds the ceiling, and the overflow is named.
    OverBudget {
        role: String,
        count: usize,
        max: usize,
        overflow: Vec<String>,
    },
    /// A role naming a tool this build does not have. Refused rather than silently
    /// seated with a smaller set, for the same reason an MCP server over budget is
    /// refused with its tool list rather than truncated.
    Unknown { role: String, names: Vec<String> },
}

impl std::fmt::Display for RoleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RoleError::OverBudget {
                role,
                count,
                max,
                overflow,
            } => write!(
                f,
                "role `{role}` resolves to {count} tools and the ceiling is {max}; \
                 over by {}: {}",
                overflow.len(),
                overflow.join(", ")
            ),
            RoleError::Unknown { role, names } => write!(
                f,
                "role `{role}` names {} tool(s) this build does not have: {}",
                names.len(),
                names.join(", ")
            ),
        }
    }
}

impl std::error::Error for RoleError {}

/// The tools a session may call.
#[derive(Default)]
pub struct Registry {
    tools: Vec<Box<dyn Tool>>,
    /// Descriptions that failed the lint but were registered anyway, from
    /// [`Registry::register_foreign`]. Surfaced rather than swallowed.
    pub foreign_findings: Vec<(String, Vec<crate::schema::DescriptionFinding>)>,
}

impl std::fmt::Debug for Registry {
    /// By name. A `Box<dyn Tool>` has nothing else to show, and a registry that
    /// cannot be printed makes every `Result` around it awkward.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field("tools", &self.names())
            .field("foreign_findings", &self.foreign_findings)
            .finish()
    }
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one of ours. Clause 6 is enforced here.
    pub fn register(&mut self, tool: Box<dyn Tool>) -> Result<(), RegisterError> {
        let schema = tool.schema();
        if self.get(&schema.name).is_some() {
            return Err(RegisterError::Duplicate(schema.name));
        }
        let findings = lint_description(&schema.description);
        if !findings.is_empty() {
            return Err(RegisterError::Description {
                tool: schema.name,
                findings,
            });
        }
        self.tools.push(tool);
        Ok(())
    }

    /// Register a tool whose description we do not control — an MCP server's, in
    /// M3. The lint still runs; its findings are recorded rather than fatal,
    /// because refusing somebody else's server over its prose would be a harness
    /// making a policy nobody asked for.
    pub fn register_foreign(&mut self, tool: Box<dyn Tool>) -> Result<(), RegisterError> {
        let schema = tool.schema();
        if self.get(&schema.name).is_some() {
            return Err(RegisterError::Duplicate(schema.name));
        }
        let findings = lint_description(&schema.description);
        if !findings.is_empty() {
            self.foreign_findings.push((schema.name.clone(), findings));
        }
        self.tools.push(tool);
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools
            .iter()
            .find(|t| t.schema().name == name)
            .map(|t| t.as_ref())
    }

    pub fn names(&self) -> Vec<String> {
        self.tools.iter().map(|t| t.schema().name).collect()
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// The schemas, in registration order. Order is part of the stable prefix, so
    /// this is the order the prompt is built in and it must not depend on a hash
    /// map's iteration.
    pub fn schemas(&self) -> Vec<ToolSchema> {
        self.tools.iter().map(|t| t.schema()).collect()
    }

    /// `StablePrefix::tools_json`, ready for the dialect.
    pub fn tools_json(&self) -> Vec<String> {
        self.tools
            .iter()
            .map(|t| t.schema().prompt_json())
            .collect()
    }

    /// §8.4, enforced: **refuse to seat a role** whose resolved tool count exceeds
    /// its ceiling, naming the overflow, and refuse one that names a tool this
    /// build does not have.
    /// Drop every tool of a denied access class. A subagent's downgrade, applied
    /// after the role is resolved: the tools leave the prompt entirely, so the
    /// model is not told it has a capability the gate would refuse.
    pub fn without_access(
        mut self,
        denied: &std::collections::BTreeSet<crate::schema::Access>,
    ) -> Registry {
        if denied.is_empty() {
            return self;
        }
        self.tools.retain(|t| !denied.contains(&t.schema().access));
        self
    }

    pub fn resolve_role(self, role: &Role) -> Result<Registry, RoleError> {
        let have = self.names();
        let missing: Vec<String> = role
            .tools
            .iter()
            .filter(|t| !have.contains(t))
            .cloned()
            .collect();
        if !missing.is_empty() {
            return Err(RoleError::Unknown {
                role: role.name.clone(),
                names: missing,
            });
        }
        if role.tools.len() > role.max_tools {
            return Err(RoleError::OverBudget {
                role: role.name.clone(),
                count: role.tools.len(),
                max: role.max_tools,
                overflow: role.tools[role.max_tools..].to_vec(),
            });
        }
        let mut kept = Registry::new();
        kept.foreign_findings = self.foreign_findings;
        // Role order, not registration order: the role is what the prompt is built
        // from, and it is written down in one place.
        let mut tools = self.tools;
        for name in &role.tools {
            if let Some(i) = tools.iter().position(|t| &t.schema().name == name) {
                kept.tools.push(tools.remove(i));
            }
        }
        Ok(kept)
    }
}

/// The runtime proper.
pub struct ToolRuntime {
    pub registry: Registry,
    pub backend: Box<dyn ExecBackend>,
    pub spiller: Spiller,
    pub gate: Box<dyn Gate>,
    pub limits: Limits,
    /// Session-scoped, and on the runtime rather than on a tool because
    /// read-before-write is a fact about the *session*, not about `edit`.
    pub files: crate::files::FileLedger,
    /// See [`OperatorWaiting`]. `None` in a runtime with no head behind it.
    pub operator_waiting: Option<OperatorWaiting>,
    /// See [`CompletionDelivered`]. `None` in a runtime with no job watcher behind
    /// it — a harness driven directly by a test, or a backend that cannot start
    /// processes.
    pub completion_delivered: Option<CompletionDelivered>,
    /// See [`OperatorRun`]. `None` in a runtime with no daemon behind it, and the
    /// operator's own run then raises no card at all — the manual way in is unaffected.
    pub operator_runs: Option<OperatorRuns>,
}

impl ToolRuntime {
    pub fn new(registry: Registry, backend: Box<dyn ExecBackend>) -> Self {
        ToolRuntime {
            registry,
            backend,
            spiller: Spiller::unset(),
            // Fail closed by default: a runtime nobody configured refuses every
            // non-read call rather than allowing it.
            gate: Box::new(NoBoundary),
            limits: Limits::default(),
            files: crate::files::FileLedger::new(),
            operator_waiting: None,
            completion_delivered: None,
            operator_runs: None,
        }
    }

    /// Wire the question a long-running tool asks before it spends another minute.
    pub fn with_operator_waiting(mut self, f: OperatorWaiting) -> Self {
        self.operator_waiting = Some(f);
        self
    }

    /// **Wire the fact that makes `job_wait` refuse to block on its own promise.**
    ///
    /// See [`CompletionDelivered`]. A daemon supplies a closure over its own
    /// per-session job watchers; a runtime that wires nothing keeps the old
    /// behaviour, which R23 leaves exactly as it was.
    pub fn with_completion_delivered(mut self, f: CompletionDelivered) -> Self {
        self.completion_delivered = Some(f);
        self
    }

    /// **Wire what the daemon is told about the operator's own run.** See [`OperatorRun`].
    ///
    /// A closure and not a trait object with named methods, for the reason every seam in
    /// this file is one: the daemon owns the state the three events are *about*, and a
    /// trait here would be this crate inventing a vocabulary for a card, a session and a
    /// hub it must not know exist.
    pub fn with_operator_runs(mut self, f: OperatorRuns) -> Self {
        self.operator_runs = Some(f);
        self
    }

    pub fn with_spiller(mut self, spiller: Spiller) -> Self {
        self.spiller = spiller;
        self
    }

    pub fn with_gate(mut self, gate: Box<dyn Gate>) -> Self {
        self.gate = gate;
        self
    }

    /// The scripts a `bash` call's command will run, read for the adjudicator.
    ///
    /// Empty for every other tool and for every command that names no script
    /// file: `python3 -c` carries its program in the argv, which the brief
    /// already renders verbatim.
    ///
    /// A file that cannot be read is reported as unreadable rather than skipped.
    /// Silence would let the brief say nothing about a script that exists, which
    /// reads to an adjudicator exactly like a command that runs no script at all
    /// — and those are the two cases it most needs to tell apart.
    fn scripts_of(&self, args: &Value) -> Vec<ScriptSource> {
        let Some(command) = args.get("command").and_then(|v| v.as_str()) else {
            return Vec::new();
        };
        scripts_for(command, self.backend.home_path().as_deref(), |path| {
            self.backend.read(path).map_err(|e| e.to_string())
        })
    }

    /// **Did a file the gate judged change before the command ran?** — R39's third case,
    /// checked rather than assumed away.
    ///
    /// `None` when every body that was READ is still exactly those bytes. Only read bodies
    /// are re-read: an unreadable one contributed nothing to the judgement, and its finding
    /// already says so — re-reading it would be inventing a new question rather than
    /// answering this one.
    ///
    /// The comparison is `ScriptBody` equality, which is the right instrument because it is
    /// the same bounding the brief used: a file that grew past the 16 KiB cap differs in its
    /// `omitted` count, and a file that shrank differs in its head, so a change beyond the
    /// part that was shown is still a change.
    ///
    /// Refuses rather than re-judging. A second pass through the gate would be a second
    /// decision the operator never saw, and an adjudicator asked twice about the same
    /// command is an adjudicator whose first answer means nothing.
    fn script_changed_since_the_gate(
        &self,
        scripts: &[ScriptSource],
    ) -> Option<(ToolOutcome, String)> {
        for s in scripts {
            if !matches!(s.body, ScriptBody::Read(_) | ScriptBody::Truncated { .. }) {
                continue;
            }
            let now = match self.backend.read(&s.path) {
                Ok(bytes) => bounded(bytes),
                Err(e) => ScriptBody::Unreadable(e.to_string()),
            };
            if now == s.body {
                continue;
            }
            return Some((
                ToolOutcome::Failed {
                    reason: format!(
                        "`{}` changed between being classified and being run",
                        s.path
                    ),
                },
                format!(
                    "`{}` changed between being classified and being run, so the command \
                     that was approved is not the command this would start. Nothing ran.\n\n\
                     The gate judged the file's bytes at classification time — layer A and \
                     the adjudicator were both shown them — and it is a file, so nothing stops \
                     it changing while an answer is being waited for. Ask again and the gate \
                     will read what is there now.",
                    s.path
                ),
            ));
        }
        None
    }

    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Run one call the model proposed.
    pub fn invoke(
        &mut self,
        turn_id: &str,
        call: &ToolCall,
        sink: &mut dyn ToolEventSink,
    ) -> ToolResult {
        self.run(turn_id, call, sink, true)
    }

    /// **Run one call the OPERATOR made**, which the admission has already answered.
    ///
    /// R31. The gate is not consulted, and that is the whole of the difference: an
    /// operator's call has already been through the door — admitted against the allowlist,
    /// written to the corpus as `human:<who>`, published as `OperatorCallAllowed` — so there
    /// is nobody left to ask. The corpus row for it says so in as many words (*"the operator
    /// ran this from their own console; there was nobody left to ask"*), and asking again
    /// here would either double-record the decision or, at `/mode automode`, let a model
    /// refuse the person's own act.
    ///
    /// **Everything else is identical**, and that is deliberate: the same schema lookup, the
    /// same argument salvage, the same byte limits, the same spill policy, the same network
    /// rules. The point of running it here rather than in a head is that the payload is what
    /// *this* program would have returned, so the corpus row and the model's next prompt are
    /// about one tool rather than two.
    pub fn invoke_operator(
        &mut self,
        turn_id: &str,
        call: &ToolCall,
        sink: &mut dyn ToolEventSink,
    ) -> ToolResult {
        self.run(turn_id, call, sink, false)
    }

    /// The body of both, with the gate the one thing that varies.
    fn run(
        &mut self,
        turn_id: &str,
        call: &ToolCall,
        sink: &mut dyn ToolEventSink,
        gated: bool,
    ) -> ToolResult {
        let Some(schema) = self.registry.get(&call.name).map(|t| t.schema()) else {
            // An unknown tool name is a miss, and clause 1 applies to it: the model
            // gets the list it can choose from and the nearest thing to what it
            // asked for, in the same call.
            let names = self.registry.names();
            let near = nearest(&call.name, &names);
            let mut payload = format!("this session has these tools: {}", names.join(", "));
            if let Some(n) = near {
                payload.push_str(&format!("\nthe nearest to `{}` is `{n}`", call.name));
            }
            let r = ToolResult::new(
                call.id.clone(),
                call.name.clone(),
                ToolOutcome::Failed {
                    reason: format!("no tool called `{}` is available here", call.name),
                },
            )
            .with_payload(payload);
            sink.emit(finished_event(turn_id, &r));
            return r;
        };

        // Clause 2, before anything else looks at the arguments.
        let (args, repairs) = match salvage(&call.arguments, &schema) {
            Ok(s) => (s.value, s.repairs),
            Err(e) => {
                let r = ToolResult::new(
                    call.id.clone(),
                    call.name.clone(),
                    ToolOutcome::Failed { reason: e.reason },
                )
                .with_payload(e.guidance);
                sink.emit(finished_event(turn_id, &r));
                return r;
            }
        };

        // Clause 4. The gate is consulted **only** when the declared access is not
        // read: a read-only tool has no code path to a question.
        // What this particular call amounts to, which for a tool with verbs of
        // different kinds is not what the schema says. Narrowing only: the tool's
        // answer is used when it is LESS consequential than the declared class, so
        // a tool cannot declare its way past the gate.
        let declared = schema.access;
        let effective = self
            .registry
            .get(&call.name)
            .and_then(|t| t.access_for(&args))
            .filter(|a| a.is_unattended() && !declared.is_unattended())
            .unwrap_or(declared);
        if gated && !effective.is_unattended() {
            // **The workspace, not the root — and the difference is R18.**
            //
            // This asked the backend for `root_path()`. For an unconfined session that
            // is `/`, because *unconfined* means the whole host and `/` is the right
            // answer to *is this path inside the boundary*. It is the wrong answer to
            // *which directory is this session working in*, and this string is the
            // first boundary fact — which the head prints as `because: …` on the gate
            // card. Measured 2026-09-22: four consecutive cards in a session started
            // with `--workspace /home/dead/Projects/letibot` said
            // `because: workspace: /`, including two ordinary `cargo test` calls.
            //
            // `workspace_path()` answers the second question; `root_path()` stays as
            // the fallback for a substrate that only knows its root, and `describe()`
            // (a sentence, not a path) as the last resort rather than the second.
            let workspace = self
                .backend
                .workspace_path()
                .or_else(|| self.backend.root_path())
                .unwrap_or_else(|| self.backend.describe());
            let target_exists = args
                .get("path")
                .and_then(|v| v.as_str())
                .map(|p| self.backend.stat(p).is_some());
            // **The code an interpreter was handed.** Read here because this is
            // where the backend is — and through the backend, so a confined
            // session reads its own copy rather than the host's.
            let scripts = self.scripts_of(&args);
            let gate_call = GateCall {
                name: &schema.name,
                access: schema.access,
                args: &args,
                turn_id,
                call_id: &call.id,
                workspace: &workspace,
                target_exists,
                scripts: &scripts,
            };
            if let GateDecision::Refuse { outcome, tell } = self.gate.admit(&gate_call) {
                let r =
                    ToolResult::new(call.id.clone(), call.name.clone(), outcome).with_payload(tell);
                sink.emit(finished_event(turn_id, &r));
                return r;
            }
            // **The file can change while the gate is answering, and this is the one place
            // the difference is actionable** (R39). A here-document body cannot: it is in
            // the command text, so it is the same bytes when it is judged and when it runs.
            // A file on disk is not — and `gate.admit` above may have waited minutes for an
            // operator to answer, which is exactly the window in which the bytes layer A
            // and the oracle both judged can stop being the bytes that would run.
            //
            // So it is CHECKED rather than merely noted, at the last moment before the
            // command starts. Only the bodies that were actually read are re-read: nothing
            // was judged from an unreadable one, and the finding already says so. A
            // mismatch refuses rather than re-judging, because a second gate pass would be
            // a second decision the operator never saw — and it refuses with the file named
            // and the remedy in the note, because *the thing you approved is not the thing
            // that is here* is not something to leave a reader to deduce.
            if let Some(r) = self.script_changed_since_the_gate(&scripts) {
                let r = ToolResult::new(call.id.clone(), call.name.clone(), r.0).with_payload(r.1);
                sink.emit(finished_event(turn_id, &r));
                return r;
            }
        }

        sink.emit(ToolEvent::Started {
            turn_id: turn_id.to_string(),
            call_id: call.id.clone(),
            name: schema.name.clone(),
            access: schema.access,
        });

        let invocation = {
            let tool = self
                .registry
                .get(&call.name)
                .expect("the schema was found a moment ago");
            let mut ctx = InvokeCtx {
                backend: self.backend.as_ref(),
                spiller: &self.spiller,
                files: &self.files,
                limits: self.limits,
                turn_id,
                call_id: &call.id,
                sink,
                operator_waiting: self.operator_waiting.as_ref(),
                completion_delivered: self.completion_delivered.as_ref(),
                operator_runs: self.operator_runs.as_ref(),
                // The operator's own run, and only it. `run`'s flag is the whole
                // distinction between the two entries — see `InvokeCtx::tty`.
                tty: !gated,
            };
            tool.invoke(&mut ctx, &args)
        };

        // **A path the boundary hid is a question, not a dead end.**
        //
        // Until this existed, a command that named something outside the session's
        // view got an `ENOENT` and a note saying the path "has to be granted into
        // the view by whoever opened the session" — with nothing in the system able
        // to ask that person. So the model's only move was to hand the operator
        // shell commands to run themselves, which is the route-around this tree
        // refuses everywhere else. The operator's report was exactly that: *"i
        // asked to do the symlink and it didnt even fallback to asking me"*.
        //
        // The decision is the gate's, so it lands in the corpus beside every other
        // one, and the options are the operator's own words: read-only, writable,
        // or no. An approval re-probes the boundary — see
        // `HostBackend::grant_into_view` — so the NEXT call sees the path; this one
        // is not retried, because its output is already written and re-running a
        // command on the model's behalf is a decision nobody made.
        let mut invocation = invocation;
        if !invocation.needs_in_view.is_empty() {
            let asked: Vec<std::path::PathBuf> = std::mem::take(&mut invocation.needs_in_view);
            for path in asked {
                // **An operator's own call does not raise a view-grant card.** The ungated
                // path (`invoke_operator`) is the one the head-run door and the `!` line
                // take, and its whole ruling is *nobody left to ask*: a card that asks the
                // operator to grant a path into view for a command THEY JUST TYPED is the
                // gate answering a question nobody asked. Nothing is lost by dropping the
                // ask, because `bash` has already put the same finding in its notes —
                // `absence_notes` and `outside_paths` scan the same output for the same
                // Outside paths — so the row still names what the boundary hid.
                if !gated {
                    continue;
                }
                match self.gate.grant_view(&path, &schema.name) {
                    ViewGrant::Refused(why) => {
                        invocation.notes.push(format!(
                            "`{}` was NOT granted into this session's view: {why}",
                            path.display()
                        ));
                    }
                    ViewGrant::NotAsked => {}
                    ViewGrant::Granted { writable } => {
                        match self.backend.grant_into_view(
                            &path,
                            writable,
                            &format!("granted mid-session for `{}`", schema.name),
                        ) {
                            Ok(view) => invocation.notes.push(format!(
                                "`{}` is now in this session's view{}, by your answer. \
                                 The command above already ran without it — run it again \
                                 and it will see the path. The view is now: {view}",
                                path.display(),
                                if writable {
                                    ", writable"
                                } else {
                                    ", read-only"
                                },
                            )),
                            Err(e) => invocation.notes.push(format!(
                                "`{}` was approved but could not be bound into the view, \
                                 so nothing changed: {e}",
                                path.display()
                            )),
                        }
                    }
                }
            }
        }

        // Clause 5, on every payload and not only the ones somebody remembered.
        //
        // The budget bounds the **tool's output**. The envelope and the `[note]`
        // lines are the harness's own bytes, are bounded by construction, and are
        // added after: a policy that counted them would make the model's share of
        // its own budget depend on how many repairs its arguments needed.
        let produced = invocation.payload.len();
        let (payload, spill) = self.spiller.apply(
            invocation.payload,
            &SpillContext {
                tool: &schema.name,
                args: &args,
                bytes: produced,
            },
        );

        let result = ToolResult {
            call_id: call.id.clone(),
            name: call.name.clone(),
            outcome: invocation.outcome,
            payload,
            repairs,
            notes: invocation.notes,
            spill,
            // Never spilled and never truncated: this is the head's copy, not the
            // model's, and clause 5 bounds what goes into the prompt. A head that
            // was handed half a file could not draw a diff at all.
            edit: invocation.edit,
            // **Carried through untouched, and deliberately NOT spilled or truncated.** The spill
            // path above exists to bound what goes into the PROMPT — a hundred thousand lines of
            // `cargo test` output — and an image is not that: it is bounded by its own format, the
            // model server resizes past 4.19 MP rather than refusing (measured; see
            // `crate::media`), and a truncated base64 is not a smaller picture, it is a corrupt one.
            // So this rides the same rule `edit` does above, for the opposite reason.
            media: invocation.media,
        };
        sink.emit(finished_event(turn_id, &result));
        result
    }

    /// The transcript row for a finished call. The payload is the **rendered**
    /// result, envelope included, because that is the byte sequence the next
    /// prompt replays. The row also carries the bounded edit excerpt, the same
    /// one the event carries: the row is what a store persists and what a
    /// resumed head replays, so without it the panels died with the turn.
    pub fn transcript_item(result: &ToolResult) -> TranscriptItem {
        TranscriptItem::ToolResult {
            call_id: result.call_id.clone(),
            name: result.name.clone(),
            outcome: result.outcome.clone(),
            payload: result.render(),
            edit: bounded_edit(result),
            origin: None,
            // **Carried whole, and the difference from `bounded_edit` one line up is the design.**
            // An edit excerpt is bounded because a head only has to DRAW it — three lines of context
            // are enough to see what changed. An image is SENT, and a bounded image is not a smaller
            // picture: it is a truncated `data:` URI, which the server refuses or renders as
            // garbage. The bound that applies to media is the file's own format plus the server's
            // own limit (measured: it resizes past 4.19 MP and never refuses), and a head that
            // imposed a second one would be discarding what the far end would have taken — the
            // failure the operator named, where an attachment dropped by the head looks exactly
            // like a picture the model ignored.
            media: result.media.clone(),
        }
    }
}

/// The excerpt both the [`ToolEvent::Finished`] and the transcript row carry:
/// three lines of context either side of the change, four hundred lines the
/// cap, so the fan-out cost is known here and not a property of whatever file
/// the model chose. One bound, two carriers — they must not drift apart.
fn bounded_edit(r: &ToolResult) -> Option<crate::edit::ToolEditExcerpt> {
    r.edit.as_ref().map(|e| e.excerpt(3, 400))
}

fn finished_event(turn_id: &str, r: &ToolResult) -> ToolEvent {
    let rendered = r.render();
    ToolEvent::Finished {
        turn_id: turn_id.to_string(),
        call_id: r.call_id.clone(),
        outcome: r.outcome.clone(),
        payload_digest: payload_digest(&rendered),
        inline_bytes: rendered.len() as u64,
        full_bytes: r
            .spill
            .as_ref()
            .map(|s| s.full_bytes as u64)
            .unwrap_or(rendered.len() as u64),
        spill: r.spill.as_ref().map(|s| s.hash.clone()),
        repairs: r.repairs.len() as u32,
        edit: bounded_edit(r),
    }
}

/// The nearest known name, by a cheap edit distance. Used only to *suggest*.
fn nearest(want: &str, have: &[String]) -> Option<String> {
    have.iter()
        .map(|h| (distance(want, h), h))
        .filter(|(d, h)| *d <= h.len().max(want.len()) / 2)
        .min_by_key(|(d, _)| *d)
        .map(|(_, h)| h.clone())
}

fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// The repairs a result carries, as one line for a head.
pub fn repair_summary(repairs: &[Repair]) -> String {
    repairs
        .iter()
        .map(|r| r.code)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::RecordingToolSink;

    struct Probe {
        access: Access,
    }

    impl Tool for Probe {
        fn schema(&self) -> ToolSchema {
            ToolSchema::new(
                "probe",
                "Do a thing. Takes a path.",
                serde_json::json!({
                    "type": "object",
                    "properties": {"path": {"type": "string"}},
                    "required": ["path"],
                }),
                self.access,
            )
        }
        fn invoke(&self, _ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
            Invocation::ok(format!("probed {}", args["path"]))
        }
    }

    /// **A tool that runs a command line**, for R39's changed-file check — the real
    /// `scripts_of` reads any call whose arguments carry a `command`, so the check is
    /// exercised through the same path a `bash` call takes without needing a process host.
    struct CommandProbe {
        ran: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl Tool for CommandProbe {
        fn schema(&self) -> ToolSchema {
            ToolSchema::new(
                "run",
                "Run a command line.",
                serde_json::json!({
                    "type": "object",
                    "properties": {"command": {"type": "string"}},
                    "required": ["command"],
                }),
                Access::Exec,
            )
        }
        fn invoke(&self, _ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
            self.ran.store(true, std::sync::atomic::Ordering::SeqCst);
            Invocation::ok(format!("ran {}", args["command"]))
        }
    }

    /// **A gate that answers by changing the file it was asked about.** That is exactly the
    /// window R39 names — the file is read, an answer is waited for, and the answer takes
    /// time — reproduced in one process rather than described in a comment.
    struct RewritingGate {
        path: std::path::PathBuf,
        to: String,
    }

    impl Gate for RewritingGate {
        fn admit(&mut self, _call: &GateCall<'_>) -> GateDecision {
            std::fs::write(&self.path, &self.to).unwrap();
            GateDecision::Admit
        }
    }

    /// **A file the gate judged that changed before the command ran is refused, not run.**
    ///
    /// The here-document case cannot have this window: its body is in the command text. A
    /// path can, and the window is the operator's own thinking time — so it is checked at
    /// the last moment before the command starts rather than noted and left.
    #[test]
    fn a_script_that_changed_while_the_gate_was_answering_is_refused() {
        let d = crate::backend::tempdir::TempDir::new();
        let script = d.path().join("deploy.py");
        std::fs::write(&script, "print('judged')\n").unwrap();

        let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut reg = Registry::new();
        reg.register(Box::new(CommandProbe { ran: ran.clone() }))
            .unwrap();
        let backend = crate::backend::HostBackend::new(d.path()).unwrap();
        let mut rt = ToolRuntime::new(reg, Box::new(backend)).with_gate(Box::new(RewritingGate {
            path: script.clone(),
            to: "print('swapped')\n".into(),
        }));
        let mut sink = RecordingToolSink::default();
        let r = rt.invoke(
            "t1",
            &call("run", r#"{"command":"python3 deploy.py"}"#),
            &mut sink,
        );
        assert!(
            !ran.load(std::sync::atomic::Ordering::SeqCst),
            "the command ran with a body nobody judged: {r:?}"
        );
        let payload = format!("{r:?}");
        assert!(
            payload.contains("changed between being classified and being run")
                && payload.contains("deploy.py")
                && payload.contains("Ask again"),
            "the refusal does not say what happened or what to do: {payload}"
        );
    }

    /// **And the ordinary case is untouched**: a file that did not change runs. Without
    /// this, the test above would pass on a runtime that refused everything.
    #[test]
    fn a_script_that_did_not_change_is_not_refused() {
        let d = crate::backend::tempdir::TempDir::new();
        std::fs::write(d.path().join("deploy.py"), "print('judged')\n").unwrap();

        let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut reg = Registry::new();
        reg.register(Box::new(CommandProbe { ran: ran.clone() }))
            .unwrap();
        let backend = crate::backend::HostBackend::new(d.path()).unwrap();
        let mut rt = ToolRuntime::new(reg, Box::new(backend)).with_gate(Box::new(AdmitGate));
        let mut sink = RecordingToolSink::default();
        let r = rt.invoke(
            "t1",
            &call("run", r#"{"command":"python3 deploy.py"}"#),
            &mut sink,
        );
        assert!(
            ran.load(std::sync::atomic::Ordering::SeqCst),
            "an unchanged script was refused: {r:?}"
        );
    }

    struct AdmitGate;

    impl Gate for AdmitGate {
        fn admit(&mut self, _call: &GateCall<'_>) -> GateDecision {
            GateDecision::Admit
        }
    }

    struct ExplodingGate;

    impl Gate for ExplodingGate {
        fn admit(&mut self, call: &GateCall<'_>) -> GateDecision {
            panic!("a read-only tool must never reach the gate: {}", call.name);
        }
    }

    fn runtime(access: Access) -> ToolRuntime {
        let mut reg = Registry::new();
        reg.register(Box::new(Probe { access })).unwrap();
        let d = crate::backend::tempdir::TempDir::new();
        let backend = crate::backend::HostBackend::new(d.path()).unwrap();
        // The temp dir outlives the backend only within one test; leak it there
        // rather than complicate every caller.
        std::mem::forget(d);
        ToolRuntime::new(reg, Box::new(backend)).with_gate(Box::new(ExplodingGate))
    }

    fn call(name: &str, args: &str) -> ToolCall {
        ToolCall {
            id: "c0".into(),
            name: name.into(),
            arguments: args.into(),
        }
    }

    #[test]
    fn without_access_drops_a_class_and_leaves_the_rest_in_role_order() {
        let mut reg = Registry::new();
        reg.register(Box::new(Probe {
            access: Access::Read,
        }))
        .unwrap();
        let mut denied = std::collections::BTreeSet::new();
        denied.insert(Access::Read);
        assert!(reg.schemas().iter().any(|s| s.name == "probe"));
        let reg = reg.without_access(&denied);
        assert!(reg.schemas().is_empty());
        let mut reg = Registry::new();
        reg.register(Box::new(Probe {
            access: Access::Read,
        }))
        .unwrap();
        let mut other = std::collections::BTreeSet::new();
        other.insert(Access::Exec);
        assert_eq!(reg.without_access(&other).schemas().len(), 1);
    }

    #[test]
    fn a_read_only_tool_never_reaches_the_gate() {
        // Clause 4, as a control-flow fact rather than a promise: the gate panics
        // if it is consulted, and this call must not consult it.
        let mut rt = runtime(Access::Read);
        let mut sink = RecordingToolSink::new();
        let r = rt.invoke("t1", &call("probe", r#"{"path":"a"}"#), &mut sink);
        assert_eq!(r.outcome, ToolOutcome::Ok);
    }

    #[test]
    fn a_write_tool_does_and_m1_has_nobody_to_ask() {
        let mut reg = Registry::new();
        reg.register(Box::new(Probe {
            access: Access::Write,
        }))
        .unwrap();
        let d = crate::backend::tempdir::TempDir::new();
        let backend = crate::backend::HostBackend::new(d.path()).unwrap();
        let mut rt = ToolRuntime::new(reg, Box::new(backend));
        let mut sink = RecordingToolSink::new();
        let r = rt.invoke("t1", &call("probe", r#"{"path":"a"}"#), &mut sink);
        match r.outcome {
            // `NotRun`, not `Denied`: nobody decided anything.
            ToolOutcome::NotRun { why } => assert!(why.contains("fails closed"), "{why}"),
            other => panic!("a write tool must not run unattended: {other:?}"),
        }
        assert!(
            !sink.kinds().contains(&"ToolStarted"),
            "a refused call never started"
        );
    }

    #[test]
    fn an_unknown_tool_comes_back_with_the_list_and_the_nearest_name() {
        let mut rt = runtime(Access::Read);
        let mut sink = RecordingToolSink::new();
        let r = rt.invoke("t1", &call("probes", "{}"), &mut sink);
        assert!(r.payload.contains("probe"), "{}", r.payload);
        assert!(r.payload.contains("nearest"), "{}", r.payload);
    }

    #[test]
    fn a_description_that_names_data_is_refused_at_registration() {
        struct Stale;
        impl Tool for Stale {
            fn schema(&self) -> ToolSchema {
                ToolSchema::new(
                    "stale",
                    "Search the corpus. The corpus contains the Rust book.",
                    serde_json::json!({"type": "object"}),
                    Access::Read,
                )
            }
            fn invoke(&self, _c: &mut InvokeCtx<'_>, _a: &Value) -> Invocation {
                Invocation::ok("")
            }
        }
        let mut reg = Registry::new();
        let e = reg.register(Box::new(Stale)).unwrap_err();
        assert!(format!("{e}").contains("clause 6"), "{e}");
        // A foreign tool is seated, and its findings are visible.
        assert!(reg.register_foreign(Box::new(Stale)).is_ok());
        assert_eq!(reg.foreign_findings.len(), 1);
    }

    #[test]
    fn a_role_over_the_ceiling_is_refused_with_the_overflow_named() {
        let mut reg = Registry::new();
        reg.register(Box::new(Probe {
            access: Access::Read,
        }))
        .unwrap();
        let mut role = Role::new("greedy", &["probe"]);
        role.max_tools = 0;
        let e = reg.resolve_role(&role).unwrap_err();
        assert!(format!("{e}").contains("probe"), "{e}");
    }

    /// **`m2_coder` seats the delegation pair** (R58).
    ///
    /// `task` and `task_result` are listed on the seat rather than stripped at the depth
    /// limit, on purpose: the cap is enforced where the call is made
    /// (`harnessd::harness::subagent_depth_refusal`), so the seat must name all three at
    /// every depth or a child could start work it cannot collect or correct —
    /// `orchestrator`'s own rule.
    #[test]
    fn the_coder_seat_names_the_delegation_pair() {
        let seat = roles::m2_coder();
        assert!(
            seat.tools.iter().any(|t| t == "task"),
            "a subagent must be able to delegate: {:?}",
            seat.tools
        );
        assert!(
            seat.tools.iter().any(|t| t == "task_result"),
            "a seat that can start work and not collect it is worse than one that \
             cannot: {:?}",
            seat.tools
        );
        assert!(
            seat.tools.iter().any(|t| t == "task_message"),
            "a seat that can start work and not CORRECT it has only the wrong answer to \
             wait for: {:?}",
            seat.tools
        );
        // The pair is seated, not pushed: a role over its ceiling is refused at resolve,
        // so an overflow here would be a prompt the build cannot seat at all.
        assert!(
            seat.tools.len() <= seat.max_tools,
            "the seat is over its own ceiling: {} > {}",
            seat.tools.len(),
            seat.max_tools
        );
    }

    /// **The gatekeeper seat is read-only by construction.**
    ///
    /// It may read, grep, glob, and run `bash` for `git diff`/`git log`, and it has
    /// no `write`, no `edit`, no `task` — a reviewer that can author code, directly
    /// or through a child, becomes a second author and then nobody reviewed.
    #[test]
    fn the_gatekeeper_seat_has_no_writing_tool() {
        let seat = roles::gatekeeper();
        // The reading and the diff are there.
        for want in ["read", "grep", "glob", "bash"] {
            assert!(
                seat.tools.iter().any(|t| t == want),
                "the gatekeeper seat must name `{want}`: {:?}",
                seat.tools
            );
        }
        // And nothing that can author code: no write, no edit, no delegation.
        for forbidden in ["write", "edit", "task", "task_result", "task_message"] {
            assert!(
                !seat.tools.iter().any(|t| t == forbidden),
                "the gatekeeper seat must not name `{forbidden}` — a reviewer that \
                 can edit becomes a second author and then nobody reviewed: {:?}",
                seat.tools
            );
        }
        // The seat is under its own ceiling, so it is seatable.
        assert!(
            seat.tools.len() <= seat.max_tools,
            "the seat is over its own ceiling: {} > {}",
            seat.tools.len(),
            seat.max_tools
        );
    }

    /// **`task_start` is seated wherever `task` is.**
    ///
    /// The operator's ask — *"we will need a new tool - task_start or what that will
    /// arrange worktree, firecode and subagent"* — is that placement lives in the tool,
    /// not in the caller's memory. A seat that can start a child but not arrange its
    /// placement is the seat that spawns it into the main tree and then kills it for
    /// being in the wrong place: the two mistakes the tool exists to prevent. So every
    /// role that names `task` must name `task_start` beside it, and this pins that for
    /// the three seats that do.
    #[test]
    fn task_start_is_seated_wherever_task_is() {
        for (name, seat) in [
            ("orchestrator", roles::orchestrator()),
            ("leticode", roles::leticode()),
            ("m2_coder", roles::m2_coder()),
        ] {
            let has_task = seat.tools.iter().any(|t| t == "task");
            let has_task_start = seat.tools.iter().any(|t| t == "task_start");
            assert!(
                has_task,
                "the {name} seat is expected to name `task` in this test: {:?}",
                seat.tools
            );
            assert!(
                has_task_start,
                "a seat that can start a child but not arrange its placement spawns it \
                 into the main tree and then kills it for being in the wrong place: {:?}",
                seat.tools
            );
            // Seated, not pushed: a role over its ceiling is refused at resolve.
            assert!(
                seat.tools.len() <= seat.max_tools,
                "the {name} seat is over its own ceiling: {} > {}",
                seat.tools.len(),
                seat.max_tools
            );
        }
    }

    #[test]
    fn a_role_naming_a_tool_this_build_lacks_is_refused_not_shrunk() {
        let mut reg = Registry::new();
        reg.register(Box::new(Probe {
            access: Access::Read,
        }))
        .unwrap();
        let e = reg.resolve_role(&roles::coder()).unwrap_err();
        let msg = format!("{e}");
        assert!(msg.contains("write") && msg.contains("bash"), "{msg}");
    }

    #[test]
    fn the_events_bracket_the_call() {
        let mut rt = runtime(Access::Read);
        let mut sink = RecordingToolSink::new();
        rt.invoke("t1", &call("probe", r#"{"path":"a"}"#), &mut sink);
        assert_eq!(sink.kinds(), vec!["ToolStarted", "ToolFinished"]);
    }

    #[test]
    fn the_row_and_the_event_carry_the_same_bounded_excerpt() {
        // One bound, two carriers: the `ToolFinished` event reaches the heads
        // watching live, the transcript row reaches every head after a
        // restart. They are built by the same helper at the same bounds, and
        // this pins that — a row whose panels disagree with the card the
        // operator watched draw would be a defect, not a choice.
        let before: String = (1..=50).map(|i| format!("line {i}\n")).collect();
        let after = before.replacen("line 25", "line 25 changed", 1);
        let r = ToolResult {
            call_id: "c1".into(),
            name: "edit".into(),
            outcome: ToolOutcome::Ok,
            payload: "done".into(),
            repairs: Vec::new(),
            notes: Vec::new(),
            spill: None,
            media: None,
            edit: Some(crate::edit::FileEdit {
                path: "f".into(),
                before: before.clone(),
                after: after.clone(),
                created: false,
                before_digest: String::new(),
                after_digest: String::new(),
                replacements: 1,
                changed: crate::edit::changed_span(&before, &after),
            }),
        };
        let item = ToolRuntime::transcript_item(&r);
        let TranscriptItem::ToolResult { edit: row, .. } = &item else {
            panic!("transcript_item built a {item:?}");
        };
        let expected = r.edit.as_ref().unwrap().excerpt(3, 400);
        assert_eq!(row.as_ref(), Some(&expected), "same bound, same excerpt");
    }

    /// **R39: a path in the secret store is not opened to find out what it contains.**
    ///
    /// The file a script argument names is read for the adjudicator's brief — *"judge
    /// THIS, not the filename"* — and *because* that entry exists, the reader is a place
    /// where `python3 ~/.ssh/id_rsa` would put a private key into the brief, the
    /// transcript and the model's context. The refusal is at the reader, before the open,
    /// which is where a disclosure hazard has to be stopped: a filter after the read has
    /// already read it.
    ///
    /// The `read` closure here **counts its own calls**, which is the only way to assert
    /// *not opened* rather than *not shown* — the body comes back as `Unreadable` either
    /// way.
    #[test]
    fn a_secret_store_script_is_never_opened() {
        use std::cell::Cell;
        let opened = Cell::new(0);
        let found = crate::runtime::scripts_for("python3 ~/.ssh/id_rsa", Some("/home/dead"), |p| {
            opened.set(opened.get() + 1);
            Ok(format!("the contents of {p}").into_bytes())
        });
        assert_eq!(opened.get(), 0, "the reader opened a secret-store file");
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].path, "~/.ssh/id_rsa");
        let ScriptBody::Unreadable(why) = &found[0].body else {
            panic!(
                "a secret-store path came back readable: {:?}",
                found[0].body
            );
        };
        assert!(why.contains(".ssh"), "{why}");
        assert!(
            why.contains("not opened to find out what it contains"),
            "the refusal does not say what is being refused: {why}"
        );

        // **An ordinary path is read**, so the guard is not a blanket refusal — the
        // assertion above would pass on a reader that never opened anything.
        let opened = Cell::new(0);
        let found = crate::runtime::scripts_for("python3 deploy.py", Some("/home/dead"), |p| {
            opened.set(opened.get() + 1);
            Ok(format!("print('{p}')").into_bytes())
        });
        assert_eq!(
            opened.get(),
            1,
            "an ordinary script was not read: {found:?}"
        );
        assert!(
            matches!(&found[0].body, ScriptBody::Read(t) if t.contains("deploy.py")),
            "{:?}",
            found[0].body
        );
    }

    /// **The reader and the classifier agree about which file is the program.** A wrapped
    /// interpreter is unwrapped on both sides — `scripts_for` through `unwrap_wrapper`,
    /// layer A through `absorb_stage` — so `sudo python3 foo.py` does not have its file
    /// read by neither half and reported as a hole in one.
    #[test]
    fn a_wrapped_interpreter_has_its_file_read() {
        let found = crate::runtime::scripts_for(
            "sudo -u dead python3 deploy.py",
            Some("/home/dead"),
            |_| Ok(b"print(1)".to_vec()),
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].path, "deploy.py");
    }

    /// An inline program names no file, so nothing is read and nothing is claimed — and a
    /// command that names no script at all stays empty.
    #[test]
    fn a_command_with_no_script_file_reads_nothing() {
        let read = |_: &str| -> Result<Vec<u8>, String> { panic!("opened a file") };
        assert!(crate::runtime::scripts_for("python3 -c 'print(1)'", None, read).is_empty());
        let read = |_: &str| -> Result<Vec<u8>, String> { panic!("opened a file") };
        assert!(crate::runtime::scripts_for("ls -la", None, read).is_empty());
    }
}
