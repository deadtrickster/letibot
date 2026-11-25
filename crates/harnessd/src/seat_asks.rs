//! A Claude Code seat's operator question on a board nobody watches. The head's own board read
//! (`ServerFrame::Todos`) carries a seat's rows, but a question with no card is a question a
//! person hears about only when they open the pane; so each open one is relayed onto this
//! daemon's main session, and the answer settles the seat's row where it was asked.
//!
//! The shape is `crate::queue_asks`'s, because it is the same problem one door over: a board
//! whose owner has no head of its own.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::event::{Decider, DecisionOutcome, OnTimeout, SubagentAsk, todo_title};
use letibot_sessionlog::hub::{AnswerSink, Hub, Reply};
use letibot_sessionlog::question::QuestionAnswer;
use letibot_sessionlog::registry::Registry;
use letibot_tokencore::store::{Store, operator_asks};
use letibot_tools::builtins::todo::TodoBoard;

use crate::config::StoreAt;

/// How long an answered card is not posted again, while the seat's stored board catches up.
const ANSWERED_GRACE: Duration = Duration::from_secs(90);

/// How often a daemon looks while a seat is waiting on the person - the relay's own clock, since
/// nothing else in the pass is armed by a board that lives in another process.
pub const TICK: Duration = Duration::from_secs(15);

const PREFIX: &str = "seat-ask:";

/// One open question on a seat's board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ask {
    /// The seat's id, which is what its board is keyed by (`cc@proj`, `lab2x1/cc-…`).
    pub seat: String,
    pub row: String,
    pub question: String,
    pub options: Vec<String>,
}

impl Ask {
    /// Stable across passes and restarts, so one question is one card.
    pub fn req_id(&self) -> String {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in self
            .seat
            .bytes()
            .chain([0])
            .chain(self.row.bytes())
            .chain([0])
            .chain(self.question.bytes())
        {
            h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
        format!("{PREFIX}{}:{h:016x}", self.seat)
    }
}

/// Every open operator ask on every seat's board the store holds. A seat with no board, and a
/// seat whose board cannot be read, contribute nothing: an absence is not an ending.
pub fn open_asks(store: &Store) -> Vec<Ask> {
    let mut out = Vec::new();
    for (seat, todos) in store.seat_boards().unwrap_or_default() {
        for (row, question, options) in operator_asks(&todos) {
            out.push(Ask {
                seat: seat.clone(),
                row: row.content.trim().to_string(),
                question: question.to_string(),
                options: options.to_vec(),
            });
        }
    }
    out
}

/// The card a head draws: a question, its choices the row's options, naming the seat that asked.
pub fn card(ask: &Ask, root: &str) -> SessionEvent {
    SessionEvent::DecisionRequested {
        req_id: ask.req_id(),
        subagent: Some(SubagentAsk {
            handle: ask.seat.clone(),
            task: todo_title(&ask.row).chars().take(100).collect(),
            root: root.to_string(),
            ..Default::default()
        }),
        kind: "question".into(),
        call_id: None,
        access: String::new(),
        summary: ask.question.clone(),
        target: String::new(),
        write_targets: Vec::new(),
        edits: Vec::new(),
        detail: format!("its board row: {}", ask.row),
        options: Vec::new(),
        options_note: String::new(),
        choices: ask.options.clone(),
        because: format!(
            "the Claude Code seat `{}` asks you on its board; a seat has no head of its own, so \
             the ask is here and the answer goes to its row",
            ask.seat
        ),
        advice: None,
        shadow: None,
        deadline: None,
        on_timeout: OnTimeout::Ask,
    }
}

/// Cards answered recently, by `req_id`, so a pass does not post one again before the row moves.
pub type Answered = Arc<Mutex<HashMap<String, Instant>>>;

/// Bring the daemon's pending set in line with `asks`, shown on `root`: post the new ones and
/// withdraw the settled ones. Returns how many were posted.
pub fn relay(
    root: &Arc<Hub>,
    registry: &Arc<Registry>,
    at: Option<StoreAt>,
    asks: &[Ask],
    answered: &Answered,
) -> usize {
    let set = registry.pending_asks();
    set.set_main(root);
    let open: Vec<String> = set
        .ids()
        .into_iter()
        .filter(|id| id.starts_with(PREFIX))
        .collect();
    let want: HashMap<String, &Ask> = asks.iter().map(|a| (a.req_id(), a)).collect();
    for id in open.iter().filter(|id| !want.contains_key(*id)) {
        set.withdraw(id, "the seat's row no longer asks you anything");
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
            root: Arc::downgrade(root),
            at: at.clone(),
            answered: answered.clone(),
        });
        if set.post(card(ask, &root.session_id()), sink) {
            // Said on the main session's log as well as drawn: a head that attaches later reads
            // the card, and the person reading now sees the line arrive.
            root.warn(
                "waiting_on_operator",
                format!(
                    "`{}` asks: {} (on its row `{}`) - the card is on this session",
                    ask.seat,
                    ask.question,
                    todo_title(&ask.row)
                ),
            );
            posted += 1;
        }
    }
    posted
}

/// Where one relayed card's answer goes: the asking seat's row, in the store the bridge reads.
/// The seat itself is another process, so the row is settled here and the bridge adopts it on its
/// next pass - which is what wakes the session.
struct Relay {
    ask: Ask,
    root: Weak<Hub>,
    at: Option<StoreAt>,
    answered: Answered,
}

impl Relay {
    fn settle_stored(&self, answer: &QuestionAnswer) -> Result<String, String> {
        let at = self.at.as_ref().ok_or("no store")?;
        let store = at.open().map_err(|e| e.to_string())?;
        let board = TodoBoard::new(
            store
                .seat_todos(&self.ask.seat)
                .map_err(|e| e.to_string())?,
        );
        crate::operator_ask::settle(&board, &self.ask.row, &self.ask.question, answer)?;
        store
            .put_seat_todos(&self.ask.seat, &board.snapshot())
            .map_err(|e| e.to_string())?;
        Ok(format!("written on `{}`'s board", self.ask.seat))
    }
}

impl AnswerSink for Relay {
    fn answer(&self, req_id: &str, identity: &str, reply: &Reply) -> bool {
        let Reply::Question(answer) = reply else {
            return false;
        };
        let said = self.settle_stored(answer);
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

    /// An interrupt of the main session's turn is not an answer to a seat's row.
    fn cancel(&self, _why: &str) {}

    fn describe(&self) -> String {
        format!("the operator, for `{}`'s board", self.ask.seat)
    }
}

#[cfg(test)]
mod tests;
