//! One daemon, several sessions: the `Harness` per `Hub`.
//!
//! ```text
//!   Registry ── "s-1" → Hub ──┐            ┌── Harness(s-1)  engine · ledger · region · store
//!            ── "s-2" → Hub ──┼─ Bell ─> Sessions ── Harness(s-2)  … its own of each
//!            ── "s-3" → Hub ──┘            └── (s-3: not opened yet)
//! ```
//!
//! # What is per session and what is shared, exactly
//!
//! Shared, because it is read-only and expensive: [`Parts`] — the vocabulary GGUF
//! and the dialect's renderer and parser. One 0.6 s `vocab_only` load for the
//! process, borrowed by every session's engine.
//!
//! Per session, because the prefix invariant is: **the `TurnEngine`, the `Session`,
//! the `TokenLedger` and its `TokenRegion`, the `ToolRuntime` and the store's
//! transcript row.** None of that is arranged here — it falls out of opening one
//! [`Harness`] per hub. `TokenLedger::new` creates its own memfd, `TokenRegion` is
//! not `Clone` and appending takes `&mut`, so *two sessions sharing a token region
//! is not an expression this crate could write.* That is the guarantee, and it is
//! structural rather than upheld.
//!
//! # Sessions are opened lazily, and that is a decision
//!
//! The daemon's first session is opened eagerly at startup, because that is where a
//! dialect that does not fit the vocabulary, a missing GGUF or an unwritable store
//! is caught — all three are silent at run time, which is why `Harness::open` raises
//! them at open time.
//!
//! A session created later by a head is opened on its **first command**. It uses
//! the same `Parts` and the same `Config`, so the three failures above cannot
//! newly appear: if they were going to, the daemon would not have started. What can
//! still fail is the store, and that failure is published as a `Warning` **on that
//! session's own log**, which is where the head that just prompted into it is
//! looking. A creation that reserved nothing and cost nothing is also what makes
//! `/new` instant rather than a two-second pause on a busy box.

use std::collections::HashMap;

use letibot_sessionlog::hub::{CommandKind, QueuedCommand};
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_sessionlog::{SessionEvent, hub::Hub};
use std::sync::Arc;

use crate::config::Config;
use crate::harness::{Harness, HarnessError, Parts, Reply};

/// What the worker did with one command.
pub enum Outcome {
    Replied(Box<Reply>),
    Failed(String),
    Ignored,
}

/// Every session this daemon is serving, and the harness behind each one.
pub struct Sessions<'a> {
    parts: &'a Parts,
    /// The command line, minus the session id. Cloned per session with its own id
    /// substituted, so a second session is the same daemon and not a second
    /// configuration nobody typed.
    base: Config,
    registry: Arc<Registry>,
    open: HashMap<String, Harness<'a>>,
}

impl<'a> Sessions<'a> {
    /// Start with one session, opened eagerly. See the module header for why the
    /// first one is not lazy.
    pub fn open_first(
        parts: &'a Parts,
        cfg: Config,
        registry: Arc<Registry>,
    ) -> Result<Sessions<'a>, HarnessError> {
        let id = cfg.session_id.clone();
        let hub = registry
            .get(&id)
            .ok_or_else(|| HarnessError::Setup(format!("session {id} is not in the registry")))?;
        let harness = Harness::open(parts, cfg.clone(), hub)?;
        let mut open = HashMap::new();
        open.insert(id, harness);
        Ok(Sessions {
            parts,
            base: cfg,
            registry,
            open,
        })
    }

    /// What a session is attached to, from the daemon's own command line.
    pub fn wiring(cfg: &Config) -> SessionWiring {
        SessionWiring {
            model: cfg.model.clone(),
            dialect: cfg.dialect.name().to_string(),
            endpoint: cfg.endpoint.authority(),
            workspace: cfg.workspace.display().to_string(),
        }
    }

    pub fn registry(&self) -> &Arc<Registry> {
        &self.registry
    }

    /// The harness for `session_id`, opening it if a head made the session and
    /// nothing has run in it yet.
    ///
    /// `Err` is a session that cannot be served at all; the caller announces it on
    /// that session's log rather than taking the daemon down, because every other
    /// session is still fine.
    fn harness(&mut self, session_id: &str) -> Result<&mut Harness<'a>, HarnessError> {
        if !self.open.contains_key(session_id) {
            let hub = self.registry.get(session_id).ok_or_else(|| {
                HarnessError::Setup(format!("no session {session_id} in this daemon"))
            })?;
            let cfg = Config {
                session_id: session_id.to_string(),
                ..self.base.clone()
            };
            let h = Harness::open(self.parts, cfg, hub)?;
            self.open.insert(session_id.to_string(), h);
        }
        Ok(self.open.get_mut(session_id).expect("just inserted"))
    }

    /// Sessions with a harness behind them. The rest exist and have never run.
    pub fn opened(&self) -> usize {
        self.open.len()
    }

    /// Submit one prompt to a session and wait for the answer.
    ///
    /// The scripted path (`harnessd --prompt …`), which has no head and no socket
    /// traffic. It goes through the same `harness()` as the worker, so a scripted
    /// run and a driven one open a session the same way.
    pub fn submit(&mut self, session_id: &str, text: &str) -> Result<Reply, HarnessError> {
        let hub = self.registry.get(session_id);
        let harness = self.harness(session_id)?;
        match harness.submit(text) {
            Ok(r) => Ok(r),
            Err(e) => {
                let turn_id = harness.last_turn_id().to_string();
                if let Some(hub) = &hub {
                    publish_failure(hub, &turn_id, &e);
                }
                Err(e)
            }
        }
    }

    /// Read-only access to a session's harness, for a caller that wants the ledger
    /// or the prefix. `None` for a session nothing has run in yet.
    pub fn harness_of(&self, session_id: &str) -> Option<&Harness<'a>> {
        self.open.get(session_id)
    }

    /// Run one command against its session.
    pub fn dispatch(&mut self, session_id: &str, cmd: &QueuedCommand) -> Outcome {
        let hub = self.registry.get(session_id);
        let harness = match self.harness(session_id) {
            Ok(h) => h,
            Err(e) => {
                if let Some(hub) = &hub {
                    hub.publish(SessionEvent::Warning {
                        code: "session_unavailable".into(),
                        detail: format!(
                            "this session could not be opened, so nothing was run: {e}"
                        ),
                    });
                }
                return Outcome::Failed(e.to_string());
            }
        };
        match &cmd.kind {
            CommandKind::Prompt { text } => match harness.submit(text) {
                Ok(reply) => Outcome::Replied(Box::new(reply)),
                Err(e) => {
                    let turn_id = harness.last_turn_id().to_string();
                    if let Some(hub) = &hub {
                        publish_failure(hub, &turn_id, &e);
                    }
                    Outcome::Failed(e.to_string())
                }
            },
            // Between turns there is nothing to interrupt. Announced rather than
            // dropped: "I pressed the key and nothing happened" is the report this
            // avoids.
            CommandKind::Interrupt { reason } => {
                if let Some(hub) = &hub {
                    hub.publish(SessionEvent::Warning {
                        code: "interrupt_idle".into(),
                        detail: format!(
                            "interrupt ({reason}) arrived between turns; nothing was generating"
                        ),
                    });
                }
                Outcome::Ignored
            }
            CommandKind::Answer { req_id, .. } => {
                if let Some(hub) = &hub {
                    hub.publish(SessionEvent::Warning {
                        code: "no_adjudication".into(),
                        detail: format!(
                            "an answer to {req_id} arrived, but M1 has no adjudication: \
                             read-only tools never ask (clause 4)"
                        ),
                    });
                }
                Outcome::Ignored
            }
        }
    }
}

/// Announce a failed turn on the session's own log, as a **terminal** event.
///
/// `crates/ui/DESIGN.md` §4.5, paid. This used to publish a `Warning` and nothing
/// else, so a head's `TurnState` stayed `Running` for the rest of the session and
/// its spinner span at a dead turn — the head covered for it with *"nothing
/// received for 17.0s"*, which cannot tell a failed turn from a slow one. Only the
/// party that saw the error can, and that party is this function.
///
/// Both frames go out: `TurnFailed` is what stops the spinner and puts the reason
/// under the turn, and the `Warning` is what puts it in the scrollback with a code
/// somebody can grep the log for. They are not the same disclosure — one is state,
/// the other is history — and neither is a substitute for the other.
fn publish_failure(hub: &Arc<Hub>, turn_id: &str, e: &HarnessError) {
    hub.publish(SessionEvent::TurnFailed {
        turn_id: turn_id.to_string(),
        error: e.to_string(),
        // §5.7 commits nothing on a failed turn, so there is no partial to keep.
        // The field is here so a future failure that *does* keep one cannot arrive
        // looking like this.
        partial_kept: false,
    });
    hub.publish(SessionEvent::Warning {
        code: "turn_failed".into(),
        detail: e.to_string(),
    });
}
