//! Claude Code in the rooms: the bridge as the external seat `claude-code`. It may not depend on
//! the room crate, so it speaks the chat wire itself (`docs/room.md`). No tool argument reaches
//! `author`, `kind` or `seat`, so it cannot post as the operator or a session.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// The handle this bridge speaks as.
pub const HANDLE: &str = "claude-code";

/// The room wire's version (`ROOM_PROTOCOL_VERSION`). A coordinator on another number refuses the
/// hello with both numbers, which reaches the tool as its error.
const PROTOCOL: u16 = 6;

const REPLY_TIMEOUT: Duration = Duration::from_secs(10);
const REDIAL_EVERY: Duration = Duration::from_secs(2);

/// What the stream hands a message to: the bridge's MCP writer.
pub type Deliver = Arc<dyn Fn(&Value) + Send + Sync>;

type Hook = Arc<dyn Fn(&Value, bool) + Send + Sync>;

/// The host key, as the room crate and `scripts/letibot` derive it.
pub fn host_key_of(raw: &str) -> String {
    let mut safe: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    safe.truncate(24);
    while safe.ends_with('-') {
        safe.pop();
    }
    if safe.is_empty() {
        safe.push_str("host");
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in raw.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{safe}-{:08x}", h as u32)
}

/// This box's coordinator socket: `$LETIBOT_ROOM_SOCKET`, else `room-<host key>.sock` in the
/// letibot data directory.
pub fn socket_path() -> PathBuf {
    if let Some(p) = std::env::var_os("LETIBOT_ROOM_SOCKET").filter(|v| !v.is_empty()) {
        return PathBuf::from(p);
    }
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .map(|h| PathBuf::from(h).join(".local/share"))
        })
        .unwrap_or_else(|| PathBuf::from("."))
        .join("letibot");
    let name = std::process::Command::new("uname")
        .arg("-n")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "localhost".to_string());
    data.join(format!("room-{}.sock", host_key_of(&name)))
}

/// How much of the room a reader wants. Mirrors the coordinator's filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attention {
    ToMe,
    Mentions,
    All,
}

impl Attention {
    pub fn parse(raw: &str) -> Result<Attention, String> {
        match raw {
            "to_me" => Ok(Attention::ToMe),
            "mentions" => Ok(Attention::Mentions),
            "all" => Ok(Attention::All),
            other => Err(format!(
                "`attention` is `to_me`, `mentions` or `all`, not `{other}`"
            )),
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Attention::ToMe => "to_me",
            Attention::Mentions => "mentions",
            Attention::All => "all",
        }
    }

    /// Does a pushed message pass? `names_you` is the coordinator's word that it addresses or
    /// mentions this seat.
    pub fn passes(self, message: &Value, names_you: bool) -> bool {
        if message["author"] == HANDLE {
            return false;
        }
        match self {
            Attention::All => true,
            Attention::Mentions => names_you,
            Attention::ToMe => {
                names_you
                    || (message["kind"] == "operator"
                        && message["to"].is_null()
                        && message["room"] != "dm")
            }
        }
    }
}

/// One message as a plain line: `#seq [room] author -> to: text (todo t-7)`.
pub fn line(m: &Value) -> String {
    let to = m["to"]
        .as_str()
        .map(|t| format!(" -> {t}"))
        .unwrap_or_default();
    let refs: String = m["refs"]
        .as_array()
        .map(|rs| {
            rs.iter()
                .map(|r| {
                    format!(
                        " ({} {})",
                        r["kind"].as_str().unwrap_or("?"),
                        r["id"].as_str().unwrap_or("?")
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    format!(
        "#{} [{}] {}{to}: {}{refs}",
        m["seq"],
        m["room"].as_str().unwrap_or("?"),
        m["author"].as_str().unwrap_or("?"),
        m["text"].as_str().unwrap_or("")
    )
}

/// The MCP channel notification for one message. `meta` keys are identifiers, as the channel
/// contract wants.
pub fn channel_notification(m: &Value) -> Value {
    let mut meta = serde_json::Map::new();
    for key in ["room", "author", "kind", "to"] {
        if let Some(s) = m[key].as_str() {
            meta.insert(key.to_string(), json!(s));
        }
    }
    meta.insert("seq".into(), json!(m["seq"].to_string()));
    if let Some(t) = m["thread"].as_u64() {
        meta.insert("thread".into(), json!(t.to_string()));
    }
    json!({
        "jsonrpc": "2.0",
        "method": "notifications/claude/channel",
        "params": { "content": line(m), "meta": meta },
    })
}

struct Live {
    writer: UnixStream,
    generation: u64,
}

struct Inner {
    path: PathBuf,
    live: Mutex<Option<Live>>,
    /// One round trip at a time: replies carry no id of their own.
    call: Mutex<()>,
    waiting: Mutex<Option<Sender<Value>>>,
    hook: Mutex<Option<Hook>>,
    generation: AtomicU64,
    ids: AtomicU64,
    /// The newest seq handed to the stream, so a replay never repeats a push.
    delivered: Mutex<u64>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Ok(live) = self.live.get_mut()
            && let Some(live) = live.take()
        {
            let _ = live.writer.shutdown(std::net::Shutdown::Both);
        }
    }
}

/// The bridge's one connection to the coordinator, opened on first use and again after it drops.
#[derive(Clone)]
pub struct RoomLink {
    inner: Arc<Inner>,
}

impl Default for RoomLink {
    fn default() -> Self {
        RoomLink::at(socket_path())
    }
}

impl RoomLink {
    pub fn at(path: impl Into<PathBuf>) -> RoomLink {
        RoomLink {
            inner: Arc::new(Inner {
                path: path.into(),
                live: Mutex::new(None),
                call: Mutex::new(()),
                waiting: Mutex::new(None),
                hook: Mutex::new(None),
                generation: AtomicU64::new(0),
                ids: AtomicU64::new(0),
                delivered: Mutex::new(0),
            }),
        }
    }

    pub fn connected(&self) -> bool {
        self.inner
            .live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    /// Hear pushed chat on the reader thread: the message and the coordinator's `names_you`.
    pub fn on_chat(&self, hook: Hook) {
        *self.inner.hook.lock().unwrap_or_else(|e| e.into_inner()) = Some(hook);
    }

    /// Open the connection if it is not open. `true` when this call opened it.
    pub fn ensure(&self) -> Result<bool, String> {
        let _one = self.inner.call.lock().unwrap_or_else(|e| e.into_inner());
        self.ensure_locked()
    }

    fn ensure_locked(&self) -> Result<bool, String> {
        if self.connected() {
            return Ok(false);
        }
        self.open()?;
        Ok(true)
    }

    fn open(&self) -> Result<(), String> {
        let path = &self.inner.path;
        let stream = UnixStream::connect(path).map_err(|e| {
            format!(
                "there is no room coordinator at {} ({e}); `letibot --room` starts one",
                path.display()
            )
        })?;
        let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
        let mut writer = stream;
        let _ = writer.set_read_timeout(Some(Duration::from_secs(2)));
        let _ = writer.set_write_timeout(Some(Duration::from_secs(2)));
        // Seat and speaker are constants: nothing a tool says reaches them.
        let hello = json!({
            "type": "hello",
            "protocol": PROTOCOL,
            "speaker": { "session": HANDLE, "parent": null },
            "room": "",
            "seat": "external",
        });
        write_line(&mut writer, &hello)?;
        let mut first = String::new();
        reader
            .read_line(&mut first)
            .map_err(|e| format!("reading from the room: {e}"))?;
        let first: Value = serde_json::from_str(first.trim())
            .map_err(|_| "the room closed the connection before saying hello".to_string())?;
        match first["type"].as_str() {
            Some("welcome") => {}
            Some("refused") => {
                return Err(format!(
                    "the room refused `{HANDLE}`: {}",
                    why(&first["why"])
                ));
            }
            Some("bye") => {
                return Err(format!(
                    "the room closed the connection: {}",
                    first["why"].as_str().unwrap_or("no reason given")
                ));
            }
            _ => return Err(format!("the room answered hello with {first}")),
        }
        let _ = writer.set_read_timeout(None);
        let _ = writer.set_write_timeout(None);
        let generation = self.inner.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let weak = Arc::downgrade(&self.inner);
        std::thread::Builder::new()
            .name("room-read".into())
            .spawn(move || read_loop(reader, weak, generation))
            .map_err(|e| e.to_string())?;
        *self.inner.live.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(Live { writer, generation });
        Ok(())
    }

    /// A fresh request id, unique to this bridge process.
    pub fn id(&self) -> String {
        let n = self.inner.ids.fetch_add(1, Ordering::SeqCst);
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        format!("cc-{}-{ms}-{n}", std::process::id())
    }

    /// One frame, one reply. A dead connection is reopened and the frame sent again once, which
    /// the coordinator's replay window makes safe for `say` and `dm`.
    pub fn request(&self, frame: &Value) -> Result<Value, String> {
        let _one = self.inner.call.lock().unwrap_or_else(|e| e.into_inner());
        let mut last = String::new();
        for _ in 0..2 {
            self.ensure_locked()?;
            match self.round_trip(frame) {
                Ok(reply) => {
                    if reply["type"] == "refused" {
                        return Err(format!("the room refused it: {}", why(&reply["why"])));
                    }
                    return Ok(reply);
                }
                Err(e) => {
                    self.drop_connection();
                    last = e;
                }
            }
        }
        Err(last)
    }

    fn round_trip(&self, frame: &Value) -> Result<Value, String> {
        let (tx, rx) = mpsc::channel();
        *self.inner.waiting.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
        let sent = {
            let mut live = self.inner.live.lock().unwrap_or_else(|e| e.into_inner());
            match live.as_mut() {
                Some(l) => write_line(&mut l.writer, frame),
                None => Err("the room connection is not open".to_string()),
            }
        };
        let got = sent.and_then(|()| {
            rx.recv_timeout(REPLY_TIMEOUT).map_err(|_| {
                format!(
                    "the room at {} did not answer in {}s",
                    self.inner.path.display(),
                    REPLY_TIMEOUT.as_secs()
                )
            })
        });
        *self.inner.waiting.lock().unwrap_or_else(|e| e.into_inner()) = None;
        got
    }

    fn drop_connection(&self) {
        let mut live = self.inner.live.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(l) = live.take() {
            let _ = l.writer.shutdown(std::net::Shutdown::Both);
        }
    }

    /// Start the stream: every pushed message that passes `to_me` goes to `deliver` once, in
    /// order. A supervisor reopens a dropped connection and replays the inbox after the gap.
    pub fn stream(&self, deliver: Deliver) {
        let weak = Arc::downgrade(&self.inner);
        let hook_weak = weak.clone();
        let live_deliver = deliver.clone();
        // A `Weak`, so the hook the link stores does not keep the link alive.
        self.on_chat(Arc::new(move |m, names_you| {
            if Attention::ToMe.passes(m, names_you)
                && let Some(inner) = hook_weak.upgrade()
            {
                RoomLink { inner }.deliver_once(m, &live_deliver);
            }
        }));
        let spawned = std::thread::Builder::new()
            .name("room-stream".into())
            .spawn(move || supervise(weak, deliver));
        if let Err(e) = spawned {
            eprintln!("letibot-mcp: the room stream did not start: {e}");
        }
    }

    fn deliver_once(&self, m: &Value, deliver: &Deliver) {
        let seq = m["seq"].as_u64().unwrap_or(0);
        let mut last = self
            .inner
            .delivered
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if seq <= *last {
            return;
        }
        *last = seq;
        deliver(m);
    }

    /// What was missed since the cursor, through `attention`. The cursor is not moved.
    pub fn inbox(&self, attention: Attention, limit: u32) -> Result<Value, String> {
        self.request(&json!({
            "type": "inbox", "attention": attention.word(), "limit": limit,
        }))
    }

    pub fn ack(&self, seq: u64) -> Result<Value, String> {
        self.request(&json!({ "type": "ack", "seq": seq }))
    }
}

fn supervise(weak: Weak<Inner>, deliver: Deliver) {
    loop {
        let Some(inner) = weak.upgrade() else {
            return;
        };
        let link = RoomLink { inner };
        if let Ok(true) = link.ensure() {
            // A gap or a first start: what the cursor still holds for this seat.
            if let Ok(reply) = link.inbox(Attention::ToMe, 200) {
                for m in reply["rows"].as_array().into_iter().flatten() {
                    link.deliver_once(m, &deliver);
                }
            }
        }
        drop(link);
        std::thread::sleep(REDIAL_EVERY);
    }
}

fn read_loop(mut reader: BufReader<UnixStream>, weak: Weak<Inner>, generation: u64) {
    let mut buf = String::new();
    loop {
        buf.clear();
        let n = reader.read_line(&mut buf).unwrap_or(0);
        let Some(inner) = weak.upgrade() else {
            return;
        };
        let frame = if n == 0 {
            None
        } else {
            serde_json::from_str::<Value>(buf.trim()).ok()
        };
        let closed = match frame {
            None => n == 0,
            Some(f) => match f["type"].as_str() {
                Some("chat") => {
                    let hook = inner.hook.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    if let Some(hook) = hook {
                        hook(&f["message"], f["names_you"].as_bool().unwrap_or(false));
                    }
                    false
                }
                Some("event") => false,
                Some("bye") => true,
                _ => {
                    let slot = inner.waiting.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(tx) = slot.as_ref() {
                        let _ = tx.send(f);
                    }
                    false
                }
            },
        };
        if closed {
            let mut live = inner.live.lock().unwrap_or_else(|e| e.into_inner());
            // Only this reader's own connection: a newer one must survive an old reader's end.
            if live.as_ref().is_some_and(|l| l.generation == generation) {
                live.take();
            }
            return;
        }
    }
}

fn write_line(w: &mut UnixStream, v: &Value) -> Result<(), String> {
    let mut bytes = serde_json::to_vec(v).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    w.write_all(&bytes)
        .and_then(|()| w.flush())
        .map_err(|e| format!("writing to the room: {e}"))
}

/// A `Refused` as a sentence: its name with the fields it carries.
fn why(v: &Value) -> String {
    let Some(obj) = v.as_object() else {
        return v.to_string();
    };
    let name = obj
        .get("refused")
        .and_then(Value::as_str)
        .unwrap_or("refused")
        .replace('_', " ");
    let rest: Vec<String> = obj
        .iter()
        .filter(|(k, _)| k.as_str() != "refused")
        .map(|(k, v)| format!("{k}: {}", v.as_str().map_or(v.to_string(), str::to_string)))
        .collect();
    if rest.is_empty() {
        name
    } else {
        format!("{name} ({})", rest.join(", "))
    }
}

/// The tool names this module serves.
pub const TOOLS: [&str; 6] = [
    "rooms",
    "room_read",
    "room_say",
    "room_dm",
    "room_roster",
    "room_inbox",
];

fn tool(name: &str, description: &str, schema: Value) -> Value {
    json!({ "name": name, "description": description, "inputSchema": schema })
}

pub fn tool_list() -> Vec<Value> {
    vec![
        tool(
            "rooms",
            "The chat rooms on this host as `claude-code` sees them (`host` and one \
             `project/<key>` per project), with members, the newest `last_seq` and `unread` \
             (messages after your inbox cursor).",
            json!({ "type": "object", "properties": {} }),
        ),
        tool(
            "room_read",
            "Read a room's messages. Moves no cursor. `room` is `host`, `project/<key>` or `dm` \
             (your direct messages). `since` is a seq, `last` the newest N (default and most \
             200), `thread` a root seq: the root and its replies.",
            json!({
                "type": "object",
                "properties": {
                    "room": { "type": "string", "description": "`host`, `project/<key>` or `dm`." },
                    "since": { "type": "integer", "description": "Only messages after this seq." },
                    "last": { "type": "integer", "description": "The newest N messages." },
                    "thread": { "type": "integer", "description": "A root seq: return it and its replies." }
                },
                "required": ["room"]
            }),
        ),
        tool(
            "room_say",
            "Say something in a room, as `claude-code`. The author and kind come from this \
             connection, never from arguments. `to` names one session, `operator` or a handle: \
             a session named wakes and starts a turn. `@session-id` in the text does the same. \
             `thread` answers a message by its seq. `refs` points at a todo (`<session>/<title>`) \
             or a queue row. Text is at most 4000 bytes and is refused, never cut.",
            json!({
                "type": "object",
                "properties": {
                    "room": { "type": "string", "description": "`host` (default) or `project/<key>`." },
                    "text": { "type": "string" },
                    "to": { "type": "string", "description": "The one principal this is for." },
                    "thread": { "type": "integer", "description": "The seq this answers." },
                    "refs": {
                        "type": "array",
                        "description": "At most 8 rows this message is about.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "kind": { "type": "string", "description": "`todo` or `queue`." },
                                "id": { "type": "string" }
                            },
                            "required": ["kind", "id"]
                        }
                    }
                },
                "required": ["text"]
            }),
        ),
        tool(
            "room_dm",
            "A direct message to one session, `operator` or a handle. Only you and `to` can read it.",
            json!({
                "type": "object",
                "properties": {
                    "to": { "type": "string" },
                    "text": { "type": "string" },
                    "thread": { "type": "integer" }
                },
                "required": ["to", "text"]
            }),
        ),
        tool(
            "room_roster",
            "Who is in a room: name, kind, whether connected, project and the tree's label.",
            json!({
                "type": "object",
                "properties": { "room": { "type": "string" } },
                "required": ["room"]
            }),
        ),
        tool(
            "room_inbox",
            "What `claude-code` missed since its inbox cursor, through an attention filter: \
             `to_me` (default: what names you, the operator's unaddressed messages, DMs), \
             `mentions` or `all`. Delivery is at least once: with `ack: true` the cursor moves \
             past what this call returned, so the next call does not repeat it.",
            json!({
                "type": "object",
                "properties": {
                    "attention": { "type": "string", "enum": ["to_me", "mentions", "all"] },
                    "limit": { "type": "integer", "description": "Default 50." },
                    "ack": { "type": "boolean", "description": "Move the cursor past the returned rows." }
                }
            }),
        ),
    ]
}

fn arg_str(args: &Value, key: &str) -> Result<String, String> {
    match args.get(key) {
        Some(Value::String(s)) if !s.is_empty() => Ok(s.clone()),
        Some(Value::String(_)) | Some(Value::Null) | None => Err(format!("`{key}` is required")),
        Some(_) => Err(format!("`{key}` must be a string")),
    }
}

fn arg_opt_str(args: &Value, key: &str) -> Result<Option<String>, String> {
    match args.get(key) {
        Some(Value::Null) | None => Ok(None),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(format!("`{key}` must be a string")),
    }
}

fn arg_opt_u64(args: &Value, key: &str) -> Result<Option<u64>, String> {
    match args.get(key) {
        Some(Value::Null) | None => Ok(None),
        Some(v) => v
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("`{key}` must be a non-negative integer")),
    }
}

fn arg_refs(args: &Value) -> Result<Vec<Value>, String> {
    let Some(v) = args.get("refs").filter(|v| !v.is_null()) else {
        return Ok(Vec::new());
    };
    let rows = v.as_array().ok_or("`refs` must be an array")?;
    rows.iter()
        .map(|r| {
            let kind = r["kind"].as_str().ok_or("a ref needs a string `kind`")?;
            let id = r["id"].as_str().ok_or("a ref needs a string `id`")?;
            Ok(json!({ "kind": kind, "id": id }))
        })
        .collect()
}

fn with_lines(mut reply: Value) -> Value {
    let lines: Vec<String> = reply["rows"]
        .as_array()
        .map(|rows| rows.iter().map(line).collect())
        .unwrap_or_default();
    if reply["type"] == "history" || reply["type"] == "inbox" {
        reply["lines"] = json!(lines);
    }
    reply
}

/// Run one room tool.
pub fn call(link: &RoomLink, name: &str, args: &Value) -> Result<Value, String> {
    match name {
        "rooms" => link.request(&json!({ "type": "rooms" })),
        "room_read" => {
            let mut f = json!({ "type": "read", "room": arg_str(args, "room")? });
            if let Some(n) = arg_opt_u64(args, "since")? {
                f["since"] = json!(n);
            }
            if let Some(n) = arg_opt_u64(args, "last")? {
                f["last"] = json!(n);
            }
            if let Some(n) = arg_opt_u64(args, "thread")? {
                f["thread"] = json!(n);
            }
            link.request(&f).map(with_lines)
        }
        "room_say" => {
            let mut f = json!({
                "type": "say",
                "id": link.id(),
                "room": arg_opt_str(args, "room")?.unwrap_or_else(|| "host".into()),
                "text": arg_str(args, "text")?,
                "refs": arg_refs(args)?,
            });
            if let Some(to) = arg_opt_str(args, "to")? {
                f["to"] = json!(to);
            }
            if let Some(n) = arg_opt_u64(args, "thread")? {
                f["thread"] = json!(n);
            }
            link.request(&f)
        }
        "room_dm" => {
            let mut f = json!({
                "type": "dm",
                "id": link.id(),
                "to": arg_str(args, "to")?,
                "text": arg_str(args, "text")?,
            });
            if let Some(n) = arg_opt_u64(args, "thread")? {
                f["thread"] = json!(n);
            }
            link.request(&f)
        }
        "room_roster" => link.request(&json!({ "type": "roster", "room": arg_str(args, "room")? })),
        "room_inbox" => {
            let attention = match arg_opt_str(args, "attention")? {
                Some(a) => Attention::parse(&a)?,
                None => Attention::ToMe,
            };
            let limit = arg_opt_u64(args, "limit")?.unwrap_or(50).clamp(1, 200) as u32;
            let ack = matches!(args.get("ack"), Some(Value::Bool(true)));
            let mut reply = link.inbox(attention, limit)?;
            if ack {
                let rows = reply["rows"].as_array().map_or(0, Vec::len);
                // A short page is everything the filter let through, so the cursor may go to the
                // head. A full page may be cut, so it stops at the last row returned.
                let to = if rows < limit as usize {
                    reply["head"].as_u64()
                } else {
                    reply["rows"]
                        .as_array()
                        .and_then(|r| r.last())
                        .and_then(|m| m["seq"].as_u64())
                };
                if let Some(seq) = to.filter(|s| *s > reply["cursor"].as_u64().unwrap_or(0)) {
                    let acked = link.ack(seq)?;
                    reply["acked"] = acked["cursor"].clone();
                }
            }
            Ok(with_lines(reply))
        }
        other => Err(format!("`{other}` is not a room tool")),
    }
}

/// `room-wait`: block until a message passes `attention`, print it, move the cursor past it.
/// Exit codes: 0 a message (its lines on stdout), 20 the coordinator could not be reached,
/// 30 the deadline passed.
pub fn wait(link: &RoomLink, attention: Attention, deadline: Instant) -> (i32, String) {
    let (tx, rx) = mpsc::channel::<Value>();
    link.on_chat(Arc::new(move |m, names_you| {
        if attention.passes(m, names_you) {
            let _ = tx.send(m.clone());
        }
    }));
    loop {
        // After a drop the cursor still holds what was missed, so a reopen replays it.
        match link.ensure() {
            Err(e) => return (20, e),
            Ok(_) => match replay(link, attention) {
                Ok(Some(found)) => return (0, found),
                Ok(None) => {}
                Err(e) => return (20, e),
            },
        }
        // The live window: until the connection drops or something arrives.
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return (30, String::new());
            }
            match rx.recv_timeout(left.min(Duration::from_secs(1))) {
                Ok(m) => {
                    let seq = m["seq"].as_u64().unwrap_or(0);
                    let out = line(&m);
                    let _ = link.ack(seq);
                    return (0, out);
                }
                Err(mpsc::RecvTimeoutError::Timeout) if link.connected() => {}
                Err(_) => break,
            }
        }
    }
}

fn replay(link: &RoomLink, attention: Attention) -> Result<Option<String>, String> {
    let reply = link.inbox(attention, 200)?;
    let rows = reply["rows"].as_array().cloned().unwrap_or_default();
    let Some(last) = rows.last().and_then(|m| m["seq"].as_u64()) else {
        return Ok(None);
    };
    let out = rows.iter().map(line).collect::<Vec<_>>().join("\n");
    link.ack(last)?;
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the same table as the room crate's `the_host_key_matches_what_the_launcher_derives`.
    #[test]
    fn the_host_key_matches_the_room_crate() {
        for (raw, want) in [
            ("lab2x1", "lab2x1-e666427d"),
            ("MacBook-Pro", "macbook-pro-45a1c8db"),
            ("dead's box", "dead-s-box-5f45071c"),
            ("localhost", "localhost-03ad937a"),
        ] {
            assert_eq!(host_key_of(raw), want);
        }
        assert_eq!(host_key_of("---"), format!("host-{:08x}", 0xde1b_3246u32));
    }

    #[test]
    fn to_me_passes_what_names_the_seat_and_the_operators_broadcast() {
        let from_op =
            json!({"seq": 1, "author": "operator", "kind": "operator", "room": "host", "to": null});
        assert!(Attention::ToMe.passes(&from_op, false));
        let dm_op =
            json!({"seq": 1, "author": "operator", "kind": "operator", "room": "dm", "to": null});
        assert!(!Attention::ToMe.passes(&dm_op, false));
        let from_s =
            json!({"seq": 2, "author": "s-a", "kind": "session", "room": "host", "to": "s-b"});
        assert!(!Attention::ToMe.passes(&from_s, false));
        assert!(Attention::ToMe.passes(&from_s, true));
        assert!(Attention::All.passes(&from_s, false));
        assert!(!Attention::Mentions.passes(&from_op, false));
        let own = json!({"seq": 3, "author": HANDLE, "kind": "external", "room": "host"});
        assert!(!Attention::All.passes(&own, true));
    }
}
