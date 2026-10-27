//! **`on_tool_result` — what happens after a call** (`docs/plugins-plan.md` §4.2).
//!
//! The bug it was written for: the model opens a pull request with `gh pr create`, and nothing
//! watches it — so the harness grew `pr_watch`, a hard-coded intercept that recognised one
//! command and said one sentence. This is that intercept, as a hook: a layer sees the call and its
//! result and may append to the result, rewrite it, and arm monitors. When the GitHub family
//! becomes plugins (§7 step 3) the built-in `pr_watch` goes and one of these does the work.
//!
//! # Successful results only, and that is a decision
//!
//! A call that failed did nothing to react to; its result is the failure's own words, which the
//! model has to read exactly. So the hook runs on `Ok` and `Backgrounded` (a job that started did
//! happen) and never on a refusal or a failure — `outcome` says which, so a layer can read it and
//! behave, but a layer is not given a hook where its own reason could bury a real error.
//!
//! # Arming goes through the operator's door
//!
//! `arm` is not a second way to start a monitor: the host turns each entry into the same `monitor`
//! call the model would type, runs it the way `pr_watch` runs its own, and so the coalescing, the
//! already-watched refusal, the TTL caps and the name collision rules all apply unchanged. What a
//! layer gets from this field is *the harness noticed something* — never *a monitor with no
//! rules*.

use serde_json::Value;

/// The export name. One for the kind: every layer sees every result, and they stack.
pub const HOOK: &str = "on_tool_result";

/// The result the hook is told about, in the words the transcript uses.
pub const OK: &str = "ok";
/// The call was backgrounded: the job started, and the result is its handle.
pub const BACKGROUNDED: &str = "backgrounded";

/// The input: the call, how it ended, and what the model would read.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolResultIn {
    /// [`V`](crate::V).
    pub v: u32,
    pub call: crate::Call,
    /// [`OK`] or [`BACKGROUNDED`] — the two outcomes a hook is asked about.
    pub outcome: String,
    /// What the call produced: the text the model is about to read.
    pub payload: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub below: Option<Value>,
}

impl ToolResultIn {
    /// One result, as the host builds it.
    pub fn new(call: crate::Call, outcome: &str, payload: impl Into<String>) -> ToolResultIn {
        ToolResultIn {
            v: crate::V,
            call,
            outcome: outcome.to_string(),
            payload: payload.into(),
            below: None,
        }
    }

    /// The synthetic input `plugin test` and the shadow gate use when a session has none
    /// recorded: a pull request that was just opened, which is §4.2's own example.
    pub fn synthetic() -> ToolResultIn {
        ToolResultIn::new(
            crate::Call::new(
                "bash",
                serde_json::json!({"command": "gh pr create --fill"}),
            ),
            OK,
            "https://github.com/o/r/pull/24\n",
        )
    }
}

/// A monitor a layer asks the host to arm.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Arm {
    /// Which kind of watch — the `plugin` field a `monitor` call takes, naming the layer's own
    /// `condition_<name>` hook.
    pub plugin: String,
    #[serde(default)]
    pub args: Value,
}

impl Arm {
    pub fn new(plugin: impl Into<String>, args: Value) -> Arm {
        Arm {
            plugin: plugin.into(),
            args,
        }
    }
}

/// The answer: the result, as the model should read it, and anything to watch.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolResultOut {
    /// The result, replaced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<String>,
    /// A suffix added to the result — the shape `pr_watch`'s sentence has: the harness saying what
    /// it just did, in the call's own answer so the model reads it where it looks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub append: Option<String>,
    /// Monitors to arm, through the same door the model's own `monitor` calls use.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arm: Vec<Arm>,
}

/// What one layer decided about a result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Said<'a> {
    /// Nothing: the result is the call's own.
    Unchanged,
    /// The text the model reads — borrowed when it is the answer's own replacement, owned when
    /// the answer's suffix had to be joined onto it.
    Text(std::borrow::Cow<'a, str>),
    /// Nothing about the text; this much appended to it.
    Appended(&'a str),
}

impl ToolResultOut {
    /// Add this to the end of the result.
    pub fn appended(text: impl Into<String>) -> ToolResultOut {
        ToolResultOut {
            append: Some(text.into()),
            ..ToolResultOut::default()
        }
    }

    /// Replace the result.
    pub fn replaced(payload: impl Into<String>) -> ToolResultOut {
        ToolResultOut {
            payload: Some(payload.into()),
            ..ToolResultOut::default()
        }
    }

    /// **The result this answer means** — a replacement, or a suffix, or neither.
    ///
    /// Both may appear: *this is what it was* and *here is what I noticed about it* are two
    /// different sentences and a layer may want both. `Unchanged` when the answer names neither,
    /// which is the common case and must cost nothing.
    pub fn said(&self) -> Said<'_> {
        use std::borrow::Cow;
        match (&self.payload, &self.append) {
            (None, None) => Said::Unchanged,
            (Some(replaced), Some(append)) => Said::Text(Cow::Owned(format!("{replaced}{append}"))),
            (Some(replaced), None) => Said::Text(Cow::Borrowed(replaced)),
            (None, Some(append)) => Said::Appended(append),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The plan's own example parses into the type and back out again — the round trip the host
    /// and a guest each do one half of.
    #[test]
    fn the_plans_example_is_this_type() {
        let out: ToolResultOut = serde_json::from_str(
            r#"{"append":"\n[harness] watching o/r#24 for checks, reviews…",
                "arm":[{"plugin":"github-pr","args":{"pr":"o/r#24"}}]}"#,
        )
        .expect("the example parses");
        assert_eq!(
            out.said(),
            Said::Appended("\n[harness] watching o/r#24 for checks, reviews…")
        );
        assert_eq!(out.arm.len(), 1);
        assert_eq!(out.arm[0].plugin, "github-pr");
        assert_eq!(out.arm[0].args, json!({"pr": "o/r#24"}));
    }

    /// A replacement and a suffix compose, and neither alone is the other.
    #[test]
    fn a_result_can_be_replaced_then_added_to() {
        let both = ToolResultOut {
            payload: Some("opened 24".into()),
            append: Some(" (watching it)".into()),
            ..ToolResultOut::default()
        };
        assert_eq!(both.said(), Said::Text("opened 24 (watching it)".into()));
        assert_eq!(ToolResultOut::replaced("x").said(), Said::Text("x".into()));
        assert_eq!(ToolResultOut::appended("+").said(), Said::Appended("+"));
        let empty: ToolResultOut = serde_json::from_str("{}").expect("an empty answer");
        assert_eq!(empty.said(), Said::Unchanged);
    }

    /// The synthetic input is the shape the host hands over: the call as an object, the outcome
    /// as a word, and the payload the model would have read.
    #[test]
    fn the_synthetic_input_is_the_hosts_own_shape() {
        let input = serde_json::to_value(ToolResultIn::synthetic()).expect("serialises");
        for field in ["v", "call", "outcome", "payload"] {
            assert!(input.get(field).is_some(), "no `{field}`: {input}");
        }
        assert_eq!(input["outcome"], json!(OK));
        assert_eq!(input["v"], json!(crate::V));
        assert!(input.get("below").is_none());
    }

    /// An empty arm list is absent on the wire rather than `[]`, so a layer that arms nothing says
    /// nothing about monitors.
    #[test]
    fn no_arms_is_no_field() {
        let out = serde_json::to_value(ToolResultOut::appended("x")).expect("serialises");
        assert!(out.get("arm").is_none(), "{out}");
    }
}
