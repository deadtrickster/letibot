//! **The letibot plugin SDK: the typed contract of every hook kind**, in one place.
//!
//! A hook is a wasm export that takes one JSON document and answers another
//! (`docs/plugins-plan.md` §4). The shapes live here rather than in a markdown table for the
//! reason `crates/sessionlog/src/pane.rs` gives for the pane's: **a renamed field breaks a
//! compilation, not a sentence**. The daemon builds every input from these types, parses every
//! answer into them, and `plugin_hooks` prints its worked examples by serialising them — so the
//! document a model copies and the bytes the host accepts cannot drift apart.
//!
//! # One crate, two sides of the same wire
//!
//! This is compiled into the daemon *and* into a guest plugin, so its dependency list is serde
//! and nothing else. It deliberately does **not** carry rano's ABI glue: that is `rano-plugin`'s,
//! a path dependency to a rano checkout, and a guest's `Cargo.toml` names both — this crate for
//! the types, rano's for the memory and the result block.
//!
//! # What is here so far
//!
//! | kind | export | runs | answers |
//! |---|---|---|---|
//! | [`notice`] | `notice_<the notice's kind>` | when the harness is about to tell the model something | the sentence, reworded — or dropped |
//! | [`tool_result`] | `on_tool_result` | on a successful tool result, before the model reads it | a rewritten result, a suffix, monitors to arm |
//! | [`tool_call`] | `on_tool_call` | on a proposed call, **before the gate** | run it, run it with these arguments, refuse it, or answer it |
//! | [`condition`] | `condition_<name>` | when a monitor polls | what to look at next, and whether it fired |
//!
//! The other kinds of `docs/plugins-plan.md` §4 arrive one package at a time, each with the host
//! wiring that gives it a place in a turn; a kind is not declared here before the host runs it,
//! because a contract nothing calls is a document that lies.
//!
//! # Two spellings of a hook name, and why the difference is not an accident
//!
//! `pane_` and `condition_` are **per subject**: each pane is drawn, each condition is armed, and
//! the name says which one. `notice_<kind>`, `on_tool_result` and `on_tool_call` are **one hook per
//! kind**: every layer that wants to answer notices of one kind, or to intercept a call, exports
//! the same name, and they are resolved as a stack — the top answers, the ones below answer when
//! it asks (`{"below": …}`), one of them may mask the hook, and a layer that traps falls through
//! to the one beneath (`docs/plugins-plan.md` §1 principle 4, §2.1).
//!
//! # The layer protocol is the stack's, not this crate's
//!
//! `{"pass":true}`, `{"mask":true}` and `{"below": <input>}` are answered *around* these types:
//! the stack reads the three markers before it hands an answer on, and a hook asked again after a
//! `below` sees the lower layer's answer under `below` — which is why every input type here has
//! one.
//!
//! # Tighten only
//!
//! Nothing here can widen a permission. `on_tool_call` is the one that could have, and it runs
//! *before* the gate: the rewritten call is the call the gate adjudicates, the tool's name is not
//! a field of the answer, and there is no marker by which a layer could say *allow*. The allow side
//! of adjudication, sudo, VMs and the merge queue's landing stay Rust (§1 principle 3).

/// The contract version every input carries, and the only one this crate describes.
///
/// A host that changes a contract keeps answering the old version or refuses the plugin by name
/// (§1 principle 6); an added field is not a version.
pub const V: u32 = 1;

pub mod condition;
pub mod notice;
pub mod pane;
pub mod tool_call;
pub mod tool_result;

/// **One tool call, as a hook sees it**: the name and the arguments the model proposed.
///
/// `arguments` is an object, not the model's raw text: a hook that rewrites a call rewrites
/// fields, and a text blob would make every layer a JSON parser. A call whose arguments are not
/// JSON never reaches a hook at all — `salvage` owns that case inside the runtime, and a layer
/// asked about a shape no tool accepts could only guess.
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

    /// Every hook kind's name is one the host's own prefix list admits (`letibot_plugins`'s
    /// `HOOK_PREFIXES`), which is the check that keeps a kind from being spelled in a way rano
    /// never looks for. Duplicated here as a list rather than depended on, because this crate is
    /// compiled into a guest that must not depend on the daemon.
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
