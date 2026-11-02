//! The `condition` hook: what a monitor watches, answered by a plugin.
//!
//! The plugin says what to look at, the host runs it, and the plugin is asked again with the result
//! and says whether it fired and what to look at next.
//!
//! The plugin never runs anything. `look` in an answer is a declaration, the host spawns it under
//! the layer's declared capability, and the result comes back in the next input's `look`, so a hook
//! call is pure and the shadow gate's replay means something.
//!
//! `look` is a result on the way in ([`LookResult`]) and an ask on the way out ([`LookAsk`]). Two
//! types, because the directions mean opposite things and one struct with both fields would be a
//! document a plugin can read backwards.
//!
//! `state` is opaque JSON the plugin returns and the host stores with the monitor, so a restart, a
//! reload or a newer build of the plugin keeps the baseline a watch needs. A `null` on the first
//! call is *nothing has been looked at yet*.

use serde_json::Value;

/// The prefix of a condition's export name; the name after it is the hook a `monitor` call asks
/// for (`condition_coderabbit` is `{"plugin": "coderabbit"}`).
pub const PREFIX: &str = "condition_";

/// The export name for one condition.
pub fn hook(name: &str) -> String {
    format!("{PREFIX}{name}")
}

/// The shortest interval a condition may ask for, matching the monitors' own floor: a look per
/// second would make a watch into a busy loop on somebody else's API.
pub const EVERY_MIN_S: u64 = 5;

/// The input's `look`: what the host saw when it ran the last one.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LookResult {
    /// The exit status, or 126 for a look the host refused to run because the layer does not hold
    /// the capability: the same spelling a refused `read` uses, so a plugin has one thing to check.
    pub exit: i32,
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
}

impl LookResult {
    /// What a look that could not run at all looks like from inside a plugin.
    pub fn refused(why: impl Into<String>) -> LookResult {
        LookResult {
            exit: 126,
            stdout: String::new(),
            stderr: why.into(),
        }
    }

    pub fn ok(stdout: impl Into<String>) -> LookResult {
        LookResult {
            exit: 0,
            stdout: stdout.into(),
            stderr: String::new(),
        }
    }
}

/// The answer's `look`: what to run next, externally tagged so the key IS the kind and there is no
/// second place to say it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LookAsk {
    /// Run this program with these arguments. The layer must declare `exec:<program>`; an argv,
    /// never a shell, so nothing a plugin writes can be re-split into more commands.
    Exec(Vec<String>),
    /// Read this file, relative to the session's workspace. The layer must declare `read`.
    Read(String),
}

/// One watch of one pull request, shared with every other watch on it: the host refuses a second
/// watch on a pull request another holds, and a condition declaring this takes part in that rule.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Github {
    /// `owner/repo#N`.
    pub pr: String,
    /// What this watch is waiting for: `checks`, `review`, `closed`, `comment`, `coderabbit`,
    /// `merged`. Words, so the refusal names what the held watch already covers.
    #[serde(default)]
    pub watches: Vec<String>,
}

/// The input: the monitor's arguments, the state it kept, and the last look's result.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConditionIn {
    /// [`V`](crate::V).
    pub v: u32,
    /// The `args` of the `monitor` call that armed this watch — the plugin's own vocabulary.
    #[serde(default)]
    pub args: Value,
    /// What this hook returned last time, or `null` before the first call.
    #[serde(default)]
    pub state: Option<Value>,
    /// The last look's result, or `null` on the call that arms the watch.
    #[serde(default)]
    pub look: Option<LookResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub below: Option<Value>,
}

impl ConditionIn {
    /// The first call: the watch is being armed, nothing has been looked at.
    pub fn arm(args: Value) -> ConditionIn {
        ConditionIn {
            v: crate::V,
            args,
            state: None,
            look: None,
            below: None,
        }
    }

    /// A later call, with what the look saw.
    pub fn looked(args: Value, state: Option<Value>, look: LookResult) -> ConditionIn {
        ConditionIn {
            v: crate::V,
            args,
            state,
            look: Some(look),
            below: None,
        }
    }

    /// The synthetic input `plugin test` and the shadow gate use when a session has none
    /// recorded: a watch being armed for one pull request.
    pub fn synthetic() -> ConditionIn {
        ConditionIn::arm(serde_json::json!({"pr": "o/r#24"}))
    }
}

/// The answer: what to look at next, how often, and whether the watch fired.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConditionOut {
    /// What to run next. Absent means the host keeps the last one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub look: Option<LookAsk>,
    /// How often to look, in seconds. Absent keeps the last interval; the host's floor is
    /// [`EVERY_MIN_S`], and a smaller number is raised to it rather than refused — an interval is
    /// a preference, and a watch that refuses to arm over one is worse than a slow one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every_s: Option<u64>,
    /// The longest this watch may live, so a watch on a review that takes days is not killed by
    /// the ordinary hour. The host caps it at a week, exactly as it caps a `github` watch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_ttl_s: Option<u64>,
    /// The firing's own sentence: why it fired, in the plugin's words. Absent means *not yet*.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fired: Option<String>,
    /// Handed back on the next call. The host keeps it; the plugin does not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<Value>,
    /// The pull request this watch is on, when it is on one — for the coalescing rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github: Option<Github>,
}

impl ConditionOut {
    /// Look at this next, and nothing has fired.
    pub fn ask(look: LookAsk, every_s: u64) -> ConditionOut {
        ConditionOut {
            look: Some(look),
            every_s: Some(every_s),
            ..ConditionOut::default()
        }
    }

    /// It fired, and this is why.
    pub fn fired(why: impl Into<String>) -> ConditionOut {
        ConditionOut {
            fired: Some(why.into()),
            ..ConditionOut::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The plan's own example is this type, both directions.
    #[test]
    fn the_plans_example_parses_both_ways() {
        let answer = json!({
            "look": {"exec": ["gh", "pr", "view", "24", "--json", "state,reviews"]},
            "every_s": 90,
            "max_ttl_s": 604800,
            "fired": null,
            "state": {"seen": 3},
            "github": {"pr": "o/r#24", "watches": ["coderabbit"]}
        });
        let out: ConditionOut = serde_json::from_value(answer).expect("the example parses");
        assert_eq!(
            out.look,
            Some(LookAsk::Exec(
                ["gh", "pr", "view", "24", "--json", "state,reviews"]
                    .map(str::to_string)
                    .to_vec()
            ))
        );
        assert_eq!(out.every_s, Some(90));
        assert!(out.fired.is_none(), "a null `fired` is not a firing");
        assert_eq!(out.github.as_ref().map(|g| g.pr.as_str()), Some("o/r#24"));
        // And back out: a guest writes this, so it has to be the same document.
        let round: Value = serde_json::to_value(&out).expect("serialises");
        assert_eq!(round["look"]["exec"][0], json!("gh"));
    }

    /// The two `look`s are two types: a result on the way in, an ask on the way out. A host that
    /// mixed them up would run `{"exit": 0}` as a program.
    #[test]
    fn an_input_look_and_an_answer_look_are_not_the_same_shape() {
        let input = ConditionIn::looked(json!({}), None, LookResult::ok("{}"));
        let as_json = serde_json::to_value(&input).expect("serialises");
        assert_eq!(as_json["look"]["exit"], json!(0));
        assert!(as_json["look"].get("exec").is_none());
        let out = ConditionOut::ask(LookAsk::Read(".letibot/x.json".into()), 30);
        let as_json = serde_json::to_value(&out).expect("serialises");
        assert_eq!(as_json["look"]["read"], json!(".letibot/x.json"));
        assert!(as_json["look"].get("exit").is_none());
        // A refused look is a RESULT the plugin reads, with the same 126 a refused read uses.
        assert_eq!(LookResult::refused("no capability").exit, 126);
    }

    /// Arming is the call with no look: a plugin that cannot tell *arming* from *a look that
    /// produced nothing* would fire on the first poll of every watch.
    #[test]
    fn arming_is_the_call_with_no_look_at_all() {
        let arming = serde_json::to_value(ConditionIn::synthetic()).expect("serialises");
        assert!(arming["look"].is_null(), "{arming}");
        assert!(arming["state"].is_null(), "{arming}");
        assert_eq!(arming["v"], json!(crate::V));
        for field in ["v", "args", "state", "look"] {
            assert!(arming.get(field).is_some(), "no `{field}`: {arming}");
        }
    }
}
