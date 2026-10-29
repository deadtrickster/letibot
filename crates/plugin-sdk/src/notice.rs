//! **`notice` — what the harness is about to say to the model**, and how a layer may say it
//! better (`docs/plugins-plan.md` §4.3).
//!
//! Every sentence the harness injects — a monitor firing, a job settling, a reminder, a reseat
//! card, the merge queue's verdict on a branch — is composed from structured facts and, before it
//! becomes a transcript row, offered to this hook with those facts beside it. A layer can fix a
//! misleading wording or add what the model was not told (*the pull request is in conflict*);
//! that is the whole of the kind, and it is the cheapest of the four patch points because it
//! changes no decision, only what a decision reads like.
//!
//! # The name after `notice_` is the notice's KIND
//!
//! `notice_monitor` answers monitor firings and nothing else; `notice_job` answers settlements.
//! Resolving per kind rather than per plugin is what makes a fix small: a layer that patches the
//! wording of one firing does not become the answerer for every notice in the session, and the
//! stack still decides which layer's `notice_monitor` is in force.
//!
//! # A notice the operator must see cannot be dropped
//!
//! [`MUST_SEE`] is the host's list, and it is not the plugin's: a job settlement, a due todo, a
//! reseat, a queue verdict and the workspace's own state are facts the model is owed, so their
//! hooks may reword and may not delete. The two a project legitimately never wants —
//! [`MONITOR`](KINDS) firings, which are the plan's own example, and the harness's nudges — are
//! droppable. A layer that answers `{"drop": true}` for one of the others is refused, and the
//! refusal is reported so the model learns why its layer did nothing.
//!
//! # Facts are for reading, not for trusting
//!
//! `facts` is whatever the composing site knows: a monitor's name and why it fired, a job's
//! handle and its fate. A layer that rewords a notice can build a sentence out of them instead of
//! parsing English. Nothing a layer answers from `facts` is trusted for a decision — a decision
//! reads the same facts directly, in Rust.

use serde_json::{Value, json};

/// The name a notice hook is exported under: `notice_<kind>`.
pub const PREFIX: &str = "notice_";

/// A monitor firing or expiry — the plan's own example of a sentence worth patching.
pub const MONITOR: &str = "monitor";
/// A background job settling.
pub const JOB: &str = "job";
/// A todo row whose condition became met.
pub const TODO: &str = "todo";
/// The harness's own reminder to a model that is going in circles.
pub const NUDGE: &str = "nudge";
/// A session reseated, restarted, or moved to a new model.
pub const RESEAT: &str = "reseat";
/// The merge queue's words about a branch: a verdict, a handover, a target's emergency.
pub const MERGE_QUEUE: &str = "merge_queue";
/// A plugin layer's own failure — the sentence that tells the model to fix its fix.
pub const PLUGIN: &str = "plugin";
/// The session's workspace, or another fact about the ground it stands on.
pub const WORKSPACE: &str = "workspace";
/// **A call the gate admitted without asking anybody** (D16) — the sentence that makes a mode
/// which stopped asking visible in the transcript rather than only in the audit rows. A kind of
/// its own because a layer that rewords one is rewording a disclosure, and the merge queue's
/// sentences are not the place to put it.
pub const GATE: &str = "gate";
/// A turn that produced no answer being salvaged (§5.7).
pub const SALVAGE: &str = "salvage";
/// **The host's room** — a tagged queue somebody else posted to, claimed, finished or lost a
/// claimant on, and the questions a raised item puts to the arbiter. A kind of its own because it
/// is news about *other sessions on this box* rather than about this session's own work, and a
/// layer that rewords one is rewording a disclosure about a neighbour.
pub const ROOM: &str = "room";

/// Every kind the host composes. A hook name outside this list is one nothing ever asks for, and
/// `plugin_hooks` says so rather than leaving a layer that answers silence.
pub const KINDS: &[&str] = &[
    MONITOR,
    JOB,
    TODO,
    NUDGE,
    RESEAT,
    MERGE_QUEUE,
    PLUGIN,
    WORKSPACE,
    GATE,
    SALVAGE,
    ROOM,
];

/// **The kinds whose notice may not be dropped**, only reworded — the host's set, not the
/// plugin's (`docs/plugins-plan.md` §4.3).
pub const MUST_SEE: &[&str] = &[
    JOB,
    TODO,
    RESEAT,
    MERGE_QUEUE,
    PLUGIN,
    WORKSPACE,
    GATE,
    SALVAGE,
    ROOM,
];

/// The export name for one notice kind.
pub fn hook(kind: &str) -> String {
    format!("{PREFIX}{kind}")
}

/// Whether the kind is one the host composes at all.
pub fn known(kind: &str) -> bool {
    KINDS.contains(&kind)
}

/// Whether this kind's notice may be dropped. A kind the host does not compose is treated as
/// must-see, because an unknown name cannot be the one a model is owed.
pub fn droppable(kind: &str) -> bool {
    known(kind) && !MUST_SEE.contains(&kind)
}

/// **The input**: one notice, with the facts it was composed from.
///
/// Every field is present on the wire — `facts` is `{}` rather than absent when a site has none —
/// because a guest deserialises this into a struct and a missing key is a plugin that cannot tell
/// *nothing to say about it* from *the host forgot*.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Notice {
    /// [`V`](crate::V).
    pub v: u32,
    /// The notice's kind — [`KINDS`] — and the same word the hook's own name ends in.
    pub kind: String,
    /// The sentence as the harness composed it, which is what the model reads if this hook
    /// answers nothing.
    pub text: String,
    /// What the sentence was built from. Shape is the site's: nobody but that site reads it.
    #[serde(default)]
    pub facts: Value,
    /// Whether this kind may be dropped at all — [`droppable`]'s answer, carried so that a layer
    /// can see the rule it is under instead of discovering it from a refusal.
    pub must_see: bool,
    /// The lower layer's answer, when this call is the second half of a `{"below": …}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub below: Option<Value>,
}

impl Notice {
    /// One notice, as the host builds it.
    pub fn new(kind: &str, text: impl Into<String>, facts: Value) -> Notice {
        Notice {
            v: crate::V,
            kind: kind.to_string(),
            text: text.into(),
            facts,
            must_see: !droppable(kind),
            below: None,
        }
    }

    /// The synthetic input `plugin test` and the shadow gate use when a session has none
    /// recorded: the plan's own example, a monitor firing.
    pub fn synthetic() -> Notice {
        Notice::new(
            MONITOR,
            "[monitor] `pr-24` fired: its checks finished.",
            json!({"monitor": "pr-24", "why": "its checks finished"}),
        )
    }
}

/// **The answer**: the sentence, reworded — or nothing at all.
///
/// Absent fields are absent on the wire (never `null`), so *this layer said nothing about the
/// text* and *this layer said the text is null* cannot be confused. `{"pass": true}` and
/// `{"mask": true}` are the stack's and never appear here.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NoticeOut {
    /// The sentence to record instead of the harness's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Ask for the notice not to be recorded at all. Honoured only where [`droppable`] says so.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drop: Option<bool>,
}

/// What a layer decided about one notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Said<'a> {
    /// The harness's own sentence stands.
    Unchanged,
    /// Say this instead.
    Text(&'a str),
    /// Do not say it — only for a kind that may be dropped.
    Drop,
}

impl NoticeOut {
    /// The sentence, reworded.
    pub fn reworded(text: impl Into<String>) -> NoticeOut {
        NoticeOut {
            text: Some(text.into()),
            drop: None,
        }
    }

    /// The notice, dropped — a request the host honours only for a droppable kind.
    pub fn dropped() -> NoticeOut {
        NoticeOut {
            text: None,
            drop: Some(true),
        }
    }

    /// **What this answer means**, in the one order the host applies it: a drop, then a rewording,
    /// then the harness's own sentence. A drop is the stronger statement, so an answer that says
    /// both drops.
    pub fn said(&self) -> Said<'_> {
        if self.drop == Some(true) {
            return Said::Drop;
        }
        match &self.text {
            Some(text) => Said::Text(text),
            None => Said::Unchanged,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three markers the stack owns are absent from this contract, so a layer cannot spell
    /// one of them by accident into an answer that also says something.
    #[test]
    fn the_layer_markers_are_not_part_of_a_notices_answer() {
        let out = serde_json::to_value(NoticeOut::reworded("x")).expect("serialises");
        for marker in ["pass", "mask", "below"] {
            assert!(out.get(marker).is_none(), "{marker}: {out}");
        }
        let input = serde_json::to_value(Notice::synthetic()).expect("serialises");
        assert!(
            input.get("below").is_none(),
            "no below on the first call: {input}"
        );
    }

    /// A guest that leaves everything out said nothing — which is `Unchanged`, not an empty
    /// sentence.
    #[test]
    fn an_empty_answer_leaves_the_sentence_alone() {
        let out: NoticeOut = serde_json::from_str("{}").expect("an empty answer");
        assert_eq!(out.said(), Said::Unchanged);
        assert_eq!(
            serde_json::from_str::<NoticeOut>(r#"{"drop":true}"#)
                .expect("a drop")
                .said(),
            Said::Drop
        );
        // A drop outranks a rewording: the stronger statement is the one that speaks.
        assert_eq!(
            serde_json::from_str::<NoticeOut>(r#"{"drop":true,"text":"x"}"#)
                .expect("both")
                .said(),
            Said::Drop
        );
    }

    /// The kinds a project may switch off are exactly the two the host has no one owed: a
    /// firing, and a nudge. Everything else is a fact the model is owed.
    #[test]
    fn only_a_firing_and_a_nudge_may_be_dropped() {
        for kind in KINDS {
            assert_eq!(
                droppable(kind),
                !MUST_SEE.contains(kind),
                "{kind} disagrees with the must-see list"
            );
        }
        assert!(droppable(MONITOR));
        assert!(droppable(NUDGE));
        assert!(!droppable(MERGE_QUEUE));
        // An unknown kind cannot be the one a model is owed, so it is kept.
        assert!(!droppable("a-kind-nobody-composes"));
        assert!(!known("a-kind-nobody-composes"));
    }

    /// The synthetic input is the shape the host hands over: every field present, and the kind's
    /// own rule carried with it.
    #[test]
    fn the_synthetic_input_is_the_hosts_own_shape() {
        let input = serde_json::to_value(Notice::synthetic()).expect("serialises");
        for field in ["v", "kind", "text", "facts", "must_see"] {
            assert!(input.get(field).is_some(), "no `{field}`: {input}");
        }
        assert_eq!(input["v"], json!(crate::V));
        assert_eq!(input["must_see"], json!(false), "a firing may be dropped");
    }
}
