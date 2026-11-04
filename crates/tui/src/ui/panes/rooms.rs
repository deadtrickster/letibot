//! The `/rooms` view: the channel list on the left, the selected channel on the right. Text only,
//! cut to the width it is given, so it reads the same at 80 columns and at 120.

use crate::app::rooms::{Chan, ChatRef, Member, Msg, OPERATOR, RoomsView, Send, mentions_operator};
use crate::app::*;
use crate::ui::render::{row_strings, trim_to, visible_width, wrap};
use crate::ui::transcript::blocks::clock_time;
use rano::agent::pane;
use rano::agent::text::clean_line;
use rano::render::{Line, Span};
use rano::style::Role;

const SEP: &str = " │ ";

/// The channel list's width: wide enough for a project's short key and a count.
fn left_width(w: usize) -> usize {
    if w >= 110 {
        28
    } else if w >= 70 {
        22
    } else {
        (w / 3).max(10)
    }
}

fn label(c: &Chan) -> String {
    match c {
        Chan::Room(id) => match id.strip_prefix("project/") {
            Some(key) => format!("#{}", key.chars().take(8).collect::<String>()),
            None => id.clone(),
        },
        Chan::Dm(p) => clean_line(p),
    }
}

fn title(c: &Chan) -> String {
    match c {
        Chan::Room(id) => match id.strip_prefix("project/") {
            Some(key) => format!("project {key}"),
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
fn entry(c: &Chan, picked: bool, unread: u64, lw: usize) -> Line {
    let count = if unread > 0 {
        unread.to_string()
    } else {
        String::new()
    };
    let room = lw.saturating_sub(count.len() + 1).max(3);
    let name = trim_to(&format!("{} {}", pane::mark(picked), label(c)), room);
    let mut l = Line::new(vec![Span::raw(name)]);
    let pad = lw.saturating_sub(l.width() + count.len());
    l.push(Span::raw(" ".repeat(pad)));
    l.push(Span::role(count, Role::Attention));
    pane::picked(l, picked)
}

fn refs_text(refs: &[ChatRef]) -> String {
    refs.iter()
        .map(|r| format!(" [{} {}]", clean_line(&r.kind), clean_line(&r.id)))
        .collect()
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

/// A todo or queue line: quiet, one row, with the row it points at.
fn system_line(m: &Msg, rw: usize) -> Line {
    let text = format!(
        "  · {}: {}{}",
        clean_line(&m.author),
        clean_line(&m.text),
        refs_text(&m.refs)
    );
    pane::faint(trim_to(&text, rw))
}

fn message_lines(m: &Msg, rw: usize, with_time: bool) -> Vec<Line> {
    if m.kind == "system" {
        return vec![system_line(m, rw)];
    }
    let named = mentions_operator(m);
    let time = if with_time {
        let t = clock_time(m.ts);
        if t.len() >= 5 {
            format!("{} ", &t[..5])
        } else {
            String::new()
        }
    } else {
        String::new()
    };
    let seq = format!("{:>3}{}", m.seq, if named { '!' } else { ' ' });
    let author = format!("<{}> ", trim_to(&clean_line(&m.author), 12));
    let indent = visible_width(&seq) + visible_width(&time) + visible_width(&author);
    let cols = rw.saturating_sub(indent).max(8);
    let reply = m.thread.map(|t| format!("↳{t} ")).unwrap_or_default();

    let mut text = format!("{}{}", reply, m.text);
    text.push_str(&refs_text(&m.refs));
    let mut rows: Vec<String> = Vec::new();
    for para in text.split('\n') {
        let para = clean_line(para);
        if rows.is_empty() || !para.trim().is_empty() {
            rows.extend(wrap(&para, cols));
        }
    }
    if rows.is_empty() {
        rows.push(String::new());
    }

    let (mark_role, name_role, body_role) = if named {
        (Role::Attention, Role::Attention, Role::Attention)
    } else {
        (Role::Faint, Role::Strong, Role::Plain)
    };
    let mut out = Vec::with_capacity(rows.len());
    for (i, row) in rows.into_iter().enumerate() {
        if i == 0 {
            let mut spans = vec![
                Span::role(seq.clone(), mark_role),
                Span::role(time.clone(), Role::Faint),
                Span::role(author.clone(), name_role),
            ];
            match row.strip_prefix(&reply).filter(|_| !reply.is_empty()) {
                Some(rest) => {
                    spans.push(Span::role(reply.clone(), Role::Faint));
                    spans.push(Span::role(rest.to_string(), body_role));
                }
                None => spans.push(Span::role(row, body_role)),
            }
            out.push(Line::new(spans));
        } else {
            out.push(Line::new(vec![
                Span::raw(" ".repeat(indent)),
                Span::role(row, body_role),
            ]));
        }
    }
    out
}

fn roster_lines(c: &Chan, members: Option<&Vec<Member>>, rw: usize) -> Vec<Line> {
    if matches!(c, Chan::Dm(_)) {
        return vec![pane::faint("  a direct chat has two people in it")];
    }
    let Some(members) = members else {
        return vec![pane::faint("  asking the coordinator...")];
    };
    let mut out = vec![pane::faint(format!("  {} in this channel", members.len()))];
    for m in members {
        let dot = if m.connected { "●" } else { "○" };
        let mut text = format!("  {dot} {}  {}", clean_line(&m.name), clean_line(&m.kind));
        if !m.label.is_empty() {
            text.push_str(&format!("  {}", clean_line(&m.label)));
        }
        let role = if m.connected {
            Role::Plain
        } else {
            Role::Faint
        };
        out.push(Line::new(vec![Span::role(trim_to(&text, rw), role)]));
    }
    out
}

impl RoomsView {
    fn header(&self, c: &Chan, rw: usize) -> Line {
        let mut spans = vec![Span::role(title(c), Role::Strong)];
        if let Chan::Room(id) = c
            && let Some(r) = self.rows.iter().find(|r| &r.id == id)
        {
            spans.push(Span::role(format!("  {} here", r.members), Role::Faint));
        }
        if self.roster_open {
            spans.push(Span::role("  · roster (r closes)", Role::Faint));
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
    fn rooms_left(&self, lw: usize, room: usize) -> Vec<Line> {
        let channels = self.rooms.channels();
        let picked = self.rooms.selected();
        let mut lines = vec![padded(pane::faint(trim_to("channels", lw)), lw)];
        let mut sel_row = 0;
        let mut dm_heading = false;
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
            lines.push(entry(c, is_sel, unread, lw));
        }
        let skip = (sel_row + 1).saturating_sub(room);
        lines.into_iter().skip(skip).take(room).collect()
    }

    fn rooms_body(&self, rw: usize) -> Vec<Line> {
        let Some(c) = self.rooms.selected() else {
            // The selected channel is gone rather than empty, and the sentence says which:
            // an empty channel draws "nothing here yet", which would be a lie here.
            let which = self
                .rooms
                .gone
                .clone()
                .unwrap_or_else(|| "the channel".into());
            return vec![pane::faint(format!(
                "  {which} is gone - up or down selects a channel"
            ))];
        };
        if self.rooms.roster_open {
            let key = match &c {
                Chan::Room(id) => id.as_str(),
                Chan::Dm(_) => "",
            };
            return roster_lines(&c, self.rooms.roster.get(key), rw);
        }
        let msgs = self.rooms.messages(&c);
        let with_time = rw >= 64;
        let mut out: Vec<Line> = msgs
            .into_iter()
            .flat_map(|m| message_lines(m, rw, with_time))
            .collect();
        out.extend(send_lines(&self.rooms.sends, &c, rw));
        if out.is_empty() {
            let said = if self.rooms.heard {
                "  nothing here yet"
            } else {
                "  asking the coordinator..."
            };
            out.push(pane::faint(said));
        }
        out
    }

    pub(crate) fn rooms_lines(&mut self, w: usize, room: usize) -> Vec<String> {
        let palette = self.cfg.palette();
        if room < 3 || w < 24 {
            self.pane_len = 0;
            self.pane_room = room;
            return row_strings(
                &[pane::faint(trim_to("rooms: the window is too small", w))],
                palette,
            );
        }
        let lw = left_width(w);
        let rw = w.saturating_sub(lw + visible_width(SEP));
        let chat_room = room - 2;

        let left = self.rooms_left(lw, room);
        let body = self.rooms_body(rw);

        // Stay on the tail unless the reader scrolled back from it.
        let max = body.len().saturating_sub(chat_room);
        if self.pane_scroll >= self.rooms.prev_max {
            self.pane_scroll = max;
        }
        self.pane_scroll = self.pane_scroll.min(max);
        self.rooms.prev_max = max;
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
            .map_or_else(|| pane::title("no channel"), |c| self.rooms.header(&c, rw));
        right.insert(0, header);
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
            .map(|(l, r)| {
                let mut out = Line::default();
                // The left's own style (the picked row's inverse) stays on the left alone.
                for s in &l.spans {
                    out.push(Span::styled(s.content.clone(), l.style.patch(&s.style)));
                }
                out.push(Span::role(SEP, Role::Faint));
                for s in &r.spans {
                    out.push(Span::styled(s.content.clone(), r.style.patch(&s.style)));
                }
                out
            })
            .collect();
        row_strings(&rows, palette)
    }
}
