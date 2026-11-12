//! The room, as the built-in `chat` layer: a run of one speaker's messages within ten minutes keeps
//! one header, a handle keeps one hue, a mention reads as the name behind it, and a reply quotes what
//! it answers. It is not a second renderer - the host draws its `stream` block with the same code.

use std::borrow::Cow;
use std::collections::BTreeMap;

use letibot_guest_abi as abi;
use letibot_plugin::complete::{Candidate, Completion};
use letibot_plugin::pane::{Cell, Chat, ChatMessage, ChatVerb, HUES, Role};
use serde::Serialize;

/// How long a run of one speaker may pause before the next message opens a new one: ten minutes,
/// which is long enough for a burst and short enough that a new subject gets a header.
const RUN_MS: u64 = 10 * 60_000;

/// How much of the message a reply's quote line shows of it: a quote orients, the answer explains.
const QUOTE_COLS: usize = 60;

/// Who the person is, in the room's own words.
const ME: &str = "operator";

/// How many candidates a completion row offers: as many as one row of the composer's edge is read
/// at a glance, and fewer than the host's own eight.
const TOP: usize = 6;

/// The act names the host performs, exactly as `plugin.toml` declares them.
const SAY: &str = "room_say";
const DM: &str = "room_dm";

/// The key the composer is bound to when no channel is selected, matching the head's own.
const NO_CHANNEL: &str = "rooms";

/// The pane hook - `pane_<name>`, the name a head opens by.
#[unsafe(no_mangle)]
pub extern "C" fn pane_chat(ptr: i32, len: i32) -> i32 {
    abi::hook(ptr, len, answer)
}

/// The line Enter took - `command_<name>`, asked with the text, the input's key and the feed.
#[unsafe(no_mangle)]
pub extern "C" fn command_chat(ptr: i32, len: i32) -> i32 {
    abi::hook(ptr, len, command)
}

/// Tab's candidates - `complete_<name>`, asked with the line and the caret.
#[unsafe(no_mangle)]
pub extern "C" fn complete_chat(ptr: i32, len: i32) -> i32 {
    abi::hook(ptr, len, complete)
}

/// The layer's policy, tested natively - see `tests.rs`.
#[cfg(test)]
mod tests;

// ─────────────────────────────── the pane ───────────────────────────────

/// The input a `pane_chat` call is handed, as far as this layer reads it: the feed it declared and
/// the head's clock. The SDK's `pane::Input` carries `feeds` as a map of `Value`, which would parse
/// the window twice, so the envelope is written here while the payload is still the SDK's.
#[derive(Debug, serde::Deserialize)]
struct Ask {
    #[serde(default)]
    feeds: Feeds,
    #[serde(default)]
    now_ms: Option<u64>,
    #[serde(default)]
    utc_offset_s: Option<i32>,
}

/// The feeds this layer declared, by the name its manifest spells. Absent is absent, never empty.
#[derive(Debug, Default, serde::Deserialize)]
struct Feeds {
    #[serde(default)]
    chat: Option<Chat>,
}

fn answer(bytes: &[u8]) -> Option<String> {
    let ask: Ask = serde_json::from_slice(bytes).ok()?;
    let chat = ask.feeds.chat?;
    // No clock, no zone, no layer. The day a divider names and the hour a message prints are the
    // local reading, and a guest has neither; the head's own adapter has both, so it draws.
    let now = ask.now_ms?;
    let off = ask.utc_offset_s?;
    let stream = stream(&chat, now, off);
    serde_json::to_string(&view(&chat, stream)).ok()
}

/// The view: one panel whose summary is the stream, and the input surface that makes the head's
/// composer this pane's. Typed rather than a `json!` tree: a guest module is embedded in the binary,
/// and marshalling through `Value` costs more bytes than a struct that serialises straight.
#[derive(Debug, Serialize)]
struct View {
    title: &'static str,
    panels: Vec<Panel>,
    input: PaneInput,
}

#[derive(Debug, Serialize)]
struct Panel {
    id: &'static str,
    key: &'static str,
    summary: Vec<Block>,
}

#[derive(Debug, Serialize)]
struct Block {
    stream: Stream,
}

#[derive(Debug, Serialize)]
struct PaneInput {
    key: String,
    placeholder: &'static str,
    footer: &'static str,
}

fn view(chat: &Chat, stream: Stream) -> View {
    View {
        title: "the room",
        panels: vec![Panel {
            id: "chat",
            key: "c",
            summary: vec![Block { stream }],
        }],
        input: PaneInput {
            key: chat
                .selected
                .clone()
                .unwrap_or_else(|| NO_CHANNEL.to_string()),
            placeholder: "say it to the room",
            // A `/` line is the head's before it is ours (`composer.rs` dispatches it first), so
            // the hint names the spelling that reaches this layer: the room's own `>N`.
            footer: "enter sends · >N text answers · tab completes",
        },
    }
}

/// The stream, as `crates/sessionlog/src/pane.rs` spells it. A local shape rather than a second
/// contract: a host field this build does not know is one the head parses with its own defaults,
/// and the head's parity test is what holds the two answers together.
#[derive(Debug, Default, Serialize)]
struct Stream {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    entries: Vec<Entry>,
    #[serde(skip_serializing_if = "String::is_empty")]
    empty: String,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    mentions: BTreeMap<String, Mention>,
}

/// One entry: a message, or a divider drawn between two of them.
#[derive(Debug, Default, Serialize)]
struct Entry {
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    head: Vec<Cell>,
    #[serde(skip_serializing_if = "Option::is_none")]
    time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    quote: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pictures: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    facts: Vec<Cell>,
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<Role>,
    /// `today` or `unread`: the two marks the host draws between two entries.
    #[serde(skip_serializing_if = "Option::is_none")]
    divider: Option<&'static str>,
    #[serde(skip_serializing_if = "String::is_empty")]
    text: String,
}

/// One `@token` of a body: the name to show instead of it, and the colour to show it in.
#[derive(Debug, Default, Serialize)]
struct Mention {
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hue: Option<u8>,
    #[serde(skip_serializing_if = "is_false")]
    me: bool,
}

/// The selected channel as a stream: every message an entry, a divider where the day turns and
/// where the reader's own reading had got to, and each message's share of the decisions above.
fn stream(chat: &Chat, now: u64, off: i32) -> Stream {
    let mut s = Stream::default();
    if chat.messages.is_empty() {
        // The two empty channels are two facts: nothing was said, or nobody has answered yet.
        s.empty = if chat.heard {
            "nothing here yet"
        } else {
            "asking the coordinator..."
        }
        .to_string();
        return s;
    }
    let mut marked = false;
    let mut prev: Option<&ChatMessage> = None;
    let mut prev_day: Option<Day> = None;
    for m in &chat.messages {
        let day = local(m.ts, off).map(|l| l.day);
        let mut divided = false;
        if day.is_some() && day != prev_day {
            s.entries.push(Entry {
                divider: Some("today"),
                text: day.map(|d| d.label(now, off)).unwrap_or_default(),
                ..Entry::default()
            });
            prev_day = day;
            divided = true;
        }
        if let Some(at) = chat.opened_seen
            && !marked
            && m.seq > at
        {
            s.entries.push(Entry {
                divider: Some("unread"),
                text: "unread".to_string(),
                ..Entry::default()
            });
            marked = true;
            divided = true;
        }
        let runs_on = !divided
            && m.kind != "system"
            && !mentions_operator(m)
            && prev.is_some_and(|p| {
                p.author == m.author
                    && p.to == m.to
                    && p.kind != "system"
                    && m.ts > p.ts
                    && m.ts - p.ts < RUN_MS
                    && local(m.ts, off).map(|l| l.day) == local(p.ts, off).map(|l| l.day)
            });
        // A continuation keeps its time only when the minute it was sent in differs from the one
        // above it: a row that repeated the run's own minute would say nothing.
        let same_minute = prev.is_some_and(|p| stamp(p, off) == stamp(m, off));
        s.entries.push(entry(
            m,
            &chat.messages,
            &chat.names,
            runs_on,
            same_minute,
            off,
        ));
        tokens(&mut s.mentions, &m.text, &chat.names);
        prev = Some(m);
    }
    s
}

/// One message as an entry. A continuation carries no head and no facts: what it has to say is its
/// body, and the time in its gutter when that differs from the message above it.
fn entry(
    m: &ChatMessage,
    all: &[ChatMessage],
    names: &BTreeMap<String, String>,
    runs_on: bool,
    same_minute: bool,
    off: i32,
) -> Entry {
    let mut e = Entry {
        id: Some(m.seq.to_string()),
        ..Entry::default()
    };
    if m.kind == "system" {
        // A todo or a queue line is not somebody talking: one quiet row, never a header to scan.
        e.body = Some(system_text(m));
        e.role = Some(Role::Faint);
        return e;
    }
    e.body = Some(m.text.clone());
    e.pictures = m.attachments.iter().map(|a| a.id.clone()).collect();
    if !runs_on {
        e.head = head(m, names);
        e.facts = vec![Cell::role(format!("#{}", m.seq), Role::Faint)];
        e.time = stamp(m, off);
    } else if !same_minute {
        e.time = stamp(m, off);
    }
    if let Some(t) = m.thread {
        e.quote = Some(quote(t, all, names));
    }
    e
}

/// The header's cells: the mark a message that names the reader wears, the speaker in its hue, and
/// who it was addressed to. Nothing else - a header is scanned, not read.
fn head(m: &ChatMessage, names: &BTreeMap<String, String>) -> Vec<Cell> {
    let mut out = Vec::new();
    if mentions_operator(m) {
        // A message that names the reader is worth knowing, and it is not a decision they owe:
        // `attention` is the register for what needs a person.
        out.push(Cell::role("!", Role::Warning));
    }
    let who = if m.author == ME {
        Role::Strong
    } else {
        Role::Hue(hue(&m.author))
    };
    out.push(Cell::role(display(names, &m.author), who));
    if let Some(to) = m.to.as_deref().filter(|to| *to != ME) {
        out.push(Cell::role(
            format!("to {}", display(names, to)),
            Role::Faint,
        ));
    }
    out
}

/// The line a reply answers: the number it names, and as much of the message as a quote line has
/// room for. A reply to a message this window does not hold still says which one it answers.
fn quote(t: u64, all: &[ChatMessage], names: &BTreeMap<String, String>) -> String {
    match all.iter().find(|m| m.seq == t) {
        Some(target) => {
            let first = target
                .text
                .lines()
                .map(clean_line)
                .find(|l| !l.trim().is_empty())
                .unwrap_or_default();
            let said = truncate(first.trim(), QUOTE_COLS);
            format!("↳ {t} {}: {said}", display(names, &target.author))
        }
        None => format!("↳ {t}"),
    }
}

/// A todo or queue line: who it came from, what it says, and the rows it is about. The refs are
/// bracketed and the brackets are escaped, because an unescaped `[queue m-41]` is markdown's
/// reference-link syntax and the renderer draws it as the bare words inside it.
fn system_text(m: &ChatMessage) -> String {
    let refs: String = m
        .refs
        .iter()
        .map(|r| format!(" \\[{} {}\\]", clean_line(&r.kind), clean_line(&r.id)))
        .collect();
    format!("· {}: {}{refs}", clean_line(&m.author), m.text)
}

/// What a handle is called: the head's name for it, the handle when nobody named it.
fn display<'a>(names: &'a BTreeMap<String, String>, handle: &'a str) -> Cow<'a, str> {
    clean_line(names.get(handle).map(String::as_str).unwrap_or(handle))
}

/// The token that names the reader, spelled once: this runs on every message of the window.
const ME_TAG: &str = "@operator";

/// Whether a message names the reader, by a DM or by an `@operator` on a word boundary.
fn mentions_operator(m: &ChatMessage) -> bool {
    if m.author == ME {
        return false;
    }
    if m.to.as_deref() == Some(ME) {
        return true;
    }
    // Most messages name nobody, and lowering a copy of every one of them is the window's price.
    if !m.text.contains('@') {
        return false;
    }
    let lower = m.text.to_ascii_lowercase();
    let name = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    lower.match_indices(ME_TAG).any(|(i, _)| {
        let before = lower[..i].chars().next_back().is_none_or(|c| !name(c));
        let after = lower[i + ME_TAG.len()..]
            .chars()
            .next()
            .is_none_or(|c| !name(c));
        before && after
    })
}

/// Every `@token` of a body the reader can be told more about: a member's handle reads as its name
/// in its own hue, and the reader's own handle is the one mention that is not quiet.
fn tokens(out: &mut BTreeMap<String, Mention>, text: &str, names: &BTreeMap<String, String>) {
    if !text.contains('@') {
        return;
    }
    for token in mentions(text) {
        let handle = token[1..].to_string();
        if handle.eq_ignore_ascii_case(ME) {
            out.insert(
                token,
                Mention {
                    me: true,
                    ..Mention::default()
                },
            );
        } else if let Some(name) = names.get(&handle) {
            out.insert(
                token,
                Mention {
                    text: Some(format!("@{}", clean_line(name))),
                    hue: Some(hue(&handle)),
                    me: false,
                },
            );
        }
    }
}

/// The `@token`s of a text, with the `@`: the room's own lexer (`crates/room/src/chat.rs`), which
/// this crate cannot link and the head keeps its own copy of. A `@` on a word boundary, then a
/// handle, where `@` and `:` join a bridge's `cc@folder:name` into one token.
fn mentions(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let boundary = i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
        if bytes[i] != b'@' || !boundary {
            i += 1;
            continue;
        }
        let start = i;
        let mut end = i + 1;
        while end < bytes.len() {
            let b = bytes[end];
            let joins = (matches!(b, b'@' | b':')
                && end > start + 1
                && bytes
                    .get(end + 1)
                    .is_some_and(|n| n.is_ascii_alphanumeric() || (b == b':' && *n == b'*')))
                || (b == b'*' && bytes[end - 1] == b':');
            if !(b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_') || joins) {
                break;
            }
            end += 1;
        }
        if end > start + 1 {
            out.push(text[start..end].to_string());
        }
        i = end.max(i + 1);
    }
    out
}

// ─────────────────────────── the line Enter took ───────────────────────────

/// The input a `command_` hook is handed: the line, the key the composer is bound to, the message
/// it answers, the pictures in it, and the feed.
#[derive(Debug, serde::Deserialize)]
struct Line {
    #[serde(default)]
    line: String,
    #[serde(default)]
    reply_to: Option<u64>,
    /// The blobs pasted into the line, in the order they were pasted. The head holds their bytes
    /// and names them; the layer only knows them as references.
    #[serde(default)]
    chips: Vec<String>,
    #[serde(default)]
    feeds: Feeds,
}

fn command(bytes: &[u8]) -> Option<String> {
    let input: Line = serde_json::from_slice(bytes).ok()?;
    let line = input.line.trim();
    if line.is_empty() {
        return None;
    }
    let chat = input.feeds.chat?;
    let act = match line.strip_prefix('/') {
        Some(rest) => {
            let (verb, args) = split_verb(rest);
            match verb {
                "reply" => reply(args),
                "dm" => dm(args),
                other => refusal(&format!(
                    "this chat layer knows `/reply N text` and `/dm NAME text`, not `/{other}` \
                     — a plain line goes to the room, and `>N text` answers message N"
                )),
            }
        }
        // The room's own spelling of a reply, which the composer's hint still names.
        None => match thread_of(line) {
            Some((thread, rest)) => say(rest, Some(thread)),
            None => say(line, input.reply_to),
        },
    };
    // A picture pasted into the line rides with it as a reference: the head holds the bytes and
    // names the blob, and a say without the reference would post the words and lose the picture.
    let act = with_pictures(act, &input.chips);
    serde_json::to_string(&answer_of(act, chat.selected.as_deref())).ok()
}

/// The pictures the composer held, appended to what was said - `![blob](blob:ID)` per blob, the
/// spelling the head's own `rooms_outgoing` writes.
fn with_pictures(act: Act, chips: &[String]) -> Act {
    if chips.is_empty() {
        return act;
    }
    let refs: String = chips
        .iter()
        .map(|id| format!("\n![blob](blob:{id})"))
        .collect();
    match act {
        Act::Say { text, thread } => Act::Say {
            text: format!("{text}{refs}"),
            thread,
        },
        Act::Dm { to, text } => Act::Dm {
            to,
            text: format!("{text}{refs}"),
        },
        refused => refused,
    }
}

/// The answer the head performs: acts, or one sentence when the line was not one this layer does.
#[derive(Debug, Default, Serialize)]
struct Said {
    #[serde(skip_serializing_if = "Option::is_none")]
    said: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    acts: Vec<ActOut>,
}

#[derive(Debug, Serialize)]
struct ActOut {
    name: &'static str,
    args: ActArgs,
}

/// Both acts' arguments in one shape: `to` is a dm's and `room`/`thread` are a say's, and a field
/// neither carries is absent rather than null.
#[derive(Debug, Default, Serialize)]
struct ActArgs {
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    thread: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    room: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    to: Option<String>,
}

/// The answer the head performs. A line typed in a DM is a `room_dm`: the head's `room_say`
/// spells the channel's key as a room's name, and `dm:s-b` is nobody's room.
fn answer_of(act: Act, selected: Option<&str>) -> Said {
    let (text, thread) = match act {
        Act::Refused(said) => {
            return Said {
                said: Some(said),
                acts: Vec::new(),
            };
        }
        Act::Dm { to, text } => {
            return Said {
                said: None,
                acts: vec![ActOut {
                    name: DM,
                    args: ActArgs {
                        text,
                        to: Some(to),
                        ..ActArgs::default()
                    },
                }],
            };
        }
        Act::Say { text, thread } => (text, thread),
    };
    let mut args = ActArgs {
        text,
        thread,
        ..ActArgs::default()
    };
    let name = match selected.and_then(|ch| ch.strip_prefix("dm:")) {
        Some(peer) => {
            args.to = Some(peer.to_string());
            DM
        }
        // The channel the line was typed in, so a project room is not whichever was last named.
        None => {
            args.room = selected.map(str::to_string);
            SAY
        }
    };
    Said {
        said: None,
        acts: vec![ActOut { name, args }],
    }
}

/// What a line came to.
#[derive(Debug, PartialEq, Eq)]
enum Act {
    Say { text: String, thread: Option<u64> },
    Dm { to: String, text: String },
    Refused(String),
}

/// A refusal: the layer's own sentence, shown rather than silently doing nothing.
fn refusal(said: &str) -> Act {
    Act::Refused(said.to_string())
}

/// `/reply N text`.
fn reply(args: &str) -> Act {
    match args.trim().split_once(char::is_whitespace) {
        Some((n, rest)) => match n.parse::<u64>() {
            Ok(thread) if !rest.trim().is_empty() => say(rest, Some(thread)),
            Ok(_) => refusal("`/reply N` wants the words after the number"),
            Err(_) => refusal("`/reply` wants the number of the message and then the words"),
        },
        None => refusal("`/reply` wants the number of the message and then the words"),
    }
}

/// `/dm NAME text`: a direct message to one peer.
fn dm(args: &str) -> Act {
    match args.trim().split_once(char::is_whitespace) {
        Some((to, text)) if !to.is_empty() && !text.trim().is_empty() => Act::Dm {
            to: to.to_string(),
            text: text.trim().to_string(),
        },
        _ => refusal("`/dm` wants who to write to and then the words"),
    }
}

/// A plain line, with the reply target the composer held.
fn say(text: &str, thread: Option<u64>) -> Act {
    Act::Say {
        text: text.trim().to_string(),
        thread,
    }
}

/// The verb and its arguments: `/reply 12 hi` is `("reply", "12 hi")`.
fn split_verb(rest: &str) -> (&str, &str) {
    match rest.trim_start().split_once(char::is_whitespace) {
        Some((verb, args)) => (verb, args.trim()),
        None => (rest.trim(), ""),
    }
}

/// `>14 text` answers message 14. Anything else is the whole line.
fn thread_of(text: &str) -> Option<(u64, &str)> {
    let rest = text.strip_prefix('>')?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let n = digits.parse::<u64>().ok()?;
    let after = &rest[digits.len()..];
    let body = after.trim_start();
    (!body.is_empty() && after.starts_with(char::is_whitespace)).then_some((n, body))
}

// ─────────────────────────────── Tab's row ───────────────────────────────

/// What a `complete_chat` call is handed: the line, the caret and the feed - the same envelope
/// reason as [`Ask`], and one that matters more here, because Tab is a keystroke and the window it
/// carries is the whole one.
#[derive(Debug, serde::Deserialize)]
struct Typed {
    #[serde(default)]
    text: String,
    #[serde(default)]
    cursor: usize,
    #[serde(default)]
    feeds: Feeds,
}

fn complete(bytes: &[u8]) -> Option<String> {
    let input: Typed = serde_json::from_slice(bytes).ok()?;
    let chat = input.feeds.chat?;
    serde_json::to_string(&rank(&chat, &input.text, input.cursor)).ok()
}

/// The candidates under the caret: a `/` word ranks the verbs, an `@` word ranks the people and
/// the bridge's aliases, and anything else has nothing to offer.
fn rank(chat: &Chat, text: &str, cursor: usize) -> Completion {
    let cursor = boundary(text, cursor);
    if let Some((from, frag)) = verb_fragment(text, cursor) {
        return Completion {
            from,
            to: cursor,
            candidates: verbs_for(&chat.verbs, frag),
        };
    }
    if let Some((from, frag)) = at_fragment(text, cursor) {
        return Completion {
            from,
            to: cursor,
            candidates: people_for(chat, frag),
        };
    }
    Completion::default()
}

/// The `/` verbs: prefix before substring, and the list's own order within a rank - which is what
/// puts the room's own verbs ahead of the head's.
fn verbs_for(verbs: &[ChatVerb], frag: &str) -> Vec<Candidate> {
    let needle = frag.to_lowercase();
    let mut found: Vec<(u8, Candidate)> = verbs
        .iter()
        .filter_map(|v| {
            let rank = hit(&needle, &v.name)?;
            Some((
                rank,
                Candidate {
                    // A trailing space, so the argument is typed on from where the verb left it.
                    insert: format!("/{} ", v.name),
                    note: (!v.hint.is_empty()).then(|| v.hint.clone()),
                    ..Candidate::default()
                },
            ))
        })
        .collect();
    found.sort_by_key(|(rank, _)| *rank);
    found
        .into_iter()
        .take(TOP)
        .map(|(_, candidate)| candidate)
        .collect()
}

/// The `@` names: the roster's people and the bridge's aliases, ranked by how they match and then
/// by where they are. The handle is what gets inserted, because the server resolves handles and a
/// display name would post as nobody.
fn people_for(chat: &Chat, frag: &str) -> Vec<Candidate> {
    let needle = frag.to_lowercase();
    let mut people: Vec<(&str, &str, bool, bool)> = Vec::new();
    for (room, list) in &chat.roster {
        let here = chat.selected.as_deref() == Some(room.as_str());
        for m in list {
            let name = chat
                .names
                .get(&m.name)
                .map(String::as_str)
                .unwrap_or(&m.name);
            people.push((&m.name, name, here, m.connected));
        }
    }
    people.sort_by(|a, b| a.0.cmp(b.0));
    let mut found: Vec<(u8, u8, usize, Candidate)> = Vec::new();
    for (i, (handle, name, here, connected)) in people.iter().enumerate() {
        let Some(rank) = best(&needle, handle, name) else {
            continue;
        };
        let group = match (here, connected) {
            (true, true) => 0,
            (true, false) => 1,
            _ => 2,
        };
        found.push((
            rank,
            group,
            i,
            Candidate {
                insert: format!("@{handle}"),
                label: (name != handle).then(|| (*name).to_string()),
                note: match (here, connected) {
                    (true, true) => None,
                    (true, false) => Some("away".to_string()),
                    _ => Some("elsewhere".to_string()),
                },
                hue: Some(hue(handle)),
            },
        ));
    }
    let base = people.len();
    for (i, alias) in chat.aliases.iter().enumerate() {
        let Some(rank) = hit(&needle, alias.insert.trim_start_matches('@')) else {
            continue;
        };
        found.push((
            rank,
            3,
            base + i,
            Candidate {
                insert: alias.insert.clone(),
                note: (!alias.note.is_empty()).then(|| alias.note.clone()),
                ..Candidate::default()
            },
        ));
    }
    found.sort_by_key(|(rank, group, order, _)| (*rank, *group, *order));
    found
        .into_iter()
        .take(TOP)
        .map(|(_, _, _, candidate)| candidate)
        .collect()
}

/// How well a needle matches: `0` at the front, `1` anywhere, `None` not at all. Case-blind,
/// because a person types `@Fix` at a handle called `fix-room-push`.
fn hit(needle: &str, hay: &str) -> Option<u8> {
    let hay = hay.to_lowercase();
    if hay.starts_with(needle) {
        Some(0)
    } else if hay.contains(needle) {
        Some(1)
    } else {
        None
    }
}

/// The better of the handle's and the name's match: one fragment can name a person by either.
fn best(needle: &str, handle: &str, name: &str) -> Option<u8> {
    match (hit(needle, handle), hit(needle, name)) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// The caret as a byte offset the line can be split at: the hooks promise a boundary, and a caller
/// that hands back one inside a character must not panic the split.
fn boundary(text: &str, cursor: usize) -> usize {
    let mut c = cursor.min(text.len());
    while c > 0 && !text.is_char_boundary(c) {
        c -= 1;
    }
    c
}

/// The `/` word being typed: a slash at the start of the line, and no space after it yet.
fn verb_fragment(text: &str, cursor: usize) -> Option<(usize, &str)> {
    let word = text.get(..cursor)?.strip_prefix('/')?;
    if word.contains(char::is_whitespace) {
        return None;
    }
    Some((0, word))
}

/// The `@` word being typed: the last word before the caret, when it starts with `@`. The span
/// starts at the `@`, so accepting a candidate replaces the fragment and not the `@` with it.
fn at_fragment(text: &str, cursor: usize) -> Option<(usize, &str)> {
    let head = text.get(..cursor)?;
    let start = match head.rfind(char::is_whitespace) {
        Some(i) => i + head[i..].chars().next().map_or(1, char::len_utf8),
        None => 0,
    };
    let frag = head[start..].strip_prefix('@')?;
    Some((start, frag))
}

// ─────────────────────────────── the clock ───────────────────────────────

/// A local calendar reading: the day a divider names, and the hour a message prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Local {
    day: Day,
    hh: u32,
    mm: u32,
}

/// A local calendar date.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Day {
    y: i64,
    m: u32,
    d: u32,
}

impl Day {
    /// What the divider over this day says: `today` while it is, `yesterday` behind it, `Nov 7`
    /// inside this year, and the year too when the talk is older than that.
    fn label(self, now_ms: u64, off: i32) -> String {
        let today = local(now_ms, off).map(|l| l.day);
        match today {
            Some(t) if t == self => "today".to_string(),
            Some(t) if self.number() == t.number() - 1 => "yesterday".to_string(),
            Some(t) if t.y == self.y => format!("{} {}", month(self.m), self.d),
            _ => format!("{} {}, {}", month(self.m), self.d, self.y),
        }
    }

    /// This date as a day number, so two of them can be told apart by more than a day. Days-from-
    /// civil, by Hinnant: the arithmetic a calendar needs and no crate.
    fn number(self) -> i64 {
        let y = if self.m <= 2 { self.y - 1 } else { self.y };
        let era = if y >= 0 { y } else { y - 399 } / 400;
        let yoe = y - era * 400;
        let mp = (i64::from(self.m) + 9) % 12;
        let doy = (153 * mp + 2) / 5 + i64::from(self.d) - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }
}

/// A timestamp in the person's own zone. `None` for a message written before the wire had a
/// timestamp, which is not midnight 1970 and must not be drawn as if it were.
fn local(ms: u64, off: i32) -> Option<Local> {
    if ms == 0 {
        return None;
    }
    let secs = (ms / 1000) as i64 + i64::from(off);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    Some(Local {
        day: Day { y, m, d },
        hh: (rem / 3600) as u32,
        mm: ((rem % 3600) / 60) as u32,
    })
}

/// Days-since-epoch to a calendar date, by Hinnant - the other half of [`Day::number`].
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The time a message was sent at, as this layer prints it.
fn stamp(m: &ChatMessage, off: i32) -> Option<String> {
    local(m.ts, off).map(|l| format!("{:02}:{:02}", l.hh, l.mm))
}

fn month(m: u32) -> &'static str {
    const NAMES: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    NAMES[usize::from(m as u16).clamp(1, 12) - 1]
}

// ──────────────────────────── text a host did not author ────────────────────────────

/// Text made safe to measure: an escape sequence goes whole, every other control character is a
/// space, and a newline with it. It must agree with `rano::agent::text::clean_line`, which a guest
/// cannot link, or a name is measured one way and drawn another.
fn clean_line(s: &str) -> Cow<'_, str> {
    if !s.chars().any(is_control) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\u{1b}' => match it.peek().copied() {
                Some('[') => {
                    it.next();
                    skip_csi(&mut it);
                }
                Some(']') => {
                    it.next();
                    skip_osc(&mut it);
                }
                Some('\n') | None => {}
                Some(_) => {
                    it.next();
                }
            },
            // The C1 forms mean the same with no `ESC` in front.
            '\u{9b}' => skip_csi(&mut it),
            '\u{9d}' => skip_osc(&mut it),
            '\u{9c}' => {}
            c if is_control(c) => out.push(' '),
            c => out.push(c),
        }
    }
    Cow::Owned(out)
}

/// A `CSI` sequence's body, the introducer already consumed.
fn skip_csi(it: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    while it.peek().is_some_and(|c| ('\u{20}'..='\u{3f}').contains(c)) {
        it.next();
    }
    if it.peek().is_some_and(|c| ('\u{40}'..='\u{7e}').contains(c)) {
        it.next();
    }
}

/// An `OSC` string, the introducer already consumed: it ends at `BEL` or at `ST`, and one that
/// never ends takes the rest of the line with it, which is what a terminal would do.
fn skip_osc(it: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    while let Some(&c) = it.peek() {
        if c == '\n' {
            return;
        }
        it.next();
        if c == '\u{7}' {
            return;
        }
        if c == '\u{1b}' {
            if it.peek() == Some(&'\\') {
                it.next();
            }
            return;
        }
    }
}

/// Shorten `s` to `cols` columns, marking the cut - the same rule as `rano::width::text::truncate`
/// for text with no escapes left in it, which [`clean_line`] has already done.
fn truncate(s: &str, cols: usize) -> String {
    if width(s) <= cols {
        return s.to_string();
    }
    if cols == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for c in s.chars() {
        let w = char_width(c);
        if used + w > cols.saturating_sub(1) {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

fn width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

/// A codepoint's columns. The common half of rano's table: a combining mark takes none, the CJK,
/// Hangul and emoji blocks take two, everything else one.
fn char_width(c: char) -> usize {
    let u = c as u32;
    if is_control(c) || is_zero_width(u) {
        0
    } else if is_wide(u) {
        2
    } else {
        1
    }
}

/// C0, C1 and DEL: not a column, and never part of the cluster beside them.
fn is_control(c: char) -> bool {
    let u = c as u32;
    u < 0x20 || (0x7f..0xa0).contains(&u)
}

/// Combining marks, joiners and selectors: they attach to the character before them. The blocks a
/// name or a quote of prose actually holds, rather than rano's whole Unicode table, because a guest
/// is embedded in the binary and four thousand ranges would cost most of a kilobyte.
fn is_zero_width(u: u32) -> bool {
    matches!(u,
        0x0300..=0x036f      // combining diacritical marks, and the extended block
        | 0x0483..=0x0489 | 0x0591..=0x05c7 | 0x0610..=0x061a | 0x064b..=0x065f
        | 0x06d6..=0x06ed | 0x0900..=0x0963 | 0x0e31..=0x0e4e
        | 0x1ab0..=0x1aff | 0x1dc0..=0x1dff
        | 0x200b..=0x200f | 0x2028..=0x202e | 0x2060..=0x2064
        | 0x20d0..=0x20f0 | 0xfe00..=0xfe0f | 0xfe20..=0xfe2f | 0xfeff
        | 0xe0100..=0xe01ef
    )
}

/// East-Asian Wide and Fullwidth, plus the emoji planes a terminal renders double-width - block by
/// block rather than rano's per-character enumeration, for the reason above.
fn is_wide(u: u32) -> bool {
    matches!(u,
        0x1100..=0x115f      // Hangul Jamo initial
        | 0x2e80..=0x303e    // CJK radicals, Kangxi radicals, CJK symbols
        | 0x3041..=0x4dbf    // kana, Bopomofo, CJK unified ideographs
        | 0x4e00..=0xa4c6    // CJK unified ideographs, Yi
        | 0xa960..=0xd7a3    // Hangul Jamo extended-A, Hangul syllables
        | 0xf900..=0xfaff    // CJK compatibility ideographs
        | 0xfe10..=0xfe6b | 0xff01..=0xff60 | 0xffe0..=0xffe6
        | 0x16fe0..=0x18d08 | 0x1b000..=0x1b2fb
        | 0x1f300..=0x1f64f | 0x1f680..=0x1f6ff | 0x1f900..=0x1f9ff | 0x1fa70..=0x1faff
        | 0x20000..=0x3fffd  // CJK extension B and beyond
    )
}

/// The hue a handle speaks in: FNV-1a of the lowercased handle mod the palette, so one speaker
/// keeps one colour and a rename keeps it too (`crates/tui/src/ui/chat/colour.rs`).
fn hue(handle: &str) -> u8 {
    let mut h: u32 = 0x811c_9dc5;
    for b in handle.to_ascii_lowercase().bytes() {
        h = (h ^ b as u32).wrapping_mul(0x0100_0193);
    }
    (h % u32::from(HUES)) as u8
}

fn is_false(b: &bool) -> bool {
    !*b
}
