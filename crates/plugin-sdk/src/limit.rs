//! `limit_<name>` - what a session does when its provider refuses on a rate or usage limit. With a
//! layer answering [`HOOK`], the layer alone decides: the host's own wait and fallback rules do not
//! run. The host keeps the layer's `state` per limit episode and hands it back unchanged.

use serde_json::Value;

/// The export-name prefix: `limit_<name>`. Names other than [`HOOK`] are the targets of `call`.
pub const PREFIX: &str = "limit_";

/// The export the host asks first on every refusal of an episode.
pub const HOOK: &str = "limit_decide";

/// The hook a layer named `name` answers.
pub fn hook(name: &str) -> String {
    format!("{PREFIX}{name}")
}

/// The refusal, as the host read it.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Refusal {
    /// The HTTP status, `429` for a rate limit.
    pub status: u16,
    pub provider: String,
    pub model: String,
    /// `rate_limit` or `quota`.
    pub kind: String,
    /// The limit's window as the provider named it (`5 hour`), when it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<String>,
    /// The provider's reset, unix ms, when it named one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_ms: Option<u64>,
    /// Which refusal of this episode this is, from 1.
    pub attempt: u32,
    /// The provider's own words, one line.
    #[serde(default)]
    pub said: String,
}

/// The session the refusal happened in.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Session {
    pub id: String,
    pub seat: String,
    /// Who the session is for, when the host knows: the parent's id for a child.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    pub turn_in_flight: bool,
}

/// One configured fallback target, and whether this session already knows it is limited.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Candidate {
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limited_until_ms: Option<u64>,
}

/// The operator's answer to this episode's last `ask_user`.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Answered {
    pub question: String,
    #[serde(default)]
    pub options: Vec<String>,
    /// The chosen option's index, when one was chosen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option: Option<usize>,
    /// The chosen option's label, or the words typed instead.
    pub text: String,
    /// Who answered, read from the head.
    pub by: String,
}

/// Everything one call is handed.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Input {
    /// [`V`](crate::V).
    pub v: u32,
    /// The host's clock, unix ms: what `when` is measured against.
    pub now_ms: u64,
    pub refusal: Refusal,
    /// The plan's usage where the host has a reading, as the provider reported it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Value>,
    pub session: Session,
    #[serde(default)]
    pub candidates: Vec<Candidate>,
    /// The `state` the last call of this episode returned; `null` on its first call.
    #[serde(default)]
    pub state: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered: Option<Answered>,
}

impl Input {
    /// The input `plugin test` and the shadow gate use when none is recorded: a first 429.
    pub fn synthetic() -> Input {
        Input {
            v: crate::V,
            now_ms: 1_794_000_000_000,
            refusal: Refusal {
                status: 429,
                provider: "glm".into(),
                model: "glm-5.3".into(),
                kind: "rate_limit".into(),
                window: Some("5 hour".into()),
                reset_ms: Some(1_794_000_300_000),
                attempt: 1,
                said: "Usage limit reached for 5 hour.".into(),
            },
            usage: None,
            session: Session {
                id: "s-synthetic".into(),
                seat: "orchestrator".into(),
                parent: None,
                turn_in_flight: true,
            },
            candidates: vec![Candidate {
                provider: "deepseek".into(),
                model: "deepseek-flash".into(),
                limited_until_ms: None,
            }],
            state: Value::Null,
            answered: None,
        }
    }
}

/// What to do. A name outside this set is a malformed answer.
#[derive(Debug, Clone, PartialEq)]
pub enum Do {
    /// Park, then run the same request at `when`.
    Wait,
    /// Continue this session on another target.
    Switch { provider: String, model: String },
    /// Continue in a new session on that target.
    NewSession { provider: String, model: String },
    /// Ask the operator; the answer is the next call's `answered`.
    AskUser {
        question: String,
        options: Vec<String>,
    },
    /// Run `limit_<function>` next, with the same input and this answer's state.
    Call { function: String },
    /// End the turn and tell the operator.
    Stop,
}

impl Do {
    /// The word on the wire.
    pub fn name(&self) -> &'static str {
        match self {
            Do::Wait => "wait",
            Do::Switch { .. } => "switch",
            Do::NewSession { .. } => "new_session",
            Do::AskUser { .. } => "ask_user",
            Do::Call { .. } => "call",
            Do::Stop => "stop",
        }
    }
}

/// One answer: `{"state": …, "do": "wait", "when": <unix ms>}`, with the action's own fields beside
/// `do` (`provider`, `model`, `question`, `options`, `function`).
#[derive(Debug, Clone, PartialEq)]
pub struct Out {
    pub state: Value,
    pub act: Do,
    /// Unix ms. Required for `wait`, unless the refusal carried a reset.
    pub when: Option<u64>,
}

impl Out {
    /// Read an answer, refusing anything outside the record: a malformed answer must never be
    /// half-applied.
    pub fn parse(v: &Value) -> Result<Out, String> {
        let o = v
            .as_object()
            .ok_or_else(|| format!("a limit answer is an object, not {v}"))?;
        let word = o
            .get("do")
            .and_then(Value::as_str)
            .ok_or("a limit answer names its action in `do`")?;
        let text = |k: &str| -> Result<String, String> {
            o.get(k)
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map(str::to_string)
                .ok_or_else(|| format!("`do: {word}` needs `{k}` as a non-empty string"))
        };
        let (act, fields): (Do, &[&str]) = match word {
            "wait" => (Do::Wait, &[]),
            "switch" => (
                Do::Switch {
                    provider: text("provider")?,
                    model: text("model")?,
                },
                &["provider", "model"],
            ),
            "new_session" => (
                Do::NewSession {
                    provider: text("provider")?,
                    model: text("model")?,
                },
                &["provider", "model"],
            ),
            "ask_user" => {
                let options = match o.get("options") {
                    None | Some(Value::Null) => Vec::new(),
                    Some(Value::Array(a)) => a
                        .iter()
                        .map(|x| x.as_str().map(str::to_string))
                        .collect::<Option<Vec<_>>>()
                        .ok_or("`options` is a list of strings")?,
                    Some(other) => return Err(format!("`options` is a list, not {other}")),
                };
                (
                    Do::AskUser {
                        question: text("question")?,
                        options,
                    },
                    &["question", "options"],
                )
            }
            "call" => (
                Do::Call {
                    function: text("function")?,
                },
                &["function"],
            ),
            "stop" => (Do::Stop, &[]),
            other => {
                return Err(format!(
                    "`do: {other}` is not an action; the set is wait, switch, new_session, \
                     ask_user, call, stop"
                ));
            }
        };
        if let Some(k) = o.keys().find(|k| {
            !matches!(k.as_str(), "state" | "do" | "when") && !fields.contains(&k.as_str())
        }) {
            return Err(format!("`{k}` is not a field of a `do: {word}` answer"));
        }
        let when = match o.get("when") {
            None | Some(Value::Null) => None,
            Some(w) => Some(
                w.as_u64()
                    .ok_or_else(|| format!("`when` is unix ms, not {w}"))?,
            ),
        };
        Ok(Out {
            state: o.get("state").cloned().unwrap_or(Value::Null),
            act,
            when,
        })
    }

    /// The one line `plugin test` prints for an accepted answer.
    pub fn said(&self) -> String {
        match &self.act {
            Do::Wait => match self.when {
                Some(at) => format!("it waits until {at}"),
                None => "it waits for the provider's reset".into(),
            },
            Do::Switch { provider, model } => format!("it switches to {provider}/{model}"),
            Do::NewSession { provider, model } => {
                format!("it continues in a new session on {provider}/{model}")
            }
            Do::AskUser { question, .. } => format!("it asks the operator: {question}"),
            Do::Call { function } => format!("it calls {}", hook(function)),
            Do::Stop => "it ends the turn".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_action_parses_and_nothing_else_does() {
        let wait = Out::parse(&json!({"state": {"n": 1}, "do": "wait", "when": 5})).unwrap();
        assert_eq!(wait.act, Do::Wait);
        assert_eq!(wait.when, Some(5));
        assert_eq!(wait.state, json!({"n": 1}));
        let sw = Out::parse(&json!({"do": "switch", "provider": "p", "model": "m"})).unwrap();
        assert_eq!(
            sw.act,
            Do::Switch {
                provider: "p".into(),
                model: "m".into()
            }
        );
        let ask = Out::parse(&json!({"do": "ask_user", "question": "q", "options": ["a"]}));
        assert!(matches!(ask.unwrap().act, Do::AskUser { options, .. } if options == ["a"]));
        assert!(Out::parse(&json!({"do": "call", "function": "next"})).is_ok());
        assert!(Out::parse(&json!({"do": "stop"})).is_ok());

        for bad in [
            json!({"do": "retry"}),
            json!({"state": 1}),
            json!({"do": "switch", "provider": "p"}),
            json!({"do": "wait", "when": "soon"}),
            json!({"do": "stop", "why": "extra"}),
            json!("wait"),
        ] {
            assert!(Out::parse(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_synthetic_input_round_trips() {
        let i = Input::synthetic();
        let back: Input = serde_json::from_value(serde_json::to_value(&i).unwrap()).unwrap();
        assert_eq!(back, i);
    }
}
