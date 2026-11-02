//! The letibot plugin SDK: the typed contract of every hook kind, in one place.
//!
//! A hook is a wasm export that takes one JSON document and answers another. The shapes live here
//! as types, not as a table, so a renamed field breaks a compilation and not a sentence.
//!
//! `pane_` and `condition_` are named per subject; `notice_<kind>`, `on_tool_result` and
//! `on_tool_call` are one hook per kind, resolved as a stack - the top answers, and the ones below
//! answer when it asks. Nothing here can widen a permission: `on_tool_call` runs before the gate.

/// The contract version every input carries, and the only one this crate describes.
///
/// An added field is not a version: a host keeps answering the old version or refuses by name.
pub const V: u32 = 1;

pub mod condition;
pub mod notice;
pub mod pane;
pub mod tool_call;
pub mod tool_result;

/// One tool call, as a hook sees it: the name and the arguments the model proposed.
/// `arguments` is an object because a hook rewrites fields, not text; a call whose arguments are
/// not JSON never reaches a hook, so no layer is asked to guess.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Call {
    pub name: String,
    #[serde(default)]
    pub arguments: serde_json::Value,
}

impl Call {
    pub fn new(name: impl Into<String>, arguments: serde_json::Value) -> Call {
        Call {
            name: name.into(),
            arguments,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every hook kind's name must be one `letibot_plugins`'s `HOOK_PREFIXES` admits, or rano
    /// never looks for it. Duplicated rather than depended on: this crate is compiled into a
    /// guest, and a guest must not depend on the daemon.
    const PREFIXES: &[&str] = &[
        "on_",
        "provider_",
        "pane_",
        "condition_",
        "notice_",
        "digest_",
        "link_",
        "command_",
        "tool_",
    ];

    fn admitted(hook: &str) -> bool {
        PREFIXES
            .iter()
            .any(|p| hook.strip_prefix(p).is_some_and(|rest| !rest.is_empty()))
    }

    #[test]
    fn every_kinds_hook_name_is_one_the_host_finds() {
        assert!(admitted(tool_call::HOOK), "{}", tool_call::HOOK);
        assert!(admitted(tool_result::HOOK), "{}", tool_result::HOOK);
        assert!(admitted(&condition::hook("coderabbit")));
        for kind in notice::KINDS {
            let hook = notice::hook(kind);
            assert!(admitted(&hook), "{hook}");
        }
        // And a name with nothing after the prefix names no hook: `is_hook_kind`'s own rule.
        assert!(!admitted("notice_"));
    }
}
