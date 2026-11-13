//! The todos' state: the operator's list, the todo card's draft, the repository's queue,
//! and the stops a click lands on.

use super::*;

/// The id a todo row's question card carries - `todo-ask:<the row's words>`. Not a daemon
/// `req_id` (those are minted by the gate), so it cannot collide with one, and
/// [`App::settle_question`] reads the row back out of it.
pub(crate) const TODO_ASK: &str = "todo-ask:";

/// One row's question for the operator, as this head's board holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OperatorAsk {
    pub(crate) content: String,
    pub(crate) question: String,
    pub(crate) options: Vec<String>,
}

/// A question card the person put away with Esc: still pending, nothing sent. Esc is not an answer of any kind,
/// and it keeps the decision, the words typed under it and the highlight, so alt-a puts it back. `session_id` is
/// where it was asked, so a switch never brings it back over the wrong conversation.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AwayCard {
    pub(crate) session_id: String,
    pub(crate) decision: letibot_sessionlog::view::OpenDecision,
    pub(crate) draft: String,
    pub(crate) sel: usize,
}

/// A card whose answer has left and is not settled yet - see [`App::answering`].
#[derive(Debug, Clone)]
pub(crate) struct Answering {
    pub(crate) decision: letibot_sessionlog::view::OpenDecision,
    /// The row that was marked, so a refused answer puts the card back as it was.
    pub(crate) sel: usize,
    /// The frame's `client_request_id`, once the driver has sent it - what a `Rejected` names.
    pub(crate) request: Option<String>,
}

impl AwayCard {
    /// Whether this is a todo row's card rather than the model's question.
    pub(crate) fn is_todo(&self) -> bool {
        self.decision.req_id.starts_with(TODO_ASK)
    }
}

impl App {
    /// The person's rows, out of the union the daemon serves: the head keeps no second list, since the daemon
    /// persists them and every head sees them, and sending is the whole list, so a divergence cannot accumulate.
    pub(crate) fn operator_todos(&self) -> Vec<letibot_sessionlog::event::TodoEntry> {
        self.todos
            .iter()
            .filter(|t| t.by == letibot_sessionlog::event::TodoBy::Operator)
            .cloned()
            .collect()
    }

    /// Whether the board this head is holding is the session's own - i.e. the `asks` settings
    /// row has nothing left to say. See [`App::asks_board`]: until a board has been read, the
    /// settings row is the only thing that knows a question was asked; after it, the board is.
    pub(crate) fn board_read_here(&self) -> bool {
        self.asks_board.as_deref() == Some(self.session_id.as_str())
    }

    /// The rows on this head's board that wait on the operator, with the question and the
    /// options - open rows only (a done or struck-off row asks nobody anything), in board order.
    /// The same reading as the daemon's `operator_asks`, over the wire's copy of the board.
    pub(crate) fn operator_asks(&self) -> Vec<OperatorAsk> {
        use letibot_sessionlog::event::{TodoNeed, TodoStatus};
        let mut out = Vec::new();
        for t in &self.todos {
            if matches!(t.status, TodoStatus::Completed | TodoStatus::Cancelled) {
                continue;
            }
            for n in &t.needs {
                if let TodoNeed::Operator { question, options } = n {
                    if self
                        .todo_answered
                        .iter()
                        .any(|(c, q)| c == &t.content && q == question)
                    {
                        // Answered from here; the board saying so is on its way.
                        continue;
                    }
                    out.push(OperatorAsk {
                        content: t.content.clone(),
                        question: question.clone(),
                        options: options.clone(),
                    });
                }
            }
        }
        out
    }

    /// Open the question card for a row that waits on the person, or the first one. It is the same card
    /// `ask_user_question` draws, with the row's options as its choices, and the answer goes to the row rather than
    /// to the daemon, so a resync takes it down.
    pub(crate) fn open_operator_ask(&mut self, content: Option<&str>) {
        let asks = self.operator_asks();
        let ask = match content {
            Some(c) => asks.into_iter().find(|a| a.content == c),
            None => asks.into_iter().next(),
        };
        let Some(ask) = ask else {
            // A board already read says `nothing` and not `still being read`: the settings row may still quote a
            // question the row no longer carries, and telling the person to press alt-a again repeats a stale sentence.
            let unread = self.asks_setting.is_some() && !self.board_read_here();
            self.say(if unread {
                "the board is still being read — alt-a again in a moment"
            } else {
                "nothing is waiting on you"
            });
            if unread && !self.queued.contains(&Action::ListTodos) {
                self.queued.push(Action::ListTodos);
            }
            return;
        };
        let req_id = format!("{TODO_ASK}{}", ask.content);
        if let Some(first) = self.open.first()
            && first.req_id != req_id
            && !first.req_id.starts_with(TODO_ASK)
        {
            self.say("answer the open card first — then alt-a opens the question waiting on you");
            return;
        }
        if self.open.first().is_some_and(|d| d.req_id == req_id) {
            // Already the card on the screen: nothing to open, and its draft is the composer.
            return;
        }
        // What was put away for this row comes back with it, the words and the highlight, provided it is still the
        // same question: a row that asks something new starts clean. Looked up after the front card is stashed.
        if self
            .open
            .first()
            .is_some_and(|d| d.req_id.starts_with(TODO_ASK))
        {
            self.stash_front_card();
        }
        // The operator's own line is held, never the card's answer.
        self.hold_typed_line();
        let kept = self.away.iter().position(|a| {
            a.decision.req_id == req_id
                && a.decision.summary == ask.question
                && a.session_id == self.session_id
        });
        let (draft, sel) = match kept {
            Some(i) => {
                let a = self.away.remove(i);
                (a.draft, a.sel)
            }
            None => (String::new(), 0),
        };
        if !draft.is_empty() {
            self.set_composer(&draft);
        }
        self.sel = sel.min(ask.options.len().saturating_sub(1));
        self.sel_for = req_id.clone();
        self.open.insert(
            0,
            letibot_sessionlog::view::OpenDecision {
                req_id,
                kind: "question".into(),
                call_id: None,
                subagent: None,
                access: String::new(),
                summary: ask.question.clone(),
                target: format!("the todo row `{}` waits on your answer", ask.content.trim()),
                write_targets: Vec::new(),
                detail: String::new(),
                options: Vec::new(),
                // A todo row's question has no adjudication ladder, so no rung was left
                // off it and there is nothing to explain.
                options_note: String::new(),
                choices: ask.options.clone(),
                because: String::new(),
                advice: None,
                deadline: None,
                on_timeout: letibot_sessionlog::event::OnTimeout::Ask,
                asked_ts: 0,
            },
        );
    }

    /// A question card's answer, as the action that carries it - to the ROW for a card
    /// [`App::open_operator_ask`] opened (and the card comes down: nothing on the daemon will
    /// settle it), to the open decision for every other question.
    pub(crate) fn settle_question(
        &mut self,
        req_id: String,
        answer: letibot_sessionlog::question::QuestionAnswer,
    ) -> Action {
        if let Some(content) = req_id.strip_prefix(TODO_ASK) {
            let question = self
                .open
                .iter()
                .find(|d| d.req_id == req_id)
                .map(|d| d.summary.clone())
                .unwrap_or_default();
            self.open.retain(|d| d.req_id != req_id);
            self.away.retain(|a| a.decision.req_id != req_id);
            self.sel = 0;
            self.todo_answered
                .push((content.to_string(), question.clone()));
            self.advance_operator_asks();
            return Action::AnswerTodo {
                content: content.to_string(),
                question,
                answer,
            };
        }
        Action::AnswerQuestion { req_id, answer }
    }

    /// After a todo answer, the card moves on to the next waiting row in this session and closes when none remain, saying which
    /// happened. Another card on screen is left alone: the next row waits for alt-a.
    ///
    /// ```text
    /// answered — 2 more waiting on you
    /// answered — nothing else is waiting on you
    /// ```
    fn advance_operator_asks(&mut self) {
        let left = self.operator_asks();
        let Some(next) = left.first() else {
            // The `asks` settings row said so before the answer; it is stale now, and the
            // resident line must not fall back to it.
            self.asks_setting = None;
            self.say("answered — nothing else is waiting on you");
            return;
        };
        let n = left.len();
        if self.open.is_empty() {
            let content = next.content.clone();
            self.open_operator_ask(Some(&content));
            self.say(&format!("answered — {n} more waiting on you"));
        } else {
            self.say(&format!(
                "answered — {n} more waiting on you; alt-a opens the next once this card is done"
            ));
        }
    }

    /// Esc on a question card: put it away and answer nothing, since it is neither an abstention nor a denial, and
    /// the resident line keeps saying so. The words go with the card: while a question is up the composer is its
    /// field, so what is typed there is a draft that would otherwise be sent as a prompt.
    pub(crate) fn put_question_away(&mut self) {
        let Some(d) = self.open.first() else {
            return;
        };
        let question = d.kind == "question";
        self.stash_front_card();
        self.say(if question {
            "put away — nothing was sent, it is still waiting on you · alt-a brings it back"
        } else {
            "put away — nothing was answered, the call is still waiting on you · alt-a brings it \
             back"
        });
    }

    /// Move the card on top of the open list to [`App::away`] with the composer's words and the
    /// highlight, and empty the composer.
    fn stash_front_card(&mut self) {
        if self.open.is_empty() {
            return;
        }
        let decision = self.open.remove(0);
        let draft = self.editor.expanded();
        if !self.editor.text().is_empty() {
            // ctrl-c on a non-empty composer is the editor's own "clear what I typed", and an
            // undo away; Home + kill-to-end would clear one line of several.
            self.editor.key(letibot_ui::editor::Key::CtrlC, self.now_ms);
        }
        self.away.retain(|a| a.decision.req_id != decision.req_id);
        self.away.push(AwayCard {
            session_id: self.session_id.clone(),
            decision,
            draft,
            sel: self.sel,
        });
        self.sel = 0;
    }

    /// A resync or a switch replaces the open list with the daemon's, and this head's own todo cards are not on it:
    /// they are put away with the front card's words rather than dropped, or the card vanished and the composer kept
    /// the words typed for it.
    pub(crate) fn stash_todo_cards(&mut self) {
        let mut stashed = false;
        if self
            .open
            .first()
            .is_some_and(|d| d.req_id.starts_with(TODO_ASK))
        {
            self.stash_front_card();
            stashed = true;
        }
        while let Some(i) = self
            .open
            .iter()
            .position(|d| d.req_id.starts_with(TODO_ASK))
        {
            let decision = self.open.remove(i);
            self.away.retain(|a| a.decision.req_id != decision.req_id);
            self.away.push(AwayCard {
                session_id: self.session_id.clone(),
                decision,
                draft: String::new(),
                sel: 0,
            });
            stashed = true;
        }
        if stashed {
            self.say("the todo question was put away — alt-a brings it back with what you typed");
        }
    }

    /// The board moved, so a todo card whose row no longer asks is closed and one put away is forgotten: another head
    /// answered the row or the model completed it, and answering a card nobody was waiting on sent an answer for a
    /// question that was gone.
    pub(crate) fn prune_todo_asks(&mut self) {
        let asks = self.operator_asks();
        let live = |req_id: &str, question: &str| {
            req_id.strip_prefix(TODO_ASK).and_then(|content| {
                asks.iter()
                    .find(|a| a.content == content && a.question == question)
            })
        };
        let mut closed_front = false;
        let mut i = 0;
        while i < self.open.len() {
            let d = &self.open[i];
            if !d.req_id.starts_with(TODO_ASK) {
                i += 1;
                continue;
            }
            match live(&d.req_id, &d.summary) {
                Some(a) => {
                    let choices = a.options.clone();
                    self.open[i].choices = choices;
                    i += 1;
                }
                None => {
                    self.open.remove(i);
                    closed_front |= i == 0;
                }
            }
        }
        let sid = self.session_id.clone();
        self.away.retain(|a| {
            !a.is_todo()
                || a.session_id != sid
                || live(&a.decision.req_id, &a.decision.summary).is_some()
        });
        if closed_front {
            // The composer was the card's field: its words were an answer, not a prompt.
            let typed = !self.editor.text().is_empty();
            self.clear_composer();
            self.say(if typed {
                "that todo row no longer waits on you — its card closed, and what you typed on it \
                 was cleared (ctrl-z brings it back)"
            } else {
                "that todo row no longer waits on you — its card closed"
            });
        }
        if let Some(d) = self.open.first() {
            self.sel = self.sel.min(decision_rows(d).saturating_sub(1));
        }
    }

    /// An answer to the model's question or to a permission is leaving, so its card closes now: kept
    /// up until `DecisionAnswered` came back, a second Enter in that gap sent the same answer again.
    /// The card is held in [`App::answering`] so a refusal can put it back.
    pub(crate) fn answer_leaving(&mut self, req_id: &str) {
        let Some(i) = self.open.iter().position(|d| d.req_id == req_id) else {
            return;
        };
        let decision = self.open.remove(i);
        let sel = if i == 0 { self.sel } else { 0 };
        self.answering.retain(|a| a.decision.req_id != req_id);
        self.answering.push(Answering {
            decision,
            sel,
            request: None,
        });
    }

    /// The driver sent it, under this `client_request_id` - what a refusal will name.
    pub fn answer_sent(&mut self, req_id: &str, request: String) {
        if let Some(a) = self
            .answering
            .iter_mut()
            .find(|a| a.decision.req_id == req_id)
        {
            a.request = Some(request);
        }
    }

    /// The daemon refused an answer this head sent - its card goes back in front, the
    /// marked row as it was. `false` when `request` was not an answer of ours. A refusal that
    /// says the decision is no longer open puts nothing back: it was settled elsewhere.
    pub(crate) fn answer_refused(&mut self, request: &str, reason: &str) -> bool {
        let Some(i) = self
            .answering
            .iter()
            .position(|a| a.request.as_deref() == Some(request))
        else {
            return false;
        };
        let a = self.answering.remove(i);
        if reason != letibot_sessionlog::protocol::REJECT_UNKNOWN_DECISION {
            self.open.insert(0, a.decision.clone());
            self.sel_for = a.decision.req_id.clone();
            self.sel = a.sel;
        }
        true
    }

    /// The front card changed, or did not: run after every key and every frame. The highlight is reset exactly when
    /// `open[0]` is a different card, and a question arriving over composer text holds that text rather than taking
    /// it as its answer.
    pub(crate) fn sync_front(&mut self) {
        let front = self
            .open
            .first()
            .map(|d| d.req_id.clone())
            .unwrap_or_default();
        let question = self.open.first().is_some_and(|d| d.kind == "question");
        // The mode card is not in `open`, so it needs its own detector: a card that appears
        // over a half-typed line holds it, exactly as a question does, or the Enter that
        // meant to send the line answers the card instead.
        let mode_up = self.mode_confirm.is_some();
        if mode_up != self.mode_up_for {
            self.mode_up_for = mode_up;
            if mode_up && !self.editor.text().is_empty() {
                self.hold_line();
                self.say(
                    "the allow-all card came up — your line is held, and comes back when the \
                     card is answered or put away",
                );
            }
        }
        // The typed cards are not in `open`, and a sudo password or an API key typed into the
        // open file is a leak: while one is up the editor does not hold the keyboard.
        let typed_card = (self.secret.is_some() && !self.secret_away)
            || (self.prompt.is_some() && !self.prompt_away)
            || self.key_ask.is_some()
            || self.mode_confirm.is_some();
        if typed_card
            && let Some(p) = self.edit_pane.as_mut()
            && p.focused
        {
            p.focused = false;
            p.dirty = true;
            self.say(
                "a card came up — the keyboard is back on the card; ctrl-] returns to the editor",
            );
        }
        if front != self.sel_for {
            self.sel_for = front.clone();
            self.sel = 0;
            // The arming window starts with the card: see `App::card_arming`.
            self.card_arrived = self.now_ms;
            // A decision coming up closes the quit card: the decision is drawn ahead
            // of it and takes its keys, so a `1` meant as "detach" would answer the decision.
            if !front.is_empty() && self.quit_card {
                self.quit_card = false;
                self.say("a decision came up — the quit card closed; ctrl-c twice asks again");
            }
            // A decision coming up takes the keyboard back from the editor pane:
            // rano took every key while it was focused, so `1` or Enter meant for the card
            // edited the file.
            if !front.is_empty()
                && let Some(p) = self.edit_pane.as_mut()
                && p.focused
            {
                p.focused = false;
                p.dirty = true;
                self.say("a decision came up — the keyboard is back on the card; ctrl-] returns to the editor");
            }
            // A draft card in front keeps the keys, and says what waits behind it.
            if !front.is_empty() && (self.todo_draft.is_some() || self.key_ask.is_some()) {
                self.say("a decision is waiting behind this card — enter or esc it, then answer");
            }
            if question && !self.editor.text().is_empty() {
                self.hold_line();
                self.say(
                    "a question came up — your line is held, and comes back when the card is \
                     answered or put away",
                );
            }
        }
        if !question
            && !mode_up
            && let Some(held) = self.held_line.take()
        {
            let now = self.editor.expanded();
            self.clear_composer();
            if now.trim().is_empty() {
                self.editor.insert(&held);
            } else {
                self.editor.insert(&format!("{held}\n{now}"));
            }
        }
    }

    /// [`App::hold_line`], saying so when there was a line to hold.
    fn hold_typed_line(&mut self) {
        if !self.editor.text().is_empty() {
            self.hold_line();
            self.say("your line is held — it comes back when the card is answered or put away");
        }
    }

    /// Take the composer's words aside into [`App::held_line`] and empty it.
    pub(crate) fn hold_line(&mut self) {
        if self.editor.text().is_empty() {
            return;
        }
        let words = self.editor.expanded();
        self.held_line = Some(match self.held_line.take() {
            Some(h) => format!("{h}\n{words}"),
            None => words,
        });
        self.clear_composer();
    }

    /// Empty the composer, every line of it - ctrl-c on a non-empty composer is the editor's own
    /// "clear what I typed", and an undo away; Home + kill-to-end would clear one line of several.
    pub(crate) fn clear_composer(&mut self) {
        if !self.editor.text().is_empty() {
            self.editor.key(letibot_ui::editor::Key::CtrlC, self.now_ms);
        }
    }

    /// alt-a: bring back the oldest pending question of either kind. One key for both, since the resident line names
    /// one key, and oldest is read as the order the two can be compared in: a model's question first, since it has a
    /// clock, then a todo row's card, then the first row waiting.
    pub(crate) fn bring_back_question(&mut self) {
        // The prompt card put away comes back first: it is the operator's own
        // command blocked on a line, and alt-a was no way back to it at all - only `!send`.
        if self.prompt.is_some() && self.prompt_away {
            self.prompt_away = false;
            self.prompt_buf.clear();
            return;
        }
        // Then the password card Esc put away: sudo keeps waiting, and this is the only
        // way back to it. Behind a card that is up it waits its turn, as questions do.
        if self.secret.is_some() && self.secret_away && self.card_up().is_none() {
            self.secret_away = false;
            return;
        }
        // And then an allow-all another head asked for: the request is still open at the
        // daemon, and the card comes back armed, exactly as it first arrived.
        if let Some(asked) = self.mode_away.clone()
            && self.card_up().is_none()
        {
            self.mode_away = None;
            let name = asked.name.clone();
            self.open_mode_card(name, Some(asked));
            return;
        }
        if let Some(d) = self.open.first()
            && d.kind == "question"
            && !d.req_id.starts_with(TODO_ASK)
        {
            // About THIS card, and what alt-a will do once it is out of the way - the
            // operator pressing alt-a wanted the other question.
            self.say(
                "a question is the open card — answer it, or esc puts it away; then alt-a opens \
                 what else waits on you",
            );
            return;
        }
        let sid = self.session_id.clone();
        if self
            .away
            .iter()
            .any(|a| !a.is_todo() && a.session_id == sid)
        {
            if !self.open.is_empty() && !self.open[0].req_id.starts_with(TODO_ASK) {
                self.say("answer the open card first — then alt-a brings the question back");
                return;
            }
            if !self.open.is_empty() {
                self.stash_front_card();
            }
            // The operator's own line is held, never the card's answer.
            self.hold_typed_line();
            let Some(i) = self
                .away
                .iter()
                .position(|a| !a.is_todo() && a.session_id == sid)
            else {
                return;
            };
            let a = self.away.remove(i);
            if !a.draft.is_empty() {
                self.set_composer(&a.draft);
            }
            self.sel = a.sel.min(decision_rows(&a.decision).saturating_sub(1));
            self.sel_for = a.decision.req_id.clone();
            self.open.insert(0, a.decision);
            return;
        }
        let asks = self.operator_asks();
        let kept = self.away.iter().find_map(|a| {
            let content = a.decision.req_id.strip_prefix(TODO_ASK)?;
            (a.session_id == sid
                && asks
                    .iter()
                    .any(|x| x.content == content && x.question == a.decision.summary))
            .then(|| content.to_string())
        });
        self.open_operator_ask(kept.as_deref());
    }

    /// Open the new-todo card, and put the composer where it expects it: the composer is the field, so it is emptied
    /// and handed over, since a half-typed prompt left under a card whose Enter adds an item costs somebody a message.
    pub(crate) fn open_todo_card(&mut self) {
        if self.session_id.is_empty() {
            self.say("not attached to a session yet");
            return;
        }
        self.set_composer("");
        self.todo_draft = Some(TodoDraft::new());
        self.say(
            "adding a todo item — title, then tab for the description; enter adds it to the \
             session's plan as yours, esc cancels",
        );
    }

    /// `/todo ...`, the person's own rows. Every form sends the whole list.
    ///
    /// ```text
    /// /todo finish the parity row        add it, at the end
    /// /todo done 2                       mark the second of MY rows complete
    /// /todo rm 2                         strike it off: it STAYS, marked `[c]`
    /// /todo postpone 2                   set it aside: it stays, and nothing nags about it
    /// /todo resume 2                     put it back in the list
    /// /todo postpone paste the token     the same, naming a row the MODEL wrote
    /// ```
    ///
    /// A number counts the person's rows only. A non-numeric argument names any row by its words, since the pane numbers one half and
    /// `todo_write` refuses to postpone. A parent's row is refused, as a parent owns its children's states. Neither verb removes a row:
    /// `done` and `rm` keep the words and place, and `/todo resume N` lifts either. The model may move a status, not remove a row.
    /// The person's own rows are drawn from the moment they are sent, as `pending_prompts` does for their words: the command reaches the
    /// worker behind the running turn, so until `TodosUpdated` arrives the pane would show nothing. The daemon's answer replaces it.
    pub(crate) fn echo_operator_todos(&mut self, mine: Vec<letibot_sessionlog::event::TodoEntry>) {
        self.todos
            .retain(|t| t.by != letibot_sessionlog::event::TodoBy::Operator);
        self.todos.extend(mine);
    }

    /// Is a starter-todo seed due for this project? The switch is on, the workspace is known and the record does not
    /// hold it. The record and not the list, since a starter row the person deleted must not come back.
    pub(crate) fn todo_seed_due(&self) -> bool {
        self.todo_template != crate::prefs::TodoTemplate::Off
            && !self.wiring.workspace.is_empty()
            && !self
                .todo_seed
                .contains(&todo_seed_key(&self.wiring.workspace))
    }

    /// Where the starter todos come from: `todo_template`'s three shapes as a path, one function because the setting
    /// has three shapes and two callers must not spell them differently. `None` when the switch is off or this head
    /// has no config directory.
    pub(crate) fn todo_template_file(&self) -> Option<std::path::PathBuf> {
        match &self.todo_template {
            crate::prefs::TodoTemplate::Off => None,
            crate::prefs::TodoTemplate::Path(p) => Some(std::path::PathBuf::from(p)),
            crate::prefs::TodoTemplate::Default => self
                .prefs_path
                .as_ref()
                .and_then(|p| p.parent())
                .map(|d| d.join("todo-template.md")),
        }
    }

    /// Copy the template `TODO.md`'s items onto the person's half of the board, the moment the head has learned its
    /// list: parsed by the same reader the repo section uses, items only, with `[x]` seeding a completed row. A body
    /// line joins into `content`, since this head keeps no second list.
    pub(crate) fn seed_todos(&mut self) {
        if !self.todo_seed_due() {
            return;
        }
        let Some(path) = self.todo_template_file() else {
            self.say("todo_template is on but this head has no config directory to read it from");
            return;
        };
        let body = match std::fs::read_to_string(&path) {
            Ok(b) => b,
            Err(e) => {
                // Not marked - see the docstring: the file may still be written.
                self.say(&format!(
                    "todo_template is on but {} is not there: {e}",
                    path.display()
                ));
                return;
            }
        };
        let rows = render_todo_md(&body);
        let items: Vec<&TodoRow> = rows
            .iter()
            .filter(|r| r.item && !r.text.trim().is_empty())
            .collect();
        if items.is_empty() {
            self.mark_seeded();
            self.say(&format!(
                "{} has no items in it, so nothing was added",
                path.display()
            ));
            return;
        }
        let mut mine = self.operator_todos();
        // A template item over the row byte cap is not seeded, and it is said: from the person's side a row that
        // silently did not arrive is indistinguishable from a file never read, and the rest of the template still
        // lands.
        let mut skipped: Vec<(String, String)> = Vec::new();
        for r in &items {
            let mut content = r.text.trim().to_string();
            if !r.body.is_empty() {
                content.push_str(" — ");
                content.push_str(&r.body.join(" · "));
            }
            if let Some(why) = row_refusal(&content) {
                skipped.push((content, why));
                continue;
            }
            mine.push(letibot_sessionlog::event::TodoEntry {
                content,
                status: if r.mark == Some(TodoMark::Done) {
                    letibot_sessionlog::event::TodoStatus::Completed
                } else {
                    letibot_sessionlog::event::TodoStatus::Pending
                },
                by: letibot_sessionlog::event::TodoBy::Operator,
                when: None,
                // A template's items are rows, not a graph: the file is a checklist and has
                // nowhere to say what waits on what, so a seeded row waits on nothing.
                needs: Vec::new(),
            });
        }
        let n = items.len() - skipped.len();
        self.mark_seeded();
        self.echo_operator_todos(mine.clone());
        self.queued.push(Action::SetOperatorTodos {
            items: mine,
            moved: Vec::new(),
        });
        // The count, and then every item that was too long to be a title, by name - one notice,
        // because `say` replaces what it last said: two calls would leave only the refusals on the
        // screen and the count unread. The seed's own rule is *"refusals are notes, not silences"*.
        let mut said = format!(
            "{n} starter todo{} from {}",
            if n == 1 { "" } else { "s" },
            path.display()
        );
        if !skipped.is_empty() {
            let listed: Vec<String> = skipped
                .iter()
                .map(|(content, why)| format!("  {why}\n    `{content}`"))
                .collect();
            said.push_str(&format!(
                "\n{} item{} of the template not seeded:\n{}",
                skipped.len(),
                if skipped.len() == 1 { "" } else { "s" },
                listed.join("\n")
            ));
        }
        self.say(&said);
    }

    /// Record that this project has had its starter todos, and do it before the send: a record that waits for an
    /// acknowledgement re-fires on the next start if the write failed quietly, and the person gets the duplicates
    /// this feature exists to avoid.
    pub(crate) fn mark_seeded(&mut self) {
        let key = todo_seed_key(&self.wiring.workspace);
        if !self.todo_seed.contains(&key) {
            self.todo_seed.push(key);
        }
        if let Some(path) = self.prefs_path.clone() {
            let mut p = self.prefs();
            p.todo_seed = crate::prefs::merge_todo_seed(&path, &self.todo_seed);
            self.todo_seed = p.todo_seed.clone();
            // And the retired set, for the reason `merge_todo_seed` exists: `prefs()` carries THIS head's `dismissed`
            // and `prefs::save` writes the whole file, so this write replaces the record of which notes were retired.
            p.retired = crate::prefs::merge_retired(&path, &self.dismissed);
            if let Err(e) = crate::prefs::save(&path, &p) {
                self.say(&format!("seed not recorded: {e}"));
            }
        }
    }

    /// How many rows the done group holds right now, both halves of the board whichever way the group is folded:
    /// one number, and it does not move with the fold, since the line speaks for the whole pane.
    pub(crate) fn todos_done_count(&self) -> usize {
        use letibot_sessionlog::event::TodoStatus;
        let mine = self
            .todos
            .iter()
            .filter(|t| t.status == TodoStatus::Completed)
            .count();
        let repo = self.repo_todos.as_ref().map_or(0, |rows| {
            rows.iter()
                .filter(|r| r.item && r.mark == Some(TodoMark::Done))
                .count()
        });
        mine + repo
    }

    /// The stop the cursor is on, read through the one enumeration and clamped - `None` on a
    /// board with nothing on it.
    fn selected_todo_stop(&self) -> Option<TodoStop> {
        let stops = self.todos_stops();
        stops
            .get(self.todos_sel.min(stops.len().saturating_sub(1)))
            .cloned()
    }

    /// `d` on the todos pane: fold the done group away, or open it. The cursor is carried across by identity and
    /// not by its index, since folding takes entries OUT of `todos_stops` and an index kept across it lands on
    /// whichever row slid into the hole.
    pub(crate) fn toggle_todos_fold_done(&mut self) {
        let was = self.selected_todo_stop();
        self.todos_fold_done = !self.todos_fold_done;
        let stops = self.todos_stops();
        self.todos_sel = match was.and_then(|s| stops.iter().position(|here| *here == s)) {
            Some(at) => at,
            None => self.todos_sel.min(stops.len().saturating_sub(1)),
        };
        self.sync_repo_from_stop(&stops);
        // The window follows on the next DRAW, which is the only place that knows which pane
        // row the cursor landed on ([`App::todos_stop_rows`]). A scroll computed here would read
        // the record the last frame wrote, and that record describes the list as it was.
        self.todos_follow = true;
        // Remembered across restarts, like every other switch on `/config` - a switch that
        // forgets is a switch the reader keeps re-making, and the operator asked for this one
        // about a board whose done rows are the noise.
        let saved = self.save_prefs(RetiredWrite::Union);
        let n = self.todos_done_count();
        let line = if n == 0 {
            // A key that changes nothing on the screen reads as a key that does nothing: said,
            // not silently a no-op.
            "nothing is done — there is nothing to fold".to_string()
        } else if self.todos_fold_done {
            format!("{n} done folded away — d shows them")
        } else {
            format!(
                "the {n} done row{} back",
                if n == 1 { " is" } else { "s are" }
            )
        };
        self.say(&format!("{line}{saved}"));
    }

    /// Every row of the todos pane the cursor may land on, in the order the pane draws them, and the ONE enumeration
    /// everything about the cursor reads. Two readings of where the cursor is were the defect the operator reported
    /// twice.
    pub(crate) fn todos_stops(&self) -> Vec<TodoStop> {
        let mut out = vec![TodoStop::Add];
        for line in board_drawn(&self.todos, self.todos_fold_done) {
            let BoardLine::Row(i) = line else { continue };
            let t = &self.todos[i];
            out.push(if t.by == letibot_sessionlog::event::TodoBy::Operator {
                TodoStop::Mine(t.content.clone())
            } else {
                TodoStop::Theirs(t.content.clone())
            });
        }
        if let Some(rows) = &self.repo_todos {
            for line in repo_drawn(rows, self.todos_fold_done) {
                if let RepoLine::Item(i) = line {
                    out.push(TodoStop::Repo(i));
                }
            }
        }
        out
    }

    /// The pane row the stop at the cursor was drawn on, read out of [`App::todos_stop_rows`], which is the record
    /// the pane wrote while drawing and not arithmetic over the lists it drew from.
    pub(crate) fn todos_row_of(&self) -> usize {
        let at = self
            .todos_sel
            .min(self.todos_stop_rows.len().saturating_sub(1));
        self.todos_stop_rows.get(at).copied().unwrap_or(0)
    }

    /// The repo cursor follows the stop cursor, for the repo's own keys - the unfold and the
    /// body it shows read `repo_sel`, and a second cursor that did not follow would be the two
    /// enumerations this whole change removed.
    pub(crate) fn sync_repo_from_stop(&mut self, stops: &[TodoStop]) {
        let at = self.todos_sel.min(stops.len().saturating_sub(1));
        if let Some(TodoStop::Repo(i)) = stops.get(at) {
            self.repo_sel = *i;
        }
    }

    /// Re-read `TODO.md` when it has changed since the last read, and not otherwise: called on open and before every
    /// draw. A failed `stat` reads again, so the pane's *no TODO.md* line is said either way.
    pub(crate) fn refresh_repo_todos(&mut self) {
        // Resolved, like the read it guards: the mtime watch and the parse must look at the same file, or a nested
        // layout re-reads on every draw. Both ask `todos_path`.
        let path = crate::gitfield::todos_path(&self.wiring.workspace);
        let now = std::fs::metadata(&path)
            .ok()
            .and_then(|m| Some((m.modified().ok()?, m.len())));
        if self.repo_todos.is_some() && now.is_some() && now == self.repo_todos_at {
            return;
        }
        self.repo_todos_at = now;
        self.repo_todos = Some(repo_todos_map(&self.wiring.workspace));
    }
}

/// What a todo row waits on that this head can open: a job, or a child session, and the handle is a link to the
/// real thing. The row's `when` job condition when it has one, else its first `needs` edge on a job or a child.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WaitOn {
    /// A job handle, `j97`.
    Job(String),
    /// A child's session id, `s-…-sub-…`.
    Child(String),
}

/// The thing `t` waits on, by the rule [`WaitOn`] names.
pub(crate) fn wait_of(t: &letibot_sessionlog::event::TodoEntry) -> Option<WaitOn> {
    use letibot_sessionlog::event::{TodoCondition, TodoNeed};
    match &t.when {
        Some(TodoCondition::Job { handle }) => Some(WaitOn::Job(handle.clone())),
        None => t.needs.iter().find_map(|n| match n {
            TodoNeed::Job { handle } => Some(WaitOn::Job(handle.clone())),
            TodoNeed::Child { id } => Some(WaitOn::Child(id.clone())),
            _ => None,
        }),
    }
}

/// What a row waits on that the pane SAYS and cannot open: another session's answer, and the answer
/// this session owes somebody. [`wait_of`]'s sibling - a job and a child are links because the head
/// holds their tables, and a peer is only a name.
pub(crate) fn waits_words(t: &letibot_sessionlog::event::TodoEntry) -> Option<String> {
    use letibot_sessionlog::event::TodoNeed;
    t.needs.iter().find_map(|n| match n {
        // The peer, by its handle: that is the name a person reads on the other session's board.
        TodoNeed::Ask { of, .. } => Some(of.clone()),
        // The OWED side: the row is the model's own work, and what it waits on is the person
        // sitting in front of it - which is the pane's `you` one register down.
        TodoNeed::Owed { .. } => Some("your answer".to_string()),
        // A process on this host, by its number: said, and openable by nothing here.
        TodoNeed::Pid { pid, .. } => Some(format!("pid {pid}")),
        // Something outside this machine, in the row's own words: the pane draws `waits on <what>`
        // and holds no table behind it, because nothing here can see that wait end.
        TodoNeed::External { what } => Some(what.clone()),
        _ => None,
    })
}

/// One drawn `waits on …` link: the pane row it is on, the columns its words cover, and the row it belongs to,
/// which is its place among the session rows the pane DREW and not an index into the board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WaitLink {
    pub(crate) row: usize,
    pub(crate) from: usize,
    pub(crate) to: usize,
    pub(crate) todo: usize,
    pub(crate) target: WaitOn,
}

impl App {
    /// The words a wait is drawn as: a job with the slug the head knows, the bare handle otherwise, and a child as
    /// the subagents pane names it rather than the whole id, which the pane could not fit.
    pub(crate) fn wait_label(&self, w: &WaitOn) -> String {
        match w {
            WaitOn::Job(h) => match self.job_slug(h) {
                Some(slug) => format!("{h} · {slug}"),
                None => h.clone(),
            },
            // A child by its name when it has one - `…14688401 · fix-login`, the handle first
            // as a job's is (`j97 · pg-timing`) - and by its role when it has none, as before.
            WaitOn::Child(id) => {
                let short = letibot_sessionlog::registry::short_id(id);
                let row = self.subagents.iter().find(|s| s.session_id == *id);
                let slug = row
                    .map(|s| s.slug.clone())
                    .filter(|s| !s.is_empty())
                    .or_else(|| {
                        self.sessions
                            .iter()
                            .find(|s| s.session_id == *id)
                            .map(|s| s.slug.clone())
                            .filter(|s| !s.is_empty())
                    });
                match (slug, row) {
                    (Some(slug), _) => format!("{short} · {slug}"),
                    (None, Some(s)) if !s.role.is_empty() => format!("{short} · {}", s.role),
                    _ => short,
                }
            }
        }
    }

    /// A job's slug, as the head knows it: the daemon's job table first, then a completion
    /// notice on the conversation for a job the table no longer lists. `None` for a job nobody
    /// named and for one the head has never heard of - nothing is invented from the command.
    pub(crate) fn job_slug(&self, handle: &str) -> Option<String> {
        match self.jobs.iter().find(|j| j.id == handle) {
            Some(j) => Some(j.slug.clone()).filter(|s| !s.is_empty()),
            None => self
                .notice_job(handle)
                .and_then(|(_, j)| j.slug)
                .filter(|s| !s.is_empty()),
        }
    }

    /// The newest completion notice on the conversation that names `handle` - its index in
    /// `items`, which is what the notice popup opens, and the job as the notice reported it.
    pub(crate) fn notice_job(
        &self,
        handle: &str,
    ) -> Option<(usize, crate::ui::transcript::job_card::EndedJob)> {
        self.items.iter().enumerate().rev().find_map(|(k, it)| {
            let text = crate::ui::transcript::user::agent_row_text(it)?;
            let jobs = crate::ui::transcript::job_card::notice_jobs(&text)?;
            jobs.into_iter()
                .find(|(_, j)| j.id == handle)
                .map(|(_, j)| (k, j))
        })
    }

    /// Open what a row waits on, which is what a click on its link and `w` on the selected row share: a job opens
    /// its output, the same window Enter on that job in the jobs pane opens, and a child opens its session.
    pub(crate) fn open_wait(&mut self, w: &WaitOn) -> Option<Action> {
        if self.session_id.is_empty() {
            self.say("not attached to a session yet");
            return None;
        }
        match w {
            WaitOn::Job(h) => {
                if let Some(i) = self.jobs.iter().position(|j| j.id == *h) {
                    return self.open_job_out(i);
                }
                if let Some((k, _)) = self.notice_job(h) {
                    self.open_notice_popup(k);
                    return None;
                }
                self.say(&format!(
                    "job {} is not in this session's job list — nothing to open",
                    rano::agent::text::clean_line(h)
                ));
                None
            }
            WaitOn::Child(id) => {
                if self
                    .subagents
                    .iter()
                    .any(|s| s.session_id == *id && s.state == "opening")
                {
                    self.say("that subagent is still opening — nothing to read yet");
                    return None;
                }
                self.ask_peek(id);
                Some(Action::Peek(id.clone()))
            }
        }
    }

    /// A click on link `k` of the last frame: the cursor goes to the row the link is on  -
    /// so the pane and the bar say which row was acted on - and its wait opens.
    pub(crate) fn open_wait_link(&mut self, k: usize) -> Option<Action> {
        let link = self.todos_wait_links.get(k)?.clone();
        // The stops are the add control, then every session row the pane DREW, in its order  -
        // which is what `WaitLink::todo` counts.
        self.todos_sel = 1 + link.todo;
        self.open_wait(&link.target)
    }

    /// What the row under the todos cursor waits on, if it is a session row that waits on a
    /// job or a child - `w`'s question, and the bar's.
    pub(crate) fn selected_wait(&self) -> Option<WaitOn> {
        use letibot_sessionlog::event::TodoBy;
        let stops = self.todos_stops();
        let stop = stops.get(self.todos_sel.min(stops.len().saturating_sub(1)))?;
        let (words, mine) = match stop {
            TodoStop::Mine(w) => (w, true),
            TodoStop::Theirs(w) => (w, false),
            _ => return None,
        };
        let t = self
            .todos
            .iter()
            .find(|t| t.content == *words && (t.by == TodoBy::Operator) == mine)?;
        wait_of(t)
    }
}

/// The row byte cap at the person's own door: the refusal `/todo TEXT`, the new-todo card and the template seed all
/// ask, so one rule is spelled once for the person the way `todo_write` spells it for the model. The number is the
/// wire's copy of the store's.
pub(crate) fn row_refusal(text: &str) -> Option<String> {
    use letibot_sessionlog::event::TODO_ROW_MAX_BYTES;
    let n = text.len();
    (n > TODO_ROW_MAX_BYTES).then(|| {
        format!(
            "that row is {n} bytes, and a board row is at most {TODO_ROW_MAX_BYTES}, so nothing \
             was added"
        )
    })
}

/// One row of the todos pane the cursor may land on - see [`App::todos_stops`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TodoStop {
    /// The `[+] add todo item` control, at the head of the list.
    Add,
    /// One of the person's own rows, by its words: there is no id on the wire, so the words are the identity, which
    /// is the same key `/todo done N` uses. A row renamed is a different row, and the overlay says so.
    Mine(String),
    /// One of the model's rows, by its words, and the rows the operator's report was about: they are stops now
    /// because Enter READS a row and a stop is where a key acts.
    Theirs(String),
    /// A row of the workspace's `TODO.md`, by index into `repo_todos` - the file's own order IS
    /// its identity, because the pane re-reads the file.
    Repo(usize),
}

/// One line of the session's list, in the order the pane draws it: a board row, or the `done (N)` line the
/// finished rows are gathered under. The order is a value because the done rows go to a group of their own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BoardLine {
    /// A board row, by its index in [`App::todos`].
    Row(usize),
    /// The group line, and how many done rows it stands for. It is drawn after the rows still
    /// owed and before the done ones - or, with the group folded, as all that is left of them.
    DoneGroup { done: usize },
}

/// The board as the pane draws it: every row still owed in board order, the `done (N)` line, then the done rows,
/// or nothing of them but the line when folded. Done is `completed` and only `completed`, since a row set aside and
/// one struck off are on the board for not being finished.
pub(crate) fn board_drawn(
    todos: &[letibot_sessionlog::event::TodoEntry],
    folded: bool,
) -> Vec<BoardLine> {
    use letibot_sessionlog::event::TodoStatus;
    let mut out = Vec::with_capacity(todos.len() + 1);
    for (i, t) in todos.iter().enumerate() {
        if t.status != TodoStatus::Completed {
            out.push(BoardLine::Row(i));
        }
    }
    let done: Vec<usize> = (0..todos.len())
        .filter(|i| todos[*i].status == TodoStatus::Completed)
        .collect();
    if done.is_empty() {
        // No done rows, no group line - a line saying `done (0)` would be furniture about
        // nothing, and the board with nothing finished is the ordinary board.
        return out;
    }
    out.push(BoardLine::DoneGroup { done: done.len() });
    if !folded {
        out.extend(done.into_iter().map(BoardLine::Row));
    }
    out
}

/// One line of the repository's list, in the order the pane draws it: the file is grouped per SECTION, and a
/// section's items belong to the heading over them, one step less far than the session's own list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RepoLine {
    /// A `##` heading - the file's outline, drawn where the file has it, and never a stop.
    Heading(usize),
    /// One of the section's items, by its index in [`App::repo_todos`].
    Item(usize),
    /// The `done (N)` line of one section: as far as the grouping goes on this half, because the
    /// file's outline is not this pane's to rewrite. It carries the section's item INDENT so the
    /// line is drawn in the column its items are, whatever the file (or a fixture) chose.
    DoneGroup { done: usize, indent: usize },
}

/// The repository's `TODO.md` as the pane draws it: each section's heading, its items still owed in file order, its
/// `done (N)` line and the items the file marks `[x]`. A heading is drawn in place and is not a group of its own.
pub(crate) fn repo_drawn(rows: &[TodoRow], folded: bool) -> Vec<RepoLine> {
    let mut out = Vec::with_capacity(rows.len() + 1);
    let mut i = 0;
    while i < rows.len() {
        if !rows[i].item {
            out.push(RepoLine::Heading(i));
            i += 1;
        }
        // One section's items, up to the next heading.
        let from = i;
        while i < rows.len() && rows[i].item {
            i += 1;
        }
        let done = |k: usize| rows[k].mark == Some(TodoMark::Done);
        for k in from..i {
            if !done(k) {
                out.push(RepoLine::Item(k));
            }
        }
        let finished: Vec<usize> = (from..i).filter(|k| done(*k)).collect();
        if finished.is_empty() {
            continue;
        }
        out.push(RepoLine::DoneGroup {
            done: finished.len(),
            // A section with a done item has items, so the first one's indent is the column the
            // group line belongs in.
            indent: rows[from].indent,
        });
        if !folded {
            out.extend(finished.into_iter().map(RepoLine::Item));
        }
    }
    out
}

/// A todo being filed from the card: its fields, and which one owns the composer. The focus was a `bool` while
/// there were two fields, and a third is what a bool cannot hold.
pub(crate) struct TodoDraft {
    pub(crate) title: String,
    pub(crate) detail: String,
    /// The handle this row waits on, or empty for a row that waits on nothing. A bare handle:
    /// the *condition* is what the row holds, and the card does not collect a kind because there is
    /// one kind - see `TodoCondition`.
    pub(crate) when: String,
    pub(crate) focus: TodoField,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TodoField {
    Title,
    Detail,
    When,
}

impl TodoDraft {
    pub(crate) fn new() -> TodoDraft {
        TodoDraft {
            title: String::new(),
            detail: String::new(),
            when: String::new(),
            focus: TodoField::Title,
        }
    }

    /// What one field holds, with the composer's live text standing in for the focused one.
    pub(crate) fn shown(&self, live: &str, which: TodoField) -> String {
        if self.focus == which {
            return live.to_string();
        }
        match which {
            TodoField::Title => self.title.clone(),
            TodoField::Detail => self.detail.clone(),
            TodoField::When => self.when.clone(),
        }
    }

    /// Commit the composer into the field it belongs to - every key that leaves a field does
    /// this first, so a Tab cannot lose what was just typed.
    pub(crate) fn take(&mut self, live: &str) {
        let into = match self.focus {
            TodoField::Title => &mut self.title,
            TodoField::Detail => &mut self.detail,
            TodoField::When => &mut self.when,
        };
        *into = live.to_string();
    }

    /// The field Tab goes to next, wrapping - a cycle, so there is no field a reader cannot reach.
    pub(crate) fn next(&self) -> TodoField {
        match self.focus {
            TodoField::Title => TodoField::Detail,
            TodoField::Detail => TodoField::When,
            TodoField::When => TodoField::Title,
        }
    }
}

/// A workspace's key in the seeded-projects record: its FNV-1a hash, in `note_key`'s shape, since the record is a
/// comma list in `head.toml` and a path may contain a comma.
pub(crate) fn todo_seed_key(ws: &str) -> String {
    format!("{:016x}", fnv1a(ws))
}
