//! **Events about the session and its heads**: a rename, a head attaching or leaving, a screen
//! asked for, a command another head issued.

use super::*;
use letibot_sessionlog::event::SessionEvent;

impl App {
    /// **The session and its heads**: a rename, attach/detach, a requested screen, an issued command. One family of
    /// [`App::event`]'s arms, moved here verbatim; `event` hands it only these variants.
    pub(crate) fn on_session_event(&mut self, e: SessionEvent) -> Disposition {
        match e {
            // The name of the session this head is *in*. Folded into the row this
            // head already holds rather than triggering a `ListSessions` round trip:
            // the event carries the whole of the change, and asking the daemon to
            // resend a list to learn something it just told us is how a head ends up
            // one frame behind its own screen.
            SessionEvent::SessionRenamed { title } => {
                let id = self.session_id.clone();
                if let Some(row) = self.sessions.iter_mut().find(|s| s.session_id == id) {
                    row.title = title.clone();
                }
                self.redraw = true;
                // Said out loud, because the header changes under the operator and an
                // unexplained change of the one label that identifies where you are
                // is worse than no label.
                self.say(&format!("this session is now called {title:?}"));
                Disposition::Control
            }
            SessionEvent::HeadAttached { .. } => {
                self.heads += 1;
                // **The `system` switch**: *"who attached, and who issued which command"* —
                // the same three events `>= Verbosity::Loud` gated, and now the switch the
                // three profiles name, so `loud` is the set that turns them on.
                if self.visibility.shows(Show::System) {
                    Disposition::Rendered
                } else {
                    Disposition::Filtered
                }
            }
            SessionEvent::HeadDetached { .. } => {
                self.heads = self.heads.saturating_sub(1);
                if self.visibility.shows(Show::System) {
                    Disposition::Rendered
                } else {
                    Disposition::Filtered
                }
            }
            // Into the transcript, where it happened.
            //
            // It used to be pinned to the bottom of the body: the last three
            // warnings sat above the status line forever, so a warning about turn
            // three was still shoving turn nine up the screen, and the operator had
            // no way to say "seen". A warning is an event with a place in the
            // conversation, and putting it there is what makes it scroll away like
            // one — and still be there when you scroll back.
            SessionEvent::ScreenRequested { req_id } => {
                // Queued, not answered here: the answer is the rows this head
                // DRAWS, and they do not exist until the frame is built. The
                // driver takes these after `screen()` and sends exactly what it
                // put on the terminal — anything rendered here instead would be
                // a second rendering, which is the reconstruction this whole
                // frame exists to avoid.
                self.screen_requests.push(req_id);
                self.redraw = true;
                // **And `Filtered` here too, for the same reason R53 gives one paragraph over.**
                // This is a `SessionEvent` — session content, read, and deliberately not drawn as
                // a row of its own, because the answer IS the rows this head draws. `Control`'s
                // definition is *"Not an event"*, and its own docstring above says the same thing
                // about this arm that `OperatorCallAllowed`'s says about its: a reader would never
                // find the difference. The two are changed together because they are one defect
                // spelled at two arms.
                Disposition::Filtered
            }
            SessionEvent::CommandIssued {
                head_id,
                identity,
                command,
                note,
                ..
            } => {
                // Two humans in one session: seeing who did what is the point — and
                // seeing *yourself* do what you just did is not. Our own routine
                // acceptances are already covered by `Accepted`.
                if head_id != self.head_id {
                    self.say(&format!("{identity} · {command}: {note}"));
                }
                if self.visibility.shows(Show::System) {
                    Disposition::Rendered
                } else {
                    Disposition::Filtered
                }
            }
            _ => unreachable!("on_session_event was handed an event it does not handle"),
        }
    }
}
