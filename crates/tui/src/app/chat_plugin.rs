//! The chat as a LAYER: the `chat` feed (`X1`), the memo that asks it once per generation rather
//! than once per frame, the two hooks its input surface answers (`command_chat`, `complete_chat`),
//! and the acts an answer may ask the host to perform (`X7`).
//!
//! Nothing here draws. What a layer answers is a `Stream`, which is P2's renderer's input, and the
//! native adapter stays the drawing a broken layer falls to in the same frame.

use std::collections::BTreeMap;
use std::sync::Mutex;

use letibot_plugin::complete as pc;
use letibot_plugin::pane as pp;
use letibot_sessionlog::pane as pv;
use serde_json::{Value, json};

use crate::app::rooms::{Attach, Chan, Msg, OPERATOR, RoomsView};
use crate::app::*;

/// The hooks a chat layer answers.
pub(crate) const HOOK: &str = "pane_chat";
const COMMAND: &str = "command_chat";
const COMPLETE: &str = "complete_chat";

/// `X1`'s window: at most this many messages, and this many bytes of JSON. The byte cap is what
/// keeps a window of long messages under the guest's own 1 MiB input arena, and well under
/// `MAX_HOOK_BYTES`.
pub(crate) const WINDOW_MESSAGES: usize = 200;
pub(crate) const WINDOW_BYTES: usize = 256 * 1024;

/// The feed generation a memo holds, and what the layer answered for it. `None` is *nothing has
/// been asked*, which is not *the layer answered nothing*.
#[derive(Debug, Default)]
struct Memo {
    generation: Option<u64>,
    /// The layer the generation was asked of: a layer that arrives, leaves or is replaced is a
    /// different question, and the feed alone cannot say so.
    layer: Option<String>,
    held: Option<pv::View>,
    why: Option<String>,
    fuel: Option<u64>,
    /// The generation whose `why` has already been said.
    said: Option<u64>,
}

/// What the head keeps for the chat layer: the last answer, and the generation it answered.
#[derive(Debug, Default)]
pub(crate) struct ChatPane {
    memo: Mutex<Memo>,
}

impl ChatPane {
    /// Take the reason the layer drew nothing, once per generation: the caller that has a screen
    /// says it, and a sentence repeated every frame would be worse than silence.
    pub(crate) fn take_why(&self) -> Option<String> {
        let mut memo = self.memo.lock().ok()?;
        if memo.said == memo.generation {
            return None;
        }
        memo.said = memo.generation;
        memo.why.clone()
    }
}

impl App {
    /// The selected channel as the stream the view draws: the layer's when a chat layer answers,
    /// and P2's adapter otherwise. ONE call per feed generation - see [`App::chat_view`].
    pub(crate) fn chat_stream(&self, c: &Chan, names: &BTreeMap<String, String>) -> pv::Stream {
        if let Some(s) = self.chat_layer_stream() {
            return s;
        }
        crate::app::chat_adapter::stream(&self.rooms, c, names, OPERATOR, self.now_ms)
    }

    /// Say why the layer drew nothing, once per generation. The drawing is where a fallback is
    /// visible, so it is where the reason is owed: a chat that quietly stopped being the layer's
    /// is the one thing a fallback must not be.
    pub(crate) fn chat_note(&mut self) {
        if let Some(why) = self.chat_pane.take_why() {
            self.say(&why);
        }
    }

    /// The layer's own stream, if it answered one: the first `stream` block of its view, in the
    /// order the panels are drawn. A layer that answers no stream block is a layer with nothing to
    /// draw here, and the adapter draws.
    ///
    /// [`App::chat_note`] above is its other half: the drawing has a screen, and the reason a
    /// fallback happened is only worth keeping if somebody says it.
    fn chat_layer_stream(&self) -> Option<pv::Stream> {
        let view = self.chat_view()?;
        view.panels
            .iter()
            .flat_map(|p| p.summary.iter().chain(p.view.iter()))
            .find_map(|b| match b {
                pv::Block::Stream(s) => Some(s.clone()),
                _ => None,
            })
    }

    /// The chat layer's view, asked again only when the feed's generation moves, the layer
    /// changes, or nothing was asked yet. `None` is *the adapter draws this frame*.
    pub(crate) fn chat_view(&self) -> Option<pv::View> {
        let feed = self.chat_feed();
        self.chat_ask(&feed)
    }

    /// Whether a chat layer owns the composer: a view that declares an `input` surface is the
    /// pane's, and the head's own line handling steps aside for it (`X5`).
    pub(crate) fn chat_owns_input(&self) -> bool {
        self.chat_view().is_some_and(|v| v.input.is_some())
    }

    /// Ask the layer, unless the generation is the one it already answered. The layer id is part
    /// of the key because the feed is not: a layer that is rebuilt answers the same feed with
    /// different words, and a memo that only watched the feed would keep the old words.
    fn chat_ask(&self, feed: &pp::Chat) -> Option<pv::View> {
        let generation = fingerprint(feed);
        let slot = self.plugin_host();
        let host = slot.as_ref();
        let reader = host.and_then(|h| h.reader(HOOK, pp::Feed::Chat));
        let layer = reader.map(|l| l.id.clone());
        if let Ok(memo) = self.memo().lock()
            && memo.generation == Some(generation)
            && memo.layer == layer
        {
            return memo.held.clone();
        }
        let (Some(host), Some(reader)) = (host, reader) else {
            // No layer is not a failure: the adapter draws and nobody is owed a sentence.
            self.memo_note(generation, None, None, None, None);
            return None;
        };
        // A layer nobody vouched for is not one whose answers the operator's DMs are drawn from.
        // The rule is checked at admit as well; this is the half that has an answer to fall to.
        if !reader.trusted {
            let why = untrusted_sentence(reader);
            self.memo_note(generation, layer, None, Some(why), None);
            return None;
        }
        let declared = host.declared(HOOK, pp::Feed::Chat);
        let state = host.state_of(HOOK);
        let input = pp::Input {
            v: pp::V,
            state,
            look: None,
            event: None,
            size: pp::Size {
                w: self.cfg.width as u32,
                h: self.screen_rows.max(1) as u32,
            },
            feeds: BTreeMap::from([(pp::Feed::Chat.name().to_string(), json!(feed))]),
            folded: BTreeMap::new(),
            below: None,
            now_ms: Some(self.now_ms),
            utc_offset_s: utc_offset_s(self.now_ms),
        };
        let answered = host.call(HOOK, "the chat", &json!(input));
        if let Some(view) = answered.view.as_ref()
            && let Some(state) = &view.state
        {
            host.remember(HOOK, Some(state.clone()));
        }
        // The host's own rules, and the acts the layer declared (`View::validate_for`): a view the
        // head cannot honour is drawn by the adapter, not half-drawn.
        let held = answered.view.filter(|v| v.validate_for(&declared).is_ok());
        let why = held
            .is_none()
            .then(|| host.peek_problem())
            .flatten()
            .map(|why| why.to_string());
        let fuel = answered.fuel;
        self.memo_note(generation, layer, held.clone(), why, fuel);
        held
    }

    /// Write what an ask came to. One door, so no arm can leave the memo half-written.
    fn memo_note(
        &self,
        generation: u64,
        layer: Option<String>,
        held: Option<pv::View>,
        why: Option<String>,
        fuel: Option<u64>,
    ) {
        if let Ok(mut memo) = self.memo().lock() {
            memo.generation = Some(generation);
            memo.layer = layer;
            memo.held = held;
            memo.why = why;
            memo.fuel = fuel;
        }
    }

    fn memo(&self) -> &Mutex<Memo> {
        &self.chat_pane.memo
    }

    /// The feed a chat layer is handed (`X1`): the channels with their unread counts and labels,
    /// the roster, the selected channel, the two reading marks, the sends that have not landed, and
    /// a WINDOW of the selected channel's messages - the viewport plus a page each way, bounded by
    /// [`WINDOW_MESSAGES`] and [`WINDOW_BYTES`].
    pub(crate) fn chat_feed(&self) -> pp::Chat {
        let mut feed = pp::Chat::default();
        let selected = self.rooms.selected();
        for c in self.rooms.channels() {
            let here = selected.as_ref() == Some(&c);
            feed.channels.push(pp::ChatChannel {
                room: RoomsView::key(&c),
                label: self.rooms_name(&c),
                unread: if here { 0 } else { self.rooms.unread(&c) },
            });
        }
        feed.selected = selected.as_ref().map(RoomsView::key);
        for (room, list) in &self.rooms.roster {
            feed.roster.insert(
                room.clone(),
                list.iter().map(|m| m.name.clone()).collect::<Vec<_>>(),
            );
        }
        if let Some(c) = &selected {
            let key = RoomsView::key(c);
            feed.seen = self.rooms.seen.get(&key).copied().unwrap_or(0);
            feed.opened_seen = self.rooms.opened_seen.get(&key).copied().unwrap_or(0);
        }
        for s in &self.rooms.sends {
            feed.failed.push(pp::ChatFailed {
                room: RoomsView::key(&s.chan),
                text: s.text.clone(),
                why: s.error.clone().unwrap_or_default(),
            });
        }
        let Some(c) = &selected else {
            return feed;
        };
        let room = RoomsView::key(c);
        let msgs = self.rooms.messages(c);
        let mut from = msgs.len().saturating_sub(WINDOW_MESSAGES);
        loop {
            feed.messages = msgs[from..]
                .iter()
                .map(|m| chat_message(&room, m))
                .collect::<Vec<_>>();
            let bytes = serde_json::to_string(&feed).map(|s| s.len()).unwrap_or(0);
            // One message over the cap is still one message: a window with nothing in it would
            // draw an empty channel, which is the one thing it never is.
            if bytes <= WINDOW_BYTES || from + 1 >= msgs.len() {
                break;
            }
            from += 1;
        }
        feed
    }

    /// Enter on the chat layer's input: the line goes to `command_chat` with the key, the reply
    /// target, the chips and the feed, and the acts it answers with are performed as the operator.
    /// `None` is *the layer had the line and nothing is to be sent*, which is not a fallback: the
    /// head does not send a line the layer took.
    pub(crate) fn chat_command(&mut self, text: &str) -> Option<Action> {
        let view = self.chat_view()?;
        let input = view.input.clone()?;
        let feeds = BTreeMap::from([(pp::Feed::Chat.name().to_string(), json!(self.chat_feed()))]);
        let chips: Vec<String> = self
            .compose
            .chips
            .iter()
            .filter_map(|c| c.blob.clone())
            .collect();
        let asked = json!({
            "line": text,
            "key": input.key,
            "reply_to": self.compose.reply,
            "chips": chips,
            "feeds": feeds,
        });
        self.compose.reply = None;
        self.compose.open = None;
        self.compose.history.record(&input.key, text);
        let answered = {
            let slot = self.plugin_host();
            slot.as_ref()
                .map(|h| h.call(COMMAND, "the chat's input", &asked))
                .and_then(|a| a.answer)
        };
        let said = self.take_sessions_failure();
        let mut act = None;
        if let Some(answer) = answered {
            // A layer may answer a REFUSAL instead of acts: its own sentence, shown as said. The
            // text is the layer's bytes, so it goes through the cleaner every drawn word does.
            if let Some(words) = answer.get("said").and_then(Value::as_str)
                && !words.trim().is_empty()
            {
                self.say(&rano::agent::text::clean_line(words));
            }
            act = self.chat_acts(&answer);
        }
        if let Some(why) = said {
            self.say(&why);
        }
        act.unwrap_or(None)
    }

    /// Tab on the chat layer's input: `complete_chat` with the line and the caret, then the row a
    /// room's completion draws - open, or cycle, exactly as `rooms_complete` does. `false` is *not
    /// ours*, and the head's own completion keeps the key.
    pub(crate) fn chat_complete(&mut self) -> bool {
        if self.focused_surface() != Some(Surface::Rooms) {
            return false;
        }
        let Some(view) = self.chat_view() else {
            return false;
        };
        let Some(input) = view.input.map(|i| i.key) else {
            return false;
        };
        let text = self.editor.text().to_string();
        let cycle = self.compose.open.as_ref().and_then(|o| {
            (o.line == text && !o.answer.candidates.is_empty()).then(|| {
                let next = (o.idx + 1) % o.answer.candidates.len();
                (
                    o.answer.from,
                    o.written,
                    o.answer.candidates[next].insert.clone(),
                    next,
                )
            })
        });
        if let Some((from, written, insert, next)) = cycle {
            let line = self.write_candidate(from, from + written, &insert);
            if let Some(open) = &mut self.compose.open {
                open.idx = next;
                open.written = insert.len();
                open.line = line;
            }
            return true;
        }
        let asked = json!(pc::Input {
            v: pp::V,
            text: text.clone(),
            cursor: self.editor.cursor(),
            key: Some(input),
            feeds: BTreeMap::from([(pp::Feed::Chat.name().to_string(), json!(self.chat_feed()),)]),
            now_ms: Some(self.now_ms),
            utc_offset_s: utc_offset_s(self.now_ms),
        });
        let answer = {
            let slot = self.plugin_host();
            slot.as_ref()
                .map(|h| h.call(COMPLETE, "the chat's input", &asked))
                .and_then(|a| a.answer)
                .and_then(|a| serde_json::from_value::<pc::Completion>(a).ok())
                .filter(|c| c.validate(&text).is_ok())
        };
        let Some(answer) = answer.filter(|c| !c.candidates.is_empty()) else {
            self.compose.open = None;
            return false;
        };
        let insert = answer.candidates[0].insert.clone();
        let line = self.write_candidate(answer.from, answer.to, &insert);
        self.compose.open = Some(chat_input::complete::Open {
            line,
            answer,
            idx: 0,
            written: insert.len(),
        });
        true
    }

    /// The acts an answer carries (`X7`): each one declared in the layer's manifest and each one
    /// performed by the host. An undeclared name is SAID and not performed - the allowlist a
    /// reviewer reads is the list of words that can be done.
    fn chat_acts(&mut self, answer: &Value) -> Option<Option<Action>> {
        let acts = answer.get("acts").and_then(Value::as_array)?;
        // The allowlist is a fact of the layer, read before anything is done: a sentence written
        // between two acts must not be said with the stack borrowed.
        let declared = {
            let slot = self.plugin_host();
            slot.as_ref()
                .map(|h| h.declared(HOOK, pp::Feed::Chat))
                .unwrap_or_default()
        };
        let mut first: Option<Action> = None;
        for act in acts {
            let name = act.get("name").and_then(Value::as_str).unwrap_or("");
            let args = act.get("args").cloned().unwrap_or(Value::Null);
            if name.is_empty() || !declared.iter().any(|d| d == name) {
                let names = if declared.is_empty() {
                    "nothing".to_string()
                } else {
                    declared.join(", ")
                };
                self.say(&format!(
                    "the chat layer answered `{name}`, which its manifest does not declare — it \
                     may ask for {names}, so nothing was done"
                ));
                continue;
            }
            let Some(action) = self.chat_act(name, &args) else {
                continue;
            };
            match &first {
                None => first = Some(action),
                Some(_) => self.queued.push(action),
            }
        }
        Some(first)
    }

    /// One declared act, performed with the operator's identity: the same request the native view
    /// sends, so the coordinator's own checks and the seat's allowlist are the only doors.
    fn chat_act(&mut self, name: &str, args: &Value) -> Option<Action> {
        let text = args.get("text").and_then(Value::as_str);
        let thread = args.get("thread").and_then(Value::as_u64);
        match name {
            "room_say" => {
                let room = args
                    .get("room")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| self.rooms.selected().map(|c| RoomsView::key(&c)))?;
                let text = text.filter(|t| !t.trim().is_empty())?;
                Some(self.chat_say(Chan::Room(room), text, thread))
            }
            "room_dm" => {
                let to = args.get("to").and_then(Value::as_str).map(str::to_string)?;
                let text = text.filter(|t| !t.trim().is_empty())?;
                Some(self.chat_say(Chan::Dm(to), text, thread))
            }
            "room_read" => {
                let op = match args.get("thread").and_then(Value::as_u64) {
                    Some(thread) => {
                        let room = args.get("room").and_then(Value::as_str)?.to_string();
                        json!({"type": "read", "room": room, "thread": thread})
                    }
                    None => {
                        // An older page is asked for the room it names, or the one selected.
                        let room = args
                            .get("room")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .or_else(|| match self.rooms.selected() {
                                Some(Chan::Room(id)) => Some(id),
                                _ => None,
                            })?;
                        match args.get("before").and_then(Value::as_u64) {
                            Some(before) => {
                                json!({"type": "read", "room": room, "before": before, "last": 200})
                            }
                            None => self.rooms.older_op(&room)?,
                        }
                    }
                };
                self.rooms.next_poll_ms = 0;
                Some(Action::RoomChat { op, id: None })
            }
            "room_invite" | "room_leave" => {
                let verb = if name == "room_invite" {
                    "invite"
                } else {
                    "leave"
                };
                let who = args.get("session").and_then(Value::as_str)?.to_string();
                let room = args.get("room").and_then(Value::as_str)?.to_string();
                let id = format!("{verb}:{who}:{room}");
                Some(Action::RoomChat {
                    op: json!({"type": verb, "room": room, "session": who}),
                    id: Some(id),
                })
            }
            // OSC 52, the door `/copy` and `y` already use: the terminal's clipboard, never a file.
            "copy" => {
                let what = text.unwrap_or_default();
                if what.is_empty() {
                    return None;
                }
                self.clipboard_out = Some(what.to_string());
                self.say(&format!("copied {} line(s)", what.lines().count()));
                None
            }
            "open" => {
                if let Some(url) = args.get("url").and_then(Value::as_str) {
                    return self.open_link(url);
                }
                if let Some(blob) = args.get("blob").and_then(Value::as_str) {
                    if !self.room_open_picture(blob) {
                        self.say("that picture has not arrived yet");
                    }
                    return None;
                }
                let path = args.get("path").and_then(Value::as_str)?;
                let line = args.get("line").and_then(Value::as_u64).unwrap_or(1) as usize;
                self.open_in_editor(&FileRef {
                    path: path.to_string(),
                    line,
                    change: None,
                });
                None
            }
            // The host's picker is the session list's and the settings cards': a layer-filled one
            // is P6's switcher, and a sentence beats a key that does nothing.
            "pick" => {
                self.say(
                    "the chat layer asked for the host's picker, which this head has not got for a \
                     layer yet — type the room instead",
                );
                None
            }
            other => {
                self.say(&format!(
                    "`{other}` is declared but this head has no such act"
                ));
                None
            }
        }
    }

    /// One line as a `say` or a `dm`, recorded under a request id the head answers for: the same
    /// request `rooms_say` builds, for a channel the layer named rather than the selected one.
    fn chat_say(&mut self, chan: Chan, text: &str, thread: Option<u64>) -> Action {
        self.rooms.said += 1;
        if self.rooms.tag.is_empty() {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.subsec_nanos());
            self.rooms.tag = format!("{:x}{nanos:x}", std::process::id());
        }
        let id = format!("op-{}-{}-{}", self.rooms.tag, self.now_ms, self.rooms.said);
        let body = text.trim().to_string();
        self.rooms.sends.push(rooms::Send {
            id: id.clone(),
            chan: chan.clone(),
            thread,
            text: body.clone(),
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
        Action::RoomChat { id: Some(id), op }
    }
}

/// One message as the feed spells it - the wire's own fields, nothing reformatted.
fn chat_message(room: &str, m: &Msg) -> pp::ChatMessage {
    pp::ChatMessage {
        seq: m.seq,
        ts: m.ts,
        room: room.to_string(),
        author: m.author.clone(),
        kind: m.kind.clone(),
        to: m.to.clone(),
        thread: m.thread,
        refs: m
            .refs
            .iter()
            .map(|r| pp::ChatRef {
                kind: r.kind.clone(),
                id: r.id.clone(),
            })
            .collect(),
        attachments: m
            .attachments
            .iter()
            .map(|a| pp::ChatAttachment {
                id: a.id.clone(),
                mime: a.mime.clone(),
                name: a.name.clone(),
                bytes: a.bytes,
            })
            .collect(),
        text: m.text.clone(),
    }
}

/// The blobs a pane's stream draws, as the head's picture cache wants them: a layer names ids, and
/// an id it lists under `pictures` is a picture it expects the terminal to show.
pub(crate) fn stream_pictures(s: &pv::Stream) -> Vec<Attach> {
    let mut out: Vec<Attach> = Vec::new();
    for e in &s.entries {
        for id in &e.pictures {
            if out.iter().any(|a| &a.id == id) {
                continue;
            }
            out.push(Attach {
                id: id.clone(),
                mime: "image/png".to_string(),
                name: String::new(),
                bytes: 0,
                w: None,
                h: None,
            });
        }
    }
    out
}

/// A feed's generation: a stable 64-bit fingerprint of the JSON the layer would be handed. The
/// content decides, not a counter, because every page, push, poll and send lands in the feed and a
/// counter would have to be bumped at each of them and would be wrong at the first one forgotten.
fn fingerprint(feed: &pp::Chat) -> u64 {
    let text = serde_json::to_string(feed).unwrap_or_default();
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// What a layer that nobody vouched for is told: the rule, and the one command that fixes it.
fn untrusted_sentence(layer: &letibot_plugins::Layer) -> String {
    format!(
        "the plugin layer `{}` is not pinned any more, and the chat feed is the operator's own \
         conversations — `/plugin trust {}` pins the bytes on disk now",
        layer.manifest.name, layer.manifest.name
    )
}

/// The person's local offset from UTC, in seconds east of it - the reading a `HH:MM` needs, which
/// a guest has no clock to take. `None` for a machine whose zone cannot be read.
fn utc_offset_s(ms: u64) -> Option<i32> {
    let secs = (ms / 1000) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `localtime_r` writes into `tm` and reads `secs`, both owned here; the `_r` form keeps
    // no shared static, and the driver is not the only thread in this process.
    let ok = unsafe { !libc::localtime_r(&secs, &mut tm).is_null() };
    ok.then_some(tm.tm_gmtoff as i32)
}
