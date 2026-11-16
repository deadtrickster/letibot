//! The daemon's pending asks: every card a session raised that its own heads cannot answer, kept
//! once and shown on the daemon's main session. A head attaching to the main session is given each
//! open one, and an ask settled anywhere is taken off every head.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};

use letibot_transcript::pending::{Pending, PendingKind};

use crate::event::{Decider, DecisionOutcome, SessionEvent};
use crate::hub::{AnswerSink, Hub, Reply};

struct Entry {
    pending: Pending,
    card: SessionEvent,
    sink: Arc<dyn AnswerSink>,
}

#[derive(Default)]
struct Inner {
    main: Option<Weak<Hub>>,
    open: BTreeMap<String, Entry>,
    askers: BTreeMap<String, Asker>,
    annotator: Option<Arc<dyn Annotator>>,
    #[cfg(feature = "tools")]
    oracle: Option<Arc<dyn letibot_tools::Adjudicator>>,
}

/// A queue session named for the operator: its role and work, and its workspace in short.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Asker {
    /// `reviewer of agent-p2-followups (rano review queue)`.
    pub role: String,
    /// The workspace path's prefix as the gate writes it, and the words it is shown as.
    pub workspace: String,
    pub workspace_short: String,
}

/// `card` with its asker named by `asker`: the role on the `SubagentAsk`, and every word of
/// `because` under the asker's workspace written in short.
pub fn named(mut card: SessionEvent, asker: &Asker) -> SessionEvent {
    if let SessionEvent::DecisionRequested {
        subagent, because, ..
    } = &mut card
    {
        if let Some(s) = subagent.as_mut() {
            s.role = asker.role.clone();
        }
        if !asker.workspace.is_empty() {
            *because = because
                .split(' ')
                .map(|w| match w.starts_with(&asker.workspace) {
                    true => asker.workspace_short.as_str(),
                    false => w,
                })
                .collect::<Vec<_>>()
                .join(" ");
        }
    }
    card
}

/// A relayed permission call, as the annotator is shown it.
#[derive(Debug, Clone, Default)]
pub struct RelayedCall {
    pub req_id: String,
    /// The session that asked.
    pub asker: String,
    /// The asker's role, when it is a named queue session: `reviewer of …`, `…'s gatekeeper`.
    pub role: Option<String>,
    pub tool: String,
    pub arguments: serde_json::Value,
    /// Layer A's reading, in the asker's own surroundings.
    pub reading: String,
    /// The path, when the ask is ONLY a read of a path outside the asker's view: a read class,
    /// `may_approve`, and a card that offers the read-only rung.
    pub view_read: Option<String>,
    /// The asker's own gate. `None` is an asker whose rules nobody passed, and nothing acts on it.
    pub gate: Option<AskerGate>,
}

/// What the asking session's gate holds, for a judge of its call: its real workspace and its
/// permission rules in order, so a rule written for another session never covers this one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AskerGate {
    pub workspace: std::path::PathBuf,
    pub rules: Vec<AskerRule>,
}

/// One permission rule of the asker's, as `permission pattern action`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskerRule {
    pub permission: String,
    pub pattern: String,
    /// `allow`, `deny` or `ask`.
    pub action: String,
}

/// What the annotator says about a card. A line only: a relayed card is answered by the operator
/// or by the set's oracle ([`PendingAsks::oracle`]), never by its annotator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Note {
    /// One line on the card, which still goes to the operator.
    Line(String),
}

/// What the main session says beside a relayed card.
pub trait Annotator: Send + Sync {
    /// The card's line, or `None`. Bounded by the annotator's own deadline.
    fn line(&self, call: &RelayedCall) -> Option<String>;
    /// The operator's answer to a card `line` was asked about.
    fn answered(&self, req_id: &str, effect: &str);
    /// The card's note: its line, as [`Note`].
    fn note(&self, call: &RelayedCall) -> Option<Note> {
        self.line(call).map(Note::Line)
    }
    /// The operator took back a grant the set's oracle gave.
    fn revoked(&self, _grant: &RevokedGrant) {}
}

/// A kept grant the annotator answered, revoked by the operator: the store's row id, the
/// session it was kept for and the path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevokedGrant {
    pub id: i64,
    pub session: String,
    pub path: String,
}

/// The set, one per daemon. Keyed by the card's `req_id`, which is the ask's id on every surface.
#[derive(Default)]
pub struct PendingAsks {
    inner: Mutex<Inner>,
}

impl PendingAsks {
    pub fn new() -> Arc<PendingAsks> {
        Arc::new(PendingAsks::default())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The session whose heads answer for the daemon. Every open ask is delivered to it now, so a
    /// main session that was reopened shows what the old one showed.
    pub fn set_main(self: &Arc<Self>, hub: &Arc<Hub>) {
        self.lock().main = Some(Arc::downgrade(hub));
        self.deliver(hub);
    }

    /// The main session's hub, while it is open.
    pub fn main(&self) -> Option<Arc<Hub>> {
        self.lock().main.as_ref().and_then(Weak::upgrade)
    }

    /// Keep `card` and post it on the main session, with `sink` as its answer path. `false` when
    /// there is no main session or `card` is not a `DecisionRequested`: the asker then has nobody.
    pub fn post(self: &Arc<Self>, card: SessionEvent, sink: Arc<dyn AnswerSink>) -> bool {
        let Some(main) = self.main() else {
            return false;
        };
        let SessionEvent::DecisionRequested {
            req_id,
            kind,
            subagent,
            ..
        } = &card
        else {
            return false;
        };
        let session = subagent
            .as_ref()
            .map_or_else(|| main.session_id(), |s| s.handle.clone());
        let pending = Pending::new(PendingKind::of_card(kind), session, req_id.clone());
        let req_id = req_id.clone();
        self.lock().open.insert(
            req_id.clone(),
            Entry {
                pending,
                card: card.clone(),
                sink: sink.clone(),
            },
        );
        main.post_relayed(card, self.settling(&req_id, sink))
    }

    /// The ask was settled where it was raised, or withdrawn: drop it and take its card off the
    /// main session's heads with `close`, the asker's own `DecisionAnswered`.
    pub fn settle(&self, req_id: &str, close: SessionEvent) -> bool {
        let had = self.lock().open.remove(req_id).is_some();
        if let Some(main) = self.main() {
            main.withdraw_relayed_with(req_id, close);
        }
        had
    }

    /// [`PendingAsks::settle`] with a plain withdrawal as the close.
    pub fn withdraw(&self, req_id: &str, basis: impl Into<String>) -> bool {
        self.settle(
            req_id,
            SessionEvent::DecisionAnswered {
                req_id: req_id.to_string(),
                outcome: DecisionOutcome::Cancelled,
                by: Decider {
                    kind: "relay".into(),
                    identity: String::new(),
                },
                basis: basis.into(),
                late: false,
            },
        )
    }

    /// Post every open ask `hub` does not show yet, when `hub` is the main session. Called before a
    /// head attaches, so the snapshot it gets carries them. Returns how many were posted.
    pub fn deliver(self: &Arc<Self>, hub: &Arc<Hub>) -> usize {
        let is_main = self.main().is_some_and(|m| Arc::ptr_eq(&m, hub));
        if !is_main {
            return 0;
        }
        let shown = hub.relayed_ids();
        let missing: Vec<(String, SessionEvent, Arc<dyn AnswerSink>)> = self
            .lock()
            .open
            .iter()
            .filter(|(id, _)| !shown.contains(id))
            .map(|(id, e)| (id.clone(), e.card.clone(), e.sink.clone()))
            .collect();
        let mut posted = 0;
        for (id, card, sink) in missing {
            if hub.post_relayed(card, self.settling(&id, sink)) {
                posted += 1;
            }
        }
        posted
    }

    /// Name the sessions of the queue tree `tree` (the gatekeeper and its `-sub-` children):
    /// replaces what was known about that tree.
    pub fn name_askers(&self, tree: &str, askers: impl IntoIterator<Item = (String, Asker)>) {
        let sub = format!("{tree}-sub-");
        let mut g = self.lock();
        g.askers.retain(|id, _| id != tree && !id.starts_with(&sub));
        g.askers.extend(askers);
    }

    /// What `session` is known as, when it is a named queue session.
    pub fn asker(&self, session: &str) -> Option<Asker> {
        self.lock().askers.get(session).cloned()
    }

    /// The main session's annotator, or none.
    pub fn set_annotator(&self, a: Option<Arc<dyn Annotator>>) {
        self.lock().annotator = a;
    }

    pub fn annotator(&self) -> Option<Arc<dyn Annotator>> {
        self.lock().annotator.clone()
    }

    /// The main session's oracle for relayed asks: an adjudicator that may settle one before its
    /// card is posted, within the scope it has earned. `None` posts every card.
    #[cfg(feature = "tools")]
    pub fn set_oracle(&self, o: Option<Arc<dyn letibot_tools::Adjudicator>>) {
        self.lock().oracle = o;
    }

    #[cfg(feature = "tools")]
    pub fn oracle(&self) -> Option<Arc<dyn letibot_tools::Adjudicator>> {
        self.lock().oracle.clone()
    }

    /// Ask the annotator about a posted card on its own thread. The card is never held for it, and
    /// a line that comes back after the ask settled is dropped.
    pub fn annotate(self: &Arc<Self>, call: RelayedCall) -> bool {
        let Some(a) = self.annotator() else {
            return false;
        };
        let set = Arc::downgrade(self);
        std::thread::Builder::new()
            .name("relay-annotate".into())
            .spawn(move || {
                if let Some(line) = a.line(&call)
                    && let Some(set) = set.upgrade()
                {
                    set.amend(&call.req_id, &line);
                }
            })
            .is_ok()
    }

    /// Put `line` on an open card and show it again. `false` when the ask is no longer open.
    pub fn amend(self: &Arc<Self>, req_id: &str, line: &str) -> bool {
        let Some(main) = self.main() else {
            return false;
        };
        let (card, sink) = {
            let mut g = self.lock();
            let Some(e) = g.open.get_mut(req_id) else {
                return false;
            };
            if let SessionEvent::DecisionRequested { shadow, .. } = &mut e.card {
                *shadow = Some(line.to_string());
            }
            (e.card.clone(), e.sink.clone())
        };
        main.post_relayed(card, self.settling(req_id, sink));
        // Settled between the check and the post: the copy just shown comes off again.
        if !self.lock().open.contains_key(req_id) {
            main.withdraw_relayed(req_id, "settled while its line was added");
            return false;
        }
        true
    }

    /// Every open ask, typed.
    pub fn pending(&self) -> Vec<Pending> {
        self.lock()
            .open
            .values()
            .map(|e| e.pending.clone())
            .collect()
    }

    /// Every open ask's id.
    pub fn ids(&self) -> Vec<String> {
        self.lock().open.keys().cloned().collect()
    }

    fn settling(self: &Arc<Self>, req_id: &str, inner: Arc<dyn AnswerSink>) -> Arc<dyn AnswerSink> {
        Arc::new(Settling {
            set: Arc::downgrade(self),
            req_id: req_id.to_string(),
            inner,
        })
    }
}

/// The answer path a posted copy carries: the first answer leaves the set and goes to the asker.
struct Settling {
    set: Weak<PendingAsks>,
    req_id: String,
    inner: Arc<dyn AnswerSink>,
}

impl AnswerSink for Settling {
    fn answer(&self, req_id: &str, identity: &str, reply: &Reply) -> bool {
        if let Some(set) = self.set.upgrade() {
            set.lock().open.remove(&self.req_id);
        }
        self.inner.answer(req_id, identity, reply)
    }

    /// An interrupt of the main session's turn is not an answer to another session's ask.
    fn cancel(&self, _why: &str) {}

    fn describe(&self) -> String {
        self.inner.describe()
    }
}

#[cfg(test)]
mod tests;
