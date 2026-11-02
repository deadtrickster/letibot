//! **Subagents**: the runner behind `task`, the child session's own turn loop,
//! and the bookkeeping (slots, records, spawn parameters) around it. The parent
//! half of every door stays on `Harness`; the child half is `serve_child_with`.
use super::*;

impl Harness {
    /// **Stop this session's children, and say so on its own log** — the downward edge, for a
    /// caller that holds a `Harness` rather than a `HubSteering`.
    ///
    /// Two callers, and they are two of the three doors a stop arrives through: [`serve_child`]'s
    /// stop arm (a child between turns, on the thread that runs it) and `Sessions::dispatch`'s
    /// interrupt arm (a session the daemon holds, with nothing generating). The third — the
    /// steering source's interrupt arm, for a turn in flight — holds no `Harness` and calls
    /// [`stop_children_first`] directly, which is the one implementation all three go through.
    pub fn stop_children(&self) -> Option<String> {
        stop_children_first(Some(&self.subagents), &self.hub)
    }

    /// **Why this session may NOT be parked right now, or `None` when it may** — the half of
    /// R68's idle-child policy that is about the session's own work rather than its queue (see
    /// `Config::child_park_after`).
    ///
    /// Two facts, each one a thing that would be lost or orphaned by dropping the harness: a
    /// child of this session that still has a thread (its settlement must find this session's
    /// reader), and a job still running in this session's process host (dropping the host ends
    /// it). Asked of the facts' owners — the runner's slots and the process table — rather than
    /// remembered, so a job that ended a second ago does not keep a session alive.
    /// **This session's children that have not answered yet** — see
    /// [`letibot_tools::builtins::task::TaskRunner::unsettled_children`]. Non-zero is a child
    /// holding its answer for them.
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

    /// **Wait until every subagent in this tree has left its own thread**, up to `timeout`;
    /// `true` when they all have.
    ///
    /// The other half of [`Harness::stop_children`], and the half that was missing: a tree can
    /// be stopped and closed, and there was still no way to learn that its threads were GONE.
    /// The reason that matters is the process's exit and not the tree's tidiness — `exit()`
    /// runs the ELF finalizers of every loaded library, so a child still inside `libllama`
    /// when `_dl_fini` finalizes `libggml-cuda` takes SIGSEGV **after everything else has
    /// finished**, which is a crash that looks like somebody else's change. See
    /// [`letibot_tools::builtins::task::TaskRunner::wait_for_children`].
    ///
    /// **Call it after the hubs are closed**, which is what makes it finite: a closed hub is
    /// what a child parked in `serve_child` is waiting for. It sends nothing and stops nothing
    /// itself, so calling it on a live tree with a child mid-turn waits for that turn — the
    /// deadline is the caller's answer to how long that is worth.
    pub fn wait_for_children(&self, timeout: std::time::Duration) -> bool {
        self.subagents.wait_for_children(timeout)
    }
}

/// A few words from the first message, as a session name.
///
/// Deliberately dumb: the first line, first six words, no model call, no
/// summarisation. A name is an **address** — the operator has to recognise it in a
/// list a week later — and a paraphrase is a worse address than the words that were
/// actually typed. A model-written title would also be a turn that costs a slot and
/// can fail, on the path where somebody is waiting for an answer.
///
/// Empty for a message with no words in it, and the caller then leaves the session
/// unnamed rather than storing a blank.
pub fn derive_title(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let words: Vec<&str> = line.split_whitespace().take(6).collect();
    let mut out = words.join(" ");
    // Long enough to recognise, short enough for a picker row and a header.
    //
    // **This is a NAMING rule and its bound is the ADDRESS**, which is why it is small and
    // why nothing about a terminal belongs here: the title is also the fabric alias
    // (`@seat/title`), and a person has to be able to type it. A string that is only ever
    // *displayed* takes a different rule — see [`subagent_title`].
    const MAX: usize = 48;
    if out.chars().count() > MAX {
        out = out.chars().take(MAX - 1).collect::<String>();
        out.push('…');
    }
    out
}

/// **How long a subagent's title may be** — a display bound, not an address bound.
///
/// The operator, 2026-10-03: *"subagents list has everything capped at 48 chars. but this
/// cap is a ui concern — for example my terminal is 160 cells wide, make the cap 256."*
///
/// 256, and the head trims the row to the width it actually has. The number is generous on
/// purpose: the daemon does not know the terminal, so the only job left for a bound here is
/// stopping something pathological, and 256 is far past any width a row can show.
pub const SUBAGENT_TITLE_MAX: usize = 256;

/// **The subtask, as the subagent's title.**
///
/// **Not [`derive_title`], and the difference is the whole of a defect.** That one is a
/// naming rule whose bound is the fabric ADDRESS — six words, 48 characters, a string a
/// person types — while a subagent's title is *not* an address (nothing addresses a child
/// by name; a child hears the room through its parent) but **a row in the pane**, and the
/// only cap that belongs on a row is the width of the terminal, which the daemon cannot
/// know. Reusing the address rule here cut every row to a name's length; the operator read
/// the result as *"everything capped at 48 chars"*.
///
/// So the task's first line is kept whole up to [`SUBAGENT_TITLE_MAX`] and the head trims.
/// The row the operator actually reads is drawn from
/// [`letibot_sessionlog::SessionEvent::Subagent::task`] — whole, on every state — so this
/// title is what names the child's own session and appears in the picker; it is generous
/// for the same reason.
/// **The name a child spawned without one is given** — a resumed child whose record predates
/// names, or a caller that bypassed `start`: the slug its worktree's branch `agent/<slug>` was cut
/// from, else the derivation from its prompt. The same rule
/// [`crate::subagent_record::recorded_slug`] reads a record with.
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

/// The real `task` runner: spawns a **persistent subagent session** and runs it to
/// completion, returning the subagent's final answer to the parent.
///
/// A subagent is a full session — its own hub (an operator can attach and message
/// it), its own store row (it survives the daemon and can be resumed), and its own
/// harness — not an ephemeral nested turn. It inherits the parent's permission
/// ruleset and mode, so its calls are governed by the same allow/deny/ask rules, and
/// what reaches an `ask` is posted to the tree's ROOT: the operator's ruling is that a
/// subagent's ask *"should surface to the parent head all the way to the root
/// obviously"*, so [`crate::answers::SubagentAdjudicator`] is what it opens with.
///
/// The subagent seats [`Seat::Coder`] (read, write, edit, grep, glob) — nothing
/// that spawns further subagents — so delegation is one level by construction, not
/// by convention.
/// One spawned subagent, as the parent can see it: where it has got to, and a
/// door to knock on until it gets further.
///
/// A `Condvar` rather than a poll loop, for the same reason [`letibot_tools::exec`]
/// gives a job one: a `task_result` with a `timeout_ms` should wake when the
/// child answers, not on the next tick of somebody's chosen interval.
pub(crate) struct TaskSlot {
    pub(crate) state: std::sync::Mutex<letibot_tools::builtins::task::TaskStatus>,
    settled: std::sync::Condvar,
    /// **Whether this subagent was stopped rather than finishing**, which is the one
    /// thing the child's own result cannot say.
    ///
    /// An interrupted turn still returns text — whatever it had written when the
    /// interrupt landed — so `run_to_completion` comes back `Ok`, and a `task_result`
    /// that called that `Done` would hand the parent a truncated answer under the name of
    /// the child's last word. The flag is set by [`HarnessTaskRunner::kill`], before the
    /// interrupt is sent, so the settlement that follows cannot be read as an answer.
    killed: std::sync::atomic::AtomicBool,
    /// **Which session started this child** — the one field supervision needs, because a
    /// tree's handle list is shared and the runner that owns a child is the only one that may
    /// stop it (see [`HarnessTaskRunner::stop_all`]).
    ///
    /// `task_result` deliberately reads the WHOLE list, so any ancestor can collect any
    /// handle in its tree — a decision made on purpose and unchanged. Stopping is the other
    /// direction and goes to parents, which is what this field says: one list, one owner per
    /// entry.
    owner: String,
    /// **Whether the child's own thread has ended.**
    ///
    /// Not the same question as [`TaskSlot::state`], and the difference is the whole of what
    /// `stop_all` has to get right: a child that has ANSWERED is still alive (a host child
    /// parks in `serve_child` and can be asked more), so a `Done` slot is a child that still
    /// owns whatever it started and must still be stopped. This flag is set where the child's
    /// thread leaves `run_to_completion`, and it is what makes `stop_all` skip a child that is
    /// really gone instead of submitting an interrupt nothing will read.
    exited: std::sync::atomic::AtomicBool,
    /// **Whether the thread ended by PARKING** — R68's idle-child policy
    /// (`Config::child_park_after`). A parked child's thread has exited like any other, and is
    /// not gone: its whole session is in the store, so [`HarnessTaskRunner::send`] reopens it
    /// rather than refusing a message to "a thread that has ended".
    parked: std::sync::atomic::AtomicBool,
    /// **The child's row in the store** (`crate::subagent_record`), moved when this slot
    /// settles or is stopped — what a daemon that comes up after this one reads to bring the
    /// child back. `None` for a slot with no store behind it (a test, a storeless run, the review
    /// queue's reviewers) and for a settlement re-delivered after a restart.
    record: Option<crate::subagent_record::SubagentRecord>,
    /// **The child's name** ([`letibot_tools::builtins::task::TaskSpec::slug`]) — what a handle
    /// may be given as, and what every sentence about the child labels it by. `None` for a slot
    /// made without a spec (a settlement re-delivered after a restart, whose name is in its
    /// record; a test).
    slug: Option<String>,
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

    /// The child parked as store-only. Set before [`TaskSlot::mark_exited`], on the same thread.
    pub(crate) fn mark_parked(&self) {
        self.parked.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub(crate) fn is_parked(&self) -> bool {
        self.parked.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// **This child's thread has ended.** Read by [`HarnessTaskRunner::stop_all`], which is
    /// the only caller that has to tell "already answered" from "still here".
    ///
    /// **Set under the state lock and announced on its condvar**, because a second reader
    /// arrived: [`TaskSlot::wait_until_exited`], which is how a caller that is shutting a tree
    /// down waits for the threads it started to be gone. Storing the flag outside the lock
    /// would be the classic lost wakeup — the waiter checks, this sets and notifies, the
    /// waiter then sleeps — and the cost of losing it is a shutdown that gives up on a child
    /// that has already left.
    pub(crate) fn mark_exited(&self) {
        let _held = self.state.lock().expect("task slot");
        self.exited.store(true, std::sync::atomic::Ordering::SeqCst);
        self.settled.notify_all();
    }

    pub(crate) fn has_exited(&self) -> bool {
        self.exited.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// **Block until this child's thread has ended**, or `timeout` passes; `true` if it has.
    ///
    /// The fact [`TaskSlot::has_exited`] holds, as something a caller can WAIT on rather than
    /// poll — and the difference is not politeness. A child's thread is spawned detached, so
    /// nothing else in the process can be joined to it, and a caller that is about to let the
    /// process exit has to know the thread is gone: the ELF finalizers that `exit()` runs
    /// tear down `libllama`/`libggml-cuda`, and a child still inside `tokenize` when they do
    /// takes SIGSEGV at address `0x3` — measured on `message_between_turns`, where every test
    /// printed `ok` first. See [`TaskRunner::wait_for_children`].
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

    /// The daemon's own progress line for this child, kept while it is still
    /// running: a note on a settled subagent would overwrite its answer.
    ///
    /// **Not the child's last word, and named so.** The callback `run_to_completion` takes is
    /// called three times, all of them during the child's OPENING phase — *"subagent … open
    /// after 1.2s"* — so this is the spawn's account of itself and nothing about the turn that
    /// follows. It is labelled `the spawn reported:` wherever it is printed, and what a reader
    /// wants instead (what the child is doing NOW) is `child_reading`'s, attached at the moment
    /// it is asked for by [`HarnessTaskRunner::collect`].
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

    /// **The status as the slot holds it**, for a test that has to assert a settlement happened —
    /// or did not overwrite an earlier one. `collect` is the real reader.
    #[cfg(test)]
    pub(crate) fn status(&self) -> letibot_tools::builtins::task::TaskStatus {
        self.state.lock().expect("task slot").clone()
    }

    /// **Settle a slot nobody has settled** — the spawning thread's last word, for a refusal
    /// `run_to_completion` returned before its own guard existed. A slot already settled keeps
    /// what it has: that answer, or that failure, came from the child's own path and is the
    /// truer one.
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
        // through — see `SubagentRecord::settled` for the one judgement it makes (a stop during
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

    /// **What a settlement becomes on this slot** — the kill rule, on its own so
    /// [`AnswerOnce::say`] can apply it BEFORE it adds the dirty-worktree line: a killed
    /// child's `Done` is replaced wholesale, and a line added to the answer first would be
    /// replaced with it.
    ///
    /// **`Done` under a kill is not an answer.** See the field: the text is what the
    /// child had written when it was stopped, and reporting it as the child's word is
    /// the same defect as reporting a truncated summary as whole.
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
}

/// **One child's answer, settled exactly once, whatever path the turn takes out of the call.**
///
/// `run_to_completion` has several early exits (`fail(...)`) and one long tail, and what it
/// settles is what `task_result` reads: a settlement that never fires leaves a parent waiting for
/// ever, and a second one lets a later failure overwrite the answer the child actually gave. So
/// the settlement belongs to a guard rather than to each exit — [`AnswerOnce::say`] records the
/// answer and `Drop` covers every path that did not.
///
/// **Why it moved here at all.** It used to be the caller's: `start`'s thread settled from this
/// function's RETURN, which is right while the function ends with the turn — and wrong the moment
/// a child stays reachable afterwards, because the park does not end until the hub does. So the
/// answer is given as the turn ends and the serving begins after it.
pub(crate) struct AnswerOnce<'a> {
    slot: &'a TaskSlot,
    said: std::cell::Cell<bool>,
    /// **The worktree whose branch is this child's deliverable**, when it has one — read at the
    /// moment of settlement so that EVERY ending the parent is handed (an answer, a failure, a
    /// kill, a turn that ended with nothing to say) carries the dirty-tree line at its top. See
    /// [`deliverable`]: *"Seven times tonight a child has ended with its work outside its
    /// branch"*, and the parent found out each time by looking.
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

    /// The settlement as the parent will read it: the kill rule first, then — for a worktree
    /// child whose tree is dirty — the line naming what its branch does not hold.
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

/// **A message from another session, in the words the receiver reads** — one function, because
/// two doors deliver it and a session that could tell which door the words came through would be
/// reading the machinery instead of the message.
///
/// The two doors are the two states a session can be in: a **running** turn takes it at its next
/// round boundary ([`HubSteering`]'s `Message` arm, as a `SteeringMessage`), and a session
/// **between turns** has a turn started for it (`serve_child`'s [`ChildCommand::Hear`], or the
/// worker's own `Message` arm for a session it holds). Same text, same `Speaker::Agent` row, same
/// trail line.
///
/// # The sender is named, and the sentence says which way the edge runs
///
/// **The sender is NAMED, and nothing else can name it.** `AuthorisationTrail::say` takes a
/// speaker KIND, not an identity, so *which* session spoke survives nowhere else — and the
/// operator's whole point was addressability: a message from a named session is actionable in a
/// way the same sentence from nowhere is not.
///
/// **`mine` is the RECEIVER's own `parent_session_id`**, and it is the one fact that makes the
/// sentence true in both directions. Until 2026-10 the edge ran one way — a parent correcting a
/// child — so the sentence was written for that case and said *"your parent session"* of
/// whatever id it was handed. The upward edge (`task_message {"task": "parent"}`) makes the
/// same door carry a child's words to its parent, and there the old wording would have said
/// *"message from your parent session `s-…-sub-…`"* about the reader's own CHILD: a false
/// sentence about the tree, in the one place a reader is told who is speaking. So the relation
/// is claimed only when it holds, and a message from anywhere else — a child, a peer, the
/// session hosting a review — names its sender and claims nothing.
pub fn message_from_a_session(from: &str, text: &str, mine: Option<&str>) -> String {
    match mine {
        Some(parent) if parent == from => {
            format!("message from your parent session `{from}`: {text}")
        }
        _ => format!("message from session `{from}`: {text}"),
    }
}

/// **What a child does with a command that reached its queue between turns.**
///
/// A free function so the decision can be read and tested without a model, a hub or a socket —
/// the same reason [`subagent_depth_refusal`] is one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChildCommand {
    /// Answer it: this is the operator (or a head) asking the child something, and a child is an
    /// agent with a session of its own — the operator's ruling in one line: *"subagent is just
    /// another agent. its 'sub' is a way to inherit something and be managable. so mid turn, post
    /// turn whatever"*.
    Answer,
    /// **The operator answered a row's question on this child's board** — applied here, where the
    /// board lives, and the line it makes is a turn (see [`crate::operator_ask`]).
    AnswerTodo,
    /// **Hear it: a parent's message, and there is no turn to steer it into.** The child's own
    /// turn is STARTED for it — see [`ChildCommand::Hear`]'s arm in [`serve_child`] and
    /// [`Harness::submit_a_parents_message`] — because the operator's ruling is that a message
    /// which is queued must be heard: *"fix task_message - it should enqueue"*.
    Hear,
    /// **Leave, and stop this session's children on the way out.** See [`ChildCommand::Stop`].
    Leave(&'static str),
    /// **A stop, and a stop walks the subtree downward.** See the supervision invariant on
    /// [`HarnessTaskRunner`]: a parent owns its children's lifecycle, so the last thing this
    /// session does is stop what it started, and then it goes.
    Stop,
    /// **`/clear`, answered on the thread that owns this child's harness** — a fresh
    /// conversation for the child, with its board, jobs, children and parent kept. No turn runs.
    Clear,
    /// **`/nag` — the other slash verb a child answers itself**, because the clock it moves lives
    /// on this thread (`ChildTurns::nag`) and nowhere else. A child is a session, and the
    /// operator's pause is per session: see [`crate::nag_pause`].
    Nag,
}

pub(crate) fn child_command(kind: &letibot_sessionlog::CommandKind) -> ChildCommand {
    use letibot_sessionlog::CommandKind as K;
    match kind {
        K::Prompt { .. } => ChildCommand::Answer,
        // **The operator answered a question on this child's board** — the child's board is
        // this thread's, so the answer is applied here and the turn it makes runs here.
        K::AnswerTodo { .. } => ChildCommand::AnswerTodo,
        // **An interrupt IS this child's stop, whether or not a turn is running.** The turn it
        // was written for is the common case and it never arrives here (a running turn takes its
        // own interrupt through the steering poll); what DOES arrive here is the case that was
        // measured: a parent stopped while this child sat between turns with children of its
        // own still computing. Saying *nothing is running to interrupt* was true of the session
        // and false of its subtree, which is the whole of what a supervision tree is for.
        K::Interrupt { .. } => ChildCommand::Stop,
        // **A message is a turn when there is no turn to steer.** Until 2026-10 this arm LEFT
        // the message here, because the parent's `task_message` refused to send one to a child
        // that was not running — so a message in this queue could only be the race that refusal
        // names. The operator's ruling makes the between-turns case the ordinary one: *"fix
        // task_message - it should enqueue"*, and the queue a message lands on is this one.
        // What must NOT change is who is speaking: answering it as a prompt would put the
        // parent's words in the operator's mouth and let them authorise — which is why this is
        // a variant of its own rather than `Answer`.
        K::Message { .. } => ChildCommand::Hear,
        // **`/clear` is a slash verb a child answers itself.** The rest of a session's
        // machinery is its parent's, but a clear is about THIS conversation's history — the
        // degradation the operator clears is the child's own — and everything it keeps (the
        // child's board, jobs and own children, its place under its parent) lives in the
        // harness this thread owns. See `harness/clear.rs`.
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
        // The rest is a session's own machinery — a compaction, a re-seat, a mode, the operator's
        // door — and a child inherits all of it from its parent rather than owning a copy.
        _ => ChildCommand::Leave("a door a child does not own"),
    }
}

/// **What a subagent's head is told about a command the child does not run** — pure, for the
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

/// **The sentence a session leaves about the children it stopped on the way down** — or
/// `None` when it had none.
///
/// Pure, so what a stop SAYS can be asserted without a daemon — the same reason
/// [`subagent_depth_refusal`] and [`child_command`] are free functions. It is one function
/// because there are three callers (the steering source's interrupt arm, a child's own serving
/// loop, and the daemon's between-turns arm) and a sentence written three times is a sentence
/// that drifts.
///
/// **A refusal is named, not counted away.** A child that could not be stopped is the one
/// fact a reader needs, and "stopped 3 subagents" over a refusal is exactly the claim-versus-
/// fact defect this tree keeps paying for.
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

/// **Stop every child this session owns, and say so on its own log.**
///
/// The downward edge of a supervision tree, in one place, for the three doors a stop arrives
/// through: the steering source's interrupt arm (a turn in flight), [`serve_child`]'s stop arm
/// (a child between turns), and `Sessions::dispatch`'s interrupt arm (a session the daemon holds
/// and that has nothing running). One function, so a stop cannot mean different things at
/// different doors — the same reason `subagent_depth_refusal` is one.
///
/// It publishes on the session's own hub and not the children's: the reader who has to know
/// that this session stopped its subtree is the one looking at this session — the operator who
/// pressed Esc, or the parent that killed it.
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

/// **A child that ends while its own children are still on their task stops them** — `None`, and
/// nothing is stopped, when none is. The edge a hold does not cover: a child that ANSWERS waits
/// for its children (`serve_child_with`'s `hold`), but one whose turn FAILS has no answer to hold,
/// and its children used to run on — in a VM, holding a slot — with nobody left to report to.
pub(crate) fn a_child_that_ends_stops_its_own(
    runner: Option<&Arc<dyn letibot_tools::builtins::task::TaskRunner>>,
    hub: &Hub,
) -> Option<String> {
    if runner?.unsettled_children() == 0 {
        return None;
    }
    stop_children_first(runner, hub)
}

/// **A child's own turns, after the task it was spawned for** — and the door its parent's
/// stop comes through.
///
/// The door is the one the first prompt came through — [`Harness::submit`] — and that is the whole
/// of the honesty here: a child's turn IS that call (see `run_to_completion`), so a later prompt is
/// not a second, smaller `run_prompt`. What a child does not get is what it never had:
/// `Sessions::run_prompt`'s per-turn machinery belongs to the sessions the daemon opened, and a
/// child's harness borrows `Parts` built on this thread — which is why the daemon cannot serve it
/// (recorded, with the four hazards, on the row that would do that properly).
///
/// **It is the child's own SERVANT, and that is what makes it a session the daemon can serve.**
/// Something this child owns settling is queued on the child's own watch set and the ring names the
/// child — who the daemon cannot run a turn for. So the daemon hands the wake HERE
/// ([`Hub::wake_its_own_reader`] through `Sessions::wake`), and this loop answers it with
/// [`Harness::wake`]: drain what you own, run the turn it makes. The alternative is what R58
/// measured happening — a settlement that rings a bell with no worker behind it.
///
/// **And it is on a clock, because a child is a session and the nag is per session** — the
/// operator's ruling, verbatim: *"childs being just a session should get nags"*. Until now the
/// clock was the one thing this loop genuinely was not: `Sessions::rearm_todo_nag` reads
/// `Sessions::open`, a child's harness is not in it, and the rows this daemon persistently
/// publishes for a parked child (`child_todos`) were nag-worthy by construction and never asked
/// about. **The clock lives HERE, on the thread that owns the harness** — the same
/// [`crate::sessions::TodoNagClock`] the daemon holds for its own sessions, with the same window
/// and the same `nag_should_arm` decision — because a clock whose deadline the worker read would
/// need the worker woken out of its NIL-when-nothing-armed wait by every arming from two other
/// threads (this one at a turn's end, the parent's at a board write), and delivery would still
/// have to come back here: this thread is the only one that may drive the harness. Co-locating
/// the clock with the only executor needs no hand-off and no poke. What it costs the worker's
/// wait is NOTHING: a child's deadline never enters `Sessions::next_nag_at`, and a child with
/// nothing armed hands `take_own_work_until` a `None`, which is the old indefinite wait.
///
/// It ends when the hub is closed — the same liveness test `jobwatch::watch_task` uses for the
/// thread it parks per child — and **nothing else here is on a clock**: a child asked something an
/// hour later answers it.
///
/// **Every door here is the shared turn tail** ([`crate::turn_tail::run_turn`], R68.4), with
/// [`ChildTurns`] as its owner — the loop itself is in [`serve_child_with`].
pub(crate) fn serve_child(
    sub: &mut Harness,
    hub: &Hub,
    sub_id: &str,
    mut turns: ChildTurns,
) -> ChildExit {
    serve_child_with(sub, hub, sub_id, &mut turns, false)
}

/// **The one exit every subagent thread takes** (R68.2): closes the child's hub and removes the
/// session from the registry ([`Registry::remove`]) when the thread leaves `run_to_completion`,
/// however it leaves. A `Drop` rather than a call at each `return` because the exits are `?`s
/// and early returns, and the defect this replaces was exactly an exit that forgot.
pub(crate) struct ChildSessionGuard<'a> {
    registry: &'a Arc<letibot_sessionlog::registry::Registry>,
    id: &'a str,
}

impl Drop for ChildSessionGuard<'_> {
    fn drop(&mut self) {
        self.registry.remove(self.id);
    }
}

/// **A spawned session, announced before it exists** — see [`Registry::announce_spawn`].
///
/// Held by the child's own thread, so the announcement ends where the spawn does: an open that
/// succeeded spent it already (the registry's `adopt` is what puts an entry there), and one that
/// failed, or a thread that panicked, withdraws it here. That shape rather than a call after
/// `run_to_completion` because the exits are the same `?`s and early returns
/// [`ChildSessionGuard`] exists for.
pub(crate) struct SpawnAnnounced {
    registry: Arc<letibot_sessionlog::registry::Registry>,
    id: String,
}

impl Drop for SpawnAnnounced {
    fn drop(&mut self) {
        self.registry.spawn_settled(&self.id);
    }
}

/// **Why a child's serving loop ended** — the three exits of [`serve_child_under`], told apart
/// because one of them is not an end: a PARKED child is in the store and comes back on the next
/// prompt (`Config::child_park_after`), while a stopped or closed one is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildExit {
    /// The hub closed: the daemon is going away, or the session was removed.
    Closed,
    /// A stop reached it between turns; its children were stopped first.
    Stopped,
    /// **Idle past the park window with nothing alive under it** — parked as store-only. See
    /// `Config::child_park_after` for the policy and what reopens it.
    Parked,
    /// **Held for its children, and none is left unsettled** — the hold's own end
    /// (`serve_child_with`'s `hold`): the caller now gives the child's newest answer.
    Settled,
}

/// [`serve_child`] with the clock handed in — the tests' door, and the only difference between
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

/// **A subagent's own half of the shared turn tail** (R68.4) — the [`TurnOwner`] for a session
/// served on its own thread, holding the clocks that thread keeps.
///
/// Every door of a child — its task, a later prompt, a parent's message, a wake, the idle
/// plan-check — runs through [`crate::turn_tail::run_turn`], the same sequence the daemon runs
/// for its own sessions. Before it, a child had a second copy of that sequence
/// (`submit_spoken_as_a_normal_session` plus this loop's arms), and what the copy was missing is
/// what this struct now supplies: its title reaches the registry (`take_new_title` was never
/// drained for a child, so a picker row never took one), its monitors get a waiter, it is
/// SWEPT for tool calls nothing will answer (the daemon's sweep walked only `Sessions.open`),
/// and its failures carry their reason.
pub struct ChildTurns {
    registry: Arc<letibot_sessionlog::registry::Registry>,
    id: String,
    /// The idle plan-check — the same [`crate::sessions::TodoNagClock`] the daemon keeps for its
    /// own sessions, kept here because this thread is the only one that may drive the harness.
    nag: crate::sessions::TodoNagClock,
    /// **The board's version is how this loop sees a write that ran no turn.** A parent's
    /// `todo_write target=` moves the board from the PARENT's thread and wakes this reader; the
    /// version is what tells that wake apart from a settlement that found nothing
    /// (`Harness::wake`'s `Ok(None)`), because only one of them should re-arm the clock.
    board_version: u64,
    /// When this child's abandoned-call sweep is due — the child's own copy of the daemon's
    /// `sweep_at`, armed by every turn and run on this thread when it comes due.
    sweep_at: Option<std::time::Instant>,
    /// Whether this child's monitor waiter is running — one per session, `Sessions::armed`'s
    /// rule.
    wake_armed: bool,
    /// **The child's row in the store**, which every turn AFTER its first moves back to `running`
    /// for its length (`SubagentRecord::later_turn_began`) — so a daemon restart in the middle of
    /// a parent's `task_message` turn resumes the child instead of reading its first answer as
    /// its state. `None` for a child with no record (a test, a storeless run, a reviewer).
    record: Option<crate::subagent_record::SubagentRecord>,
}

impl ChildTurns {
    pub fn new(sub: &Harness, sub_id: &str, nag: crate::sessions::TodoNagClock) -> ChildTurns {
        ChildTurns {
            registry: sub.session_registry.clone(),
            id: sub_id.to_string(),
            nag,
            // **Read BEFORE the rows it describes, and that order is load-bearing.** A parent's
            // write lands between this read and the first re-arm in the worst case, and the two
            // orders fail differently: rows-then-version arms from the OLD rows and then finds
            // the version already moved, so the wake re-arms nothing and the nag is lost for
            // good — while version-then-rows arms from the old rows, sees the version move, and
            // re-arms from the rows the write just left. The version is the wider read.
            board_version: sub.todo_board_version(),
            sweep_at: None,
            wake_armed: false,
            record: None,
        }
    }

    /// The child's record, for its later turns to move — see the field.
    fn with_record(mut self, record: Option<crate::subagent_record::SubagentRecord>) -> Self {
        self.record = record;
        self
    }

    /// **Re-arm the idle plan-check from NOW, when a turn ran or the board moved.**
    ///
    /// The two anchors are exactly `Sessions`' two: the end of every turn, and the arm at a
    /// board write (`SetOperatorTodos`/`SetTodos` for a session the daemon holds, a parent's
    /// `todo_write target=` bumping the board's version here). `turned` is the first; the
    /// version comparison is the second, and it is what turns a wake from a parent's write into
    /// a re-arm while a wake that found nothing stays silent.
    ///
    /// **And the children are taken fresh**, exactly as `Sessions::rearm_todo_nag` takes them:
    /// they are the one input that moves while neither anchor does. **And the jobs with them**,
    /// from this session's own process table: a row that waits on a background job is silent
    /// while it runs, and whether it still runs is not a fact the board or the version holds.
    ///
    /// **And the operator's pause is read on every call**, moved or not, so the clock's copy of
    /// it can never be older than the session's (`Harness::nag_hold`).
    fn rearm(&mut self, sub: &Harness, turned: bool) {
        let v = sub.todo_board_version();
        let moved = v != self.board_version;
        self.board_version = v;
        self.nag.hold(sub.nag_hold());
        // **The queue's census too, read the same way and for the same reason** — and empty for a
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

    /// **The parent or a head has spoken: the check is earned back** —
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
        // **Said even when the turn failed** — the metronome rule: a check the model never
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

/// [`serve_child`]'s loop, with the owner's state — built by the caller so a child's FIRST turn
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
    // old loop — a child that waits for its hub to close, however long that is.
    let park_after = sub.config().child_park_after;
    let mut park_at = park_after.map(|d| std::time::Instant::now() + d);
    // **The first turn just ended on this thread, so the clock starts from its end** — the
    // same arm the daemon makes for a session it holds, at the same moment relative to the
    // turn. A child that finished its task with an unfinished plan is exactly a session whose
    // last turn left work open.
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
        // **The sweep, on the thread that owns the harness** — `Sessions::sweep_abandoned_calls`
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
            // watching it, says the session is still in use — then look again a window later.
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
            // **A wake has no command behind it and needs none**: its whole content is
            // *something this session owns has settled*, and the settlement is in this session's
            // own queue. `Harness::wake` takes it and runs the turn it makes — or finds it already
            // taken at a round boundary, says `Ok(None)` and runs nothing, which is what stops one
            // settlement being delivered twice.
            OwnWork::Wake => {
                // **A wake that ran nothing re-arms nothing** — the worker's `Sessions::wake`
                // answers `Ignored` for the same case. A board that MOVED under the wake does
                // re-arm, which is the parent's write: the daemon's `SetOperatorTodos` arm does
                // the same for a session it holds.
                if later_turn(sub, turns, Door::Wake, |h| h.wake()).is_none() {
                    turns.rearm(sub, false);
                }
                continue;
            }
            // **The idle plan-check, delivered on the thread that owns the harness.** This is
            // `Sessions::deliver_due_nags`' one-session body, run here because the worker
            // cannot: it holds no `Harness` for this session. Disarmed before the turn, exactly
            // as `due_now` disarms for the worker: a delivery that left the old deadline in place
            // would fire again at once.
            OwnWork::TimedOut => {
                turns.nag.fired();
                // **A paused child's deadline is the pause's end, not a check** —
                // `Sessions::deliver_due_nags`' rule on this thread: the pause is lifted and said,
                // the clock re-armed, and no turn runs. Still paused (a wall clock a hair behind
                // the monotonic one) runs nothing and re-arms for the end.
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
            // **The same door the first prompt took** — a child is a session, and a prompt to it
            // is a turn through the shared tail like any other.
            // **The operator's answer to a row's question, between this child's turns** — the
            // daemon's `AnswerTodo` arm, on the thread that owns this board.
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
            // **A parent's message, and there is no turn to steer it into — so one is started.**
            // The between-turns half of the operator's ruling (*"fix task_message - it should
            // enqueue"*). The words are recorded as the parent's (`Speaker::Agent`), naming which
            // parent, so they can neither authorise an act nor be read as something the operator
            // typed; the mid-turn door (`HubSteering::try_next`) speaks the same text through
            // [`message_from_a_session`], so a session cannot tell which state it was in.
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
            // **The children first, then this session.** Stopping is a tree's downward edge — see
            // the supervision invariant on [`HarnessTaskRunner`] — and this is where a child that
            // is between turns receives it. It then leaves: an interrupt that reaches a session
            // stops that session, and a stopped session that stayed would be the shape this whole
            // change is against.
            ChildCommand::Stop => {
                sub.stop_children();
                return ChildExit::Stopped;
            }
            // **A clear is not a turn**, so nothing runs after it: the fork lands, the row says
            // so, and the child waits for its next word as it was waiting before. The same two
            // publications the daemon's arm makes for a session it holds, so a head attached to
            // a child reads the same boundary it reads on a root.
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
                // No turn ran and the board did not move — `Leave`'s reading of the clock.
                turns.rearm(sub, false);
            }
            // **`/nag` on this child**, answered here and published as the worker publishes every
            // verb's reply (`slash` / `slash_refused`, the line echoed first), then the clock
            // re-armed with whatever the verb left — the pause's end, or the plan's own check.
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

/// **A turn of a child after its first** — [`run_turn`](crate::turn_tail::run_turn), with the
/// child's record moved to `running` for its length and put back when it ends (or left running
/// when it ended because the daemon is stopping, which is what the next daemon resumes). See
/// [`ChildTurns::record`].
fn later_turn(
    sub: &mut Harness,
    turns: &mut ChildTurns,
    door: crate::turn_tail::Door,
    turn: impl FnOnce(&mut Harness) -> Result<Option<Reply>, HarnessError>,
) -> Option<Result<Reply, HarnessError>> {
    let record = turns.record.clone();
    let prior = record.as_ref().and_then(|r| r.later_turn_began());
    let out = crate::turn_tail::run_turn(sub, turns, door, turn);
    if let (Some(r), Some(prior)) = (&record, &prior) {
        r.later_turn_ended(prior);
    }
    out
}

/// **The session's children, and the whole of the supervision edge between them.**
///
/// The operator's design, in their words: *"think about it like it is an erlang supervision
/// tree. we talk to parents and they own lifecycle."* Four sentences are the whole of it, and
/// every one of them is a line somewhere below:
///
///   1. **A parent owns its children's lifecycle.** A child is started by one session
///      ([`HarnessTaskRunner::start`]) and that session is its owner — [`TaskSlot::owner`] —
///      which is what lets a runner stop exactly what it started and not its siblings' work or
///      its own grandchildren.
///   2. **An exit goes parent-ward, never to the root.** What a child's settlement is queued
///      on is the session that STARTED it (`jobwatch`'s own routing), never an ancestor's.
///      The parent decides what it means — collect it, act on it, or finish — and when the
///      parent itself finishes, ITS exit is what travels the next level up. That is why the
///      operator is never handed a grandchild's exit, and why nothing here needs a parent
///      pointer.
///   3. **Termination walks the subtree downward.** A session that is stopped stops its
///      children FIRST ([`TaskRunner::stop_all`], called from the interrupt doors and from a
///      child's own serving loop) and only then goes itself; each child does the same, so the
///      walk is one level per session and no runner needs to know its grandchildren.
///   4. **Depth is a bound, not a prohibition.** `task` is seated at every level and refused at
///      the cap by NAME — see [`subagent_depth_refusal`].
///
/// # What is NOT here, and is the second half of the design
///
/// **TODO(supervision-2, the operator's second half): the RESTART POLICY.** Nothing in this
/// tree decides what a child's exit *means* beyond reporting it: a parent that wants a child
/// restarted starts one again itself, and there is no `one_for_one`/`one_for_all`, no
/// max-restarts-in-a-window, and no supervision strategy on the parent. The seam for it is the
/// parent's own drain of its child's settlement — the notice `Harness::wake` builds — because
/// that is the one place a parent already decides something about a child that has ended. The
/// same half adds a configurable `max_depth`; today the bound is `--max-subagent-depth`
/// ([`subagent_depth_refusal`]), which is the number the operator's design calls for and is
/// left exactly as it is rather than invented a second time.
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
    /// **The daemon's board map, so a child's `Parts` carries the same one its parent's
    /// did** (`crate::child_todos`): a grandchild's board must land in the SAME map the
    /// grandparent's `todo_write` resolves through, or a parent could write one generation
    /// of children and not the next. Cloned from `Parts` the way every other shared piece
    /// here is.
    pub(crate) child_boards: Arc<crate::child_todos::ChildBoards>,
    /// **The daemon's review desk, so a gatekeeper child's own harness registers the tool
    /// against the same desk this session opens its reviews on** — the one thing that makes a
    /// reviewer's `review_verdict` call reach the queue that asked it. See
    /// [`Harness::serve_reviews`] and `letibot_tools::builtins::verdict`.
    pub(crate) reviews: Arc<letibot_tools::builtins::verdict::ReviewDesk>,
    /// The subagents this session has started, in the order it started them.
    /// Shared with every clone of this runner — the thread that runs a child
    /// holds one, and so does the tool that collects it.
    pub(crate) slots: TaskSlots,
    /// **The watcher set a child of THIS session joins** (R58).
    ///
    /// A slot rather than a value because the runner is built before the harness has
    /// built its own `job_watch`, and the runner must outlive the assignment — so the
    /// harness fills this once, a few lines later, with the set a child should join.
    /// For a root that set is the root's own; for a child it is the child's, which carries the
    /// tree's ROOT (for a permission card) and the level above (for what this session cannot
    /// drain, and for the ring that says so) — so a `Parts` built for a grandchild inherits both
    /// without a parent-chain walk.
    pub(crate) tree_watch: Arc<std::sync::Mutex<Option<Arc<crate::jobwatch::JobWatchers>>>>,
    /// **The head's answers a child of THIS session asks through** — the ask route's
    /// half of R58, and the same slot shape as `tree_watch` for the same reason: the
    /// runner is built before the gate that creates them.
    ///
    /// For a root this is the `Answers` of its own head adjudicator; for a child it is
    /// the one inherited from the tree, which is the ROOT's — so a grandchild's card
    /// reaches the same head a child's does, with no parent-chain walk. `None` is a
    /// tree with no head-wired adjudicator at all, and a child then refuses by name
    /// rather than posting a card nothing can settle.
    pub(crate) head_answers: Arc<std::sync::Mutex<Option<Arc<crate::answers::Answers>>>>,
    /// **The daemon's own server window, as this session last knew it** — `None` until
    /// the session first switches away from local, and the server's window once it has.
    ///
    /// A cell shared with the [`Harness`] because a spawn may ask for `model: "local"`
    /// while the parent is on a cloud provider: the child then runs on the daemon's
    /// server and must plan its compaction against THAT window, not the cloud model's —
    /// the same mistake `retune_window` exists to prevent, one direction over. The
    /// harness writes it wherever it updates its own copy; the runner reads it at spawn.
    pub(crate) local_window: Arc<std::sync::Mutex<Option<Option<u64>>>>,
    /// **This session's point, as it stands NOW** — the one thing `base` cannot answer.
    ///
    /// `base` is a clone taken at open and nothing updates it: a `/mode` mid-session
    /// moves `Harness::cfg.mode` and leaves `base.mode` where it was, so a child built
    /// from `base` came up at the point its parent *opened* at. Measured 2026-10-06: the
    /// operator's own session at `allow-all` — consented, on their box — and every child
    /// of it at `automode-edits`, each child's `bash` putting a person in front of a
    /// question the parent had already answered, with the decision row reading as the
    /// child's own. Written by [`Harness::set_mode_consented`]; read by the spawn, which
    /// hands it to the child as `sub_cfg.mode`.
    pub(crate) point: Arc<std::sync::Mutex<letibot_tools::mode::Mode>>,
    /// **Vocabularies loaded for other weights, by GGUF path** — the spawn cache.
    ///
    /// A fan-out of children all seated on one declared model would otherwise load
    /// that model's tokenizer once per child, and a tokenizer load is seconds of
    /// reading a first shard. Keyed by the path the target's own `/props` named,
    /// so two different models never share an entry and the same model is loaded
    /// once however many children ask. The session switch does not use this: it
    /// loads its own, once per switch, because the switch is rare and the harness
    /// does not hold this runner.
    pub(crate) foreign_vocabs:
        Arc<std::sync::Mutex<std::collections::HashMap<PathBuf, Arc<Vocab>>>>,
    /// **The `task_start` children this session has put in a tree**, by handle — the ask each
    /// was given and the placement it works in.
    ///
    /// A record rather than a second look at the child: the enqueue needs the brief and the
    /// placement, and by the time a child has finished its own session may be gone, its
    /// workspace torn down, and its transcript the only place the ask survives — which is not
    /// a place a queue can read from. So the two facts are kept here, where the runner that
    /// arranged the tree can answer for them, and the entry is built from them.
    ///
    /// **Keyed by handle, and the handle is the entry's id.** That is what makes *one entry
    /// per finished child* true across the two notices (the child's own settlement and a
    /// `task_result` that arrives later): both look the same child up and write the same row.
    pub(crate) placed: Arc<std::sync::Mutex<Vec<PlacedChild>>>,
    /// **Where a finished child's entry is written** — the session store, opened on first use.
    ///
    /// Opened lazily and held as a cell for [`crate::jobwatch::JobRecorder`]'s reason: a store
    /// opened at construction is opened for every session that never starts a `task_start`
    /// child. The cell is a `Mutex` because a `rusqlite::Connection` is `Send` and not `Sync`.
    pub(crate) merge_store: Arc<std::sync::Mutex<Option<Store>>>,
}

/// **One `task_start` child, as the runner remembers it** — see
/// [`HarnessTaskRunner::placed`].
///
/// The two fields are the two things the enqueue needs and nothing else in this process can
/// answer for: the ask the child was given (the reviewer's only framing) and where its branch
/// is. The path is inside the placement rather than beside it because the entry's `worktree`
/// column and the placement's `path` are one fact.
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
    /// **The record [`HarnessTaskRunner::start_worktree`] keeps for a child, or `None` for
    /// one the queue has no business with.**
    ///
    /// One rule, and it is a rule about the MAIN checkout: **a `task_start` child put in the
    /// main tree is not recorded.** It works on `main`'s own branch, so there is no branch of
    /// its own to land and no base to land it on — [`crate::mergequeue::entry_for_finished`]
    /// refuses one anyway, and a record here would only be a second place that has to know
    /// it. Everything else — a fresh worktree, with a branch cut from the base the caller
    /// named — is recorded, because it is exactly the shape the queue serves.
    ///
    /// **A function rather than three assignments at the call site**, for the reason every
    /// other seam in this tree is one: the recording IS half of the enqueue (the `placed`
    /// lookup in [`HarnessTaskRunner::enqueue_entry`] reads what this wrote), and a test that
    /// built a `PlacedChild` of its own would be asserting against a copy of this rule rather
    /// than against it. See `queue_e2e`, which calls this and then the enqueue.
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

/// **A subagent's model, resolved from the `task` tool's `model` argument.**
///
/// The operator's ask, 2026-10-05: *"I want to be able to have subagents using different
/// models. say you deepseek should be able to run local model"*, and the other direction
/// too: *"local qwen in main session should be able to run cloud glm"*. **Symmetric on
/// purpose** — the child's model has nothing to do with the parent's, so the two names are
/// resolved by one function and neither direction is special.
///
/// The resolution goes through [`crate::slash::models_choice`], the same door `/models`
/// uses, and that is the point rather than a convenience: a name a person can type is a
/// name a child can run on, the key lookup is the same one the picker's green rows use,
/// and a name neither accepts is refused by the SAME sentence. A separate resolver here
/// would be a second answer to *what is this provider* — the class of defect this tree
/// keeps finding by looking for it.
///
/// `local_window` is the daemon's own server window as the parent last knew it
/// ([`HarnessTaskRunner::local_window`]), used for `local`; a cloud name takes its window
/// from the catalogue, and a name the catalogue has no row for leaves the number alone
/// rather than inventing one.
#[derive(Debug)]
pub struct SubagentModel {
    /// What the record says: `local`, or `PROVIDER/MODEL`.
    pub label: String,
    /// `None` is the daemon's own server; `Some` a cloud provider, its key already found.
    pub provider: Option<crate::config::ProviderConfig>,
    /// The window to plan compaction against, `None` when nobody knows one.
    pub window: Option<u64>,
    /// **A declared local model the child runs on**, instead of the daemon's own server.
    ///
    /// The operator, 2026-10-05: *"models from providers.toml must be usable for
    /// subagents"*. `provider` cannot carry one — it is `None` for local and a cloud
    /// preset otherwise — and overloading `None` to mean *either* the daemon's server
    /// *or* some other box is the same conflation `ModelChoice` was widened to end.
    ///
    /// The child tokenizes with the parent's vocabulary when the target serves the
    /// same weights, and with the target's own when it does not — the spawn path
    /// runs [`local_switch_decision`] exactly as a session switch does, and seats
    /// the child on the pair it answers with.
    pub local: Option<letibot_provider::keys::LocalModel>,
}

/// **The child's window, or the refusal that names the knob** — pure, so both halves are
/// testable without a harness, and beside [`subagent_depth_refusal`] because it is the same
/// shape of decision: *refuse by NAME rather than by absence*, so the model always sees `task`
/// and the sentence says which knob moves.
///
/// **Why this exists, measured 2026-10-05.** Three children of one session ran on this daemon's
/// own server (`qwen-3.8-27b`, 256k) while their config carried the PARENT's `context_window`
/// (deepseek's catalogue ~1M) and the parent's `ledger_scale`, which [`Config::planning_window`]
/// scales that window by — so their wall was computed four times their own, `should_compact` was
/// faithful to it and therefore never fired, and they reached 911,522 / 910,486 / 911,708
/// resident tokens **with no compaction item in their logs at all**. The spawn resolved a model
/// only when one was NAMED, and an absent `model:` was assumed to mean "the parent's" — but what
/// answers such a child is the daemon's own server whenever this session sits on a provider.
///
/// `Some(Some(n))` is a named window. `Some(None)` is *somebody looked and the server did not
/// say*, and `None` is *nobody has an opinion*: for the child those are the same answer, because
/// `None` is not a large window — it is no wall at all, and every check that would compact reads
/// `let Some(window) = …` and is skipped.
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

/// **Whether a child will be answered by the same model as its parent** — the one condition
/// under which the parent's window is the child's window rather than a number belonging to a
/// different server. `None` on both sides is the daemon's own server, which is the same server
/// for both.
///
/// This exists so the refusal above is not BROADER than the defect it was written for. The
/// defect was a child inheriting a window from a model it was not running on; a child that runs
/// on exactly the parent's provider and model inherits nothing foreign, and it is the case the
/// catalogue cannot answer at all: a session started with an explicit `--context-window` for a
/// model the catalogue does not carry has a real number, and the operator stated it.
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

/// **How a child samples, as the project's `leticode.toml` says it does.**
///
/// The operator: *"when used in subagents mode we need to call qwen with the parameters
/// similar to dense78 - otherwise it overthinks terribly"*. `role` is the seat the child
/// was seated on, and the parameters are `[roles.<seat>]` over `[roles.subagent]` over the
/// dense78 numbers the operator measured — see `LeticodeConfig::parameters_for`.
///
/// **They REPLACE what this config already carries rather than merging onto it**, the way a
/// `[model.*]` block replaces the built-in: the project file is the more specific statement
/// about this project's subagents, and a merge would keep the parent's greedy literal
/// (`temperature = 0.0`, `top_k = 1`) in force under a profile that named only
/// `presence_penalty` — the built-in deciding the most consequential knob in a profile
/// somebody wrote.
///
/// **`sampling` and no other field.** The operator's second sentence is *"we dont have the
/// same context limit as with dense obviously"*, so the child's window stays whatever was
/// measured for the model it actually runs on: `context_window` is not touched here, and a
/// project file has no way to state one at all (`LeticodeConfig` carries models and
/// samplers, not walls). This function is one line wide so that stays true.
fn apply_spawn_parameters(
    sub_cfg: &mut Config,
    leticode: &crate::leticode_config::LeticodeConfig,
    role: &str,
) {
    sub_cfg.sampling = leticode.parameters_for(role).to_sampling();
}

/// **The model a child runs on when nobody named one: its parent's.** The parent's own
/// provider configuration when it is on one — taken as it is, key and all, so a parent that
/// was given its key by `--api-key` or the masked card does not leave a child that cannot find
/// it again — and `local` when the parent is on this daemon's own server.
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

/// [`subagent_model`] against a named `providers.toml`.
///
/// The file is a parameter for the reason [`crate::slash::models_choice`]'s is: the
/// declared local models are read from it, so a test that cannot name one is a test
/// of whatever happens to be in the operator's config. The public entry point passes
/// `None`, which is the real file.
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
            // out to a provider, or whatever `None` means — *nobody measured one* — rather
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
        // **A declared local model seats a child like any other model** — the
        // operator's ruling: *"models from providers.toml must be usable for
        // subagents"*. `provider` stays `None`, because this IS the local path; what
        // changes is where that path points, and `local` carries it.
        //
        // The window comes from the block when it states one and is left `None`
        // otherwise, for the caller to measure off that server's `/props`. This
        // function does no network: its docstring promises it is testable without a
        // harness, and a probe in here would make every test of it either a lie or a
        // live test.
        crate::slash::ModelChoice::Local(m) => {
            return Ok(SubagentModel {
                label: m.name.clone(),
                provider: None,
                window: m.profile.window,
                local: Some(m),
            });
        }
        // `local` is handled above by name, so reaching here means the store spelled a
        // local switch some other way — a resolver bug, refused rather than guessed.
        crate::slash::ModelChoice::OwnServer => {
            return Err(vec![format!(
                "`{want}` resolved to no provider and no model"
            )]);
        }
    };
    provider_model(pc)
}

/// A child seated on a provider configuration already in hand: the label and window the
/// catalogue gives it, and the configuration — key included — as it is.
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

/// **The settings row's `mode_source` for a subagent** — the vocabulary the row already uses
/// for where a point came from, and the clamp when there was one.
///
/// A free function because it is a sentence, and because the two spellings have to stay one
/// vocabulary: the row a head draws for a child says *inherited from the parent session* —
/// the phrase this file already used for it — and a child that could not carry the point its
/// parent is at adds the sentence `Mode::inherited_by` wrote, which names the seat, the point
/// inherited, what was missing and where it landed.
///
/// **The clamped case is the whole reason this exists.** A child silently at a different
/// point than its parent is the state that made this a bug hunt: the operator's session was
/// at `allow-all` and every child came up at `automode-edits` with nothing anywhere saying
/// the two were not the same.
pub(crate) fn child_mode_source(why: Option<&str>) -> String {
    const INHERITED: &str = "inherited from the parent session";
    match why {
        None => INHERITED.to_string(),
        Some(why) => format!("{INHERITED}, clamped to what this seat can carry: {why}"),
    }
}

/// **The R58 depth refusal, as one pure function so it is testable without a harness.**
///
/// `Some(message)` refuses a `task` call that would open a level past `max`;
/// `None` is *go*. Two design decisions are the whole of it:
///
/// * **It refuses by NAME and not by absence.** `task` stays seated on `m2_coder` at
///   every depth, so the model always sees the tool; the refusal says which knob moves
///   the limit. A seat that simply lacked `task` at the cap manufactures the workaround
///   this tree has already paid for once (`m2_coder`'s note on `todo`: 13 calls and
///   ~15k tokens emulating a tool that had not been given).
/// * **It is assessed at the CALL and not on the seat**, because a seat can say only
///   *names `task`* (unlimited) or *does not* (never), and the ruling wants a
///   configured number in between.
///
/// A `max` of 0 refuses every `task` call, which is the honest reading of *no nesting*.
///
/// **TODO(supervision-2): `max_depth`, and it is the second half of the design rather than a
/// missing knob.** The cap is IN FORCE tonight — `Config::max_subagent_depth`, default 3,
/// `--max-subagent-depth` — and the half that is not built is a *runtime* depth the tree can be
/// told at spawn (per-call, and per-child, so one branch can be told to go three deep while its
/// sibling stops at one) together with the restart policy beside it. Depth is a BOUND here and
/// never a prohibition: `task` is seated at every level and this refuses by naming the knob, so
/// a model that hits it learns what to change rather than inventing a way round. Do not add a
/// second number before that decision is made — the operator's design names one bound and this
/// is it.
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
/// `b/c`.` — or nothing when there are none.
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

/// **Run a git command in a directory and return its stdout, trimmed.**
///
/// The one door `task_start`'s filesystem work goes through: the tool layer has no
/// git and no exec of its own, so the runner is where `git worktree add` and its
/// companions are run. `Ok` is the command's stdout, trimmed; `Err` is the command
/// and its stderr, so a refusal names what was tried and what git said rather than
/// swallowing the failure.
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

/// **A subagent's session id, minted so that two children cannot share one.**
///
/// leticl found this one on the wire and diagnosed it without reading this tree: three `task`
/// calls minted in a loop got ONE id, *"so they cost one pane row, no subagent counter, one
/// `[task]` notice — and **two of the three answers never reach me at all**"*. The id was
/// `"{parent}-sub-{milliseconds}"`, and a millisecond clock is not an id source at all: three
/// calls in one round land in the same millisecond trivially. It is now the entropy this daemon
/// already mints its own session id from ([`crate::config::now_ns`], the value behind
/// [`crate::config::Config::session_id`]) with the counter loop `server.rs::mint_session_id`
/// already uses for a root session — the same shape one prefix along, rather than a second
/// scheme.
///
/// **Why a duplicate is not cosmetic.** The id is the HANDLE the model collects by
/// (`task_result`) and the key every head folds on, and `task_result` finds the FIRST slot
/// under a handle — so the other replies exist, settle, and are unreachable. That is the same
/// failure `ToolOutcome` refuses to collapse into *success with an empty payload*, one layer
/// down: a child's whole reply, silently unaddressable.
///
/// **A finer clock would not fix it; the guard is what does.** The operator's framing: *"a
/// timestamp is enough for a browser, not for a harness with automated loops"*, and *"nanos
/// would shrink the window and keep the race; a counter, a per-session sequence, or a random
/// suffix removes it"*. So there are two checks, because either alone misses the case that was
/// actually hit:
///
///   * the **registry** catches an id whose child has been adopted;
///   * `handed_out` catches one whose child is still being OPENED — and that is the ordinary
///     shape, not an edge: `adopt` happens inside the child's thread, after a whole harness has
///     been opened, while the mint returns immediately. Three `task` calls in one round
///     therefore all mint before any of them adopts.
///
/// **The timestamp stays and an ordinal disambiguates.** The timestamp is what keeps a subagent
/// minted after a daemon restart from colliding with one already in the STORE — a counter
/// resets, and `adopt` checks only the live registry, so a collision there would RESUME the old
/// child instead of spawning a new one. So the nanosecond is the starting point and the suffix
/// is added while the id is taken.
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

/// **The target's own tokenizer and dialect, for a child seated on other weights.**
///
/// Built by the spawn path from the same decision a session switch makes; the
/// child's [`Parts`], its config and its session row are all derived from these
/// facts so nothing about the child still names the daemon's weights.
struct ForeignWeights {
    vocab: Arc<Vocab>,
    wiring: Arc<Wiring>,
    dialect: crate::dialect::Dialect,
    gguf: PathBuf,
    media_marker: Option<String>,
}

impl HarnessTaskRunner {
    /// **This session's slot for one handle**, if it started one under that name. `None` is a
    /// handle this runner never minted — the same answer `collect` gives as `Unknown`.
    ///
    /// A helper rather than a fourth copy of the lookup: `send` reads `has_exited` off it (a
    /// thread that has ended takes no wake) and `collect` reads the status.
    /// **Bring a parked child back from the store** — `Registry::create_under` with what its
    /// stored row says (title, wiring, parent), which rings the worker to OPEN it before any
    /// command for it is served. The open is the worker's, as every resume's is: this thread is
    /// the parent's and may not build a harness for another session. A second sender that raced
    /// this one finds it already created and uses that hub.
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

    /// **This runner's store, for the children's records** — the same lazily opened connection
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

    /// **This session's children the store names and the record does not** — see
    /// [`letibot_tokencore::store::Store::children_without_runs`], whose window is what keeps this
    /// to the children a record was *due* for. **The window is not computed here**: the store reads
    /// it, so this list and [`Self::unrecorded_child`] cannot disagree about it — and a store that
    /// has never recorded a child lists none rather than every child it holds.
    ///
    /// **The review queue's session is not asked**, for the reason
    /// [`HarnessTaskRunner::record_start`] does not record its children: a reviewer is the queue's
    /// (it has its own lease and starts its own), so *no record* is the ordinary state there and
    /// the queue's pane is the door onto it. Measured on the operator's box, that session has 447
    /// such children and every one of them would have been news to a model.
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

    /// [`Self::unrecorded_children`] for one handle — the same gatekeeper rule and the same store,
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

    /// **Write a fresh child's row** — `running`, owned by this daemon, with the subtask and the
    /// spawn's description — and hand back the record its slot moves. See
    /// `crate::subagent_record`.
    ///
    /// **The review queue's reviewers are not recorded.** The queue keeps its own lease and starts
    /// a reviewer for every row nobody is working, so a reviewer resumed beside the one the queue
    /// starts would be two verdicts on one branch. Their parent is the queue's own session.
    ///
    /// A row that cannot be written is said on the daemon's log and the child starts anyway: the
    /// record is what a RESTART needs, and refusing the spawn over it would cost the work now for
    /// the sake of a restart that may never come.
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
            // **The row is kept rather than dropped.** A busy store is a moment, not a verdict:
            // the child's own settlement (or kill, or later turn) writes this row, so a spawn
            // that lost the write still leaves a record — and a child that never gets that far is
            // still named by its session (`children`, `look`).
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

    /// **The child's thread, for a fresh child and a resumed one alike** — the slot, the thread
    /// that runs [`HarnessTaskRunner::run_to_completion`], and the refusal when the thread cannot
    /// start. `restart` is `Some` for a child brought back after a daemon restart: the notice its
    /// continued turn opens with.
    fn spawn_child(
        &self,
        sub_id: String,
        prompt: &str,
        spec: &letibot_tools::builtins::task::TaskSpec,
        record: Option<crate::subagent_record::SubagentRecord>,
        restart: Option<String>,
    ) -> Result<String, String> {
        // **The child's name** — the spec's (a fresh spawn's, made unique by `start`), else the one
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
        // **The child EXISTS from the moment its handle does, and the registry has to say so.**
        // The handle is minted and returned before the child's own thread opens its session
        // (`Registry::adopt`, inside `run_to_completion`), so a reader between the two — the
        // merge queue asking whether a row's reviewer is still alive — was answered *no session
        // by that id* about a child that had just been started and was still booting its VM.
        // MEASURED on the operator's box, 2026-10-26: 2.0 s after the start, the reviewer was
        // *not alive in this daemon any more* and its row re-seated, three passes running.
        //
        // Announced BEFORE the thread is made, so there is no instant in which the handle is
        // out and the registry does not know it; withdrawn by the guard when this thread ends
        // (a successful open has spent it by then — see `Registry::announce_spawn`).
        self.registry.announce_spawn(&sub_id);
        let announced = SpawnAnnounced {
            registry: self.registry.clone(),
            id: sub_id.clone(),
        };
        // The whole spec, carried through verbatim — the worktree `task_start` arranged (the
        // child's workspace is the worktree, not the parent's tree), and the tree a VM placement
        // copies, which is the caller's word about where the work is and must not be invented
        // at this layer.
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
                // **The settlement is the function's own now** (see `AnswerOnce`): it happens as
                // the turn ends, because what follows — a host child serving its own queue until
                // the hub closes — does not end until the daemon does. There is nothing for this
                // thread to do with the answer; the slot already has it.
                //
                // **Except for a refusal that came before the guard existed.** The checks at the
                // top of `run_to_completion` — the seat, the model door, the local vocabulary,
                // the child's window — return before `AnswerOnce` is made, and their sentence
                // used to be dropped right here: the slot stayed `Running` with no thread behind
                // it, and `task_result` waited out its whole timeout to report a child "still
                // working" that had never been created. The operator, on a stroppy session whose
                // `where: firecode` child did exactly that: *"it hanged on task_result wtf"*.
                let ran = me.run_to_completion(
                    &id,
                    &prompt,
                    &spec,
                    &slot,
                    &mut |n| slot2.note(n),
                    restart.as_deref(),
                );
                finish_child_thread(&slot, ran);
                // **And this is where the child's thread ends — the fact stopping needs.** See
                // [`TaskSlot::exited`]: a child that answered is still alive (it parks and can be
                // asked more), so `stop_all` cannot read "has an answer" as "is gone".
                slot.mark_exited();
            });
        if let Err(e) = spawned {
            // Nothing is running; say so rather than handing back a handle for a
            // child that was never started. The announcement went with the failed spawn — the
            // closure `spawn` dropped held it, and a session nothing will ever open is not one
            // that is opening.
            self.slots
                .lock()
                .expect("task slots")
                .retain(|(h, _)| h != &sub_id);
            return Err(format!("the subagent thread could not be started: {e}"));
        }
        Ok(sub_id)
    }

    /// **The children a daemon that is gone left behind, brought back** — the operator's *"we
    /// should bring subagents back … and restart automatically"*.
    ///
    /// Reads this session's records and acts on each by [`crate::subagent_record::decide`]:
    ///
    /// * **mid-task** — its row is stamped with this daemon and one more resume, and the child is
    ///   spawned by [`HarnessTaskRunner::spawn_child`] under its own id with its recorded spec, so
    ///   its session is reopened from the store in its own worktree and its turn continues with
    ///   the restart notice;
    /// * **settled, never delivered** — a slot already holding the answer, which the parent's
    ///   watcher (armed by `Harness::restore_after_restart`) delivers as it would any settlement;
    /// * **not to be resumed** — the row is marked `abandoned` with the reason, which is the
    ///   sentence the parent is told.
    ///
    /// Stamping the row with this daemon is what keeps a second open of the same parent inside
    /// this daemon from bringing the child back twice.
    /// **Spawn one recorded child again under its own id** — the one body behind the automatic
    /// restore and `task_resume`. The row is stamped with this daemon and one more resume first,
    /// so a second open of the parent in this daemon leaves it alone.
    fn resume_run(
        &self,
        store: &crate::subagent_record::RunStore,
        run: &letibot_tokencore::store::SubagentRun,
        mut spec: letibot_tools::builtins::task::TaskSpec,
    ) -> Result<(), String> {
        // **A record from before names comes back under the name its listing gives it** — unique
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

    /// **Load and seat another model's tokenizer for a child** — the spawn half of
    /// [`LocalSwitch::OtherWeights`].
    ///
    /// The session switch loads the target's vocabulary itself, once per switch;
    /// this is the fan-out version, cached by GGUF path so five children on one
    /// model load its tokenizer once. The dialect is resolved against the
    /// vocabulary HERE rather than at the child's open, because a pairing that
    /// does not fit is a fact about the request (the operator's block names a
    /// model this tree cannot drive) and `start` answers those inline rather than
    /// parking them in a failed child.
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
    /// **Start the child and come back.** The body below is the work, and it
    /// runs on its own thread: the daemon executes a round's calls in order on
    /// one thread, so a `task` that waited for its child held every call behind
    /// it for the child's whole life — fifteen minutes, measured 2026-09-17, of
    /// four other calls sitting unfinished while the operator and the model both
    /// read it as a hang.
    ///
    /// What is NOT deferred is the failure to start: a bad role, a spec that does
    /// not parse. Those are answered by this call, because they are facts about
    /// the request rather than about the child.
    fn start(
        &self,
        prompt: &str,
        spec: &letibot_tools::builtins::task::TaskSpec,
    ) -> Result<String, String> {
        // **The depth cap, and it is refused by NAME rather than by absence** (R58).
        // `task` is seated on `m2_coder` unconditionally, so a model always sees the
        // tool and learns *why* it cannot use it here; the alternative — stripping
        // `task` from the seat at the limit — is the workaround this tree has already
        // paid for once (the `todo` note on `m2_coder`: 13 calls and ~15k tokens
        // emulating a tool that had not been given). `depth` is the session's own level,
        // so this refuses the spawn that would create level `depth + 1`.
        if let Some(why) = subagent_depth_refusal(self.base.depth, self.base.max_subagent_depth) {
            return Err(why);
        }
        // **The id is minted here rather than inside the body** — it is what this call returns
        // and the body has not run yet — **and it is minted UNIQUELY, which is the half that
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
        // **Recorded before the thread exists** (the operator: *"we should record them … and
        // restart automatically"*): a daemon that dies while this child is still opening must
        // know it had one. See `crate::subagent_record`.
        // **The child's name, unique among this session's children**: the one the caller gave
        // (`task`'s `slug`), else the derivation from the prompt `task_start` makes — `-2`, `-3` …
        // when a sibling already answers to it, because a name two children share resolves to
        // neither. **A child in a worktree of its own is named after its branch** (`agent/<slug>`)
        // and kept as that: the branch is already unique against the repository, and a name that
        // drifted from it would be two names for one child.
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

    /// **Bring back the children a daemon that is gone left mid-task** — see
    /// `crate::subagent_record` and [`HarnessTaskRunner::bring_back_recorded`].
    fn resume_recorded(
        &self,
        mode: letibot_tools::builtins::task::RestoreMode,
    ) -> Vec<letibot_tools::builtins::task::RestartedChild> {
        self.bring_back_recorded(mode)
    }

    /// **`task_resume`: bring back ONE child a daemon that is gone left unfinished** — and the
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
                // **A child whose record was never written cannot be resumed, and it says so.**
                // The spawn's own description is what a resume is given — the subtask, the role,
                // the placement, the worktree — and that is exactly the row that is missing. The
                // refusal names the two things that are still there rather than calling the child
                // a stranger.
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

    /// **Arrange the tree, then start the child** — the `task_start` door.
    ///
    /// The order is the point: the worktree is created *before* the child is
    /// spawned, and a path that already exists is refused by name and without
    /// spawning anything. The operator's ask, in their words: *"we will need a
    /// new tool - task_start or what that will arrange worktree, firecode and
    /// subagent"*. The two mistakes this prevents are a child spawned into the
    /// main tree and a child killed instead of corrected for being in the wrong
    /// place — so the placement is a fact the spawn carries (see
    /// [`letibot_tools::builtins::task::TaskSpec::worktree`]), not a thing the
    /// caller remembers to say.
    ///
    /// **The default is never the main tree.** A `task_start` that did not name
    /// the main checkout creates a fresh worktree at
    /// `<workspace>/.claude/worktrees/agent-<slug>` on branch `agent/<slug>`, cut
    /// from the base the caller named (or the repo's HEAD). The main tree is the
    /// exception, and it requires the explicit `main_tree` argument.
    ///
    /// **The build cache is shared deliberately, and nothing is written into the
    /// tree for it.** The child's commands carry `CARGO_TARGET_DIR` pointing at the
    /// main tree's `target/` ([`worktree_build_env`], set on the child's config at
    /// the spawn and put into its exec host's standing environment, the pairs that
    /// survive `env_clear`), because cargo's own lock serialises concurrent builds
    /// and a fresh worktree otherwise pays a cold build of the whole graph. The tree
    /// the child is handed is exactly the branch: `git status --porcelain` is empty.
    ///
    /// **The branch is the deliverable, and it is never pushed.** This method does
    /// not push, and will not: the branch is to be landed by a merge queue.
    ///
    /// **TODO(merge-queue): the merge queue does not exist yet.** The branch is
    /// left where it is and the answer says so, rather than inventing an interface
    /// for a queue that is not built. When the queue lands, this is the door it
    /// takes.
    fn start_worktree(
        &self,
        prompt: &str,
        spec: &letibot_tools::builtins::task::TaskSpec,
        worktree: &letibot_tools::builtins::task::WorktreeSpec,
    ) -> Result<letibot_tools::builtins::task::WorktreeHandle, String> {
        use letibot_tools::builtins::task::{
            WorktreeHandle, WorktreePlacement, slug_from_prompt, worktree_branch, worktree_path,
        };
        // **The depth cap, refused by name before any tree is arranged** — the same
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
            // **The refusal of an existing path, by name and without spawning
            // anything.** The tool pre-checks this; the runner re-checks it because
            // it is the one that creates the path, and a check the creator does not
            // make is a check that can be raced.
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
            // a spawn that fails, named — never a child run in the parent's tree and
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
            // **No file is written into the tree.** The build cache is shared through the
            // child's environment (`worktree_build_env`, at the spawn). This used to write
            // `.cargo/config.toml` here and "hide" it in `$(git rev-parse --git-dir)/info/
            // exclude` — inside a worktree that is `.git/worktrees/<name>/info/exclude`, which
            // git does not read, so every worktree showed `?? .cargo/`, a child's `git add -A`
            // committed it, and a repository that tracks its own `.cargo/config.toml` had it
            // overwritten.
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
        // **And the two facts the enqueue will need are kept, now that both exist.** The
        // handle is minted by `start`, and the placement and the ask are in hand here — so
        // this is the only moment at which *which child*, *which branch* and *what was asked*
        // can be recorded together. Which children are recorded at all is
        // [`PlacedChild::of`]'s rule, so the end-to-end test beside this file exercises the
        // same one rather than a copy of it.
        if let Some(record) = PlacedChild::of(&handle, prompt, &placement) {
            self.placed.lock().expect("placed children").push(record);
        }
        Ok(WorktreeHandle { handle, placement })
    }

    /// **Stop a subagent by interrupting the turn it is running.**
    ///
    /// The child spends its whole life inside `sub.submit`, and the one door that reaches
    /// a turn already running is that session's own steering — the same door a head's
    /// Esc-esc uses, addressed to the child's hub rather than to this session's. The hub
    /// is reachable because a subagent is a real registered session (that is what lets an
    /// operator attach to one), and it is found by the handle the model was given.
    ///
    /// **The address is the daemon's, and the daemon is the one caller the hub admits without a
    /// seat** — see [`letibot_sessionlog::hub::DAEMON_SUBMITTER`], whose arm in `Hub::submit`
    /// admits this name for an interrupt and for nothing else. `expected_seq` is `0`, which the
    /// hub reads as *no expectation*: a stop has nothing to say about where the reader is, and a
    /// stale mark would refuse the kill.
    ///
    /// **AND THE ANSWER DECIDES THE RETURN.** This function used to call `hub.submit` with a
    /// name the hub did not know and then format whatever came back into a sentence beginning
    /// *"interrupted the turn"* — so every subagent kill answered `Rejected { reason: "not
    /// attached" }` to a caller that was told the opposite. MEASURED 2026-10-05: the operator's
    /// three subagents were *"stopped"*, went on running, and their row counts grew (102 to 108)
    /// while the kill was in flight. A tool may not report a state change the daemon refused; it
    /// is the same claim-versus-fact rule the rest of this tree holds to, and it is why the
    /// match below is exhaustive over the daemon's answer rather than over our hopes.
    fn kill(&self, handle: &str) -> Result<String, String> {
        let slot = self
            .slots
            .lock()
            .expect("task slots")
            .iter()
            .find(|(h, _)| h == handle)
            .map(|(_, s)| s.clone());
        let Some(slot) = slot else {
            // **A child this session HAS, with nothing running it** — its record was never
            // written, so there is no slot and nothing to interrupt. Saying *no subagent in this
            // session* about a child `task_result` lists is the contradiction this arm removes,
            // and the cause is the store's own class rather than a guess: a child older than the
            // record was never due one.
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
                // **Marked only once the daemon said yes**, and the order is the point. The flag
                // is what makes `task_result` say *stopped* rather than *answered*; setting it
                // for an interrupt that was refused would make a running subagent look settled,
                // and setting it before the submit is how the old version could label a child
                // that was never touched. The child may still settle between the submit and
                // this line — with the acceptance in hand that reads as the stop it is, because
                // its turn is no longer running.
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

    /// **Say something to a session that is still working** — the operator's ruling, 2026-10-06:
    /// *"in the tree all subagents must be addressable by their parents. that is how live
    /// corrections delivered."*
    ///
    /// **The door is a session's own hub, and it is NOT parent-shaped.** The registry is the
    /// daemon's, keyed by session id, so `handle` may name **any live session**: a subagent this
    /// session started, or a peer — the operator's correction to the whole framing of this
    /// channel, verbatim: *"well gatekeeper is not a child so we are not doing up"*. A peer is a
    /// session in its own right, one session sends another session a message, and
    /// [`letibot_sessionlog::CommandKind::Message`] — a sender and a text, addressed at a session
    /// — is already exactly that. So the refusal below is for a session that DOES NOT EXIST and
    /// for nothing else; a live session that is not this session's child is delivered to, and the
    /// `from` it carries is what lets the recipient judge provenance.
    ///
    /// The address is `DAEMON_SUBMITTER` for the same reason it is for [`Self::kill`]: the sender
    /// is seated in another session's hub entirely. What it submits is a
    /// [`letibot_sessionlog::CommandKind::Message`] rather than a `Prompt`, and that difference is
    /// the feature — the recipient hears an agent's utterance at its next round boundary, recorded
    /// as an agent's (`from`, never the daemon: `Hub::submit`'s relayed-message arm keeps the
    /// author), instead of something the operator is supposed to have typed.
    ///
    /// **REFUSED BY NAME WHEN THE SESSION'S THREAD IS GONE, and the answer says WHICH delivery the
    /// sender got.** What this door used to do was refuse anything that was not a running turn,
    /// and its reason was a measurement rather than caution: a message rides the same queue as
    /// every other command, so a submission against a child whose turn had ended answered
    /// `Accepted` and then sat there, because *nothing drains a child's queue between turns* —
    /// the parent would have been told it had corrected a child that never heard a word of it.
    /// The operator met the other end of this: *"message was queued when you stopped and it didnt
    /// restart you"*, and then ruled on it: *"fix task_message - it should enqueue"*.
    ///
    /// **What changed is the sentence in the middle, and it changed by being made false rather
    /// than by being deleted.** A session's queue now HAS a between-turns reader, whichever kind
    /// of session it is: a subagent's own thread (`serve_child`'s `ChildCommand::Hear`, which
    /// submits through [`Harness::submit_a_parents_message`]), and — for a session the daemon
    /// itself holds, the main session — the daemon's worker, whose `Message` arm appends the row
    /// and starts the turn (`Sessions::dispatch`). So there are two deliveries and they are not
    /// the same sentence:
    ///
    ///   * **the recipient's turn is running** — it hears the message at its next round boundary,
    ///     as an agent's utterance, and keeps working;
    ///   * **it is between turns** — a turn is STARTED for it, and the message is what that turn
    ///     is about.
    ///
    /// **The order is the race handling: durable first, wake second.** The message is on the
    /// queue before the wake is sent, so a wake that is missed, or a turn that ends in the window
    /// between the state read below and the recipient's next look at its queue, still finds work
    /// waiting — the message is never `Accepted` into a queue that nothing will look at. The
    /// window is genuinely narrow and genuinely there (the turn can end between the read and the
    /// drain) and it is CLOSED by the wake rather than named and left: whichever of the two states
    /// the recipient was in, the queue entry and the wake are both delivered.
    ///
    /// **The two failures stay failures, by name.** A handle no session in the daemon answers to
    /// is refused: nothing was sent, and the sentence says which of the two kinds of handle this
    /// door takes. A child of THIS session whose own thread has ENDED is refused too, because a
    /// thread that has ended takes no wake — [`TaskSlot::has_exited`] is the same fact `stop_all`
    /// reads before submitting an interrupt nothing would answer — and that check happens BEFORE
    /// the submit, so "nothing was sent" is true when it is said. The one residual window is a hub
    /// that closes between the submit and the wake: then the message really is on a queue with no
    /// reader, and the answer says exactly that instead of claiming a delivery.
    fn send(&self, handle: &str, text: &str) -> Result<String, String> {
        // **A session that does not exist is the refusal; a peer is not.** `Registry::get`
        // answers for the whole daemon, keyed by session id, so a handle naming a live session
        // resolves here whichever session started it. What this sentence used to say — *"no
        // subagent `{handle}` in this session, or its session is gone"* — described the door as a
        // parent's list of its children, which is the framing the operator struck (*"well
        // gatekeeper is not a child so we are not doing up"*): it named a peer as something this
        // session does not have, and it said *gone* about a session that is very much there.
        // **A PARKED child is brought back first** (R68's idle-child policy,
        // `Config::child_park_after`): it went store-only because it was idle, not because it
        // ended, so a message to it is the "next prompt" that reopens it. It comes back as a
        // session the daemon's WORKER holds — the resume every stored session takes — and the
        // worker's `Message` arm runs the turn that hears this.
        //
        // **A thread that has ended takes no wake, and a message nobody reads is the lie this
        // door exists not to tell.** Checked before the submit, because after it the message is
        // on a queue and "nothing was sent" would be false — and before the registry lookup,
        // because a child whose thread ended is removed from the registry on the way out (R68.2)
        // and "no session is live" would hide WHY. A parked child's thread has ended too, and is
        // the one exception: it is reopened below. A hub the worker owns is a reopened child,
        // whose old slot's thread is not its reader.
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
        // **A child still opening has a slot and no session yet** — a `firecode` child is
        // registered only once its VM has booted and answered, which takes minutes. Saying *no
        // session is live* there is what made a model take a booting run for dead and start a
        // second VM doing the same work (the operator's stroppy session, 2026-10-14).
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
        // **Which of the two deliveries this is, read before the submit.** Both are real and
        // they are told apart by the child's state at the moment the parent spoke — a running
        // turn hears it at its next round boundary, a child between turns has a turn started.
        //
        // **And it is the SAME reading the rest of this change uses** — `Hub::turn_in_flight`,
        // not `SessionStatus::running`. That flag is the ROUND's (see `turn_in_flight`'s own doc),
        // so this door said *"it was between turns, so a turn has been started for it"* about a
        // child whose turn was very much running — mid-tool-call — while `task_result` and the
        // idle plan-check, reading the corrected fact, said it was working. Two doors, one child,
        // two answers is the failure this whole change is against, and this line is where the
        // third door (a parent's message) joins the other two.
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
                // **Durable first, wake second.** `submit` put the message on the child's
                // queue and did the two things a queued command does: it notified this hub's
                // own condvar — which is what a PARKED child's reader is blocked on — and it
                // rang the BELL, which is the daemon's worker. The bell is the wrong reader for
                // a child (the worker cannot run its turn, and `Sessions::dispatch` hands a
                // message back to the thread that can), so the wake below is sent as well, and
                // for the two things the queue alone cannot say:
                //
                //   * **whether the session is still there.** `Hub::submit` does not refuse a
                //     command on a closed hub — it queues it into a queue nothing will drain —
                //     so this return value is the only liveness answer this door has, and a
                //     `false` here is a message that will NOT be heard.
                //   * **the belt for a reader that is not parked on that condvar.** If the
                //     child's turn ends in the window between the state read above and its next
                //     look at its queue, the queue entry is what it finds and the wake is what
                //     makes it look; a wake that finds nothing settled runs no turn at all
                //     (`Harness::wake` answers `Ok(None)`).
                let woken = hub.wake_its_own_reader();
                // **`false` is not a delivery.** It means the hub is closed — `Hub::submit` does
                // not check that (it queues into a queue nothing will drain), so this return
                // value is the only liveness answer this door has. No turn will be STARTED for
                // the message: a child between turns has no reader left (`take_own_work`
                // answers `Closed` and its thread leaves), and a turn already running could
                // still take it at its next round boundary through its own steering poll — but
                // nothing after it will, which is not a correction a parent may rely on and not
                // one this door will claim.
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

    /// **Where the child has got to, which is two facts and not one.**
    ///
    /// The SLOT is the record of the child's first turn: `Running` until that turn ends, then
    /// `Done` with its answer or `Failed` with why. The READING is the child's present, taken at
    /// the moment it is asked for — a child that answered and is parked, one running the second
    /// turn a `task_message` started, and one that has wedged mid-call all read `Done` or
    /// `Running` in the slot and are three different states NOW.
    ///
    /// **So the reading is attached here rather than settled into the slot**, which is the whole
    /// reason this is not one line of `g.clone()` any more: a value written once at the turn's
    /// end would describe the child as it was then, for ever, which is exactly what *"still
    /// working"* was — the same sentence for a working child, an idle one and a wedged one.
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

    /// **Where a child has got to, for a reader** — [`Self::collect`], and for a child of this
    /// session that a restart left without a slot, what its record says.
    ///
    /// No slot is not "never started here". Such a child is still this session's: `task_result`
    /// with no `task` LISTS it (`children`), and `task_resume` refuses it with *"finished its task
    /// (done); read its answer with `task_result`"*. Answering `Unknown` made the third door say
    /// *"no subagent called … was started here"* about the same child (pg-noop, 2026-10-14). So
    /// the record answers ([`crate::subagent_record::recorded_status`]). The watcher keeps
    /// `collect`'s `Unknown`: a record is not a settlement arriving now, and a recorded answer
    /// handed to a watcher would be announced to the model as news.
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
            // **No record: its own session is the witness.** A spawn whose row the store refused —
            // or a child from before the store recorded any — still opened a session, and that
            // session is the only thing left that says this child existed. Answering `Unknown`
            // made the third door say *"no subagent called … was started here"* about a child
            // this session started and whose work is on disk; the sentence it gets now says which
            // of the two it is ([`crate::subagent_record::no_record_cause`]).
            None => match self.unrecorded_child(handle) {
                Some(child) => crate::subagent_record::a_child_with_no_record(&child),
                None => letibot_tools::builtins::task::TaskStatus::Unknown,
            },
        }
    }

    /// **A child's settlement was read** — `task_result` handed it to the model, or `digest` read
    /// its part reader — so its record is marked delivered and a restart does not announce it
    /// again. Only this session's own children are marked: a handle in the tree that another
    /// session started is that session's to be told about.
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

    /// **Whether this child has no record, and how far it got** — asked BY HANDLE, because that is
    /// the question `task_result` has when a model names a child, and the answer must be right for
    /// a child the LIST does not carry: the list is the children a record was due for, while a
    /// child from before the store recorded any is one a model may still name.
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

    /// **The session that started this one, if any** — the upward edge of the tree
    /// [`Self::children`] is the downward edge of, and the answer to
    /// `task_message {"task": "parent"}`.
    ///
    /// One field, because the runner is built per session and `base` IS that session's config:
    /// a child's runner carries `parent_session_id = Some(parent)`, a root's carries `None`. No
    /// registry lookup and no store read — the tree's own record of the edge is the config the
    /// child was opened with, which is also what the store's `session.parent_session_id` column
    /// was written from.
    ///
    /// **`None` is not "unknown"**, it is *this session is the top of its tree*: a root has
    /// nobody above it, and the tool refuses the handle by name there rather than sending a
    /// message to itself.
    fn parent(&self) -> Option<String> {
        self.base.parent_session_id.clone()
    }

    /// **Every child a handle or a name may resolve to**: the tree's slots, with each one's name
    /// and whether THIS session started it, and then this session's recorded children that a
    /// restart left without a slot (a child paused past the restore threshold is still one
    /// `task_resume` may be given by name). A slot with no name of its own — a settlement
    /// re-delivered after a restart — is named from its record.
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
                // **A tool's helper is not listed from its record** — it was never the model's
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
        // **And the children whose record was never written** — a spawn whose `subagent_run` row
        // the store refused. The record is what this list was built from, so such a child was
        // simply absent: not `working`, not `answered`, not `failed` (MEASURED 2026-11-01, a child
        // that ran six and a half minutes and left four commits). Its own session is the second
        // witness, and it is named by the same handle.
        //
        // **And it carries how far it got** (`subagent_record::how_far`), which is the one thing
        // its own rows can still say: the list used to print one fixed sentence for every child
        // that did not end on an answer, so a parent could not tell a child that died mid-`bash`
        // from one that never opened a session.
        for child in self.unrecorded_children() {
            if out.iter().any(|c| c.handle == child.id) {
                continue;
            }
            let lost = crate::subagent_record::how_far(&child);
            out.push(ChildName {
                handle: child.id,
                // **No name**: a child with no record was never given one that anything wrote
                // down — its session's `title` is the first line of its brief, not a slug — so it
                // answers to its handle, which is what the model was handed when it started it.
                slug: None,
                own: true,
                lost: Some(lost),
            });
        }
        out
    }

    /// **The child has finished, so its branch goes to be landed** — the merge queue's enqueue,
    /// on the runner that arranged the tree.
    ///
    /// The operator's ask, in their words: *"we need a gated merge to main, and worktree
    /// cleanup"*, and *"a permanent subagent that lazily starts as soon as task_start was
    /// finished and item put into the queue"*.
    ///
    /// # Why this is a runner's act and not a tool's
    ///
    /// The row needs the ask the child was given, the placement it worked in, and the store the
    /// queue lives in — and the tool layer has none of the three. The runner arranged the tree,
    /// so the runner is the one that can hand them over; see
    /// [`letibot_tools::builtins::task::TaskRunner::finished`] for the seam itself.
    ///
    /// # Two doors, one entry
    ///
    /// This is called from two places, and they are one act rather than two answers:
    ///
    ///   * **The child's own settlement**, inside `run_to_completion` — the true moment the
    ///     child finished. A host child parks after its turn and serves its own queue until the
    ///     daemon closes it, so *the thread ending* is not the finish; the answer being given is
    ///     (see `AnswerOnce`).
    ///   * **`task_result` on a `Done`** — the tool layer's one sight of a finish, and the door
    ///     that makes this callable by a fake runner in a test.
    ///
    /// The two write **the same row under the same id** (the child's handle), and `put_merge_entry`
    /// is an upsert, so the second is a no-op that reports the same entry. That is deliberate:
    /// the alternative — one door only — is either a branch that never lands because nobody
    /// collected the child, or a queue that depends on the model remembering to call
    /// `task_result`.
    ///
    /// # What is refused, and what is not
    ///
    /// * **A plain `task` child** answers [`Finished::NoBranch`]: it works in the parent's own
    ///   tree and has no branch of its own. Not an error — the ordinary case.
    /// * **A child of the main checkout** answers the same, by name: there is no branch to land.
    /// * **A session with no `--store`** cannot be enqueued at all, and that is a refusal rather
    ///   than a quiet `NoBranch`: the queue lives in `sessions.db`, so a daemon without one has
    ///   no queue, and a parent told *queued* about a branch in no queue is the lie this tree
    ///   keeps refusing to tell.
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

    /// **Stop every child this session still owns** — the downward edge of the tree.
    ///
    /// The operator's design, in their words: *"think about it like it is an erlang supervision
    /// tree. we talk to parents and they own lifecycle."* A session that is being stopped stops
    /// its children first, so nothing is left computing for nobody — which was measured, the
    /// night this was written: a stalled grandchild survived the parent that owned it, because
    /// the interrupt reached exactly one session.
    ///
    /// **Which children, and why each filter is a fact rather than a precaution**, is
    /// [`stoppable_children`]'s — pure, so it is asserted without a vocabulary on this box.
    /// What is left here is the delivery: [`Self::kill`] once per handle, which is the same door
    /// `job_kill` uses — the child's own turn is interrupted, or its serving loop is woken, and
    /// its OWN runner stops its children in turn. That is how one stop walks a whole tree
    /// without any session holding a pointer to its grandchildren.
    ///
    /// **What this is NOT: a restart policy.** Nothing here decides whether a child should be
    /// started again; see the TODO on [`HarnessTaskRunner`].
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

    /// **Wait until every subagent in this tree has left its own thread.**
    ///
    /// The fourth edge of the tree [`Self::stop_all`] is the third of, and the one that was
    /// missing until a SIGSEGV at exit said so. A child's thread is spawned detached —
    /// [`HarnessTaskRunner::start`] drops the `JoinHandle` — so nothing in the process can join
    /// it, and a caller that is about to let the process end had no way to tell a child that
    /// has finished from one still running. The cost is not theoretical: `exit()` runs the
    /// loader's finalizers for every loaded `.so`, so a child still inside `libllama`'s
    /// tokenizer when `_dl_fini` finalizes `libggml-cuda` takes SIGSEGV — **after every test in
    /// the binary has printed `ok`**, at ~3–40% of runs, which is exactly the shape that gets
    /// blamed on the next person's change. See [`TaskSlot::wait_until_exited`].
    ///
    /// **The whole tree's slots and not only this session's children.** `slots` is the tree's
    /// list (`Parts::tree_slots`), and the caller here is a shutdown: the hub is already closed,
    /// so every thread in the tree is on its way out, and a wait that covered one level would
    /// leave a grandchild's thread inside the tokenizer at exit. Stopping is the direction that
    /// must be per-level ([`stoppable_children`]); waiting is not.
    ///
    /// **A deadline, and it is the caller's.** Waiting is the caller's act and so is giving up:
    /// this returns `false` rather than blocking forever, so a shutdown that cannot be waited
    /// out says so instead of hanging. `true` is the honest "nothing of mine is running".
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

/// **Which of a session's slots are children IT may stop** — the ownership filter, on its own so
/// it can be read and asserted without a harness, a vocabulary or a live child.
///
/// Two filters, and each one is a fact rather than a precaution:
///
/// * **Owned by this session.** `slots` is the TREE's list (`Parts::tree_slots`), shared on
///   purpose so any ancestor can `task_result` any handle in its tree — but stopping is the other
///   direction, and a runner that stopped every entry it could see would stop its siblings'
///   children (and, from a root, its own children's children) in the name of a stop that was
///   never theirs. [`TaskSlot::owner`] is what tells them apart, and it is the field this whole
///   function exists for.
/// * **Not exited.** A child that has ANSWERED is still alive — a host child parks in
///   [`serve_child`] and can be asked more, and it may own grandchildren at that moment — so
///   "settled" cannot mean "gone". A child whose thread has ended is skipped: there is no
///   reader left for an interrupt, and a stop reported for one would be a claim with nothing
///   behind it.
///
/// **The children `me` started that have not answered yet** — still on their task. Not
/// [`stoppable_children`]: a child that has answered and sits idle between turns has not exited,
/// and holding for it would hold for ever. What the hold waits on (a child with live children is
/// not done).
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

/// **Where a subagent's ask goes: the tree's ROOT, and the answers that settle it.**
///
/// One function because two decisions are one fact — *which session the card is posted to*
/// and *which sink can settle it* — and separating them is how a card ends up on a queue
/// whose only listener cannot answer it. It is split out of the spawn so the addressing can
/// be tested without a model turn, which is the only way anything inside that function can.
///
/// **The root is read, not walked for.** `tree` is the watcher set a child of this session
/// joins, and `tree_root` is the ROOT it inherited: the one session in a tree that has a HEAD —
/// the session `Sessions::open` holds and a card can be drawn by. **The card and the bell answer
/// two different questions and no longer share a field**: a bell now names the session that OWNS
/// a settlement (`jobwatch`'s module header), and a card needs the session that can ANSWER one,
/// so `tree_root` reads its own field. `None` is a session with no tree above it, and then its
/// parent IS the tree's root.
///
/// The hub is resolved by that id. `None` — a root this daemon does not hold, or a tree with
/// no head-wired adjudicator — is a refusal by name rather than a card nothing could settle.
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
    /// **A runner with only the ENQUEUE half wired** — the seam the end-to-end test needs, and
    /// the whole of the visibility change that test required.
    ///
    /// [`HarnessTaskRunner`] is private and is built in exactly one place,
    /// [`Harness::open_with_registry`], which needs a vocabulary, a live subagent host and a
    /// gate. [`Self::enqueue_entry`] and [`Self::enqueue_finished`] need none of those three:
    /// they read four facts — `placed` (what [`Self::start_worktree`] recorded), `base.store`
    /// and `base.session_id`, `merge_store`, and `registry` — and the other eleven fields
    /// exist for the SPAWN. So before this, the glue that turns a finished child into a queue
    /// row was executed by no test at all: every piece it calls (`PlacedChild::of`,
    /// `entry_for_finished`, `Store::open`, `Store::put_merge_entry`, the broadcast) had a test
    /// of its own, and the wire between them was only read.
    ///
    /// **Why a constructor and not a widened API.** The alternative was `pub` on the struct,
    /// its fields and the two methods so that an integration test could reach them, which
    /// widens a daemon-internal type into the crate's public surface for the sake of a test.
    /// This does not: the struct, every field and both methods keep the visibility they had,
    /// and `#[cfg(test)]` means the function is not compiled into a binary at all. The test
    /// that uses it is `queue_e2e`, beside this file and in this crate for the same reason.
    ///
    /// `parts` is taken rather than assembled because the two fields nothing here reads
    /// (`vocab`, `wiring`) have no cheap stand-in — a [`Vocab`] is a loaded GGUF — and the
    /// tree's own precedent for that is `compact.rs`: a test that needs the vocabulary and
    /// not the server says so and skips when the GGUF is absent.
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

    /// **What the daemon can see about one child RIGHT NOW**, or `None` when it has nothing to
    /// report — a handle this daemon holds no session for, or a child whose own thread has ended
    /// (a thread that is gone has no present; what the parent wants from it is its answer, which
    /// is what the slot already carries).
    ///
    /// `turn_started` is the SLOT's own record: a `Running` slot is a child whose first turn has
    /// not settled, which is the daemon's record that a turn is in flight. The hub's half of the
    /// same fact (`Hub::turn_in_flight`) is the only thing that sees a turn the child runs AFTER
    /// its first — the second turn a `task_message` starts, when the slot has already settled and
    /// the journal says `done` — and `crate::sessions::child_reading` is where the two are ORed.
    /// **This is that function and not a second fill**, so `task_result` and the idle plan-check's
    /// sentence about one child cannot come to different answers.
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

    /// **This session's point, as it stands now** — see the field.
    fn point(&self) -> letibot_tools::mode::Mode {
        *self.point.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// **The child has finished: enqueue its branch, and say so where a person is looking.**
    ///
    /// A thin wrapper around [`TaskRunner::finished`], and it exists for the two things a
    /// caller inside `run_to_completion` cannot do for itself:
    ///
    ///   * **It is infallible from the child's side.** The child's turn is over either way, and
    ///     a merge queue that could not be reached must not turn a finished subagent into a
    ///     failed one — the child's answer is already settled by the time this runs, and
    ///     `task_result` must keep returning it. So a refusal is printed on the parent's own log
    ///     (where the operator watching a tree is looking) rather than propagated.
    ///   * **It says what happened, unprompted.** A `task_start` child's branch being enqueued
    ///     is news the parent did not ask for and would otherwise learn only by calling
    ///     `task_result` — and a merge queue nothing announces is a merge queue nobody watches.
    ///
    /// # Two readers of one fact, and they go to two different places
    ///
    /// The `Warning` goes to **the parent's hub** and the `QueueMoved` goes to **every
    /// session's log** ([`letibot_sessionlog::registry::Registry::broadcast`]), and the
    /// difference is the whole of the events decision:
    ///
    ///   * *Your child's branch is queued* is a fact about THIS conversation. It belongs in the
    ///     log the person reading this conversation is attached to, and nowhere else — the same
    ///     rule `Subagent` and `JobSettled` follow.
    ///   * *The queue now holds this entry* is a fact about the daemon, and a head attached to
    ///     any other session can open the queue pane. So the structured event is broadcast, and
    ///     the pane that folds it is the pane's own work.
    ///
    /// The enqueue itself is idempotent by id (the handle), so a later `task_result` on the
    /// same child is a no-op rather than a second entry — and the events are emitted once per
    /// call rather than once per entry, which is why the second notice is not a second
    /// `QueueMoved`.
    ///
    /// [`TaskRunner::finished`]: letibot_tools::builtins::task::TaskRunner::finished
    pub(crate) fn enqueue_finished(&self, sub_id: &str) {
        match self.enqueue_entry(sub_id) {
            Ok(Some(entry)) => {
                // **The pane's move**, for every session — see the doc above for why this one is
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

    /// **The write, and a store that will not open is a refusal rather than a silent drop.**
    ///
    /// The half of [`TaskRunner::finished`] that does the work, on its own so the two callers
    /// can have different shapes of the same answer: the trait method needs the tool layer's
    /// `Finished`, and the enqueue's own notice needs the row it wrote — for the
    /// `MergeEntryAdded` it broadcasts.
    ///
    /// `Ok(None)` is *there is no branch of its own to land*: a plain `task` child, a child of
    /// the main checkout, or a handle this session never placed. It is not a failure, and the
    /// caller says nothing about it.
    ///
    /// [`TaskRunner::finished`]: letibot_tools::builtins::task::TaskRunner::finished
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
        // **The row, built by the queue's own function** — the repository the worktree is in,
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

    fn run_to_completion(
        &self,
        sub_id: &str,
        prompt: &str,
        spec: &letibot_tools::builtins::task::TaskSpec,
        slot: &TaskSlot,
        progress: &mut dyn FnMut(&str),
        restart: Option<&str>,
    ) -> Result<String, String> {
        use letibot_tools::builtins::task::Placement;
        let role = spec.role.as_str();
        // **The downgrade is a union**: whatever this session was denied, its
        // children are denied, plus what this call asks. A downgraded session
        // cannot spawn a wider child by naming a wider role.
        let downgrade = self.base.downgrade.and(&spec.downgrade);
        // Placement: a VM is booted on a copy of this session's workspace by the
        // child's own `open` (see `firecode.rs`); a boot that fails is a spawn that
        // fails, named — never a subagent run on the host and called a VM.
        //
        // **A child of a remote session runs on the same host** unless it asks for another
        // placement — the contract's rule, spelled the way a VM's children already follow it
        // (*"children live in the same vm"*). `host` in a remote session's mouth is *"my own
        // boundary"*, which for that parent is that machine.
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
        // **A VM holds one repository** — the operator: *"VMs are per repo anyway — at least must
        // be"*. A plain `task` placed in a VM copies the session's workspace, and a session above
        // several repositories has no one tree to copy: the stroppy children copied the folder.
        // So the model names it (`repo`), and the copy is that repository or nothing.
        let vm_copy = vm_copy_for(
            placement == Placement::Firecode,
            spec.worktree.is_some(),
            spec.vm_source.as_deref(),
            &self.base.workspace,
        )?;
        // A subagent is a real session: mint its id and create its hub. **The id was already
        // minted by [`HarnessTaskRunner::start`]** — it is what that call returned — so what
        // arrives here is a handle, and it is unique because [`mint_sub_id`] made it so. The
        // nanosecond in it is what keeps a subagent minted after a daemon restart from
        // colliding with a persisted one (a bare counter resets, and `adopt` checks only the
        // live registry, so a collision would RESUME the old subagent instead of spawning a
        // new one); the ordinal the mint can append is what keeps two children of one
        // millisecond — or of one nanosecond — from sharing the handle `task_result`
        // collects by. The title is the subtask's first line, so a picker row says what the
        // subagent was for.
        let spawned = std::time::Instant::now();
        let sub_id = sub_id.to_string();
        let title = subagent_title(prompt);
        // The subagent seats the role it was asked for — any seat this build knows
        // — and coder when none was named, which is the `task` tool's own default.
        // A role this build does not know is refused by name rather than seated
        // as coder: a survey asked for as `researcher` and run as a coder would
        // be a subagent with more than it was meant to have.
        let seat = if role.is_empty() || role == "coder" {
            Seat::Coder
        } else {
            Seat::parse(role)?
        };
        let parent = self.base.session_id.clone();

        // Publish the subagent's state on the **parent's** hub, so a head attached to
        // the parent sees the spawn and finish without subscribing to the subagent's
        // own hub. Same three states the journal records, same prompt.
        //
        // **`task` is the whole subtask and `prompt` keeps its old two-meaning shape**
        // (title on the opening states, the answer's first line on the finish) so a head
        // older than the field sees exactly what it saw before. A new head reads the row
        // from `task` and the subtitle from `answer` — see `SessionEvent::Subagent`.
        // **THE CHILD'S OWN MODEL, when it was given one** — the operator's ask, 2026-10-05:
        // *"I want to be able to have subagents using different models. say you deepseek
        // should be able to run local model"*, and the other way: *"local qwen in main
        // session should be able to run cloud glm"*. Resolved through `models_choice`, the
        // same door `/models` uses, so a name a person can type is a name a child can run
        // on and a key the picker finds is a key the spawn finds. A refusal is the tool's
        // own sentence and the spawn does not happen — never a child quietly run on the
        // parent's model while the record says otherwise.
        // **EVERY spawn goes through the model door — the absent `model:` case included.**
        //
        // This resolved a model only when one was NAMED, and a spawn without one kept
        // `sub_cfg`'s inherited window and `ledger_scale`: the PARENT's, which for a session on
        // a provider is the cloud model's number, while what answers the child is the daemon's
        // own server. Three children died that way (see [`child_window_refusal`]). `local` is
        // not a guess about the absent case — it is what `models_choice` and `/models local`
        // already mean by it, and the child's `provider: None` below is that same door.
        // **The call's own word for the model, then the project file's for this seat, then
        // the parent's model.** The two leticode keys are read here and nowhere else: the
        // call's `model:` beats `[roles.<seat>] model`, which beats the file's
        // `subagent_model`, which beats the inheritance below. Read at the spawn rather than at daemon start because the seat is
        // only known now — and read at all because both keys were parsed, disclosed and
        // answered by nobody until this line, which is the defect the feature is written
        // against. The label the refusals below name is this one, so a child stopped for its
        // window says the model the PROJECT chose for it.
        //
        // **Then the PARENT's model, not `local`** — the operator, 2026-10-07, after a DeepSeek
        // session on a Mac with no model server spawned a child with no `model:`: *"so if no
        // local config, and no model - default to the same"*, and *"local - anyway must be
        // configured"*. `local` as the silent default was a guess that this box serves one,
        // and where it did not the child was refused before it opened. A parent on its own
        // server still gives `local`, which is the same model; a parent on a provider gives
        // that provider and model, through the same door a named one takes — so its window is
        // the catalogue's, or the parent's own via `runs_on_the_parents_model`. `local` is now
        // only ever a choice somebody wrote.
        let named = spec
            .model
            .as_deref()
            .or_else(|| self.base.leticode.spawn_model(seat.as_str()));
        // **And measure the local window when nobody remembered one.** `retune_window` fills it
        // *on the way out to a first provider*, so a session that STARTED on one has never
        // measured its own server — this session had not. `served_ctx` is the same `/props`
        // read the daemon's own start makes, so a `local` child is given the server's number
        // rather than `None`, which is no wall at all.
        let local_window = {
            let remembered = *self.local_window.lock().unwrap_or_else(|e| e.into_inner());
            match remembered {
                Some(w) => Some(w),
                None => Some(letibot_turn::serving::served_ctx(&self.base.endpoint)),
            }
        };
        // **No model named is the parent's model** (main, 65d05eb: a child with no
        // `model:` runs on what its parent runs on — `inherited_spawn_model`), and a
        // NAMED one goes through the same door `/models` uses. Both doors produce
        // the four facts a spawn needs, and neither is allowed to guess.
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
        // **A declared local model: decide the weights, then measure the wall.**
        //
        // One `/props` probe decides it, from the same function a session switch
        // uses rather than a second copy of the reasoning. Same weights and the
        // child tokenizes with the parent's vocabulary — one daemon, one GGUF.
        // Other weights and the child gets THEIR vocabulary and THEIR dialect,
        // loaded and seated here, because a child rendering qwen's template onto
        // glm's weights is the same silent corruption a session switch refuses to
        // commit — and the child is the case nobody is watching.
        //
        // And the window is this box's, not the daemon's. Measured on this fleet the
        // day it was written: the same 27B GGUF at `n_ctx` 262144 on `127.0.0.1:8080`
        // and 57344 on `192.168.1.78:8082`. A child seated on the smaller one while
        // planning against the larger would run off the end of the KV cache with every
        // check it makes saying there is room — which is why a block that states no
        // window is probed here rather than allowed to inherit.
        let mut sub_foreign: Option<ForeignWeights> = None;
        let (sub_endpoint, sub_window) = match &sub_local {
            Some(m) => {
                let want_at =
                    parse_local_url(&m.url).map_err(|e| format!("[model.\"{}\"] {e}", m.name))?;
                // The switch's own decision, on the parent's vocabulary — which is
                // optional the same way it is everywhere else: a byte-vocabulary
                // parent cannot give a child SAME weights (there are none), so the
                // child gets the target's own, loaded here.
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
        // **And a window nobody can name may still be the parent's own — if the child runs on
        // the parent's own MODEL.** Same model, same window: that is the one case where the
        // parent's number is not a guess about a different server, and it is the case the
        // catalogue cannot answer (an explicit `--context-window` on an uncatalogued model).
        // A child on a DIFFERENT model never inherits it, which is the defect being fixed.
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

        // **`opening`, not `running`, until the child is actually open.** A spawn
        // that said "running" from this line was listed in the subagents pane
        // while its VM was still copying the workspace — for the operator, a row
        // they could press Enter on that led to an empty conversation (measured
        // 2026-09-16). `opening` is a row that says what is happening and that
        // there is nothing to attach to yet; `running` is published below, after
        // the harness is open and the hub is in the registry.
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
        // **Every early return from here on records the failure rather than leaving a
        // "running" row forever** — and it settles the child's slot as it does, because this
        // function no longer returns to a caller that settles for it (see [`AnswerOnce`]).
        // **A worktree child's every ending carries what its branch does not hold** — the answer
        // and every `fail` below alike, because a child that dies is the case the report was
        // about: *"the child's death cost the work nothing except its own account of it — and
        // that is only true because the worktree was checked"*. See [`deliverable`].
        let the_deliverable = spec
            .worktree
            .as_ref()
            .and_then(deliverable::Deliverable::of);
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
            // **The box that will answer this child**, which is not the daemon's own
            // when the child was seated on a declared local model. The row is what the
            // picker and the store show, and a row naming the wrong address is the
            // same silent-provenance defect as a model name that does not match.
            endpoint: sub_endpoint
                .as_ref()
                .map(|e| e.authority())
                .unwrap_or_else(|| self.base.endpoint.authority()),
            workspace: self.base.workspace.display().to_string(),
        };
        // Built, not registered: nothing can switch into it, and the worker is not
        // told to open it, until `open_with_registry` below has succeeded.
        let sub_hub = self.registry.new_hub(sub_id.clone());
        // **This thread is the session's one reader** (R68.1) — marked before the hub can be
        // adopted, so there is no instant in which the daemon's worker can see the child and take
        // a command off its queue. See `letibot_sessionlog::hub::Owner`.
        sub_hub.owned_by_its_own_thread();

        let point = self.point();
        let mut sub_cfg = Config {
            session_id: sub_id.clone(),
            title: title.clone(),
            seat,
            // **The caller's word** — the only source that exists before the child does (it is
            // minted, configured and opened before anything registers it). The rule itself is
            // `Registry::parent_for`, applied at the child's open.
            parent_session_id: Some(parent.clone()),
            // **The parent's point, read LIVE and not off `base`.** `base` is the config as it
            // stood at the parent's open, and nothing updates it — so a child built from it came
            // up at the point its parent had *opened* at, which is the operator's measured
            // defect of 2026-10-06: session at `allow-all` (consented), children at
            // `automode-edits`, every child's `bash` asking a person a question the parent had
            // already answered. What travels is the point the session is at; whether this child's
            // SEAT can carry it is the child's own question, answered at its open by
            // `Mode::inherited_by`, which clamps and says so rather than refusing to open.
            mode: point,
            // **One level down, incremented once and only here** (R58). It rides the
            // config the child is built from, the way `unconfined` and the workspace do,
            // so a grandchild's `depth` is 2 without anything walking a parent chain —
            // and `HarnessTaskRunner::start` refuses the spawn at the cap before this is
            // reached, so a config at the cap never gets a child to increment for.
            depth: self.base.depth + 1,
            downgrade,
            placement: placement.clone(),
            // **The child's own model, when it has one.** `provider` is the whole switch:
            // `None` is the daemon's own server, exactly as `/models local` means, and a
            // `ProviderConfig` is a cloud preset whose key `models_choice` already checked.
            provider: sub_provider,
            ..self.base.clone()
        };
        // **The skills this child was started with**, when the call named any — the child's own
        // list REPLACES the one it inherited through `..self.base.clone()`, so a parent that was
        // started with three does not hand all three to a child asked for one.
        if !spec.skills.is_empty() {
            sub_cfg.skills = spec.skills.clone();
        }
        // **The child's prompt is composed for ITS seat, at its open** (R93.G5) — not the
        // parent's finished prompt, which carried the parent's own-session paragraph (a
        // reviewer under the queue session was told "You are the review queue") and the
        // fabric block naming `flowy` and `skill` doors a child does not hold. A parent
        // running on a prompt it was GIVEN (`seat_prompt: None`: a test's fixture,
        // `--system`) hands that prompt down as before.
        if sub_cfg.seat_prompt.is_some() {
            sub_cfg.seat_prompt = Some(crate::config::SeatPrompt::default());
        }
        sub_cfg.seat_facts = None;
        // **The worktree `task_start` arranged is the child's workspace**, not the
        // parent's tree. The placement rides the spec (see `TaskSpec::worktree`), and
        // a child whose workspace is not the worktree the tool just reported is the
        // same silent provenance defect as a model name that does not match — the
        // child would be working in the parent's tree while the answer said it was in
        // its own. `None` is a plain `task`, which keeps the parent's workspace.
        if let Some(wt) = &spec.worktree {
            sub_cfg.workspace = std::path::PathBuf::from(&wt.path);
            // **And the main tree's build cache, through the child's environment** — never a
            // file in its tree. Computed from the worktree itself, so a child brought back after
            // a restart (`RecordedSpec`) gets the same pair. The main-tree exception builds in
            // its own `target/` already and is given nothing.
            sub_cfg.build_env.retain(|(k, _)| k != "CARGO_TARGET_DIR");
            sub_cfg.build_env.extend(worktree_build_env(wt));
        }
        // **And the tree a VM placement copies**, when the caller named one that is not the child's
        // workspace — the same route, and the same reason: the spawn is the one place that turns a
        // spec into a session, and a child whose VM copied a different tree from the one the
        // caller asked for is a record that does not match what happened. `Harness::open` reads it
        // through `Config::vm_copy_source`. `None` is every other caller, and it leaves the field
        // as the parent had it — which for a plain `task` is nothing at all, so the child copies
        // its own workspace exactly as it did before this field existed.
        if let Some(src) = &vm_copy {
            sub_cfg.vm_source = Some(src.clone());
        }
        // **A child of a session in a VM lives in that VM** unless it asks for one of its own —
        // the operator: *"children live in the _same_ vm"*, and *"child starts grandchild based
        // on its world"*. `host` is the tool's *"your own boundary"*, which for a session in a VM
        // is the VM; the stroppy grandchild that read *"You are in a firecode microVM"* in its
        // prompt ran on the operator's Mac instead, asking for every command. The same VM down a
        // whole chain: an attached session's children attach to the VM it is attached to.
        // Named the way the parent's backend named it: its worktree when that was mounted, its
        // copy under the cache otherwise (`firecode::project_for`).
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
        // **And the foreign weights' own vocabulary, dialect and marker**, when the
        // declared model serves other weights: the child's engine, its stable
        // prefix and its session row are all built from these three, so a child
        // on `local.glm` from a qwen daemon is a glm child in every part and not
        // a qwen child posting to a glm port. `sub_cfg.dialect` is also what the
        // child's OWN spawns and switches will compare against, so the fact
        // travels down the tree.
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
        // **And the project's parameters for the role this child was seated on**, which are
        // the last word on how it samples: the file beats the `[model.*]` profile applied
        // just above, the way the project beats `~/.config/letibot/` everywhere else. They
        // touch `sampling` and nothing else — least of all the window set one statement
        // above, which belongs to the box that answers this child.
        apply_spawn_parameters(&mut sub_cfg, &self.base.leticode, seat.as_str());

        // **And say which wall the child plans against, and WHICH TREE its VM is booted on**, in
        // the line a person is already reading. `config.rs` records the other half of this bug in
        // its own words — *"the silent half of this bug was that nobody could tell which window was
        // in force"* — and for a child, until now, NOTHING recorded it: not the store, not the log,
        // not this line. It is also what would have made the three dead children legible in one
        // glance instead of after a query against the store.
        //
        // **The tree is read off the child's OWN config, by the same resolution the copy uses**
        // ([`Config::vm_copy_source`]), which is why this line is published HERE rather than beside
        // the `opening` row: the sentence used to say *copying the workspace*, which was a lie the
        // moment a session was seated a level above its repository — the reviewer's copy is the
        // ENTRY's repository and a folder-seated session's is the repository below it. A copy that
        // is refused at the ceiling is a failure this line is the first word of, so it has to name
        // the tree the copy is actually made from.
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

        // Reassemble the shared parts so the sub harness can borrow them for the
        // duration of this call. Cheap: the vocabs and the wiring are already `Arc`.
        //
        // **Except when the child was seated on other weights**: then the
        // vocabulary and the wiring are the TARGET's, from the decision this
        // spawn already made, and every other part — the mode store, the task
        // journal, the tree — stays the daemon's, because a child on another
        // model is still a child of this tree.
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
            // **And the daemon's board map is the one thing a child must SHARE exactly** —
            // its own board lands here at its open, and its parent's `todo_write` finds it
            // through the same map (`crate::child_todos`). A fresh map per child would make a
            // grandchild unaddressable by everybody above it.
            child_boards: self.child_boards.clone(),
            reviews: self.reviews.clone(),
            // **The child joins this tree's watcher set** (R58): the parent's own set, which
            // carries the tree's ROOT (for a permission card) and the level above (for what this
            // session cannot drain, and for the ring that says so). The parent is a root at depth
            // 0, and at depth ≥ 1 the parent's set already carries the same root from the same
            // field one level up, so the card reaches the head with no parent-chain walk.
            tree_watch: self.tree_watch.lock().expect("tree watch").clone(),
            // **And the child shares this tree's handle list** (R58), the other half of
            // *"every ancestor's handle can be collected from any level"*: a grandchild
            // pushed into this list is collectable and killable by the root, which is the
            // session the tree's wake is delivered to.
            tree_slots: Some(self.slots.clone()),
            // **And the tree's head answers** (R58's ask route). A child has no head, so what
            // reaches an `ask` is posted to the tree's ROOT — and the root's `Answers` is
            // what settles it. Handed down through `Parts` exactly as the watcher set and
            // the handle list are, so a grandchild reaches the same root with no
            // parent-chain walk. `None` is a tree with no head-wired adjudicator, and the
            // child's adjudicator then refuses by name instead of posting a card nothing
            // could answer.
            tree_head: self
                .head_answers
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
        };

        // **The child's adjudicator: its asks surface at the tree's ROOT.** The operator's
        // ruling, and the whole of what changed here — a child used to deny every `ask` on a
        // policy (`a subagent has no operator to ask`), and now the card goes to the session
        // whose head exists and the operator's answer comes back as the child's decision.
        // The addressing itself is [`subagent_ask_target`], which says why the root comes
        // from the watcher set rather than from the parent chain.
        let (sub_tree_root, sub_head) = subagent_ask_target(
            &self.registry,
            parts.tree_watch.as_deref(),
            &parent,
            parts.tree_head.as_ref(),
        );
        let sub_adjudicator = crate::answers::SubagentAdjudicator::new(
            letibot_sessionlog::event::SubagentAsk {
                handle: sub_id.clone(),
                // The task's first line — the same title the picker row and the parent's
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
        // Open. Now it is a session a head can switch into, and now it is running.
        //
        // **Adopted into the REGISTRY and not into `Sessions::open`, and that difference is R58's
        // whole constraint — and NOT a hole any more.** A head can peek at this session and attach
        // to it (both go through `registry.resolve`), while the daemon holds no `Harness` for it:
        // `Sessions::wake` cannot run its turn. What that used to mean is that a settlement rung
        // for a session created HERE was discarded, which made depth 2 a hole — the child's own
        // watcher rings the child, and nobody answered. **The daemon serves it by handing the wake
        // to the thread that owns it** (`Hub::wake_its_own_reader`, answered by `serve_child`), so
        // the ring names the session that owns the settlement and the delivery is real at every
        // depth. See `Sessions::wake`.
        self.registry
            // **Cloned, because the serving loop below needs it back**: this hub is the child's
            // queue, and `serve_child` blocks on it until the daemon closes it — so the same
            // `Arc` the registry was given is the one the child serves from.
            .adopt(sub_hub.clone(), title.clone(), wiring, Some(parent.clone()))
            // **A taken id here is a MINTING bug, and it says so.** `mint_sub_id` is supposed
            // to make this unreachable; if it is reached, the sentence has to name the id and
            // say what it costs, because the quiet symptom is a child's whole reply that
            // cannot be collected — `task_result` finds the first slot under a handle, so the
            // second child under one id is unaddressable (leticl, 2026-10-04). Reported as
            // what it is rather than as the child failing, which would look like the child's
            // own fault.
            .map_err(|e| {
                fail(format!(
                    "the id `{sub_id}` was already taken ({e}). That is a MINTING bug, not \
                     this child failing: the id is the handle `task_result` collects by, so a \
                     duplicate makes a reply unreachable. The mint has to be made unique — see \
                     `mint_sub_id` — rather than the reply written off."
                ))
            })?;
        // **From here every exit of this thread closes the session and takes it out of the
        // registry, through this one guard** (R68.2). Before it, three exits left the hub
        // registered with nobody reading it — a failed first turn, a VM placement returning
        // before `serve_child`, a stop — and `wake_route` then handed wakes, messages and
        // interrupts into a void as `HandedOn`; the fourth (a normal end) never came at all.
        // Declared AFTER `sub`, so it drops FIRST: the hub is closed and the registry's closures
        // into the harness (`runs`, `prompts`, `notes`) are gone before the harness itself drops,
        // and nothing is left holding its `HostProcesses`.
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
        // **And if it is NOT at this session's point, say so where the person is looking.**
        //
        // The child's own settings row carries the same sentence (`child_mode_source`), and a
        // row is read by whoever opens that child's pane. This line is the half the parent's
        // reader gets: the person watching a tree of children is the one who has to know that
        // one of them is asking about things its parent already answered — or that one of them
        // cannot exec, and why.
        if sub.config().mode.name != point.name {
            progress(&format!(
                "subagent {} opened at `{}`, not this session's `{}` — {}",
                letibot_sessionlog::registry::short_id(&sub_id),
                sub.config().mode.name,
                point.name,
                sub.mode_source()
            ));
        }

        // **Through the seam every session has — the child's whole defect was that
        // this call was a bare `submit`.** The operator's requirement, in their
        // words: *"subagent is a normal session. it must be able to compact on tool
        // call overrun"*. A bare `submit` returns `ContextWall` as an error and
        // nothing behind this line reacts to it — the child's task failed with a
        // notice promising a compaction nothing attempted (measured 2026-10-06:
        // 207 rounds, 1,131,717 of 1,146,137 tokens, one transcript, no fork).
        // [`Harness::submit_as_a_normal_session`] compacts at the wall and
        // continues on the summary, bounded; a wall it cannot dig out of still
        // comes back here as the honest failure it is.
        // **The task is the child's first turn, through the shared tail with the child's own
        // owner** (R68.4) — the same owner every later turn of this thread uses, so its title,
        // its monitors and its sweep are the child's from the first turn on.
        let mut turns = ChildTurns::new(&sub, &sub_id, crate::sessions::TodoNagClock::new())
            .with_record(slot.record.clone());
        // **The branch is the deliverable, said in the brief and checked as each turn ends.** The
        // rule rides the child's task (see `worktree_brief`), and the same fact is read when the
        // turn is about to end (`Harness::set_deliverable`): one nudge if the tree is dirty.
        //
        // **Set for a RESUMED child too**: its placement comes back with it (`RecordedSpec`
        // keeps the worktree), and a child brought back after a restart that is not checked
        // at its turns' ends is exactly the child the deliverable rule exists for — it was
        // interrupted mid-work, so its tree is the likeliest of all to hold uncommitted work.
        sub.set_deliverable(the_deliverable.clone());
        let brief = match &the_deliverable {
            Some(d) => format!(
                "{prompt}\n\n{}",
                letibot_tools::builtins::task::worktree_brief(&d.path, &d.branch)
            ),
            None => prompt.to_string(),
        };
        // **A RESUMED child carries on; a fresh one is asked.** `restart` is the notice a child
        // brought back after a daemon restart reads (`crate::subagent_record`): its session was
        // reopened from the store under the same id just above, so the conversation it is
        // continuing is its own (its brief, the worktree rule included, is already in it). A
        // child whose daemon died before its first row was written has nothing to continue, and
        // is asked its task as if for the first time.
        if restart.is_some() {
            // **And a child of a child comes back with its parent** — this session's own records
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
                // **Its own children end with it.** A child whose first turn FAILED while its
                // children still worked left them running with nobody to report to — and a child
                // placed in a VM keeps its VM up for as long as it runs. pg-noop, 2026-10-14:
                // `…-sub-1791998134948544726` failed on a store error at 17:18:21, its children
                // went on to boot two VM grandchildren, and those held two of the four VM slots
                // for a day (`crate::vm_reap`). Stopped before the backend is closed, the same
                // order a stop takes: children first.
                a_child_that_ends_stops_its_own(Some(&sub.subagents), &sub.hub);
                // The backend is closed on this exit too — the other two close it below — so a
                // child whose first turn failed does not leave its watchers and cgroups behind.
                sub.close_backend();
                return Err(fail(e.to_string()));
            }
        };
        // **A child with children still on their task is not done** — the operator, of a
        // `kinds-e2e` that answered while its `tidb-kinds-e2e` ran on: *"it is strange that agent
        // marked as done and grandchildren kinda stays detached"*, and their ruling that it
        // waits. Its answer is held: each child's result reaches it as a turn of its own (the
        // ordinary wake), it is served as any child is meanwhile, and it is done when none is
        // left unsettled — with its NEWEST answer, the one that read their results. A VM child's
        // VM stays up for the hold, which is what a child running inside it needs.
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
        // **A VM's life is its turn's, and closing it is where the placement sentence comes
        // from** — the one the parent reads as *where the work went*. So a VM child answers the
        // task and no more: its substrate cannot be held open for a later question, and that is
        // stated here rather than discovered as silence.
        if placement != letibot_tools::builtins::task::Placement::Host {
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
            // **The child has finished, so its branch goes to the queue** — the runner's own
            // half of `task_start`'s door, taken here because THIS is the finish. See
            // [`HarnessTaskRunner::finished`]: the two notices (this one and a `task_result`
            // that arrives later) are one entry under one id.
            self.enqueue_finished(&sub_id);
            return Ok(said);
        }
        // **A host child stays reachable** — the operator's ruling (*"mid turn, post turn
        // whatever"*), and this is where the answer is given: **as the turn ends**, not on the
        // way out of this function, because what follows does not end until the hub does. The
        // caller used to settle from this function's return, which a park would have delayed for
        // the life of the daemon — `task_result` would simply never answer.
        answer.say(letibot_tools::builtins::task::TaskStatus::Done {
            answer: reply.text.clone(),
            // **No reading here either, and for a reason that is the whole design.** This is
            // where the answer is given, and the reading is taken where it is ASKED for
            // (`collect`): a child that answers and then parks would otherwise be described as
            // it was at the moment it answered, for ever.
            child: None,
        });
        // **The child has finished, so its branch goes to the queue.** Here rather than at the
        // thread's exit, and the difference is the whole reason `AnswerOnce` exists: a host
        // child parks in `serve_child` below and does not leave this function until the daemon
        // closes its hub, so enqueueing on the way out would hold a finished branch out of the
        // queue for the life of the daemon. The answer being given IS the finish.
        self.enqueue_finished(&sub_id);
        if serve_child(&mut sub, &sub_hub, &sub_id, turns) == ChildExit::Parked {
            slot.mark_parked();
        }
        // The hub closed (the daemon is going away), a stop arrived, or the child parked. Released
        // here rather than when the harness is dropped, for the reason it always was — and there
        // is no placement sentence to add on the host, which `close_backend` says by returning
        // `None`.
        let landed = sub.close_backend();
        Ok(match landed {
            Some(where_) => format!("{}\n\n[subagent placement] {where_}", reply.text),
            None => reply.text,
        })
    }
}

/// **The intent diff at a turn boundary**, or `None` when there is nothing to say
/// — which is the common case and is meant to be.
///
/// A free function because the choice it makes is the one worth testing on its own,
/// and testing it through a `Harness` would need a model server to produce a turn.
///
/// Two functions, and the difference is `prose`:
///
/// * [`letibot_tools::builtins::intent::close_the_turn`] reads the assistant's own
///   text for the `commitments` heuristic **as well as** the tool-declared half.
/// * `IntentLedger::reconcile(turn_id, "")` is the tool-declared half alone, which
///   that method's own doc calls *"the deterministic one"*.
///
/// The deterministic half is the default because of the rule this whole strand is
/// under: **nothing widens by default.** The prose heuristic can only fire on a turn
/// that ran nothing at all, which for a plain answer is the normal case, so switching
/// it on for every existing session would put *"you said you would X"* into
/// conversations that were working.
///
/// The deterministic half needs no flag because it cannot fire unless the model used
/// `todo` or `goal`, and those are seated only by a role that names them. A default
/// `letibot` session is behaviourally identical with it on.
/// `, worktree `…` on branch `…`` for a child placed in a worktree, empty otherwise.
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

    /// **A declared local model seats a child** — the operator's ruling, 2026-10-05:
    /// *"models from providers.toml must be usable for subagents"*.
    ///
    /// `provider` stays `None` because this IS the local path; what travels is `local`,
    /// which carries the address the child's config is pointed at. The two cannot be
    /// collapsed: `None` alone would mean the daemon's own server, which is a different
    /// box with a different window.
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

    /// **The project's parameters reach the child, and its window does not come from them.**
    ///
    /// The operator: *"when used in subagents mode we need to call qwen with the parameters
    /// similar to dense78 - otherwise it overthinks terribly"* — and in the same breath
    /// *"we dont have the same context limit as with dense obviously"*. So the five samplers
    /// land on the config the child is built from, replacing what it would otherwise have
    /// inherited or taken from its model's `[model.*]` profile, and `context_window` — the
    /// wall [`child_window_refusal`] above is about — is not touched: a child's room is the
    /// number the box answering it reported.
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

    /// **`local` is the daemon's own server and needs no key** — the deepseek→local half of
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
    /// spawned** — with the sentence naming `local` and the verb that lists providers, so
    /// the model can correct itself rather than guess.
    #[test]
    fn an_unknown_provider_is_refused_by_name_with_the_fix() {
        let err = subagent_model("definitely-not-a-provider", None)
            .expect_err("an unknown provider must not resolve");
        let said = err.join(" | ");
        assert!(said.contains("definitely-not-a-provider"), "{said}");
        assert!(said.contains("/models"), "{said}");
    }

    /// **The other direction: a cloud model on a child, resolved through the door `/models`
    /// uses.** Guarded on the box actually holding the key — a test that invented one would
    /// be asserting the catalogue rather than the resolver, and skipping is said out loud
    /// rather than passed off as green.
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
        // PROVIDER` follows — one resolver, so the child and the session cannot disagree
        // about what a bare name means.
        let bare = subagent_model("deepseek", None).expect("the key is held");
        assert!(bare.label.starts_with("deepseek/"), "{}", bare.label);
    }

    /// **A child nobody can give a window to is refused, by name, rather than run unbounded.**
    ///
    /// The failure this exists to stop is the one three children hit: their wall was computed
    /// from the parent's cloud window (~1M) while the server answering them held 256k, so
    /// nothing compacted and the server ended them. `None` and `Some(None)` are the same answer
    /// for a child, and both are refused; a child with the server's own window is spawned.
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

    /// **The parent's window is the child's only on the parent's own model** — so the refusal
    /// above stays the width of the defect it was written for, and no wider.
    ///
    /// A `local` child of a `local` session is the same server (and the daemon measures it, so
    /// this branch is the fallback rather than the path). A child named for the parent's own
    /// provider AND model is the parent's number, which is where an explicit
    /// `--context-window` for a model the catalogue does not carry lives. **The case that must
    /// never be true is the last one**: a child on a different model than its parent.
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
