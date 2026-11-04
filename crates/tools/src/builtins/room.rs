//! `room` is the host's tagged queues, seen from a top-level session: one tool and a `verb`.
//! The socket behind it is the daemon's ([`RoomLink`]), so a subagent is refused at the door.
//! The daemon beats while a turn runs, and [`RoomLink::beats_on_my_behalf`] says which.

use std::sync::Arc;

use serde_json::{Value, json};

use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

/// The tool's name.
pub const TOOL: &str = "room";

/// What the daemon's room connection can do for this session: one verb with its arguments, and
/// the sentence the model reads back. A refusal is an `Err`, never a silence.
pub trait RoomLink: Send + Sync {
    fn call(&self, verb: &str, args: &Value) -> Result<String, String>;

    /// Whether this session's daemon beats while its turn runs, which decides the paragraph
    /// [`RoomTool::schema`] composes: a model that believes it must beat will spend a call on it.
    fn beats_on_my_behalf(&self) -> bool {
        true
    }
}

/// No room behind this tool - a registry built without a daemon, and the sentence it says.
pub struct NoRoom;

impl RoomLink for NoRoom {
    fn call(&self, _verb: &str, _args: &Value) -> Result<String, String> {
        Err(
            "this session has no room: the box's coordinator is a daemon of its own (`harnessd \
             --room`, which `letibot --room` starts on this box), and no socket was there when this \
             session opened, so there is nothing to post to, claim, or hold. A session opened after \
             the coordinator starts has one"
                .to_string(),
        )
    }

    fn beats_on_my_behalf(&self) -> bool {
        false
    }
}

/// The `room` tool.
pub struct RoomTool {
    link: Arc<dyn RoomLink>,
}

impl RoomTool {
    pub fn new(link: Arc<dyn RoomLink>) -> Self {
        Self { link }
    }
}

impl Tool for RoomTool {
    fn schema(&self) -> ToolSchema {
        // The beat paragraph comes from the link, not from here: the daemon beats while the turn
        // runs, or nothing does (a `NoRoom`) and a model told the wrong one loses its claim.
        let beat = if self.link.beats_on_my_behalf() {
            "Your daemon beats for you while your turn runs, so a claim is not lost while you work; \
             one whose beat stops is taken back."
        } else {
            "Nothing beats on your behalf in this session — there is no room behind this tool — so \
             a claim is kept alive only by the `beat` calls you make yourself."
        };
        ToolSchema::new(
            TOOL,
            format!(
                "The host's room: a queue per TAG shared by every top-level session on this machine, \
                 for coordinating scarce things (RAM, a port, the staging database). A tag is a LOCK — one live claim per tag, and anybody \
                 else is refused with the holder's name — so two jobs that may run at once need two \
                 tags, and urgent work gets its own tag rather than a priority number. `post` a \
                 need, `claim` a tag's head, `beat` what you hold, `waiting` what you are waiting \
                 on, `done`, `abandon`, `dispute`, `queue`, `holding`, `questions`, and `rule` — the \
                 arbiter's verb — to answer with `retry`, `hand` or `drop`. `say` and `read` talk to the \
                 operator and other sessions in room `project` or `host`. {beat}"
            ),
            json!({
                "type": "object",
                "properties": {
                    "verb": {
                        "type": "string",
                        "enum": [
                            "post", "claim", "claim_item", "beat", "waiting", "done", "abandon",
                            "dispute", "rule", "queue", "holding", "reminders", "questions",
                            "say", "read"
                        ],
                        "description": "What to do. `post` needs `tag` and `body`; `claim` needs `tag`; `claim_item`, `beat`, `done` need `item`; `waiting` needs `item` and `text`; `abandon` needs `item` and `why`; `dispute` needs `item` and `says`; `rule` needs `question` and `decision`; `queue` needs `tag`; `holding` takes an optional `session`; `reminders` and `questions` take nothing; `say` needs `text` and takes `room`, `to`, `thread`, `todo`, `queue`; `read` takes `room`, `since`, `last`, `thread`."
                    },
                    "tag": {
                        "type": "string",
                        "description": "The queue's name — the scarce thing, not a category. Lowercased and trimmed by the room, so `RAM` and `ram` are one lock."
                    },
                    "body": {
                        "type": "string",
                        "description": "What you need, in your own words: `8 GB, ~10 min`, `the staging database`, `a reviewer for #412`. The room never parses it."
                    },
                    "item": {
                        "type": "integer",
                        "description": "The item's number, as a reply gave it (`#7`)."
                    },
                    "text": {
                        "type": "string",
                        "description": "For `waiting`: what you are waiting on. Empty clears it."
                    },
                    "why": {
                        "type": "string",
                        "description": "For `abandon`: why you are giving it up. It goes on the item and the arbiter reads it."
                    },
                    "says": {
                        "type": "string",
                        "description": "For `dispute`: why you still have it, in your own words."
                    },
                    "question": {
                        "type": "integer",
                        "description": "For `rule`: the question's number, from `questions` or from the prompt."
                    },
                    "decision": {
                        "type": "string",
                        "enum": ["retry", "hand", "drop"],
                        "description": "For `rule`: `retry` offers the item again with its count kept, `hand` gives the claim to `to` now, `drop` takes it out of the queue for good."
                    },
                    "to": {
                        "type": "string",
                        "description": "For `rule` with `decision: \"hand\"`: the session that gets the claim. It must be a top-level session the room has met."
                    },
                    "room": {
                        "type": "string",
                        "description": "For `say` and `read`: `project` (the default) is the room of your project, `host` is the room every session on this machine is in, `dm` (read only) is your direct messages."
                    },
                    "since": {
                        "type": "integer",
                        "description": "For `read`: only messages after this number."
                    },
                    "last": {
                        "type": "integer",
                        "description": "For `read`: how many of the newest messages, default 20."
                    },
                    "thread": {
                        "type": "integer",
                        "description": "For `say`: the number of the message you answer. For `read`: that message and its replies."
                    },
                    "todo": {
                        "type": "string",
                        "description": "For `say`: a todo row id this message is about."
                    },
                    "queue": {
                        "type": "string",
                        "description": "For `say`: a merge-queue row id this message is about."
                    },
                    "session": {
                        "type": "string",
                        "description": "For `holding`: whose claims to read. Omit for your own."
                    }
                },
                "required": ["verb"]
            }),
            // It changes the host's shared state - who holds what - and nothing on the operator's
            // disk, no process and no network. `Session` is the class for that, and it is
            // unattended: a card per room verb would be a room nobody uses.
            Access::Session,
        )
    }

    fn invoke(&self, _ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        match dispatch(self.link.as_ref(), args) {
            Ok(said) => Invocation::ok(said),
            Err(failure) => failure,
        }
    }
}

/// The tool's whole decision with no context, which is what the tests drive: the rule that a
/// verb without its field never reaches the room cannot be asserted through an `InvokeCtx`.
#[allow(clippy::result_large_err)] // Invocation is the failed row the caller shows; boxing it reshapes the tool API
fn dispatch(link: &dyn RoomLink, args: &Value) -> Result<String, Invocation> {
    let verb = args
        .get("verb")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if verb.is_empty() {
        return Err(Invocation::failed(
            "no verb",
            "the `room` tool needs a `verb`: post, claim, beat, waiting, done, abandon, queue \
             or holding (see the schema).",
        ));
    }
    // The required field of each verb, checked here so a model that forgot one is told
    // which, in the room's own words, rather than by a refusal from three layers down.
    let missing = match verb.as_str() {
        "post" if !nonempty(args, "tag") => Some("tag"),
        "post" if !nonempty(args, "body") => Some("body"),
        "claim" | "queue" if !nonempty(args, "tag") => Some("tag"),
        "claim_item" | "beat" | "done" => {
            if item_of(args).is_none() {
                Some("item")
            } else {
                None
            }
        }
        "waiting" => {
            if item_of(args).is_none() {
                Some("item")
            } else {
                None
            }
        }
        "abandon" => {
            if item_of(args).is_none() {
                Some("item")
            } else if !nonempty(args, "why") {
                Some("why")
            } else {
                None
            }
        }
        "dispute" => {
            if item_of(args).is_none() {
                Some("item")
            } else if !nonempty(args, "says") {
                Some("says")
            } else {
                None
            }
        }
        "say" if !nonempty(args, "text") => Some("text"),
        "rule" => {
            if args.get("question").and_then(|v| v.as_u64()).is_none() {
                Some("question")
            } else if !nonempty(args, "decision") {
                Some("decision")
            } else {
                None
            }
        }
        _ => None,
    };
    if let Some(field) = missing {
        return Err(Invocation::failed(
            format!("`{verb}` needs `{field}`"),
            format!(
                "the `room` verb `{verb}` cannot be run without `{field}` — nothing was sent to \
                 the room."
            ),
        ));
    }
    link.call(&verb, args)
        .map_err(|why| Invocation::failed(why.clone(), why))
}

fn nonempty(args: &Value, key: &str) -> bool {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
}

fn item_of(args: &Value) -> Option<u64> {
    args.get("item").and_then(|v| v.as_u64())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A link that records what it was asked and answers in its own words - the tool's contract,
    /// with no daemon and no socket.
    struct Recorder {
        seen: Mutex<Vec<(String, Value)>>,
        answer: Result<String, String>,
    }

    impl Recorder {
        fn new(answer: Result<&str, &str>) -> Arc<Recorder> {
            Arc::new(Recorder {
                seen: Mutex::new(Vec::new()),
                answer: answer.map(|s| s.to_string()).map_err(|s| s.to_string()),
            })
        }
    }

    impl RoomLink for Recorder {
        fn call(&self, verb: &str, args: &Value) -> Result<String, String> {
            self.seen
                .lock()
                .unwrap()
                .push((verb.to_string(), args.clone()));
            self.answer.clone()
        }
    }

    fn invoke(link: Arc<dyn RoomLink>, args: Value) -> (bool, String) {
        match dispatch(link.as_ref(), &args) {
            Ok(said) => (true, said),
            Err(failure) => (
                false,
                match &failure.outcome {
                    letibot_transcript::ToolOutcome::Failed { reason } => {
                        format!("{} — {}", reason, failure.payload)
                    }
                    other => format!("{other:?} — {}", failure.payload),
                },
            ),
        }
    }

    #[test]
    fn the_description_is_clean_and_fits_the_prompt() {
        let tool = RoomTool::new(Arc::new(NoRoom));
        let s = tool.schema();
        assert_eq!(s.name, TOOL);
        assert_eq!(crate::schema::lint_description(&s.description), vec![]);
        assert!(s.description.len() < 900, "{} bytes", s.description.len());
        assert_eq!(s.access, Access::Session);
    }

    /// The beat paragraph is the link's, and the two links say opposite things: the sentence a
    /// model reads is chosen by the answer, so both answers are reachable and pinned here.
    #[test]
    fn the_description_says_whether_the_daemon_beats_on_my_behalf() {
        let none = RoomTool::new(Arc::new(NoRoom)).schema().description;
        assert!(
            none.contains("Nothing beats on your behalf"),
            "a registry with no room must not promise a beat: {none}"
        );
        assert!(!none.contains("Your daemon beats for you"), "{none}");

        let real = RoomTool::new(Recorder::new(Ok("posted #1")) as Arc<dyn RoomLink>)
            .schema()
            .description;
        assert!(
            real.contains("Your daemon beats for you while your turn runs"),
            "a real link is the daemon beating, and the model has to be told: {real}"
        );
        assert!(!real.contains("Nothing beats on your behalf"), "{real}");
    }

    #[test]
    fn a_verb_is_handed_to_the_daemon_with_its_arguments() {
        let link = Recorder::new(Ok("posted #1 under `ram`"));
        let (ok, text) = invoke(
            link.clone(),
            json!({"verb": "post", "tag": "ram", "body": "8 GB"}),
        );
        assert!(ok, "{text}");
        assert_eq!(text, "posted #1 under `ram`");
        let seen = link.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, "post");
        assert_eq!(seen[0].1["tag"], "ram");
    }

    #[test]
    fn a_verb_missing_its_field_is_refused_here_and_nothing_is_sent() {
        for (args, field) in [
            (json!({"verb": "post", "tag": "ram"}), "body"),
            (json!({"verb": "claim"}), "tag"),
            (json!({"verb": "beat"}), "item"),
            (json!({"verb": "abandon", "item": 1}), "why"),
            (json!({"verb": "rule", "question": 1}), "decision"),
            (json!({"verb": ""}), "verb"),
        ] {
            let link = Recorder::new(Ok("never"));
            let (ok, text) = invoke(link.clone(), args);
            assert!(!ok, "a verb without its field must fail: {text}");
            assert!(text.contains(field), "it must name `{field}`: {text}");
            assert!(
                link.seen.lock().unwrap().is_empty(),
                "nothing reaches the room: {text}"
            );
        }
    }

    #[test]
    fn a_refusal_from_the_room_is_a_refusal_and_not_a_failure_of_the_tool() {
        let link = Recorder::new(Err("tag `ram` is held by `s-b` on #2 for 40s"));
        let (ok, text) = invoke(link, json!({"verb": "claim", "tag": "ram"}));
        assert!(!ok);
        assert!(text.contains("held by `s-b`"), "{text}");
    }

    #[test]
    fn a_registry_with_no_room_says_so_in_words() {
        let (ok, text) = invoke(Arc::new(NoRoom), json!({"verb": "claim", "tag": "ram"}));
        assert!(!ok);
        assert!(text.contains("harnessd --room"), "{text}");
        assert!(!NoRoom.beats_on_my_behalf());
    }

    /// A session with no room is told how the box gets one, because the launcher starts a
    /// coordinator when none serves and a refusal that stopped naming it would send the model
    /// looking for a socket it cannot start.
    #[test]
    fn a_session_with_no_room_is_told_which_verb_starts_one() {
        let (ok, text) = invoke(Arc::new(NoRoom), json!({"verb": "queue", "tag": "ram"}));
        assert!(!ok);
        assert!(text.contains("letibot --room"), "{text}");
    }
}
