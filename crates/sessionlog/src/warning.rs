//! **Which warnings are routine, and which are failures.**
//!
//! `head-parity-2026-09-21.md` **R19**, the operator's ruling of 2026-09-22, on
//! restarting a head and being met by twelve red lines: *"i dont want to see that on
//! restart."* One of the three faults in that ruling is this table's whole subject:
//!
//! > **Routine is painted as failure.** `compacted` and `auto_compact` are the
//! > session doing exactly what it should, and they arrive in the same red as a denial
//! > or a gate timeout. The colour asserts a severity the fact does not have. A
//! > housekeeping notice and a refused call must not look alike.
//!
//! **Why it is not taste.** An operator met by a red block on every restart learns to
//! skip it, and the block is where a real denial lives. Making routine announcements
//! loud is how the loud ones stop being read — R10's argument for letting a note be
//! retired at all, one step earlier in the sequence.
//!
//! # The rule, stated so it can be applied to a code nobody has written yet
//!
//! **Routine** is a sentence about something that worked, something the operator
//! asked for, or something the session is doing by design. Delete it and the reader
//! is no worse off — the compaction still happened, the mode still moved, the round
//! is still being retried.
//!
//! **Failure** is a sentence about something that did *not* work, did not happen, was
//! refused, or could not be checked. Delete it and the reader is misinformed: the
//! session is at the wall with compaction switched off, the mode did *not* move, the
//! import stopped part-way, the store cannot be written to.
//!
//! A third case — *a caveat that a check did not happen* (`reseat_unchecked`,
//! `prefix_check_skipped`, `monitor_wake_not_armed`) — is a **failure** by that rule.
//! It is not a thing going wrong so much as a thing that was not verified, and the
//! register's job is to say *look at this*, which is right for both.
//!
//! # Where this lives, and why not in a head
//!
//! The codes are the log's vocabulary: `SessionEvent::Warning { code, .. }` is
//! defined in this crate, the daemon and the turn engine publish through it, and
//! *every* head renders it — so a split that only `letibot-tui` knew would have to be
//! copied by the other one and the two copies would drift. §11.6 rules the same way
//! about `JobState::word`: the vocabulary belongs where the enum is.
//!
//! **Every emitter's code is in this table, including the head's own**
//! (`log_gap`, `protocol_skew`, `reattached`, `unreadable_frame`, `orphan_body`,
//! `sudo`) — a head files those as notes without ever publishing an event, and they
//! land in the same register as everything else. One table, or the split is two.
//!
//! # The two ways a code can be missing from it, and what happens
//!
//! 1. **A new code with a literal at its emission site.** The guard in
//!    `tests/warning_codes.rs` reads this tree, finds every `code: "…"` inside a
//!    `Warning`/`Warned` literal, and fails until the code has a row here. It has to be
//!    falsified by adding a code and watching it fail, which is the whole point of
//!    pointing a guard at the thing it guards (§11.5, ruled for the other table that
//!    had this shape).
//! 2. **A code that reaches the wire through a variable** — the turn engine's
//!    `code: trip.code` and `code: from check.warning()`, which no scan can see. Those
//!    are the reason [`class`] answers **`Failure` for anything it has not been told
//!    about**: a code this table has never heard of is drawn loudly, so the cost of
//!    forgetting one is a red line somebody asks about rather than a quiet line nobody
//!    notices. The safe direction to be wrong in is the register that gets read.

/// **A warning's register, for how it is drawn.** Three, and the middle one is new:
/// R29 part two, ruled 2026-09-23.
///
/// # Why two was not enough, and the census that decided it
///
/// The operator, having been shown a red note for a mistyped `/qwe` sitting in the same
/// colour as `ledger_chain_mismatch`: *"red is stop the world event … a mistyped /qwe is not
/// a session in trouble."* The instruction was to **measure before ruling**, and this is the
/// count over the table **as it stood at the ruling** — 69 codes, 23 Routine, 46 Failure
/// (**73 today**; `the_register_census` is the number that is pinned, and this paragraph is
/// the measurement the ruling was made on rather than a live mirror of the table) — of which
/// **7** are *the reader asked for something that is not there*:
///
/// ```text
/// slash_refused        the verb they typed is not one
/// mode_unknown         the mode they named does not exist
/// mode_set_refused     the mode exists and this session cannot carry it
/// job_output_refused   there is no job by that id here
/// reseat_refused       there is nothing to re-seat (or the attempt did not land)
/// import_no_session    there is no such session in the other store
/// import_no_db         there is nothing to import from
/// ```
///
/// **Seven of forty-six, so the register is not diluted and the two-register rule was not
/// wrong** — and the operator allowed for exactly that outcome (*"if it is two codes out of
/// forty, the right answer may be to move those two and leave the rule alone"*). What settles
/// it for a third register rather than for leaving the rule alone is **where those seven
/// fire**: at the moment the operator is typing. `slash_refused` is emitted on a typo, in
/// front of the reader, while `ledger_chain_mismatch` is emitted at a replay nobody is
/// watching. So the seven are not merely 15% of the table — they are the codes a reader meets
/// *most often*, and they teach the red reflex on the occasions when nothing is wrong.
///
/// # The discriminator, which is *not* "was it refused"
///
/// **Whose act, and what is at risk beyond it.** A `Refused` note is the answer to something
/// the reader just did, the correction is theirs to make now, and nothing but the line they
/// typed is at stake. A `Failure` is a fact about the session — its integrity, its room, its
/// ability to run the work at all — and the reader cannot fix it by typing something else.
///
/// **Two refusals of the reader's own act are not `Refused`**, and both are worth naming
/// because they look like it: `mode_unpersisted` (the mode moved, and the *next* session will
/// not carry it — at risk is a session that is not this one) and `title_not_stored` (the name
/// is lost at restart). The reader's line worked; what did not is the durable half.
///
/// **Nothing moved to `Routine`**, and the table's own rule forbids it: *routine is a
/// sentence you can delete with the reader no worse off*, and deleting a refusal leaves the
/// reader believing a command took effect. That is why the middle is its own register rather
/// than the quiet end of the other two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Housekeeping, a command's own answer, or a hiccup being handled. Drawn in the
    /// dim register — no mark, no red.
    Routine,
    /// **The reader asked for something that is not there, and the fix is theirs.** Drawn
    /// in the notice register — a mark, no red — because it is the *answer* to what they
    /// typed, and the sentence already says what to do instead.
    Refused,
    /// Something did not work, did not happen, or could not be checked, and **what is at
    /// risk is the session rather than the line**. Drawn as a failure: red, prefixed `!`.
    Failure,
}

impl Class {
    pub fn is_routine(self) -> bool {
        matches!(self, Class::Routine)
    }

    /// **The red one.** What a reader is being asked to *stop for* — and the reason the
    /// middle register exists is so this answer stays rare enough to carry that weight.
    pub fn is_failure(self) -> bool {
        matches!(self, Class::Failure)
    }
}

/// **The split.** One row per code, and a code that is not here is a [`Class::Failure`].
///
/// The order is the order it is read in — routine first, because that is the list
/// whose *absences* caused the complaint — and each row carries the reason it is where
/// it is. A row with no comment is a code whose name says it.
pub const TABLE: &[(&str, Class)] = &[
    // ───────────────────────── routine ─────────────────────────
    //
    // Housekeeping the session does by design, in the words of whoever watched it
    // happen. These are the four the operator was met by.
    ("auto_compact", Class::Routine),
    ("compacted", Class::Routine),
    // **The fold saying what it is doing while it does it** — *"compacting half 1 of 2: the
    // most recent exchanges — 348 item(s), 940612 token(s) to read"*, and then what the half
    // answered. Routine by the same rule as the four above: the operation the operator is
    // waiting on, described in its own units, and deleting it loses only the reason the wait
    // was minutes long. It is the opposite of a fault — a fault is what a silent wait gets
    // read as. Measured 2026-10-02: a fold ran for minutes with nothing on the screen, and
    // the operator asked *"leticl compacts but why no progress bar?"*.
    ("compact_half", Class::Routine),
    // **The operator's own call ran** — R31's size disclosure. Routine by the rule
    // above: it is a sentence about something the operator asked for that worked, and
    // deleting it loses only the number they were owed. It is the count of context they
    // just chose to buy, said while they can still act on it — the opposite of a fault.
    ("operator_call_ran", Class::Routine),
    // The operator's own `!` line ran, and the note says what it put in the conversation.
    // Routine for the door's own reason: the size disclosure is the feature working, not
    // a defect — the command's own failure, if it had one, is on the `ToolResult` row it
    // produced, under its own outcome.
    ("operator_shell_ran", Class::Routine),
    // The `!` line ran but its rows could not be recorded. A failure, and the same one
    // the door's `operator_call_abandoned` is: the work happened and the record of it is
    // what is missing.
    ("operator_shell_failed", Class::Failure),
    // **The daemon cannot tell whether the operator's own run is waiting for a line.** One of
    // its processes belongs to another user — `! sudo apt install mc`, where `apt` waits at
    // `Continue? [Y/n]` as root and `/proc/<pid>/fd/0` is `EACCES` for the daemon — so no card
    // can be raised and nothing says the command is waiting. (In the days when a `!` run held
    // the daemon's one worker, it held it until its deadline; the run has a thread of its own
    // now, and this sentence is still the only word about it.) A **Failure** by the rule
    // above and not a routine note: it is a check that did not happen, and the sentence's whole
    // job is *look at this* — the way in is `!send`. Deleting it puts the operator back in front
    // of a command that says nothing and never ends, which is the report it was written for.
    ("operator_run_unreadable", Class::Failure),
    // The compaction also picked up the tools this daemon now seats, and the second
    // summary turn `/reseat` would have cost was not paid. It is a *report of a
    // repair that succeeded* — the compaction's own second half.
    ("reseated", Class::Routine),
    // The evidence for a refused frame was written where somebody can read it. The
    // refusal is the failure and it is said elsewhere; this is the capture working.
    ("frame_capture_written", Class::Routine),
    // Capture is switched off, so nothing was written. A disclosure of configuration —
    // the code above, one branch along.
    ("frame_capture_disabled", Class::Routine),
    // A command the operator typed, answered. `/mode`'s two routine answers are the
    // ones where the point moved (`mode_set`) and where it moved for *later* sessions
    // because this one cannot carry it (`mode_set_next_session_only`) — the second is a
    // fact about the next session, not a failure of this one.
    ("mode_set", Class::Routine),
    ("mode_set_next_session_only", Class::Routine),
    // A consented `allow-all` is this session's and was deliberately NOT written to the
    // project row. Consent is the thing working; the row not moving is the point.
    ("mode_session_only", Class::Routine),
    // The model server did not answer and the round is being taken again. The retry is
    // the handling — and when the retries are exhausted the turn fails under
    // `turn_failed`, which is a failure.
    ("model_endpoint_retry", Class::Routine),
    // The operator typed a provider key on the masked card and it was saved — the ask
    // working, said once so the file it went to is on the record.
    ("provider_key_saved", Class::Routine),
    // The key card was closed with no key: the operator's own answer, and the turn says
    // what happens instead.
    ("provider_key_refused", Class::Refused),
    // **A provider that has not started answering, said while it is still silent.**
    // Routine, and the distinction matters: nothing failed. The request is open, the
    // round is still running, and a degraded provider that takes 12 s to its first byte
    // is the case this is FOR — measured on DeepSeek, 2026-10-02. Filing it as a failure
    // would mark a session as having gone wrong for the crime of waiting, which is
    // exactly the misreading the code exists to prevent.
    ("model_slow_first_byte", Class::Routine),
    // Their interrupt and their promote arrived between turns, so there was nothing to
    // stop or move. The operator's own act, and a no-op.
    ("interrupt_idle", Class::Routine),
    // **A stop took the session's children with it** — the downward edge of a supervision
    // tree, said out loud. Routine by the rule above and for the same reason: it reports an
    // act the operator (or the parent that killed it) asked for, and deleting it would lose
    // only the list of what went with it. See the supervision invariant on
    // `HarnessTaskRunner` and `stop_children_first`, which is the one place it is written.
    ("subagents_stopped", Class::Routine),
    // **A child was woken because something it started had settled, and the turn that would
    // have read the settlement failed.** A failure: the settlement is still queued and the
    // next wake finds it (the session is not stuck), but a notice the child was owed did not
    // reach it, which is exactly what the wake exists to prevent.
    ("wake_failed", Class::Failure),
    // **A child was due its idle plan-check and the turn that carries it failed.** The
    // sibling of `wake_failed` above, and a failure by the same argument: the session is not
    // stuck — the check is spent rather than lost, and it comes back when the plan moves or
    // somebody speaks to the child — but the model was owed the one sentence that says its
    // plan is unfinished and did not get it, and a child has no worker to report that the way
    // a session the daemon holds does. It goes on the child's own log, which its parent and
    // the operator both read. See `harness::serve_child_under`'s `OwnWork::TimedOut` arm.
    ("todo_check_failed", Class::Failure),
    ("promote_idle", Class::Routine),
    // **A promote routed, not answered here.** The operator's Ctrl+O arrived while
    // their own run is in flight on the bang thread, so the worker — free, by the
    // design that put the run on a thread of its own — leaves the flag for that
    // run's wait loop to take and move the run. Routine: it reports the operator's
    // own act on its way to its consumer, and the promotion's own record is the
    // answer. See `Sessions::dispatch`'s `CommandKind::Promote` arm for the drop
    // this routing replaces.
    ("promote_in_flight", Class::Routine),
    // **A parent's message to a subagent that had already finished** — see
    // `CommandKind::Message`. The runner refuses these by name before submitting
    // (`HarnessTaskRunner::send`), so this arm is the race it cannot close: the child's turn
    // ended between that check and this drain. Routine, because nothing went wrong in the
    // session — the parent's next `task_result` reads a finished child and says so.
    ("message_idle", Class::Routine),
    // **A finished `task_start` child's branch is in the merge queue**, said to the parent that
    // started it. Routine by the rule at the top of this table, and for the reason
    // `operator_call_ran` is: it reports something that worked, and what it puts on the screen
    // is where the work went. The entry is in the queue either way — a reader who was not told
    // has lost the sentence, not the branch — and the same fact is on the queue pane and in
    // `task_result`'s own answer. See `HarnessTaskRunner::enqueue_finished`.
    ("merge_queued", Class::Routine),
    // **A finished child's branch could NOT be enqueued** — the store would not open, or this
    // daemon was started without `--store`. A failure: the branch is still on its branch in
    // its worktree and nothing is lost, but nothing will land it either, and that is not
    // something the reader fixes by typing something else. The two words are one door's two
    // verdicts, which is why they sit together — see `HarnessTaskRunner::enqueue_entry`,
    // whose refusal is the sentence this code carries.
    ("merge_not_queued", Class::Failure),
    // **A FAILURE, and the one code here that is a claim about the corpus rather than about
    // the session.** An operator's own call was admitted and the head that asked went away
    // before reporting what it did, so the record holds an `admit` whose outcome nobody
    // knows. That is not a thing going right and it is not a no-op: the reader is *less*
    // informed than the row suggests, which is exactly the rule at the top of this table —
    // the session is fine, and a claim on the record is not.
    ("operator_call_abandoned", Class::Failure),
    // Somebody asked this daemon to stop. The session is on disk and `/continue` reopens
    // it; the sentence exists so that a session ending is not a session disappearing.
    ("daemon_stopping", Class::Routine),
    // **What the stop ended on its way out.** Beside `daemon_stopping` because it is the
    // same fact one step further on — the daemon was asked to stop, and a command was in
    // flight, so the command was ended rather than waited for. Routine for the same reason
    // and by the same rule: the operator asked for this, so it is not a fault, and painting
    // it as one would be R19's second fault with a longer sentence.
    ("daemon_stopping_runs", Class::Routine),
    // The store's own notes about what opening or resuming did — the workspace it took
    // from the row, the title, the seat. Facts about where you are.
    ("resume_note", Class::Routine),
    ("open_note", Class::Routine),
    // **This head's own**: it got its connection back. Filed at a real seam as it
    // happens, so it is news rather than history.
    ("reattached", Class::Routine),
    // A slash command's answer. `slash_refused` is the same channel with the other
    // verdict, and it is a failure.
    ("slash", Class::Routine),
    // An import's three routine notes: whose rows these are, what could not be a row
    // (an unknown tag, a patch, a compaction — *counted and said, never dropped*), and
    // the count and spend it left behind.
    ("imported", Class::Routine),
    ("import_scrap", Class::Routine),
    ("imported_summary", Class::Routine),
    // The operator's steering prompt arrived, so the turn was aborted to take it. Their
    // own act, and the interruption itself is drawn where interruptions go.
    ("steering_urgent", Class::Routine),
    // A diagnostic of the server's prefix cache: the prompts are proven identical over
    // the span, so this is llama.cpp resuming from a checkpoint and snapping `n_past`
    // back. Off the screen unless the debug channel is on.
    ("cache_reuse_shortfall", Class::Routine),
    // The `testing` module's helper, and there is no production fact behind it. It has a
    // row because the guard reads the tree and this is a code literal in it.
    ("test", Class::Routine),
    // ───────────────────────── failure ─────────────────────────
    //
    // The turn did not do what it should, the session is out of room, or something could not
    // be checked. **What is at risk here is the session, not the line** — which is the whole
    // of the test the reader applies when deciding whether to stop and look.
    //
    // **Not ordered by register, and that is deliberate.** The rows are grouped by where the
    // code COMES FROM — the `mode` codes together, the store's together, this head's own
    // together — because that is what a reader looking one up is doing, and a `Refused` row
    // among its siblings says so on its own line. See `Class` for the census and the
    // discriminator.
    ("turn_failed", Class::Failure),
    ("context_wall", Class::Failure),
    // *At* the wall, and nothing was compacted because automatic compaction is off for
    // this session. Exactly the sentence an operator has to read: `/compact` does it by
    // hand.
    ("auto_compact_skipped", Class::Failure),
    // Compaction ran and left the summary within the headroom of the window, so
    // automatic compaction is off rather than looping once a turn. *"Start a fresh
    // session, or raise --context-window."*
    ("auto_compact_no_progress", Class::Failure),
    ("auto_compact_failed", Class::Failure),
    // The turn was cut short at the length limit, or the batch of calls was truncated so
    // none of them ran, or it stopped with the reasoning block still open.
    ("length_empty_turn", Class::Failure),
    ("length_batch_refused", Class::Failure),
    ("ended_in_reasoning", Class::Failure),
    // A guard fired. §18's own note on this register: *a guard nobody notices is a guard
    // nobody wrote* — and both of these are a turn that was going nowhere, stopped.
    ("repetition_collapse", Class::Failure),
    ("reasoning_stall", Class::Failure),
    // The prompt this box rebuilt diverged from what the server was sent — the code's own
    // comment calls it *our defect* — or the check that would catch it was skipped.
    ("prefix_divergence", Class::Failure),
    ("prefix_check_skipped", Class::Failure),
    // The token chain does not agree with the ledger, or rows are missing from the
    // store's coverage, or N row events arrived for M items.
    ("ledger_chain_mismatch", Class::Failure),
    ("row_coverage_gap", Class::Failure),
    ("record_item_pairing", Class::Failure),
    // A caveat that a check could not be made: the carry was not verified against the
    // window, and the monitors will be polled rather than woken.
    ("reseat_unchecked", Class::Failure),
    ("monitor_wake_not_armed", Class::Failure),
    // **Refused**: the dominant case is *there is nothing to re-seat*, which the sentence
    // says outright (*"this conversation's prompt already carries exactly the tools that are
    // seated"*), and the other branch — a summary turn that proposed a tool call — leaves the
    // transcript unchanged with *"try again"*. Both put nothing at risk but the attempt.
    ("reseat_refused", Class::Refused),
    // The capture of a refused frame did not happen, which is the one branch of that
    // trio that is not the mechanism working.
    ("frame_capture_failed", Class::Failure),
    // An answer to a password prompt arrived with nothing waiting for it — the helper had
    // given up, or another head answered first — so the call that needed the password did
    // not get one.
    ("secret_late", Class::Failure),
    // **A line for the operator's own run found nothing waiting.** The card was raised for
    // a command that has since ended, or another head answered it first, so nothing was
    // written to any program's stdin. A `Failure` and not the reader's input being wrong:
    // the person typed an answer and the command did not get it.
    ("prompt_late", Class::Failure),
    // **There was no command of the operator's own to send to.** `!send` with nothing
    // running, or with the run already ended. Not a failure of the harness and not a
    // mistake the reader can be told to correct beyond the one sentence — the act simply
    // had nothing to act on, which is the same shape as `!term` with no pane.
    ("nothing_to_send_to", Class::Refused),
    // **Refused, and it must be read.** The mode they named does not exist, or the session
    // cannot carry the one they asked for: the point did NOT move, and the fix is their next
    // line. *"this session stays at `writes-allowed`: …"* is an answer, not a fault.
    //
    // **And one of the three stays a Failure**, which is the test of the discriminator rather
    // than an exception to it: `mode_unpersisted` means the mode MOVED and the *next* session
    // will not carry it. The reader's line worked; what did not is the durable half, and what
    // is at risk is a session that is not this one.
    ("mode_unknown", Class::Refused),
    ("mode_set_refused", Class::Refused),
    ("mode_unpersisted", Class::Failure),
    // A seat is attached to this session and nothing said on the fabric can wake it.
    ("flowy_not_seated", Class::Failure),
    // An answer reached the queue with nothing waiting for it — one of the two branches
    // says outright that this is a defect.
    ("answer_unclaimed", Class::Failure),
    // **Refused, and these two are the clearest case in the table.** Each sits one `match`
    // arm from a Routine twin on the SAME emission — `code: if reply.ok { "slash" } else
    // { "slash_refused" }` — and `mode_set`/`mode_set_refused` are the same shape. One
    // channel's two verdicts belong in one register; only one of the two verdicts means the
    // session is in trouble; and a red `slash_refused` is the code that taught a reader to
    // skim red.
    ("job_output_refused", Class::Refused),
    ("slash_refused", Class::Refused),
    // The store, in every way it can fail to do its job: the session does not open, the
    // transcript store cannot be written, the decision corpus cannot be consulted, the
    // title cannot be recorded, the session the operator asked for does not resume, and
    // an import that stopped part-way or had nothing to read.
    ("session_unavailable", Class::Failure),
    ("transcript_store", Class::Failure),
    ("decision_corpus", Class::Failure),
    ("title_not_stored", Class::Failure),
    ("resume_failed", Class::Failure),
    // **Refused**: there is nothing to import from, or no session by that name in the other
    // store — *"the session stays up and empty"*, which is the sentence of something whose
    // remedy is the reader's. `import_failed` stays a Failure: an import that stopped
    // part-way is a half-filled transcript, and that is a fact about this session.
    ("import_no_db", Class::Refused),
    ("import_no_session", Class::Refused),
    ("import_failed", Class::Failure),
    ("fabric_refresh_failed", Class::Failure),
    // **This head's own failure, and it is about the reader rather than the session.** The
    // row a scrolled viewport was holding is not in the transcript any more, because a
    // compaction, a resync or a snapshot replaced it — so the thing they were reading is
    // gone and they have to be told (R36). `Failure` by R29 part two's own test: it is not
    // the reader's act, and what is at risk is their orientation rather than any durability.
    ("anchor_lost", Class::Failure),
    // **This head's own failures.** Events that never reached it, a frame it could not
    // read, a daemon that disagrees about the protocol, a row's content with no row to
    // land on, and a password prompt that settled — including the case where nobody
    // gave one, which is a privileged call that did not happen.
    ("log_gap", Class::Failure),
    ("unreadable_frame", Class::Failure),
    ("protocol_skew", Class::Failure),
    // **A head that outlives its daemon, and does not notice.** The operator's box: three
    // `letibot-tui` processes up for days while the daemon was replaced underneath them, and
    // the registry is in memory — so the answers still on the screen were a *predecessor's*,
    // and nothing said so. `Failure` by the register's own discriminator: the session is
    // fine and the head is the thing that is wrong, and a reader who is not told is reading
    // a picture of a daemon that no longer exists.
    ("daemon_replaced", Class::Failure),
    ("orphan_body", Class::Failure),
    ("sudo", Class::Failure),
    // **A command of the operator's own, answered or not.** The head's own note beside
    // `sudo`'s, one channel over: it says whether the line went down the run's stdin, and
    // the two cases are different things to have happened to a person who was about to
    // type — `answer sent by WHO` is the feature working, and `nothing sent (WHY)` is a
    // line that did not reach a program that was waiting for it.
    //
    // **`Failure`, and it is not the `nothing sent` case that decides it.** A register is
    // per code and not per instance, and the same code carries the line that DID go out;
    // the case that settles it is the other one — the command ended with the card still up,
    // which is an answer that was not delivered. Routine would paint that as housekeeping.
    ("prompt", Class::Failure),
    // **A head that was told something is edge-bound and has nowhere to put it.** The code is
    // in [`ALARM_ONLY`], so the daemon's sentence is not drawn in the conversation; the head
    // is supposed to move it to a counter, and this is what it says when it has no counter to
    // move it to. `Failure` and not `Routine`, and the argument is the same one that keeps
    // `anchor_lost` here: the diagnostic reaches neither the record nor the triangle, so a
    // reader who is not told has been told nothing at all. It is also the loudest thing this
    // module can say about a *build* rather than about a session — the two halves of the tree
    // disagree about where a note goes, and no event, socket or grant is involved.
    ("alarm_only_unregistered", Class::Failure),
    // Codes that exist only in fixtures in this tree — a head under test is handed a
    // wall, a gap, a decision that timed out, a guard that tripped. They are classified
    // because the guard reads every `code:` literal under `crates/`, which is where the
    // tests that use them live.
    ("gate", Class::Failure),
    ("gate_timeout", Class::Failure),
    ("gap", Class::Failure),
    ("guard.empty", Class::Failure),
];

/// **Which register a code is drawn in.** See [`TABLE`], and the module doc for why an
/// unknown code is a failure rather than a guess.
pub fn class(code: &str) -> Class {
    match TABLE.iter().find(|(c, _)| *c == code) {
        Some((_, class)) => *class,
        None => Class::Failure,
    }
}

/// [`class`], as the question a renderer asks about the quiet end.
pub fn is_routine(code: &str) -> bool {
    class(code).is_routine()
}

/// [`class`], as the question a renderer asks about the **red** end — and the one R29 part
/// two is about, because `Refused` is neither.
pub fn is_failure(code: &str) -> bool {
    class(code).is_failure()
}

/// **The codes that belong on the edge rather than in the record** — the ⚠ and `/status`,
/// never a row in the conversation.
///
/// # The rule, because the next person needs it to place a new code
///
/// **An event in the record goes in the record; a note about the weather goes on the
/// edge.** A compaction *changes the conversation*: `compacted` and `auto_compact` are
/// `Class::Routine` and are drawn as rows, and the operator has repeatedly wanted to see
/// them happen. A slow first byte changes nothing about the conversation — it is a fact
/// about the provider's latency, and it is the same fact whether or not anybody read the
/// sentence. Their words, on `model_slow_first_byte`: *"it is important diagnostics -
/// we have a yellow triangle for that. both heads should not emit it inside
/// conversation."*
///
/// So this is **not** "all of `Class::Routine`", and widening it to that would hide
/// compactions from the people who asked to see them. `Class` answers *how bad is it*
/// (routine, refused, failure) and this answers *where does it go* — two axes, and a code
/// has to answer both. A code here is still counted, still reachable, still unable to be
/// lost: the triangle is a pointer at `/status`, which is where the number lives.
///
/// **A `Failure` or a `Refused` can never be here**, and that is asserted below rather
/// than left to whoever adds the next row: hiding a fault on the edge would make it a
/// fault nobody sees, which is the one direction this whole module exists to close.
pub const ALARM_ONLY: &[&str] = &[
    // A first byte slower than the head's patience. The turn is running and nothing is
    // wrong with it; what the reader would learn from a row is that the provider was
    // slow once, which is what the triangle is for.
    "model_slow_first_byte",
];

/// Whether this code is drawn on the edge rather than in the conversation — see
/// [`ALARM_ONLY`], whose docstring is the rule.
pub fn to_the_alarm(code: &str) -> bool {
    ALARM_ONLY.contains(&code)
}

/// **The codes whose sentence is drawn in the conversation for a while and then taken
/// down** — the third answer to *where does it go*, beside the record ([`TABLE`]'s rows,
/// drawn as notes) and the edge ([`ALARM_ONLY`], which never gets a row at all).
///
/// # The rule, because the next person needs it to place a new code
///
/// **A code belongs here only when something else on the screen carries the same fact.**
/// The sentence may go; the fact may not. That is the whole of why a timer is allowed at
/// all, and why it is not allowed one register over: a timer is a *disclosure* decision,
/// and the disclosure is the thing that has to survive it.
///
/// `merge_queued` is the case. The daemon says *your child's branch is in the merge
/// queue*; the head's bottom edge counts the queue — how many entries are in review, how
/// many are being merged, how many are parked — for as long as the queue holds anything,
/// and the queue pane names every entry and its branch. So the row is the news and the
/// edge is the standing fact.
///
/// It is **not** *all of `Class::Routine`*, for [`ALARM_ONLY`]'s reason one register over:
/// `compacted` and `auto_compact` change the conversation and the operator has asked to
/// see them happen, `daemon_stopping` is a sentence about the session ending, and
/// `resume_note` is a fact about where you are. None of those is on the edge, so none of
/// them may go quiet.
///
/// **A `Failure` or a `Refused` can never be here**, and that is asserted below rather
/// than left to whoever adds the next row: a fault that stops being drawn after thirty
/// seconds is a fault nobody sees, and the operator's own act is not news that expires.
/// (`merge_not_queued`, the failure twin of the one row above, is exactly the code that
/// must stay: it says a branch nothing will land.)
///
/// # Which head, and for how long
///
/// The **set** is here, with the class vocabulary, because it is the same axis
/// [`ALARM_ONLY`] answers — *where does it go* — and a split only one head knew would have
/// to be copied by the other. The **duration** is the head's, and it is one constant where
/// the drawing happens: `letibot-tui`'s `FLEETING_MS`.
///
/// **Not the same as [`ALARM_ONLY`]**, and the two are not alternatives: an alarm-only code
/// never gets a row (its number is in `/status` from the moment it arrives), and one of
/// these gets a row and then loses it.
pub const FLEETING: &[&str] = &[
    // A finished `task_start` child's branch has entered the merge queue. The queue's
    // standings are what the bottom edge counts, so the sentence is news and the count is
    // the fact that stays. See `HarnessTaskRunner::enqueue_finished`, which publishes it.
    "merge_queued",
];

/// Whether this code's sentence is drawn for a while and then taken down — see
/// [`FLEETING`], whose docstring is the rule.
pub fn is_fleeting(code: &str) -> bool {
    FLEETING.contains(&code)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Nothing that is a fault may hide on the edge.**
    ///
    /// [`ALARM_ONLY`] moves a code out of the conversation and onto the triangle, where it is
    /// a number in `/status` and not a sentence in the record. That is right for weather and
    /// wrong for a fault: a failure drawn only as a counter is a failure nobody reads, which
    /// is the one direction this module exists to close. So the table is checked against the
    /// class table rather than trusted, and the check lives beside the rule so that adding a
    /// row is when it runs.
    #[test]
    fn an_alarm_only_code_is_never_a_failure_or_a_refusal() {
        for code in ALARM_ONLY {
            assert_eq!(
                class(code),
                Class::Routine,
                "`{code}` is alarm-only and is not Routine — a fault or a refusal drawn \
                 only as a triangle is a fault nobody reads"
            );
            assert!(to_the_alarm(code), "the table is what `to_the_alarm` reads");
        }
        // **And the rule was not widened to all of `Class::Routine`.** These change the
        // conversation, so they are rows in it; the operator has asked to see compactions
        // happen more than once, and moving them to the edge would answer that request
        // with silence.
        for code in ["compacted", "auto_compact", "compact_half", "reseated"] {
            assert!(
                !to_the_alarm(code),
                "`{code}` changes the conversation and belongs in the record"
            );
        }
    }

    /// **Nothing that is a fault, and nothing the operator did, may go quiet on a timer.**
    ///
    /// [`FLEETING`] takes a code's sentence off the screen after the head's thirty seconds,
    /// which is only honest when something else carries the fact. The class is the half of
    /// that a test can check, and it is the half that matters: a `Failure` or a `Refused`
    /// drawn for thirty seconds and then gone is a fault nobody reads, which is the
    /// direction [`ALARM_ONLY`]'s guard above closes one register over. The other half —
    /// *something else on the screen carries it* — is a fact about a head's own frame and is
    /// argued in the table's docstring and tested where the timer lives.
    #[test]
    fn a_fleeting_code_is_never_a_failure_or_a_refusal() {
        // The vacuity guard: an empty set would satisfy the loop below and prove nothing.
        assert!(
            is_fleeting("merge_queued"),
            "the one code this table exists for has been removed, and the loop below \
             would then check nothing: {FLEETING:?}"
        );
        for code in FLEETING {
            assert_eq!(
                class(code),
                Class::Routine,
                "`{code}` is fleeting and is not Routine — a fault that goes quiet on a \
                 timer is a fault nobody sees"
            );
            assert!(is_fleeting(code), "the table is what `is_fleeting` reads");
        }
        // **And the rule was not widened to all of `Class::Routine`.** None of these is on
        // the edge, so none of them has a second place to be read: a compaction the operator
        // asked to watch, a session ending, and where the workspace came from.
        for code in [
            "compacted",
            "auto_compact",
            "daemon_stopping",
            "daemon_stopping_runs",
            "resume_note",
        ] {
            assert!(
                !is_fleeting(code),
                "`{code}` is not carried anywhere else, so its sentence has to stay"
            );
        }
    }

    /// The four the operator was met by, and the one that must never go quiet.
    ///
    /// **This test asserted `slash_refused` was a `Failure` and R29 part two overturned
    /// exactly that** — it is the register the operator was disputing, and the sentence they
    /// gave is in this file's own history: *"a mistyped `/qwe` is not a session in trouble,
    /// and on the same screen in the same colour sit `ledger_chain_mismatch`, `context_wall`
    /// and `prefix_divergence`."* So the pin moved rather than being deleted: a refusal is
    /// neither the quiet register nor the red one, and this test now says so.
    #[test]
    fn the_complaint_is_routine_and_a_refusal_is_not() {
        for code in [
            "daemon_stopping",
            "daemon_stopping_runs",
            "compacted",
            "auto_compact",
            "reseated",
        ] {
            assert_eq!(class(code), Class::Routine, "{code}");
        }
        for code in ["turn_failed", "context_wall", "prefix_divergence"] {
            assert_eq!(class(code), Class::Failure, "{code}");
        }
        for code in ["slash_refused", "mode_unknown", "job_output_refused"] {
            assert_eq!(class(code), Class::Refused, "{code}");
        }
    }

    /// **The safe direction.** A code this table has never heard of is loud, because a
    /// quiet line nobody notices is the failure mode R19 is about and a loud line
    /// somebody asks about is not.
    #[test]
    fn a_code_nobody_has_classified_is_a_failure() {
        assert_eq!(class("a_code_from_next_week"), Class::Failure);
    }

    /// Two rows for one code are two answers to one question, and the first would win
    /// silently.
    #[test]
    fn no_code_is_classified_twice() {
        let mut seen: Vec<&str> = Vec::new();
        for (code, _) in TABLE {
            assert!(!code.is_empty(), "an empty code");
            assert!(
                !seen.contains(code),
                "`{code}` has more than one row in the table"
            );
            seen.push(code);
        }
        // The floor, so a table that was emptied passes no test at all.
        assert!(seen.len() > 50, "{} codes classified", seen.len());
    }
}

#[cfg(test)]
mod the_register_census {
    use super::*;
    use std::collections::BTreeSet;

    /// **The seven codes the middle register exists for**, named so a new code has to be
    /// classified deliberately rather than by whoever adds it copying the neighbour.
    ///
    /// This list is the *finding*, not a rule: the rule is `Class`'s discriminator — whose act,
    /// and what is at risk beyond it. If a future code is the answer to something the reader
    /// just typed and nothing but that line is at stake, it belongs here and the count below
    /// moves with it, on purpose.
    const READER_INPUT: &[&str] = &[
        "import_no_db",
        "import_no_session",
        "job_output_refused",
        "mode_set_refused",
        "mode_unknown",
        // **`!send` with nothing of yours running**, or with the run already over. The reader
        // typed a line and there was no command of theirs to give it to — the same shape as
        // `!term` with no pane, which is `job_output_refused`'s shape too: the act is well
        // formed, the thing it names is not there, and the fix is the reader's next line.
        "nothing_to_send_to",
        // **The key card closed with no key** — the reader's own answer to the daemon's
        // question, and the turn's sentence says what to do (send again, or set the
        // variable). The same shape as `slash_refused`: the act was theirs, not a fault.
        "provider_key_refused",
        "reseat_refused",
        "slash_refused",
    ];

    /// **The census, pinned.** 90 codes, of which **9** are the reader's own input refused.
    ///
    /// R29 part two's instruction was to *measure before ruling*, and this is the measurement
    /// kept where it cannot drift: `Class`'s docs quote these numbers, and a code moved or
    /// added without a thought fails here rather than silently changing what a reader is
    /// taught by the colour of the screen.
    #[test]
    fn the_table_is_32_routine_9_refused_and_49_failures() {
        let count = |c: Class| TABLE.iter().filter(|(_, k)| *k == c).count();
        // **90, not the 89 the last census was taken at.** One arrival, and it moves the red
        // register: `todo_check_failed` is a CHILD that was due its idle plan-check and whose
        // turn failed. A Failure by `wake_failed`'s own ruling three rows up — the session is
        // not stuck (the check is spent rather than lost, and it comes back when the plan
        // moves or somebody speaks), but a sentence the model was owed did not reach it, and
        // a child has no worker to say so the way a session the daemon holds does. See
        // `harness::serve_child_under`'s `OwnWork::TimedOut` arm.
        //
        // **89, not the 88 the last census was taken at.** One arrival: `promote_in_flight`
        // is the sentence said when a head's Ctrl+O arrives while the operator's own run is
        // in flight on the bang thread — the worker is free by that thread's design, so the
        // flag is left for the run's own wait loop to take and move the run. Routine by
        // `promote_idle`'s own ruling beside it: the operator asked for this, the line says
        // where the request went, and the promotion's own record is the answer.
        //
        // **88, not the 86 the last census was taken at.** Two arrivals, one per verdict of the
        // key card (`Harness::obtain_key`, the daemon asking for a provider's API key on the
        // masked secret card): `provider_key_saved` is Routine — the ask worked, and the line
        // says which file the key went to — and `provider_key_refused` is Refused, the
        // reader's own answer, named in READER_INPUT. Neither moves the red register.
        //
        // The census before that, 86, kept its own list for the same reason:
        //
        // **86, not the 85 the last census was taken at.** One arrival, and it is the
        // daemon's own half of the stop: `daemon_stopping_runs` is the sentence said when a
        // stop arrived while a command of the operator's was in flight, so the command was
        // ended rather than waited for — which is what turned a stop that took the run's own
        // two minutes into one that takes half a second. Routine by `daemon_stopping`'s own
        // ruling: the operator asked for this, so it is not a fault.
        //
        // The census before that, 85, kept its own list for the same reason:
        //
        // **85, not the 84 the last census was taken at.** One arrival, and it is the head's
        // half of the operator's own report: `daemon_replaced` is the sentence said when the
        // process at the other end of this socket is not the one this head attached to — a
        // head outlives its daemon, and a daemon's registry is in memory, so the answers
        // still on the screen were a predecessor's. A Failure, because the session is fine
        // and the reader is being shown a picture of a daemon that no longer exists.
        //
        // The census before that, 84, kept its own list for the same reason:
        //
        //   · `operator_run_unreadable` — the daemon's half of the operator's own report: the
        //     sentence said when the process that is waiting cannot be looked at at all —
        //     `! sudo apt install mc`, where `apt` runs as root and `/proc/<pid>/fd/0` is
        //     `EACCES`. A Failure, because it is a check that did not happen and the reader's
        //     next move depends on being told.
        //
        // The census before that, 83, kept its own list for the same reason:
        //
        //   · `prompt` — the note the HEAD files when a card for the operator's own command
        //     settles, `answer sent by WHO` or `nothing sent (WHY)` — is a Failure for the reason
        //     `sudo` beside it is, and the instance that settles it is the second: an answer that
        //     did not reach a program that was waiting for it.
        //
        // The census before that, 82, kept its own list for the same reason:
        //
        //   · `prompt_late` — a line sent for a card that is no longer open, because the
        //     command ended or another head answered first — is a Failure, and it is
        //     `secret_late`'s own case one channel over: the person answered and the command
        //     did not get it, which is worse than a sentence the reader can correct;
        //   · `nothing_to_send_to` — `!send` with nothing of theirs running, or a daemon with
        //     no way to reach a command's stdin at all — is Refused: the act was well formed
        //     and the thing it names is not there, exactly like `job_output_refused`.
        //
        // The census before that, 80, kept its own list for the same reason:
        //
        //   · `subagents_stopped` — a stop took the session's children with it — is Routine
        //     by the rule at the top of the table: it reports an act somebody asked for (the
        //     operator's Esc, or the parent that killed the session), and what it puts on the
        //     screen is the list of what went with it. The one thing a reader would lose by
        //     not being told is that list, and losing it is not a session in trouble.
        //   · `wake_failed` — a child was woken because something it started had settled and
        //     the turn that would have read the settlement failed — is a Failure, and it is
        //     the register's ordinary shape: the settlement is still queued and the next wake
        //     finds it, so the session is not stuck, but a notice it was owed did not reach it.
        //
        // The census before that, 76, kept its own list for the same reason:
        //
        //   · `operator_shell_ran` — the operator's own `!` line, and what it put in the
        //     conversation — is Routine by the same ruling as the door's
        //     `operator_call_ran` beside it: the size disclosure is the feature working,
        //     and the command's own failure, if it had one, is on the `ToolResult` row it
        //     produced, under its own outcome;
        //   · `operator_shell_failed` is the failure the door's `operator_call_abandoned`
        //     is: the work happened and the RECORD of it is what is missing.
        //
        // The census before that, 73 to 74, kept its own list for the same reason:
        //
        //   · `model_slow_first_byte` — a provider that has not started answering, said while it
        //     is still silent — is Routine and therefore moves THIS register rather than the red
        //     one;
        //   · `compact_half` does the same for a fold that is running;
        //   · **`alarm_only_unregistered` is the first code here that the HEAD names rather than
        //     the daemon**, and it is a Failure by the same argument that keeps `anchor_lost`: the
        //     diagnostic reaches neither the conversation nor the triangle, so a reader who is not
        //     told has been told nothing at all;
        //   · **`operator_run_unreadable`** is the operator's own report — `! sudo apt install mc`,
        //     where the process that is waiting cannot be looked at at all. A Failure by the rule
        //     and not a routine note: it is a check that did not happen, and the sentence's job is
        //     *look at this*, because the alternative is a command that says nothing and never
        //     ends.
        assert_eq!(TABLE.len(), 90, "the table's size");
        assert_eq!(count(Class::Routine), 32);
        assert_eq!(count(Class::Refused), 9, "the nine in READER_INPUT");
        assert_eq!(count(Class::Failure), 49);
        // And the census the ruling turns on, as a ratio a reader can check: **the red
        // register is 49 of 90 and the middle is 9**, which is why the third register is a
        // correction rather than a redefinition — most of the failures were already the
        // right kind of thing.
    }

    /// **Every code in `READER_INPUT` is `Refused`, and every `Refused` code is in it.**
    ///
    /// Both directions, because each failure mode is different: a code that should have moved
    /// and did not keeps a red note on a typo, and a code that moved without being named here
    /// is a register being widened by accident.
    #[test]
    fn refused_is_exactly_the_readers_own_input() {
        let refused: BTreeSet<&str> = TABLE
            .iter()
            .filter(|(_, k)| *k == Class::Refused)
            .map(|(c, _)| *c)
            .collect();
        let named: BTreeSet<&str> = READER_INPUT.iter().copied().collect();
        assert_eq!(refused, named, "the refused set and READER_INPUT disagree");
    }

    /// **The two emission sites that decided this are one channel's two verdicts**, and they
    /// are in the same register on purpose.
    ///
    /// `slash`/`slash_refused` is literally one `match` on one boolean
    /// (`harnessd/src/sessions.rs`: `code: if reply.ok { "slash" } else { "slash_refused" }`),
    /// and `mode_set`/`mode_set_refused` is the same shape in `harness.rs`. Drawing the two
    /// verdicts of one channel in two registers is what taught a reader that red sometimes
    /// means *you made a typo* — which is the whole of R29 part two.
    #[test]
    fn one_channels_two_verdicts_share_a_register() {
        for (ok, refused) in [("slash", "slash_refused"), ("mode_set", "mode_set_refused")] {
            assert_ne!(
                class(ok),
                Class::Failure,
                "`{ok}` is the acceptance half and was a Failure"
            );
            assert_eq!(
                class(refused),
                Class::Refused,
                "`{refused}` is the same emission as `{ok}` with the other verdict, and \\
                 red on a typo is the defect"
            );
        }
    }

    /// **The three that look like the reader's act and are not**, kept as failures on the
    /// discriminator rather than on the name: what is at risk is beyond the line they typed.
    ///
    /// This is the test that stops the middle register becoming the place everything goes when
    /// a code is hard to classify. Each of these three is a code somebody could argue into
    /// `Refused` — a mode that did not persist, a title that did not store, a password that
    /// arrived late — and each is worse than that: the durable half did not happen, or a
    /// privileged call did not run.
    #[test]
    fn what_is_at_risk_beyond_the_line_stays_a_failure() {
        for code in ["mode_unpersisted", "title_not_stored", "secret_late"] {
            assert_eq!(
                class(code),
                Class::Failure,
                "`{code}` is not the reader's input being wrong"
            );
        }
        // And the other direction: a code that is a fact about the session, however much the
        // reader prompted it, stays red.
        for code in [
            "context_wall",
            "ledger_chain_mismatch",
            "prefix_divergence",
            "turn_failed",
        ] {
            assert_eq!(class(code), Class::Failure, "`{code}`");
        }
    }

    /// The predicate the head used to render with still answers for the quiet end, and a code
    /// the table has never heard of is still drawn loudly — the safe direction to be wrong in.
    #[test]
    fn the_unknown_code_is_still_a_failure() {
        assert!(is_routine("compacted"));
        assert!(
            !is_routine("slash_refused"),
            "a refusal is not housekeeping"
        );
        assert!(!is_failure("slash_refused"), "and it is not red either");
        assert_eq!(class("a_code_from_the_future"), Class::Failure);
        assert!(is_failure("a_code_from_the_future"));
    }
}
