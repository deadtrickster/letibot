//! The `/rooms` view: the channel list on the left, the selected channel on the right. Text only,
//! cut to the width it is given, so it reads the same at 80 columns and at 120.

use std::collections::BTreeMap;

use letibot_sessionlog::pane::{Divider, Stream, StreamEntry};

use crate::app::chat_adapter;
use crate::app::rooms::{Chan, ListRow, Member, OPERATOR, RoomsView, Send, SessInfo};
use crate::app::*;
use crate::ui::chat::{Block, ChatCtx, chat_blocks};
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
            if let Chan::Session(id) = &c
                && let Some(s) = self.rooms.sessions.iter().find(|s| &s.id == id)
            {
                needed = needed.max(2 + visible_width(&session_facts(&self.rooms, s)));
            }
        }
        // The scorecard's lines set the column's width too, up to the cap: a line cut to the floor
        // would hide the numbers it is there for.
        if let Some(card) = &self.rooms.signals {
            for r in &card.senders {
                needed = needed.max(2 + visible_width(&r.line()));
            }
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
        Chan::Dm(p) if v.sessions.iter().any(|s| &s.id == p) => "  dm".into(),
        // A DM's channel is keyed by the peer's identity, and drawn by the label the roster holds
        // for it now - a renamed seat is not a channel list full of names nobody answers to.
        Chan::Dm(p) => peer(v, p),
        Chan::Session(id) => v
            .sessions
            .iter()
            .find(|s| &s.id == id)
            .map_or_else(|| clean_line(id), |s| clean_line(&s.name)),
        Chan::AllDms => "all dms".into(),
    }
}

/// A peer's label, by the identity the channel is keyed on.
fn peer(v: &RoomsView, id: &str) -> String {
    v.roster_label(id)
        .map_or_else(|| clean_line(id), |n| clean_line(&n))
}

fn title(v: &RoomsView, c: &Chan) -> String {
    match c {
        Chan::Room(id) => match id.strip_prefix("project/") {
            Some(_) => format!("project {}", clean_line(&v.room_label(id))),
            None => format!("{id} room"),
        },
        Chan::Dm(p) => format!("dm with {}", peer(v, p)),
        Chan::Session(_) => "session".into(),
        Chan::AllDms => "every dm on the box".into(),
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

/// The second line of a session row: its project and its state, the coordinator's derived
/// presence when a roster holds it, else the head's own running/idle/asking.
fn session_facts(v: &RoomsView, s: &SessInfo) -> String {
    let state = v
        .seat_of(&s.id)
        .filter(|seat| !seat.says.is_empty())
        .map_or_else(|| s.state.to_string(), |seat| clean_line(&seat.says));
    if s.project.is_empty() {
        state
    } else {
        format!("{} · {}", clean_line(&s.project), state)
    }
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
    l.push(Span::role(count, Role::Warning));
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
                for row in wrap(&format!("{why} · enter resends, del dismisses"), cols) {
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
        // The derived presence under its member, the same click: the coordinator's words.
        if let Some(seat) = m.seat.as_ref().filter(|x| !x.says.is_empty()) {
            let mark = letibot_transcript::pending::mark(&seat.pending);
            let role = if seat.cannot_act() || mark.is_some() {
                Role::Attention
            } else {
                Role::Faint
            };
            rows.push((
                format!("{} {}", mark.unwrap_or(' '), clean_line(&seat.says)),
                role,
                Some(m.name.clone()),
            ));
            if !seat.doing.is_empty() {
                rows.push((
                    format!("  {}", clean_line(&seat.doing)),
                    Role::Faint,
                    Some(m.name.clone()),
                ));
            }
            if !seat.owns.is_empty() {
                rows.push((
                    format!("  owns {}", clean_line(&seat.owns.join(", "))),
                    Role::Faint,
                    Some(m.name.clone()),
                ));
            }
        }
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
        if let Chan::Session(_) = c {
            spans.push(Span::role(format!("  {}", label(self, c)), Role::Faint));
        }
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
            // Every dm on the box is a read: the operator is a party to almost none of those rows,
            // so there is nothing to type here and the line says so rather than offering a box.
            (None, true) if matches!(self.selected(), Some(Chan::AllDms)) => {
                "every dm on the box · read-only · d on a session opens your own dm with it".into()
            }
            (None, true) => {
                // With a failed line in the channel the keys are the whole point: the reason
                // sits on the line, and what can be done about it is the footer's job.
                if self
                    .selected()
                    .is_some_and(|c| self.failed_of(&c).is_some())
                {
                    format!(
                        "posting as {OPERATOR} · enter resends the failed line · del dismisses it"
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
    /// The sidebar's `h` rows, `w` columns each, drawn with the pane's own list. The click rows it
    /// leaves are the sidebar's, and the pane's are put back.
    pub(crate) fn sidebar_lines(&mut self, w: usize, h: usize) -> Vec<String> {
        let palette = self.cfg.palette();
        let saved = std::mem::take(&mut self.rooms.chan_rows);
        let lines = self.rooms_left(w, h);
        let drawn = std::mem::replace(&mut self.rooms.chan_rows, saved);
        self.sidebar.rows = drawn.into_iter().collect();
        let lines: Vec<Line> = lines.into_iter().map(|l| padded(l, w)).collect();
        row_strings(&lines, palette)
    }

    /// The Queues section: a heading and two lines a repository. The first is health, name and the
    /// unread queue moves, the second the counts and the mute checkbox at its right edge.
    fn queue_section(&self, lines: &mut Vec<Line>, at: &mut Vec<(usize, ListRow)>, lw: usize) {
        let repos = self.queue_repos();
        if repos.is_empty() {
            return;
        }
        lines.push(padded(pane::faint("Queues"), lw));
        for r in repos {
            let (dot, role) = match r.health {
                crate::app::queue_rows::Health::Green => ("●", Role::Success),
                crate::app::queue_rows::Health::Red => ("●", Role::Failure),
                crate::app::queue_rows::Health::Unjudged => ("○", Role::Faint),
            };
            let count = if r.unread > 0 {
                r.unread.to_string()
            } else {
                String::new()
            };
            let name = cut_keeping_ends(&r.name, lw.saturating_sub(count.len() + 3).max(3));
            let mut l = Line::new(vec![
                Span::raw("  "),
                Span::role(dot, role),
                Span::raw(format!(" {name}")),
            ]);
            let pad = lw.saturating_sub(l.width() + count.len());
            l.push(Span::raw(" ".repeat(pad)));
            l.push(Span::role(count, Role::Warning));
            at.push((lines.len(), ListRow::Queue(r.repo.clone())));
            let held = self.rooms.queue_sel.as_deref() == Some(r.repo.as_str());
            lines.push(if held { pane::picked(l, true) } else { l });
            let facts = format!("  {}w {}g {}p", r.waiting, r.gating, r.person);
            let check = if r.muted { "[x]" } else { "[ ]" };
            let mut l2 = Line::new(vec![Span::role(
                trim_to(&facts, lw.saturating_sub(4)),
                Role::Faint,
            )]);
            let pad = lw.saturating_sub(l2.width() + 3);
            l2.push(Span::raw(" ".repeat(pad)));
            l2.push(Span::role(
                check,
                if r.muted { Role::Warning } else { Role::Faint },
            ));
            // The row opens the queue, the checkbox mutes it: the checkbox is the later, narrower
            // hit, so it wins its three cells.
            at.push((lines.len(), ListRow::Queue(r.repo.clone())));
            at.push((lines.len(), ListRow::Mute(r.repo)));
            lines.push(l2);
        }
    }

    /// One line per sender of the coordinator's scorecard. Every sender is listed: the list
    /// scrolls with the rest, and `/signals` says the same rows in full.
    fn signal_section(&self, lines: &mut Vec<Line>, lw: usize) {
        let Some(card) = self
            .rooms
            .signals
            .as_ref()
            .filter(|c| !c.senders.is_empty())
        else {
            return;
        };
        lines.push(padded(
            pane::faint(trim_to(&format!("Signal {}m", card.window_ms / 60_000), lw)),
            lw,
        ));
        for r in &card.senders {
            lines.push(padded(
                pane::faint(trim_to(&format!("  {}", r.line()), lw)),
                lw,
            ));
        }
        if card.forgotten > 0 {
            lines.push(padded(
                pane::faint(trim_to(
                    &format!("  {} older not counted", card.forgotten),
                    lw,
                )),
                lw,
            ));
        }
    }

    fn rooms_left(&mut self, lw: usize, room: usize) -> Vec<Line> {
        let channels = self.rooms.channels();
        let picked = self.rooms.selected();
        let mut lines = vec![padded(pane::faint(trim_to("Rooms", lw)), lw)];
        let mut sel_row = 0;
        let mut dm_heading = false;
        let mut at: Vec<(usize, ListRow)> = Vec::new();
        let mut sess_heading = false;
        for c in channels.iter() {
            if matches!(c, Chan::Session(_)) && !sess_heading {
                sess_heading = true;
                lines.push(padded(pane::faint("Sessions"), lw));
            }
            let of_session =
                matches!(c, Chan::Dm(p) if self.rooms.sessions.iter().any(|s| &s.id == p));
            if matches!(c, Chan::Dm(_)) && !of_session && !dm_heading {
                dm_heading = true;
                lines.push(padded(pane::faint("direct messages"), lw));
            }
            let is_sel = picked.as_ref() == Some(c) && self.rooms.queue_sel.is_none();
            if is_sel {
                sel_row = lines.len();
            }
            let unread = if is_sel { 0 } else { self.rooms.unread(c) };
            at.push((lines.len(), ListRow::Chan(c.clone())));
            lines.push(entry(&self.rooms, c, is_sel, unread, lw));
            if let Chan::Session(id) = c
                && let Some(s) = self.rooms.sessions.iter().find(|s| &s.id == id)
            {
                // The facts are the same row's: a click on either line is the session's.
                at.push((lines.len(), ListRow::Chan(c.clone())));
                let facts = trim_to(&format!("  {}", session_facts(&self.rooms, s)), lw);
                let line = if self.rooms.seat_of(id).is_some_and(|x| x.cannot_act()) {
                    Line::new(vec![Span::role(facts, Role::Attention)])
                } else {
                    pane::faint(facts)
                };
                lines.push(padded(line, lw));
            }
        }
        self.queue_section(&mut lines, &mut at, lw);
        self.signal_section(&mut lines, lw);
        let skip = (sel_row + 1).saturating_sub(room);
        self.rooms.chan_rows = at
            .into_iter()
            .filter_map(|(r, c)| r.checked_sub(skip).filter(|d| *d < room).map(|d| (d, c)))
            .collect();
        lines.into_iter().skip(skip).take(room).collect()
    }

    /// The body of a selected session row: who it is and the two keys that act on it.
    fn session_card(&self, id: &str, rw: usize) -> Vec<Line> {
        let Some(s) = self.rooms.sessions.iter().find(|s| s.id == id) else {
            return vec![pane::faint("  this session is gone")];
        };
        let dm = self.rooms.unread(&Chan::Dm(id.to_string()));
        let mut out = vec![
            pane::faint(format!(
                "  {}",
                trim_to(&session_facts(&self.rooms, s), rw.saturating_sub(2))
            )),
            pane::faint(format!("  id {id}")),
        ];
        if let Some(doing) = self
            .rooms
            .seat_of(id)
            .map(|x| x.doing.as_str())
            .filter(|d| !d.is_empty())
        {
            out.push(pane::faint(format!(
                "  {}",
                trim_to(&clean_line(doing), rw.saturating_sub(2))
            )));
        }
        if let Some(owns) = self
            .rooms
            .seat_of(id)
            .filter(|x| !x.owns.is_empty())
            .map(|x| x.owns.join(", "))
        {
            out.push(pane::faint(format!(
                "  owns {}",
                trim_to(&clean_line(&owns), rw.saturating_sub(7))
            )));
        }
        if dm > 0 {
            out.push(Line::new(vec![Span::role(
                format!("  {dm} unread in the DM"),
                Role::Warning,
            )]));
        }
        out.push(pane::faint("  enter switches to it · d opens the DM"));
        out
    }

    /// What a session is called, by the one rule for every handle ([`App::rooms_display_name`]): a
    /// chat layer is handed this map and the completion reads that same rule, so two precedences
    /// for one handle would be a name that changes between the stream and the row.
    pub(crate) fn rooms_names(&self) -> BTreeMap<String, String> {
        let mut handles: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        for list in self.rooms.roster.values() {
            for m in list {
                handles.insert(m.name.as_str());
                // The identity as well as the label: a renamed seat's row names only its label,
                // and a message is authored by the identity.
                if !m.id.is_empty() {
                    handles.insert(m.id.as_str());
                }
            }
        }
        for b in &self.sessions {
            handles.insert(b.session_id.as_str());
        }
        let mut names = BTreeMap::new();
        for handle in handles {
            let name = self.rooms_display_name(handle);
            // A handle nobody named is the handle: the map says only where it is worth saying.
            if name != handle {
                names.insert(handle.to_string(), name);
            }
        }
        names
    }

    /// The chat column: the selected channel as a stream of message blocks, and the sends of ours
    /// that have not landed. The first message entry's `seq` comes back with the rows because a page
    /// of older messages arriving above must not move the reader, and so does the line the unread
    /// divider is drawn on, because a channel opens there.
    fn rooms_body(&self, rw: usize) -> Body {
        let Some(c) = self.rooms.selected() else {
            // The selected channel is gone rather than empty, and the sentence says which:
            // an empty channel draws "nothing here yet", which would be a lie here.
            let which = self.rooms.gone.clone().unwrap_or_else(|| "the room".into());
            let why = match self.rooms.gone_archived {
                true => format!("  {which} is archived - up or down selects a room"),
                false => format!("  {which} is gone - up or down selects a room"),
            };
            return Body::sentence(vec![pane::faint(why)]);
        };
        if let Chan::Session(id) = &c {
            return Body::sentence(self.session_card(id, rw));
        }
        // The channel as a layer sees it: what a message means is the layer's to say - a chat
        // plugin's when one answers, the adapter's when none does - and this pane draws the stream
        // it answers without ever reading a `Msg`. What the layer's window could not carry is the
        // one exception: the head holds those messages, so it draws them itself.
        let names = self.rooms_names();
        let mut stream = self.chat_stream(&c, &names);
        // Every dm on the box is many conversations in one column, so each pair gets a rule with
        // its two names on it: the groups are the pairs, and the newest pair is the first.
        let pairs = chat_adapter::pair_heads(&self.rooms, &c, &names);
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
        let mut blocks: Vec<Block> = Vec::new();
        let mut place = Place::default();
        let split = self
            .chat_window_first()
            .filter(|_| !stream.entries.is_empty());
        if let Some(first) = split
            && let Some(above) =
                chat_adapter::stream_before(&self.rooms, &c, first, &names, OPERATOR, self.now_ms)
        {
            // One day is one divider: the window's own first divider is only drawn when the day
            // turns there, and only when the rows above it did not already say it.
            if chat_adapter::same_day_at(&self.rooms, &c, first, self.now_ms) {
                drop_leading(&mut stream.entries, Divider::Today);
            }
            // The unread divider is one row: the rows above the window draw it when the first
            // unread message is among them, and the window's own copy is then not drawn again.
            if above
                .entries
                .iter()
                .any(|e| e.divider == Some(Divider::Unread))
            {
                drop_leading(&mut stream.entries, Divider::Unread);
            }
            add_part(&above, &ctx, &mut blocks, &mut place, &pairs);
        }
        add_part(&stream, &ctx, &mut blocks, &mut place, &pairs);
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
        // The rows above the blocks: the sentence that says older messages may exist. A scroll is
        // a row of the BODY, so where the divider sits is counted from the body's top - and taken
        // before the sends below, which are not part of the channel's history.
        let lead = out.len() - blocks.iter().map(|b| b.lines.len()).sum::<usize>();
        if blocks.is_empty() {
            // The layer's own sentence: an empty channel and one nobody has answered are two
            // facts, and only the layer knows which one this is.
            out.push(pane::faint(format!("  {}", clean_line(&stream.empty))));
        }
        out.extend(send_lines(&self.rooms.sends, &c, rw));
        Body {
            lines: out,
            first: blocks.iter().find_map(|b| b.seq()),
            unread_at: place.unread.map(|at| at + lead),
        }
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
        // With the sidebar drawn beside this view it is the navigation, and the room takes the
        // whole width instead of repeating the list.
        let listless = self.sidebar.drawn;
        let lw = if listless {
            0
        } else {
            self.rooms_left_width(w)
        };
        let sep = if listless { 0 } else { visible_width(SEP) };
        // Recorded for the same reason click arithmetic is: a test reading the column must
        // not re-derive a width the frame already computed.
        self.rooms_left_w = lw;
        let full = w.saturating_sub(lw + sep);
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
            self.rooms.member_col = (lw + sep + rw + visible_width(SEP), mw);
        }

        let left = if listless {
            Vec::new()
        } else {
            self.rooms_left(lw, room)
        };
        let body = self.rooms_body(rw);
        // The body has asked the chat layer by now: a layer that could not answer is a fallback
        // drawn here, and this is the frame that owes the reason - once, not every frame.
        self.chat_note();

        // Stay on the tail unless the reader scrolled back from it.
        let max = body.lines.len().saturating_sub(chat_room);
        // A page of older messages landing above keeps the lines the reader was on.
        let key = self.rooms.selected().map(|c| RoomsView::key(&c));
        let opener = key.as_deref() != self.rooms.drawn_key.as_deref();
        if let (Some((k, seq, len)), Some(key), Some(first)) =
            (self.rooms.prev_first.clone(), key.clone(), body.first)
            && k == key
            && first < seq
            && self.pane_scroll < self.rooms.prev_max
        {
            self.pane_scroll += body.lines.len().saturating_sub(len);
        }
        self.rooms.prev_first = key
            .clone()
            .zip(body.first)
            .map(|(k, f)| (k, f, body.lines.len()));
        if self.pane_scroll >= self.rooms.prev_max {
            self.pane_scroll = max;
        }
        // A channel that has just been opened starts where the reader stopped reading: the news is
        // on the screen, and the history over the divider is one scroll up.
        if opener && let Some(at) = body.unread_at {
            self.pane_scroll = at.min(max);
        }
        self.pane_scroll = self.pane_scroll.min(max);
        self.rooms.prev_max = max;
        self.rooms.drawn_key = key;
        self.rooms.at_top = self.pane_scroll == 0;
        self.rooms_page_back();
        self.pane_len = body.lines.len();
        self.pane_room = chat_room;

        let mut shown: Vec<Line> = body
            .lines
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
                if !listless {
                    out.push(Span::role(SEP, Role::Faint));
                }
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

/// What the drawing has placed so far: the row the unread divider is on (a channel opens there)
/// once a message is already drawn above it, and the pair whose rows are being drawn.
#[derive(Default)]
struct Place {
    unread: Option<usize>,
    messages: bool,
    pair: Option<String>,
}

/// The chat column as one frame draws it, and the two facts the scroll arithmetic needs: the first
/// message on it, and the row the unread divider is drawn on (a channel opens there).
struct Body {
    lines: Vec<Line>,
    first: Option<u64>,
    unread_at: Option<usize>,
}

impl Body {
    /// A body that is one sentence rather than a channel: no channel is selected, or one is a
    /// session's card.
    fn sentence(lines: Vec<Line>) -> Body {
        Body {
            lines,
            first: None,
            unread_at: None,
        }
    }
}

/// Draw one part of the body - the rows above the layer's window, or the window itself - and note
/// where its unread divider landed, so the pane can open the channel there. `pairs` is the every-dm
/// view's pair per message: a rule with the two names on it opens each pair's rows.
fn add_part(
    stream: &Stream,
    ctx: &ChatCtx<'_>,
    blocks: &mut Vec<Block>,
    place: &mut Place,
    pairs: &BTreeMap<u64, String>,
) {
    let mut at: usize = blocks.iter().map(|b| b.lines.len()).sum();
    for (entry, block) in stream.entries.iter().zip(chat_blocks(stream, ctx)) {
        // A new pair opens a group. The rule takes no id, so it is a divider to the cursor and
        // to the click arithmetic, which is what a row between two conversations is.
        if let Some(head) = entry
            .id
            .as_deref()
            .and_then(|id| id.parse::<u64>().ok())
            .and_then(|seq| pairs.get(&seq))
            && place.pair.as_ref() != Some(head)
        {
            place.pair = Some(head.clone());
            let rule = Block {
                id: String::new(),
                lines: vec![crate::ui::chat::rule_line(head, ctx.width, Role::Strong)],
            };
            at += rule.lines.len();
            blocks.push(rule);
        }
        // A divider over the channel's FIRST message is *nothing read yet*, and that is not a place
        // to open at: the reader who has read nothing wants the news, not the start of the log.
        if place.unread.is_none() && entry.divider == Some(Divider::Unread) && place.messages {
            place.unread = Some(at);
        }
        place.messages |= !block.id.is_empty();
        at += block.lines.len();
        blocks.push(block);
    }
}

/// Take a divider off the front of a part: the rows above it already drew that day, or that unread
/// mark, and two rules across a window's edge would be one fact drawn twice.
fn drop_leading(entries: &mut Vec<StreamEntry>, kind: Divider) {
    if entries.first().is_some_and(|e| e.divider == Some(kind)) {
        entries.remove(0);
    }
}
