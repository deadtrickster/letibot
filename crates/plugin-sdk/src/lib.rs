//! The letibot plugin SDK: the typed contract of every hook kind, in one place.
//!
//! A hook is a wasm export that takes one JSON document and answers another. The shapes live here
//! rather than in a markdown table because a renamed field then breaks a compilation rather than a
//! sentence: the daemon builds every input from these types, parses every answer into them, and
//! prints its worked examples by serialising them, so the document a model copies and the bytes
//! the host accepts cannot drift apart.
//!
//! The crate is compiled into the daemon and into a guest plugin, so its dependencies are serde and
//! nothing else. It does not carry rano's ABI glue: a guest's `Cargo.toml` names this crate for the
//! types and rano's for the memory and the result block.
//!
//! | kind | export | runs | answers |
//! |---|---|---|---|
//! | [`notice`] | `notice_<the notice's kind>` | when the harness is about to tell the model something | the sentence, reworded or dropped |
//! | [`tool_result`] | `on_tool_result` | on a successful tool result, before the model reads it | a rewritten result, a suffix, monitors to arm |
//! | [`tool_call`] | `on_tool_call` | on a proposed call, before the gate | run it, run it with these arguments, refuse it, or answer it |
//! | [`condition`] | `condition_<name>` | when a monitor polls | what to look at next, and whether it fired |
//!
//! A kind is declared here only once the host runs it, because a contract nothing calls is a
//! document that lies.
//!
//! `pane_` and `condition_` are per subject: each pane is drawn, each condition is armed, and the
//! name says which one. `notice_<kind>`, `on_tool_result` and `on_tool_call` are one hook per kind:
//! every layer that wants to answer those exports the same name, and they resolve as a stack where
//! the top answers and may ask the ones below with `{"below": …}`.
//!
//! The markers `{"pass":true}`, `{"mask":true}` and `{"below": <input>}` belong to the stack,
//! which reads them before it hands an answer on. That is why every input type here has a `below`.
//!
//! Nothing here can widen a permission. `on_tool_call` runs before the gate, the rewritten call is
//! the call the gate adjudicates, and there is no field by which a layer could say *allow*.

/// The contract version every input carries. A host that changes a contract keeps answering the old
/// version or refuses the plugin by name; an added field is not a version.
pub const V: u32 = 1;

pub mod condition;
pub mod notice;
pub mod pane;
pub mod tool_call;
pub mod tool_result;

/// One tool call, as a hook sees it. `arguments` is an object, not the model's raw text, so a layer
/// rewrites fields rather than parsing JSON; a call whose arguments are not JSON never reaches a hook.
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

    /// Every hook kind's name is one the host's prefix list admits, duplicated here because this crate
    /// is compiled into a guest that must not depend on the daemon.
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
