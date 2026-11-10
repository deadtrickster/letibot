//! The harness's own turn-end decision, as the built-in `steer` layer.
//!
//! ```jsonc
//! // in
//! {"v":1, "reply_tail":"…", "open_rows":["…"], "native":"…"|null, "intent":"…"|null,
//!  "deliverable":{"uncommitted":["…"]}|null, "claim":{"line":"…","resident":0,"window":0}|null,
//!  "stall_rounds":0, "after":"turn", "budget":{"continues_left":1}}
//! // out
//! {"continue":"…"} | {"stop":true}
//! ```
//!
//! It answers what the harness decided: the sentence it would have appended, or a stop when it had
//! nothing to say. A project puts a layer above this one to change the decision - to keep a turn
//! going after a compaction, or to stop one the harness would have nudged - and the two rules a
//! layer cannot cross (the budget, and an interrupted turn) are the host's, not this file's.

use letibot_guest_abi as abi;

#[unsafe(no_mangle)]
pub extern "C" fn steer_turn(ptr: i32, len: i32) -> i32 {
    abi::hook(ptr, len, answer)
}

fn answer(bytes: &[u8]) -> Option<String> {
    let input: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    Some(decide(&input).to_string())
}

/// The answer for one turn's end. `native` is the harness's own sentence, so echoing it is a layer
/// that changed nothing; a layer above overrides this one and never the harness's budget.
pub fn decide(input: &serde_json::Value) -> serde_json::Value {
    match input.get("native").and_then(|n| n.as_str()) {
        Some(sentence) if !sentence.trim().is_empty() => {
            serde_json::json!({ "continue": sentence })
        }
        _ => serde_json::json!({ "stop": true }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(native: Option<&str>) -> serde_json::Value {
        let mut input = serde_json::json!({
            "v": 1,
            "reply_tail": "I will add the last test now.",
            "open_rows": ["write the last test"],
            "intent": "the turn said it would write the last test and made no tool call",
            "deliverable": {"uncommitted": ["crates/plugin-sdk/src/steer.rs"]},
            "claim": serde_json::Value::Null,
            "stall_rounds": 0,
            "after": "turn",
            "budget": {"continues_left": 1},
        });
        if let Some(native) = native {
            input["native"] = serde_json::json!(native);
        }
        input
    }

    /// The layer is the harness's decision, said back: what it decided to say, and a stop when it
    /// had nothing - which is exactly the two answers the native turn-end block gave.
    #[test]
    fn the_layer_answers_what_the_harness_decided() {
        let said = decide(&input(Some("run the suite you promised")));
        assert_eq!(
            said["continue"],
            serde_json::json!("run the suite you promised")
        );
        assert_eq!(decide(&input(None))["stop"], serde_json::json!(true));
        // A blank sentence is not a continuation: nothing would be said and a round spent.
        assert_eq!(decide(&input(Some("   ")))["stop"], serde_json::json!(true));
    }

    /// The answer is one of the two the host parses, and nothing else - a layer that says both, or
    /// neither, is refused at the host and cannot be read as a decision.
    #[test]
    fn the_answer_is_one_of_the_hosts_two_shapes() {
        for native in [Some("go on"), None] {
            let said = decide(&input(native));
            let object = said.as_object().expect("an object");
            assert_eq!(object.len(), 1, "{said}");
            let key = object.keys().next().expect("a key").as_str();
            assert!(key == "continue" || key == "stop", "{said}");
        }
    }
}
