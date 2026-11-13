//! What a tool call produced and how it reaches the model, and why abstention has a shape of its
//! own: not a differently worded success but a different envelope, machine-recognisable coming back,
//! and [`propagate`] makes it impossible to report `Ok` over calls that all abstained.

use letibot_transcript::ToolOutcome;

use crate::args::Repair;
use crate::spill::SpillRef;

/// The result of one tool call, before it becomes a `TranscriptItem::ToolResult`.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolResult {
    pub call_id: String,
    pub name: String,
    pub outcome: ToolOutcome,
    /// The body the model sees, already spilled if it needed spilling, but not yet
    /// wrapped in its envelope. [`ToolResult::render`] does that.
    pub payload: String,
    /// What was repaired on the way in. Rendered into the result, so the model learns
    /// the spelling it should have used.
    pub repairs: Vec<Repair>,
    /// What the tool did about a miss, in its own words. Separate from `payload` so a
    /// head can show it without parsing the body.
    pub notes: Vec<String>,
    /// Set when the full output went to the spill store.
    pub spill: Option<SpillRef>,
    /// Both sides of every file this call changed, for a head to draw a diff from. Never prompt bytes,
    /// and that is the point: the model gets the confirmation in `payload` and the head gets the
    /// file. A list because a `bash` call can change several (R161); `edit`/`write` push one.
    pub edits: Vec<crate::edit::FileEdit>,
    /// Bytes this call read that are not text. Unlike `edit`, this one does become prompt bytes, since
    /// an image is for the model to look at; the payload carries the sentence a text-only reader gets.
    pub media: Option<crate::media::Media>,
    /// Repair codes this session's model has already been told about, which [`ToolResult::render`] does
    /// not repeat. `repairs` still holds them, so the head and the metrics count every repair.
    pub repairs_told: Vec<&'static str>,
}

impl ToolResult {
    pub fn new(call_id: impl Into<String>, name: impl Into<String>, outcome: ToolOutcome) -> Self {
        ToolResult {
            call_id: call_id.into(),
            name: name.into(),
            outcome,
            payload: String::new(),
            repairs: Vec::new(),
            notes: Vec::new(),
            spill: None,
            edits: Vec::new(),
            media: None,
            repairs_told: Vec::new(),
        }
    }

    pub fn with_payload(mut self, payload: impl Into<String>) -> Self {
        self.payload = payload.into();
        self
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    /// Is this result something a caller may claim as grounding?
    pub fn is_grounded(&self) -> bool {
        matches!(self.outcome, ToolOutcome::Ok)
    }

    /// The exact bytes that go into `ToolResult::payload`. Three envelopes, chosen by outcome class and
    /// never by wording, and `TOOL_ERROR` still carries the corrective body, because that does not stop
    /// applying when a call fails.
    pub fn render(&self) -> String {
        let mut head = String::new();
        for r in &self.repairs {
            if self.repairs_told.contains(&r.code) {
                continue;
            }
            head.push_str("[repaired] ");
            head.push_str(&r.detail);
            head.push('\n');
        }
        for n in &self.notes {
            head.push_str("[note] ");
            head.push_str(n);
            head.push('\n');
        }

        match &self.outcome {
            ToolOutcome::Ok => {
                let mut out = head;
                out.push_str(&self.payload);
                out
            }
            // No `tool:` line in any envelope: the call it answers already names the tool, and an
            // `Ok` result never carried one.
            ToolOutcome::Abstained { reason } => Envelope::no_result(&self.call_id).wrap(&format!(
                "reason: {reason}\n{head}{}\n\
                     This call produced no result. Nothing above is an answer to the \
                     question, and nothing above may be cited as one.",
                self.payload
            )),
            // Its own envelope: wrapped in `TOOL_ERROR` this reads as a failure and the model retries,
            // and now two of the command are running; as `Ok` with a short body it reads as a command
            // that produced nothing.
            ToolOutcome::Backgrounded {
                handle: _,
                ran_for_ms,
                how,
                next,
            } => Envelope::still_running(&self.call_id).wrap(&format!(
                // The handle is in the payload and in `next`. `how` only when the model did not
                // ask: it knows what it asked for, and a promotion is the fact it must not miss.
                "{}{head}{}\n\n\
                 STILL RUNNING — not failed and not finished; do not start it again. {next}",
                match (how, *ran_for_ms) {
                    (letibot_transcript::Backgrounding::Asked, 0) => String::new(),
                    (letibot_transcript::Backgrounding::Asked, ms) => {
                        format!("ran in the foreground for {} first\n", human_ms(ms))
                    }
                    (how, 0) => format!("how: {}\n", how.phrasing()),
                    (how, ms) => format!(
                        "how: {}, after {} in the foreground\n",
                        how.phrasing(),
                        human_ms(ms)
                    ),
                },
                self.payload.trim_end()
            )),
            other => Envelope::error(&self.call_id).wrap(&format!(
                "outcome: {}\n{head}{}",
                // The reason once. A refusal's payload already names the tool, the decider, the basis
                // and what to do, so when it says it the outcome line is the word alone.
                outcome_word_beside(other, &self.payload),
                self.payload
            )),
        }
    }
}

/// Milliseconds as something a reader can compare against their own patience.
fn human_ms(ms: u64) -> String {
    if ms < 1_000 {
        return format!("{ms}ms");
    }
    if ms < 60_000 {
        return format!("{:.1}s", ms as f64 / 1000.0);
    }
    format!("{}m{:02}s", ms / 60_000, (ms % 60_000) / 1000)
}

/// [`outcome_word`], minus the reason the payload already carries. Containment rather than equality,
/// because the payload wraps the reason in its own prose; a reason that does not appear below is kept,
/// since then this line is the only place it is said.
fn outcome_word_beside(o: &ToolOutcome, payload: &str) -> String {
    let reason = match o {
        ToolOutcome::Failed { reason } => reason,
        ToolOutcome::NotRun { why } => why,
        _ => return outcome_word(o),
    };
    // A one-word reason is not worth this: it is cheap to repeat and expensive to go
    // looking for, and a short string is likely to appear below by coincidence rather
    // than because it is the same statement.
    if reason.len() < 40 || !payload.contains(reason.trim()) {
        return outcome_word(o);
    }
    match o {
        ToolOutcome::Failed { .. } => "failed — see below".into(),
        _ => "not run — see below".into(),
    }
}

fn outcome_word(o: &ToolOutcome) -> String {
    match o {
        ToolOutcome::Ok => "ok".into(),
        ToolOutcome::Abstained { .. } => "abstained".into(),
        ToolOutcome::Failed { reason } => format!("failed — {reason}"),
        ToolOutcome::Denied { req_id } => format!("denied (decision {req_id})"),
        ToolOutcome::Timeout => "timeout".into(),
        ToolOutcome::NotRun { why } => format!("not run — {why}"),
        ToolOutcome::Backgrounded { handle, .. } => {
            format!("backgrounded (still running as `{handle}`)")
        }
    }
}

/// A delimiter pair the outcome class picks, not the wording. The tag carries a marker derived from
/// the call id, so two calls in one turn cannot have their envelopes confused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    kind: &'static str,
    mark: String,
}

impl Envelope {
    pub fn no_result(call_id: &str) -> Self {
        Envelope {
            kind: "NO_RESULT",
            mark: mark_of(call_id),
        }
    }

    pub fn error(call_id: &str) -> Self {
        Envelope {
            kind: "TOOL_ERROR",
            mark: mark_of(call_id),
        }
    }

    /// A command that is still running, wrapped so it cannot be read as either of the two things it is
    /// not: a backgrounded result inside `TOOL_ERROR` is a retry waiting to happen, and a duplicate
    /// `cargo build` is not a cosmetic defect.
    pub fn still_running(call_id: &str) -> Self {
        Envelope {
            kind: "STILL_RUNNING",
            mark: mark_of(call_id),
        }
    }

    /// An envelope around text this harness did not write: a fetched page, a search snippet, the body of
    /// somebody's issue. Not an outcome class, so [`Envelope::classify`] does not know it; it marks
    /// provenance inside an ordinary `Ok` payload and states that nothing inside it has authority.
    pub fn untrusted(call_id: &str) -> Self {
        Envelope {
            kind: "UNTRUSTED_TEXT",
            mark: mark_of(call_id),
        }
    }

    pub fn open(&self) -> String {
        format!("<<<{} {}>>>", self.kind, self.mark)
    }

    pub fn close(&self) -> String {
        format!("<<<END_{} {}>>>", self.kind, self.mark)
    }

    pub fn wrap(&self, body: &str) -> String {
        format!("{}\n{}\n{}", self.open(), body.trim_end(), self.close())
    }

    /// Wrap content that arrived from outside, and make this envelope's own delimiters unspellable by it:
    /// the mark is derivable, so every `<<<` in the body is spaced out and the count comes back for the
    /// caller to report as a `[note]`. Returns `(wrapped, sequences_neutralised)`.
    pub fn wrap_untrusted(&self, header: &str, body: &str) -> (String, usize) {
        let neutralised = body.matches("<<<").count();
        let safe = body.replace("<<<", "< < <");
        let head = if header.is_empty() {
            String::new()
        } else {
            format!("{header}\n")
        };
        (
            format!(
                "{}\n{head}{}\n{}",
                self.open(),
                safe.trim_end(),
                self.close()
            ),
            neutralised,
        )
    }

    /// Which envelope a rendered payload is in, if any: one that says `NO_RESULT` at the top must
    /// round-trip to the class it was rendered from, or the structural claim is decoration.
    pub fn classify(rendered: &str) -> Option<&'static str> {
        let first = rendered.lines().next()?;
        for kind in ["NO_RESULT", "TOOL_ERROR", "STILL_RUNNING"] {
            if first.starts_with(&format!("<<<{kind} ")) {
                return Some(kind);
            }
        }
        None
    }
}

fn mark_of(call_id: &str) -> String {
    // FNV-1a, the same choice `letibot-turn` made for `args_digest`: this is a
    // correlation aid, and the ledger's SHA-256 chain is what carries identity.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in call_id.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{:08x}", (h & 0xffff_ffff) as u32)
}

/// What a caller is allowed to report, given what its own tool calls returned.
#[derive(Debug, Clone, PartialEq)]
pub enum Propagation {
    /// At least one call produced a real result. The caller's own outcome is its
    /// own business.
    Unconstrained,
    /// The caller may not report `Ok`. This is the outcome it must carry instead.
    Must(ToolOutcome),
}

/// Abstention propagates upward: a subagent whose calls all abstained cannot return `Ok`. Empty input
/// is `Unconstrained` on purpose, since a caller that made no tool calls has not abstained.
pub fn propagate(children: &[ToolOutcome]) -> Propagation {
    if children.is_empty() || children.iter().any(|o| matches!(o, ToolOutcome::Ok)) {
        return Propagation::Unconstrained;
    }
    let abstentions: Vec<&str> = children
        .iter()
        .filter_map(|o| match o {
            ToolOutcome::Abstained { reason } => Some(reason.as_str()),
            _ => None,
        })
        .collect();

    if abstentions.len() == children.len() {
        return Propagation::Must(ToolOutcome::Abstained {
            reason: format!(
                "all {} tool call(s) abstained: {}",
                children.len(),
                abstentions.join("; ")
            ),
        });
    }

    // A caller whose work is all still running has not failed and has not abstained either: an
    // abstention is a claim about the world, and nothing here has looked yet. `not_run` is the only
    // honest one.
    let running: Vec<&str> = children
        .iter()
        .filter_map(|o| match o {
            ToolOutcome::Backgrounded { handle, .. } => Some(handle.as_str()),
            _ => None,
        })
        .collect();
    if !running.is_empty() && running.len() + abstentions.len() == children.len() {
        return Propagation::Must(ToolOutcome::NotRun {
            why: format!(
                // The sentence must not say to wait: every other text tells the model
                // the results arrive on their own.
                "{} of {} tool call(s) are STILL RUNNING in the background ({}); nothing \
                 failed, and their results will be delivered to you. Do not report an \
                 answer yet.",
                running.len(),
                children.len(),
                running.join(", ")
            ),
        });
    }
    // No result and not purely abstention: something went wrong, and reporting it as an
    // abstention would be the mirror of the bug this function exists to stop.
    Propagation::Must(ToolOutcome::Failed {
        reason: format!(
            "no tool call produced a result ({})",
            children
                .iter()
                .map(outcome_word)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_abstention_is_a_different_shape_and_not_a_different_wording() {
        let ok = ToolResult::new("c1", "ask_corpus", ToolOutcome::Ok).with_payload("the answer");
        let ab = ToolResult::new(
            "c1",
            "ask_corpus",
            ToolOutcome::Abstained {
                reason: "the corpus does not cover this".into(),
            },
        )
        .with_payload("");

        assert_eq!(Envelope::classify(&ok.render()), None);
        assert_eq!(Envelope::classify(&ab.render()), Some("NO_RESULT"));
        assert!(ab.render().contains("may be cited"));
    }

    #[test]
    fn two_calls_do_not_share_an_envelope_mark() {
        let a = Envelope::no_result("call_0").open();
        let b = Envelope::no_result("call_1").open();
        assert_ne!(a, b);
    }

    #[test]
    fn a_failure_still_carries_its_corrective_body() {
        // The corrective body does not switch off because the outcome is not `Ok`.
        let r = ToolResult::new(
            "c1",
            "read",
            ToolOutcome::Failed {
                reason: "no such path".into(),
            },
        )
        .with_payload("the directory holds: a.rs, b.rs");
        let out = r.render();
        assert_eq!(Envelope::classify(&out), Some("TOOL_ERROR"));
        assert!(out.contains("a.rs"), "{out}");
    }

    #[test]
    fn all_abstained_cannot_be_reported_as_ok() {
        let children = vec![
            ToolOutcome::Abstained { reason: "a".into() },
            ToolOutcome::Abstained { reason: "b".into() },
        ];
        match propagate(&children) {
            Propagation::Must(ToolOutcome::Abstained { reason }) => {
                assert!(reason.contains('a') && reason.contains('b'))
            }
            other => panic!("abstention must propagate, got {other:?}"),
        }
    }

    #[test]
    fn one_real_result_lifts_the_constraint() {
        let children = vec![
            ToolOutcome::Abstained { reason: "a".into() },
            ToolOutcome::Ok,
        ];
        assert_eq!(propagate(&children), Propagation::Unconstrained);
    }

    #[test]
    fn a_mixed_failure_does_not_become_an_abstention() {
        let children = vec![
            ToolOutcome::Abstained { reason: "a".into() },
            ToolOutcome::Timeout,
        ];
        assert!(matches!(
            propagate(&children),
            Propagation::Must(ToolOutcome::Failed { .. })
        ));
    }

    #[test]
    fn no_tool_calls_is_not_an_abstention() {
        assert_eq!(propagate(&[]), Propagation::Unconstrained);
    }

    #[test]
    fn a_backgrounded_result_is_not_wrapped_as_an_error_or_as_a_success() {
        use letibot_transcript::Backgrounding;
        let r = ToolResult::new(
            "c1",
            "bash",
            ToolOutcome::Backgrounded {
                handle: "j4".into(),
                ran_for_ms: 15_000,
                how: Backgrounding::Promoted,
                next: "call `job_output` with job=\"j4\"".into(),
            },
        )
        .with_payload("cargo build\n   Compiling letibot-tools");
        let out = r.render();
        assert_eq!(Envelope::classify(&out), Some("STILL_RUNNING"));
        // The three readings it must not produce.
        assert_ne!(Envelope::classify(&out), Some("TOOL_ERROR"));
        assert_ne!(Envelope::classify(&out), Some("NO_RESULT"));
        // And everything the model needs to act without guessing.
        assert!(out.contains("j4"), "{out}");
        assert!(out.contains("15.0s"), "{out}");
        assert!(out.contains("job_output"), "{out}");
        assert!(out.contains("do not start it again"), "{out}");
        assert!(out.contains("did not ask"), "{out}");
        // It is not grounding: there is no answer yet to be grounded in.
        assert!(!r.is_grounded());
    }

    /// The call names the tool and the payload names the handle, so the envelope repeats neither;
    /// and a model that asked for the background is not told it asked.
    #[test]
    fn an_envelope_does_not_restate_the_call_or_the_handle() {
        use letibot_transcript::Backgrounding;
        let asked = ToolResult::new(
            "c1",
            "bash",
            ToolOutcome::Backgrounded {
                handle: "j4".into(),
                ran_for_ms: 0,
                how: Backgrounding::Asked,
                next: "`job_output` with job=\"j4\" reads it".into(),
            },
        )
        .with_payload("started `gate · j4` (pid 1).")
        .render();
        for gone in ["tool:", "job:", "how:", "ran in the foreground"] {
            assert!(!asked.contains(gone), "{gone}: {asked}");
        }
        assert_eq!(asked.matches("j4").count(), 2, "{asked}");
        let promoted = ToolResult::new(
            "c1",
            "bash",
            ToolOutcome::Backgrounded {
                handle: "j4".into(),
                ran_for_ms: 15_000,
                how: Backgrounding::Promoted,
                next: String::new(),
            },
        )
        .render();
        assert_eq!(promoted.matches("how:").count(), 1, "{promoted}");
        // Who moved it and when, on one line: the move is said once.
        assert_eq!(promoted.matches("background").count(), 1, "{promoted}");
        assert!(
            promoted.contains("after 15.0s in the foreground"),
            "{promoted}"
        );
        let failed = ToolResult::new(
            "c1",
            "read",
            ToolOutcome::Failed {
                reason: "no such path".into(),
            },
        )
        .render();
        assert!(!failed.contains("tool:"), "{failed}");
        let abstained =
            ToolResult::new("c1", "grep", ToolOutcome::Abstained { reason: "x".into() }).render();
        assert!(!abstained.contains("tool:"), "{abstained}");
    }

    #[test]
    fn calls_that_are_all_still_running_are_not_a_failure() {
        let children = vec![
            ToolOutcome::Backgrounded {
                handle: "j1".into(),
                ran_for_ms: 15_000,
                how: letibot_transcript::Backgrounding::Promoted,
                next: "job_output".into(),
            },
            ToolOutcome::Backgrounded {
                handle: "j2".into(),
                ran_for_ms: 15_000,
                how: letibot_transcript::Backgrounding::Asked,
                next: "job_output".into(),
            },
        ];
        match propagate(&children) {
            Propagation::Must(ToolOutcome::NotRun { why }) => {
                assert!(why.contains("j1") && why.contains("j2"), "{why}");
                assert!(why.contains("STILL RUNNING"), "{why}");
            }
            other => panic!("still-running must not propagate as a failure: {other:?}"),
        }
    }

    #[test]
    fn a_real_failure_alongside_a_running_job_is_still_a_failure() {
        // The branch above must not swallow a genuine error just because
        // something else happens to be running.
        let children = vec![
            ToolOutcome::Backgrounded {
                handle: "j1".into(),
                ran_for_ms: 1,
                how: letibot_transcript::Backgrounding::Asked,
                next: "job_output".into(),
            },
            ToolOutcome::Failed {
                reason: "no such path".into(),
            },
        ];
        assert!(matches!(
            propagate(&children),
            Propagation::Must(ToolOutcome::Failed { .. })
        ));
    }
}

#[cfg(test)]
mod once_tests {
    use super::*;

    const WHY: &str = "this command's meaning does not exist yet, so nothing can decide \
                       about it. The grammar read 149 bytes and could not resolve `$n`.";

    /// The reason once. A refusal's payload already names the tool, the decider, the
    /// basis and what to do, so putting the same paragraph above it made three copies of
    /// one paragraph in one card.
    #[test]
    fn an_outcome_does_not_restate_a_reason_the_payload_carries() {
        let refusal =
            format!("REFUSED — not failed.\n\nwho decided: boundary:normaliser\nbasis: {WHY}\n");
        let o = ToolOutcome::NotRun { why: WHY.into() };
        assert_eq!(outcome_word_beside(&o, &refusal), "not run — see below");
        // The same rule for a failure whose payload explains itself.
        let f = ToolOutcome::Failed { reason: WHY.into() };
        assert_eq!(outcome_word_beside(&f, &refusal), "failed — see below");
    }

    /// And when the payload does NOT carry it, the reason stays: this line is
    /// then the only place it is said, and dropping it would lose it entirely.
    #[test]
    fn a_reason_the_payload_does_not_carry_is_kept() {
        let o = ToolOutcome::NotRun { why: WHY.into() };
        let said = outcome_word_beside(&o, "some unrelated output\n");
        assert!(said.contains("meaning does not exist yet"), "{said}");
        assert_eq!(said, outcome_word(&o));
    }

    /// A short reason is repeated rather than hunted for: cheap to say twice,
    /// and a short string can appear below by coincidence rather than because it
    /// is the same statement.
    #[test]
    fn a_short_reason_is_left_alone() {
        let o = ToolOutcome::Failed {
            reason: "no such file".into(),
        };
        assert_eq!(
            outcome_word_beside(&o, "no such file\n"),
            "failed — no such file"
        );
    }

    /// Every other outcome is untouched - this is about reasons, and `ok`, `timeout` and
    /// the rest do not carry one.
    #[test]
    fn the_other_outcomes_are_unchanged() {
        for o in [
            ToolOutcome::Ok,
            ToolOutcome::Timeout,
            ToolOutcome::Denied {
                req_id: "r1".into(),
            },
        ] {
            assert_eq!(outcome_word_beside(&o, "anything"), outcome_word(&o));
        }
    }
}
