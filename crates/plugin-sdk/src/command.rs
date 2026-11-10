//! The `command_<verb>` hook and the two verb lists a layer may not reach across. A `/` verb the
//! host has not got is offered here first, at the head and at the daemon; the host's own verbs and
//! a person's own verbs are refused at admit, and a model gets a self-scoped verb in their place.

/// The export-name prefix: `command_<verb>`.
pub const PREFIX: &str = "command_";

/// The export name one verb is asked under.
pub fn hook(verb: &str) -> String {
    format!("command_{verb}")
}

/// The answer a `command_` layer passes with, the shape the head has always read: `-1` is *not
/// mine*, and the line falls through to the host.
pub const PASS: i64 = -1;

/// What one `/` line hands a layer.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Input {
    /// The contract version this host speaks - [`crate::V`].
    pub v: u32,
    /// The verb, without its `/`: the same word the export is named after.
    pub verb: String,
    /// The rest of the line as typed, whole, for the layer to parse.
    pub line: String,
    /// The session the line was typed in: `None` at a head that is not attached yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// The feeds this layer declared, and only those, by name (`pane::Feed::name`).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub feeds: std::collections::BTreeMap<String, serde_json::Value>,
}

/// The answer a layer wrote: one act, or a refusal sentence - or `PASS`, which is no document at all.
/// The act's name is the trust boundary (the host performs the names its own code knows), so an act
/// is a word and its arguments rather than a code path.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Answer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub act: Option<Act>,
    /// The sentence the host shows the person when the layer will not do it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
}

/// One act: the host's verb and the arguments it is performed with.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Act {
    /// The word `plugin.toml`'s `actions` declares - `add_todo` for a `/` verb that writes a row.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<serde_json::Value>,
}

/// The `/` verbs the host answers itself: the head's arms, the daemon's `Slash::parse`, and the door
/// tools a head offers as verbs. A layer naming one is refused at admit, as a pane is refused a
/// `HOST_KEYS` key: a hook that can never fire is a layer its author has been lied to about.
pub const HOST_VERBS: &[&str] = &[
    // The head's own dispatcher (`crates/tui/src/app/commands.rs`), aliases included: an alias is a
    // spelling of a verb the head answers, so it shadows just as surely.
    "asks",
    "cells",
    "compact",
    "config",
    "copy",
    "diff",
    "dismiss",
    "dm",
    "fb",
    "forget",
    "h",
    "help",
    "i",
    "interrupt",
    "invite",
    "jobs",
    "leave",
    "mode",
    "models",
    "monitors",
    "new",
    "notes",
    "pane",
    "peek",
    "plugin",
    "promote",
    "q",
    "queue",
    "quit",
    "r",
    "rename",
    "repo",
    "reseat",
    "resume",
    "resync",
    "rooms",
    "s",
    "search",
    "sessions",
    "settings",
    "signals",
    "skills",
    "standing",
    "stats",
    "status",
    "subagents",
    "switch",
    "t",
    "think",
    "todos",
    "tools",
    "v",
    "verbosity",
    "?",
    // The daemon's (`crates/harnessd/src/slash.rs`'s `VERBS`), aliases included.
    "auto-compact",
    "boundary",
    "clear",
    "default",
    "default-model",
    "default_model",
    "flowy",
    "gate",
    "job",
    "listen",
    "login",
    "model",
    "monitor",
    "nag",
    "nags",
    "prune",
    "supervise",
    "supervised",
    // The door's, hyphenated from the tool's own name (`sessionlog::head_run_verb`), so `/read` and
    // `/web-search` are the head's before any layer is asked.
    "read",
    "web-fetch",
    "web-search",
];

/// Whether a verb is one the host answers, and so one no layer may take.
pub fn is_host_verb(verb: &str) -> bool {
    HOST_VERBS.contains(&verb)
}

/// The verbs this build's own layers answer: `command_todo` is the built-in `todo` layer, and a
/// project's layer above it refines what a new row may look like - which is the hook's whole design,
/// not a shadow. Outside this list a verb a layer answers is one the host has not got.
pub const BUILTIN_VERBS: &[&str] = &["todo"];

/// Whether a verb is answered by a layer shipped with this build.
pub fn is_builtin_verb(verb: &str) -> bool {
    BUILTIN_VERBS.contains(&verb)
}

/// The verbs only a person may cause, with the self-scoped verb a model gets in their place
/// ([`SELF_VERBS`]): `/reseat` typed in a head, `reseat_self` called by the session itself. A layer
/// asking for one is refused at admit, and a model reaching for one is told which self verb exists.
pub const OPERATOR_VERBS: &[(&str, Option<&str>)] = &[
    ("reseat", Some(RESEAT_SELF)),
    ("compact", Some(COMPACT_SELF)),
    ("nag", Some(NAG_PAUSE_SELF)),
    ("nag_pause", Some(NAG_PAUSE_SELF)),
    // The merge queue's verdicts are a person's, and there is no self verb for them: a session that
    // could approve its own work would be the reviewer of every branch it wrote.
    ("queue", None),
    ("queue_approve", None),
    ("queue_veto", None),
    ("queue_reset", None),
    ("queue_clean", None),
    ("gate", None),
    ("supervise", None),
    ("mode", None),
];

/// The model's own verbs, one session deep: the name is the whole scope.
pub const RESEAT_SELF: &str = "reseat_self";
pub const COMPACT_SELF: &str = "compact_self";
pub const NAG_PAUSE_SELF: &str = "nag_pause_self";

/// Every self-scoped verb, so a host can seat them together and a test can name them all.
pub const SELF_VERBS: &[&str] = &[RESEAT_SELF, COMPACT_SELF, NAG_PAUSE_SELF];

/// Whether a name is one of the model's own verbs.
pub fn is_self_verb(name: &str) -> bool {
    SELF_VERBS.contains(&name)
}

/// The self verb that stands where an operator verb would, if one does.
pub fn self_verb_for(operator: &str) -> Option<&'static str> {
    OPERATOR_VERBS
        .iter()
        .find(|(v, _)| *v == operator)
        .and_then(|(_, self_)| *self_)
}

/// Whether a verb is a person's, which a model may not cause and a layer may not ask for.
pub fn is_operator_verb(verb: &str) -> bool {
    OPERATOR_VERBS.iter().any(|(v, _)| *v == verb)
}

/// The acts only a person's own head may perform; a manifest naming one in `actions` is refused at
/// admit. `queue_op` is the operator's, and the three self verbs are the model's own door and nobody
/// else's - a layer that could act them would be a plugin reseating a session.
pub const OPERATOR_ACTS: &[&str] = &["queue_op", "gate_rule", "reseat", "compact", "nag_pause"];

/// Whether an act name is one only the operator's own head may perform.
pub fn is_operator_act(act: &str) -> bool {
    OPERATOR_ACTS.contains(&act)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_verbs_hook_is_its_name_and_a_pass_is_a_number_no_answer_parses_as() {
        assert_eq!(hook("standup"), "command_standup");
        assert!(
            serde_json::from_value::<Answer>(serde_json::json!(PASS)).is_err(),
            "`-1` is not an answer: it is the absence of one"
        );
    }

    #[test]
    fn an_input_carries_the_verb_the_line_and_the_session() {
        let input = Input {
            v: crate::V,
            verb: "standup".into(),
            line: "yesterday today blockers".into(),
            session: Some("s-1789462738453908838".into()),
            feeds: Default::default(),
        };
        let wire = serde_json::to_value(&input).expect("serialises");
        assert_eq!(wire["verb"], serde_json::json!("standup"));
        assert_eq!(wire["line"], serde_json::json!("yesterday today blockers"));
        assert_eq!(serde_json::from_value::<Input>(wire).unwrap(), input);
    }

    #[test]
    fn an_answer_is_one_act_or_a_refusal() {
        let wire = serde_json::json!({
            "act": {"name": "add_todo", "args": {"content": "buy milk"}}
        });
        let answer: Answer = serde_json::from_value(wire.clone()).expect("an answer parses");
        assert_eq!(answer.act.as_ref().unwrap().name, "add_todo");
        assert_eq!(
            serde_json::to_value(&answer).unwrap(),
            wire,
            "written back as it came"
        );
        let refused: Answer =
            serde_json::from_value(serde_json::json!({"refusal": "not this project's channel"}))
                .expect("a refusal parses");
        assert!(refused.act.is_none());
    }

    #[test]
    fn the_hosts_verbs_are_not_the_models_own_and_the_operator_keeps_theirs() {
        for verb in SELF_VERBS {
            assert!(!is_host_verb(verb), "`{verb}` is not a `/` verb");
            assert!(!is_operator_verb(verb), "`{verb}` is the model's");
        }
        // Every operator verb that has a self stand-in names a seated one.
        for (verb, self_) in OPERATOR_VERBS {
            assert!(is_operator_verb(verb));
            if let Some(self_) = self_ {
                assert!(is_self_verb(self_), "`{verb}` points at `{self_}`");
            }
        }
        assert_eq!(self_verb_for("reseat"), Some(RESEAT_SELF));
        assert_eq!(self_verb_for("queue_approve"), None);
        // The acts a layer cannot perform are not the acts it may: `add_todo` is how a `command_`
        // layer writes a row, and it stays.
        assert!(!is_operator_act("add_todo"));
        assert!(is_operator_act("queue_op"));
    }

    #[test]
    fn no_verb_is_listed_twice_and_nothing_dangles() {
        let mut sorted = HOST_VERBS.to_vec();
        sorted.sort_unstable();
        let before = sorted.len();
        sorted.dedup();
        assert_eq!(before, sorted.len(), "a verb listed twice");
        for (verb, _) in OPERATOR_VERBS {
            if verb.contains('_') {
                continue; // `nag_pause` is a spelling a model may reach for, not a `/` verb
            }
            assert!(
                is_host_verb(verb),
                "`{verb}` is an operator verb and not a host one"
            );
        }
    }
}
