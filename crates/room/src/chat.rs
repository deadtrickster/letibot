//! Chat: the coordinator's talk, beside the tagged queue and not instead of it. One append-only log
//! with one cursor (`seq`), cut into rooms (the host's, one per project) and direct messages.
//!
//! This module is the state machine and the vocabulary. It reads no clock and opens no socket: the
//! server hands in `now` and decides who a message is pushed to.

use std::collections::{BTreeMap, VecDeque};

use serde::{Deserialize, Serialize};

use crate::room::Refused;

/// The room every session on the host is in.
pub const HOST_ROOM: &str = "host";
/// The pseudo room a principal's direct messages are read from. A DM has no room of its own.
pub const DM_ROOM: &str = "dm";
/// The operator's name, reserved: only an operator seat may author under it.
pub const OPERATOR: &str = "operator";
/// Messages kept per room (and for all direct messages together). The oldest is dropped first.
pub const RING: usize = 500;
/// The longest message text, in bytes. Longer is refused, never cut.
pub const MAX_TEXT: usize = 4_000;
/// The most rows one read returns.
pub const MAX_READ: usize = 200;
/// Request ids remembered so a repeated `say` is answered, not appended twice.
const REPLAY: usize = 256;

/// The id of a project's chat room: `project/<project key>`.
pub fn project_room(key: &str) -> String {
    format!("project/{key}")
}

/// The project key a chat room id names, or `None` for the host room, `dm` and anything underived.
pub fn project_of_room(room: &str) -> Option<&str> {
    room.strip_prefix("project/")
        .filter(|k| crate::is_project_key(k))
}

/// A handle an external participant may use: lowercase letters, digits and `-`, at most 32, not
/// starting with `s-` (a session id's prefix) and never the operator's name.
pub fn valid_handle(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name != OPERATOR
        && !name.starts_with("s-")
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// What kind of participant a connection is. It is fixed at the `Hello`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Seat {
    /// A top-level session: the host room and its project's room.
    #[default]
    Session,
    /// The person, through a head: every room.
    Operator,
    /// A named outsider such as `claude-code`: every room.
    External,
    /// The harness reporting a fact (a todo moved, a branch landed). Authored by the daemon, never
    /// by a model or a head.
    System,
}

impl Seat {
    /// Does this seat see every room, whatever project it is in?
    pub fn is_wide(self) -> bool {
        matches!(self, Seat::Operator | Seat::External)
    }
}

/// A pointer from a message to a row another part of letibot owns. The row is not copied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ref {
    /// `todo` or `queue`.
    pub kind: String,
    pub id: String,
}

/// How many refs one message may carry.
pub const MAX_REFS: usize = 8;
/// The longest ref id, in bytes.
pub const MAX_REF_ID: usize = 200;

/// One chat message. `room` is `host`, `project/<key>` or `dm`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub seq: u64,
    pub ts: u64,
    pub room: String,
    pub author: String,
    pub kind: Seat,
    /// The one principal it is addressed to. For a DM, the only reader besides the author.
    #[serde(default)]
    pub to: Option<String>,
    /// The `seq` of the message this answers, in the same room.
    #[serde(default)]
    pub thread: Option<u64>,
    /// Todo and merge-queue rows this message is about.
    #[serde(default)]
    pub refs: Vec<Ref>,
    pub text: String,
}

/// The names `@name` puts in a text, lowercase, without the `@`.
pub fn mentions(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let boundary = i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
        if bytes[i] == b'@' && boundary {
            let start = i + 1;
            let mut end = start;
            while end < bytes.len()
                && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'-' || bytes[end] == b'_')
            {
                end += 1;
            }
            if end > start {
                out.push(text[start..end].to_lowercase());
            }
            i = end.max(i + 1);
        } else {
            i += 1;
        }
    }
    out
}

/// How much of the room a reader wants to be told about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Attention {
    /// What names you, a person's unaddressed broadcast, and DMs to you.
    #[default]
    ToMe,
    /// Only what names you.
    Mentions,
    /// Everything you may see, except your own.
    All,
}

impl Message {
    /// The message as one plain line: `#seq [room] author -> to: text (todo t-7)`.
    pub fn line(&self) -> String {
        let to = self
            .to
            .as_deref()
            .map(|t| format!(" -> {t}"))
            .unwrap_or_default();
        let refs: String = self
            .refs
            .iter()
            .map(|r| format!(" ({} {})", r.kind, r.id))
            .collect();
        format!(
            "#{} [{}] {}{to}: {}{refs}",
            self.seq, self.room, self.author, self.text
        )
    }

    /// Does this message name `who`: addressed to them, or `@who` in the text?
    pub fn names(&self, who: &str) -> bool {
        self.to.as_deref() == Some(who) || mentions(&self.text).iter().any(|m| m == who)
    }

    /// Does `attention` let this message through to `who`? Their own never pass.
    pub fn attends(&self, who: &str, attention: Attention) -> bool {
        if self.author == who {
            return false;
        }
        match attention {
            Attention::All => true,
            Attention::Mentions => self.names(who),
            Attention::ToMe => {
                self.names(who)
                    || (self.kind == Seat::Operator && self.to.is_none() && self.room != DM_ROOM)
            }
        }
    }
}

/// A room as one principal lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomInfo {
    pub id: String,
    /// `host` or `project`.
    pub kind: String,
    /// The project key, empty for the host room.
    #[serde(default)]
    pub project: String,
    /// Participants connected or known now.
    pub members: u64,
    /// The `seq` of the newest message in the room, 0 when empty.
    pub last_seq: u64,
    /// Messages after this principal's inbox cursor that are not its own.
    pub unread: u64,
}

/// One participant in a room's roster.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Presence {
    pub name: String,
    pub kind: Seat,
    /// Holding a connection to the coordinator now.
    pub connected: bool,
    #[serde(default)]
    pub project: String,
    /// The tree's label for a session, empty otherwise.
    #[serde(default)]
    pub label: String,
}

/// The answer to a read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct History {
    pub room: String,
    pub rows: Vec<Message>,
    /// The newest `seq` in the room, to resume from.
    pub last_seq: u64,
    /// True when the ring dropped messages the reader asked for.
    pub trimmed: bool,
}

#[derive(Default)]
pub struct Chat {
    next: u64,
    rings: BTreeMap<String, VecDeque<Message>>,
    dropped_to: BTreeMap<String, u64>,
    cursors: BTreeMap<String, u64>,
    said: VecDeque<((String, String), u64)>,
}

impl Chat {
    pub fn new() -> Chat {
        Chat::default()
    }

    /// The newest `seq` in the whole log.
    pub fn head(&self) -> u64 {
        self.next
    }

    /// Append a message. `room` is already checked by the caller. A repeat of `(author, request)`
    /// returns the message the first one made.
    #[allow(clippy::too_many_arguments)]
    pub fn append(
        &mut self,
        now: u64,
        request: &str,
        room: &str,
        author: &str,
        kind: Seat,
        to: Option<String>,
        thread: Option<u64>,
        refs: Vec<Ref>,
        text: &str,
    ) -> Result<Message, Refused> {
        let key = (author.to_string(), request.to_string());
        if let Some((_, seq)) = self.said.iter().find(|(k, _)| *k == key)
            && let Some(m) = self.find(room, *seq)
        {
            return Ok(m.clone());
        }
        let text = text.trim();
        if text.is_empty() {
            return Err(Refused::ChatEmpty);
        }
        if text.len() > MAX_TEXT {
            return Err(Refused::ChatTooLong {
                max: MAX_TEXT as u64,
                got: text.len() as u64,
            });
        }
        if let Some(t) = thread
            && self.find(room, t).is_none()
        {
            return Err(Refused::UnknownThread { thread: t });
        }
        if refs.len() > MAX_REFS
            || refs.iter().any(|r| {
                !matches!(r.kind.as_str(), "todo" | "queue")
                    || r.id.trim().is_empty()
                    || r.id.len() > MAX_REF_ID
            })
        {
            return Err(Refused::BadRef);
        }
        self.next += 1;
        let m = Message {
            seq: self.next,
            ts: now,
            room: room.to_string(),
            author: author.to_string(),
            kind,
            to,
            thread,
            refs,
            text: text.to_string(),
        };
        let ring = self.rings.entry(room.to_string()).or_default();
        ring.push_back(m.clone());
        while ring.len() > RING {
            if let Some(old) = ring.pop_front() {
                self.dropped_to.insert(room.to_string(), old.seq);
            }
        }
        self.said.push_back((key, m.seq));
        while self.said.len() > REPLAY {
            self.said.pop_front();
        }
        Ok(m)
    }

    fn find(&self, room: &str, seq: u64) -> Option<&Message> {
        self.rings.get(room)?.iter().find(|m| m.seq == seq)
    }

    /// The newest `seq` in one room.
    pub fn last_in(&self, room: &str) -> u64 {
        self.rings
            .get(room)
            .and_then(|r| r.back())
            .map_or(0, |m| m.seq)
    }

    /// Messages of `room` after `since`, oldest first. `last` keeps only the newest n; `thread`
    /// keeps the root and its replies. A read moves no cursor. `see` filters what the reader may see.
    pub fn read(
        &self,
        room: &str,
        since: u64,
        last: Option<usize>,
        thread: Option<u64>,
        see: impl Fn(&Message) -> bool,
    ) -> History {
        let mut rows: Vec<Message> = self
            .rings
            .get(room)
            .into_iter()
            .flatten()
            .filter(|m| m.seq > since && see(m))
            .filter(|m| thread.is_none_or(|t| m.seq == t || m.thread == Some(t)))
            .cloned()
            .collect();
        let keep = last.unwrap_or(MAX_READ).clamp(1, MAX_READ);
        if rows.len() > keep {
            // The newest rows are the ones a reader wants when it asks for a tail.
            rows.drain(..rows.len() - keep);
        }
        History {
            room: room.to_string(),
            rows,
            last_seq: self.last_in(room),
            trimmed: self.dropped_to.get(room).is_some_and(|d| *d > since),
        }
    }

    /// Start `name`'s inbox at the head of the log if it has none: a reader hears what happens next.
    pub fn arm(&mut self, name: &str) {
        let head = self.next;
        self.cursors.entry(name.to_string()).or_insert(head);
    }

    pub fn cursor(&self, name: &str) -> u64 {
        self.cursors.get(name).copied().unwrap_or(self.next)
    }

    /// Move `name`'s cursor forward. It never moves back and never past the head.
    pub fn ack(&mut self, name: &str, seq: u64) -> u64 {
        let head = self.next;
        let c = self.cursors.entry(name.to_string()).or_insert(head);
        *c = (*c).max(seq.min(head));
        *c
    }

    /// What `name` missed: visible messages after its cursor that pass `attention`, oldest first, up
    /// to `limit`, plus how many visible ones were left out. Moves nothing.
    pub fn inbox(
        &self,
        name: &str,
        attention: Attention,
        limit: usize,
        see: impl Fn(&Message) -> bool,
    ) -> (Vec<Message>, u64) {
        let cursor = self.cursor(name);
        let mut rows = Vec::new();
        let mut skipped = 0;
        let mut all: Vec<&Message> = self
            .rings
            .values()
            .flatten()
            .filter(|m| m.seq > cursor && m.author != name && see(m))
            .collect();
        all.sort_by_key(|m| m.seq);
        for m in all {
            if m.attends(name, attention) && rows.len() < limit.clamp(1, MAX_READ) {
                rows.push(m.clone());
            } else if !m.attends(name, attention) {
                skipped += 1;
            }
        }
        (rows, skipped)
    }

    /// Messages after `name`'s cursor that are not its own and that it may see - a room's unread.
    pub fn unread(&self, name: &str, room: &str, see: impl Fn(&Message) -> bool) -> u64 {
        let cursor = self.cursor(name);
        self.rings
            .get(room)
            .into_iter()
            .flatten()
            .filter(|m| m.seq > cursor && m.author != name && see(m))
            .count() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn say(c: &mut Chat, id: &str, room: &str, who: &str, kind: Seat, text: &str) -> Message {
        c.append(1, id, room, who, kind, None, None, Vec::new(), text)
            .expect("a message")
    }

    #[test]
    fn room_ids_derive_both_ways() {
        let id = project_room("0123456789ab");
        assert_eq!(id, "project/0123456789ab");
        assert_eq!(project_of_room(&id), Some("0123456789ab"));
        assert_eq!(project_of_room("project/../x"), None);
        assert_eq!(project_of_room(HOST_ROOM), None);
    }

    #[test]
    fn handles_are_plain_and_never_a_session_or_the_operator() {
        assert!(valid_handle("claude-code"));
        assert!(!valid_handle("operator"));
        assert!(!valid_handle("s-abc"));
        assert!(!valid_handle("Claude"));
        assert!(!valid_handle(""));
    }

    #[test]
    fn mentions_read_at_names_and_not_emails() {
        assert_eq!(mentions("@s-a look, @Op"), vec!["s-a", "op"]);
        assert!(mentions("mail me at x@y.z").is_empty());
        assert!(mentions("a @ b").is_empty());
    }

    #[test]
    fn the_log_has_one_cursor_across_rooms() {
        let mut c = Chat::new();
        let a = say(&mut c, "1", HOST_ROOM, "s-a", Seat::Session, "hi");
        let b = say(
            &mut c,
            "2",
            "project/aaaaaaaaaaaa",
            "s-b",
            Seat::Session,
            "yo",
        );
        assert_eq!((a.seq, b.seq), (1, 2));
        assert_eq!(c.last_in(HOST_ROOM), 1);
        assert_eq!(c.head(), 2);
    }

    #[test]
    fn a_repeated_request_is_one_message() {
        let mut c = Chat::new();
        let a = say(&mut c, "1", HOST_ROOM, "s-a", Seat::Session, "hi");
        let b = say(&mut c, "1", HOST_ROOM, "s-a", Seat::Session, "hi");
        assert_eq!(a, b);
        assert_eq!(c.head(), 1);
    }

    #[test]
    fn text_is_bounded_and_refused_not_cut() {
        let mut c = Chat::new();
        let long = "x".repeat(MAX_TEXT + 1);
        let err = c
            .append(
                1,
                "1",
                HOST_ROOM,
                "s-a",
                Seat::Session,
                None,
                None,
                Vec::new(),
                &long,
            )
            .unwrap_err();
        assert!(matches!(err, Refused::ChatTooLong { .. }), "{err:?}");
        let err = c
            .append(
                1,
                "2",
                HOST_ROOM,
                "s-a",
                Seat::Session,
                None,
                None,
                Vec::new(),
                "  ",
            )
            .unwrap_err();
        assert_eq!(err, Refused::ChatEmpty);
    }

    #[test]
    fn the_ring_drops_the_oldest_and_a_read_says_so() {
        let mut c = Chat::new();
        for i in 0..(RING + 5) {
            say(&mut c, &i.to_string(), HOST_ROOM, "s-a", Seat::Session, "m");
        }
        let h = c.read(HOST_ROOM, 0, Some(MAX_READ), None, |_| true);
        assert!(h.trimmed, "the first five are gone");
        assert_eq!(h.rows.len(), MAX_READ);
        assert_eq!(h.rows.last().map(|m| m.seq), Some((RING + 5) as u64));
        let tail = c.read(HOST_ROOM, 10, Some(3), None, |_| true);
        assert!(!tail.trimmed, "nothing after seq 10 was lost");
        assert_eq!(tail.rows.len(), 3);
        assert_eq!(c.rings[HOST_ROOM].len(), RING);
    }

    #[test]
    fn a_thread_is_its_root_and_replies_in_the_same_room() {
        let mut c = Chat::new();
        let root = say(&mut c, "1", HOST_ROOM, "s-a", Seat::Session, "q");
        let r = c
            .append(
                2,
                "2",
                HOST_ROOM,
                "s-b",
                Seat::Session,
                None,
                Some(root.seq),
                Vec::new(),
                "a",
            )
            .unwrap();
        say(&mut c, "3", HOST_ROOM, "s-c", Seat::Session, "other");
        let h = c.read(HOST_ROOM, 0, None, Some(root.seq), |_| true);
        assert_eq!(
            h.rows.iter().map(|m| m.seq).collect::<Vec<_>>(),
            vec![root.seq, r.seq]
        );
        let err = c
            .append(
                3,
                "4",
                "project/aaaaaaaaaaaa",
                "s-b",
                Seat::Session,
                None,
                Some(root.seq),
                Vec::new(),
                "x",
            )
            .unwrap_err();
        assert!(matches!(err, Refused::UnknownThread { .. }));
    }

    #[test]
    fn attention_is_to_me_mentions_or_everything() {
        let mut c = Chat::new();
        let plain = say(&mut c, "1", HOST_ROOM, "s-a", Seat::Session, "chatter");
        let named = say(&mut c, "2", HOST_ROOM, "s-a", Seat::Session, "ping @s-me");
        let bcast = say(
            &mut c,
            "3",
            HOST_ROOM,
            OPERATOR,
            Seat::Operator,
            "all hands",
        );
        let own = say(&mut c, "4", HOST_ROOM, "s-me", Seat::Session, "mine");
        for (att, want) in [
            (Attention::All, vec![plain.seq, named.seq, bcast.seq]),
            (Attention::ToMe, vec![named.seq, bcast.seq]),
            (Attention::Mentions, vec![named.seq]),
        ] {
            let got: Vec<u64> = [&plain, &named, &bcast, &own]
                .into_iter()
                .filter(|m| m.attends("s-me", att))
                .map(|m| m.seq)
                .collect();
            assert_eq!(got, want, "{att:?}");
        }
    }

    #[test]
    fn an_inbox_replays_after_the_cursor_and_counts_what_went_past() {
        let mut c = Chat::new();
        c.arm("s-me");
        say(&mut c, "1", HOST_ROOM, "s-a", Seat::Session, "chatter");
        say(&mut c, "2", HOST_ROOM, "s-a", Seat::Session, "hey @s-me");
        let (rows, skipped) = c.inbox("s-me", Attention::ToMe, 50, |_| true);
        assert_eq!((rows.len(), skipped), (1, 1));
        assert_eq!(c.cursor("s-me"), 0, "the inbox moves nothing");
        c.ack("s-me", 2);
        let (rows, skipped) = c.inbox("s-me", Attention::All, 50, |_| true);
        assert_eq!((rows.len(), skipped), (0, 0));
        assert_eq!(c.ack("s-me", 1), 2, "a cursor never moves back");
        assert_eq!(c.ack("s-me", 99), 2, "nor past the head");
    }

    #[test]
    fn a_message_may_point_at_a_todo_or_a_queue_row_and_nothing_else() {
        let mut c = Chat::new();
        let r = |k: &str, id: &str| Ref {
            kind: k.into(),
            id: id.into(),
        };
        let m = c
            .append(
                1,
                "1",
                HOST_ROOM,
                "todo",
                Seat::System,
                None,
                None,
                vec![r("todo", "t-7")],
                "filed",
            )
            .unwrap();
        assert_eq!(m.refs, vec![r("todo", "t-7")]);
        for bad in [r("memory", "1"), r("todo", " ")] {
            let err = c
                .append(
                    2,
                    "2",
                    HOST_ROOM,
                    "s-a",
                    Seat::Session,
                    None,
                    None,
                    vec![bad],
                    "x",
                )
                .unwrap_err();
            assert_eq!(err, Refused::BadRef);
        }
    }

    #[test]
    fn a_new_reader_starts_at_the_head() {
        let mut c = Chat::new();
        say(&mut c, "1", HOST_ROOM, "s-a", Seat::Session, "old");
        c.arm("s-new");
        assert_eq!(c.cursor("s-new"), 1);
        let (rows, _) = c.inbox("s-new", Attention::All, 50, |_| true);
        assert!(rows.is_empty());
    }
}
