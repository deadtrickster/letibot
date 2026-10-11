//! **The terminal pane's frames**: attached, status, output, ended.

use super::*;
use letibot_sessionlog::protocol::ServerFrame;

impl App {
    /// **The terminal pane**: attached, status, output, ended. One family of [`App::apply`]'s arms, moved here
    /// verbatim; `apply` hands it only these frames.
    pub(crate) fn on_term_frame(&mut self, frame: ServerFrame) -> Disposition {
        match frame {
            ServerFrame::TermAttached {
                pane,
                command,
                opened,
            } => {
                // **Every head learns the pane exists**; only the head holding a pane that is
                // waiting for its number adopts it. A head drawing another pane, or none, is
                // told a fact about the session and nothing more.
                self.term_fact = self.term_fact.with(pane, &command);
                let adopt = self.term.as_ref().is_some_and(|p| p.pane.is_none());
                if !adopt {
                    self.redraw = true;
                    return Disposition::Control;
                }
                // **The number, and the keys that were waiting for it** — see
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
                    self.redraw = true;
                    return Disposition::Control;
                }
                // **The daemon naming the pane this head attached to**, which is the half of the
                // attach the head cannot know: it sent `!term` with no command, and the head that
                // typed the original line may be another one or may have switched away. The
                // command is what the daemon was handed at `TermOpen` — with the verb stripped,
                // because that is how it received it — so the verb goes back on here, where the
                // line is a thing a person reads.
                //
                // **The bytes follow this frame**, so the pane is up and empty when this lands.
                // See `ClientFrame::TermOpen` for why the daemon sends the name first: a head
                // that drew the screen and learned what it was afterwards would flash a
                // rectangle it could not name.
                if let Some(p) = self.term.as_mut() {
                    p.line = format!("!term {command}");
                }
                // **And the fact the head draws when it is not drawing the pane** was set above,
                // from the same frame, so a head that detaches a moment later already knows what
                // to say.
                self.say(&format!(
                    "attached to pane {pane}, `!term {command}`. \
                     ctrl-\\ leaves it running, `!term close` ends it."
                ));
                self.redraw = true;
                Disposition::Control
            }
            // **What this session's pane is running, or nothing** — the answer to a read, and
            // the fact a head draws **instead of a row** when it is not drawing the pane (see
            // [`App::pane_behind`] and [`PaneFact`]).
            ServerFrame::TermStatus { panes } => {
                self.term_fact = if panes.is_empty() {
                    PaneFact::None
                } else {
                    PaneFact::Running(panes)
                };
                self.redraw = true;
                // **A `!term close` that was waiting for this answer runs now**, through the same
                // decision it would have taken had the head known — see [`App::begin_close`]. The
                // line is held rather than guessed, which is the whole reason `Unasked` is a
                // state and not an `Option`.
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
                // **A pane this head opened, fed its bytes.** Not an event and not counted as
                // one: `TermOutput` carries no seq, so it is `Control` for the same reason a
                // `Jobs` reply is — the ack's `rendered`/`filtered` are this head's disclosure
                // about *the batch*, and this frame is not in any batch.
                //
                // **A pane this head did NOT open is dropped, quietly.** The frames are fanned
                // out to every head of the session like events (see `TerminalDriver`), and a
                // second head attached to the same session has no
                // rectangle to draw them in. Dropping them is the honest reading of *a pane
                // this head did not open*, and the alternative — opening a pane from a frame
                // nobody asked for — would be a screen program appearing on a head that never
                // ran `!term`.
                //
                // **And no `redraw` flag**, which is the difference between a pane and a
                // transcript. `redraw` makes the driver call `Terminal::invalidate`, which
                // forgets the glass so the next frame is written whole — right for Ctrl-L, a
                // resize and a fold, and *wrong here*: a screen program repaints ten times a
                // second and the terminal's own diff writes exactly the rows that changed. A
                // flag per frame would pin the terminal rewriting all 24 rows ten times a
                // second, which is the flicker `term.rs`'s whole diff encoder exists to
                // remove. The frame is composed and drawn every tick either way — this flag
                // is about the *glass*, not about whether to draw.
                //
                // **And only the pane this head holds is fed** (protocol 44): a session's other
                // panes keep writing, their bytes reach every head, and the daemon keeps their
                // screens for the `!term N` that comes back to one.
                if let Some(p) = self.term.as_mut()
                    && p.pane == Some(pane)
                {
                    p.feed(&bytes);
                }
                Disposition::Control
            }
            ServerFrame::TermEnded { pane, reason } => {
                // **The one place a pane closes, and the reason always comes from the daemon**
                // — *"the program exited with 3"*, *"you closed the terminal"*, *"there is no pane
                // 4 running in this session"*. So a head never guesses why its rectangle
                // came back, and a refusal to start is the same frame as an ending.
                //
                // **And it becomes a row, not a notice.** This was `self.say(…)` — a sentence
                // on the chrome for `NOTICE_MS` — which is exactly why a program that dies at
                // once was *invisible*: the rectangle came back, one line appeared, and a few
                // seconds later the session said nothing about what had happened or what the
                // program had printed. The screen the program left is read here, while the
                // pane still holds it, and goes into the note with the daemon's sentence. See
                // [`Note::Pane`].
                //
                // **A pane this head had DETACHED from is the same arm, and that is the
                // point.** The pane is kept while the operator is away (see
                // [`TermPane::detached`]), the bytes keep arriving into its screen, and this
                // is where the row they would have seen had they been looking is filed — so
                // detaching does not hide a death. The only difference is that there is no
                // rectangle to give back, which `pane_open()` already accounts for.
                //
                // **Whose ending** (protocol 44): `None` is a refusal written to this head alone,
                // which can only be about the pane it is waiting to have numbered; a number is
                // one pane's, and this head files the row only if that is the pane it holds.
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
                    self.redraw = true;
                }
                Disposition::Control
            }
            _ => unreachable!("on_term_frame was handed a variant it does not handle"),
        }
    }
}
