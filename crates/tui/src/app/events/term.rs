//! The terminal pane's frames: attached, status, output, ended.

use super::*;
use letibot_sessionlog::protocol::ServerFrame;

impl App {
    /// One family of [`App::apply`]'s arms: `apply` hands it only the terminal pane's frames.
    pub(crate) fn on_term_frame(&mut self, frame: ServerFrame) -> Disposition {
        match frame {
            ServerFrame::TermAttached {
                pane,
                command,
                opened,
            } => {
                // Every head learns the pane exists; only the head holding a pane that is
                // waiting for its number adopts it. A head drawing another pane, or none, is
                // told a fact about the session and nothing more.
                self.term_fact = self.term_fact.with(pane, &command);
                let adopt = self.term.as_ref().is_some_and(|p| p.pane.is_none());
                if !adopt {
                    return Disposition::Control;
                }
                // The number, and the keys that were waiting for it - see
                // [`App::pane_input`]. In order, and before anything typed after this frame.
                if let Some(p) = self.term.as_mut() {
                    p.pane = Some(pane);
                    let waiting = std::mem::take(&mut p.waiting);
                    if !waiting.is_empty() {
                        self.queued.push(Action::TermInput {
                            pane,
                            bytes: waiting,
                        });
                    }
                }
                if opened {
                    // The answer to this head's own `!term COMMAND`: the line it typed is
                    // already on the pane, and the number is all that was missing.
                    return Disposition::Control;
                }
                // The line is the daemon's: this head sent `!term` with no command, so only the
                // daemon knows what runs, and the verb goes back on for a person to read. The name
                // arrives before the bytes so the pane is never drawn unnamed.
                if let Some(p) = self.term.as_mut() {
                    p.line = format!("!term {command}");
                }
                // And the fact the head draws when it is not drawing the pane was set above,
                // from the same frame, so a head that detaches a moment later already knows what
                // to say.
                self.say(&format!(
                    "attached to pane {pane}, `!term {command}`. \
                     ctrl-\\ leaves it running, `!term close` ends it."
                ));
                Disposition::Control
            }
            // What this session's pane is running, or nothing - the answer to a read, and
            // the fact a head draws instead of a row when it is not drawing the pane (see
            // [`App::pane_behind`] and [`PaneFact`]).
            ServerFrame::TermStatus { panes } => {
                self.term_fact = if panes.is_empty() {
                    PaneFact::None
                } else {
                    PaneFact::Running(panes)
                };
                // A `!term close` held while the pane was unknown runs now, through the same
                // decision it would have taken had the head known - so the line is held, not
                // guessed, which is why `Unasked` is a state and not an `Option`.
                if self.close_pending {
                    self.close_pending = false;
                    let which = self.close_which.take();
                    if let Some(action) = self.begin_close(which) {
                        self.queued.push(action);
                    }
                }
                Disposition::Control
            }
            ServerFrame::TermOutput { pane, bytes } => {
                // No `redraw` here: it invalidates the glass, and a program repainting ten times a
                // second would rewrite every row each time - the flicker the diff encoder exists to
                // remove. Bytes for a pane this head does not hold are dropped, never opened.
                if let Some(p) = self.term.as_mut()
                    && p.pane == Some(pane)
                {
                    p.feed(&bytes);
                }
                Disposition::Control
            }
            ServerFrame::TermEnded { pane, reason } => {
                // The reason is always the daemon's, so a head never guesses why a rectangle came
                // back. It files a row rather than a notice: a program that dies at once is
                // invisible on the chrome, and a detached pane must not hide a death either.
                if let Some(n) = pane {
                    self.term_fact = self.term_fact.without(n);
                }
                let ours = self.term.as_ref().is_some_and(|p| p.pane == pane);
                if ours && let Some(p) = self.term.take() {
                    self.file_note(Note::Pane {
                        line: p.line.clone(),
                        said: p.last_rows(),
                        closed: p.closing,
                        reason,
                    });
                }
                Disposition::Control
            }
            _ => unreachable!("on_term_frame was handed a variant it does not handle"),
        }
    }
}
