//! The find bar: one line that searches the open conversation, or the project's conversations.
//!
//! The conversation's half is answered by the head itself, over the rows it holds, so a hit is
//! tinted as the query is typed; the rows it does not hold are asked of the daemon with the same
//! syntax (`letibot_transcript::search`), and so is every conversation of the project.

use super::*;
use crate::app::pointer::restyle;
use letibot_sessionlog::protocol::SearchHit;
use letibot_transcript::TranscriptItem;
use letibot_transcript::search::Query;
use std::ops::ControlFlow;

/// Which place the bar is looking in. One bar rather than two screens: the same query syntax
/// answers both, and the wider one is the same question asked of more conversations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    /// The open conversation.
    Conversation,
    /// The project's conversations, archived included.
    Sessions,
}

/// Where a hit's row sits relative to the window this head holds. The ordering of the list is
/// reading order, so `n` walks down the conversation however the rows were found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum At {
    /// Above the window, by session ordinal.
    Before(usize),
    /// The index of the row in [`App::items`].
    Held(usize),
    /// Below the window, by session ordinal.
    After(usize),
}

/// One row of the open conversation a query matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FindHit {
    pub(crate) item_id: String,
    pub(crate) at: At,
    /// The row's ordinal in the session, when it has one: what the daemon answers by.
    pub(crate) row: usize,
    pub(crate) excerpt: String,
}

impl FindHit {
    fn order(&self) -> (u8, usize) {
        match self.at {
            At::Before(row) => (0, row),
            At::Held(i) => (1, i),
            At::After(row) => (2, row),
        }
    }
}

/// The bar: what is typed, what it matched, and which hit the reader is on.
#[derive(Debug, Clone, Default)]
pub(crate) struct Find {
    pub(crate) query: String,
    pub(crate) q: Query,
    pub(crate) scope: Option<Scope>,
    /// Take the sessions that are not the operator's own, in the sessions scope.
    pub(crate) all: bool,
    /// The open conversation's hits, in reading order.
    pub(crate) hits: Vec<FindHit>,
    /// Which hit the reader is on. `None` before anything matched.
    pub(crate) sel: Option<usize>,
    /// The project's conversations, from the daemon: what the sessions scope draws.
    pub(crate) results: Vec<SearchHit>,
    pub(crate) more: bool,
    /// The query the daemon was last asked, so one query is one request.
    pub(crate) asked: Option<String>,
    /// What the last answer said - a daemon with no store, or a limit it stopped on.
    pub(crate) said: Option<String>,
    /// The row of `results` the cursor is on.
    pub(crate) sel_result: usize,
    /// The session a hit asked the head to open, and the row to put on the screen once the
    /// switch lands. Held here because the rows of another conversation are not the head's yet.
    pub(crate) goto: Option<(String, String, usize)>,
}

impl Find {
    /// Is the bar looking at the project's conversations rather than the open one? The drawing,
    /// the keys and the counter all ask this one question.
    pub(crate) fn sessions(&self) -> bool {
        self.scope == Some(Scope::Sessions)
    }

    /// Is this row one the query matched, by id? What the tint asks per drawn row.
    pub(crate) fn matched(&self, item_id: &str) -> bool {
        self.hits.iter().any(|h| h.item_id == item_id)
    }

    /// The hit the reader is on.
    pub(crate) fn current(&self) -> Option<&FindHit> {
        self.sel.and_then(|i| self.hits.get(i))
    }
}

impl App {
    /// `ctrl-f`: the bar, on the conversation it was typed over. A second press while it is open
    /// moves it to the project's conversations and back, because that is the same question.
    pub(crate) fn find_key_chord(&mut self) -> Option<Action> {
        match self.find.as_mut() {
            Some(f) => {
                f.scope = match f.sessions() {
                    true => Some(Scope::Conversation),
                    false => Some(Scope::Sessions),
                }
            }
            None => {
                let mut f = Find {
                    scope: Some(Scope::Conversation),
                    ..Find::default()
                };
                // The composer's line is what the reader meant to look for, when they typed one:
                // a search for what is on the screen already in front of them is the common case.
                let held = self.editor.text().trim().to_string();
                if !held.is_empty() && !held.starts_with('/') {
                    f.query = held;
                    f.q = Query::parse(&f.query);
                }
                self.find = Some(f);
            }
        }
        // The wider scope is a pane in the conversation's rectangle, through the one door a
        // surface is opened by; the narrower one gives the rectangle back.
        if self.find.as_ref().is_some_and(|f| f.sessions()) {
            self.open_surface(Surface::Search);
        } else {
            while self.top_surface() == Some(Surface::Search) {
                self.pop_surface();
            }
        }
        // The two scopes are two questions, and an answer to the one the bar just left is not an
        // answer to this one: the daemon is asked again.
        if let Some(f) = self.find.as_mut() {
            f.asked = None;
        }
        self.find_recompute();
        None
    }

    /// Esc, or `ctrl-f` on a bar with nothing to search: the bar goes and the view stays where
    /// the reader left it - the whole point of a find that keeps its place.
    pub(crate) fn find_close(&mut self) {
        self.find = None;
        while self.top_surface() == Some(Surface::Search) {
            self.pop_surface();
        }
    }

    /// The query changed: the hits of the rows this head holds are found here, at once, and the
    /// rows it does not hold are asked of the daemon.
    pub(crate) fn find_recompute(&mut self) {
        let Some(q) = self.find.as_ref().map(|f| Query::parse(&f.query)) else {
            return;
        };
        let mut hits: Vec<FindHit> = Vec::new();
        if q.complaint().is_none() && !q.is_empty() {
            // The ordinal of `items[0]`: the base the snapshot named, less the lead rows a resume
            // republished above it. Computed once, so the walk borrows `items` and not `self`.
            let base = self.items_base.saturating_sub(self.items_lead);
            for (i, it) in self.items.iter().enumerate() {
                let Some(item) = it.item.as_ref() else {
                    continue;
                };
                let Some(m) = letibot_transcript::search::row_match(&q, item) else {
                    continue;
                };
                hits.push(FindHit {
                    item_id: it.item_id.clone(),
                    at: At::Held(i),
                    row: base + i,
                    excerpt: m.excerpt,
                });
            }
        }
        let Some(f) = self.find.as_mut() else { return };
        f.q = q;
        // What the daemon already answered about the rows outside the window survives a keystroke
        // that did not change its question; anything else is dropped, because it answered a
        // query nobody is looking at any more.
        hits.extend(
            f.hits
                .iter()
                .filter(|h| !matches!(h.at, At::Held(_)))
                .cloned(),
        );
        hits.sort_by_key(FindHit::order);
        f.hits = hits;
        f.sel = match f.hits.is_empty() {
            true => None,
            false => Some(f.sel.unwrap_or(0).min(f.hits.len() - 1)),
        };
        let asking = f.query.clone();
        self.find_ask(asking);
        // The reader is on a hit and the conversation is what they are looking at: it is the
        // first one that is put on the screen, so the bar and the view agree from the first letter.
        let Some(f) = self.find.as_ref() else { return };
        if !f.sessions() && !self.following() {
            return;
        }
        if let Some(hit) = self.find.as_ref().and_then(|f| f.current()).cloned() {
            self.find_goto(&hit, false);
        }
    }

    /// The ordinal of `items[i]` in the session, as the daemon numbers a row: the base the
    /// snapshot named, plus how far past the lead this row is.
    pub(crate) fn ordinal_of(&self, i: usize) -> usize {
        self.items_base
            .saturating_sub(self.items_lead)
            .saturating_add(i)
    }

    /// Ask the daemon, when it has something to add: the rows above or below the window this
    /// head holds. A session the head holds whole is answered by the head, and no request is
    /// spent on it.
    fn find_ask(&mut self, query: String) {
        let Some(f) = self.find.as_mut() else { return };
        if f.q.is_empty() || f.q.complaint().is_some() {
            f.asked = None;
            f.results.clear();
            f.more = false;
            f.said = None;
            return;
        }
        if f.asked.as_deref() == Some(query.as_str()) {
            return;
        }
        let (session, all) = match f.sessions() {
            // A project's search is a request every time the query changes: those rows are the
            // daemon's alone.
            true => (None, f.all),
            false => {
                if self.items_base == 0 && self.items_below == 0 {
                    f.asked = Some(query);
                    return;
                }
                (Some(self.session_id.clone()), false)
            }
        };
        f.asked = Some(query.clone());
        f.said = None;
        self.queued.push(Action::Search {
            query,
            session,
            all,
            from: 0,
            count: 40,
        });
    }

    /// The daemon's answer: the project's conversations for the sessions scope, and the rows
    /// outside this head's window for the conversation's.
    pub(crate) fn on_searched(
        &mut self,
        query: String,
        session: Option<String>,
        hits: Vec<SearchHit>,
        more: bool,
        why: Option<String>,
    ) {
        // The window's own numbering, read before the bar is borrowed mutably: a hit is placed by
        // where it sits against the rows this head holds.
        let base = self.items_base;
        let end = self.ordinal_of(self.items.len());
        let held: std::collections::HashSet<String> =
            self.items.iter().map(|i| i.item_id.clone()).collect();
        let Some(f) = self.find.as_mut() else { return };
        // An answer to a query nobody is typing any more is dropped: the bar has moved on.
        if f.asked.as_deref() != Some(query.as_str()) {
            return;
        }
        f.said = why;
        f.more = more;
        match session {
            None => {
                f.results = hits;
                f.sel_result = f.sel_result.min(f.results.len().saturating_sub(1));
            }
            Some(_) => {
                for h in hits {
                    if held.contains(&h.item_id) {
                        continue;
                    }
                    let at = match h.row < base {
                        true => At::Before(h.row),
                        false if h.row >= end => At::After(h.row),
                        // A row between the window's base and its end that the head does not hold
                        // is one of the lead rows a resume republished: it is above the base's
                        // numbering, and it is not the daemon's to place.
                        false => continue,
                    };
                    f.hits.push(FindHit {
                        item_id: h.item_id,
                        at,
                        row: h.row,
                        excerpt: h.excerpt,
                    });
                }
                f.hits.sort_by_key(FindHit::order);
                f.sel = match f.hits.is_empty() {
                    true => None,
                    false => Some(f.sel.unwrap_or(0).min(f.hits.len() - 1)),
                };
            }
        }
    }

    /// Move to the next hit (or the one before), wrapping, and put it on the screen.
    pub(crate) fn find_next(&mut self, forward: bool) {
        let Some(f) = self.find.as_mut() else { return };
        if f.hits.is_empty() {
            return;
        }
        let n = f.hits.len();
        let at = match f.sel {
            Some(i) if forward => (i + 1) % n,
            Some(i) => (i + n - 1) % n,
            None => 0,
        };
        f.sel = Some(at);
        let hit = f.hits[at].clone();
        self.find_goto(&hit, true);
    }

    /// Put one hit on the screen: scroll to its row, open the fold that hid it, and let the frame
    /// draw the tint. A row this head does not hold yet is paged in first, and `goto` keeps the
    /// place so it is revealed when the page lands.
    pub(crate) fn find_goto(&mut self, hit: &FindHit, _jump: bool) {
        if let At::Held(i) = hit.at {
            let id = self.items.get(i).map(|it| it.item_id.clone());
            if let Some(id) = id {
                self.find_reveal(&id);
                return;
            }
        }
        // A row outside the window: ask for the page it is on, and remember to put it on the
        // screen when it arrives.
        self.find_page_to(hit.row);
        if let Some(f) = self.find.as_mut() {
            f.goto = Some((self.session_id.clone(), hit.item_id.clone(), hit.row));
        }
    }

    /// Reveal a row by id, opening the fold that hid it: a row drawn nowhere is a hit the reader
    /// cannot read, so the run or the window it belongs to is opened.
    ///
    /// The anchor is set here rather than through [`App::reveal_row`], whose two guards are about a
    /// reader's own scroll: a hit is put on the screen from the first letter typed, before any
    /// frame has said where the window can hold, and a search is exactly the act that may park a
    /// view nobody has scrolled.
    pub(crate) fn find_reveal(&mut self, id: &str) {
        if let Some(ordinal) = self.items.iter().position(|it| it.item_id == id) {
            self.fill_to_row(ordinal);
            self.anchor = Some(Held {
                item_id: id.to_string(),
                ordinal,
                into: 0,
            });
            self.invalidate_history();
        }
        if self.span_for(id).is_none()
            && let Some(it) = self.items.iter().find(|it| it.item_id == id)
            && matches!(
                it.item,
                Some(TranscriptItem::ToolResult { .. } | TranscriptItem::Reasoning { .. })
            )
        {
            self.open_payload_window(id.to_string());
        }
    }

    /// Ask for the page a row is on, so a hit the head does not hold can be put on the screen.
    fn find_page_to(&mut self, row: usize) {
        let from = row.saturating_sub(4);
        self.queued.push(Action::FetchRows {
            session: self.session_id.clone(),
            transcript: None,
            from,
            count: 12,
        });
    }

    /// A page of rows landed: a hit waiting for one of them is revealed now, which is what makes
    /// a jump past the loaded page land where the bar said.
    pub(crate) fn find_rows_arrived(&mut self) {
        let Some((session, id, _row)) = self.find.as_ref().and_then(|f| f.goto.clone()) else {
            return;
        };
        if session != self.session_id {
            // The switch has not landed yet: the rows of another session are not these.
            return;
        }
        if !self.items.iter().any(|it| it.item_id == id) {
            return;
        }
        if let Some(f) = self.find.as_mut() {
            f.goto = None;
        }
        self.find_reveal(&id);
        self.invalidate_history();
    }

    /// `tab`: the roles the query wants, rewritten in the query itself so what the bar shows is
    /// what it searches. The cycle is `Wants`'s, so the tokens and the cycle cannot disagree.
    pub(crate) fn find_cycle_roles(&mut self) {
        let Some(f) = self.find.as_mut() else { return };
        let next = f.q.wants().cycled();
        let words = strip_tokens(&f.query);
        f.query = match next.everything() {
            true => words,
            false => format!("{} {words}", next.tokens()),
        }
        .trim()
        .to_string();
        self.find_recompute();
    }

    /// `alt-o`: take the sessions that are not the operator's own. A project's search is theirs
    /// by default, because a subagent's rows are the work, not the conversation.
    pub(crate) fn find_toggle_all(&mut self) {
        let Some(f) = self.find.as_mut() else { return };
        f.all = !f.all;
        // The answer in hand was filtered by the old rule, so it is asked again.
        f.asked = None;
        let query = f.query.clone();
        self.find_ask(query);
    }

    /// A key while the bar is open. The bar owns them - it is a line being typed - so nothing
    /// reaches the composer until it closes.
    pub(crate) fn key_find(&mut self, k: &Key) -> ControlFlow<Option<Action>> {
        if self.find.is_none() {
            return ControlFlow::Continue(());
        }
        match k {
            Key::Esc | Key::CtrlC => {
                self.find_close();
                ControlFlow::Break(None)
            }
            Key::Char(c) => {
                if let Some(f) = self.find.as_mut() {
                    f.query.push(*c);
                }
                self.find_recompute();
                ControlFlow::Break(None)
            }
            Key::Paste(text) => {
                if let Some(f) = self.find.as_mut() {
                    f.query.push_str(text);
                }
                self.find_recompute();
                ControlFlow::Break(None)
            }
            Key::Backspace => {
                if let Some(f) = self.find.as_mut() {
                    f.query.pop();
                }
                self.find_recompute();
                ControlFlow::Break(None)
            }
            Key::Tab => {
                self.find_cycle_roles();
                ControlFlow::Break(None)
            }
            Key::CtrlF => ControlFlow::Break(self.find_key_chord()),
            Key::AltO => {
                self.find_toggle_all();
                ControlFlow::Break(None)
            }
            Key::Enter if self.find.as_ref().is_some_and(|f| f.sessions()) => {
                ControlFlow::Break(self.search_open())
            }
            Key::Enter | Key::CtrlN => {
                self.find_next(true);
                ControlFlow::Break(None)
            }
            Key::SoftEnter | Key::CtrlP => {
                self.find_next(false);
                ControlFlow::Break(None)
            }
            Key::Down => {
                if self.find.as_ref().is_some_and(|f| f.sessions()) {
                    self.search_move(true);
                } else {
                    self.find_next(true);
                }
                ControlFlow::Break(None)
            }
            Key::Up => {
                if self.find.as_ref().is_some_and(|f| f.sessions()) {
                    self.search_move(false);
                } else {
                    self.find_next(false);
                }
                ControlFlow::Break(None)
            }
            _ => ControlFlow::Break(None),
        }
    }

    /// The sessions scope's cursor.
    pub(crate) fn search_move(&mut self, forward: bool) -> Option<Action> {
        let f = self.find.as_mut()?;
        if f.results.is_empty() {
            return None;
        }
        let n = f.results.len();
        f.sel_result = match forward {
            true => (f.sel_result + 1) % n,
            false => (f.sel_result + n - 1) % n,
        };
        self.scroll_into_view(self.search_row_of());
        None
    }

    /// The pane row the cursor is on, off the record of where each row was DRAWN - the rule every
    /// list pane here follows, so a scroll lands where the pane drew the row.
    pub(crate) fn search_row_of(&self) -> usize {
        let at = self
            .find
            .as_ref()
            .map_or(0, |f| f.sel_result.min(f.results.len().saturating_sub(1)));
        self.search_stop_rows.get(at).copied().unwrap_or(0)
    }

    /// Enter in the sessions scope: open the conversation the hit is in, at the row it matched.
    pub(crate) fn search_open(&mut self) -> Option<Action> {
        let f = self.find.as_ref()?;
        let hit = f
            .results
            .get(f.sel_result.min(f.results.len().saturating_sub(1)))?;
        let (session, item, row) = (hit.session.clone(), hit.item_id.clone(), hit.row);
        let same = session == self.session_id;
        if let Some(f) = self.find.as_mut() {
            f.goto = Some((session.clone(), item.clone(), row));
        }
        // The conversation the hit is in, from the row it matched: the bar stays open so the
        // next hit is a key away.
        if same {
            self.find_reveal(&item);
            return None;
        }
        Some(Action::Switch(session))
    }

    /// The bar's own row: what is typed, what it found, and the keys that are live on it.
    pub(crate) fn find_bar(&self, w: usize) -> String {
        use rano::render::{Line, Span};
        use rano::style::Role;
        let Some(f) = self.find.as_ref() else {
            return String::new();
        };
        let mut spans = vec![Span::styled("find", Role::Strong), Span::raw(": ")];
        spans.push(Span::raw(if f.query.is_empty() {
            "…".to_string()
        } else {
            f.query.clone()
        }));
        let count = match f.q.complaint() {
            Some(why) => format!("  bad pattern: {}", crate::ui::render::trim_to(why, 40)),
            None if f.q.is_empty() => "  type to search".to_string(),
            None => match f.sessions() {
                true => format!(
                    "  {} hit(s) in {} session(s){}",
                    f.results.len(),
                    f.results
                        .iter()
                        .map(|h| h.session.as_str())
                        .collect::<std::collections::BTreeSet<_>>()
                        .len(),
                    if f.more { "+" } else { "" }
                ),
                false => match (f.sel, f.hits.len()) {
                    (Some(i), n) => format!("  {}/{n}{}", i + 1, if f.more { "+" } else { "" }),
                    (None, n) => format!("  0/{n}"),
                },
            },
        };
        spans.push(Span::styled(count, Role::Faint));
        if let Some(why) = &f.said {
            spans.push(Span::styled(
                format!("  {}", crate::ui::render::trim_to(why, 50)),
                Role::Warning,
            ));
        }
        let keys = match f.sessions() {
            true => format!(
                "  ↑↓ pick · enter opens · ctrl-f conversation · alt-o all ({}) · tab roles ({}) · esc",
                if f.all { "on" } else { "off" },
                f.q.wants().words()
            ),
            false => format!(
                "  enter next · shift-enter back · tab roles ({}) · ctrl-f all sessions · esc keeps place",
                f.q.wants().words()
            ),
        };
        spans.push(Span::styled(keys, Role::Faint));
        let line = Line::new(spans);
        let body = crate::ui::render::row(&line, self.cfg.palette());
        crate::ui::render::trim_to(&body, w).to_string()
    }

    /// The colours a match is tinted in: reverse video, the same mark a selection uses, because
    /// both mean *these cells and no others*.
    pub(crate) const FIND_ON: &'static str = "\x1b[7m";
    pub(crate) const FIND_OFF: &'static str = "\x1b[27m";

    /// Tint every match on the conversation's rows of one frame. The row's own text is what is
    /// searched, so what is tinted is what the reader sees - a match inside a fold that is not
    /// drawn is a hit the counter knows about and the screen cannot show.
    pub(crate) fn paint_find(&self, frame: &mut [String]) {
        let Some(f) = self.find.as_ref() else { return };
        if f.sessions() || f.q.is_empty() || f.q.complaint().is_some() {
            return;
        }
        let Some(rect) = self
            .hits
            .iter()
            .find(|h| h.target == Target::Conversation)
            .map(|h| h.rect)
        else {
            return;
        };
        for y in rect.y..(rect.y + rect.h).min(frame.len()) {
            let Some(id) = self.item_at(y as u16).map(str::to_string) else {
                continue;
            };
            if !f.matched(&id) {
                continue;
            }
            let cells: Vec<char> = keyhints::cells(&frame[y]);
            let text: String = cells.iter().collect();
            for r in f.q.hits(&text) {
                // The match is a byte range in the row's text and the tint is in CELLS, so the two
                // conversions are here: a column is one character of this string, and a wide
                // character counts as two cells in `restyle` but one here.
                let from = text[..r.start].chars().count();
                let to = from + text[r.start..r.end].chars().count();
                frame[y] = restyle(&frame[y], from, to, Self::FIND_ON, Self::FIND_OFF);
            }
        }
    }

    /// The sessions scope's rows: session title, folder, time, role and the excerpt, one stop
    /// each - the shape the operator asked for, drawn off the daemon's answer.
    pub(crate) fn search_lines(&mut self, w: usize) -> Vec<String> {
        use rano::agent::pane::{self, PaneLines};
        use rano::render::{Line, Span};
        let mut out = PaneLines::new();
        let mut stops: Vec<usize> = Vec::new();
        let (hits, sel, why, more) = match self.find.as_ref() {
            Some(f) => (f.results.clone(), f.sel_result, f.said.clone(), f.more),
            None => (Vec::new(), 0, None, false),
        };
        out.push(pane::title(format!(
            "conversations — {} hit(s){}; enter opens one at the row it matched",
            hits.len(),
            if more { ", more to find" } else { "" }
        )));
        out.blank();
        if let Some(why) = why {
            out.push(pane::faint(format!("    {why}")));
            self.search_stop_rows = stops;
            return crate::ui::render::row_strings(&out.lines, self.cfg.palette());
        }
        if hits.is_empty() {
            out.push(pane::faint(
                "    no row of this project's conversations carries it",
            ));
            self.search_stop_rows = stops;
            return crate::ui::render::row_strings(&out.lines, self.cfg.palette());
        }
        let cursor = sel.min(hits.len().saturating_sub(1));
        for (i, h) in hits.iter().enumerate() {
            let picked = i == cursor;
            let row = out.lines.len();
            out.push(Line::new(vec![
                Span::raw(format!("{} ", pane::mark(picked))),
                Span::raw(format!(
                    "{}  ",
                    crate::ui::render::trim_to(&title_of(h), w.saturating_sub(24).max(12))
                )),
                Span::styled(
                    format!("{}  ", folder_of(h)),
                    rano::render::Style::default(),
                ),
            ]));
            let facts = format!("    {} · {} · {}", stamp(h.at_ms), h.role, h.tool.as_str());
            out.push(pane::faint(facts));
            out.push(pane::faint(format!(
                "    {}",
                crate::ui::render::trim_to(&h.excerpt, w.saturating_sub(6).max(12))
            )));
            stops.push(row);
        }
        self.search_stop_rows = stops;
        crate::ui::render::row_strings(&out.lines, self.cfg.palette())
    }
}

/// A hit's name: the session's title, or its id when nobody named it.
fn title_of(h: &SearchHit) -> String {
    match h.title.trim() {
        "" => h.session.clone(),
        t => t.to_string(),
    }
}

/// A hit's folder: the last component, which is what a person calls the place.
fn folder_of(h: &SearchHit) -> String {
    std::path::Path::new(&h.folder)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| h.folder.clone())
}

/// A hit's time, as a list row has room for: the hour and minute of the row's own clock, UTC.
fn stamp(at_ms: u64) -> String {
    if at_ms == 0 {
        return "—".to_string();
    }
    let secs = at_ms / 1000;
    let (h, m) = ((secs / 3600) % 24, (secs / 60) % 60);
    format!("{h:02}:{m:02}")
}

/// The query with its role tokens taken out: what a role cycle rewrites around.
fn strip_tokens(query: &str) -> String {
    query
        .split_whitespace()
        .filter(|w| {
            let l = w.to_ascii_lowercase();
            !(l.starts_with("me:") || l.starts_with("agent:") || l.starts_with("tool:"))
        })
        .collect::<Vec<_>>()
        .join(" ")
}
