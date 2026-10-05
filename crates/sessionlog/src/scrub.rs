//! The scrub, as a **projection** rather than an afterthought.
//!
//! §13.2b: *"Live and stored are different artifacts of the same stream, and
//! anything interactive must be stripped from the stored copy … otherwise they are
//! answered or acted on twice."*
//!
//! The generalisation this crate commits to is one predicate:
//!
//! > An event is **interactive** if replaying it to a reader could cause an action
//! > that has already happened, or display a state that is no longer true.
//!
//! [`is_interactive`] is that predicate and it is the *only* place the rule lives.
//! Everything downstream — the late-join snapshot, the resume gap, a stored
//! session file — is `filter(!interactive)` composed with a fold. A new event
//! variant therefore has exactly one place to be classified, and forgetting is a
//! compile error because the match is exhaustive.
//!
//! There are two shapes of interactivity and they are stripped differently:
//!
//! - **Ephemeral state.** `PromptProgress`, `ToolProgress`. Their durable residue
//!   is the event that ends them (`TurnFinished`, `ToolFinished`). Dropped
//!   outright.
//! - **Open questions.** `DecisionRequested`. Its durable residue is the
//!   `DecisionAnswered` that settled it — so a settled request is dropped and the
//!   answer is kept, which is §13.2b's *"settled decisions render as their outcome,
//!   not as an open prompt"*. An **unsettled** request is not interactive-in-the-bad-
//!   sense: it is still owed an answer, and a reattaching head is exactly who owes
//!   it.
//!
//! And in every case the count of what was stripped travels with the result
//! ([`ScrubReport`]), because *"busy, and none of it was for me"* is a different
//! fact from *"quiet"*.

use std::collections::{HashMap, HashSet};

use crate::event::{DecisionOutcome, Envelope, SessionEvent};

/// Which artifact of the stream is being produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Projection {
    /// Fan-out to an attached head, as it happens. Nothing is stripped: an open
    /// question *should* reach the head that is there to answer it.
    Live,
    /// Anything replayed — a late head's snapshot, a resume gap, a stored session.
    /// Interactive frames are stripped.
    Stored,
}

/// Is replaying this event capable of causing a second action, or of showing a
/// state that has since moved?
///
/// `DecisionRequested` answers "yes" here and is then *rescued* by
/// [`StoredProjection`] when it is still unsettled, because the predicate cannot
/// see the future and settledness is a property of the rest of the stream. Keeping
/// the two apart is deliberate: the predicate stays a property of one event.
pub fn is_interactive(event: &SessionEvent) -> bool {
    match event {
        // Ephemeral: a progress frame from four minutes ago is a lie about now.
        SessionEvent::PromptProgress { .. } => true,
        // Ephemeral, the same way: the counter's durable residue is
        // `TurnFinished`'s `usage.predicted_tokens`, and a count from a turn that
        // has ended is a state that is no longer true.
        SessionEvent::TokensGenerated { .. } => true,
        // Ephemeral, and worse: partial tool output replayed reads as new output.
        SessionEvent::ToolProgress { .. } => true,
        // An open question. Settled ones are dropped, open ones are rescued.
        SessionEvent::DecisionRequested { .. } => true,
        // A password prompt is answered on the connection that raised it, within
        // its deadline; replayed, it would show a masked field for a sudo that
        // has long since failed. Its settlement is a record and stays.
        SessionEvent::SecretRequested { .. } => true,
        // Replayed, this would ask a head to draw itself for a tool call that
        // finished long ago.
        SessionEvent::ScreenRequested { .. } => true,
        // Ephemeral, and the same as a progress frame: a job-output window is what
        // the job had written when the head asked, and a window from four minutes
        // ago is a lie about now. The job's durable residue is `JobSettled`; this
        // is the pane answering a keypress, not a fact that stays true.
        SessionEvent::JobOutput { .. } => true,
        // Ephemeral: a fill's *progress* is a line that walks while it fills, and a
        // tick from four minutes ago is a lie about now. The operation's durable residue
        // is the rows it appended and the note that says it finished; scrubbing this
        // and keeping those is exactly right.
        SessionEvent::Filling { .. } => true,
        // Ephemeral, for `Filling`'s reason exactly: a compaction's progress is a line
        // that walks while a fold runs, and a tick from four minutes ago is a lie about
        // now. What survives the compaction is the fork and the `compacted` warning —
        // scrubbing this and keeping those is the same split `Filling` makes.
        SessionEvent::CompactionProgress { .. } => true,

        // Everything below is durable: replaying it states a fact that is still
        // true, or that was true at its seq and is timestamped as such.
        SessionEvent::TurnStarted { .. }
        | SessionEvent::Delta { .. }
        | SessionEvent::ToolCallProposed { .. }
        | SessionEvent::DecisionAnswered { .. }
        | SessionEvent::ToolStarted { .. }
        | SessionEvent::ToolFinished { .. }
        | SessionEvent::TurnFinished { .. }
        | SessionEvent::TurnInterrupted { .. }
        | SessionEvent::TurnFailed { .. }
        | SessionEvent::TranscriptAppended { .. }
        // Durable, and the *point* of it is that it is durable: it exists so the
        // stored projection of the stream contains the conversation. Stripping it
        // would put back the hole it was added to close.
        | SessionEvent::TranscriptContent { .. }
        | SessionEvent::HeadAttached { .. }
        | SessionEvent::HeadDetached { .. }
        | SessionEvent::Warning { .. }
        | SessionEvent::SecretSettled { .. }
        | SessionEvent::Explain { .. }
        // Durable: "bob interrupted at 14:02" stays true. It is a record of what a
        // head did, not a request for a head to do something.
        | SessionEvent::CommandIssued { .. }
        // Durable: the session still has that name. A head replaying it re-reads a
        // fact that is still true, which is exactly the test above.
        | SessionEvent::SessionRenamed { .. }
        // Durable: the list is what it is as of this seq, and a head replaying
        // the backlog keeps the last one it saw.
        | SessionEvent::TodosUpdated { .. }
        // **Durable, and deliberately so.** A denial is a decision taken on the
        // operator's behalf which they alone can lift, and the grant path it
        // carries is still live for a late head. Scrubbing it would put back
        // exactly the hole `docs/boundary-and-adjudication.md` §4b was written
        // against — an operator who joins after the refusal seeing only a task
        // that stopped. It is not a `DecisionRequested`: nothing is waiting on an
        // answer, so there is no settled-question hazard to scrub for.
        | SessionEvent::DenialRaised { .. } => false,
        // Durable: "this session spawned a subagent and it is running/done/failed"
        // stays true, and a head replaying the backlog keeps the latest state for
        // each subagent, which is exactly what a tree is drawn from.
        | SessionEvent::Subagent { .. }
        // Durable, and the *point* of it: a background job settles between turns,
        // when nobody else is publishing, and the settlement is a fact about the
        // process that does not stop being true. Scrubbing it would put every
        // head's picture of the job back at "running" forever.
        | SessionEvent::JobSettled { .. } => false,
        // Durable: the admission is recorded whether or not the head that asked is still
        // watching. A replay that dropped it would show a `ToolResult` row with an `origin`
        // and no decision standing behind it.
        | SessionEvent::OperatorCallAllowed { .. } => false,
    }
}

/// What the scrub removed, by reason. A head that suppresses events must be able
/// to say how many, and so must the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct ScrubReport {
    /// Prefill progress frames dropped as stale.
    pub prompt_progress: u64,
    /// Generation counters dropped as stale.
    pub tokens_generated: u64,
    /// Partial tool output dropped so it is not acted on twice.
    pub tool_progress: u64,
    /// Decision prompts dropped because the decision had already settled. This is
    /// the count that, if it is ever nonzero on a *live* path, means a head is
    /// about to be asked something twice.
    pub settled_decisions: u64,
    /// Job-output windows dropped as stale. A window is what the job had written
    /// when a head asked, so a replayed one is a lie about now — the same shape as
    /// the progress frames above, and the pane re-reads rather than replaying.
    #[serde(default)]
    pub job_output: u64,
}

impl ScrubReport {
    pub fn total(&self) -> u64 {
        self.prompt_progress
            + self.tokens_generated
            + self.tool_progress
            + self.settled_decisions
            + self.job_output
    }

    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }
}

/// The stored projection of a window of the stream.
///
/// Two passes, because settledness is lookahead: pass one learns which `req_id`s
/// were answered *anywhere in the window or before it*, pass two filters. The
/// "or before it" is why [`StoredProjection::of`] takes the settled set from the
/// whole log rather than from the window — a decision answered before a resuming
/// head's `since_seq` must still not be re-asked.
#[derive(Debug, Clone, Default)]
pub struct StoredProjection {
    settled: HashSet<String>,
    outcomes: HashMap<String, DecisionOutcome>,
    report: ScrubReport,
}

impl StoredProjection {
    /// Learn what has settled, from every event the daemon still remembers.
    pub fn of<'a>(all: impl Iterator<Item = &'a Envelope>) -> Self {
        let mut p = StoredProjection::default();
        for env in all {
            if let SessionEvent::DecisionAnswered {
                req_id, outcome, ..
            } = &env.event
            {
                p.settled.insert(req_id.clone());
                p.outcomes.insert(req_id.clone(), outcome.clone());
            }
        }
        p
    }

    /// Whether a decision has been answered. The snapshot's fold uses this to
    /// decide whether a request is *open* or *settled*.
    pub fn is_settled(&self, req_id: &str) -> bool {
        self.settled.contains(req_id)
    }

    pub fn outcome(&self, req_id: &str) -> Option<&DecisionOutcome> {
        self.outcomes.get(req_id)
    }

    /// Project one envelope. `None` means it was stripped, and the reason has been
    /// counted.
    pub fn keep(&mut self, env: &Envelope) -> Option<Envelope> {
        match &env.event {
            SessionEvent::PromptProgress { .. } => {
                self.report.prompt_progress += 1;
                None
            }
            SessionEvent::TokensGenerated { .. } => {
                self.report.tokens_generated += 1;
                None
            }
            SessionEvent::ToolProgress { .. } => {
                self.report.tool_progress += 1;
                None
            }
            SessionEvent::DecisionRequested { req_id, .. } if self.settled.contains(req_id) => {
                self.report.settled_decisions += 1;
                None
            }
            // A job-output window is what the job had written when the head asked.
            // Replayed it shows a state that has moved — the job has written more,
            // or ended — so it is dropped exactly as a progress frame is, and the
            // pane that wants it asks again.
            SessionEvent::JobOutput { .. } => {
                self.report.job_output += 1;
                None
            }
            // An unsettled request survives the scrub. It is still owed an answer
            // and the reattaching head is who owes it.
            _ => {
                debug_assert!(
                    !is_interactive(&env.event)
                        || matches!(env.event, SessionEvent::DecisionRequested { .. }),
                    "BUG: an interactive event reached the stored projection unhandled. \
                     `is_interactive` and `keep` must classify the same set."
                );
                Some(env.clone())
            }
        }
    }

    pub fn report(&self) -> ScrubReport {
        self.report
    }
}

/// Project a window for replay, returning what survived and what was stripped.
pub fn scrub_replay<'a>(
    all: impl Iterator<Item = &'a Envelope>,
    window: &[Envelope],
) -> (Vec<Envelope>, ScrubReport) {
    let mut p = StoredProjection::of(all);
    let kept: Vec<Envelope> = window.iter().filter_map(|e| p.keep(e)).collect();
    (kept, p.report())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Decider, OnTimeout};
    use crate::log::{LogBounds, SessionLog};
    use crate::testing::{answered, progress, requested, tokens_generated, tool_progress, warn};

    fn log_with_a_settled_decision() -> SessionLog {
        let mut log = SessionLog::new("s", LogBounds::default());
        log.append(warn("before"));
        log.append(requested("r1", "run `rm -rf /`"));
        log.append(progress("t1"));
        log.append(tokens_generated("t1", 7));
        log.append(answered("r1", "deny"));
        log.append(tool_progress("c1", "12 of 400 lines"));
        log.append(warn("after"));
        log
    }

    #[test]
    fn a_settled_decision_replays_as_its_outcome_and_never_as_a_prompt() {
        let log = log_with_a_settled_decision();
        let window: Vec<_> = log.retained().cloned().collect();
        let (kept, report) = scrub_replay(log.retained(), &window);

        assert!(
            !kept
                .iter()
                .any(|e| matches!(e.event, SessionEvent::DecisionRequested { .. })),
            "a replayed prompt is a decision answered twice"
        );
        assert!(
            kept.iter()
                .any(|e| matches!(e.event, SessionEvent::DecisionAnswered { .. })),
            "the outcome is what survives"
        );
        assert_eq!(report.settled_decisions, 1);
        assert_eq!(report.prompt_progress, 1);
        assert_eq!(report.tokens_generated, 1);
        assert_eq!(report.tool_progress, 1);
        assert_eq!(report.total(), 4);
    }

    #[test]
    fn an_unsettled_decision_survives_because_it_is_still_owed_an_answer() {
        let mut log = SessionLog::new("s", LogBounds::default());
        log.append(requested("r9", "write to /etc/hosts"));
        let window: Vec<_> = log.retained().cloned().collect();
        let (kept, report) = scrub_replay(log.retained(), &window);
        assert_eq!(kept.len(), 1);
        assert_eq!(report.settled_decisions, 0);
    }

    #[test]
    fn a_job_settlement_survives_the_scrub_because_it_is_the_only_record_of_it() {
        // A background job settles between turns, when nothing else is publishing.
        // The settlement is a fact about a process that already happened; scrubbing
        // it would put every head's picture of the job back at "running" forever,
        // which is the lie about now this module exists to keep off the wire.
        let mut log = SessionLog::new("s", LogBounds::default());
        log.append(SessionEvent::JobSettled {
            job: "j3".into(),
            state: "exited 0".into(),
            produced: 512,
            elapsed_ms: 1_400,
        });
        let window: Vec<_> = log.retained().cloned().collect();
        let (kept, report) = scrub_replay(log.retained(), &window);
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert!(report.is_empty(), "{report:?}");
        assert!(
            matches!(&kept[0].event, SessionEvent::JobSettled { job, state, .. } if job == "j3" && state == "exited 0"),
            "{:?}",
            kept[0].event
        );
    }

    #[test]
    fn a_job_output_window_is_stripped_from_the_replay_and_the_count_travels() {
        // A window is what the job had written when the head asked, so replaying it
        // to a late head states a now that has moved — and there is nothing for
        // that head to have asked for. Dropped, like a progress frame, and counted
        // so "busy" and "quiet" stay distinguishable.
        let mut log = SessionLog::new("s", LogBounds::default());
        log.append(SessionEvent::JobOutput {
            job: "j4".into(),
            from: 0,
            to: 10,
            produced: 10,
            dropped: 0,
            state: "exited 0".into(),
            never_ran: false,
            lines: vec!["hello".into()],
            next: None,
        });
        let window: Vec<_> = log.retained().cloned().collect();
        let (kept, report) = scrub_replay(log.retained(), &window);
        assert!(
            kept.is_empty(),
            "a replayed window is a lie about now: {kept:?}"
        );
        assert_eq!(report.job_output, 1);
    }

    #[test]
    fn settledness_is_learned_from_the_whole_log_not_only_from_the_window() {
        // The answer is at seq 4; a head resuming from seq 4 gets a window that
        // starts after it. Without the whole-log pre-pass the request at seq 2 is
        // not in the window either, so this test only bites once a request is
        // re-emitted — which is exactly what a RESYNC snapshot does. Assert on the
        // set directly.
        let log = log_with_a_settled_decision();
        let p = StoredProjection::of(log.retained());
        assert!(p.is_settled("r1"));
        assert!(matches!(
            p.outcome("r1"),
            Some(DecisionOutcome::Selected { option_id }) if option_id == "deny"
        ));
    }

    #[test]
    fn the_predicate_is_the_only_place_the_rule_lives() {
        // Every variant is classified. If a new one is added the match in
        // `is_interactive` fails to compile, which is the point.
        for e in crate::testing::one_of_each() {
            let interactive = is_interactive(&e);
            let expected = matches!(
                e,
                SessionEvent::PromptProgress { .. }
                    | SessionEvent::TokensGenerated { .. }
                    | SessionEvent::ToolProgress { .. }
                    | SessionEvent::DecisionRequested { .. }
                    | SessionEvent::JobOutput { .. }
            );
            assert_eq!(interactive, expected, "{}", e.kind());
        }
    }

    #[test]
    fn a_decision_that_timed_out_is_settled_and_is_not_re_asked() {
        let mut log = SessionLog::new("s", LogBounds::default());
        log.append(SessionEvent::DecisionRequested {
            write_targets: Vec::new(),
            req_id: "r2".into(),
            kind: "exec".into(),
            call_id: None,
            access: String::new(),
            summary: "run it".into(),
            target: String::new(),
            detail: String::new(),
            options: vec![],
            choices: vec![],
            because: String::new(),
            advice: None,
            subagent: None,
            deadline: Some(1),
            on_timeout: OnTimeout::Deny,
        });
        log.append(SessionEvent::DecisionAnswered {
            req_id: "r2".into(),
            outcome: DecisionOutcome::TimedOut,
            by: Decider {
                kind: "timeout".into(),
                identity: String::new(),
            },
            basis: "deadline passed".into(),
            late: false,
        });
        let window: Vec<_> = log.retained().cloned().collect();
        let (kept, _) = scrub_replay(log.retained(), &window);
        assert_eq!(kept.len(), 1);
        assert!(matches!(
            kept[0].event,
            SessionEvent::DecisionAnswered { .. }
        ));
    }
}
