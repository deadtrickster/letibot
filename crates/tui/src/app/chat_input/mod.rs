//! The composer while `/rooms` is focused: `@` completion and the room's `/` verbs, the history
//! and drafts of an input, the reply target, and what a submitted line becomes once it may carry
//! a picture.
//!
//! Everything here is keyed by the input's `key` - `RoomsView::key` for a room, `View.input.key`
//! for a plugin's chat pane - so a layer's input gets the same history, drafts and cycle without
//! this module knowing what a channel is. The table that routes keys lives in `keys/` and the
//! composer's dispatch in `composer.rs`, so every entry point here answers `false` for *not
//! ours*: the dispatch calls it and the key falls through when it declines.

#![allow(dead_code)] // The seam is P0's, its caller is the key table's, and its proof is `rooms_compose`.

pub(crate) mod complete;
pub(crate) mod drafts;
pub(crate) mod history;
pub(crate) mod reply;

use letibot_plugin::complete::Completion;
use rano::render::Line;

use self::complete::Complete;
use crate::app::rooms::{Chan, Msg, RoomsView};
use crate::app::*;

/// The key the composer is bound to when no channel is selected: a room view with nothing to type
/// to still has one input, and a history entry is better kept than lost.
pub(crate) const NO_CHANNEL: &str = "rooms";

/// A picture that is in the line and goes with it: the words the composer shows, the blob the say
/// names, and the name the markdown reference carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Chip {
    pub text: String,
    /// `None` until P4's upload answers with an id. A chip whose bytes are not a blob yet stays in
    /// the line and is named by no `say`, which is what an upload that has not happened looks like.
    pub blob: Option<String>,
    pub name: String,
}

/// The room composer's own state: one per head, kept across channels by its keys.
#[derive(Debug, Default)]
pub(crate) struct Compose {
    /// The input key the composer is bound to, when something bound it. `None` reads the key off
    /// the selected channel, so a switch nobody announced still walks the right history.
    pub bound: Option<String>,
    pub history: history::History,
    pub drafts: drafts::Drafts,
    /// The message the next line answers.
    pub reply: Option<u64>,
    /// The completion a Tab opened, and which candidate is in the line.
    pub open: Option<complete::Open>,
    /// The pictures a paste put in the line, in the order they were pasted.
    pub chips: Vec<Chip>,
}

impl App {
    /// The input key the composer is bound to: the pane's when one bound it, and otherwise the
    /// selected channel's.
    pub(crate) fn rooms_input_key(&self) -> String {
        if let Some(k) = &self.compose.bound {
            return k.clone();
        }
        match self.rooms.selected() {
            Some(c) => RoomsView::key(&c),
            None => NO_CHANNEL.to_string(),
        }
    }

    /// Bind the composer to another input key: the draft of the one being left is kept and the one
    /// arriving is restored. A channel switch is one call of this, and nothing here knows what a
    /// `Chan` is - which is how a plugin's input gets the same drafts.
    pub(crate) fn rooms_input_key_set(&mut self, key: &str) {
        let text = self.editor.text().to_string();
        let leaving = self.rooms_input_key();
        self.compose.drafts.keep(&leaving, &text);
        self.compose.history.stop();
        self.compose.open = None;
        let draft = self.compose.drafts.take(key).unwrap_or_default();
        self.set_composer(&draft);
        self.compose.bound = Some(key.to_string());
    }

    /// The selected channel moved: keep the draft of the one being left and restore the one
    /// arriving. `from` is named rather than read off the view, because a poll can move the
    /// selection under a caller that still knows where it came from.
    pub(crate) fn rooms_switched(&mut self, from: Option<&Chan>, to: &Chan) {
        let text = self.editor.text().to_string();
        if let Some(from) = from {
            self.compose.drafts.keep(&RoomsView::key(from), &text);
        }
        self.compose.history.stop();
        self.compose.open = None;
        let key = RoomsView::key(to);
        let draft = self.compose.drafts.take(&key).unwrap_or_default();
        self.set_composer(&draft);
        self.compose.bound = Some(key);
    }

    /// One key of the room's composer, offered before the composer the session already has: Tab
    /// opens or cycles the completion, Esc closes it and then the reply, Up and Down walk this
    /// input's history. `false` is not ours, and the key keeps the meaning it had.
    pub(crate) fn rooms_compose_key(&mut self, k: &Key) -> bool {
        match k {
            Key::Tab => self.rooms_complete(),
            Key::Esc => self.rooms_escape(),
            Key::Up => self.rooms_history(true),
            Key::Down => self.rooms_history(false),
            _ => false,
        }
    }

    /// Tab in a room: open the `@`/`/` completion, or cycle the one that is open. The candidate is
    /// written into the line as it is chosen, so Tab is also how it is accepted. `false` is not
    /// ours: nothing to offer, and the key falls through.
    pub(crate) fn rooms_complete(&mut self) -> bool {
        if self.focused_surface() != Some(Surface::Rooms) {
            return false;
        }
        let text = self.editor.text().to_string();
        // The cycle is live only while the line is exactly what its candidate wrote: a character
        // typed on makes this a fresh question, not the next candidate of the old one.
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
        let answer = self.room_completer().complete(&text, self.editor.cursor());
        if answer.candidates.is_empty() {
            self.compose.open = None;
            return false;
        }
        let insert = answer.candidates[0].insert.clone();
        let line = self.write_candidate(answer.from, answer.to, &insert);
        self.compose.open = Some(complete::Open {
            line,
            answer,
            idx: 0,
            written: insert.len(),
        });
        true
    }

    /// Esc in a room: close the completion Tab opened, then clear a reply target. `false` is not
    /// ours - the card's or the surface's Esc keeps its meaning.
    pub(crate) fn rooms_escape(&mut self) -> bool {
        if self.compose.open.take().is_some() {
            return true;
        }
        self.compose.reply.take().is_some()
    }

    /// Up or down on an input that is empty or already walking this key's lines. `false` is not
    /// ours: Up on a typed line moves through a wrapped one, and that is the editor's.
    pub(crate) fn rooms_history(&mut self, up: bool) -> bool {
        if self.focused_surface() != Some(Surface::Rooms) {
            return false;
        }
        let key = self.rooms_input_key();
        if !self.compose.history.walking(&key) && !self.editor.text().is_empty() {
            return false;
        }
        self.compose.open = None;
        match self.compose.history.step(&key, up) {
            Some(line) => {
                self.set_composer(&line);
                true
            }
            None => false,
        }
    }

    /// A submitted line as it goes out: the reply target as the `>N` prefix `rooms_say` threads,
    /// each chip's `![name](blob:ID)` in the chip's place, and the blob ids the say carries.
    ///
    /// The line is remembered for Up as it went out rather than as it was typed: a recall is a
    /// resend, and a resend that lost its references would post the words and not the picture.
    pub(crate) fn rooms_outgoing(&mut self, text: &str) -> (String, Vec<String>) {
        let mut out = match self.compose.reply {
            Some(seq) if !text.trim_start().starts_with('>') => format!(">{seq} {text}"),
            _ => text.to_string(),
        };
        self.compose.reply = None;
        self.compose.open = None;
        let mut blobs = Vec::new();
        for chip in &self.compose.chips {
            let Some(blob) = &chip.blob else { continue };
            if !out.contains(&chip.text) {
                continue;
            }
            out = out.replace(&chip.text, &format!("![{}](blob:{blob})", chip.name));
            blobs.push(blob.clone());
        }
        self.compose.chips.retain(|c| c.blob.is_none());
        let key = self.rooms_input_key();
        if !out.trim().is_empty() {
            self.compose.history.record(&key, &out);
        }
        (out, blobs)
    }

    /// A chip in the line: the words the composer shows, the blob the send will name, and the
    /// picture's name for the markdown reference. P4's `room_upload` is where the first two come
    /// from; a caller that has no blob yet leaves it `None` and the line keeps the chip's words.
    pub(crate) fn rooms_chip(&mut self, chip: &str, blob: Option<&str>, name: &str) {
        if chip.is_empty() {
            return;
        }
        let lead =
            self.editor.text().is_empty() || self.editor.text().ends_with(char::is_whitespace);
        if lead {
            self.editor.insert(chip);
        } else {
            self.editor.insert(&format!(" {chip}"));
        }
        self.compose.chips.push(Chip {
            text: chip.to_string(),
            blob: blob.map(str::to_string),
            name: name.to_string(),
        });
    }

    /// A pasted or dropped picture. P4's `room_upload` answers the chip's words and uploads the
    /// parts; until that lands it answers nothing, so a paste here adds no chip and the line goes
    /// out unchanged.
    pub(crate) fn rooms_paste_picture(&mut self, png: Vec<u8>, name: &str) -> bool {
        let chip = self.room_upload(png, name);
        if chip.is_empty() {
            return false;
        }
        self.rooms_chip(&chip, None, name);
        true
    }

    /// The answer the open completion drew from, for the row.
    pub(crate) fn rooms_completion(&self) -> Option<&Completion> {
        self.compose.open.as_ref().map(|o| &o.answer)
    }

    /// Which candidate of that answer is in the line.
    pub(crate) fn rooms_completion_at(&self) -> usize {
        self.compose.open.as_ref().map_or(0, |o| o.idx)
    }

    /// The completion row the composer's edge draws, or `None` when nothing is open.
    pub(crate) fn rooms_completion_row(&self, w: usize) -> Option<Line> {
        let answer = self.rooms_completion()?;
        crate::ui::chat_compose::completion_row(answer, self.rooms_completion_at(), w)
    }

    /// The reply strip above the field, while a target is set.
    pub(crate) fn rooms_reply_row(&self, w: usize) -> Option<Line> {
        let quote = self.rooms_reply_quote()?;
        crate::ui::chat_compose::reply_strip(&quote, w)
    }

    /// The room's completion, read off the view: the selected room's roster, the other rooms'
    /// members, this head's own verbs behind the room's, and the bridge's aliases.
    pub(crate) fn room_completer(&self) -> complete::RoomComplete {
        let here = match self.rooms.selected() {
            Some(Chan::Room(id)) => Some(id),
            _ => None,
        };
        let mut people: Vec<complete::Person> = Vec::new();
        for (room, list) in &self.rooms.roster {
            for m in list {
                let handle = m.name.clone();
                people.push(complete::Person {
                    name: self.rooms_display_name(&handle),
                    handle,
                    here: here.as_deref() == Some(room.as_str()),
                    connected: m.connected,
                });
            }
        }
        people.sort_by(|a, b| a.handle.cmp(&b.handle));
        let verbs = self.rooms_verbs();
        let aliases = self.rooms_aliases();
        complete::RoomComplete {
            people,
            verbs,
            aliases,
        }
    }

    /// The bridge's aliases the composer offers: `@cc` hears every folder, and one word per project
    /// in this room list. One list, for the same reason [`App::rooms_verbs`] is one.
    pub(crate) fn rooms_aliases(&self) -> Vec<(String, String)> {
        let mut aliases = vec![("@cc".to_string(), "every folder".to_string())];
        for r in &self.rooms.rows {
            if r.id == "host" || r.id == "dm" || r.label.is_empty() {
                continue;
            }
            aliases.push((
                format!("@cc@{}:*", r.label),
                format!("every session in {}", r.label),
            ));
        }
        aliases
    }

    /// The `/` verbs the room's composer offers: P3's own table, then every head verb it does not
    /// already name. One list, because the native completion and the chat feed a layer ranks must
    /// not drift into two sets of verbs.
    pub(crate) fn rooms_verbs(&self) -> Vec<(String, String)> {
        let mut verbs: Vec<(String, String)> = complete::ROOM_VERBS
            .iter()
            .map(|(name, hint)| ((*name).to_string(), (*hint).to_string()))
            .collect();
        for (name, hint) in self.command_names() {
            if verbs.iter().all(|(known, _)| *known != name) {
                verbs.push((name, hint));
            }
        }
        verbs
    }

    /// The name a handle is read as: the roster's label, the session list's title, and the handle
    /// itself when neither knows it.
    pub(crate) fn rooms_display_name(&self, handle: &str) -> String {
        for m in self.rooms.roster.values().flatten() {
            if m.name == handle && !m.label.is_empty() {
                return m.label.clone();
            }
        }
        if let Some(b) = self.sessions.iter().find(|b| b.session_id == handle) {
            let named = if b.slug.trim().is_empty() {
                b.title.trim()
            } else {
                b.slug.trim()
            };
            if !named.is_empty() {
                return named.to_string();
            }
        }
        handle.to_string()
    }

    /// A loaded message by its seq, from whichever room holds it: the reply strip's quote.
    pub(crate) fn rooms_message(&self, seq: u64) -> Option<&Msg> {
        self.rooms.msgs.values().flatten().find(|m| m.seq == seq)
    }

    /// Write a candidate into the line: the span it names goes, the insert lands at the span's
    /// start and the caret after it, so the next word is typed on from there. Crate-visible
    /// because a LAYER's completion (`complete_`) writes into the same editor as a room's.
    pub(crate) fn write_candidate(&mut self, from: usize, to: usize, insert: &str) -> String {
        let len = self.editor.text().len();
        let end = to.min(len);
        let start = from.min(end);
        self.editor.kill_range(start..end);
        self.editor.insert(insert);
        self.editor.text().to_string()
    }
}
