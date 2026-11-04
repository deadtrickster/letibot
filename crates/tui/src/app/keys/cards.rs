//! Keys a card owns: while a card is up - a confirmation, a draft, a secret, an ask, a decision's
//! ladder, the quit card, a setting's values - its keys are its own.

use super::*;
use std::ops::ControlFlow;

/// How long after the `allow-all` card appears its answer keys take nothing. A card a frame
/// opens sits under keys already on their way, and the arming window is the guard that catches
/// them: the hold and the composer guard cannot, being read only after a key has landed.
pub(crate) const MODE_ARM_MS: u64 = 500;

impl App {
    /// The `allow-all` confirmation owns the keyboard, as the password field does: a question this
    /// consequential must not be answered by a key aimed at the composer. `y` and Enter both
    /// confirm, everything else is a no, and the pointer answers nothing - a wheel scrolls on.
    pub(crate) fn key_mode_confirm(&mut self, k: &Key) -> ControlFlow<Option<Action>> {
        if self.mode_confirm.is_some() {
            match k {
                Key::WheelUp | Key::WheelDown => return ControlFlow::Continue(()),
                Key::Click { .. } => return ControlFlow::Break(None),
                _ => {}
            }
            // The card opens asynchronously, from another head's frame, so an Enter on its
            // heels can be the keystroke that sent the line the operator was typing. Words in
            // the composer hold it back, and so does the card's first moments (MODE_ARM_MS).
            if matches!(k, Key::Enter | Key::Char('y') | Key::Char('Y')) {
                if !self.editor.text().is_empty() {
                    self.say(
                        "the composer has words in it — clear them (ctrl-c) to answer, or esc puts \
                         the card away",
                    );
                    self.redraw = true;
                    return ControlFlow::Break(None);
                }
                if self.now_ms.saturating_sub(self.mode_confirm_at) < MODE_ARM_MS {
                    self.say("the card just came up — press enter again to answer it");
                    self.redraw = true;
                    return ControlFlow::Break(None);
                }
            }
            let name = self.mode_confirm.take().unwrap();
            // Which card this was decides which frame goes out: the operator's own `/mode allow-all`
            // is a point THEY asked for and goes as `Mode { consented }`, while a card another head
            // asked for is answered by its id, so the daemon settles only the request shown.
            let asked = self.mode_asked.take();
            self.redraw = true;
            let yes = matches!(k, Key::Char('y') | Key::Char('Y') | Key::Enter);
            let out = match (asked, yes) {
                (Some(asked), yes) => {
                    self.say(if yes {
                        "allow-all confirmed for this session"
                    } else {
                        "allow-all cancelled — the mode did not change"
                    });
                    Some(Action::ModeConsent {
                        req_id: asked.req_id,
                        yes,
                    })
                }
                (None, true) => {
                    self.say("allow-all confirmed for this session");
                    Some(Action::Mode {
                        name,
                        consented: true,
                    })
                }
                (None, false) => {
                    self.say("allow-all cancelled — the mode did not change");
                    None
                }
            };
            // A confirmation waiting behind this one takes the slot now: the queue exists so
            // the front card is never silently replaced, and the drain is what keeps it moving.
            self.bring_next_mode_card();
            return ControlFlow::Break(out);
        }
        ControlFlow::Continue(())
    }

    /// The new-todo card owns the keyboard, ahead of the composer and behind nothing else that is
    /// modal. Three keys are its own; everything else is the composer's, so the title and the
    /// description are typed, edited and pasted with the keys the operator already has.
    pub(crate) fn key_todo_draft(&mut self, k: &Key) -> ControlFlow<Option<Action>> {
        if self.todo_draft.is_some() {
            match k {
                Key::Tab => {
                    let live = self.input().to_string();
                    let mut shown = String::new();
                    if let Some(draft) = self.todo_draft.as_mut() {
                        // The composer's text goes into the field being LEFT and the field entered
                        // comes out. It is read BEFORE the focus moves: `shown` answers for the focused
                        // field, so asking after the move would answer with the one just left.
                        draft.take(&live);
                        let next = draft.next();
                        shown = draft.shown("", next);
                        draft.focus = next;
                    }
                    self.set_composer(&shown);
                    self.redraw = true;
                    return ControlFlow::Break(None);
                }
                Key::Enter => {
                    let live = self.input().to_string();
                    let Some(mut draft) = self.todo_draft.take() else {
                        return ControlFlow::Break(None);
                    };
                    draft.take(&live);
                    // A title is required and the card stays up without one - the only field
                    // rule, and saying so beats storing a row of nothing.
                    if draft.title.trim().is_empty() {
                        self.todo_draft = Some(draft);
                        self.say("a todo item needs a title — type one, or esc to cancel");
                        self.redraw = true;
                        return ControlFlow::Break(None);
                    }
                    let mut text = draft.title.trim().to_string();
                    if !draft.detail.trim().is_empty() {
                        text.push_str(" — ");
                        text.push_str(draft.detail.trim());
                    }
                    // The row byte cap, with the card still up: the rule `/todo TEXT` and `todo_write`
                    // keep, asked of the COMPOSED row so both doors measure one string. A row is
                    // stored as written; only this bound is refused.
                    if let Some(why) = row_refusal(&text) {
                        self.todo_draft = Some(draft);
                        self.say(&why);
                        self.redraw = true;
                        return ControlFlow::Break(None);
                    }
                    let when = if draft.when.trim().is_empty() {
                        None
                    } else {
                        Some(letibot_sessionlog::event::TodoCondition::Job {
                            handle: draft.when.trim().to_string(),
                        })
                    };
                    self.set_composer("");
                    // The row is filed HERE, not by handing the card's words to the verb parser: a
                    // title reading `done 2` would be taken for the verb and move a row the reader
                    // never named. A form's fields are fields; the verbs stay the typed door.
                    let mut mine = self.operator_todos();
                    mine.push(letibot_sessionlog::event::TodoEntry {
                        content: text,
                        status: letibot_sessionlog::event::TodoStatus::Pending,
                        by: letibot_sessionlog::event::TodoBy::Operator,
                        when,
                        // A row filed here waits on nothing: the card's three fields are the
                        // title, the detail and a JOB handle, and an edge is not one of them -
                        // `todo_write` is where a row learns to wait on another.
                        needs: Vec::new(),
                    });
                    self.echo_operator_todos(mine.clone());
                    self.redraw = true;
                    return ControlFlow::Break(Some(Action::SetOperatorTodos {
                        items: mine,
                        moved: Vec::new(),
                    }));
                }
                Key::Esc | Key::CtrlC => {
                    self.todo_draft = None;
                    self.set_composer("");
                    self.say("nothing added");
                    self.redraw = true;
                    return ControlFlow::Break(None);
                }
                _ => {}
            }
        }
        ControlFlow::Continue(())
    }

    /// A password field owns the keyboard: while `sudo` is waiting, characters and pastes go into
    /// the buffer, Enter sends it and Esc or Ctrl+C refuses. Nothing reaches the composer, the
    /// ladder or the scrollback, so a password cannot land in a prompt.
    pub(crate) fn key_secret(&mut self, k: &Key) -> ControlFlow<Option<Action>> {
        if let Some(ask) = &self.secret {
            let req_id = ask.req_id.clone();
            match k {
                Key::Char(c) => self.secret_buf.push(*c),
                Key::Paste(s) => self.secret_buf.push_str(s.trim_end_matches(['\n', '\r'])),
                Key::Backspace => {
                    self.secret_buf.pop();
                }
                Key::KillToStart | Key::KillToEnd => self.secret_buf.clear(),
                Key::Enter => {
                    let secret = std::mem::take(&mut self.secret_buf);
                    self.secret = None;
                    self.redraw = true;
                    return ControlFlow::Break(Some(Action::Secret {
                        req_id,
                        secret: Some(secret),
                    }));
                }
                Key::Esc | Key::CtrlC => {
                    self.secret_buf.clear();
                    self.secret = None;
                    self.redraw = true;
                    return ControlFlow::Break(Some(Action::Secret {
                        req_id,
                        secret: None,
                    }));
                }
                _ => {}
            }
            self.redraw = true;
            return ControlFlow::Break(None);
        }
        ControlFlow::Continue(())
    }

    /// The confirmation that ends a pane owns the keys while it is up, checked ahead of the prompt
    /// card so one keystroke cannot answer both. A detach never asks - it ends nothing - so this
    /// card exists only for a `!term close` the operator typed while a program is running.
    pub(crate) fn key_term_ask(&mut self, k: &Key) -> ControlFlow<Option<Action>> {
        if let Some(ask) = self.term_ask.take() {
            self.redraw = true;
            return ControlFlow::Break(match k {
                Key::Char('y') | Key::Char('Y') => {
                    // An ending is on its way, so the keys stop here: a byte written into a pty whose
                    // program is being signalled is a byte nobody reads. The row is filed when the
                    // daemon's `TermEnded` lands, and nothing is said here - that row is the disclosure.
                    if let Some(p) = self.term.as_mut()
                        && p.pane == Some(ask.pane)
                    {
                        p.closing = true;
                    }
                    Some(Action::TermClose { pane: ask.pane })
                }
                // Anything that is not a deliberate yes is the cancel, Esc included - the
                // only shape a destructive confirmation can have. It says what it did NOT do,
                // because a card that vanishes in silence reads as an act.
                _ => {
                    self.say(&format!(
                        "{} is still running — nothing was ended",
                        ask.line
                    ));
                    None
                }
            });
        }
        ControlFlow::Continue(())
    }

    /// A command of the operator's own asked them something, and this owns the keyboard: a typed
    /// character answers the card, not the next thing. Enter sends - an empty line included, since
    /// `Continue? [Y/n]` defaults - and Esc only puts the card away, refusing nothing.
    pub(crate) fn key_prompt(&mut self, k: &Key) -> ControlFlow<Option<Action>> {
        if let Some(ask) = &self.prompt
            && !self.prompt_away
        {
            let req_id = ask.req_id.clone();
            match k {
                Key::Char(c) => self.prompt_buf.push(*c),
                Key::Paste(s) => self.prompt_buf.push_str(s.trim_end_matches(['\n', '\r'])),
                Key::Backspace => {
                    self.prompt_buf.pop();
                }
                Key::KillToStart | Key::KillToEnd => self.prompt_buf.clear(),
                Key::Enter => {
                    let line = std::mem::take(&mut self.prompt_buf);
                    self.prompt = None;
                    self.redraw = true;
                    return ControlFlow::Break(Some(Action::PromptAnswer { req_id, line }));
                }
                Key::Esc | Key::CtrlC => {
                    self.prompt_buf.clear();
                    // Put away, not closed: the daemon keeps the request open and the run keeps
                    // waiting, which is why `App::submit` reads `self.prompt` - a bare line typed
                    // while the card is away must not become a prompt for the model.
                    self.prompt_away = true;
                    self.redraw = true;
                    self.say(
                        "card put away — the command is still waiting · alt-a brings it back, \
                         and `!send LINE` answers it",
                    );
                    return ControlFlow::Break(None);
                }
                _ => {}
            }
            self.redraw = true;
            return ControlFlow::Break(None);
        }
        ControlFlow::Continue(())
    }

    /// A key the picker asked for owns the keyboard as the password field does: characters and
    /// pastes go into the buffer, Enter does both things in one verb - stores the key and takes the
    /// row - and Esc cancels with nothing stored. The composer draws a dot per character meanwhile.
    pub(crate) fn key_key_ask(&mut self, k: &Key) -> ControlFlow<Option<Action>> {
        if let Some(ask) = self.key_ask.clone() {
            match k {
                Key::Char(c) => self.key_buf.push(*c),
                Key::Paste(s) => self.key_buf.push_str(s.trim_end_matches(['\n', '\r'])),
                Key::Backspace => {
                    self.key_buf.pop();
                }
                Key::KillToStart | Key::KillToEnd => self.key_buf.clear(),
                Key::Enter => {
                    let key = std::mem::take(&mut self.key_buf);
                    self.key_ask = None;
                    self.redraw = true;
                    // An empty enter is a cancelled ask, not a stored empty key - the
                    // daemon would refuse it and the row was not taken.
                    if key.is_empty() {
                        self.say("no key given — the row was not taken");
                        return ControlFlow::Break(None);
                    }
                    self.say(&format!(
                        "storing the {} key and switching to {}…",
                        ask.provider, ask.choice
                    ));
                    // The switch, then a re-read of the rows it changed - the same order the
                    // plain switch keeps, so the header names what answers now.
                    self.queued.push(Action::Settings);
                    // And, when the chooser was opened because the local model could not be
                    // reached, the default and the held message after it - `app::reach`.
                    self.took_cloud_row(&ask.choice);
                    return ControlFlow::Break(Some(Action::Slash {
                        line: format!("models {} --key {}", ask.choice, key),
                    }));
                }
                Key::Esc | Key::CtrlC => {
                    self.key_buf.clear();
                    self.key_ask = None;
                    self.say("cancelled — nothing was stored and the row was not taken");
                    self.redraw = true;
                    return ControlFlow::Break(None);
                }
                _ => {}
            }
            self.redraw = true;
            return ControlFlow::Break(None);
        }
        ControlFlow::Continue(())
    }

    /// An open decision owns Up/Down and a bare Enter, before the composer: while a prompt is up
    /// those keys mean the ladder, and Up/Down move it even with a line being typed, because
    /// emptying the composer must not be the price of choosing. Enter and digits keep the guard.
    pub(crate) fn key_decision(&mut self, k: &Key) -> ControlFlow<Option<Action>> {
        // Not while a draft card is drawn in front of it: ↑↓ and the digits moved a
        // ladder the operator could not see.
        if !self.open.is_empty() && self.todo_draft.is_none() && self.key_ask.is_none() {
            let n = decision_rows(&self.open[0]);
            // A question's digits are the characters of its answer, so only a permission's rows
            // are answered by number; see the digit arm below.
            let numbered = self.open[0].kind != "question";
            let typing = !self.editor.text().is_empty();
            match k {
                // Esc puts a question card away and answers nothing: the words leave WITH the card
                // and alt-a brings both back. The press is not handed to the editor, or a second Esc
                // would interrupt a turn the operator meant only to step away from.
                Key::Esc => {
                    self.put_question_away();
                    return ControlFlow::Break(None);
                }
                Key::Up if n > 0 => {
                    self.sel = if self.sel == 0 { n - 1 } else { self.sel - 1 };
                    self.redraw = true;
                    return ControlFlow::Break(None);
                }
                Key::Down if n > 0 => {
                    self.sel = (self.sel + 1) % n;
                    self.redraw = true;
                    return ControlFlow::Break(None);
                }
                Key::Enter if n > 0 && !typing => {
                    return ControlFlow::Break(self.answer_marked());
                }
                // A card with no ladder: ↑↓ have nothing to walk and must not pull
                // history into the answer field, and an empty Enter says what it wants.
                Key::Up | Key::Down if n == 0 => return ControlFlow::Break(None),
                Key::Enter if n == 0 && !typing => {
                    self.say("type your answer — enter sends it");
                    self.redraw = true;
                    return ControlFlow::Break(None);
                }
                // A row number MARKS the row on a permission; Enter answers it. Answering on the
                // digit took a `16` typed as a free answer to "how many workers?" for choice 1,
                // and marking it ate the first character of a question's answer.
                _ if numbered && !typing && digit_row(k, n).is_some() => {
                    self.sel = digit_row(k, n).unwrap();
                    self.redraw = true;
                    return ControlFlow::Break(None);
                }
                _ => {}
            }
        }
        ControlFlow::Continue(())
    }

    /// The quit card owns the keys while it is open, ahead of every other list: it was opened by a
    /// key that means "I am leaving", and a stray arrow landing in the transcript under it would be
    /// a keystroke the operator aimed at the card.
    pub(crate) fn key_quit_card(&mut self, k: &Key) -> ControlFlow<Option<Action>> {
        if self.quit_card {
            match k {
                Key::Up | Key::Down => {
                    self.quit_sel = 1 - self.quit_sel.min(1);
                    self.redraw = true;
                    return ControlFlow::Break(None);
                }
                // A row number MARKS the row; Enter answers it. Answering on the digit let a `2`
                // typed as the first character of a line stop the daemon with every job in it, while
                // the bar said "1/2 or up/down then enter". A digit is a move now, and Enter answers.
                _ if self.editor.text().is_empty() && digit_row(k, 2).is_some() => {
                    self.quit_sel = digit_row(k, 2).expect("just checked");
                    self.redraw = true;
                    return ControlFlow::Break(None);
                }
                // Enter has the digits' empty-composer guard: words typed after the
                // card opened were lost on an Enter that quit.
                Key::Enter if !self.editor.text().is_empty() => {
                    self.say(
                        "the composer has words in it — clear them (ctrl-c) to choose, or esc \
                         stays",
                    );
                    self.redraw = true;
                    return ControlFlow::Break(None);
                }
                Key::Enter => {
                    self.quit_card = false;
                    self.quit = true;
                    return ControlFlow::Break(Some(if self.quit_sel == 0 {
                        Action::Quit
                    } else {
                        Action::StopDaemon
                    }));
                }
                // Esc is "I did not mean to leave", which is the answer a card
                // like this has to have - the alternative is an operator who
                // hit Ctrl+C twice by habit and cannot take it back.
                Key::Esc | Key::CtrlC => {
                    self.quit_card = false;
                    self.say("staying");
                    self.redraw = true;
                    return ControlFlow::Break(None);
                }
                _ => {}
            }
        }
        ControlFlow::Continue(())
    }

    /// An open picker owns Up/Down and Enter on an empty line, after the decision ladder for the
    /// same reason. The empty-composer rule is the ladder's own: a half-typed line's Enter still
    /// means the line, and the typed path lands in `pick_mode` through `submit`.
    pub(crate) fn key_pick(&mut self, k: &Key) -> ControlFlow<Option<Action>> {
        if let Some(subject) = self.pick {
            let choices = self
                .pick_values()
                .into_iter()
                .map(|(v, _)| v)
                .collect::<Vec<_>>();
            let n = choices.len();
            match k {
                Key::Up if n > 0 => {
                    self.mode_sel = if self.mode_sel == 0 {
                        n - 1
                    } else {
                        self.mode_sel - 1
                    };
                    // The reader has taken the cursor, so a settings answer landing from
                    // here on must not move it - see [`App::pick_unseeded`]. A worse defect
                    // than the one the re-seed fixes: a cursor that jumps while they arrow.
                    self.pick_unseeded = false;
                    self.redraw = true;
                    return ControlFlow::Break(None);
                }
                Key::Down if n > 0 => {
                    self.mode_sel = (self.mode_sel + 1) % n;
                    self.pick_unseeded = false;
                    self.redraw = true;
                    return ControlFlow::Break(None);
                }
                Key::Enter if self.editor.text().is_empty() => {
                    if n == 0 {
                        // A daemon older than protocol 18 sends no choices; the
                        // pane says so rather than cycling a list it made up -
                        // the same words the config pane's mode row says.
                        self.say(if subject == Pick::Model {
                            "this daemon does not send the model list; use `/models PROVIDER/MODEL`"
                        } else {
                            "this daemon does not send the mode list; use `/mode NAME`"
                        });
                        return ControlFlow::Break(None);
                    }
                    let name = choices[self.mode_sel.min(n - 1)].clone();
                    return ControlFlow::Break(self.take_pick(name));
                }
                _ if self.editor.text().is_empty() && digit_row(k, n).is_some() => {
                    let at = digit_row(k, n).unwrap();
                    self.mode_sel = at;
                    return ControlFlow::Break(self.take_pick(choices[at].clone()));
                }
                // Esc leaves the setting alone and closes the card - the card's own key now,
                // because the card is in front of any surface, not one arm of a
                // block that closed every pane and card at once.
                Key::Esc | Key::CtrlC => {
                    self.pick = None;
                    self.redraw = true;
                    return ControlFlow::Break(None);
                }
                _ => {}
            }
        }
        ControlFlow::Continue(())
    }
}
