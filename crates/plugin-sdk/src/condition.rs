//! **`condition` — what a monitor watches** (`docs/plugins-plan.md` §4.4).
//!
//! A monitor today is a Rust `Condition`: `GithubCondition` spawns `gh` at most once per interval,
//! polls the child with `try_wait` so a slow network never stalls the poller, keeps a baseline,
//! and fires when something new happened. This kind is that shape, answered by a plugin: the
//! plugin says what to look at, the host runs it, the plugin is asked again with the result and
//! says whether it fired and what to look at next.
//!
//! # The plugin never runs anything
//!
//! `look` in an answer is a *declaration*, not an act (`docs/plugins-plan.md` §1 principle 1): the
//! host spawns it, under the layer's declared capability, and hands the result back in the next
//! input's `look`. So a hook call is pure, the same input gives the same output, and the shadow
//! gate's replay means something.
//!
//! # `look` is a result on the way in and an ask on the way out
//!
//! Two types, [`LookResult`] and [`LookAsk`], because the two directions mean opposite things and
//! a single struct with both fields would be a document a plugin can read backwards: the input's
//! `look` is `{"exit": 0, "stdout": "…"}` — what the host saw — and the answer's is
//! `{"exec": ["gh", "pr", "view", …]}` or `{"read": "path"}` — what to run next. This is the same
//! collision `docs/plugins-slice1.md` C2 and C3 had over one word, resolved the way that one was:
//! by naming the two things differently in the type while the wire keeps the plan's spelling.
//!
//! # State belongs to the host
//!
//! `state` is opaque JSON the plugin returns and the host stores with the monitor, so a restart, a
//! reload and a newer build of the plugin all keep the baseline a watch needs (§1 principle 5). A
//! `null` on the first call is *nothing has been looked at yet*.

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
    /// The exit status, or 126 for a look the host refused to run (a capability it does not
    /// hold) — the same spelling a refused `read` uses, so a plugin has one thing to check.
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

/// The answer's `look`: what to run next. Two kinds, and both are the host's act.
///
/// The wire is `{"exec": ["prog", "arg"]}` or `{"read": "path"}` — externally tagged, so the key
/// IS the kind and there is no second place to say it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LookAsk {
    /// Run this program with these arguments. The layer must declare `exec:<program>`; an argv,
    /// never a shell, so nothing a plugin writes can be re-split into more commands.
    Exec(Vec<String>),
    /// Read this file, relative to the session's workspace. The layer must declare `read`.
    Read(String),
}

/// **One watch of one pull request**, shared with every other watch on it (§4.4's `github`).
///
/// The host already coalesces: `monitor` refuses a second watch on a pull request another watch
/// holds, and names the one to use instead. A plugin condition that declares this takes part in
/// that rule, so a project that watches pull requests through a plugin and through `github` does
/// not spend two looks on one pull request.
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
