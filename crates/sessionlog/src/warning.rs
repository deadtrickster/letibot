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
/// count over the table: **69 codes — 23 Routine, 46 Failure** — of which **7** are *the
/// reader asked for something that is not there*:
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
    // **The operator's own call ran** — R31's size disclosure. Routine by the rule
    // above: it is a sentence about something the operator asked for that worked, and
    // deleting it loses only the number they were owed. It is the count of context they
    // just chose to buy, said while they can still act on it — the opposite of a fault.
    ("operator_call_ran", Class::Routine),
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

/// [`class`], as the question a renderer asks about the quiet end.
pub fn is_routine(code: &str) -> bool {
    class(code).is_routine()
}

/// [`class`], as the question a renderer asks about the **red** end — and the one R29 part
/// two is about, because `Refused` is neither.
pub fn is_failure(code: &str) -> bool {
    class(code).is_failure()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        for code in ["daemon_stopping", "compacted", "auto_compact", "reseated"] {
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
        "reseat_refused",
        "slash_refused",
    ];

    /// **The census, pinned.** 71 codes, of which **7** are the reader's own input refused.
    ///
    /// R29 part two's instruction was to *measure before ruling*, and this is the measurement
    /// kept where it cannot drift: `Class`'s docs quote these numbers, and a code moved or
    /// added without a thought fails here rather than silently changing what a reader is
    /// taught by the colour of the screen.
    #[test]
    fn the_table_is_24_routine_7_refused_and_40_failures() {
        let count = |c: Class| TABLE.iter().filter(|(_, k)| *k == c).count();
        // **71, not the 70 the census was taken at**, because `model_slow_first_byte` —
        // a provider that has not started answering, said while it is still silent — is
        // Routine and therefore moves THIS register rather than the red one. Counted
        // rather than left implicit, because a census that quietly moves is not a census.
        assert_eq!(TABLE.len(), 71, "the table's size");
        assert_eq!(count(Class::Routine), 24);
        assert_eq!(count(Class::Refused), 7, "the seven in READER_INPUT");
        assert_eq!(count(Class::Failure), 40);
        // And the census the ruling turns on, as a ratio a reader can check: **the red
        // register is 40 of 71 and the middle is 7**, which is why the third register is a
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
