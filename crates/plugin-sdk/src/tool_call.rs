//! `on_tool_call` - the tool call, before the gate: a layer may narrow it, rewrite it, refuse it,
//! or answer it without letting it run at all.
//!
//! The hook runs before the gate, and the rewritten call is the call the gate then adjudicates, so
//! every route through this kind ends at the same boundary. A layer cannot answer *admitted*
//! because there is no field for it: the class, the mode, the adjudicator and the operator's card
//! are decided in Rust after this returns. What a layer can do is make a call smaller.
//! Every layer exports the same name: the top answers, and it may ask the layer beneath.

use serde::Deserialize;
use serde_json::Value;

/// The export name. One name for the kind: the call is the subject, and every layer sees the same
/// subject (`crate`'s header on the two spellings).
pub const HOOK: &str = "on_tool_call";

/// **The session the call is being made in**, so a layer can be narrower than *always*: the mode
/// is the gate's own word for how much this session may do without asking.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Session {
    /// The session's id, as the store names it.
    pub session: String,
    /// The mode in force - `read-only`, `writes-allowed`, `allow-edits`, `allow-all`: the words
    /// the gate and the status line already use.
    pub mode: String,
    /// Where the session's tree is, so a layer can be about one project.
    pub workspace: String,
}

/// The input: the call, the session, and the lower layer's answer when this is the second half of
/// a `{"below": …}` round trip.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolCallIn {
    /// [`V`](crate::V).
    pub v: u32,
    pub call: crate::Call,
    #[serde(default)]
    pub session: Session,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub below: Option<Value>,
}

impl ToolCallIn {
    /// One call, as the host builds it.
    pub fn new(call: crate::Call, session: Session) -> ToolCallIn {
        ToolCallIn {
            v: crate::V,
            call,
            session,
            below: None,
        }
    }

    /// The synthetic input `plugin test` and the shadow gate use when a session has none
    /// recorded: the plan's own example, the third monitor on a pull request that is already
    /// watched.
    pub fn synthetic() -> ToolCallIn {
        ToolCallIn::new(
            crate::Call::new(
                "monitor",
                serde_json::json!({
                    "name": "pr-24-coderabbit",
                    "github": {"url": "https://github.com/o/r/pull/24", "until": "coderabbit"},
                    "repeat": true
                }),
            ),
            Session {
                session: "s-synthetic".to_string(),
                mode: "allow-edits".to_string(),
                workspace: "/work".to_string(),
            },
        )
    }
}

/// What one layer decided about a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict<'a> {
    /// Run it exactly as the model proposed.
    Pass,
    /// Run it with these arguments instead - **through the gate**.
    Rewrite(&'a Value),
    /// Do not run it; the model reads this sentence as the result.
    Refuse(&'a str),
    /// Do not run it; this text is the result.
    Answer(&'a str),
}

/// The answer. Absent fields are absent on the wire, so *this layer said nothing* and *this layer
/// said nothing with these fields* are one thing and the stack's `pass` marker is another.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolCallOut {
    /// Run it as it is. The plainest way to say *not my business*, and the same meaning as
    /// answering `None` through the ABI.
    #[serde(default, skip_serializing_if = "is_false")]
    pub pass: bool,
    /// The arguments to run it with instead.
    /// `null` here is a rewrite, not silence (see [`present`]): a layer narrowing a call to no
    /// arguments is narrowing it, and serde's reading of `null` as absent would run the model's call.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub arguments: Option<Value>,
    /// Do not run it, and tell the model this.
    ///
    /// A layer that means this must write the sentence: `null` is silence and lets the call run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refuse: Option<String>,
    /// Do not run it, and answer the model with this. As with `refuse`, `null` is silence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Any JSON value, present or null, as `Some` - so `{"arguments": null}` and `{}` are two
/// different answers. Absence is `#[serde(default)]`'s `None` and nothing else is.
fn present<'de, D>(d: D) -> Result<Option<Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Value::deserialize(d).map(Some)
}

impl ToolCallOut {
    /// Let it through unchanged.
    pub fn pass() -> ToolCallOut {
        ToolCallOut {
            pass: true,
            ..ToolCallOut::default()
        }
    }

    /// Run it with these arguments instead.
    pub fn rewrite(arguments: Value) -> ToolCallOut {
        ToolCallOut {
            arguments: Some(arguments),
            ..ToolCallOut::default()
        }
    }

    /// Do not run it: the model is owed this reason.
    pub fn refuse(why: impl Into<String>) -> ToolCallOut {
        ToolCallOut {
            refuse: Some(why.into()),
            ..ToolCallOut::default()
        }
    }

    /// Do not run it: this is its result.
    pub fn answer(text: impl Into<String>) -> ToolCallOut {
        ToolCallOut {
            answer: Some(text.into()),
            ..ToolCallOut::default()
        }
    }

    /// What this answer means, in the order the host applies it: refuse, answer, rewrite, pass.
    /// The restrictive order is on purpose: a reader of a call that ran must never have to work
    /// out whether the layer that refused it also wanted different arguments.
    pub fn verdict(&self) -> Verdict<'_> {
        if let Some(why) = &self.refuse {
            return Verdict::Refuse(why);
        }
        if let Some(text) = &self.answer {
            return Verdict::Answer(text);
        }
        if let Some(args) = &self.arguments {
            return Verdict::Rewrite(args);
        }
        Verdict::Pass
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// **Nothing in this contract can admit a call.** The five fields are the whole answer, and
    /// not one of them is a decision the gate asks for: this is the type-level half of §1
    /// principle 3, and it is asserted rather than promised in prose.
    #[test]
    fn there_is_no_field_by_which_a_layer_could_admit_a_call() {
        let full = ToolCallOut {
            pass: true,
            arguments: Some(json!({})),
            refuse: Some("no".into()),
            answer: Some("did it".into()),
        };
        let out = serde_json::to_value(&full).expect("serialises");
        for forbidden in [
            "admit",
            "allow",
            "permission",
            "mode",
            "access",
            "gate",
            "class",
        ] {
            assert!(
                out.get(forbidden).is_none(),
                "`{forbidden}` would be a way round the gate: {out}"
            );
        }
        // And the four the contract does have, so a renamed field breaks here too.
        for field in ["pass", "arguments", "refuse", "answer"] {
            assert!(out.get(field).is_some(), "{field}: {out}");
        }
    }

    /// The order is refuse, answer, rewrite, pass - the restrictive statement wins, and a layer
    /// that says two things has one meaning.
    #[test]
    fn the_strongest_statement_is_the_one_that_speaks() {
        let both = ToolCallOut {
            refuse: Some("already watched".into()),
            arguments: Some(json!({"name": "x"})),
            ..ToolCallOut::default()
        };
        assert_eq!(both.verdict(), Verdict::Refuse("already watched"));
        let answered = ToolCallOut {
            answer: Some("done".into()),
            arguments: Some(json!({"name": "x"})),
            ..ToolCallOut::default()
        };
        assert_eq!(answered.verdict(), Verdict::Answer("done"));
        let rewritten = ToolCallOut::rewrite(json!({"name": "x"}));
        assert_eq!(rewritten.verdict(), Verdict::Rewrite(&json!({"name": "x"})));
        let empty: ToolCallOut = serde_json::from_str("{}").expect("an empty answer");
        assert_eq!(empty.verdict(), Verdict::Pass, "silence lets it run");
        assert_eq!(ToolCallOut::pass().verdict(), Verdict::Pass);
    }

    /// The synthetic input is the shape the host hands over: the call as an object, the session's
    /// three facts, and no `below` on the first call.
    #[test]
    fn the_synthetic_input_is_the_hosts_own_shape() {
        let input = serde_json::to_value(ToolCallIn::synthetic()).expect("serialises");
        for field in ["v", "call", "session"] {
            assert!(input.get(field).is_some(), "no `{field}`: {input}");
        }
        assert_eq!(input["v"], json!(crate::V));
        assert_eq!(input["call"]["name"], json!("monitor"));
        assert!(input.get("below").is_none());
        for field in ["session", "mode", "workspace"] {
            assert!(
                input["session"].get(field).is_some(),
                "no `{field}`: {input}"
            );
        }
    }

    /// A guest may answer a rewrite with `null` arguments - which is a call with no arguments,
    /// not a licence to run the model's. The distinction matters: a layer narrowing a call to
    /// nothing is narrowing it.
    #[test]
    fn an_explicit_null_rewrite_is_not_silence() {
        let out: ToolCallOut = serde_json::from_str(r#"{"arguments":null}"#).expect("a null");
        assert_eq!(out.verdict(), Verdict::Rewrite(&Value::Null));
    }
}
