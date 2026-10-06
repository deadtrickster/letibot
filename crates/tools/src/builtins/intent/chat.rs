//! `say` — the fleet chat verb, so a plan can be discussed while it is still a plan
//! (D9).
//!
//! The other half of D9. Plan mode is *no writes to the work*, and the two things a
//! plan is are **writing the plan** ([`super::write_plan`]) and **talking about it**.
//! A planner that can neither record nor discuss its plan is a planner that has to
//! finish alone and hand over prose.
//!
//! # This is not how you talk to the person driving the session
//!
//! Two different acts, deliberately two different tools:
//!
//! | | reaches | blocks | refuses with |
//! |---|---|---|---|
//! | [`super::ask::AskUserQuestion`] | the person at the head of *this* session | yes — it waits for an answer | `not_run`, nobody was asked |
//! | [`Say`] | a room on the fabric, read by other seats and by the operator later | no — a message is not a question | `not_run`, no fabric is attached |
//!
//! Conflating them is how a model ends up posting a question into a room and then
//! treating the absence of an immediate reply as an answer. `say` returns when the
//! node has taken the message; it never returns an answer, so there is no answer to
//! misread.
//!
//! # A mount, as everywhere else here
//!
//! Not attached is the default and it is a configuration, not a fault — the same
//! rule [`super::queue`] states at length. The local half of this crate needs no
//! fabric; `say` is the one verb that has nothing to fall back to, so unattached it
//! is `not_run` naming what is missing and never a message written to a local file
//! that nobody reads.
//!
//! # A room belongs to a project, and the speaker is not the model's to choose
//!
//! Measured on this fleet: a message addressed to a seat by name, sent in the wrong
//! project's `#general`, was **never delivered**. So the room matters and a default
//! room is a message into the void. And [`Chat::identity`] is read from the seam,
//! never from an argument: a tool that let the model name the speaker is a tool that
//! can speak under another seat's name, which is the one thing a shared fabric
//! cannot forgive.

use letibot_transcript::ToolOutcome;
use serde_json::Value;

use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

/// One message, as it goes out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub room: String,
    /// A seat to address it to, when it is for somebody in particular. `None` is a
    /// message to the room.
    pub to: Option<String>,
    pub text: String,
}

/// What came back: the node took it, and this is what it took.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Posted {
    /// Which room it actually landed in, as the node names it.
    pub room: String,
    /// The seat it went out as. Read from the fabric, never from the call.
    pub by: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatError {
    /// No fabric is attached. **A configuration, not a fault** — and not a message
    /// written somewhere else either.
    NotAttached(String),
    /// Attached and unreachable. Nothing was said.
    Unreachable(String),
    /// The node has no such room, or this token cannot write to it.
    NoSuchRoom {
        room: String,
        known: Vec<String>,
    },
    Refused(String),
}

impl ChatError {
    /// The two that mean *nothing was said and nobody decided* are `NotRun`.
    pub fn outcome(&self) -> ToolOutcome {
        match self {
            ChatError::NotAttached(w) | ChatError::Unreachable(w) => {
                ToolOutcome::NotRun { why: w.clone() }
            }
            other => ToolOutcome::Failed {
                reason: other.to_string(),
            },
        }
    }
}

impl std::fmt::Display for ChatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChatError::NotAttached(w) | ChatError::Unreachable(w) => write!(f, "{w}"),
            ChatError::NoSuchRoom { room, known } => {
                write!(
                    f,
                    "there is no room `{room}` this session can write to, and nothing was \
                     said. A room belongs to a project, so a name that exists in another \
                     project is not this one — measured on this fleet: a message \
                     addressed to a seat by name, in the wrong project's room, was never \
                     delivered and looked exactly like a quiet room."
                )?;
                if !known.is_empty() {
                    write!(f, " This session can write to: {}", known.join(", "))?;
                }
                Ok(())
            }
            ChatError::Refused(w) => write!(f, "the fabric refused, and nothing was said: {w}"),
        }
    }
}

/// The seam to the fabric's rooms.
///
/// As with [`super::queue::Queue`], there is **no client here**: this crate has no
/// HTTP client, no exec path and no async runtime. The implementation that shells
/// out to `flowy say` or speaks to the node belongs to the daemon.
pub trait Chat: Send + Sync {
    /// The handle the fabric knows this seat by. Never an argument.
    fn identity(&self) -> String;

    /// The rooms this session can write to, for a miss report.
    fn rooms(&self) -> Result<Vec<String>, ChatError>;

    fn say(&self, msg: &Message) -> Result<Posted, ChatError>;

    fn describe(&self) -> String;
}

/// **The default, and it is not an error state.**
#[derive(Debug, Default, Clone, Copy)]
pub struct NoFabric;

impl NoFabric {
    fn refuse<T>(&self) -> Result<T, ChatError> {
        Err(ChatError::NotAttached(
            "no fabric is attached to this session, so NOTHING was said and no other \
             seat heard anything. That is a configuration and not a fault — but it is \
             also not a message stored somewhere for later: there is no message. If \
             this plan needs another seat's input, say so in the plan document and \
             stop, or ask the person driving this session with `ask_user_question`."
                .into(),
        ))
    }
}

impl Chat for NoFabric {
    fn identity(&self) -> String {
        // Never a guess. A session with no fabric has no fabric identity, and
        // inventing one is how a message goes out under the wrong name.
        String::new()
    }

    fn rooms(&self) -> Result<Vec<String>, ChatError> {
        self.refuse()
    }

    fn say(&self, _msg: &Message) -> Result<Posted, ChatError> {
        self.refuse()
    }

    fn describe(&self) -> String {
        "not attached — `say` refuses with not_run and nothing is queued".into()
    }
}

/// The most bytes one message may be.
///
/// The fleet's own rule for a room, and it is not arbitrary: *"Three lines is
/// normal. Ten is a report, and a report belongs in a filed row."* A cap is the
/// only version of that rule that binds, since a prompt is a prior and not a
/// constraint.
const MAX_MESSAGE_BYTES: usize = 1200;

pub struct Say {
    pub chat: std::sync::Arc<dyn Chat>,
}

impl Say {
    pub fn new(chat: std::sync::Arc<dyn Chat>) -> Self {
        Say { chat }
    }
}

impl Tool for Say {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "say",
            "Put one message in a room on the fabric, where other agents and the \
             operator read it. Give `room` and `text`; optionally `to` to address a \
             particular seat. It returns when the node has taken the message — it \
             never returns a reply, so silence afterwards is silence and not an \
             answer. This is not how you ask the person driving this session \
             something: that is `ask_user_question`. Keep it to a measurement and a \
             decision; a report belongs in a filed item.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "room": {"type": "string", "description": "Which room. A room belongs to a project, so a name from elsewhere is not this one; there is no default."},
                    "text": {"type": "string", "description": "The message. A few lines."},
                    "to": {"type": "string", "description": "A seat to address it to, when it is for somebody in particular."}
                },
                "required": ["room", "text"]
            }),
            // It leaves the box.
            Access::Network,
        )
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let room = args.get("room").and_then(|v| v.as_str()).unwrap_or("");
        let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
        if room.trim().is_empty() || text.trim().is_empty() {
            let missing = if room.trim().is_empty() {
                "room"
            } else {
                "text"
            };
            let rooms = self.chat.rooms().unwrap_or_default();
            let mut body = format!(
                "call `say` again with `{missing}` set. There is deliberately no default \
                 room: a room belongs to a project, and a message sent to the wrong \
                 project's room by the right name is never delivered."
            );
            if !rooms.is_empty() {
                body.push_str(&format!(
                    "\nthis session can write to: {}",
                    rooms.join(", ")
                ));
            }
            return Invocation::failed(format!("say needs `{missing}`"), body);
        }
        if text.len() > MAX_MESSAGE_BYTES {
            return Invocation::failed(
                format!("this message is {} bytes", text.len()),
                format!(
                    "a room message is capped at {MAX_MESSAGE_BYTES} bytes and NOTHING was \
                     said. Send the measurement and the decision; put the reasoning in a \
                     filed item or a plan document, where somebody can choose to read it."
                ),
            );
        }

        let msg = Message {
            room: room.trim().to_string(),
            to: args
                .get("to")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            text: text.to_string(),
        };
        ctx.progress(format!("saying it in {}", msg.room));

        match self.chat.say(&msg) {
            Ok(p) => Invocation::ok(format!(
                "said in {} as {}{}",
                p.room,
                if p.by.is_empty() {
                    "an unnamed seat"
                } else {
                    &p.by
                },
                msg.to
                    .as_deref()
                    .map(|t| format!(", addressed to {t}"))
                    .unwrap_or_default()
            ))
            .with_note(
                "the node took the message. That is not a reply and nobody has \
                 necessarily read it — do not treat what happens next as an answer."
                    .to_string(),
            ),
            Err(e) => Invocation {
                outcome: e.outcome(),
                payload: e.to_string(),
                notes: vec![],
                edit: None,
                needs_in_view: Vec::new(),
                media: None,
            },
        }
    }
}

#[cfg(any(test, feature = "testing"))]
pub use fake::FakeChat;

/// An in-memory fabric, for tests. **No test in this repository says anything in a
/// real room** — other seats read those.
#[cfg(any(test, feature = "testing"))]
mod fake {
    use super::*;
    use std::sync::Mutex;

    pub struct FakeChat {
        pub me: String,
        pub rooms: Vec<String>,
        pub offline: bool,
        pub said: Mutex<Vec<Message>>,
    }

    impl FakeChat {
        pub fn new(me: &str, rooms: &[&str]) -> Self {
            FakeChat {
                me: me.to_string(),
                rooms: rooms.iter().map(|r| r.to_string()).collect(),
                offline: false,
                said: Mutex::new(Vec::new()),
            }
        }

        pub fn offline(me: &str) -> Self {
            FakeChat {
                offline: true,
                ..FakeChat::new(me, &["general"])
            }
        }
    }

    impl Chat for FakeChat {
        fn identity(&self) -> String {
            self.me.clone()
        }

        fn rooms(&self) -> Result<Vec<String>, ChatError> {
            if self.offline {
                return Err(ChatError::Unreachable("the node did not answer".into()));
            }
            Ok(self.rooms.clone())
        }

        fn say(&self, msg: &Message) -> Result<Posted, ChatError> {
            if self.offline {
                return Err(ChatError::Unreachable(
                    "the fabric node did not answer, so NOTHING was said and no other \
                     seat heard anything. The message was not queued anywhere."
                        .into(),
                ));
            }
            if !self.rooms.contains(&msg.room) {
                return Err(ChatError::NoSuchRoom {
                    room: msg.room.clone(),
                    known: self.rooms.clone(),
                });
            }
            self.said.lock().unwrap().push(msg.clone());
            Ok(Posted {
                room: msg.room.clone(),
                by: self.me.clone(),
            })
        }

        fn describe(&self) -> String {
            format!("fake fabric (in memory), as `{}`", self.me)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn say(chat: Arc<dyn Chat>, args: &str) -> crate::result::ToolResult {
        let mut h = crate::testing::harness();
        h.rt.registry.register(Box::new(Say::new(chat))).unwrap();
        h.rt = h.rt.with_gate(crate::testing::allow_all());
        h.call("say", args)
    }

    #[test]
    fn no_fabric_is_not_run_and_nothing_is_queued() {
        let r = say(
            Arc::new(NoFabric),
            r#"{"room":"general","text":"gating X"}"#,
        );
        assert!(matches!(r.outcome, ToolOutcome::NotRun { .. }), "{r:?}");
        let out = r.render();
        assert!(out.contains("NOTHING was said"), "{out}");
        assert!(out.contains("configuration and not a fault"), "{out}");
        assert!(out.contains("there is no message"), "{out}");
    }

    #[test]
    fn an_unreachable_node_says_nothing_was_said() {
        let r = say(
            Arc::new(FakeChat::offline("claude-lab2x1")),
            r#"{"room":"general","text":"x"}"#,
        );
        assert!(matches!(r.outcome, ToolOutcome::NotRun { .. }));
        assert!(r.render().contains("not queued anywhere"), "{}", r.render());
    }

    #[test]
    fn there_is_no_default_room() {
        let r = say(
            Arc::new(FakeChat::new("claude-lab2x1", &["general", "lab"])),
            r#"{"text":"x"}"#,
        );
        assert!(matches!(r.outcome, ToolOutcome::Failed { .. }));
        let out = r.render();
        assert!(out.contains("no default room"), "{out}");
        assert!(out.contains("general") && out.contains("lab"), "{out}");
    }

    #[test]
    fn a_room_in_another_project_is_named_rather_than_guessed_at() {
        let r = say(
            Arc::new(FakeChat::new("claude-lab2x1", &["general"])),
            r#"{"room":"flowy-general","text":"x"}"#,
        );
        assert!(matches!(r.outcome, ToolOutcome::Failed { .. }));
        assert!(
            r.render().contains("belongs to a project"),
            "{}",
            r.render()
        );
    }

    #[test]
    fn the_speaker_comes_from_the_fabric_and_not_from_the_call() {
        let chat = Arc::new(FakeChat::new("claude-lab2x1", &["general"]));
        // There is no `as` or `from` argument to try; the schema has three
        // properties and none of them names a speaker.
        let s = Say::new(chat.clone()).schema();
        let props = s.param_names();
        assert!(
            !props
                .iter()
                .any(|p| ["as", "from", "agent", "by"].contains(p)),
            "{props:?}"
        );
        let r = say(chat, r#"{"room":"general","text":"gating X"}"#);
        assert_eq!(r.outcome, ToolOutcome::Ok);
        assert!(r.render().contains("as claude-lab2x1"), "{}", r.render());
    }

    #[test]
    fn a_message_over_the_cap_says_nothing() {
        let chat = Arc::new(FakeChat::new("me", &["general"]));
        let big = "x".repeat(MAX_MESSAGE_BYTES + 1);
        let args = serde_json::json!({"room": "general", "text": big}).to_string();
        let r = say(chat.clone(), &args);
        assert!(matches!(r.outcome, ToolOutcome::Failed { .. }));
        assert!(r.render().contains("NOTHING was"), "{}", r.render());
    }

    #[test]
    fn a_posted_message_is_never_reported_as_a_reply() {
        let r = say(
            Arc::new(FakeChat::new("me", &["general"])),
            r#"{"room":"general","text":"x","to":"claude-host-lab"}"#,
        );
        assert_eq!(r.outcome, ToolOutcome::Ok);
        let out = r.render();
        assert!(out.contains("addressed to claude-host-lab"), "{out}");
        assert!(out.contains("That is not a reply"), "{out}");
    }

    #[test]
    fn the_description_lints_clean() {
        let s = Say::new(Arc::new(NoFabric)).schema();
        assert_eq!(crate::schema::lint_description(&s.description), vec![]);
        assert!(s.description.len() < 800, "{}", s.description.len());
    }
}
