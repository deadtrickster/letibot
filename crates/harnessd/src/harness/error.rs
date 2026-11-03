//! **What an open or a turn refuses with**, and the one question a caller asks of it: whether the
//! refusal came from the provider rather than from this process.

use super::*;

#[derive(Debug)]
pub enum HarnessError {
    Setup(String),
    Turn(TurnFailure),
    Store(String),
    /// The model went round the tool loop `max_tool_rounds` times and was still producing results:
    /// the backstop, not a judgement about the work, and reported rather than silently truncated to
    /// whatever the last round happened to say.
    LoopBound {
        rounds: usize,
    },
    /// The turn stopped because the next round would not fit the window: not a failure of the work,
    /// since everything produced is committed and `Sessions` compacts before the next turn.
    /// `resident` and `window` are in `unit`, and `ledger` keeps the box's own pair for the parenthesis.
    ContextWall {
        rounds: usize,
        resident: u64,
        window: u64,
        unit: String,
        ledger: (u64, u64),
    },
    /// The progress check fired: `stall_rounds` consecutive rounds produced nothing this turn had not
    /// already seen. `evidence` names what the detector saw, because a stop that cites evidence is one
    /// an operator can contradict and a round count is not.
    NoProgress {
        evidence: String,
    },
    /// The endpoint failed, and this is the whole account of it: the class, how many times the call
    /// was tried and for how long, and the provider's own last words. `cause` stays typed because two
    /// questions are asked of a failure after it is said, and nothing about it is secret.
    Endpoint {
        said: String,
        cause: letibot_turn::HttpError,
    },
}

impl HarnessError {
    /// **The HTTP failure underneath this error**, for the two questions asked after it is
    /// said - never for re-deciding whether to retry, which the ladder already answered.
    pub fn http_cause(&self) -> Option<&letibot_turn::HttpError> {
        match self {
            HarnessError::Turn(TurnFailure::Http(e)) | HarnessError::Endpoint { cause: e, .. } => {
                Some(e)
            }
            _ => None,
        }
    }
}

impl std::fmt::Display for HarnessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HarnessError::Setup(s) => write!(f, "{s}"),
            HarnessError::Turn(e) => write!(f, "{e}"),
            // **The whole account of one endpoint failure**: what it was, how many calls it
            // took, and what the provider said last. See [`HarnessError::Endpoint`].
            HarnessError::Endpoint { said, .. } => write!(f, "{said}"),
            HarnessError::Store(s) => write!(f, "store: {s}"),
            // Never "the model called tools N times without answering": it WAS answering and had not
            // finished, and reaching here means every round still produced new results, which is a fact
            // about the size of the task and not about the model.
            HarnessError::ContextWall {
                rounds,
                resident,
                window,
                unit,
                ledger,
            } => write!(
                f,
                "stopped after {rounds} round(s) at the context wall: {resident} of \
                 {window} {unit} resident{} and the next round would not fit. Everything \
                 this turn produced is committed. A compaction was ATTEMPTED — whether \
                 it succeeded is its own line above, because a summary turn can refuse \
                 (it may not call tools) and saying it worked when it did not is the \
                 failure this whole check exists to avoid. If it compacted, the turn \
                 continues on the summary by itself, a bounded number of times; if the \
                 wall comes back after those, the task does not fit the window and \
                 `/compact` or a fresh session is the way past it.",
                if *ledger == (*resident, *window) {
                    String::new()
                } else {
                    format!(" ({} of {} ledger tokens)", ledger.0, ledger.1)
                }
            ),
            HarnessError::LoopBound { rounds } => write!(
                f,
                "stopped after {rounds} rounds — the round backstop, and it is not a \
                 judgement about the work: the turn was still producing results it had \
                 not seen before when the bound was reached. The progress check did not \
                 fire. Raise --max-tool-rounds if the task is this big."
            ),
            HarnessError::NoProgress { evidence } => write!(f, "{evidence}"),
        }
    }
}

impl std::error::Error for HarnessError {}

impl From<TurnFailure> for HarnessError {
    fn from(e: TurnFailure) -> Self {
        HarnessError::Turn(e)
    }
}

impl From<letibot_turn::EngineError> for HarnessError {
    fn from(e: letibot_turn::EngineError) -> Self {
        HarnessError::Turn(TurnFailure::Engine(e))
    }
}
