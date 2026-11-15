//! The review queue's sessions ask the operator on boards nobody watches. Each open ask is a
//! question card on the daemon's main session naming the asker, and its answer goes to the asker's
//! row. The boards are the durable half, so a restart posts the cards again.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::event::{Decider, DecisionOutcome, OnTimeout, SubagentAsk, todo_title};
use letibot_sessionlog::hub::{AnswerSink, CommandKind, Hub, Reply};
use letibot_sessionlog::question::QuestionAnswer;
use letibot_sessionlog::registry::Registry;
use letibot_tokencore::store::{Store, operator_asks};
use letibot_tools::builtins::todo::TodoBoard;

use crate::config::StoreAt;

/// How long an answered card is not posted again, while the asker's row catches up.
const ANSWERED_GRACE: Duration = Duration::from_secs(90);

const PREFIX: &str = "relay-ask:";

/// One open question on a queue session's board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ask {
    pub asker: String,
    pub row: String,
    pub question: String,
    pub options: Vec<String>,
}

impl Ask {
    /// Stable across passes and restarts, so one ask is one card.
    pub fn req_id(&self) -> String {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in self
            .asker
            .bytes()
            .chain([0])
            .chain(self.row.bytes())
            .chain([0])
            .chain(self.question.bytes())
        {
            h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
        format!("{PREFIX}{}:{h:016x}", self.asker)
    }
}

/// Whether `id` is the queue session `gatekeeper` or one of the sessions under it.
pub fn in_queue_tree(gatekeeper: &str, id: &str) -> bool {
    id == gatekeeper || id.starts_with(&format!("{gatekeeper}-sub-"))
}

/// Every open operator ask on a board of `gatekeeper`'s tree, read from the store.
pub fn open_asks(store: &Store, gatekeeper: &str) -> Vec<Ask> {
    let waiting = store.sessions_waiting_on_operator().unwrap_or_default();
    let mut out = Vec::new();
    for asker in waiting.keys().filter(|id| in_queue_tree(gatekeeper, id)) {
        let todos = store.todos(asker).unwrap_or_default();
        for (row, question, options) in operator_asks(&todos) {
            out.push(Ask {
                asker: asker.clone(),
                row: row.content.trim().to_string(),
                question: question.to_string(),
                options: options.to_vec(),
            });
        }
    }
    out
}

/// A board's open operator asks, typed, under the id a relayed card of the same ask carries.
pub fn board_pending(
    asker: &str,
    todos: &[letibot_tokencore::store::TodoItem],
) -> Vec<letibot_transcript::pending::Pending> {
    use letibot_transcript::pending::{Pending, PendingKind};
    operator_asks(todos)
        .into_iter()
        .map(|(row, question, options)| {
            let ask = Ask {
                asker: asker.to_string(),
                row: row.content.trim().to_string(),
                question: question.to_string(),
                options: options.to_vec(),
            };
            Pending::new(PendingKind::Question, asker, ask.req_id())
        })
        .collect()
}

/// The card a head draws: a question, its choices the row's options, naming who asks.
pub fn card(ask: &Ask, gatekeeper: &str, root: &str) -> SessionEvent {
    let who = match ask.asker == gatekeeper {
        true => "the review queue's session",
        false => "a reviewer",
    };
    SessionEvent::DecisionRequested {
        req_id: ask.req_id(),
        subagent: Some(SubagentAsk {
            handle: ask.asker.clone(),
            task: todo_title(&ask.row).chars().take(100).collect(),
            root: root.to_string(),
        }),
        kind: "question".into(),
        call_id: None,
        access: String::new(),
        summary: ask.question.clone(),
        target: String::new(),
        write_targets: Vec::new(),
        detail: format!("its board row: {}", ask.row),
        options: Vec::new(),
        options_note: String::new(),
        choices: ask.options.clone(),
        because: format!(
            "{who} `{}` asks you on its board; nobody watches that session, so the ask is here and \
             the answer goes to its row",
            ask.asker
        ),
        advice: None,
        deadline: None,
        on_timeout: OnTimeout::Ask,
    }
}

/// Cards answered recently, by `req_id`, so a pass does not post one again before the row moves.
pub type Answered = Arc<Mutex<HashMap<String, Instant>>>;

/// Bring `root`'s relayed cards in line with `asks`: post the new ones, withdraw the settled ones.
/// Returns how many were posted.
pub fn relay(
    root: &Arc<Hub>,
    registry: &Arc<Registry>,
    at: Option<StoreAt>,
    gatekeeper: &str,
    asks: &[Ask],
    answered: &Answered,
) -> usize {
    let open = root.relayed_ids();
    let want: HashMap<String, &Ask> = asks.iter().map(|a| (a.req_id(), a)).collect();
    for id in open.iter().filter(|id| !want.contains_key(*id)) {
        root.withdraw_relayed(id, "the question is no longer open on the asker's board");
    }
    let now = Instant::now();
    let mut recent = answered.lock().unwrap_or_else(|e| e.into_inner());
    recent.retain(|_, at| now.duration_since(*at) < ANSWERED_GRACE);
    let mut posted = 0;
    for (id, ask) in &want {
        if open.contains(id) || recent.contains_key(id) {
            continue;
        }
        let sink = Arc::new(Relay {
            ask: (*ask).clone(),
            registry: registry.clone(),
            root: Arc::downgrade(root),
            at: at.clone(),
            answered: answered.clone(),
        });
        if root.post_relayed(card(ask, gatekeeper, &root.session_id()), sink) {
            root.warn(
                "waiting_on_operator",
                format!(
                    "`{}` asks: {} (on its row `{}`) - the card is on this session",
                    ask.asker,
                    ask.question,
                    todo_title(&ask.row)
                ),
            );
            posted += 1;
        }
    }
    posted
}

/// Where one relayed card's answer goes: the asker's row.
struct Relay {
    ask: Ask,
    registry: Arc<Registry>,
    root: Weak<Hub>,
    at: Option<StoreAt>,
    answered: Answered,
}

impl Relay {
    /// A session that is not open has no reader for the command, so its stored board is settled
    /// and it reads the answer when it next opens.
    fn settle_stored(&self, answer: &QuestionAnswer) -> Result<String, String> {
        let at = self.at.as_ref().ok_or("no store")?;
        let store = at.open().map_err(|e| e.to_string())?;
        let board = TodoBoard::new(store.todos(&self.ask.asker).map_err(|e| e.to_string())?);
        crate::operator_ask::settle(&board, &self.ask.row, &self.ask.question, answer)?;
        store
            .put_todos(&self.ask.asker, &board.snapshot())
            .map_err(|e| e.to_string())?;
        Ok(format!("written on `{}`'s board", self.ask.asker))
    }
}

impl AnswerSink for Relay {
    fn answer(&self, req_id: &str, identity: &str, reply: &Reply) -> bool {
        let Reply::Question(answer) = reply else {
            return false;
        };
        let said = match self.registry.get(&self.ask.asker) {
            Some(hub)
                if hub.submit_daemon(
                    identity,
                    req_id,
                    CommandKind::AnswerTodo {
                        content: self.ask.row.clone(),
                        question: self.ask.question.clone(),
                        answer: answer.clone(),
                    },
                ) =>
            {
                Ok(format!("sent to `{}`", self.ask.asker))
            }
            _ => self.settle_stored(answer),
        };
        self.answered
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(req_id.to_string(), Instant::now());
        let basis = match &said {
            Ok(s) => s.clone(),
            Err(why) => format!("the answer was not taken: {why}"),
        };
        if let Some(root) = self.root.upgrade() {
            root.publish(SessionEvent::DecisionAnswered {
                req_id: req_id.to_string(),
                outcome: DecisionOutcome::Cancelled,
                by: Decider {
                    kind: "human".into(),
                    identity: identity.to_string(),
                },
                basis,
                late: false,
            });
        }
        said.is_ok()
    }

    /// An interrupt of the main session's turn is not an answer to another session's row.
    fn cancel(&self, _why: &str) {}

    fn describe(&self) -> String {
        format!("the operator, for `{}`'s board", self.ask.asker)
    }
}

#[cfg(test)]
mod tests;
