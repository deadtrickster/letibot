//! `push_<name>` - what reaches Claude Code, and in what words
//! (`docs/plugin-extension-points-wide.md` X25). One subject today, [`CHANNEL`]: the channel a
//! Claude Code bridge wakes its session on.
//!
//! The host computes the facts - a room message's reach, the package events it watches - and a layer
//! decides whether the push happens and how it reads. Two events are never silenced: an `ask` and a
//! `failed` are a session needing somebody, and a layer that hides one has hidden the work.

use serde_json::Value;

/// The export-name prefix: `push_<name>`, because a kind with no name after its prefix names no hook.
pub const PREFIX: &str = "push_";

/// The one subject the bridge asks about: the channel it pushes to a Claude Code session.
pub const CHANNEL: &str = "channel";

/// The hook the host asks.
pub const HOOK: &str = "push_channel";

/// The hook a layer named `name` answers.
pub fn hook(name: &str) -> String {
    format!("{PREFIX}{name}")
}

/// The two events a layer may reword and never take away. A permission ask and a failure are the
/// session saying it needs a person; silencing one is a channel that looks quiet while work is stuck.
pub const MUST_SAY: [&str; 2] = ["ask", "failed"];

/// What a room message asks of its readers, in the room's own words (`letibot_room::Reach`). This is
/// the whole of R117's open choice - *every room message or only addressed* - as a rule a layer
/// writes: push the `addressed` ones, or all four.
pub const ROOM_EVENTS: [&str; 4] = ["operator", "dm", "addressed", "passing"];

/// The package events the bridge pushes, so a layer's rule can name one. The list is documentation
/// and not a gate: a new event is a new word on the wire, not a contract a layer must be rebuilt for.
pub const PACKAGE_EVENTS: [&str; 8] = [
    "turn_ended",
    "ask",
    "failed",
    "trouble",
    "limited",
    "switched",
    "resumed",
    "job_ended",
];

/// What the host would do with no layer answering. A built-in layer says this back, which is how a
/// stack has a bottom that is today's behaviour.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Native {
    /// Whether the bridge would push this one on its own.
    pub push: bool,
}

/// Everything a layer is handed about one push.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Input {
    /// [`V`](crate::V).
    pub v: u32,
    /// The event, as a room [`Reach`] word ([`ROOM_EVENTS`]) or a [`PACKAGE_EVENTS`] one.
    pub event: String,
    /// The session the push is about: a package's id, or the bridge's own handle for a room message.
    #[serde(default)]
    pub session: String,
    /// The room, when the push is a room message - and `None` for every package event, so a rule can
    /// tell the two apart in one field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room: Option<String>,
    /// Who the message was addressed to, when it named anybody.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// The sentence the bridge would push, header line and all.
    #[serde(default)]
    pub text: String,
    /// The bridge's own decision, which a layer that changes nothing says back.
    #[serde(default)]
    pub native: Native,
}

impl Input {
    /// Whether this is one of the two events no layer may take away.
    pub fn must_say(&self) -> bool {
        MUST_SAY.contains(&self.event.as_str())
    }

    /// The input `plugin test` and the shadow gate use when a bridge has none recorded: a room line
    /// addressed to this seat, which is the push this hook exists for.
    pub fn synthetic() -> Input {
        Input {
            v: crate::V,
            event: "addressed".to_string(),
            session: "cc@package".to_string(),
            room: Some("host".to_string()),
            to: Some("cc@package".to_string()),
            text: "#12 [host] from s-a (session) to you\n> the gate is red on `wait_jobs`"
                .to_string(),
            native: Native { push: true },
        }
    }
}

/// What a layer answered: push it (these words), or say nothing.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Out {
    /// Whether the push happens. Absent means *as the bridge decided*.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push: Option<bool>,
    /// The words to push instead of the bridge's, when the layer has better ones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// What the push came to, once the two host rules above have been applied.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    /// Whether the notification is pushed at all.
    pub push: bool,
    /// The sentence it carries - empty when nothing is pushed.
    pub text: String,
}

impl Decision {
    /// What the bridge would have done with no layer: the fallback a silent, trapping or unreadable
    /// answer falls to.
    pub fn native(input: &Input) -> Decision {
        Decision {
            push: input.native.push,
            text: input.text.clone(),
        }
    }
}

impl Out {
    /// This answer applied to the input it was given, or why the host refuses it.
    ///
    /// Three rules and no others: an `ask` or a `failed` may be reworded and never silenced; an answer
    /// that says neither `push` nor `text` is not an answer; and a `push` the layer leaves out follows
    /// the bridge's own decision, so a layer that only rewords cannot widen one by accident.
    pub fn decide(&self, input: &Input) -> Result<Decision, String> {
        if self.push.is_none() && self.text.as_deref().is_none_or(|t| t.trim().is_empty()) {
            return Err(format!(
                "it answered neither `push` nor `text`, and a push answer is \
                 `{{\"push\":true|false}}` and/or `{{\"text\":\"…\"}}`: {input:?}"
            ));
        }
        if self.push == Some(false) && input.must_say() {
            return Err(format!(
                "it silenced a `{}`, and a session's ask and its failures are the two events a layer \
                 may reword and never take away - this one is pushed in the bridge's own words",
                input.event
            ));
        }
        if self.push == Some(false) {
            return Ok(Decision {
                push: false,
                text: String::new(),
            });
        }
        let text = self
            .text
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| input.text.clone());
        Ok(Decision {
            push: self.push.unwrap_or(input.native.push),
            text,
        })
    }

    /// What this answer means, in one line, for `plugin test` and the gate's report.
    pub fn said(&self, input: &Input) -> String {
        match self.decide(input) {
            Err(why) => format!("refused: {why}"),
            Ok(decision) if !decision.push => "it says nothing about this one".to_string(),
            Ok(decision) if decision.text == input.text => {
                "it pushes the bridge's own words".into()
            }
            Ok(decision) => format!("it pushes these words instead: {}", decision.text),
        }
    }
}

/// The answer off the wire, or why it is not one.
pub fn read(answer: &Value) -> Result<Out, String> {
    serde_json::from_value(answer.clone()).map_err(|e| format!("not a push answer ({e})"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn room(event: &str) -> Input {
        Input {
            event: event.to_string(),
            native: Native {
                push: event != "passing",
            },
            ..Input::synthetic()
        }
    }

    /// The rule this hook exists for: a layer may not hide the session asking for somebody. Both
    /// must-say events are refused by name, and both may be reworded.
    #[test]
    fn a_push_layer_cannot_suppress_an_ask() {
        for event in MUST_SAY {
            let input = Input {
                event: event.to_string(),
                native: Native { push: true },
                ..Input::synthetic()
            };
            let refused = Out {
                push: Some(false),
                text: None,
            }
            .decide(&input)
            .expect_err("a silenced ask is not an answer");
            assert!(refused.contains(event), "{refused}");
            // The words are the layer's, though: a reworded ask still reaches the person.
            let said = Out {
                push: Some(true),
                text: Some(format!("{event}: read me first")),
            }
            .decide(&input)
            .expect("a reworded ask");
            assert!(said.push);
            assert_eq!(said.text, format!("{event}: read me first"));
        }
    }

    /// A room message that names nobody is the one a layer may stay quiet about, and an absent
    /// `push` is the bridge's own decision rather than a widening.
    #[test]
    fn a_passing_message_can_be_dropped_and_an_absent_push_follows_the_bridge() {
        let quiet = Out {
            push: Some(false),
            text: None,
        }
        .decide(&room("passing"))
        .expect("a passing message");
        assert!(!quiet.push && quiet.text.is_empty());

        let reworded = Out {
            push: None,
            text: Some("[quietly] the gate is red".to_string()),
        };
        assert!(
            !reworded.decide(&room("passing")).expect("reworded").push,
            "a reword does not push what the bridge was not going to push"
        );
        let kept = reworded.decide(&room("addressed")).expect("addressed");
        assert!(kept.push);
        assert_eq!(kept.text, "[quietly] the gate is red");
    }

    /// An answer that says nothing is not one - and the bridge's own decision is what stands.
    #[test]
    fn an_answer_with_neither_field_is_not_an_answer() {
        for empty in [
            Out::default(),
            Out {
                push: None,
                text: Some("   ".into()),
            },
        ] {
            assert!(empty.decide(&room("addressed")).is_err());
        }
        // The bridge's own answer, for the caller that has to fall back.
        let fallback = Decision::native(&room("passing"));
        assert!(!fallback.push);
        assert!(fallback.text.starts_with("#12 [host]"));
    }

    /// The wire is the two fields the doc names, and nothing else has to be there.
    #[test]
    fn an_answer_is_read_off_the_wire() {
        assert_eq!(
            read(&json!({"push": false})).expect("an answer"),
            Out {
                push: Some(false),
                text: None
            }
        );
        assert_eq!(
            read(&json!({"push": true, "text": "hi"})).expect("an answer"),
            Out {
                push: Some(true),
                text: Some("hi".into())
            }
        );
        assert!(read(&json!("nope")).is_err());
    }

    /// The synthetic input is the shape a layer will meet: a room event, and an event word a rule
    /// can match on.
    #[test]
    fn the_synthetic_input_is_a_room_message_that_names_the_seat() {
        let input = Input::synthetic();
        assert!(ROOM_EVENTS.contains(&input.event.as_str()));
        assert!(input.room.is_some());
        assert!(input.native.push);
        assert!(!input.must_say());
    }
}
