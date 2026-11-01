//! **Taking a row off the session list** — the operator's *"add controls to remove sessions"*.
//!
//! # Three senses of "remove", and which one this is
//!
//! 1. **Hide it from this list.** The row goes, the session does not: it keeps its store rows, its
//!    transcript and its children, and `u` puts it back. This is the one built here, because it is
//!    the one a person can take back.
//! 2. **Forget it in the store** — its `session` row and its transcript. `Store::delete_empty_session`
//!    exists and refuses a session holding transcript rows (`transcript_item` carries a `BEFORE
//!    DELETE` trigger: an append-only log whose history can be removed is not one), and **no frame
//!    reaches it from a head**: the only door is `letibot --delete <id>`, a one-shot on the store
//!    file. A head cannot ask the daemon to do it, so `f` refuses and says so ([`App::forget_row`]).
//! 3. **Close it if it is live, then forget it.** `ClientFrame::CloseSession` closes *the session a
//!    connection is attached to*, so a head can close the session it is in and no other. A list
//!    cannot close its neighbours, which is why sense 3 has no control either.
//!
//! # Why a removal is asked twice
//!
//! It is not undoable *by the row's own key*: `x` arms, `x` again removes, and `u` is the way back.
//! Two presses rather than one, because this tree has already been bitten by a key that did
//! something to a running thing on its first character (`app::keys::cards`' `digit_row` note).
//!
//! # What a removal may not do
//!
//! **The session this head is standing in is refused.** The list's whole answer to *where am I* is
//! the row, the chain above it and the open project — removing it would leave a list that cannot
//! say. The refusal names the id, and so does every other one: a key that says *no* has to say
//! *which*.

use super::*;

/// **How long an armed removal waits for its second press.** Long enough for a person who read the
/// sentence, short enough that an `x` typed at something else does not remove a row minutes later.
const ARM_MS: u64 = 5_000;

impl App {
    /// **Whether session `i` is off this list** — itself, or because a session above it is: the
    /// unit of removal is a ROW AND ITS SUBTREE, since a child whose parent is not drawn has no
    /// place on a tree.
    ///
    /// One function, read by the head's own walk and by the feed the built-in layer draws from, so
    /// the two drawings cannot come to different conclusions about what is on the list.
    pub(crate) fn removed_chain(&self, i: usize) -> bool {
        if self.removed.is_empty() {
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

    /// The act itself, on a session index — the half a test can drive without a cursor.
    pub(crate) fn remove_row(&mut self, i: usize) {
        let id = self.sessions[i].session_id.clone();
        if id == self.session_id {
            self.removal_armed = None;
            self.say(&format!(
                "{id} is the session you are in — a list that removes where you are standing \
                 cannot answer where am I. Nothing was removed."
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

    /// **`u`** — the last removal, back. One at a time, in the order they were removed, and each
    /// press says how many are still hidden.
    pub(crate) fn restore_removed(&mut self) {
        let Some(id) = self.removed.pop() else {
            self.say("nothing is removed from this list — `x` removes the row under the cursor");
            self.redraw = true;
            return;
        };
        self.removal_armed = None;
        self.say(&self.removed_words(&id));
        self.redraw = true;
    }

    /// **`f` — forget it in the store.** Refused, always, and the sentence is the finding: what a
    /// forget would take, and the one door that could take it.
    ///
    /// Two sentences, because the two cases are two different facts. A live session is refused
    /// *first* for being live — the daemon is holding it, and nothing here stops it — and a dead one
    /// for the store's own rule: `transcript_item` is append-only, so a session that holds rows can
    /// never be deleted, by any build.
    pub(crate) fn forget_selected(&mut self) {
        let Some(row) = self.picker_row() else {
            return;
        };
        let Some(i) = row.session else {
            self.say("that line is a group — there is no session under it to forget");
            self.redraw = true;
            return;
        };
        self.forget_row(i);
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
            "x again removes {name} ({id}){below} from this list — {keeps}; u brings it back. \
             Forgetting it is `f`, and this head cannot."
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
            format!("{id} is off this list — {hidden}; u brings the last one back")
        } else {
            format!("{id} is back on this list — {hidden}")
        }
    }

    /// **A row that folds is not a session**: the arm is dropped by any other key on the pane, so a
    /// second `x` minutes later is a first press again. Called by the surface dispatch, which is the
    /// one place every key of the pane goes through.
    pub(crate) fn clear_removal_arm(&mut self) {
        self.removal_armed = None;
    }
}
