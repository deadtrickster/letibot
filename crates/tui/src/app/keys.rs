//! **A key, handled**: the dispatch from a keystroke to whichever card, pane, window or the
//! composer owns it, and the matching of a typed answer to a decision's options.

use super::*;
use letibot_sessionlog::view::OpenDecision;
use letibot_ui::editor::Reaction;

impl App {
    /// A key. Returns an action for the driver to send, if any.
    ///
    /// # Who owns which key
    ///
    /// Five keys are the head's: the two folds, the redraw, and the two page
    /// keys that move the transcript. **Everything else goes to the composer**,
    /// which is the whole reason `letibot_ui::editor` exists — word motion, undo
    /// batching, the kill ring, the paste ledger and the two double-tap windows
    /// are interaction policy, and interaction policy in a match arm in a head is
    /// how it ends up subtly different in the second head.
    ///
    /// # Up and Down
    ///
    /// The composer's, and only then the transcript's. `Editor::vertical` moves
    /// by **visual** row inside a wrapped prompt and walks history at the edges,
    /// which is what every shell does and what a person pressing Up after
    /// sending something expects. When it refuses — nothing to recall, or a
    /// recalled entry has been edited and moving would destroy the edit — the
    /// press falls through to scrolling by a line, so an empty composer with no
    /// history still scrolls with the arrows it always did.
    ///
    /// # Ctrl+C no longer interrupts
    ///
    /// It cleared nothing and quit an idle head on one press, so there was no way
    /// to abandon a half-typed prompt and a stray Ctrl+C killed the head. The
    /// composer's rule (opencode's) is: Ctrl+C on a non-empty composer **clears
    /// it**, twice within a second on an empty one quits, and **Esc twice within
    /// five seconds interrupts the turn**. The hint bar says which, and changes
    /// after the first press — that is the entire mechanism by which anyone
    /// discovers a double-tap exists.
    pub fn key(&mut self, k: Key) -> Option<Action> {
        // **The terminal talking about itself**, not the person: focus and the background
        // colour. Recorded and nothing else — they must not dismiss a notice, end a recall or
        // reach a pane the way a keystroke does.
        match k {
            Key::FocusIn | Key::FocusOut => {
                self.focused = Some(matches!(k, Key::FocusIn));
                return None;
            }
            Key::Background { light } => {
                if self.light_background != Some(light) {
                    self.light_background = Some(light);
                    self.invalidate_history();
                    self.redraw = true;
                }
                return None;
            }
            _ => {}
        }
        // **The session's prompts, before a recall starts** — see `refresh_prompt_history`.
        // Up may yet be taken by a card or a pane further down; refreshing the list then
        // changes nothing a reader sees, and the editor ignores it mid-recall.
        if matches!(k, Key::Up) {
            self.refresh_prompt_history();
        }
        // **Any key is an acknowledgement of whatever the notice said**: the deadline
        // moves to now, so this tick's `screen()` — which runs below the key handling —
        // takes the sentence down before anything is drawn. The arrows and the paging
        // keys are exempt because they are the keys a reader scrolls *with*.
        //
        // A notice nobody timed is not the reader's to dismiss, exactly as only `say`
        // starts a clock: a key moves a deadline, it does not invent one.
        if !matches!(k, Key::Up | Key::Down | Key::PageUp | Key::PageDown)
            && self.notice_until.is_some()
        {
            self.notice_until = Some(self.now_ms);
        }
        // **The `allow-all` confirmation owns the keyboard too**, and for the same
        // reason the password field does: a question this consequential must not be
        // answered by a keystroke the operator aimed at the composer.
        //
        // **`y` and Enter both confirm.** It was `y` alone, on the fail-closed
        // argument that a mistyped answer should be a no — which is right about
        // stray keys and wrong about Enter, the key every other card in this file
        // confirms with (the quit card takes it, the ladder takes it, the pickers
        // take it). The operator, 2026-09-20: *"i did allow-all and even got to
        // that giant red warning"* — and the session was still at
        // `automode-edits` afterwards, because the natural keystroke on a
        // confirmation silently cancelled it. A card that names two keys and
        // means one of them is a card that lies.
        //
        // Everything else still cancels, Esc included, so a key aimed at the
        // composer is still a no.
        if self.mode_confirm.is_some() {
            let name = self.mode_confirm.take().unwrap();
            self.redraw = true;
            return match k {
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
            };
        }
        // **A password field owns the keyboard.** While `sudo` is waiting, every
        // key is the password's: characters and pastes go into the buffer, Enter
        // sends it, Esc or Ctrl+C refuses. Nothing reaches the composer, the
        // ladder or the scrollback, so a password cannot land in a prompt.
        // **The new-todo card owns the keyboard**, ahead of the composer and behind nothing else
        // that is modal. Three keys are its own; everything else is the composer's, so the title and
        // the description are typed, edited and pasted with the keys the operator already has.
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
                    return None;
                }
                Key::Enter => {
                    let live = self.input().to_string();
                    let Some(mut draft) = self.todo_draft.take() else {
                        return None;
                    };
                    draft.take(&live);
                    // **A title is required and the card stays up without one** — the only field
                    // rule, and saying so beats storing a row of nothing.
                    if draft.title.trim().is_empty() {
                        self.todo_draft = Some(draft);
                        self.say("a todo item needs a title — type one, or esc to cancel");
                        self.redraw = true;
                        return None;
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
                    });
                    self.echo_operator_todos(mine.clone());
                    self.redraw = true;
                    return Some(Action::SetOperatorTodos(mine));
                }
                Key::Esc | Key::CtrlC => {
                    self.todo_draft = None;
                    self.set_composer("");
                    self.say("nothing added");
                    self.redraw = true;
                    return None;
                }
                _ => {}
            }
        }
        if let Some(ask) = &self.secret {
            let req_id = ask.req_id.clone();
            match k {
                Key::Char(c) => self.secret_buf.push(c),
                Key::Paste(s) => self.secret_buf.push_str(s.trim_end_matches(['\n', '\r'])),
                Key::Backspace => {
                    self.secret_buf.pop();
                }
                Key::KillToStart | Key::KillToEnd => self.secret_buf.clear(),
                Key::Enter => {
                    let secret = std::mem::take(&mut self.secret_buf);
                    self.secret = None;
                    self.redraw = true;
                    return Some(Action::Secret {
                        req_id,
                        secret: Some(secret),
                    });
                }
                Key::Esc | Key::CtrlC => {
                    self.secret_buf.clear();
                    self.secret = None;
                    self.redraw = true;
                    return Some(Action::Secret {
                        req_id,
                        secret: None,
                    });
                }
                _ => {}
            }
            self.redraw = true;
            return None;
        }
        // **The confirmation that ends a pane owns the keys while it is up**, and it is checked
        // ahead of the prompt card so the two can never both be answered by one keystroke — see
        // [`TermAsk`] for why the yes is `y` and not Enter, and why every other key cancels.
        //
        // **A detach never asks**, because it ends nothing: this card exists only for a
        // `!term close` the operator typed, and only while a program is running (see
        // [`App::begin_close`]).
        if let Some(ask) = self.term_ask.take() {
            self.redraw = true;
            return match k {
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
            };
        }
        // **A command of the operator's own asked them something, and this owns the
        // keyboard** — the password field's rule one card over, and for the same reason:
        // while a card is up, a character typed is an answer to it and not the first letter
        // of the next thing the operator meant to say.
        //
        // **Enter sends and Esc puts the card away, and the two are not the same act.**
        // Enter answers the command: the line goes down the frame the daemon writes into the
        // run's stdin. **Esc does NOT refuse anything** — the command is still running and
        // still waiting, and there is nothing to refuse — it only takes this head's card off
        // the screen, which is what a person wants when they would rather type the answer as
        // a `!send` line or watch the stream for a moment longer. The daemon keeps the
        // request open and the run keeps waiting; the card does not come back, because the
        // run has not asked a new question.
        //
        // **An empty line is a real answer** and Enter on an empty field sends it: `Continue?
        // [Y/n]` takes Enter as its default, and a person accepting a default must not have
        // to type a letter to say so.
        if let Some(ask) = &self.prompt {
            let req_id = ask.req_id.clone();
            match k {
                Key::Char(c) => self.prompt_buf.push(c),
                Key::Paste(s) => self.prompt_buf.push_str(s.trim_end_matches(['\n', '\r'])),
                Key::Backspace => {
                    self.prompt_buf.pop();
                }
                Key::KillToStart | Key::KillToEnd => self.prompt_buf.clear(),
                Key::Enter => {
                    let line = std::mem::take(&mut self.prompt_buf);
                    self.prompt = None;
                    self.redraw = true;
                    return Some(Action::PromptAnswer { req_id, line });
                }
                Key::Esc | Key::CtrlC => {
                    self.prompt_buf.clear();
                    self.prompt = None;
                    self.redraw = true;
                    self.say(
                        "card put away — the command is still waiting, and \
                              `!send LINE` answers it",
                    );
                    return None;
                }
                _ => {}
            }
            self.redraw = true;
            return None;
        }
        // **A key the picker asked for owns the keyboard**, exactly as the password field
        // does and for the same reason: characters and pastes go into the buffer, Enter does
        // both things in one verb — `/models CHOICE --key K` stores the key (mode 600, the
        // file the daemon reads) AND takes the row, which is the round trip the typed
        // spelling already is — and Esc cancels with nothing stored. Nothing reaches the
        // composer, so a key cannot land in a prompt, and the composer's own box draws a dot
        // per character while this is up (`composer_rows`).
        if let Some(ask) = self.key_ask.clone() {
            match k {
                Key::Char(c) => self.key_buf.push(c),
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
                        return None;
                    }
                    self.say(&format!(
                        "storing the {} key and switching to {}…",
                        ask.provider, ask.choice
                    ));
                    // The switch, then a re-read of the rows it changed — the same order the
                    // plain switch keeps, so the header names what answers now.
                    self.queued.push(Action::Settings);
                    return Some(Action::Slash {
                        line: format!("models {} --key {}", ask.choice, key),
                    });
                }
                Key::Esc | Key::CtrlC => {
                    self.key_buf.clear();
                    self.key_ask = None;
                    self.say("cancelled — nothing was stored and the row was not taken");
                    self.redraw = true;
                    return None;
                }
                _ => {}
            }
            self.redraw = true;
            return None;
        }
        match k {
            // **A switch's chord, from the table that advertises it — and through `/verbosity`
            // its own self.**
            //
            // The two arms this replaces named their keys by hand, twice each: `ctrl-r` flipped
            // `self.reasoning` and `ctrl-x` flipped `self.raw_calls` — both of them the MIRROR the
            // drawing reads rather than the SET — so a press moved the field under the picture
            // while `Visibility` (the status row, `head.toml`, `keeps`, `rung()`) went on saying
            // the old set. `read-edits` plus `ctrl-r` was the sharp end: the thinking appeared
            // while the ladder still hid everything the thinking belongs with.
            //
            // The word is spelled and handed to [`App::set_verbosity`] rather than the fields
            // being written here, because that function is the ONE writer: it refuses a set no
            // rung can draw, moves the folds with the set, reanchors off a row the change hid,
            // closes what the change invalidates, writes the preference file, and says what
            // changed. So a chord is `/verbosity thinking=open` under a shorter spelling — which
            // is the operator's own ask, *"some toggled by shortcuts some by /commands"* — and it
            // cannot drift from the verb, because it IS the verb.
            k if k.show().is_some() => {
                let s = k.show().expect("the guard just asked the same question");
                let next = s.by_chord(self.visibility.level(s))?;
                return self.set_verbosity(&format!("{}={}", s.name(), next.as_str()));
            }
            Key::CtrlV => {
                // **One row, not a switch — R10's ruling on the overload.**
                //
                // This used to flip the conversation-wide tool fold *and* seed a window
                // on the newest long result, so one chord did two things: the wall, and
                // one row's rest. The seam under the reader's eyes says `… +N lines ·
                // ctrl-v opens it`, which reads per-row, and the operator's report is
                // exactly that **what surprised them was that ctrl-t triggered the wall
                // AT ALL**. A chord cannot be named by a per-row seam and mean the whole
                // conversation, so it keeps the meaning a seam can honestly name and the
                // conversation-wide unfold keeps `/t`, which is where it already lived
                // (and where `/help` now points).
                //
                // The window follows the newest long result for the reason
                // [`App::newest_payload_row`] gives, and the seam names this chord only
                // on that row — every other row names `/t`, because a chord may only be
                // named where it acts.
                //
                // **Under `conversation` the same chord opens the run** (R37 AMENDED), and
                // that is not a second meaning: the marker's own seam says `ctrl-t opens
                // it`, and what it opens is the newest thing on the screen that has a rest
                // to read — one result's window under every other rung, one run of hidden
                // work under this one. [`App::newest_openable`] is the one place that
                // choice is made, so the chord and the seam cannot come to disagree about
                // which of the two it is.
                if self.payload_sel.is_some() {
                    self.payload_sel = None;
                    self.payload_page = 0;
                    // **And an open run closes.** It was opened by this key, so this key is
                    // what closes it — the same bargain every other window in this file
                    // makes, and without it the second press would open a payload window
                    // inside a row that is only on screen because the run is open.
                    self.invalidate_history();
                } else if let Some(id) = self.newest_openable() {
                    self.payload_sel = Some(id);
                    self.payload_page = 0;
                    self.payload_max.set(usize::MAX);
                }
                // **Not `refold`.** That is the fold's own: it resets the scroll and
                // announces the fold state, and neither happened here.
                self.invalidate_history();
                self.redraw = true;
                return None;
            }
            Key::CtrlL => {
                // **This one keeps `redraw`.** The flag means *throw the glass away* — the next
                // frame erases and rewrites in full — and that is right exactly when this head's
                // memory of the screen is known to be wrong. Ctrl-L is the operator saying
                // something else wrote to their terminal, which is that case, and it is the only
                // key that is. The scroll keys are the opposite case and take the diff; see
                // [`App::hold`] for why the wheel does not.
                self.redraw = true;
                return None;
            }
            Key::CtrlS => {
                self.picker = !self.picker;
                self.redraw = true;
                // The two pickers are never open together: each opener closes
                // the other, so the screen holds one list and the arrows mean
                // one thing.
                if self.picker {
                    self.pick = None;
                }
                // Opening it asks for a fresh list rather than drawing the one from
                // the attach: sessions are a shared thing, and a picker showing what
                // was true when this head connected is a picker that hides the
                // session somebody else just started.
                if self.picker {
                    // The cursor starts where you are, so Enter on an untouched list
                    // is a no-op and the arrows move from a row that means something.
                    // **Through the same enumeration the arrows and Enter read.** The session this
                    // head is in is always a row — `session_rows` shows the chain down to it
                    // whatever is collapsed — so this position always exists.
                    self.picker_sel = self
                        .session_rows()
                        .iter()
                        .position(|r| self.sessions[r.idx].session_id == self.session_id)
                        .unwrap_or(0);
                }
                return self.picker.then_some(Action::ListSessions);
            }
            Key::CtrlT => return self.toggle_todos(),
            // **Hold the view (R56).** While held the head writes nothing at all, so a
            // mouse selection survives a streaming turn; the second press releases it and
            // says how much arrived while it was held. The whole contract is in
            // [`App::toggle_hold`].
            Key::CtrlP => return self.toggle_hold(),
            // Ctrl+G for the subagent tree: R/T/X/L/S/P are taken, A/E/W/U/Y/K/B/F
            // are the composer's readline keys, and the subagent tree is a *view*,
            // not a thing the composer needs a letter for.
            Key::CtrlG => {
                self.toggle_subagents();
                return None;
            }
            // Ctrl+Q for the background jobs. J would have been the mnemonic and
            // is line-feed; Q is XON, dead the same way Ctrl+S's XOFF would be —
            // and fixed the same way: cfmakeraw clears IXON, so nothing is
            // listening for flow control and the byte arrives like any other.
            Key::CtrlQ => {
                self.jobs_pane = !self.jobs_pane;
                self.pane_scroll = 0;
                self.redraw = true;
                // Opening it asks the daemon, the way the todos pane does: the
                // process table is the daemon's and a head that drew its own
                // version drew a stale one. Later changes arrive as `JobSettled`.
                return self.jobs_pane.then_some(Action::ListJobs);
            }
            // **R22: clearing your own screen costs one key.**
            //
            // Retiring a note used to be `/notes dismiss all` — the right power in the wrong
            // hand, because the thing you do to clear your own screen is a reflex and every
            // other reflex here is already a chord. The operator, on being told how to hide a
            // note: *"typing `/notes dismiss all` is not humane."*
            //
            // **It calls the VERB rather than reimplementing it.** `notes_command` is the one
            // writer for this act — it computes the keys, retires them through `retire()`,
            // saves the file and composes the sentence — so the chord and `/notes dismiss all`
            // cannot come to disagree about any of those four things. A second copy of "retire
            // every note" is a second place for the persisted set to be written differently,
            // and the whole point of agreeing the key with the other head is that one act has
            // one behaviour.
            //
            // **And the empty press says so.** That is the one place this head's answer to
            // R22 differs from the proposal it agreed with, and the reason is this tree's own
            // rule that *a chord may only be named where it acts*: `ctrl-t` is silent with
            // nothing to open because its seam names it only on the row it can open, and the
            // hint bar cannot be conditional — it names `ctrl-n` always, so the chord answers
            // when pressed. `ctrl-o` sets the same precedent one arm away ("nothing is running
            // to move to the background"), and the operator's own worry is that a reflex which
            // appears to do nothing invites a second press. One line, routine register, taken
            // down by any key including this one.
            Key::CtrlN => {
                // **Read the durable list before counting what is left.** Another head
                // may have retired one of these since this one loaded the file, and the
                // count is what decides between the verb and the honest empty answer.
                self.refresh_retired();
                // **"Nothing to retire" means nothing LEFT to retire**, not "no notes held".
                //
                // A retired note stays in `notes` on purpose (R10: retired is not deleted), so
                // after a first press the set is *all retired* rather than *empty* — and a guard
                // on `notes.is_empty()` would send the second press down the verb's path, where
                // it would say `retired 0 note(s)`, which is a true sentence an operator should
                // not be shown. Counting what is left makes the chord idempotent and honest:
                // first press retires N and says so, second press says this.
                let left = self
                    .notes
                    .iter()
                    .filter(|(_, n)| !self.is_retired(n))
                    .count();
                if left == 0 {
                    self.say("nothing to retire");
                } else {
                    let _ = self.notes_command("notes", "dismiss all");
                }
                return None;
            }
            // **Ctrl+O: move the running command to the background**, the same action
            // `/promote` names. See [`App::promote`] for why the fact it guards is a
            // running CALL and not a running turn.
            Key::CtrlO => return self.promote(),
            // **The wheel and the page keys move what is on the screen.** They
            // moved the transcript unconditionally, so a wheel in the subagent
            // output view scrolled the conversation underneath it, and Esc
            // came back to a transcript parked wherever the wheel had left it
            // — the operator's report (2026-09-17): "if i scroll subagent
            // output and return to the main conversation the scroll position
            // saved for some reason". The output view takes them; any other
            // screen on top swallows them, because a view that is not on the
            // screen does not move.
            // **An open payload window takes the page keys and the wheel**, as it takes the
            // arrows below: the reader opened one result to read it, and these moved the
            // transcript underneath it instead — which, at the bottom already, looked like
            // nothing happening at all.
            Key::PageUp | Key::PageDown | Key::WheelUp | Key::WheelDown
                if self.payload_sel.is_some() =>
            {
                let by = match k {
                    Key::WheelUp | Key::WheelDown => 3,
                    _ => self.screen_rows.max(1) / 2 + 1,
                };
                self.page_payload(matches!(k, Key::PageUp | Key::WheelUp), by);
                return None;
            }
            Key::PageUp | Key::PageDown | Key::WheelUp | Key::WheelDown => {
                // **A page is a screen, not ten lines.** `PageUp` moved by a constant ten,
                // which on a 40-row terminal is a quarter of the page the key is named for
                // — and on the tail path it compounded with the crawl `scroll_up` fixes.
                let (up, by) = match k {
                    Key::PageUp => (true, self.screen_rows.max(1)),
                    Key::PageDown => (false, self.screen_rows.max(1)),
                    // Three lines a notch: a wheel notch is a row at a time in
                    // a pager, but a transcript row can be two screen rows after
                    // wrapping, and a notch that moves one wrapped row reads as
                    // nothing happened.
                    Key::WheelUp => (true, 3),
                    _ => (false, 3),
                };
                // **An open card takes them while its own content has somewhere to go**
                // (R20). The card is the thing that needs an answer, it already owns
                // Up/Down and Enter, and the wall above its ladder is the one screenful
                // the operator may have to read past — so the page keys move *that* window
                // for as long as there is one. When the content fits, nothing here fires
                // and the keys do exactly what they always did: a card being up must not
                // cost the transcript its scroll.
                //
                // The two numbers are the last frame's, because whether anything is out of
                // view is a fact about the width — see [`App::card_window`].
                if !self.open.is_empty() && self.dec_content_len > self.dec_content_room {
                    let max = self.dec_content_len - self.dec_content_room;
                    self.dec_scroll = if up {
                        self.dec_scroll.saturating_sub(by)
                    } else {
                        (self.dec_scroll + by).min(max)
                    };
                    self.redraw = true;
                    return None;
                }
                if self.scroll_tail_overlay(up, by) {
                    return None;
                }
                // **An open pane takes them.** They used to be swallowed here,
                // on the reasoning that a view underneath a pane should not
                // move — which is right, and left the pane itself unable to
                // scroll at all. A pane longer than the terminal was a pane
                // whose tail could not be read: `leticl`'s TODO.md is 98 rows.
                //
                // The two pickers are excluded: they are short, and their click
                // arithmetic is keyed on rows counted from the top of the card.
                if self.picker || self.pick.is_some() {
                    return None;
                }
                if self.help
                    || self.stats
                    || self.todos_pane
                    || self.subagents_pane
                    || self.jobs_pane
                    || self.config_pane
                    // **And the slash listing, which is a document read from its
                    // head.** It has its own arm for the arrows, and it was missing
                    // from *this* list — so PageDown while a `/notes` listing was up
                    // scrolled the transcript underneath it, one pane over from the
                    // same defect the subagent view had.
                    || self.slash_out.is_some()
                {
                    // **The polarity is the opposite of the transcript's**, and
                    // getting it wrong here made PageDown a no-op that looked
                    // exactly like the swallowing this replaced. `self.scroll`
                    // counts rows back from the BOTTOM — scrolling up increases
                    // it — because the transcript is read from its tail.
                    // `pane_scroll` counts rows hidden above the TOP, because a
                    // pane is read from its head. So down is the one that grows.
                    let max = self.pane_len.saturating_sub(self.pane_room);
                    self.pane_scroll = if up {
                        self.pane_scroll.saturating_sub(by)
                    } else {
                        (self.pane_scroll + by).min(max)
                    };
                    self.redraw = true;
                    return None;
                }
                if up {
                    // **Through `scroll_up`, which renders as it goes.** Assigning `scroll`
                    // here and letting the frame's fill catch up is what made a page crawl:
                    // the frame clamps against the rows rendered so far, so a press could
                    // never express "further up than I have drawn".
                    self.scroll_up(by);
                } else {
                    // **Down is a WALK, the wheel included — and arriving at the tail is what
                    // resumes following.**
                    //
                    // The mirror of `scroll_up`, and `hold` for the same reason (R36): moving
                    // down is moving over the same rows in the other direction, and it is the
                    // same conversion from lines to a row. It also lands the reader back in
                    // *following* when it reaches the bottom, which is the one act that does —
                    // arriving content never will.
                    //
                    // **A `WheelDown` used to skip all of that and be the tail in ONE notch.**
                    // That was 2026-10-05's answer to *"I cant scroll back to bottom with a
                    // mouse wheel - have to press escape"*, and the cure cost more than the
                    // complaint: clearing the anchor made a single notch a jump rather than a
                    // step, so the reader could not walk *down* through a conversation at all.
                    // The operator again, with a mouse: *"one simple stroke gets me to the
                    // bottom immediately — effectively like Esc"*. Both reports are one coin,
                    // and this is the reconciliation — the notch walks three lines like its up
                    // twin, and a RUN of notches still returns the reader to the bottom, which
                    // answers October's need without the one-notch jump.
                    //
                    // The deliberate act keeps its own meaning and is not folded in here: Esc
                    // while parked — *"Esc while parked in the scrollback means \"follow the
                    // stream again\""* — and the parked `↓` below still clear the anchor in one
                    // press, and they are the keys the banner names for it.
                    self.hold(by as isize);
                }
                return None;
            }
            _ => {}
        }

        // **The subagent output view owns the keys while it is open.** Arrows
        // scroll it like a terminal — up toward the beginning, down back to the
        // tail — Enter reads the same subagent again, because a running one has
        // new output, and Esc goes back to the tree. This sits ahead of the
        // generic Esc below on purpose: Esc here means "back to the tree", not
        // "close everything".
        if self.sub_out.is_some() {
            match k {
                // **Through the one function that knows the sign**, so the arrows, the
                // page keys and the wheel cannot disagree about which way is back.
                Key::Up => {
                    self.scroll_tail_overlay(true, 1);
                    return None;
                }
                Key::Down => {
                    self.scroll_tail_overlay(false, 1);
                    return None;
                }
                Key::Enter => {
                    let id = self.sub_out.as_ref().unwrap().session_id.clone();
                    self.sub_out_pending = Some(id.clone());
                    return Some(Action::Peek(id));
                }
                Key::Esc | Key::CtrlC => {
                    self.sub_out = None;
                    self.redraw = true;
                    return None;
                }
                _ => {}
            }
        }

        // **The job-output view owns the keys while it is open.** Up and down walk
        // the loaded window; right asks for the next page and left walks back the
        // way right came; Enter refreshes a running job, or takes the next page when
        // there is one; Esc goes back to the jobs list, not out of everything — the
        // same shape, key for key, as the subagent-output view above.
        if self.job_out.is_some() {
            match k {
                Key::Up => {
                    self.scroll_tail_overlay(true, 1);
                    return None;
                }
                Key::Down => {
                    self.scroll_tail_overlay(false, 1);
                    return None;
                }
                Key::Enter => {
                    return self.job_out_page(true);
                }
                // **`Right` keeps the guard that `Enter` just lost**, and the difference
                // is what the key is *for*. Enter here is the pane's — it is the key the
                // pane advertises and the operator's words are not what they meant by it.
                // Right is a cursor key first: a half-typed line keeps its motion, which
                // is the same reason the composer's own arrows are not up for grabs.
                Key::Right if self.editor.text().is_empty() => {
                    return self.job_out_page(true);
                }
                Key::Left if self.editor.text().is_empty() => {
                    return self.job_out_page(false);
                }
                Key::Esc | Key::CtrlC => {
                    self.job_out = None;
                    self.redraw = true;
                    return None;
                }
                _ => {}
            }
        }

        // **The slash listing owns the keyboard while it is up**, the way the
        // subagent-output pane above does: it is a screen covering the
        // conversation, so the keys that scroll and dismiss it must not also
        // reach the composer behind it.
        if self.slash_out.is_some() {
            match k {
                Key::Esc | Key::CtrlC => {
                    self.slash_out = None;
                    self.pane_scroll = 0;
                    self.redraw = true;
                    return None;
                }
                // **Up moves toward the beginning, which means DECREASING this
                // offset.** `pane_scroll` counts rows hidden **above the top** —
                // `pane_window` is literally `skip(self.pane_scroll)` — so adding to it
                // walks further *down* the document. These two arms had it inverted, so
                // `↑` scrolled a `/notes` listing toward its end while the footer under
                // it said `up/down scrolls`. It is leticl's own finding in the same
                // place: *"called with the top-origin sign, ↑ walked toward the END
                // while the hint bar said otherwise."*
                Key::Up => {
                    self.pane_scroll = self.pane_scroll.saturating_sub(1);
                    self.redraw = true;
                    return None;
                }
                // Down is the bounded one: `pane_window` clamps it against the rows
                // it actually has, which is the only place that knows how many there
                // are (the slash listing is built on every draw).
                Key::Down => {
                    self.pane_scroll = self.pane_scroll.saturating_add(1);
                    self.redraw = true;
                    return None;
                }
                _ => {}
            }
        }

        // Help and the picker are screens, and the two keys that mean "go back"
        // close them before the composer ever sees them.
        // The config pane owns Up/Down/Enter while it is open: arrows move,
        // Enter changes the row under the cursor when it is one that can change
        // now, and says why when it is not.
        if self.config_pane {
            match k {
                Key::Up => {
                    let n = self.config_rows().len().max(1);
                    self.config_sel = if self.config_sel == 0 {
                        n - 1
                    } else {
                        self.config_sel - 1
                    };
                    self.redraw = true;
                    return None;
                }
                Key::Down => {
                    let n = self.config_rows().len().max(1);
                    self.config_sel = (self.config_sel + 1) % n;
                    self.redraw = true;
                    return None;
                }
                Key::Enter => {
                    return self.config_change();
                }
                _ => {}
            }
        }
        // **An entry's detail overlay owns Esc, the arrows and nothing else.** Esc goes back to
        // the LIST, which is still behind it — the jobs pane's rule, one pane along.
        //
        // **It sits ABOVE the block that closes every pane on Esc, and that is load-bearing.**
        // Below it, the first Esc closed the PANE and left the overlay standing — and because
        // the overlay is drawn before the pane, the screen did not change: one press did
        // nothing, two presses left the queue. Every other overlay in this file (`sub_out`,
        // `job_out`) is above that block for the same reason, and
        // `esc_leaves_the_entry_overlay_with_the_queue_still_behind_it` is the test that holds
        // this one there.
        if self.queue_open.is_some() {
            if matches!(k, Key::Esc | Key::CtrlC) {
                self.queue_open = None;
                self.pane_scroll = 0;
                self.redraw = true;
                return None;
            }
            if matches!(k, Key::Up | Key::Down | Key::PageUp | Key::PageDown) {
                let page = self.pane_room.max(1);
                match k {
                    Key::Up => self.pane_scroll = self.pane_scroll.saturating_sub(1),
                    Key::Down => self.pane_scroll += 1,
                    Key::PageUp => self.pane_scroll = self.pane_scroll.saturating_sub(page),
                    _ => self.pane_scroll += page,
                }
                // Clamped against the last draw's own numbers: the key handler has no width and
                // no height, and a scroll clamped against a guess walks past the end.
                let max = self.pane_len.saturating_sub(self.pane_room);
                self.pane_scroll = self.pane_scroll.min(max);
                self.redraw = true;
                return None;
            }
        }

        if (self.help
            || self.picker
            || self.pick.is_some()
            || self.stats
            || self.todos_pane
            || self.subagents_pane
            || self.jobs_pane
            || self.queue_pane
            || self.config_pane)
            && matches!(k, Key::Esc | Key::CtrlC)
        {
            self.help = false;
            self.picker = false;
            self.pick = None;
            self.quit_card = false;
            self.stats = false;
            self.todos_pane = false;
            self.subagents_pane = false;
            self.jobs_pane = false;
            self.queue_pane = false;
            self.config_pane = false;
            self.sub_out_pending = None;
            self.redraw = true;
            return None;
        }
        // **Esc while parked in the scrollback means "follow the stream again"**,
        // which is what the scrollback banner says it means. Only then does Esc
        // start arming an interrupt — **and not while a payload window is open**: that
        // window's own seam prints `esc closes`, and the surface carrying the promise is
        // the one Esc has to keep it to. See the arm below.
        if matches!(k, Key::Esc) && !self.following() && self.payload_sel.is_none() {
            // **Back to following**, which is what the banner says this key does — and it
            // clears the anchor as well as the count, because the two are one state (R36).
            self.scroll = 0;
            self.anchor = None;
            return None;
        }
        // **An open payload view owns the arrows and Esc**, and it sits here — ahead of
        // the transcript's own scrolling — for two reasons. The reader has said which row
        // they are reading, so Up/Down must move *inside* it rather than moving the
        // conversation underneath; and the seam it draws says `esc closes`, so Esc must
        // mean that while it is up. It used to lose Esc to the scrollback arm above,
        // which meant a reader who was parked in the history *and* had a window open got
        // the transcript un-parked instead — a panel on the screen advertising a key that
        // had just done something else. Ahead of the decision ladder too, because a
        // payload view is opened deliberately and a permission that arrives while it is
        // open should not steal the arrows from under it.
        //
        // The same bargain the subagent-output pane makes, and the rule behind both:
        // whichever surface prints `esc closes` owns Esc, and only one can be up.
        if self.payload_sel.is_some() {
            /// How far one press pages. The same unit the transcript scrolls by.
            const BY: usize = 10;
            match k {
                Key::Esc => {
                    self.payload_sel = None;
                    self.payload_page = 0;
                    self.redraw = true;
                    return None;
                }
                Key::Up | Key::PageUp => {
                    self.page_payload(true, BY);
                    return None;
                }
                Key::Down | Key::PageDown => {
                    self.page_payload(false, BY);
                    return None;
                }
                // The ends, which a long build log is read from as often as its head.
                Key::Home => {
                    self.page_payload(true, usize::MAX);
                    return None;
                }
                Key::End => {
                    self.page_payload(false, usize::MAX);
                    return None;
                }
                _ => {}
            }
        }
        // **An open decision owns Up/Down and a bare Enter.**
        //
        // Before the composer, because while a prompt is on the screen those keys mean
        // the ladder and cannot sensibly mean anything else -- the same argument the
        // picker arm above already makes for a bare row number.
        //
        // **Up/Down move the ladder whether or not a line is being typed**, and
        // this used to require an empty composer so that "a half-typed line still
        // scrolls and still edits". The cost of that was not visible until the
        // operator hit it: a permission arrives while you are typing, and the only
        // way to reach the menu is to empty the composer first — so the words you
        // were writing are the price of choosing an option. Their words,
        // 2026-09-20: *"suppose i type a prompt and permission ask arrives — until
        // i press down arrow I wont get into the permissions menu, by which time
        // my prompt is erased and gone"*.
        //
        // The quit card and the jobs pane in this same file take Up/Down
        // unconditionally — they no longer gate Enter, which is now every
        // pane's (see the arm that closes the composer to it below); the ladder
        // was the odd one out for the arrows.
        // Nothing is taken from the composer, because a one-line composer does not
        // edit with Up/Down — what moves aside is scrollback scrolling, for as long
        // as an ask is open, and PageUp/PageDown still do that.
        //
        // Enter and the digits keep the empty-composer guard, and for a reason that
        // is the opposite of this one: with a typed line, Enter is `submit`'s, which
        // answers the marked row and HOLDS the words — a permission arriving
        // mid-typing must not turn Enter into "send the half-thought" — and a line
        // being typed keeps its digits.
        if !self.open.is_empty() {
            let n = decision_rows(&self.open[0]);
            let typing = !self.editor.text().is_empty();
            match k {
                Key::Up if n > 0 => {
                    self.sel = if self.sel == 0 { n - 1 } else { self.sel - 1 };
                    self.redraw = true;
                    return None;
                }
                Key::Down if n > 0 => {
                    self.sel = (self.sel + 1) % n;
                    self.redraw = true;
                    return None;
                }
                Key::Enter if n > 0 && !typing => {
                    return self.answer_marked();
                }
                // A row number is the row, and answering it — see `digit_row`.
                _ if !typing && digit_row(&k, n).is_some() => {
                    self.sel = digit_row(&k, n).unwrap();
                    return self.answer_marked();
                }
                _ => {}
            }
        }

        // **An open session picker owns Up and Down, and Enter on an empty line.**
        //
        // After the decision ladder, which keeps precedence while a prompt is up. The
        // number path is untouched — digits still land in the composer and Enter
        // still answers them — but the list is on the screen, so the arrows move the
        // cursor on it rather than the caret in a composer the picker is covering.
        // The empty-composer rule is the decision ladder's own: a half-typed id's
        // Enter still means the id.
        if self.picker && !self.sessions.is_empty() {
            let rows = self.session_rows();
            let n = rows.len();
            match k {
                Key::Up => {
                    self.picker_sel = if self.picker_sel == 0 {
                        n - 1
                    } else {
                        self.picker_sel - 1
                    };
                    self.redraw = true;
                    return None;
                }
                Key::Down => {
                    self.picker_sel = (self.picker_sel + 1) % n;
                    self.redraw = true;
                    return None;
                }
                // **A conversation with sub-sessions opens and closes**, the two gestures a tree
                // uses everywhere — and the reason a collapsed default can afford to be collapsed.
                // Guarded on an empty composer, like every other pane's arrows.
                Key::Right if self.editor.text().is_empty() => {
                    let id = self.sessions[rows[self.picker_sel.min(n - 1)].idx]
                        .session_id
                        .clone();
                    if !self.expanded.iter().any(|e| *e == id) {
                        self.expanded.push(id);
                        self.redraw = true;
                    }
                    return None;
                }
                Key::Left if self.editor.text().is_empty() => {
                    let row = rows[self.picker_sel.min(n - 1)];
                    // **A child closes its PARENT**, so `←` means *back up the tree* rather than
                    // nothing at all on the row you just arrived at.
                    let want = match row.depth {
                        0 => Some(self.sessions[row.idx].session_id.clone()),
                        _ => self.sessions[row.idx].parent_session_id.clone(),
                    };
                    if let Some(want) = want
                        && let Some(pos) = self.expanded.iter().position(|e| *e == want)
                    {
                        self.expanded.remove(pos);
                        self.redraw = true;
                    }
                    return None;
                }
                Key::Enter if self.editor.text().is_empty() => {
                    let id = self.sessions[rows[self.picker_sel.min(n - 1)].idx]
                        .session_id
                        .clone();
                    return self.switch_to(id);
                }
                Key::Click { y, .. } => {
                    // The same arithmetic the screen did: the optional session
                    // header takes a row, then the picker's title and a blank,
                    // then the sessions. Only a row the last render actually
                    // drew is trusted — `picker_rows_drawn` knows where
                    // `truncate(room)` cut the list off, so a click into the
                    // blank space under a truncated list moves nothing.
                    let header_rows =
                        usize::from(self.screen_rows >= 6 && !self.session_id.is_empty());
                    let first = header_rows + 2;
                    let row = usize::from(y).saturating_sub(first);
                    if row < self.picker_rows_drawn.saturating_sub(2) {
                        self.picker_sel = row.min(n - 1);
                        self.redraw = true;
                    }
                    return None;
                }
                _ => {}
            }
        }

        // **An open mode picker owns Up and Down, and Enter on an empty line.**
        //
        // The session picker's twin, one question narrow: the mode this session
        // runs under. It sits after the session picker, which keeps precedence
        // while both are up — though neither ever is, each opener closing the
        // other — and after the decision ladder for the same reason. The
        // empty-composer rule is the ladder's own: a half-typed line's Enter
        // still means the line, and the typed path lands in `pick_mode` through
        // `submit`.
        // **The quit card owns the keys while it is open**, ahead of every other
        // list: it was opened by a key that means "I am leaving", and a stray
        // arrow landing in the transcript under it would be a keystroke the
        // operator aimed at the card.
        if self.quit_card {
            match k {
                Key::Up | Key::Down => {
                    self.quit_sel = 1 - self.quit_sel.min(1);
                    self.redraw = true;
                    return None;
                }
                _ if self.editor.text().is_empty() && digit_row(&k, 2).is_some() => {
                    self.quit_sel = digit_row(&k, 2).unwrap();
                    self.quit_card = false;
                    self.quit = true;
                    return Some(if self.quit_sel == 0 {
                        Action::Quit
                    } else {
                        Action::StopDaemon
                    });
                }
                Key::Enter => {
                    self.quit_card = false;
                    self.quit = true;
                    return Some(if self.quit_sel == 0 {
                        Action::Quit
                    } else {
                        Action::StopDaemon
                    });
                }
                // Esc is "I did not mean to leave", which is the answer a card
                // like this has to have — the alternative is an operator who
                // hit Ctrl+C twice by habit and cannot take it back.
                Key::Esc | Key::CtrlC => {
                    self.quit_card = false;
                    self.say("staying");
                    self.redraw = true;
                    return None;
                }
                _ => {}
            }
        }
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
                    return None;
                }
                Key::Down if n > 0 => {
                    self.mode_sel = (self.mode_sel + 1) % n;
                    self.pick_unseeded = false;
                    self.redraw = true;
                    return None;
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
                        return None;
                    }
                    let name = choices[self.mode_sel.min(n - 1)].clone();
                    return self.take_pick(name);
                }
                _ if self.editor.text().is_empty() && digit_row(&k, n).is_some() => {
                    let at = digit_row(&k, n).unwrap();
                    self.mode_sel = at;
                    return self.take_pick(choices[at].clone());
                }
                Key::Click { y, .. } => {
                    // The arithmetic the last frame did: the card's first
                    // choice sat at `mode_first_row`, and only a row the card
                    // provably drew in full is trusted — `mode_rows_drawn` is
                    // zero when the fit loop or the backstop cut the card, so
                    // a click into a list nobody saw whole moves nothing.
                    let row = usize::from(y).saturating_sub(self.mode_first_row);
                    if n > 0 && row < self.mode_rows_drawn {
                        self.mode_sel = row.min(n - 1);
                        // A click is the reader's too, for the reason the arrows are.
                        self.pick_unseeded = false;
                        self.redraw = true;
                    }
                    return None;
                }
                _ => {}
            }
        }

        // **An open subagent pane owns Up and Down, and ENTER IS THE SWITCH INTO THAT
        // SUBAGENT'S SESSION.** One keystroke, because that is what entering a row means
        // everywhere else in this head and going into a subagent *is* going to that
        // session — the operator, having driven into one and then been unable to get out
        // again: *"when I \"Enter\" Subagent it is like completely switching session"*,
        // and *"so after o I couldnt just Esc from the subagent — had to switch back here
        // via session. Which narrows the subagent prompt - make \"o\" to \"Enter\""*.
        //
        // `o` stays as an alias for the same act: it is the key this pane has always used
        // to move the head, and a hand that learned it must not have to learn something
        // new. What moved is the READ — `p` now, and `p` is `/peek ID`'s own key. Reading
        // a child's output without leaving the session is a real thing to want (R20's whole
        // argument for the `Peek` frame), and it must not be the thing Enter does when the
        // operator means to go there. It is neither Enter nor Esc, which is what the two
        // gestures had to be kept apart from.
        //
        // **And the group row is the third thing Enter means here.** The finished children live
        // under a fold (see [`App::subagent_stops`]); Enter on that row unfolds them, which is
        // the same *Enter acts on what the cursor is on* rule the todos pane keeps. The arrows
        // also scroll the cursor into view now — the fix for the operator's other report,
        // *"subagents panel doesnt scroll"*, which was a child appended below the fold and no
        // key that would bring it up.
        if self.subagents_pane {
            let stops = self.subagent_stops();
            if !stops.is_empty() {
                let n = stops.len();
                let at = self.subagents_sel.min(n - 1);
                match k {
                    Key::Up => {
                        self.subagents_sel = if at == 0 { n - 1 } else { at - 1 };
                        self.scroll_into_view(self.subagents_row_of());
                        self.redraw = true;
                        return None;
                    }
                    Key::Down => {
                        self.subagents_sel = (at + 1) % n;
                        self.scroll_into_view(self.subagents_row_of());
                        self.redraw = true;
                        return None;
                    }
                    // **Enter takes the row, and `o` is the same act as the alias this pane
                    // has always used.** One arm for one behaviour: Enter is unconditional (a
                    // pane owns Enter), and `o` keeps the composer's claim on a letter that is
                    // being typed — half a word on the line falls through to the composer, which
                    // is why the guard is here rather than in a second copy of these six lines.
                    Key::Enter | Key::Char('o')
                        if matches!(k, Key::Enter) || self.editor.text().is_empty() =>
                    {
                        match stops[at] {
                            // **The group row is a fold, not a session**: nobody to switch into,
                            // so Enter is the unfold.
                            SubStop::Finished => {
                                self.subagents_finished_open = !self.subagents_finished_open;
                                self.redraw = true;
                                return None;
                            }
                            SubStop::Agent(i) => {
                                // **Moving, not reading.** The row is a session and Enter goes to
                                // it; one that is not open yet is refused here, by name, rather
                                // than bounced off the daemon.
                                let row = &self.subagents[i];
                                if row.state == "opening" {
                                    // Nothing to attach to yet, and the daemon would refuse the
                                    // switch by name anyway; saying it here keeps the operator in
                                    // the pane they were using rather than bouncing them through
                                    // a rejection.
                                    self.say(
                                        "that subagent is still opening — nothing to attach to yet",
                                    );
                                    self.redraw = true;
                                    return None;
                                }
                                let id = row.session_id.clone();
                                self.subagents_pane = false;
                                return self.switch_to(id);
                            }
                        }
                    }
                    // **Reading, not moving: the output pane opens on the `Peeked` reply and
                    // this head never leaves the session it is in.** Nothing to read under the
                    // fold header, so `p` on it falls through.
                    Key::Char('p') if self.editor.text().is_empty() => match stops[at] {
                        SubStop::Finished => {}
                        SubStop::Agent(i) => {
                            let row = &self.subagents[i];
                            if row.state == "opening" {
                                self.say("that subagent is still opening — nothing to read yet");
                                self.redraw = true;
                                return None;
                            }
                            let id = row.session_id.clone();
                            self.sub_out_pending = Some(id.clone());
                            return Some(Action::Peek(id));
                        }
                    },
                    _ => {}
                }
            }
        }

        // **An open todos pane owns Up and Down, and Enter unfolds the item.**
        //
        // The items carry the detail a TODO.md puts under them — the commit a
        // vendoring pins, the `Deps:` that says what blocks it — and the pane
        // showed the first line only, so an item trailed off mid-sentence. Arrows
        // move, Enter acts: the same two the jobs and subagent panes use.
        // **ONE ENUMERATION, AND EVERY KEY READS IT** — leticl's `todos-stops`, whose docstring is
        // the operator's two reports: *"arrows dont go here"* and *"mouse doesnt click"*. Both were
        // the same defect, a cursor whose position came from one list and whose row came from
        // another. The stops are the add control, the operator's own items, and the repo's items —
        // and NOT the model's rows, which no key acts on.
        //
        // **A key this block does not name FALLS THROUGH**, which is what keeps the pane from
        // eating the composer: `Tab` is the completion key while a `/command` is half-typed (the
        // guard below is the same empty-composer one the card uses), and every ordinary character
        // is the operator's to type. The first cut of this block ended in an unconditional
        // `return None` and the pane swallowed the whole keyboard — a `▸` that looked right with
        // nothing behind it, which is a worse defect than the one it replaced.
        if self.todos_pane {
            let stops = self.todos_stops();
            let n = stops.len();
            let at = self.todos_sel.min(n.saturating_sub(1));
            match k {
                Key::Up => {
                    self.todos_sel = if at == 0 { n - 1 } else { at - 1 };
                    self.todos_sel = self.todos_sel.min(n - 1);
                    self.sync_repo_from_stop(&stops);
                    self.scroll_into_view(self.todos_row_of());
                    self.redraw = true;
                    return None;
                }
                Key::Down => {
                    self.todos_sel = (at + 1) % n;
                    self.sync_repo_from_stop(&stops);
                    self.scroll_into_view(self.todos_row_of());
                    self.redraw = true;
                    return None;
                }
                // **Enter acts on what the cursor is ON**, which is the whole point of one
                // enumeration: the add control opens the card, one of your rows is marked done,
                // and a repo item unfolds. Tab stays the repo's unfold, because the hand is
                // already there for it and the composer is empty here.
                // **A click moves the cursor, and Enter still does the act** — the same two
                // acts, kept two, that the pickers here already keep (*"select and confirm stay
                // two acts"*). Straight off the recorded rows, so the row the pointer is on is
                // the row the pane drew there.
                //
                // **A click into the blank space moves nothing**, and so does one on a row that
                // is not a stop at all — one of the model's rows, or a repo heading. There is no
                // key that would act on it, which is the same reason it is not a stop.
                Key::Click { y, .. } => {
                    if let Some(sel) = self.todo_stop_at_row(y) {
                        self.todos_sel = sel;
                        self.sync_repo_from_stop(&stops);
                        self.redraw = true;
                    }
                    return None;
                }
                Key::Enter | Key::Tab if self.editor.text().is_empty() => match &stops[at] {
                    TodoStop::Add => {
                        self.open_todo_card();
                        return None;
                    }
                    // **Marking done is the model's act too and it is the operator's own row**:
                    // the daemon's `set_operator_states` is its own door and `/todo done N` is the
                    // typed one. Here it is the same act under the cursor. A completed row can be
                    // reopened, because a cursor that can only go one way is a cursor you cannot
                    // correct.
                    TodoStop::Mine(content) => {
                        let content = content.clone();
                        let mut mine = self.operator_todos();
                        if let Some(t) = mine.iter_mut().find(|t| t.content == content) {
                            t.status =
                                if t.status == letibot_sessionlog::event::TodoStatus::Completed {
                                    letibot_sessionlog::event::TodoStatus::Pending
                                } else {
                                    letibot_sessionlog::event::TodoStatus::Completed
                                };
                        }
                        self.say("toggled");
                        self.echo_operator_todos(mine.clone());
                        return Some(Action::SetOperatorTodos(mine));
                    }
                    TodoStop::Repo(i) => {
                        self.repo_sel = *i;
                        self.repo_open = !self.repo_open;
                        self.scroll_into_view(self.todos_row_of());
                        self.redraw = true;
                        return None;
                    }
                },
                _ => {}
            }
        }

        // **An open queue pane owns Up and Down, and Enter opens the row it is on.** The rows
        // are ONE enumeration (`merge`, in the daemon's own order), so the drawn cursor, the
        // arrows and Enter cannot disagree.
        if self.queue_pane && self.queue_open.is_none() {
            if !self.merge.is_empty() {
                let n = self.merge.len();
                let at = self.queue_sel.min(n - 1);
                match k {
                    Key::Up => {
                        self.queue_sel = if at == 0 { n - 1 } else { at - 1 };
                        self.scroll_into_view(self.queue_row_of());
                        self.redraw = true;
                        return None;
                    }
                    Key::Down => {
                        self.queue_sel = (at + 1) % n;
                        self.scroll_into_view(self.queue_row_of());
                        self.redraw = true;
                        return None;
                    }
                    Key::Enter => {
                        // **What Enter opens is the ENTRY, and everything the queue knows
                        // about it**: the ask it was produced under, the state and its reason
                        // — which is the gate's own words when the gate refused it, and the
                        // reviewer's verdict when the reviewer did — and the verdict's
                        // evidence. Nothing here is a second read: the row carries the
                        // evidence and the snapshot carries the verdict, so the overlay is
                        // the two rows the pane already holds, in full.
                        self.queue_open = Some(self.merge[at].id.clone());
                        self.pane_scroll = 0;
                        self.redraw = true;
                        return None;
                    }
                    // **A click moves the cursor, and Enter still opens the entry** — the same
                    // two acts the pickers here keep (*select and confirm stay two acts*),
                    // straight off the rows the pane recorded while drawing.
                    Key::Click { y, .. } => {
                        if let Some(sel) = self.queue_stop_at_row(y) {
                            self.queue_sel = sel;
                            self.redraw = true;
                        }
                        return None;
                    }
                    _ => {}
                }
            }
        }

        // **An open jobs pane owns Up and Down, and Enter reads the row it is on — or folds the
        // group.** The same shape the subagents pane keeps, and for the same reason the
        // operator gave: *"jobs panel - same as subagents - show list of running, group
        // finished"*. The rows are ONE enumeration ([`App::job_stops`]), so the drawn cursor,
        // the arrows and Enter cannot disagree — and the arrows scroll the cursor into view, so
        // a job below the fold is reachable.
        if self.jobs_pane {
            let stops = self.job_stops();
            if !stops.is_empty() {
                let n = stops.len();
                let at = self.jobs_sel.min(n - 1);
                match k {
                    Key::Up => {
                        self.jobs_sel = if at == 0 { n - 1 } else { at - 1 };
                        self.scroll_into_view(self.jobs_row_of());
                        self.redraw = true;
                        return None;
                    }
                    Key::Down => {
                        self.jobs_sel = (at + 1) % n;
                        self.scroll_into_view(self.jobs_row_of());
                        self.redraw = true;
                        return None;
                    }
                    Key::Enter => match stops[at] {
                        // **The group row is a fold, not a job**: nobody to read.
                        JobStop::Finished => {
                            self.jobs_finished_open = !self.jobs_finished_open;
                            self.redraw = true;
                            return None;
                        }
                        JobStop::Job(i) => {
                            if self.session_id.is_empty() {
                                self.say("not attached to a session yet");
                                self.redraw = true;
                                return None;
                            }
                            let row = &self.jobs[i];
                            let job = row.id.clone();
                            // **Where its output went, when it did not come here** (R41). The
                            // window for a redirected job is empty by construction, so the
                            // pane needs the file's name to say anything true at all — see
                            // [`JobOut::redirect`].
                            let redirect = row.redirect.clone();
                            // **The output opens in a pane, not in the conversation.**
                            // This used to return `Action::Slash { "job {job}" }`, whose
                            // reply is a `Warning` on the session log — so the pane closed
                            // and the operator read a build log scrolling past in the chat.
                            // The operator, 2026-09-20: *"when i press enter on jobs pane im
                            // not shown the job output im brought back to the main
                            // conversation with /job <id> posted - this is not what i
                            // want"*. Now the read is a `ReadJobOutput`: it comes back as a
                            // `JobOutput` event with the offsets attached, and the overlay
                            // draws it. The jobs list stays behind it, so Esc returns here.
                            self.job_out = Some(JobOut {
                                job: job.clone(),
                                state: String::new(),
                                never_ran: false,
                                redirect,
                                from: 0,
                                to: 0,
                                produced: 0,
                                dropped: 0,
                                lines: Vec::new(),
                                next: None,
                                back: Vec::new(),
                                scroll: 0,
                                loading: true,
                                error: None,
                            });
                            self.redraw = true;
                            return Some(Action::ReadJobOutput { job, offset: 0 });
                        }
                    },
                    _ => {}
                }
            }
        }
        // **Up with an empty composer recalls the queued line.**
        //
        // The echo above the composer is the operator's own words, held only
        // until the next step boundary — and the one thing this head can do
        // about a message it already sent is take it back before it lands. Up
        // is the key readline taught for "the previous entry", and behind a
        // running turn the queue is one entry now. The take-back rides with the
        // recall: the daemon drops the queued prompts and the held operator
        // text, so the edited resend replaces the original instead of stacking
        // onto it. Parked in the scrollback, Up still scrolls — reading history
        // is what the operator is there for — and a half-typed line keeps the
        // editor's own Up: readline history, not the queue's recall. No take-back
        // rides on browsing history, or one press of Up behind a running turn
        // would silently drop the queue under the operator.
        if matches!(k, Key::Up)
            && self.editor.text().is_empty()
            && self.scroll == 0
            && !self.pending_prompts.is_empty()
        {
            let text = self.pending_prompts.join("\n");
            self.pending_prompts.clear();
            self.set_composer(&text);
            self.redraw = true;
            return Some(Action::WithdrawPrompts);
        }

        // Tab: completion, dispatched by the line's first character. A `/` line
        // completes a command; a `!` line completes from what this session has
        // actually run. The composer's own keys run after it because Tab means
        // nothing to the editor — its byte used to be eaten by the decoder — and
        // every other key leaves a running completion cycle alone: it re-validates
        // its prefix the next time Tab is pressed, so there is nothing to reset in
        // each arm here.
        if let Key::Tab = k {
            if self.editor.text().starts_with('!') {
                self.complete_shell();
            } else {
                self.complete_slash();
            }
            self.redraw = true;
            return None;
        }

        // **Parked in the scrollback, the arrows belong to the scrollback — and the composer
        // does not get them.**
        //
        // The intent was already written down two arms above (*"parked in the scrollback, Up
        // still scrolls"*) and it was never true. The composer is asked FIRST, and an empty
        // composer hands Up straight to the editor's `recall(true)`: that walks readline history
        // and answers `Changed`, so the transcript's own fallback below was reached only by the
        // keys the editor had no use for.
        //
        // MEASURED in the operator's own window, 2026-09-27, on a head that had been up for
        // hours — the three lines are the banner, then three presses of the key it names:
        //
        // ```text
        //   ── holding your place · 138 line(s) below … ↓ to the bottom or esc follows again
        //   Down ×3   ·  139 line(s) below                      ← the number did not move
        //   Up        ·  composer: "on this letibot head scrolll is …"
        //   Down ×3   ·  composer: "yeah scrol…"
        // ```
        //
        // Four presses of the keys the banner advertises, and the reader was no closer to the
        // bottom while an old prompt had appeared in the box. Their report: *"no way to scroll
        // back to the bottom, stuck at holding"*. It was not stuck — the arrows were being spent
        // on history, and a head with a long session had hundreds of entries to spend them on.
        //
        // **And ↓ is the whole of the way back, in one press.** A key that moves one line cannot
        // out-run a stream that adds lines faster, so *"↓ to the bottom"* was unreachable by
        // design as well as by the ordering: three ↓'s against a generating turn moved the count
        // by nothing measurable. The reference does not move one line either — `%normal-key`'s
        // `:down` sets the scroll to ZERO when the reader is parked: *"parked in the scrollback,
        // ↓ follows the stream again — it is what the banner says it does; only then does it move
        // inside the prompt"*. So this arm does what this head's banner already promised, and
        // what `esc` does beside it.
        //
        // **Esc is the way back to the composer**, which is the other key the banner names, and
        // the reason taking ↑ is safe: a reader who wants their draft's arrows back has an
        // advertised key for it rather than a hunt.
        if !self.following() && self.todo_draft.is_none() {
            match k {
                Key::Up => {
                    self.scroll_up(1);
                    self.redraw = true;
                    return None;
                }
                Key::Down => {
                    self.anchor = None;
                    self.scroll = 0;
                    self.redraw = true;
                    return None;
                }
                _ => {}
            }
        }

        // **An open pane owns Enter, even when it has nothing to act on.**
        //
        // The operator, 2026-10-03, having gone to the jobs pane and pressed Enter with a
        // stray character in the composer: *"the pane own keyboard in a way, so enter is a
        // pane thing."* That is the rule this arm is, and it is the rule the arms above now
        // keep — each of them used to gate its own Enter on `editor.text().is_empty()`, so
        // **a pane's Enter silently became "send what I was typing"** the moment there was
        // anything in the composer. What they read as a keystroke aimed at the pane was
        // sent to the model.
        //
        // This is the second half of that: the panes whose blocks above are conditional on
        // having rows (`jobs_pane && !self.jobs.is_empty()`, and the subagent tree's twin)
        // do not run at all over an empty list, and `help`, `stats` and the two overlay
        // screens have no Enter arm. Without this, Enter in an empty jobs pane is still the
        // composer's, which is the same defect with one row fewer on the screen.
        //
        // **What is deliberately NOT here.** The pickers, the mode picker, the decision
        // ladder and the todos stops: for those, a typed line IS the answer — a row number,
        // an id prefix, a name — and `submit` routes it to the right one and holds the
        // words. Swallowing Enter there would break the typed path those panes advertise.
        // The rule is *the pane owns the key*, not *the composer is dead*: a pane that
        // names no meaning for Enter takes it anyway, and one that names a meaning for the
        // typed line keeps it.
        //
        // **Nor is the ctrl-v window**, which was here and should not have been. It is not
        // an overlay over the composer; it is a row of the conversation opened wide, with
        // the composer live underneath and no Enter of its own to lose. Holding Enter for
        // it meant the operator could not send until they closed it: *"untill i hit ctrl-v
        // again - i couldnt sent my new prompt"*. A typed line now goes, and the window
        // closes with it — the reader has moved on to the next turn, and the arrows go
        // back to the conversation and the composer's history. An empty Enter still does
        // nothing, as it does with no window open.
        if matches!(k, Key::Enter) && self.payload_sel.is_some() {
            if self.editor.text().trim().is_empty() {
                return None;
            }
            self.payload_sel = None;
            self.payload_page = 0;
            self.invalidate_history();
            self.redraw = true;
        }
        if matches!(k, Key::Enter)
            && (self.help
                || self.stats
                || self.jobs_pane
                || self.subagents_pane
                || self.slash_out.is_some())
        {
            return None;
        }

        // **A single Esc inside a subagent's session is the way back UP the tree.**
        //
        // `ctrl-s` and a row's Enter was the only way back before this, and it is the
        // gesture the operator had to invent: *"so after o I couldnt just Esc from the
        // subagent — had to switch back here via session … make sure a single Esc goes up
        // to subagents list"*. Enter goes down a level (the pane's arm), Esc comes back
        // up, and the list you came out of is on the screen when you land — which is the
        // tree walk, with no state kept beyond the parent link the daemon already sends.
        //
        // # It sits HERE, after everything else that claims Esc
        //
        // Because Esc already means four things in this head and every one of them keeps
        // its contract:
        //
        // * **Esc closes a pane** — the arm near the top of this function closes help,
        //   the picker, the todos/jobs/subagents/config panes, a `pick` and the quit card,
        //   and *whichever surface prints `esc closes` owns Esc*. None of those says
        //   `esc goes up`, so none of them is shaved: this arm is below all of them.
        // * **Esc un-parks the scrollback**, **Esc closes a payload window** and **Esc
        //   closes a slash listing**, whose seams print exactly that — the arms above,
        //   likewise untouched.
        // * **Esc-Esc is the interrupt frame**, and it is armed by the composer's own
        //   editor below. That one *is* changed while this head is inside a subagent, and
        //   it is the conflict rather than an oversight — see the note under this arm.
        //
        // The empty-composer and no-decision guards are the ones every pane key in this
        // file uses: while a permission is on the screen, or words are half-typed, Esc is
        // not available to mean *up*.
        //
        // # The conflict, reported rather than taken
        //
        // Esc-Esc is the ONLY interrupt key, and it is the editor's (five seconds, two
        // presses). A single Esc that leaves the session and a first Esc that arms an
        // interrupt are the same keystroke, so inside a subagent the arm is what it is:
        // **one Esc goes up, and the pair no longer interrupts the child from inside the
        // child.** Two things bound the cost, and both are existing, tested behaviour
        // rather than something added here: the child's turn can still be interrupted
        // *from the child* with `/interrupt`, and from the parent with `job_kill HANDLE`,
        // which is how this tree already documents stopping a subagent (`harness.rs`: *"a
        // subagent is stopped by interrupting the turn it runs"*). Nothing else about the
        // frame moves: in a session that is not a subagent there is no parent to go to and
        // this arm does not fire, so Esc-Esc is byte for byte what it was.
        //
        // **The one thing this arm does to the frame here, with Esc gone up:** the editor
        // never sees this press, so it is not counted as the first of a pair. That is the
        // true statement — the keystroke went to the tree and not to the composer — but it
        // has a second edge: an Esc the operator pressed in the PARENT, inside the five
        // seconds, can pair with the NEXT press after an up-and-down round trip, and that
        // second press would interrupt the parent. Three presses across a switch inside
        // one window, and it needs the descent arm's press to have been taken by this arm
        // too; the parent presses are otherwise untouched. It is written down rather than
        // fixed because the fix is a way for a head to disarm the editor's pair, and
        // `Editor` publishes no such call — inventing one is a change to the UI crate for
        // a window narrower than the keystroke that opens it.
        if matches!(k, Key::Esc)
            && self.editor.text().is_empty()
            && self.open.is_empty()
            && let Some(parent) = self.parent_session()
        {
            // The rows the operator climbed out of are the ones the pane shows, so the
            // pane opens and the cursor lands on the child they came from — the row is
            // found by id once the parent's `Hello` has rebuilt the list
            // ([`App::fold_subagents`]), because an index taken now would be an index
            // into the rows of the session being left.
            self.subagents_pane = true;
            self.pane_scroll = 0;
            self.up_from = Some(self.session_id.clone());
            return self.switch_to(parent);
        }

        let now = self.now_ms;
        let cols = self.composer_cols();
        let reaction = match k {
            Key::Up => self.editor.vertical(true, cols),
            Key::Down => self.editor.vertical(false, cols),
            ref other => self.editor.key(other.composer()?, now),
        };
        match reaction {
            Reaction::Submit(text) => self.submit(text),
            Reaction::Interrupt => {
                // **Busy, not generating.** This gate read the state name, so it was dead for the
                // whole of every command — which is the only time anybody reaches for it. Measured
                // on leticl's head: esc esc during a `sleep 60`, and forty seconds later still
                // `Responding · 42.0s` with the call running. A turn whose only work is a running
                // tool call is exactly the turn a person wants to interrupt.
                if self.turn_busy() {
                    // Interrupt is not quit. A shared session's interrupt is
                    // announced with the issuer, so it must be a deliberate act —
                    // and two presses of Esc inside five seconds is one.
                    Some(Action::Interrupt("operator pressed esc twice".into()))
                } else {
                    self.say("nothing is running");
                    None
                }
            }
            // **The second Ctrl+C asks instead of leaving.** It used to detach
            // and that was the only thing it could do; an operator who wanted
            // the daemon stopped as well needed a second terminal and
            // `letibot --stop`. The card is one keystroke either way and it
            // makes the irreversible half a choice rather than a default.
            Reaction::Quit => {
                self.quit_card = true;
                self.quit_sel = 0;
                self.redraw = true;
                None
            }
            Reaction::Changed => None,
            // The composer had no use for it. Up and Down then belong to the
            // transcript; see the note above.
            Reaction::Idle => {
                match k {
                    // `redraw` here is not "rebuild the frame" — the loop rebuilds
                    // one every pass. It is "throw the glass away", and without it
                    // a scroll repaints only the rows whose TEXT differs, which for
                    // a window that slid by one line over similar rows can be
                    // almost none of them. Every other state change in this file
                    // sets it; these two did not.
                    Key::Up => {
                        self.scroll_up(1);
                        self.redraw = true;
                    }
                    Key::Down => {
                        self.hold(1);
                    }
                    _ => {}
                }
                None
            }
        }
    }

    /// The marked row is the answer: the marker IS the thing Enter takes, the
    /// same contract the pickers keep.
    ///
    /// **Two kinds on one card, and they answer through different frames** (§1.7). A
    /// permission's marked row is an option id and goes as `Action::Answer`; a
    /// question's is an index into the model's offered choices and goes as
    /// `Action::AnswerQuestion`. `None` when the row is not an answer: a permission
    /// that offers nothing, or a question whose model offered no choices and expects
    /// words instead — which the typed path carries, and which a bare Enter must not
    /// turn into an empty answer (the daemon refuses that, and a refusal the head
    /// could have predicted is a keystroke thrown away).
    pub(crate) fn answer_marked(&mut self) -> Option<Action> {
        let d = self.open.first()?;
        let n = decision_rows(d);
        if n == 0 {
            return None;
        }
        let at = self.sel.min(n - 1);
        if d.kind == "question" {
            return Some(Action::AnswerQuestion {
                req_id: d.req_id.clone(),
                answer: letibot_sessionlog::question::QuestionAnswer::choosing(at),
            });
        }
        let option_id = d.options[at].option_id.clone();
        let req_id = d.req_id.clone();
        Some(Action::Answer {
            req_id,
            option_id,
            // The ladder is the no-glob path by construction: there is nothing
            // typed to read one from. A glob is given by typing
            // `allow_always <pattern>` on the line, and a reason the same way
            // with `deny_and_tell <why>` — a `deny_and_tell` taken from the
            // ladder alone denies without a reason and says so, which is honest
            // about what was actually given.
            pattern: None,
            note: None,
        })
    }

    /// **A pane with no daemon is a pane with no program.**
    ///
    /// The pty is the *daemon's*, so a head that cannot reach the daemon cannot feed the
    /// screen and cannot forward a key: the rectangle would sit frozen on whatever the
    /// program drew last, with `ctrl-\` — the one way out, and a frame — going nowhere. The
    /// transcript is the honest thing to show, and this is the two moments it is known:
    /// the link going down, and an action the driver could not send.
    ///
    /// **The daemon's pane is not closed here**, and cannot be: the close is a frame, and the
    /// frame is exactly what cannot be sent. The program is left to the session it belongs to
    /// — the same bargain [`App::load`] makes on a switch, and the same TODO.
    ///
    /// Returns whether there was a pane, so a caller can say so once rather than per report.
    pub fn drop_pane(&mut self) -> bool {
        let had = self.term.take().is_some();
        if had {
            self.redraw = true;
        }
        had
    }

    /// Whether a pane is open **and drawn**, and therefore owns the keyboard. See [`TermPane`].
    ///
    /// The one question `Link::tick` asks before it decides whether a byte this head read is a
    /// key of its own or the program's, and it is asked of `App` because the pane is the
    /// head's state and not the terminal's.
    ///
    /// **A detached pane owns nothing**: the composer has its rows and its keys back, which is
    /// the whole point of leaving. The two questions are deliberately different — *is there a
    /// program here* ([`App::term`]) and *is it on the screen* (this) — and a head that answered
    /// both with one predicate would keep the keyboard for a pane nobody can see.
    pub fn pane_open(&self) -> bool {
        self.term.as_ref().is_some_and(|p| !p.detached)
    }

    /// **Does this head hold a pane at all** — drawn, or detached and still running.
    ///
    /// The third of the three questions about a pane, and the one about the PROGRAM rather
    /// than about the screen: [`App::pane_open`] is *is it being drawn*, [`App::pane_keys`] is
    /// *who gets the keys*, and this is *is there a process behind it*. They come apart in
    /// exactly one state — a detach — and that state is the whole of protocol 34's split: the
    /// pane is held (so `!term` attaches back and `!term close` has something to end) and it is
    /// not drawn (so the conversation has the rectangle and the composer has its keys).
    ///
    /// Public because an integration test that drives the head through the driver's own loop
    /// has to be able to ask it: `pane_open()` answers `false` for a detached pane, and a test
    /// that read *no pane* off it would pass whether the detach kept the program or killed it.
    pub fn holds_pane(&self) -> bool {
        self.term.is_some()
    }

    /// **The pane this session has that this head is not drawing**, as a line a person reads —
    /// or `None` when there is nothing to report.
    ///
    /// Two sources, and the order is the decision: **this head's own pane first** (a detach
    /// keeps the pane, so the head holds the program *and* the line the operator typed), and
    /// then **the daemon's answer** to `ClientFrame::TermStatus`, which is the only thing that
    /// can answer for a session this head has no pane in — a head that switched away and came
    /// back, or a second head attached to the same session.
    ///
    /// **Not a row, and that is the operator's rule.** A detach is not an event: there is no
    /// `SessionEvent` for it, nothing durable happened, and a transcript row saying *you left a
    /// pane* would be a disclosure about a moment that did not change anything. What the head
    /// draws instead is this — a fact about **now**, drawn while it is true and gone the moment
    /// it stops being true.
    pub(crate) fn pane_behind(&self) -> Option<String> {
        // On the screen: the pane itself is the fact, and a sentence about it would be the same
        // fact twice.
        if self.pane_open() {
            return None;
        }
        match (&self.term, &self.term_fact) {
            (Some(p), _) => Some(p.line.clone()),
            (None, PaneFact::Running(command)) => Some(format!("!term {command}")),
            (None, PaneFact::None | PaneFact::Unasked) => None,
        }
    }

    /// **The pane's keyboard: the raw bytes the reader consumed, turned into actions.**
    ///
    /// # The way out is found HERE, and that is what makes it untrappable
    ///
    /// `0x1c` — `Ctrl-\` — is looked for in the byte stream **before anything is forwarded**,
    /// and the bytes before it are the last thing the program gets. A key the program never
    /// receives is a key no program can trap, whatever it does to `SIGQUIT` or to its own input
    /// handling. **And the act it performs is a detach**: the head hides the rectangle and sends
    /// nothing at all, so the program is not signalled, not killed, and not even told — see
    /// [`TermPane`] for why the default is the non-destructive one.
    ///
    /// **The byte cannot be part of anything else.** `0x1c` is below `0x20`, so it is not a
    /// UTF-8 continuation and cannot appear inside a character; and it is not a CSI final byte
    /// (those are `0x40`-`0x7e`), so it cannot appear inside an escape sequence the reader is
    /// holding. It *can* appear inside a bracketed paste — somebody pasting a file that
    /// contains a literal `0x1c` — and the pane detaches: the honest reading of *the operator's
    /// terminal sent the way-out byte*, and a hole named in [`TermPane`] rather than a
    /// silent one.
    pub fn pane_keys(&mut self, raw: &[u8]) -> Vec<Action> {
        let Some(p) = self.term.as_ref() else {
            return Vec::new();
        };
        // An ending is in flight: between the confirmed `!term close` and the daemon's
        // `TermEnded` there is a kill on the way, and a byte written into a pty whose program is
        // being signalled is a byte nobody will read. The window is milliseconds, and this is
        // what makes it closed rather than merely short.
        //
        // **And a DETACHED pane takes no keys either**, which is the same rule from the other
        // end: the composer has its rows and its keys back, so a keystroke the operator aimed at
        // the composer must not reach a program nobody is drawing. `Link::tick` already routes
        // on [`App::pane_open`]; this is the second door, closed rather than left to the caller.
        if p.closing || p.detached {
            return Vec::new();
        }
        match raw.iter().position(|b| *b == WAY_OUT) {
            Some(at) => {
                let mut out = Vec::new();
                if at > 0 {
                    out.push(Action::TermInput {
                        bytes: raw[..at].to_vec(),
                    });
                }
                // **Detach: nothing leaves the head.** Not a frame and not a keystroke — the
                // whole point is that the program keeps running and this head stops drawing it.
                self.detach();
                out
            }
            None if raw.is_empty() => Vec::new(),
            None => vec![Action::TermInput {
                bytes: raw.to_vec(),
            }],
        }
    }

    /// **Tab on a `!` line completes from what this session has actually run — and,
    /// when the history has nothing, from what the model proposes.**
    ///
    /// The operator's own words for the feature: *"smart autocomplete here for ! -
    /// you trying to suggest me commands based on conversation context"*, and then
    /// *"i want smart ! when a model suggest completions."* The candidates are whole
    /// lines, newest first, deduped — the operator's own `!` rows verbatim, and the
    /// model's `bash` calls as `! ` plus the command they ran — and the match is a
    /// whole-line prefix, so `! ls` reaches `! ls .`.
    ///
    /// **History first, model second.** The history is the first answer, because a
    /// command this session actually ran is a fact and a model's proposal is a guess,
    /// and a real command beats an invented one. The model is the fallback, asked
    /// only when the history has no match for the prefix — or its cycle is exhausted
    /// — and asked once per (prefix, transcript position), so the same prefix asked
    /// twice is not two model calls.
    ///
    /// **The one recogniser for "is this a `!` line" is `operator_shell_command`**,
    /// the same rule the daemon re-checks at the send: a bang with nothing after it
    /// is not a `!` line, so `!` alone does nothing here, the way it is refused
    /// there. A second list of what counts would be a second answer to the same
    /// question.
    ///
    /// **The cycle is the field `complete_slash` uses, and the same rule holds**: it
    /// only trusts a prefix that is still being typed, so a character typed on after
    /// a completion matches fresh rather than clobbering what was typed, and a
    /// prefix nothing matches leaves the composer exactly as it was and says so.
    /// Nothing is ever submitted — a candidate only fills the composer.
    /// **Move the open payload window** by `by` wrapped lines, clamped to the last full
    /// page the draw recorded (`payload_max`) — so Down stops where the output ends and the
    /// first Up after it moves at once, instead of unwinding steps past the end.
    ///
    /// **And what the window cannot take, the conversation does** — the operator: *"i want
    /// them to connect. so say i scrolled to the bottom of the ctrl-v view port it should
    /// keep scrolling the main convo"*. A window at its last line passes the rest of a
    /// Down to the transcript, and one at its first line passes the rest of an Up, the way
    /// a scroll box nested in a page hands over at its edge. Home and End are jumps inside
    /// the window and pass nothing on.
    pub(crate) fn page_payload(&mut self, up: bool, by: usize) {
        let max = self.payload_max.get();
        let from = self.payload_page.min(max);
        let to = if up {
            from.saturating_sub(by)
        } else {
            from.saturating_add(by).min(max)
        };
        // `max` is only known once the window has been drawn; before that nothing chains,
        // because "at the end" is not yet a fact.
        let rest = by.saturating_sub(from.abs_diff(to));
        let chain = !(by == usize::MAX || rest == 0 || (!up && max == usize::MAX));
        // **The handover first, while the rows are still measured.** Invalidating the history
        // drops the line spans the transcript scroll finds its row by, so a scroll made
        // after it landed nowhere — found by the test: the window was at its head, Up was
        // passed on, and the conversation did not move.
        if chain {
            if up {
                self.scroll_up(rest);
            } else {
                self.hold(rest as isize);
            }
        }
        if to != from {
            self.payload_page = to;
            // **The history buffer is a cache of the rendered rows**, and a page offset
            // changes what one of those rows renders to — so `redraw` alone re-draws the
            // *old* lines.
            self.invalidate_history();
        }
        self.redraw = true;
    }
}

/// **The option a typed line names** — the answer, or why there is not one.
///
/// # Three outcomes, not an `Option`
///
/// *"nothing answers to that name"* and *"several things do"* are different answers and
/// the caller has to treat them differently: the first is the mid-typing courtesy (hold
/// the words, answer the marked row, because a permission that arrives under somebody's
/// half-thought must not turn their Enter into a wasted keystroke), and the second is a
/// refusal that must answer **nothing at all**.
///
/// # The glob, and where it may ride
///
/// > *"please add globbing to my answers somehow too"*
///
/// `allow_always crates/**/tests/*.rs` answers the permission AND says what the rule
/// should cover, instead of accepting the pattern the gate derives from the one call in
/// front of you. The two halves split on the first space; everything after it is the
/// pattern, verbatim and un-lowercased — a glob is a path and `Cargo.toml` is not
/// `cargo.toml`.
///
/// A pattern is only meaningful with `allow_always`, which is the only option that writes
/// a rule. Typed after anything else it is **refused** rather than dropped: somebody who
/// wrote `allow_once src/**` meant the rule to cover `src/**`, and silently granting one
/// call instead is the answer they did not give.
///
/// # A name that fits several options is refused, not resolved
///
/// The prefix path in [`match_option`] is a courtesy for how people actually type
/// (`deny` for `deny_and_tell`), and it stops at one: **a prefix that begins more than
/// one option id is refused, and the candidates are named.** This was
/// `.find(starts_with)`, which took the first hit in list order, so `allow` silently
/// answered `allow_once` out of `allow_once`, `allow_session`, `allow_always` — and
/// which of three grants the operator gave is the entire content of the answer. Where an
/// option sits in a list is not something they said. This is a gate: a grant invented by
/// list position is an answer nobody gave, and the audit row it writes names an option
/// the operator cannot see on the card.
///
/// The reference implementation can afford first-match only because its fallback for a
/// line that names nothing is to answer the marked row anyway — once that fallback
/// declines, as this one does (it holds the words and says so), first-match stops being
/// a convenience and becomes a different answer from the one that was typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OptionChoice {
    /// Exactly one option answers to the line. This is the operator's answer, and the
    /// caller sends it.
    One {
        option_id: String,
        pattern: Option<String>,
        note: Option<String>,
    },
    /// The first word of the line begins more than one option id, so the operator has
    /// not said which one they mean. Refused, with the candidates named.
    Ambiguous {
        /// What they typed, echoed back so the sentence reads as a reply.
        word: String,
        /// The option ids it could have meant, in card order.
        candidates: Vec<String>,
    },
    /// Nothing on the card answers to this line.
    Unnamed,
}

/// The options whose id **begins with** `t`, as indices into the card — all of them.
///
/// All of them, and not the first: naming the candidates is the whole of the refusal in
/// [`OptionChoice::Ambiguous`], and a helper that returned the first hit is the exact
/// shape of the bug this exists against. Case-folded, because an id is typed by a person
/// and `allow_once` is spelled the same way in every case.
///
/// An empty `t` has no candidates: an empty line is the composer's, and every id starts
/// with the empty string, which would make every card ambiguous.
pub(crate) fn option_candidates(d: &OpenDecision, t: &str) -> Vec<usize> {
    if t.is_empty() {
        return Vec::new();
    }
    d.options
        .iter()
        .enumerate()
        .filter(|(_, o)| o.option_id.to_ascii_lowercase().starts_with(t))
        .map(|(i, _)| i)
        .collect()
}

/// **The sentence an ambiguous prefix gets.** The candidates by name, and the two ways
/// out — finish typing, or use the arrows — because a refusal that does not say what to
/// do next is a head that has stopped listening.
///
/// The same shape `App::pick` and `App::pick_mode` give the same problem for their own
/// lists ("{n} sessions match …; type the number on the left instead"): this file's answer
/// to an ambiguous name, in the place the operator is already reading.
pub(crate) fn ambiguous_option_line(word: &str, candidates: &[String]) -> String {
    /// Enough to name the difference, few enough to stay on one line. A card past this
    /// says how many there are, which is the fact that matters when the list is long.
    const SHOWN: usize = 6;
    let shown = candidates
        .iter()
        .take(SHOWN)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    let more = if candidates.len() > SHOWN {
        ", …"
    } else {
        ""
    };
    format!(
        "`{word}` starts {} options here: {shown}{more} — finish typing, or ↑↓ then enter",
        candidates.len(),
    )
}

pub(crate) fn match_option(d: &OpenDecision, typed: &str) -> OptionChoice {
    let line = typed.trim();
    let (word, rest) = match line.split_once(char::is_whitespace) {
        Some((w, r)) => (w, r.trim()),
        None => (line, ""),
    };
    let t = word.to_ascii_lowercase();
    // **An exact name is an answer, and it is checked first.** An equality cannot be
    // ambiguous, so it is never the case this requirement is about: `allow_once` is the
    // spelling the card prints beside every option, and a label that is one word
    // (`Deny`) resolves from the same rule. Only the *prefix* path can be ambiguous.
    let exact = d.options.iter().position(|o| {
        o.option_id.eq_ignore_ascii_case(&t) || (!t.is_empty() && o.label.to_ascii_lowercase() == t)
    });
    let at = match exact {
        Some(i) => i,
        None => {
            // **A prefix that fits more than one option is refused, not resolved.**
            //
            // This was `.or_else(|| …find(|o| o.option_id.starts_with(&t)))`, which took
            // the first hit in list order — so `allow` silently answered `allow_once`
            // out of `allow_once`, `allow_session`, `allow_always`. **Which of three
            // grants the operator gave is the whole content of the answer**, and where
            // an option sits in a list is not something they said. A head may take a
            // prefix that names exactly one option (that is how people actually type);
            // it may not choose among several.
            let candidates = option_candidates(d, &t);
            match candidates.as_slice() {
                [] => return OptionChoice::Unnamed,
                [only] => *only,
                _ => {
                    return OptionChoice::Ambiguous {
                        word: word.to_string(),
                        candidates: candidates
                            .into_iter()
                            .map(|i| d.options[i].option_id.clone())
                            .collect(),
                    };
                }
            }
        }
    };
    let id = &d.options[at];
    if rest.is_empty() {
        return OptionChoice::One {
            option_id: id.option_id.clone(),
            pattern: None,
            note: None,
        };
    }
    match id.kind {
        // A glob, for the option that writes a rule.
        letibot_sessionlog::event::OptionKind::AllowAlways => OptionChoice::One {
            option_id: id.option_id.clone(),
            pattern: Some(rest.to_string()),
            note: None,
        },
        // **The reason, for the option that promised one.** `deny_and_tell` is
        // labelled *"Deny, and tell the model why"* and typing the why used to
        // land here and be refused — the line stayed in the composer and nothing
        // was answered at all. The operator: *"deny and tell doesnt work - there
        // is no input for the 'tell' part"*.
        letibot_sessionlog::event::OptionKind::RejectAlways => OptionChoice::One {
            option_id: id.option_id.clone(),
            pattern: None,
            note: Some(rest.to_string()),
        },
        // Everything else refuses trailing words rather than dropping them:
        // somebody who typed them meant them, and answering as though they had
        // not is the answer they did not give.
        _ => OptionChoice::Unnamed,
    }
}

/// The row a `ToolStarted` / `ToolProgress` / `ToolFinished` is about: the
/// **last** call with that id that has not finished yet.
///
/// Not the first, which is what this used to be. A call id is positional within a
/// round (`call_0`, `call_1`, …), so a turn that makes fourteen rounds of calls
/// has fourteen rows called `call_0` in one `TurnPane`, and `find` handed every
/// one of those events to the first of them. Measured on the operator's own
/// session, replayed: round one's card was re-finished eight times and wore the
/// last round's duration, while rounds two onward sat at `○ Reading README.md ·
/// proposed` for the rest of the turn — a call that had returned twenty seconds
/// earlier, drawn as one that had not started.
///
/// Searching from the back for a row that is still open is exact rather than
/// heuristic: within a turn the engine proposes and settles in order, so the only
/// row a start or a finish can be about is the newest unfinished one.
/// A row number typed on a card: `'1'`..`'9'` to a 0-based index, within `n`.
///
/// **Nine, not more.** A card with ten rows would make `1` ambiguous between
/// row one and the start of row twelve, and the fix for that is a composer that
/// collects digits — which is what the session picker already does and is why
/// this is not used there. Every card that uses it has a handful of rows; when
/// one grows past nine, its tenth row is reachable by the arrows and by typing,
/// and nothing here silently picks the wrong one.
///
/// The operator asked for it on all three cards at once (2026-09-17): *"it
/// shows numbered lists anyway so me pressing row number should constitute
/// focus and enter"*.
pub(crate) fn digit_row(k: &Key, n: usize) -> Option<usize> {
    let Key::Char(c) = k else { return None };
    let d = c.to_digit(10)? as usize;
    (1..=n.min(9)).contains(&d).then(|| d - 1)
}

/// **How many rows a card's ladder has** — and one question for both kinds, because
/// the two kinds do not carry their rows in the same field (§1.7).
///
/// A `permission` puts its allowed answers in `options`. A **`question` carries
/// `options: []`** and puts the model's offered choices in `choices`
/// (`Vec<String>`), so a head that asks `options.len()` gets `0` for every question —
/// which is exactly how this head came to be unable to answer one at all: the ladder
/// bound was zero, `answer_marked` returned `None`, and a typed line was held with
/// *"this ask offers no options"*.
///
/// One function rather than a condition at each of the four call sites, because those
/// four have to agree about it: the bound the arrows wrap on, the bound the digits
/// use, the row `answer_marked` takes, and the rows the card draws.
pub(crate) fn decision_rows(d: &OpenDecision) -> usize {
    if d.kind == "question" {
        d.choices.len()
    } else {
        d.options.len()
    }
}

/// A key, decoded from the terminal.
///
/// Everything down to [`Key::Eof`] is one of `letibot_ui::editor::Key`'s and is
/// forwarded to the composer verbatim; the five below it are the head's own and
/// never reach it. Two enums rather than one because the composer is a library
/// that knows nothing about folds, and the head is a program that must not own a
/// keymap for word motion.
///
/// No longer `Copy`: [`Key::Paste`] carries the paste, because the whole point of
/// bracketed paste is that three thousand characters are **one** key and not
/// three thousand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    Char(char),
    /// The terminal window gained focus (`?1004`, see `crate::features`).
    FocusIn,
    /// The terminal window lost focus.
    FocusOut,
    /// The terminal's answer about its background colour (OSC 11).
    Background {
        light: bool,
    },
    /// A bracketed paste, arriving whole.
    Paste(String),
    Enter,
    /// Alt+Enter: a newline that does not submit.
    SoftEnter,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    WordLeft,
    WordRight,
    Home,
    End,
    KillToEnd,
    KillToStart,
    KillWordBack,
    Yank,
    Undo,
    Redo,
    Esc,
    CtrlC,
    Eof,
    /// Fold or unfold the model's reasoning.
    CtrlR,
    /// **Open the rest of the newest long tool result**, or close that window.
    ///
    /// One row, and not the conversation: the whole-conversation unfold is the verb
    /// `/t`. See the `CtrlV` arm in `App::key` for the ruling. **R56 moved this off
    /// `ctrl-t`**, which is now the todos pane; `v` for *view* is the mnemonic it never
    /// had, and `0x16` had no arm at all before this — see `term.rs`.
    CtrlV,
    /// Show or hide the raw, unparsed text of tool calls.
    CtrlX,
    /// Repaint from scratch.
    CtrlL,
    /// Open or close the session picker.
    CtrlS,
    /// Open or close the todos pane: the session's plan and the repo's queue.
    ///
    /// **R56 moved this off `ctrl-p`** (which is now the hold) and onto leticl's own key,
    /// so one operator learns one chord for one pane — see [`Self::CtrlP`].
    CtrlT,
    /// **Hold the view** (R56): while held, the head writes nothing at all, so a mouse
    /// selection survives a streaming turn. The reader is the only party who can know a
    /// selection exists — the terminal does not forward a Shift-drag — so the reader, not
    /// the head, decides when to stop painting. See [`App::toggle_hold`] for the contract.
    CtrlP,
    /// Open or close the subagent tree: the subagents this session spawned.
    CtrlG,
    /// Move the running command to the background (Ctrl+O, like Claude Code's
    /// Ctrl+B — B is the readline left-arrow here).
    CtrlO,
    /// Open or close the background-jobs pane: the jobs this session started.
    CtrlQ,
    /// **Retire every note this head is holding** (R22).
    ///
    /// R10 gave the reader the power to retire a note and spelled it `/notes dismiss all` —
    /// *the right power in the wrong hand*: the thing you do to clear your own screen is a
    /// **reflex, not a sentence**, and every other reflex on this screen is already a chord.
    /// The operator, being told how to hide a note: *"typing `/notes dismiss all` is not
    /// humane."*
    ///
    /// **ALL of them, and it is one decision made in `head-parity-2026-09-21.md` §R22 with the
    /// other head rather than here** — a reflex that does different things on two screens is
    /// worse than the verb it replaces. The argument for all over newest is in that section
    /// and in the answer beside the proposal; the short form is that `/notes dismiss N` is
    /// where a *deliberate* single retire belongs (it numbers the notes, so the operator can
    /// see which is which), and that over-clearing is one verb to undo while under-clearing
    /// cannot be undone by a chord at all.
    ///
    /// **What it keeps from R10, because the reason has not changed:** retired is not deleted.
    /// The note stays in `notes`, `/notes` prints it in full, `/status` counts it, and
    /// `/notes restore` brings it back. A head that can silently drop a warning is a head whose
    /// warnings cannot be trusted to be complete.
    CtrlN,
    PageUp,
    PageDown,
    /// Mouse wheel up, decoded from the SGR mouse protocol. Scrolls the
    /// transcript back; drags and motion are decoded and dropped, because the
    /// terminal's own Shift+drag is what selects.
    WheelUp,
    WheelDown,
    /// Tab: complete the `/command` being typed.
    Tab,
    /// A left-button press, 0-based screen coordinates. An open picker takes
    /// it: the row under the pointer becomes the selected row, and Enter still
    /// does the switching — select and confirm stay two acts.
    Click {
        x: u16,
        y: u16,
    },
}

impl Key {
    /// The composer's key, when this is one of its.
    pub(crate) fn composer(&self) -> Option<letibot_ui::editor::Key> {
        use letibot_ui::editor::Key as E;
        Some(match self {
            Key::Char(c) => E::Char(*c),
            Key::Paste(s) => E::Paste(s.clone()),
            Key::Enter => E::Enter,
            Key::SoftEnter => E::SoftEnter,
            Key::Backspace => E::Backspace,
            Key::Delete => E::Delete,
            Key::Left => E::Left,
            Key::Right => E::Right,
            Key::Up => E::Up,
            Key::Down => E::Down,
            Key::WordLeft => E::WordLeft,
            Key::WordRight => E::WordRight,
            Key::Home => E::Home,
            Key::End => E::End,
            Key::KillToEnd => E::KillToEnd,
            Key::KillToStart => E::KillToStart,
            Key::KillWordBack => E::KillWordBack,
            Key::Yank => E::Yank,
            Key::Undo => E::Undo,
            Key::Redo => E::Redo,
            Key::Esc => E::Esc,
            Key::CtrlC => E::CtrlC,
            Key::Eof => E::Eof,
            Key::CtrlR
            | Key::CtrlT
            | Key::CtrlV
            | Key::CtrlX
            | Key::CtrlL
            | Key::CtrlS
            | Key::CtrlP
            | Key::CtrlG
            | Key::CtrlO
            | Key::CtrlQ
            | Key::CtrlN
            | Key::PageUp
            | Key::PageDown
            | Key::WheelUp
            | Key::WheelDown
            | Key::Tab
            | Key::Click { .. }
            | Key::FocusIn
            | Key::FocusOut
            | Key::Background { .. } => {
                return None;
            }
        })
    }

    /// **The switch this key is the chord of, read from [`Show::chord`]** — the reverse lookup the
    /// key dispatch uses, so a chord is advertised and acts from ONE entry rather than from a pair
    /// of hand-written arms that each spelled their key twice. A key no switch has a chord for, and
    /// a switch whose chord no longer matches, both come out of here as `None`, which is what makes
    /// the drift impossible instead of merely unlikely.
    pub(crate) fn show(&self) -> Option<Show> {
        Show::ALL
            .into_iter()
            .find(|s| s.chord().is_some_and(|(_, k)| &k == self))
    }
}
