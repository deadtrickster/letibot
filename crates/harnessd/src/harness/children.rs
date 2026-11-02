//! **Subagents**: the runner behind `task`, the child session's own turn loop,
//! and the bookkeeping (slots, records, spawn parameters) around it. The parent
//! half of every door stays on `Harness`; the child half is `serve_child_with`.
use super::*;

impl Harness {
    /// Stop this session's children, and say so on its own log: the downward edge, for a caller that
    /// holds a `Harness` rather than a `HubSteering`. All three stop doors go through
    /// `stop_children_first`, which is the one implementation.
    pub fn stop_children(&self) -> Option<String> {
        stop_children_first(Some(&self.subagents), &self.hub)
    }

    /// Why this session may NOT be parked right now, or `None` when it may: a child of this session
    /// that still has a thread, a job still running in its process host, and its unsettled children.
    /// Each is a fact that dropping the harness would lose or orphan, and each is asked of its owner.
    pub fn unsettled_children(&self) -> usize {
        self.subagents.unsettled_children()
    }

    pub fn park_refusal(&self) -> Option<&'static str> {
        if self.subagents.live_children() > 0 {
            return Some("a subagent it started is still alive");
        }
        let running = self.runtime.backend.processes().is_some_and(|p| {
            p.jobs()
                .iter()
                .any(|j| matches!(j.state, letibot_tools::exec::JobState::Running))
        });
        if running {
            return Some("a job it started is still running");
        }
        None
    }

    /// Wait until every subagent in this tree has left its own thread, up to `timeout`; `true` when they
    /// all have. Call it after the hubs are closed, which is what makes it finite. It sends and stops
    /// nothing itself, so on a live tree with a child mid-turn it waits for that turn.
    pub fn wait_for_children(&self, timeout: std::time::Duration) -> bool {
        self.subagents.wait_for_children(timeout)
    }
}

/// A few words from the first message, as a session name. Deliberately dumb: the first line, first six
/// words, no model call, because a name is an ADDRESS a person recognises a week later and a paraphrase
/// is a worse one. Empty for a message with no words, and the caller then leaves the session unnamed.
pub fn derive_title(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let words: Vec<&str> = line.split_whitespace().take(6).collect();
    let mut out = words.join(" ");
    // Long enough to recognise, short enough for a picker row and a header. This is a NAMING rule and its
    // bound is the ADDRESS, which is why nothing about a terminal belongs here: the title is also the
    // fabric alias a person has to type. A string only ever DISPLAYED takes `subagent_title`'s rule.
    const MAX: usize = 48;
    if out.chars().count() > MAX {
        out = out.chars().take(MAX - 1).collect::<String>();
        out.push('…');
    }
    out
}

/// How long a subagent's title may be: a display bound, not an address bound. The head trims the row to
/// the width it actually has, so the only job left for this number is stopping something pathological.
pub const SUBAGENT_TITLE_MAX: usize = 256;

/// The subtask, as the subagent's title. Not `derive_title`, and the difference is a defect: that bound is
/// the fabric address, while a title is a row in the pane whose only cap is the terminal's width. The head
/// trims; a child spawned without a name takes its worktree branch's slug, else the prompt's derivation.
pub(crate) fn legacy_slug(spec: &letibot_tools::builtins::task::TaskSpec, prompt: &str) -> String {
    spec.worktree
        .as_ref()
        .filter(|w| !w.main_tree)
        .and_then(|w| w.branch.strip_prefix("agent/"))
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| letibot_tools::builtins::task::slug_from_prompt(prompt))
}

pub(crate) fn subagent_title(prompt: &str) -> String {
    let line = prompt
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let mut out = line.to_string();
    if out.chars().count() > SUBAGENT_TITLE_MAX {
        out = out.chars().take(SUBAGENT_TITLE_MAX - 1).collect::<String>();
        out.push('…');
    }
    out
}

/// The visible text of a batch of appended items.
pub(crate) fn visible_text(items: &[TranscriptItem]) -> String {
    let mut out = String::new();
    for i in items {
        if let TranscriptItem::Assistant { text, .. } = i {
            out.push_str(text);
        }
    }
    out
}

/// The real `task` runner: spawns a persistent subagent session and runs it to completion, returning its
/// answer. A subagent is a full session with its own hub, store row and harness, governed by the parent's
/// ruleset, whose ask is posted to the tree's ROOT. A slot is one child as the parent sees it.
pub(crate) struct TaskSlot {
    pub(crate) state: std::sync::Mutex<letibot_tools::builtins::task::TaskStatus>,
    settled: std::sync::Condvar,
    /// Whether this subagent was stopped rather than finishing, which is the one thing its result cannot
    /// say: an interrupted turn still returns text, so a `Done` read as an answer would hand the parent a
    /// truncated reply under the child's name. Set by `kill` before the interrupt is sent.
    killed: std::sync::atomic::AtomicBool,
    /// Which session started this child: the one field supervision needs, because the tree's handle list
    /// is shared and only the owning runner may stop a child. `task_result` reads the whole list on
    /// purpose, so any ancestor can collect; stopping is the other direction and goes to the parent.
    owner: String,
    /// Whether the child's own thread has ended. Not the same question as its state: a child that has
    /// ANSWERED is still alive and still owns whatever it started, so it must still be stopped. This is
    /// what makes `stop_all` skip a child that is really gone rather than interrupt nothing.
    exited: std::sync::atomic::AtomicBool,
    /// Whether the thread ended by PARKING: a parked child's thread has exited like any other and is not
    /// gone, because its session is in the store. `send` reopens it rather than refusing a message to a
    /// thread that has ended.
    parked: std::sync::atomic::AtomicBool,
    /// The child's row in the store, moved when this slot settles or is stopped: what a later daemon reads
    /// to bring the child back. `None` for a slot with no store behind it.
    record: Option<crate::subagent_record::SubagentRecord>,
    /// The child's name: what a handle may be given as, and what every sentence about the child labels it
    /// by. `None` for a slot made without a spec, whose name is in its record.
    slug: Option<String>,
    /// **The tree this child's branch lives in**, when it has one. `finish_child_thread` reaps it
    /// once the child has failed: nothing else removes a refused, failed or killed child's tree, and
    /// a tree left behind is a copy of a branch that already holds the work.
    deliverable: std::sync::Mutex<Option<deliverable::Deliverable>>,
}

/// The handles one runner has handed out, with the slot each knocks on, in the order the
/// children were started.
pub(crate) type TaskSlots =
    std::sync::Arc<std::sync::Mutex<Vec<(String, std::sync::Arc<TaskSlot>)>>>;

impl TaskSlot {
    pub(crate) fn recorded(
        owner: &str,
        record: Option<crate::subagent_record::SubagentRecord>,
    ) -> TaskSlot {
        TaskSlot {
            record,
            ..TaskSlot::new(owner)
        }
    }

    pub(crate) fn new(owner: &str) -> TaskSlot {
        TaskSlot {
            record: None,
            slug: None,
            deliverable: std::sync::Mutex::new(None),
            state: std::sync::Mutex::new(letibot_tools::builtins::task::TaskStatus::Running {
                note: None,
                // **The reading is attached where it is ASKED for, not settled here.** A value
                // written once would be stale the moment the child was asked something more; see
                // [`HarnessTaskRunner::collect`].
                child: None,
            }),
            settled: std::sync::Condvar::new(),
            killed: std::sync::atomic::AtomicBool::new(false),
            owner: owner.to_string(),
            exited: std::sync::atomic::AtomicBool::new(false),
            parked: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// **The tree this child was handed**, recorded so a failure can leave it: see the field.
    pub(crate) fn set_deliverable(&self, d: Option<deliverable::Deliverable>) {
        *self.deliverable.lock().expect("task slot") = d;
    }

    /// Whether this child ended without an answer — the case whose tree nothing else reaps.
    pub(crate) fn failed(&self) -> bool {
        matches!(
            *self.state.lock().expect("task slot"),
            letibot_tools::builtins::task::TaskStatus::Failed { .. }
        )
    }

    /// The child parked as store-only. Set before [`TaskSlot::mark_exited`], on the same thread.
    pub(crate) fn mark_parked(&self) {
        self.parked.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub(crate) fn is_parked(&self) -> bool {
        self.parked.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// This child's thread has ended, read by `stop_all`, the only caller that has to tell "already
    /// answered" from "still here". Set under the state lock and announced on its condvar, or a waiter
    /// would lose the wakeup; `wait_until_exited` is the other reader.
    pub(crate) fn mark_exited(&self) {
        let _held = self.state.lock().expect("task slot");
        self.exited.store(true, std::sync::atomic::Ordering::SeqCst);
        self.settled.notify_all();
    }

    pub(crate) fn has_exited(&self) -> bool {
        self.exited.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Block until this child's thread has ended, or `timeout` passes; `true` if it has. A child's thread
    /// is spawned detached, so nothing can join it and a caller about to let the process exit has to know
    /// it is gone: a child still inside `tokenize` when `exit()` finalizes takes SIGSEGV.
    pub(crate) fn wait_until_exited(&self, timeout: std::time::Duration) -> bool {
        let g = self.state.lock().expect("task slot");
        // Re-checks the flag while holding the mutex, which is what pairs with the locked
        // store in `mark_exited` to make the wakeup unlosable.
        let _ = self
            .settled
            .wait_timeout_while(g, timeout, |_| !self.has_exited())
            .expect("task slot");
        self.has_exited()
    }

    /// The daemon's own progress line for this child, kept while it runs, because a note on a settled
    /// subagent would overwrite its answer. It is the SPAWN's account of itself and not the child's last
    /// word, and what it is doing now is `child_reading`'s, asked at the moment it is wanted.
    pub(crate) fn note(&self, text: &str) {
        let mut g = self.state.lock().expect("task slot");
        if let letibot_tools::builtins::task::TaskStatus::Running { note, .. } = &mut *g {
            *note = Some(text.to_string());
        }
    }

    /// Marked before the interrupt goes out, so a settlement that arrives a microsecond
    /// later cannot be recorded as the child having answered.
    pub(crate) fn kill(&self) {
        self.killed.store(true, std::sync::atomic::Ordering::SeqCst);
        // **And the store hears it now**, not when the turn winds down: a daemon that dies in
        // between must not bring back a child somebody stopped.
        if let Some(record) = &self.record {
            record.killed();
        }
    }

    /// **The status as the slot holds it**, for a test that has to assert a settlement happened  -
    /// or did not overwrite an earlier one. `collect` is the real reader.
    #[cfg(test)]
    pub(crate) fn status(&self) -> letibot_tools::builtins::task::TaskStatus {
        self.state.lock().expect("task slot").clone()
    }

    /// Settle a slot nobody has settled: the spawning thread's last word for a refusal that returned
    /// before its guard existed. A slot already settled keeps what it has, because that came from the
    /// child's own path and is the truer answer.
    pub(crate) fn settle_if_running(&self, status: letibot_tools::builtins::task::TaskStatus) {
        let running = matches!(
            *self.state.lock().expect("task slot"),
            letibot_tools::builtins::task::TaskStatus::Running { .. }
        );
        if running {
            self.settle(status);
        }
    }

    pub(crate) fn settle(&self, status: letibot_tools::builtins::task::TaskStatus) {
        let status = self.as_settled(status);
        // **The record moves with the slot**, from this one place every settlement passes
        // through - see `SubagentRecord::settled` for the one judgement it makes (a stop during
        // an orderly shutdown is the daemon's ending, not the child's).
        if let Some(record) = &self.record {
            record.settled(
                &status,
                self.killed.load(std::sync::atomic::Ordering::SeqCst),
            );
        }
        *self.state.lock().expect("task slot") = status;
        self.settled.notify_all();
    }

    /// What a settlement becomes on this slot: the kill rule, on its own so `say` can apply it before it
    /// adds the dirty-worktree line. A killed child's `Done` is replaced wholesale, and a line added first
    /// would go with it.
    pub(crate) fn as_settled(
        &self,
        status: letibot_tools::builtins::task::TaskStatus,
    ) -> letibot_tools::builtins::task::TaskStatus {
        match status {
            letibot_tools::builtins::task::TaskStatus::Done { .. }
                if self.killed.load(std::sync::atomic::Ordering::SeqCst) =>
            {
                letibot_tools::builtins::task::TaskStatus::Failed {
                    why: "stopped by `job_kill` — this is not an answer".into(),
                }
            }
            other => other,
        }
    }
}

/// **What a child's thread leaves behind when `run_to_completion` returns**: a refusal that came
/// before the child's own [`AnswerOnce`] existed becomes the slot's answer, because nothing else
/// will ever settle it. A slot already settled keeps what it has.
pub(crate) fn finish_child_thread(slot: &TaskSlot, ran: Result<String, String>) {
    if let Err(why) = ran {
        slot.settle_if_running(letibot_tools::builtins::task::TaskStatus::Failed { why });
    }
    // **A failed child's tree is debris**: the branch keeps whatever it committed, and the removal
    // keeps a tree that still holds uncommitted work, so nothing that matters goes.
    if slot.failed() {
        let d = slot.deliverable.lock().expect("task slot").clone();
        let _ = deliverable::reap_failed(d.as_ref());
    }
}

/// One child's answer, settled exactly once, whatever path the turn takes out of the call: a settlement
/// that never fires leaves a parent waiting for ever, and a second lets a later failure overwrite the real
/// answer. So it belongs to a guard, and `Drop` covers every path that did not speak.
pub(crate) struct AnswerOnce<'a> {
    slot: &'a TaskSlot,
    said: std::cell::Cell<bool>,
    /// The worktree whose branch is this child's deliverable, when it has one, read at the moment of
    /// settlement so EVERY ending the parent is handed carries the dirty-tree line at its top.
    deliverable: Option<deliverable::Deliverable>,
}

impl<'a> AnswerOnce<'a> {
    pub(crate) fn new(slot: &'a TaskSlot) -> Self {
        AnswerOnce {
            slot,
            said: std::cell::Cell::new(false),
            deliverable: None,
        }
    }

    /// The same guard for a child whose branch is its deliverable.
    pub(crate) fn with_deliverable(mut self, d: Option<deliverable::Deliverable>) -> Self {
        self.deliverable = d;
        self
    }

    /// **The first answer is the answer.** A later one is another path out of the same call, not
    /// a second opinion about how the turn went.
    pub(crate) fn say(&self, status: letibot_tools::builtins::task::TaskStatus) {
        if self.said.replace(true) {
            return;
        }
        self.slot.settle(self.with_the_line(status));
    }

    /// The settlement as the parent will read it: the kill rule first, then - for a worktree
    /// child whose tree is dirty - the line naming what its branch does not hold.
    pub(crate) fn with_the_line(
        &self,
        status: letibot_tools::builtins::task::TaskStatus,
    ) -> letibot_tools::builtins::task::TaskStatus {
        use letibot_tools::builtins::task::TaskStatus;
        let d = self.deliverable.as_ref();
        if d.is_none() {
            return status;
        }
        match self.slot.as_settled(status) {
            TaskStatus::Done { answer, child } => TaskStatus::Done {
                answer: deliverable::with_the_line(d, &answer),
                child,
            },
            TaskStatus::Failed { why } => TaskStatus::Failed {
                why: deliverable::with_the_line(d, &why),
            },
            other => other,
        }
    }
}

impl Drop for AnswerOnce<'_> {
    fn drop(&mut self) {
        if !self.said.get() {
            self.slot.settle(
                self.with_the_line(letibot_tools::builtins::task::TaskStatus::Failed {
                    why: "this subagent's turn ended without saying how it went — the record is \
                          short of an answer, not in possession of one"
                        .into(),
                }),
            );
        }
    }
}

/// A message from another session, in the words the receiver reads: one function, because two doors
/// deliver it and a session that could tell which door would be reading the machinery. A running turn
/// takes it at its next round boundary; a session between turns has one started for it.
pub fn message_from_a_session(from: &str, text: &str, mine: Option<&str>) -> String {
    match mine {
        Some(parent) if parent == from => {
            format!("message from your parent session `{from}`: {text}")
        }
        _ => format!("message from session `{from}`: {text}"),
    }
}

/// What a child does with a command that reached its queue between turns: a free function, so the
/// decision is readable and testable without a model, a hub or a socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChildCommand {
    /// Answer it: this is the operator or a head asking the child something, and a child is an agent with
    /// a session of its own, so the answer is a turn whether it lands mid-turn or between.
    Answer,
    /// **The operator answered a row's question on this child's board** - applied here, where the
    /// board lives, and the line it makes is a turn (see [`crate::operator_ask`]).
    AnswerTodo,
    /// Hear it: a parent's message with no turn to steer it into, so the child's own turn is STARTED for
    /// it. A message that is queued must be heard.
    Hear,
    /// **Leave, and stop this session's children on the way out.** See [`ChildCommand::Stop`].
    Leave(&'static str),
    /// **A stop, and a stop walks the subtree downward.** See the supervision invariant on
    /// [`HarnessTaskRunner`]: a parent owns its children's lifecycle, so the last thing this
    /// session does is stop what it started, and then it goes.
    Stop,
    /// **`/clear`, answered on the thread that owns this child's harness** - a fresh
    /// conversation for the child, with its board, jobs, children and parent kept. No turn runs.
    Clear,
    /// **`/nag` - the other slash verb a child answers itself**, because the clock it moves lives
    /// on this thread (`ChildTurns::nag`) and nowhere else. A child is a session, and the
    /// operator's pause is per session: see [`crate::nag_pause`].
    Nag,
}

pub(crate) fn child_command(kind: &letibot_sessionlog::CommandKind) -> ChildCommand {
    use letibot_sessionlog::CommandKind as K;
    match kind {
        K::Prompt { .. } => ChildCommand::Answer,
        // **The operator answered a question on this child's board** - the child's board is
        // this thread's, so the answer is applied here and the turn it makes runs here.
        K::AnswerTodo { .. } => ChildCommand::AnswerTodo,
        // An interrupt IS this child's stop, whether or not a turn is running: a running turn takes its own
        // interrupt through the steering poll, so what arrives here is a parent stopped while this child sat
        // between turns with children of its own still computing.
        K::Interrupt { .. } => ChildCommand::Stop,
        // A message is a turn when there is no turn to steer. What must not change is who is speaking:
        // answering it as a prompt would put the parent's words in the operator's mouth and let them
        // authorise, which is why this is a variant of its own rather than `Answer`.
        K::Message { .. } => ChildCommand::Hear,
        // `/clear` is a slash verb a child answers itself: a clear is about THIS conversation's history,
        // and everything it keeps lives in the harness this thread owns.
        K::Slash { line }
            if matches!(
                crate::slash::Slash::parse(line),
                crate::slash::Slash::Clear { .. }
            ) =>
        {
            ChildCommand::Clear
        }
        // **And `/nag`, for the same reason**: the clock it moves is this thread's.
        K::Slash { line }
            if matches!(
                crate::slash::Slash::parse(line),
                crate::slash::Slash::Nag(_)
            ) =>
        {
            ChildCommand::Nag
        }
        // The rest is a session's own machinery - a compaction, a re-seat, a mode, the operator's
        // door - and a child inherits all of it from its parent rather than owning a copy.
        _ => ChildCommand::Leave("a door a child does not own"),
    }
}

/// **What a subagent's head is told about a command the child does not run** - pure, for the
/// reason [`child_command`] is: what it says is asserted without a daemon.
pub(crate) fn child_leave_said(
    sub_id: &str,
    kind: &letibot_sessionlog::CommandKind,
    why: &str,
) -> String {
    use letibot_sessionlog::CommandKind as K;
    let what = match kind {
        K::OperatorShell { line, .. } => format!("`{line}`"),
        K::Slash { line } => format!("`{line}`"),
        other => format!("a `{}` command", other.verb()),
    };
    format!(
        "{what} was not run in {sub_id}: it is {why}. A subagent's compaction, mode, `!` line, \
         slash commands and todo edits are its parent's — send it to the parent session, or ask \
         this one in words."
    )
}

/// The sentence a session leaves about the children it stopped on the way down, or `None` when it had
/// none. Pure, so what a stop SAYS is asserted without a daemon, and one function because three callers
/// write it. A refusal is named, not counted away.
pub(crate) fn children_stopped_notice(
    results: &[(String, Result<String, String>)],
) -> Option<String> {
    if results.is_empty() {
        return None;
    }
    let mut stopped = Vec::new();
    let mut refused = Vec::new();
    for (handle, said) in results {
        match said {
            Ok(_) => stopped.push(handle.clone()),
            Err(why) => refused.push(format!("`{handle}` — {why}")),
        }
    }
    let mut out = String::new();
    if !stopped.is_empty() {
        out.push_str(&format!(
            "stopping this session stopped the {} subagent(s) it was still running, before \
             stopping itself: {}. They stop their own children in turn, so the whole subtree \
             goes; `task_result` on any of them says it was stopped.",
            stopped.len(),
            stopped.join(", ")
        ));
    }
    if !refused.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!(
            "{} subagent(s) could NOT be stopped and are still running:\n  {}",
            refused.len(),
            refused.join("\n  ")
        ));
    }
    Some(out)
}

/// Stop every child this session owns, and say so on its own log: the downward edge of a supervision
/// tree, one function for the three doors a stop arrives through, so a stop cannot mean different things
/// at different doors. It publishes on this session's hub, where the reader looking at it will see.
pub(crate) fn stop_children_first(
    runner: Option<&Arc<dyn letibot_tools::builtins::task::TaskRunner>>,
    hub: &Hub,
) -> Option<String> {
    let runner = runner?;
    let said = children_stopped_notice(&runner.stop_all())?;
    hub.publish(letibot_sessionlog::SessionEvent::Warning {
        code: "subagents_stopped".into(),
        detail: said.clone(),
        compaction: None,
    });
    Some(said)
}

/// A child that ends while its own children are still on their task stops them; `None`, and nothing is
/// stopped, when none is. A child that answers holds for its children, but one whose turn FAILS has no
/// answer to hold, and its children used to run on with nobody left to report to.
pub(crate) fn a_child_that_ends_stops_its_own(
    runner: Option<&Arc<dyn letibot_tools::builtins::task::TaskRunner>>,
    hub: &Hub,
) -> Option<String> {
    if runner?.unsettled_children() == 0 {
        return None;
    }
    stop_children_first(runner, hub)
}

/// A child's own turns after the task it was spawned for, and the door its parent's stop comes through.
/// The door is the one the first prompt came through, since a child's turn IS that call, and the loop
/// also holds the idle plan-check, on the one thread that may drive the harness.
pub(crate) fn serve_child(
    sub: &mut Harness,
    hub: &Hub,
    sub_id: &str,
    mut turns: ChildTurns,
) -> ChildExit {
    serve_child_with(sub, hub, sub_id, &mut turns, false)
}

/// The one exit every subagent thread takes (R68.2): closes the child's hub and removes the session from
/// the registry however `run_to_completion` leaves. A `Drop` rather than a call at each return, because
/// the exits are `?`s and early returns and the defect this replaces was an exit that forgot.
pub(crate) struct ChildSessionGuard<'a> {
    registry: &'a Arc<letibot_sessionlog::registry::Registry>,
    id: &'a str,
}

impl Drop for ChildSessionGuard<'_> {
    fn drop(&mut self) {
        self.registry.remove(self.id);
    }
}

/// A spawned session, announced before it exists. Held by the child's own thread, so the announcement
/// ends where the spawn does: an open that succeeded spent it already, and one that failed, or a thread
/// that panicked, withdraws it here, which is the shape `ChildSessionGuard` exists for.
pub(crate) struct SpawnAnnounced {
    registry: Arc<letibot_sessionlog::registry::Registry>,
    id: String,
}

impl Drop for SpawnAnnounced {
    fn drop(&mut self) {
        self.registry.spawn_settled(&self.id);
    }
}

/// **Why a child's serving loop ended** - the three exits of [`serve_child_under`], told apart
/// because one of them is not an end: a PARKED child is in the store and comes back on the next
/// prompt (`Config::child_park_after`), while a stopped or closed one is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildExit {
    /// The hub closed: the daemon is going away, or the session was removed.
    Closed,
    /// A stop reached it between turns; its children were stopped first.
    Stopped,
    /// **Idle past the park window with nothing alive under it** - parked as store-only. See
    /// `Config::child_park_after` for the policy and what reopens it.
    Parked,
    /// **Held for its children, and none is left unsettled** - the hold's own end
    /// (`serve_child_with`'s `hold`): the caller now gives the child's newest answer.
    Settled,
}

/// [`serve_child`] with the clock handed in - the tests' door, and the only difference between
/// a child this daemon parks and one a test parks: the WINDOW is injected so the schedule can
/// be driven without sleeping through `TODO_NAG_AFTER`, and nothing else varies.
pub fn serve_child_under(
    sub: &mut Harness,
    hub: &Hub,
    sub_id: &str,
    nag: crate::sessions::TodoNagClock,
) -> ChildExit {
    let mut turns = ChildTurns::new(sub, sub_id, nag);
    serve_child_with(sub, hub, sub_id, &mut turns, false)
}

/// A subagent's own half of the shared turn tail (R68.4): the `TurnOwner` for a session served on its own
/// thread, holding the clocks that thread keeps. What a child's second copy of that sequence was missing
/// is what this supplies: its title, a monitor waiter, the abandoned-call sweep and the failure's reason.
pub struct ChildTurns {
    registry: Arc<letibot_sessionlog::registry::Registry>,
    id: String,
    /// The idle plan-check - the same [`crate::sessions::TodoNagClock`] the daemon keeps for its
    /// own sessions, kept here because this thread is the only one that may drive the harness.
    nag: crate::sessions::TodoNagClock,
    /// The board's version is how this loop sees a write that ran no turn: a parent's `todo_write
    /// target=` moves the board from the parent's thread and wakes this reader, and only one of that and
    /// a wake that found nothing should re-arm the clock.
    board_version: u64,
    /// When this child's abandoned-call sweep is due - the child's own copy of the daemon's
    /// `sweep_at`, armed by every turn and run on this thread when it comes due.
    sweep_at: Option<std::time::Instant>,
    /// Whether this child's monitor waiter is running - one per session, `Sessions::armed`'s
    /// rule.
    wake_armed: bool,
    /// The child's row in the store, which every turn AFTER its first moves back to `running` for its
    /// length, so a restart in the middle of a parent's message turn resumes the child instead of reading
    /// its first answer as its state. `None` for a child with no record.
    record: Option<crate::subagent_record::SubagentRecord>,
    /// **The parent's slot for THIS child.** A later turn settles it again and hands the settlement
    /// up, because the slot was the whole of what a parent was told: it settled once, at the task
    /// answer, and every turn after it ended in silence.
    slot: Option<Arc<TaskSlot>>,
}

impl ChildTurns {
    pub fn new(sub: &Harness, sub_id: &str, nag: crate::sessions::TodoNagClock) -> ChildTurns {
        ChildTurns {
            registry: sub.session_registry.clone(),
            id: sub_id.to_string(),
            nag,
            // Read BEFORE the rows it describes, and that order is load-bearing: a parent's write landing
            // between the two orders fails differently. Rows-then-version would arm from the old rows and
            // find the version already moved, losing the nag for good; the version is the wider read.
            board_version: sub.todo_board_version(),
            sweep_at: None,
            wake_armed: false,
            record: None,
            slot: None,
        }
    }

    /// The slot the parent's runner holds for this child - see the field.
    fn with_slot(mut self, slot: &Arc<TaskSlot>) -> Self {
        self.slot = Some(slot.clone());
        self
    }

    /// The child's record, for its later turns to move - see the field.
    fn with_record(mut self, record: Option<crate::subagent_record::SubagentRecord>) -> Self {
        self.record = record;
        self
    }

    /// Re-arm the idle plan-check from NOW, when a turn ran or the board moved: the same two anchors
    /// `Sessions` has, since a parent's `todo_write target=` bumps the version here. The children and the
    /// jobs are taken fresh, and the operator's pause is read on every call so it can never be older.
    fn rearm(&mut self, sub: &Harness, turned: bool) {
        let v = sub.todo_board_version();
        let moved = v != self.board_version;
        self.board_version = v;
        self.nag.hold(sub.nag_hold());
        // **The queue's census too, read the same way and for the same reason** - and empty for a
        // child, because a child is not the project of its repository (`Harness::stuck_census`),
        // so a subagent's clock speaks about its plan and nothing else.
        self.nag.queue(sub.queue_news());
        if turned || moved {
            let rows = sub.todo_list();
            let jobs = sub.running_jobs(&rows);
            self.nag.rearm(
                &rows,
                std::time::Instant::now(),
                &sub.child_sessions(),
                &jobs,
            );
        }
    }
}

impl crate::turn_tail::TurnOwner for ChildTurns {
    fn arm_sweep(&mut self) {
        let at = std::time::Instant::now() + crate::sessions::SWEEP_AFTER;
        self.sweep_at = Some(self.sweep_at.map_or(at, |prev| prev.min(at)));
    }

    /// **The parent or a head has spoken: the check is earned back**  -
    /// `Sessions::note_operator_prompt`'s rule, for the session whose operator is whoever
    /// prompts it.
    fn spoken_to(&mut self, _h: &mut Harness) {
        self.nag.prompt_arrived();
    }

    fn title(&mut self, _h: &mut Harness, title: String) {
        self.registry.set_title(&self.id, title);
    }

    fn arm_wake(&mut self, h: &mut Harness) {
        if self.wake_armed {
            return;
        }
        match crate::turn_tail::spawn_monitor_wake(
            &self.registry,
            h,
            &self.id,
            std::time::Duration::from_secs(5),
        ) {
            Ok(armed) => self.wake_armed = armed,
            Err(e) => {
                h.hub().publish(letibot_sessionlog::SessionEvent::Warning {
                    code: "monitor_wake_not_armed".into(),
                    detail: format!(
                        "this session's monitors will be POLLED, not woken: the waiter thread \
                         could not be started ({e})."
                    ),
                    compaction: None,
                });
            }
        }
    }

    fn turn_ended(&mut self, h: &mut Harness, door: &crate::turn_tail::Door) {
        // **Said even when the turn failed** - the metronome rule: a check the model never
        // answered must not come back every window, it is the same notice and the plan has not
        // moved. Recorded BEFORE the re-arm, which reads it.
        match door {
            crate::turn_tail::Door::Nag(notice) => {
                self.nag.queue(h.queue_news());
                self.nag.said(notice.clone(), std::time::Instant::now())
            }
            // A child is never told a census (`Harness::stuck_census` answers `None` for anything
            // but the project's root), so this arm exists for the clock's sake and not the
            // child's: the door is in the tail's vocabulary and the owner answers every door.
            crate::turn_tail::Door::Census => {
                self.nag.queue(h.queue_news());
                self.nag.census_said(std::time::Instant::now())
            }
            _ => {}
        }
        self.rearm(h, true);
    }
}

/// [`serve_child`]'s loop, with the owner's state - built by the caller so a child's FIRST turn
/// (its task, run in `run_to_completion`) goes through the same owner as every later one.
pub(crate) fn serve_child_with(
    sub: &mut Harness,
    hub: &Hub,
    sub_id: &str,
    turns: &mut ChildTurns,
    hold: bool,
) -> ChildExit {
    use crate::turn_tail::Door;
    use letibot_sessionlog::hub::OwnWork;
    // **The park clock** (R68's idle-child policy, `Config::child_park_after`): restarted by
    // every piece of work this loop takes, so it measures idleness and not age. `None` is the
    // old loop - a child that waits for its hub to close, however long that is.
    let park_after = sub.config().child_park_after;
    let mut park_at = park_after.map(|d| std::time::Instant::now() + d);
    // The first turn just ended on this thread, so the clock starts from its end: the same arm the daemon
    // makes for a session it holds. A child that finished with an unfinished plan is exactly a session
    // whose last turn left work open.
    turns.rearm(sub, true);
    loop {
        // **Holding its answer for its children** (the operator: a child with live children is
        // not done): back to the caller the moment none is left unsettled, which is when its
        // newest answer is the one to give.
        if hold && sub.unsettled_children() == 0 {
            return ChildExit::Settled;
        }
        let deadline = [turns.nag.due_at(), park_at, turns.sweep_at]
            .into_iter()
            .flatten()
            .min();
        let work = hub.take_own_work_until(deadline);
        let now_after = std::time::Instant::now();
        // **The sweep, on the thread that owns the harness** - `Sessions::sweep_abandoned_calls`
        // for a session the daemon holds, here for a child, which the daemon's sweep never
        // reached. It is not activity: nothing was asked of the child.
        if matches!(work, OwnWork::TimedOut) && turns.sweep_at.is_some_and(|at| at <= now_after) {
            turns.sweep_at = None;
            sub.sweep_abandoned_calls();
            continue;
        }
        // **Is this the park clock's deadline, and may the child go?** Only when nothing else is
        // due: a plan-check that is armed is work outstanding, and a child with one is not idle.
        if matches!(work, OwnWork::TimedOut)
            && turns.nag.due_at().is_none_or(|d| d > now_after)
            && let (Some(at), Some(window)) = (park_at, park_after)
            && at <= now_after
        {
            // Nothing armed, the window spent: park unless something the harness owns, or a head
            // watching it, says the session is still in use - then look again a window later.
            if turns.nag.due_at().is_none()
                && hub.attached_heads() == 0
                && sub.park_refusal().is_none()
            {
                return ChildExit::Parked;
            }
            park_at = Some(now_after + window);
            continue;
        }
        // Anything else is activity, and the park clock starts over from it.
        park_at = park_after.map(|d| std::time::Instant::now() + d);
        let kind = match work {
            // The hub closed: the daemon is going away, or this session was reaped.
            OwnWork::Closed => return ChildExit::Closed,
            // A wake has no command behind it and needs none: its content is *something this session owns
            // has settled*, and the settlement is in its own queue. `wake` takes it and runs the turn, or
            // finds it taken at a round boundary and runs nothing, so one settlement is not delivered twice.
            OwnWork::Wake => {
                // A wake that ran nothing re-arms nothing, the same answer the worker's `wake` gives. A
                // board that MOVED under the wake does re-arm, which is the parent's write arriving.
                if later_turn(sub, turns, Door::Wake, |h| h.wake()).is_none() {
                    turns.rearm(sub, false);
                }
                continue;
            }
            // The idle plan-check, delivered on the thread that owns the harness, because the worker holds
            // no harness for this session. Disarmed before the turn as `due_now` does for the worker, since a
            // delivery that left the old deadline would fire again at once.
            OwnWork::TimedOut => {
                turns.nag.fired();
                // A paused child's deadline is the pause's end, not a check: the pause is lifted and said,
                // the clock re-armed, and no turn runs.
                if let Some(p) = sub.nag_pause() {
                    if p.over_at(letibot_sessionlog::registry::now_ms()) {
                        let _ = sub.resume_nags(crate::nag_pause::Resumed::AtItsEnd);
                    }
                    turns.rearm(sub, true);
                    continue;
                }
                // Recorded as the clock reads the plan, not as the wording sent (`nag_said_as`).
                let notice = sub.nag_said_as();
                if later_turn(sub, turns, Door::Nag(notice), |h| h.nag_turn()).is_none() {
                    turns.rearm(sub, false);
                }
                continue;
            }
            OwnWork::Command(cmd) => cmd.kind,
        };
        match child_command(&kind) {
            // The same door the first prompt took: a child is a session, and a prompt to it is a turn
            // through the shared tail like any other. The operator's answer to a row's question arrives here
            // too, on the thread that owns this board.
            ChildCommand::AnswerTodo => {
                let letibot_sessionlog::CommandKind::AnswerTodo {
                    content,
                    question,
                    answer,
                } = &kind
                else {
                    unreachable!("`child_command` answers a todo for AnswerTodo and nothing else");
                };
                match sub.answer_operator_ask(content, question, answer) {
                    Ok(line) => {
                        later_turn(sub, turns, Door::Prompt, |h| h.submit(&line).map(Some));
                    }
                    Err(why) => {
                        hub.publish(letibot_sessionlog::SessionEvent::Warning {
                            code: "todo_answer_refused".into(),
                            detail: format!(
                                "your answer on `{}` was not taken: {why}",
                                content.trim()
                            ),
                            compaction: None,
                        });
                        turns.rearm(sub, false);
                    }
                }
            }
            ChildCommand::Answer => {
                let (text, attachments) = match &kind {
                    letibot_sessionlog::CommandKind::Prompt { text, attachments } => {
                        (text.clone(), attachments.clone())
                    }
                    _ => unreachable!("`child_command` answers a Prompt and nothing else"),
                };
                later_turn(sub, turns, Door::Prompt, |h| {
                    h.submit_attached(&text, &attachments).map(Some)
                });
            }
            // A parent's message with no turn to steer it into, so one is started. The words are recorded as
            // the parent's, naming which, so they can neither authorise nor be read as the operator's; the
            // mid-turn door speaks the same text, so a session cannot tell which state it was in.
            ChildCommand::Hear => {
                let (from, text) = match &kind {
                    letibot_sessionlog::CommandKind::Message { from, text } => {
                        (from.clone(), text.clone())
                    }
                    _ => unreachable!("`child_command` hears a Message and nothing else"),
                };
                later_turn(sub, turns, Door::Message, |h| {
                    h.parents_message_turn(&from, &text).map(Some)
                });
            }
            // The children first, then this session: stopping is a tree's downward edge, and this is where a
            // child between turns receives it. It then leaves, because an interrupt that reaches a session
            // stops it, and a stopped session that stayed would be the shape this change is against.
            ChildCommand::Stop => {
                sub.stop_children();
                return ChildExit::Stopped;
            }
            // A clear is not a turn, so nothing runs after it: the fork lands, the row says so, and the child
            // waits for its next word as it was. The same two publications the daemon's arm makes, so a head
            // attached to a child reads the same boundary it reads on a root.
            ChildCommand::Clear => {
                let next = match &kind {
                    letibot_sessionlog::CommandKind::Slash { line } => {
                        match crate::slash::Slash::parse(line) {
                            crate::slash::Slash::Clear { next } => next,
                            _ => None,
                        }
                    }
                    _ => None,
                };
                let (code, detail) = match sub.clear_conversation(next.as_deref()) {
                    Ok(r) => {
                        sub.publish_cleared(&r);
                        ("slash", format!("/clear\n{}", clear_reply(&r)))
                    }
                    Err(e) => ("slash_refused", format!("/clear\n{e}")),
                };
                hub.publish(letibot_sessionlog::SessionEvent::Warning {
                    code: code.into(),
                    detail,
                    compaction: None,
                });
                // No turn ran and the board did not move - `Leave`'s reading of the clock.
                turns.rearm(sub, false);
            }
            // **`/nag` on this child**, answered here and published as the worker publishes every
            // verb's reply (`slash` / `slash_refused`, the line echoed first), then the clock
            // re-armed with whatever the verb left - the pause's end, or the plan's own check.
            ChildCommand::Nag => {
                let line = match &kind {
                    letibot_sessionlog::CommandKind::Slash { line } => line.clone(),
                    _ => unreachable!("`child_command` answers `/nag` and nothing else"),
                };
                let reply = match crate::slash::Slash::parse(&line) {
                    crate::slash::Slash::Nag(verb) => sub.nag_verb(verb),
                    _ => unreachable!("`child_command` matched this line as `/nag`"),
                };
                hub.publish(letibot_sessionlog::SessionEvent::Warning {
                    code: if reply.ok {
                        "slash".into()
                    } else {
                        "slash_refused".into()
                    },
                    detail: format!("/{line}\n{}", reply.lines.join("\n")),
                    compaction: None,
                });
                turns.rearm(sub, true);
            }
            ChildCommand::Leave(why) => {
                eprintln!(
                    "  {sub_id}: left a command in this subagent's queue — {why}. A child's \
                     machinery is its parent's, so nothing was run."
                );
                // **And the head that sent it is told** (R67 item 15).
                hub.publish(letibot_sessionlog::SessionEvent::Warning {
                    code: "child_door_refused".into(),
                    detail: child_leave_said(sub_id, &kind, why),
                    compaction: None,
                });
                // Nothing ran and nothing moved, so the clock keeps its own anchor.
                turns.rearm(sub, false);
            }
        }
    }
}

/// A turn of a child after its first: `run_turn`, with the child's record moved to `running` for its
/// length and put back when it ends, or left running when it ended because the daemon is stopping, which
/// is what the next daemon resumes.
fn later_turn(
    sub: &mut Harness,
    turns: &mut ChildTurns,
    door: crate::turn_tail::Door,
    turn: impl FnOnce(&mut Harness) -> Result<Option<Reply>, HarnessError>,
) -> Option<Result<Reply, HarnessError>> {
    let record = turns.record.clone();
    let prior = record.as_ref().and_then(|r| r.later_turn_began());
    // **And the parent hears it**: its slot for this child settled at the task answer and nothing
    // moved it again, so a second answer was invisible. Only the doors that are somebody's word:
    // the plan-check and the census are the harness's own business, not the parent's.
    let somebody_spoke = matches!(
        door,
        crate::turn_tail::Door::Prompt
            | crate::turn_tail::Door::Message
            | crate::turn_tail::Door::Wake
    );
    let out = crate::turn_tail::run_turn(sub, turns, door, turn);
    if let (Some(r), Some(prior)) = (&record, &prior) {
        r.later_turn_ended(prior);
    }
    if let Some(slot) = &turns.slot
        && somebody_spoke
        && let Some(ended) = &out
    {
        let status = match ended {
            Ok(reply) => letibot_tools::builtins::task::TaskStatus::Done {
                answer: reply.text.clone(),
                child: None,
            },
            Err(e) => letibot_tools::builtins::task::TaskStatus::Failed {
                why: e.to_string(),
            },
        };
        let (state, detail) = match &status {
            letibot_tools::builtins::task::TaskStatus::Failed { why } => {
                ("failed", crate::jobwatch::first_line(why))
            }
            other => (
                "done",
                crate::jobwatch::first_line(match other {
                    letibot_tools::builtins::task::TaskStatus::Done { answer, .. } => answer,
                    _ => "",
                }),
            ),
        };
        slot.settle(status);
        if let (Some(watch), Some(owner)) = (sub.job_watchers(), sub.config().parent_session_id.clone())
        {
            watch.settle_up(crate::jobwatch::JobCompletion {
                memory: None,
                kind: crate::jobwatch::BackgroundKind::Subagent,
                owner,
                job: turns.id.clone(),
                command: String::new(),
                slug: slot.slug.clone(),
                state: state.to_string(),
                produced: 0,
                elapsed_ms: 0,
                detail,
                arrived: std::time::SystemTime::now(),
            });
        }
    }
    out
}

/// The session's children, and the whole of the supervision edge between them. A parent owns its
/// children's lifecycle, an exit goes parent-ward and never to the root, termination walks the subtree
/// downward one level per session, and depth is a bound refused by name rather than a prohibition.
#[derive(Clone)]
pub(crate) struct HarnessTaskRunner {
    /// The shared pieces, held as `Arc` so the runner is `'static` while the parent
    /// harness still borrows them. Reassembled into a temporary [`Parts`] inside
    /// [`HarnessTaskRunner::run`] for the sub harness to borrow.
    pub(crate) vocab: Arc<Vocab>,
    pub(crate) wiring: Arc<Wiring>,
    pub(crate) mode_store: Arc<std::sync::RwLock<crate::modes::ModeStore>>,
    /// The daemon's session registry, so a subagent is a real session an operator
    /// can see and attach rather than a turn hidden inside the parent's.
    pub(crate) registry: Arc<letibot_sessionlog::registry::Registry>,
    /// The parent's configuration, already resolved (workspace, mode, title) by
    /// [`Harness::open_with_registry`] before this was built. Cloned as the base for
    /// each sub session; only the id, title and seat are overridden.
    pub(crate) base: Config,
    /// The shared subagent journal, so a spawn and its finish reach the dashboard's
    /// state file.
    pub(crate) tasks: Arc<crate::tasks::TaskJournal>,
    /// The loaded skills and LSP config, shared with the sub session's `skill`/`lsp`
    /// tools so a subagent sees the same capabilities the parent does.
    pub(crate) skills: Arc<letibot_tools::builtins::skill::SkillRegistry>,
    pub(crate) lsp: Arc<letibot_tools::builtins::lsp::LspConfig>,
    /// The daemon's board map, so a child's `Parts` carries the same one its parent's did: a grandchild's
    /// board must land in the same map the grandparent's `todo_write` resolves through, or a parent could
    /// write one generation of children and not the next.
    pub(crate) child_boards: Arc<crate::child_todos::ChildBoards>,
    /// The daemon's review desk, so a gatekeeper child's own harness registers the tool against the same
    /// desk this session opens its reviews on: that is what makes a reviewer's `review_verdict` reach the
    /// queue that asked it.
    pub(crate) reviews: Arc<letibot_tools::builtins::verdict::ReviewDesk>,
    /// The subagents this session has started, in the order it started them.
    /// Shared with every clone of this runner - the thread that runs a child
    /// holds one, and so does the tool that collects it.
    pub(crate) slots: TaskSlots,
    /// The watcher set a child of THIS session joins (R58). A slot because the runner is built before the
    /// harness has its own `job_watch`; for a root that set is the root's, and for a child it carries the
    /// tree's ROOT and the level above, so a grandchild's `Parts` inherits both without a chain walk.
    pub(crate) tree_watch: Arc<std::sync::Mutex<Option<Arc<crate::jobwatch::JobWatchers>>>>,
    /// The head's answers a child of THIS session asks through: the ask route's half of R58, and a slot for
    /// `tree_watch`'s reason. For a root it is its own head adjudicator's; a child inherits the ROOT's, and
    /// `None` is a tree with no head-wired adjudicator, which a child refuses by name.
    pub(crate) head_answers: Arc<std::sync::Mutex<Option<Arc<crate::answers::Answers>>>>,
    /// The daemon's own server window, as this session last knew it: `None` until it first switches away
    /// from local. A cell shared with the harness because a spawn may ask for `model: "local"` while the
    /// parent is on a cloud provider, and the child must plan against THAT window.
    pub(crate) local_window: Arc<std::sync::Mutex<Option<Option<u64>>>>,
    /// This session's point as it stands NOW, which is the one thing `base` cannot answer: `base` is a
    /// clone taken at open, so a `/mode` mid-session left a child coming up at the point its parent OPENED
    /// at. Written wherever the point moves; read by the spawn, which hands it to the child.
    pub(crate) point: Arc<std::sync::Mutex<letibot_tools::mode::Mode>>,
    /// Vocabularies loaded for other weights, by GGUF path: the spawn cache. A fan-out of children on one
    /// declared model would otherwise load that tokenizer once per child, and a load is seconds. Keyed by
    /// the path `/props` named, so two models never share an entry.
    pub(crate) foreign_vocabs:
        Arc<std::sync::Mutex<std::collections::HashMap<PathBuf, Arc<Vocab>>>>,
    /// The `task_start` children this session has put in a tree, by handle: the ask each was given and the
    /// placement it works in. A record rather than a second look at the child, whose session may be gone by
    /// the time the enqueue needs its brief. Keyed by handle, which is the entry's id, so one child is one row.
    pub(crate) placed: Arc<std::sync::Mutex<Vec<PlacedChild>>>,
    /// Where a finished child's entry is written: the session store, opened lazily and held as a cell, so a
    /// store is not opened for every session that never starts a `task_start` child. The cell is a `Mutex`
    /// because a `rusqlite::Connection` is `Send` and not `Sync`.
    pub(crate) merge_store: Arc<std::sync::Mutex<Option<Store>>>,
}

/// One `task_start` child, as the runner remembers it: the two things the enqueue needs and nothing else in
/// this process can answer for, the ask the child was given and where its branch is. The path is inside the
/// placement because the entry's `worktree` column and the placement's `path` are one fact.
#[derive(Debug, Clone)]
pub(crate) struct PlacedChild {
    /// The handle `task_result` collects by, and the id the entry is minted under.
    pub(crate) handle: String,
    /// The prompt the child was spawned with, verbatim.
    pub(crate) brief: String,
    /// Where the child was put: the path, the branch and the base SHA.
    pub(crate) placement: letibot_tools::builtins::task::WorktreePlacement,
}

impl PlacedChild {
    /// The record `start_worktree` keeps for a child, or `None` for one the queue has no business with. A
    /// `task_start` child put in the MAIN tree is not recorded: it works on `main`'s own branch, so there is
    /// no branch to land. A function, so the recording IS the enqueue's rule rather than a copy of it.
    pub(crate) fn of(
        handle: &str,
        brief: &str,
        placement: &letibot_tools::builtins::task::WorktreePlacement,
    ) -> Option<PlacedChild> {
        if placement.main_tree {
            return None;
        }
        Some(PlacedChild {
            handle: handle.to_string(),
            brief: brief.to_string(),
            placement: placement.clone(),
        })
    }
}

/// A subagent's model, resolved from `task`'s `model` argument, symmetrically: the child's model has
/// nothing to do with the parent's. It goes through `slash::models_choice`, the door `/models` uses, so a
/// name a person can type is one a child can run. `local_window` serves `local`; a cloud name the catalogue.
#[derive(Debug)]
pub struct SubagentModel {
    /// What the record says: `local`, or `PROVIDER/MODEL`.
    pub label: String,
    /// `None` is the daemon's own server; `Some` a cloud provider, its key already found.
    pub provider: Option<crate::config::ProviderConfig>,
    /// The window to plan compaction against, `None` when nobody knows one.
    pub window: Option<u64>,
    /// A declared local model the child runs on, instead of the daemon's own server. `provider` cannot carry
    /// one, and overloading `None` to mean either server is the conflation `ModelChoice` was widened to end.
    /// The child tokenizes with the parent's vocabulary when the weights match and the target's otherwise.
    pub local: Option<letibot_provider::keys::LocalModel>,
}

/// The child's window, or the refusal that names the knob: pure, so both halves are testable without a
/// harness. It refuses by NAME rather than by absence, so the model always sees `task` and the sentence
/// says which knob moves. Without it a child inherited the PARENT's window and never compacted.
fn child_window_refusal(label: &str, window: Option<Option<u64>>) -> Option<String> {
    match window {
        Some(Some(_)) => None,
        _ => Some(format!(
            "nothing was spawned on `{label}`: nobody can name a window for it — not this \
             daemon's own server and not the catalogue `/models` reads — and a child planned \
             against no window is not a child with room: nothing would compact it and it would \
             run to the server's own limit. Name a model that has one"
        )),
    }
}

/// Whether a child will be answered by the same model as its parent: the one condition under which the
/// parent's window is the child's. `None` on both sides is the daemon's own server. This exists so the
/// refusal is not broader than the defect, which was a window inherited from a model it was not running on.
fn runs_on_the_parents_model(
    child: &Option<crate::config::ProviderConfig>,
    parent: &Option<crate::config::ProviderConfig>,
) -> bool {
    match (child, parent) {
        (None, None) => true,
        (Some(c), Some(p)) => c.name == p.name && c.model == p.model,
        _ => false,
    }
}

/// How a child samples, as the project's `leticode.toml` says: `[roles.<seat>]` over `[roles.subagent]`
/// over the measured dense78 numbers. They REPLACE what the config carries rather than merging, or the
/// parent's greedy literal would survive under a profile naming one knob. `sampling` alone; not the window.
fn apply_spawn_parameters(
    sub_cfg: &mut Config,
    leticode: &crate::leticode_config::LeticodeConfig,
    role: &str,
) {
    sub_cfg.sampling = leticode.parameters_for(role).to_sampling();
}

/// The model a child runs on when nobody named one: its parent's. The parent's own provider configuration as
/// it stands, key and all, so a parent given its key by the card does not leave a child that cannot find it
/// again, and `local` when the parent is on this daemon's own server.
pub(crate) fn inherited_spawn_model(
    parent: &Option<crate::config::ProviderConfig>,
    local_window: Option<Option<u64>>,
) -> Result<SubagentModel, Vec<String>> {
    match parent {
        Some(pc) => provider_model(pc.clone()),
        None => subagent_model("local", local_window),
    }
}

/// See [`SubagentModel`]. `want` is `local`, a declared local model, or
/// `PROVIDER[/MODEL]`.
pub fn subagent_model(
    want: &str,
    local_window: Option<Option<u64>>,
) -> Result<SubagentModel, Vec<String>> {
    subagent_model_in(None, want, local_window)
}

/// `subagent_model` against a named `providers.toml`: the file is a parameter for `models_choice`'s reason,
/// since the declared local models are read from it and a test that cannot name one is a test of whatever
/// happens to be in the operator's config. The public entry point passes `None`, which is the real file.
pub fn subagent_model_in(
    file: Option<&std::path::Path>,
    want: &str,
    local_window: Option<Option<u64>>,
) -> Result<SubagentModel, Vec<String>> {
    let want = want.trim();
    if want == "local" {
        return Ok(SubagentModel {
            label: "local".into(),
            provider: None,
            // The daemon's own server window: the number the parent remembered on its way
            // out to a provider, or whatever `None` means - *nobody measured one* - rather
            // than the cloud model's number wearing the local child's name.
            window: local_window.flatten(),
            // `local` names a DECLARED model; bare `local` is the daemon's own server,
            // which needs no declaration and takes none.
            local: None,
        });
    }
    let (provider, model) = match want.split_once('/') {
        Some((p, m)) if !p.is_empty() && !m.is_empty() => (p, Some(m)),
        _ => (want, None),
    };
    let (pc, _notes) =
        crate::slash::models_choice(provider, model, None, file).map_err(|lines| {
            let mut out = lines;
            out.push(format!(
                "nothing was spawned on `{want}`; `local` needs no name, and a provider's \
                 name is the one `/models` lists"
            ));
            out
        })?;
    let pc = match pc {
        crate::slash::ModelChoice::Metered(pc) => pc,
        // A declared local model seats a child like any other model: `provider` stays `None`, because this IS
        // the local path, and `local` says where it points. The window comes from the block when it states one
        // and is left `None` otherwise, for the caller to measure off that server: this does no network.
        crate::slash::ModelChoice::Local(m) => {
            return Ok(SubagentModel {
                label: m.name.clone(),
                provider: None,
                window: m.profile.window,
                local: Some(m),
            });
        }
        // `local` is handled above by name, so reaching here means the store spelled a
        // local switch some other way - a resolver bug, refused rather than guessed.
        crate::slash::ModelChoice::OwnServer => {
            return Err(vec![format!(
                "`{want}` resolved to no provider and no model"
            )]);
        }
    };
    provider_model(pc)
}

/// A child seated on a provider configuration already in hand: the label and window the
/// catalogue gives it, and the configuration - key included - as it is.
pub(crate) fn provider_model(
    pc: crate::config::ProviderConfig,
) -> Result<SubagentModel, Vec<String>> {
    let preset = letibot_provider::Preset::parse(&pc.name).map_err(|e| vec![e])?;
    let cat = letibot_provider::catalogue::Catalogue::load();
    let window = preset.window(pc.model.as_deref(), &cat);
    let label = format!(
        "{}/{}",
        pc.name,
        pc.model
            .clone()
            .unwrap_or_else(|| preset.default_model(&cat))
    );
    Ok(SubagentModel {
        label,
        provider: Some(pc),
        window,
        local: None,
    })
}

/// The settings row's `mode_source` for a subagent: the row's own vocabulary, plus the clamp sentence when
/// there was one. The clamped case is why this exists, since a child silently at another point than its
/// parent is the state that made this a bug hunt.
pub(crate) fn child_mode_source(why: Option<&str>) -> String {
    const INHERITED: &str = "inherited from the parent session";
    match why {
        None => INHERITED.to_string(),
        Some(why) => format!("{INHERITED}, clamped to what this seat can carry: {why}"),
    }
}

/// The R58 depth refusal, as one pure function: `Some` refuses a `task` call past `max`, `None` is go. It
/// refuses by NAME, so the model sees the tool and learns which knob moves, and it is assessed at the CALL
/// because a seat can only say always or never. A `max` of 0 refuses every call, which is no nesting.
pub(crate) fn subagent_depth_refusal(depth: u32, max: u32) -> Option<String> {
    if depth < max {
        return None;
    }
    Some(format!(
        "this session is at subagent depth {depth} and `--max-subagent-depth` is {max}, so \
         `task` here would open depth {}. Nothing was started. Raise \
         `--max-subagent-depth` (or start the run with it higher) if the tree should go \
         deeper.",
        depth + 1,
    ))
}

/// **The git repositories under `root`**, at most `depth` directories down, for a refusal to
/// name: a session a level above its repositories is told which ones it can pick. A directory
/// holding `.git` (a directory or a worktree's file) is one, and is not descended into.
pub(crate) fn repos_under(root: &std::path::Path, depth: usize) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(root) else {
        return out;
    };
    let mut dirs: Vec<std::path::PathBuf> = rd
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| !n.starts_with('.') && n != "target" && n != "node_modules")
        })
        .collect();
    dirs.sort();
    for d in dirs {
        if d.join(".git").exists() {
            out.push(d);
        } else if depth > 1 {
            out.extend(repos_under(&d, depth - 1));
        }
        if out.len() >= 20 {
            break;
        }
    }
    out
}

/// The repositories a refusal names, relative to the session root: ` Repositories here: `a`,
/// `b/c`.` - or nothing when there are none.
pub(crate) fn name_repos(root: &str, repos: &[std::path::PathBuf]) -> String {
    if repos.is_empty() {
        return String::new();
    }
    let names: Vec<String> = repos
        .iter()
        .map(|p| {
            let rel = p.strip_prefix(root).unwrap_or(p);
            format!("`{}`", rel.display())
        })
        .collect();
    format!(" Repositories here: {}.", names.join(", "))
}

/// Run a git command in a directory and return its stdout, trimmed: the one door `task_start`'s filesystem
/// work goes through, because the tool layer has no git and no exec. `Err` carries the command and its
/// stderr, so a refusal names what was tried and what git said.
pub(crate) fn git_in(dir: &str, args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("git {} could not be run: {e}", args.join(" ")))?;
    if !out.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// A subagent's session id, minted so two children cannot share one: a millisecond clock is not an id
/// source, since three calls in a round land in the same millisecond and `task_result` finds only the first
/// slot under a handle. Nanoseconds plus two guards: the registry, and `handed_out` for a child still opening.
pub(crate) fn mint_sub_id(
    registry: &letibot_sessionlog::registry::Registry,
    parent: &str,
    handed_out: impl Fn(&str) -> bool,
) -> String {
    mint_sub_id_at(registry, parent, crate::config::now_ns(), handed_out)
}

/// [`mint_sub_id`] with the clock stated rather than read, so the collision can be driven in a
/// test without racing a nanosecond.
pub(crate) fn mint_sub_id_at(
    registry: &letibot_sessionlog::registry::Registry,
    parent: &str,
    ns: u128,
    handed_out: impl Fn(&str) -> bool,
) -> String {
    let mut id = format!("{parent}-sub-{ns}");
    let mut n = 1u32;
    // **Announced as well as held**: a spawn that has been handed its handle and has not opened
    // yet owns this id, and minting it again would give two children one name
    // ([`Registry::announce_spawn`]).
    while registry.get(&id).is_some() || registry.opening(&id) || handed_out(&id) {
        n += 1;
        id = format!("{parent}-sub-{ns}-{n}");
    }
    id
}

/// The target's own tokenizer and dialect, for a child seated on other weights: built by the spawn path from
/// the same decision a session switch makes, with the child's `Parts`, config and row all derived from these
/// facts so nothing about the child still names the daemon's weights.
struct ForeignWeights {
    vocab: Arc<Vocab>,
    wiring: Arc<Wiring>,
    dialect: crate::dialect::Dialect,
    gguf: PathBuf,
    media_marker: Option<String>,
}

impl HarnessTaskRunner {
    /// This session's slot for one handle, if it started one under that name; `None` is a handle this runner
    /// never minted, the same answer `collect` gives as `Unknown`. A helper rather than a fourth copy of the
    /// lookup, which `send` and `collect` both read.
    fn reopen_parked(&self, handle: &str) -> Result<Arc<Hub>, String> {
        let stored = self.registry.resumable(handle).ok_or_else(|| {
            format!(
                "`{handle}` was parked (idle, its session kept in the store) and the store has no \
                 row for it now, so it cannot be reopened. Nothing was sent."
            )
        })?;
        let parent = stored
            .parent_session_id
            .clone()
            .or_else(|| Some(self.base.session_id.clone()));
        match self.registry.create_under(
            handle,
            stored.title.clone(),
            stored.wiring.clone(),
            parent,
        ) {
            Ok(hub) => Ok(hub),
            Err(letibot_sessionlog::registry::CreateError::Exists(_)) => {
                self.registry.get(handle).ok_or_else(|| {
                    format!("`{handle}` was reopened and closed again at once. Nothing was sent.")
                })
            }
            Err(e) => Err(format!(
                "`{handle}` was parked and could not be reopened: {e}. Nothing was sent."
            )),
        }
    }

    fn slot(&self, handle: &str) -> Option<Arc<TaskSlot>> {
        self.slots
            .lock()
            .expect("task slots")
            .iter()
            .find(|(h, _)| h == handle)
            .map(|(_, s)| s.clone())
    }

    /// **This runner's store, for the children's records** - the same lazily opened connection
    /// the merge queue's enqueue uses (`merge_store`), so a runner holds one. `None` without a
    /// store: nothing is recorded and nothing can be brought back.
    fn run_store(&self) -> Option<crate::subagent_record::RunStore> {
        self.base
            .store
            .as_ref()
            .map(|path| crate::subagent_record::RunStore {
                path: path.clone(),
                cell: self.merge_store.clone(),
            })
    }

    /// This session's children the store names and the record does not, with the store's own window so this
    /// and `unrecorded_child` cannot disagree. The review queue's session is not asked, for the reason its
    /// children are not recorded: a reviewer is the queue's, and on this box it has 447 unrecorded.
    fn unrecorded_children(&self) -> Vec<letibot_tokencore::store::UnrecordedChild> {
        if self.base.session_id == crate::sessions::gatekeeper::session(&self.base.workspace) {
            return Vec::new();
        }
        let Some(store) = self.run_store() else {
            return Vec::new();
        };
        let me = self.base.session_id.clone();
        store
            .with(|s| s.children_without_runs(&me).map_err(|e| e.to_string()))
            .unwrap_or_default()
    }

    /// [`Self::unrecorded_children`] for one handle - the same gatekeeper rule and the same store,
    /// and the same window (the store's own): the keyed read answers for a child from before the
    /// store recorded any, and says which class it is in rather than dropping it.
    fn unrecorded_child(&self, handle: &str) -> Option<letibot_tokencore::store::UnrecordedChild> {
        if self.base.session_id == crate::sessions::gatekeeper::session(&self.base.workspace) {
            return None;
        }
        let store = self.run_store()?;
        let me = self.base.session_id.clone();
        store
            .with(|s| s.child_without_run(&me, handle).map_err(|e| e.to_string()))
            .ok()
            .flatten()
    }

    /// Write a fresh child's row (`running`, owned by this daemon) and hand back the record its slot moves.
    /// The review queue's reviewers are not recorded, since a resumed reviewer beside the one the queue starts
    /// would be two verdicts on one branch. A row that cannot be written is said, and the child starts anyway.
    fn record_start(
        &self,
        sub_id: &str,
        prompt: &str,
        spec: &letibot_tools::builtins::task::TaskSpec,
    ) -> Option<crate::subagent_record::SubagentRecord> {
        if self.base.session_id == crate::sessions::gatekeeper::session(&self.base.workspace) {
            return None;
        }
        let store = self.run_store()?;
        let now = letibot_sessionlog::registry::now_ms();
        let run = letibot_tokencore::store::SubagentRun {
            child_id: sub_id.to_string(),
            parent_id: self.base.session_id.clone(),
            state: "running".into(),
            prompt: prompt.to_string(),
            spec_json: crate::subagent_record::RecordedSpec::of(spec).to_json(),
            instance: self.registry.instance().to_string(),
            resumes: 0,
            outcome: None,
            delivered: false,
            started_ms: now,
            updated_ms: now,
        };
        if let Err(why) = store.with(|s| s.put_subagent_run(&run).map_err(|e| e.to_string())) {
            eprintln!(
                "  {sub_id} · not recorded ({why}); it runs, and a daemon restart will not bring \
                 it back"
            );
            // The row is kept rather than dropped: a busy store is a moment and not a verdict. The child's own
            // settlement, kill or later turn writes this row, so a spawn that lost the write still leaves a
            // record, and a child that never gets that far is still named by its session.
            return Some(crate::subagent_record::SubagentRecord {
                store,
                child: sub_id.to_string(),
                registry: self.registry.clone(),
                pending: Some(run),
            });
        }
        Some(crate::subagent_record::SubagentRecord {
            store,
            child: sub_id.to_string(),
            registry: self.registry.clone(),
            pending: None,
        })
    }

    /// The child's thread, for a fresh child and a resumed one alike: the slot, the thread that runs
    /// `run_to_completion`, and the refusal when it cannot start. `restart` is `Some` for a child brought back
    /// after a daemon restart, and is the notice its continued turn opens with.
    fn spawn_child(
        &self,
        sub_id: String,
        prompt: &str,
        spec: &letibot_tools::builtins::task::TaskSpec,
        record: Option<crate::subagent_record::SubagentRecord>,
        restart: Option<String>,
    ) -> Result<String, String> {
        // **The child's name** - the spec's (a fresh spawn's, made unique by `start`), else the one
        // its record would give it (`recorded_slug`'s rule: the worktree branch, then the prompt).
        let slug = spec
            .slug
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| legacy_slug(spec, prompt));
        self.registry.set_slug(&sub_id, &slug);
        let slot = Arc::new(TaskSlot {
            slug: Some(slug),
            ..TaskSlot::recorded(&self.base.session_id, record)
        });
        self.slots
            .lock()
            .expect("task slots")
            .push((sub_id.clone(), slot.clone()));

        let me = self.clone();
        let prompt = prompt.to_string();
        // The child EXISTS from the moment its handle does, and the registry has to say so: a handle is
        // minted and returned before the child's thread opens its session, so a reader in between was answered
        // *no session by that id* about a child that was still booting. Announced before the thread is made.
        self.registry.announce_spawn(&sub_id);
        let announced = SpawnAnnounced {
            registry: self.registry.clone(),
            id: sub_id.clone(),
        };
        // The whole spec, carried through verbatim: the worktree `task_start` arranged, which is the child's
        // workspace and not the parent's tree, and the tree a VM placement copies, which is the caller's word
        // about where the work is and must not be invented here.
        let spec = spec.clone();
        let id = sub_id.clone();
        let spawned = std::thread::Builder::new()
            .name(format!(
                "subagent-{}",
                letibot_sessionlog::registry::short_id(&sub_id)
            ))
            .spawn(move || {
                // Held for this thread's whole life; see the type's doc.
                let _announced = announced;
                let slot2 = slot.clone();
                // The settlement is the function's own now: it happens as the turn ends, because what follows
                // (a host child serving its queue until the hub closes) does not end until the daemon does. The
                // exception is a refusal that returned before the guard existed, whose sentence was dropped.
                let ran = me.run_to_completion(
                    &id,
                    &prompt,
                    &spec,
                    &slot,
                    &mut |n| slot2.note(n),
                    restart.as_deref(),
                );
                finish_child_thread(&slot, ran);
                // **And this is where the child's thread ends - the fact stopping needs.** See
                // [`TaskSlot::exited`]: a child that answered is still alive (it parks and can be
                // asked more), so `stop_all` cannot read "has an answer" as "is gone".
                slot.mark_exited();
            });
        if let Err(e) = spawned {
            // Nothing is running, so say so rather than handing back a handle for a child that was never
            // started. The announcement went with the failed spawn, since a session nothing will ever open is
            // not one that is opening.
            self.slots
                .lock()
                .expect("task slots")
                .retain(|(h, _)| h != &sub_id);
            return Err(format!("the subagent thread could not be started: {e}"));
        }
        Ok(sub_id)
    }

    /// The children a daemon that is gone left behind, brought back: each record is acted on by
    /// `subagent_record::decide`. Mid-task is spawned again under its own id, settled but undelivered is a
    /// slot the parent's watcher delivers, and anything else is marked `abandoned` with its reason.
    fn resume_run(
        &self,
        store: &crate::subagent_record::RunStore,
        run: &letibot_tokencore::store::SubagentRun,
        mut spec: letibot_tools::builtins::task::TaskSpec,
    ) -> Result<(), String> {
        // **A record from before names comes back under the name its listing gives it** - unique
        // among its parent's children (`recorded_slugs`), not the bare derivation two siblings
        // may share.
        if spec.slug.is_none() {
            let runs = store
                .with(|s| {
                    s.subagent_runs_of(&run.parent_id)
                        .map_err(|e| e.to_string())
                })
                .unwrap_or_default();
            spec.slug = runs
                .iter()
                .zip(crate::subagent_record::recorded_slugs(&runs))
                .find(|(r, _)| r.child_id == run.child_id)
                .map(|(_, slug)| slug);
        }
        let mut row = run.clone();
        row.state = "running".into();
        row.instance = self.registry.instance().to_string();
        row.resumes = run.resumes + 1;
        row.outcome = None;
        store.with(|s| s.put_subagent_run(&row).map_err(|e| e.to_string()))?;
        // A slot left by an earlier life of this handle in this daemon (an exited thread) is
        // replaced, so `task_result` reads the child that is running now.
        self.slots
            .lock()
            .expect("task slots")
            .retain(|(h, s)| h != &run.child_id || !s.has_exited());
        let record = crate::subagent_record::SubagentRecord {
            store: store.clone(),
            child: run.child_id.clone(),
            registry: self.registry.clone(),
            pending: None,
        };
        let notice = crate::subagent_record::child_notice(letibot_sessionlog::registry::now_ms());
        match self.spawn_child(
            run.child_id.clone(),
            &run.prompt,
            &spec,
            Some(record),
            Some(notice),
        ) {
            Ok(_) => Ok(()),
            Err(why) => {
                let mut row = row;
                row.state = "abandoned".into();
                row.outcome = Some(why.clone());
                let _ = store.with(|s| s.put_subagent_run(&row).map_err(|e| e.to_string()));
                Err(why)
            }
        }
    }

    fn bring_back_recorded(
        &self,
        mode: letibot_tools::builtins::task::RestoreMode,
    ) -> Vec<letibot_tools::builtins::task::RestartedChild> {
        use crate::subagent_record::{Decision, decide};
        use letibot_tools::builtins::task::{RestartFate, RestartedChild};
        let Some(store) = self.run_store() else {
            return Vec::new();
        };
        let runs = match store.with(|s| {
            s.subagent_runs_of(&self.base.session_id)
                .map_err(|e| e.to_string())
        }) {
            Ok(runs) => runs,
            Err(why) => {
                eprintln!(
                    "  {} · its subagents' records could not be read ({why}); none were resumed",
                    self.base.session_id
                );
                return Vec::new();
            }
        };
        let me = self.registry.instance().to_string();
        let mut out = Vec::new();
        for run in runs {
            let handle = run.child_id.clone();
            let held = self.slot(&handle).is_some() || self.registry.get(&handle).is_some();
            let title = subagent_title(&run.prompt);
            let restamp = |state: &str, resumes: u32, outcome: Option<String>| {
                let mut row = run.clone();
                row.state = state.to_string();
                row.instance = me.clone();
                row.resumes = resumes;
                row.outcome = outcome;
                if let Err(why) =
                    store.with(|s| s.put_subagent_run(&row).map_err(|e| e.to_string()))
                {
                    eprintln!("  {handle} · its record could not be updated ({why})");
                }
            };
            match decide(&run, &me, held, |p| std::path::Path::new(p).is_dir(), mode) {
                Decision::Leave => {}
                Decision::Deliver(status) => {
                    restamp(&run.state, run.resumes, run.outcome.clone());
                    let slot = Arc::new(TaskSlot::new(&self.base.session_id));
                    slot.settle(status);
                    slot.mark_exited();
                    self.slots
                        .lock()
                        .expect("task slots")
                        .push((handle.clone(), slot));
                    out.push(RestartedChild {
                        handle,
                        title,
                        fate: RestartFate::Settled,
                    });
                }
                // A tool's helper caught mid-task: closed, and said to nobody.
                Decision::Retire { why } => {
                    restamp("abandoned", run.resumes, Some(why));
                }
                Decision::Abandon { why } => {
                    restamp("abandoned", run.resumes, Some(why.clone()));
                    out.push(RestartedChild {
                        handle,
                        title,
                        fate: RestartFate::NotResumed { why },
                    });
                }
                Decision::Resume { spec } => {
                    let fate = match self.resume_run(&store, &run, spec) {
                        Ok(()) => RestartFate::Resumed,
                        Err(why) => RestartFate::NotResumed { why },
                    };
                    out.push(RestartedChild {
                        handle,
                        title,
                        fate,
                    });
                }
                Decision::Remind { spec } => {
                    restamp("paused", run.resumes, None);
                    out.push(RestartedChild {
                        handle,
                        title,
                        fate: RestartFate::Reminded {
                            worktree: spec.worktree.as_ref().map(|w| w.path.clone()),
                            branch: spec.worktree.as_ref().map(|w| w.branch.clone()),
                        },
                    });
                }
            }
        }
        out
    }

    /// Load and seat another model's tokenizer for a child: the spawn half of `LocalSwitch::OtherWeights`,
    /// cached by GGUF path so five children on one model load it once. The dialect is resolved against the
    /// vocabulary HERE, because a pairing that does not fit is a fact about the request.
    fn foreign_weights(
        &self,
        gguf: &std::path::Path,
        dialect: crate::dialect::Dialect,
        props: &letibot_turn::serving::ServedProps,
        want_at: &Endpoint,
    ) -> Result<ForeignWeights, String> {
        if !gguf.is_file() {
            return Err(format!(
                "{} serves weights at `{}` (its /props answer), and that is not a \
                 file this box can read. A child tokenizes here and sends ids, so it \
                 needs those weights' GGUF on THIS box — mount or link it at that path \
                 (the first shard of a split model is enough) and spawn again.",
                want_at.authority(),
                gguf.display()
            ));
        }
        let vocab = {
            let mut cache = self
                .foreign_vocabs
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(v) = cache.get(gguf) {
                v.clone()
            } else {
                let v = Arc::new(load_gguf(gguf).map_err(|e| {
                    format!(
                        "loading the vocabulary at `{}`, which {}'s /props named: {e}. \
                         A child tokenizes here and sends ids, so it needs that file on \
                         this box.",
                        gguf.display(),
                        want_at.authority()
                    )
                })?);
                cache.insert(gguf.to_path_buf(), v.clone());
                v
            }
        };
        let wiring = Arc::new(dialect.wiring(self.base.effort.as_deref()));
        // The fit check, with this method's own words for the same reason `start`
        // answers inline. The child's open would run it too; this way the refusal
        // is a spawn answer rather than a child that failed at birth.
        TurnEngine::new(
            vocab.clone(),
            wiring.renderer.clone(),
            wiring.parser.clone(),
            want_at.clone(),
            letibot_backend::BackendCaps::OWN_SERVER,
            "fit-probe",
            serde_json::Value::Null,
        )
        .map_err(|e| {
            format!(
                "the {} dialect does not fit the vocabulary at `{}`: {e}. Every control \
                 token and stop literal must resolve to exactly one vocab entry — a \
                 mismatch is a dialect/weights pairing error, refused now because at \
                 runtime it is silent.",
                dialect.name(),
                gguf.display()
            )
        })?;
        Ok(ForeignWeights {
            vocab,
            wiring,
            dialect,
            gguf: gguf.to_path_buf(),
            media_marker: props.media_marker.clone(),
        })
    }
}

impl letibot_tools::builtins::task::TaskRunner for HarnessTaskRunner {
    /// Start the child and come back: the body runs on its own thread, because a `task` that waited for its
    /// child held every call behind it for the child's whole life, which read as a hang. What is NOT deferred
    /// is a failure to start, since a bad role or an unparseable spec is a fact about the request.
    fn start(
        &self,
        prompt: &str,
        spec: &letibot_tools::builtins::task::TaskSpec,
    ) -> Result<String, String> {
        // The depth cap, refused by NAME rather than by absence (R58): `task` stays seated so the model sees
        // the tool and learns why it cannot use it, since stripping it at the limit is the workaround this tree
        // has paid for once. `depth` is this session's level, so this refuses the spawn that would pass it.
        if let Some(why) = subagent_depth_refusal(self.base.depth, self.base.max_subagent_depth) {
            return Err(why);
        }
        // **The id is minted here rather than inside the body** - it is what this call returns
        // and the body has not run yet - **and it is minted UNIQUELY, which is the half that
        // was missing.** See [`mint_sub_id`] for the three-children-one-id defect.
        let sub_id = mint_sub_id(&self.registry, &self.base.session_id, |id| {
            self.slots
                .lock()
                .expect("task slots")
                .iter()
                .any(|(h, _)| h == id)
        });
        // The seat is checked NOW: a role this build does not know is a fact
        // about the call, and answering it from a thread would report "started"
        // for something that never could.
        let role = spec.role.as_str();
        if !(role.is_empty() || role == "coder") {
            Seat::parse(role)?;
        }
        // Recorded before the thread exists, so a daemon that dies while this child is opening knows it had
        // one. And the child's name, unique among this session's children: the caller's slug, else the
        // derivation, suffixed when a sibling answers to it. A worktree child keeps its branch's name.
        let mut spec = spec.clone();
        let own_branch = spec.worktree.as_ref().is_some_and(|w| !w.main_tree);
        if own_branch {
            if spec.slug.is_none() {
                spec.slug = Some(legacy_slug(&spec, prompt));
            }
        } else {
            let taken: Vec<String> = self
                .children()
                .into_iter()
                .filter(|c| c.own)
                .filter_map(|c| c.slug)
                .collect();
            spec.slug = Some(letibot_tools::builtins::task::child_slug(
                spec.slug.as_deref(),
                prompt,
                |s| taken.iter().any(|t| t == s),
            ));
        }
        let record = self.record_start(&sub_id, prompt, &spec);
        self.spawn_child(sub_id, prompt, &spec, record, None)
    }

    /// **Bring back the children a daemon that is gone left mid-task** - see
    /// `crate::subagent_record` and [`HarnessTaskRunner::bring_back_recorded`].
    fn resume_recorded(
        &self,
        mode: letibot_tools::builtins::task::RestoreMode,
    ) -> Vec<letibot_tools::builtins::task::RestartedChild> {
        self.bring_back_recorded(mode)
    }

    /// **`task_resume`: bring back ONE child a daemon that is gone left unfinished** - and the
    /// door the automatic restore goes through too ([`HarnessTaskRunner::resume_run`]). The
    /// rule for what may come back is `crate::subagent_record::resumable`.
    fn resume(&self, handle: &str) -> Result<String, String> {
        let Some(store) = self.run_store() else {
            return Err(format!(
                "this session has no store, so it holds no record of `{handle}`. Nothing was \
                 spawned."
            ));
        };
        let run = store
            .with(|s| s.subagent_run(handle).map_err(|e| e.to_string()))?
            .filter(|r| r.parent_id == self.base.session_id)
            .ok_or_else(|| match self.unrecorded_child(handle) {
                // A child whose record was never written cannot be resumed, and it says so: the spawn's own
                // description is what a resume is given, and that is the missing row. The refusal names the two
                // things still there rather than calling the child a stranger.
                Some(child) => format!(
                    "`{handle}` is one of this session's children, and there is no record of it to \
                     resume from: {}, so the subtask it was given and the placement it worked in \
                     were never written down. Nothing was spawned. Its session `{handle}` and its \
                     work in `{}` are still there — `transcript` with session=\"{handle}\" reads \
                     what it did.",
                    crate::subagent_record::no_record_cause(child.why),
                    child.workspace_root
                ),
                None => format!(
                    "this session started no subagent `{handle}` that the store remembers; \
                     `task_result` with no `task` lists the ones it did. Nothing was spawned."
                ),
            })?;
        let held = self
            .slot(handle)
            .is_some_and(|s| !s.has_exited() && !s.is_parked())
            || self.registry.get(handle).is_some();
        let spec = crate::subagent_record::resumable(&run, self.registry.instance(), held, |p| {
            std::path::Path::new(p).is_dir()
        })?;
        self.resume_run(&store, &run, spec)?;
        Ok(format!(
            "`{handle}` is resumed in its own session — the same conversation{}, role and model — \
             and its turn continues with a restart notice. Its answer reaches you on its own when \
             it finishes; `task_result` with task=\"{handle}\" shows where it has got to.",
            run_place(&run)
        ))
    }

    /// Arrange the tree, then start the child: the worktree is created BEFORE the spawn, and a path that
    /// already exists is refused by name and without spawning. The default is never the main tree, which
    /// takes the explicit argument, and the branch is never pushed because a merge queue lands it.
    fn start_worktree(
        &self,
        prompt: &str,
        spec: &letibot_tools::builtins::task::TaskSpec,
        worktree: &letibot_tools::builtins::task::WorktreeSpec,
    ) -> Result<letibot_tools::builtins::task::WorktreeHandle, String> {
        use letibot_tools::builtins::task::{
            WorktreeHandle, WorktreePlacement, slug_from_prompt, worktree_branch, worktree_path,
        };
        // **The depth cap, refused by name before any tree is arranged** - the same
        // check [`Self::start`] makes, done here first so a child that cannot be
        // spawned does not get a worktree created for it.
        if let Some(why) = subagent_depth_refusal(self.base.depth, self.base.max_subagent_depth) {
            return Err(why);
        }
        // **The directory the git work runs in**: the repository the caller named, or the
        // session root. A root that is in no repository (a session a level above its repos) is
        // refused by name with the repositories it can see, so the next call can name one.
        let root = self.base.workspace.display().to_string();
        let workspace = match worktree.repo.as_deref() {
            Some(r) if r.starts_with('/') => r.to_string(),
            Some(r) => format!("{root}/{r}"),
            None => root.clone(),
        };
        if git_in(&workspace, &["rev-parse", "--show-toplevel"]).is_err() {
            let repos = repos_under(std::path::Path::new(&root), 3);
            return Err(match &worktree.repo {
                Some(r) => format!(
                    "`{r}` is not in a git repository, so no worktree can be cut there. Nothing \
                     was arranged and nothing was spawned.{}",
                    name_repos(&root, &repos)
                ),
                None => format!(
                    "the session root `{root}` is in no git repository, so there is nothing to \
                     cut a worktree from. Name the repository with `repo`.{} Nothing was \
                     arranged and nothing was spawned.",
                    name_repos(&root, &repos)
                ),
            });
        }
        let slug = if worktree.slug.is_empty() {
            slug_from_prompt(prompt)
        } else {
            worktree.slug.clone()
        };
        let base = worktree.base.clone().unwrap_or_else(|| "HEAD".to_string());

        let placement = if worktree.main_tree {
            // **The exception**: the child works in the main checkout. No tree is
            // arranged; the placement is the main tree's own path, branch and HEAD.
            let branch = git_in(&workspace, &["rev-parse", "--abbrev-ref", "HEAD"])
                .map_err(|e| format!("the main tree's branch could not be read: {e}"))?;
            let base_sha = git_in(&workspace, &["rev-parse", "HEAD"])
                .map_err(|e| format!("the main tree's HEAD could not be read: {e}"))?;
            WorktreePlacement {
                path: workspace.clone(),
                branch,
                base_sha,
                main_tree: true,
            }
        } else {
            // **The default**: a fresh worktree, arranged before the child is spawned.
            let path = worktree_path(&workspace, &slug);
            let branch = worktree_branch(&slug);
            // The refusal of an existing path, by name and without spawning anything: the tool pre-checks it
            // and the runner re-checks it, because the runner is what creates the path and a check the creator
            // does not make is a check that can be raced.
            if std::fs::symlink_metadata(&path).is_ok() {
                return Err(format!(
                    "`{path}` already exists, so a fresh worktree cannot be created there. \
                     Nothing was arranged and nothing was spawned. Use a different `slug`, \
                     or `main_tree: true` to work in the main checkout (the exception)."
                ));
            }
            // **The base SHA, read before the branch is cut**, so the answer names the
            // commit the branch actually starts from rather than a guess.
            let base_sha = git_in(&workspace, &["rev-parse", &base])
                .map_err(|e| format!("the base `{base}` could not be resolved: {e}"))?;
            // **The worktree, created before the child is spawned.** A failure here is
            // a spawn that fails, named - never a child run in the parent's tree and
            // called placed.
            git_in(
                &workspace,
                &["worktree", "add", &path, "-b", &branch, &base],
            )
            .map_err(|e| {
                format!(
                    "the worktree could not be created at `{path}`: {e}. Nothing was \
                     spawned. The base was `{base}` and the branch would have been \
                     `{branch}`.{}",
                    leftover_hint(&workspace, &branch, &e)
                )
            })?;
            // No file is written into the tree: the build cache is shared through the child's environment. This
            // used to write `.cargo/config.toml` and hide it in a git exclude that git does not read inside a
            // worktree, so every worktree showed `?? .cargo/` and a child's `git add -A` committed it.
            WorktreePlacement {
                path,
                branch,
                base_sha,
                main_tree: false,
            }
        };

        // **The placement rides the spec into the spawn**, so the child's workspace is
        // the worktree (see `run_to_completion`), and the handle the tool returns names
        // the path, branch and base SHA the child was put in.
        let mut spec = spec.clone();
        spec.worktree = Some(placement.clone());
        // **The child is named after its worktree**: the slug the branch was cut from, which is
        // the name `task_start`'s answer and the branch both already say.
        spec.slug = Some(slug.clone());
        let handle = self.start(prompt, &spec)?;
        // And the two facts the enqueue will need are kept, now that both exist: the handle is minted by
        // `start` and the placement and the ask are in hand here, so this is the only moment at which which
        // child, which branch and what was asked can be recorded together.
        if let Some(record) = PlacedChild::of(&handle, prompt, &placement) {
            self.placed.lock().expect("placed children").push(record);
        }
        Ok(WorktreeHandle { handle, placement })
    }

    /// Stop a subagent by interrupting the turn it is running: the child spends its life inside `submit`, and
    /// the door that reaches a running turn is its own steering, addressed at the child's hub. The answer
    /// decides the return, because a tool may not report a state change the daemon refused.
    fn kill(&self, handle: &str) -> Result<String, String> {
        let slot = self
            .slots
            .lock()
            .expect("task slots")
            .iter()
            .find(|(h, _)| h == handle)
            .map(|(_, s)| s.clone());
        let Some(slot) = slot else {
            // A child this session HAS, with nothing running it: its record was never written, so there is no
            // slot and nothing to interrupt. Saying *no subagent in this session* about a child `task_result`
            // lists is the contradiction this arm removes.
            return Err(match self.unrecorded_child(handle) {
                Some(child) => format!(
                    "nothing is running `{handle}`: this daemon holds no turn of it, and {}. \
                     Nothing was stopped.",
                    crate::subagent_record::no_record_cause(child.why)
                ),
                None => format!(
                    "no subagent `{handle}` in this session; `task_result` with no argument \
                     lists the ones there are"
                ),
            });
        };
        let Some(hub) = self.registry.get(handle) else {
            return Err(format!(
                "`{handle}` has already settled, or its session is gone, so there is no \
                 turn left to interrupt. Nothing was stopped."
            ));
        };
        let f = hub.submit(
            letibot_sessionlog::hub::DAEMON_SUBMITTER,
            format!(
                "job_kill-{}",
                letibot_sessionlog::registry::short_id(handle)
            ),
            0,
            letibot_sessionlog::CommandKind::Interrupt {
                reason: "`job_kill` stopped this subagent; whatever it had written is \
                         incomplete"
                    .into(),
            },
        );
        match &f {
            letibot_sessionlog::ServerFrame::Accepted { .. } => {
                // Marked only once the daemon said yes: the flag is what makes `task_result` say *stopped* rather
                // than *answered*, so setting it for a refused interrupt would make a running child look settled.
                // The child may still settle before this line, which with the acceptance reads as the stop it is.
                slot.kill();
                Ok(format!(
                    "the daemon accepted the interrupt for `{handle}`, so its turn stops at its \
                     next boundary. A subagent is stopped by interrupting its turn, not by \
                     reaping a process, so there is no cgroup and no pid to report. \
                     `task_result` on it now says it was stopped rather than answered."
                ))
            }
            other => Err(format!(
                "`job_kill` did NOT stop `{handle}`: the daemon refused the interrupt and \
                 answered {other:?}. Nothing was stopped — do not read this as a stop, and the \
                 same spelling will reach the same answer."
            )),
        }
    }

    /// Say something to a session that is still working. The door is a session's own hub and is not
    /// parent-shaped, so `handle` may name any live session, a child or a peer. A running turn hears it at
    /// its next boundary and one between turns has a turn started. Durable first, wake second.
    fn send(&self, handle: &str, text: &str) -> Result<String, String> {
        // A session that does not exist is the refusal; a peer is not. A PARKED child is brought back
        // first, because a message is the next prompt that reopens it. A thread that has ended takes no wake,
        // and that is checked before the submit, so nothing was sent when the refusal says so.
        let live = self.registry.get(handle);
        let own_thread = live
            .as_ref()
            .is_none_or(|h| h.owner() == letibot_sessionlog::hub::Owner::OwnThread);
        if own_thread
            && self
                .slot(handle)
                .is_some_and(|s| s.has_exited() && !s.is_parked())
        {
            return Err(format!(
                "`{handle}`'s own thread has ended, so nothing would read a message queued for \
                 it: a subagent between turns is reached by starting a turn for it, and this one \
                 has no thread left to start. Nothing was sent. `task_result` reads what it \
                 answered, and a new subagent is how to say something more."
            ));
        }
        let reopened = live.is_none() && self.slot(handle).is_some_and(|s| s.is_parked());
        let hub = match reopened {
            true => Some(self.reopen_parked(handle)?),
            false => live,
        };
        // A child still opening has a slot and no session yet: a `firecode` child is registered only once its
        // VM has booted, which takes minutes. Saying *no session is live* there is what made a model take a
        // booting run for dead and start a second VM doing the same work.
        if hub.is_none() && self.slot(handle).is_some_and(|s| !s.has_exited()) {
            return Err(format!(
                "`{handle}` is still starting — its session opens once its placement is up (a \
                 VM boots and its tree is copied in first), and a message can only be queued on \
                 a session that has opened. It is not gone: nothing needs dispatching again. \
                 Nothing was sent; say it once the child is running, or wait on it."
            ));
        }
        let Some(hub) = hub else {
            return Err(format!(
                "no session `{handle}` is live in this daemon, so there is nothing to say it to: \
                 a message is delivered to a session's own queue, and a session that has closed \
                 has no queue left. Nothing was sent. A message may name any live session — a \
                 subagent this session started, or a peer — and `task_result` with no argument \
                 lists the subagents this session still holds."
            ));
        };
        // Which of the two deliveries this is, read before the submit, and it is the same reading the rest of
        // this uses: `Hub::turn_in_flight`, not the round's status flag. Reading the round's made this door say
        // *it was between turns* about a child mid-tool-call, which is two doors for one child giving two answers.
        let running = hub.turn_in_flight();
        let f = hub.submit(
            letibot_sessionlog::hub::DAEMON_SUBMITTER,
            format!(
                "task_message-{}",
                letibot_sessionlog::registry::short_id(handle)
            ),
            0,
            letibot_sessionlog::CommandKind::Message {
                from: self.base.session_id.clone(),
                text: text.to_string(),
            },
        );
        match &f {
            letibot_sessionlog::ServerFrame::Accepted { .. } => {
                // Durable first, wake second: `submit` queued the message and rang the bell, which is the wrong
                // reader for a child, so the wake is sent as well. It answers what the queue cannot: whether the
                // session is still there, and a look for a reader not parked on the condvar.
                let woken = hub.wake_its_own_reader();
                // False is not a delivery: the hub is closed, and no turn will be started for the message. A turn
                // already running could still take it at its next boundary, but nothing after it will, which is not
                // a correction a parent may rely on and not one this door claims.
                if !woken {
                    return Err(format!(
                        "`task_message` reached `{handle}`'s queue and then its session went \
                         away: the hub is closed, so no turn will be started for it. Do not read \
                         this as a correction delivered — `task_result` reads what the child \
                         answered."
                    ));
                }
                if reopened {
                    return Ok(format!(
                        "`{handle}` was told, as `{}`: it had been idle long enough to be parked \
                         (its session was kept in the store and its thread released), so it has \
                         been reopened and a turn started for the message — it hears this now, \
                         and its answer is on its own log.",
                        self.base.session_id
                    ));
                }
                if running {
                    return Ok(format!(
                        "`{handle}` was told, as `{}`: its turn is running, so it hears this at \
                         its next round boundary and keeps working.",
                        self.base.session_id
                    ));
                }
                Ok(format!(
                    "`{handle}` was told, as `{}`: it was between turns, so the message is on \
                     its queue and a turn has been started for it — it hears this now, and its \
                     answer is what `task_result` reads.",
                    self.base.session_id
                ))
            }
            // The same rule `kill` follows, and for the same reason: a tool may not report a
            // state change the daemon refused.
            other => Err(format!(
                "`task_message` did NOT reach `{handle}`: the daemon answered {other:?}. \
                 Nothing was sent."
            )),
        }
    }

    /// Where the child has got to, which is two facts: the SLOT is the record of its first turn, and the
    /// READING is its present, taken when asked. A child that answered and parked, one running a second turn
    /// and one wedged mid-call all read the same slot, so the reading is attached rather than settled in.
    fn collect(
        &self,
        handle: &str,
        timeout: std::time::Duration,
    ) -> letibot_tools::builtins::task::TaskStatus {
        use letibot_tools::builtins::task::TaskStatus;
        let slot = self.slot(handle);
        let Some(slot) = slot else {
            return TaskStatus::Unknown;
        };
        let status = {
            let g = slot.state.lock().expect("task slot");
            let (g, _) = slot
                .settled
                .wait_timeout_while(g, timeout, |s| matches!(s, TaskStatus::Running { .. }))
                .expect("task slot");
            g.clone()
        };
        let Some(child) = self.live_reading(handle, &status, &slot) else {
            return status;
        };
        match status {
            TaskStatus::Running { note, .. } => TaskStatus::Running {
                note,
                child: Some(child),
            },
            TaskStatus::Done { answer, .. } => TaskStatus::Done {
                answer,
                child: Some(child),
            },
            other => other,
        }
    }

    /// Where a child has got to, for a reader: `collect`, and for a child a restart left without a slot, what
    /// its record says. No slot is not "never started here", since `task_result` lists such a child and
    /// `task_resume` refuses it by name. The watcher keeps `collect`'s `Unknown`, because a record is not news.
    fn look(
        &self,
        handle: &str,
        timeout: std::time::Duration,
    ) -> letibot_tools::builtins::task::TaskStatus {
        if self.slot(handle).is_some() {
            return self.collect(handle, timeout);
        }
        let me = self.base.session_id.clone();
        let run = self
            .run_store()
            .and_then(|store| {
                store
                    .with(|s| s.subagent_run(handle).map_err(|e| e.to_string()))
                    .ok()
            })
            .flatten()
            .filter(|r| r.parent_id == me);
        match run {
            Some(run) => crate::subagent_record::recorded_status(&run, self.registry.instance()),
            // No record: its own session is the witness. A spawn whose row the store refused still opened a
            // session, and that session is the only thing left saying this child existed, so the sentence it
            // gets now says which of the two it is rather than calling it a stranger.
            None => match self.unrecorded_child(handle) {
                Some(child) => crate::subagent_record::a_child_with_no_record(&child),
                None => letibot_tools::builtins::task::TaskStatus::Unknown,
            },
        }
    }

    /// A child's settlement was read (`task_result` handed it over, or `digest` read its part reader), so its
    /// record is marked delivered and a restart does not announce it again. Only this session's own children
    /// are marked.
    fn consumed(&self, handle: &str) {
        let Some(store) = self.run_store() else {
            return;
        };
        let me = self.base.session_id.clone();
        if let Err(why) =
            store.with(
                |s| match s.subagent_run(handle).map_err(|e| e.to_string())? {
                    Some(run) if run.parent_id == me => {
                        s.deliver_subagent_run(handle).map_err(|e| e.to_string())
                    }
                    _ => Ok(()),
                },
            )
        {
            eprintln!(
                "  {handle} · that its answer was read was not recorded ({why}); a restart may \
                 hand it over again"
            );
        }
    }

    /// Whether this child has no record, and how far it got: asked BY HANDLE, because that is the question
    /// `task_result` has when a model names a child the LIST does not carry.
    fn lost(&self, handle: &str) -> Option<letibot_tools::builtins::task::LostChild> {
        self.unrecorded_child(handle)
            .as_ref()
            .map(crate::subagent_record::how_far)
    }

    fn started(&self) -> Vec<String> {
        self.slots
            .lock()
            .expect("task slots")
            .iter()
            .map(|(h, _)| h.clone())
            .collect()
    }

    /// The session that started this one, if any: the upward edge of the tree `children` is the downward edge
    /// of, and the answer to `task_message {"task": "parent"}`. One field, because `base` IS that session's
    /// config, so no registry lookup and no store read. `None` is the top of a tree, not an unknown.
    fn parent(&self) -> Option<String> {
        self.base.parent_session_id.clone()
    }

    /// Every child a handle or a name may resolve to: the tree's slots with each one's name and whether THIS
    /// session started it, then this session's recorded children a restart left without a slot, which
    /// `task_resume` may still be given by name. A slot with no name is named from its record.
    fn children(&self) -> Vec<letibot_tools::builtins::task::ChildName> {
        use letibot_tools::builtins::task::ChildName;
        let me = self.base.session_id.as_str();
        let mut out: Vec<ChildName> = self
            .slots
            .lock()
            .expect("task slots")
            .iter()
            .map(|(h, s)| ChildName {
                handle: h.clone(),
                slug: s.slug.clone(),
                own: s.owner == me,
                lost: None,
            })
            .collect();
        let runs = self
            .run_store()
            .and_then(|store| {
                store
                    .with(|s| s.subagent_runs_of(me).map_err(|e| e.to_string()))
                    .ok()
            })
            .unwrap_or_default();
        // **Names unique among this session's children** (`recorded_slugs`): twelve records from
        // before names all derived `you-are-reading-part`, and a name twelve children answer to
        // resolves to none of them.
        let slugs = crate::subagent_record::recorded_slugs(&runs);
        for (run, slug) in runs.into_iter().zip(slugs) {
            match out.iter_mut().find(|c| c.handle == run.child_id) {
                Some(c) if c.slug.is_none() => c.slug = Some(slug),
                Some(_) => {}
                // **A tool's helper is not listed from its record** - it was never the model's
                // child (`TaskSpec::helper`), and twelve `digest` readers listed after a restart
                // were twelve rows the model had never heard of.
                None if crate::subagent_record::is_helper(&run) => {}
                None => out.push(ChildName {
                    handle: run.child_id,
                    slug: Some(slug),
                    own: true,
                    lost: None,
                }),
            }
        }
        // And the children whose record was never written, whose spawn row the store refused: the record is
        // what this list was built from, so such a child was absent. Its own session is the witness, and it
        // says how far it got.
        for child in self.unrecorded_children() {
            if out.iter().any(|c| c.handle == child.id) {
                continue;
            }
            let lost = crate::subagent_record::how_far(&child);
            out.push(ChildName {
                handle: child.id,
                // **No name**: a child with no record was never given one that anything wrote
                // down - its session's `title` is the first line of its brief, not a slug - so it
                // answers to its handle, which is what the model was handed when it started it.
                slug: None,
                own: true,
                lost: Some(lost),
            });
        }
        out
    }

    /// The child has finished, so its branch goes to be landed, and the row needs the ask, the placement and
    /// the store, which the tool layer has none of. Two callers, one act: the child's own settlement, and
    /// `task_result` on a `Done`. A plain `task` or main-checkout child answers `NoBranch`; no store is a refusal.
    fn finished(&self, handle: &str) -> Result<letibot_tools::builtins::task::Finished, String> {
        use letibot_tools::builtins::task::Finished;
        match self.enqueue_entry(handle)? {
            Some(entry) => Ok(Finished::Queued {
                id: entry.id,
                branch: entry.branch,
            }),
            None => Ok(Finished::NoBranch {
                why: format!(
                    "`{handle}` left no branch of its own to land — it was not a `task_start` \
                     child of this session, or it worked in the main checkout"
                ),
            }),
        }
    }

    /// Stop every child this session still owns: the downward edge. A session being stopped stops its
    /// children first, so nothing computes for nobody. Which children is `stoppable_children`'s, pure; here it
    /// is the delivery, one `kill` per handle, and each child stops its own. Not a restart policy.
    fn stop_all(&self) -> Vec<(String, Result<String, String>)> {
        let mine = stoppable_children(
            &self.slots.lock().expect("task slots"),
            &self.base.session_id,
        );
        mine.into_iter()
            .map(|handle| {
                let said = self.kill(&handle);
                (handle, said)
            })
            .collect()
    }

    /// Wait until every subagent in this tree has left its own thread: the fourth edge, and the one a SIGSEGV
    /// at exit asked for. A child's thread is detached, so nothing can join it, and `exit()` finalizes a
    /// `libllama` a live child may still be in. The whole tree's slots, and the deadline is the caller's.
    fn live_children(&self) -> usize {
        stoppable_children(
            &self.slots.lock().expect("task slots"),
            &self.base.session_id,
        )
        .len()
    }

    fn unsettled_children(&self) -> usize {
        unsettled_children(
            &self.slots.lock().expect("task slots"),
            &self.base.session_id,
        )
        .len()
    }

    fn wait_for_children(&self, timeout: std::time::Duration) -> bool {
        let slots: Vec<Arc<TaskSlot>> = self
            .slots
            .lock()
            .expect("task slots")
            .iter()
            .map(|(_, s)| s.clone())
            .collect();
        let began = std::time::Instant::now();
        let mut all = true;
        for slot in slots {
            if !slot.wait_until_exited(timeout.saturating_sub(began.elapsed())) {
                all = false;
            }
        }
        all
    }
}

/// Which of a session's slots are children IT may stop: the ownership filter, on its own so it is asserted
/// without a harness. Two facts: owned by this session, since the slot list is the TREE's and stopping is the
/// other direction, and not exited, since an answered child is alive and a gone one has no reader left.
pub(crate) fn unsettled_children(slots: &[(String, Arc<TaskSlot>)], me: &str) -> Vec<String> {
    slots
        .iter()
        .filter(|(_, slot)| {
            slot.owner == me
                && !slot.has_exited()
                && matches!(
                    *slot.state.lock().unwrap_or_else(|e| e.into_inner()),
                    letibot_tools::builtins::task::TaskStatus::Running { .. }
                )
        })
        .map(|(handle, _)| handle.clone())
        .collect()
}

pub(crate) fn stoppable_children(slots: &[(String, Arc<TaskSlot>)], me: &str) -> Vec<String> {
    slots
        .iter()
        .filter(|(_, slot)| slot.owner == me && !slot.has_exited())
        .map(|(handle, _)| handle.clone())
        .collect()
}

/// Where a subagent's ask goes: the tree's ROOT, and the answers that settle it. One function, because which
/// session receives the card and which sink can settle it are one fact, and separating them posts a card on
/// a queue whose only listener cannot answer. The root is read off the watcher set, not walked for.
pub(crate) fn subagent_ask_target(
    registry: &letibot_sessionlog::registry::Registry,
    tree: Option<&crate::jobwatch::JobWatchers>,
    parent: &str,
    answers: Option<&Arc<crate::answers::Answers>>,
) -> (String, Option<crate::answers::HeadAdjudicator>) {
    let root = tree
        .map(|w| w.tree_root().to_string())
        .unwrap_or_else(|| parent.to_string());
    let head = match (registry.get(&root), answers) {
        (Some(hub), Some(answers)) => {
            Some(crate::answers::HeadAdjudicator::new(hub, answers.clone()))
        }
        _ => None,
    };
    (root, head)
}

impl HarnessTaskRunner {
    /// A runner with only the ENQUEUE half wired: the seam the end-to-end test needs, since the struct is
    /// private and its one builder needs a vocabulary, a live host and a gate while the enqueue reads four
    /// facts. `#[cfg(test)]`, so a test reaches it without widening the type.
    #[cfg(test)]
    pub(crate) fn for_enqueue(
        parts: &Parts,
        base: Config,
        registry: Arc<letibot_sessionlog::registry::Registry>,
        placed: Vec<PlacedChild>,
    ) -> Self {
        let point = base.mode;
        Self {
            vocab: parts.vocab.clone(),
            wiring: parts.wiring.clone(),
            mode_store: parts.mode_store.clone(),
            registry,
            base,
            tasks: parts.tasks.clone(),
            skills: parts.skills.clone(),
            lsp: parts.lsp.clone(),
            child_boards: parts.child_boards.clone(),
            reviews: parts.reviews.clone(),
            slots: parts.tree_slots.clone().unwrap_or_default(),
            // The three cells a SPAWN reads and an enqueue never does: a tree's watcher set,
            // the head's answers, and the daemon's server window. A test that starts no child
            // has nothing to put in them, and `None` is what a root session starts with.
            tree_watch: Arc::new(std::sync::Mutex::new(None)),
            head_answers: Arc::new(std::sync::Mutex::new(None)),
            // No spawn runs through this constructor, so no foreign vocabulary is
            // ever loaded through it: an empty cache is the honest start.
            foreign_vocabs: Default::default(),
            local_window: Arc::new(std::sync::Mutex::new(None)),
            point: Arc::new(std::sync::Mutex::new(point)),
            placed: Arc::new(std::sync::Mutex::new(placed)),
            merge_store: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// What the daemon can see about one child RIGHT NOW, or `None` when it has nothing to report: a handle
    /// it holds no session for, or a child whose thread has ended. `turn_started` is the slot's own record;
    /// `sessions::child_reading` ORs it with the hub's, so `task_result` and the plan-check cannot disagree.
    fn live_reading(
        &self,
        handle: &str,
        status: &letibot_tools::builtins::task::TaskStatus,
        slot: &TaskSlot,
    ) -> Option<letibot_tools::builtins::todo::RunningChild> {
        if slot.has_exited() {
            return None;
        }
        // No session, no reading: a child this daemon has not adopted yet is still opening, and
        // *nothing to say about it* is the honest answer rather than *nothing is happening*.
        self.registry.get(handle)?;
        let turn_started = matches!(
            status,
            letibot_tools::builtins::task::TaskStatus::Running { .. }
        );
        Some(crate::sessions::child_reading(
            &self.registry,
            handle,
            turn_started,
            letibot_sessionlog::registry::now_ms(),
        ))
    }

    /// **This session's point, as it stands now** - see the field.
    fn point(&self) -> letibot_tools::mode::Mode {
        *self.point.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The child has finished: enqueue its branch and say so where a person is looking. Infallible from the
    /// child's side, because a queue that could not be reached must not turn a finished subagent into a failed
    /// one; unprompted, because a queue nothing announces is one nobody watches. The `QueueMoved` is broadcast.
    pub(crate) fn enqueue_finished(&self, sub_id: &str) {
        match self.enqueue_entry(sub_id) {
            Ok(Some(entry)) => {
                // **The pane's move**, for every session - see the doc above for why this one is
                // broadcast and the sentence below is not.
                self.registry
                    .broadcast(letibot_sessionlog::SessionEvent::QueueMoved {
                        queue: "review".into(),
                        id: entry.id.clone(),
                        to: entry.state.as_str().into(),
                        why: entry.evidence.clone(),
                    });
                // **The person's sentence**, for this conversation only.
                if let Some(hub) = self.registry.get(&self.base.session_id) {
                    hub.publish(letibot_sessionlog::SessionEvent::Warning {
                        code: "merge_queued".into(),
                        detail: format!(
                            "`{}` is in the review queue as `{}` — a reviewer judges its head \
                             against the brief the child was given; once approved, the merge queue \
                             rebases it onto `{}`'s tip, runs the gate and lands it.",
                            entry.branch, entry.id, entry.target
                        ),
                        compaction: None,
                    });
                }
            }
            // **The ordinary case for a plain `task`, and it says nothing.** A child that works
            // in the parent's own tree leaves no branch to land, and a line per such child would
            // be noise on every `task` call there is.
            Ok(None) => {}
            Err(why) => {
                if let Some(hub) = self.registry.get(&self.base.session_id) {
                    hub.publish(letibot_sessionlog::SessionEvent::Warning {
                        code: "merge_not_queued".into(),
                        detail: format!(
                            "the branch of `{sub_id}` was NOT enqueued: {why} Its worktree \
                             and branch are untouched — nothing has been deleted — but nothing \
                             will land it until it is enqueued."
                        ),
                        compaction: None,
                    });
                }
            }
        }
    }

    /// The write, and a store that will not open is a refusal rather than a silent drop: the half of
    /// `finished` that does the work, on its own so the trait method and the enqueue's own notice can have
    /// different shapes of one answer. `Ok(None)` is no branch of its own to land, which is not a failure.
    fn enqueue_entry(
        &self,
        handle: &str,
    ) -> Result<Option<letibot_tokencore::store::ReviewRow>, String> {
        let Some(child) = self
            .placed
            .lock()
            .expect("placed children")
            .iter()
            .find(|c| c.handle == handle)
            .cloned()
        else {
            return Ok(None);
        };
        let Some(path) = self.base.store.clone() else {
            return Err(format!(
                "this daemon has no `--store`, and the queues live in the session store — so \
                 there is nowhere to put `{}` up for review. The branch is still in `{}`, and \
                 nothing is lost; a daemon started with `--store` will serve it when the child is \
                 enqueued again.",
                child.placement.branch, child.placement.path
            ));
        };
        let now_ms = (crate::config::now_ns() / 1_000_000) as u64;
        let mut cell = self.merge_store.lock().expect("merge store");
        if cell.is_none() {
            *cell = Some(
                Store::open(&path)
                    .map_err(|e| format!("the session store at {}: {e}", path.display()))?,
            );
        }
        let store = cell.as_ref().expect("just opened");
        // **The row, built by the queue's own function** - the repository the worktree is in,
        // the project's target (decision 12), and the head as it is now.
        crate::mergequeue::review::enqueue_finished(
            store,
            handle,
            &child.brief,
            &child.placement,
            &crate::mergequeue::settings::for_repository,
            now_ms,
        )
    }

    /// **Where the child's tools run, and the tree a VM copies** - the two answers a `task`
    /// gives before anything is minted or opened.
    fn child_placement(
        &self,
        spec: &letibot_tools::builtins::task::TaskSpec,
    ) -> Result<(letibot_tools::builtins::task::Placement, Option<PathBuf>), String> {
        use letibot_tools::builtins::task::Placement;
        // A VM is booted on a copy of this session's workspace by the child's own `open`, and a boot that
        // fails is a spawn that fails, named. A child of a remote session runs on the same host unless it asks
        // for another placement, since `host` in a remote session's mouth is its own boundary.
        let placement = match (spec.placement.clone(), self.base.placement.host()) {
            // A child of a session whose tools are in a guest on that host gets a guest of its
            // own there, for the reason a VM's children get their own VM.
            (Placement::Host, Some(parent_host)) => match &self.base.placement {
                Placement::RemoteVm(_) => Placement::RemoteVm(parent_host.to_string()),
                _ => Placement::Remote(parent_host.to_string()),
            },
            (asked, _) => asked,
        };
        let _ = Placement::Host;
        // A VM holds one repository, and a session above several has no one tree to copy: the model names it
        // with `repo`, and the copy is that repository or nothing.
        let vm_copy = vm_copy_for(
            placement == Placement::Firecode,
            spec.worktree.is_some(),
            spec.vm_source.as_deref(),
            &self.base.workspace,
        )?;
        Ok((placement, vm_copy))
    }

    /// **A VM child's life is its turn's**: close the backend, keep what died with it, say
    /// where the work went, and hand its branch to the queue. There is no later to answer in.
    fn finish_a_vm_child(
        &self,
        sub: &mut Harness,
        reply: Reply,
        answer: &AnswerOnce<'_>,
        sub_id: &str,
    ) -> String {
        // Read before the close: the close is what kills them (R92.1).
        let lost = sub.jobs_lost_with_the_vm();
        let landed = sub.close_backend();
        let mut said = match landed {
            Some(where_) => format!("{}\n\n[subagent placement] {where_}", reply.text),
            None => reply.text,
        };
        if let Some(line) = vm_jobs_lost_line(&lost) {
            said.push_str("\n\n");
            said.push_str(&line);
        }
        answer.say(letibot_tools::builtins::task::TaskStatus::Done {
            answer: said.clone(),
            // **No reading for a VM child**: its substrate is closed with its turn, so the
            // child is not reachable and there is no present to report. See `collect`.
            child: None,
        });
        // The child has finished, so its branch goes to the queue: this is the finish, and the two notices
        // (this one and a later `task_result`) are one entry under one id.
        self.enqueue_finished(sub_id);
        said
    }

    fn run_to_completion(
        &self,
        sub_id: &str,
        prompt: &str,
        spec: &letibot_tools::builtins::task::TaskSpec,
        slot: &Arc<TaskSlot>,
        progress: &mut dyn FnMut(&str),
        restart: Option<&str>,
    ) -> Result<String, String> {
        use letibot_tools::builtins::task::Placement;
        let role = spec.role.as_str();
        // **The downgrade is a union**: whatever this session was denied, its
        // children are denied, plus what this call asks. A downgraded session
        // cannot spawn a wider child by naming a wider role.
        let downgrade = self.base.downgrade.and(&spec.downgrade);
        let (placement, vm_copy) = self.child_placement(spec)?;
        // A subagent is a real session: its id was minted by `start` and is unique because `mint_sub_id` made
        // it so, with a nanosecond so a restart cannot collide with a persisted child and an ordinal so two
        // children of one nanosecond do not share a handle. The title is the subtask's first line.
        let spawned = std::time::Instant::now();
        let sub_id = sub_id.to_string();
        let title = subagent_title(prompt);
        // The subagent seats the role it was asked for, and coder when none was named. A role this build does
        // not know is refused by name rather than seated as coder, since that would be a subagent with more
        // than it was meant to have.
        let seat = if role.is_empty() || role == "coder" {
            Seat::Coder
        } else {
            Seat::parse(role)?
        };
        let parent = self.base.session_id.clone();

        // Publish the state on the parent's hub, so a head attached there sees the spawn and the finish. Every
        // spawn goes through the model door, the absent `model:` case included, which used to keep the PARENT's
        // window while the daemon's own server answered: the call's word, then the seat's, then the parent's.
        let named = spec
            .model
            .as_deref()
            .or_else(|| self.base.leticode.spawn_model(seat.as_str()));
        // And measure the local window when nobody remembered one: `retune_window` fills it only on the way
        // out to a first provider, so a session that STARTED on one never measured its own server. `served_ctx`
        // is the same `/props` read the daemon's start makes.
        let local_window = {
            let remembered = *self.local_window.lock().unwrap_or_else(|e| e.into_inner());
            match remembered {
                Some(w) => Some(w),
                None => Some(letibot_turn::serving::served_ctx(&self.base.endpoint)),
            }
        };
        // No model named is the parent's model, and a NAMED one goes through the same door `/models` uses.
        // Both produce the four facts a spawn needs and neither is allowed to guess.
        let chosen = match named {
            Some(want) => subagent_model(want, local_window),
            None => inherited_spawn_model(&self.base.provider, local_window),
        };
        let (sub_model, sub_provider, sub_window, sub_local) = match chosen {
            Ok(m) => (m.label, m.provider, Some(m.window), m.local),
            Err(why) => return Err(why.join("; ")),
        };
        // The name the refusals below use: what was asked for, or what was inherited.
        let want: &str = named.unwrap_or(&sub_model);
        // A declared local model: one `/props` probe decides the weights, and other weights mean the child gets
        // THEIR vocabulary and dialect, or it would render one template onto another's weights. The window is
        // this box's, so a block stating none is probed rather than inherited.
        let mut sub_foreign: Option<ForeignWeights> = None;
        let (sub_endpoint, sub_window) = match &sub_local {
            Some(m) => {
                let want_at =
                    parse_local_url(&m.url).map_err(|e| format!("[model.\"{}\"] {e}", m.name))?;
                // The switch's own decision, on the parent's vocabulary, which is optional as everywhere else: a
                // byte-vocabulary parent cannot give a child SAME weights, so the child gets the target's own.
                match local_switch_decision(&want_at, m, self.base.vocab_gguf.as_deref()) {
                    Ok(LocalSwitch::SameWeights) => {}
                    Ok(LocalSwitch::OtherWeights {
                        props,
                        dialect,
                        gguf,
                    }) => {
                        sub_foreign = Some(self.foreign_weights(&gguf, dialect, &props, &want_at)?);
                    }
                    Err(why) => return Err(why),
                }
                let w = match sub_window {
                    Some(Some(w)) => Some(Some(w)),
                    _ => Some(letibot_turn::serving::served_ctx(&want_at)),
                };
                (Some(want_at), w)
            }
            None => (None, sub_window),
        };
        // And a window nobody can name may still be the parent's own, if the child runs on the parent's own
        // MODEL: same model, same window, which is the case the catalogue cannot answer. A child on a different
        // model never inherits it, which is the defect being fixed.
        let sub_window = match sub_window {
            Some(Some(w)) => Some(Some(w)),
            _ if runs_on_the_parents_model(&sub_provider, &self.base.provider) => {
                Some(self.base.context_window)
            }
            _ => sub_window,
        };
        if let Some(why) = child_window_refusal(want, sub_window) {
            return Err(why);
        }
        let task = prompt.to_string();
        // The child's name, on every state of its row (`TaskSlot::slug`).
        let sub_slug = slot.slug.clone().unwrap_or_default();
        let publish = |state: &str, said: &str, answer: Option<String>| {
            if let Some(hub) = self.registry.get(&parent) {
                hub.publish(SessionEvent::Subagent {
                    subagent_id: sub_id.clone(),
                    state: state.to_string(),
                    prompt: said.to_string(),
                    role: seat.as_str().to_string(),
                    task: task.clone(),
                    model: sub_model.clone(),
                    answer,
                    slug: sub_slug.clone(),
                });
            }
        };

        // `opening`, not `running`, until the child is actually open: a spawn that said running from this line
        // was listed in the pane while its VM was still copying, and pressing Enter led to an empty conversation.
        // `running` is published below, once the harness is open and the hub is in the registry.
        self.tasks.record(crate::tasks::TaskEntry {
            name: sub_id.clone(),
            role: seat.as_str().to_string(),
            state: "opening".into(),
            tokens: 0,
            elapsed: 0.0,
            prompt: title.clone(),
            parent: parent.clone(),
        });
        publish("opening", &title, None);
        // Every early return from here on records the failure rather than leaving a `running` row, and settles
        // the slot as it does, since this function no longer returns to a caller that settles for it. And a
        // worktree child's every ending carries what its branch does not hold, the answer and every `fail` alike.
        let the_deliverable = spec
            .worktree
            .as_ref()
            .and_then(deliverable::Deliverable::of);
        // On the slot as well as the guard: the guard settles the turn, and the slot is what the
        // thread's own exit has when it comes to reaping a failed child's tree.
        slot.set_deliverable(the_deliverable.clone());
        let answer = AnswerOnce::new(slot).with_deliverable(the_deliverable.clone());
        let fail = |why: String| {
            answer.say(letibot_tools::builtins::task::TaskStatus::Failed { why: why.clone() });
            self.tasks.record(crate::tasks::TaskEntry {
                name: sub_id.clone(),
                role: seat.as_str().to_string(),
                state: "failed".into(),
                tokens: 0,
                elapsed: spawned.elapsed().as_secs_f64(),
                prompt: why.clone(),
                parent: parent.clone(),
            });
            publish("failed", &why, None);
            why
        };

        #[allow(unused_mut)]
        let mut wiring = letibot_sessionlog::registry::SessionWiring {
            // **The child's own model when it has one**, so its session row and the
            // picker name what actually answers it; the parent's otherwise.
            model: if sub_model.is_empty() {
                self.base.model.clone()
            } else {
                sub_model.clone()
            },
            dialect: sub_foreign
                .as_ref()
                .map(|f| f.dialect.name())
                .unwrap_or_else(|| self.base.dialect.name())
                .to_string(),
            // The box that will answer this child, which is not the daemon's own when it was seated on a
            // declared local model. The row is what the picker and the store show, and a row naming the wrong
            // address is the same silent-provenance defect as a model name that does not match.
            endpoint: sub_endpoint
                .as_ref()
                .map(|e| e.authority())
                .unwrap_or_else(|| self.base.endpoint.authority()),
            workspace: self.base.workspace.display().to_string(),
        };
        // Built, not registered: nothing can switch into it, and the worker is not
        // told to open it, until `open_with_registry` below has succeeded.
        let sub_hub = self.registry.new_hub(sub_id.clone());
        // **This thread is the session's one reader** (R68.1) - marked before the hub can be
        // adopted, so there is no instant in which the daemon's worker can see the child and take
        // a command off its queue. See `letibot_sessionlog::hub::Owner`.
        sub_hub.owned_by_its_own_thread();

        let point = self.point();
        let mut sub_cfg = Config {
            session_id: sub_id.clone(),
            title: title.clone(),
            seat,
            // **The caller's word** - the only source that exists before the child does (it is
            // minted, configured and opened before anything registers it). The rule itself is
            // `Registry::parent_for`, applied at the child's open.
            parent_session_id: Some(parent.clone()),
            // The parent's point, read LIVE and not off `base`, which is the config as it stood at the parent's
            // open: a child built from that came up at the point its parent had OPENED at. Whether this child's
            // seat can carry the point is `Mode::inherited_by`'s, which clamps and says so.
            mode: point,
            // One level down, incremented once and only here (R58): it rides the config the child is built from,
            // the way `unconfined` does, so a grandchild's depth is 2 with no chain walk, and `start` refuses the
            // spawn at the cap before this is reached.
            depth: self.base.depth + 1,
            downgrade,
            placement: placement.clone(),
            // **The child's own model, when it has one.** `provider` is the whole switch:
            // `None` is the daemon's own server, exactly as `/models local` means, and a
            // `ProviderConfig` is a cloud preset whose key `models_choice` already checked.
            provider: sub_provider,
            ..self.base.clone()
        };
        // **The skills this child was started with**, when the call named any - the child's own
        // list REPLACES the one it inherited through `..self.base.clone()`, so a parent that was
        // started with three does not hand all three to a child asked for one.
        if !spec.skills.is_empty() {
            sub_cfg.skills = spec.skills.clone();
        }
        // The child's prompt is composed for ITS seat at its open, not inherited: the parent's finished prompt
        // carried its own-session paragraph and named doors a child does not hold. A parent running on a prompt
        // it was GIVEN hands that prompt down as before.
        if sub_cfg.seat_prompt.is_some() {
            sub_cfg.seat_prompt = Some(crate::config::SeatPrompt::default());
        }
        sub_cfg.seat_facts = None;
        // The worktree `task_start` arranged is the child's workspace, not the parent's tree: a child whose
        // workspace is not the worktree the tool reported would work in the wrong tree while the answer said
        // otherwise. `None` is a plain `task`, which keeps the parent's workspace.
        if let Some(wt) = &spec.worktree {
            sub_cfg.workspace = std::path::PathBuf::from(&wt.path);
            // And the main tree's build cache, through the child's environment and never a file in its tree.
            // Computed from the worktree itself, so a child brought back after a restart gets the same pair.
            sub_cfg.build_env.retain(|(k, _)| k != "CARGO_TARGET_DIR");
            sub_cfg.build_env.extend(worktree_build_env(wt));
        }
        // And the tree a VM placement copies, when the caller named one that is not the child's workspace: the
        // spawn is the one place that turns a spec into a session, and a child whose VM copied another tree is a
        // record that does not match what happened. `None` leaves the field as the parent had it.
        if let Some(src) = &vm_copy {
            sub_cfg.vm_source = Some(src.clone());
        }
        // A child of a session in a VM lives in that VM unless it asks for one of its own, the same VM down a
        // whole chain: `host` is the tool's *your own boundary*, which for a VM session is the VM. Named the way
        // the parent's backend named it, its worktree when that was mounted and its copy under the cache otherwise.
        let parent_vm = self.base.vm_attach.clone().or_else(|| {
            (self.base.placement == Placement::Firecode).then(|| {
                if self.base.vm_mounts_workspace() {
                    self.base.workspace.clone()
                } else {
                    letibot_tools::firecode::project_for(
                        &self.base.vm_copy_source(),
                        &self.base.session_id,
                    )
                }
            })
        });
        sub_cfg.vm_attach = match placement {
            Placement::Host => parent_vm,
            Placement::Firecode => None,
            // A remote child is not inside the parent's VM, and its own checkout is
            // arranged by its `open` (`SshBackend::place`) rather than attached.
            Placement::Remote(_) | Placement::RemoteVm(_) => None,
        };
        // A child given a VM of its own copies (or mounts its worktree), as before - only the
        // session `--vm` placed works in the folder itself, and its attached children with it.
        sub_cfg.vm_mount = false;
        // The parent's acceptance check rides to the child's VM the same way: on the spec, into
        // the child's config at the spawn, and from there onto `firecode up` (`Harness::open`).
        sub_cfg.vm_verify = spec.vm_verify.clone();
        // **And the local model's own address, alias and sampling**, after the spread,
        // because `..self.base.clone()` would otherwise put the parent's back.
        if let (Some(e), Some(m)) = (&sub_endpoint, &sub_local) {
            sub_cfg.endpoint = e.clone();
            sub_cfg.model = m.model.clone();
            if !m.profile.sampling.is_empty() {
                sub_cfg.sampling = serde_json::Value::Object(m.profile.sampling.clone());
            }
            if let Some(eff) = &m.profile.effort {
                sub_cfg.effort = Some(eff.clone());
            }
        }
        // And the foreign weights' own vocabulary, dialect and marker, when the declared model serves other
        // weights: the child's engine, prefix and row are all built from these three, and `sub_cfg.dialect` is
        // what its own spawns compare against, so the fact travels down the tree.
        if let Some(f) = &sub_foreign {
            sub_cfg.dialect = f.dialect;
            sub_cfg.vocab_gguf = Some(f.gguf.clone());
            sub_cfg.media_marker = f.media_marker.clone();
        }
        if let Some(w) = sub_window {
            sub_cfg.context_window = w;
            // The child's ledger is its own conversation, so it has no measured ratio:
            // the provider's number stands rather than the parent's scaled one.
            sub_cfg.ledger_scale = None;
        }
        // And the project's parameters for the role this child was seated on, which are the last word on how it
        // samples: the file beats the `[model.*]` profile, the way the project beats `~/.config/letibot/`
        // everywhere else. They touch `sampling` and not the window, which belongs to the box answering.
        apply_spawn_parameters(&mut sub_cfg, &self.base.leticode, seat.as_str());

        // And say which wall the child plans against, and WHICH TREE its VM is booted on, in the line a person
        // is already reading: nothing else records either, and the sentence used to say *copying the workspace*,
        // which was a lie for a session seated above its repository. The tree is `vm_copy_source`'s read.
        let on_window = match sub_window {
            Some(Some(w)) => format!(" on {sub_model} ({w} token window)"),
            _ => format!(" on {sub_model}"),
        };
        progress(&match placement {
            Placement::Host if sub_cfg.vm_attach.is_some() => format!(
                "opening subagent {} inside this session's own VM — the same tree, toolchain and \
                 memory{on_window}",
                letibot_sessionlog::registry::short_id(&sub_id)
            ),
            Placement::Firecode => format!(
                "opening subagent {} in a firecode VM: copying {}, booting{on_window}",
                letibot_sessionlog::registry::short_id(&sub_id),
                sub_cfg.vm_copy_source().display()
            ),
            Placement::Remote(ref host) => format!(
                "opening subagent {} on {host}: sending {} over as a checkout, on branch \
                 remote/<its short id>{on_window}",
                letibot_sessionlog::registry::short_id(&sub_id),
                sub_cfg.vm_copy_source().display()
            ),
            Placement::RemoteVm(ref host) => format!(
                "opening subagent {} in a firecode VM on {host}: sending {} over as a checkout \
                 and booting the guest there{on_window}",
                letibot_sessionlog::registry::short_id(&sub_id),
                sub_cfg.vm_copy_source().display()
            ),
            _ => format!(
                "opening subagent {}{on_window}",
                letibot_sessionlog::registry::short_id(&sub_id)
            ),
        });

        // Reassemble the shared parts so the sub harness can borrow them: cheap, since the vocabs and the wiring
        // are already `Arc`. Except when the child was seated on other weights, where those two are the TARGET's
        // and every other part stays the daemon's, because a child on another model is still this tree's child.
        let parts = Parts {
            vocab: sub_foreign
                .as_ref()
                .map(|f| f.vocab.clone())
                .unwrap_or_else(|| self.vocab.clone()),
            wiring: sub_foreign
                .as_ref()
                .map(|f| f.wiring.clone())
                .unwrap_or_else(|| self.wiring.clone()),
            mode_store: self.mode_store.clone(),
            tasks: self.tasks.clone(),
            lsp: self.lsp.clone(),
            skills: self.skills.clone(),
            // And the daemon's board map is the one thing a child must SHARE exactly: its own board lands here at
            // its open and its parent's `todo_write` finds it through the same map. A fresh map per child would
            // make a grandchild unaddressable by everybody above it.
            child_boards: self.child_boards.clone(),
            reviews: self.reviews.clone(),
            // The child joins this tree's watcher set (R58): the parent's own, which carries the tree's ROOT for a
            // permission card and the level above for what this session cannot drain. At depth 1 the parent's set
            // already carries the same root, so the card reaches the head with no chain walk.
            tree_watch: self.tree_watch.lock().expect("tree watch").clone(),
            // And the child shares this tree's handle list (R58), the other half of *every ancestor's handle can be
            // collected from any level*: a grandchild pushed into this list is collectable and killable by the root.
            tree_slots: Some(self.slots.clone()),
            // And the tree's head answers (R58's ask route): a child has no head, so an `ask` is posted to the
            // tree's ROOT and the root's `Answers` settles it. Handed down through `Parts` like the watcher set, and
            // `None` is a tree with no head-wired adjudicator, which the child's adjudicator refuses by name.
            tree_head: self
                .head_answers
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
        };

        // The child's adjudicator: its asks surface at the tree's ROOT. A child used to deny every `ask` on a
        // policy, and now the card goes to the session whose head exists. The addressing is `subagent_ask_target`'s,
        // which says why the root comes from the watcher set rather than from the parent chain.
        let (sub_tree_root, sub_head) = subagent_ask_target(
            &self.registry,
            parts.tree_watch.as_deref(),
            &parent,
            parts.tree_head.as_ref(),
        );
        let sub_adjudicator = crate::answers::SubagentAdjudicator::new(
            letibot_sessionlog::event::SubagentAsk {
                handle: sub_id.clone(),
                // The task's first line - the same title the picker row and the parent's
                // `Subagent` event carry, so the card names the child the way every other
                // surface does.
                task: title.clone(),
                root: sub_tree_root.clone(),
            },
            sub_head,
        );

        let mut sub = Harness::open_with_registry(
            &parts,
            sub_cfg,
            sub_hub.clone(),
            Some(Box::new(sub_adjudicator)),
            None,
            self.registry.clone(),
        )
        .map_err(|e| fail(e.to_string()))?;
        // Open. Now it is a session a head can switch into, and now it is running. Adopted into the REGISTRY and
        // not into `Sessions::open`, which is R58's constraint: a head can peek and attach while the daemon holds
        // no harness for it, and the daemon serves it by handing the wake to the thread that owns it.
        self.registry
            // **Cloned, because the serving loop below needs it back**: this hub is the child's
            // queue, and `serve_child` blocks on it until the daemon closes it - so the same
            // `Arc` the registry was given is the one the child serves from.
            .adopt(sub_hub.clone(), title.clone(), wiring, Some(parent.clone()))
            // A taken id here is a MINTING bug, and it says so: `mint_sub_id` is supposed to make this
            // unreachable, and the quiet symptom is a child's whole reply that cannot be collected, since
            // `task_result` finds the first slot under a handle. Reported as what it is, not as the child failing.
            .map_err(|e| {
                fail(format!(
                    "the id `{sub_id}` was already taken ({e}). That is a MINTING bug, not \
                     this child failing: the id is the handle `task_result` collects by, so a \
                     duplicate makes a reply unreachable. The mint has to be made unique — see \
                     `mint_sub_id` — rather than the reply written off."
                ))
            })?;
        // From here every exit of this thread closes the session and takes it out of the registry, through this
        // one guard (R68.2): before it, three exits left the hub registered with nobody reading it. Declared
        // AFTER `sub`, so it drops first, before the harness and whatever holds its `HostProcesses`.
        let _session = ChildSessionGuard {
            registry: &self.registry,
            id: &sub_id,
        };
        self.tasks.record(crate::tasks::TaskEntry {
            name: sub_id.clone(),
            role: seat.as_str().to_string(),
            state: "running".into(),
            tokens: 0,
            elapsed: spawned.elapsed().as_secs_f64(),
            prompt: title.clone(),
            parent: parent.clone(),
        });
        publish("running", &title, None);
        progress(&format!(
            "subagent {} open after {:.1}s — running; ctrl-g lists it, enter attaches",
            letibot_sessionlog::registry::short_id(&sub_id),
            spawned.elapsed().as_secs_f64()
        ));
        // And if it is NOT at this session's point, say so where the person is looking: the child's own settings
        // row carries the same sentence for whoever opens its pane, and this is the half the parent's reader gets.
        if sub.config().mode.name != point.name {
            progress(&format!(
                "subagent {} opened at `{}`, not this session's `{}` — {}",
                letibot_sessionlog::registry::short_id(&sub_id),
                sub.config().mode.name,
                point.name,
                sub.mode_source()
            ));
        }

        // Through the seam every session has, which is the child's whole defect: a bare `submit` returned
        // `ContextWall` and nothing reacted, so the task failed with a notice promising a compaction nothing
        // attempted. The task is the child's first turn through the shared tail, with the owner later turns use.
        let mut turns = ChildTurns::new(&sub, &sub_id, crate::sessions::TodoNagClock::new())
            .with_record(slot.record.clone())
            .with_slot(slot);
        // The branch is the deliverable, said in the brief and checked as each turn ends: one nudge if the tree
        // is dirty. Set for a RESUMED child too, since its placement comes back with it and an interrupted child
        // is the likeliest of all to hold uncommitted work.
        sub.set_deliverable(the_deliverable.clone());
        let brief = match &the_deliverable {
            Some(d) => format!(
                "{prompt}\n\n{}",
                letibot_tools::builtins::task::worktree_brief(&d.path, &d.branch)
            ),
            None => prompt.to_string(),
        };
        // A RESUMED child carries on; a fresh one is asked. `restart` is the notice a child brought back reads:
        // its session was reopened from the store under the same id, so the conversation it continues is its own.
        // A child whose daemon died before its first row has nothing to continue and is asked its task.
        if restart.is_some() {
            // **And a child of a child comes back with its parent** - this session's own records
            // are read the way a root's are when the daemon opens it.
            sub.restore_after_restart();
        }
        let restart = restart.filter(|_| {
            sub.resumed().is_some()
                && sub
                    .items()
                    .iter()
                    .any(|i| matches!(i, TranscriptItem::User { .. }))
        });
        let first =
            crate::turn_tail::run_turn(&mut sub, &mut turns, crate::turn_tail::Door::Prompt, |h| {
                match restart {
                    Some(notice) => h.continue_after_restart(notice).map(Some),
                    None => h.submit(&brief).map(Some),
                }
            })
            .expect("a prompt always runs a turn");
        let reply = match first {
            Ok(reply) => reply,
            Err(e) => {
                // Its own children end with it: a child whose first turn FAILED while its children worked left them
                // running with nobody to report to, and a VM child keeps its VM up while it runs, so orphaned
                // grandchildren held VM slots for a day. Stopped before the backend is closed, as a stop does.
                a_child_that_ends_stops_its_own(Some(&sub.subagents), &sub.hub);
                // The backend is closed on this exit too - the other two close it below - so a
                // child whose first turn failed does not leave its watchers and cgroups behind.
                sub.close_backend();
                return Err(fail(e.to_string()));
            }
        };
        // A child with children still on their task is not done: its answer is held, each child's result reaches
        // it as a turn of its own, it is served meanwhile, and it is done when none is left unsettled, with the
        // newest answer. A VM child's VM stays up for the hold, which is what a child inside it needs.
        let mut reply = reply;
        if sub.unsettled_children() > 0 {
            publish(
                "running",
                &format!(
                    "{title} — answered, and holding its answer until its own subagents have \
                     answered"
                ),
                None,
            );
            let _ = serve_child_with(&mut sub, &sub_hub, &sub_id, &mut turns, true);
            if let Some(newest) = sub.items().iter().rev().find_map(|i| match i {
                TranscriptItem::Assistant { text, .. } if !text.trim().is_empty() => {
                    Some(text.clone())
                }
                _ => None,
            }) {
                reply.text = newest;
            }
        }
        let tokens: u64 = reply.metrics.iter().map(|m| m.predicted_tokens).sum();
        let first_line = reply.text.lines().next().unwrap_or("").to_string();
        self.tasks.record(crate::tasks::TaskEntry {
            name: sub_id.clone(),
            role: seat.as_str().to_string(),
            state: "done".into(),
            tokens,
            elapsed: spawned.elapsed().as_secs_f64(),
            prompt: first_line.clone(),
            parent: parent.clone(),
        });
        publish("done", &first_line, Some(first_line.clone()));
        // A VM's life is its turn's, and closing it is where the placement sentence comes from, the one the parent
        // reads as where the work went. So a VM child answers the task and no more: its substrate cannot be held
        // open for a later question, and that is stated rather than discovered as silence.
        if placement != letibot_tools::builtins::task::Placement::Host {
            return Ok(self.finish_a_vm_child(&mut sub, reply, &answer, &sub_id));
        }
        // A host child stays reachable, and the answer is given AS the turn ends rather than on the way out of
        // this function, because what follows does not end until the hub does. Settling from the return would have
        // let a park delay it for the life of the daemon, so `task_result` would never answer.
        answer.say(letibot_tools::builtins::task::TaskStatus::Done {
            answer: reply.text.clone(),
            // No reading here either, and for a reason that is the design: the reading is taken where it is ASKED
            // for, or a child that answered and then parked would be described as it was at that moment, for ever.
            child: None,
        });
        // The child has finished, so its branch goes to the queue, here rather than at the thread's exit: a host
        // child parks and does not leave this function until the daemon closes its hub, so enqueueing on the way
        // out would hold a finished branch out of the queue. The answer being given IS the finish.
        self.enqueue_finished(&sub_id);
        if serve_child(&mut sub, &sub_hub, &sub_id, turns) == ChildExit::Parked {
            slot.mark_parked();
        }
        // The hub closed, a stop arrived, or the child parked. Released here rather than when the harness drops,
        // and there is no placement sentence to add on the host, which `close_backend` says by returning `None`.
        let landed = sub.close_backend();
        Ok(match landed {
            Some(where_) => format!("{}\n\n[subagent placement] {where_}", reply.text),
            None => reply.text,
        })
    }
}

/// The intent diff at a turn boundary, or `None` when there is nothing to say, which is the common case and the
/// point. A free function because the choice is worth testing on its own. `prose` is opt-in: it can only fire on
/// a turn that ran nothing, so nothing widens by default, and the deterministic half needs no flag.
pub(crate) fn run_place(run: &letibot_tokencore::store::SubagentRun) -> String {
    crate::subagent_record::RecordedSpec::from_json(&run.spec_json)
        .ok()
        .and_then(|r| r.worktree)
        .map(|w| format!(", worktree `{}` on branch `{}`", w.path, w.branch))
        .unwrap_or_default()
}

#[cfg(test)]
pub(crate) fn steer_for_turn(
    ledger: &IntentLedger,
    prose: bool,
    turn_id: &str,
    items: &[TranscriptItem],
) -> Option<String> {
    steer_for_turn_over(ledger, prose, turn_id, items, None)
}

/// [`steer_for_turn`] with the seat's `todo_write` board when that is its plan tool (R93.F30):
/// one plan tool per seat, so the check reads the board the model actually holds.
pub(crate) fn steer_for_turn_over(
    ledger: &IntentLedger,
    prose: bool,
    turn_id: &str,
    items: &[TranscriptItem],
    plan: Option<intent_tools::ledger::Board>,
) -> Option<String> {
    if prose {
        intent_tools::close_the_turn_over(ledger, turn_id, items, plan)
    } else {
        ledger.reconcile(turn_id, "").steering_over(plan)
    }
}

#[cfg(test)]
mod subagent_model_tests {

    /// A declared local model seats a child. `provider` stays `None` because this IS the local path; what
    /// travels is `local`, which carries the address the child's config points at. The two cannot be collapsed,
    /// since `None` alone would mean the daemon's own server, a different box with a different window.
    #[test]
    fn a_declared_local_model_can_seat_a_subagent() {
        let f = providers(
            "a_declared_local_model_seats",
            "[model.\"dense78\"]\nurl = \"http://192.168.1.78:8082\"\n\
             model = \"qwen-3.8-27b\"\nwindow = 57344\ntemperature = 0.7\n",
        );
        let m = super::subagent_model_in(Some(&f), "dense78", Some(Some(262_144)))
            .expect("a declared local model needs no key");
        assert_eq!(m.label, "dense78");
        assert!(m.provider.is_none(), "local, not metered");
        let local = m.local.expect("the declared model travels with the choice");
        assert_eq!(local.model, "qwen-3.8-27b");
        assert_eq!(local.url, "http://192.168.1.78:8082");
        assert_eq!(
            m.window,
            Some(57_344),
            "the BLOCK's window, never the parent's 262144 -- the same GGUF is served at \
             both sizes on this fleet and inheriting would run the child off the KV cache"
        );
    }

    /// A block that states no window leaves it `None` here, for the spawn path to
    /// measure off that server. Asserted because the alternative anybody would reach
    /// for -- inheriting the parent's -- is the bug.
    #[test]
    fn a_local_model_without_a_stated_window_does_not_inherit_the_parents() {
        let f = providers(
            "a_local_model_without_a_window",
            "[model.\"dense78\"]\nurl = \"http://192.168.1.78:8082\"\nmodel = \"qwen-3.8-27b\"\n",
        );
        let m =
            super::subagent_model_in(Some(&f), "dense78", Some(Some(262_144))).expect("resolving");
        assert_eq!(
            m.window, None,
            "nobody has asked that server yet, and the parent's number is about another box"
        );
    }

    /// `window = ` is read, and a value that is not a token count is named rather than
    /// quietly leaving the child with no wall.
    #[test]
    fn a_stated_window_that_is_not_a_number_is_named() {
        let f = providers(
            "a_window_that_is_not_a_number",
            "[model.\"dense78\"]\nurl = \"http://192.168.1.78:8082\"\nwindow = \"big\"\n",
        );
        let fleet = letibot_provider::keys::local_models(Some(&f));
        assert_eq!(fleet.len(), 1);
        assert_eq!(fleet[0].profile.window, None);
        assert!(
            fleet[0]
                .profile
                .unknown
                .iter()
                .any(|u| u.contains("window")),
            "{:?}",
            fleet[0].profile.unknown
        );
    }

    /// The project's parameters reach the child, and its window does not come from them: the five samplers land
    /// on the config the child is built from, replacing what it would have inherited, and `context_window` is not
    /// touched, because a child's room is the number the box answering it reported.
    #[test]
    fn the_project_parameters_reach_the_child_and_the_window_is_not_taken_from_them() {
        // A child's config as the spawn builds it: the daemon's greedy literal in force and
        // the wall the server reported for the model this child actually runs on.
        let mut sub_cfg = Config::for_this_box("/tmp");
        sub_cfg.sampling = serde_json::json!({"temperature": 0.0, "top_k": 1, "seed": 7});
        sub_cfg.context_window = Some(57_344);

        let dir = std::env::temp_dir().join(format!("letibot-leticode-sub-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch");
        let path = dir.join(crate::leticode_config::FILE_NAME);
        std::fs::write(&path, "[roles.coder]\ntemperature = 1.0\n").expect("write");
        let leticode =
            crate::leticode_config::LeticodeConfig::load(&path).expect("a role's samplers");

        apply_spawn_parameters(&mut sub_cfg, &leticode, "coder");

        assert_eq!(
            sub_cfg.sampling["temperature"], 1.0,
            "the seat's own number"
        );
        assert_eq!(
            sub_cfg.sampling["top_p"], 0.8,
            "over the operator's dense78 set, key by key"
        );
        assert_eq!(sub_cfg.sampling["top_k"], 20.0);
        assert_eq!(
            sub_cfg.sampling["min_p"], 0.0,
            "and a min_p SET to zero is not absent"
        );
        assert_eq!(sub_cfg.sampling["presence_penalty"], 1.5);
        assert_eq!(
            sub_cfg.sampling["seed"],
            serde_json::Value::Null,
            "the daemon's greedy literal is replaced rather than merged under -- a profile that \
             named `temperature` must not keep `top_k = 1` from the built-in"
        );
        assert_eq!(
            sub_cfg.context_window,
            Some(57_344),
            "the wall is the box's, never dense78's: *\"we dont have the same context limit as \
             with dense obviously\"*"
        );

        // And the file has no way to say otherwise: a role table carrying a window is refused
        // where it is read, so there is no path by which a number from the profile could
        // reach the field asserted just above.
        std::fs::write(&path, "[roles.coder]\nwindow = 262144\n").expect("write");
        let why = crate::leticode_config::LeticodeConfig::load(&path)
            .expect_err("a window in a role's table is refused");
        assert!(why.contains("window"), "named: {why}");
    }

    fn providers(tag: &str, body: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("letibot-sub-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&d).expect("scratch");
        let f = d.join("providers.toml");
        std::fs::write(&f, body).expect("write");
        f
    }
    use super::*;

    /// **`local` is the daemon's own server and needs no key** - the deepseek→local half of
    /// the operator's ask, and the half that could never fail for a missing credential.
    #[test]
    fn local_resolves_to_no_provider_and_the_servers_own_window() {
        let m = subagent_model("local", Some(Some(262_144))).expect("local needs no key");
        assert_eq!(m.label, "local");
        assert!(m.provider.is_none(), "local is the daemon's own server");
        assert_eq!(
            m.window,
            Some(262_144),
            "the child on the local server plans against the SERVER's window, not the \
             cloud model's the parent was carrying"
        );
        // A session that never switched away has no remembered server window, and `None`
        // is *nobody measured one* rather than the parent's cloud number.
        let unknown = subagent_model("local", None).expect("still local");
        assert_eq!(unknown.window, None);
    }

    /// **A name neither `/models` nor the catalogue knows is refused, and nothing is
    /// spawned** - with the sentence naming `local` and the verb that lists providers, so
    /// the model can correct itself rather than guess.
    #[test]
    fn an_unknown_provider_is_refused_by_name_with_the_fix() {
        let err = subagent_model("definitely-not-a-provider", None)
            .expect_err("an unknown provider must not resolve");
        let said = err.join(" | ");
        assert!(said.contains("definitely-not-a-provider"), "{said}");
        assert!(said.contains("/models"), "{said}");
    }

    /// The other direction: a cloud model on a child, resolved through the door `/models` uses. Guarded on the
    /// box actually holding the key, since a test that invented one would assert the catalogue and not the
    /// resolver, and skipping is said out loud rather than passed off as green.
    #[test]
    fn a_cloud_model_resolves_when_the_box_holds_its_key() {
        let has = letibot_provider::Preset::parse("deepseek")
            .ok()
            .is_some_and(|p| letibot_provider::keys::resolve(p, None, None).is_ok());
        if !has {
            eprintln!(
                "SKIPPED: this box holds no deepseek key, so the resolver's cloud arm cannot \
                 be exercised here and THIS IS NOT A PASS."
            );
            return;
        }
        let m = subagent_model("deepseek/deepseek-flash", None).expect("the key is held");
        assert_eq!(m.label, "deepseek/deepseek-flash");
        assert!(m.provider.is_some(), "a cloud child carries its provider");
        assert_eq!(
            m.provider.as_ref().and_then(|p| p.model.clone()).as_deref(),
            Some("deepseek-flash")
        );
        // **A bare provider takes the preset's default model**, the same rule `/models
        // PROVIDER` follows - one resolver, so the child and the session cannot disagree
        // about what a bare name means.
        let bare = subagent_model("deepseek", None).expect("the key is held");
        assert!(bare.label.starts_with("deepseek/"), "{}", bare.label);
    }

    /// A child nobody can give a window to is refused, by name, rather than run unbounded: the failure is three
    /// children whose wall came from the parent's cloud window while the server answering them held 256k, so
    /// nothing compacted. `None` and `Some(None)` are the same answer and both are refused.
    #[test]
    fn a_child_with_no_window_at_all_is_refused_by_name() {
        for unknown in [Some(None), None] {
            let why = child_window_refusal("local", unknown).expect("no window is a refusal");
            assert!(why.contains("`local`"), "names the model: {why}");
            assert!(why.contains("no window"), "says what is wrong: {why}");
            assert!(why.contains("/models"), "and the knob: {why}");
        }
        assert!(
            child_window_refusal("local", Some(Some(262_144))).is_none(),
            "the server's own window is a child with a wall, so it is spawned"
        );
        assert!(child_window_refusal("deepseek/deepseek-flash", Some(Some(1_000_000))).is_none());
    }

    /// The parent's window is the child's only on the parent's own model, so the refusal stays the width of the
    /// defect. A `local` child of a `local` session is the same server, and a child named for the parent's
    /// provider and model is the parent's number. A child on a different model must never inherit it.
    #[test]
    fn the_parents_window_is_the_childs_only_on_the_parents_own_model() {
        use crate::config::ProviderConfig;
        assert!(
            runs_on_the_parents_model(&None, &None),
            "one server, both local"
        );
        let ds = |model: &str| {
            Some(ProviderConfig {
                name: "deepseek".into(),
                model: Some(model.into()),
                ..Default::default()
            })
        };
        assert!(
            runs_on_the_parents_model(&ds("deepseek-flash"), &ds("deepseek-flash")),
            "same provider and model: the parent's stated number is the child's"
        );
        assert!(
            !runs_on_the_parents_model(&ds("deepseek-chat"), &ds("deepseek-flash")),
            "a different model's window is not the parent's to give"
        );
        assert!(
            !runs_on_the_parents_model(&None, &ds("deepseek-flash")),
            "THE DEFECT: a local child of a cloud session never inherits the cloud window"
        );
        assert!(!runs_on_the_parents_model(&ds("deepseek-flash"), &None));
    }
}
