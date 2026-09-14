//! What a session is woken for: a level per room, a thread override, and the
//! human-broadcast clause as a named switch.
//!
//! # flowy's definitions, kept exactly
//!
//! `wakesFor` (`internal/flowy/inbox.go`) has three levels and each edge was paid
//! for by a delivery that went wrong on the fleet:
//!
//! | level | wakes for |
//! |---|---|
//! | `all` | everything in the room |
//! | `addressed` | what names you — an `@name` in the body arrives as an addressee — **plus a person's unaddressed broadcast**, because *"who is here?"* names nobody and the human's messages were otherwise the least likely in the room to be answered |
//! | `mentions` | only what names you. The person's-broadcast clause is off |
//! | `off` | nothing — a room you are in but asked not to be told about |
//!
//! Your own messages never wake you. A `todo.note` reaches the seat the row was
//! assigned to (or raised by, when unowned) and nobody else — the node has
//! already decided that, and a note arriving is a note for this seat. A direct
//! message has no room and names you: it is addressed.
//!
//! # flowy's shape, dropped
//!
//! The level is one flag per poll on the node, because a reader belongs to a
//! name and a name has one waiter. A client that holds state can do better, and
//! `docs/implementation-plan.md` §12.2b says the harness should: the level is a
//! **table keyed by room**, with a session default, and a **thread** set that
//! overrides the room — *watch this conversation whatever the room's level is*.
//!
//! # The clause that failed in both directions
//!
//! First a person's *"who is here?"* reached nobody, because agents pass
//! unaddressed traffic by habit. Then a person's message addressed to ONE agent
//! woke every `--to-me` waiter in the room, and a seat acted on work meant for
//! someone else. Both fixes are inside `addressed` on the node, and both are
//! kept here — but as [`Attention::wake_on_human_broadcast`], a switch with a
//! name, rather than a property of the word "addressed".

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::client::{Event, ServerFilter};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Off,
    Mentions,
    Addressed,
    All,
}

impl Level {
    pub fn parse(s: &str) -> Option<Level> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" | "mute" | "ignore" => Some(Level::Off),
            "mentions" | "mention" | "named" => Some(Level::Mentions),
            "addressed" | "to-me" | "to_me" | "tome" => Some(Level::Addressed),
            "all" | "everything" => Some(Level::All),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Level::Off => "off",
            Level::Mentions => "mentions",
            Level::Addressed => "addressed",
            Level::All => "all",
        }
    }

    /// The node's two flags for this level. `Off` polls as `mentions` — the node
    /// cannot be asked for nothing, and the mark must still move over the room.
    pub fn server_filter(&self) -> ServerFilter {
        match self {
            Level::All => ServerFilter {
                addressed: false,
                mentions: false,
            },
            Level::Addressed => ServerFilter {
                addressed: true,
                mentions: false,
            },
            Level::Mentions | Level::Off => ServerFilter {
                addressed: false,
                mentions: true,
            },
        }
    }
}

/// Who "you" are, for the addressee and actor tests. Both ids, because a message
/// to the person and a message to the agent working for them both reach the
/// seat — flowy's `isOwnActor` asks the same question of both columns.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub user_id: String,
    pub agent_id: String,
    /// The seat's name — what a note's assignee stamp holds.
    pub name: String,
}

impl Identity {
    pub fn is_me(&self, id: &str) -> bool {
        !id.is_empty() && (id == self.user_id || id == self.agent_id)
    }
}

/// One session's attention. Serialisable, so a session can persist it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attention {
    pub default: Level,
    #[serde(default)]
    pub rooms: BTreeMap<String, Level>,
    /// Threads watched at `all` whatever their room says.
    #[serde(default)]
    pub threads: BTreeSet<String>,
    /// Inside `addressed`: a person's message with no addressee wakes you.
    pub wake_on_human_broadcast: bool,
    /// The project everything is delivered from; elsewhere only what names you.
    /// The seat's home project unless a session says otherwise.
    #[serde(default)]
    pub focus: Option<String>,
}

impl Default for Attention {
    fn default() -> Self {
        Attention {
            default: Level::Addressed,
            rooms: BTreeMap::new(),
            threads: BTreeSet::new(),
            wake_on_human_broadcast: true,
            focus: None,
        }
    }
}

/// Why a message did or did not wake this session. A sentence, because a
/// filter that cannot explain itself is one nobody can tell from a broken one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Wake(&'static str),
    Skip(&'static str),
}

impl Verdict {
    pub fn wakes(&self) -> bool {
        matches!(self, Verdict::Wake(_))
    }

    pub fn why(&self) -> &'static str {
        match self {
            Verdict::Wake(w) | Verdict::Skip(w) => w,
        }
    }
}

impl Attention {
    pub fn level_for(&self, room: &str) -> Level {
        self.rooms.get(room).copied().unwrap_or(self.default)
    }

    pub fn set_room(&mut self, room: &str, level: Level) {
        self.rooms.insert(room.to_string(), level);
    }

    /// The loosest level anywhere in the table: what the seat asks the node for,
    /// so nothing a session wants is filtered before it arrives. A watched thread
    /// needs `all` on the wire — the node cannot filter by thread.
    pub fn loosest(&self) -> Level {
        let mut l = self.default;
        for v in self.rooms.values() {
            l = l.max(*v);
        }
        if !self.threads.is_empty() {
            l = Level::All;
        }
        l
    }

    /// The delivery rule. Port of `wakesFor`, per room, with the switches named.
    pub fn wakes_for(&self, me: &Identity, e: &Event) -> Verdict {
        if me.is_me(&e.actor) {
            return Verdict::Skip("your own message");
        }
        // A note reaches the seat the row was assigned to, and nobody else. The
        // node decided that before delivering; here it is only checked against
        // the seat's name, and a note is never room traffic to be muted.
        if e.kind == "todo.note" {
            let who = e.note_assignee();
            if !who.is_empty() {
                return if who == me.name {
                    Verdict::Wake("a note on a row assigned to you")
                } else {
                    Verdict::Skip("a note on a row assigned to somebody else")
                };
            }
            let raiser = e.note_raiser();
            return if !raiser.is_empty() && raiser == me.name {
                Verdict::Wake("a note on an unowned row you raised")
            } else {
                Verdict::Skip("a note on a row that is neither yours nor raised by you")
            };
        }
        let named = me.is_me(&e.addressee);
        // Outside the focus, only what names you.
        if let Some(focus) = self.focus.as_deref().filter(|f| !f.is_empty()) {
            let inside = e.project.as_deref() == Some(focus);
            if !inside && !named {
                return Verdict::Skip("outside your focus project and not addressed to you");
            }
        }
        // A DM: no room, and it names you.
        if e.private || (e.room.is_empty() && named) {
            return Verdict::Wake("a direct message");
        }
        if self.threads.contains(e.thread_or_self()) {
            return Verdict::Wake("in a thread you are watching");
        }
        match self.level_for(&e.room) {
            Level::Off => Verdict::Skip("the room is off"),
            Level::All => Verdict::Wake("the room is at `all`"),
            Level::Mentions => {
                if named {
                    Verdict::Wake("addressed to you")
                } else {
                    Verdict::Skip("the room is at `mentions` and this does not name you")
                }
            }
            Level::Addressed => {
                if named {
                    return Verdict::Wake("addressed to you");
                }
                // A PERSON'S BROADCAST, and only a broadcast: when the person said
                // who they meant, honouring it is the whole point of `addressed`.
                if self.wake_on_human_broadcast && e.said_by_a_person() && e.addressee.is_empty() {
                    return Verdict::Wake("a person's unaddressed broadcast");
                }
                if e.said_by_a_person() {
                    Verdict::Skip("a person's message addressed to somebody else")
                } else {
                    Verdict::Skip("the room is at `addressed` and this does not name you")
                }
            }
        }
    }

    /// One block for a listing or a disclosure.
    pub fn describe(&self) -> String {
        let mut s = format!(
            "default {}; human broadcast {}",
            self.default.as_str(),
            if self.wake_on_human_broadcast {
                "wakes"
            } else {
                "does not wake"
            }
        );
        if let Some(f) = &self.focus {
            s.push_str(&format!("; focus {f}"));
        }
        for (room, level) in &self.rooms {
            s.push_str(&format!("; #{room} {}", level.as_str()));
        }
        if !self.threads.is_empty() {
            s.push_str(&format!(
                "; watching thread(s) {}",
                self.threads.iter().cloned().collect::<Vec<_>>().join(", ")
            ));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn me() -> Identity {
        Identity {
            user_id: "U1".into(),
            agent_id: "A1".into(),
            name: "claude-lab2x1".into(),
        }
    }

    fn chat(room: &str, actor: &str, kind: &str, to: &str, body: &str) -> Event {
        serde_json::from_value(json!({
            "id": format!("e-{body}"), "type": "chat", "room": room, "actor": actor,
            "project": "Lab", "addressee": to, "body": body,
            "meta": {"actor_kind": kind}
        }))
        .unwrap()
    }

    // The delivery tests flowy paid for, one each, ported to the table.

    #[test]
    fn an_addressed_waiter_still_hears_a_persons_unaddressed_broadcast() {
        let a = Attention::default();
        let e = chat("general", "OP", "user", "", "who is here?");
        assert!(a.wakes_for(&me(), &e).wakes());
    }

    #[test]
    fn a_persons_message_to_one_agent_does_not_wake_the_others() {
        // 2026-08-27: "i think @dead-claude can continue grinding thru todos"
        // was delivered to claude-host, whose listener called itself
        // addressed-only.
        let a = Attention::default();
        let e = chat("general", "OP", "user", "A-other", "@dead-claude continue");
        let v = a.wakes_for(&me(), &e);
        assert!(!v.wakes(), "{v:?}");
        assert_eq!(v.why(), "a person's message addressed to somebody else");
    }

    #[test]
    fn mentions_turns_the_broadcast_clause_off() {
        let mut a = Attention::default();
        a.default = Level::Mentions;
        assert!(
            !a.wakes_for(&me(), &chat("general", "OP", "user", "", "hello all"))
                .wakes()
        );
        assert!(
            a.wakes_for(&me(), &chat("general", "OP", "user", "A1", "you"))
                .wakes()
        );
    }

    #[test]
    fn the_broadcast_clause_is_a_switch_not_a_property_of_addressed() {
        let mut a = Attention::default();
        a.wake_on_human_broadcast = false;
        let e = chat("general", "OP", "user", "", "who is here?");
        assert!(!a.wakes_for(&me(), &e).wakes());
    }

    #[test]
    fn your_own_messages_never_wake_you() {
        let mut a = Attention::default();
        a.default = Level::All;
        assert!(
            !a.wakes_for(&me(), &chat("general", "A1", "agent", "", "me"))
                .wakes()
        );
        assert!(
            !a.wakes_for(&me(), &chat("general", "U1", "user", "", "me"))
                .wakes()
        );
    }

    #[test]
    fn an_agent_addressing_another_agent_matches_only_by_addressee() {
        // Agents address each other by habit; that is why `addressed` is not
        // "from a person".
        let a = Attention::default();
        assert!(
            a.wakes_for(&me(), &chat("general", "A9", "agent", "A1", "to you"))
                .wakes()
        );
        assert!(
            !a.wakes_for(&me(), &chat("general", "A9", "agent", "A7", "to them"))
                .wakes()
        );
    }

    #[test]
    fn a_room_off_is_silent_but_a_watched_thread_in_it_is_not() {
        let mut a = Attention::default();
        a.set_room("noisy", Level::Off);
        let e = chat("noisy", "OP", "user", "A1", "even named");
        assert!(!a.wakes_for(&me(), &e).wakes());
        let mut t = chat("noisy", "A9", "agent", "", "in thread");
        t.thread = "T1".into();
        a.threads.insert("T1".into());
        assert!(a.wakes_for(&me(), &t).wakes());
    }

    #[test]
    fn a_note_reaches_the_assignee_or_the_raiser_of_an_unowned_row() {
        let a = Attention::default();
        let mine: Event = serde_json::from_value(json!({
            "id": "n1", "type": "todo.note", "actor": "OP", "artifact": "T",
            "meta": {"assignee": "claude-lab2x1"}, "body": "?"
        }))
        .unwrap();
        assert!(a.wakes_for(&me(), &mine).wakes());
        let theirs: Event = serde_json::from_value(json!({
            "id": "n2", "type": "todo.note", "actor": "OP", "artifact": "T",
            "meta": {"assignee": "someone"}, "body": "?"
        }))
        .unwrap();
        assert!(!a.wakes_for(&me(), &theirs).wakes());
        let raised: Event = serde_json::from_value(json!({
            "id": "n3", "type": "todo.note", "actor": "OP", "artifact": "T",
            "meta": {"raiser": "claude-lab2x1"}, "body": "answer"
        }))
        .unwrap();
        assert!(a.wakes_for(&me(), &raised).wakes());
        // Two empty strings must never match: that is the shape that turns
        // silence into a firehose.
        let mut nobody = me();
        nobody.name = String::new();
        let blank: Event = serde_json::from_value(json!({
            "id": "n4", "type": "todo.note", "actor": "OP", "artifact": "T", "body": "x"
        }))
        .unwrap();
        assert!(!a.wakes_for(&nobody, &blank).wakes());
    }

    #[test]
    fn outside_the_focus_only_what_names_you() {
        let mut a = Attention::default();
        a.default = Level::All;
        a.focus = Some("Lab".into());
        let mut e = chat("general", "OP", "user", "", "flowy's general");
        e.project = Some("flowy".into());
        assert!(!a.wakes_for(&me(), &e).wakes());
        e.addressee = "A1".into();
        assert!(a.wakes_for(&me(), &e).wakes());
    }

    #[test]
    fn the_loosest_level_is_what_goes_on_the_wire() {
        let mut a = Attention::default();
        assert_eq!(a.loosest(), Level::Addressed);
        a.set_room("build", Level::Off);
        assert_eq!(a.loosest(), Level::Addressed);
        a.set_room("general", Level::All);
        assert_eq!(a.loosest(), Level::All);
        let mut m = Attention::default();
        m.default = Level::Mentions;
        m.threads.insert("T".into());
        assert_eq!(m.loosest(), Level::All);
        assert_eq!(Level::Off.server_filter(), Level::Mentions.server_filter());
    }
}
