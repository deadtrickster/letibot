//! The `/rooms` view's state: the channels, the messages read so far, what was shown, and the
//! requests that keep them fresh. A head has no push for chat, so the view polls its daemon while
//! it is open and acks what it showed. The chat is the coordinator's: nothing here is a source.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::ControlFlow;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::app::*;

/// How often the open view asks for new messages and counts.
pub(crate) const POLL_MS: u64 = 2_000;
/// While the view is closed only the room list is asked for: counts, no history.
pub(crate) const CLOSED_POLL_MS: u64 = 10_000;

/// The coordinator's ring size per room. This head keeps up to four rings of one room.
const KEEP: usize = 500;

/// Request id prefix of a page-back read, so its answer is told from a poll's.
pub(crate) const OLDER: &str = "older:";

/// Messages asked for per page back, the most one read returns.
const PAGE: u32 = 200;

/// The name the head speaks under.
pub(crate) const OPERATOR: &str = "operator";

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(crate) struct RoomRow {
    pub id: String,
    #[serde(default)]
    pub members: u64,
    #[serde(default)]
    pub last_seq: u64,
    #[serde(default)]
    pub unread: u64,
    /// The project's folder name, empty for the host room.
    #[serde(default)]
    pub label: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(crate) struct ChatRef {
    pub kind: String,
    pub id: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[allow(dead_code)] // P2 draws the pictures and the size; the wire carries them from P0.
pub(crate) struct Attach {
    /// The sha256 hex of the bytes: what a `![alt](blob:ID)` in the text names.
    pub id: String,
    #[serde(default)]
    pub mime: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub bytes: u64,
    /// The picture's size, when the coordinator could read it out of the header.
    #[serde(default)]
    pub w: Option<u32>,
    #[serde(default)]
    pub h: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(crate) struct Msg {
    pub seq: u64,
    #[serde(default)]
    pub ts: u64,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub thread: Option<u64>,
    #[serde(default)]
    pub refs: Vec<ChatRef>,
    /// The pictures and files the message carries, defaulted so a coordinator that predates them
    /// parses exactly as before.
    #[serde(default)]
    #[allow(dead_code)] // P2 draws them.
    pub attachments: Vec<Attach>,
    #[serde(default)]
    pub text: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(crate) struct Member {
    pub name: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub connected: bool,
    #[serde(default)]
    pub label: String,
    /// The project the member works in, empty when it has none.
    #[serde(default)]
    pub project: String,
}

/// One entry of the channel list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Chan {
    /// `host` or `project/<key>`.
    Room(String),
    /// Direct messages with one principal.
    Dm(String),
    /// One of the operator's own top-level sessions, by session id.
    Session(String),
}

/// What a row of the left list stands for when it is clicked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ListRow {
    Chan(Chan),
    /// A repository's queue row, by the repository key the queue snapshot uses.
    Queue(String),
    /// The mute checkbox of that repository's queue row.
    Mute(String),
}

/// A top-level session of the operator's, as the Sessions section lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessInfo {
    pub id: String,
    pub name: String,
    pub project: String,
    /// `running`, `idle` or `asking`.
    pub state: &'static str,
}

/// A line this head sent and is still answerable for, under the request id it went out with.
/// The composer is already empty when it leaves, so until it lands this record is the only
/// copy of the words - and once it fails, the only place the reason is kept.
#[derive(Debug, Clone)]
pub(crate) struct Send {
    pub id: String,
    pub chan: Chan,
    pub thread: Option<u64>,
    pub text: String,
    pub error: Option<String>,
}

#[derive(Debug, Default)]
pub(crate) struct RoomsView {
    pub rows: Vec<RoomRow>,
    pub msgs: BTreeMap<String, Vec<Msg>>,
    /// The newest `seq` read per room (`dm` for direct messages): where the next `read` starts.
    pub read_to: BTreeMap<String, u64>,
    /// The newest `seq` shown per channel key.
    pub seen: BTreeMap<String, u64>,
    /// The daemon's marks were asked for: they outlive this head, and seed `seen` once.
    pub marks_asked: bool,
    /// The channel's `seen` mark at the moment it was opened: the unread divider sits before the
    /// first message newer than it, and stays for as long as the channel is selected.
    #[allow(dead_code)] // P2 reads it to place the divider; P4 fills it.
    pub opened_seen: BTreeMap<String, u64>,
    /// The selected channel by identity, never by position: a poll replaces `rows` wholesale,
    /// so a room arriving ahead of the current one would move an index and send the next line
    /// to another channel.
    pub sel: Option<Chan>,
    /// The channel that vanished while selected, named so the view can say so. `None` until a
    /// poll drops the selected channel, cleared by the next explicit move.
    pub gone: Option<String>,
    /// The roster lists the offline members too, not just their count.
    pub roster_all: bool,
    pub roster: BTreeMap<String, Vec<Member>>,
    /// The last draw's member column: its x offset in the pane, its width, and the handle
    /// on each row, so a click is answered from the frame and not recomputed.
    pub member_col: (usize, usize),
    /// The operator's top-level sessions, kept in step with the head's session list.
    pub sessions: Vec<SessInfo>,
    /// The channel list's rows as the last frame drew them: the pane row and the channel on it.
    pub chan_rows: Vec<(usize, ListRow)>,
    /// Sessions the review queue runs (the gatekeeper and its reviewers): what they say is a queue
    /// event, counted on the Queues rows, and never a room message.
    pub queue_authors: BTreeSet<String>,
    /// The newest queue move each repository's row has shown, by repository key.
    pub queue_seen: BTreeMap<String, u64>,
    /// The newest `seq` of a queue post dropped from each room, so reading the room to its end
    /// still reaches the room's last `seq`.
    pub hidden: BTreeMap<String, u64>,
    pub member_rows: Vec<(usize, String)>,
    /// Rooms whose oldest stored message is on screen, so asking further back finds nothing.
    pub oldest_known: BTreeSet<String>,
    /// Rooms with a page-back request in flight.
    pub paging: BTreeSet<String>,
    /// The chat was at its first line at the last draw: older messages are wanted.
    pub at_top: bool,
    /// The first seq and line count of the last draw, to keep the view in place when a page of
    /// older messages lands above it.
    pub prev_first: Option<(String, u64, usize)>,
    pub next_poll_ms: u64,
    pub error: Option<String>,
    /// The inbox cursor this head last set.
    pub acked: u64,
    pub said: u64,
    /// Per-head part of a request id, so two heads' ids never collide in the coordinator's
    /// replay window. Set on the first send.
    pub tag: String,
    /// Peers the operator started a DM with that have no message yet.
    pub fresh: BTreeSet<String>,
    /// The scroll limit of the last draw, to tell a view at the tail from one scrolled back.
    pub prev_max: usize,
    /// Any answer has arrived since the view opened.
    pub heard: bool,
    /// Lines this head sent, oldest first, until each lands or is dealt with. Bounded by the
    /// operator's own pace: a record goes when its answer arrives, when it is resent, or when
    /// it is dismissed - never because a poll came back.
    pub sends: Vec<Send>,
}

impl RoomsView {
    /// The principal on the other end of a direct message, from the operator's side.
    fn peer_of(m: &Msg) -> Option<String> {
        if m.author == OPERATOR {
            m.to.clone()
        } else {
            Some(m.author.clone())
        }
    }

    fn peers(&self) -> Vec<String> {
        let mut out: BTreeSet<String> = self.fresh.clone();
        for m in self.msgs.get("dm").into_iter().flatten() {
            out.extend(Self::peer_of(m));
        }
        out.into_iter().collect()
    }

    /// The host room, then the project rooms as the coordinator listed them, then the DMs.
    pub(crate) fn channels(&self) -> Vec<Chan> {
        let mut out = vec![Chan::Room("host".into())];
        for r in &self.rows {
            if r.id != "host" && r.id != "dm" {
                out.push(Chan::Room(r.id.clone()));
            }
        }
        let peers = self.peers();
        for s in &self.sessions {
            out.push(Chan::Session(s.id.clone()));
            if peers.contains(&s.id) {
                out.push(Chan::Dm(s.id.clone()));
            }
        }
        out.extend(
            peers
                .into_iter()
                .filter(|p| !self.sessions.iter().any(|s| &s.id == p))
                .map(Chan::Dm),
        );
        out
    }

    /// The selected channel, if it is still a channel: a poll that dropped it selects nothing
    /// rather than landing the next line on whichever channel took its index.
    pub(crate) fn selected(&self) -> Option<Chan> {
        let all = self.channels();
        self.sel.clone().filter(|c| all.contains(c))
    }

    pub(crate) fn key(c: &Chan) -> String {
        match c {
            Chan::Room(id) => id.clone(),
            Chan::Dm(p) => format!("dm:{p}"),
            Chan::Session(id) => format!("session:{id}"),
        }
    }

    pub(crate) fn messages(&self, c: &Chan) -> Vec<&Msg> {
        match c {
            Chan::Room(id) => self.msgs.get(id).into_iter().flatten().collect(),
            Chan::Session(_) => Vec::new(),
            Chan::Dm(p) => self
                .msgs
                .get("dm")
                .into_iter()
                .flatten()
                .filter(|m| Self::peer_of(m).as_deref() == Some(p.as_str()))
                .collect(),
        }
    }

    /// Messages of `c` the operator has not been shown.
    pub(crate) fn unread(&self, c: &Chan) -> u64 {
        let seen = self.seen.get(&Self::key(c)).copied().unwrap_or(0);
        match c {
            Chan::Room(id) => self
                .rows
                .iter()
                .find(|r| &r.id == id)
                .filter(|r| r.last_seq > seen)
                .map_or(0, |r| {
                    // The coordinator counts from one cursor for the whole log, which a held ack
                    // leaves behind what this head showed: past a known mark only what followed
                    // it is news.
                    if seen == 0 {
                        return r.unread;
                    }
                    let loaded = self.msgs.get(id).map_or(&[][..], Vec::as_slice);
                    let top = loaded
                        .last()
                        .map_or(0, |m| m.seq)
                        .max(self.hidden.get(id).copied().unwrap_or(0));
                    let after = loaded
                        .iter()
                        .filter(|m| m.author != OPERATOR && m.seq > seen)
                        .count() as u64;
                    r.unread.min(after + u64::from(r.last_seq > top))
                }),
            Chan::Dm(_) => self
                .messages(c)
                .iter()
                .filter(|m| m.author != OPERATOR && m.seq > seen)
                .count() as u64,
            Chan::Session(id) => self.unread(&Chan::Dm(id.clone())),
        }
    }

    /// Unread messages across the rooms the coordinator listed, from the last room list.
    pub(crate) fn unread_total(&self) -> u64 {
        self.rows
            .iter()
            .map(|r| self.unread(&Chan::Room(r.id.clone())))
            .sum()
    }

    /// A send of ours failed: keep the line and put the reason on it. `false` when no send of
    /// this id is waiting, so the caller can treat the reply as its own trouble instead.
    fn send_failed(&mut self, id: &str, why: String) -> bool {
        let Some(s) = self.sends.iter_mut().find(|s| s.id == id) else {
            return false;
        };
        if s.error.is_none() {
            s.error = Some(why);
        }
        true
    }

    /// The send landed: the poll brings the message, so the record's work is done.
    fn send_landed(&mut self, id: &str) {
        self.sends.retain(|s| s.id != id);
    }

    /// The newest failed send of `c`, if any: the one Enter resends and `d` dismisses.
    pub(crate) fn failed_of(&self, c: &Chan) -> Option<&Send> {
        self.sends
            .iter()
            .rev()
            .find(|s| s.chan == *c && s.error.is_some())
    }

    pub(crate) fn merge(&mut self, room: &str, rows: Vec<Msg>) {
        let cursor = self.read_to.entry(room.to_string()).or_insert(0);
        let list = self.msgs.entry(room.to_string()).or_default();
        for m in rows {
            *cursor = (*cursor).max(m.seq);
            if self.queue_authors.contains(&m.author) || m.author.starts_with("s-gatekeeper-") {
                let at = self.hidden.entry(room.to_string()).or_insert(0);
                *at = (*at).max(m.seq);
                continue;
            }
            if list.iter().all(|x| x.seq != m.seq) {
                list.push(m);
            }
        }
        list.sort_by_key(|m| m.seq);
        // Paging back adds to what the ring holds, so the cap is a multiple of it. Past it the
        // oldest go and the view stops paging, so a dropped page is not fetched again.
        if list.len() > KEEP * 4 {
            list.drain(..list.len() - KEEP * 4);
            self.oldest_known.insert(room.to_string());
        }
    }

    /// The page before the oldest message held, for a room that may have older ones.
    pub(crate) fn older_op(&self, room: &str) -> Option<Value> {
        if self.oldest_known.contains(room) || self.paging.contains(room) {
            return None;
        }
        let first = self.msgs.get(room)?.first()?.seq;
        Some(json!({"type": "read", "room": room, "before": first, "last": PAGE}))
    }

    /// The label of a project channel's room, the folder name; the id when the coordinator
    /// sent none.
    pub(crate) fn room_label(&self, id: &str) -> String {
        match self.rows.iter().find(|r| r.id == id) {
            Some(r) if !r.label.is_empty() => r.label.clone(),
            _ => match id.strip_prefix("project/") {
                Some(key) => key.to_string(),
                None => id.to_string(),
            },
        }
    }

    fn read_op(&self, room: &str) -> Value {
        json!({"type": "read", "room": room, "since": self.read_to.get(room).copied().unwrap_or(0)})
    }

    /// What one poll asks: the room list with counts, the selected channel's news, and the DMs.
    /// With nothing selected - the channel went away - only the list is asked for.
    pub(crate) fn poll_ops(&self) -> Vec<Value> {
        let mut ops = vec![json!({"type": "rooms"})];
        match self.selected() {
            Some(Chan::Room(id)) => {
                ops.push(self.read_op(&id));
                ops.push(self.read_op("dm"));
                ops.push(json!({"type": "roster", "room": id}));
            }
            Some(Chan::Dm(_) | Chan::Session(_)) => ops.push(self.read_op("dm")),
            None => {}
        }
        ops
    }

    /// Mark the selected channel shown, and say how far the inbox cursor may move. `seq`
    /// counts the whole log, not one room, so any other channel with something undrawn
    /// holds the cursor - it may not pass a message this head has not drawn anywhere.
    pub(crate) fn shown(&mut self) -> Option<u64> {
        let c = self.selected()?;
        let shown = self.messages(&c).last().map(|m| m.seq)?;
        // A queue post the room dropped is not news to read, so reading to the end passes it.
        let top = match &c {
            Chan::Room(id) => shown.max(self.hidden.get(id).copied().unwrap_or(0)),
            _ => shown,
        };
        self.seen.insert(Self::key(&c), top);
        let undrawn = |c: &Chan| -> bool {
            let seen = self.seen.get(&Self::key(c)).copied().unwrap_or(0);
            match c {
                Chan::Room(id) => self
                    .rows
                    .iter()
                    .find(|r| &r.id == id)
                    .is_some_and(|r| r.last_seq > seen),
                Chan::Dm(_) => self.messages(c).iter().any(|m| m.seq > seen),
                Chan::Session(_) => false,
            }
        };
        let held = self
            .channels()
            .iter()
            .any(|other| *other != c && undrawn(other));
        // With nothing undrawn anywhere, every channel's tail is drawn: the frontier is the
        // highest seq this head has drawn in any channel, not just the selected one's.
        let frontier = self.seen.values().copied().max().unwrap_or(top);
        (!held && frontier > self.acked).then(|| {
            self.acked = frontier;
            frontier
        })
    }
}

/// Does `m` name the operator: addressed to it, or `@operator` in the text.
pub(crate) fn mentions_operator(m: &Msg) -> bool {
    if m.author == OPERATOR {
        return false;
    }
    if m.to.as_deref() == Some(OPERATOR) {
        return true;
    }
    let lower = m.text.to_ascii_lowercase();
    let tag = format!("@{OPERATOR}");
    let name = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    lower.match_indices(&tag).any(|(i, _)| {
        let before = lower[..i].chars().next_back().is_none_or(|c| !name(c));
        let after = lower[i + tag.len()..]
            .chars()
            .next()
            .is_none_or(|c| !name(c));
        before && after
    })
}

/// `>14 text` answers message 14. Anything else is the whole text.
fn split_reply(text: &str) -> (Option<u64>, &str) {
    if let Some(rest) = text.strip_prefix('>') {
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if let Ok(n) = digits.parse::<u64>() {
            let body = rest[digits.len()..].trim_start();
            if !body.is_empty() && rest[digits.len()..].starts_with(char::is_whitespace) {
                return (Some(n), body);
            }
        }
    }
    (None, text)
}

impl App {
    /// The first requests of an open view. The rest of the first poll rides in `queued`.
    pub(crate) fn open_rooms(&mut self) -> Option<Action> {
        self.rooms.heard = false;
        self.rooms.error = None;
        self.rooms.gone = None;
        // A fresh open starts on the host room, and so does a reopen after the selected channel
        // went away: `gone` is a state of one selection, not of the view.
        self.rooms.sel.get_or_insert(Chan::Room("host".into()));
        self.rooms.next_poll_ms = self.now_ms.saturating_add(POLL_MS);
        self.rooms_ask_marks();
        let mut ops = self.rooms.poll_ops().into_iter();
        let first = ops.next()?;
        self.queued
            .extend(ops.map(|op| Action::RoomChat { op, id: None }));
        Some(Action::RoomChat {
            op: first,
            id: None,
        })
    }

    /// Ask the daemon, once, how far the operator has read each channel: a head that attaches
    /// later starts with no marks of its own and would count everything unread again.
    fn rooms_ask_marks(&mut self) {
        if !std::mem::replace(&mut self.rooms.marks_asked, true) {
            self.queued.push(Action::RoomChat {
                op: json!({"type": "marks"}),
                id: None,
            });
        }
    }

    /// Ask again when the interval is up. An open view asks for everything it draws. A closed
    /// one asks for the room list alone, so the unread count stays current.
    pub(crate) fn rooms_poll(&mut self) {
        if self.now_ms < self.rooms.next_poll_ms {
            return;
        }
        if !self.is_open(Surface::Rooms) {
            self.rooms.next_poll_ms = self.now_ms.saturating_add(CLOSED_POLL_MS);
            self.rooms_ask_marks();
            self.queued.push(Action::RoomChat {
                op: json!({"type": "rooms"}),
                id: None,
            });
            return;
        }
        self.rooms.next_poll_ms = self.now_ms.saturating_add(POLL_MS);
        let ops = self.rooms.poll_ops();
        self.queued
            .extend(ops.into_iter().map(|op| Action::RoomChat { op, id: None }));
    }

    /// The daemon's answer to a chat request. `id` is the request the answer belongs to: a
    /// send's answer is told from a poll's by it, and no poll may clear a send's record.
    /// `/invite NAME [ROOM]` and `/leave NAME [ROOM]`: a session in or out of a project room that is
    /// not its own. ROOM is `project/<key>` or a label, and defaults to the project room selected in
    /// `/rooms`.
    pub(crate) fn room_guest(&mut self, verb: &str, args: &str) -> Option<Action> {
        let mut words = args.split_whitespace();
        let (Some(who), room) = (words.next(), words.next()) else {
            self.say(&format!(
                "/{verb} SESSION [ROOM] - ROOM is `project/<key>` or its label, or the project \
                 room selected in /rooms"
            ));
            return None;
        };
        let room = match room {
            Some(r) => r.to_string(),
            None => match self.rooms.selected() {
                Some(Chan::Room(r)) if r.starts_with("project/") => r,
                _ => {
                    self.say(&format!(
                        "/{verb} {who} needs a room: name it, or select a project room in /rooms"
                    ));
                    return None;
                }
            },
        };
        let id = format!("{verb}:{who}:{room}");
        Some(Action::RoomChat {
            op: json!({"type": verb, "room": room, "session": who}),
            id: Some(id),
        })
    }

    pub(crate) fn on_room_chat(&mut self, id: &str, reply: Value) {
        // The pictures' answers - a blob's parts, and the id an upload became - are told by their
        // own reply type and belong to the cache, not to the channel a poll was about.
        if self.on_room_blob(&reply) {
            return;
        }
        // An invite's or a leave's outcome is said where it was typed, the pane open or not.
        if let Some((verb, rest)) = id.split_once(':')
            && matches!(verb, "invite" | "leave")
        {
            let (who, room) = rest.split_once(':').unwrap_or((rest, ""));
            let said = match reply.get("why").and_then(Value::as_str) {
                Some(why) => format!("/{verb} {who} {room} was refused: {why}"),
                None if verb == "invite" => format!("{who} is a guest of {room} until it leaves"),
                None => format!("{who} has left {room}"),
            };
            self.say(&said);
        }
        if let Some(e) = reply.get("error").and_then(Value::as_str) {
            // A send that never reached the coordinator keeps its line with the reason on it;
            // the footer is for the poll's own trouble, which no send owns.
            if self.rooms.send_failed(id, format!("not sent: {e}")) {
                return;
            }
            if let Some(room) = id.strip_prefix(OLDER) {
                self.rooms.paging.remove(room);
            }
            self.rooms.error = Some(e.to_string());
            return;
        }
        let rows = |v: &Value| -> Value { v.get("rows").cloned().unwrap_or(Value::Null) };
        match reply.get("type").and_then(Value::as_str).unwrap_or("") {
            "rooms" => {
                if let Ok(r) = serde_json::from_value(rows(&reply)) {
                    self.rooms.rows = r;
                    // A room the coordinator dropped takes its channel with it. Say so and
                    // select nothing: the next line must not land on whichever channel took
                    // its place in the list.
                    let gone = self
                        .rooms
                        .sel
                        .as_ref()
                        .is_some_and(|c| !self.rooms.channels().contains(c));
                    if gone {
                        let name = self.rooms_name(self.rooms.sel.as_ref().expect("just checked"));
                        self.rooms.gone = Some(name);
                        self.rooms.sel = None;
                    }
                    self.rooms.heard = true;
                    self.rooms.error = None;
                }
            }
            "history" => {
                let room = reply.get("room").and_then(Value::as_str).unwrap_or("");
                if let Ok(r) = serde_json::from_value::<Vec<Msg>>(rows(&reply)) {
                    if id.starts_with(OLDER) {
                        self.rooms.paging.remove(room);
                        if r.is_empty() {
                            self.rooms.oldest_known.insert(room.to_string());
                        }
                    }
                    self.rooms.merge(room, r);
                    self.rooms.heard = true;
                    self.rooms.error = None;
                    self.rooms_shown();
                }
            }
            "roster" => {
                let room = reply.get("room").and_then(Value::as_str).unwrap_or("");
                if let Ok(r) = serde_json::from_value(rows(&reply)) {
                    self.rooms.roster.insert(room.to_string(), r);
                }
            }
            "marks" => {
                if let Some(m) = reply.get("marks").and_then(Value::as_object) {
                    for (key, seq) in m {
                        let seq = seq.as_u64().unwrap_or(0);
                        let at = self.rooms.seen.entry(key.clone()).or_insert(0);
                        *at = (*at).max(seq);
                    }
                }
            }
            "said" => {
                // Only this send's record goes. The footer error is not cleared here: it may be
                // another send's, and the poll this fires will answer for itself.
                let said = reply.get("id").and_then(Value::as_str).unwrap_or("");
                self.rooms.send_landed(said);
                self.rooms.next_poll_ms = 0;
            }
            "refused" => {
                let why = reply.get("why").and_then(Value::as_str).unwrap_or("?");
                let refused = reply.get("id").and_then(Value::as_str).unwrap_or("");
                if !self.rooms.send_failed(refused, format!("refused: {why}")) {
                    self.rooms.error = Some(format!("refused: {why}"));
                }
            }
            _ => {}
        }
    }

    /// Ask for the page before the oldest message on screen, once, when the chat is scrolled to
    /// its first line.
    pub(crate) fn rooms_page_back(&mut self) {
        if !self.rooms.at_top {
            return;
        }
        let Some(Chan::Room(room)) = self.rooms.selected() else {
            return;
        };
        if let Some(op) = self.rooms.older_op(&room) {
            self.rooms.paging.insert(room.clone());
            self.queued.push(Action::RoomChat {
                op,
                id: Some(format!("{OLDER}{room}")),
            });
        }
    }

    /// What the selected channel shows is seen, and the inbox cursor follows when it can.
    pub(crate) fn rooms_shown(&mut self) {
        if !self.is_open(Surface::Rooms) {
            return;
        }
        // Both marks are taken BEFORE what is drawn is marked seen: the divider's place is the mark
        // from when the channel was opened, and the news starts after what the reader had at the
        // tail. Taken after, both would already be the channel's newest message.
        self.rooms_opened_note();
        self.rooms_tail_note();
        let key = self.rooms.selected().map(|c| RoomsView::key(&c));
        let before = key.as_ref().and_then(|k| self.rooms.seen.get(k).copied());
        let acked = self.rooms.shown();
        // The mark goes to the daemon even when the cursor is held back by another channel: the
        // cursor is one number for the whole log, the mark is this channel's.
        if let Some(k) = key
            && let Some(seq) = self.rooms.seen.get(&k).copied()
            && Some(seq) != before
        {
            self.queued.push(Action::RoomChat {
                op: json!({"type": "mark", "key": k, "seq": seq}),
                id: None,
            });
        }
        if let Some(seq) = acked {
            self.queued.push(Action::RoomChat {
                op: json!({"type": "ack", "seq": seq}),
                id: None,
            });
        }
    }

    /// A channel's name in a sentence: the room id a project channel is known by, the host room
    /// as itself, a DM by its peer.
    pub(crate) fn rooms_name(&self, c: &Chan) -> String {
        match c {
            Chan::Room(id) => match id.strip_prefix("project/") {
                Some(_) => format!("#{}", self.rooms.room_label(id)),
                None => id.clone(),
            },
            Chan::Dm(p) => format!("dm with {p}"),
            Chan::Session(id) => self.session_label(id),
        }
    }

    /// A submitted line while the view is up: a message to the selected channel.
    pub(crate) fn rooms_say(&mut self, text: &str) -> Option<Action> {
        let (thread, body) = split_reply(text.trim());
        if body.is_empty() {
            return None;
        }
        if let Some(Chan::Session(id)) = self.rooms.selected() {
            self.set_composer(text.trim());
            self.say(&format!(
                "{} is a session row - d opens its DM, enter switches to it",
                self.session_label(&id)
            ));
            return None;
        }
        let Some(chan) = self.rooms.selected() else {
            let which = self.rooms.gone.clone().unwrap_or_else(|| "the room".into());
            // The line is held, not swallowed: nothing was sent, so the words are still the
            // operator's to send somewhere real.
            self.set_composer(text.trim());
            self.say(&format!(
                "{which} is gone, so nothing is selected - up or down picks a room"
            ));
            return None;
        };
        self.rooms.said += 1;
        if self.rooms.tag.is_empty() {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.subsec_nanos());
            self.rooms.tag = format!("{:x}{nanos:x}", std::process::id());
        }
        let id = format!("op-{}-{}-{}", self.rooms.tag, self.now_ms, self.rooms.said);
        self.rooms.sends.push(Send {
            id: id.clone(),
            chan: chan.clone(),
            thread,
            text: body.to_string(),
            error: None,
        });
        let op = match chan {
            Chan::Room(room) => {
                json!({"type": "say", "id": id, "room": room, "text": body, "thread": thread})
            }
            Chan::Dm(to) | Chan::Session(to) => {
                json!({"type": "dm", "id": id, "to": to, "text": body, "thread": thread})
            }
        };
        self.rooms.next_poll_ms = 0;
        self.scroll_rooms_to_tail();
        Some(Action::RoomChat { op, id: Some(id) })
    }

    /// Enter on an empty composer with a failed line in the selected channel: send it again
    /// under a new id, superseding the old record. A view binding rather than the composer's,
    /// because the editor answers an empty Enter with `Idle` before `submit` is called.
    pub(crate) fn rooms_resend_send(&mut self) -> ControlFlow<Option<Action>> {
        if let Some(Chan::Session(id)) = self.rooms.selected() {
            return ControlFlow::Break(self.rooms_switch_session(id));
        }
        let act = self
            .rooms
            .selected()
            .and_then(|chan| self.rooms.failed_of(&chan).cloned())
            .and_then(|failed| {
                let text = match failed.thread {
                    Some(t) => format!(">{t} {}", failed.text),
                    None => failed.text,
                };
                self.rooms.sends.retain(|s| s.id != failed.id);
                self.rooms_say(&text)
            });
        ControlFlow::Break(act)
    }

    /// `d` on an empty composer: drop the newest failed line of the selected channel. The
    /// operator has read the reason and chosen not to send it again.
    pub(crate) fn rooms_dismiss_send(&mut self) -> ControlFlow<Option<Action>> {
        if let Some(Chan::Session(id)) = self.rooms.selected() {
            self.rooms_open_dm(id);
            return ControlFlow::Break(None);
        }
        if let Some(chan) = self.rooms.selected()
            && let Some(failed) = self.rooms.failed_of(&chan).map(|s| s.id.clone())
        {
            self.rooms.sends.retain(|s| s.id != failed);
        }
        ControlFlow::Break(None)
    }

    /// `/dm NAME TEXT`: a direct message from anywhere, with the view opened on it.
    pub(crate) fn rooms_dm(&mut self, rest: &str) -> Option<Action> {
        let (name, text) = match rest.split_once(char::is_whitespace) {
            Some((n, t)) => (n.trim().trim_start_matches('@'), t.trim()),
            None => (rest.trim().trim_start_matches('@'), ""),
        };
        if name.is_empty() || text.is_empty() {
            self.say("/dm NAME TEXT - a direct message, posted as operator");
            return None;
        }
        if !self.is_open(Surface::Rooms) {
            let open = self.open_surface(Surface::Rooms);
            self.queued.extend(open);
        }
        self.rooms.fresh.insert(name.to_string());
        self.rooms.sel = Some(Chan::Dm(name.to_string()));
        self.rooms.gone = None;
        self.rooms_say(text)
    }

    /// Move to the next or previous channel. A move is explicit, so from nothing selected it
    /// lands on the first channel (down) or the last (up).
    pub(crate) fn rooms_step(&mut self, up: bool) {
        let all = self.rooms.channels();
        let n = all.len();
        let at = self
            .rooms
            .selected()
            .and_then(|c| all.iter().position(|x| *x == c));
        let next = match at {
            Some(i) if up => (i + n - 1) % n,
            Some(i) => (i + 1) % n,
            None if up => n - 1,
            None => 0,
        };
        self.rooms.sel = Some(all[next].clone());
        self.rooms.gone = None;
        self.scroll_rooms_to_tail();
        self.rooms.next_poll_ms = 0;
        self.rooms_shown();
    }

    /// Keep the Sessions section in step with the session list: the operator's own top-level
    /// sessions the daemon holds, with the project folder and what each is doing now.
    pub(crate) fn rooms_sync_sessions(&mut self) {
        let mine: Vec<_> = self
            .sessions
            .iter()
            // The one test the session picker and `--continue` use, so none of them can differ.
            .filter(|b| {
                b.live
                    && letibot_sessionlog::is_operator_top_level(&letibot_sessionlog::GroupRow {
                        id: &b.session_id,
                        parent: b.parent_session_id.as_deref(),
                        origin: b.origin,
                        role: None,
                        title: &b.title,
                    })
            })
            .map(|b| rooms::SessInfo {
                id: b.session_id.clone(),
                name: self.session_label(&b.session_id),
                project: std::path::Path::new(&b.wiring.workspace)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                state: if b.waits_on_operator > 0 {
                    "asking"
                } else if b.status.running {
                    "running"
                } else {
                    "idle"
                },
            })
            .collect();
        self.rooms.sessions = mine;
        // The queue's own sessions: a queue-origin session and whatever it spawned (its reviewers).
        let queue: BTreeSet<String> = self
            .sessions
            .iter()
            .filter(|b| {
                let mut cur = Some(*b);
                for _ in 0..8 {
                    let Some(s) = cur else { break };
                    if s.origin == letibot_sessionlog::SessionOrigin::Queue {
                        return true;
                    }
                    cur = s
                        .parent_session_id
                        .as_deref()
                        .and_then(|p| self.sessions.iter().find(|x| x.session_id == p));
                }
                false
            })
            .map(|b| b.session_id.clone())
            .collect();
        self.rooms.queue_authors = queue;
    }

    /// Enter on a session row: this head moves to that session, as picking it in the session list.
    pub(crate) fn rooms_switch_session(&mut self, id: String) -> Option<Action> {
        // Going to a session leaves the rooms view, including the session already on screen: the
        // transcript is what is behind the view.
        self.close_surface(Surface::Rooms);
        self.sidebar.focus = false;
        if id == self.session_id {
            return None;
        }
        self.switch_to(id)
    }

    /// `d` on a session row: the DM with it.
    pub(crate) fn rooms_open_dm(&mut self, id: String) {
        self.rooms.fresh.insert(id.clone());
        self.rooms_go(Chan::Dm(id));
    }

    /// Enter a channel: select it and start reading it at the tail. The switcher's Enter and a
    /// click on a channel's name are this one act.
    pub(crate) fn rooms_go(&mut self, c: Chan) {
        self.rooms.sel = Some(c);
        self.rooms.gone = None;
        // A channel is a different stream: the cursor, the search and the tail mark all belong
        // to the one being left.
        self.rooms_cursor_set(None);
        self.nav.search = None;
        self.rooms_to_tail();
        self.rooms.next_poll_ms = 0;
        self.rooms_shown();
    }

    /// The roster always shows the connected members. `r` adds or drops the offline ones.
    pub(crate) fn rooms_toggle_roster(&mut self) -> ControlFlow<Option<Action>> {
        self.rooms.roster_all = !self.rooms.roster_all;
        ControlFlow::Break(None)
    }

    /// A click on a member: `@handle ` into the composer, after a space when words precede it.
    pub(crate) fn rooms_mention(&mut self, k: usize) {
        let Some((_, handle)) = self.rooms.member_rows.get(k).cloned() else {
            return;
        };
        let text = self.editor.text();
        let lead = if text.is_empty() || text.ends_with(char::is_whitespace) {
            ""
        } else {
            " "
        };
        self.editor.insert(&format!("{lead}@{handle} "));
    }

    fn scroll_rooms_to_tail(&mut self) {
        self.pane_scroll = usize::MAX;
        self.rooms.prev_max = 0;
    }
}
