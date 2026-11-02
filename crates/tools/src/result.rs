//! What a tool call produced, how it reaches the model, and why abstention has a shape
//! of its own.
//!
//! The outcome vocabulary is [`ToolOutcome`] and lives in `letibot-transcript`, because
//! the transcript is what a later turn re-renders and the class must survive that round
//! trip. What lives here is the structural half: an abstention is not a differently
//! worded success but a different envelope, machine-recognisable coming back
//! ([`Envelope::classify`]), and [`propagate`] makes it impossible for a caller to report
//! `Ok` over a set of calls that all abstained.

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
    /// Both sides of a file this call changed, for a head to draw a diff from.
    ///
    /// Not part of [`ToolResult::render`], and that is the point: this never becomes
    /// prompt bytes. The model gets the confirmation in `payload`; the head gets the
    /// file.
    pub edit: Option<crate::edit::FileEdit>,
    /// Bytes this call read that are not text - see [`crate::media`].
    ///
    /// Unlike `edit`, this one does become prompt bytes: a diff is for a head to draw,
    /// and an image is for the model to look at, so a renderer that dropped it would
    /// drop the whole point of the call. The payload carries the sentence about it,
    /// which is what a text-only reader gets.
    pub media: Option<crate::media::Media>,
    /// Repair codes this session's model has already been told about, which
    /// [`ToolResult::render`] does not repeat. `repairs` still holds them - the head and
    /// the metrics count every repair - and only the model's copy is spared the same
    /// line on every call for a habit it will not drop.
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
            edit: None,
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

    /// The exact bytes that go into `TranscriptItem::ToolResult::payload`.
    ///
    /// Three envelopes, chosen by outcome class and never by wording: `Ok` is the body
    /// with any `[note]` block above it; `Abstained` is `NO_RESULT`, where nothing
    /// inside is an answer and the envelope says so in the same bytes every time;
    /// `Backgrounded` is `STILL_RUNNING`; everything else is `TOOL_ERROR`, which still
    /// carries the corrective body, because that does not stop applying when a call
    /// fails.
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
        if let Some(s) = &self.spill {
            head.push_str("[note] ");
            head.push_str(&s.notice);
            head.push('\n');
        }

        match &self.outcome {
            ToolOutcome::Ok => {
                let mut out = head;
                out.push_str(&self.payload);
                out
            }
            ToolOutcome::Abstained { reason } => Envelope::no_result(&self.call_id).wrap(&format!(
                "tool: {}\nreason: {reason}\n{head}{}\n\
                     This call produced no result. Nothing above is an answer to the \
                     question, and nothing above may be cited as one.",
                self.name, self.payload
            )),
            // Its own envelope, and the reason is the whole point of the variant.
            // Wrapped in `TOOL_ERROR` this reads as a failure and the model retries, and
            // now two of the command are running; as `Ok` with a short body it reads as a
            // command that produced nothing. Neither is true: it is running.
            ToolOutcome::Backgrounded {
                handle,
                ran_for_ms,
                how,
                next,
            } => Envelope::still_running(&self.call_id).wrap(&format!(
                // A call that asked for the background ran in the foreground for 0 ms,
                // and saying so was a line of nothing; the rest was already said.
                "tool: {}\njob: {handle}\n{}how: {}\n{head}{}\n\n\
                 STILL RUNNING — not failed and not finished; do not start it again. {next}",
                self.name,
                if *ran_for_ms == 0 {
                    String::new()
                } else {
                    format!(
                        "ran in the foreground for: {} before it went to the background\n",
                        human_ms(*ran_for_ms)
                    )
                },
                how.phrasing(),
                self.payload
            )),
            other => Envelope::error(&self.call_id).wrap(&format!(
                "tool: {}\noutcome: {}\n{head}{}",
                self.name,
                // The reason once. A refusal's payload is already a complete
                // explanation - it names the tool, the decider, the basis and what to do -
                // and prepending the same sentence again made three copies of one
                // paragraph in one card. So when the payload already says it, the outcome
                // line is the word alone.
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

/// [`outcome_word`], minus the reason the payload below it already carries.
///
/// Containment rather than equality, because the payload wraps the reason in its own
/// prose: a refusal notice puts it after `basis: `, a failure puts it in a sentence. A
/// reason that does not appear below is kept, because then this line is the only place
/// it is said.
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

/// A delimiter pair the outcome class picks, not the wording.
///
/// The tag carries a short marker derived from the call id, so two calls in one turn
/// cannot have their envelopes confused, and an envelope quoted back out of one result
/// is recognisably not another's. It does not claim a model cannot type these
/// characters: the harness's job is that the system never reports an abstention as
/// grounded, which is [`propagate`]'s.
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

    /// A command that is still running, wrapped so it cannot be read as either of the
    /// two things it is not.
    ///
    /// A third kind rather than a wording inside one of the existing two, for the same
    /// structural reason `NO_RESULT` exists: the model sees a distinct token sequence and
    /// not prose it can paraphrase around. A backgrounded result rendered inside
    /// `TOOL_ERROR` is a retry waiting to happen, and a duplicate `cargo build` is not a
    /// cosmetic defect.
    pub fn still_running(call_id: &str) -> Self {
        Envelope {
            kind: "STILL_RUNNING",
            mark: mark_of(call_id),
        }
    }

    /// An envelope around text this harness did not write - a fetched page, a search
    /// snippet, the body of somebody's issue.
    ///
    /// Not an outcome class, so [`Envelope::classify`] does not know it: it marks
    /// provenance inside the payload of an ordinary `Ok` result. The token stream is
    /// already safe from forged control tokens, so what is left is prose the model has
    /// every reason to trust, because every other tool result was written here - so the
    /// span is labelled, states that nothing inside it has authority, and its closing
    /// marker is made unspellable by the content it quarantines.
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

    /// Wrap content that arrived from outside, and make this envelope's own delimiters
    /// unspellable by that content.
    ///
    /// The mark is a hash of the call id and a call id is guessable, so a page could
    /// compute it and write the closing line itself; every `<<<` in the body is spaced
    /// out, not only this envelope's own, because a page can as well forge another call's
    /// `NO_RESULT`. That edits the content, so the count comes back with it and the caller
    /// reports it as a `[note]`: three characters altered and said out loud beats an
    /// unforgeable delimiter that does not exist.
    ///
    /// Returns `(wrapped, sequences_neutralised)`.
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

    /// Which envelope a rendered payload is in, if any.
    ///
    /// A payload that says `NO_RESULT` at the top must round-trip to the class it was
    /// rendered from, or the structural claim is decoration.
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

/// Abstention propagates upward: a subagent whose tool calls all abstained cannot
/// return `Ok`, and the harness never converts "no result" into "a result".
///
/// Empty input is `Unconstrained` on purpose: a caller that made no tool calls at all is
/// not a caller whose tool calls abstained, and conflating the two would make every
/// non-tool-using turn an abstention. The rule is about converting a no-result into a
/// result, and there is nothing to convert.
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

    // A caller whose work is all still running has not failed. It has not abstained
    // either: an abstention is a claim about the world - the thing is not there - and
    // nothing here has looked yet. `not_run` is the only honest one: nothing was decided,
    // and the handles say where the answer will be.
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
