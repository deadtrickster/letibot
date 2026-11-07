//! The daemon's half of the model's own verbs (X24): each one becomes the same queued command the
//! operator's head sends, so a session moves itself with no tmux, no head and nobody watching.
//!
//! The queue is what makes it safe. A command arrives while the turn that asked for it is still
//! running, so it waits for that turn to end - which is exactly when a re-seat or a compaction can
//! happen at all - and the daemon then performs it through the door the head's own verb takes, with
//! this session as the identity it is recorded under.

use std::sync::Arc;

use letibot_sessionlog::hub::{CommandKind, Hub};
use letibot_tools::builtins::session_self::{NagAsk, SessionSelf};

/// One session's desk: its id, and the queue its own verbs land in.
pub struct SessionAsks {
    session: String,
    hub: Arc<Hub>,
}

impl SessionAsks {
    pub fn new(session: impl Into<String>, hub: Arc<Hub>) -> SessionAsks {
        SessionAsks {
            session: session.into(),
            hub,
        }
    }

    /// Put one command in this session's own queue, as the session itself.
    fn queue(&self, kind: CommandKind, id: &str, said: String) -> Result<String, String> {
        let identity = format!("agent:{}", self.session);
        if !self.hub.submit_daemon(identity, format!("self-{id}"), kind) {
            return Err(format!(
                "this session's queue is closed, so the {id} it asked for was not taken. \
                 Nothing was changed."
            ));
        }
        Ok(said)
    }
}

impl SessionSelf for SessionAsks {
    fn session(&self) -> String {
        self.session.clone()
    }

    fn reseat(&self, summarise: bool) -> Result<String, String> {
        kind_reseat(self, summarise)
    }

    fn compact(&self) -> Result<String, String> {
        self.queue(
            CommandKind::Compact,
            "compact",
            "compacting this conversation when the turn ends: one summary replaces the history, \
             and the summary is the base every later turn reads"
                .into(),
        )
    }

    fn nag_pause(&self, ask: NagAsk) -> Result<String, String> {
        let (line, said) = match ask {
            NagAsk::Pause(None) => (
                "nag pause".to_string(),
                "this session's plan-check is paused when the turn ends, until `/nag resume`"
                    .to_string(),
            ),
            NagAsk::Pause(Some(for_how)) => (
                format!("nag pause {for_how}"),
                format!("this session's plan-check is paused for `{for_how}` when the turn ends"),
            ),
            NagAsk::Resume => (
                "nag resume".to_string(),
                "this session's plan-check is resumed when the turn ends".to_string(),
            ),
        };
        self.queue(CommandKind::Slash { line }, "nag", said)
    }
}

/// The re-seat, with the sentence the caller is told: taken out of [`SessionAsks`] only because the
/// message is the longest of the three and this keeps the trait's body one line each.
fn kind_reseat(desk: &SessionAsks, summarise: bool) -> Result<String, String> {
    let said = if summarise {
        "re-seating this session when the turn ends: the conversation is summarised and the \
         summary replaces it, on the prompt this seat holds now"
    } else {
        "re-seating this session when the turn ends: every item is carried across as it is onto \
         the prompt this seat holds now, and the next turn re-sends all of it once"
    };
    desk.queue(CommandKind::Reseat { summarise }, "reseat", said.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The verb the model calls puts the SAME command in the queue the operator's own verb does,
    /// with no head and no tmux - which is the whole of X24's first half. Read off the queue itself
    /// rather than off a reply, because the reply is the tool's opinion of what it did.
    #[test]
    fn the_model_reseats_itself_without_tmux() {
        let hub = Hub::new("s-mine");
        let desk = SessionAsks::new("s-mine", hub.clone());
        let said = desk.reseat(false).expect("the command is taken");
        assert!(said.contains("re-seating"), "{said}");

        let queued = hub.try_command().expect("the re-seat is queued");
        assert_eq!(
            queued.kind,
            CommandKind::Reseat { summarise: false },
            "the head's own `/reseat` is this command and no other"
        );
        assert_eq!(queued.head_id, letibot_sessionlog::hub::DAEMON_SUBMITTER);
        assert_eq!(
            queued.identity, "agent:s-mine",
            "the session asked, and the row says so"
        );

        // A summarising re-seat is the same verb with the flag, which is `/reseat summarise`.
        desk.reseat(true).expect("taken");
        assert_eq!(
            hub.try_command().expect("queued").kind,
            CommandKind::Reseat { summarise: true }
        );

        // And the other two verbs are the daemon's own commands, not a second implementation.
        desk.compact().expect("taken");
        assert_eq!(
            hub.try_command().expect("queued").kind,
            CommandKind::Compact
        );
        desk.nag_pause(NagAsk::Pause(Some("30m".into())))
            .expect("taken");
        assert_eq!(
            hub.try_command().expect("queued").kind,
            CommandKind::Slash {
                line: "nag pause 30m".into()
            }
        );
        desk.nag_pause(NagAsk::Resume).expect("taken");
        assert_eq!(
            hub.try_command().expect("queued").kind,
            CommandKind::Slash {
                line: "nag resume".into()
            }
        );
        assert_eq!(desk.session(), "s-mine");
    }
}
