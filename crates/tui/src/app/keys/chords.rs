//! **The global chords**: the keys that mean the same thing whatever is open.

use super::*;
use std::ops::ControlFlow;

impl App {
    pub(crate) fn key_chords(&mut self, k: &Key) -> ControlFlow<Option<Action>> {
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
                let Some(next) = s.by_chord(self.visibility.level(s)) else {
                    return ControlFlow::Break(None);
                };
                return ControlFlow::Break(self.set_verbosity(&format!(
                    "{}={}",
                    s.name(),
                    next.as_str()
                )));
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
                return ControlFlow::Break(None);
            }
            Key::CtrlL => {
                // **This one keeps `redraw`.** The flag means *throw the glass away* — the next
                // frame erases and rewrites in full — and that is right exactly when this head's
                // memory of the screen is known to be wrong. Ctrl-L is the operator saying
                // something else wrote to their terminal, which is that case, and it is the only
                // key that is. The scroll keys are the opposite case and take the diff; see
                // [`App::hold`] for why the wheel does not.
                self.redraw = true;
                return ControlFlow::Break(None);
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
                return ControlFlow::Break(self.picker.then_some(Action::ListSessions));
            }
            Key::CtrlT => return ControlFlow::Break(self.toggle_todos()),
            // **Hold the view (R56).** While held the head writes nothing at all, so a
            // mouse selection survives a streaming turn; the second press releases it and
            // says how much arrived while it was held. The whole contract is in
            // [`App::toggle_hold`].
            Key::CtrlP => return ControlFlow::Break(self.toggle_hold()),
            // Ctrl+G for the subagent tree: R/T/X/L/S/P are taken, A/E/W/U/Y/K/B/F
            // are the composer's readline keys, and the subagent tree is a *view*,
            // not a thing the composer needs a letter for.
            Key::CtrlG => {
                self.toggle_subagents();
                return ControlFlow::Break(None);
            }
            // Ctrl+Q for the background jobs. J would have been the mnemonic and
            // is line-feed; Q is XON, dead the same way Ctrl+S's XOFF would be —
            // and fixed the same way: cfmakeraw clears IXON, so nothing is
            // listening for flow control and the byte arrives like any other.
            // The act itself is `toggle_jobs`'s, shared with `/jobs` and a click
            // on the count label so the three cannot drift.
            Key::CtrlQ => return ControlFlow::Break(self.toggle_jobs()),
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
                return ControlFlow::Break(None);
            }
            // **Ctrl+O: move the running command to the background**, the same action
            // `/promote` names. See [`App::promote`] for why the fact it guards is a
            // running CALL and not a running turn.
            Key::CtrlO => return ControlFlow::Break(self.promote()),
            // **Ctrl+]: to the editor pane** — back into it when it is open, onto the newest
            // change when it is not. See [`App::editor_chord`].
            Key::CtrlBracket => return ControlFlow::Break(self.editor_chord()),
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
                return ControlFlow::Break(None);
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
                    // Three lines a notch here too — the card, the panes and the windows
                    // page by it below, and their paging is not the transcript's walk.
                    // The wheel's own down path replaces `by` at the foot of this arm,
                    // after every screen that could take the notch has passed: see
                    // [`App::wheel_down_notch`].
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
                    return ControlFlow::Break(None);
                }
                if self.scroll_tail_overlay(up, by) {
                    return ControlFlow::Break(None);
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
                    return ControlFlow::Break(None);
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
                    return ControlFlow::Break(None);
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
                    // **And 2026-10-09's second report is why the run has to gather speed.**
                    // A three-line walk needs sixty-four clean notches from 190 lines up, and
                    // a stream that keeps arriving — the wedged session was in a loop of
                    // automatic turns — recedes faster than the walk advances: the notch moved
                    // every time and the count still grew. The run is the reader's own gesture,
                    // so that is what accelerates; see [`App::wheel_down_notch`].
                    //
                    // The deliberate act keeps its own meaning and is not folded in here: Esc
                    // while parked — *"Esc while parked in the scrollback means \"follow the
                    // stream again\""* — and the parked `↓` below still clear the anchor in one
                    // press, and they are the keys the banner names for it.
                    //
                    // **And the wheel's notch is sized HERE, at the walk itself** — not in the
                    // `(up, by)` match above, which the card, the panes and the windows share:
                    // their paging stays three lines a notch. Only the transcript's walk can
                    // gather speed, and only the run of notches the reader spent on it counts.
                    let by = match k {
                        Key::WheelDown => self.wheel_down_notch(),
                        _ => by,
                    };
                    self.hold(by as isize);
                }
                return ControlFlow::Break(None);
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }
}
