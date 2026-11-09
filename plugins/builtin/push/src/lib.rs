//! The bridge's own push rule, as the built-in `push` layer (X25).
//!
//! ```jsonc
//! // in
//! {"v":1, "event":"addressed", "session":"cc@pkg", "room":"host", "to":"cc@pkg",
//!  "text":"#12 [host] from s-a (session) to you\n> …", "native":{"push":true}}
//! // out
//! {"push":true, "text":"…"}
//! ```
//!
//! It answers what the bridge itself would have: `native` when the input carries one, and otherwise
//! the room's own rule - a line that only passes by is not pushed and everything else is. A project
//! puts a layer above this one to change the rule (R117's *every room message* is `{"push": true}`
//! for `passing` too); the two events no layer may silence (an `ask` and a `failed`) are the host's.

use letibot_guest_abi as abi;

#[unsafe(no_mangle)]
pub extern "C" fn push_channel(ptr: i32, len: i32) -> i32 {
    abi::hook(ptr, len, answer)
}

fn answer(bytes: &[u8]) -> Option<String> {
    let input: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    Some(decide(&input).to_string())
}

/// The answer for one push. The bridge's own decision wins when it is there, so this layer changes
/// nothing on a bridge that asks it; a bare input - `plugin test`, a hand-written call - is read off
/// the event, which is what makes the rule readable on its own.
pub fn decide(input: &serde_json::Value) -> serde_json::Value {
    let native = input
        .get("native")
        .and_then(|n| n.get("push"))
        .and_then(serde_json::Value::as_bool);
    let push = native.unwrap_or_else(|| {
        // `passing` is the room's word for a line addressed to somebody else: it reaches this seat
        // as content and wakes nobody, which is the whole of "only addressed" (R117).
        input.get("event").and_then(serde_json::Value::as_str) != Some("passing")
    });
    let text = input
        .get("text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    serde_json::json!({ "push": push, "text": text })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(event: &str, native: Option<bool>) -> serde_json::Value {
        let mut input = serde_json::json!({
            "v": 1,
            "event": event,
            "session": "cc@package",
            "room": "host",
            "to": "cc@package",
            "text": "#12 [host] from s-a (session) to you\n> the gate is red",
        });
        if let Some(native) = native {
            input["native"] = serde_json::json!({ "push": native });
        }
        input
    }

    /// The layer is today's rule: only an addressed line is pushed, and the words are the bridge's.
    #[test]
    fn the_layer_says_todays_rule() {
        for event in ["operator", "dm", "addressed", "turn_ended", "ask", "failed"] {
            let said = decide(&input(event, None));
            assert_eq!(said["push"], serde_json::json!(true), "{event}");
            assert_eq!(said["text"], input(event, None)["text"]);
        }
        assert_eq!(
            decide(&input("passing", None))["push"],
            serde_json::json!(false),
            "a line that only passes by is not a wake"
        );
    }

    /// The bridge's own decision wins when it is handed over, so installing this layer moves nothing
    /// - including for the two events no layer may silence.
    #[test]
    fn the_bridge_own_decision_is_said_back() {
        assert_eq!(
            decide(&input("passing", Some(true)))["push"],
            serde_json::json!(true),
            "a bridge whose own rule pushes a passing line is not overridden"
        );
        for event in ["ask", "failed", "turn_ended"] {
            assert_eq!(
                decide(&input(event, Some(true)))["push"],
                serde_json::json!(true),
                "{event}"
            );
        }
    }

    /// The answer is one of the two the host parses: an object with the push flag, and never a
    /// silence of the layer's own.
    #[test]
    fn the_answer_is_the_shape_the_host_parses() {
        let said = decide(&input("turn_ended", Some(true)));
        let object = said.as_object().expect("an object");
        assert!(
            object.contains_key("push") && object.contains_key("text"),
            "{said}"
        );
    }
}
