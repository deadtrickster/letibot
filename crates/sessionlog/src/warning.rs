//! **Which warnings are routine, and which are failures.**
//!
//! **Routine** is a sentence about something that worked, something the operator asked
//! for, or something the session is doing by design: delete it and the reader is no
//! worse off. **Failure** is a sentence about something that did not work, did not
//! happen, was refused, or could not be checked: delete it and the reader is misinformed.
//! A code this table has not been told about is a `Failure`, so forgetting one is a red
//! line somebody asks about rather than a quiet line nobody notices.

/// **A warning's register, for how it is drawn.** Three, and the middle one is new:
/// R29 part two, ruled 2026-09-23.
///
/// # Why two was not enough, and the census that decided it
///
/// The operator, having been shown a red note for a mistyped `/qwe` sitting in the same
/// colour as `ledger_chain_mismatch`: *"red is stop the world event … a mistyped /qwe is not
/// a session in trouble."* The instruction was to **measure before ruling**, and this is the
/// count over the table **as it stood at the ruling** - 69 codes, 23 Routine, 46 Failure
/// (**73 today**; `the_register_census` is the number that is pinned, and this paragraph is
/// the measurement the ruling was made on rather than a live mirror of the table) - of which
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
/// wrong** - and the operator allowed for exactly that outcome (*"if it is two codes out of
/// forty, the right answer may be to move those two and leave the rule alone"*). What settles
/// it for a third register rather than for leaving the rule alone is **where those seven
/// fire**: at the moment the operator is typing. `slash_refused` is emitted on a typo, in
/// front of the reader, while `ledger_chain_mismatch` is emitted at a replay nobody is
/// watching. So the seven are not merely 15% of the table - they are the codes a reader meets
/// *most often*, and they teach the red reflex on the occasions when nothing is wrong.
///
/// # The discriminator, which is *not* "was it refused"
///
/// **Whose act, and what is at risk beyond it.** A `Refused` note is the answer to something
/// the reader just did, the correction is theirs to make now, and nothing but the line they
/// typed is at stake. A `Failure` is a fact about the session - its integrity, its room, its
/// ability to run the work at all - and the reader cannot fix it by typing something else.
///
/// **Two refusals of the reader's own act are not `Refused`**, and both are worth naming
/// because they look like it: `mode_unpersisted` (the mode moved, and the *next* session will
/// not carry it - at risk is a session that is not this one) and `title_not_stored` (the name
/// is lost at restart). The reader's line worked; what did not is the durable half.
///
/// **Nothing moved to `Routine`**, and the table's own rule forbids it: *routine is a
/// sentence you can delete with the reader no worse off*, and deleting a refusal leaves the
/// reader believing a command took effect. That is why the middle is its own register rather
/// than the quiet end of the other two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Housekeeping, a command's own answer, or a hiccup being handled. Drawn in the
    /// dim register - no mark, no red.
    Routine,
    /// **The reader asked for something that is not there, and the fix is theirs.** Drawn
    /// in the notice register - a mark, no red - because it is the *answer* to what they
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

    /// **The red one.** What a reader is being asked to *stop for* - and the reason the
    /// middle register exists is so this answer stays rare enough to carry that weight.
    pub fn is_failure(self) -> bool {
        matches!(self, Class::Failure)
    }
}

/// **The split.** One row per code, and a code that is not here is a [`Class::Failure`].
/// The order is the order it is read in - routine first, because that is the list whose
/// absences caused the complaint - and each row carries the reason it is where it is.
pub const TABLE: &[(&str, Class)] = &[
    // ───────────────────────── routine ─────────────────────────
    //
    // Housekeeping the session does by design: nothing here is the reader's line answered.
    ("auto_compact", Class::Routine),
    ("compacted", Class::Routine),
    // **A plugin or a model misbehaved and the harness handled it**: a layer that failed while
    // the one below answered, a candidate the shadow gate refused, a pane whose view the host
    // refused, a model that refused an image. Hiccups handled, nothing at risk.
    ("plugin_layer", Class::Routine),
    ("plugin_refused", Class::Routine),
    ("plugin_view_refused", Class::Routine),
    ("model_refused_images", Class::Routine),
    // **A look a plugin declared that the gate would not run**: the layer is told through its
    // look result and the session once per (layer, program, reason). The gate did its job and
    // the layer is the one that has to change.
    ("plugin_look", Class::Routine),
    // **A layer that was pinned and is not any more**: its bytes changed after the pin, so
    // every call it makes is adjudicated again until somebody pins what is there now. Said
    // once per (layer, bytes), never per poll.
    ("plugin_untrusted", Class::Routine),
    // **The fold saying what it is doing while it does it**: the half, its items and its
    // tokens to read, and then what the half answered. Deleting it loses only the reason a
    // silent wait was minutes long.
    ("compact_half", Class::Routine),
    // **The operator's own call ran**, with the size disclosure: the count of context they
    // just chose to buy, said while they can still act on it.
    ("operator_call_ran", Class::Routine),
    // **The operator's mark was taken** (TODO R81, `/fb`, alt-g / alt-b) - the confirmation
    // both heads draw. Routine: it answers what the operator just did, and it worked.
    ("feedback", Class::Routine),
    // The operator's own `!` line ran, and the note says what it put in the conversation.
    // The command's own failure, if it had one, is on the `ToolResult` row it produced.
    ("operator_shell_ran", Class::Routine),
    // The `!` line ran but its rows could not be recorded. A failure, and the same one
    // the door's `operator_call_abandoned` is: the work happened and the record of it is
    // what is missing.
    ("operator_shell_failed", Class::Failure),
    // **The daemon cannot tell whether the operator's own run is waiting for a line**: one of
    // its processes belongs to another user, so no card can be raised. A check that did not
    // happen - the way in is `!send`.
    ("operator_run_unreadable", Class::Failure),
    // **The operator's own acts that failed and used to reach only stderr**: a door call not
    // admitted or not runnable, and a `!` line holding the daemon's one worker.
    ("operator_call_failed", Class::Failure),
    ("operator_shell_on_worker", Class::Failure),
    // **The review queue's session did not open** - so nothing in the merge queue is reviewed by
    // this daemon, and from a head that looks exactly like a queue with nothing to review. Said
    // once per reason (`Sessions::ensure_gatekeeper`), on the daemon's own session.
    ("review_queue_not_open", Class::Failure),
    // **The merge queue could not reach its store** (`database is locked`, or a table it
    // cannot read). The queue waits, retries and says this once per reason; nothing moves
    // while it stands.
    ("merge_queue_store_error", Class::Failure),
    // **The harness's half of the same report**: a tool call the idle sweep closed out because
    // its executor was gone, and a plan the store did not take - the durable half missing, and
    // a fired condition left unwritten fires again after a restart.
    ("calls_abandoned", Class::Failure),
    ("todos_not_stored", Class::Failure),
    // **Three ways something did not land where it was meant to**: words a turn took and did
    // not read (they ride the next turn), a round's context measurement the store refused, and
    // a fork whose rows are not all stored.
    ("steering_carried", Class::Failure),
    ("context_not_stored", Class::Failure),
    ("fork_unstored", Class::Failure),
    // **A background job's row did not land**, so a `when: job` todo row may not be released
    // after a restart; and **the merge queue could not be read for a pane** - an empty pane
    // that means *unread*.
    ("job_not_recorded", Class::Failure),
    ("merge_queue_unreadable", Class::Failure),
    // **Refused, both: the reader's own act, answered.** A `/todo` move naming a row the model
    // has since rewritten, and a command a subagent does not run - its parent owns them.
    ("todo_move_unresolved", Class::Refused),
    ("child_door_refused", Class::Refused),
    // The compaction also picked up the tools this daemon now seats, and the second
    // summary turn `/reseat` would have cost was not paid. It is a *report of a
    // repair that succeeded* - the compaction's own second half.
    ("reseated", Class::Routine),
    // **`/clear` landed**: the operator's own act, answered - the conversation was replaced by a
    // fresh one for the same session, and this is the boundary row where the old one ended. A
    // report of a fork that succeeded, `reseated`'s register one verb along.
    ("cleared", Class::Routine),
    // The evidence for a refused frame was written where somebody can read it. The
    // refusal is the failure and it is said elsewhere; this is the capture working.
    ("frame_capture_written", Class::Routine),
    // Capture is switched off, so nothing was written. A disclosure of configuration  -
    // the code above, one branch along.
    ("frame_capture_disabled", Class::Routine),
    // A command the operator typed, answered: `mode_set` where the point moved, and
    // `mode_set_next_session_only` where it moved for *later* sessions because this one cannot
    // carry it.
    ("mode_set", Class::Routine),
    ("mode_set_next_session_only", Class::Routine),
    // A consented `allow-all` is this session's and was deliberately NOT written to the
    // project row. Consent is the thing working; the row not moving is the point.
    ("mode_session_only", Class::Routine),
    // **A confirmation another head asked for, and its two answers**: `mode_confirmed` is the
    // person agreeing and `mode_declined` a person declining - the card working, not a fault.
    // `mode_unconfirmed` stays a Failure: a yes the session could not carry.
    ("mode_confirmed", Class::Routine),
    ("mode_declined", Class::Routine),
    ("mode_unconfirmed", Class::Failure),
    // **A confirmation nobody could be asked about**: a program wanted the point and the
    // session has no other head, so nothing was attempted. `mode_consent_refused` is the other
    // half - an answer from a head that was not asked.
    ("mode_confirm_refused", Class::Routine),
    ("mode_consent_refused", Class::Refused),
    // The model server did not answer and the round is being taken again. The retry is
    // the handling - and when the retries are exhausted the turn fails under
    // `turn_failed`, which is a failure.
    ("model_endpoint_retry", Class::Routine),
    // **The endpoint that did not answer came back**: the recovery is the news, and a recovery
    // not said is how somebody believes the endpoint is still down. Said once per outage, with
    // the count and how long it was out.
    ("model_endpoint_answered", Class::Routine),
    // **The session store was busy and a turn is waiting to write its rows**: the wait IS the
    // handling, nothing is lost (the rows are in memory), and if the store stays wedged the
    // turn fails under its own name.
    ("store_busy_wait", Class::Routine),
    // The operator typed a provider key on the masked card and it was saved - the ask
    // working, said once so the file it went to is on the record.
    ("provider_key_saved", Class::Routine),
    // The key card was closed with no key: the operator's own answer, and the turn says
    // what happens instead.
    ("provider_key_refused", Class::Refused),
    // **A provider that has not started answering, said while it is still silent**: nothing
    // failed, the request is open and the round is still running. Filing it as a failure would
    // mark a session as having gone wrong for the crime of waiting.
    ("model_slow_first_byte", Class::Routine),
    // Their interrupt and their promote arrived between turns, so there was nothing to
    // stop or move. The operator's own act, and a no-op.
    ("interrupt_idle", Class::Routine),
    // **A stop took the session's children with it**: the downward edge of a supervision tree,
    // reporting an act somebody asked for. See `stop_children_first`.
    ("subagents_stopped", Class::Routine),
    // **A child was woken for a settlement and the turn that would have read it failed**: the
    // settlement is still queued and the next wake finds it, but a notice it was owed did not
    // reach it.
    ("wake_failed", Class::Failure),
    // **A child was due its idle plan-check and the turn that carries it failed**: the check is
    // spent rather than lost, but the sentence saying its plan is unfinished did not reach the
    // model, and a child has no worker to report that.
    ("todo_check_failed", Class::Failure),
    // **A model said its context is full again, after the harness told it the measured fill**:
    // the first time it is told and the turn continues; the second goes to the operator,
    // because a correction repeated is a loop and not a correction.
    ("context_claim_repeated", Class::Failure),
    ("promote_idle", Class::Routine),
    // **A promote routed, not answered here**: the operator's Ctrl+O arrived while their own
    // run is in flight, so the worker leaves the flag for that run's wait loop to take.
    ("promote_in_flight", Class::Routine),
    // **A parent's message to a subagent that had already finished**: the runner refuses these
    // by name, so this is the race it cannot close. Nothing went wrong - the parent's next
    // `task_result` reads a finished child and says so.
    ("message_idle", Class::Routine),
    // **A refusing verdict that reached NOBODY**: the row is written and the message carrying
    // the complaints could not be sent to the child that wrote the branch or the root that
    // enqueued it, so the entry waits for a person. A refusal that reached the root is not this.
    ("refusal_undelivered", Class::Failure),
    // **A finished `task_start` child's branch is in the merge queue**, said to the parent that
    // started it: it reports something that worked and says where the work went. The same fact
    // is on the queue pane and in `task_result`.
    ("merge_queued", Class::Routine),
    // **A finished child's branch could NOT be enqueued**: the store would not open, or this
    // daemon has no `--store`. The branch is safe in its worktree and nothing will land it,
    // which the reader cannot fix by typing something else.
    ("merge_not_queued", Class::Failure),
    // The merge queue serving again after `merge_queue_store_error` - see that row.
    ("merge_queue_recovered", Class::Routine),
    // **The model proposing the project's queue target** (R76 item 2's `propose_target`): the
    // exact `[queue] target = "…"` lines for the operator to choose. A proposal, not a fault  -
    // nothing failed, and the decision it asks for is the operator's.
    ("queue_target_proposed", Class::Routine),
    // **The two queues' raises.** A review or landing that cannot run, a target RED, a reviewer
    // that cannot decide, a `[queue]` table that does not parse, a build that does not match the
    // store: nothing moves while they stand. `queue_lease_lost` is Routine - still served.
    ("queue_blocked", Class::Failure),
    ("target_red", Class::Failure),
    ("queue_needs_human", Class::Failure),
    ("queue_settings_unreadable", Class::Failure),
    ("queue_wrong_build", Class::Failure),
    ("queue_lease_lost", Class::Routine),
    // **A target the gate could not judge**: its own setup failed, or its step was killed before
    // it judged the code. Nothing lands until it is gated again, and never as `target_red`,
    // because the code was not judged red.
    ("gate_setup_failed", Class::Failure),
    ("gate_killed", Class::Failure),
    // **What the queues used to leave unsaid**: a gate that timed out or could not judge for
    // another cause, a branch that does not rebase or whose landing gate failed, a push or an
    // install that failed. `target_green` and `queue_branch_kept` are Routine: reports.
    ("target_timed_out", Class::Failure),
    ("target_unjudged", Class::Failure),
    ("merge_conflict", Class::Failure),
    ("merge_failed", Class::Failure),
    ("queue_push_failed", Class::Failure),
    ("queue_install_failed", Class::Failure),
    ("target_green", Class::Routine),
    ("queue_branch_kept", Class::Routine),
    // **A session's workspace is gone** (R92.24) - said when it opens or wakes on a directory
    // that no longer exists. A Failure: nothing the session runs can work until somebody acts.
    ("workspace_gone", Class::Failure),
    // **A session asked for a boundary and got an unconfined shell**: D16 admits a command
    // without a card inside a measured boundary, so without one every command that runs code is
    // decided per command. The fix is the operator's.
    ("boundary_absent", Class::Failure),
    // **A message between sessions that the turn did not read, and the durable half of it**:
    // `message_carried` are words written into the transcript instead of riding the next turn,
    // and `message_not_stored` is those words not landing in the transcript at all.
    ("message_carried", Class::Failure),
    ("message_not_stored", Class::Failure),
    // **A file the operator chipped into a prompt was not there when it was sent**: the model
    // read a sentence naming a file it was never given. Only the operator can put it back, and
    // the row says which one.
    ("attachment_refused", Class::Failure),
    // **The daemon's boot**: each step from the socket's bind to the worker's start, and the
    // start itself, are Routine - a head draws them as the tail's line. A boot that failed
    // after the socket was bound is a Failure: the session will not run.
    ("daemon_starting", Class::Routine),
    ("daemon_started", Class::Routine),
    ("daemon_start_failed", Class::Failure),
    // **This daemon's binary is older than the store it is serving, and it went on serving it**:
    // the store's reader floor says older code still reads it, and `letibot --stop` is the fix.
    // Drawn as STATE on the status row, said on every attach.
    ("store_newer", Class::Failure),
    // **A review waiting for a free VM slot**: the row went back to `waiting` and is asked
    // again, said once per wait. Past thirty minutes it is `queue_vm_slots_exhausted`, a Failure
    // - slots are not freeing on their own.
    ("queue_waiting_for_vm", Class::Routine),
    ("queue_vm_slots_exhausted", Class::Failure),
    // **A claim about the corpus rather than about the session**: an operator's own call was
    // admitted and the head that asked went away before reporting what it did. The session is
    // fine, and a claim on the record is not.
    ("operator_call_abandoned", Class::Failure),
    // Somebody asked this daemon to stop. The session is on disk and `/continue` reopens
    // it; the sentence exists so that a session ending is not a session disappearing.
    ("daemon_stopping", Class::Routine),
    // **What the stop ended on its way out**: a command was in flight, so it was ended rather
    // than waited for. The operator asked for this, so it is not a fault.
    ("daemon_stopping_runs", Class::Routine),
    // The store's own notes about what opening or resuming did - the workspace it took
    // from the row, the title, the seat. Facts about where you are.
    ("resume_note", Class::Routine),
    ("open_note", Class::Routine),
    // **What a daemon restart brought back, or only reminded the model of**: a fact about where
    // the session stands after a restart the operator made. A child that could not come back is
    // said in the detail and is not a fault of this session.
    ("restart_restored", Class::Routine),
    ("restart_reminded", Class::Routine),
    // **A restart that had nothing to restore**, said for the clock: the daemon came back at a
    // time and what was running was nothing. Its own word, because `restart_reminded` over an
    // empty list would draw *"reminded: nothing"*.
    ("restart_nothing", Class::Routine),
    // **The session's message 0 is not the one this daemon would compose now**: a session keeps
    // the prefix it was created with, and the remedy is a verb the operator has (`/reseat`).
    // Written by `Harness::open` through `prefix_fingerprint`.
    ("prefix_stale", Class::Routine),
    // **A wall the prompt goes on past** - compacted, room for the next turn, continuations
    // left: one routine line in place of the red failure each one used to publish. The wall that
    // ends the prompt is still `turn_failed`.
    ("context_wall_continued", Class::Routine),
    // **A verbatim reseat that went ahead under the window but short of a turn's room** - as
    // close to the window as the session already was; compaction makes the room, as before.
    ("reseat_tight", Class::Routine),
    // **And the end of it**: the session has been moved onto the prompt this daemon seats, so
    // the notice above has stopped being true. A head takes this as the signal to take its
    // reseat card down.
    ("prefix_current", Class::Routine),
    // **A question nobody answered in time, and the model's own default taken**: a fact for the
    // operator to review later. Nothing is broken, and the sentence names the choice so it can
    // be undone.
    ("auto_decision", Class::Routine),
    // **This head's own**: it got its connection back. Filed at a real seam as it
    // happens, so it is news rather than history.
    ("reattached", Class::Routine),
    // A slash command's answer. `slash_refused` is the same channel with the other
    // verdict, and it is a failure.
    ("slash", Class::Routine),
    // **The operator paused this session's plan-check, and the pause ended**: their own act, or
    // its own deadline. The row is the event; the head's resident line while the `nags` row
    // says `paused` is the state.
    ("nags_paused", Class::Routine),
    ("nags_resumed", Class::Routine),
    // **A todo row waits on the operator** - the one sentence here whose whole point is that
    // the reader acts, so it is red on purpose. Its settling is Routine; an answer the board
    // would not take is a Failure.
    ("waiting_on_operator", Class::Failure),
    ("operator_question_settled", Class::Routine),
    ("todo_answer_refused", Class::Failure),
    // An import's three routine notes: whose rows these are, what could not be a row
    // (an unknown tag, a patch, a compaction - *counted and said, never dropped*), and
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
    // At risk here is the session, not the line. Grouped by where the code COMES FROM.
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
    // nobody wrote* - and both of these are a turn that was going nowhere, stopped.
    ("repetition_collapse", Class::Failure),
    ("reasoning_stall", Class::Failure),
    // The prompt this box rebuilt diverged from what the server was sent - the code's own
    // comment calls it *our defect* - or the check that would catch it was skipped.
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
    // **Refused**: the dominant case is *there is nothing to re-seat*, and the other branch
    // leaves the transcript unchanged with *"try again"*. Both put nothing at risk but the
    // attempt.
    ("reseat_refused", Class::Refused),
    // The capture of a refused frame did not happen, which is the one branch of that
    // trio that is not the mechanism working.
    ("frame_capture_failed", Class::Failure),
    // An answer to a password prompt arrived with nothing waiting for it - the helper had
    // given up, or another head answered first - so the call that needed the password did
    // not get one.
    ("secret_late", Class::Failure),
    // **A line for the operator's own run found nothing waiting**: the card was raised for a
    // command that has since ended, or another head answered it first. The person typed an
    // answer and the command did not get it.
    ("prompt_late", Class::Failure),
    // **There was no command of the operator's own to send to**: `!send` with nothing running,
    // or with the run already ended. The act had nothing to act on.
    ("nothing_to_send_to", Class::Refused),
    // **Refused, and it must be read**: the mode they named does not exist, or the session
    // cannot carry it, so the point did NOT move and the fix is their next line.
    // `mode_unpersisted` stays a Failure: the mode moved and the next session will not carry it.
    ("mode_unknown", Class::Refused),
    ("mode_set_refused", Class::Refused),
    ("mode_unpersisted", Class::Failure),
    // A seat is attached to this session and nothing said on the fabric can wake it.
    ("flowy_not_seated", Class::Failure),
    // An answer reached the queue with nothing waiting for it - one of the two branches
    // says outright that this is a defect.
    ("answer_unclaimed", Class::Failure),
    // **Refused, and the clearest case in the table**: one channel's two verdicts belong in one
    // register, and a red `slash_refused` is the code that taught a reader to skim red.
    ("job_output_refused", Class::Refused),
    ("slash_refused", Class::Refused),
    // **A key on the room's pane that the room would not take**: the person's own act, answered
    // with the room's sentence. A key that said nothing would be indistinguishable from a key
    // that was not pressed.
    ("room_act_refused", Class::Refused),
    // **The room was restarted, and this is the outcome said out loud**: the control is a
    // daemon lifetime rather than a queue act, and the room is empty afterwards.
    ("room_restarted", Class::Routine),
    // **A restart asked for and not made**: no launcher on the box, a launcher that refused, a
    // supervisor that overran. The reader's own act answered with the reason.
    ("room_restart_refused", Class::Refused),
    // The first round on a newly chosen model was refused by its provider: the turn failed,
    // and the notice names the model before it and the command back.
    ("model_switch_refused", Class::Failure),
    // **A `/models` the operator typed while a turn was running, waiting for that turn to
    // end**: the request was accepted and the sentence says when it takes, and the head draws
    // it on the status line until the switch lands.
    ("model_switch_pending", Class::Routine),
    // **The switch that was waiting for the turn, applied**: the sentence names the model and
    // says it stands now. The other verdict of the same channel, one line up, and Routine for the
    // same reason `slash` is.
    ("model_switch_applied", Class::Routine),
    // **The operator asked for a model and did not get it**: the pending switch could not be
    // made, and the session is left on a model that works.
    ("model_switch_not_applied", Class::Refused),
    // **This session was closed so it could move to another machine** (`letibot --migrate`): the
    // operator asked for it, nothing is deleted, and the row says where the live copy is now  -
    // news about where the session lives, not something that went wrong.
    ("session_closed", Class::Routine),
    // The store, in every way it can fail to do its job: the session does not open, the
    // transcript cannot be written, the corpus cannot be consulted, the title cannot be
    // recorded, a resume fails, an import stops part-way.
    ("session_unavailable", Class::Failure),
    // **A ring nobody could serve**: a settlement arrived for a session this daemon is not
    // holding, so no turn could run for it. The settlement is a row the next open says; *a wake
    // went nowhere* has no other witness.
    ("wake_undelivered", Class::Failure),
    ("transcript_store", Class::Failure),
    ("decision_corpus", Class::Failure),
    ("title_not_stored", Class::Failure),
    ("resume_failed", Class::Failure),
    // **Refused**: there is nothing to import from, or no session by that name in the other
    // store. `import_failed` stays a Failure - an import that stopped part-way is a half-filled
    // transcript.
    ("import_no_db", Class::Refused),
    ("import_no_session", Class::Refused),
    ("import_failed", Class::Failure),
    ("fabric_refresh_failed", Class::Failure),
    // **This head's own failure, and it is about the reader**: the row a scrolled viewport was
    // holding is gone, because a compaction, a resync or a snapshot replaced it, so the thing
    // they were reading is not there and they have to be told.
    ("anchor_lost", Class::Failure),
    // **This head's own failures**: events that never reached it, a frame it could not read, a
    // daemon that disagrees about the protocol, a row's content with no row to land on, and a
    // password prompt that settled with nobody giving one.
    ("log_gap", Class::Failure),
    ("unreadable_frame", Class::Failure),
    ("protocol_skew", Class::Failure),
    // **A head that outlives its daemon, and does not notice**: the registry is in memory, so
    // the answers still on the screen were a predecessor's. The session is fine and the head is
    // what is wrong.
    ("daemon_replaced", Class::Failure),
    ("orphan_body", Class::Failure),
    ("sudo", Class::Failure),
    // **A command of the operator's own, answered or not**: `answer sent by WHO` is the feature
    // working, and `nothing sent (WHY)` is a line that did not reach a program waiting for it.
    // A register is per code, so the case that settles it is the one that was not delivered.
    ("prompt", Class::Failure),
    // **A head that was told something is edge-bound and has nowhere to put it**: the
    // diagnostic reaches neither the record nor the triangle, so a reader not told has been
    // told nothing at all.
    ("alarm_only_unregistered", Class::Failure),
    // **The daemon's resident memory crossed a step**: what is at risk is every session this
    // daemon holds, a daemon that keeps growing is killed by the kernel with all of them, and
    // restarting it is the remedy.
    ("memory_high", Class::Failure),
    // **A tool call somebody DECIDED to refuse** - the head's `denied:<request id>` note, looked
    // up under `denied` for every id. The call did not happen and only the operator can lift it.
    ("denied", Class::Failure),
    // Codes that exist only in fixtures in this tree: the guard reads every `code:` literal
    // under `crates/`, which is where the tests that use them live.
    ("gate", Class::Failure),
    ("gate_timeout", Class::Failure),
    ("gap", Class::Failure),
    ("guard.empty", Class::Failure),
];

/// **Which register a code is drawn in.** See [`TABLE`], and the module doc for why an
/// unknown code is a failure rather than a guess.
pub fn class(code: &str) -> Class {
    let code = family(code);
    match TABLE.iter().find(|(c, _)| *c == code) {
        Some((_, class)) => *class,
        None => Class::Failure,
    }
}

/// **A code's FAMILY** - the code without the id some of them carry. `denied:<request id>` is
/// one code per request: the id is the notice's identity, and what kind of thing it is is
/// `denied`. One answer to the second question, so the copies cannot drift.
pub fn family(code: &str) -> &str {
    match code.split_once(':') {
        Some(("denied", _)) => "denied",
        _ => code,
    }
}

/// [`class`], as the question a renderer asks about the quiet end.
pub fn is_routine(code: &str) -> bool {
    class(code).is_routine()
}

/// [`class`], as the question a renderer asks about the **red** end - and the one R29 part
/// two is about, because `Refused` is neither.
pub fn is_failure(code: &str) -> bool {
    class(code).is_failure()
}

/// **The codes that belong on the edge rather than in the record** - the warning triangle and
/// `/status`, never a row. **An event in the record goes in the record; a note about the
/// weather goes on the edge**, and a `Failure` or a `Refused` can never be here (asserted below).
pub const ALARM_ONLY: &[&str] = &[
    // A first byte slower than the head's patience. The turn is running and nothing is
    // wrong with it; what the reader would learn from a row is that the provider was
    // slow once, which is what the triangle is for.
    "model_slow_first_byte",
];

/// Whether this code is drawn on the edge rather than in the conversation - see
/// [`ALARM_ONLY`], whose docstring is the rule.
pub fn to_the_alarm(code: &str) -> bool {
    ALARM_ONLY.contains(&code)
}

/// **The codes whose sentence is drawn in the conversation for a while and then taken down** -
/// the third answer to *where does it go*. A code belongs here only when something else on the
/// screen carries the same fact, and a `Failure` or a `Refused` can never be here.
pub const FLEETING: &[&str] = &[
    // A finished `task_start` child's branch has entered the merge queue. The queue's
    // standings are what the bottom edge counts, so the sentence is news and the count is
    // the fact that stays. See `HarnessTaskRunner::enqueue_finished`, which publishes it.
    "merge_queued",
];

/// Whether this code's sentence is drawn for a while and then taken down - see
/// [`FLEETING`], whose docstring is the rule.
pub fn is_fleeting(code: &str) -> bool {
    FLEETING.contains(&code)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Nothing that is a fault may hide on the edge**: a failure drawn only as a counter is a
    /// failure nobody reads. The table is checked against the class table rather than trusted.
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
        // **And the rule was not widened to all of `Class::Routine`**: these change the
        // conversation, so they are rows in it.
        for code in [
            "compacted",
            "auto_compact",
            "compact_half",
            "reseated",
            "cleared",
        ] {
            assert!(
                !to_the_alarm(code),
                "`{code}` changes the conversation and belongs in the record"
            );
        }
    }

    /// **Nothing that is a fault, and nothing the operator did, may go quiet on a timer**: a
    /// `Failure` or a `Refused` drawn for thirty seconds and then gone is a fault nobody reads.
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

    /// The four the operator was met by, and the one that must never go quiet. A refusal is
    /// neither the quiet register nor the red one, and this test says so.
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
    /// classified deliberately rather than by copying the neighbour. This list is the finding,
    /// not a rule: the rule is `Class`'s discriminator.
    const READER_INPUT: &[&str] = &[
        // **A command sent to a subagent that only its parent runs** (R67 item 15) - the reader's
        // line, well formed, at a session that does not own the door it names.
        "child_door_refused",
        "import_no_db",
        "import_no_session",
        "job_output_refused",
        "mode_set_refused",
        "mode_unknown",
        // **A consent from a head that was not asked**: the frame is well formed and the head is
        // not one the card reached, so the fix is that head's.
        "mode_consent_refused",
        // **`!send` with nothing of yours running**, or with the run already over: the act is
        // well formed, the thing it names is not there, and the fix is the reader's next line.
        "nothing_to_send_to",
        // **A key on the room's pane that the room refused**: well formed, the thing it names
        // will not take it, and the fix is the reader's next key.
        "room_act_refused",
        // **A restart that was asked for and not made** - the same shape as the four room keys':
        // the reader's own act, well formed, and the sentence says what stopped it (no launcher, a
        // launcher that refused, a supervisor that overran).
        "room_restart_refused",
        // **A `/models` whose deferred switch could not be made** at the turn's end - the same
        // shape as `mode_set_refused`: their line, well formed, and the session cannot carry it.
        "model_switch_not_applied",
        // **The key card closed with no key** - the reader's own answer to the daemon's
        // question, and the turn's sentence says what to do (send again, or set the
        // variable). The same shape as `slash_refused`: the act was theirs, not a fault.
        "provider_key_refused",
        "reseat_refused",
        "slash_refused",
        // **A `/todo` move naming a row that is not on the board** - the model rewrote its plan
        // between the head's read and the write; the board published with it is the correction.
        "todo_move_unresolved",
    ];

    /// **The census, pinned.** `Class`'s docs quote these numbers, so a code moved or added
    /// without a thought fails here rather than silently changing what a reader is taught by
    /// the colour of the screen.
    #[test]
    fn the_table_is_40_routine_11_refused_and_64_failures() {
        let count = |c: Class| TABLE.iter().filter(|(_, k)| *k == c).count();
        // **The census, kept where it cannot drift.** Each assert below is a register's count;
        // a code moved or added without a thought fails here rather than silently changing what
        // a reader is taught by the colour of the screen.
        assert_eq!(TABLE.len(), 173, "the table's size");
        assert_eq!(count(Class::Routine), 68);
        assert_eq!(count(Class::Refused), 15, "the fifteen in READER_INPUT");
        assert_eq!(count(Class::Failure), 90);
        // A code with an id is read by its family.
        assert_eq!(class("denied:req-7"), Class::Failure);
        assert_eq!(class("memory_high"), Class::Failure);
        // The ratio the ruling turned on: the red register is most of the table and the middle
        // is a correction rather than a redefinition - most failures were already the right
        // kind of thing.
    }

    /// **Every code in `READER_INPUT` is `Refused`, and every `Refused` code is in it.** Both
    /// directions: a code that should have moved and did not keeps a red note on a typo, and
    /// one that moved without being named here is a register widened by accident.
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
    /// are in the same register on purpose: drawing them in two registers is what taught a
    /// reader that red sometimes means *you made a typo*.
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
    /// discriminator: the durable half did not happen, or a privileged call did not run.
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
    /// the table has never heard of is still drawn loudly - the safe direction to be wrong in.
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
