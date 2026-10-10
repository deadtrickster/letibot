//! **Keys a card owns**: while a card is up — a confirmation, a draft, a secret, an ask, a
//! decision's ladder, the quit card, a setting's values — its keys are its own.

use super::*;
use std::ops::ControlFlow;

impl App {
    /// **The `allow-all` confirmation owns the keyboard too**, and for the same
    /// reason the password field does: a question this consequential must not be
    /// answered by a keystroke the operator aimed at the composer.
    ///
    /// **`y` and Enter both confirm.** It was `y` alone, on the fail-closed
    /// argument that a mistyped answer should be a no — which is right about
    /// stray keys and wrong about Enter, the key every other card in this file
    /// confirms with (the quit card takes it, the ladder takes it, the pickers
    /// take it). The operator, 2026-09-20: *"i did allow-all and even got to
    /// that giant red warning"* — and the session was still at
    /// `automode-edits` afterwards, because the natural keystroke on a
    /// confirmation silently cancelled it. A card that names two keys and
    /// means one of them is a card that lies.
    ///
    /// Everything else still cancels, Esc included, so a key aimed at the
    /// composer is still a no.
    pub(crate) fn key_mode_confirm(&mut self, k: &Key) -> ControlFlow<Option<Action>> {
        if self.mode_confirm.is_some() {
            let name = self.mode_confirm.take().unwrap();
            self.redraw = true;
            return ControlFlow::Break(match k {
                Key::Char('y') | Key::Char('Y') | Key::Enter => {
                    self.say("allow-all confirmed for this session");
                    Some(Action::Mode {
                        name,
                        consented: true,
                    })
                }
                _ => {
                    self.say("allow-all cancelled — the mode did not change");
                    None
                }
            });
        }
        ControlFlow::Continue(())
    }

    /// **A password field owns the keyboard.** While `sudo` is waiting, every
    /// key is the password's: characters and pastes go into the buffer, Enter
    /// sends it, Esc or Ctrl+C refuses. Nothing reaches the composer, the
    /// ladder or the scrollback, so a password cannot land in a prompt.
    /// **The new-todo card owns the keyboard**, ahead of the composer and behind nothing else
    /// that is modal. Three keys are its own; everything else is the composer's, so the title and
    /// the description are typed, edited and pasted with the keys the operator already has.
    pub(crate) fn key_todo_draft(&mut self, k: &Key) -> ControlFlow<Option<Action>> {
        if self.todo_draft.is_some() {
            match k {
                Key::Tab => {
                    let live = self.input().to_string();
                    let mut shown = String::new();
                    if let Some(draft) = self.todo_draft.as_mut() {
                        // The composer's text goes into the field being LEFT, and the field being
                        // entered comes out — leticl's `%todo-draft-focus`, one order. **The text is
                        // read BEFORE the focus moves**: `shown` answers with the composer's live
                        // text for the field that is focused, so asking after the move answers with
                        // the field we just left and the composer would come up empty.
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
                    // **A title is required and the card stays up without one** — the only field
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
                    let when = if draft.when.trim().is_empty() {
                        None
                    } else {
                        Some(letibot_sessionlog::event::TodoCondition::Job {
                            handle: draft.when.trim().to_string(),
                        })
                    };
                    self.set_composer("");
                    // **The row is filed HERE, not by handing the card's words to the verb parser.**
                    //
                    // The add used to go through `todo_command`, on the argument that a card and a
                    // typed line must not become different acts. They still are one act — this
                    // writes the same row, tagged the same way, as `SetOperatorTodos`, and echoes
                    // it on the same path — but the WORDS are not re-read as a command line, and
                    // that matters more with three fields than it did with two: a title reading
                    // `done 2` or `when 1 j7` was taken for the verb by that door and would move or
                    // condition a row the reader never named. A form's fields are fields; the three
                    // verbs stay the typed door, which is the other one the operator asked for
                    // (*"or via a form, when I file a todo"*).
                    let mut mine = self.operator_todos();
                    mine.push(letibot_sessionlog::event::TodoEntry {
                        content: text,
                        status: letibot_sessionlog::event::TodoStatus::Pending,
                        by: letibot_sessionlog::event::TodoBy::Operator,
                        when,
                        // **A row filed here waits on nothing**: the card's three fields are the
                        // title, the detail and a JOB handle, and an edge is not one of them —
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

    /// **The confirmation that ends a pane owns the keys while it is up**, and it is checked
    /// ahead of the prompt card so the two can never both be answered by one keystroke — see
    /// [`TermAsk`] for why the yes is `y` and not Enter, and why every other key cancels.
    ///
    /// **A detach never asks**, because it ends nothing: this card exists only for a
    /// `!term close` the operator typed, and only while a program is running (see
    /// [`App::begin_close`]).
    pub(crate) fn key_term_ask(&mut self, k: &Key) -> ControlFlow<Option<Action>> {
        if let Some(ask) = self.term_ask.take() {
            self.redraw = true;
            return ControlFlow::Break(match k {
                Key::Char('y') | Key::Char('Y') => {
                    // **An ending is on its way.** The keys stop here from now on — a byte written
                    // into a pty whose program is being signalled is a byte nobody will read —
                    // and the row the ending becomes is filed when the daemon's `TermEnded`
                    // lands, with `closed: true` so the register is the operator's own act.
                    //
                    // **Nothing is said here.** The card coming down is the act, and the row the
                    // ending files a moment later is the disclosure: a notice on top of the two
                    // would be the third copy of one fact, and it is the one that fades.
                    if let Some(p) = self.term.as_mut() {
                        p.closing = true;
                    }
                    Some(Action::TermClose)
                }
                // **Anything that is not a deliberate yes is the cancel**, Esc included — the
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

    /// **A command of the operator's own asked them something, and this owns the
    /// keyboard** — the password field's rule one card over, and for the same reason:
    /// while a card is up, a character typed is an answer to it and not the first letter
    /// of the next thing the operator meant to say.
    ///
    /// **Enter sends and Esc puts the card away, and the two are not the same act.**
    /// Enter answers the command: the line goes down the frame the daemon writes into the
    /// run's stdin. **Esc does NOT refuse anything** — the command is still running and
    /// still waiting, and there is nothing to refuse — it only takes this head's card off
    /// the screen, which is what a person wants when they would rather type the answer as
    /// a `!send` line or watch the stream for a moment longer. The daemon keeps the
    /// request open and the run keeps waiting; the card does not come back, because the
    /// run has not asked a new question.
    ///
    /// **An empty line is a real answer** and Enter on an empty field sends it: `Continue?
    /// [Y/n]` takes Enter as its default, and a person accepting a default must not have
    /// to type a letter to say so.
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
                    // **Put away, not closed.** The daemon keeps the request open and the run
                    // keeps waiting, and [`App::submit`] reads `self.prompt` for exactly that
                    // reason: a bare line typed while it is away must not become a prompt for
                    // the model. See [`App::prompt_away`].
                    self.prompt_away = true;
                    self.redraw = true;
                    self.say(
                        "card put away — the command is still waiting, and \
                              `!send LINE` answers it",
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

    /// **A key the picker asked for owns the keyboard**, exactly as the password field
    /// does and for the same reason: characters and pastes go into the buffer, Enter does
    /// both things in one verb — `/models CHOICE --key K` stores the key (mode 600, the
    /// file the daemon reads) AND takes the row, which is the round trip the typed
    /// spelling already is — and Esc cancels with nothing stored. Nothing reaches the
    /// composer, so a key cannot land in a prompt, and the composer's own box draws a dot
    /// per character while this is up (`composer_rows`).
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
                    // **An empty enter is a cancelled ask, not a stored empty key** — the
                    // daemon would refuse it and the row was not taken.
                    if key.is_empty() {
                        self.say("no key given — the row was not taken");
                        return ControlFlow::Break(None);
                    }
                    self.say(&format!(
                        "storing the {} key and switching to {}…",
                        ask.provider, ask.choice
                    ));
                    // The switch, then a re-read of the rows it changed — the same order the
                    // plain switch keeps, so the header names what answers now.
                    self.queued.push(Action::Settings);
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

    /// **An open decision owns Up/Down and a bare Enter.**
    ///
    /// Before the composer, because while a prompt is on the screen those keys mean
    /// the ladder and cannot sensibly mean anything else -- the same argument the
    /// picker arm above already makes for a bare row number.
    ///
    /// **Up/Down move the ladder whether or not a line is being typed**, and
    /// this used to require an empty composer so that "a half-typed line still
    /// scrolls and still edits". The cost of that was not visible until the
    /// operator hit it: a permission arrives while you are typing, and the only
    /// way to reach the menu is to empty the composer first — so the words you
    /// were writing are the price of choosing an option. Their words,
    /// 2026-09-20: *"suppose i type a prompt and permission ask arrives — until
    /// i press down arrow I wont get into the permissions menu, by which time
    /// my prompt is erased and gone"*.
    ///
    /// The quit card and the jobs pane in this same file take Up/Down
    /// unconditionally — they no longer gate Enter, which is now every
    /// pane's (see the arm that closes the composer to it below); the ladder
    /// was the odd one out for the arrows.
    /// Nothing is taken from the composer, because a one-line composer does not
    /// edit with Up/Down — what moves aside is scrollback scrolling, for as long
    /// as an ask is open, and PageUp/PageDown still do that.
    ///
    /// Enter and the digits keep the empty-composer guard, and for a reason that
    /// is the opposite of this one: with a typed line, Enter is `submit`'s, which
    /// answers the marked row and HOLDS the words — a permission arriving
    /// mid-typing must not turn Enter into "send the half-thought" — and a line
    /// being typed keeps its digits.
    pub(crate) fn key_decision(&mut self, k: &Key) -> ControlFlow<Option<Action>> {
        if !self.open.is_empty() {
            let n = decision_rows(&self.open[0]);
            let typing = !self.editor.text().is_empty();
            match k {
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
                // A row number is the row, and answering it — see `digit_row`.
                _ if !typing && digit_row(&k, n).is_some() => {
                    self.sel = digit_row(&k, n).unwrap();
                    return ControlFlow::Break(self.answer_marked());
                }
                _ => {}
            }
        }
        ControlFlow::Continue(())
    }

    /// **An open mode picker owns Up and Down, and Enter on an empty line.**
    ///
    /// The session picker's twin, one question narrow: the mode this session
    /// runs under. It sits after the session picker, which keeps precedence
    /// while both are up — though neither ever is, each opener closing the
    /// other — and after the decision ladder for the same reason. The
    /// empty-composer rule is the ladder's own: a half-typed line's Enter
    /// still means the line, and the typed path lands in `pick_mode` through
    /// `submit`.
    /// **The quit card owns the keys while it is open**, ahead of every other
    /// list: it was opened by a key that means "I am leaving", and a stray
    /// arrow landing in the transcript under it would be a keystroke the
    /// operator aimed at the card.
    pub(crate) fn key_quit_card(&mut self, k: &Key) -> ControlFlow<Option<Action>> {
        if self.quit_card {
            match k {
                Key::Up | Key::Down => {
                    self.quit_sel = 1 - self.quit_sel.min(1);
                    self.redraw = true;
                    return ControlFlow::Break(None);
                }
                _ if self.editor.text().is_empty() && digit_row(&k, 2).is_some() => {
                    self.quit_sel = digit_row(&k, 2).unwrap();
                    self.quit_card = false;
                    self.quit = true;
                    return ControlFlow::Break(Some(if self.quit_sel == 0 {
                        Action::Quit
                    } else {
                        Action::StopDaemon
                    }));
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
                // like this has to have — the alternative is an operator who
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
                    // **The reader has taken the cursor**, so a settings answer landing from
                    // here on must not move it — see [`App::pick_unseeded`]. A worse defect
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
                        // pane says so rather than cycling a list it made up —
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
                _ if self.editor.text().is_empty() && digit_row(&k, n).is_some() => {
                    let at = digit_row(&k, n).unwrap();
                    self.mode_sel = at;
                    return ControlFlow::Break(self.take_pick(choices[at].clone()));
                }
                Key::Click { y, .. } => {
                    // The arithmetic the last frame did: the card's first
                    // choice sat at `mode_first_row`, and only a row the card
                    // provably drew in full is trusted — `mode_rows_drawn` is
                    // zero when the fit loop or the backstop cut the card, so
                    // a click into a list nobody saw whole moves nothing.
                    let row = usize::from(*y).saturating_sub(self.mode_first_row);
                    if n > 0 && row < self.mode_rows_drawn {
                        self.mode_sel = row.min(n - 1);
                        // A click is the reader's too, for the reason the arrows are.
                        self.pick_unseeded = false;
                        self.redraw = true;
                    }
                    return ControlFlow::Break(None);
                }
                _ => {}
            }
        }
        ControlFlow::Continue(())
    }
}
