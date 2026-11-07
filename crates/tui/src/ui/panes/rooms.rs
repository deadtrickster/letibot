//! The `/rooms` view: the channel list on the left, the selected channel on the right. Text only,
//! cut to the width it is given, so it reads the same at 80 columns and at 120.

use std::collections::BTreeMap;

use crate::app::chat_adapter;
use crate::app::rooms::{Chan, Member, OPERATOR, RoomsView, Send};
use crate::app::*;
use crate::ui::chat::{ChatCtx, chat_blocks};
use crate::ui::render::{row_strings, trim_to, visible_width, wrap};
use rano::agent::pane;
use rano::agent::text::clean_line;
use rano::render::{Line, Span};
use rano::style::Role;

const SEP: &str = " │ ";

/// The channel column's floor: enough for the column's own heading ("direct messages"), so a
/// short list does not shrink the column into a sliver that cuts every name.
const LEFT_FLOOR: usize = 16;

impl App {
    /// The channel column's width: what the widest label and its count need, capped so the
    /// chat keeps at least half the pane. The labels decide, not a fixed tier - two projects
    /// whose keys share a prefix must both fit whole at any ordinary width.
    pub(crate) fn rooms_left_width(&self, w: usize) -> usize {
        let mut needed = LEFT_FLOOR;
        for c in self.rooms.channels() {
            let count = self.rooms.unread(&c).to_string().len();
            let name = visible_width(&label(&self.rooms, &c));
            needed = needed.max(2 + name + 1 + count);
        }
        let cap = (w / 2).saturating_sub(visible_width(SEP) + 1);
        needed.min(cap.max(12))
    }
}

/// A name cut to `n` columns, keeping BOTH ends around `…`: two names that share a prefix
/// stay distinguishable by their tail, which an end cut would take.
fn cut_keeping_ends(s: &str, n: usize) -> String {
    if visible_width(s) <= n {
        return s.to_string();
    }
    if n <= 1 {
        return "…".into();
    }
    let budget = n - 1;
    let chars: Vec<char> = s.chars().collect();
    let head_at = budget.div_ceil(2);
    let head: String = chars[..head_at.min(chars.len())].iter().collect();
    let mut tail: String = if chars.len() > head_at {
        chars[chars.len() - (budget / 2).min(chars.len() - head_at)..]
            .iter()
            .collect()
    } else {
        String::new()
    };
    // A wide glyph can make the kept ends wider than the column asked for; drop from the tail
    // until it fits rather than push the count off the column's edge.
    while visible_width(&format!("{head}…{tail}")) > n && !tail.is_empty() {
        tail.pop();
    }
    format!("{head}…{tail}")
}

/// The project's folder name, as the coordinator labelled it.
fn label(v: &RoomsView, c: &Chan) -> String {
    match c {
        Chan::Room(id) => match id.strip_prefix("project/") {
            Some(_) => format!("#{}", clean_line(&v.room_label(id))),
            None => id.clone(),
        },
        Chan::Dm(p) => clean_line(p),
    }
}

fn title(v: &RoomsView, c: &Chan) -> String {
    match c {
        Chan::Room(id) => match id.strip_prefix("project/") {
            Some(_) => format!("project {}", clean_line(&v.room_label(id))),
            None => format!("{id} room"),
        },
        Chan::Dm(p) => format!("dm with {}", clean_line(p)),
    }
}

/// `l` padded with spaces to `w` columns. Callers cut first.
fn padded(mut l: Line, w: usize) -> Line {
    let cur = l.width();
    if cur < w {
        l.push(Span::raw(" ".repeat(w - cur)));
    }
    l
}

/// One channel row: mark, name, and the unread count at the right edge.
fn entry(v: &RoomsView, c: &Chan, picked: bool, unread: u64, lw: usize) -> Line {
    let count = if unread > 0 {
        unread.to_string()
    } else {
        String::new()
    };
    let room = lw.saturating_sub(count.len() + 1).max(3);
    let name = cut_keeping_ends(&format!("{} {}", pane::mark(picked), label(v, c)), room);
    let mut l = Line::new(vec![Span::raw(name)]);
    let pad = lw.saturating_sub(l.width() + count.len());
    l.push(Span::raw(" ".repeat(pad)));
    l.push(Span::role(count, Role::Attention));
    pane::picked(l, picked)
}

/// A send of ours that has not landed: `…` while it is out, `✗` and the reason once it failed.
/// Drawn at the tail of its own channel, where the message would have been - it stays until
/// the operator resends it or dismisses it, whatever the polls bring back.
fn send_lines(sends: &[Send], c: &Chan, rw: usize) -> Vec<Line> {
    let mut out = Vec::new();
    let cols = rw.saturating_sub(2).max(8);
    for s in sends.iter().filter(|s| s.chan == *c) {
        match &s.error {
            None => {
                for row in wrap(&format!("… {}", clean_line(&s.text)), cols) {
                    out.push(pane::faint(format!("  {row}")));
                }
            }
            Some(why) => {
                for row in wrap(&format!("✗ {}", clean_line(&s.text)), cols) {
                    out.push(Line::new(vec![Span::role(
                        format!("  {row}"),
                        Role::Failure,
                    )]));
                }
                for row in wrap(&format!("{why} · enter resends, d dismisses"), cols) {
                    out.push(pane::faint(format!("  {row}")));
                }
            }
        }
    }
    out
}

/// The chat keeps at least this many columns before the member column takes any.
const CHAT_FLOOR: usize = 40;
const MEMBER_MAX: usize = 28;
const MEMBER_MIN: usize = 14;

/// A kind's marker: session, operator, cc/external, gatekeeper/queue.
fn kind_mark(kind: &str) -> &'static str {
    match kind {
        "session" => "s",
        "operator" => "op",
        "external" => "cc",
        "queue" | "gatekeeper" => "q",
        _ => "?",
    }
}

/// The members to show: connected first, each group by name, offline after only when `all`.
/// Returns the shown and how many offline members are not listed.
fn ordered<'a>(
    members: &'a [Member],
    all: bool,
    names: &BTreeMap<String, String>,
) -> (Vec<&'a Member>, usize) {
    let shown_name = |m: &Member| clean_line(names.get(&m.name).unwrap_or(&m.name));
    let mut on: Vec<&Member> = members.iter().filter(|m| m.connected).collect();
    let mut off: Vec<&Member> = members.iter().filter(|m| !m.connected).collect();
    on.sort_by_key(|m| shown_name(m));
    off.sort_by_key(|m| shown_name(m));
    let hidden = if all { 0 } else { off.len() };
    if all {
        on.extend(off);
    }
    (on, hidden)
}

/// The member column's rows: a heading, the members, then the offline count. Rows past `room`
/// are replaced by a `+N more` line, never dropped unsaid. Returns each member row with its
/// handle for the click.
fn member_column(
    room_label: &str,
    members: Option<&Vec<Member>>,
    all: bool,
    names: &BTreeMap<String, String>,
    room: usize,
) -> (Vec<Line>, Vec<(usize, String)>, usize) {
    let Some(members) = members else {
        return (vec![pane::faint("asking...")], Vec::new(), MEMBER_MIN);
    };
    let online = members.iter().filter(|m| m.connected).count();
    let (list, hidden) = ordered(members, all, names);
    let mut rows: Vec<(String, Role, Option<String>)> =
        vec![(format!("{online} here"), Role::Faint, None)];
    for m in &list {
        let dot = if m.connected { "●" } else { "○" };
        let name = clean_line(names.get(&m.name).unwrap_or(&m.name));
        let mut text = format!("{dot} {} {name}", kind_mark(&m.kind));
        if !m.project.is_empty() && m.project != room_label {
            text.push_str(&format!(" ({})", clean_line(&m.project)));
        }
        let role = if m.connected {
            Role::Plain
        } else {
            Role::Faint
        };
        rows.push((text, role, Some(m.name.clone())));
    }
    if hidden > 0 {
        rows.push((format!("+{hidden} offline (r lists)"), Role::Faint, None));
    } else if members.iter().any(|m| !m.connected) {
        rows.push(("r hides the offline".into(), Role::Faint, None));
    }
    let width = rows
        .iter()
        .map(|(t, _, _)| visible_width(t))
        .max()
        .unwrap_or(0)
        .clamp(MEMBER_MIN, MEMBER_MAX);
    if rows.len() > room {
        let more = rows.len() - (room - 1);
        rows.truncate(room - 1);
        rows.push((format!("+{more} more"), Role::Faint, None));
    }
    let mut lines = Vec::new();
    let mut handles = Vec::new();
    for (i, (t, role, h)) in rows.into_iter().enumerate() {
        lines.push(Line::new(vec![Span::role(trim_to(&t, width), role)]));
        if let Some(h) = h {
            handles.push((i, h));
        }
    }
    (lines, handles, width)
}

/// One line under the channel header for a pane too narrow for the column: `N here: a, b +M`.
fn member_strip(
    members: Option<&Vec<Member>>,
    names: &BTreeMap<String, String>,
    rw: usize,
) -> Line {
    let Some(members) = members else {
        return pane::faint(trim_to("asking who is here...", rw));
    };
    let (list, _) = ordered(members, false, names);
    let n = list.len();
    let all: Vec<String> = list
        .iter()
        .map(|m| clean_line(names.get(&m.name).unwrap_or(&m.name)))
        .collect();
    let head = format!("{n} here");
    if all.is_empty() {
        return pane::faint(trim_to(&head, rw));
    }
    // Names go in while the `+M` that would follow still fits.
    let mut shown: Vec<&str> = Vec::new();
    for i in 0..all.len() {
        let rest = all.len() - i - 1;
        let tail = if rest > 0 {
            format!(" +{rest}")
        } else {
            String::new()
        };
        let mut next = shown.clone();
        next.push(&all[i]);
        if visible_width(&format!("{head}: {}{tail}", next.join(", "))) > rw {
            break;
        }
        shown.push(&all[i]);
    }
    let more = all.len() - shown.len();
    let mut text = format!("{head}: {}", shown.join(", "));
    if more > 0 {
        text.push_str(&format!(" +{more}"));
    }
    pane::faint(trim_to(&text, rw))
}

impl RoomsView {
    fn header(&self, c: &Chan, rw: usize) -> Line {
        let mut spans = vec![Span::role(title(self, c), Role::Strong)];
        if let Chan::Room(id) = c
            && let Some(r) = self.rows.iter().find(|r| &r.id == id)
        {
            spans.push(Span::role(format!("  {} here", r.members), Role::Faint));
        }
        let l = Line::new(spans);
        if l.width() > rw {
            pane::title(trim_to(&l.plain(), rw))
        } else {
            l
        }
    }

    fn footer(&self, rw: usize) -> Line {
        let text = match (&self.error, self.heard) {
            (Some(e), _) => {
                return Line::new(vec![Span::role(trim_to(&clean_line(e), rw), Role::Failure)]);
            }
            (None, false) => "asking the coordinator...".to_string(),
            (None, true) => {
                // With a failed line in the channel the keys are the whole point: the reason
                // sits on the line, and what can be done about it is the footer's job.
                if self
                    .selected()
                    .is_some_and(|c| self.failed_of(&c).is_some())
                {
                    format!(
                        "posting as {OPERATOR} · enter resends the failed line · d dismisses it"
                    )
                } else {
                    format!("posting as {OPERATOR} · >N text replies to #N")
                }
            }
        };
        pane::faint(trim_to(&text, rw))
    }
}

impl App {
    fn rooms_left(&mut self, lw: usize, room: usize) -> Vec<Line> {
        let channels = self.rooms.channels();
        let picked = self.rooms.selected();
        let mut lines = vec![padded(pane::faint(trim_to("Rooms", lw)), lw)];
        let mut sel_row = 0;
        let mut dm_heading = false;
        let mut at: Vec<(usize, Chan)> = Vec::new();
        for c in channels.iter() {
            if matches!(c, Chan::Dm(_)) && !dm_heading {
                dm_heading = true;
                lines.push(padded(pane::faint("direct messages"), lw));
            }
            let is_sel = picked.as_ref() == Some(c);
            if is_sel {
                sel_row = lines.len();
            }
            let unread = if is_sel { 0 } else { self.rooms.unread(c) };
            at.push((lines.len(), c.clone()));
            lines.push(entry(&self.rooms, c, is_sel, unread, lw));
        }
        let skip = (sel_row + 1).saturating_sub(room);
        self.rooms.chan_rows = at
            .into_iter()
            .filter_map(|(r, c)| r.checked_sub(skip).filter(|d| *d < room).map(|d| (d, c)))
            .collect();
        lines.into_iter().skip(skip).take(room).collect()
    }

    /// What a session is called: its title or slug in this head's session list, else the
    /// label the roster carries, else its id.
    fn rooms_names(&self) -> BTreeMap<String, String> {
        let mut names = BTreeMap::new();
        for list in self.rooms.roster.values() {
            for m in list {
                if !m.label.is_empty() {
                    names.insert(m.name.clone(), m.label.clone());
                }
            }
        }
        for b in &self.sessions {
            let named = if b.slug.trim().is_empty() {
                b.title.trim()
            } else {
                b.slug.trim()
            };
            if !named.is_empty() {
                names.insert(b.session_id.clone(), named.to_string());
            }
        }
        names
    }

    /// The chat column: the selected channel as a stream of message blocks, and the sends of ours
    /// that have not landed. The first entry's `seq` comes back with the rows because a page of
    /// older messages arriving above must not move the reader.
    fn rooms_body(&self, rw: usize) -> (Vec<Line>, Option<u64>) {
        let Some(c) = self.rooms.selected() else {
            // The selected channel is gone rather than empty, and the sentence says which:
            // an empty channel draws "nothing here yet", which would be a lie here.
            let which = self.rooms.gone.clone().unwrap_or_else(|| "the room".into());
            return (
                vec![pane::faint(format!(
                    "  {which} is gone - up or down selects a room"
                ))],
                None,
            );
        };
        // The channel as a layer sees it: what a message means is the layer's to say - a chat
        // plugin's when one answers, the adapter's when none does - and this pane draws the stream
        // it answers without ever reading a `Msg`.
        let names = self.rooms_names();
        let stream = self.chat_stream(&c, &names);
        let blobs = chat_adapter::blobs(&self.rooms, &c);
        let picture = |blob: &str| self.room_picture(blob);
        // P4's cursor is a `seq`; an entry's id is what the renderer draws as selected.
        let cursor = self.rooms_cursor().map(|seq| seq.to_string());
        let ctx = ChatCtx {
            width: rw,
            cfg: &self.cfg,
            picture: &picture,
            blobs: &blobs,
            selected: cursor.as_deref(),
            highlight: self.rooms_search_needle(),
        };
        let blocks = chat_blocks(&stream, &ctx);
        let mut out: Vec<Line> = Vec::new();
        if let Chan::Room(id) = &c
            && !blocks.is_empty()
        {
            out.push(pane::faint(if self.rooms.oldest_known.contains(id) {
                "  start of the stored history"
            } else {
                "  scroll up for older messages"
            }));
        }
        out.extend(blocks.iter().flat_map(|b| b.lines.iter().cloned()));
        if blocks.is_empty() {
            // The layer's own sentence: an empty channel and one nobody has answered are two
            // facts, and only the layer knows which one this is.
            out.push(pane::faint(format!("  {}", clean_line(&stream.empty))));
        }
        out.extend(send_lines(&self.rooms.sends, &c, rw));
        (out, blocks.first().and_then(|b| b.seq()))
    }

    pub(crate) fn rooms_lines(&mut self, w: usize, room: usize) -> Vec<String> {
        let palette = self.cfg.palette();
        self.rooms.member_rows.clear();
        self.rooms.member_col = (0, 0);
        self.rooms.chan_rows.clear();
        if room < 3 || w < 24 {
            self.pane_len = 0;
            self.pane_room = room;
            return row_strings(
                &[pane::faint(trim_to("rooms: the window is too small", w))],
                palette,
            );
        }
        let lw = self.rooms_left_width(w);
        // Recorded for the same reason click arithmetic is: a test reading the column must
        // not re-derive a width the frame already computed.
        self.rooms_left_w = lw;
        let full = w.saturating_sub(lw + visible_width(SEP));
        let names = self.rooms_names();
        let room_key = match self.rooms.selected() {
            Some(Chan::Room(id)) => Some(id),
            _ => None,
        };
        let members = room_key
            .as_ref()
            .and_then(|id| self.rooms.roster.get(id))
            .cloned();
        let column = room_key.as_ref().map(|id| {
            member_column(
                &self.rooms.room_label(id),
                members.as_ref(),
                self.rooms.roster_all,
                &names,
                room,
            )
        });
        // The column takes its width from its rows, but never the chat's floor.
        let wide = column
            .as_ref()
            .is_some_and(|(_, _, mw)| full >= CHAT_FLOOR + visible_width(SEP) + mw);
        let (mlines, mw) = match column {
            Some((lines, handles, mw)) if wide => {
                self.rooms.member_rows = handles;
                (lines, mw)
            }
            _ => (Vec::new(), 0),
        };
        let rw = if wide {
            full - mw - visible_width(SEP)
        } else {
            full
        };
        let strip = (room_key.is_some() && !wide && room >= 5)
            .then(|| member_strip(members.as_ref(), &names, rw));
        let chat_room = room - 2 - usize::from(strip.is_some());
        if wide {
            self.rooms.member_col = (lw + visible_width(SEP) + rw + visible_width(SEP), mw);
        }

        let left = self.rooms_left(lw, room);
        let (body, first) = self.rooms_body(rw);
        // The body has asked the chat layer by now: a layer that could not answer is a fallback
        // drawn here, and this is the frame that owes the reason - once, not every frame.
        self.chat_note();

        // Stay on the tail unless the reader scrolled back from it.
        let max = body.len().saturating_sub(chat_room);
        // A page of older messages landing above keeps the lines the reader was on.
        let key = self.rooms.selected().map(|c| RoomsView::key(&c));
        if let (Some((k, seq, len)), Some(key), Some(first)) =
            (self.rooms.prev_first.clone(), key.clone(), first)
            && k == key
            && first < seq
            && self.pane_scroll < self.rooms.prev_max
        {
            self.pane_scroll += body.len().saturating_sub(len);
        }
        self.rooms.prev_first = key.zip(first).map(|(k, f)| (k, f, body.len()));
        if self.pane_scroll >= self.rooms.prev_max {
            self.pane_scroll = max;
        }
        self.pane_scroll = self.pane_scroll.min(max);
        self.rooms.prev_max = max;
        self.rooms.at_top = self.pane_scroll == 0;
        self.rooms_page_back();
        self.pane_len = body.len();
        self.pane_room = chat_room;

        let mut shown: Vec<Line> = body
            .into_iter()
            .skip(self.pane_scroll)
            .take(chat_room)
            .collect();
        // Newest at the bottom, as in a chat.
        let mut right: Vec<Line> = vec![Line::default(); chat_room - shown.len()];
        right.append(&mut shown);
        let header = self
            .rooms
            .selected()
            .map_or_else(|| pane::title("no room"), |c| self.rooms.header(&c, rw));
        right.insert(0, header);
        if let Some(l) = strip {
            right.insert(1, l);
        }
        right.push(self.rooms.footer(rw));

        let rows: Vec<Line> = left
            .into_iter()
            .map(|l| padded(l, lw))
            .chain(std::iter::repeat_with(|| padded(Line::default(), lw)))
            .zip(
                right
                    .into_iter()
                    .chain(std::iter::repeat_with(Line::default)),
            )
            .take(room)
            .enumerate()
            .map(|(i, (l, r))| {
                let mut out = Line::default();
                // The left's own style (the picked row's inverse) stays on the left alone.
                for s in &l.spans {
                    out.push(Span::styled(s.content.clone(), l.style.patch(&s.style)));
                }
                out.push(Span::role(SEP, Role::Faint));
                let r = if wide { padded(r, rw) } else { r };
                for s in &r.spans {
                    out.push(Span::styled(s.content.clone(), r.style.patch(&s.style)));
                }
                if let Some(m) = mlines.get(i) {
                    out.push(Span::role(SEP, Role::Faint));
                    for sp in &m.spans {
                        out.push(sp.clone());
                    }
                }
                out
            })
            .collect();
        row_strings(&rows, palette)
    }
}
