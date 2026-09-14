//! `flowy` — the one tool the model drives the connector with.
//!
//! One tool taking an `action`, for the reason `monitor` is one tool: the tool
//! ceiling is real, and none of these is a different question — they all act on
//! this session's standing in the fabric. What is NOT here, on purpose:
//!
//! - **declaring, renewing or retiring the listener.** The monitor is declared
//!   by the daemon when the session opens and renewed by the seat's loop. The
//!   operator's words: *"I don't want to fiddle with monitors, nor timers which
//!   pollute the context."* The model reads messages; it does not arm a bell.
//! - **polling.** There is no `wait` action. A message arrives as a firing; a
//!   tool that blocked on the room would be the exit-per-message shape with the
//!   turn held hostage.
//! - **claiming rows.** `flowy todo claim` is a door with `--expect`, and the seat
//!   brief says rows are taken through it, not announced in the room. That door
//!   is a separate verb with its own refusal, and it is not this one.
//!
//! The actions: `status`, `attention`, `subscribe`, `unsubscribe`, `say`, `dm`,
//! `read`, `replay`.

use std::sync::Arc;

use letibot_tools::runtime::{Invocation, InvokeCtx, Tool};
use letibot_tools::schema::{Access, ToolSchema};
use serde_json::Value;

use crate::attention::Level;
use crate::inbox::InboxCondition;
use crate::render;
use crate::seat::Seat;
use crate::subs::Subscription;

/// A session's attachment to a seat: the seat, and this session's condition
/// on it. Behind a slot, so a session opened before a seat existed gets one
/// when `/flowy login` attaches it — the tool is always a door; whether it is
/// open is state.
pub struct Attachment {
    pub seat: Seat,
    pub cond: Arc<InboxCondition>,
}

pub type SeatSlot = Arc<std::sync::Mutex<Option<Attachment>>>;

pub struct Flowy {
    slot: SeatSlot,
}

impl Flowy {
    /// Attached from the start.
    pub fn new(seat: Seat, cond: Arc<InboxCondition>) -> Flowy {
        Flowy {
            slot: Arc::new(std::sync::Mutex::new(Some(Attachment { seat, cond }))),
        }
    }

    /// A door with nothing behind it yet; the slot is how the daemon fills it.
    pub fn unattached() -> (Flowy, SeatSlot) {
        let slot: SeatSlot = Arc::new(std::sync::Mutex::new(None));
        (Flowy { slot: slot.clone() }, slot)
    }

    pub fn slot(&self) -> SeatSlot {
        self.slot.clone()
    }
}

/// The attached pair, borrowed for one call.
struct SeatView<'a> {
    seat: &'a Seat,
    cond: &'a Arc<InboxCondition>,
}

impl SeatView<'_> {
    fn status(&self) -> Invocation {
        let st = self.seat.state();
        let stats = self.seat.stats();
        let me = self.cond.identity();
        let reader = match self.seat.reader() {
            Ok(Some(r)) => format!("reader `{}` at cursor {}", r.reader, r.cursor),
            Ok(None) => "reader NOT DECLARED on the node".to_string(),
            Err(e) => format!("reader: could not ask ({e})"),
        };
        let mut s = format!(
            "seat `{}` — {}\n  this session's address: {} — others on the fabric reach exactly \
             this session by writing that in a message; sessions on this seat: {}\n  {}\n  \
             node {}\n  identity user {} / agent {}\n  {}\n  \
             polls {}, delivered {}, node-filtered {}, acks {} (failed {}), last poll {}\n  \
             attention: {}\n  pending for this session: {}\n",
            self.seat.name(),
            st.word(),
            self.cond.address(),
            self.seat.attached_names(),
            self.seat.credentials().describe(),
            reader,
            if me.user_id.is_empty() {
                "?"
            } else {
                &me.user_id
            },
            if me.agent_id.is_empty() {
                "?"
            } else {
                &me.agent_id
            },
            self.seat.credentials().addr,
            stats.polls,
            stats.delivered,
            stats.server_skipped,
            stats.acks,
            stats.ack_failures,
            stats.last_poll.as_deref().unwrap_or("never"),
            self.cond.attention().describe(),
            self.cond.pending_count(),
        );
        let subs = self.cond.subscriptions();
        if subs.is_empty() {
            s.push_str("  subscriptions: none\n");
        } else {
            s.push_str("  subscriptions:\n");
            for sub in subs {
                s.push_str(&format!("    - {sub}\n"));
            }
        }
        if !stats.last_node_now.is_empty() {
            s.push_str(&format!(
                "  the node's clock last read {}\n",
                stats.last_node_now
            ));
        }
        Invocation::ok(s)
    }

    fn attention(&self, args: &Value) -> Invocation {
        let level = match args.get("level").and_then(|v| v.as_str()) {
            None => None,
            Some(l) => match Level::parse(l) {
                Some(l) => Some(l),
                None => {
                    return Invocation::failed(
                        format!("`{l}` is not an attention level"),
                        "there are four: `off` (nothing), `mentions` (only what names you), \
                         `addressed` (what names you, plus a person's unaddressed broadcast), \
                         `all` (everything). Nothing was changed.",
                    );
                }
            },
        };
        let room = args
            .get("room")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|r| !r.is_empty());
        let human = args.get("human_broadcast").and_then(|v| v.as_bool());
        let focus = args.get("focus").and_then(|v| v.as_str());
        if level.is_none() && human.is_none() && focus.is_none() {
            return Invocation::ok(format!(
                "attention for this session: {}",
                self.cond.attention().describe()
            ));
        }
        let mut changed = Vec::new();
        let a = self.cond.edit_attention(|a| {
            if let Some(l) = level {
                match room {
                    Some(r) => {
                        a.set_room(r.trim_start_matches('#'), l);
                        changed.push(format!("#{} → {}", r.trim_start_matches('#'), l.as_str()));
                    }
                    None => {
                        a.default = l;
                        changed.push(format!("default → {}", l.as_str()));
                    }
                }
            }
            if let Some(h) = human {
                a.wake_on_human_broadcast = h;
                changed.push(format!(
                    "a person's unaddressed broadcast {}",
                    if h { "wakes you" } else { "does not wake you" }
                ));
            }
            if let Some(f) = focus {
                let f = f.trim();
                a.focus = if f.is_empty() || f == "*" {
                    None
                } else {
                    Some(f.to_string())
                };
                changed.push(match &a.focus {
                    Some(f) => format!("focus → {f}"),
                    None => "focus → every project the token reaches".into(),
                });
            }
        });
        Invocation::ok(format!(
            "changed: {}.\nattention is now: {}\nThe seat polls the node at the loosest \
             level any session wants ({}); the table narrows per room after delivery, and \
             the count of what it filtered rides with every firing.",
            changed.join("; "),
            a.describe(),
            a.loosest().as_str()
        ))
    }

    fn subscribe(&self, args: &Value, on: bool) -> Invocation {
        let kind = args.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        let id = args.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let Some(sub) = Subscription::parse(kind, id) else {
            return Invocation::failed(
                "subscribe needs a `kind` and an `id`",
                "`kind` is one of `thread` (a message id whose replies you want, whatever \
                 the room's level), `todo` (a row: its notes and moves), `artifact` (any \
                 row — a diagram, a memory — re-read every 30 s and reported when it \
                 changes). `id` is the ULID. Nothing was changed.",
            );
        };
        if !on {
            return if self.seat.unsubscribe(&self.cond, &sub) {
                Invocation::ok(format!("no longer watching {sub}"))
            } else {
                Invocation::ok(format!(
                    "{sub} was not being watched by this session; nothing changed"
                ))
            };
        }
        match self.seat.subscribe(&self.cond, sub.clone()) {
            Ok(summary) => Invocation::ok(format!(
                "{summary}.\nA change arrives as a firing of the `flowy` monitor, as an \
                 envelope — that something happened, with what the node said about it — and \
                 you re-read the row for the rest."
            )),
            Err(e) => Invocation::failed(
                format!("could not watch {sub}"),
                format!(
                    "{e}. Nothing is watched: a subscription to a row that cannot be read \
                         would be a watch that fires never."
                ),
            ),
        }
    }

    fn say(&self, args: &Value, direct: bool) -> Invocation {
        let body = args
            .get("body")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if body.is_empty() {
            return Invocation::failed("say needs a `body`", "nothing was sent.");
        }
        let to = args
            .get("to")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|t| !t.is_empty());
        let thread = args
            .get("thread")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|t| !t.is_empty());
        let lines = body.lines().count();
        let result = if direct {
            let Some(to) = to else {
                return Invocation::failed("dm needs a `to`", "nothing was sent.");
            };
            self.seat.dm(to, body, thread)
        } else {
            let room = args
                .get("room")
                .and_then(|v| v.as_str())
                .map(|r| r.trim().trim_start_matches('#'))
                .filter(|r| !r.is_empty())
                .unwrap_or("general");
            self.seat.say(room, body, to, thread)
        };
        match result {
            Ok(e) => {
                let mut inv = Invocation::ok(format!(
                    "sent as `{}`: id {}{}{}",
                    self.seat.name(),
                    e.id,
                    if e.thread.is_empty() {
                        String::new()
                    } else {
                        format!(", thread {}", e.thread)
                    },
                    if e.room.is_empty() {
                        String::new()
                    } else {
                        format!(", #{}", e.room)
                    },
                ));
                if lines > 6 {
                    inv = inv.with_note(format!(
                        "{lines} lines. Three is a message; ten is a report, and a report \
                         belongs in a filed row where somebody can choose to read it."
                    ));
                }
                inv
            }
            Err(e) => Invocation::failed("not sent", e.to_string()),
        }
    }

    fn read(&self, args: &Value) -> Invocation {
        let room = args
            .get("room")
            .and_then(|v| v.as_str())
            .map(|r| r.trim().trim_start_matches('#'))
            .filter(|r| !r.is_empty())
            .unwrap_or("general");
        let last = args
            .get("last")
            .and_then(|v| v.as_u64())
            .unwrap_or(20)
            .clamp(1, 200) as usize;
        match self.seat.room_read(room, last) {
            Ok(events) => {
                let me = self.cond.identity();
                let mut s = format!("last {} in #{room}, oldest first:\n", events.len());
                for e in &events {
                    s.push_str(&render::message(e, &me));
                }
                Invocation::ok(s)
            }
            Err(e) => Invocation::failed(format!("could not read #{room}"), e.to_string()),
        }
    }

    fn replay(&self, args: &Value) -> Invocation {
        let last = args
            .get("last")
            .and_then(|v| v.as_u64())
            .unwrap_or(20)
            .clamp(1, 500) as usize;
        match self.seat.spool().replay(last) {
            Ok((events, bad)) => {
                let me = self.cond.identity();
                let mut s = format!(
                    "last {} spooled deliveries for `{}` (from {}), oldest first — every \
                     one of these was acked on the node:\n",
                    events.len(),
                    self.seat.name(),
                    self.seat.spool().path().display()
                );
                for e in &events {
                    s.push_str(&render::message(e, &me));
                }
                if bad > 0 {
                    s.push_str(&format!(
                        "({bad} line(s) in the spool did not parse and were skipped)\n"
                    ));
                }
                Invocation::ok(s)
            }
            Err(e) => Invocation::failed("could not read the spool", e.to_string()),
        }
    }
}

impl Tool for Flowy {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "flowy",
            "Your seat on the flowy fabric: the room, the people and agents in it, \
             and the rows on the board. Messages for you ARRIVE ON THEIR OWN as \
             firings of the `flowy` monitor — never poll for them. `action`: \
             `status` (the seat, its listener, your attention table and \
             subscriptions); `attention` (set what wakes you: `level` per `room` or \
             as the default — `off`, `mentions`, `addressed`, `all` — and \
             `human_broadcast`); `subscribe`/`unsubscribe` (`kind` `thread`, `todo` \
             or `artifact`, plus `id`: watch a conversation, a row's notes and moves, \
             or any row for a change); `say` (`body` into `room`, default general; \
             `to` names a seat or person, or one session of a seat as `seat/session` — \
             how agents on the same project address each other; `thread` replies \
             under a message id); \
             `dm` (`to`, `body`); `read` (the `last` N messages of `room`, for the \
             antecedents of a mention); `replay` (the `last` N spooled deliveries, \
             for what arrived while you could not read). Chat is caveman: a \
             measurement and a decision, three lines. Reasoning goes in a row.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "description": "`status`, `attention`, `subscribe`, `unsubscribe`, `say`, `dm`, `read`, `replay`."},
                    "room": {"type": "string", "description": "A room name, without the `#`. For `attention`: which room the level is for (omit to set the default). For `say`/`read`: which room; default `general`."},
                    "level": {"type": "string", "description": "For `attention`: `off`, `mentions`, `addressed` or `all`."},
                    "human_broadcast": {"type": "boolean", "description": "For `attention`: inside `addressed`, whether a person's message that names nobody wakes you. Default true."},
                    "focus": {"type": "string", "description": "For `attention`: the project everything is delivered from; elsewhere only what names you. `*` for every project the token reaches."},
                    "kind": {"type": "string", "description": "For `subscribe`/`unsubscribe`: `thread`, `todo` or `artifact`."},
                    "id": {"type": "string", "description": "For `subscribe`/`unsubscribe`: the message id (thread) or row id (todo, artifact)."},
                    "body": {"type": "string", "description": "For `say`/`dm`: the text. Prose; backticks are safe here."},
                    "to": {"type": "string", "description": "For `say`: address it to a seat or person by name — or to ONE session of a seat as `seat/session` (the session's id or title; `flowy status` shows this session's own address), which reaches exactly that agent and nobody else on that seat. For `dm`: required."},
                    "thread": {"type": "string", "description": "For `say`/`dm`: reply under this message id."},
                    "last": {"type": "integer", "description": "For `read`/`replay`: how many, default 20."}
                },
                "required": ["action"]
            }),
            Access::Network,
        )
    }

    fn invoke(&self, _ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("status");
        let guard = self.slot.lock().unwrap_or_else(|e| e.into_inner());
        let Some(att) = guard.as_ref() else {
            return Invocation::not_run(
                "this daemon holds no flowy seat",
                "nothing on the fabric is reachable from here: no room is heard, nothing can be \
                 said, the shelf is not readable. The operator attaches a seat from the head \
                 with `/flowy login` (or starts the daemon with --flowy); `/flowy status` says \
                 what is there. This is not a claim that the fabric is empty.",
            );
        };
        let view = SeatView {
            seat: &att.seat,
            cond: &att.cond,
        };
        match action {
            "status" => view.status(),
            "attention" => view.attention(args),
            "subscribe" | "watch" => view.subscribe(args, true),
            "unsubscribe" | "unwatch" => view.subscribe(args, false),
            "say" => view.say(args, false),
            "dm" => view.say(args, true),
            "read" => view.read(args),
            "replay" => view.replay(args),
            other => Invocation::failed(
                format!("`{other}` is not a flowy action"),
                "there are eight: `status`, `attention`, `subscribe`, `unsubscribe`, `say`, \
                 `dm`, `read`, `replay`. There is no `wait` — messages arrive as firings of \
                 the `flowy` monitor — and no `claim`: rows are taken through `flowy todo \
                 claim --expect`, which is a door with its own refusal. Nothing was done.",
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tool_has_no_wait_and_no_pattern() {
        let names = Flowy::unattached().0.schema().param_names().join(",");
        for banned in ["wait", "pattern", "match", "cmdline", "command", "poll"] {
            assert!(
                !names.split(',').any(|n| n == banned),
                "{banned} in {names}"
            );
        }
    }
}
