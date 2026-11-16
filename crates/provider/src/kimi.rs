//! What Kimi's coding endpoint needs beyond a plain OpenAI request. Each rule is switched on by a
//! [`crate::presets::Compat`] key, never the preset's name, so a providers.toml door behaves the same.

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

/// How this client names itself, whatever the endpoint. Never another agent's name: Moonshot
/// documents rewriting the `User-Agent` as a violation, and the coding endpoint admits the
/// clients it recognises by this header.
pub const USER_AGENT: &str = concat!("letibot/", env!("CARGO_PKG_VERSION"));

/// Kimi refuses an assistant tool-call message with no `reasoning_content`. Its own client sends
/// the empty string, so every such message that lacks one gets it here; imported transcripts and
/// silent turns are the shape that earns the 400.
pub fn fill_absent_thoughts(body: &mut Value) {
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    for message in messages.iter_mut() {
        let Some(object) = message.as_object_mut() else {
            continue;
        };
        let calls = object
            .get("tool_calls")
            .and_then(Value::as_array)
            .is_some_and(|calls| !calls.is_empty());
        if object.get("role").and_then(Value::as_str) == Some("assistant")
            && calls
            && !object.contains_key("reasoning_content")
        {
            object.insert(
                "reasoning_content".to_string(),
                Value::String(String::new()),
            );
        }
    }
}

/// The sentence a 403 earns on a door with a client gate (`Compat::client_gate`), or `None` for
/// any other door or status. A plan gate names the model or the context cap in its own words; the client gate, the
/// one failure this build can be blamed for, names nothing at all.
pub fn explain_a_403(compat: &crate::presets::Compat, status: u16) -> Option<String> {
    if !compat.client_gate || status != 403 {
        return None;
    }
    Some(format!(
        "a 403 here is either the plan's own gate (a model or a context your plan does not \
         cover) or the endpoint's client gate, which admits the coding agents it recognises by \
         `User-Agent` — this request said `{USER_AGENT}`. Altering that header to pass for \
         another agent is a documented violation, so the honest fix is asking Moonshot to admit \
         this client"
    ))
}

/// A value for `prompt_cache_key`, which is what makes Kimi's implicit cache hit across a
/// conversation's turns. Its own client sends the session id; no session id reaches this crate,
/// and one provider instance is built per session open, so the instance's own name is the key.
pub fn cache_key() -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    format!(
        "letibot-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The empty thought is put only where the server demands it, and a recorded one is never
    /// overwritten: a turn with thinking must still reach the model as it was thought.
    #[test]
    fn an_absent_thought_is_filled_and_a_present_one_is_left_alone() {
        let mut body = json!({"messages": [
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": null,
             "tool_calls": [{"id": "c1", "type": "function",
                             "function": {"name": "read", "arguments": "{}"}}]},
            {"role": "tool", "tool_call_id": "c1", "content": "ok"},
            {"role": "assistant", "content": "done"},
            {"role": "assistant", "content": null, "reasoning_content": "I looked",
             "tool_calls": [{"id": "c2", "type": "function",
                             "function": {"name": "read", "arguments": "{}"}}]}
        ]});
        fill_absent_thoughts(&mut body);
        let m = body["messages"].as_array().unwrap();
        assert_eq!(
            m[1]["reasoning_content"], "",
            "a tool-call message with no thought"
        );
        assert!(
            m[3].get("reasoning_content").is_none(),
            "a plain assistant answer is not a tool call and gains nothing"
        );
        assert_eq!(m[4]["reasoning_content"], "I looked");
        assert!(m[0].get("reasoning_content").is_none());
        assert!(m[2].get("reasoning_content").is_none());
    }

    /// Web Search's UA constant is `letibot-webfetch/<version>`; this one is the product's own
    /// name, and it must never be a vendor's: the endpoint reads it as the client's identity.
    #[test]
    fn this_client_names_itself_and_no_one_else() {
        assert!(USER_AGENT.starts_with("letibot/"), "{USER_AGENT}");
        assert!(!USER_AGENT.contains(' '));
        for impostor in ["kimi", "claude", "codex", "opencode", "cursor"] {
            assert!(
                !USER_AGENT.to_ascii_lowercase().contains(impostor),
                "{USER_AGENT} passes for {impostor}"
            );
        }
    }

    #[test]
    fn only_k3_1_refuses_a_disabled_thinking_switch() {
        let kimi = crate::presets::KIMI_CODE.compat;
        assert!(kimi.may_switch_thinking_off("k3"));
        assert!(kimi.may_switch_thinking_off("k3-256k"));
        assert!(kimi.may_switch_thinking_off("kimi-for-coding"));
        assert!(!kimi.may_switch_thinking_off("k3.1-pro"));
        assert!(
            !crate::presets::KIMI_CODE_GLOBAL
                .compat
                .may_switch_thinking_off("k3.1-pro-highspeed")
        );
        // Another door's model name is not Kimi's to gate.
        assert!(
            crate::presets::GLM
                .compat
                .may_switch_thinking_off("glm-3.1")
        );
    }

    /// Two instances are two keys: a provider is one session, and two sessions sharing a cache
    /// key would be billed as one conversation's prefix.
    #[test]
    fn every_provider_instance_gets_its_own_cache_key() {
        let (a, b) = (cache_key(), cache_key());
        assert_ne!(a, b);
        assert!(a.starts_with("letibot-"), "{a}");
        assert!(!a.contains(' '));
    }

    #[test]
    fn a_403_is_explained_and_nothing_else_is() {
        let kimi = crate::presets::KIMI_CODE.compat;
        let said = explain_a_403(&kimi, 403).expect("a 403 is explained");
        assert!(
            said.contains("User-Agent") && said.contains(USER_AGENT),
            "{said}"
        );
        assert!(explain_a_403(&kimi, 429).is_none());
        assert!(explain_a_403(&kimi, 401).is_none());
        assert!(explain_a_403(&crate::presets::DEEPSEEK.compat, 403).is_none());
    }
}
