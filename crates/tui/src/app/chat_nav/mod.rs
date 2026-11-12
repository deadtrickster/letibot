//! Moving through the chat rather than through the channels: the message cursor, the jump to the
//! tail or to the unread divider, the `Ctrl+K` switcher and `/search`.
//!
//! Everything here walks the chat's STOPS - one stop is one entry of a stream, never one screen
//! row - so a pane whose entries a plugin answers (`Block::Stream`, H0) gets the same cursor, the
//! same jumps and the same search for free. In `/rooms` the entries are the channel's messages,
//! addressed by their `seq`; in a plugin pane they are `StreamEntry`s addressed by their `id`.

#![allow(dead_code)] // A few readers are P2's: the pane draws the cursor, the footer and the needle.

mod cursor;
mod jump;
mod search;
mod switcher;

pub(crate) use search::Search;
pub(crate) use switcher::{Row, Switcher, shown};

use crate::app::rooms::RoomsView;
use crate::app::*;

/// Where a jump goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Jump {
    /// The newest message: what `End` and `G` do.
    Tail,
    /// The divider before the first message the reader had not seen.
    Unread,
}

/// What P4 keeps between two keys: where the cursor is, what is being searched for, what the
/// switcher has typed, and how far the reader had read at the tail.
#[derive(Debug, Default)]
pub(crate) struct Nav {
    /// The entry under the cursor in the rooms view - a message's `seq`. `None` is the composer's.
    pub(crate) cursor: Option<u64>,
    /// The channel that cursor belongs to. A channel switch drops the cursor rather than moving it
    /// to whatever message took that `seq` in another room.
    pub(crate) chan: Option<String>,
    /// The channel whose `opened_seen` mark was taken last, so a switch re-takes it once.
    pub(crate) opened: Option<String>,
    /// The open search, while `/search` has one.
    pub(crate) search: Option<Search>,
    /// `Ctrl+K`'s list, while it is open.
    pub(crate) switcher: Option<Switcher>,
    /// The newest `seq` this head had drawn at the tail, per channel key: a message after it
    /// arrived while the reader was scrolled back, and that is the `N new` count.
    pub(crate) tail: std::collections::BTreeMap<String, u64>,
    /// The thread `t` opened, as its root's `seq`.
    pub(crate) thread: Option<u64>,
}

impl Nav {
    /// The cursor belongs to the channel that is selected now, or to nothing.
    fn anchored(&self, chan: &str) -> Option<u64> {
        (self.chan.as_deref() == Some(chan))
            .then_some(self.cursor)
            .flatten()
    }
}

impl App {
    /// The `seq` under the message cursor, `None` while the composer has the keys.
    pub(crate) fn rooms_cursor(&self) -> Option<u64> {
        let c = self.rooms.selected()?;
        self.nav.anchored(&RoomsView::key(&c))
    }

    /// One message up or down. `Continue` is *not ours*: the key is the composer's as before, and
    /// with no cursor up the arrows are the channel switch they have always been.
    pub(crate) fn rooms_cursor_move(&mut self, up: bool) -> std::ops::ControlFlow<Option<Action>> {
        cursor::r#move(self, up)
    }

    /// `Tab`: the cursor onto the newest message, which is where a reader joins a chat. Answers
    /// whether there was a message to put it on.
    pub(crate) fn rooms_cursor_newest(&mut self) -> bool {
        cursor::newest(self)
    }

    pub(crate) fn rooms_jump(&mut self, to: Jump) -> std::ops::ControlFlow<Option<Action>> {
        jump::to(self, to)
    }

    /// `Ctrl+K`: fuzzy over the channel labels and DM peers, unread first.
    pub(crate) fn rooms_switcher(&mut self) -> std::ops::ControlFlow<Option<Action>> {
        switcher::open(self)
    }

    /// The keys of an open switcher, before the surface's own: its query is typed there, and a
    /// letter that reached the composer would be sent as a message instead. `Continue` while no
    /// switcher is open, which is every key of an ordinary frame.
    pub(crate) fn key_room_switcher(&mut self, k: &Key) -> std::ops::ControlFlow<Option<Action>> {
        switcher::key(self, k)
    }

    /// `/search TEXT`: a substring over the loaded messages of this channel.
    pub(crate) fn rooms_search(&mut self, needle: &str) {
        search::open(self, needle);
    }

    /// The needle the chat is drawn against, when a search is open.
    pub(crate) fn rooms_search_needle(&self) -> Option<&str> {
        self.nav.search.as_ref().map(|s| s.needle.as_str())
    }

    /// The room answered a `/search` with rows out of its own log: their hits join the ring's.
    pub(crate) fn rooms_search_merged(&mut self) {
        search::merged(self);
    }

    /// `y`: the message under the cursor onto the operator's clipboard (OSC 52).
    pub(crate) fn rooms_copy(&mut self) -> std::ops::ControlFlow<Option<Action>> {
        cursor::copy(self)
    }

    /// `t`: the thread of the message under the cursor, root and replies.
    pub(crate) fn rooms_thread(&mut self) -> std::ops::ControlFlow<Option<Action>> {
        cursor::thread(self)
    }

    /// `n` and `N`: the next or the previous hit of the open search.
    pub(crate) fn rooms_search_step(
        &mut self,
        forward: bool,
    ) -> std::ops::ControlFlow<Option<Action>> {
        search::step(self, forward)
    }

    /// Enter on an empty composer in `/rooms`: the message under the cursor opens what it carries,
    /// its picture or its first link. `Continue` with no cursor, so Enter is the failed line's
    /// resend and then the composer's, exactly as before.
    pub(crate) fn rooms_enter(&mut self) -> std::ops::ControlFlow<Option<Action>> {
        cursor::enter(self)
    }

    /// The messages that arrived while the reader was scrolled back from the tail - the `N new`
    /// count the footer names. `None` at the tail, where the chat follows them itself, and `None`
    /// when nothing has landed since the reader left it.
    pub(crate) fn rooms_new_count(&self) -> Option<usize> {
        // Scrolled back is the last draw's own arithmetic: the body did not fit the window and the
        // reader is not at its end. `rooms_tail_note` keeps the baseline while they are at it.
        if self.pane_scroll >= self.rooms.prev_max {
            return None;
        }
        let c = self.rooms.selected()?;
        let from = self.nav.tail.get(&RoomsView::key(&c)).copied().unwrap_or(0);
        let n = self
            .rooms
            .messages(&c)
            .iter()
            .filter(|m| m.seq > from)
            .count();
        (n > 0).then_some(n)
    }

    /// The footer's sentence for [`App::rooms_new_count`], as the design words it.
    pub(crate) fn rooms_new_footer(&self) -> Option<String> {
        self.rooms_new_count()
            .map(|n| format!("{n} new - End jumps"))
    }

    /// The reader had the tail when this was called: the messages just merged are ones they have
    /// been shown, so the `N new` count starts after them. Called where a poll's rows land.
    pub(crate) fn rooms_tail_note(&mut self) {
        if self.pane_scroll < self.rooms.prev_max {
            return;
        }
        if let Some(c) = self.rooms.selected()
            && let Some(top) = self.rooms.messages(&c).last().map(|m| m.seq)
        {
            self.nav.tail.insert(RoomsView::key(&c), top);
        }
    }

    /// The selected channel is the one being read from now on, so its seen mark at this moment is
    /// where the unread divider goes. Called before what the view shows is marked seen, or the
    /// mark it saved would be the mark it was supposed to sit behind.
    pub(crate) fn rooms_opened_note(&mut self) {
        let Some(c) = self.rooms.selected() else {
            return;
        };
        let key = RoomsView::key(&c);
        if self.nav.opened.as_deref() == Some(&key) {
            return;
        }
        let seen = self.rooms.seen.get(&key).copied().unwrap_or(0);
        self.rooms.opened_seen.insert(key.clone(), seen);
        self.nav.opened = Some(key);
    }

    /// Put the message cursor on `seq` of the selected channel, and remember which channel that
    /// was. A channel switch later drops it rather than pointing it at another room's message.
    pub(crate) fn rooms_cursor_set(&mut self, seq: Option<u64>) {
        self.nav.cursor = seq;
        self.nav.chan = seq.and(self.rooms.selected().map(|c| RoomsView::key(&c)));
    }

    /// The cursor goes back to the composer: `true` when it took the Esc, so the key does not pop
    /// the view behind it.
    pub(crate) fn rooms_cursor_drop(&mut self) -> bool {
        if self.nav.cursor.is_none() {
            return false;
        }
        self.rooms_cursor_set(None);
        true
    }

    /// The chat jumps to its tail: the newest message is on screen again, and nothing is waiting
    /// behind the reader.
    pub(crate) fn rooms_to_tail(&mut self) {
        self.pane_scroll = usize::MAX;
        self.rooms.prev_max = 0;
        if let Some(c) = self.rooms.selected()
            && let Some(top) = self.rooms.messages(&c).last().map(|m| m.seq)
        {
            self.nav.tail.insert(RoomsView::key(&c), top);
        }
    }
}
