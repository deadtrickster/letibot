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

/// **A warning's severity, for the register it is drawn in.**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Housekeeping, a command's own answer, or a hiccup being handled. Drawn in the
    /// dim register — no `!`, no red.
    Routine,
    /// Something did not work, did not happen, was refused, or could not be checked.
    /// Drawn as a failure: red, prefixed `!`.
    Failure,
}

impl Class {
    pub fn is_routine(self) -> bool {
        matches!(self, Class::Routine)
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
    // Their interrupt and their promote arrived between turns, so there was nothing to
    // stop or move. The operator's own act, and a no-op.
    ("interrupt_idle", Class::Routine),
    ("promote_idle", Class::Routine),
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
    // The turn did not do what it should, or the session is out of room.
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
    ("reseat_refused", Class::Failure),
    // The capture of a refused frame did not happen, which is the one branch of that
    // trio that is not the mechanism working.
    ("frame_capture_failed", Class::Failure),
    // An answer to a password prompt arrived with nothing waiting for it — the helper had
    // given up, or another head answered first — so the call that needed the password did
    // not get one.
    ("secret_late", Class::Failure),
    // The mode they named does not exist, or the session cannot carry the one they asked
    // for. A refusal of their own command, and it has to be read: the point did NOT move.
    ("mode_unknown", Class::Failure),
    ("mode_set_refused", Class::Failure),
    ("mode_unpersisted", Class::Failure),
    // A seat is attached to this session and nothing said on the fabric can wake it.
    ("flowy_not_seated", Class::Failure),
    // An answer reached the queue with nothing waiting for it — one of the two branches
    // says outright that this is a defect.
    ("answer_unclaimed", Class::Failure),
    ("job_output_refused", Class::Failure),
    ("slash_refused", Class::Failure),
    // The store, in every way it can fail to do its job: the session does not open, the
    // transcript store cannot be written, the decision corpus cannot be consulted, the
    // title cannot be recorded, the session the operator asked for does not resume, and
    // an import that stopped part-way or had nothing to read.
    ("session_unavailable", Class::Failure),
    ("transcript_store", Class::Failure),
    ("decision_corpus", Class::Failure),
    ("title_not_stored", Class::Failure),
    ("resume_failed", Class::Failure),
    ("import_no_db", Class::Failure),
    ("import_no_session", Class::Failure),
    ("import_failed", Class::Failure),
    ("fabric_refresh_failed", Class::Failure),
    // **This head's own failures.** Events that never reached it, a frame it could not
    // read, a daemon that disagrees about the protocol, a row's content with no row to
    // land on, and a password prompt that settled — including the case where nobody
    // gave one, which is a privileged call that did not happen.
    ("log_gap", Class::Failure),
    ("unreadable_frame", Class::Failure),
    ("protocol_skew", Class::Failure),
    ("orphan_body", Class::Failure),
    ("sudo", Class::Failure),
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

/// [`class`], as the question the renderer actually asks.
pub fn is_routine(code: &str) -> bool {
    class(code).is_routine()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four the operator was met by, and the one that must never go quiet.
    #[test]
    fn the_complaint_is_routine_and_a_refusal_is_not() {
        for code in ["daemon_stopping", "compacted", "auto_compact", "reseated"] {
            assert_eq!(class(code), Class::Routine, "{code}");
        }
        for code in ["turn_failed", "context_wall", "slash_refused"] {
            assert_eq!(class(code), Class::Failure, "{code}");
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
