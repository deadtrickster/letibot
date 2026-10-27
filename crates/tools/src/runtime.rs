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

/// **Why a call says it wants a result the moment it exists** (R79).
///
/// A completion nothing waits on is HELD and handed over in one batch at the next break
/// (`crates/harnessd/src/delivery.rs`), so a model working on X is not pulled off it by the
/// end of Y. These are the two things a call can say that the plan's own structure (a todo
/// row's `when` or `needs`) does not already say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WantedNow {
    /// The call is WAITING on it: a `job_wait` or `task_result` on a handle the daemon is
    /// already delivering, or a foreground command moved to the background while its call
    /// was blocked on it. The wait returns at once (R23), and this is what keeps that promise
    /// at once rather than at the next break.
    Awaited,
    /// The work was started with `wake: "now"`.
    WakeNow,
}

impl WantedNow {
    /// The word the delivery record and the notices use.
    pub fn word(self) -> &'static str {
        match self {
            WantedNow::Awaited => "awaited",
            WantedNow::WakeNow => "wake_now",
        }
    }
}

/// **Tell the daemon a result is wanted the moment it exists** — see [`WantedNow`]. Same shape
/// as [`CompletionDelivered`]: the daemon owns the state, so `crates/tools` hands over the fact
/// and never learns what a watcher is.
pub type CompletionWanted = std::sync::Arc<dyn Fn(&str, WantedNow) + Send + Sync>;

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
/// * [`OperatorRun::Unreadable`] — **this daemon may not look at one of the run's
///   processes**, so it cannot tell whether the run is waiting. See the variant: this is the
///   `sudo` case, and the report exists because silence there is what the operator read as
///   *the daemon hangs*. Reported whether or not the run has been quiet — see the variant's
///   own section on why the beat is the card's and not this one's.
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
    /// **This daemon cannot tell whether the run is waiting** — at least one of its
    /// processes could not be looked at.
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
    ///
    /// # Why this carries the beat, and why it is not gated by it
    ///
    /// `quiet` is *the run has written nothing for a beat*, which is the condition the
    /// **card** needs and which this report deliberately does not: the card claims *some
    /// process is blocked reading the answer we hold*, and a run that is still drawing is
    /// genuinely not blocked — so the beat is the card's, and only the card's.
    ///
    /// This is the opposite kind of fact: it is the **absence** of a reading, and it is about
    /// **permissions**, not about the clock. One of the run's processes is not this daemon's
    /// to look at at a 100 ms write interval exactly as much as at a 2 s one. The operator's
    /// second report is what gating it behind the beat costs — *"so i start it completely
    /// fresh and do apt install and harnessd hangs without printing anything to me"* — where
    /// `apt` was **streaming its progress** (their own transcript's words) and the run
    /// therefore never had a beat's silence to be reported on. From the person's seat the
    /// command's own output does not land until the run ends, so *working* and *blocked* look
    /// the same; this report is the only thing that can tell them apart, and it may not be
    /// withheld on a timer.
    ///
    /// The flag travels with the report so the sentence can **say which of the two facts it
    /// is** — a run that has gone quiet may well be waiting, and a run that is still writing
    /// is either working or blocked with something still drawing. One sentence for both would
    /// be the defect the askpass deadline's own line has (`no head answered before the
    /// deadline, or the person refused`), which is one sentence for two facts and wrong for
    /// one of them.
    Unreadable {
        /// The job's handle.
        job: &'a str,
        /// **Whether the run had been quiet for a beat when this was reported.** See the
        /// variant's docs for why the report is made either way and why the sentence still
        /// needs to know.
        quiet: bool,
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
    /// See [`CompletionWanted`]. `None` in a runtime with no daemon behind it.
    completion_wanted: Option<&'a CompletionWanted>,
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
    /// **The gate, for a question the tool must put before it starts** — today only
    /// [`InvokeCtx::grant_memory`]. `None` on the operator's own run, which has nobody
    /// left to ask, and in a runtime with no gate.
    gate: Option<&'a std::sync::Mutex<Box<dyn Gate>>>,
    /// **Whether a plan row could wait on what this call starts**: `todo_write` is seated in
    /// this runtime and its board has an open row ([`Tool::open_plan`]). The `wait_on` offer a
    /// background start carries is said only then (R93.B6) — it was on every start, to seats
    /// with no plan and to seats with no `todo_write` at all.
    pub plan_open: bool,
    /// **Whether the session this call belongs to ends when this run ends** — a one-shot
    /// (`letibot "a question"`), which has no daemon to come back to.
    ///
    /// It is a fact about the RUN and not about the call, carried here because it is the tool
    /// that has to act on it: a command left in the background is watched by a daemon and read
    /// by a head, and a one-shot run has neither — so the job would outlive the only thing that
    /// could ever have delivered its result. `bash` refuses `background: true` here rather than
    /// starting work nobody can read. See [`ToolRuntime::with_one_shot`].
    pub one_shot: bool,
}

impl InvokeCtx<'_> {
    /// **Put a memory ask above the ceiling to the operator.** See [`Gate::grant_memory`].
    /// The gate's lock is taken for the question, as `admit` takes it: a card may wait
    /// minutes for a person, and a `/mode` typed meanwhile must not move the gate.
    pub fn grant_memory(
        &self,
        ask: &crate::exec::memory::MemoryAsk,
    ) -> crate::exec::memory::MemoryGrant {
        match self.gate {
            Some(g) => g
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .grant_memory(ask),
            None => crate::exec::memory::MemoryGrant::NotAsked,
        }
    }

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

    /// **Say that this handle's result is wanted the moment it exists** (R79) — see
    /// [`WantedNow`]. A no-op when nobody wired it: a runtime with no daemon has no batch to
    /// hold anything in.
    pub fn want_completion(&self, handle: &str, why: WantedNow) {
        if let Some(f) = self.completion_wanted {
            f(handle, why);
        }
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

    /// **Whether this tool holds a plan with an open row** — `Some` only from the tool that
    /// keeps the board (`todo_write`), `None` from every other. Read once per call into
    /// [`InvokeCtx::plan_open`].
    fn open_plan(&self) -> Option<bool> {
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

/// **The command line the gate reads for an argv somebody declared** — one word per element, every
/// word quoted so nothing can be re-split or expanded.
///
/// `docs/plugins-plan.md` §3 gives a layer `exec:<program>` and means the ARGV: no shell, so
/// nothing a plugin wrote can become two commands. The gate's ladder is nonetheless written over a
/// command *string* — layer A parses one with the grammar, a `permission` rule matches segments of
/// one, and R21 decides from one whether the call only reads — so a look that wants the gate's own
/// answer has to hand it a command. The quoting is what makes the two the same thing: each element
/// is one single-quoted word, so the shell the grammar describes could neither split a word nor
/// expand anything inside it, and what the gate reads is exactly the argv.
///
/// The caller still owns the rest of the call — the tool's name and its declared access — because
/// those are what it is standing in for. There is no such thing as a gate call the host invents.
pub fn command_of_argv(argv: &[String]) -> String {
    argv.iter()
        .map(|word| crate::backend::shell_quote(word))
        .collect::<Vec<_>>()
        .join(" ")
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

    /// **The same decision, for a call nobody may be asked about** — `docs/plugins-plan.md` §1,
    /// principle 2.
    ///
    /// A look a plugin declares goes through the same gate the model's own calls meet, and
    /// `docs/plugins-plan.md` §4.4's `exec` look runs on the session's **monitor thread**: a card
    /// raised there would park that thread, and the watch with it, until a person answered. So a
    /// look is adjudicated and never asked, and this is the one entry both halves use — the ladder
    /// that settles a call without a person (a `permission` rule, a point that admits the class, a
    /// standing grant, the always-ask tier's own refusals) runs, and the ask does not.
    ///
    /// `Admit` means *run it*; `Refuse` means *do not*, and its class says which: `NotRun` is
    /// **it would have been put to somebody** and `Denied` is **somebody's rule says no**. A caller
    /// that told those two apart by nothing would be telling a plugin its program is forbidden when
    /// what it needs is a person.
    ///
    /// The default is [`Gate::admit`], which is right for every gate with nobody behind it —
    /// `NoBoundary` refuses, an allow-all admits, and a gate that can ask overrides this with the
    /// front half of its own ladder.
    fn admit_unasked(&mut self, call: &GateCall<'_>) -> GateDecision {
        self.admit(call)
    }

    /// **How this session stands**, for a hook that must be told: the session's id and the mode's
    /// own word. The default is an empty [`Standing`], which is every test's `NoBoundary` — a
    /// session that has no id to give and no mode to name, and a plugin layer that reads either
    /// gets the honest nothing rather than an invented value.
    ///
    /// It is asked of the GATE and not of a copy because the mode is the gate's own field and it
    /// moves at runtime (`/mode`): a second copy anywhere else would be a second answer to *how
    /// much may this session do without asking*.
    fn standing(&mut self) -> Standing {
        Standing::default()
    }

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

    /// **May this call run with more memory than the model may take on its own?** Asked
    /// before the command starts, when its `memory` ask is above the ceiling or is
    /// `uncapped` (TODO.md R65: *"the max is uncapped but if model wants it - must ask
    /// me"*).
    ///
    /// The default is [`crate::exec::memory::MemoryGrant::NotAsked`] for the reason
    /// [`Gate::grant_view`]'s is: a gate with nobody behind it has not decided, and the
    /// call does not run on a question nobody answered.
    fn grant_memory(
        &mut self,
        _ask: &crate::exec::memory::MemoryAsk,
    ) -> crate::exec::memory::MemoryGrant {
        crate::exec::memory::MemoryGrant::NotAsked
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

/// **How a session stands**, as a hook is told it: the id a session is known by, and the mode's
/// own word — `read-only`, `writes-allowed`, `allow-edits`, `allow-all`. Both are the gate's
/// ([`Gate::standing`]), and an empty pair is a runtime whose gate has neither.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Standing {
    pub session: String,
    pub mode: String,
}

/// **A plugin layer's answer about one call**, and the layer that gave it — the by-line a rewrite
/// or a refusal is reported with, because a call that was changed must say who changed it.
#[derive(Debug, Clone, PartialEq)]
pub struct Patched {
    pub out: letibot_plugin::tool_call::ToolCallOut,
    pub by: String,
}

/// **A hook that sees a tool call before the gate** — `on_tool_call`, `docs/plugins-plan.md` §4.1,
/// in the crate that owns the path through the gate.
///
/// A trait here for the reason [`Gate`] is one: this crate holds no plugin stack, and a caller that
/// has one supplies it. `None` — the default, and what a runtime with no plugins has — is a session
/// nothing is patching, which behaves exactly as it did before there were plugins.
///
/// **It cannot admit anything.** The answer says *pass*, *rewrite*, *refuse* or *answer*; the
/// rewritten call is then gated like any other, and there is no field by which a layer could say
/// *allow*.
pub trait CallHook: Send + Sync {
    /// What the layer says about this call, or `None` for *nothing said* — run it as it is.
    fn before(&self, call: &letibot_plugin::Call, standing: &Standing) -> Option<Patched>;
}

/// What [`ToolRuntime::plugin_patch`] resolved to.
enum Patch {
    /// Nothing said: run the call as the model proposed it.
    None,
    /// Run this instead.
    Rewritten(Rewritten),
    /// Do not run it: this is the result the model reads.
    Done(ToolResult),
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
            // **`hosts` is seated wherever `task` is**, and it is the same rule one step on: a seat
            // that can place a child on `host:NAME` and cannot learn which names exist has only
            // the refusal to go on, and the refusal costs a round trip per guess. Read-only, so
            // the seat pays one entry for it.
            &[
                "task",
                "task_result",
                "task_message",
                "task_resume",
                "task_start",
                "hosts",
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
                // **The plugin loop, wherever a session writes code** — the operator's ask:
                // *"you building panes at runtime — i want the same in letibot"*. `plugin`
                // writes, builds, tests and loads a pane plugin in the session, and
                // `plugin_hooks` is the contract it is written against; a seat that can write
                // Rust but not run it here has the half of the loop that cannot show anybody
                // anything.
                "plugin",
                "plugin_hooks",
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
                // `notes`: the standing-notes writer, seated beside `write`/`edit`
                // for the same reason it is in `m2_coder` — a standing note is
                // a write to the operator's tree. It is the entry that moves
                // `max_tools` below; the comment there says why.
                "notes",
                "todo_write",
                "skill",
                "lsp",
                "task",
                "task_result",
                "task_message",
                "task_resume",
                // **`hosts`: which machines exist, and which is least loaded** — read from the
                // operator's file at the call, because the list is theirs and the prompt is a
                // cache. Seated beside `task`, whose `where: "host:NAME"` it is the way to choose.
                "hosts",
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
                // **`fanout` and `scenario`: work on many hosts at once** (RH8). The operator,
                // 2026-10-26: *"since we introduced remote hosts we can do map reduce now — usually
                // agents hand roll orchestration that span multiple hosts, say 'run this server
                // tests here and that server there', either on both separately or on both
                // together"*. Seated beside `bash` and `hosts`, because a seat that can reach one
                // machine at a time is the seat that hand-rolls `ssh … &` and a `ps` poll — 641
                // lines of `fleet.sh` in the session these two exist to replace.
                "fanout",
                "scenario",
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
                // `merge_gate`: the merge queue's gate for this repository, and the choices
                // its files suggest — read-only; the agent writes the AGENTS.md section the
                // operator chose. The operator: *"let main project agent manage it"*.
                "merge_gate",
                // `queue`: the merge queue itself — read it, re-gate the target, restart or
                // withdraw a row, propose the target (R76 item 2). The operator, on a red `main`
                // the project's agent never heard of: *"it should be able to get out of it
                // itself obviously"*.
                "queue",
                // **`plugin` and `plugin_hooks`: a pane, written and loaded in the session.**
                //
                // The operator: *"remember our unfinished wasm plan — this is exactly the case —
                // you building panes at runtime — i want the same in letibot"*. The pane language
                // landed in slice 1 (`docs/plugins-slice1.md` C3) with no way for a model to
                // write one; these two are that way, and they are the point of the whole plan
                // (§2: a fix in the middle of a session, built and answering within the turn).
                // `plugin_hooks` is seated beside `plugin` for the reason `transcript` is seated
                // beside `digest`: the tool that reports a refusal names the contract, and a seat
                // that is told and cannot read it writes the same wrong view twice.
                "plugin",
                "plugin_hooks",
                // **`ask_user_question`: the seat can ask the person, not only tell them.**
                //
                // The operator's ruling, 2026-10-09: *"yeah i want you to be able to ask
                // me for a choice. each choice can have my note, and i can abstain or type
                // my answer"*. Before this a leticode session had no such verb at all — the
                // tool exists, and the `intent` bundle it lives in is the planner's — so a
                // model could only ask in prose and the operator had no way to answer *as*
                // an answer. `Access::Session` (it changes nothing on disk and interrupts a
                // person), which is the treatment `intent/mod.rs` asserts for it.
                "ask_user_question",
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
        // **Twenty-five exactly since `notes`** — the standing-notes writer, seated
        // beside `write`/`edit` — and this one MOVES the declared ceiling by one,
        // to twenty-six, because the room is added on top of this list for a
        // root session (`flowy`, in `role_for_seat`) and `notes` took the last
        // spare seat: the alternative was a coder session that cannot open the
        // door, which the `flowy_is_a_door_for_every_root_session` test refuses
        // on our behalf. Declared here, like `m2_runner`'s ninth, because a
        // ceiling quietly raised for everybody is not a ceiling.
        // **Twenty-seven since `merge_gate`**, which takes its own seat: the queue holds a
        // repository's branches until its gate exists, and the agent that runs the project is
        // the one the operator asked to set it.
        // **And twenty-eight since `ask_user_question`**, which takes its own seat for the
        // operator's own reason rather than by trade: *"yeah i want you to be able to ask me
        // for a choice"*. A seat that can spawn subagents, run commands and write files but
        // cannot ask the person a question with answerable options is a seat that asks in
        // prose and reads silence as assent — which is the defect `ask.rs` exists against.
        // The room is this list (27) plus the root session's `flowy` door, so the declared
        // ceiling moves with it, here, where somebody deciding can read it.
        // **Twenty-nine since `queue`** (R76 item 2), which takes its own seat for the reason
        // `merge_gate` did: the project's agent is the one told its target is red, and a seat
        // that can be told and cannot act sends the operator to type `/queue` for it.
        // **Thirty since `task_resume`** (the operator, 2026-10-12: *"we should bring subagents
        // back"*), which takes its own seat beside `task_message`: the restart reminder names it
        // as the one call that brings a child back, and a seat that is told and cannot act is the
        // same defect `queue` was seated against.
        // **Thirty-one since `hosts`**, and this one MOVES the declared ceiling by one, to 31: a
        // seat that places children on `host:NAME` while being unable to learn which hosts exist
        // has only the refusal to go on, and trading a seat away from a working verb for a
        // read-only listing would be the wrong half of the choice. Declared here, where somebody
        // deciding can read it.
        // **Thirty-three since `plugin` and `plugin_hooks`** (P2), and they move the declared
        // ceiling by two, to 33: a session that can write a Rust file, compile it and load it as a
        // pane is the whole of what a polymorphic harness is for, and trading two working verbs
        // away for it would be the wrong half of the choice — the same argument `hosts` made one
        // entry above. Declared here, where somebody deciding can read it.
        // **Thirty-five since RH8's `fanout` and `scenario`**, and they move the declared ceiling
        // by two, to 35, for `hosts`' reason one entry above: a session that can reach many
        // machines is exactly the session that needs a verb for it, and trading away two working
        // verbs to pay for the capability would be the wrong half of the choice.
        r.max_tools = 35;
        r
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
                // **`notes` is seated beside `write`/`edit`, and nowhere else** —
                // the standing-notes writer. A note is a write to the
                // operator's tree (it is read into every later session's
                // prompt), so it rides the write decision rather than
                // arriving with a read-only seat. Thirteen tools against a
                // ceiling of sixteen.
                "notes",
                "task",
                "task_result",
                "task_message",
                "task_resume",
                "task_start",
                // `hosts`, beside `task`: this seat can place a child on another machine, so it
                // has to be able to see which machines there are. Read-only, and the seat has the
                // room — fifteen tools against `DEFAULT_MAX_TOOLS` (16).
                "hosts",
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
    ///
    /// **Eleven since RH8's `fanout` and `scenario`**, and they take their own seats rather than
    /// trading for one: a runner seat that can start a shell on this box but cannot run the same
    /// command on four machines is the seat the Stroppy session worked around with 641 lines of
    /// `fleet.sh` — and this is the seat that runs builds and tests, which is what a fleet is for.
    /// The ceiling moves here, declared, for the reason the ninth seat moved it.
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
                "fanout",
                "scenario",
            ],
        );
        r.max_tools = 11;
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
    /// **And one tool that is not read-only, because the verdict has to go somewhere.**
    /// `review_verdict` ([`crate::builtins::verdict`]) is the reviewer's own door: the child
    /// hands its decision, its reasons and its evidence to the daemon that asked it, and the
    /// daemon writes the entry's review row. It is `Access::Session` and not a write — no file,
    /// no process, no network — so a reviewer's verdict raises no permission card at the head of
    /// the tree it runs in, which matters because that child has no head of its own and a review
    /// that cannot finish unattended is a review that never finishes.
    ///
    /// Six tools against a ceiling of sixteen: `read`, `grep`, `glob`, `bash`, `read_spill` and
    /// `review_verdict`. `read_spill` is seated beside `bash` for the same reason
    /// [`m2_runner`] seats it — a `git diff` on a large branch spills, and a seat
    /// that cannot read back its own spilled output is a seat that cannot review a
    /// large change.
    pub fn gatekeeper() -> Role {
        Role::new(
            "gatekeeper",
            &[
                "read",
                "grep",
                "glob",
                "bash",
                "read_spill",
                crate::builtins::verdict::TOOL,
            ],
        )
    }

    /// **The notes keeper's seat: the read tools and `notes`, and nothing that can author
    /// code.**
    ///
    /// The design: *"Its seat is read + `notes`, not the coder toolset."* So there is no
    /// `write`, no `edit`, no `bash`, and no job or task surface: a keeper inspects a note
    /// against the tree and repairs *memory*, and a seat that could do more would be a seat that
    /// could do something nobody asked it to.
    ///
    /// **`notes` is the one writer, and that is the point rather than a convenience.** It is how
    /// a repair keeps authorship: the tool takes a bare NAME, so there is no spelling of a write
    /// that leaves `.letibot/notes/`; it refuses `AGENTS.md` and the box-wide notes, which are
    /// the operator's; it keeps the author's `<!-- abstract: … -->` line; and every `read` of a
    /// note carries the caveat that says the note is a record of what was true when it was
    /// written. A keeper that could `write` a note file could make its own prose
    /// indistinguishable from the operator's, which is the one failure the notes tool exists to
    /// prevent.
    ///
    /// Because `notes` declares [`crate::Access::Write`], the daemon gives this seat
    /// `HostBackend::writable` (`Seat::NotesKeeper::needs_writable_backend`) — a write path, and
    /// no exec path. `grep` and `glob` ride along with `read` because they are the same act by
    /// other means, and the gatekeeper's seat has them for the same reason.
    pub fn notes_keeper() -> Role {
        Role::new(
            "notes_keeper",
            &["read", "grep", "glob", "read_spill", "notes"],
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
        // **R93.F30 — one plan tool per seat.** `todo` (the intent ledger's list) and
        // `todo_write` (the board the nag, the pane and `wait_on` read) are two lists with two
        // models; a seat holding both is told about one and nagged about the other. Where
        // `todo_write` is seated, `todo` is not.
        let one_plan;
        let role = if role.tools.iter().any(|t| t == "todo_write")
            && role.tools.iter().any(|t| t == "todo")
        {
            one_plan = Role {
                tools: role
                    .tools
                    .iter()
                    .filter(|t| *t != "todo")
                    .cloned()
                    .collect(),
                ..role.clone()
            };
            &one_plan
        } else {
            role
        };
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
    /// **Behind a mutex because the runtime is shared, and the gate is the one field
    /// that mutates in place.** Every other field is either read-only after open
    /// (`registry`, `backend`, `spiller`) or already synchronised (`files`), so two
    /// threads can invoke through one runtime — which is what the operator's own run
    /// and a turn running at the same time now do. The gate cannot follow that rule
    /// by aliasing alone: [`Gate::admit`] takes `&mut self`, and an operator decision
    /// can hold it for minutes. So the mutex is *the gate's*, held only across the
    /// decision, and the ungated path — [`ToolRuntime::invoke_operator`], the door's
    /// calls and the operator's `!` line — never takes it at all: there is nobody
    /// left to ask, so there is nothing here to wait behind.
    pub gate: std::sync::Mutex<Box<dyn Gate>>,
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
    /// See [`CompletionWanted`]. `None` in a runtime with no daemon behind it.
    pub completion_wanted: Option<CompletionWanted>,
    /// See [`OperatorRun`]. `None` in a runtime with no daemon behind it, and the
    /// operator's own run then raises no card at all — the manual way in is unaffected.
    pub operator_runs: Option<OperatorRuns>,
    /// The repair codes the model has been told about in this runtime (R93.F56) — see
    /// [`ToolResult::repairs_told`].
    pub repairs_told: std::sync::Mutex<std::collections::BTreeSet<&'static str>>,
    /// **Whether this runtime belongs to a one-shot run**, which ends with the process that
    /// started it. See [`InvokeCtx::one_shot`] for the rule a tool applies and why.
    pub one_shot: bool,
    /// **The plugin layer's look at a call, before the gate** (`docs/plugins-plan.md` §4.1).
    /// `None` in a runtime with no plugin stack — every test's, and every session the daemon did
    /// not open — and then the call path is exactly what it was before there were plugins.
    pub call_hook: Option<std::sync::Arc<dyn CallHook>>,
}

/// **A call the model proposed and this layer reproposed as a different tool.**
///
/// One shape produces one of these today — a `bash` heredoc that is a file write
/// (see [`crate::heredoc`]) — and it is a struct rather than a tuple because a
/// second shape would otherwise be a second four-tuple nobody could read.
struct Rewritten {
    /// The call as it now stands: the same `id`, so the model's result and the
    /// transcript row still answer the call it made, and the *other* tool's name.
    call: ToolCall,
    args: Value,
    schema: ToolSchema,
    /// The one line the model is told, because a row that says `write` about a
    /// `bash` call is otherwise a difference it cannot account for.
    note: String,
}

impl ToolRuntime {
    pub fn new(registry: Registry, backend: Box<dyn ExecBackend>) -> Self {
        ToolRuntime {
            registry,
            backend,
            spiller: Spiller::unset(),
            // Fail closed by default: a runtime nobody configured refuses every
            // non-read call rather than allowing it.
            gate: std::sync::Mutex::new(Box::new(NoBoundary) as Box<dyn Gate>),
            limits: Limits::default(),
            files: crate::files::FileLedger::new(),
            operator_waiting: None,
            completion_delivered: None,
            completion_wanted: None,
            operator_runs: None,
            repairs_told: Default::default(),
            one_shot: false,
            call_hook: None,
        }
    }

    /// **Wire the fact that this runtime is a one-shot run's** — a `letibot "…"`, whose
    /// daemon exits when the prompt's answer does.
    ///
    /// Nothing else in the tree has this shape: every other session outlives its head, so a
    /// background job has a watcher and a place for its result to land. A one-shot has neither,
    /// and `bash` says so by name instead of starting a process nobody will ever read.
    pub fn with_one_shot(mut self, yes: bool) -> Self {
        self.one_shot = yes;
        self
    }

    /// Wire the question a long-running tool asks before it spends another minute.
    pub fn with_operator_waiting(mut self, f: OperatorWaiting) -> Self {
        self.operator_waiting = Some(f);
        self
    }

    /// **Wire the plugin layer that sees a call before the gate** (`docs/plugins-plan.md` §4.1).
    /// The daemon installs this when it opens a session it serves; a runtime nobody wired keeps
    /// the call path it had, which is what makes *a session with no layers behaves as it did* a
    /// property of this field being `None` rather than a promise.
    pub fn with_call_hook(mut self, hook: std::sync::Arc<dyn CallHook>) -> Self {
        self.call_hook = Some(hook);
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

    /// **Wire where a call says it wants a result at once** (R79). See [`CompletionWanted`].
    pub fn with_completion_wanted(mut self, f: CompletionWanted) -> Self {
        self.completion_wanted = Some(f);
        self
    }

    pub fn with_spiller(mut self, spiller: Spiller) -> Self {
        self.spiller = spiller;
        self
    }

    pub fn with_gate(mut self, gate: Box<dyn Gate>) -> Self {
        self.gate = std::sync::Mutex::new(gate);
        self
    }

    /// **The `write` call a `bash` heredoc stands for**, when the command is exactly
    /// that one shape and this seat can write at all.
    ///
    /// Two of the facts this needs are not in the command text, which is why
    /// [`crate::heredoc::recognise`] is a pure function and this is not: **where the
    /// session sits** — the call's own `cwd`, the workspace unless it named another —
    /// and **what an append would extend**, read through the backend so a confined
    /// session extends its own copy rather than the host's.
    ///
    /// `None` for every command that is not that shape, and for a seat with no
    /// `write` tool. The second is not tidiness: the gatekeeper's seat holds `bash`
    /// and no writing tool, and a rewrite into a tool the seat does not have would
    /// turn a working command into *"no tool called `write` is available here"*.
    fn heredoc_rewrite(&self, call: &ToolCall, args: &Value) -> Option<Rewritten> {
        let schema = self.registry.get("write")?.schema();
        let command = args.get("command").and_then(|v| v.as_str())?;
        let recognised = crate::heredoc::recognise(command)?;
        let cwd = args.get("cwd").and_then(|v| v.as_str()).unwrap_or(".");
        let write =
            crate::heredoc::as_a_write(&recognised, cwd, |path| match self.backend.read(path) {
                Ok(bytes) => crate::heredoc::Existing::Bytes(bytes),
                Err(crate::backend::BackendError::NotFound(_)) => crate::heredoc::Existing::Absent,
                Err(_) => crate::heredoc::Existing::Unknown,
            })?;
        let note = format!(
            "recorded as a write to `{}` (heredoc through bash is converted; use `write` directly)",
            write.path
        );
        let args = serde_json::json!({"path": write.path, "content": write.content});
        Some(Rewritten {
            call: ToolCall {
                id: call.id.clone(),
                name: schema.name.clone(),
                arguments: serde_json::to_string(&args).unwrap_or_else(|_| "{}".to_string()),
            },
            args,
            schema,
            note,
        })
    }

    /// **What a plugin layer says about a call, before the gate** — `docs/plugins-plan.md` §4.1,
    /// reduced to the three things this runtime can do with an answer.
    ///
    /// # Why a rewrite cannot widen anything
    ///
    /// The only field a layer can change is the ARGUMENTS; the tool's name is the model's. So a
    /// rewritten call is a call the model could itself have made, judged by the same gate, by the
    /// same tool's `access_for`, against the same boundary — there is no path by which a layer
    /// reaches something the model could not. What it can do is make a call smaller, which is the
    /// point (§4.1's example: the model armed nine monitors for three pull requests).
    ///
    /// The rewritten arguments are put through [`salvage`] again, because they are a tool call's
    /// arguments whoever wrote them: a layer cannot put a shape past the schema's repair, and one
    /// whose rewrite the tool would refuse is reported rather than run.
    fn plugin_patch(&self, call: &ToolCall, args: &Value) -> Patch {
        let Some(hook) = self.call_hook.as_ref() else {
            return Patch::None;
        };
        // How the session stands, asked of the gate — the mode is its own field and it moves at
        // runtime, so a copy anywhere else would be a second answer to the same question.
        let standing = self
            .gate
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .standing();
        let asked = letibot_plugin::Call::new(call.name.clone(), args.clone());
        let Some(Patched { out, by }) = hook.before(&asked, &standing) else {
            return Patch::None;
        };
        let refused = |why: String| {
            ToolResult::new(
                call.id.clone(),
                call.name.clone(),
                ToolOutcome::NotRun {
                    why: format!("the plugin layer `{by}` refused this call"),
                },
            )
            .with_payload(why)
        };
        match out.verdict() {
            letibot_plugin::tool_call::Verdict::Pass => Patch::None,
            letibot_plugin::tool_call::Verdict::Refuse(why) => {
                Patch::Done(refused(why.to_string()))
            }
            letibot_plugin::tool_call::Verdict::Answer(text) => Patch::Done(
                ToolResult::new(call.id.clone(), call.name.clone(), ToolOutcome::Ok)
                    .with_payload(text.to_string()),
            ),
            letibot_plugin::tool_call::Verdict::Rewrite(new_args) => {
                let text = serde_json::to_string(new_args).unwrap_or_else(|_| "null".into());
                let rewritten = ToolCall {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    arguments: text.clone(),
                };
                let Some(schema) = self.registry.get(&call.name).map(|t| t.schema()) else {
                    return Patch::None;
                };
                match salvage(&rewritten.arguments, &schema) {
                    Ok(s) => Patch::Rewritten(Rewritten {
                        call: rewritten,
                        args: s.value,
                        schema,
                        note: format!(
                            "the plugin layer `{by}` rewrote this call's arguments, and the \
                             rewritten call is what ran: {text}"
                        ),
                    }),
                    Err(e) => Patch::Done(refused(format!(
                        "the rewrite is not something `{}` accepts: {}",
                        schema.name, e.guidance
                    ))),
                }
            }
        }
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
    ///
    /// `&self`, and that is the design and not a convenience: a turn holds this
    /// method for as long as its longest tool call, and the operator's own run now
    /// goes through the same runtime on its own thread at the same time. Everything
    /// shared is either read-only after open or synchronised; the gate — the one
    /// `&mut` user — sits behind its own mutex (see the field).
    pub fn invoke(
        &self,
        turn_id: &str,
        call: &ToolCall,
        sink: &mut dyn ToolEventSink,
    ) -> ToolResult {
        self.run(turn_id, call, sink, true)
    }

    /// **Run one call the OPERATOR made**, which the admission has already answered.
    ///
    /// **`&self`, and that is load-bearing**: the operator's own run runs on its own
    /// thread now — it must, because it holds that thread for as long as the command
    /// does — and a turn may be invoking through this same runtime while it goes.
    /// Nothing on this path needs exclusivity: the gate is never consulted, so the one
    /// field that mutates in place is never touched. See [`ToolRuntime::invoke`] for
    /// the gated half of the same sentence.
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
        &self,
        turn_id: &str,
        call: &ToolCall,
        sink: &mut dyn ToolEventSink,
    ) -> ToolResult {
        self.run(turn_id, call, sink, false)
    }

    /// The body of both, with the gate the one thing that varies.
    fn run(
        &self,
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

        // **A heredoc through `bash` is proposed as the write it is**, and this is
        // before the gate because that is the whole point of it.
        //
        // The operator's report is the argument: a model wrote
        // `cd ~/… && cat > crates/workloads/src/banking.rs <<'RUST' … RUST` instead of
        // calling `write`, and the file landed with no diff row, no editor jump, no
        // path for the never-write list or the workspace boundary to be asked about,
        // and no entry in the file ledger — the gate judged an exec that happened to
        // contain a path. After this the call is a `write`: adjudicated as one,
        // recorded as one, drawn as one. See [`crate::heredoc`] for the one shape
        // recognised and for why every near form is deliberately left alone.
        //
        // **A plugin layer sees the call before the gate** (`docs/plugins-plan.md` §4.1).
        //
        // Here, and not later: everything below has to be true of the call the layer left behind.
        // A rewrite is re-read against the schema exactly as the model's own arguments were, the
        // heredoc re-spelling below sees the rewritten command, and — the point of the whole hook
        // — the gate adjudicates the arguments the tool would actually run with. A layer can make
        // a call smaller and can never make it admissible: there is no field in its answer that
        // says *allow*, and the class below is derived from the schema and this tool's own
        // `access_for`, not from anything a layer said.
        //
        // **On the gated path only**, the same rule the re-spelling below follows: the ungated one
        // is the operator's own run, adjudicated at the door, and a layer rewriting it would be
        // policy over the person who typed it.
        let patched = if gated {
            match self.plugin_patch(call, &args) {
                Patch::None => None,
                Patch::Rewritten(r) => Some(r),
                Patch::Done(result) => {
                    sink.emit(finished_event(turn_id, &result));
                    return result;
                }
            }
        } else {
            None
        };
        let (call, args, schema) = match &patched {
            Some(r) => (&r.call, r.args.clone(), r.schema.clone()),
            None => (call, args, schema),
        };

        // **On the gated path only.** The ungated one is the operator's own run and
        // the head-run door: `! cat > f <<EOF` is a person's own act, already
        // adjudicated at the door, and rewriting what they typed would be this layer
        // deciding their command was not the command they meant.
        let rewritten = if gated {
            self.heredoc_rewrite(call, &args)
        } else {
            None
        };
        let (call, args, schema) = match &rewritten {
            Some(r) => (&r.call, r.args.clone(), r.schema.clone()),
            None => (call, args, schema),
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
            // **And on which machine**, when it is not this one. A remote session's
            // workspace is a path on another box; a card that named only the path would
            // be asking the operator to decide about a machine it never mentioned.
            let workspace = match self.backend.host_label() {
                Some(host) => format!("{workspace} (on {host})"),
                None => workspace,
            };
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
            // **The lock is the gate's, and it is held across the decision on purpose.**
            // `admit` may wait minutes for a person; that is exactly the serialisation
            // wanted — a `/mode` typed while a card is up must not move the gate out
            // from under the decision being made. The ungated path never reaches this
            // line, so an operator's own run and a turn's tool call do not queue on it.
            if let GateDecision::Refuse { outcome, tell } = self
                .gate
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .admit(&gate_call)
            {
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
                completion_wanted: self.completion_wanted.as_ref(),
                operator_runs: self.operator_runs.as_ref(),
                // The operator's own run, and only it. `run`'s flag is the whole
                // distinction between the two entries — see `InvokeCtx::tty`.
                tty: !gated,
                gate: gated.then_some(&self.gate),
                plan_open: self
                    .registry
                    .get("todo_write")
                    .and_then(|t| t.open_plan())
                    .unwrap_or(false),
                one_shot: self.one_shot,
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
        // **One line, on the result, saying what the row is.** The model called
        // `bash` and the transcript says `write`; a difference it cannot account for
        // is one it reads as a harness bug and works around.
        if let Some(r) = &patched {
            invocation = invocation.with_note(r.note.clone());
        }
        if let Some(r) = &rewritten {
            invocation = invocation.with_note(r.note.clone());
        } else if gated && schema.name == "bash" && self.registry.get("write").is_some() {
            // **The near miss**, which is the common case: still a file's contents
            // written through a heredoc, and still no diff and no record, so the row
            // carries the one line naming the tools that would have produced both.
            // Only for a seat that has them — advice to call a tool the seat does not
            // hold is advice that cannot be taken.
            if let Some(nudge) = args
                .get("command")
                .and_then(|v| v.as_str())
                .and_then(crate::heredoc::nudge)
            {
                invocation = invocation.with_note(nudge);
            }
        }
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
                match self
                    .gate
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .grant_view(&path, &schema.name)
                {
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

        // R93.F56: a repair is said to the model once per session per KIND — every kind, not only
        // the envelope's (a fence, a leaked tag): "`id` was given as a number" recurred 132× in
        // one session, and a model that keeps a habit is not helped by hearing of it every call.
        // `repairs` still carries every one, for the head and the metrics.
        let repairs_told = {
            let mut told = self.repairs_told.lock().unwrap_or_else(|e| e.into_inner());
            repairs
                .iter()
                .map(|r| r.code)
                .filter(|c| !told.insert(*c))
                .collect()
        };
        let result = ToolResult {
            call_id: call.id.clone(),
            name: call.name.clone(),
            outcome: invocation.outcome,
            payload,
            repairs_told,
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

    /// **R93.F56: an envelope repair is said to the model once per session**, and still counted.
    #[test]
    fn an_envelope_repair_is_said_once_and_still_counted() {
        let rt = runtime(Access::Read);
        let mut sink = RecordingToolSink::default();
        let fenced = "```json\n{\"path\": \"a\"}\n```";
        let first = rt.invoke("t1", &call("probe", fenced), &mut sink);
        let second = rt.invoke("t1", &call("probe", fenced), &mut sink);
        assert!(first.render().contains("[repaired]"), "{}", first.render());
        assert!(
            !second.render().contains("[repaired]"),
            "{}",
            second.render()
        );
        assert!(
            second.repairs.iter().any(|r| r.code == "envelope.fence"),
            "the head and the metrics still see it: {:?}",
            second.repairs
        );
    }

    /// **R93.F56, every kind**: a repair that is not the envelope's — a renamed key here — is
    /// said once per session too, and still counted.
    #[test]
    fn every_repair_kind_is_said_once_and_still_counted() {
        let rt = runtime(Access::Read);
        let mut sink = RecordingToolSink::default();
        let renamed = "{\"file_path\": \"a\"}";
        let first = rt.invoke("t1", &call("probe", renamed), &mut sink);
        let second = rt.invoke("t1", &call("probe", renamed), &mut sink);
        assert!(
            first.repairs.iter().any(|r| r.code == "key.renamed"),
            "{:?}",
            first.repairs
        );
        assert!(first.render().contains("[repaired]"), "{}", first.render());
        assert!(
            !second.render().contains("[repaired]"),
            "{}",
            second.render()
        );
        assert!(second.repairs.iter().any(|r| r.code == "key.renamed"));
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
        // **And the one tool that is not a read: the door its verdict goes through.** A seat
        // without it is a reviewer that can judge and cannot say so, which is the state this tree
        // was in while the parent parsed its answer instead — see `crate::builtins::verdict`.
        assert!(
            seat.tools
                .iter()
                .any(|t| t == crate::builtins::verdict::TOOL),
            "the gatekeeper seat must carry its own verdict door: {:?}",
            seat.tools
        );
        // The seat is under its own ceiling, so it is seatable.
        assert!(
            seat.tools.len() <= seat.max_tools,
            "the seat is over its own ceiling: {} > {}",
            seat.tools.len(),
            seat.max_tools
        );
    }

    /// **The notes keeper seat: the read tools and `notes`, and nothing that authors code.**
    ///
    /// The design, verbatim: *"Its seat is read + `notes`, not the coder toolset."* `notes` is
    /// the one writer, and it is how a repair keeps authorship — the tool takes a bare NAME, so
    /// no spelling of a write leaves `.letibot/notes/`, and it refuses `AGENTS.md` and the
    /// box-wide notes, which are the operator's. Everything a keeper needs to *see* is here;
    /// nothing it could *author* is.
    #[test]
    fn the_notes_keeper_seat_reads_and_repairs_memory_but_never_code() {
        let seat = roles::notes_keeper();
        for want in ["read", "grep", "glob", "read_spill", "notes"] {
            assert!(
                seat.tools.iter().any(|t| t == want),
                "the notes keeper seat must name `{want}`: {:?}",
                seat.tools
            );
        }
        // Nothing that can author code, and nothing that can run it: the keeper inspects a note
        // and repairs memory. `bash` in particular is NOT here even behind a flag — the commits
        // a note cites were resolved for it before it was asked (`crate::references`).
        for forbidden in [
            "write",
            "edit",
            "bash",
            "task",
            "task_start",
            "job_kill",
            "monitor",
        ] {
            assert!(
                !seat.tools.iter().any(|t| t == forbidden),
                "the notes keeper seat must not name `{forbidden}`: {:?}",
                seat.tools
            );
        }
        assert!(
            seat.tools.len() <= seat.max_tools,
            "the seat is over its own ceiling: {} > {}",
            seat.tools.len(),
            seat.max_tools
        );
    }

    /// **R93.F30: one plan tool per seat** — a role naming both `todo` and `todo_write` resolves
    /// to `todo_write` alone.
    #[test]
    fn a_seat_with_todo_write_does_not_also_get_todo() {
        let mut reg = Registry::new();
        let board = std::sync::Arc::new(crate::builtins::todo::TodoBoard::new(Vec::new()));
        reg.register(Box::new(crate::builtins::todo::TodoWriteTool::new(board)))
            .unwrap();
        crate::builtins::intent::register_into(
            &mut reg,
            &crate::builtins::intent::Wiring::standalone(),
        )
        .unwrap();
        let both = Role::new("both", &["todo", "todo_write"]);
        let seated = reg.resolve_role(&both).unwrap().names();
        assert!(seated.contains(&"todo_write".to_string()), "{seated:?}");
        assert!(!seated.contains(&"todo".to_string()), "{seated:?}");
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

    /// **The leticode seat can ask the person a question.**
    ///
    /// The operator's ruling, 2026-10-09: *"yeah i want you to be able to ask me for a
    /// choice. each choice can have my note, and i can abstain or type my answer"*.
    /// The tool has existed since D10 and was seated on no seat a person actually runs:
    /// it lives in the `intent` bundle, which is the planner's, so a leticode session
    /// had no such verb at all and a model could only ask in prose.
    ///
    /// **What this can and cannot prove.** The leticode seat names tools this crate
    /// does not register — `harness`, `transcript`, `merge_gate` and the rest are the
    /// daemon's — so `resolve_role` cannot be run against it here, and the resolve is
    /// proven where the real registry is built (`letibot-harnessd`'s
    /// `wired.rs::the_leticode_seat_carries_the_ask_and_its_seam_refuses_by_name`).
    /// What this holds is the seat's own list and its ceiling, which is the half that
    /// lives in this file.
    #[test]
    fn the_leticode_seat_carries_ask_user_question() {
        let seat = roles::leticode();
        assert!(
            seat.tools.iter().any(|t| t == "ask_user_question"),
            "the leticode seat does not name `ask_user_question`: {:?}",
            seat.tools
        );
        // It took its own seat rather than trading for one: nothing left the list when
        // it arrived, which is the property `max_tools` is declared for. Twenty-eight since
        // `queue` (R76 item 2), which took its own seat the same way.
        // Twenty-nine since `task_resume`; thirty since `hosts`, which asked for no seat to be
        // given up — a seat that can place a child on another machine and cannot list the
        // machines is a seat that guesses. **Thirty-two since `plugin` and `plugin_hooks`**
        // (P2): a pane written and loaded in the session, which is the point of the plugin plan
        // and is nothing a seat without them can do. **Thirty-four since RH8's `fanout` and
        // `scenario`**, which took their own seats for the same reason `hosts` did.
        assert_eq!(seat.tools.len(), 34, "{:?}", seat.tools);
        assert!(
            seat.tools.len() <= seat.max_tools,
            "the seat is over its own ceiling: {} > {}",
            seat.tools.len(),
            seat.max_tools
        );
        // And the declaration is the one `intent/mod.rs` asserts for it: it changes
        // nothing the operator owns and reads nothing off disk; it interrupts a person.
        let mut reg =
            crate::coder_tools(std::sync::Arc::new(crate::builtins::retrieval::Unavailable))
                .unwrap();
        crate::builtins::intent::register_into(
            &mut reg,
            &crate::builtins::intent::Wiring::standalone(),
        )
        .unwrap();
        let access = reg
            .schemas()
            .into_iter()
            .find(|s| s.name == "ask_user_question")
            .expect("the tool is registered")
            .access;
        assert_eq!(access, Access::Session);
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
            repairs_told: Vec::new(),
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

    // -----------------------------------------------------------------------
    // A heredoc through `bash` is a write (see `crate::heredoc`)
    // -----------------------------------------------------------------------

    /// A `bash` stand-in. It records that it ran, so a test can tell a converted
    /// call — which must never reach the shell — from one left alone.
    struct FakeBash {
        ran: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl Tool for FakeBash {
        fn schema(&self) -> ToolSchema {
            ToolSchema::new(
                "bash",
                "Run a shell command in the session's workspace and return its output.",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": {"type": "string"},
                        "cwd": {"type": "string"}
                    },
                    "required": ["command"]
                }),
                Access::Exec,
            )
        }
        fn invoke(&self, _ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
            self.ran.store(true, std::sync::atomic::Ordering::SeqCst);
            Invocation::ok(format!("ran {}", args["command"]))
        }
    }

    /// **What the gate was shown**, which is the half of the rewrite that is not
    /// the file on disk: a `write` to a path, adjudicated as one.
    #[derive(Clone, Default)]
    struct Seen(std::sync::Arc<std::sync::Mutex<Vec<(String, Access, Value)>>>);

    impl Seen {
        fn calls(&self) -> Vec<(String, Access, Value)> {
            self.0.lock().unwrap().clone()
        }
    }

    impl Gate for Seen {
        fn admit(&mut self, call: &GateCall<'_>) -> GateDecision {
            self.0
                .lock()
                .unwrap()
                .push((call.name.to_string(), call.access, call.args.clone()));
            GateDecision::Admit
        }
    }

    /// A writable temp tree with a fake `bash`, the real `write`, and a gate that says
    /// what it was asked about. Returns the directory too, so a test can read what
    /// landed.
    fn heredoc_runtime(
        with_write: bool,
    ) -> (
        ToolRuntime,
        Seen,
        std::sync::Arc<std::sync::atomic::AtomicBool>,
        crate::backend::tempdir::TempDir,
    ) {
        let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut reg = Registry::new();
        reg.register(Box::new(FakeBash { ran: ran.clone() }))
            .unwrap();
        if with_write {
            reg.register(Box::new(crate::builtins::write::Write))
                .unwrap();
        }
        let d = crate::backend::tempdir::TempDir::new();
        let backend = crate::backend::HostBackend::writable(d.path()).unwrap();
        let seen = Seen::default();
        let rt = ToolRuntime::new(reg, Box::new(backend)).with_gate(Box::new(seen.clone()));
        (rt, seen, ran, d)
    }

    fn bash_call(command: &str) -> ToolCall {
        call("bash", &serde_json::json!({"command": command}).to_string())
    }

    /// **The operator's own command, through the runtime.** It is a `write` by the
    /// time the gate sees it: the path is a path argument, the access is `Write`, the
    /// file lands with the heredoc's body as its contents, the result carries the
    /// diff a write draws, and the model is told once what happened to its call.
    #[test]
    fn a_heredoc_bash_call_is_adjudicated_recorded_and_drawn_as_a_write() {
        let (rt, seen, ran, d) = heredoc_runtime(true);
        let mut sink = RecordingToolSink::default();
        let r = rt.invoke(
            "t1",
            &bash_call(
                "cd crates && cat > workloads/src/banking.rs <<'RUST'\n\
                 pub fn balance() -> i64 { 0 }\nRUST",
            ),
            &mut sink,
        );

        assert!(
            !ran.load(std::sync::atomic::Ordering::SeqCst),
            "the heredoc reached the shell instead of being recorded as a write: {r:?}"
        );
        let landed = std::fs::read_to_string(d.path().join("crates/workloads/src/banking.rs"))
            .expect("the file the heredoc wrote");
        assert_eq!(landed, "pub fn balance() -> i64 { 0 }\n");

        let seen = seen.calls();
        assert_eq!(seen.len(), 1, "the gate was asked {seen:?}");
        assert_eq!(seen[0].0, "write", "the gate judged an exec, not a write");
        assert_eq!(seen[0].1, Access::Write);
        assert_eq!(
            seen[0].2["path"], "crates/workloads/src/banking.rs",
            "the gate was given the command rather than the path: {:?}",
            seen[0].2
        );

        assert_eq!(r.name, "write", "the transcript row still says bash");
        let edit = r.edit.as_ref().expect("a write draws a diff");
        assert_eq!(edit.path, "crates/workloads/src/banking.rs");
        assert!(edit.created);
        assert_eq!(edit.after, "pub fn balance() -> i64 { 0 }\n");

        let rendered = r.render();
        assert!(
            rendered.contains(
                "recorded as a write to `crates/workloads/src/banking.rs` \
                 (heredoc through bash is converted; use `write` directly)"
            ),
            "{rendered}"
        );
        assert!(
            !rendered.contains("call `write` with the contents"),
            "a converted call also got the near-miss nudge: {rendered}"
        );
    }

    /// The `cwd` argument is where the shell would have been, so a relative path is
    /// resolved against it.
    #[test]
    fn a_cwd_argument_is_where_the_heredoc_writes() {
        let (rt, seen, _, d) = heredoc_runtime(true);
        let mut sink = RecordingToolSink::default();
        let r = rt.invoke(
            "t1",
            &call(
                "bash",
                &serde_json::json!({
                    "command": "cat > f.rs <<'EOF'\nfn a() {}\nEOF",
                    "cwd": "crates/tools",
                })
                .to_string(),
            ),
            &mut sink,
        );
        assert_eq!(r.name, "write");
        assert_eq!(seen.calls()[0].2["path"], "crates/tools/f.rs");
        assert_eq!(
            std::fs::read_to_string(d.path().join("crates/tools/f.rs")).unwrap(),
            "fn a() {}\n"
        );
    }

    /// **An append is the file that is there plus the body** — `write` replaces
    /// whole contents, and a `>>` that replaced would be a deletion.
    #[test]
    fn an_appending_heredoc_keeps_what_the_file_already_held() {
        let (rt, seen, _, d) = heredoc_runtime(true);
        std::fs::create_dir_all(d.path().join("src")).unwrap();
        std::fs::write(d.path().join("src/lib.rs"), "// the head\n").unwrap();
        let mut sink = RecordingToolSink::default();
        let r = rt.invoke(
            "t1",
            &bash_call("cat >> src/lib.rs <<'EOF'\npub fn a() {}\nEOF"),
            &mut sink,
        );
        assert_eq!(r.name, "write");
        assert_eq!(seen.calls()[0].2["path"], "src/lib.rs");
        assert_eq!(
            std::fs::read_to_string(d.path().join("src/lib.rs")).unwrap(),
            "// the head\npub fn a() {}\n"
        );
        let edit = r.edit.as_ref().expect("a diff");
        assert!(!edit.created);
        assert_eq!(edit.before, "// the head\n");
    }

    /// **A near miss is left to the shell and told what it cost.** `mkdir -p … &&`
    /// is a second command, so this is not the one shape — the call still runs, and
    /// the result carries the one line naming the tools that would have recorded it.
    #[test]
    fn an_unconverted_heredoc_write_carries_the_nudge() {
        let (rt, seen, ran, _d) = heredoc_runtime(true);
        let mut sink = RecordingToolSink::default();
        let r = rt.invoke(
            "t1",
            &bash_call("mkdir -p d && cat > d/f <<'EOF'\nx\nEOF"),
            &mut sink,
        );
        assert!(
            ran.load(std::sync::atomic::Ordering::SeqCst),
            "a command that is not this shape was taken away from the shell"
        );
        assert_eq!(r.name, "bash");
        assert_eq!(seen.calls()[0].0, "bash");
        assert!(r.edit.is_none());
        assert!(
            r.render().contains("call `write` with the contents"),
            "{}",
            r.render()
        );
    }

    /// **A seat with no `write` keeps its shell.** The gatekeeper holds `bash` and no
    /// writing tool, and a rewrite there would turn a working command into "no tool
    /// called `write` is available here".
    #[test]
    fn a_seat_without_write_is_not_rewritten_and_not_nudged() {
        let (rt, seen, ran, d) = heredoc_runtime(false);
        let mut sink = RecordingToolSink::default();
        let r = rt.invoke("t1", &bash_call("cat > f <<'EOF'\nx\nEOF"), &mut sink);
        assert!(ran.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(seen.calls()[0].0, "bash");
        assert_eq!(r.name, "bash");
        assert!(
            !r.render().contains("`write`"),
            "a seat that cannot write was told to write: {}",
            r.render()
        );
        assert!(!d.path().join("f").exists());
    }

    /// **The operator's own line is not rewritten.** `invoke_operator` is the `!`
    /// line and the head-run door: what a person typed is what runs.
    #[test]
    fn the_operators_own_heredoc_is_left_as_they_typed_it() {
        let (rt, seen, ran, _d) = heredoc_runtime(true);
        let mut sink = RecordingToolSink::default();
        let r = rt.invoke_operator("t1", &bash_call("cat > f <<'EOF'\nx\nEOF"), &mut sink);
        assert!(ran.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(r.name, "bash");
        assert!(seen.calls().is_empty(), "the ungated path asked the gate");
    }

    // -------------------------------------------------------------------------------------
    // `on_tool_call` (§4.1): a plugin layer sees the call before the gate
    // -------------------------------------------------------------------------------------

    /// **A gate that records what it was asked about and admits**, so a test can read the
    /// arguments the gate adjudicated rather than trusting that it saw the right ones.
    struct RecordingGate {
        seen: std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
    }

    impl Gate for RecordingGate {
        fn admit(&mut self, call: &GateCall<'_>) -> GateDecision {
            self.seen
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(call.args.clone());
            GateDecision::Admit
        }
    }

    /// **A tool that says whether it ran** — the difference between a call a layer refused and a
    /// call whose result a layer replaced, which no outcome alone can tell apart.
    struct RanProbe {
        ran: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl Tool for RanProbe {
        fn schema(&self) -> ToolSchema {
            ToolSchema::new(
                "probe",
                "Do a thing. Takes a path.",
                serde_json::json!({
                    "type": "object",
                    "properties": {"path": {"type": "string"}},
                    "required": ["path"],
                }),
                Access::Exec,
            )
        }
        fn invoke(&self, _ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
            self.ran.store(true, std::sync::atomic::Ordering::SeqCst);
            Invocation::ok(format!("probed {}", args["path"]))
        }
    }

    /// **A plugin layer, answered from a fixed verdict**, and recording every call it was shown.
    struct FixedHook {
        answer: Option<letibot_plugin::tool_call::ToolCallOut>,
        seen: std::sync::Arc<std::sync::Mutex<Vec<letibot_plugin::Call>>>,
    }

    impl CallHook for FixedHook {
        fn before(&self, call: &letibot_plugin::Call, _standing: &Standing) -> Option<Patched> {
            self.seen
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(call.clone());
            self.answer.clone().map(|out| Patched {
                out,
                by: "p1".to_string(),
            })
        }
    }

    /// A runtime over one `probe` tool, a recording gate, and a hook that answers `answer`.
    fn hooked(
        answer: Option<letibot_plugin::tool_call::ToolCallOut>,
    ) -> (
        ToolRuntime,
        std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
        std::sync::Arc<std::sync::Mutex<Vec<letibot_plugin::Call>>>,
        std::sync::Arc<std::sync::atomic::AtomicBool>,
        crate::backend::tempdir::TempDir,
    ) {
        let d = crate::backend::tempdir::TempDir::new();
        let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut reg = Registry::new();
        reg.register(Box::new(RanProbe { ran: ran.clone() }))
            .unwrap();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let rt = ToolRuntime::new(
            reg,
            Box::new(crate::backend::HostBackend::new(d.path()).unwrap()),
        )
        .with_gate(Box::new(RecordingGate { seen: seen.clone() }))
        .with_call_hook(std::sync::Arc::new(FixedHook {
            answer,
            seen: calls.clone(),
        }));
        (rt, seen, calls, ran, d)
    }

    /// **The rewritten call is the call the gate adjudicates** — the whole of §4.1's safety: a
    /// layer can change what a call asks for, and what it asks for then meets the same gate.
    ///
    /// This is the property that keeps *tighten only* true through a rewrite: the model asked for
    /// one path, the layer asked for another, and the gate was asked about the OTHER one.
    #[test]
    fn a_rewritten_call_is_the_call_the_gate_adjudicates() {
        let (rt, seen, _calls, ran, _d) =
            hooked(Some(letibot_plugin::tool_call::ToolCallOut::rewrite(
                serde_json::json!({"path": "/tmp/the-rewritten-one"}),
            )));
        let mut sink = RecordingToolSink::default();
        let r = rt.invoke(
            "t1",
            &call("probe", r#"{"path":"/tmp/as-the-model-asked"}"#),
            &mut sink,
        );
        assert!(ran.load(std::sync::atomic::Ordering::SeqCst));
        let judged = seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert_eq!(judged.len(), 1, "the gate was asked once: {judged:?}");
        assert_eq!(
            judged[0]["path"],
            serde_json::json!("/tmp/the-rewritten-one"),
            "the gate judged the model's arguments, not the layer's"
        );
        assert!(
            r.render().contains("the-rewritten-one"),
            "and the tool ran with them: {}",
            r.render()
        );
        assert!(
            r.render().contains("rewrote this call's arguments"),
            "the result says the call was rewritten, so the row is not a mystery: {}",
            r.render()
        );
    }

    /// **A layer can refuse a call, and then nothing runs.** The outcome says who refused and the
    /// payload is the layer's own sentence, which is what the model acts on.
    #[test]
    fn a_refused_call_does_not_run() {
        let (rt, seen, _calls, ran, _d) = hooked(Some(
            letibot_plugin::tool_call::ToolCallOut::refuse("already watched as pr-24; use that"),
        ));
        let mut sink = RecordingToolSink::default();
        let r = rt.invoke("t1", &call("probe", r#"{"path":"/tmp/x"}"#), &mut sink);
        assert!(
            !ran.load(std::sync::atomic::Ordering::SeqCst),
            "it ran anyway"
        );
        assert!(
            seen.lock().unwrap_or_else(|e| e.into_inner()).is_empty(),
            "a refused call never reached the gate"
        );
        assert!(
            matches!(&r.outcome, ToolOutcome::NotRun { why } if why.contains("p1")),
            "{:?}",
            r.outcome
        );
        assert!(r.render().contains("already watched as pr-24"));
    }

    /// **A layer can answer a call without letting it run** — §4.1's `answer`, which is the model
    /// asking something the layer already knows.
    #[test]
    fn a_layer_can_answer_a_call_without_running_it() {
        let (rt, _seen, _calls, ran, _d) =
            hooked(Some(letibot_plugin::tool_call::ToolCallOut::answer(
                "pr-24 is already watched for its checks",
            )));
        let mut sink = RecordingToolSink::default();
        let r = rt.invoke("t1", &call("probe", r#"{"path":"/tmp/x"}"#), &mut sink);
        assert!(
            !ran.load(std::sync::atomic::Ordering::SeqCst),
            "it ran anyway"
        );
        assert_eq!(r.outcome, ToolOutcome::Ok);
        assert!(r.render().contains("already watched for its checks"));
    }

    /// **`pass`, and a layer that says nothing, both leave the call exactly as it was.**
    #[test]
    fn a_pass_runs_the_call_as_the_model_proposed_it() {
        for answer in [Some(letibot_plugin::tool_call::ToolCallOut::pass()), None] {
            let (rt, seen, _calls, ran, _d) = hooked(answer);
            let mut sink = RecordingToolSink::default();
            let r = rt.invoke(
                "t1",
                &call("probe", r#"{"path":"/tmp/as-asked"}"#),
                &mut sink,
            );
            assert!(ran.load(std::sync::atomic::Ordering::SeqCst));
            assert_eq!(r.outcome, ToolOutcome::Ok);
            let judged = seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
            assert_eq!(judged[0]["path"], serde_json::json!("/tmp/as-asked"));
            assert!(!r.render().contains("rewrote this call"));
        }
    }

    /// **The operator's own run is not shown to the layer.** `invoke_operator` is the `!` line and
    /// the head-run door — a person's own act, adjudicated at the door — and a layer rewriting it
    /// would be policy over the person, which is the rule the heredoc re-spelling follows too.
    #[test]
    fn the_operators_own_call_is_not_shown_to_the_layer() {
        let (rt, _seen, calls, ran, _d) = hooked(Some(
            letibot_plugin::tool_call::ToolCallOut::refuse("not on my watch"),
        ));
        let mut sink = RecordingToolSink::default();
        let _ = rt.invoke_operator("t1", &call("probe", r#"{"path":"/tmp/x"}"#), &mut sink);
        assert!(
            ran.load(std::sync::atomic::Ordering::SeqCst),
            "the operator's call was stopped"
        );
        assert!(
            calls.lock().unwrap_or_else(|e| e.into_inner()).is_empty(),
            "the ungated path showed the call to the layer"
        );
    }

    /// **A rewrite the tool would refuse is reported, not run** — a layer's arguments go through
    /// the same `salvage` the model's do, so a layer cannot put a shape past the schema.
    #[test]
    fn a_rewrite_the_tool_refuses_does_not_run() {
        let (rt, seen, _calls, ran, _d) =
            hooked(Some(letibot_plugin::tool_call::ToolCallOut::rewrite(
                serde_json::json!({"nothing": "the schema wants"}),
            )));
        let mut sink = RecordingToolSink::default();
        let r = rt.invoke("t1", &call("probe", r#"{"path":"/tmp/x"}"#), &mut sink);
        // `salvage` repairs what it can; a `required` path it cannot invent is a refusal.
        if ran.load(std::sync::atomic::Ordering::SeqCst) {
            // A repair was possible: then the gate must have judged the REPAIRED arguments.
            let judged = seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
            assert!(
                !judged.is_empty(),
                "a repaired rewrite ran without being adjudicated"
            );
        } else {
            assert!(
                matches!(r.outcome, ToolOutcome::NotRun { .. }),
                "{:?}",
                r.outcome
            );
            assert!(
                r.render().contains("p1"),
                "the layer is named: {}",
                r.render()
            );
        }
    }
}
