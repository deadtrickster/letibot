//! **A key, handled**: the dispatch from a keystroke to whichever card, pane, window or the
//! composer owns it, and the matching of a typed answer to a decision's options.

use super::*;
use letibot_sessionlog::view::OpenDecision;
use letibot_ui::editor::Reaction;

mod cards;
mod chords;
mod input;
mod panes;
mod window;
pub use input::key_of;

use std::ops::ControlFlow;

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
        if let ControlFlow::Break(r) = self.key_mode_confirm(&k) {
            return r;
        }
        if let ControlFlow::Break(r) = self.key_todo_draft(&k) {
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
        if let ControlFlow::Break(r) = self.key_chords(&k) {
            return r;
        }
        // **A click on an edit or write row opens rano on its change.** The whole row — its
        // header and the diff under it — is the target, because the row is one call about one
        // file and a reader aims at the diff as often as at the name. Measured against the last
        // frame's map (`file_rows`), which holds only rows the conversation drew this frame; a
        // click anywhere else falls through to whatever it did before.
        if let Key::Click { y, .. } = k
            && let Some(f) = self.file_at_row(y)
        {
            self.open_in_editor(&f);
            return None;
        }

        if let ControlFlow::Break(r) = self.key_sub_out(&k) {
            return r;
        }

        if let ControlFlow::Break(r) = self.key_job_out(&k) {
            return r;
        }

        if let ControlFlow::Break(r) = self.key_slash_out(&k) {
            return r;
        }

        if let ControlFlow::Break(r) = self.key_config_pane(&k) {
            return r;
        }
        if let ControlFlow::Break(r) = self.key_queue_review(&k) {
            return r;
        }

        if let ControlFlow::Break(r) = self.key_overlays(&k) {
            return r;
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
        if let ControlFlow::Break(r) = self.key_payload_window(&k) {
            return r;
        }
        if let ControlFlow::Break(r) = self.key_decision(&k) {
            return r;
        }

        if let ControlFlow::Break(r) = self.key_picker(&k) {
            return r;
        }

        if let ControlFlow::Break(r) = self.key_quit_card(&k) {
            return r;
        }
        if let ControlFlow::Break(r) = self.key_pick(&k) {
            return r;
        }

        if let ControlFlow::Break(r) = self.key_subagents_pane(&k) {
            return r;
        }

        if let ControlFlow::Break(r) = self.key_todos_pane(&k) {
            return r;
        }

        if let ControlFlow::Break(r) = self.key_queue_pane(&k) {
            return r;
        }

        if let ControlFlow::Break(r) = self.key_jobs_pane(&k) {
            return r;
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
        // No empty-composer guard here, unlike every pane key in this file: see the note at
        // the arm. An open decision keeps the operator in the child, said and not armed.
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
        // **Inside a subagent, a single Esc never reaches the composer**, and that is the
        // second fix of this arm. It used to step aside for a half-typed line and for an open
        // decision, and both handed the press to the editor — which counted it as the FIRST of
        // the Esc-Esc pair, so the operator's next Esc interrupted the child they were only
        // trying to leave: *"pressed esc and it didnt work, pressed second time - subagent
        // stopped lol"* (2026-10-08). A switch keeps the composer's text (it is the head's, not
        // the session's), so the draft goes up with the operator and nothing is lost by
        // leaving; a decision waiting in the child is the one thing that keeps them there, and
        // it is said rather than armed.
        if matches!(k, Key::Esc)
            && let Some(parent) = self.parent_session()
        {
            if !self.open.is_empty() {
                self.say("a decision is waiting in this subagent — answer it, then esc goes up");
                self.redraw = true;
                return None;
            }
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
    /// **Ctrl+`]` — between the conversation and the editor pane.**
    ///
    /// From the composer it opens rano on the newest file this conversation changed, or goes
    /// back into a pane already open; from inside the pane rano hands the same chord back and
    /// the keyboard returns to the composer (see `app/editor.rs`). `]` because every letter
    /// is a readline key, a composer key or one of the head's panes already, and `0x1d` was
    /// the one control byte this head decoded to nothing that rano does not bind either.
    CtrlBracket,
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
            | Key::CtrlBracket
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
