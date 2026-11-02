//! The runtime: between a `ToolCall` coming out of the parser and a
//! `TranscriptItem::ToolResult` going back in.
//!
//! Every clause that is not a property of one tool lives here: args salvaged before a
//! tool sees them, the [`Gate`] consulted only for a non-`Read` tool, every payload
//! spilled, and an unknown tool name answered with the nearest match.

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
/// Three outcomes and not two: `NotAsked` is nobody having been asked, which is
/// not a refusal, and the sentence a caller says differs for each.
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
    /// `None` for every tool that changed nothing, including a write tool whose
    /// call was a no-op: an empty diff card is indistinguishable from a bug.
    pub edit: Option<crate::edit::FileEdit>,
    /// Paths this call could not see because they are outside the session's view.
    /// Read off the boundary, not guessed from the output.
    ///
    /// Non-empty is a dead end the runtime can act on: it raises a grant decision.
    pub needs_in_view: Vec<std::path::PathBuf>,
    /// Bytes this call read that are not text, which a provider can be sent.
    ///
    /// Beside [`Invocation::edit`] and unlike it: `edit` is display-only, and this
    /// one exists to be sent. `None` for every tool that read no picture.
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

    /// The deadline passed and the command was killed, not left running in the
    /// background. The payload says how to ask for more time.
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

    /// Nothing ran: not a denial, not an abstention, not a failure.
    ///
    /// The `why` sentence comes from [`crate::attach`], so every tool with nothing
    /// behind it produces the same shape.
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

    /// Still running, and here is the handle: not finished and not failed.
    ///
    /// The fourth outcome needs its own constructor, or an author reaches for
    /// whichever of the other three is nearest and says something false.
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
    /// The most lines `read` returns when the call named no `limit`. The tool's
    /// own note hands back the offset that continues it, so nothing is lost.
    pub max_read_lines: usize,
    /// The most bytes of numbered text `read` returns in one call, whatever the
    /// line count. Minified javascript is one enormous line, so the line cap
    /// cannot bound a file and this budget is the guarantee.
    pub max_read_bytes: usize,
    /// The most characters of one line `read` shows before it clips the rest. A
    /// clipped line is named in the notes, because a model that cannot see a
    /// boundary will assume there is none.
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

/// Is the operator waiting to say something?
///
/// A closure rather than a flag because the answer changes while a call runs, and
/// `crates/tools` must not know what a hub is.
pub type OperatorWaiting = std::sync::Arc<dyn Fn() -> bool + Send + Sync>;

/// Is this job's completion already on its way to the model?
///
/// The daemon delivers a settled job as a turn of its own, so waiting on it would
/// block on an answer already in flight. Keyed by job id, because the promise is
/// per job: two waits in one session must get different answers.
pub type CompletionDelivered = std::sync::Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// Why a call says it wants a result the moment it exists.
///
/// A completion nothing waits on is held and handed over at the next break, so
/// these are the two things a call can say that the plan's own structure does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WantedNow {
    /// The call is waiting on it, so the wait must return at once rather than at
    /// the next break.
    Awaited,
    /// The work was started with `wake: "now"`.
    WakeNow,
    /// The model said it does not need waking for it, so a caller's `wait` does not
    /// hold on it: what a server left running is.
    WakeLater,
}

impl WantedNow {
    /// The word the delivery record and the notices use.
    pub fn word(self) -> &'static str {
        match self {
            WantedNow::Awaited => "awaited",
            WantedNow::WakeNow => "wake_now",
            WantedNow::WakeLater => "wake_later",
        }
    }
}

/// Tell the daemon a result is wanted the moment it exists. See [`WantedNow`].
///
/// The daemon owns the state, so `crates/tools` hands over the fact and never
/// learns what a watcher is.
pub type CompletionWanted = std::sync::Arc<dyn Fn(&str, WantedNow) + Send + Sync>;

/// What the daemon is told about the operator's own run, while it runs.
///
/// The tool reports and the daemon decides, because a card is raised on another
/// thread while this one keeps waiting. Carries a question, a command and a job,
/// never a secret: a prompt card is drawn in the open and a password must not be.
pub enum OperatorRun<'a> {
    /// This run can be answered: it has a stdin this daemon holds, and this is the
    /// handle to it. It is what makes the manual way in work when no card is raised.
    Answerable {
        /// The job's handle, as `job_list` spells it.
        job: &'a str,
        /// The command as the operator typed it, verbatim.
        command: &'a str,
        /// The way in, cloned out of the job: the answer arrives on a thread that
        /// does not hold the job and must not have to find it.
        stdin: crate::exec::Stdin,
    },
    /// This run is waiting for an answer, by [`crate::exec::ask`]'s reading of the
    /// process and not of its words.
    Waiting {
        /// The job's handle.
        job: &'a str,
        /// The last thing the program said, for the card to show, or `None` when it
        /// said nothing at all. Nothing anywhere decides anything by this string.
        question: Option<&'a str>,
    },
    /// This daemon cannot tell whether the run is waiting: one of its processes could
    /// not be looked at.
    ///
    /// Not a card, because a card is a reading of the process and this is a guess, and
    /// `sudo` is the case: the process that waits at a prompt is not ours to read.
    Unreadable {
        /// The job's handle.
        job: &'a str,
        /// Whether the run had been quiet for a beat when this was reported. The
        /// report is made either way: the absence of a reading is about permissions
        /// and not the clock, and the sentence still has to say which fact it is.
        quiet: bool,
    },
    /// The run is over, so whatever card was up for it comes down: a card outliving
    /// its command takes an answer into a program that has already exited.
    Ended {
        /// The job's handle.
        job: &'a str,
    },
}

/// See [`OperatorRun`]. `None` in a runtime with no daemon behind it, and every
/// operator run then behaves as if there were no card at all.
pub type OperatorRuns = std::sync::Arc<dyn Fn(OperatorRun<'_>) + Send + Sync>;

pub struct InvokeCtx<'a> {
    pub backend: &'a dyn ExecBackend,
    pub spiller: &'a Spiller,
    /// What this session has shown the model, and therefore what it may change.
    ///
    /// On the context rather than on each tool, so two tools cannot keep two ledgers.
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
    /// Whether this call is the operator's own, which decides whether the command meets the
    /// world their console gives it: a pty, their shell's rc, pagers that cannot page.
    ///
    /// Derived from the gated and ungated paths, so a caller cannot invent a third answer.
    pub tty: bool,
    /// The gate, for a question the tool must put before it starts. `None` on the
    /// operator's own run, which has nobody left to ask.
    gate: Option<&'a std::sync::Mutex<Box<dyn Gate>>>,
    /// Whether a plan row could wait on what this call starts, which is the only
    /// case where the `wait_on` offer on a background start is worth saying.
    pub plan_open: bool,
    /// Whether the session this call belongs to ends when this run ends: a one-shot,
    /// which has no daemon to come back to.
    ///
    /// The tool has to act on it, because a command left in the background is watched
    /// by a daemon and read by a head, and a one-shot has neither.
    pub one_shot: bool,
    /// Where a write's provenance goes. `None` in a runtime with no daemon behind it,
    /// and then nothing is recorded.
    provenance: Option<&'a dyn crate::provenance::Sink>,
}

impl InvokeCtx<'_> {
    /// Say that this call wrote these bytes here, recorded at the write.
    ///
    /// Called after the backend accepted the write, so a refused one records nothing,
    /// and never for a remote placement, whose paths do not exist on this box.
    pub fn wrote(&self, path: &str, bytes: &[u8]) {
        let Some(sink) = self.provenance else {
            return;
        };
        if self.backend.host_label().is_some() {
            return;
        }
        let workspace = self.backend.workspace_path().map(std::path::PathBuf::from);
        let absolute =
            crate::provenance::absolute(std::path::Path::new(path), workspace.as_deref());
        sink.wrote(&absolute, bytes);
    }

    /// Put a call to the gate without a card: the door a tool of an unpinned plugin
    /// layer goes through.
    ///
    /// `None` means this runtime has no gate, which the caller must read as fail-closed,
    /// and the lock is held around the ladder so no adjudicator can be asked.
    pub fn admit_unasked(
        &self,
        name: &str,
        access: crate::schema::Access,
        args: &serde_json::Value,
    ) -> Option<GateDecision> {
        let gate = self.gate?;
        let scripts: [crate::runtime::ScriptSource; 0] = [];
        // The same workspace a call of the model's own reports, so a `permission` rule written for
        // one covers the other: `workspace_path()` is the directory the session works in, and
        // `root_path()` the fallback for a substrate that only knows its root.
        let workspace = self
            .backend
            .workspace_path()
            .or_else(|| self.backend.root_path())
            .unwrap_or_else(|| self.backend.describe());
        let call = GateCall {
            name,
            access,
            args,
            turn_id: self.turn_id,
            call_id: self.call_id,
            workspace: &workspace,
            target_exists: None,
            scripts: &scripts,
        };
        Some(
            gate.lock()
                .unwrap_or_else(|e| e.into_inner())
                .admit_unasked(&call),
        )
    }

    /// Put a memory ask above the ceiling to the operator. See [`Gate::grant_memory`].
    ///
    /// The lock is held for the question, as `admit` takes it: a card may wait minutes
    /// for a person, and a `/mode` typed meanwhile must not move the gate.
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

    /// Say that the call is still working, which counts as liveness, so a tool that
    /// walks a large tree must produce it.
    pub fn progress(&mut self, note: impl Into<String>) {
        self.sink.emit(ToolEvent::Progress {
            turn_id: self.turn_id.to_string(),
            call_id: self.call_id.to_string(),
            note: note.into(),
        });
    }

    /// The operator has typed something and is waiting for this call to end.
    ///
    /// A tool that runs for minutes should ask and stop: what it has read is reported,
    /// and the operator's sentence very often makes the rest of the work pointless.
    pub fn operator_waiting(&self) -> bool {
        self.operator_waiting.map(|f| f()).unwrap_or(false)
    }

    /// This job's completion is already being delivered to you.
    ///
    /// `false` when nobody wired the question, so an unwired runtime blocks: a wait
    /// is never wrong, only slow.
    pub fn completion_delivered(&self, job: &str) -> bool {
        self.completion_delivered.map(|f| f(job)).unwrap_or(false)
    }

    /// Say that this handle's result is wanted the moment it exists. See [`WantedNow`].
    /// A no-op when nobody wired it: a runtime with no daemon has no batch to hold
    /// anything in.
    pub fn want_completion(&self, handle: &str, why: WantedNow) {
        if let Some(f) = self.completion_wanted {
            f(handle, why);
        }
    }

    /// Tell the daemon something about the operator's own run. See [`OperatorRun`].
    ///
    /// A no-op when nobody wired it, and then the run is one a person answers with
    /// `!send`, which needs no card at all.
    pub fn operator_run(&mut self, what: OperatorRun<'_>) {
        if let Some(f) = self.operator_runs {
            f(what);
        }
    }

    /// Whether anybody is listening. The one thing a tool may branch on, because work
    /// done to report something nobody wired pays for a card that cannot be raised.
    pub fn operator_runs_wired(&self) -> bool {
        self.operator_runs.is_some()
    }

    pub fn call_id(&self) -> &str {
        self.call_id
    }

    /// The turn this call belongs to.
    ///
    /// Needed by a tool whose result is a fact about the turn rather than about the
    /// tree: "declared this turn and nothing ran this turn" is the whole question.
    pub fn turn_id(&self) -> &str {
        self.turn_id
    }
}

/// One tool.
pub trait Tool: Send + Sync {
    fn schema(&self) -> ToolSchema;

    /// `args` has already been salvaged against the schema, so the shape is right or
    /// the object is empty. A missing argument is still the tool's to handle: filling
    /// one in would be inventing meaning.
    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation;

    /// What THIS call does, when a tool's verbs differ in kind: the schema's class
    /// covers the whole tool, so a read verb would be adjudicated like a write one.
    ///
    /// The default is the schema's class. A tool may only narrow, never widen.
    fn access_for(&self, _args: &Value) -> Option<crate::schema::Access> {
        None
    }

    /// Whether this tool holds a plan with an open row, `Some` only from the tool that
    /// keeps the board. Read once per call into [`InvokeCtx::plan_open`].
    fn open_plan(&self) -> Option<bool> {
        None
    }
}

/// One gated call, as the gate sees it.
///
/// The turn and call ids let an audit row be correlated, and `target_exists` lets the
/// gate say whether an action is reversible.
#[derive(Debug, Clone, Copy)]
pub struct GateCall<'a> {
    /// The tool as declared, not as the model spelled it.
    pub name: &'a str,
    pub access: Access,
    /// Already salvaged (clause 2), so the gate reads the same values the tool will.
    pub args: &'a Value,
    pub turn_id: &'a str,
    pub call_id: &'a str,
    /// What the backend calls itself, so the operator can see where a write would
    /// land.
    pub workspace: &'a str,
    /// Whether the `path` argument names something that exists. `None` means the call
    /// has no path argument, which is a different fact from the path not being there.
    pub target_exists: Option<bool>,
    /// The scripts this command will run, read from disk.
    ///
    /// `python3 script.py` otherwise shows the classifier a filename and nothing to
    /// judge, so the bytes travel in the brief. Read up front rather than on request,
    /// because an oracle that has to ask can forget and then judge blind.
    pub scripts: &'a [ScriptSource],
}

/// The command line the gate reads for an argv somebody declared: one word per
/// element, each quoted so nothing can be re-split or expanded.
///
/// The gate's ladder is written over a command string, so the quoting is what makes
/// the string and the argv the same thing.
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
    /// The bytes, or why there are none. Not `Option<String>`: "too big", "not there"
    /// and "these are the bytes" are three facts, and an adjudicator that cannot tell
    /// them apart reads a missing file as an empty one.
    pub body: ScriptBody,
}

/// The scripts a shell command will run, read through `read` so that the half which decides
/// the tier and the half which decides permission see one set of bytes.
///
/// A path in the secret store is never opened: naming it is a fact, putting a key in the
/// brief is not. A script that changed while the gate was answering is refused, not run.
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
        // The same unwrapping the classifier does, so `sudo python3 foo.py` has its file
        // read here and scanned by layer A rather than by neither.
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
        // Before the open, not after: a store path is named and not read, and the sentence
        // says which store so a reader of the brief is told what is there.
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
/// Past the cap the head is kept and the omission stated, so an adjudicator knows it is
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
    /// Named but not readable, and why: missing, a directory, not UTF-8.
    Unreadable(String),
}

impl GateCall<'_> {
    /// Whether the `path` argument stays inside the workspace, decided lexically and
    /// before anything is opened.
    ///
    /// [`crate::backend::HostBackend::resolve`] does the stricter canonicalising check,
    /// which catches a symlink out of the tree, but only once a path is actually used.
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

/// The gate, from the tool side.
///
/// Consulted only for a call whose declared access is not [`Access::Read`]: no code
/// path leads from a read-only tool to a question. `Send + Sync` so the runtime can be
/// moved across threads.
pub trait Gate: Send + Sync {
    fn admit(&mut self, call: &GateCall<'_>) -> GateDecision;

    /// The same decision, for a call nobody may be asked about.
    ///
    /// A plugin look runs on the session's monitor thread, where a card would park the
    /// watch until a person answered, so the ladder settles it and the ask does not run.
    fn admit_unasked(&mut self, call: &GateCall<'_>) -> GateDecision {
        self.admit(call)
    }

    /// How this session stands, for a hook that must be told: the session's id and the
    /// mode's own word. The default is empty, which is the honest nothing.
    ///
    /// Asked of the gate because the mode is the gate's own field and moves at runtime.
    fn standing(&mut self) -> Standing {
        Standing::default()
    }

    /// May this session see `path`? Asked when a call named something the boundary hid, so
    /// the operator gets the question instead of the model getting a dead end.
    ///
    /// The default is `NotAsked`: not a refusal, because nobody decided.
    fn grant_view(&mut self, _path: &std::path::Path, _tool: &str) -> ViewGrant {
        ViewGrant::NotAsked
    }

    /// May this call run with more memory than the model may take on its own? Asked before
    /// the command starts, when its ask is above the ceiling or is `uncapped`.
    ///
    /// The default is `NotAsked`, and a call does not run on a question nobody answered.
    fn grant_memory(
        &mut self,
        _ask: &crate::exec::memory::MemoryAsk,
    ) -> crate::exec::memory::MemoryGrant {
        crate::exec::memory::MemoryGrant::NotAsked
    }

    /// Who is adjudicating, for the daemon's startup disclosure.
    ///
    /// Defaults to naming the absence: a gate that does not identify an adjudicator does
    /// not have one, and a hard-coded banner gets that case wrong.
    fn describe(&self) -> String {
        "none (no adjudicator attached)".into()
    }

    /// Turn supervision on or off while the session is running.
    ///
    /// Supervision is the gate's and not the mode's, so it moves without rebuilding
    /// anything. The default refuses: a gate with no advisor must not report success.
    fn set_supervision(&mut self, _on: bool) -> Result<String, String> {
        Err("this gate has no adjudicator, so there is nothing to supervise".into())
    }

    /// Whether the guard model currently gets a turn on every call.
    fn supervising(&self) -> bool {
        false
    }

    /// Move this gate to another point in mode-space, now.
    ///
    /// The gate reads its mode at decision time, so this is a field write. Standing grants
    /// are dropped: a grant answered a question asked under the old point, and keeping it
    /// across a tightening would leak permission.
    fn set_mode(&mut self, _mode: crate::mode::Mode) -> Result<usize, String> {
        Err("this gate has no mode to move".into())
    }

    /// Attach a guard model to a session that opened without one, so that turning
    /// supervision on never means restarting anything. Replaces whatever was there.
    ///
    /// The default refuses, for `set_supervision`'s reason.
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
        /// What the model is told, which is not what the audit records: the model sees the
        /// derived outcome, not the deliberation. Empty for a plain deny, whose reason stays
        /// in the row.
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

/// The gate for a session with nothing attached, and it refuses.
///
/// Not a deny-all: `Denied` means a decision was made and none was made here. Use
/// [`crate::adjudicate::AdjudicatedGate::closed`] where an audit row is wanted.
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

/// How a session stands, as a hook is told it: the id a session is known by, and the
/// mode's own word. An empty pair is a runtime whose gate has neither.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Standing {
    pub session: String,
    pub mode: String,
}

/// A plugin layer's answer about one call, and the layer that gave it: a call that was
/// changed must say who changed it.
#[derive(Debug, Clone, PartialEq)]
pub struct Patched {
    pub out: letibot_plugin::tool_call::ToolCallOut,
    pub by: String,
    /// A line the harness puts above the result, for an answer the model will read as
    /// though the tool had said it. `by` is a layer id, which needs `plugin stack` beside it
    /// to mean anything, so the host composes this from the layer's own name.
    pub note: Option<String>,
}

/// A hook that sees a tool call before the gate.
///
/// It cannot admit anything: the rewritten call is gated like any other, and no field
/// lets a layer say allow. A trait here for the reason [`Gate`] is one, since this crate
/// holds no plugin stack.
pub trait CallHook: Send + Sync {
    /// What the layer says about this call, or `None` for nothing said: run it as it is.
    fn before(&self, call: &letibot_plugin::Call, standing: &Standing) -> Option<Patched>;
}

/// What [`ToolRuntime::plugin_patch`] resolved to.
#[allow(clippy::large_enum_variant)] // Done already owns its ToolResult by value; boxing adds an alloc per patch
enum Patch {
    /// Nothing said: run the call as the model proposed it.
    None,
    /// Run this instead.
    Rewritten(Rewritten),
    /// Do not run it: this is the result the model reads.
    Done(ToolResult),
}

/// A role's tool set, and the ceiling its seat may reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Role {
    pub name: String,
    pub tools: Vec<String>,
    pub max_tools: usize,
}

/// The default ceiling. Small models get worse at choosing tools past a handful, so the
/// ceiling is a hard stop, and a role that needs more declares its own number.
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

/// The role table. Roles naming tools that do not exist fail to resolve, loudly, which
/// is how a missing tool is noticed rather than assumed.
pub mod roles {
    use super::Role;

    pub fn orchestrator() -> Role {
        Role::new(
            "orchestrator",
            // `task_result` beside `task`: `task` hands back a handle, so a seat with
            // `task` and no way to collect it could start work and never read it. The rest
            // of the family follows: correct it, resume it, place it, find it a host.
            &[
                "task",
                "task_result",
                "task_message",
                "task_resume",
                "task_start",
                "hosts",
                // `room`: the host's queues. Named here because this is the role table, but
                // nothing seats this role in production: the live orchestrator seat is
                // [`m1_orchestrator`], so a capability added here belongs there too.
                "room",
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
                // A subagent seats this role and arrives with none of the conversation it is
                // working on, so `transcript` is how it checks what the operator decided
                // instead of asking its parent to paste it.
                "transcript",
                // The plugin loop, wherever a session writes code: `plugin` writes, builds,
                // tests and loads a pane, and `plugin_hooks` is the contract it is written
                // against. A seat that can write Rust but not run it shows nobody anything.
                "plugin",
                "plugin_hooks",
            ],
        )
    }

    /// leticode's seat: opencode's toolset, by opencode's names, plus `skill`.
    ///
    /// `bash` is stripped unless the daemon was started with `--bash`, exactly as for
    /// `coder`.
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
                // `notes` beside `write`/`edit`: a standing note is a write to the
                // operator's tree. It is the entry that moves `max_tools` below.
                "notes",
                "todo_write",
                "skill",
                "lsp",
                "task",
                "task_result",
                "task_message",
                "task_resume",
                // `hosts`: which machines exist and which is least loaded, read from the
                // operator's file at the call because the prompt is a cache. Seated beside
                // `task`, whose `where: "host:NAME"` it is the way to choose.
                "hosts",
                // `task_start` beside `task`: a seat that can start a child but not arrange
                // its placement spawns it into the main tree and then kills it for being in
                // the wrong place.
                "task_start",
                // The background-job surface, so a backgrounded `bash` can be waited, read,
                // killed and listed. Beside `bash` and not instead of it: a background task
                // without `job_wait` is a handle the model cannot follow up on.
                "job_list",
                "job_output",
                "job_wait",
                "job_kill",
                "monitor",
                // `pkill`: find by a string, kill by pid, never itself. Seated
                // with the exec surface, because signalling a process is one.
                "pkill",
                // `ps`: the question `ps | grep -v grep` is usually asked to answer, as a
                // read-only table that never lists this process. Seated with the shell.
                "ps",
                // `fanout` and `scenario`: work on many hosts at once. Beside `bash` and
                // `hosts`, because a seat that reaches one machine at a time hand-rolls
                // `ssh ... &` and a `ps` poll.
                "fanout",
                "scenario",
                // `room`: the host's tagged queues, the only place two sessions that are not
                // parent and child can talk. The two seats that carry it are this one and
                // [`m1_orchestrator`], because the room refuses a subagent at its own door.
                "room",
                // `harness`: what this session is running inside. Read-only, and how a model
                // stops asking the operator to read their own terminal aloud.
                "harness",
                // `host_changelog`: what this machine's settings were, are now, and on whose
                // words. Seated with `bash` because the change itself is a `bash` call, and
                // the tool applies nothing: `revert` hands the old value back.
                "host_changelog",
                // `host_config`: this box's own settings in the platform's vocabulary, with the
                // privilege stated and every change recorded by `host_changelog`. `set` declares
                // `Exec`; `settings` and `read` narrow to a read.
                "host_config",
                // `transcript` and `digest`: this session's own conversation, including what
                // compaction replaced. Seated together because `transcript` names `digest` as
                // the way to ask about the rest, and that offer is a lie on a seat with one.
                "transcript",
                "digest",
                // `decisions`: what the gate decided and why, so "why was that refused" is
                // not a question put to the operator.
                "decisions",
                // `merge_gate`: the merge queue's gate for this repository and the choices its
                // files suggest, read-only. The agent writes the section the operator chose.
                "merge_gate",
                // `queue`: read the merge queue, re-gate the target, restart or withdraw a
                // row, propose the target. A project's agent should get its own red main out
                // of trouble rather than send the operator to type it.
                "queue",
                // `plugin` and `plugin_hooks`: a pane written and loaded in the session, which
                // is the point of the whole plan. `plugin_hooks` sits beside it for
                // `transcript`'s reason: the tool that reports a refusal names the contract.
                "plugin",
                "plugin_hooks",
                // `ask_user_question`: the seat can ask the person, not only tell them. A
                // choice carries the operator's own note, and abstaining is an answer.
                // `Access::Session`: it changes nothing on disk and interrupts a person.
                "ask_user_question",
            ],
        );
        // The ceiling is a guard against a prompt nobody counted, so each entry above takes
        // its own seat rather than trading one away, and the number is declared here where
        // somebody deciding can read it.
        r.max_tools = 38;
        r
    }

    /// What M1 can actually seat: `orchestrator` without `task`, plus `read_spill`.
    ///
    /// `room` is seated here and not only in [`orchestrator`], because this is the seat a
    /// session actually runs on: a missing `room` is an arbiter told to call a tool it does
    /// not have. No `Write` tool, because this seat opens read-only.
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
                "room",
            ],
        )
    }

    /// `researcher` with the web instead of `search_corpus`, which no build has.
    ///
    /// The role exists so that "which tools would this agent have" has a written answer
    /// rather than one settled per session. `todo_write` takes the eighth seat, and nothing
    /// was displaced to fit it.
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

    /// What a session in plan mode seats: nothing that can change the work.
    ///
    /// A capability boundary and not a flag, so `write` and `edit` are absent from
    /// `tools_json` rather than refused at call time. It keeps `write_plan` and `say`,
    /// because a planner that can neither record nor discuss its plan has nothing.
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

    /// What M2 can seat: `coder` without `bash`.
    ///
    /// `bash` is `Access::Exec` and is not named here: a shell arriving as a side effect
    /// of somebody else's workstream is how a capability arrives without a decision.
    pub fn m2_coder() -> Role {
        Role::new(
            "coder",
            // `todo` is seated here because a role must both do the work and declare what
            // it meant to do, or the intent-versus-effect diff has nothing to diff. The task
            // family stays seated at the depth cap, which is enforced where the call is made.
            &[
                "read",
                "write",
                "edit",
                "grep",
                "glob",
                "read_spill",
                "todo",
                "bash",
                // `notes` is seated beside `write`/`edit` and nowhere else: a note is a
                // write to the operator's tree, since it is read into every later
                // session's prompt, so it rides the write decision.
                "notes",
                "task",
                "task_result",
                "task_message",
                "task_resume",
                "task_start",
                // `hosts`, beside `task`: this seat can place a child on another machine,
                // so it has to be able to see which machines there are.
                "hosts",
            ],
        )
    }

    /// The only role that can run a command.
    ///
    /// The exec surface plus `monitor` takes nine seats, so `max_tools` is 9 here and
    /// declared: a ceiling quietly raised for everybody is not a ceiling. `glob` is what it
    /// gives up, since a shell substitutes for it and nothing substitutes for `read_spill`.
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

    /// The gatekeeper's seat: read-only by construction, and blind to the child's report.
    ///
    /// A reviewer that can edit becomes a second author, and one that reads the child's answer
    /// reviews the story rather than the artifact. `review_verdict` is the one non-read, and it
    /// is `Session`, so a verdict raises no card at a head that does not exist.
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

    /// The notes keeper's seat: the read tools and `notes`, and nothing that can author code.
    ///
    /// `notes` is the one writer and that is the point: it takes a bare name, so no spelling of
    /// a write leaves the notes directory, and a keeper cannot pass its prose off as the
    /// operator's.
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
    /// A role naming a tool this build does not have. Refused rather than seated with a
    /// smaller set, so a missing tool is noticed instead of silently dropped.
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

    /// Register a tool whose description we do not control, such as an MCP server's. The
    /// lint still runs, but its findings are recorded rather than fatal: refusing somebody
    /// else's server over its prose is a policy nobody asked for.
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

    /// The schemas, in registration order. Order is part of the stable prefix, so this is
    /// the order the prompt is built in and it must not depend on a hash map's iteration.
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

    /// Drop every tool of a denied access class. A subagent's downgrade, applied after the
    /// role is resolved: the tools leave the prompt entirely, so the model is not told it
    /// has a capability the gate would refuse.
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

    /// Keep only the tools a set of declared skills allows: a session started with
    /// `skill.allowed-tools` may call those names and no others, so the rest leave the
    /// prompt and the model is not told it has what the list refuses.
    ///
    /// An empty set narrows to nothing; callers pass `None` for no declaration.
    pub fn only_tools(mut self, allowed: &std::collections::BTreeSet<String>) -> Registry {
        self.tools
            .retain(|t| allowed.contains(&t.schema().name.clone()));
        self
    }

    pub fn resolve_role(self, role: &Role) -> Result<Registry, RoleError> {
        // One plan tool per seat. `todo` (the intent ledger's list) and `todo_write` (the
        // board the nag, the pane and `wait_on` read) are two lists with two models, so a
        // seat holding both is told about one and nagged about the other.
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
        // Role order, not registration order: the role is what the prompt is built from,
        // and it is written down in one place.
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
    /// Behind a mutex because the runtime is shared and the gate is the one field that
    /// mutates in place: [`Gate::admit`] takes `&mut self` and an operator decision can hold
    /// it for minutes. It is the gate's mutex, held only across the decision, and the
    /// ungated path never takes it at all, since there is nobody left to ask.
    pub gate: std::sync::Mutex<Box<dyn Gate>>,
    pub limits: Limits,
    /// Session-scoped, and on the runtime rather than on a tool because read-before-write
    /// is a fact about the session, not about `edit`.
    pub files: crate::files::FileLedger,
    /// See [`OperatorWaiting`]. `None` in a runtime with no head behind it.
    pub operator_waiting: Option<OperatorWaiting>,
    /// See [`CompletionDelivered`]. `None` in a runtime with no job watcher behind it.
    pub completion_delivered: Option<CompletionDelivered>,
    /// See [`CompletionWanted`]. `None` in a runtime with no daemon behind it.
    pub completion_wanted: Option<CompletionWanted>,
    /// See [`OperatorRun`]. `None` in a runtime with no daemon behind it, and the
    /// operator's own run then raises no card at all.
    pub operator_runs: Option<OperatorRuns>,
    /// The repair codes the model has been told about in this runtime, so one repair is not
    /// said twice. See [`ToolResult::repairs_told`].
    pub repairs_told: std::sync::Mutex<std::collections::BTreeSet<&'static str>>,
    /// Whether this runtime belongs to a one-shot run, which ends with the process that
    /// started it. See [`InvokeCtx::one_shot`].
    pub one_shot: bool,
    /// The plugin layer's look at a call, before the gate. `None` in a runtime with no plugin
    /// stack, and then the call path is exactly what it was before there were plugins.
    pub call_hook: Option<std::sync::Arc<dyn CallHook>>,
    /// Where a write's provenance goes, recorded at the write. `None` in a runtime with no
    /// daemon behind it, and then nothing is recorded.
    pub provenance: Option<std::sync::Arc<dyn crate::provenance::Sink>>,
}

/// A call the model proposed and this layer reproposed as a different tool.
///
/// One shape produces one today, a `bash` heredoc that is a file write, and it is a struct
/// because a second shape would otherwise be a second four-tuple nobody could read.
struct Rewritten {
    /// The call as it now stands: the same `id`, so the model's result and the transcript
    /// row still answer the call it made, and the other tool's name.
    call: ToolCall,
    args: Value,
    schema: ToolSchema,
    /// The one line the model is told, because a row that says `write` about a `bash` call
    /// is otherwise a difference it cannot account for.
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
            provenance: None,
        }
    }

    /// Wire the fact that this runtime is a one-shot run's: a `letibot "..."`, whose daemon
    /// exits when the prompt's answer does.
    ///
    /// Every other session outlives its head, so a background job has a watcher and a place
    /// for its result to land; a one-shot has neither.
    pub fn with_one_shot(mut self, yes: bool) -> Self {
        self.one_shot = yes;
        self
    }

    /// Wire the question a long-running tool asks before it spends another minute.
    pub fn with_operator_waiting(mut self, f: OperatorWaiting) -> Self {
        self.operator_waiting = Some(f);
        self
    }

    /// Wire the plugin layer that sees a call before the gate. A runtime nobody wired keeps
    /// the call path it had, so a session with no layers behaves as it did by this field being
    /// `None` rather than by a promise.
    pub fn with_call_hook(mut self, hook: std::sync::Arc<dyn CallHook>) -> Self {
        self.call_hook = Some(hook);
        self
    }

    /// Wire where a write's provenance goes. The daemon installs this so a plugin module or
    /// manifest the model writes is recorded at the write; a runtime nobody wired records
    /// nothing.
    pub fn with_provenance(mut self, sink: std::sync::Arc<dyn crate::provenance::Sink>) -> Self {
        self.provenance = Some(sink);
        self
    }

    /// Wire the fact that makes `job_wait` refuse to block on its own promise.
    ///
    /// See [`CompletionDelivered`]. A runtime that wires nothing blocks as it always did.
    pub fn with_completion_delivered(mut self, f: CompletionDelivered) -> Self {
        self.completion_delivered = Some(f);
        self
    }

    /// Wire what the daemon is told about the operator's own run. See [`OperatorRun`].
    ///
    /// A closure and not a trait, because the daemon owns the state the events are about: a
    /// trait would be this crate inventing a vocabulary for a card it must not know exists.
    pub fn with_operator_runs(mut self, f: OperatorRuns) -> Self {
        self.operator_runs = Some(f);
        self
    }

    /// Wire where a call says it wants a result at once. See [`CompletionWanted`].
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

    /// The `write` call a `bash` heredoc stands for, when the command is exactly that shape.
    ///
    /// Two facts are not in the command text, so [`crate::heredoc::recognise`] stays pure: where
    /// the session sits, and what an append would extend. `None` for a seat with no `write`
    /// tool, since a rewrite into a tool the seat lacks turns a working command into a refusal.
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

    /// What a plugin layer says about a call, before the gate, reduced to the three things this
    /// runtime can do with an answer.
    ///
    /// A rewrite cannot widen anything: the only field a layer can change is the arguments, so
    /// the call is one the model could itself have made, judged by the same gate and re-salvaged.
    fn plugin_patch(&self, call: &ToolCall, args: &Value) -> Patch {
        let Some(hook) = self.call_hook.as_ref() else {
            return Patch::None;
        };
        // How the session stands, asked of the gate: the mode is its own field and it moves
        // at runtime, so a copy anywhere else would be a second answer to the same question.
        let standing = self
            .gate
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .standing();
        let asked = letibot_plugin::Call::new(call.name.clone(), args.clone());
        let Some(Patched { out, by, note }) = hook.before(&asked, &standing) else {
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
            letibot_plugin::tool_call::Verdict::Answer(text) => {
                let result = ToolResult::new(call.id.clone(), call.name.clone(), ToolOutcome::Ok)
                    .with_payload(text.to_string());
                // The result is somebody else's words, and it says so: an `answer` never ran
                // the tool, so nothing in the payload tells the model whether a `probe` looked
                // or a `monitor` armed. `[note]` is the channel every harness sentence uses.
                let result = match note {
                    Some(note) => result.with_note(note),
                    None => result,
                };
                Patch::Done(result)
            }
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
    /// Empty for every other tool and for a command naming no script file: `python3 -c` carries
    /// its program in the argv. A file that cannot be read is reported as unreadable rather than
    /// skipped, because silence reads exactly like a command that runs no script at all.
    fn scripts_of(&self, args: &Value) -> Vec<ScriptSource> {
        let Some(command) = args.get("command").and_then(|v| v.as_str()) else {
            return Vec::new();
        };
        scripts_for(command, self.backend.home_path().as_deref(), |path| {
            self.backend.read(path).map_err(|e| e.to_string())
        })
    }

    /// Did a file the gate judged change before the command ran?
    ///
    /// `None` when every body that was read is still those bytes; only read bodies are re-read,
    /// since an unreadable one contributed nothing. A mismatch refuses rather than re-judging,
    /// because a second gate pass would be a decision the operator never saw.
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
    /// `&self`, and that is the design: a turn holds this method for as long as its longest
    /// tool call, and the operator's own run goes through the same runtime at the same time.
    /// The gate, the one `&mut` user, sits behind its own mutex.
    pub fn invoke(
        &self,
        turn_id: &str,
        call: &ToolCall,
        sink: &mut dyn ToolEventSink,
    ) -> ToolResult {
        self.run(turn_id, call, sink, true)
    }

    /// Run one call the OPERATOR made, which the admission has already answered.
    ///
    /// The gate is skipped because the call has already been through the door, so asking
    /// again would double-record the decision or, at `/mode automode`, let a model refuse
    /// the person's own act. `&self` because that run holds its thread for the whole command.
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
            // An unknown tool name is a miss, and clause 1 applies to it: the model gets the
            // list it can choose from and the nearest thing to what it asked for, in the same
            // call.
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

        // A plugin layer sees the call before the gate, and a heredoc through `bash` is
        // proposed as the write it is. Both are gated-path only: the ungated path is the
        // operator's own act, already adjudicated at the door.
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

        // On the gated path only: `! cat > f <<EOF` is a person's own act, already adjudicated
        // at the door, and rewriting what they typed would be deciding their command was not
        // the command they meant.
        let rewritten = if gated {
            self.heredoc_rewrite(call, &args)
        } else {
            None
        };
        let (call, args, schema) = match &rewritten {
            Some(r) => (&r.call, r.args.clone(), r.schema.clone()),
            None => (call, args, schema),
        };

        // Clause 4: the gate is consulted only when the effective access is not a read, so a
        // read-only tool has no code path to a question. `access_for` narrows and never widens,
        // so a tool cannot declare its way past the gate.
        let declared = schema.access;
        let effective = self
            .registry
            .get(&call.name)
            .and_then(|t| t.access_for(&args))
            .filter(|a| a.is_unattended() && !declared.is_unattended())
            .unwrap_or(declared);
        if gated && !effective.is_unattended() {
            // The workspace, not the root: `root_path()` is `/` for an unconfined session,
            // which answers "is this inside the boundary" and not "which directory is this
            // session working in", and this string is the card's first boundary fact.
            let workspace = self
                .backend
                .workspace_path()
                .or_else(|| self.backend.root_path())
                .unwrap_or_else(|| self.backend.describe());
            // And on which machine, when it is not this one: a card naming only the path
            // would ask the operator to decide about a machine it never mentioned.
            let workspace = match self.backend.host_label() {
                Some(host) => format!("{workspace} (on {host})"),
                None => workspace,
            };
            let target_exists = args
                .get("path")
                .and_then(|v| v.as_str())
                .map(|p| self.backend.stat(p).is_some());
            // The code an interpreter was handed, read through the backend so a confined
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
            // The lock is the gate's, held across the decision on purpose: `admit` may wait
            // minutes for a person, and a `/mode` typed while a card is up must not move the
            // gate out from under it. The ungated path never reaches this line.
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
            // A file can change while the gate is answering, and this is the last moment the
            // difference is actionable: a here-document body cannot change, being in the
            // command text, but a file on disk can, and `admit` may have waited minutes.
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
                // The operator's own run, and only it: `run`'s flag is the whole distinction
                // between the two entries. See `InvokeCtx::tty`.
                tty: !gated,
                gate: gated.then_some(&self.gate),
                plan_open: self
                    .registry
                    .get("todo_write")
                    .and_then(|t| t.open_plan())
                    .unwrap_or(false),
                one_shot: self.one_shot,
                provenance: self.provenance.as_deref(),
            };
            tool.invoke(&mut ctx, &args)
        };

        // A path the boundary hid is a question, not a dead end: without this the model's only
        // move was to hand the operator shell commands to run themselves. An approval re-probes
        // the boundary, so the next call sees the path; this one is not retried.
        let mut invocation = invocation;
        // One line on the result saying what the row is: the model called `bash` and the
        // transcript says `write`, and a difference it cannot account for is one it reads as a
        // harness bug and works around.
        if let Some(r) = &patched {
            invocation = invocation.with_note(r.note.clone());
        }
        if let Some(r) = &rewritten {
            invocation = invocation.with_note(r.note.clone());
        } else if gated && schema.name == "bash" && self.registry.get("write").is_some() {
            // The near miss, which is the common case: still a file written through a heredoc,
            // still no diff and no record. Only for a seat that has those tools, since advice to
            // call one the seat does not hold cannot be taken.
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
                // An operator's own call raises no view-grant card: its whole ruling is that
                // nobody is left to ask, so a card asking them to grant a path for a command
                // they just typed is the gate answering a question nobody asked. `bash` has
                // already put the same finding in its notes.
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

        // Clause 5, on every payload and not only the ones somebody remembered. The budget
        // bounds the tool's output alone: the envelope and `[note]` lines are the harness's own
        // bytes, so counting them would charge the model for its arguments' repairs.
        let produced = invocation.payload.len();
        let (payload, spill) = self.spiller.apply(
            invocation.payload,
            &SpillContext {
                tool: &schema.name,
                args: &args,
                bytes: produced,
            },
        );

        // A repair is said to the model once per session per kind, so a habit it keeps is not
        // repeated on every call. `repairs` still carries every one, for the head and the
        // metrics.
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
            // Never spilled and never truncated: this is the head's copy, not the model's, and
            // clause 5 bounds what goes into the prompt. A head handed half a file could not
            // draw a diff at all.
            edit: invocation.edit,
            // Carried through untouched, and deliberately not spilled: an image is bounded by
            // its own format and the server's own limit, and a truncated base64 is not a smaller
            // picture but a corrupt one. The same rule `edit` follows, for the opposite reason.
            media: invocation.media,
        };
        sink.emit(finished_event(turn_id, &result));
        result
    }

    /// The transcript row for a finished call. The payload is the rendered result, envelope
    /// included, because that is the byte sequence the next prompt replays. The row also
    /// carries the bounded edit excerpt, so a resumed head can still draw the panels.
    pub fn transcript_item(result: &ToolResult) -> TranscriptItem {
        TranscriptItem::ToolResult {
            call_id: result.call_id.clone(),
            name: result.name.clone(),
            outcome: result.outcome.clone(),
            payload: result.render(),
            edit: bounded_edit(result),
            origin: None,
            // Carried whole, unlike `bounded_edit`: an edit excerpt is bounded because a head
            // only draws it, while an image is sent, and a truncated `data:` URI is refused or
            // rendered as garbage. A second bound would discard what the far end would take.
            media: result.media.clone(),
        }
    }
}

/// The excerpt both the [`ToolEvent::Finished`] and the transcript row carry: three lines of
/// context either side of the change, four hundred lines the cap, so the fan-out cost is known
/// here and not a property of whatever file the model chose. One bound, two carriers.
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

/// The nearest known name, by a cheap edit distance. Used only to suggest.
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

    /// A declared skill's `allowed-tools` narrows the seat and cannot widen it: the tools it
    /// does not name leave the registry, so the prompt does not announce them.
    #[test]
    fn only_tools_narrows_to_the_declared_names() {
        let mut reg = Registry::new();
        reg.register(Box::new(Probe {
            access: Access::Read,
        }))
        .unwrap();
        reg.register(Box::new(crate::builtins::bash::Bash::default()))
            .unwrap();
        let allowed: std::collections::BTreeSet<String> =
            ["probe".to_string()].into_iter().collect();
        let narrowed = reg.only_tools(&allowed);
        let names: Vec<String> = narrowed.schemas().iter().map(|s| s.name.clone()).collect();
        assert_eq!(names, vec!["probe".to_string()]);
    }

    /// A tool that runs a command line, so the changed-file check is exercised through the
    /// same path a `bash` call takes without needing a process host.
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

    /// A gate that answers by changing the file it was asked about: the window the check
    /// exists for, reproduced in one process rather than described in a comment.
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

    /// A file the gate judged that changed before the command ran is refused, not run.
    ///
    /// A here-document body cannot have this window, being in the command text; a path can, and
    /// the window is the operator's own thinking time.
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
        let rt = ToolRuntime::new(reg, Box::new(backend)).with_gate(Box::new(RewritingGate {
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

    /// And the ordinary case is untouched: a file that did not change runs. Without this, the
    /// test above would pass on a runtime that refused everything.
    #[test]
    fn a_script_that_did_not_change_is_not_refused() {
        let d = crate::backend::tempdir::TempDir::new();
        std::fs::write(d.path().join("deploy.py"), "print('judged')\n").unwrap();

        let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut reg = Registry::new();
        reg.register(Box::new(CommandProbe { ran: ran.clone() }))
            .unwrap();
        let backend = crate::backend::HostBackend::new(d.path()).unwrap();
        let rt = ToolRuntime::new(reg, Box::new(backend)).with_gate(Box::new(AdmitGate));
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
        // The temp dir outlives the backend only within one test; leak it there rather than
        // complicate every caller.
        std::mem::forget(d);
        ToolRuntime::new(reg, Box::new(backend)).with_gate(Box::new(ExplodingGate))
    }

    /// An envelope repair is said to the model once per session, and still counted.
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

    /// Every kind, not only the envelope's: a renamed key here is said once per session too,
    /// and still counted.
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
        // Clause 4, as a control-flow fact rather than a promise: the gate panics if it is
        // consulted, and this call must not consult it.
        let rt = runtime(Access::Read);
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
        let rt = ToolRuntime::new(reg, Box::new(backend));
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
        let rt = runtime(Access::Read);
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

    /// The `coder` seat names the whole delegation family.
    ///
    /// The tools are listed on the seat rather than stripped at the depth limit, because the cap
    /// is enforced where the call is made: a child could otherwise start work it cannot collect
    /// or correct.
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
        // The pair is seated, not pushed: a role over its ceiling is refused at resolve, so an
        // overflow here would be a prompt the build cannot seat at all.
        assert!(
            seat.tools.len() <= seat.max_tools,
            "the seat is over its own ceiling: {} > {}",
            seat.tools.len(),
            seat.max_tools
        );
    }

    /// The gatekeeper seat is read-only by construction: it may read, grep, glob and run `bash`
    /// for `git diff`, and has no `write`, no `edit`, no `task`. A reviewer that can author code
    /// becomes a second author and then nobody reviewed.
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
        // And the one tool that is not a read: the door its verdict goes through. A seat without
        // it is a reviewer that can judge and cannot say so.
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

    /// The notes keeper seat: the read tools and `notes`, and nothing that authors code.
    ///
    /// `notes` is the one writer, and it is how a repair keeps authorship: it takes a bare name,
    /// so no spelling of a write leaves the notes directory.
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
        // and repairs memory. `bash` is not here even behind a flag.
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

    /// One plan tool per seat: a role naming both `todo` and `todo_write` resolves to
    /// `todo_write` alone.
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

    /// The room is on the two seats a person's own session runs on, and on nothing else.
    ///
    /// A seat naming `room` while the resolved seat does not is an arbiter told to answer with
    /// a tool it has not got, and a raised item that freezes for ever. The seats that must not
    /// carry it are the ones a subagent or hidden child runs on.
    #[test]
    fn the_room_is_seated_on_the_seats_a_top_level_session_actually_runs_on() {
        for seat in [roles::m1_orchestrator(), roles::leticode()] {
            assert!(
                seat.tools.iter().any(|t| t == "room"),
                "`{}` is a seat a top-level session runs on, so it must carry `room`: {:?}",
                seat.name,
                seat.tools
            );
        }
        for seat in [
            roles::m2_coder(),
            roles::m2_runner(),
            roles::gatekeeper(),
            roles::notes_keeper(),
        ] {
            assert!(
                !seat.tools.iter().any(|t| t == "room"),
                "`{}` is a subagent's or a hidden child's seat, and the room is top-level only: \
                 {:?}",
                seat.name,
                seat.tools
            );
        }
        // The seat the coordinator resolves is `m1_orchestrator`, not the table above it. What
        // this crate can pin is that the two are not the same list, which is the fact the
        // defect turned on.
        assert_ne!(
            roles::m1_orchestrator().tools,
            roles::orchestrator().tools,
            "the live orchestrator seat and §8.4's table are different lists; if they ever \
             collapse into one, `harnessd`'s `base_role_for_seat` has to be re-read before \
             anybody believes a tool added here is seated"
        );
    }

    /// `task_start` is seated wherever `task` is.
    ///
    /// Placement lives in the tool, not in the caller's memory: a seat that can start a child
    /// but not arrange its placement spawns it into the main tree and then kills it for being in
    /// the wrong place, which is the two mistakes the tool exists to prevent.
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

    /// The leticode seat can ask the person a question, not only tell them.
    ///
    /// The tool lives in the `intent` bundle, which is the planner's, so without this seat a
    /// leticode session has no such verb. The resolve itself is proven where the real registry
    /// is built, since the seat names tools this crate does not register.
    #[test]
    fn the_leticode_seat_carries_ask_user_question() {
        let seat = roles::leticode();
        assert!(
            seat.tools.iter().any(|t| t == "ask_user_question"),
            "the leticode seat does not name `ask_user_question`: {:?}",
            seat.tools
        );
        // It took its own seat rather than trading for one: nothing left the list when it
        // arrived, which is the property `max_tools` is declared for.
        assert_eq!(seat.tools.len(), 37, "{:?}", seat.tools);
        assert!(
            seat.tools.len() <= seat.max_tools,
            "the seat is over its own ceiling: {} > {}",
            seat.tools.len(),
            seat.max_tools
        );
        // And the declaration `intent/mod.rs` asserts for it: it changes nothing the operator
        // owns and reads nothing off disk; it interrupts a person.
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
        let rt = runtime(Access::Read);
        let mut sink = RecordingToolSink::new();
        rt.invoke("t1", &call("probe", r#"{"path":"a"}"#), &mut sink);
        assert_eq!(sink.kinds(), vec!["ToolStarted", "ToolFinished"]);
    }

    #[test]
    fn the_row_and_the_event_carry_the_same_bounded_excerpt() {
        // One bound, two carriers: the event reaches the heads watching live and the transcript
        // row reaches every head after a restart, so a row whose panels disagreed with the card
        // the operator watched draw would be a defect.
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

    /// A path in the secret store is not opened to find out what it contains.
    ///
    /// The reader exists so the adjudicator can judge the program rather than the filename, and
    /// that is exactly what would put a private key in the brief and the transcript. The closure
    /// counts its own calls, which is the only way to assert not opened rather than not shown.
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

        // An ordinary path is read, so the guard is not a blanket refusal: the assertion above
        // would pass on a reader that never opened anything.
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

    /// The reader and the classifier agree about which file is the program: a wrapped
    /// interpreter is unwrapped on both sides, so `sudo python3 foo.py` has its file read by one
    /// half and judged by the other rather than by neither.
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

    /// An inline program names no file, so nothing is read and nothing is claimed; a command
    /// that names no script at all stays empty.
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

    /// A `bash` stand-in that records that it ran, so a test can tell a converted call, which
    /// must never reach the shell, from one left alone.
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

    /// What the gate was shown, which is the half of the rewrite that is not the file on disk:
    /// a `write` to a path, adjudicated as one.
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

    /// A writable temp tree with a fake `bash`, optionally the real `write`, and a gate that
    /// says what it was asked about. Returns the directory too, so a test can read what landed.
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

    /// The operator's own command, through the runtime: by the time the gate sees it the call is
    /// a `write` with a path argument and `Write` access, the file lands with the heredoc's body,
    /// the result carries the diff, and the model is told once what happened to its call.
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

    /// The `cwd` argument is where the shell would have been, so a relative path is resolved
    /// against it.
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

    /// An append is the file that is there plus the body: `write` replaces whole contents, and a
    /// `>>` that replaced would be a deletion.
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

    /// A near miss is left to the shell and told what it cost: `mkdir -p ... &&` is a second
    /// command, so the call still runs and the result carries the line naming the tools that
    /// would have recorded it.
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

    /// A seat with no `write` keeps its shell: a rewrite there would turn a working command into
    /// "no tool called `write` is available here".
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

    /// The operator's own line is not rewritten: what a person typed is what runs.
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
    // `on_tool_call`: a plugin layer sees the call before the gate
    // -------------------------------------------------------------------------------------

    /// A gate that records what it was asked about and admits, so a test can read the arguments
    /// the gate adjudicated rather than trusting that it saw the right ones.
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

    /// A tool that says whether it ran: the difference between a call a layer refused and a call
    /// whose result a layer replaced, which no outcome alone can tell apart.
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

    /// A plugin layer answering from a fixed verdict, recording every call it was shown.
    struct FixedHook {
        answer: Option<letibot_plugin::tool_call::ToolCallOut>,
        seen: std::sync::Arc<std::sync::Mutex<Vec<letibot_plugin::Call>>>,
        /// What a host that knows the layer's name would put above the result. `None` is the
        /// layer's id with nothing said about it.
        note: Option<String>,
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
                note: self.note.clone(),
            })
        }
    }

    /// What `hooked` hands back: the runtime plus the registers its gate and hook record into.
    type Hooked = (
        ToolRuntime,
        std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
        std::sync::Arc<std::sync::Mutex<Vec<letibot_plugin::Call>>>,
        std::sync::Arc<std::sync::atomic::AtomicBool>,
        crate::backend::tempdir::TempDir,
    );

    /// A runtime over one `probe` tool, a recording gate, and a hook that answers `answer`.
    fn hooked(answer: Option<letibot_plugin::tool_call::ToolCallOut>) -> Hooked {
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
            note: None,
        }));
        (rt, seen, calls, ran, d)
    }

    /// The rewritten call is the call the gate adjudicates, which is the whole of the safety
    /// here: a layer can change what a call asks for, and what it asks for then meets the same
    /// gate. This is what keeps tighten-only true through a rewrite.
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

    /// A layer can refuse a call, and then nothing runs. The outcome says who refused and the
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

    /// A layer can answer a call without letting it run: the model is asking something the layer
    /// already knows.
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

    /// An answered call says whose words these are, and that the tool did not run.
    ///
    /// The payload is the layer's sentence and nothing else, so without this line a model reads
    /// an `answer` exactly as it reads a tool's output, and a `monitor` that never armed becomes
    /// a fact about the world nobody stated.
    #[test]
    fn an_answered_call_is_attributed_and_the_payload_is_otherwise_unchanged() {
        let d = crate::backend::tempdir::TempDir::new();
        let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut reg = Registry::new();
        reg.register(Box::new(RanProbe { ran: ran.clone() }))
            .unwrap();
        let rt = ToolRuntime::new(
            reg,
            Box::new(crate::backend::HostBackend::new(d.path()).unwrap()),
        )
        .with_gate(Box::new(AdmitGate))
        .with_call_hook(std::sync::Arc::new(FixedHook {
            answer: Some(letibot_plugin::tool_call::ToolCallOut::answer(
                "pr-24 is watched for its checks",
            )),
            seen: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            note: Some("answered by plugin pr-watch (project); the tool did not run".to_string()),
        }));
        let mut sink = RecordingToolSink::default();
        let r = rt.invoke("t1", &call("probe", r#"{"path":"/tmp/x"}"#), &mut sink);
        assert!(!ran.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(
            r.payload, "pr-24 is watched for its checks",
            "the payload is the layer's own words, unchanged"
        );
        let rendered = r.render();
        assert_eq!(
            r.notes,
            vec!["answered by plugin pr-watch (project); the tool did not run".to_string()]
        );
        assert!(
            rendered.contains("[note] answered by plugin pr-watch (project); the tool did not run"),
            "the model is not told whose words it is reading: {rendered}"
        );
        assert!(
            rendered.contains("pr-24 is watched for its checks"),
            "{rendered}"
        );
    }

    /// `pass`, and a layer that says nothing, both leave the call exactly as it was.
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

    /// The operator's own run is not shown to the layer: it is a person's own act, already
    /// adjudicated at the door, and a layer rewriting it would be policy over the person.
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

    /// A rewrite the tool would refuse is reported, not run: a layer's arguments go through the
    /// same `salvage` the model's do, so a layer cannot put a shape past the schema.
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
