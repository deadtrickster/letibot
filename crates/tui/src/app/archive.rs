//! Putting a session out of sight and taking it back. The row is the DAEMON's - an archive is a
//! store write - so this module only ever asks, and the list it draws is what came back: `del` twice
//! archives, `ctrl-z` brings the last one back, and the archived rows ride the daemon's list as a
//! group of their own. Nothing here deletes anything, and `/forget` says so.

use super::*;

/// How long an armed archive waits for its second press: long enough to read the sentence, short
/// enough that a stray `del` is not an archive minutes later. It is also the only guard for keys
/// that never reach a binding, such as a letter typed into the composer.
const ARM_MS: u64 = 5_000;

/// What the store's row says about an archive made from the list. It is printed beside the session
/// by `letibot --sessions --archived`, so it says which door put it away rather than nothing.
pub(crate) const FROM_THE_LIST: &str = "put away from the session list";

/// Where a drawn row stands: a session of the daemon's list, one it has put away, or neither - a
/// group's line, or another daemon's package. The three are read off the row's id, because an
/// archived row is not in `App::sessions` and so has no index there.
enum Standing {
    Listed(usize),
    Archived,
    Neither,
}

impl App {
    /// `del` on the session list: arm the row under the cursor, or archive it on the second press.
    /// On an ARCHIVED row the key is the other way round and needs no second press: putting a row
    /// back is not the act that asks twice.
    pub(crate) fn archive_selected(&mut self) -> Option<Action> {
        let row = self.picker_row()?;
        match self.standing(&row.id) {
            Standing::Listed(i) => self.archive_row(i),
            Standing::Archived => self.unarchive(&row.id),
            // Another daemon's package: its row is that daemon's store, and this head's archive
            // cannot reach it. Said as the different thing it is, rather than as a group.
            Standing::Neither if self.is_fleet(&row.id) => {
                self.archive_armed = None;
                self.say(
                    "that row is another daemon's package — `del` archives a session of THIS \
                     daemon, and enter says how to attach to that one",
                );
                None
            }
            // A node's line is a fold, not a session: `del` on it says what the key is for rather
            // than silently doing nothing.
            Standing::Neither => {
                self.archive_armed = None;
                self.say(
                    "that line is a group, not a session — `del` twice archives a session's row, \
                     and enter or → folds a group",
                );
                None
            }
        }
    }

    /// Whether a row is another daemon's package - in the fleet this head read, or the daemon's own
    /// record of the one this folder holds.
    fn is_fleet(&self, id: &str) -> bool {
        self.fleet
            .iter()
            .chain(self.fleet_mine.iter())
            .any(|f| f.id == id)
    }

    /// Where a row of the drawn list stands.
    fn standing(&self, id: &str) -> Standing {
        match self.sessions.iter().position(|s| s.session_id == id) {
            Some(i) => Standing::Listed(i),
            None if self.archived.iter().any(|s| s.session_id == id) => Standing::Archived,
            None => Standing::Neither,
        }
    }

    /// The act itself, on a session index - the half a test can drive without a cursor.
    pub(crate) fn archive_row(&mut self, i: usize) -> Option<Action> {
        let id = self.sessions[i].session_id.clone();
        if !self.may_archive(&id) {
            return None;
        }
        // The second press, on the same row, inside the arm's window.
        if let Some((armed, at)) = self.archive_armed.clone()
            && armed == id
            && self.now_ms <= at.saturating_add(ARM_MS)
        {
            self.archive_armed = None;
            return Some(self.archive(&id));
        }
        self.archive_armed = Some((id.clone(), self.now_ms));
        self.say(&self.archive_ask(i, &id));
        None
    }

    /// Whether a row may be archived at all, and the sentence when it may not. The session this head
    /// is standing in is not the one to put away: the daemon closes what it archives, so the head
    /// would be left in a conversation nothing is driving.
    fn may_archive(&mut self, id: &str) -> bool {
        if id != self.session_id {
            return true;
        }
        self.archive_armed = None;
        self.say(&format!(
            "{id} is the session you are in — archiving it would close it under this head, \
             which is left in a conversation nothing is driving. Switch away first (ctrl-s), \
             or just quit. Nothing was archived."
        ));
        false
    }

    /// `/archive [ID]`: the verb, which is asked once because it was typed. A row already archived
    /// is said so rather than archived twice.
    pub(crate) fn archive_named(&mut self, typed: &str) -> Option<Action> {
        let id = match self.named(typed) {
            Ok(id) => id,
            Err(said) => {
                self.say(&said);
                return None;
            }
        };
        match self.standing(&id) {
            Standing::Archived => {
                self.say(&format!(
                    "{id} is archived already — `ctrl-z` or `del` on its row brings it back"
                ));
                None
            }
            Standing::Neither => {
                self.say(&format!("{id} is not a session this list holds"));
                None
            }
            Standing::Listed(i) => {
                let id = self.sessions[i].session_id.clone();
                self.may_archive(&id).then(|| self.archive(&id))
            }
        }
    }

    /// The frame, and the two facts this head keeps to be able to undo it: what it archived last,
    /// and the arm's end. The daemon answers with the list, which is what takes the row off the
    /// tree and puts it in the archived group.
    fn archive(&mut self, id: &str) -> Action {
        self.archived_last = Some(id.to_string());
        self.say(&format!(
            "{id} is archived — off this list and kept in the store; `ctrl-z` brings it back, and \
             enter on its row in the archived group resumes it"
        ));
        Action::Archive {
            session_id: id.to_string(),
            reason: FROM_THE_LIST.to_string(),
        }
    }

    /// `ctrl-z`: the archive this head made last, back. When it made none, the archived row under
    /// the cursor, because that is the one the person is looking at.
    pub(crate) fn unarchive_last(&mut self) -> Option<Action> {
        self.archive_armed = None;
        if let Some(id) = self.archived_last.take() {
            return self.unarchive(&id);
        }
        let row = self.picker_row()?;
        if !matches!(self.standing(&row.id), Standing::Archived) {
            self.say(
                "nothing here was archived by this head — `del` twice archives the row under the \
                 cursor, and the archived rows are under `archived` at the bottom of the list",
            );
            return None;
        }
        let id = row.id.clone();
        self.unarchive(&id)
    }

    /// The other direction, as one act: the frame, and what it will mean when the daemon answers.
    fn unarchive(&mut self, id: &str) -> Option<Action> {
        if self.archived_last.as_deref() == Some(id) {
            self.archived_last = None;
        }
        self.say(&format!(
            "{id} is back on the list — its archive stamp is cleared, its rows never left the \
             store, and `enter` on its row resumes it"
        ));
        Some(Action::Unarchive {
            session_id: id.to_string(),
        })
    }

    /// `/close [ID]`: let go of a session WITHOUT putting it out of sight. The daemon stops driving
    /// it; the row stays in the list, reading as one on disk. Not this head's own session, for the
    /// reason `/archive` refuses it.
    pub(crate) fn close_named(&mut self, typed: &str) -> Option<Action> {
        let id = match self.named(typed) {
            Ok(id) => id,
            Err(said) => {
                self.say(&said);
                return None;
            }
        };
        if id == self.session_id {
            self.say(&format!(
                "{id} is the session you are in — `/close` would leave this head in a \
                 conversation the daemon has let go of. Quit, or `ctrl-s` and pick another. \
                 Nothing was closed."
            ));
            return None;
        }
        match self.standing(&id) {
            Standing::Archived => {
                self.say(&format!(
                    "{id} is archived, and nothing is holding it: there is nothing to close. \
                     `ctrl-z` brings it back into the list."
                ));
                None
            }
            Standing::Neither => {
                self.say(&format!("{id} is not a session this list holds"));
                None
            }
            Standing::Listed(i) if !self.sessions[i].live => {
                self.say(&format!(
                    "{} ({id}) is not live — the daemon already let go of it. `/close` is for a \
                     session it is holding.",
                    self.session_label(&id)
                ));
                None
            }
            Standing::Listed(_) => {
                self.say(&format!(
                    "{id} is closed here — the daemon lets go of it and its rows stay in the \
                     store, so `enter` on its row resumes it. It stays on this list."
                ));
                Some(Action::Close {
                    session_id: id.clone(),
                })
            }
        }
    }

    /// `/forget [ID]`: nothing is deleted, and the sentence points at the door that does exist.
    /// A verb rather than a key because the picker's typed line is a name, and the id is resolved
    /// (an id, or the unique prefix of one) rather than guessed.
    pub(crate) fn forget_named(&mut self, typed: &str) {
        let id = match self.named(typed) {
            Ok(id) => id,
            Err(said) => {
                self.say(&said);
                return;
            }
        };
        let rows = match self.standing(&id) {
            Standing::Listed(i) => self.sessions[i].stored_items,
            Standing::Archived => self
                .archived
                .iter()
                .find(|s| s.session_id == id)
                .map(|s| s.stored_items)
                .unwrap_or(0),
            Standing::Neither => 0,
        };
        let kept = match rows {
            0 => "its row and whatever it is named by".to_string(),
            n => format!("its {n} transcript row(s) and the transcript they are in"),
        };
        self.say(&format!(
            "nothing here is forgotten: forgetting {id} would take {kept}, and an append-only log \
             whose history can be removed is not one. `/archive {id}` is the door that exists — it \
             puts the session out of sight and keeps every row of it."
        ));
    }

    /// Which session a typed word names: its id, an id's unique prefix, or the word inside one
    /// title. Read over both halves of the list, because `/archive` and `/close` take a row either
    /// side of the fold. A miss and an ambiguous prefix are said by name: acting on the wrong
    /// conversation is not a keystroke to take back.
    fn named(&self, typed: &str) -> Result<String, String> {
        let typed = typed.trim();
        if typed.is_empty() {
            // No argument is the row under the cursor, when the list is on the screen - the same
            // reading a key would have taken, without the key.
            return match self.picker_row() {
                Some(row) if !matches!(self.standing(&row.id), Standing::Neither) => Ok(row.id),
                _ => Err(
                    "/archive ID — name a session, or open the list (ctrl-s) and give the verb no \
                     argument: it names the row under the cursor"
                        .to_string(),
                ),
            };
        }
        let lower = typed.to_ascii_lowercase();
        let all = self.sessions.iter().chain(self.archived.iter());
        let hits: Vec<&str> = all
            .filter(|s| {
                s.session_id.starts_with(typed)
                    || (!s.title.is_empty() && s.title.to_ascii_lowercase().contains(&lower))
            })
            .map(|s| s.session_id.as_str())
            .collect();
        match hits.len() {
            1 => Ok(hits[0].to_string()),
            0 => Err(format!(
                "no session matches {typed:?} — /sessions lists them, with the numbers and ids"
            )),
            n => Err(format!(
                "{n} sessions match {typed:?} — give an id, and this head will not guess which \
                 conversation you meant"
            )),
        }
    }

    /// The sentence before the second press - what the row is, what stays, and the way back.
    fn archive_ask(&self, i: usize, id: &str) -> String {
        let s = &self.sessions[i];
        let name = if s.title.is_empty() {
            id.to_string()
        } else {
            // A title is somebody's, and this sentence is drawn on the status row, which does
            // not strip anything: an escape in a title would be measured one way and drawn
            // another - the rule the layer's `clean` and `row_name` keep for the same text.
            s.title
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect()
        };
        let below = self
            .sessions
            .iter()
            .filter(|c| c.parent_session_id.as_deref() == Some(id))
            .count();
        let below = match below {
            0 => String::new(),
            1 => " and the 1 session drawn under it".to_string(),
            n => format!(" and the {n} sessions drawn under it"),
        };
        let keeps = if s.live {
            "the daemon closes it, and every row of it stays in the store"
        } else {
            "every row of it stays in the store"
        };
        format!(
            "del again archives {name} ({id}){below} — {keeps}; ctrl-z brings it back, and enter \
             on its row in the archived group resumes it. `/forget {id}` deletes nothing."
        )
    }

    /// A key that is not the archive ends its arm, so a `del` minutes later is a first press again.
    /// Not every key gets here - a letter typed into the composer never reaches a binding - so the
    /// arm's own clock (`ARM_MS`) is what ends that case.
    pub(crate) fn clear_archive_arm(&mut self) {
        self.archive_armed = None;
    }

    /// Whether `id` is a session the head may act on at all - in the list, or put away. Read by
    /// `/switch`'s own resolution, so a name the picker can show can be reached.
    pub(crate) fn archived_here(&self, id: &str) -> bool {
        self.archived.iter().any(|s| s.session_id == id)
    }
}
