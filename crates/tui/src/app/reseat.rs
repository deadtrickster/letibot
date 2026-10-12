//! **The reseat card**: a session whose prompt is stale asks the operator, and keeps asking.
//!
//! The operator, 2026-10-12: *"regarding reseat - since it is important id suggest to show me
//! "select" with one option - reseat and abstain and then when on abstain keep reseat select
//! open. this way i keep reseat in mind because it stares at me yet can continue at my risk"*.
//!
//! # The two states, and what moves between them
//!
//! * **Open** — a card in the ask slot above the composer: what changed (the daemon's
//!   `prefix_stale` sentence, short, the whole of it one `tab` away) and two rows,
//!   [`RESEAT_NOW`] and [`NOT_NOW`]. It opens when the daemon says `prefix_stale` — on every
//!   attach to a stale session, and live when a restart's open finishes after the head is
//!   already watching — unless the operator has words in the composer, in which case it
//!   arrives pinned rather than taking their Enter.
//! * **Pinned** — one warning-coloured line above the composer, [`App::reseat_strip`], drawn on
//!   every frame while the prompt is stale: `PROMPT IS STALE … · alt-r reseats now · alt-p
//!   shows the card`. Both key hints are buttons (`app::keyhints`), so the strip is clickable.
//!
//! `Not now` and Esc put the open card into the pinned state — **abstaining never dismisses
//! it**. Typing into an empty composer does the same and the key goes to the composer, so the
//! card never stands between the operator and the line they started. `alt-p` (or a click on
//! the strip's hint) opens it again; `alt-r` reseats from anywhere.
//!
//! **Only the daemon takes it down**: `prefix_current` (the sentence was cleared — a re-seat,
//! or any fork onto the seated prompt) or `reseated`, which every head attached hears, so a
//! reseat chosen on one head clears the card on all of them. A `Hello` forgets it too, because
//! the daemon says `prefix_stale` again on every attach while it is still true — which is what
//! makes the card survive a head reattach and a daemon restart without this head remembering
//! anything.

use super::*;
use std::ops::ControlFlow;

/// The card's first row: the bare `/reseat`, which carries the conversation across.
pub(crate) const RESEAT_NOW: &str = "Reseat now — carry the conversation onto the current prompt";
/// **The first row once the daemon has said the conversation no longer fits verbatim**:
/// `/reseat summarise`, the reseat that can work. The operator pasted the card still offering
/// `alt-r reseats now` under the refusal that said it could not.
pub(crate) const RESEAT_SUMMARISE: &str =
    "Reseat with a summary — the conversation no longer fits onto the prompt verbatim";
/// The card's second row: abstain, which pins the card rather than closing it.
pub(crate) const NOT_NOW: &str = "Not now — continue on the old prompt (at my risk)";

/// **The reseat card's state** — `App::reseat` is `Some` exactly while the daemon says this
/// session's prompt is stale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReseatCard {
    /// The daemon's sentence, verbatim (`prefix_stale`'s detail).
    pub(crate) said: String,
    /// Open in the ask slot (`true`) or pinned above the composer (`false`).
    pub(crate) open: bool,
    /// The highlighted row: 0 reseat now, 1 not now.
    pub(crate) sel: usize,
    /// The whole sentence is shown rather than its short form.
    pub(crate) whole: bool,
    /// A reseat was sent from this head and the daemon has not answered it yet.
    pub(crate) asked: bool,
    /// The daemon refused a verbatim reseat because the conversation does not fit: the card
    /// offers, and alt-r sends, `/reseat summarise` instead.
    pub(crate) summarise: bool,
}

impl App {
    /// **The daemon said the prompt is stale.** A card that is already up keeps its state and
    /// takes the newer sentence: the daemon says it on every attach, so a second head attaching
    /// to the same session must not pop this head's pinned card open again.
    pub(crate) fn prefix_stale(&mut self, said: String) {
        match self.reseat.as_mut() {
            Some(c) => c.said = said,
            None => {
                self.reseat = Some(ReseatCard {
                    said,
                    open: self.editor.text().is_empty(),
                    sel: 0,
                    whole: false,
                    asked: false,
                    summarise: false,
                });
            }
        }
        self.redraw = true;
    }

    /// **The prompt is current**: the card comes down, wherever it was.
    pub(crate) fn prefix_current(&mut self) {
        if self.reseat.take().is_some() {
            self.redraw = true;
        }
    }

    /// **A reseat was refused** — the card is still true, and alt-r may send again. A refusal
    /// because the conversation does not fit verbatim (the daemon's sentence names
    /// `/reseat summarise` as what fits) turns the card's reseat into that one, and opens the
    /// card so the new choice is seen rather than discovered on the next alt-r.
    pub(crate) fn reseat_refused(&mut self, said: &str) {
        let empty = self.editor.text().is_empty();
        if let Some(c) = self.reseat.as_mut() {
            c.asked = false;
            if said.contains("/reseat summarise") && !c.summarise {
                c.summarise = true;
                c.sel = 0;
                c.open = empty;
            }
            self.redraw = true;
        }
    }

    /// **Something else holds the ask slot** — a decision, a prompt, a password, a picker, the
    /// quit card. The reseat card waits behind every one of them and is drawn as the strip
    /// meanwhile, so it is never off the screen.
    ///
    /// **And an open surface takes it too** (R90.1): a pane's keys are the pane's. With the card
    /// up over the todos pane, Enter on a row sent `/reseat` and `1` in the session picker
    /// reseated, while the bar named the pane's keys. While a pane has the keyboard the card is
    /// the strip, and it comes back when the pane closes.
    pub(crate) fn slot_taken(&self) -> bool {
        self.focused_surface().is_some()
            || self.secret.is_some()
            || self.term_ask.is_some()
            || (self.prompt.is_some() && !self.prompt_away)
            || !self.open.is_empty()
            || self.key_ask.is_some()
            || self.todo_draft.is_some()
            || self.quit_card
            || self.pick.is_some()
            || self.mode_confirm.is_some()
    }

    /// **The open card is what the slot shows.**
    pub(crate) fn reseat_card_up(&self) -> bool {
        self.reseat.as_ref().is_some_and(|c| c.open) && !self.slot_taken()
    }

    /// **Reseat now** — exactly the line `/reseat` typed and sent, so the same refusals (no
    /// session yet, a read-only attach) and the same notice apply.
    pub(crate) fn reseat_now(&mut self) -> Option<Action> {
        match self.reseat.as_ref() {
            None => {
                self.say("this session speaks the current prompt — there is nothing to reseat");
                return None;
            }
            Some(c) if c.asked => {
                self.say(
                    "a reseat is already on its way — the card goes when the daemon says it landed",
                );
                return None;
            }
            Some(_) => {}
        }
        let line = if self.reseat.as_ref().is_some_and(|c| c.summarise) {
            "/reseat summarise"
        } else {
            "/reseat"
        };
        let action = self.submit(line.to_string());
        if let Some(c) = self.reseat.as_mut() {
            c.open = false;
            c.asked = action.is_some();
        }
        self.redraw = true;
        action
    }

    /// **Not now** — the card is pinned, never dismissed.
    pub(crate) fn reseat_abstain(&mut self) {
        if let Some(c) = self.reseat.as_mut() {
            c.open = false;
            c.sel = 0;
            self.redraw = true;
        }
    }

    /// **Open the card again** (`alt-p`, or a click on the strip's hint).
    ///
    /// **While something else holds the slot nothing would visibly happen** (R90.21) — the card
    /// would pop up later, maybe in front of an Enter meant for something else — so it says so
    /// and stays pinned.
    pub(crate) fn reseat_expand(&mut self) {
        let taken = self.slot_taken();
        match self.reseat.as_mut() {
            Some(_) if taken => self.say(
                "the reseat card waits behind what is open now — close it, then alt-p shows the \
                 card (alt-r reseats from anywhere)",
            ),
            Some(c) => {
                c.open = true;
                self.redraw = true;
            }
            None => self.say("this session speaks the current prompt — there is nothing to reseat"),
        }
    }

    /// **The open card's keys**, while it is the card on the screen and the composer is empty.
    ///
    /// ↑↓ move, Enter and `1`/`2` choose, Tab shows the whole sentence, Esc is `Not now`.
    /// **Anything typed pins the card and goes to the composer** — the operator continuing to
    /// work is the abstention, and their key is not swallowed for it.
    pub(crate) fn key_reseat_card(&mut self, k: &Key) -> ControlFlow<Option<Action>> {
        if !self.reseat_card_up() {
            return ControlFlow::Continue(());
        }
        let typing = !self.editor.text().is_empty();
        let choose = |a: &mut App, at: usize| {
            if at == 0 {
                ControlFlow::Break(a.reseat_now())
            } else {
                a.reseat_abstain();
                ControlFlow::Break(None)
            }
        };
        match k {
            Key::Up | Key::Down if !typing => {
                if let Some(c) = self.reseat.as_mut() {
                    c.sel = 1 - c.sel.min(1);
                }
                self.redraw = true;
                ControlFlow::Break(None)
            }
            Key::Tab if !typing => {
                if let Some(c) = self.reseat.as_mut() {
                    c.whole = !c.whole;
                }
                self.redraw = true;
                ControlFlow::Break(None)
            }
            Key::Enter if !typing => {
                let at = self.reseat.as_ref().map_or(1, |c| c.sel);
                choose(self, at)
            }
            _ if !typing && digit_row(k, 2).is_some() => {
                let at = digit_row(k, 2).expect("just checked");
                choose(self, at)
            }
            Key::Esc => {
                self.reseat_abstain();
                ControlFlow::Break(None)
            }
            Key::Char(_) | Key::Paste(_) | Key::Backspace | Key::Delete | Key::SoftEnter => {
                self.reseat_abstain();
                ControlFlow::Continue(())
            }
            Key::Enter => {
                self.reseat_abstain();
                ControlFlow::Continue(())
            }
            _ => ControlFlow::Continue(()),
        }
    }
}
