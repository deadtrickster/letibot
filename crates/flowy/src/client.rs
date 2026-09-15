//! The node's API, as the connector uses it. Bearer-authenticated, blocking,
//! over [`letibot_http`].
//!
//! Every method here is a door that exists on the node today (`GET /api/node`'s
//! route table, checked against build `0.8.0+d1f2415` on 2026-09-14). Nothing is
//! written against an endpoint that was asked for and has not landed —
//! `docs/flowy-monitor.md` lists those separately, and the seat is built to the
//! long-poll shape the node has.
//!
//! # Three kinds of failure, kept apart
//!
//! [`NodeError::Unreachable`] is the encoder gone (closed loop §5): the seat goes
//! stalled and says so. [`NodeError::Refused`] is the node answering *no* — a bad
//! token, a room that is not yours, an argument it did not like — and it carries
//! the node's own sentence, because flowy's refusals are written to be read.
//! [`NodeError::NoReader`] is the one refusal a listener must treat specially:
//! the same sentence appears when a token has been SWITCHED, and re-declaring
//! there loses every message since the switch. It carries the labels that do
//! exist so the caller can see which case it is in.

use std::time::Duration;

use letibot_http::{Endpoint, HttpError, Request, SseFrame};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The node's long-poll ceiling is 25 s; flowy's own CLI asks for 20 and this
/// asks for the same, so a dead node is noticed within one window.
pub const POLL_WINDOW: Duration = Duration::from_secs(20);

/// One event, as the node delivers it. The fields the connector reads; the rest
/// stays in `raw` so a rendering can show what it did not model.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Event {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub room: String,
    #[serde(default)]
    pub thread: String,
    #[serde(default)]
    pub actor: String,
    #[serde(default)]
    pub artifact: String,
    #[serde(default)]
    pub seq_hlc: i64,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub meta: Value,
    #[serde(default)]
    pub addressee: String,
    #[serde(default)]
    pub addressee_name: String,
    #[serde(default)]
    pub private: bool,
    #[serde(default)]
    pub created: String,
    #[serde(default)]
    pub citation: Option<Value>,
    #[serde(default)]
    pub standing: Option<Value>,
    #[serde(default)]
    pub disowned: Option<Value>,
}

impl Event {
    fn meta_str(&self, key: &str) -> &str {
        self.meta.get(key).and_then(|v| v.as_str()).unwrap_or("")
    }

    /// `meta.actor_kind`, stamped by the node from the token — a client cannot
    /// claim to be a person.
    pub fn actor_kind(&self) -> &str {
        self.meta_str("actor_kind")
    }

    pub fn actor_name(&self) -> &str {
        self.meta_str("actor_name")
    }

    /// `meta.actor_kind == "user"`: said by a person, according to the node.
    pub fn said_by_a_person(&self) -> bool {
        self.actor_kind() == "user"
    }

    /// For a `todo.note`: whom the row was assigned to when the note was written.
    pub fn note_assignee(&self) -> &str {
        self.meta_str("assignee")
    }

    /// For a `todo.note`: who raised the row.
    pub fn note_raiser(&self) -> &str {
        self.meta_str("raiser")
    }

    /// The thread this event belongs to: its own `thread`, or itself when it is
    /// a root that others reply under.
    pub fn thread_or_self(&self) -> &str {
        if self.thread.is_empty() {
            &self.id
        } else {
            &self.thread
        }
    }
}

/// What `GET /api/inbox/wait` answers.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct InboxPage {
    #[serde(default)]
    pub reader: String,
    #[serde(default)]
    pub events: Vec<Event>,
    /// How many went past this waiter that it was not woken for — flowy's
    /// `Skipped`, which is what makes a broken filter look different from a
    /// quiet room.
    #[serde(default)]
    pub skipped: i64,
    #[serde(default)]
    pub since: i64,
    /// Where the log had got to when the poll answered. The mark to ack.
    #[serde(default)]
    pub cursor: i64,
    /// The NODE'S clock, RFC3339. Every rendering carries it beside the message
    /// time, because an agent session carries its start date for hours.
    #[serde(default)]
    pub now: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Reader {
    #[serde(default)]
    pub reader: String,
    #[serde(default)]
    pub cursor: i64,
    #[serde(default)]
    pub acked_delivery: i64,
    #[serde(default)]
    pub acked_quiet: i64,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct WhoAmI {
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub agent: String,
    #[serde(default)]
    pub agent_kind: String,
    #[serde(default)]
    pub project: String,
}

/// A row on the node: a todo, a memory, a diagram, a decision. Only what a
/// change-watch compares.
#[derive(Debug, Clone, Deserialize, Default, PartialEq)]
pub struct Artifact {
    #[serde(default)]
    pub id: String,
    /// The node's `type`: `memory`, `todo`, `report`, `diagram`…
    #[serde(rename = "type", default)]
    pub type_: String,
    /// The node's `kind`, a finer label inside a type: a skill is a `memory` of
    /// kind `skill`. The shelf keys on this, not on `type`.
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub assignee: String,
    #[serde(default)]
    pub visibility: String,
    #[serde(default)]
    pub updated: String,
}

/// What `GET /api/nag` answers: the seat's board state, as counts with the ids
/// behind them.
///
/// **Read off the node, not off the docs** (build `0.8.0+d1f2415`, 2026-09-15).
/// A note asking for this work said `stale` was the one count with no `_ids`
/// companion; the node returns `stale_ids` like every other. Field names here
/// were taken from an actual response body, because the only in-tree mention of
/// them describes the CLI rather than the API.
///
/// Every field is `#[serde(default)]`: a node that grows a count must not break
/// a seat, and one that drops a count must leave the seat reading zero rather
/// than failing to parse.
#[derive(Debug, Clone, Deserialize, Default, PartialEq)]
pub struct Nag {
    /// Open rows assigned to this seat.
    #[serde(default)]
    pub mine: i64,
    #[serde(default)]
    pub open: i64,
    #[serde(default)]
    pub unowned: i64,
    /// Assigned to me and still to do — the bucket a seat is nagged about.
    #[serde(default)]
    pub mine_todo: i64,
    #[serde(default)]
    pub mine_todo_ids: Vec<String>,
    #[serde(default)]
    pub mine_waiting: i64,
    #[serde(default)]
    pub mine_waiting_ids: Vec<String>,
    /// Questions this seat owes somebody an answer to.
    #[serde(default)]
    pub answers_owed: i64,
    #[serde(default)]
    pub answers_owed_ids: Vec<String>,
    #[serde(default)]
    pub unowned_waiting: i64,
    #[serde(default)]
    pub unowned_waiting_ids: Vec<String>,
    /// Mine, untouched for longer than `stale_after_seconds`.
    #[serde(default)]
    pub stale: i64,
    #[serde(default)]
    pub stale_ids: Vec<String>,
    #[serde(default)]
    pub stale_after_seconds: i64,
}

/// What the server-side filter is asked for on a poll: flowy's own three
/// levels, sent as its two flags. The seat sends the LOOSEST level any attached
/// session wants, and [`crate::attention`] narrows per room after delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerFilter {
    pub addressed: bool,
    pub mentions: bool,
}

/// The process behind a poll, reported so a repair can name a pid rather than
/// match a command line — see flowy's `store.WaiterProcessOf`.
#[derive(Debug, Clone)]
pub struct WaiterProcess {
    pub pid: u32,
    /// RFC3339, when this process started polling.
    pub since: String,
    pub host: String,
}

#[derive(Debug)]
pub enum NodeError {
    /// Could not connect, or the connection died mid-answer. The encoder is gone.
    Unreachable(String),
    /// The node answered and said no.
    Refused { code: u16, message: String },
    /// `no inbox reader called NAME` — with the labels that DO exist.
    NoReader { name: String, known: Vec<String> },
    /// 2xx, but not the JSON expected.
    Malformed(String),
}

impl std::fmt::Display for NodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NodeError::Unreachable(m) => write!(f, "node unreachable: {m}"),
            NodeError::Refused { code, message } => write!(f, "node refused ({code}): {message}"),
            NodeError::NoReader { name, known } => write!(
                f,
                "no inbox reader called `{name}` for this token (readers here: {}). Read \
                 this before re-declaring: the same sentence appears when the token has \
                 been SWITCHED, and the old identity is still holding every message since",
                if known.is_empty() {
                    "none declared yet".to_string()
                } else {
                    known.join(", ")
                }
            ),
            NodeError::Malformed(m) => write!(f, "node answered something unexpected: {m}"),
        }
    }
}

impl std::error::Error for NodeError {}

impl NodeError {
    pub fn is_unreachable(&self) -> bool {
        matches!(self, NodeError::Unreachable(_))
    }
}

/// One node, one token.
#[derive(Debug, Clone)]
pub struct Node {
    pub endpoint: Endpoint,
    token: String,
    pub addr: String,
}

impl Node {
    pub fn new(endpoint: Endpoint, addr: impl Into<String>, token: impl Into<String>) -> Node {
        Node {
            endpoint,
            token: token.into(),
            addr: addr.into(),
        }
    }

    /// Swap the credential — a re-mint noticed by the seat loop.
    pub fn with_token(&self, token: impl Into<String>) -> Node {
        Node {
            endpoint: self.endpoint.clone(),
            token: token.into(),
            addr: self.addr.clone(),
        }
    }

    fn auth(&self) -> String {
        format!("Bearer {}", self.token)
    }

    fn call(&self, req: Request<'_>) -> Result<Value, NodeError> {
        let auth = self.auth();
        let mut headers: Vec<(&str, &str)> = vec![("Authorization", &auth)];
        headers.extend_from_slice(req.headers);
        let req = Request {
            headers: &headers,
            ..req
        };
        let body = letibot_http::send(&self.endpoint, req).map_err(map_http)?;
        let text = body.read_to_string().map_err(map_http)?;
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).map_err(|e| NodeError::Malformed(format!("{e}: {text}")))
    }

    fn get(&self, path: &str) -> Result<Value, NodeError> {
        self.call(Request::get(path))
    }

    fn post(&self, path: &str, body: &Value) -> Result<Value, NodeError> {
        let s = body.to_string();
        self.call(Request::post_json(path, &s))
    }

    pub fn whoami(&self) -> Result<WhoAmI, NodeError> {
        parse(self.get("/api/whoami")?)
    }

    /// `GET /api/node`: the build and the route table.
    pub fn node(&self) -> Result<Value, NodeError> {
        self.get("/api/node")
    }

    /// `GET /api/nag`: the seat's own board STATE — what is assigned, waiting,
    /// owed and stale right now.
    ///
    /// The one read in this client that is not about something that HAPPENED.
    /// `inbox_wait` answers *what was said since my cursor*; this answers *what
    /// is true about my queue*, and a queue that has been full for an hour
    /// generates no events at all. See [`crate::seat::NagState`] for why that
    /// distinction is the whole point.
    pub fn nag(&self) -> Result<Nag, NodeError> {
        parse(self.get("/api/nag")?)
    }

    pub fn readers(&self) -> Result<Vec<Reader>, NodeError> {
        let v = self.get("/api/inbox/readers")?;
        parse(v.get("readers").cloned().unwrap_or(Value::Array(vec![])))
    }

    /// Declare a reader at the head of the log. Explicit, never implied by a
    /// wait — see [`NodeError::NoReader`].
    pub fn declare_reader(&self, name: &str) -> Result<Reader, NodeError> {
        parse(self.post("/api/inbox/reader", &json!({"as": name, "kind": "tracked"}))?)
    }

    /// Block up to [`POLL_WINDOW`] for something this reader should hear.
    ///
    /// `focus` is the project everything is delivered from; elsewhere, only what
    /// names the seat. `Some("")` is not sent.
    pub fn inbox_wait(
        &self,
        reader: &str,
        filter: ServerFilter,
        focus: Option<&str>,
        proc: Option<&WaiterProcess>,
    ) -> Result<InboxPage, NodeError> {
        let mut q = format!(
            "/api/inbox/wait?as={}&window={}&kind=tracked",
            enc(reader),
            POLL_WINDOW.as_secs()
        );
        if filter.mentions {
            q.push_str("&mentions=1");
        }
        if filter.addressed {
            q.push_str("&addressed=1");
        }
        if let Some(f) = focus.filter(|f| !f.is_empty()) {
            q.push_str(&format!("&focus={}", enc(f)));
        }
        if let Some(p) = proc {
            q.push_str(&format!(
                "&pid={}&since={}&host={}",
                p.pid,
                enc(&p.since),
                enc(&p.host)
            ));
        }
        match self.get(&q) {
            Ok(v) => parse(v),
            Err(NodeError::Refused { code: 404, message })
                if message.contains("no inbox reader") =>
            {
                Err(NodeError::NoReader {
                    name: reader.to_string(),
                    known: readers_in(&message),
                })
            }
            Err(e) => Err(e),
        }
    }

    /// Move the mark. `delivered` says why: messages were handed over and
    /// written out, or the poll expired having read nothing but our own.
    pub fn inbox_ack(
        &self,
        reader: &str,
        cursor: i64,
        delivered: bool,
    ) -> Result<Reader, NodeError> {
        parse(self.post(
            "/api/inbox/ack",
            &json!({"as": reader, "cursor": cursor, "delivered": delivered}),
        )?)
    }

    /// Say something in a room. `to` is a seat or person's name; `thread` a
    /// message id to reply under.
    pub fn say(
        &self,
        room: &str,
        body: &str,
        to: Option<&str>,
        thread: Option<&str>,
    ) -> Result<Event, NodeError> {
        let mut req = json!({"body": body});
        if let Some(t) = to.filter(|t| !t.is_empty()) {
            req["to"] = json!(t);
        }
        if let Some(t) = thread.filter(|t| !t.is_empty()) {
            req["thread"] = json!(t);
        }
        let v = self.post(&format!("/api/chat/{}/say", enc(room)), &req)?;
        parse_event_reply(v)
    }

    /// A direct message: projectless, addressed, private.
    pub fn dm(&self, to: &str, body: &str, thread: Option<&str>) -> Result<Event, NodeError> {
        let mut req = json!({"body": body});
        if let Some(t) = thread.filter(|t| !t.is_empty()) {
            req["thread"] = json!(t);
        }
        let v = self.post(&format!("/api/dm/{}", enc(to)), &req)?;
        parse_event_reply(v)
    }

    /// The log above `since`, filtered. `kind` is an event type; `thread` an id.
    pub fn events_since(
        &self,
        since: i64,
        kind: Option<&str>,
        thread: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Event>, NodeError> {
        let mut q = format!("/api/events?since={since}&limit={limit}");
        if let Some(k) = kind.filter(|k| !k.is_empty()) {
            q.push_str(&format!("&type={}", enc(k)));
        }
        if let Some(t) = thread.filter(|t| !t.is_empty()) {
            q.push_str(&format!("&thread={}", enc(t)));
        }
        let v = self.get(&q)?;
        parse(v.get("events").cloned().unwrap_or(Value::Array(vec![])))
    }

    /// The last `limit` messages in a room, oldest first — the antecedents of a
    /// mention, which a delivery alone does not carry. `order=recent` is the
    /// node's word for the tail (the other is `log`, from the start); the first
    /// spelling here was `desc`, and the node refused it by name.
    pub fn room_read(&self, room: &str, limit: usize) -> Result<Vec<Event>, NodeError> {
        let v = self.get(&format!(
            "/api/chat/{}?limit={limit}&order=recent",
            enc(room)
        ))?;
        let mut events: Vec<Event> =
            parse(v.get("events").cloned().unwrap_or(Value::Array(vec![])))?;
        events.sort_by_key(|e| e.seq_hlc);
        Ok(events)
    }

    /// The rows of one kind this token can read — `kind=skill` is the shelf.
    /// `kind`, not `type`: the `type=skill` filter answers nothing, and was tried.
    pub fn artifacts_of_kind(&self, kind: &str, limit: usize) -> Result<Vec<Artifact>, NodeError> {
        let v = self.get(&format!("/api/artifacts?kind={}&limit={limit}", enc(kind)))?;
        parse(v.get("artifacts").cloned().unwrap_or(Value::Array(vec![])))
    }

    pub fn artifact(&self, id: &str) -> Result<Artifact, NodeError> {
        parse(self.get(&format!("/api/artifact/{}", enc(id)))?)
    }

    /// Hold `GET /api/stream?topics=…` open and hand every frame over. Returns
    /// when the node closes it or `on_frame` asks to stop; the caller reconnects
    /// with the last `id` it saw.
    pub fn stream<F>(&self, topics: &str, since: Option<&str>, on_frame: F) -> Result<(), NodeError>
    where
        F: FnMut(SseFrame) -> Result<letibot_http::Flow, HttpError>,
    {
        let auth = self.auth();
        let mut headers: Vec<(&str, &str)> =
            vec![("Authorization", &auth), ("Accept", "text/event-stream")];
        if let Some(s) = since {
            headers.push(("Last-Event-ID", s));
        }
        let path = format!("/api/stream?topics={}", enc(topics));
        let body = letibot_http::send(&self.endpoint, Request::get(&path).with_headers(&headers))
            .map_err(map_http)?;
        body.for_each_frame(on_frame).map_err(map_http)
    }
}

fn parse<T: for<'de> Deserialize<'de>>(v: Value) -> Result<T, NodeError> {
    serde_json::from_value(v.clone()).map_err(|e| NodeError::Malformed(format!("{e}: {v}")))
}

/// A `say` answers with the event it wrote, sometimes wrapped as `{"event": …}`.
fn parse_event_reply(v: Value) -> Result<Event, NodeError> {
    if let Some(e) = v.get("event") {
        return parse(e.clone());
    }
    parse(v)
}

fn map_http(e: HttpError) -> NodeError {
    match e {
        HttpError::Io(io) => NodeError::Unreachable(io.to_string()),
        HttpError::Malformed(m) => NodeError::Unreachable(m),
        HttpError::Status { code, body } => {
            let message = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(String::from))
                .unwrap_or(body);
            NodeError::Refused { code, message }
        }
    }
}

/// The labels in `… readers here: a, b` — flowy's refusal carries them in prose
/// and in a `readers` array; the prose is what survives [`map_http`].
fn readers_in(message: &str) -> Vec<String> {
    let Some((_, rest)) = message.split_once("readers here:") else {
        return Vec::new();
    };
    let rest = rest.trim();
    if rest.starts_with("none") {
        return Vec::new();
    }
    rest.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Percent-encode one query value or path segment.
pub fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_refusal_hands_over_the_labels_that_exist() {
        assert_eq!(
            readers_in(
                "no inbox reader called x for this principal - declare it first with --new. readers here: a, b"
            ),
            vec!["a".to_string(), "b".to_string()]
        );
        assert!(readers_in("… readers here: none declared yet").is_empty());
    }

    #[test]
    fn an_event_reads_the_stamped_actor_kind_and_not_a_claim() {
        let e: Event = serde_json::from_value(json!({
            "id": "01X", "type": "chat", "room": "general", "actor": "u1",
            "meta": {"actor_kind": "user", "actor_name": "deadtrickster"},
            "body": "who is here?"
        }))
        .unwrap();
        assert!(e.said_by_a_person());
        assert_eq!(e.actor_name(), "deadtrickster");
        assert_eq!(e.thread_or_self(), "01X");
    }

    #[test]
    fn enc_leaves_a_name_alone_and_escapes_a_space() {
        assert_eq!(enc("claude-lab2x1"), "claude-lab2x1");
        assert_eq!(enc("a b"), "a%20b");
    }
}
