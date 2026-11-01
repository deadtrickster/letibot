//! **Taking a row off the session list** — the operator's *"add controls to remove sessions"*.
//!
//! # Three senses of "remove", and which one this is
//!
//! 1. **Hide it from this list.** The row goes, the session does not: it keeps its store rows, its
//!    transcript and its children, and `ctrl-z` puts it back. This is the one built here, because it
//!    is the one a person can take back.
//! 2. **Forget it in the store** — its `session` row and its transcript. `Store::delete_empty_session`
//!    exists and refuses a session holding transcript rows (`transcript_item` carries a `BEFORE
//!    DELETE` trigger: an append-only log whose history can be removed is not one), and **no frame
//!    reaches it from a head**: the only door is `letibot --delete <id>`, a one-shot on the store
//!    file. A head cannot ask the daemon to do it, so `/forget` refuses and says so
//!    ([`App::forget_row`]).
//! 3. **Close it if it is live, then forget it.** `ClientFrame::CloseSession` closes *the session a
//!    connection is attached to*, so a head can close the session it is in and no other. A list
//!    cannot close its neighbours, which is why sense 3 has no control either.
//!
//! # Why a removal is asked twice
//!
//! It is not undoable *by the row's own key*: `del` arms, `del` again removes, and `ctrl-z` is the
//! way back. Two presses rather than one, because this tree has already been bitten by a key that
//! did something to a running thing on its first character (`app::keys::cards`' `digit_row` note).
//!
//! # Why the keys are not letters
//!
//! The session list is the one surface in this head whose typed line is a NAME rather than a
//! `/verb`, so a letter bound here is matched exactly when the composer is empty — which is exactly
//! the first character of a filter. `del` and `ctrl-z` are the two spellings a typed line can never
//! be, and the destructive sense is `/forget`: typed, therefore unambiguous.
//!
//! # What a removal may not do
//!
//! **The session this head is standing in is refused, and so is every session above it.** The
//! list's whole answer to *where am I* is the row, the chain above it and the open project —
//! removing any link of that chain would take the row you are standing in off the list with it, so
//! the refusal is the whole path and not just its end. Every refusal names the id: a key that says
//! *no* has to say *which*.

use super::*;

/// **How long an armed removal waits for its second press.** Long enough for a person who read the
/// sentence, short enough that a `del` aimed at something else does not remove a row minutes later
/// — and the clock is the only guard for the keys that never reach a binding at all (a letter typed
/// into the composer).
const ARM_MS: u64 = 5_000;

impl App {
    /// **Whether session `i` is off this list** — itself, or because a session above it is: the
    /// unit of removal is a ROW AND ITS SUBTREE, since a child whose parent is not drawn has no
    /// place on a tree.
    ///
    /// One function, read by the head's own walk and by the feed the built-in layer draws from, so
    /// the two drawings cannot come to different conclusions about what is on the list.
    ///
    /// **The chain down to where this head is standing is never removed, whatever the list says.**
    /// That is the invariant `remove_row` enforces by refusing and `prune_removed` enforces by
    /// dropping, stated here as a property of the PREDICATE instead of at each of the three places
    /// that draw — the family view's parent row, the tree's children, and the feed. A `removed`
    /// entry for a session above `here` is then not a row that disappears from the head's drawing
    /// while the layer's falls out of the family view into the whole project tree: it is not a
    /// removal at all.
    pub(crate) fn removed_chain(&self, i: usize) -> bool {
        if self.removed.is_empty() {
            return false;
        }
        if self.chain_to(&self.session_id, &self.sessions[i].session_id) {
            return false;
        }
        let mut at = i;
        for _ in 0..MAX_CHAIN {
            let s = &self.sessions[at];
            if self.removed.contains(&s.session_id) {
                return true;
            }
            match self.parent_on_list(at) {
                Some(parent) => at = parent,
                None => return false,
            }
        }
        false
    }

    /// **`x` on the session list** — remove the row under the cursor, or arm it and say what the
    /// second press would do.
    ///
    /// The refusals come first and each names the session: the one this head is standing in, and
    /// nothing else. A live session *can* be removed — it keeps running, and the sentence says so,
    /// because a person decluttering a list of a hundred sessions must not be told to wait for the
    /// daemon to let go of them.
    pub(crate) fn remove_selected(&mut self) -> Option<Action> {
        let row = self.picker_row()?;
        // A node's line is a fold, not a session: `x` on it says what the key is for rather than
        // silently doing nothing.
        let Some(i) = row.session else {
            self.say(
                "that line is a group, not a session — `x` removes a session's row, and enter or \
                 → folds a group",
            );
            self.redraw = true;
            return None;
        };
        self.remove_row(i);
        None
    }

    /// **Whether `id` is `here`, or a session above it** — the path a removal may not cut, because
    /// the row the person is standing in is drawn *through* it.
    ///
    /// The same walk [`App::removed_chain`] makes, from the other end: that one asks *is a removed
    /// session above this row*, this one asks *is this row above the session I am in*. Both are the
    /// parent chain and nothing else, so a removal cannot make the two disagree.
    fn chain_to(&self, here: &str, id: &str) -> bool {
        let mut cur = here;
        for _ in 0..MAX_CHAIN {
            if cur == id {
                return true;
            }
            let Some(i) = self.index_of(cur) else {
                return false;
            };
            match self.sessions[i].parent_session_id.as_deref() {
                Some(parent) => cur = parent,
                None => return false,
            }
        }
        false
    }

    /// **The removals that survive a seating** — a session the daemon no longer lists, and the
    /// chain down to the session this head has just been seated in.
    ///
    /// **The second half is the refusal seen from the other side.** A removal may not cut the path
    /// to where the head is standing ([`App::remove_row`]), and a head that *moves* onto a removed
    /// row — `/switch <name>`, which searches every session the daemon holds and not just the drawn
    /// rows — would otherwise be standing in a subtree its own list filters out. So landing there
    /// brings the row back: the list's answer to *where am I* is not a thing a removal may keep.
    pub(crate) fn prune_removed(&mut self, here: &str) {
        let kept: Vec<String> = self
            .removed
            .iter()
            .filter(|id| {
                self.sessions.iter().any(|s| &s.session_id == *id) && !self.chain_to(here, id)
            })
            .cloned()
            .collect();
        self.removed = kept;
    }

    /// The act itself, on a session index — the half a test can drive without a cursor.
    pub(crate) fn remove_row(&mut self, i: usize) {
        let id = self.sessions[i].session_id.clone();
        // **The chain to where this head is standing is not removable, link by link.** The row you
        // are in is drawn under its parent, so removing the parent takes the row with it — which is
        // the same defect as removing the row itself, one step out. Both are refused, and each says
        // which of the two it is.
        if id == self.session_id {
            self.removal_armed = None;
            self.say(&format!(
                "{id} is the session you are in — a list that removes where you are standing \
                 cannot answer where am I. Nothing was removed."
            ));
            self.redraw = true;
            return;
        }
        if self.chain_to(&self.session_id, &id) {
            self.removal_armed = None;
            self.say(&format!(
                "{id} is above the session you are in — removing it would take {} off this list \
                 with it, and a list that hides where you are cannot answer where am I. Nothing \
                 was removed.",
                self.session_id
            ));
            self.redraw = true;
            return;
        }
        // **The second press**, on the same row, inside the arm's window.
        if let Some((armed, at)) = self.removal_armed.clone()
            && armed == id
            && self.now_ms <= at.saturating_add(ARM_MS)
        {
            self.removal_armed = None;
            self.removed.push(id.clone());
            self.say(&self.removed_words(&id));
            self.redraw = true;
            return;
        }
        self.removal_armed = Some((id.clone(), self.now_ms));
        self.say(&self.removal_ask(i, &id));
        self.redraw = true;
    }

    /// **`ctrl-z`** — the last removal, back. One at a time, in the order they were removed, and
    /// each press says how many are still hidden.
    pub(crate) fn restore_removed(&mut self) {
        let Some(id) = self.removed.pop() else {
            self.say("nothing is removed from this list — `del` removes the row under the cursor");
            self.redraw = true;
            return;
        };
        self.removal_armed = None;
        self.say(&self.removed_words(&id));
        self.redraw = true;
    }

    /// **`/forget [ID]` — forget it in the store.** Refused, always, and the sentence is the
    /// finding: what a forget would take, and the one door that could take it.
    ///
    /// **A verb rather than a key**, and that is the picker's own rule: its typed line is a name, so
    /// a letter bound there eats the first character of a filter. A line that starts with `/` is a
    /// verb and cannot be anything else.
    ///
    /// **The id is resolved rather than guessed**: an id, or a prefix of one id or title — the same
    /// reading `/switch` gives its argument, minus the switching. Nothing is forgotten either way;
    /// what the person gets is the sentence that says why not and which door could.
    pub(crate) fn forget_named(&mut self, typed: &str) {
        let typed = typed.trim();
        let i = if typed.is_empty() {
            // **No argument is the row under the cursor**, when the list is on the screen — the
            // same reading a key would have taken, without the key.
            match self.picker_row().and_then(|r| r.session) {
                Some(i) => i,
                None => {
                    self.say(
                        "/forget ID — name a session, or open the list (ctrl-s) and /forget with \
                         no argument forgets the row under the cursor",
                    );
                    self.redraw = true;
                    return;
                }
            }
        } else {
            match self.session_named(typed) {
                Ok(i) => i,
                Err(said) => {
                    self.say(&said);
                    self.redraw = true;
                    return;
                }
            }
        };
        self.forget_row(i);
    }

    /// **Which session a typed word names** — an id, or the unique prefix of one id or title. The
    /// refusals are `/switch`'s (`App::pick`): an ambiguous prefix is named as ambiguous and a miss
    /// is named as a miss, because forgetting the wrong conversation is not a keystroke to take
    /// back — and a refusal that resolved it anyway would be worse than the verb not existing.
    fn session_named(&self, typed: &str) -> Result<usize, String> {
        let hits: Vec<usize> = self
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                s.session_id.starts_with(typed)
                    || (!s.title.is_empty()
                        && s.title
                            .to_ascii_lowercase()
                            .contains(&typed.to_ascii_lowercase()))
            })
            .map(|(i, _)| i)
            .collect();
        match hits.len() {
            1 => Ok(hits[0]),
            0 => Err(format!(
                "no session matches {typed:?} — /sessions lists them, with the numbers and ids"
            )),
            n => Err(format!(
                "{n} sessions match {typed:?} — /forget takes an id, and this head will not guess \
                 which conversation you meant"
            )),
        }
    }

    pub(crate) fn forget_row(&mut self, i: usize) {
        let s = &self.sessions[i];
        let id = s.session_id.clone();
        let rows = s.stored_items.max(s.status.items as u32);
        let children = self
            .sessions
            .iter()
            .filter(|c| c.parent_session_id.as_deref() == Some(id.as_str()))
            .count();
        let held = if s.live {
            let heads = match s.status.heads {
                0 => String::new(),
                1 => ", 1 head attached".to_string(),
                n => format!(", {n} heads attached"),
            };
            format!("the daemon is holding it{heads}")
        } else {
            "the daemon is not holding it, so nothing here would have to stop it".to_string()
        };
        let cost = match (rows, children) {
            (0, 0) => "it holds no rows and nothing is under it".to_string(),
            (0, n) => format!("it holds no rows, and {n} session(s) hang from it"),
            (r, 0) => format!("its {r} transcript row(s) and the transcript they are in"),
            (r, n) => format!(
                "its {r} transcript row(s), the transcript they are in, and the {n} session(s) \
                 hanging from it"
            ),
        };
        // **The door that exists, named** — and what it will say when it is tried.
        let door = if rows > 0 {
            format!(
                "`letibot --delete {id}` refuses it for the same reason: transcript_item carries a \
                 BEFORE DELETE trigger, because an append-only log whose history can be removed is \
                 not one"
            )
        } else {
            format!(
                "`letibot --delete {id}` is the door that would, and it is a one-shot on the store file"
            )
        };
        self.say(&format!(
            "{id} is not forgotten: {held}, and forgetting it would take {cost}. {door} — this head \
             has no frame that reaches the store's delete. Nothing was removed."
        ));
        self.redraw = true;
    }

    /// **The sentence before the second press** — what the row is, what goes with it, and what
    /// stays.
    fn removal_ask(&self, i: usize, id: &str) -> String {
        let s = &self.sessions[i];
        let name = if s.title.is_empty() {
            id.to_string()
        } else {
            // **A title is somebody's**, and this sentence is drawn on the status row, which does
            // not strip anything: an escape in a title would be measured one way and drawn
            // another — the rule the layer's `clean` and `row_name` keep for the same text.
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
            "it keeps running, and its rows stay in the store"
        } else {
            "its rows stay in the store"
        };
        format!(
            "del again removes {name} ({id}){below} from this list — {keeps}; ctrl-z brings it \
             back. Forgetting it is `/forget {id}`, and this head cannot."
        )
    }

    /// What a removal or a restore says afterwards — one sentence for both, because a person who
    /// has just pressed one of the two wants the same fact: what is hidden now.
    fn removed_words(&self, id: &str) -> String {
        let n = self.removed.len();
        let hidden = match n {
            0 => "nothing is removed from this list".to_string(),
            1 => "1 session is removed from this list".to_string(),
            n => format!("{n} sessions are removed from this list"),
        };
        if self.removed.iter().any(|r| r == id) {
            format!("{id} is off this list — {hidden}; ctrl-z brings the last one back")
        } else {
            format!("{id} is back on this list — {hidden}")
        }
    }

    /// **A key that is not the removal ends its arm**, so a second `del` minutes later is a first
    /// press again. Called from the surface dispatch — and **not every key gets there**: a letter
    /// typed into the composer is unbound on this surface and never reaches a binding, so the arm's
    /// own clock ([`ARM_MS`]) is what ends that case.
    pub(crate) fn clear_removal_arm(&mut self) {
        self.removal_armed = None;
    }
}
