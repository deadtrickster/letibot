//! Events about the session and its heads: a rename, a head attaching or leaving, a screen
//! asked for, a command another head issued.

use super::*;
use letibot_sessionlog::event::SessionEvent;

impl App {
    /// The session and its heads: a rename, attach/detach, a requested screen, an issued command.
    /// One family of [`App::event`]'s arms; `event` hands it only these variants.
    pub(crate) fn on_session_event(&mut self, e: SessionEvent) -> Disposition {
        match e {
            // The name of the session this head is in. Folded into the row it already
            // holds: the event carries the whole change, and a `ListSessions` round trip
            // to learn what the daemon just said leaves the head a frame behind.
            SessionEvent::SessionRenamed { title } => {
                let id = self.session_id.clone();
                if let Some(row) = self.sessions.iter_mut().find(|s| s.session_id == id) {
                    row.title = title.clone();
                }
                // Said out loud, because the header changes under the operator and an
                // unexplained change of the one label that identifies where you are
                // is worse than no label.
                self.say(&format!("this session is now called {title:?}"));
                Disposition::Control
            }
            SessionEvent::HeadAttached { .. } => {
                self.heads += 1;
                // The `system` switch: who attached, and who issued which command. `loud`
                // is the profile that turns these three events on.
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
            SessionEvent::ScreenRequested { req_id } => {
                // Queued, not answered here: the answer is the rows this head DRAWS, and
                // they do not exist until `screen()` builds the frame. Rendering here
                // would be a second rendering of what the driver already sends.
                self.screen_requests.push(req_id);
                // `Filtered`, not `Control`: this is a `SessionEvent`, and `Control` means
                // "not an event". Its sibling arm says the same; the two move together
                // because they are one rule spelled at two arms.
                Disposition::Filtered
            }
            SessionEvent::CommandIssued {
                head_id,
                identity,
                command,
                note,
                ..
            } => {
                // Two humans in one session: seeing who did what is the point, and
                // seeing yourself do what you just did is not. Our own routine
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
