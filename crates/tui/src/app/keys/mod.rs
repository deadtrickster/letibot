//! **A key, handled**: the dispatch from a keystroke to whichever card, pane, window or the
//! composer owns it, and the matching of a typed answer to a decision's options.

use super::*;
use letibot_sessionlog::view::OpenDecision;
use letibot_ui::editor::Reaction;

pub(crate) mod bindings;
mod cards;
mod chords;
mod input;
mod mouse;
mod panes;
mod window;
pub use input::key_of;

use std::ops::ControlFlow;

/// How the override chord is spelled in every sentence that names it: one constant beside the key it names, so
/// the spelling and the byte cannot drift, the way a switch's `chord()` keeps its name and its key in one entry.
pub(crate) const ATTACH_ANYWAY_CHORD: &str = "ctrl-^";

impl App {
    /// A key. Returns an action for the driver to send, if any. Only the folds, the redraw and the two page keys
    /// are the head's: the rest is the composer's, because interaction policy in a match arm is how two heads
    /// drift apart.
    pub fn key(&mut self, k: Key) -> Option<Action> {
        // **The front card is settled before and after every key** (R90.3, R90.4) — see
        // [`App::sync_front`].
        self.sync_front();
        let r = self.key_dispatch(k);
        // R90.29: an answer that leaves closes its card at once.
        if let Some(Action::Answer { req_id, .. } | Action::AnswerQuestion { req_id, .. }) = &r {
            let req_id = req_id.clone();
            self.answer_leaving(&req_id);
        }
        self.sync_front();
        r
    }

    fn key_dispatch(&mut self, k: Key) -> Option<Action> {
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
            // **The pointer's own reports** — hover, and the press, drag and release a selection
            // is made of — before anything a keystroke does: moving the mouse must not dismiss
            // a notice. A release with no drag is a click, and goes on as one. See `app::pointer`.
            Key::Hover { x, y } => {
                self.hover_at(x, y);
                return None;
            }
            Key::Press { x, y } => {
                self.press_at(x, y);
                return None;
            }
            Key::Drag { x, y } => {
                self.drag_to(x, y);
                return None;
            }
            Key::Release { .. } => {
                return match self.release() {
                    Some(click) => self.key(click),
                    None => None,
                };
            }
            // **Esc drops a selection first** — the operator: *"and cancel selection on esc"* —
            // and that Esc is spent on it: a pane, a popup, a running turn stay as they were.
            Key::Esc if self.selection.is_some() => {
                self.drop_selection();
                return None;
            }
            // Enter over a picture opens it larger: what a click does on the thumbnail row, with the keyboard's
            // version of which one, which is the newest picture the last frame drew. Guarded to the state where
            // nothing else wants Enter.
            Key::Enter
                if self.editor.text().trim().is_empty()
                    && self.diff_popup.is_none()
                    && self.open.is_empty()
                    && self.prompt.is_none()
                    && self.drawn() == Drawn::Conversation
                    && !self.picture_rows.is_empty() =>
            {
                if let Some(r) = self.newest_picture_ref() {
                    self.open_picture(&r);
                }
                return None;
            }
            // A held view moves for the reader's own keys: the hold is against the STREAM moving the view, so a
            // key draws the held view once more and it is held again. A hold a selection took ends with the reader
            // acting elsewhere.
            Key::CtrlP => {}
            _ if self.hold => {
                if self.selection_held {
                    self.drop_selection();
                } else {
                    self.hold_frame = None;
                }
            }
            _ => {}
        }
        // **The session's prompts, before a recall starts** — see `refresh_prompt_history`.
        // Up may yet be taken by a card or a pane further down; refreshing the list then
        // changes nothing a reader sees, and the editor ignores it mid-recall.
        if matches!(k, Key::Up) {
            self.refresh_prompt_history();
        }
        // Any key is an acknowledgement of whatever the notice said: the deadline moves to now, so this tick's
        // `screen()` takes the sentence down before anything is drawn. The arrows and the paging keys are exempt,
        // being the keys a reader scrolls with, and a notice nobody timed is not the reader's to dismiss.
        if !matches!(k, Key::Up | Key::Down | Key::PageUp | Key::PageDown)
            && self.notice_until.is_some()
        {
            self.notice_until = Some(self.now_ms);
        }
        // `ctrl-^` is the read-only override and it runs before every card: a head locked against an older daemon
        // can be holding an open decision, and answering that card is exactly the send the lock is holding, so the
        // way to unlock it must not be behind the card.
        if matches!(k, Key::CtrlCaret) {
            self.attach_anyway();
            return None;
        }
        // `ctrl-e`, settled once here: the editor on an empty prompt with nothing in front of it, `End` everywhere
        // else and before any card or pane sees the key. A click on a key hint is that key, ahead of every card,
        // since the hint is drawn ON them.
        if let Key::Click { x, y } = k
            && let Some(hint) = self.key_hint_at(x, y).cloned()
        {
            let _ = self.press_hint(hint);
            return None;
        }
        let k = match k {
            Key::CtrlE if self.ctrl_e_opens_editor() => {
                self.open_editor();
                return None;
            }
            Key::CtrlE => Key::End,
            k => k,
        };
        // **alt-g / alt-b, settled once, here** — the `ctrl-e` shape: a mark when nothing owns
        // the keys (alt-b also needs an empty composer), word-left / nothing otherwise.
        let k = match k {
            Key::AltB if !self.feedback_key_free(true) => Key::WordLeft,
            Key::AltG if !self.feedback_key_free(false) => return None,
            Key::AltG => return self.feedback_key(true),
            Key::AltB => return self.feedback_key(false),
            // **alt-a opens the question a row is waiting on you for** — wherever the keyboard is,
            // because the line that names it is drawn on every frame — and brings back a question
            // card Esc put away, of either kind (see [`App::bring_back_question`]).
            Key::AltA => {
                self.bring_back_question();
                return None;
            }
            // alt-r reseats, alt-p shows the reseat card, wherever the keyboard is: the strip that names them is
            // drawn on every frame while the prompt is stale. Both ctrl letters are taken and the other alt keys are
            // spoken for.
            Key::AltR => return self.reseat_now(),
            Key::AltP => {
                self.reseat_expand();
                return None;
            }
            // alt-n opens the newest notice line whole: every notice is one line and a click opens the rest, and
            // the line is on the conversation whatever is in front of it. Nothing typed is lost, since the popup
            // closes with Esc onto the composer as it was.
            Key::AltN => {
                self.open_latest_notice();
                return None;
            }
            // **alt-h folds the questions waiting on you away, and back** — not Esc, which already
            // means "stop" and "back to the bottom" (the operator: *"esc is ambigous here"*).
            Key::AltH => {
                self.toggle_asks_folded();
                return None;
            }
            k => k,
        };
        // **A wheel notch goes to the surface under the pointer** — see [`App::wheel_at`]. One
        // that nothing on the frame claims, or that arrives while a card owning every key is up,
        // goes on below as a notch with no position, which is what the wheel always was.
        let k = match k {
            Key::Wheel { up, x, y } => match self.wheel_at(up, x, y) {
                ControlFlow::Break(r) => return r,
                ControlFlow::Continue(()) if up => Key::WheelUp,
                ControlFlow::Continue(()) => Key::WheelDown,
            },
            k => k,
        };
        // The cards that own the keyboard, in the order the slot draws them and the bar names them: the password,
        // the pane-ending confirmation, the prompt card, the key ask, the new-todo card. A decision's keys wait for
        // the draft cards, since Enter filing a todo under a permission card was the defect.
        if let ControlFlow::Break(r) = self.key_mode_confirm(&k) {
            return r;
        }
        if let ControlFlow::Break(r) = self.key_secret(&k) {
            return r;
        }
        if let ControlFlow::Break(r) = self.key_term_ask(&k) {
            return r;
        }
        if let ControlFlow::Break(r) = self.key_prompt(&k) {
            return r;
        }
        if let ControlFlow::Break(r) = self.key_key_ask(&k) {
            return r;
        }
        if let ControlFlow::Break(r) = self.key_todo_draft(&k) {
            return r;
        }
        // **An open diff popup owns the keyboard**, below the cards and asks above (a question
        // that arrives while the operator reads stays answerable) and above everything else.
        if let ControlFlow::Break(r) = self.key_diff_popup(&k) {
            return r;
        }
        // **A click goes to what the last frame drew under it** — see [`App::key_click`]. Placed
        // after the cards that own every key (they take clicks as theirs) and before every
        // surface: no surface decides any more whether a click is its own.
        if let ControlFlow::Break(r) = self.key_click(&k) {
            return r;
        }
        if let ControlFlow::Break(r) = self.key_chords(&k) {
            return r;
        }
        // The cards in the chrome, in front of the surface: a decision, the quit card and a setting card are
        // questions drawn by the composer, on the screen whatever holds the rectangle above them, so their keys
        // come first.
        if let ControlFlow::Break(r) = self.key_decision(&k) {
            return r;
        }
        if let ControlFlow::Break(r) = self.key_quit_card(&k) {
            return r;
        }
        if let ControlFlow::Break(r) = self.key_pick(&k) {
            return r;
        }
        // **The reseat card, behind every card above** — it is drawn only while none of them
        // holds the slot (`App::reseat_card_up`), and typing past it pins it.
        if let ControlFlow::Break(r) = self.key_reseat_card(&k) {
            return r;
        }
        // **The surface in front, and nothing behind it** — see [`App::key_surface`].
        if let ControlFlow::Break(r) = self.key_surface(&k) {
            return r;
        }
        // Esc while parked in the scrollback means follow the stream again, which is what the banner says. Only
        // then does Esc start arming an interrupt, and not while a payload window is open, since that window's own
        // seam prints `esc closes`.
        if matches!(k, Key::Esc) && !self.following() && self.payload_sel.is_none() {
            // **Back to following**, which is what the banner says this key does — and it
            // clears the anchor as well as the count, because the two are one state (R36).
            self.scroll = 0;
            self.anchor = None;
            return None;
        }
        if let ControlFlow::Break(r) = self.key_payload_window(&k) {
            return r;
        }
        // Up with an empty composer recalls the queued line: the echo above the composer is the person's own words,
        // and the one thing this head can do about a message it already sent is take it back before it lands. The
        // take-back rides with the recall, and browsing history never does.
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

        // Tab: completion by the line's first character. A `/` line completes a command and a `!` line completes
        // from what this session ran. The composer's keys run after, since Tab means nothing to the editor, and
        // every other key leaves a running cycle alone.
        if let Key::Tab = k {
            if self.editor.text().starts_with('!') {
                self.complete_shell();
            } else {
                self.complete_slash();
            }
            self.redraw = true;
            return None;
        }

        // Parked in the scrollback the arrows belong to the scrollback: the composer is asked
        // first and hands Up to its history, so the transcript's fallback was reached only for keys
        // the editor had no use for. Down at the bottom follows again, as the banner says.
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

        // The ctrl-v window does not own Enter: it is a row of the conversation opened wide, with the composer live
        // underneath and no Enter of its own to lose. A typed line goes and the window closes with it; an empty
        // Enter still does nothing.
        if matches!(k, Key::Enter) && self.payload_sel.is_some() {
            if self.editor.text().trim().is_empty() {
                return None;
            }
            self.payload_sel = None;
            self.payload_page = 0;
            self.invalidate_history();
            self.redraw = true;
        }

        // Esc inside a subagent's session goes back UP the tree: Enter goes down a level, Esc comes
        // up. It sits after every other Esc contract and never reaches the composer, so the pair
        // cannot interrupt the child; an open decision is said rather than armed.
        if matches!(k, Key::Esc)
            && let Some(parent) = self.parent_session()
        {
            if !self.open.is_empty() {
                self.say("a decision is waiting in this subagent — answer it, then esc goes up");
                self.redraw = true;
                return None;
            }
            // **A card put away here keeps the operator here too** (R90.15): the parent's
            // waiting line is filtered to the parent's session, so a child's question left
            // behind with its deadline would be invisible from up there.
            let sid = self.session_id.clone();
            if self
                .away
                .iter()
                .any(|a| a.session_id == sid && !a.is_todo())
            {
                self.say(
                    "a decision put away is waiting in this subagent — alt-a brings it back; \
                     answer it, then esc goes up",
                );
                self.redraw = true;
                return None;
            }
            // The pane opens on the rows the person climbed out of, and the cursor lands on the child they came
            // from: the row is found by id once the parent's `Hello` has rebuilt the list, since an index taken now
            // would be an index into the session being left.
            self.push_surface(Surface::Subagents);
            self.up_from = Some(self.session_id.clone());
            return self.switch_to(parent);
        }

        // A paste is read before the editor sees it: a drop of files is chips, an empty paste with a picture is
        // the picture, and the rest is text. A backspace inside a chip takes the whole chip, or a character at a
        // time leaves half a chip in the message.
        if let Key::Paste(s) = &k
            && self.paste_arrived(s.clone())
        {
            self.redraw = true;
            return None;
        }
        if matches!(k, Key::Backspace) && self.backspace_chip() {
            return None;
        }
        let now = self.now_ms;
        let cols = self.composer_cols();
        let reaction = match k {
            Key::Up => self.editor.vertical(true, cols),
            Key::Down => self.editor.vertical(false, cols),
            ref other => self.editor.key(other.composer()?, now),
        };
        // Esc returns the thinking to its default from the spelling the panel arm cannot reach: the fold `ctrl-r`
        // moved, with no panel up to close. Placed after the composer has seen the key, so a press that answers
        // `Interrupt` is the pair firing and not this arm's to reinterpret.
        if matches!(k, Key::Esc) && !matches!(reaction, Reaction::Interrupt) && self.thinking_open()
        {
            self.set_thinking(false);
        }
        match reaction {
            Reaction::Submit(text) => self.submit(text),
            Reaction::Interrupt => {
                // Busy, not generating: the gate read the state name, so it was dead for the whole of every
                // command, which is the only time anybody reaches for it. A turn whose only work is a running call
                // is exactly the turn a person wants to interrupt.
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
            // The second Ctrl+C asks instead of leaving: it used to detach, and an operator who wanted the daemon
            // stopped needed a second terminal. The card makes the irreversible half a choice, and it does not open
            // over a decision, which takes the digits first.
            Reaction::Quit if !self.open.is_empty() => {
                self.say(
                    "a decision is up — answer it or esc puts it away, then ctrl-c twice asks \
                     whether to leave",
                );
                self.redraw = true;
                None
            }
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
                    // `redraw` is not "rebuild the frame" — the loop rebuilds one every pass. It is "throw the
                    // glass away", and without it a scroll repaints only the rows whose text differs, which for a
                    // window that slid one line over similar rows can be almost none.
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

    /// The marked row is the answer: the marker is what Enter takes, the contract the pickers keep. A permission's
    /// row is an option id and goes as `Action::Answer`; a question's is an index into the model's choices and goes
    /// as `Action::AnswerQuestion`. `None` when the row is not an answer.
    pub(crate) fn answer_marked(&mut self) -> Option<Action> {
        let d = self.open.first()?;
        let n = decision_rows(d);
        if n == 0 {
            return None;
        }
        let at = self.sel.min(n - 1);
        if d.kind == "question" {
            let req_id = d.req_id.clone();
            return Some(self.settle_question(
                req_id,
                letibot_sessionlog::question::QuestionAnswer::choosing(at),
            ));
        }
        let option_id = d.options[at].option_id.clone();
        let req_id = d.req_id.clone();
        Some(Action::Answer {
            req_id,
            option_id,
            // The ladder is the no-glob path by construction: there is nothing typed to read one from. A glob is
            // given by typing `allow_always <pattern>`, and a reason with `deny_and_tell <why>`, which taken from
            // the ladder alone denies without a reason and says so.
            pattern: None,
            note: None,
        })
    }

    /// A pane with no daemon is a pane with no program: the pty is the daemon's, so the rectangle would sit frozen
    /// with `ctrl-\` going nowhere, and the transcript is the honest thing to show. The daemon's pane is not closed
    /// here, since the close is a frame and a frame is what cannot be sent.
    pub fn drop_pane(&mut self) -> bool {
        let had = self.term.take().is_some();
        if had {
            self.redraw = true;
        }
        had
    }

    /// Whether a pane is open and drawn, and therefore owns the keyboard: the question `Link::tick` asks before it
    /// decides whether a byte is a key of its own or the program's. A detached pane owns nothing, and the two
    /// questions — is there a program, is it on the screen — are deliberately different.
    pub fn pane_open(&self) -> bool {
        self.term.as_ref().is_some_and(|p| !p.detached)
    }

    /// Does this head hold a pane at all, drawn or detached and still running: the third of the three questions,
    /// and the one about the program. Public because an integration test driving the head through the driver's loop
    /// has to ask it, since `pane_open` answers false for a detached pane.
    pub fn holds_pane(&self) -> bool {
        self.term.is_some()
    }

    /// The pane this session has that this head is not drawing, as a line a person reads. Two sources and the
    /// order is the decision: this head's own pane first, since a detach keeps the program and the typed line, and
    /// then the daemon's answer, which is the only thing that can speak for a session this head has no pane in.
    pub(crate) fn pane_behind(&self) -> Option<String> {
        // On the screen: the pane itself is the fact, and a sentence about it would be the same
        // fact twice.
        if self.pane_open() {
            return None;
        }
        match (&self.term, &self.term_fact) {
            (Some(p), _) => Some(p.line.clone()),
            (None, PaneFact::Running(list)) if list.len() == 1 => {
                Some(format!("!term {}", list[0].command))
            }
            // **Several, by number** — `!term N` is how each comes back (protocol 44).
            (None, PaneFact::Running(list)) => Some(crate::app::session::panes_said(list)),
            (None, PaneFact::None | PaneFact::Unasked) => None,
        }
    }

    /// The pane's keyboard. `Ctrl-\` is looked for before anything is forwarded, so a program never
    /// receives it and cannot trap it, and the act is a detach: nothing is sent, so the program is
    /// neither signalled nor told. The byte cannot sit inside a character or a CSI final.
    pub fn pane_keys(&mut self, raw: &[u8]) -> Vec<Action> {
        let Some(p) = self.term.as_ref() else {
            return Vec::new();
        };
        // An ending is in flight: between the confirmed close and `TermEnded` a kill is on the way, and a byte
        // written into a pty whose program is being signalled is a byte nobody reads. A detached pane takes no keys
        // either, since the composer has its rows and keys back.
        if p.closing || p.detached {
            return Vec::new();
        }
        // The person's mouse is decided report by report before anything else: sent to the program only when it
        // asked for a mouse, turned into scrollback keys when it did not, never typed at it as text. `0x1c` cannot
        // be inside a report, so routing first moves no way-out byte.
        let routed = self.term.as_mut().expect("just checked").route(raw);
        let raw = &routed[..];
        match raw.iter().position(|b| *b == WAY_OUT) {
            Some(at) => {
                let mut out = Vec::new();
                if at > 0 {
                    out.extend(self.pane_input(&raw[..at]));
                }
                // **Detach: nothing leaves the head.** Not a frame and not a keystroke — the
                // whole point is that the program keeps running and this head stops drawing it.
                self.detach();
                out
            }
            None if raw.is_empty() => Vec::new(),
            None => self.pane_input(raw).into_iter().collect(),
        }
    }

    /// Keys for the pane, addressed to its number, or held until the daemon has given it one: a pane is up the
    /// moment Enter is pressed and numbered when `TermAttached` lands, and keys typed in between go down in order
    /// the moment it arrives.
    pub(crate) fn pane_input(&mut self, bytes: &[u8]) -> Option<Action> {
        let p = self.term.as_mut()?;
        match p.pane {
            Some(pane) => Some(Action::TermInput {
                pane,
                bytes: bytes.to_vec(),
            }),
            None => {
                p.waiting.extend_from_slice(bytes);
                None
            }
        }
    }

    /// Move the payload window by `by` wrapped lines, clamped to the last full page the draw
    /// recorded, so Down stops where the output ends. What the window cannot take the conversation
    /// does, the way a scroll box hands over at its edge; Home and End are jumps inside it.
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
        // The handover first, while the rows are still measured: invalidating the history drops the line spans the
        // transcript scroll finds its row by, so a scroll made after it landed nowhere.
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

/// The option a typed line names, or why there is not one. Three outcomes and not an `Option`, since *nothing
/// answers to that name* and *several things do* are different answers: the first is the mid-typing courtesy,
/// which holds the words and answers the marked row.
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

/// The options whose id begins with `t`, all of them and not the first: naming the candidates is the whole of the
/// refusal in [`OptionChoice::Ambiguous`], and a helper returning the first hit is the bug this exists against.
/// Case-folded, since an id is typed by a person.
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

/// The sentence an ambiguous prefix gets: the candidates by name and the two ways out, since a refusal that does
/// not say what to do next is a head that has stopped listening. The same shape `pick` and `pick_mode` give the
/// same problem for their own lists.
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
    // An exact name is an answer and it is checked first: an equality cannot be ambiguous, and `allow_once` is the
    // spelling the card prints beside every option. Only the prefix path can be ambiguous.
    let exact = d.options.iter().position(|o| {
        o.option_id.eq_ignore_ascii_case(&t) || (!t.is_empty() && o.label.to_ascii_lowercase() == t)
    });
    let at = match exact {
        Some(i) => i,
        None => {
            // A prefix that fits more than one option is refused, not resolved. `.find(|o| o.option_id.starts_with(t))`
            // took the first hit in list order, so `allow` silently answered `allow_once` out of three grants the
            // person was choosing between.
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
        // The reason, for the option that promised one: `deny_and_tell` is labelled *deny, and tell the model why*,
        // and typing the why used to land here and be refused, with the line left in the composer and nothing
        // answered at all.
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

/// The word that means *I am not answering this one*: one word alone on the line, because it has to be typeable at
/// a composer and answerable over a pipe, since this tree's own tests drive the head by script. It is the fourth
/// shape `QuestionAnswer` carries.
pub(crate) const ABSTAIN: &str = "abstain";

/// The separator between a choice and its note: `<choice> -- <note>`, the permission card's shape one register over.
/// A question's names are prose, so a bare space cannot separate them — *sqlite is fine* would become a note on
/// `sqlite` — and the separator is the first ` -- ` that is not inside a choice.
pub(crate) const NOTE_SEP: &str = "--";

/// The three ways a person answers a question, in one function: one of the model's choices word for word answers by
/// index, a choice then `--` then words answers with the note, and anything else is their own words. `abstain` is
/// its own answer.
pub(crate) fn question_answer(
    choices: &[String],
    typed: &str,
) -> letibot_sessionlog::question::QuestionAnswer {
    use letibot_sessionlog::question::QuestionAnswer;
    let line = typed.trim();
    // **Abstention first**, because it is the one shape whose spelling is a word rather
    // than one of the model's: a choice that happened to be called `abstain` is still a
    // choice, and a person who chose it typed its exact words, which is checked below.
    if line.eq_ignore_ascii_case(ABSTAIN) {
        return QuestionAnswer::abstaining();
    }
    if let Some(i) = choices
        .iter()
        .position(|c| c.trim().eq_ignore_ascii_case(line))
    {
        return QuestionAnswer::choosing(i);
    }
    // **The choice, then the note.** The choice has to be a prefix of the line for the
    // note to have anything to qualify, and the separator has to be there for the rest
    // to BE a note.
    for (i, c) in choices.iter().enumerate() {
        let c = c.trim();
        if c.is_empty() {
            continue;
        }
        let Some(head) = line.get(..c.len()) else {
            // The line is shorter than the choice, or `c.len()` is not a char boundary.
            continue;
        };
        if !head.eq_ignore_ascii_case(c) {
            continue;
        }
        let Some(note) = line[c.len()..]
            .trim_start()
            .strip_prefix(NOTE_SEP)
            .map(str::trim)
            .filter(|n| !n.is_empty())
        else {
            continue;
        };
        return QuestionAnswer::choosing(i).with_note(note);
    }
    QuestionAnswer::free(line)
}

/// The row a tool event is about: the last call with that id that has not finished. Not the first, since a call id is
/// positional within a round, so a turn of fourteen rounds has fourteen rows called `call_0` and `find` handed every
/// one of those to the last.
pub(crate) fn digit_row(k: &Key, n: usize) -> Option<usize> {
    let Key::Char(c) = k else { return None };
    let d = c.to_digit(10)? as usize;
    (1..=n.min(9)).contains(&d).then(|| d - 1)
}

/// How many rows a card's ladder has, and one question for both kinds because they do not carry their rows in the
/// same field: a permission puts them in `options`, while a question carries `options: []` and puts the model's
/// choices in `choices`.
pub(crate) fn decision_rows(d: &OpenDecision) -> usize {
    if d.kind == "question" {
        d.choices.len()
    } else {
        d.options.len()
    }
}

/// A key, decoded from the terminal. Everything down to [`Key::Eof`] is the composer's and is forwarded verbatim;
/// the five below are the head's own and never reach it. Two enums because the composer knows nothing about folds,
/// and the head must not own a keymap for word motion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    Char(char),
    /// The terminal window gained focus (`?1004`, see `rano::term::features`).
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
    /// **alt-g: mark the current turn good** (TODO R81, `app/feedback.rs`).
    AltG,
    /// **alt-a: answer the operator's question** — bring back a question card Esc put away, or
    /// open the card for the first todo row that is waiting on the operator (`app/todos.rs`'s
    /// `bring_back_question`). The resident `waiting on you` line names it.
    AltA,
    /// **alt-b: mark the current turn bad** — on an empty composer; readline's word-left
    /// everywhere else, which is what alt-b always was (`app/feedback.rs`).
    AltB,
    /// **alt-r: reseat now** — the reseat card's first row from anywhere, while the session's
    /// prompt is stale (`app::reseat`).
    AltR,
    /// **alt-p: show the reseat card** — open the card the strip is the pinned form of.
    AltP,
    /// **alt-n: open the newest notice whole** — the popup a click on its one line opens (R89):
    /// every notice is one line, and the keyboard needs a way to the rest of it.
    AltN,
    /// **alt-h: fold what is waiting on you away, or back** — the questions' yellow line goes,
    /// and the composer's bottom edge counts them (`app/waiting.rs`).
    AltH,
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
    /// Open the rest of the newest long tool result, or close that window: one row and not the conversation, since
    /// the whole-conversation unfold is `/t`. `v` for view is the mnemonic the byte never had, and it moved here off
    /// `ctrl-t`, which is the todos pane now.
    CtrlV,
    /// Show or hide the raw, unparsed text of tool calls.
    CtrlX,
    /// Repaint from scratch.
    CtrlL,
    /// Open or close the session picker.
    CtrlS,
    /// Open or close the todos pane: the session's plan and the repo's queue. It moved off `ctrl-p`, which is the
    /// hold now, so one person learns one chord for one pane. See [`Self::CtrlP`].
    CtrlT,
    /// Hold the view: while held the head writes nothing at all, so a mouse selection survives a streaming turn.
    /// The reader is the only party who can know a selection exists, so the reader decides when to stop painting.
    /// See [`App::toggle_hold`].
    CtrlP,
    /// Open or close the subagent tree: the subagents this session spawned.
    CtrlG,
    /// Move the running command to the background (Ctrl+O, like Claude Code's
    /// Ctrl+B — B is the readline left-arrow here).
    CtrlO,
    /// Open or close the background-jobs pane: the jobs this session started.
    CtrlQ,
    /// Retire every note this head is holding. `/notes dismiss all` is the right power in the wrong hand: clearing
    /// your own screen is a reflex and not a sentence, and every other reflex here is already a chord.
    CtrlN,
    /// Ctrl+E: the editor on an empty prompt, end-of-line otherwise. Readline keeps the key wherever there is a line
    /// to be at the end of, and it is settled at the top of [`App::key`] before any card or pane sees it.
    CtrlE,
    /// Ctrl+`]`: between the conversation and the editor pane. From the composer it opens rano on the newest file
    /// this conversation changed, or goes back into a pane already open; inside the pane rano hands the chord back.
    /// `]` because every letter is a readline key.
    CtrlBracket,
    /// Ctrl+`^`: attach anyway. Held only while the head is read-only against an older daemon, and spelled only
    /// there. `^` because the C0 tail is where this head's one-purpose keys live. See [`App::attach_anyway`].
    CtrlCaret,
    PageUp,
    PageDown,
    /// Mouse wheel up, decoded from the SGR mouse protocol. Scrolls the
    /// transcript back; drags and motion are decoded and dropped, because the
    /// terminal's own Shift+drag is what selects.
    WheelUp,
    WheelDown,
    /// A wheel notch with where the pointer was: it scrolls the surface under the pointer, read off the last
    /// frame's record ([`Key::WheelUp`] and [`Key::WheelDown`] are the same notch with no position). See `app::hits`.
    Wheel {
        up: bool,
        x: u16,
        y: u16,
    },
    /// Tab: complete the `/command` being typed.
    Tab,
    /// A left-button press, 0-based screen coordinates. An open picker takes
    /// it: the row under the pointer becomes the selected row, and Enter still
    /// does the switching — select and confirm stay two acts.
    Click {
        x: u16,
        y: u16,
    },
    /// **The left button went down** — not yet a click: with no motion before the release it
    /// becomes one ([`Key::Click`]), and with motion it is a selection. See `app::pointer`.
    Press {
        x: u16,
        y: u16,
    },
    /// Motion with the left button held.
    Drag {
        x: u16,
        y: u16,
    },
    /// The left button came up.
    Release {
        x: u16,
        y: u16,
    },
    /// Motion with no button held: what the pointer is over.
    Hover {
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
            Key::End | Key::CtrlE => E::End,
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
            | Key::CtrlBracket
            | Key::CtrlCaret
            | Key::AltG
            | Key::AltB
            | Key::AltA
            | Key::AltR
            | Key::AltP
            | Key::AltN
            | Key::AltH
            | Key::PageUp
            | Key::PageDown
            | Key::WheelUp
            | Key::WheelDown
            | Key::Wheel { .. }
            | Key::Tab
            | Key::Click { .. }
            | Key::Press { .. }
            | Key::Drag { .. }
            | Key::Release { .. }
            | Key::Hover { .. }
            | Key::FocusIn
            | Key::FocusOut
            | Key::Background { .. } => {
                return None;
            }
        })
    }

    /// The switch this key is the chord of, read from [`Show::chord`]: the reverse lookup the dispatch uses, so a
    /// chord is advertised and acts from one entry rather than a pair of arms that each spelled their key twice.
    pub(crate) fn show(&self) -> Option<Show> {
        Show::ALL
            .into_iter()
            .find(|s| s.chord().is_some_and(|(_, k)| &k == self))
    }
}
