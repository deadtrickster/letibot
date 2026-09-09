//! §5.7 — no output cap, and `finish_reason: length` is **acted on**.
//!
//! # The failure this is written against
//!
//! opencode capped output at 32,000 tokens (`provider/transform.ts:18`), parsed
//! `finish_reason`, and threw it away (`session/processor.ts`, `case "finish":
//! return`). A subagent spent its whole budget thinking and returned empty content
//! with a full `reasoning_content`. That was recorded as a completed turn. Nine
//! such rows sat in a session database unnoticed.
//!
//! The lesson is not "check a flag". It is that **success was representable** for a
//! turn that produced nothing. So the policy here does not return a bool that a
//! caller may ignore: [`LengthVerdict::HardFail`] is the only value the two empty
//! cases can produce, and the engine turns it into an `Err`. A turn that hit the
//! output limit with nothing to show cannot be constructed as a success — see
//! `crate::engine::TurnOk`, which has no variant for it.
//!
//! Grok's `LengthPolicy` (`conversation.rs:513-575`) names the two fail cases
//! `NoVisibleContent` and `ReasoningOnly`, and confirms at `:569-573` that *an
//! empty Length response fails under every policy value*. Taken as written.

use letibot_transcript::ToolCall;

/// Why an empty `length` turn failed. Grok's `empty_reason()`, ported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmptyReason {
    /// No visible content and no tool calls.
    NoVisibleContent,
    /// Reasoning only — the budget went into thinking and nothing came out.
    /// This is the exact shape of the nine unnoticed rows.
    ReasoningOnly,
}

impl EmptyReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            EmptyReason::NoVisibleContent => "no_visible_content",
            EmptyReason::ReasoningOnly => "reasoning_only",
        }
    }
}

/// What §5.7 says to do with this turn.
#[derive(Debug, Clone, PartialEq)]
pub enum LengthVerdict {
    /// The turn did not hit the limit. Nothing to decide.
    NotLength,
    /// Hard fail. Not recoverable by retrying the same request, and not
    /// recordable as a completed turn.
    HardFail(EmptyReason),
    /// Tool calls, all of whose arguments parse as complete JSON. Keep them and
    /// continue.
    ToolCallsIntact,
    /// At least one argument is truncated. **The whole batch fails** — none
    /// executed. Executing the intact half of a batch the model wrote as a unit is
    /// how a half-applied edit happens.
    ToolCallsTruncated { truncated: Vec<String> },
    /// Real text, cut short. Keep it, mark it truncated, continue.
    TruncatedText,
}

impl LengthVerdict {
    /// Whether this turn may be recorded as a completed one.
    ///
    /// Deliberately not `is_ok()`: the question a caller asks is not "did it go
    /// well" but "am I allowed to write this down as a success", and only one of
    /// those has a defensible answer here.
    pub fn may_record_as_success(&self) -> bool {
        !matches!(
            self,
            LengthVerdict::HardFail(_) | LengthVerdict::ToolCallsTruncated { .. }
        )
    }
}

/// What the turn actually produced, as the policy needs to see it.
#[derive(Debug, Clone, Copy)]
pub struct TurnShape<'a> {
    pub visible_text: &'a str,
    pub reasoning_text: &'a str,
    pub tool_calls: &'a [ToolCall],
}

/// The policy of §5.7, in the order the plan states it.
pub fn classify(hit_length: bool, shape: TurnShape<'_>) -> LengthVerdict {
    if !hit_length {
        return LengthVerdict::NotLength;
    }
    let has_text = !shape.visible_text.trim().is_empty();
    let has_calls = !shape.tool_calls.is_empty();

    if !has_text && !has_calls {
        // Both empty cases fail; which one is reported is diagnostic, and the
        // distinction is worth keeping because `ReasoningOnly` names a *budget*
        // problem while `NoVisibleContent` names a generation that never started.
        return LengthVerdict::HardFail(if shape.reasoning_text.trim().is_empty() {
            EmptyReason::NoVisibleContent
        } else {
            EmptyReason::ReasoningOnly
        });
    }

    if has_calls {
        let truncated: Vec<String> = shape
            .tool_calls
            .iter()
            .filter(|c| !arguments_are_complete(&c.arguments))
            .map(|c| c.id.clone())
            .collect();
        return if truncated.is_empty() {
            LengthVerdict::ToolCallsIntact
        } else {
            LengthVerdict::ToolCallsTruncated { truncated }
        };
    }

    LengthVerdict::TruncatedText
}

/// Whether a tool call's arguments are complete JSON.
///
/// Empty arguments count as complete: a zero-argument tool renders `{}` or `""`
/// and treating that as truncation would fail every no-argument call that happened
/// to land on the limit.
fn arguments_are_complete(arguments: &str) -> bool {
    let t = arguments.trim();
    if t.is_empty() {
        return true;
    }
    serde_json::from_str::<serde_json::Value>(t).is_ok()
}

/// The message the model is told, in the **tool result**, not in a log.
///
/// pi's wording, ported verbatim because it is already right: it says what
/// happened, why, and what to do, and it does so where the model will read it.
pub fn batch_failed_notice(call_name: &str) -> String {
    format!(
        "Tool call {call_name} was not executed: the response hit the output token \
         limit, so its arguments may be truncated. Re-issue the tool call with \
         complete arguments."
    )
}

/// Bounds the salvage loop.
///
/// Grok has a test named `length_salvage_streak_proceeds_to_the_cap_then_exhausts`
/// for a reason: a salvage that always retries is its own failure mode, and a model
/// that truncates once usually truncates again.
#[derive(Debug, Clone, Copy)]
pub struct SalvageBudget {
    pub cap: u32,
    streak: u32,
}

impl SalvageBudget {
    pub fn new(cap: u32) -> Self {
        SalvageBudget { cap, streak: 0 }
    }

    /// Record a `length` finish that was salvaged. `false` means the cap is spent
    /// and the caller must fail the turn rather than retry.
    pub fn salvaged(&mut self) -> bool {
        self.streak += 1;
        self.streak <= self.cap
    }

    /// Any terminal value other than `length` clears the streak.
    pub fn cleared(&mut self) {
        self.streak = 0;
    }

    pub fn streak(&self) -> u32 {
        self.streak
    }
}

impl Default for SalvageBudget {
    fn default() -> Self {
        SalvageBudget::new(3)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(id: &str, args: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: "read".into(),
            arguments: args.into(),
        }
    }

    fn shape<'a>(text: &'a str, reasoning: &'a str, calls: &'a [ToolCall]) -> TurnShape<'a> {
        TurnShape {
            visible_text: text,
            reasoning_text: reasoning,
            tool_calls: calls,
        }
    }

    /// The nine rows, as a test.
    #[test]
    fn a_turn_that_spent_its_budget_thinking_cannot_be_recorded_as_success() {
        let v = classify(true, shape("", "a very long deliberation", &[]));
        assert_eq!(v, LengthVerdict::HardFail(EmptyReason::ReasoningOnly));
        assert!(!v.may_record_as_success());
    }

    #[test]
    fn an_empty_length_turn_fails_whatever_else_is_true_of_it() {
        assert_eq!(
            classify(true, shape("   \n ", "", &[])),
            LengthVerdict::HardFail(EmptyReason::NoVisibleContent)
        );
        assert_eq!(
            classify(true, shape("", "  ", &[])),
            LengthVerdict::HardFail(EmptyReason::NoVisibleContent)
        );
    }

    #[test]
    fn the_same_empty_turn_is_fine_when_it_did_not_hit_the_limit() {
        // Not the policy's business: an empty assistant turn that ended on EOS is a
        // §5.4 case (content: "", never null), not a §5.7 one.
        assert_eq!(
            classify(false, shape("", "thought", &[])),
            LengthVerdict::NotLength
        );
    }

    #[test]
    fn intact_tool_calls_survive_the_limit() {
        let calls = [call("c1", r#"{"path":"a"}"#), call("c2", "{}")];
        assert_eq!(
            classify(true, shape("", "", &calls)),
            LengthVerdict::ToolCallsIntact
        );
    }

    #[test]
    fn one_truncated_argument_fails_the_whole_batch_not_just_itself() {
        let calls = [
            call("c1", r#"{"path":"a"}"#),
            call("c2", r#"{"path":"very-lo"#),
        ];
        let v = classify(true, shape("", "", &calls));
        assert_eq!(
            v,
            LengthVerdict::ToolCallsTruncated {
                truncated: vec!["c2".into()]
            }
        );
        assert!(
            !v.may_record_as_success(),
            "a batch that was not executed is not a completed turn"
        );
    }

    #[test]
    fn real_text_is_kept_and_marked() {
        let v = classify(true, shape("here is half an ans", "thought", &[]));
        assert_eq!(v, LengthVerdict::TruncatedText);
        assert!(v.may_record_as_success());
    }

    #[test]
    fn the_salvage_loop_is_bounded() {
        let mut b = SalvageBudget::new(2);
        assert!(b.salvaged());
        assert!(b.salvaged());
        assert!(!b.salvaged(), "the cap is spent");
        b.cleared();
        assert!(b.salvaged());
    }

    #[test]
    fn the_notice_tells_the_model_what_to_do_and_names_the_call() {
        let n = batch_failed_notice("read_file");
        assert!(n.contains("read_file"));
        assert!(n.contains("Re-issue"));
        assert!(n.contains("output token limit"));
    }
}
