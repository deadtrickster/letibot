//! The `lifecycle_<event>` hook (X21): a session's own boundaries - open, reseat, restart, close -
//! handed over as facts, and answered with acts only.
//!
//! A layer can never block one. The answer type has no field for it ([`Answer`] is acts and a refusal
//! sentence), and the host performs the event whatever comes back: what a layer adds is a sentence
//! the model must see, a board row, a monitor or a room message - the wording a release used to carry
//! (`WAKE_TAIL`, `RESTART_PROMISES`) and the *what exactly changed* of G4.

/// The export name a lifecycle answer comes from: `lifecycle_<event>`, beside `notice_<kind>`.
pub const PREFIX: &str = "lifecycle_";

/// The four events, as [`Input::event`] spells them and as the export is named.
pub const EVENTS: [&str; 4] = ["open", "reseat", "restart", "close"];

/// The export name one event is asked under.
pub fn hook(event: &str) -> String {
    format!("lifecycle_{event}")
}

/// The acts an answer may carry, each of which `plugin.toml`'s `actions` has to declare first.
pub const ACTS: [&str; 4] = ["add_todo", "arm", "say", "room_say"];

/// What one event hands a layer.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Input {
    /// The contract version this host speaks - [`crate::V`].
    pub v: u32,
    /// One of [`EVENTS`], carried as well as being the export's name, so one function can answer all
    /// four exports.
    pub event: String,
    /// The session this happened to. A layer acts on THIS session; there is no field naming another.
    pub session: String,
    /// What moved under the conversation, which is the reason a reseat is asked for at all.
    #[serde(default)]
    pub stale: Stale,
    /// The session's board, as the model reads it: a layer that adds a row adds it beside these.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rows: Vec<Row>,
    /// The host's clock, unix millis - absent from a host that predates it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub now_ms: Option<u64>,
}

/// What changed: the prompt's staleness and the tools the seat gained or lost.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Stale {
    /// Why the prompt this conversation speaks is no longer the one the daemon seats, or `None`
    /// when nothing moved. A sentence rather than a bool: *what exactly changed* is the fact G4
    /// asked for, and a layer that rewords it needs it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// The tools whose seat changed: `command_and_read` style names, sorted, empty when none did.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
}

/// One board row, as a layer reads it.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Row {
    pub id: String,
    pub content: String,
    /// `pending`, `in_progress`, `completed` or `cancelled`, as the wire spells them.
    pub status: String,
    /// Who wrote it: `operator` or `agent`.
    pub by: String,
    /// What this row waits on, by name - a row, a child, a job, a monitor, the operator.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub needs: Vec<String>,
}

/// One act an answer carries: the verb `plugin.toml`'s `actions` declares, and the arguments the host
/// performs it with. The wire shape is a pane's (`letibot_sessionlog::pane::Acted`) because one
/// vocabulary is what a person reads in two places; it is written out here because this crate is
/// compiled into a guest too, and the daemon's crate is not.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Act {
    pub verb: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<serde_json::Value>,
}

/// What one event's answer is: acts, and a sentence. There is no field that could stop, delay or
/// undo the event, which is what makes *a layer cannot block a reseat* a property of the type.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Answer {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acts: Vec<Act>,
    /// The sentence the host shows when the layer will not do something it was asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
}

impl Answer {
    /// The answer a layer wrote, or the sentence saying why it is not one.
    pub fn parse(v: serde_json::Value) -> Result<Answer, String> {
        serde_json::from_value(v).map_err(|e| format!("not an answer: {e}"))
    }

    /// Refuse an act the manifest did not declare: the `actions` list is the whole gate, exactly as
    /// it is for a pane's rows.
    pub fn validate_for(&self, declared: &[String]) -> Result<(), String> {
        match self
            .acts
            .iter()
            .find(|a| !declared.iter().any(|d| d == &a.verb))
        {
            Some(act) => Err(format!(
                "the answer asks for `{}`, which `actions` does not declare ({})",
                act.verb,
                declared.join(", ")
            )),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_event_has_its_own_hook_name_a_host_finds() {
        for event in EVENTS {
            let hook = hook(event);
            assert_eq!(hook, format!("lifecycle_{event}"));
            assert_eq!(
                hook.strip_prefix(PREFIX),
                Some(event),
                "a prefix with nothing after it names no hook"
            );
        }
        assert!(!EVENTS.contains(&"reseat_all"));
    }

    #[test]
    fn an_input_carries_the_event_the_stale_facts_and_the_board() {
        let input = Input {
            v: crate::V,
            event: "reseat".into(),
            session: "s-1789462738453908838".into(),
            stale: Stale {
                prompt: Some("the tool list changed: bash left the seat".into()),
                tools: vec!["bash".into(), "read".into()],
            },
            rows: vec![Row {
                id: "7".into(),
                content: "land K10".into(),
                status: "in_progress".into(),
                by: "agent".into(),
                needs: vec!["row".into()],
            }],
            now_ms: Some(1_762_000_000_000),
        };
        let wire = serde_json::to_value(&input).expect("serialises");
        assert_eq!(wire["event"], serde_json::json!("reseat"));
        assert_eq!(wire["stale"]["tools"], serde_json::json!(["bash", "read"]));
        assert_eq!(wire["rows"][0]["needs"], serde_json::json!(["row"]));
        assert_eq!(serde_json::from_value::<Input>(wire).unwrap(), input);
    }

    #[test]
    fn an_answer_is_acts_and_a_refusal_and_nothing_that_could_stop_the_event() {
        let answer = Answer {
            acts: vec![Act {
                verb: "say".into(),
                args: Some(serde_json::json!({
                    "text": "the tool list moved: `bash` left the seat"
                })),
            }],
            refusal: None,
        };
        answer.validate_for(&["say".to_string()]).expect("declared");
        let why = answer
            .validate_for(&["add_todo".to_string()])
            .expect_err("an undeclared act is refused");
        assert!(why.contains("say"), "{why}");
        // The wire has nowhere to say *do not reseat*: the four act names are the whole vocabulary.
        let wire = serde_json::to_value(&answer).expect("serialises");
        assert!(wire.get("block").is_none() && wire.get("pass").is_none());
        assert_eq!(Answer::parse(wire.clone()).expect("parses back"), answer);
        for act in ACTS {
            assert!(!act.is_empty());
        }
    }
}
