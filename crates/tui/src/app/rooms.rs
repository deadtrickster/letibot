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

/// Messages kept per room here, the coordinator's own ring size.
const KEEP: usize = 500;

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
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(crate) struct ChatRef {
    pub kind: String,
    pub id: String,
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
}

/// One entry of the channel list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Chan {
    /// `host` or `project/<key>`.
    Room(String),
    /// Direct messages with one principal.
    Dm(String),
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
    /// The selected channel by identity, never by position: a poll replaces `rows` wholesale,
    /// so a room arriving ahead of the current one would move an index and send the next line
    /// to another channel.
    pub sel: Option<Chan>,
    /// The channel that vanished while selected, named so the view can say so. `None` until a
    /// poll drops the selected channel, cleared by the next explicit move.
    pub gone: Option<String>,
    pub roster_open: bool,
    pub roster: BTreeMap<String, Vec<Member>>,
    pub next_poll_ms: u64,
    pub error: Option<String>,
    /// The inbox cursor this head last set.
    pub acked: u64,
    pub said: u64,
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
        out.extend(self.peers().into_iter().map(Chan::Dm));
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
        }
    }

    pub(crate) fn messages(&self, c: &Chan) -> Vec<&Msg> {
        match c {
            Chan::Room(id) => self.msgs.get(id).into_iter().flatten().collect(),
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
                .map_or(0, |r| r.unread),
            Chan::Dm(_) => self
                .messages(c)
                .iter()
                .filter(|m| m.author != OPERATOR && m.seq > seen)
                .count() as u64,
        }
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

    fn merge(&mut self, room: &str, rows: Vec<Msg>) {
        let cursor = self.read_to.entry(room.to_string()).or_insert(0);
        let list = self.msgs.entry(room.to_string()).or_default();
        for m in rows {
            *cursor = (*cursor).max(m.seq);
            if list.iter().all(|x| x.seq != m.seq) {
                list.push(m);
            }
        }
        list.sort_by_key(|m| m.seq);
        if list.len() > KEEP {
            list.drain(..list.len() - KEEP);
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
                if self.roster_open {
                    ops.push(json!({"type": "roster", "room": id}));
                }
            }
            Some(Chan::Dm(_)) => ops.push(self.read_op("dm")),
            None => {}
        }
        ops
    }

    /// Mark the selected channel shown, and say how far the inbox cursor may move. The cursor is
    /// one number for every channel, so it stays put while a message nobody has seen sits past it.
    pub(crate) fn shown(&mut self) -> Option<u64> {
        let c = self.selected()?;
        let top = self.messages(&c).last().map(|m| m.seq)?;
        self.seen.insert(Self::key(&c), top);
        let held_room = self.rows.iter().any(|r| {
            let other = Chan::Room(r.id.clone());
            other != c && self.unread(&other) > 0 && r.last_seq > top
        });
        let held_dm = self.peers().into_iter().any(|p| {
            let other = Chan::Dm(p);
            other != c && self.unread(&other) > 0
        });
        (!held_room && !held_dm && top > self.acked).then(|| {
            self.acked = top;
            top
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
        let mut ops = self.rooms.poll_ops().into_iter();
        let first = ops.next()?;
        self.queued
            .extend(ops.map(|op| Action::RoomChat { op, id: None }));
        Some(Action::RoomChat {
            op: first,
            id: None,
        })
    }

    /// Ask again when the interval is up. Only a view on the stack asks.
    pub(crate) fn rooms_poll(&mut self) {
        if !self.is_open(Surface::Rooms) || self.now_ms < self.rooms.next_poll_ms {
            return;
        }
        self.rooms.next_poll_ms = self.now_ms.saturating_add(POLL_MS);
        let ops = self.rooms.poll_ops();
        self.queued
            .extend(ops.into_iter().map(|op| Action::RoomChat { op, id: None }));
    }

    /// The daemon's answer to a chat request. `id` is the request the answer belongs to: a
    /// send's answer is told from a poll's by it, and no poll may clear a send's record.
    pub(crate) fn on_room_chat(&mut self, id: &str, reply: Value) {
        self.redraw = true;
        if let Some(e) = reply.get("error").and_then(Value::as_str) {
            // A send that never reached the coordinator keeps its line with the reason on it;
            // the footer is for the poll's own trouble, which no send owns.
            if self.rooms.send_failed(id, format!("not sent: {e}")) {
                return;
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
                        let name = Self::rooms_name(self.rooms.sel.as_ref().expect("just checked"));
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

    /// What the selected channel shows is seen, and the inbox cursor follows when it can.
    pub(crate) fn rooms_shown(&mut self) {
        if !self.is_open(Surface::Rooms) {
            return;
        }
        if let Some(seq) = self.rooms.shown() {
            self.queued.push(Action::RoomChat {
                op: json!({"type": "ack", "seq": seq}),
                id: None,
            });
        }
    }

    /// A channel's name in a sentence: the room id a project channel is known by, the host room
    /// as itself, a DM by its peer.
    pub(crate) fn rooms_name(c: &Chan) -> String {
        match c {
            Chan::Room(id) => match id.strip_prefix("project/") {
                Some(key) => format!("#{key}"),
                None => id.clone(),
            },
            Chan::Dm(p) => format!("dm with {p}"),
        }
    }

    /// A submitted line while the view is up: a message to the selected channel.
    pub(crate) fn rooms_say(&mut self, text: &str) -> Option<Action> {
        let (thread, body) = split_reply(text.trim());
        if body.is_empty() {
            return None;
        }
        let Some(chan) = self.rooms.selected() else {
            let which = self
                .rooms
                .gone
                .clone()
                .unwrap_or_else(|| "the channel".into());
            // The line is held, not swallowed: nothing was sent, so the words are still the
            // operator's to send somewhere real.
            self.set_composer(text.trim());
            self.say(&format!(
                "{which} is gone, so nothing is selected - up or down picks a channel"
            ));
            return None;
        };
        self.rooms.said += 1;
        let id = format!("op-{}-{}", self.now_ms, self.rooms.said);
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
            Chan::Dm(to) => {
                json!({"type": "dm", "id": id, "to": to, "text": body, "thread": thread})
            }
        };
        self.rooms.next_poll_ms = 0;
        self.scroll_rooms_to_tail();
        self.redraw = true;
        Some(Action::RoomChat { op, id: Some(id) })
    }

    /// Enter on an empty composer with a failed line in the selected channel: send it again
    /// under a new id. The old record is superseded by its own resend, so it goes. A key of
    /// the view rather than the composer's empty line - the editor answers Enter on nothing
    /// typed with `Idle`, and the binding stands behind the decision ladder either way.
    pub(crate) fn rooms_resend_send(&mut self) -> ControlFlow<Option<Action>> {
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
        if act.is_some() {
            self.redraw = true;
        }
        ControlFlow::Break(act)
    }

    /// `d` on an empty composer: drop the newest failed line of the selected channel. The
    /// operator has read the reason and chosen not to send it again.
    pub(crate) fn rooms_dismiss_send(&mut self) -> ControlFlow<Option<Action>> {
        if let Some(chan) = self.rooms.selected()
            && let Some(failed) = self.rooms.failed_of(&chan).map(|s| s.id.clone())
        {
            self.rooms.sends.retain(|s| s.id != failed);
            self.redraw = true;
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
        self.redraw = true;
    }

    /// The roster of the selected channel, on or off.
    pub(crate) fn rooms_toggle_roster(&mut self) -> ControlFlow<Option<Action>> {
        self.rooms.roster_open = !self.rooms.roster_open;
        self.rooms.next_poll_ms = 0;
        self.redraw = true;
        ControlFlow::Break(None)
    }

    fn scroll_rooms_to_tail(&mut self) {
        self.pane_scroll = usize::MAX;
        self.rooms.prev_max = 0;
    }
}
