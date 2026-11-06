//! The global chords: the keys that mean the same thing whatever is open.

use super::*;
use std::ops::ControlFlow;

impl App {
    pub(crate) fn key_chords(&mut self, k: &Key) -> ControlFlow<Option<Action>> {
        match k {
            // A switch's chord, from the table that advertises it, and through `/verbosity`'s own
            // writer: a chord must not write the fields the drawing reads while `Visibility` goes
            // on saying the old set. `set_verbosity` refuses an undrawable set and says what moved.
            k if k.show().is_some() => {
                let s = k.show().expect("the guard just asked the same question");
                // The thinking is one verb with two spellings, so its chord is not `/verbosity`'s:
                // at a rung that hides the fold the openness there is the LIFT on a run, and
                // `ctrl-r` must close what a click opened. See [`App::set_thinking`].
                if s == Show::Thinking {
                    return ControlFlow::Break(self.flip_thinking());
                }
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
                // A picture on the clipboard is what this key attaches, and that comes first -
                // a silent `osascript` lookup is the whole cost when there is none. Not while a
                // window is open: that press must close the result the reader opened.
                if self.payload_sel.is_none() && self.attach_from_clipboard() {
                    return ControlFlow::Break(None);
                }
                // One row, not a switch: a chord cannot be named by a per-row seam and mean the
                // whole conversation, so the wall's unfold stays `/t`. Under `conversation` the
                // same chord opens the run - `newest_openable` decides, so seam and chord agree.
                if self.payload_sel.is_some() {
                    self.payload_sel = None;
                    self.payload_page = 0;
                    // And the switch goes with the panel - `drop_lift` takes both, so a run
                    // this key opened cannot outlive the window that named it. (The two fields
                    // this replaces were cleared here by hand and one of them was missed.)
                    self.drop_lift();
                    // And an open run closes: it was opened by this key, so this key closes it,
                    // or the second press would open a payload window inside a row that is only
                    // on screen because the run is open.
                    self.invalidate_history();
                } else if let Some(id) = self.newest_openable() {
                    // The window opens at its top, through the one door - see
                    // [`App::open_payload_window`]. A new result never inherits the offset of the
                    // one the reader was in.
                    self.open_payload_window(id);
                }
                // Not `refold`. That is the fold's own: it resets the scroll and
                // announces the fold state, and neither happened here.
                self.invalidate_history();
                return ControlFlow::Break(None);
            }
            Key::CtrlL => {
                // Something else wrote to the terminal, so the painter's record of the glass is
                // wrong and only a full erase puts it right.
                self.repaint = true;
                return ControlFlow::Break(None);
            }
            // The panes' chords, read from the table that advertises them (`Surface::chord`) and
            // through the one door the verbs use, as toggles of the surface stack: the pane in
            // front closes, a pane behind another comes to the front.
            k if Surface::of_chord(k).is_some() => {
                let s = Surface::of_chord(k).expect("the guard just asked");
                return ControlFlow::Break(self.toggle_surface(s));
            }
            // Hold the view: while held the head writes nothing at all, so a mouse selection
            // survives a streaming turn, and the second press releases it and says how much
            // arrived. The contract is in [`App::toggle_hold`].
            Key::CtrlP => return ControlFlow::Break(self.toggle_hold()),
            // Clearing your own screen costs one key, and it calls the VERB rather than
            // reimplementing it: `notes_command` is the one writer of the key set, the
            // watermark, the file and the sentence. The empty press says so, not nothing.
            Key::CtrlN => {
                // One writer, and it is the verb: the counting, the mark and the sentence all
                // live in `notes_command`'s `dismiss all` arm, which sets the watermark as well
                // as the key set. A chord that counted here would be a second opinion.
                let _ = self.notes_command("notes", "dismiss all");
                return ControlFlow::Break(None);
            }
            // Ctrl+O: move the running command to the background, the same action
            // `/promote` names. See [`App::promote`] for why the fact it guards is a
            // running CALL and not a running turn.
            Key::CtrlO => return ControlFlow::Break(self.promote()),
            // Ctrl+]: to the editor pane - back into it when it is open, onto the newest
            // change when it is not. See [`App::editor_chord`].
            Key::CtrlBracket => return ControlFlow::Break(self.editor_chord()),
            // The wheel and the page keys move what is on the SCREEN: the subagent output view
            // takes them, an open payload window takes them, and any other screen on top
            // swallows them, because a view that is not on the screen does not move.
            Key::PageUp | Key::PageDown | Key::WheelUp | Key::WheelDown
                if self.payload_window_open() && self.drawn() == Drawn::Conversation =>
            {
                let by = match k {
                    Key::WheelUp | Key::WheelDown => 3,
                    _ => self.screen_rows.max(1) / 2 + 1,
                };
                self.page_payload(matches!(k, Key::PageUp | Key::WheelUp), by);
                return ControlFlow::Break(None);
            }
            Key::PageUp | Key::PageDown | Key::WheelUp | Key::WheelDown => {
                // A page is a screen, not ten lines. `PageUp` moved by a constant ten,
                // which on a 40-row terminal is a quarter of the page the key is named for
                // - and on the tail path it compounded with the crawl `scroll_up` fixes.
                let (up, by) = match k {
                    Key::PageUp => (true, self.screen_rows.max(1)),
                    Key::PageDown => (false, self.screen_rows.max(1)),
                    // Three lines a notch: a transcript row can be two screen rows after
                    // wrapping, and a notch that moves one wrapped row reads as nothing
                    // happened.
                    Key::WheelUp => (true, 3),
                    // Three lines a notch here too - the card, the panes and the windows page
                    // by it below. The wheel's own down path replaces `by` at the foot of this
                    // arm, after every screen that could take the notch: `wheel_down_notch`.
                    _ => (false, 3),
                };
                // An open card takes them while its own content has somewhere to go: it is what
                // needs an answer and already owns Up/Down, and the keys must not cost the
                // transcript its scroll when the content fits. Width is the last frame's.
                if self.scroll_card(up, by) {
                    return ControlFlow::Break(None);
                }
                if self.scroll_tail_overlay(up, by) {
                    return ControlFlow::Break(None);
                }
                // An open pane takes them, or a pane longer than the terminal has a tail nobody
                // can read. Which pane is the surface stack's answer, not a list of fields kept
                // by hand; the two pickers are excluded, keeping their cursor in view themselves.
                if self.pick.is_some() {
                    return ControlFlow::Break(None);
                }
                // The surface in front takes them through its own table, and a surface whose
                // table does not name them takes them and moves nothing: a view that is not the
                // one on the screen does not move.
                if let Some(s) = self.focused_surface() {
                    if let Some(b) = self.binding_for(s, k) {
                        return self.surface_act(s, b.act, k);
                    }
                    return ControlFlow::Break(None);
                }
                self.scroll_conversation(k, up, by);
                return ControlFlow::Break(None);
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }

    /// The decision card's content window takes a scroll while it has somewhere to go; true
    /// when it moved. Whatever card holds the slot drew the last frame's numbers, so a card
    /// being up does not cost the transcript its scroll. See `App::card_window`.
    pub(crate) fn scroll_card(&mut self, up: bool, by: usize) -> bool {
        if self.dec_content_len <= self.dec_content_room {
            return false;
        }
        let max = self.dec_content_len - self.dec_content_room;
        self.dec_scroll = if up {
            self.dec_scroll.saturating_sub(by)
        } else {
            (self.dec_scroll + by).min(max)
        };
        true
    }

    /// The focused surface's own scroll: an output view's tail, nothing for the session
    /// picker (it keeps its cursor in view itself), and `pane_scroll` for every pane drawn
    /// through `pane_window`.
    pub(crate) fn scroll_pane(&mut self, up: bool, by: usize) {
        if self.scroll_tail_overlay(up, by) || self.focused_surface() == Some(Surface::Picker) {
            return;
        }
        // The polarity is the opposite of the transcript's: `scroll` counts rows back from the
        // BOTTOM (the transcript is read from its tail), `pane_scroll` rows hidden above the
        // TOP. So down is the one that grows.
        let max = self.pane_len.saturating_sub(self.pane_room);
        self.pane_scroll = if up {
            self.pane_scroll.saturating_sub(by)
        } else {
            (self.pane_scroll + by).min(max)
        };
    }

    /// The conversation's own scroll - a page key's or a wheel notch's, once nothing in front
    /// of the conversation took it.
    pub(crate) fn scroll_conversation(&mut self, k: &Key, up: bool, by: usize) {
        if up {
            // Through `scroll_up`, which renders as it goes: the frame clamps against the rows
            // rendered so far, so assigning `scroll` here could never express "further up than
            // I have drawn".
            self.scroll_up(by);
        } else {
            // Down is a WALK, the wheel included, and arriving at the tail is what resumes
            // following. The notch is sized here and not in the `(up, by)` match above, which the
            // card, the panes and the windows share; only this walk gathers speed on a run.
            let by = match k {
                Key::WheelDown => self.wheel_down_notch(),
                _ => by,
            };
            self.hold(by as isize);
        }
    }
}
