//! The box's room, as this session speaks in it: the link a `room` tool and a room pane read,
//! and the rows the idle check puts on this session's own board - a reminder per item it holds, and
//! the asks between it and the other sessions.

use super::*;

impl Harness {
    /// This session's connection to the box's room, when it has one: the `room` tool's half, and what a
    /// room pane is drawn from. `None` on a box with no coordinator and on a subagent, which speaks
    /// through its parent, and both are ordinary states rather than failures.
    pub fn room_link(&self) -> Option<Arc<crate::room::link::SessionRoom>> {
        self.room.clone()
    }

    /// Whether this session speaks in the box's room - the daemon's own question, and the one
    /// the room pass's clock asks before it makes itself a reason to wake.
    pub fn has_room(&self) -> bool {
        self.room.is_some()
    }

    /// Whether this session may speak in the room but has no link yet - the coordinator was not up
    /// when it opened. The room pass keeps trying while this holds.
    pub fn wants_room(&self) -> bool {
        self.room.is_none() && self.room_connector.is_some()
    }

    /// Connect now if the coordinator's socket exists. `true` when this call made the link.
    pub fn relink_room(&mut self) -> bool {
        if !self.wants_room() || !crate::room::socket_exists() {
            return false;
        }
        let Some(connect) = self.room_connector.as_ref() else {
            return false;
        };
        let Ok(room) = connect() else {
            return false;
        };
        self.room_late.set(room.clone());
        if self.prompt_in_flight.load(Ordering::SeqCst) {
            room.turn_started();
        }
        self.room = Some(room);
        true
    }

    /// The room's reminders and its asks, onto this session's own board: one row per item it holds,
    /// one per question it owes an answer to, and the settles of its own asks applied to the rows
    /// that were waiting. Carried by the idle check, so a holder is reminded and an asker is
    /// released without a model call. A subagent returns 0 first, because the room's rule is
    /// top-level sessions only. A room that does not answer clears the rows.
    pub fn fill_room_board(&mut self) -> usize {
        // The room's rule, said here as well as at the door. See this function's own doc.
        if self.cfg.parent_session_id.is_some() {
            return 0;
        }
        self.relink_room();
        let Some(room) = self.room.clone() else {
            return 0;
        };
        let me = self.hub.session_id();
        let rows = match room.reminders() {
            Ok(all) => crate::room::remind::rows(&me, &all),
            Err(_) => Vec::new(),
        };
        let mut n = self.todos.set_room_rows(rows);
        n += self.apply_asks(&room, &me);
        self.flush_todos_or_say("the room's reminders and asks");
        n
    }

    /// The ask half of the room pass: what this session owes, what settled, what to take back, and
    /// the operator's card for a peer that has been gone too long.
    ///
    /// One read answers all four, and it is the read that makes the pass idempotent: the room holds
    /// the ask, so a daemon that restarted applies what settled while it was away on its first pass.
    fn apply_asks(&mut self, room: &Arc<crate::room::link::SessionRoom>, me: &str) -> usize {
        let Ok((by_me, of_me)) = room.asks() else {
            return 0;
        };
        let mut n = 0;
        // 1. Settles, onto every row that was waiting on one - the row keeps the words it got.
        let mut rang = false;
        for (ask, settle) in crate::room::asks::settles(&by_me) {
            // Read BEFORE the settle: the note names the rows the settle is about to release.
            let rows = self.todos.rows_with_ask(ask);
            if self.todos.settle_ask(ask, &settle) == 0 {
                continue;
            }
            rang = true;
            let ask = by_me.iter().find(|a| a.seq == ask);
            for row in rows {
                let Some(ask) = ask else { continue };
                self.hub.warn(
                    crate::room::asks::settle_class(&settle),
                    crate::room::asks::settle_line(&row, ask, &settle),
                );
            }
        }
        // The bell, once per pass: a settle is news the model may act on now, and the note alone
        // would wait for the next turn.
        if rang {
            self.session_registry.bell().ring_wake(me);
        }
        // 2. What this session owes: one row per open ask of it, retired when it is answered.
        n += self
            .todos
            .set_ask_rows(crate::room::asks::owed_rows(me, &of_me));
        // 3. What to take back: an ask of mine no open row carries any more. The row was done,
        //    cancelled, detached or re-worded, and the target must stop being nagged by it.
        for ask in crate::room::asks::withdrawals(&by_me, &self.todos.asks_on_the_board()) {
            if let Err(why) = room.withdraw(ask) {
                // Said once and not retried here: the next pass re-reads `by_me` and asks again,
                // so a room that was briefly away loses nothing but a window.
                eprintln!("harnessd: `{me}` could not withdraw ask #{ask}: {why}");
            }
        }
        // 4. The operator's card: raised while the other end has been gone past the grace, and
        //    withdrawn the moment it is back - the same words both ways, because they are its key.
        for (ask, question, on) in crate::room::asks::escalations(&by_me, &of_me, me) {
            n += self.todos.escalate_ask(ask, &question, on);
        }
        n
    }
}
