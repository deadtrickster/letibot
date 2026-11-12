//! The room's messages as the pane language's `stream`: the half of the chat a layer decides - runs
//! and headers, hues, mentions, dividers, the quote line. [`crate::ui::chat`] draws it without
//! knowing a `Msg` exists, which is the seam a chat plugin replaces. The one reader of a `Msg`.

use std::collections::BTreeMap;

use letibot_sessionlog::pane::{Cell, Divider, Mention, Role, Stream, StreamEntry};
use rano::agent::text::clean_line;
use rano::width::text::truncate;

use crate::app::rooms::{Chan, Msg, RoomsView, mentions_operator};
use crate::ui::chat::{Blob, hue};

/// How long a run of one speaker may pause before the next message opens a new one: ten minutes,
/// which is long enough for a burst and short enough that a new subject gets a header.
const RUN_MS: u64 = 10 * 60_000;

/// How much of the message a reply's quote line shows of it: a quote orients, the answer explains.
const QUOTE_COLS: usize = 60;

/// One channel as a stream: every message an entry, a divider where the day turns and where the
/// reader's own reading had got to, and each message's share of the decisions above.
pub(crate) fn stream(
    v: &RoomsView,
    c: &Chan,
    names: &BTreeMap<String, String>,
    me: &str,
    now_ms: u64,
) -> Stream {
    let msgs = v.messages(c);
    let mut s = Stream::default();
    if msgs.is_empty() {
        // The two empty channels are two facts: nothing was said, or nobody has answered yet.
        s.empty = if v.heard {
            "nothing here yet"
        } else {
            "asking the coordinator..."
        }
        .to_string();
        return s;
    }
    let opened = v.opened_seen.get(&RoomsView::key(c)).copied();
    let mut marked = false;
    let mut prev: Option<&Msg> = None;
    let mut prev_day: Option<Day> = None;
    for m in &msgs {
        let day = local(m.ts).map(|tm| Day::of(&tm));
        let mut divided = false;
        if day.is_some() && day != prev_day {
            s.entries.push(StreamEntry {
                divider: Some(Divider::Today),
                text: day.map(|d| d.label(now_ms)).unwrap_or_default(),
                ..StreamEntry::default()
            });
            prev_day = day;
            divided = true;
        }
        if let Some(at) = opened
            && !marked
            && m.seq > at
        {
            s.entries.push(StreamEntry {
                divider: Some(Divider::Unread),
                text: "unread".to_string(),
                ..StreamEntry::default()
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
                    && local(m.ts).map(|tm| Day::of(&tm)) == local(p.ts).map(|tm| Day::of(&tm))
            });
        // A continuation keeps its time only when the minute it was sent in differs from the one
        // above it: a row that repeated the run's own minute would say nothing.
        let same_minute = prev.is_some_and(|p| stamp(p) == stamp(m));
        s.entries
            .push(entry(m, &msgs, names, me, runs_on, same_minute));
        tokens(&mut s.mentions, &m.text, names, me);
        prev = Some(m);
    }
    s
}

/// One message as an entry. A continuation carries no head and no facts: what it has to say is its
/// body, and the time in its gutter when that differs from the message above it.
fn entry(
    m: &Msg,
    all: &[&Msg],
    names: &BTreeMap<String, String>,
    me: &str,
    runs_on: bool,
    same_minute: bool,
) -> StreamEntry {
    let mut e = StreamEntry {
        id: Some(m.seq.to_string()),
        ..StreamEntry::default()
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
        e.head = head(m, names, me);
        e.facts = vec![Cell::role(format!("#{}", m.seq), Role::Faint)];
        e.time = stamp(m);
    } else if !same_minute {
        e.time = stamp(m);
    }
    if let Some(t) = m.thread {
        e.quote = Some(quote(t, all, names));
    }
    e
}

/// The header's cells: the mark a message that names the reader wears, the speaker in its hue, and
/// who it was addressed to. Nothing else - a header is scanned, not read.
fn head(m: &Msg, names: &BTreeMap<String, String>, me: &str) -> Vec<Cell> {
    let mut out = Vec::new();
    if mentions_operator(m) {
        // A message that names the reader is worth knowing, and it is not a decision they owe:
        // `attention` is the register for what needs a person.
        out.push(Cell::role("!", Role::Warning));
    }
    let who = if m.author == me {
        Role::Strong
    } else {
        Role::Hue(hue(&m.author))
    };
    out.push(Cell::role(display(names, &m.author), who));
    if let Some(to) = m.to.as_deref().filter(|to| *to != me) {
        out.push(Cell::role(
            format!("to {}", display(names, to)),
            Role::Faint,
        ));
    }
    out
}

/// The line a reply answers: the number it names, and as much of the message as a quote line has
/// room for. A reply to a message this head never loaded still says which one it answers.
fn quote(t: u64, all: &[&Msg], names: &BTreeMap<String, String>) -> String {
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
/// reference-link syntax and rano draws it as the bare words inside it.
fn system_text(m: &Msg) -> String {
    let refs: String = m
        .refs
        .iter()
        .map(|r| format!(" \\[{} {}\\]", clean_line(&r.kind), clean_line(&r.id)))
        .collect();
    format!("· {}: {}{refs}", clean_line(&m.author), m.text)
}

/// What a handle is called: the roster's or the session list's name for it, the handle when nobody
/// named it.
fn display(names: &BTreeMap<String, String>, handle: &str) -> String {
    clean_line(names.get(handle).map(String::as_str).unwrap_or(handle))
}

/// The name each blob of the window was sent under, whether it is a picture, and the size the
/// store measured out of it. The renderer draws a picture where the terminal holds one; a text file
/// it only names.
pub(crate) fn blobs(v: &RoomsView, c: &Chan) -> BTreeMap<String, Blob> {
    let mut out = BTreeMap::new();
    for m in v.messages(c) {
        for a in &m.attachments {
            out.insert(
                a.id.clone(),
                Blob {
                    name: clean_line(&a.name),
                    picture: a.mime.starts_with("image/"),
                    size: a.w.zip(a.h),
                },
            );
        }
    }
    out
}

/// Every `@token` of a body the reader can be told more about: a member's handle reads as its name
/// in its own hue, and the reader's own handle is the one mention that is not quiet.
fn tokens(
    out: &mut BTreeMap<String, Mention>,
    text: &str,
    names: &BTreeMap<String, String>,
    me: &str,
) {
    for token in mentions(text) {
        let handle = token[1..].to_string();
        if handle.eq_ignore_ascii_case(me) {
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

/// The `@token`s of a text, with the `@`: the room's own lexer (`crates/room/src/chat.rs`) as this
/// head can reach it, which does not link that crate - a `@` on a word boundary, then a handle,
/// where `@` and `:` join a bridge's `cc@folder:name` into one token.
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

/// The time a message was sent at, as this head's clock prints it. `None` is a message the wire
/// carried no timestamp for, which is not midnight.
fn stamp(m: &Msg) -> Option<String> {
    local(m.ts).map(|tm| hhmm(&tm))
}

/// A local calendar date, as the clock of the machine the operator is sitting at has it.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Day {
    y: i32,
    m: u32,
    d: u32,
}

impl Day {
    fn of(tm: &libc::tm) -> Day {
        Day {
            y: tm.tm_year + 1900,
            m: (tm.tm_mon + 1) as u32,
            d: tm.tm_mday as u32,
        }
    }

    /// What the divider over this day says: `today` while it is, `Nov 7` inside this year, and the
    /// year too when the talk is older than that, which is the one thing a reader gets wrong.
    fn label(self, now_ms: u64) -> String {
        let today = local(now_ms).map(|tm| Day::of(&tm));
        match today {
            Some(t) if t == self => "today".to_string(),
            Some(t) if self.days() == t.days() - 1 => "yesterday".to_string(),
            Some(t) if t.y == self.y => format!("{} {}", month(self.m), self.d),
            _ => format!("{} {}, {}", month(self.m), self.d, self.y),
        }
    }

    /// This date as a day number, so two of them can be told apart by more than a day. Civil-from-
    /// days, by Hinnant: the arithmetic a calendar needs and no crate.
    fn days(self) -> i64 {
        let y = if self.m <= 2 {
            i64::from(self.y) - 1
        } else {
            i64::from(self.y)
        };
        let era = if y >= 0 { y } else { y - 399 } / 400;
        let yoe = y - era * 400;
        let mp = (i64::from(self.m) + 9) % 12;
        let doy = (153 * mp + 2) / 5 + i64::from(self.d) - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }
}

fn month(m: u32) -> &'static str {
    const NAMES: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    NAMES[usize::from(m as u16).clamp(1, 12) - 1]
}

/// A timestamp in the machine's own time zone. `None` for a message written before the wire had a
/// timestamp, which is not midnight 1970 and must not be drawn as if it were.
fn local(ms: u64) -> Option<libc::tm> {
    if ms == 0 {
        return None;
    }
    let secs = (ms / 1000) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `localtime_r` writes into `tm` and reads `secs`, both owned here; the `_r` form keeps
    // no shared static, which matters because the driver is not the only thread in this process.
    let ok = unsafe { !libc::localtime_r(&secs, &mut tm).is_null() };
    ok.then_some(tm)
}

fn hhmm(tm: &libc::tm) -> String {
    format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
}
