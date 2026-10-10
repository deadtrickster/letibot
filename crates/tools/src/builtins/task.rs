//! `task` — spawn a subagent to do a subtask and return its result.
//!
//! The tool holds a [`TaskRunner`]; the harness installs the real one (a nested
//! turn with a restricted role) and the default refuses by name. Keeping the seam
//! here means the tool is registered and seatable before the subagent loop lands.
//!
//! # A subagent inherits, and may be downgraded — never upgraded
//!
//! A subagent runs under its parent's permission ruleset, and `access` narrows
//! it further: a survey needs no writes, a formatter needs no network. The
//! [`Downgrade`] is applied to the tools seated, the backend opened and the
//! ruleset, and it is the **union** of the parent's own downgrade and the one
//! asked for, so a downgraded session cannot spawn a wider child by naming a
//! wider role. The role picks which tools; the downgrade says which classes of
//! them are gone whatever the role.
//!
//! # Where it runs
//!
//! `where` names the substrate: `host` (the parent's own boundary, the
//! default) or `firecode` (a VM, firecracker or libvirt). Placement never widens
//! permissions either — a subagent in a VM is still the parent's ruleset, minus
//! the downgrade, inside a boundary. The firecode placement is a declared seam:
//! a build without that backend refuses by name rather than running on the host
//! and calling it a VM.

use std::sync::Arc;

use serde_json::json;

use crate::runtime::{Invocation, InvokeCtx, Tool};
pub use crate::schema::Downgrade;
use crate::schema::{Access, ToolSchema};

/// Where a subagent runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Placement {
    /// The parent's own boundary, whatever it is.
    #[default]
    Host,
    /// A firecode VM. Which hypervisor is the backend's business.
    Firecode,
}

/// The handle's `seam:` line for a placement: what the child's work is made of and where it
/// comes back. Empty for the host, whose work is the worktree itself.
fn placement_seam(placement: Placement) -> String {
    match placement {
        // **No branch on this path.** The VM works on a copy, and firecode delivers the copy's
        // final state as a directory beside it when the VM comes down — the old line promised a
        // branch, which a reader then went looking for (FIRECODE-NOTES).
        Placement::Firecode => {
            "  seam: the VM works on a COPY of the tree; its work comes back as \
             a directory beside that copy when the VM comes down, not as this branch — diff \
             it and take what you want.\n"
                .to_string()
        }
        _ => String::new(),
    }
}

/// **The `where` argument, refused when it cannot be honoured on this host.** A `firecode`
/// placement on a box with no `firecode` used to spawn a child that died at its start —
/// `placing this session in a firecode VM: io: running firecode: No such file or directory` —
/// and the parent learned it only from `task_result`, a round later. Now the call is refused
/// with nothing spawned and `host` named as the way to go on.
fn placement_arg(args: &serde_json::Value) -> Result<Placement, Invocation> {
    let placement = match args.get("where").and_then(|v| v.as_str()) {
        None => return Ok(Placement::Host),
        Some(w) => Placement::parse(w).map_err(|e| {
            Invocation::failed(
                "`where` was not understood",
                format!("{e}. Nothing was spawned."),
            )
        })?,
    };
    if placement == Placement::Firecode
        && let Err(why) = crate::firecode::firecode_available()
    {
        return Err(Invocation::failed(
            "this host cannot place a subagent in a VM",
            format!(
                "{why}, so `where: firecode` cannot be honoured. Nothing was spawned. Leave \
                 `where` out (or say `host`) to run the subagent inside your own boundary; \
                 for a VM, the operator puts `firecode` on the daemon's PATH or names it in \
                 $FIRECODE_BIN and restarts the daemon."
            ),
        ));
    }
    Ok(placement)
}

impl Placement {
    pub fn parse(s: &str) -> Result<Placement, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "host" | "here" => Ok(Placement::Host),
            "firecode" | "vm" | "firecracker" | "libvirt" => Ok(Placement::Firecode),
            other => Err(format!(
                "`{other}` is not a placement. There are two: `host` (the default) and \
                 `firecode` (a VM)"
            )),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Placement::Host => "host",
            Placement::Firecode => "firecode",
        }
    }
}

/// What one `task` call asked for, beyond the prompt.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskSpec {
    pub role: String,
    pub downgrade: Downgrade,
    pub placement: Placement,
    /// **Which model the child runs on** — the operator's ask, 2026-10-05: *"I want to be
    /// able to have subagents using different models. say you deepseek should be able to
    /// run local model"*, and the other direction too: *"local qwen in main session
    /// should be able to run cloud glm"*.
    ///
    /// `None` inherits the parent's model, which is what every earlier build did and stays
    /// the default. `Some("local")` is the daemon's own server; `Some("provider/model")` a
    /// cloud preset, resolved by the daemon through the same door `/models` uses — so a
    /// name a person can type is a name a child can run on, and a key the picker finds is
    /// a key the spawn finds.
    pub model: Option<String>,
    /// **Where the child's files live**, when `task_start` arranged a tree for it.
    ///
    /// `None` is a plain `task` call, which works in the parent's own workspace — the
    /// behaviour every earlier build had, and the one `task` keeps. `Some` is a
    /// `task_start` call, and the child's workspace is the worktree the runner created
    /// before it spawned the child: the placement is a fact the spawn carries, not a
    /// thing the caller remembers to say. See [`WorktreePlacement`] for why the field
    /// rides the spec rather than a second argument.
    pub worktree: Option<WorktreePlacement>,
}

/// **What one `task_start` call asked for**, beyond the prompt and the [`TaskSpec`].
///
/// The request, before the runner has done anything with it: the slug the worktree and
/// branch are named by, the base the branch is cut from, and whether the exception was
/// asked for (the main checkout rather than a fresh worktree). The runner answers it
/// with a [`WorktreePlacement`] — the path, branch and base SHA that actually exist —
/// and the two are different shapes on purpose: the request is what the caller typed,
/// the placement is what the filesystem now is, and a tool that reported the request as
/// the placement would be the defect this tree keeps finding by looking for it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorktreeSpec {
    /// The slug the worktree and branch are named by. Empty is *derive it from the
    /// prompt* — the default, and the one [`slug_from_prompt`] answers.
    pub slug: String,
    /// The base ref the branch is cut from. `None` is the repo's current HEAD, which is
    /// the default and the one a caller that did not look at the branch gets.
    pub base: Option<String>,
    /// **The exception**: work in the main checkout rather than a fresh worktree.
    /// `false` is the default — a `task_start` that did not name the main tree creates
    /// a worktree, and the main tree is the thing it exists to keep the child out of.
    pub main_tree: bool,
    /// **The repository to cut the worktree from**, when it is not the one the session root is
    /// in — a session may run a level above its repositories (`~/Projects`), and then the root
    /// is in no repository at all. Relative to the session root, or absolute. `None` is the
    /// session root's own repository, which is what every earlier build used.
    pub repo: Option<String>,
}

/// **Where a `task_start` child works, as the runner arranged it.**
///
/// The answer to a [`WorktreeSpec`]: the path the child's files live in, the branch
/// they are on, and the base SHA the branch was cut from. It rides the [`TaskSpec`]
/// (as its `worktree` field) rather than a second argument to [`TaskRunner::start`]
/// because the spawn is the one place that turns a spec into a session, and a session
/// whose workspace is not the worktree the tool just reported is the same silent
/// provenance defect as a model name that does not match — the child would be working
/// in the parent's tree while the answer said it was in its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreePlacement {
    /// The path the child works in. A worktree path for the default, the main
    /// checkout's path for the exception.
    pub path: String,
    /// The branch the child works on: `agent/<slug>` for a worktree, the main tree's
    /// own branch for the exception.
    pub branch: String,
    /// The base SHA the branch was cut from. The main tree's HEAD for the exception.
    pub base_sha: String,
    /// Whether the child works in the main checkout — the exception, named in the
    /// answer so the operator sees it rather than finds it.
    pub main_tree: bool,
}

/// **The handle a `task_start` call returns, with the placement it arranged.**
///
/// A plain `task` returns a bare handle, because the child works where the parent
/// works and there is nothing else to say. A `task_start` returns this: the handle
/// `task_result` collects by, plus the path, branch and base SHA the child was put in,
/// so the caller and the operator see where the child is working without re-reading
/// the brief. The placement is the point of the tool — a handle that does not say
/// where the child is is a handle the operator has to go and find.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeHandle {
    /// The handle `task_result` collects by.
    pub handle: String,
    /// Where the child works, as the runner arranged it.
    pub placement: WorktreePlacement,
}

/// **The slug a `task_start` child's worktree and branch are named by**, derived
/// from the prompt when the caller did not name one.
///
/// Short, kebab, and readable: the first few words of the prompt, lowercased, with
/// everything that is not a letter or a digit collapsed to a single hyphen, capped
/// at a length that keeps the path and the branch legible in a `git worktree list`.
///
/// **Deterministic in the prompt, on purpose.** Two different subtasks get two
/// different slugs, and the same subtask asked twice gets the same slug — which is
/// what makes the refusal of an existing path meaningful: a second `task_start` for
/// the same work finds the first one's worktree and says so by name, rather than
/// silently creating a second tree for work that already has one. A caller that
/// wants a different name for the same work names it with the `slug` argument; the
/// derivation is the default, not a lock.
///
/// Pure, so it is testable without a repo: the slug is a fact about the prompt, and
/// the filesystem work that uses it lives in the runner.
pub fn slug_from_prompt(prompt: &str) -> String {
    const MAX: usize = 40;
    const WORDS: usize = 4;
    let mut slug = String::new();
    for word in prompt.split_whitespace().take(WORDS) {
        for c in word.chars() {
            if c.is_ascii_alphanumeric() {
                slug.push(c.to_ascii_lowercase());
            } else if !slug.is_empty() && !slug.ends_with('-') {
                slug.push('-');
            }
        }
        if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.chars().count() > MAX {
        slug = slug.chars().take(MAX).collect::<String>();
        while slug.ends_with('-') {
            slug.pop();
        }
    }
    if slug.is_empty() {
        "task".to_string()
    } else {
        slug
    }
}

/// **The path a `task_start` worktree is created at**, from the repo's workspace and
/// the slug.
///
/// `<workspace>/.claude/worktrees/agent-<slug>` — the same shape the operator's own
/// worktrees take, so a `git worktree list` in the main tree shows the child's tree
/// beside the operator's, and the child is a fact on disk rather than a handle the
/// operator has to go and find. Pure, so the tool and the runner compute the same
/// path from the same inputs: a tool that reported a path the runner did not create
/// would be the same claim-versus-fact defect the rest of this tree refuses.
pub fn worktree_path(workspace: &str, slug: &str) -> String {
    format!("{workspace}/.claude/worktrees/agent-{slug}")
}

/// **The branch a `task_start` worktree is cut onto**, from the slug.
///
/// `agent/<slug>` — the branch is the deliverable, and its name says what it is:
/// an agent's work, cut from the base the caller named (or the repo's HEAD), to be
/// landed by a merge queue rather than pushed. See [`TaskStartTool`] for the rule
/// that it is never pushed.
pub fn worktree_branch(slug: &str) -> String {
    format!("agent/{slug}")
}

/// Where a spawned subagent has got to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskStatus {
    /// Still working. `note` is the child's own last word about what it is doing,
    /// when it has said one.
    Running { note: Option<String> },
    /// Finished, with its final answer.
    Done { answer: String },
    /// It did not finish, and this is why.
    Failed { why: String },
    /// No subagent by that name was ever spawned here.
    Unknown,
}

/// **What a finished `task_start` child's branch became** — the answer to
/// [`TaskRunner::finished`], in the tool layer's own vocabulary.
///
/// The tool layer does not know what a merge queue is, and that is the point of this enum: a
/// `task_start` child leaves a BRANCH behind, the branch is the deliverable, and where it goes
/// is the runner's business. What the tool needs is the one sentence a person reads — *it is
/// in the queue*, *it was already there*, *there is no branch to land* — and a refusal when
/// the enqueue could not be done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finished {
    /// The child's branch was put in the queue for landing: this is the entry, or — when the
    /// finish was noticed twice, which is ordinary — the same entry it already had. The two
    /// are one value because they are one FACT: *this branch is in the queue*. A separate
    /// `Already` variant would make a caller branch on a difference nobody can act on, and the
    /// id is carried so a reader can find the row.
    Queued {
        /// The entry's id, which is the child's own handle.
        id: String,
        /// The branch the entry is about.
        branch: String,
    },
    /// There is no branch to land, and this is why — a plain `task` child works in the
    /// parent's own tree, and a `task_start` in the main checkout works on main itself.
    NoBranch { why: String },
}

/// Starts subagents and collects them. **Starting is not running.**
///
/// The trait used to have one method, `run`, which spawned a child and blocked
/// until it answered. The daemon runs a round's calls in order on one thread, so
/// a `task` held every call behind it for as long as the child lived: measured
/// 2026-09-17, a block of `todo_write`, `task` and three `web_search` calls sat
/// unfinished for the fifteen minutes the subagent ran, and the operator — and
/// the model, in its own reasoning — read it as a hang. The operator's reading of
/// the shape: *"task by definition must be background task no? because non
/// background is what? a simple bash?"* Right. A subagent is minutes of work by
/// construction, so the call that starts one returns a handle, the same way
/// `bash --background` does, and the answer is collected by its own call.
pub trait TaskRunner: Send + Sync {
    /// Start the subtask and return the handle it can be collected by. Returns
    /// when the child is **spawned**, not when it is finished.
    fn start(&self, prompt: &str, spec: &TaskSpec) -> Result<String, String>;

    /// **Arrange the tree, then start the child** — the `task_start` door.
    ///
    /// The difference from [`Self::start`] is the order: the worktree is created
    /// *before* the child is spawned, and the refusal (a path that already exists
    /// or is already a worktree) is answered by this call rather than by a child
    /// that was never started. The operator's ask, in their words: *"we will need
    /// a new tool - task_start or what that will arrange worktree, firecode and
    /// subagent"* — the placement is arranged by the tool, not remembered by the
    /// caller, because the two mistakes this exists to prevent are a child spawned
    /// into the main tree and a child killed instead of corrected for being in the
    /// wrong place.
    ///
    /// Returns the handle **and** the placement the child was put in — the path,
    /// branch and base SHA — so the answer can say where the child is working
    /// without the operator re-reading the brief. A runner that cannot arrange a
    /// tree refuses by name rather than spawning a child in the parent's workspace
    /// and calling it placed: the same claim-versus-fact rule [`Self::kill`] and
    /// [`Self::send`] follow.
    fn start_worktree(
        &self,
        _prompt: &str,
        _spec: &TaskSpec,
        _worktree: &WorktreeSpec,
    ) -> Result<WorktreeHandle, String> {
        Err(
            "this session's runner cannot arrange a worktree: a `task_start` needs the \
             runner that owns the tree to create the worktree before it spawns the child, \
             and this one does not. Nothing was arranged and nothing was spawned."
                .into(),
        )
    }

    /// Where a spawned subagent has got to, waiting up to `timeout` for it to
    /// finish. A zero timeout is a poll.
    fn collect(&self, handle: &str, timeout: std::time::Duration) -> TaskStatus;

    /// The handles this session has started, oldest first — what `task_result`
    /// lists when it is called with no handle, so a model that lost one can find
    /// it without the operator.
    fn started(&self) -> Vec<String> {
        Vec::new()
    }

    /// **A `task_start` child has finished: its branch goes to be landed** — the seam the
    /// merge queue's enqueue hangs on.
    ///
    /// The operator's ask, in their words: *"we need a gated merge to main, and worktree
    /// cleanup"*, and *"a permanent subagent that lazily starts as soon as task_start was
    /// finished and item put into the queue"*. A `task_start` child works on a branch that is
    /// **never pushed** — the tool says so in as many words — so the branch is a deliverable
    /// with nobody to carry it until this call exists. `task_start`'s own docstring used to
    /// carry the `TODO(merge-queue)` that stood here; this is that door.
    ///
    /// **Why the runner and not the tool.** The enqueue needs the brief the child was given,
    /// the placement it worked in and the store the queue lives in, and the tool layer has
    /// none of the three: it has no git, no store and no session. The runner arranged the
    /// tree, so the runner is the one that knows what to hand over — and the tool is left
    /// with the one thing it can honestly say, which is the sentence a person reads.
    ///
    /// **`NoBranch` is the ordinary answer for a plain `task`**, and it is not a failure: a
    /// `task` child works in the parent's own workspace, so there is no branch of its own to
    /// land. A runner that arranges nothing answers that rather than refusing, because a
    /// refusal would put a line about the merge queue under every `task_result` a model ever
    /// reads.
    ///
    /// **`Err` is a real failure and it is said rather than swallowed**, by the rule
    /// [`Self::kill`] and [`Self::send`] follow: a branch reported as queued that is not in
    /// the queue is worse than one that says it could not be, because the parent then
    /// believes the work is on its way to main.
    fn finished(&self, handle: &str) -> Result<Finished, String> {
        let _ = handle;
        Ok(Finished::NoBranch {
            why: "this session's runner arranges no worktree, so a child of it leaves no \
                  branch to land"
                .into(),
        })
    }

    /// **Stop a subagent.** `Ok` is what was done, in the words the operator gets;
    /// `Err` is why it could not be, said rather than swallowed.
    ///
    /// The operator's ruling, when `job_kill` was found to reach only the host's process
    /// table: *"expand"*. A subagent is not a process, so it cannot be reaped the way a
    /// job is — and it is still the same handle-shaped thing a model reaches for a stop
    /// with, which is the rest of the same ruling: *"in a way agent is a background job."*
    ///
    /// **The kill is an interrupt of the child's turn**, because that turn is the only
    /// thing a subagent is doing: it does not own a process, a cgroup or a scope, and the
    /// one door that reaches a turn already running is the session's own steering — the
    /// same door a head's Esc-esc uses. A runner that cannot do that says so rather than
    /// returning a kill it did not perform.
    fn kill(&self, handle: &str) -> Result<String, String> {
        Err(format!(
            "this session's runner cannot stop `{handle}`: a subagent is stopped by \
             interrupting the turn it is running, which only the runner that started it \
             can do. Nothing was stopped."
        ))
    }
    /// **Say something to a session that is still working** — the operator's ruling,
    /// 2026-10-06: *"in the tree all subagents must be addressable by their parents. that is
    /// how live corrections delivered."*
    ///
    /// `Ok` is what was done, in the words the operator gets; `Err` is why it could not be,
    /// said rather than swallowed — the rule [`TaskRunner::kill`] follows, and for the same
    /// reason: a message reported as delivered that nobody heard is worse than one refused,
    /// because the sender then believes the session was corrected.
    ///
    /// **A message is not a second prompt.** It is recorded as an agent's utterance rather
    /// than the operator's, so it can neither authorise the act it races nor be read as
    /// something a person typed. **Both states of a session are reachable, and they are
    /// different deliveries**: one with a turn RUNNING hears it at its next round boundary,
    /// and one BETWEEN turns has a turn started for it with this message as what the turn is
    /// about (the operator's ruling, *"fix task_message - it should enqueue"*). The one thing
    /// a runner must not do is accept a message nothing will read: a handle whose session is
    /// gone, or whose thread has ended, is refused by name.
    ///
    /// **The handle may name a peer, and that is not a widening of this door's job.** The
    /// operator struck the parent-shaped framing of it — *"well gatekeeper is not a child so we
    /// are not doing up"* — so one session sending another session a message is the ordinary
    /// case, and the refusal is for a session that does not exist rather than for one that is
    /// not this session's child.
    fn send(&self, handle: &str, _text: &str) -> Result<String, String> {
        Err(format!(
            "this session's runner cannot message `{handle}`: a subagent is reached through \
             the turn it is running, which only the runner that started it can do. Nothing \
             was sent."
        ))
    }

    /// **Stop every child this session still owns.**
    ///
    /// The operator's design, in their words: *"think about it like it is an erlang
    /// supervision tree. we talk to parents and they own lifecycle."* Two of the tree's edges
    /// are already here — `start` makes a child, and `collect`/`send`/`kill` each talk to one
    /// — and this is the third: **a parent that is being stopped stops its children first**,
    /// so the tree never leaves work computing for nobody. That shape was measured the same
    /// night: a stalled grandchild survived the parent that owned it, because the interrupt
    /// reached exactly one session.
    ///
    /// **The children, not the subtree.** A runner stops what IT started and answers for
    /// those; each child does the same when the stop reaches it, so the walk down is one
    /// level per session and no runner needs to know its grandchildren. That is the whole of
    /// what *"we talk to parents"* means in code, and the reason this is not a `kill` over a
    /// list of handles nobody holds.
    ///
    /// One result per child, in the words [`TaskRunner::kill`] gives, so a caller can say
    /// what was stopped and what refused rather than a count that hides a refusal — the rule
    /// this trait already holds to. A runner that starts nothing returns nothing, which is
    /// the honest answer for a session with no children rather than a failure.
    ///
    /// **Not the restart policy.** Nothing here decides whether a child should be started
    /// again; a parent's decision about what its child's exit MEANS is the second half of the
    /// operator's design and is not built. See the TODO beside `HarnessTaskRunner`'s own
    /// implementation.
    fn stop_all(&self) -> Vec<(String, Result<String, String>)> {
        Vec::new()
    }
}

/// The default: no runner, and it says so rather than pretending to have run.
pub struct NoTaskRunner;

impl TaskRunner for NoTaskRunner {
    fn start(&self, _prompt: &str, _spec: &TaskSpec) -> Result<String, String> {
        Err("no subagent runner is installed in this session".into())
    }

    fn collect(&self, _handle: &str, _timeout: std::time::Duration) -> TaskStatus {
        TaskStatus::Unknown
    }
}

pub struct TaskTool {
    runner: Arc<dyn TaskRunner>,
}

impl TaskTool {
    pub fn new(runner: Arc<dyn TaskRunner>) -> Self {
        TaskTool { runner }
    }
}

impl Tool for TaskTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "task",
            "Start a subtask in a subagent. Give `prompt` (the subtask); optionally \
             `role` to pick the subagent's toolset (defaults to coder), `access` to \
             narrow it BELOW your own permissions (`read-only` for a survey that must \
             not write; or `no-write`, `no-exec`, `no-network`), `where` (`host`, \
             the default, or `firecode` for a VM), and `model` to run the child on a \
             different model than yours (`local`, or `PROVIDER/MODEL` — an unknown \
             name or a missing key is refused at the spawn, naming the fix). A \
             subagent inherits your \
             permissions and can only be given less, never more. This returns as soon \
             as the subagent has STARTED, with a handle — a subagent is minutes of \
             work and this call does not wait for it, so the rest of your calls run \
             while it does. Collect its answer with `task_result`. **A child that \
             writes code must be told, in its prompt, to END with \
             `sh scripts/check-fmt.sh <the ref it was cut from>`**: code a child has \
             written, built and TESTED can still be unformatted, because `cargo fmt` \
             is in no brief and CI's `Format (the files this change touches)` job is \
             the only thing that reads it. The same one line goes for `cargo test \
             --workspace --no-run` when the child changes a type's SHAPE — one change \
             reaches callers in crates the child never ran.",
            json!({
                "type": "object",
                "properties": {
                    "prompt": {"type": "string", "description": "The subtask for the subagent."},
                    "role": {"type": "string", "description": "The subagent's role. Defaults to coder."},
                    "access": {"type": "string", "description": "Narrow the subagent below your own permissions: `read-only` (no write, exec or network), or any of `no-write`, `no-exec`, `no-network`, comma-separated. Omit to inherit yours unchanged. Cannot widen."},
                    "where": {"type": "string", "description": "`host` (the default: your own boundary) or `firecode` (a VM). Placement never changes permissions."}
                },
                "required": ["prompt"]
            }),
            // `Session`, not `Exec`: `task` runs no host command itself — it delegates
            // to a child turn whose own gate governs its write/exec/network calls. As
            // `Exec` it would hit the operator's rule that exec always asks (a subagent
            // spawn is not a shell, and opencode does not gate it), and a subagent that
            // could not even be spawned without a head would never run.
            Access::Session,
        )
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &serde_json::Value) -> Invocation {
        let Some(prompt) = args.get("prompt").and_then(|v| v.as_str()) else {
            return Invocation::failed(
                "task needs a prompt",
                "call `task` with `prompt` set to the subtask.",
            );
        };
        let role = args.get("role").and_then(|v| v.as_str()).unwrap_or("coder");
        let downgrade = match args.get("access").and_then(|v| v.as_str()) {
            None => Downgrade::none(),
            Some(a) => match Downgrade::parse(a) {
                Ok(d) => d,
                Err(e) => {
                    return Invocation::failed(
                        "`access` was not understood",
                        format!("{e}. Nothing was spawned."),
                    );
                }
            },
        };
        let placement = match placement_arg(args) {
            Ok(p) => p,
            Err(refused) => return refused,
        };
        let spec = TaskSpec {
            role: role.to_string(),
            downgrade,
            placement,
            model: args
                .get("model")
                .and_then(|v| v.as_str())
                .map(|m| m.trim().to_string())
                .filter(|m| !m.is_empty()),
            // A plain `task` works in the parent's workspace: no worktree is arranged.
            worktree: None,
        };
        let handle = match self.runner.start(prompt, &spec) {
            Ok(h) => h,
            Err(e) => return Invocation::failed(e, "the subagent did not start."),
        };
        ctx.progress(&format!("subagent {handle} started"));
        // **`Backgrounded`, not `Ok`** — the same reason `bash --background`
        // gives: the outcome names a fact about the world, *this is running and
        // has not answered yet*, and an `Ok` here would be grounding for an
        // answer nothing has produced.
        Invocation::backgrounded(
            handle.clone(),
            std::time::Duration::ZERO,
            letibot_transcript::Backgrounding::Asked,
            format!(
                "carry on — `{handle}`'s answer is delivered to you on its own when it \
                 finishes, so there is nothing to wait for. `task_result` with \
                 task=\"{handle}\" and no `timeout_ms` shows where it has got to."
            ),
            format!(
                "started subagent `{handle}` as `{}`.\n  subtask: {}\n\nIt is working \
                 now, and this call did not wait for it — the rest of this round runs \
                 while it does, and its answer comes back to you as a turn of its own \
                 when it finishes. `task_result` with task=\"{handle}\" (no \
                 `timeout_ms`) reports where it has got to.",
                match &spec.model {
                    // **The model is named when the child was given one** — a spawn that
                    // prints only the role would leave the reader to guess which model is
                    // answering, and the whole point of the argument is that they chose.
                    Some(m) => format!("{m} (as a {})", spec.role),
                    None => spec.role.clone(),
                },
                first_line(prompt),
            ),
        )
    }
}

/// The subtask's first line, for a one-line echo. The whole prompt back would be
/// the model's own bytes returned to it as a result.
fn first_line(prompt: &str) -> String {
    let l = prompt
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    if l.chars().count() <= 100 {
        return l.to_string();
    }
    format!("{}…", l.chars().take(100).collect::<String>())
}

/// `task_start` — arrange a subagent's placement *before* it spawns it.
///
/// The operator's ask, in their words: *"we will need a new tool - task_start or
/// what that will arrange worktree, firecode and subagent"*. The two mistakes of a
/// previous session are its spec: a child was spawned into the main tree, and then
/// that child was killed instead of corrected. Placement must live in the tool, not
/// in the caller's memory — so this tool creates the worktree first, refuses a path
/// that already exists by name and without spawning anything, and only then starts
/// the child in the tree it just made.
///
/// **The default is never the main tree.** A `task_start` that did not name the main
/// checkout creates a fresh worktree at `<workspace>/.claude/worktrees/agent-<slug>`
/// on branch `agent/<slug>`, cut from the base the caller named (or the repo's HEAD).
/// Working in the main checkout is the exception, and it requires the explicit
/// `main_tree` argument — the same shape as `where: firecode`, a declared seam rather
/// than a default.
///
/// **The build cache is shared deliberately.** The child's environment carries
/// `CARGO_TARGET_DIR` pointing at the main tree's `target/`, because cargo's own lock
/// serialises concurrent builds and a fresh worktree otherwise pays a cold build of
/// the whole graph. The runner is where that environment is set — the tool layer has
/// no git and no exec of its own.
///
/// **The branch is the deliverable, and it is never pushed.** The tool does not push,
/// and will not: the branch is to be landed by a merge queue.
///
/// **TODO(merge-queue): CLOSED — the branch is enqueued when the child finishes.**
///
/// This docstring used to carry the seam: *"the merge queue does not exist yet. The branch is
/// the deliverable and the landing is somebody else's job; this tool leaves the branch where it
/// is and says so."* The queue exists, and the door is [`TaskRunner::finished`]: when a
/// `task_start` child finishes, the runner mints the entry — the branch, the base SHA, the
/// priority, the `needs` and **the brief the child was given**, which is the one thing the
/// reviewer reads — and writes it to the session store the merge-queue daemon serves.
///
/// **The tool does not do it, and the split is deliberate.** The enqueue needs the brief, the
/// placement and the store, and this layer has no git, no store and no session; it has the
/// handle and the sentence. So the tool asks the runner when it sees the child finish
/// (`task_result` on a `Done`) and reports what the runner answered — and the runner ALSO
/// enqueues at the child's own settlement, because a branch whose answer is never collected is
/// still a branch that finished. One entry, one id (the handle), so the two notices are one
/// act rather than two.
///
/// **The branch is still never pushed by this tool**, and that has not changed: the queue
/// fast-forwards main and pushes, and the tool's rule is that the deliverable is a branch.
///
/// **A child in the wrong PLACE is corrected with `task_message`, not killed.**
/// Killing is for wrong work. A child that is in the wrong tree is a placement
/// mistake, and the remedy is to tell it where to be — the same live correction
/// `task_message` exists for — not to throw the work away.
pub struct TaskStartTool {
    runner: Arc<dyn TaskRunner>,
}

impl TaskStartTool {
    pub fn new(runner: Arc<dyn TaskRunner>) -> Self {
        TaskStartTool { runner }
    }
}

impl Tool for TaskStartTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "task_start",
            "Arrange a subagent's placement BEFORE it spawns it: create a git worktree \
             for the child, then start it in that worktree. Give `prompt` (the subtask); \
             optionally `role`, `access`, `where` and `model` exactly as `task` takes \
             them, plus `base` (the ref the branch is cut from, default the repo's \
             current HEAD), `slug` (the worktree and branch name, default derived from \
             the prompt), and `main_tree` (work in the main checkout instead of a fresh \
             worktree — the exception, off by default). By default the child works in \
             a fresh worktree at `<workspace>/.claude/worktrees/agent-<slug>` on branch \
             `agent/<slug>`, sharing the main tree's cargo build cache; a path that \
             already exists is refused by name and nothing is spawned. The branch is \
             the deliverable and is NEVER pushed — it is to be landed by a merge queue. \
             A child in the wrong PLACE is corrected with `task_message`, not killed: \
             killing is for wrong work. Collect its answer with `task_result`.",
            json!({
                "type": "object",
                "properties": {
                    "prompt": {"type": "string", "description": "The subtask for the subagent."},
                    "role": {"type": "string", "description": "The subagent's role. Defaults to coder."},
                    "access": {"type": "string", "description": "Narrow the subagent below your own permissions: `read-only` (no write, exec or network), or any of `no-write`, `no-exec`, `no-network`, comma-separated. Omit to inherit yours unchanged. Cannot widen."},
                    "where": {"type": "string", "description": "`host` (the default: your own boundary) or `firecode` (a VM). Placement never changes permissions."},
                    "model": {"type": "string", "description": "Run the child on a different model than yours (`local`, or `PROVIDER/MODEL` — an unknown name or a missing key is refused at the spawn, naming the fix)."},
                    "base": {"type": "string", "description": "The ref the branch is cut from. Defaults to the repo's current HEAD."},
                    "slug": {"type": "string", "description": "The slug the worktree and branch are named by. Defaults to a derivation from the prompt."},
                    "main_tree": {"type": "boolean", "description": "Work in the main checkout instead of a fresh worktree. The exception; off by default."},
                    "repo": {"type": "string", "description": "The repository to work in, when it is not the one the session root is in (a session above its repositories names one). Relative to the session root, or absolute."}
                },
                "required": ["prompt"]
            }),
            // `Session`, exactly as `task` is: this arranges a tree and delegates to a
            // child turn whose own gate governs its write/exec/network calls. The git
            // work happens in the runner, not here — the tool layer has no git and no
            // exec of its own.
            Access::Session,
        )
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &serde_json::Value) -> Invocation {
        let Some(prompt) = args.get("prompt").and_then(|v| v.as_str()) else {
            return Invocation::failed(
                "task_start needs a prompt",
                "call `task_start` with `prompt` set to the subtask.",
            );
        };
        let role = args.get("role").and_then(|v| v.as_str()).unwrap_or("coder");
        let downgrade = match args.get("access").and_then(|v| v.as_str()) {
            None => Downgrade::none(),
            Some(a) => match Downgrade::parse(a) {
                Ok(d) => d,
                Err(e) => {
                    return Invocation::failed(
                        "`access` was not understood",
                        format!("{e}. Nothing was arranged and nothing was spawned."),
                    );
                }
            },
        };
        // Checked before the worktree is arranged: a refusal here leaves nothing behind.
        let placement = match placement_arg(args) {
            Ok(p) => p,
            Err(refused) => return refused,
        };
        let model = args
            .get("model")
            .and_then(|v| v.as_str())
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty());
        let base = args
            .get("base")
            .and_then(|v| v.as_str())
            .map(|b| b.trim().to_string())
            .filter(|b| !b.is_empty());
        let slug_arg = args
            .get("slug")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let main_tree = args
            .get("main_tree")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let repo = args
            .get("repo")
            .and_then(|v| v.as_str())
            .map(|r| r.trim().trim_end_matches('/').to_string())
            .filter(|r| !r.is_empty() && r != ".");

        // **The slug, derived from the prompt when the caller did not name one.** The
        // derivation is the default, not a lock: a caller that wants a different name
        // for the same work names it with `slug`.
        let slug = slug_arg.unwrap_or_else(|| slug_from_prompt(prompt));

        // **The workspace, or the refusal that names its absence.** A `task_start`
        // with no workspace to arrange a worktree in is a call that cannot be done,
        // and it says so rather than guessing at a path.
        let Some(workspace) = ctx.backend.workspace_path() else {
            return Invocation::failed(
                "task_start needs a workspace",
                "this session has no workspace to arrange a worktree in. Nothing was \
                 arranged and nothing was spawned.",
            );
        };

        // **The refusal of an existing path, by name and without spawning anything.**
        // The default is a fresh worktree, and a path that already exists — a
        // directory, or a worktree a previous `task_start` made — is refused here
        // rather than by a child that was never started. The main tree is the
        // exception and does not create a path, so it is not checked.
        if !main_tree {
            // Under the named repository, when there is one — the runner arranges it there.
            let under = match &repo {
                Some(r) if r.starts_with('/') => r.clone(),
                Some(r) => format!("{workspace}/{r}"),
                None => workspace.clone(),
            };
            let path = worktree_path(&under, &slug);
            if std::fs::symlink_metadata(&path).is_ok() {
                return Invocation::failed(
                    format!("the path {path} already exists"),
                    format!(
                        "`{path}` is already a directory or a worktree, so a fresh \
                         worktree cannot be created there. Nothing was arranged and \
                         nothing was spawned. Use a different `slug`, or `main_tree: \
                         true` to work in the main checkout (the exception)."
                    ),
                );
            }
        }

        let spec = TaskSpec {
            role: role.to_string(),
            downgrade,
            placement,
            model,
            // The runner fills this after the git work: the placement is a fact the
            // spawn carries, and the tool does not invent one the runner did not make.
            worktree: None,
        };
        let worktree_spec = WorktreeSpec {
            slug,
            base,
            main_tree,
            repo,
        };

        let handle = match self.runner.start_worktree(prompt, &spec, &worktree_spec) {
            Ok(h) => h,
            Err(e) => return Invocation::failed(e, "the subagent did not start."),
        };
        ctx.progress(&format!(
            "subagent {} started in {}",
            handle.handle, handle.placement.path
        ));

        // **The placement is in the answer.** The handle names the path, the branch
        // and the base SHA, so the caller and the operator see where the child is
        // working without re-reading the brief. The main tree is named as the
        // exception when it was the exception, and a firecode spawn says the VM got
        // a copy rather than pretending it is the same directory.
        let where_line = if handle.placement.main_tree {
            "the main checkout (the exception — a fresh worktree is the default)".to_string()
        } else {
            "a fresh worktree".to_string()
        };
        let seam_line = match handle.placement.main_tree {
            true => String::new(),
            false => placement_seam(spec.placement),
        };
        // **`Backgrounded`, not `Ok`** — the same reason `task` gives: the outcome
        // names a fact about the world, *this is running and has not answered yet*.
        Invocation::backgrounded(
            handle.handle.clone(),
            std::time::Duration::ZERO,
            letibot_transcript::Backgrounding::Asked,
            format!(
                "carry on — `{0}`'s answer is delivered to you on its own when it \
                 finishes, so there is nothing to wait for. `task_result` with \
                 task=\"{0}\" and no `timeout_ms` shows where it has got to.",
                handle.handle
            ),
            format!(
                "started subagent `{}` as `{}` in {}.\n  subtask: {}\n  path: {}\n  \
                 branch: {}\n  base: {}\n{seam_line}\nThe branch is the deliverable and \
                 is NOT pushed; it is to be landed by a merge queue. It is working now, \
                 and this call did not wait for it — the rest of this round runs while \
                 it does, and its answer comes back to you as a turn of its own when it \
                 finishes. `task_result` with task=\"{}\" (no `timeout_ms`) reports \
                 where it has got to. A child in the wrong \
                 PLACE is corrected with `task_message`, not killed: killing is for \
                 wrong work.",
                handle.handle,
                match &spec.model {
                    Some(m) => format!("{m} (as a {})", spec.role),
                    None => spec.role.clone(),
                },
                where_line,
                first_line(prompt),
                handle.placement.path,
                handle.placement.branch,
                handle.placement.base_sha,
                handle.handle,
            ),
        )
    }
}

/// `task_result` — collect a subagent `task` started.
pub struct TaskResultTool {
    runner: Arc<dyn TaskRunner>,
}

impl TaskResultTool {
    pub fn new(runner: Arc<dyn TaskRunner>) -> Self {
        TaskResultTool { runner }
    }
}

impl Tool for TaskResultTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "task_result",
            "Look at a subagent started by `task`. You do not need this to get its \
             answer: that is delivered to you on its own, as a turn, when it finishes. \
             Give `task` (the handle `task` returned) to see where it has got to. \
             `timeout_ms` blocks only for a child this session is not already being \
             told about. The \
             three endings are three different outcomes: it answered, it failed, or \
             it is still working. Call with no `task` to list the subagents this \
             session started.",
            json!({
                "type": "object",
                "properties": {
                    "task": {"type": "string", "description": "The handle `task` returned. Omit to list this session's subagents."},
                    "timeout_ms": {"type": "integer", "description": "Block up to this long for a subagent whose answer is not already being delivered to you. Omit to report its state without waiting."}
                }
            }),
            // Reading the answer of a child that has already been governed by its
            // own gate. Nothing here reaches the host.
            Access::Read,
        )
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &serde_json::Value) -> Invocation {
        let Some(handle) = args.get("task").and_then(|v| v.as_str()) else {
            let started = self.runner.started();
            if started.is_empty() {
                return Invocation::ok(
                    "this session has started no subagents. `task` starts one and \
                     returns the handle to collect it by."
                        .to_string(),
                );
            }
            let mut out = format!("{} subagent(s) started here:\n", started.len());
            for h in &started {
                let state = match self.runner.collect(h, std::time::Duration::ZERO) {
                    TaskStatus::Running { note: Some(n) } => format!("running — {n}"),
                    TaskStatus::Running { note: None } => "running".into(),
                    TaskStatus::Done { .. } => "answered".into(),
                    TaskStatus::Failed { .. } => "failed".into(),
                    TaskStatus::Unknown => "unknown".into(),
                };
                out.push_str(&format!("  {h} — {state}\n"));
            }
            return Invocation::ok(out);
        };
        // A deadline always applies, and its expiry is its own outcome rather
        // than being reported as completion — `job_wait`'s rule, for the same
        // reason: a wait that ran out and a child that answered are two facts.
        let timeout = args
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .map(std::time::Duration::from_millis)
            .unwrap_or(std::time::Duration::ZERO);
        // **R23, which `job_wait` got and this tool did not.** The daemon watches every child
        // and hands its answer to the model as a turn of its own, so a wait on a watched child
        // holds the floor for a deadline while the answer is already on its way. The operator,
        // after a stroppy session spent 240 s inside `task_result`: *"why it decided to wait
        // for it explicitly if we have completion events"*, and *"on other hosts they just
        // start subagents and dont wait"*. So a watched child is looked at, not waited on: an
        // answer or a failure already in is returned as ever, and a child still working is
        // reported as such at once. Unwatched handles — another session's, a runtime with no
        // watcher — keep the blocking wait they asked for.
        let timeout = if !timeout.is_zero() && ctx.completion_delivered(handle) {
            match self.runner.collect(handle, std::time::Duration::ZERO) {
                TaskStatus::Running { note } => {
                    return Invocation::ok(format!(
                        "nothing to wait for: `{handle}` is still working{}, and its answer \
                         reaches you on its own when it finishes — the daemon is watching it \
                         and will hand you the result unprompted, as a turn of its own. \
                         Waiting here would hold the floor while the answer is already on its \
                         way. Carry on with something else; `task_message` steers it and \
                         `job_kill` stops it.",
                        note.map(|n| format!(" (last: {n})")).unwrap_or_default()
                    ));
                }
                _ => std::time::Duration::ZERO,
            }
        } else {
            timeout
        };
        match self.runner.collect(handle, timeout) {
            TaskStatus::Done { answer } => {
                // **The child has finished, so its branch goes to be landed.** This is the one
                // door the tool layer has onto a child's finish, and `task_start`'s own answer
                // already sends the model here (*"call `task_result` with task=…"*), so the
                // ordinary path reaches it. The runner does the work and says what happened;
                // a plain `task` child has no branch of its own and answers `NoBranch`, which
                // is why nothing is appended for it and every existing answer reads exactly as
                // it did.
                //
                // **The answer comes first and the queue line after it**, because the answer
                // is what the model asked for and the queue line is news about it.
                match self.runner.finished(handle) {
                    Ok(Finished::Queued { id, branch }) => Invocation::ok(format!(
                        "{answer}\n\n[merge queue] `{branch}` is enqueued as `{id}` and is \
                         waiting to be landed: the gatekeeper reviews it against the brief it \
                         was given, then the queue rebases it at the tip of main and runs the \
                         gate. The worktree stays until it lands."
                    )),
                    Ok(Finished::NoBranch { .. }) => Invocation::ok(answer),
                    // **A failure is said, not swallowed.** The branch is not pushed and nobody
                    // else carries it, so a parent told nothing would believe the work was on
                    // its way to main when it is sitting on a branch in a worktree.
                    Err(why) => Invocation::ok(format!(
                        "{answer}\n\n[merge queue] the branch of `{handle}` was NOT enqueued: \
                         {why} It is still on its branch in its worktree, and nothing has been \
                         lost — but nothing will land it until it is enqueued."
                    )),
                }
            }
            TaskStatus::Failed { why } => Invocation::failed(
                format!("subagent `{handle}` did not finish"),
                format!(
                    "{why}\n\nNothing of its work is lost: it has its own session, \
                         and its transcript is on the board under `{handle}`."
                ),
            ),
            TaskStatus::Running { note } => Invocation::abstained(
                format!("subagent `{handle}` is still working"),
                format!(
                    "the deadline passed and `{handle}` has not answered yet{}. This is \
                     the wait ending, not the subagent ending — call `task_result` again \
                     with a longer `timeout_ms`, or do something else and collect it \
                     later.",
                    note.map(|n| format!(" (last: {n})")).unwrap_or_default()
                ),
            ),
            TaskStatus::Unknown => Invocation::failed(
                format!("no subagent called `{handle}` was started here"),
                "call `task_result` with no `task` to list the ones that were.".to_string(),
            ),
        }
    }
}

/// `task_message` — say something to a session, running or not.
///
/// The third thing a parent does to work it handed off: `task` starts a child,
/// `task_result` reads it, `job_kill` stops it, and this **steers** it. Sibling by shape and
/// by reason — see [`TaskRunner::send`] for why a message is not a second prompt in the
/// recipient's session, for the two deliveries (a running turn and a session between turns),
/// and for why a session that is gone is refused rather than queued.
///
/// **And it is not only for children.** The operator's correction to this channel, verbatim:
/// *"well gatekeeper is not a child so we are not doing up"* — a session sends another session
/// a message, laterally, and the handle may name **any live session**: a subagent this one
/// started, or a peer (the session a review is hosted by, the session that queued the work).
/// That is the same door and the same act; only the wording of the refusal was child-shaped.
pub struct TaskMessageTool {
    runner: Arc<dyn TaskRunner>,
}

impl TaskMessageTool {
    pub fn new(runner: Arc<dyn TaskRunner>) -> Self {
        TaskMessageTool { runner }
    }
}

impl Tool for TaskMessageTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "task_message",
            "Say something to a session — a live correction. Give \
             `task` (the handle `task` returned, or the id of any live session) and `text` \
             (what to say). It is not a second prompt: the session hears it as your message \
             and carries on. A session whose turn is running hears it at its next round \
             boundary; a session that has answered and is waiting is woken and runs a turn \
             for it. Refused by name if no session of that id is live. Read a subagent with \
             `task_result`; stop one with `job_kill`.",
            json!({
                "type": "object",
                "properties": {
                    "task": {"type": "string", "description": "The handle `task` returned, or the id of a live session to address."},
                    "text": {"type": "string", "description": "What to say to the session. It keeps working; this steers what it does next."}
                },
                "required": ["task", "text"]
            }),
            // `Session`, exactly as `task` and `task_result` are: this reaches a child's own
            // session and nothing on the host. The child's own gate governs whatever it does
            // with what it is told — a parent cannot use a message to launder a capability.
            Access::Session,
        )
    }

    fn invoke(&self, _ctx: &mut InvokeCtx<'_>, args: &serde_json::Value) -> Invocation {
        let Some(handle) = args.get("task").and_then(|v| v.as_str()) else {
            return Invocation::failed(
                "task_message needs the `task` handle",
                "call `task_message` with `task` set to the handle `task` returned.",
            );
        };
        let Some(text) = args.get("text").and_then(|v| v.as_str()) else {
            return Invocation::failed(
                "task_message needs `text`",
                "call `task_message` with `text` set to what the subagent should be told.",
            );
        };
        let text = text.trim();
        if text.is_empty() {
            return Invocation::failed(
                "the message was empty",
                "an empty message is not a correction. Nothing was sent.",
            );
        }
        match self.runner.send(handle, text) {
            Ok(said) => Invocation::ok(said),
            // A refusal comes back unchanged, like `job_kill`'s: the runner is the only thing
            // that knows whether the child heard it, and a tool that reformatted its refusal
            // into a success is the defect this tree already paid for once.
            Err(why) => Invocation::failed(format!("subagent `{handle}` was not messaged"), why),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A runner that answers immediately: `start` records what it was asked and
    /// `collect` hands the same sentence back, so the tests are about the tool's
    /// own shape and not about threads.
    #[derive(Default)]
    struct Echo {
        answers: std::sync::Mutex<Vec<(String, String)>>,
    }

    impl TaskRunner for Echo {
        fn start(&self, prompt: &str, spec: &TaskSpec) -> Result<String, String> {
            let mut a = self.answers.lock().unwrap();
            let handle = format!("sub-{}", a.len() + 1);
            a.push((
                handle.clone(),
                format!(
                    "{} [{}] @{}: {prompt}",
                    spec.role,
                    spec.downgrade.describe(),
                    spec.placement.as_str()
                ),
            ));
            Ok(handle)
        }

        fn collect(&self, handle: &str, _timeout: std::time::Duration) -> TaskStatus {
            match self
                .answers
                .lock()
                .unwrap()
                .iter()
                .find(|(h, _)| h == handle)
            {
                Some((_, answer)) => TaskStatus::Done {
                    answer: answer.clone(),
                },
                None => TaskStatus::Unknown,
            }
        }

        fn started(&self) -> Vec<String> {
            self.answers
                .lock()
                .unwrap()
                .iter()
                .map(|(h, _)| h.clone())
                .collect()
        }
    }

    fn spec(role: &str) -> TaskSpec {
        TaskSpec {
            role: role.into(),
            ..Default::default()
        }
    }

    /// **A correction reaches the runner, and a refusal is not an `Ok`.**
    ///
    /// The whole value of `task_message` is that a parent cannot be told its child was
    /// corrected when it was not, so this pins both directions: what the runner is asked to
    /// say, and what the caller sees when the runner refuses.
    #[test]
    fn a_message_reaches_the_runner_and_a_refusal_is_not_an_ok() {
        use crate::runtime::{Registry, ToolRuntime};
        use letibot_transcript::ToolCall;

        /// Records what it was asked to say; refuses one handle by name.
        struct Steerable(std::sync::Mutex<Vec<(String, String)>>);

        impl TaskRunner for Steerable {
            fn start(&self, _p: &str, _s: &TaskSpec) -> Result<String, String> {
                Ok("sub-1".into())
            }
            fn collect(&self, _h: &str, _t: std::time::Duration) -> TaskStatus {
                TaskStatus::Unknown
            }
            fn send(&self, handle: &str, text: &str) -> Result<String, String> {
                if handle == "sub-finished" {
                    return Err("`sub-finished` is not running a turn".into());
                }
                self.0
                    .lock()
                    .unwrap()
                    .push((handle.to_string(), text.to_string()));
                Ok(format!("`{handle}` was told"))
            }
        }

        let runner = Arc::new(Steerable(std::sync::Mutex::new(Vec::new())));
        let mut reg = Registry::new();
        reg.register(Box::new(TaskMessageTool::new(runner.clone())))
            .unwrap();
        let d = crate::backend::tempdir::TempDir::new();
        let backend = crate::backend::HostBackend::new(d.path()).unwrap();
        let mut rt = ToolRuntime::new(reg, Box::new(backend));
        let mut sink = crate::NullToolSink;
        let call = |rt: &mut ToolRuntime, sink: &mut crate::NullToolSink, args: &str| {
            rt.invoke(
                "t1",
                &ToolCall {
                    id: "c1".into(),
                    name: "task_message".into(),
                    arguments: args.into(),
                },
                sink,
            )
        };

        let said = call(
            &mut rt,
            &mut sink,
            r#"{"task": "sub-1", "text": "stop and report what you have"}"#,
        );
        assert!(
            matches!(said.outcome, letibot_transcript::ToolOutcome::Ok),
            "{:?}",
            said.outcome
        );
        assert_eq!(
            runner.0.lock().unwrap().as_slice(),
            [(
                "sub-1".to_string(),
                "stop and report what you have".to_string()
            )]
        );

        // **A child that has already answered is a failure, not a quiet success.** This is
        // the defect class the kill already paid for: a tool reporting a state change that
        // nothing made.
        let missed = call(
            &mut rt,
            &mut sink,
            r#"{"task": "sub-finished", "text": "hello"}"#,
        );
        assert!(
            matches!(
                missed.outcome,
                letibot_transcript::ToolOutcome::Failed { .. }
            ),
            "{:?}",
            missed.outcome
        );
        assert!(
            missed.payload.contains("not running a turn"),
            "{}",
            missed.payload
        );

        // Both arguments are required, and the miss says which one.
        let missing = call(&mut rt, &mut sink, r#"{"task": "sub-1"}"#);
        assert!(missing.payload.contains("`text`"), "{}", missing.payload);

        // And the default a session with no runner gets refuses by name.
        let why = NoTaskRunner.send("sub-1", "x").unwrap_err();
        assert!(why.contains("cannot message"), "{why}");
    }

    /// **The call returns while the child is still working.** This is the whole
    /// point of the change: the daemon runs a round's calls in order on one
    /// thread, so a `task` that waited held every call behind it for as long as
    /// the child lived. Measured 2026-09-17 in the operator's session — a block
    /// of `todo_write`, `task` and three `web_search` calls, all unfinished for
    /// the fifteen minutes the subagent ran.
    /// **R23 for subagents: a watched child is looked at, not waited on.** The stroppy session
    /// spent 240 s in `task_result` on a child whose answer the daemon was already going to
    /// deliver as a turn — *"why it decided to wait for it explicitly if we have completion
    /// events"*. With the watch question wired, a `timeout_ms` on a child still working returns
    /// at once and says the answer is on its way; an answer already in is returned as ever; and
    /// a child nobody is watching keeps the wait it asked for.
    #[test]
    fn a_watched_subagent_is_not_waited_on() {
        use std::sync::atomic::{AtomicBool, Ordering};
        struct Gate {
            done: Arc<AtomicBool>,
        }
        impl TaskRunner for Gate {
            fn start(&self, _p: &str, _s: &TaskSpec) -> Result<String, String> {
                Ok("sub-w".into())
            }
            fn collect(&self, _h: &str, timeout: std::time::Duration) -> TaskStatus {
                let deadline = std::time::Instant::now() + timeout;
                loop {
                    if self.done.load(Ordering::SeqCst) {
                        return TaskStatus::Done {
                            answer: "the child's answer".into(),
                        };
                    }
                    if std::time::Instant::now() >= deadline {
                        return TaskStatus::Running { note: None };
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }
            fn started(&self) -> Vec<String> {
                vec!["sub-w".into()]
            }
        }
        let run = |watched: bool, done: bool| {
            let runner: Arc<dyn TaskRunner> = Arc::new(Gate {
                done: Arc::new(AtomicBool::new(done)),
            });
            let d = crate::backend::tempdir::TempDir::new();
            let backend = crate::backend::HostBackend::new(d.path()).unwrap();
            let mut reg = crate::runtime::Registry::new();
            reg.register(Box::new(TaskResultTool::new(runner))).unwrap();
            let mut rt = crate::runtime::ToolRuntime::new(reg, Box::new(backend));
            if watched {
                rt = rt.with_completion_delivered(Arc::new(|j: &str| j == "sub-w"));
            }
            let started = std::time::Instant::now();
            let r = rt.invoke(
                "t1",
                &letibot_transcript::ToolCall {
                    id: "c0".into(),
                    name: "task_result".into(),
                    arguments: r#"{"task": "sub-w", "timeout_ms": 400}"#.into(),
                },
                &mut crate::NullToolSink,
            );
            (r, started.elapsed())
        };

        let (r, took) = run(true, false);
        assert!(
            took < std::time::Duration::from_millis(200),
            "a watched child held the floor for {took:?}"
        );
        assert!(r.payload.contains("nothing to wait for"), "{}", r.payload);
        assert!(r.payload.contains("on its own"), "{}", r.payload);

        let (r, _) = run(true, true);
        assert!(r.payload.contains("the child's answer"), "{}", r.payload);

        // Unwatched: the wait it asked for, to the deadline.
        let (r, took) = run(false, false);
        assert!(
            took >= std::time::Duration::from_millis(400),
            "an unwatched wait returned early: {took:?}"
        );
        assert!(r.payload.contains("has not answered yet"), "{}", r.payload);
    }

    #[test]
    fn starting_a_subagent_does_not_wait_for_it() {
        use std::sync::Arc as StdArc;
        use std::sync::atomic::{AtomicBool, Ordering};

        /// A child that never finishes unless this test lets it.
        struct Slow {
            release: StdArc<AtomicBool>,
        }
        impl TaskRunner for Slow {
            fn start(&self, _p: &str, _s: &TaskSpec) -> Result<String, String> {
                Ok("sub-slow".into())
            }
            fn collect(&self, handle: &str, timeout: std::time::Duration) -> TaskStatus {
                if handle != "sub-slow" {
                    return TaskStatus::Unknown;
                }
                let deadline = std::time::Instant::now() + timeout;
                loop {
                    if self.release.load(Ordering::SeqCst) {
                        return TaskStatus::Done {
                            answer: "the child answered".into(),
                        };
                    }
                    if std::time::Instant::now() >= deadline {
                        return TaskStatus::Running {
                            note: Some("still going".into()),
                        };
                    }
                    std::thread::yield_now();
                }
            }
            fn started(&self) -> Vec<String> {
                vec!["sub-slow".into()]
            }
        }

        let release = StdArc::new(AtomicBool::new(false));
        let runner: Arc<dyn TaskRunner> = Arc::new(Slow {
            release: release.clone(),
        });
        let tool = TaskTool::new(runner.clone());
        let result = TaskResultTool::new(runner);

        let d = crate::backend::tempdir::TempDir::new();
        let backend = crate::backend::HostBackend::new(d.path()).unwrap();
        let mut reg = crate::runtime::Registry::new();
        reg.register(Box::new(tool)).unwrap();
        reg.register(Box::new(result)).unwrap();
        let mut rt = crate::runtime::ToolRuntime::new(reg, Box::new(backend));
        let mut sink = crate::NullToolSink;
        let call = |rt: &mut crate::runtime::ToolRuntime,
                    sink: &mut crate::NullToolSink,
                    name: &str,
                    args: &str| {
            rt.invoke(
                "t1",
                &letibot_transcript::ToolCall {
                    id: "c0".into(),
                    name: name.into(),
                    arguments: args.into(),
                },
                sink,
            )
        };

        // The child is nowhere near done, and `task` comes back anyway —
        // `Backgrounded`, not `Ok`: it is running and has not answered yet.
        let started = std::time::Instant::now();
        let r = call(&mut rt, &mut sink, "task", r#"{"prompt": "a long job"}"#);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
        assert!(
            matches!(
                r.outcome,
                letibot_transcript::ToolOutcome::Backgrounded { .. }
            ),
            "{:?}",
            r.outcome
        );
        assert!(r.payload.contains("task_result"), "{}", r.payload);

        // Collecting before it is done is the WAIT ending, not the child ending:
        // an abstention, which `is_grounded` will not treat as an answer.
        let waited = call(
            &mut rt,
            &mut sink,
            "task_result",
            r#"{"task": "sub-slow", "timeout_ms": 5}"#,
        );
        assert!(
            matches!(
                waited.outcome,
                letibot_transcript::ToolOutcome::Abstained { .. }
            ),
            "{:?}",
            waited.outcome
        );
        assert!(waited.payload.contains("still going"), "{}", waited.payload);

        release.store(true, Ordering::SeqCst);
        let done = call(&mut rt, &mut sink, "task_result", r#"{"task": "sub-slow"}"#);
        assert_eq!(done.payload, "the child answered");
        assert!(
            matches!(done.outcome, letibot_transcript::ToolOutcome::Ok),
            "{:?}",
            done.outcome
        );

        // A handle nobody started is a failure that says how to find the ones
        // that were.
        let miss = call(&mut rt, &mut sink, "task_result", r#"{"task": "sub-nope"}"#);
        assert!(matches!(
            miss.outcome,
            letibot_transcript::ToolOutcome::Failed { .. }
        ));
        let listed = call(&mut rt, &mut sink, "task_result", "{}");
        assert!(listed.payload.contains("sub-slow"), "{}", listed.payload);
    }

    /// **A child finishing enqueues its branch** — the seam the merge queue's enqueue hangs on,
    /// driven through the tool with a fake runner.
    ///
    /// The claim is the tool's half of `task_start`'s door: when a `task_start` child is seen
    /// to finish, the tool asks its runner to enqueue the branch and reports what the runner
    /// answered. The runner's own half — the row, the store, the brief — is
    /// `harnessd::mergequeue::entry_for_finished`'s and is tested there; what this test holds is
    /// that the ask happens at all, and that a plain `task` child does not trigger it.
    #[test]
    fn a_child_finishing_enqueues_its_branch() {
        use crate::runtime::{Registry, ToolRuntime};
        use letibot_transcript::ToolCall;

        /// A runner that answers `Done` and records the enqueue it was asked for.
        struct FinishedRunner {
            finished: Arc<std::sync::Mutex<Vec<String>>>,
            /// What `finished` answers with, so both arms can be driven.
            answer: Option<Result<Finished, String>>,
        }
        impl TaskRunner for FinishedRunner {
            fn start(&self, _p: &str, _s: &TaskSpec) -> Result<String, String> {
                Ok("sub-1".into())
            }
            fn collect(&self, _h: &str, _t: std::time::Duration) -> TaskStatus {
                TaskStatus::Done {
                    answer: "the child answered".into(),
                }
            }
            fn start_worktree(
                &self,
                _p: &str,
                _s: &TaskSpec,
                w: &WorktreeSpec,
            ) -> Result<WorktreeHandle, String> {
                Ok(WorktreeHandle {
                    handle: "sub-1".into(),
                    placement: WorktreePlacement {
                        path: worktree_path("/repo", &w.slug),
                        branch: worktree_branch(&w.slug),
                        base_sha: "deadbeef".into(),
                        main_tree: false,
                    },
                })
            }
            fn finished(&self, handle: &str) -> Result<Finished, String> {
                self.finished.lock().unwrap().push(handle.to_string());
                match &self.answer {
                    Some(Ok(f)) => Ok(f.clone()),
                    Some(Err(e)) => Err(e.clone()),
                    None => Ok(Finished::NoBranch {
                        why: "not a task_start child".into(),
                    }),
                }
            }
        }

        let d = crate::backend::tempdir::TempDir::new();
        let backend = crate::backend::HostBackend::new(d.path()).unwrap();
        let call = |runner: Arc<dyn TaskRunner>, args: &str| {
            let mut reg = Registry::new();
            reg.register(Box::new(TaskStartTool::new(runner.clone())))
                .unwrap();
            reg.register(Box::new(TaskResultTool::new(runner))).unwrap();
            let mut rt = ToolRuntime::new(reg, Box::new(backend.clone()));
            let mut sink = crate::NullToolSink;
            let _ = rt.invoke(
                "t1",
                &ToolCall {
                    id: "c0".into(),
                    name: "task_start".into(),
                    arguments: r#"{"prompt": "do the work", "slug": "do-the-work"}"#.into(),
                },
                &mut sink,
            );
            rt.invoke(
                "t1",
                &ToolCall {
                    id: "c1".into(),
                    name: "task_result".into(),
                    arguments: args.into(),
                },
                &mut sink,
            )
        };

        // **A `task_start` child's finish is reported.** The child answered, the runner was
        // asked to enqueue its branch, and the answer says where the branch is going.
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let r = call(
            Arc::new(FinishedRunner {
                finished: seen.clone(),
                answer: Some(Ok(Finished::Queued {
                    id: "sub-1".into(),
                    branch: "agent/do-the-work".into(),
                })),
            }),
            r#"{"task": "sub-1"}"#,
        );
        assert_eq!(
            *seen.lock().unwrap(),
            vec!["sub-1"],
            "the tool must ask its runner to enqueue the finished child's branch"
        );
        assert!(r.payload.starts_with("the child answered"), "{}", r.payload);
        assert!(r.payload.contains("agent/do-the-work"), "{}", r.payload);
        assert!(r.payload.contains("merge queue"), "{}", r.payload);

        // **A runner with no branch to land says nothing extra.** The ordinary case for a plain
        // `task` child, and the reason every existing `task_result` answer reads as it did.
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let r = call(
            Arc::new(FinishedRunner {
                finished: seen.clone(),
                answer: None,
            }),
            r#"{"task": "sub-1"}"#,
        );
        assert_eq!(*seen.lock().unwrap(), vec!["sub-1"]);
        assert_eq!(r.payload, "the child answered");

        // **A refusal is SAID, not swallowed.** The branch is not pushed and nobody else carries
        // it, so a parent told nothing would believe the work was on its way to main.
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let r = call(
            Arc::new(FinishedRunner {
                finished: seen.clone(),
                answer: Some(Err("the store could not be opened".into())),
            }),
            r#"{"task": "sub-1"}"#,
        );
        assert!(r.payload.contains("was NOT enqueued"), "{}", r.payload);
        assert!(r.payload.contains("could not be opened"), "{}", r.payload);
    }

    /// **A runner that arranges nothing answers `NoBranch`, and that is not a failure.**
    ///
    /// `NoTaskRunner` is the default seat, and a `task_result` against it must not grow a line
    /// about a merge queue the session has no door to.
    #[test]
    fn a_runner_with_no_worktree_leaves_no_branch() {
        match NoTaskRunner.finished("anything").unwrap() {
            Finished::NoBranch { why } => assert!(why.contains("no worktree"), "{why}"),
            other => panic!("a runner that arranges nothing must answer NoBranch: {other:?}"),
        }
    }

    #[test]
    fn a_runner_is_delegated_to_and_no_runner_refuses() {
        let e = Echo::default();
        let h = e.start("find the bug", &spec("coder")).unwrap();
        assert_eq!(
            e.collect(&h, std::time::Duration::ZERO),
            TaskStatus::Done {
                answer: "coder [none] @host: find the bug".into()
            }
        );
        assert!(
            NoTaskRunner
                .start("x", &spec("coder"))
                .unwrap_err()
                .contains("no subagent runner")
        );
        assert_eq!(
            NoTaskRunner.collect("anything", std::time::Duration::ZERO),
            TaskStatus::Unknown
        );
    }

    #[test]
    fn a_downgrade_only_removes_and_the_union_keeps_every_denial() {
        let survey = Downgrade::parse("read-only").unwrap();
        assert!(
            survey.denies(Access::Write)
                && survey.denies(Access::Exec)
                && survey.denies(Access::Network)
        );
        assert!(!survey.denies(Access::Read) && !survey.denies(Access::Session));
        assert_eq!(survey.describe(), "no write, no exec, no network");
        let nw = Downgrade::parse("no-write, no-network").unwrap();
        assert_eq!(nw.describe(), "no write, no network");
        // A parent that cannot write cannot hand a child writes back by asking for
        // less: the union is what the child gets.
        assert_eq!(nw.and(&Downgrade::parse("no-exec").unwrap()), survey);
        assert!(
            Downgrade::parse("no-read")
                .unwrap_err()
                .contains("not a subagent")
        );
        assert!(
            Downgrade::parse("sudo")
                .unwrap_err()
                .contains("not a downgrade")
        );
        assert!(Downgrade::parse("").unwrap().is_none());
    }

    #[test]
    fn the_tool_parses_access_and_where_and_refuses_the_unknown() {
        use crate::runtime::{Registry, ToolRuntime};
        use letibot_transcript::ToolCall;
        let mut reg = Registry::new();
        let runner: Arc<dyn TaskRunner> = Arc::new(Echo::default());
        reg.register(Box::new(TaskTool::new(runner.clone())))
            .unwrap();
        reg.register(Box::new(TaskResultTool::new(runner))).unwrap();
        let d = crate::backend::tempdir::TempDir::new();
        let backend = crate::backend::HostBackend::new(d.path()).unwrap();
        let mut rt = ToolRuntime::new(reg, Box::new(backend));
        let mut sink = crate::NullToolSink;
        let run = |rt: &mut ToolRuntime, sink: &mut crate::NullToolSink, args: &str| {
            rt.invoke(
                "t1",
                &ToolCall {
                    id: "c0".into(),
                    name: "task".into(),
                    arguments: args.into(),
                },
                sink,
            )
            .payload
        };
        // `task` hands back a handle; the spec it parsed is what the subagent was
        // started with, and `task_result` is where the answer comes from.
        // A host that has firecode: `$FIRECODE_BIN` names a program that is there. (The
        // refusal for a host that has none is the end of this test.)
        let stub =
            std::env::temp_dir().join(format!("letibot-firecode-stub-{}", std::process::id()));
        std::fs::write(&stub, "#!/bin/sh\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        unsafe { std::env::set_var("FIRECODE_BIN", &stub) };
        let started = run(
            &mut rt,
            &mut sink,
            r#"{"prompt": "survey the crates", "role": "researcher", "access": "read-only", "where": "firecode"}"#,
        );
        assert!(started.contains("started subagent `sub-1`"), "{started}");
        assert!(started.contains("survey the crates"), "{started}");
        assert!(started.contains("task_result"), "{started}");
        let collected = rt
            .invoke(
                "t1",
                &ToolCall {
                    id: "c1".into(),
                    name: "task_result".into(),
                    arguments: r#"{"task": "sub-1"}"#.into(),
                },
                &mut sink,
            )
            .payload;
        assert_eq!(
            collected, "researcher [no write, no exec, no network] @firecode: survey the crates",
            "the spec `task` parsed is the spec the subagent ran under"
        );
        let p = run(&mut rt, &mut sink, r#"{"prompt": "x", "access": "root"}"#);
        assert!(p.contains("not a downgrade"), "{p}");
        let p = run(&mut rt, &mut sink, r#"{"prompt": "x", "where": "moon"}"#);
        assert!(p.contains("not a placement"), "{p}");
        // **A host with no firecode refuses the VM at the call**, with nothing spawned and
        // `host` named — not a child that dies at its start. The operator's Mac, 2026-10-08.
        unsafe { std::env::set_var("FIRECODE_BIN", "/nonexistent/firecode") };
        let p = run(&mut rt, &mut sink, r#"{"prompt": "x", "where": "vm"}"#);
        unsafe { std::env::remove_var("FIRECODE_BIN") };
        let _ = std::fs::remove_file(&stub);
        assert!(p.contains("cannot be honoured"), "{p}");
        assert!(p.contains("Nothing was spawned"), "{p}");
        assert!(p.contains("`host`"), "{p}");
        assert!(!p.contains("started subagent `sub-2`"), "{p}");
    }

    /// **The seam line for a VM placement says where the work comes back** — FIRECODE-NOTES:
    /// *"It says … 'the work comes back as the branch, not as the directory.' There is no branch
    /// on this path — the work comes back as a sibling directory. A confidently wrong line in a
    /// tool's own output is worse than no line."*
    #[test]
    fn a_vm_placement_does_not_promise_a_branch() {
        let line = placement_seam(Placement::Firecode);
        assert!(!line.contains("comes back as the branch"), "{line}");
        assert!(line.contains("directory beside"), "{line}");
    }

    /// **The slug is short, kebab, and deterministic in the prompt.**
    ///
    /// The derivation is the default, not a lock: two different subtasks get two
    /// different slugs, and the same subtask asked twice gets the same slug — which
    /// is what makes the refusal of an existing path meaningful. Pure, so it is
    /// tested without a repo.
    #[test]
    fn the_slug_is_short_kebab_and_deterministic() {
        assert_eq!(
            slug_from_prompt("add a new tool task_start"),
            "add-a-new-tool"
        );
        assert_eq!(
            slug_from_prompt("Fix the bug in the parser"),
            "fix-the-bug-in"
        );
        // Non-alphanumerics collapse to a single hyphen.
        assert_eq!(
            slug_from_prompt("hello, world! foo_bar"),
            "hello-world-foo-bar"
        );
        // The same prompt gives the same slug.
        assert_eq!(
            slug_from_prompt("a long prompt that goes on and on"),
            slug_from_prompt("a long prompt that goes on and on")
        );
        // A different prompt gives a different slug.
        assert_ne!(
            slug_from_prompt("a long prompt that goes on and on"),
            slug_from_prompt("a different prompt that goes elsewhere")
        );
        // Capped at a length that keeps the path legible.
        let long = slug_from_prompt(
            "one two three four five six seven eight nine ten eleven twelve thirteen",
        );
        assert!(long.chars().count() <= 40, "{long}");
        assert!(!long.ends_with('-'), "{long}");
        // An empty prompt gets a default rather than an empty slug.
        assert_eq!(slug_from_prompt(""), "task");
        assert_eq!(slug_from_prompt("   "), "task");
    }

    /// **The worktree path and branch are pure functions of the workspace and slug.**
    #[test]
    fn the_worktree_path_and_branch_are_pure() {
        assert_eq!(
            worktree_path("/repo", "task-start"),
            "/repo/.claude/worktrees/agent-task-start"
        );
        assert_eq!(worktree_branch("task-start"), "agent/task-start");
    }

    /// **A `task_start` whose path already exists is refused by name, and nothing is
    /// spawned.**
    ///
    /// The default is a fresh worktree, and a path that already exists — a directory,
    /// or a worktree a previous `task_start` made — is refused here rather than by a
    /// child that was never started. The refusal names the path and says nothing was
    /// spawned.
    #[test]
    fn an_existing_path_is_refused_by_name_and_nothing_is_spawned() {
        use crate::runtime::{Registry, ToolRuntime};
        use letibot_transcript::ToolCall;

        /// A runner that records whether it was asked to arrange a worktree.
        struct PlacedRunner {
            asked: Arc<std::sync::atomic::AtomicBool>,
        }
        impl TaskRunner for PlacedRunner {
            fn start(&self, _p: &str, _s: &TaskSpec) -> Result<String, String> {
                Ok("sub-1".into())
            }
            fn collect(&self, _h: &str, _t: std::time::Duration) -> TaskStatus {
                TaskStatus::Unknown
            }
            fn start_worktree(
                &self,
                _p: &str,
                _s: &TaskSpec,
                _w: &WorktreeSpec,
            ) -> Result<WorktreeHandle, String> {
                self.asked.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(WorktreeHandle {
                    handle: "sub-1".into(),
                    placement: WorktreePlacement {
                        path: "/nowhere".into(),
                        branch: "agent/x".into(),
                        base_sha: "abc".into(),
                        main_tree: false,
                    },
                })
            }
        }

        let d = crate::backend::tempdir::TempDir::new();
        // The backend canonicalises its root, so the workspace the tool sees is the
        // canonicalised path — use the same one to compute the path the tool checks.
        let workspace = d
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .to_string();
        // Create the path the tool would try to use, so the refusal fires.
        let slug = "existing";
        let path = worktree_path(&workspace, slug);
        std::fs::create_dir_all(&path).unwrap();

        let asked = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let runner: Arc<dyn TaskRunner> = Arc::new(PlacedRunner {
            asked: asked.clone(),
        });
        let mut reg = Registry::new();
        reg.register(Box::new(TaskStartTool::new(runner))).unwrap();
        let backend = crate::backend::HostBackend::new(d.path()).unwrap();
        let mut rt = ToolRuntime::new(reg, Box::new(backend));
        let mut sink = crate::NullToolSink;
        let r = rt.invoke(
            "t1",
            &ToolCall {
                id: "c1".into(),
                name: "task_start".into(),
                arguments: format!(r#"{{"prompt": "do the work", "slug": "{slug}"}}"#).into(),
            },
            &mut sink,
        );
        assert!(
            matches!(r.outcome, letibot_transcript::ToolOutcome::Failed { .. }),
            "{:?}",
            r.outcome
        );
        assert!(r.payload.contains(&path), "{}", r.payload);
        assert!(
            r.payload.contains("already a directory or a worktree"),
            "{}",
            r.payload
        );
        // The runner was never asked to arrange a worktree: the refusal happened
        // before the spawn.
        assert!(
            !asked.load(std::sync::atomic::Ordering::SeqCst),
            "the runner was asked to arrange a worktree for a path that already exists"
        );
    }

    /// **The placement is in the answer**: the handle names the path, the branch and
    /// the base SHA, so the caller and the operator see where the child is working
    /// without re-reading the brief.
    #[test]
    fn the_placement_is_in_the_answer() {
        use crate::runtime::{Registry, ToolRuntime};
        use letibot_transcript::ToolCall;

        /// A runner that returns a known placement, so the test is about the tool's
        /// answer and not about git.
        struct KnownPlaced;
        impl TaskRunner for KnownPlaced {
            fn start(&self, _p: &str, _s: &TaskSpec) -> Result<String, String> {
                Ok("sub-1".into())
            }
            fn collect(&self, _h: &str, _t: std::time::Duration) -> TaskStatus {
                TaskStatus::Unknown
            }
            fn start_worktree(
                &self,
                _p: &str,
                _s: &TaskSpec,
                w: &WorktreeSpec,
            ) -> Result<WorktreeHandle, String> {
                let path = worktree_path("/repo", &w.slug);
                Ok(WorktreeHandle {
                    handle: "sub-1".into(),
                    placement: WorktreePlacement {
                        path: path.clone(),
                        branch: worktree_branch(&w.slug),
                        base_sha: "deadbeef".into(),
                        main_tree: w.main_tree,
                    },
                })
            }
        }

        let d = crate::backend::tempdir::TempDir::new();
        let runner: Arc<dyn TaskRunner> = Arc::new(KnownPlaced);
        let mut reg = Registry::new();
        reg.register(Box::new(TaskStartTool::new(runner))).unwrap();
        let backend = crate::backend::HostBackend::new(d.path()).unwrap();
        let mut rt = ToolRuntime::new(reg, Box::new(backend));
        let mut sink = crate::NullToolSink;
        let r = rt.invoke(
            "t1",
            &ToolCall {
                id: "c1".into(),
                name: "task_start".into(),
                arguments: r#"{"prompt": "do the work", "slug": "task-start"}"#.into(),
            },
            &mut sink,
        );
        assert!(
            matches!(
                r.outcome,
                letibot_transcript::ToolOutcome::Backgrounded { .. }
            ),
            "{:?}",
            r.outcome
        );
        // The path, branch and base SHA are all in the answer.
        assert!(
            r.payload
                .contains("/repo/.claude/worktrees/agent-task-start"),
            "{}",
            r.payload
        );
        assert!(r.payload.contains("agent/task-start"), "{}", r.payload);
        assert!(r.payload.contains("deadbeef"), "{}", r.payload);
        // The branch is the deliverable and is not pushed.
        assert!(r.payload.contains("NOT pushed"), "{}", r.payload);
        // A child in the wrong place is corrected, not killed.
        assert!(r.payload.contains("task_message"), "{}", r.payload);
    }

    /// **The main tree is the exception, and it is named as such in the answer.**
    #[test]
    fn the_main_tree_is_named_as_the_exception() {
        use crate::runtime::{Registry, ToolRuntime};
        use letibot_transcript::ToolCall;

        struct MainTreePlaced;
        impl TaskRunner for MainTreePlaced {
            fn start(&self, _p: &str, _s: &TaskSpec) -> Result<String, String> {
                Ok("sub-1".into())
            }
            fn collect(&self, _h: &str, _t: std::time::Duration) -> TaskStatus {
                TaskStatus::Unknown
            }
            fn start_worktree(
                &self,
                _p: &str,
                _s: &TaskSpec,
                w: &WorktreeSpec,
            ) -> Result<WorktreeHandle, String> {
                Ok(WorktreeHandle {
                    handle: "sub-1".into(),
                    placement: WorktreePlacement {
                        path: "/repo".into(),
                        branch: "main".into(),
                        base_sha: "cafe0000".into(),
                        main_tree: w.main_tree,
                    },
                })
            }
        }

        let d = crate::backend::tempdir::TempDir::new();
        let runner: Arc<dyn TaskRunner> = Arc::new(MainTreePlaced);
        let mut reg = Registry::new();
        reg.register(Box::new(TaskStartTool::new(runner))).unwrap();
        let backend = crate::backend::HostBackend::new(d.path()).unwrap();
        let mut rt = ToolRuntime::new(reg, Box::new(backend));
        let mut sink = crate::NullToolSink;
        let r = rt.invoke(
            "t1",
            &ToolCall {
                id: "c1".into(),
                name: "task_start".into(),
                arguments: r#"{"prompt": "do the work", "main_tree": true}"#.into(),
            },
            &mut sink,
        );
        assert!(
            matches!(
                r.outcome,
                letibot_transcript::ToolOutcome::Backgrounded { .. }
            ),
            "{:?}",
            r.outcome
        );
        // The main tree is named as the exception.
        assert!(
            r.payload.contains("the main checkout (the exception"),
            "{}",
            r.payload
        );
    }
}
