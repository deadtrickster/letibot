//! Chat: one append-only log with one cursor (`seq`), cut into rooms (the host's, one per project) and DMs.
//! State machine and vocabulary only: no clock, no socket. The server hands in `now` and decides who is pushed to.

use std::collections::{BTreeMap, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};

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
/// Text bytes kept per room (and for all direct messages together); the oldest rows go first. The
/// store is replayed into memory at start, so this bounds disk and the coordinator's heap.
pub const ROOM_BYTES: usize = 256 * 1024;
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

/// The author of a digest message, kind `system`.
pub const DIGEST_AUTHOR: &str = "room";
/// Window of history a member with no mark is told about.
pub const DIGEST_WINDOW_MS: u64 = 60 * 60 * 1000;
/// Verbatim lines at the end of one room's digest.
pub const DIGEST_TAIL: usize = 3;
/// Most authors, unanswered questions and open items one digest names.
pub const DIGEST_NAMED: usize = 5;
/// Longest text of a digest's verbatim line, in bytes.
pub const DIGEST_LINE: usize = 240;

/// The deterministic "said since" for one room: counts, authors, the last lines, what is open.
pub fn summarise(
    room: &str,
    since: &str,
    rows: &[Message],
    open_items: &[String],
) -> Option<String> {
    if rows.is_empty() {
        return None;
    }
    let mut by: BTreeMap<&str, u64> = BTreeMap::new();
    for m in rows {
        *by.entry(m.author.as_str()).or_default() += 1;
    }
    let mut authors: Vec<(&str, u64)> = by.into_iter().collect();
    authors.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let more_authors = authors.len().saturating_sub(DIGEST_NAMED);
    let mut out = format!(
        "said since {since} in `{room}`: {} message(s) - ",
        rows.len()
    );
    out.push_str(
        &authors
            .iter()
            .take(DIGEST_NAMED)
            .map(|(a, n)| format!("{a} {n}"))
            .collect::<Vec<_>>()
            .join(", "),
    );
    if more_authors > 0 {
        out.push_str(&format!(", {more_authors} more author(s)"));
    }
    let tail = rows.len().min(DIGEST_TAIL);
    for m in &rows[rows.len() - tail..] {
        let mut text = m.text.replace('\n', " ");
        if text.len() > DIGEST_LINE {
            let mut end = DIGEST_LINE;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            let cut = text.len() - end;
            text.truncate(end);
            text.push_str(&format!(" [+{cut} bytes]"));
        }
        out.push_str(&format!("\n  #{} {}: {text}", m.seq, m.author));
    }
    // A question is open while nothing in the batch answers it as a thread reply.
    let answered: std::collections::BTreeSet<u64> = rows.iter().filter_map(|m| m.thread).collect();
    let questions: Vec<&Message> = rows
        .iter()
        .filter(|m| m.text.trim_end().ends_with('?') && !answered.contains(&m.seq))
        .collect();
    if !questions.is_empty() {
        let shown: Vec<String> = questions
            .iter()
            .rev()
            .take(DIGEST_NAMED)
            .map(|m| format!("#{} {}", m.seq, m.author))
            .collect();
        out.push_str(&format!("\n  unanswered: {}", shown.join(", ")));
        if questions.len() > DIGEST_NAMED {
            out.push_str(&format!(", {} more", questions.len() - DIGEST_NAMED));
        }
    }
    if !open_items.is_empty() {
        out.push_str(&format!(
            "\n  open: {}",
            open_items
                .iter()
                .take(DIGEST_NAMED)
                .cloned()
                .collect::<Vec<_>>()
                .join("; ")
        ));
        if open_items.len() > DIGEST_NAMED {
            out.push_str(&format!("; {} more", open_items.len() - DIGEST_NAMED));
        }
    }
    let left = rows.len() - tail;
    if left > 0 {
        out.push_str(&format!("\n  {left} more, `room read {room}` for all"));
    }
    Some(out)
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

/// One line of the chat store. The file is append-only and rewritten whole when it outgrows the live
/// state, so replaying it from the top rebuilds the rings, the dedup window and the cursors.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Record {
    Msg {
        request: String,
        message: Message,
    },
    Cursor {
        name: String,
        seq: u64,
    },
    Dropped {
        room: String,
        seq: u64,
    },
    /// The newest `seq` a member was connected for, the start of what it missed after a gap.
    Seen {
        name: String,
        seq: u64,
    },
}

/// The file chat is kept in, and the size it had at the last write.
struct Store {
    path: PathBuf,
    bytes: u64,
}

#[derive(Default)]
pub struct Chat {
    next: u64,
    rings: BTreeMap<String, VecDeque<Message>>,
    dropped_to: BTreeMap<String, u64>,
    cursors: BTreeMap<String, u64>,
    seen: BTreeMap<String, u64>,
    said: VecDeque<((String, String), u64)>,
    store: Option<Store>,
}

fn ring_bytes(ring: &VecDeque<Message>) -> usize {
    ring.iter().map(|m| m.text.len()).sum()
}

impl Chat {
    pub fn new() -> Chat {
        Chat::default()
    }

    /// A chat kept in `path`: what the file holds is loaded, and every change after is appended. An
    /// unreadable line is skipped and said; a missing file is an empty chat.
    pub fn open(path: &Path) -> Chat {
        let mut c = Chat::new();
        let text = std::fs::read_to_string(path).unwrap_or_default();
        for (n, line) in text.lines().enumerate() {
            match serde_json::from_str::<Record>(line) {
                Ok(Record::Msg { request, message }) => c.restore(request, message),
                Ok(Record::Cursor { name, seq }) => {
                    let cur = c.cursors.entry(name).or_insert(0);
                    *cur = (*cur).max(seq);
                }
                Ok(Record::Seen { name, seq }) => {
                    let s = c.seen.entry(name).or_insert(0);
                    *s = (*s).max(seq);
                }
                Ok(Record::Dropped { room, seq }) => {
                    let d = c.dropped_to.entry(room).or_insert(0);
                    *d = (*d).max(seq);
                }
                Err(e) => eprintln!(
                    "room: chat store {} line {} is not a record ({e}); skipped",
                    path.display(),
                    n + 1
                ),
            }
        }
        c.store = Some(Store {
            path: path.to_path_buf(),
            bytes: text.len() as u64,
        });
        c
    }

    fn restore(&mut self, request: String, m: Message) {
        self.next = self.next.max(m.seq);
        self.said.push_back(((m.author.clone(), request), m.seq));
        while self.said.len() > REPLAY {
            self.said.pop_front();
        }
        let room = m.room.clone();
        let ring = self.rings.entry(room.clone()).or_default();
        ring.push_back(m);
        self.trim(&room);
    }

    fn trim(&mut self, room: &str) {
        let Some(ring) = self.rings.get_mut(room) else {
            return;
        };
        let mut bytes = ring_bytes(ring);
        while ring.len() > RING || (bytes > ROOM_BYTES && ring.len() > 1) {
            if let Some(old) = ring.pop_front() {
                bytes -= old.text.len();
                let d = self.dropped_to.entry(room.to_string()).or_insert(0);
                *d = (*d).max(old.seq);
            }
        }
    }

    fn write(&mut self, records: &[Record]) {
        let Some(store) = self.store.as_mut() else {
            return;
        };
        let mut buf = String::new();
        for r in records {
            if let Ok(line) = serde_json::to_string(r) {
                buf.push_str(&line);
                buf.push('\n');
            }
        }
        let wrote = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&store.path)
            .and_then(|mut f| f.write_all(buf.as_bytes()));
        match wrote {
            Ok(()) => store.bytes += buf.len() as u64,
            Err(e) => eprintln!(
                "room: chat store {} not written ({e}); chat continues in memory only",
                store.path.display()
            ),
        }
        self.compact();
    }

    /// Rewrite the store from live state once it holds twice what is live, so dropped rows and
    /// superseded cursors do not grow the file without bound.
    fn compact(&mut self) {
        let Some(store) = self.store.as_ref() else {
            return;
        };
        let live: usize = self.rings.values().map(ring_bytes).sum();
        if store.bytes < (2 * live as u64).max(64 * 1024) {
            return;
        }
        let request_of: BTreeMap<u64, &str> = self
            .said
            .iter()
            .map(|((_, r), seq)| (*seq, r.as_str()))
            .collect();
        let mut records: Vec<Record> = Vec::new();
        for (room, seq) in &self.dropped_to {
            records.push(Record::Dropped {
                room: room.clone(),
                seq: *seq,
            });
        }
        let mut msgs: Vec<&Message> = self.rings.values().flatten().collect();
        msgs.sort_by_key(|m| m.seq);
        for m in msgs {
            records.push(Record::Msg {
                request: request_of.get(&m.seq).copied().unwrap_or("").to_string(),
                message: m.clone(),
            });
        }
        for (name, seq) in &self.cursors {
            records.push(Record::Cursor {
                name: name.clone(),
                seq: *seq,
            });
        }
        for (name, seq) in &self.seen {
            records.push(Record::Seen {
                name: name.clone(),
                seq: *seq,
            });
        }
        let mut buf = String::new();
        for r in &records {
            if let Ok(line) = serde_json::to_string(r) {
                buf.push_str(&line);
                buf.push('\n');
            }
        }
        let path = store.path.clone();
        let tmp = path.with_extension("tmp");
        let done = std::fs::write(&tmp, buf.as_bytes()).and_then(|()| std::fs::rename(&tmp, &path));
        match done {
            Ok(()) => {
                if let Some(s) = self.store.as_mut() {
                    s.bytes = buf.len() as u64;
                }
            }
            Err(e) => eprintln!("room: chat store {} not compacted ({e})", path.display()),
        }
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
        self.rings
            .entry(room.to_string())
            .or_default()
            .push_back(m.clone());
        self.trim(room);
        self.said.push_back((key, m.seq));
        while self.said.len() > REPLAY {
            self.said.pop_front();
        }
        self.write(&[Record::Msg {
            request: request.to_string(),
            message: m.clone(),
        }]);
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
        if !self.cursors.contains_key(name) {
            self.cursors.insert(name.to_string(), head);
            self.write(&[Record::Cursor {
                name: name.to_string(),
                seq: head,
            }]);
        }
    }

    pub fn cursor(&self, name: &str) -> u64 {
        self.cursors.get(name).copied().unwrap_or(self.next)
    }

    /// Move `name`'s cursor forward. It never moves back and never past the head.
    pub fn ack(&mut self, name: &str, seq: u64) -> u64 {
        let head = self.next;
        let before = self.cursors.get(name).copied();
        let c = self.cursors.entry(name.to_string()).or_insert(head);
        *c = (*c).max(seq.min(head));
        let now = *c;
        if before != Some(now) {
            self.write(&[Record::Cursor {
                name: name.to_string(),
                seq: now,
            }]);
        }
        now
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

    /// The file chat is kept in, `None` for a chat in memory only.
    pub fn store_path(&self) -> Option<&Path> {
        self.store.as_ref().map(|s| s.path.as_path())
    }

    /// Note that `name` was connected while `seq` was said. Memory only, until [`Chat::keep_seen`].
    pub fn mark_seen(&mut self, name: &str, seq: u64) {
        let s = self.seen.entry(name.to_string()).or_insert(0);
        *s = (*s).max(seq);
    }

    /// Write `name`'s seen mark, so a restart knows where its gap began.
    pub fn keep_seen(&mut self, name: &str) {
        if let Some(seq) = self.seen.get(name).copied() {
            self.write(&[Record::Seen {
                name: name.to_string(),
                seq,
            }]);
        }
    }

    /// The newest `seq` `name` was connected for, `None` for a member never seen.
    pub fn seen(&self, name: &str) -> Option<u64> {
        self.seen.get(name).copied()
    }

    /// What `name` missed, for [`summarise`]: visible messages of others after `since` (a `seq`) or,
    /// for a member with no mark, newer than `window_from` (ms), oldest first.
    pub fn missed(
        &self,
        name: &str,
        since: Option<u64>,
        window_from: u64,
        see: impl Fn(&Message) -> bool,
    ) -> Vec<Message> {
        let mut all: Vec<Message> = self
            .rings
            .values()
            .flatten()
            .filter(|m| match since {
                Some(s) => m.seq > s,
                None => m.ts >= window_from,
            })
            .filter(|m| m.author != name && see(m))
            .cloned()
            .collect();
        all.sort_by_key(|m| m.seq);
        all
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

#[cfg(test)]
mod store_tests {
    use super::*;

    fn path(tag: &str) -> PathBuf {
        let p =
            std::env::temp_dir().join(format!("letibot-chat-store-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn say(c: &mut Chat, id: &str, room: &str, text: &str) -> Message {
        c.append(
            1,
            id,
            room,
            "s-a",
            Seat::Session,
            None,
            None,
            Vec::new(),
            text,
        )
        .expect("a message")
    }

    #[test]
    fn the_oldest_rows_go_first_past_the_byte_cap() {
        let p = path("cap");
        let mut c = Chat::open(&p);
        let big = "x".repeat(MAX_TEXT);
        let n = ROOM_BYTES / MAX_TEXT + 10;
        for i in 0..n {
            say(&mut c, &i.to_string(), HOST_ROOM, &big);
        }
        let h = c.read(HOST_ROOM, 0, Some(MAX_READ), None, |_| true);
        assert!(h.rows.iter().map(|m| m.text.len()).sum::<usize>() <= ROOM_BYTES);
        assert!(h.trimmed);
        let again = Chat::open(&p);
        assert_eq!(again.head(), n as u64);
        assert_eq!(again.last_in(HOST_ROOM), n as u64);
        assert!(again.read(HOST_ROOM, 0, None, None, |_| true).trimmed);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn the_store_is_rewritten_when_it_outgrows_what_is_live() {
        let p = path("compact");
        let mut c = Chat::open(&p);
        say(&mut c, "1", HOST_ROOM, "kept");
        c.arm("r");
        for i in 0..4000 {
            c.ack("r", i.min(1));
        }
        for i in 0..4000u64 {
            c.cursors.insert("r".into(), i);
            c.write(&[Record::Cursor {
                name: "r".into(),
                seq: i,
            }]);
        }
        let size = std::fs::metadata(&p).expect("the store").len();
        assert!(size < 128 * 1024, "compacted: {size}");
        let again = Chat::open(&p);
        assert_eq!(again.head(), 1);
        assert_eq!(again.cursor("r"), 3999);
        let _ = std::fs::remove_file(&p);
    }
}
