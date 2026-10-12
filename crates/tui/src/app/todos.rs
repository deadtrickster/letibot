//! **The todos' state**: the operator's list, the todo card's draft, the repository's queue,
//! and the stops a click lands on.

use super::*;

/// **The id a todo row's question card carries** — `todo-ask:<the row's words>`. Not a daemon
/// `req_id` (those are minted by the gate), so it cannot collide with one, and
/// [`App::settle_question`] reads the row back out of it.
pub(crate) const TODO_ASK: &str = "todo-ask:";

/// **One row's question for the operator**, as this head's board holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OperatorAsk {
    pub(crate) content: String,
    pub(crate) question: String,
    pub(crate) options: Vec<String>,
}

/// **A question card the operator put away with Esc** — still pending, nothing sent.
///
/// The operator, 2026-10-12: *"i want to be able to escape the selection card. sometimes i need
/// time to think and just let the card stay"*. So Esc is not an answer of any kind — not an
/// abstention, not a denial, nothing on the wire — it takes the card off the screen and keeps
/// everything that was on it: the decision itself (`decision`), the words typed under it
/// (`draft`, the composer's text, which is the card's field while the card is up) and the row the
/// highlight was on (`sel`). alt-a puts it back exactly so ([`App::bring_back_question`]).
///
/// One shape for both kinds of question card. A model's `ask_user_question` card is the daemon's
/// open decision and keeps waiting under its own deadline — the resident line counts it down
/// ([`App::waiting_line`]); a todo row's card (`req_id` starting [`TODO_ASK`]) is this head's own
/// and has no clock. `session_id` is the session it was asked in, so a switch never brings a
/// card back over the wrong conversation.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AwayCard {
    pub(crate) session_id: String,
    pub(crate) decision: letibot_sessionlog::view::OpenDecision,
    pub(crate) draft: String,
    pub(crate) sel: usize,
}

/// **A card whose answer has left and is not settled yet** (R90.29) — see [`App::answering`].
#[derive(Debug, Clone)]
pub(crate) struct Answering {
    pub(crate) decision: letibot_sessionlog::view::OpenDecision,
    /// The row that was marked, so a refused answer puts the card back as it was.
    pub(crate) sel: usize,
    /// The frame's `client_request_id`, once the driver has sent it — what a `Rejected` names.
    pub(crate) request: Option<String>,
}

impl AwayCard {
    /// Whether this is a todo row's card rather than the model's question.
    pub(crate) fn is_todo(&self) -> bool {
        self.decision.req_id.starts_with(TODO_ASK)
    }
}

impl App {
    /// **The operator's rows, out of the union the daemon serves.** The head keeps no second
    /// list: the daemon persists them, every head sees them, and this is the one place that says
    /// which half of that list the operator wrote. Sending is the whole list, so a divergence
    /// between a head's copy and the store is impossible to accumulate.
    pub(crate) fn operator_todos(&self) -> Vec<letibot_sessionlog::event::TodoEntry> {
        self.todos
            .iter()
            .filter(|t| t.by == letibot_sessionlog::event::TodoBy::Operator)
            .cloned()
            .collect()
    }

    /// **The rows on this head's board that wait on the operator**, with the question and the
    /// options — open rows only (a done or struck-off row asks nobody anything), in board order.
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

    /// **Open the question card for a row that waits on the operator** — `content` names the row
    /// (Enter on it in the todo pane), `None` takes the first one (alt-a, from the resident line).
    ///
    /// **The card is the question card `ask_user_question` draws**, not a second one: a
    /// `question` decision with the row's options as its choices, put on this head's own list of
    /// open cards. Every way of answering it is the card's — arrows and Enter for an option, `N --
    /// note` for an option with a note, typed words, `abstain` — and [`App::settle_question`]
    /// sends the answer to the ROW (`Action::AnswerTodo`) because the card's id says it is one.
    /// It is this head's alone: no decision is open on the daemon, so a resync simply takes it
    /// down and alt-a puts it back.
    pub(crate) fn open_operator_ask(&mut self, content: Option<&str>) {
        let asks = self.operator_asks();
        let ask = match content {
            Some(c) => asks.into_iter().find(|a| a.content == c),
            None => asks.into_iter().next(),
        };
        let Some(ask) = ask else {
            self.say(if self.asks_setting.is_some() {
                "the board is still being read — alt-a again in a moment"
            } else {
                "nothing is waiting on you"
            });
            if self.asks_setting.is_some() && !self.queued.contains(&Action::ListTodos) {
                self.queued.push(Action::ListTodos);
            }
            self.redraw = true;
            return;
        };
        let req_id = format!("{TODO_ASK}{}", ask.content);
        if let Some(first) = self.open.first()
            && first.req_id != req_id
            && !first.req_id.starts_with(TODO_ASK)
        {
            self.say("answer the open card first — then alt-a opens the question waiting on you");
            self.redraw = true;
            return;
        }
        if self.open.first().is_some_and(|d| d.req_id == req_id) {
            // Already the card on the screen: nothing to open, and its draft is the composer.
            self.redraw = true;
            return;
        }
        // **What was put away for this row comes back with it** — the words and the highlight —
        // provided it is still the same question (a row that asks something new starts clean).
        // Looked up after the front card is stashed, which may itself be this row's.
        // Another row's card on the screen is put away, not thrown away: its words go with it.
        if self
            .open
            .first()
            .is_some_and(|d| d.req_id.starts_with(TODO_ASK))
        {
            self.stash_front_card();
        }
        // **The operator's own line is held, never the card's answer** (R90.3).
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
                choices: ask.options.clone(),
                because: String::new(),
                advice: None,
                deadline: None,
                on_timeout: letibot_sessionlog::event::OnTimeout::Ask,
                asked_ts: 0,
            },
        );
        self.redraw = true;
    }

    /// **A question card's answer, as the action that carries it** — to the ROW for a card
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
            self.redraw = true;
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

    /// **After a todo answer, the next question — or the word that there is none.**
    ///
    /// The operator answered one of three waiting rows and the resident line simply moved to the
    /// next question: *"i answered what was asked by the todo, and it didnt hide"*. Two fixes in
    /// one place. The card goes straight on to the NEXT row still waiting on the operator in this
    /// session — the answered one is left out at once ([`App::todo_answered`]), not when the
    /// board comes back — and closes when none remain; and a short line says which of the two
    /// happened, so moving on never reads as the answer being ignored:
    ///
    /// ```text
    /// answered — 2 more waiting on you
    /// answered — nothing else is waiting on you
    /// ```
    ///
    /// Another card on the screen (the model's question, a permission) is left alone: the next
    /// row waits for alt-a rather than jumping in front of it.
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

    /// **Esc on a question card: put it away, answer nothing.**
    ///
    /// The operator: *"i want to be able to escape the selection card. sometimes i need time to
    /// think and just let the card stay"*. The card leaves the screen and the question stays
    /// PENDING — nothing is sent: not an abstention (that is the typed `abstain`, a choice), not
    /// a denial. The model's call keeps waiting under its own deadline, a todo row keeps waiting
    /// with no clock, and the resident line says so with the key that brings it back.
    ///
    /// **The words go with the card.** While a question is up the composer is its field, so
    /// what is typed there is the card's draft: it is taken off the composer (which would
    /// otherwise send it to the model as a prompt) and put back by alt-a with the highlight.
    ///
    /// **A permission card is put away the same way** (R90.5): Esc used to fall through it — it
    /// closed a pane behind the card, or reached the editor and armed the Esc-Esc interrupt, so a
    /// second Esc interrupted the very turn waiting on the card. One meaning for Esc on every
    /// decision card: step away, answer nothing, alt-a brings it back.
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
        self.redraw = true;
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
        self.redraw = true;
    }

    /// **A resync or a switch replaces the open list with the daemon's** — and this head's own
    /// todo cards are not on it. They are put away (with the front card's words) rather than
    /// dropped (R90.14): dropped, the card vanished and the composer kept the words typed for it,
    /// so the next Enter sent a half-typed answer to the model as a prompt.
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

    /// **The board moved: a todo card whose row no longer asks is closed** (R90.12), and one put
    /// away is forgotten. Another head answered the row, the model completed it or changed the
    /// question — and the card stayed up, so answering it sent `AnswerTodo` for a question
    /// nobody was asking. A row still asking the same question keeps its card, its options
    /// refreshed. Run on every board this head reads for its own session.
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
            self.redraw = true;
        }
        if let Some(d) = self.open.first() {
            self.sel = self.sel.min(decision_rows(d).saturating_sub(1));
        }
    }

    /// **The front card changed, or did not** — run after every key and every frame (R90.3,
    /// R90.4).
    ///
    /// * The highlight is reset exactly when `open[0]` is a different card from the one it was
    ///   last reset for. A decision arriving BEHIND the card on screen leaves it alone (↓↓ to
    ///   deny, a second permission arrives, Enter still denies), and the next card up starts at
    ///   its top rather than at the row the last one was answered on.
    /// * A question that comes to the front over composer text does not take that text as its
    ///   answer: the words are held ([`App::held_line`]) and the card gets an empty field — the
    ///   permission path's rule, *"your line is held"*. They come back to the composer as soon as
    ///   no question is in front.
    ///
    /// The openers that seed a card themselves (alt-a's [`App::bring_back_question`] and
    /// [`App::open_operator_ask`]) hold the line and claim `sel_for` before they put the card's
    /// own draft into the composer, so the draft is not mistaken for the operator's line here.
    /// **An answer to the model's question or a permission is leaving: its card closes now**
    /// (R90.29). It used to stay up until `DecisionAnswered` came back, and a second Enter in
    /// that gap sent the same answer again; a todo row's card already closed at once. The card
    /// is kept in [`App::answering`] so a refusal can put it back.
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
        self.redraw = true;
    }

    /// **The driver sent it**, under this `client_request_id` — what a refusal will name.
    pub fn answer_sent(&mut self, req_id: &str, request: String) {
        if let Some(a) = self
            .answering
            .iter_mut()
            .find(|a| a.decision.req_id == req_id)
        {
            a.request = Some(request);
        }
    }

    /// **The daemon refused an answer this head sent** — its card goes back in front, the
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
        self.redraw = true;
        true
    }

    pub(crate) fn sync_front(&mut self) {
        let front = self
            .open
            .first()
            .map(|d| d.req_id.clone())
            .unwrap_or_default();
        let question = self.open.first().is_some_and(|d| d.kind == "question");
        if front != self.sel_for {
            self.sel_for = front.clone();
            self.sel = 0;
            self.redraw = true;
            // **A decision coming up closes the quit card** (R90.6): the decision is drawn ahead
            // of it and takes its keys, so a `1` meant as "detach" would answer the decision.
            if !front.is_empty() && self.quit_card {
                self.quit_card = false;
                self.say("a decision came up — the quit card closed; ctrl-c twice asks again");
            }
            // **A decision coming up takes the keyboard back from the editor pane** (R90.7):
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
            // **A draft card in front keeps the keys, and says what waits behind it** (R90.16).
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
        if !question && let Some(held) = self.held_line.take() {
            let now = self.editor.expanded();
            self.clear_composer();
            if now.trim().is_empty() {
                self.editor.insert(&held);
            } else {
                self.editor.insert(&format!("{held}\n{now}"));
            }
            self.redraw = true;
        }
    }

    /// [`App::hold_line`], saying so when there was a line to hold.
    fn hold_typed_line(&mut self) {
        if !self.editor.text().is_empty() {
            self.hold_line();
            self.say("your line is held — it comes back when the card is answered or put away");
        }
    }

    /// Take the composer's words aside into [`App::held_line`] and empty it (R90.3).
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

    /// Empty the composer, every line of it — ctrl-c on a non-empty composer is the editor's own
    /// "clear what I typed", and an undo away; Home + kill-to-end would clear one line of several.
    pub(crate) fn clear_composer(&mut self) {
        if !self.editor.text().is_empty() {
            self.editor.key(letibot_ui::editor::Key::CtrlC, self.now_ms);
        }
    }

    /// **alt-a: bring back the oldest pending question of either kind.**
    ///
    /// One key for both kinds because the resident line names one key, and the operator should
    /// not have to know whose question it is to get it back. *Oldest* is read as the order the
    /// two can actually be compared in: a model's question put away comes first — it is the one
    /// with a clock, holding a running call — oldest put away first; then a todo row whose card
    /// was put away (its draft comes back with it); then the first row waiting on the operator,
    /// in board order, which is what alt-a always opened. Todo rows carry no time of asking, so
    /// board order is the only order they have.
    pub(crate) fn bring_back_question(&mut self) {
        // **The prompt card put away comes back first** (R90.5): it is the operator's own
        // command blocked on a line, and alt-a was no way back to it at all — only `!send`.
        if self.prompt.is_some() && self.prompt_away {
            self.prompt_away = false;
            self.prompt_buf.clear();
            self.redraw = true;
            return;
        }
        if let Some(d) = self.open.first()
            && d.kind == "question"
            && !d.req_id.starts_with(TODO_ASK)
        {
            // About THIS card, and what alt-a will do once it is out of the way (R90.26) — the
            // operator pressing alt-a wanted the other question.
            self.say(
                "a question is the open card — answer it, or esc puts it away; then alt-a opens \
                 what else waits on you",
            );
            self.redraw = true;
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
                self.redraw = true;
                return;
            }
            if !self.open.is_empty() {
                self.stash_front_card();
            }
            // **The operator's own line is held, never the card's answer** (R90.3).
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
            self.redraw = true;
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

    /// **Open the new-todo card**, and put the composer where the card expects it.
    ///
    /// **The composer is the field**, so it is emptied and handed to the card: a half-typed prompt
    /// left under a card whose Enter adds an item is the shape that costs somebody a message, which
    /// is the same reason `mode_confirm` takes the keyboard. leticl's `%todo-draft-open`.
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
        self.redraw = true;
    }

    /// **`/todo …` — the operator's own rows.** The verb's forms, and each one sends the
    /// whole list:
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
    /// **A number is numbered over the operator's rows and not the union**, because the model's
    /// rows are not the operator's to edit — that is the same rule `/rename` and `/compact` keep
    /// about acting on the session you are in. The count is the one the pane prints for that half.
    ///
    /// **`postpone`/`resume` also take a row's own WORDS, and that is the reach.** The pane numbers
    /// the operator's rows and only those, so a row the MODEL wrote had no name this verb could use
    /// — and no other door either: `todo_write` refuses `postponed` (a model that could silence its
    /// own plan could abandon it). MEASURED: fourteen turns spent restating one true sentence about
    /// a row that was correct and waiting on a person. The words are the board's one key
    /// (`set_operator_states` resolves the operator's rows by their exact trimmed text), so a
    /// non-numeric argument names any row on the board: the operator's own half is moved in the
    /// list this head owns, and a MODEL's row goes out as a state move beside it — one frame, one
    /// act. A parent's row is refused by name, because a parent owns its own rows' states.
    ///
    /// **Marked complete rather than deleted** by `done`, and **struck off rather than deleted** by
    /// `rm`: the operator's ruling is *"only i should be able to delete todo items. as a rule
    /// everything that ever created stays in history"*, so neither verb removes a row. `done` is a
    /// record of work finished, `rm` is a record of work decided against — the row keeps its words,
    /// its author and its place on the board either way, and `/todo resume N` lifts either. The
    /// model may move a row's STATUS (by quoting its words) and may not remove one, which is the
    /// operator's ruling recorded on `TodoBoard`.
    /// **THE OPERATOR'S OWN ROWS, DRAWN FROM THE MOMENT THEY ARE SENT** — their half of what
    /// `pending_prompts` does for their words.
    ///
    /// The operator: *"when i send todos they appear in the todo pane some time later — it feels
    /// like their appearance depend on the turn state. but to me - im not sure if i lost them or
    /// not."* They did not lose them: the write publishes at once (`set_operator_todos` has a
    /// retry for exactly that), but the COMMAND reaches the worker behind whatever is running
    /// and is injected at the next step boundary (`harness.rs`: *"the step boundary is the only
    /// place any of the three can be injected"*), and until `TodosUpdated` arrives the pane drew
    /// only what the daemon had published — nothing, for the whole of a running turn.
    ///
    /// So the head applies the list it is about to send, the same trust `pending_prompts` places:
    /// show what I sent until the daemon answers. `mine` is the operator's whole half (the
    /// command replaces it), so the optimistic state is the model's rows as they stand with this
    /// half in their place — and `TodosUpdated` replaces the union wholesale when it lands, in
    /// the daemon's own order, which is why nothing here needs to guess at that order for longer
    /// than the boundary.
    pub(crate) fn echo_operator_todos(&mut self, mine: Vec<letibot_sessionlog::event::TodoEntry>) {
        self.todos
            .retain(|t| t.by != letibot_sessionlog::event::TodoBy::Operator);
        self.todos.extend(mine);
        self.redraw = true;
    }

    /// **Is a starter-todo seed due for THIS project?** The three-part gate, in one place
    /// because the attach and the seed itself both ask it — a second spelling is how a project
    /// gets seeded by one reading and skipped by the other.
    ///
    /// The gate is leticl's `%seed-operator-todos` verbatim: the switch is on, the workspace is
    /// known (a project it cannot name is not a project it may put rows into — a head loads
    /// before the socket exists), and the record does not hold this workspace. **The record, not
    /// the list**: a starter row the operator deleted must not come back, which is what an *is
    /// the list empty* test would do on every restart.
    pub(crate) fn todo_seed_due(&self) -> bool {
        self.todo_template != crate::prefs::TodoTemplate::Off
            && !self.wiring.workspace.is_empty()
            && !self
                .todo_seed
                .contains(&todo_seed_key(&self.wiring.workspace))
    }

    /// Where the starter todos come from: `todo_template`'s three shapes as a path — leticl's
    /// `todo-template-path`, which its own docstring rules: *“one function because the setting
    /// has three shapes and two callers must not spell them differently.”* `None` when the
    /// switch is off or this head has no config directory to read a default from.
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

    /// **Copy the template TODO.md's items onto the operator's half of the board** — leticl's
    /// `%seed-operator-todos`, run where leticl runs it: the moment the head has learned its
    /// list. The behaviour copied, piece by piece:
    ///
    /// * **The template is a `TODO.md`, parsed by the same reader the repo section uses**
    ///   (`render_todo_md`) — a starter list is written in the format the operator already
    ///   writes by hand, boxes and indented bodies included, and there is no second syntax
    ///   to learn.
    /// * **ITEMS ONLY are copied**: the checkbox rows, not the headings and their roll-ups.
    ///   What is copied is what leticl copies — the text, the body, and the mark: `[x]`
    ///   seeds a completed row (how a template carries something already settled), anything
    ///   else seeds an open one.
    ///
    ///   The one place this cannot be leticl: **the body rides in `content`, joined `“ · ”`**.
    ///   leticl keeps a `:detail` beside its rows in its own sqlite; this head keeps no second
    ///   list, and the wire's `TodoEntry` is `content`/`status`/`by` — leticl's own push drops
    ///   the detail at the same door. The card already made this head's choice for a typed
    ///   detail (`title “ — ” detail`); body LINES join with `·` so each continuation stays a
    ///   segment rather than merging into one sentence.
    ///
    /// * **ONCE PER PROJECT, marked even when nothing was added** — an empty template records
    ///   the seeding and says so, because the alternative re-reads it on every start. A
    ///   MISSING file is not marked: leticl says why and leaves the project unseeded, so the
    ///   file the operator was going to write still gets its chance.
    /// * **Refusals are notes, not silences** — from the operator's side *the feature did not
    ///   work* and *I never turned it on* look identical otherwise.
    ///
    /// The rows go out through the same door `/todo TEXT` uses — `echo_operator_todos` for the
    /// optimistic view, `SetOperatorTodos` for the whole half — so a seed and a typed row cannot
    /// become different acts.
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
                // Not marked — see the docstring: the file may still be written.
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
        // **A template item over the title cap is NOT seeded, and it is said** — the seed's own rule
        // (*"refusals are notes, not silences"*), because from the operator's side a row that
        // silently did not arrive is indistinguishable from a file that was never read. The title is
        // what is measured (`title_refusal`), and the body joined after ` — ` below is the DETAIL.
        let mut skipped: Vec<(String, String)> = Vec::new();
        for r in &items {
            let mut content = r.text.trim().to_string();
            if !r.body.is_empty() {
                content.push_str(" — ");
                content.push_str(&r.body.join(" · "));
            }
            if let Some(why) = title_refusal(&content) {
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
                // **A template's items are rows, not a graph**: the file is a checklist and has
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
        // **The count, and then every item that was too long to be a title, by name** — one notice,
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

    /// **Record that this project has had its starter todos** — and do it BEFORE the send, for
    /// leticl's own reason: a record that waits for an acknowledgement re-fires on the next
    /// start if the write failed quietly, and the operator gets the duplicates this feature
    /// exists to avoid. The write is a UNION with whatever the file holds
    /// (`merge_todo_seed`) because two heads share one `head.toml` and a dropped record is a
    /// project that re-seeds.
    pub(crate) fn mark_seeded(&mut self) {
        let key = todo_seed_key(&self.wiring.workspace);
        if !self.todo_seed.contains(&key) {
            self.todo_seed.push(key);
        }
        if let Some(path) = self.prefs_path.clone() {
            let mut p = self.prefs();
            p.todo_seed = crate::prefs::merge_todo_seed(&path, &self.todo_seed);
            self.todo_seed = p.todo_seed.clone();
            if let Err(e) = crate::prefs::save(&path, &p) {
                self.say(&format!("seed not recorded: {e}"));
            }
        }
    }

    /// **EVERY ROW OF THE TODOS PANE THE CURSOR MAY LAND ON, in the order the pane draws them** —
    /// leticl's `todos-stops`, and the ONE enumeration everything about the cursor reads.
    ///
    /// Its docstring is the operator's two reports, and both were the same defect: R44's first cut
    /// spread this over a `-1` sentinel and the repo's own stop indices, and then *"arrows dont go
    /// here"* — the cursor moving to a row whose line the pane computed from another list's
    /// arithmetic — and *"mouse doesnt click"* — a click on the add row computing a negative index,
    /// thrown away. **Two enumerations was the defect.**
    ///
    /// Four kinds, and the tag carries IDENTITY rather than position: a position is a fact about
    /// the list when it was DRAWN, and a list changes between a draw and a keypress (a `TodosUpdated`
    /// arriving, a row removed), so every action would land on whatever took its neighbour's place.
    ///
    /// **The model's rows ARE stops, and Enter on one READS it.**
    ///
    /// They were not, and the reason was sound while it held: no key acted on one — the model
    /// moves its own row's status and the operator may not — and a cursor that stops where no key
    /// acts is a cursor the operator presses keys into and nothing happens. **Enter now acts on
    /// every row: it opens the row's whole text** ([`App::todos_open`]), which is the operator's
    /// own report against this pane — *"in todo list i cant expand your todo items - only
    /// truncated titles available"*, about the model's rows, which are the long ones. So the rule
    /// is kept rather than bent: a stop is where a key acts, and reading is an act. **Reading is
    /// not moving** — nothing here changes a row's state, which is the R44 boundary that still
    /// holds.
    ///
    /// The repo's headings are still skipped, and that is not the same rule broken: the rule is
    /// *a stop must have a key*, not *every key must have a row*. A heading is a section label
    /// rather than a row with words of its own, and a reader who wants the section has the items
    /// under it.
    pub(crate) fn todos_stops(&self) -> Vec<TodoStop> {
        let mut out = vec![TodoStop::Add];
        // **In the order the pane DRAWS them**, which is the daemon's own order over the whole
        // union — one walk, so the cursor's sequence and the rows on the screen cannot come from
        // two readings of the list.
        for t in &self.todos {
            out.push(if t.by == letibot_sessionlog::event::TodoBy::Operator {
                TodoStop::Mine(t.content.clone())
            } else {
                TodoStop::Theirs(t.content.clone())
            });
        }
        if let Some(rows) = &self.repo_todos {
            for (i, r) in rows.iter().enumerate() {
                if r.item {
                    out.push(TodoStop::Repo(i));
                }
            }
        }
        out
    }

    /// **The pane row the stop at the cursor was DRAWN on**, read out of
    /// [`App::todos_stop_rows`] — the record the pane wrote while drawing, and not arithmetic over
    /// the lists it drew from. leticl's `todos-lines` second value, an `aref` of the third.
    ///
    /// The clamp is the one thing here that is not a read: a list can change under the cursor — a
    /// `TodosUpdated` arriving, a row removed, another workspace — and a key pressed against a
    /// shorter list must land on a row rather than on an index that no longer exists.
    pub(crate) fn todos_row_of(&self) -> usize {
        let at = self
            .todos_sel
            .min(self.todos_stop_rows.len().saturating_sub(1));
        self.todos_stop_rows.get(at).copied().unwrap_or(0)
    }

    /// **The repo cursor follows the stop cursor**, for the repo's own keys — the unfold and the
    /// body it shows read `repo_sel`, and a second cursor that did not follow would be the two
    /// enumerations this whole change removed.
    pub(crate) fn sync_repo_from_stop(&mut self, stops: &[TodoStop]) {
        let at = self.todos_sel.min(stops.len().saturating_sub(1));
        if let Some(TodoStop::Repo(i)) = stops.get(at) {
            self.repo_sel = *i;
        }
    }

    /// Re-read `TODO.md` when it has changed since the last read, and not
    /// otherwise. Called on open and before every draw of the pane.
    ///
    /// A failed `stat` — the file was deleted, or was never there — reads again,
    /// so the pane's own "no TODO.md" line is the answer and stays current if one
    /// appears. That costs a failed `open` per draw in the case where there is
    /// nothing to show, which is the case nobody is watching.
    pub(crate) fn refresh_repo_todos(&mut self) {
        // **Resolved, like the read it guards** — the mtime watch and the parse must look at the
        // same file, or a nested layout re-reads on every draw (the watch misses, so `now` is
        // None, so the cache never holds).
        let path = crate::gitfield::project_dir(&self.wiring.workspace).join("TODO.md");
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

/// **What a todo row waits on that this head can open** — a job, or a child session. The
/// operator: *"\"waits on j97\" - make j97 a link to the real job, also render job slug"*.
///
/// The row's `when` job condition when it has one, else its first `needs` edge on a job or a
/// child — the same choice the pane's `waits on …` has always drawn, so what is drawn and what
/// a click opens are one reading of the row. A row waiting on the operator is not one of these:
/// its act is the question card, which Enter already opens.
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

/// **One drawn `waits on …` link**: the pane row it is on, the columns its words cover
/// (`from..to`, the pane's own columns, only the part that was not cut off), the row it belongs
/// to (an index into `App::todos`), and what it opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WaitLink {
    pub(crate) row: usize,
    pub(crate) from: usize,
    pub(crate) to: usize,
    pub(crate) todo: usize,
    pub(crate) target: WaitOn,
}

impl App {
    /// **The words a wait is drawn as** — `j97 · pg-timing` for a job the head knows a slug for,
    /// the bare handle otherwise; a child as the subagents pane names it (its short id, and its
    /// role when the head knows one) rather than the whole `s-…-sub-…` id, which the row's
    /// detail view still carries.
    pub(crate) fn wait_label(&self, w: &WaitOn) -> String {
        match w {
            WaitOn::Job(h) => match self.job_slug(h) {
                Some(slug) => format!("{h} · {slug}"),
                None => h.clone(),
            },
            // **A child by its name when it has one** — `…14688401 · fix-login`, the handle first
            // as a job's is (`j97 · pg-timing`) — and by its role when it has none, as before.
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

    /// **A job's slug, as the head knows it**: the daemon's job table first, then a completion
    /// notice on the conversation for a job the table no longer lists. `None` for a job nobody
    /// named and for one the head has never heard of — nothing is invented from the command.
    pub(crate) fn job_slug(&self, handle: &str) -> Option<String> {
        match self.jobs.iter().find(|j| j.id == handle) {
            Some(j) => Some(j.slug.clone()).filter(|s| !s.is_empty()),
            None => self
                .notice_job(handle)
                .and_then(|(_, j)| j.slug)
                .filter(|s| !s.is_empty()),
        }
    }

    /// **The newest completion notice on the conversation that names `handle`** — its index in
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

    /// **Open what a row waits on** — the act a click on its `waits on …` link and `w` on the
    /// selected row share.
    ///
    /// * **A job opens its output**, the view Enter on that job in the jobs pane opens — the
    ///   window over the job's own bytes, running or ended, with Esc back to the todos.
    /// * **A job the daemon's table does not list** opens its completion notice whole when the
    ///   conversation has one (the popup a click on the notice opens), and otherwise says so and
    ///   opens nothing: a handle is matched exactly, so no other job can be opened in its place.
    /// * **A child is read without leaving** — the subagents pane's `p`, the peek — since a link in
    ///   a list of rows is for looking, and going into the child is the subagents pane's Enter.
    ///   The id is the daemon's to resolve; one it does not hold is refused by name, as `/peek`
    ///   is.
    pub(crate) fn open_wait(&mut self, w: &WaitOn) -> Option<Action> {
        self.redraw = true;
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
                self.ask_peek(&id);
                Some(Action::Peek(id.clone()))
            }
        }
    }

    /// **A click on link `k` of the last frame**: the cursor goes to the row the link is on —
    /// so the pane and the bar say which row was acted on — and its wait opens.
    pub(crate) fn open_wait_link(&mut self, k: usize) -> Option<Action> {
        let link = self.todos_wait_links.get(k)?.clone();
        // The stops are the add control, then every session row in the list's order.
        self.todos_sel = 1 + link.todo;
        self.open_wait(&link.target)
    }

    /// **What the row under the todos cursor waits on**, if it is a session row that waits on a
    /// job or a child — `w`'s question, and the bar's.
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

/// **The title cap, at the OPERATOR's own door** — the refusal `/todo TEXT`, the new-todo card and
/// the template seed all ask, so one rule is spelled once for the person the same way
/// `todo_write` spells it for the model.
///
/// The rule and the number are `letibot_tokencore::store::TODO_TITLE_MAX`'s, and the head reads them
/// from the WIRE's copy of them (`letibot_sessionlog::event`) because that is where every other
/// todo fact the head holds comes from — `TodoEntry`, `TodoStatus`, `TodoBy` — and because the head
/// does not link the store: the crate is a dev-dependency here (the apparatus machinery), and the
/// TUI binary has no business pulling rusqlite in for a constant. The wire's copy is pinned to the
/// store's by a test in that crate, which does see both. The arithmetic: the operator's terminal is
/// 160 cells, the todos pane draws a row at 156, and a row's own furniture — the mark, its `/todo`
/// number, `— model` — takes 21, so a longer title is CUT at the pane's right edge and read by
/// nobody. What is capped is the TITLE, so the part after ` — ` is the row's detail and is not
/// capped — which is the shape the card's own Detail field already writes.
///
/// **It is asked where a row is CREATED and not where the half is sent.** `SetOperatorTodos`
/// replaces the operator's half wholesale, so the rows that come back with it include every row
/// written before this cap — a check at the door would refuse the operator's own `/todo done 2`
/// because an old row is long, and would make a row older than the cap impossible to close. The
/// head is the writer of the operator's words, and this is the moment they are written.
pub(crate) fn title_refusal(text: &str) -> Option<String> {
    use letibot_sessionlog::event::{TODO_TITLE_MAX, todo_title};
    let title = todo_title(text);
    let n = title.chars().count();
    (n > TODO_TITLE_MAX).then(|| {
        format!(
            "that title is {n} characters, and a title is at most {TODO_TITLE_MAX} — the width the \
             todos pane draws a row at. The rest is the DETAIL: put it after ` — ` and the pane \
             shows it after the title"
        )
    })
}

/// **One row of the todos pane the cursor may land on** — see [`App::todos_stops`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TodoStop {
    /// The `[+] add todo item` control, at the head of the list.
    Add,
    /// **One of the operator's own rows, BY ITS WORDS.** There is no id on the wire —
    /// `TodoEntry` is `content`, `status`, `by`, and the operator has ruled out a bump for one — so
    /// the words are the identity, which is the same key `/todo done N` uses. A row renamed is a
    /// different row, and that is the honest reading of a list with no ids.
    Mine(String),
    /// **One of the MODEL's rows — `Mine`'s other half, by its words**, and the rows the operator's
    /// report was about: *"in todo list i cant expand your todo items"*. They are stops now because
    /// Enter READS a row ([`App::todos_open`]) and a stop is where a key acts; nothing here moves
    /// one, which is the boundary that used to be the reason they were skipped (see
    /// [`App::todos_stops`]).
    ///
    /// It carries the words rather than the author, and that is the board's own rule: a row's
    /// identity is its exact text, and the lookup that draws the overlay takes the first row with
    /// those words that is not the operator's — which is the same row, because the stops are built
    /// in the list's own order. A `Parent <id>` or a `queue` row is one of these too: none of the
    /// three is the operator's to move.
    Theirs(String),
    /// A row of the workspace's `TODO.md`, by index into `repo_todos` — the file's own order IS
    /// its identity, because the pane re-reads the file.
    Repo(usize),
}

/// **A todo being filed from the card** — its fields, and which one owns the composer.
///
/// The FOCUS was a `bool` while there were two fields, and a third is exactly what a bool cannot
/// hold: *typing the detail?* answers nothing about a `when` field, so every reader of that flag
/// would have grown a second one and the two could disagree. One enum, and Tab cycles it.
///
/// **The composer holds the focused field and the draft holds the rest**, which is what keeps the
/// row being typed from being a keystroke behind — leticl's `%todo-draft-focus`, and the reason
/// [`TodoDraft::take`] exists: every key that LEAVES a field commits the composer into it first, so
/// nothing typed is ever lost to a Tab.
pub(crate) struct TodoDraft {
    pub(crate) title: String,
    pub(crate) detail: String,
    /// **The handle this row waits on, or empty for a row that waits on nothing.** A bare handle:
    /// the *condition* is what the row holds, and the card does not collect a kind because there is
    /// one kind — see `TodoCondition`.
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

    /// **What one field holds**, with the composer's live text standing in for the focused one.
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

    /// **Commit the composer into the field it belongs to** — every key that leaves a field does
    /// this first, so a Tab cannot lose what was just typed.
    pub(crate) fn take(&mut self, live: &str) {
        let into = match self.focus {
            TodoField::Title => &mut self.title,
            TodoField::Detail => &mut self.detail,
            TodoField::When => &mut self.when,
        };
        *into = live.to_string();
    }

    /// The field Tab goes to next, wrapping — a cycle, so there is no field a reader cannot reach.
    pub(crate) fn next(&self) -> TodoField {
        match self.focus {
            TodoField::Title => TodoField::Detail,
            TodoField::Detail => TodoField::When,
            TodoField::When => TodoField::Title,
        }
    }
}

/// **A workspace's key in the seeded-projects record** — its FNV-1a hash, in `note_key`'s
/// shape: the record lives in `head.toml` as a comma list, so a key that contains a comma or
/// whitespace (as a path can) would corrupt the list, and hashing is the same answer
/// `note_key` gives for the same reason.
pub(crate) fn todo_seed_key(ws: &str) -> String {
    format!("{:016x}", fnv1a(ws))
}
