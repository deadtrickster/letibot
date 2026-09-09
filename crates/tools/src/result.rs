//! What a tool call produced, how it reaches the model, and why abstention has a
//! shape of its own.
//!
//! §8.1 clause 3 and §8.2. The outcome vocabulary is [`ToolOutcome`] and lives in
//! `letibot-transcript`, because the transcript is what a later turn re-renders and
//! the class must survive that round trip. What lives here is the part §8.2 calls
//! *structural*:
//!
//! > The model sees `NO_RESULT` as a distinct token sequence, not as prose it can
//! > quote around.
//!
//! So an abstention is not a differently-worded success. It is a different
//! envelope, it is machine-recognisable coming back ([`Envelope::classify`]), and
//! [`propagate`] makes it impossible for a caller to report `Ok` over a set of
//! calls that all abstained.

use letibot_transcript::ToolOutcome;

use crate::args::Repair;
use crate::spill::SpillRef;

/// The result of one tool call, before it becomes a `TranscriptItem::ToolResult`.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolResult {
    pub call_id: String,
    pub name: String,
    pub outcome: ToolOutcome,
    /// The body the model sees, already spilled if it needed spilling, but **not**
    /// yet wrapped in its envelope. [`ToolResult::render`] does that.
    pub payload: String,
    /// Clause 2: what was repaired on the way in. Rendered into the result, so the
    /// model learns the spelling it should have used.
    pub repairs: Vec<Repair>,
    /// Clause 1: what the tool did about a miss, in its own words. Separate from
    /// `payload` so a head can show it without parsing the body.
    pub notes: Vec<String>,
    /// Clause 5: set when the full output went to the spill store.
    pub spill: Option<SpillRef>,
    /// Both sides of a file this call changed, for a head to draw a diff from.
    ///
    /// **Not part of [`ToolResult::render`]**, and that is the point: this never
    /// becomes prompt bytes. The model gets the confirmation in `payload`; the
    /// head gets the file. See [`crate::edit::FileEdit`].
    pub edit: Option<crate::edit::FileEdit>,
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
    /// Three envelopes, chosen by outcome class and never by wording:
    ///
    /// - `Ok` — the body, with a `[note]` block above it when there is one.
    /// - `Abstained` — the `NO_RESULT` envelope. **Nothing inside it is an
    ///   answer**, and the envelope says so in the same bytes every time.
    /// - everything else — the `TOOL_ERROR` envelope, which still carries the
    ///   corrective body, because clause 1 does not stop applying when a call
    ///   fails.
    pub fn render(&self) -> String {
        let mut head = String::new();
        for r in &self.repairs {
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
            other => Envelope::error(&self.call_id).wrap(&format!(
                "tool: {}\noutcome: {}\n{head}{}",
                self.name,
                outcome_word(other),
                self.payload
            )),
        }
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
    }
}

/// A delimiter pair the outcome class picks, not the wording.
///
/// The tag carries a short marker derived from the call id, so two calls in one
/// turn cannot have their envelopes confused with each other, and so an envelope
/// quoted back out of one result is recognisably not the envelope of another.
///
/// What this **does not** claim: that a model cannot type these characters. It
/// cannot be stopped from doing that, and §8.2 says so — the harness's job is that
/// *the system* never reports an abstention as grounded, and that is
/// [`propagate`]. The envelope's job is to be unmistakable in the prompt and
/// machine-recognisable coming back.
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

    pub fn open(&self) -> String {
        format!("<<<{} {}>>>", self.kind, self.mark)
    }

    pub fn close(&self) -> String {
        format!("<<<END_{} {}>>>", self.kind, self.mark)
    }

    pub fn wrap(&self, body: &str) -> String {
        format!("{}\n{}\n{}", self.open(), body.trim_end(), self.close())
    }

    /// Which envelope a rendered payload is in, if any.
    ///
    /// The journal and the tests use this: a payload that says `NO_RESULT` at the
    /// top must round-trip to the class it was rendered from, or the structural
    /// claim in §8.2 is decoration.
    pub fn classify(rendered: &str) -> Option<&'static str> {
        let first = rendered.lines().next()?;
        for kind in ["NO_RESULT", "TOOL_ERROR"] {
            if first.starts_with(&format!("<<<{kind} ")) {
                return Some(kind);
            }
        }
        None
    }
}

fn mark_of(call_id: &str) -> String {
    // FNV-1a, the same choice `letibot-turn` made for `args_digest` and for the
    // same reason: this is a correlation aid, and the ledger's SHA-256 chain is
    // what carries identity.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in call_id.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{:08x}", (h & 0xffff_ffff) as u32)
}

/// What a caller is **allowed** to report, given what its own tool calls returned.
#[derive(Debug, Clone, PartialEq)]
pub enum Propagation {
    /// At least one call produced a real result. The caller's own outcome is its
    /// own business.
    Unconstrained,
    /// The caller may not report `Ok`. This is the outcome it must carry instead.
    Must(ToolOutcome),
}

/// §8.2's second mechanism: **abstention propagates upward.**
///
/// > A subagent whose tool calls all abstained cannot return `Ok`. Its result
/// > carries `Abstained` and the parent's `ToolResult` is `Abstained`. The harness
/// > never converts "no result" into "a result".
///
/// Empty input is `Unconstrained` on purpose: a caller that made no tool calls at
/// all is not a caller whose tool calls abstained, and conflating the two would
/// make every non-tool-using turn an abstention. The rule is about *converting* a
/// no-result into a result, and there is nothing to convert.
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
    // No result and not purely abstention: something went wrong, and reporting it
    // as an abstention would be the mirror of the bug this function exists to stop.
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
        // Clause 1 does not switch off because the outcome is not Ok.
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
}
