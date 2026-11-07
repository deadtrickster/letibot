//! The `digest_<name>` hook: a call's output, before the spill policy budgets it
//! (`docs/plugins-plan.md` §4.5, X26). The digest is what is budgeted, so a layer that shortens
//! a transcript is shortening what the model has to hold rather than what the store keeps.
//!
//! A digest may shorten or reshape what it was handed. It may not grow it and may not erase it:
//! both are ways to spend the model's context by accident, and the hook's whole purpose is the
//! opposite. Either is refused and the call's own output stands.

/// The export-name prefix a digest answers under: `digest_<name>`.
pub const PREFIX: &str = "digest_";

/// The hook a layer named `name` answers.
pub fn hook(name: &str) -> String {
    format!("digest_{name}")
}

/// What a digest is handed: the call it belongs to, and the bytes the model would read.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Input {
    /// [`V`](crate::V).
    pub v: u32,
    /// The call as the model made it - which tool, with which arguments. A digest that needs to
    /// know what it is reading has it here, and nothing else about the session.
    pub call: crate::Call,
    /// The output, whole and before the spill.
    pub payload: String,
}

impl Input {
    /// One call's output, as the host builds it.
    pub fn new(call: crate::Call, payload: impl Into<String>) -> Input {
        Input {
            v: crate::V,
            call,
            payload: payload.into(),
        }
    }

    /// The input `plugin test` and the shadow gate use when a session has none recorded.
    pub fn synthetic() -> Input {
        Input::new(
            crate::Call::new(
                "bash",
                serde_json::json!({"command": "cargo test --workspace --no-run"}),
            ),
            "   Compiling letibot-tokencore v0.1.0\n   Compiling letibot-tools v0.1.0\n    Finished \
             `test` profile\n"
                .to_string(),
        )
    }
}

/// The answer: a shorter payload, or nothing at all - which is *leave it alone*.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Out {
    /// What the model reads instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<String>,
}

impl Out {
    /// This instead of what the call produced.
    pub fn digested(payload: impl Into<String>) -> Out {
        Out {
            payload: Some(payload.into()),
        }
    }

    /// What this answer means for `input`: the text to hand on, or `None` for unchanged. The
    /// refusals are the two ways a digest can cost more than it saves.
    pub fn checked<'a>(&'a self, input: &str) -> Result<Option<&'a str>, String> {
        let Some(text) = self.payload.as_deref() else {
            return Ok(None);
        };
        if text.len() > input.len() {
            return Err(format!(
                "it answered {} bytes for {}-byte output, and a digest is what saves the model's \
                 context: it may shorten, and not hand back more than it was given",
                text.len(),
                input.len()
            ));
        }
        if text.trim().is_empty() && !input.trim().is_empty() {
            return Err(
                "it answered nothing, and a digest may shorten the output and not hide it: the \
                 model would be reading a call that produced no text at all"
                    .to_string(),
            );
        }
        Ok(Some(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The plan's own shape: `{"call":{…},"payload":"…"} → {"payload":"…shorter…"}`.
    #[test]
    fn the_plans_example_is_this_type() {
        let input: Input = serde_json::from_str(
            r#"{"v":1,"call":{"name":"read","arguments":{"path":"big.txt"}},"payload":"aaaa"}"#,
        )
        .expect("the example parses");
        assert_eq!(input.call.name, "read");
        assert_eq!(input.payload, "aaaa");
        let out: Out = serde_json::from_str(r#"{"payload":"a"}"#).expect("an answer");
        assert_eq!(out.checked("aaaa").expect("shorter"), Some("a"));
    }

    /// Unchanged is `{}`, and it is the answer that costs a digest nothing.
    #[test]
    fn an_empty_answer_leaves_the_output_alone() {
        let out: Out = serde_json::from_str("{}").expect("an empty answer");
        assert_eq!(out.checked("anything").expect("unchanged"), None);
        let wire = serde_json::to_value(Out::default()).expect("serialises");
        assert!(wire.get("payload").is_none(), "absent, not null: {wire}");
    }

    /// Growing and erasing are refused with a sentence a layer's author can act on.
    #[test]
    fn a_digest_may_not_grow_or_erase_the_output() {
        let grew = Out::digested("aaaaa").checked("aaaa").expect_err("grew");
        assert!(grew.contains("may shorten"), "{grew}");
        let erased = Out::digested("  ").checked("aaaa").expect_err("erased");
        assert!(erased.contains("not hide it"), "{erased}");
        // An output that was already empty is not erased by an empty answer.
        assert_eq!(Out::digested("").checked("").expect("nothing"), Some(""));
    }

    /// The synthetic input is the host's own shape, and a hook name is one the host admits.
    #[test]
    fn the_synthetic_input_is_the_hosts_own_shape() {
        let input = serde_json::to_value(Input::synthetic()).expect("serialises");
        for field in ["v", "call", "payload"] {
            assert!(input.get(field).is_some(), "no `{field}`: {input}");
        }
        assert_eq!(input["v"], json!(crate::V));
        assert_eq!(hook("compilers"), "digest_compilers");
    }
}
