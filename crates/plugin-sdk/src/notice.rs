//! `notice` - what the harness is about to say to the model, and how a layer may say it better.
//! Every injected sentence is offered to this hook before it becomes a row; it changes wording and
//! never a decision. The name after `notice_` is the kind, and a [`MUST_SEE`] kind may not be deleted.

use serde_json::Value;

/// The name a notice hook is exported under: `notice_<kind>`.
pub const PREFIX: &str = "notice_";

/// A monitor firing or expiry - the example of a sentence worth patching.
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
/// A plugin layer's own failure - the sentence that tells the model to fix its fix.
pub const PLUGIN: &str = "plugin";
/// The session's workspace, or another fact about the ground it stands on.
pub const WORKSPACE: &str = "workspace";
/// A call the gate admitted without asking anybody - the sentence that makes a mode which stopped
/// asking visible in the transcript rather than only in the audit rows. A kind of its own because
/// a layer that rewords one is rewording a disclosure.
pub const GATE: &str = "gate";
/// A turn that produced no answer being salvaged.
pub const SALVAGE: &str = "salvage";
/// The host's room - a tagged queue somebody else posted to, claimed, finished or lost a claimant
/// on, and the questions a raised item puts to the arbiter. Its own kind because it is news about
/// other sessions on this box, and a layer that rewords one is rewording a disclosure about them.
pub const ROOM: &str = "room";
/// The idle plan-check: the turn the harness opens to ask a session with an unfinished plan to go
/// on. Its own kind because a layer is handed the decision behind it (`nag_`), and one that answers
/// this is answering that sentence.
pub const NAG: &str = "nag";
/// The merge queue's census: how many of a project's rows have been stuck longer than the clock
/// allows. A turn of its own, so it reaches a session nobody is typing to.
pub const CENSUS: &str = "census";
/// A daemon restart: what was restored, what could not be, and the turn that says so.
pub const RESTART: &str = "restart";
/// The turn opened when a provider refused and the session moved to another model.
pub const FALLBACK: &str = "fallback";
/// Another session's message - a parent's correction, or a peer's.
pub const PARENT: &str = "parent";
/// The harness's own correction at a turn's ending: a promise the turn did not keep, or a
/// deliverable it did not hand over.
pub const STEER: &str = "steer";
/// The stall steer, one round before the bound: what the progress detector sees, said so a turn
/// that was working can say so and carry on.
pub const STALL: &str = "stall";
/// A claim about the context that the daemon's own measurement contradicts.
pub const CONTEXT_CLAIM: &str = "context_claim";
/// The operator's answer to a row's question, as the model reads it.
pub const ANSWER: &str = "answer";
/// A directory's own `AGENTS.md`, handed to a SUBAGENT the first time a tool touches that
/// directory. Its own kind because the notice IS an instruction file: a layer may reword it and may
/// not drop it, and one that saw only `nudge` could not tell the two apart.
pub const GUIDANCE: &str = "guidance";

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
    NAG,
    CENSUS,
    RESTART,
    FALLBACK,
    PARENT,
    STEER,
    STALL,
    CONTEXT_CLAIM,
    ANSWER,
    GUIDANCE,
];

/// The kinds whose notice may not be dropped, only reworded - the host's set, not the plugin's.
/// A session never told its rows are stuck, or that the operator answered, has been lied to by
/// silence, so a census and an answer are here beside the assertions about the work.
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
    CENSUS,
    RESTART,
    FALLBACK,
    PARENT,
    ANSWER,
    GUIDANCE,
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

/// One monitor's news in one notice, folded the way the sentence is folded.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Firing {
    /// The monitor's name, as the model declared it.
    pub name: String,
    /// What it watches, when the record still has it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watch: Option<String>,
    /// How the ending happened - a fire, an expiry, a retire - when this line is an ending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<String>,
    /// The sentence this firing itself said.
    pub said: String,
    /// Firings this row stands for besides the one it carries.
    #[serde(default)]
    pub folded: usize,
    /// Whether the line is the watch's ending rather than something it saw.
    #[serde(default)]
    pub ending: bool,
}

/// One stuck state, and how many of the project's rows are in it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Stuck {
    /// The state's own word - `failed`, `conflict`, `blocked`.
    pub state: String,
    /// How many rows of the project are in it.
    pub rows: usize,
}

/// What a notice was composed from, typed per kind so a reader of the facts does not have to parse
/// the sentence, and so a field added here moves the `plugin_hooks` page rather than drifting from
/// it. A kind whose site has nothing to hand carries no fields, which is not a hole in the contract.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Facts {
    /// A monitor firing, or an ending: one row per monitor, folded as the sentence is.
    Monitor { fired: Vec<Firing> },
    /// The background jobs and subagents a notice is about, by handle.
    Job { jobs: Vec<String> },
    /// The todo rows whose condition just became met, by their own words.
    Todo { rows: Vec<String> },
    /// The harness's own reminder: a sentence, and nothing it was built from.
    Nudge,
    /// A session reseated, restarted onto a new prompt, or moved to a new model.
    Reseat,
    /// The merge queue's words about a branch - a verdict, a handover, an emergency.
    MergeQueue { rows: usize },
    /// A layer's own failure.
    Plugin,
    /// The workspace, or another fact about the ground the session stands on.
    Workspace { path: String },
    /// A call the gate admitted without asking anybody.
    Gate,
    /// A turn that produced no answer being salvaged.
    Salvage,
    /// The host's room, and how many events went into the sentence.
    Room { items: usize },
    /// The plan-check, and the rows it names.
    Nag { rows: Vec<String> },
    /// The queue census: how many rows, in which states, and the threshold they passed.
    Census { rows: usize, states: Vec<Stuck> },
    /// A restart, and how many sentences the turn opens with.
    Restart { lines: usize },
    /// The model a refusal moved the session off, and the one it moved to.
    Fallback { was: String, now: String },
    /// Another session's message, and who sent it.
    Parent { from: String },
    /// A correction at the turn's ending: the harness's own promise check.
    Steer,
    /// The stall steer, and the round it was said on.
    Stall { rounds: usize },
    /// A claim about the context, against the measurement that contradicts it.
    ContextClaim { resident: u64, window: u64 },
    /// The operator's answer, and the row's question it settled.
    Answer { row: String, question: String },
    /// A directory's `AGENTS.md`, handed to a subagent: its path and the lines it holds. The text
    /// itself is in the sentence - a layer that rewords this is re-inserting an instruction file,
    /// and must see it whole.
    Guidance { path: String, lines: usize },
}

impl Facts {
    /// The kind word - the same one [`KINDS`] carries and the hook's name ends in.
    pub fn kind(&self) -> &'static str {
        match self {
            Facts::Monitor { .. } => MONITOR,
            Facts::Job { .. } => JOB,
            Facts::Todo { .. } => TODO,
            Facts::Nudge => NUDGE,
            Facts::Reseat => RESEAT,
            Facts::MergeQueue { .. } => MERGE_QUEUE,
            Facts::Plugin => PLUGIN,
            Facts::Workspace { .. } => WORKSPACE,
            Facts::Gate => GATE,
            Facts::Salvage => SALVAGE,
            Facts::Room { .. } => ROOM,
            Facts::Nag { .. } => NAG,
            Facts::Census { .. } => CENSUS,
            Facts::Restart { .. } => RESTART,
            Facts::Fallback { .. } => FALLBACK,
            Facts::Parent { .. } => PARENT,
            Facts::Steer => STEER,
            Facts::Stall { .. } => STALL,
            Facts::ContextClaim { .. } => CONTEXT_CLAIM,
            Facts::Answer { .. } => ANSWER,
            Facts::Guidance { .. } => GUIDANCE,
        }
    }

    /// The facts as the wire carries them: one object, with the kind on it.
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    /// The facts back off the wire, or `None` for a document that is not one.
    pub fn from_value(value: &Value) -> Option<Facts> {
        serde_json::from_value(value.clone()).ok()
    }

    /// One value per kind, in [`KINDS`]'s order: the fixture a round-trip test walks, and what a
    /// page describing the contract shows. Every kind has one, so a kind added without its facts is
    /// a failing test rather than a variant nobody composes.
    pub fn one_of_each() -> Vec<Facts> {
        vec![
            Facts::Monitor {
                fired: vec![Firing {
                    name: "pr-24".into(),
                    watch: Some("pr o/r#24".into()),
                    ended: None,
                    said: "[monitor] `pr-24` fired: its checks finished.".into(),
                    folded: 0,
                    ending: false,
                }],
            },
            Facts::Job {
                jobs: vec!["j7".into()],
            },
            Facts::Todo {
                rows: vec!["land the release".into()],
            },
            Facts::Nudge,
            Facts::Reseat,
            Facts::MergeQueue { rows: 1 },
            Facts::Plugin,
            Facts::Workspace {
                path: "/home/dead/Projects/x".into(),
            },
            Facts::Gate,
            Facts::Salvage,
            Facts::Room { items: 1 },
            Facts::Nag {
                rows: vec!["land the release".into()],
            },
            Facts::Census {
                rows: 3,
                states: vec![Stuck {
                    state: "failed".into(),
                    rows: 3,
                }],
            },
            Facts::Restart { lines: 2 },
            Facts::Fallback {
                was: "glm-coding/glm-5.3".into(),
                now: "glm/glm-4.6".into(),
            },
            Facts::Parent {
                from: "s-parent".into(),
            },
            Facts::Steer,
            Facts::Stall { rounds: 4 },
            Facts::ContextClaim {
                resident: 120_000,
                window: 200_000,
            },
            Facts::Answer {
                row: "land the release".into(),
                question: "which database?".into(),
            },
            Facts::Guidance {
                path: "crates/tools/AGENTS.md".into(),
                lines: 12,
            },
        ]
    }
}

/// The input: one notice, with the facts it was composed from.
/// Every field is present on the wire - `facts` is `{}` rather than absent when a site has none -
/// because a guest deserialises this into a struct, and a missing key would read as a host bug.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Notice {
    /// [`V`](crate::V).
    pub v: u32,
    /// The notice's kind - [`KINDS`] - and the same word the hook's own name ends in.
    pub kind: String,
    /// The sentence as the harness composed it, which is what the model reads if this hook
    /// answers nothing.
    pub text: String,
    /// What the sentence was built from. Shape is the site's: nobody but that site reads it.
    #[serde(default)]
    pub facts: Value,
    /// Whether this kind may be dropped at all - [`droppable`]'s answer, carried so that a layer
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

    /// One notice, from the facts its site composed it out of - the kind comes off the facts, so a
    /// site cannot say one kind and hand over another's.
    pub fn of(text: impl Into<String>, facts: &Facts) -> Notice {
        Notice::new(facts.kind(), text, facts.to_value())
    }

    /// The synthetic input `plugin test` and the shadow gate use when a session has none
    /// recorded: one monitor firing.
    pub fn synthetic() -> Notice {
        Notice::of(
            "[monitor] `pr-24` fired: its checks finished.",
            &Facts::Monitor {
                fired: vec![Firing {
                    name: "pr-24".into(),
                    watch: Some("pr o/r#24".into()),
                    ended: None,
                    said: "[monitor] `pr-24` fired: its checks finished.".into(),
                    folded: 0,
                    ending: false,
                }],
            },
        )
    }
}

/// The answer: the sentence, reworded - or nothing at all.
/// Absent fields are absent on the wire (never `null`), so *this layer said nothing about the text*
/// and *this layer said the text is null* cannot be confused.
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
    /// Do not say it - only for a kind that may be dropped.
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

    /// The notice, dropped - a request the host honours only for a droppable kind.
    pub fn dropped() -> NoticeOut {
        NoticeOut {
            text: None,
            drop: Some(true),
        }
    }

    /// What this answer means, in the one order the host applies it: a drop, then a rewording,
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
    use serde_json::json;

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

    /// A guest that leaves everything out said nothing - which is `Unchanged`, not an empty
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

    /// The kinds a project may switch off are the six the host has no one owed: a firing, a nudge,
    /// a plan-check, a turn's own correction, the stall steer and a context claim. Everything else
    /// is a fact the model is owed, and a layer may only reword those.
    #[test]
    fn only_the_hosts_own_reminders_may_be_dropped() {
        for kind in KINDS {
            assert_eq!(
                droppable(kind),
                !MUST_SEE.contains(kind),
                "{kind} disagrees with the must-see list"
            );
        }
        for kind in [MONITOR, NUDGE, NAG, STEER, STALL, CONTEXT_CLAIM] {
            assert!(droppable(kind), "{kind} is the harness's own reminder");
        }
        for kind in [MERGE_QUEUE, CENSUS, RESTART, FALLBACK, PARENT, ANSWER] {
            assert!(!droppable(kind), "{kind} is a fact the model is owed");
        }
        // An unknown kind cannot be the one a model is owed, so it is kept.
        assert!(!droppable("a-kind-nobody-composes"));
        assert!(!known("a-kind-nobody-composes"));
    }

    /// Every kind's facts survive the wire they reach a layer on: what a layer reads and what a head
    /// card renders are the same fields, and the kind rides on them so a site cannot say one kind
    /// and hand over another's.
    #[test]
    fn facts_round_trip_per_kind() {
        let fixtures = Facts::one_of_each();
        let mut kinds: Vec<&str> = fixtures.iter().map(|f| f.kind()).collect();
        for kind in KINDS {
            assert!(kinds.contains(kind), "no facts fixture for `{kind}`");
        }
        kinds.sort_unstable();
        kinds.dedup();
        assert_eq!(
            kinds.len(),
            KINDS.len(),
            "one fixture per kind, not a repeat"
        );
        for facts in &fixtures {
            let wire = facts.to_value();
            assert_eq!(wire["kind"], json!(facts.kind()), "{wire}");
            assert_eq!(
                Facts::from_value(&wire).as_ref(),
                Some(facts),
                "{} did not survive the wire: {wire}",
                facts.kind()
            );
            // And the notice built from them carries that kind and those facts.
            let notice = Notice::of("a sentence", facts);
            assert_eq!(notice.kind, facts.kind());
            assert_eq!(notice.facts, wire);
        }
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
