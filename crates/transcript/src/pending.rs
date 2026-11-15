//! One thing a session waits on a person for, typed as dsh's `pendingInput` is: `approval`,
//! `question` or `other`, with the session that asked and the ask's id, so a reader marks it and
//! routes it without parsing words.

use serde::{Deserialize, Serialize};

/// The kind of input owed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingKind {
    /// A permission card: a tool call held at the gate.
    Approval,
    /// A question to the operator: `ask_user_question`, a board row's operator need, a room ask.
    Question,
    /// Anything else a person or a peer must move: a prompt card, a declared queue wait.
    Other,
}

impl PendingKind {
    /// A card's kind as the daemon spells it (`permission`, `question`, `prompt`).
    pub fn of_card(kind: &str) -> PendingKind {
        match kind {
            "permission" => PendingKind::Approval,
            "question" => PendingKind::Question,
            _ => PendingKind::Other,
        }
    }

    /// The wire word.
    pub fn word(self) -> &'static str {
        match self {
            PendingKind::Approval => "approval",
            PendingKind::Question => "question",
            PendingKind::Other => "other",
        }
    }
}

/// One ask a session is blocked on.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Pending {
    pub kind: PendingKind,
    /// The session that asked. A subagent's card is posted on its root, and this names the child.
    pub session: String,
    /// The ask's own id: a card's request id, a board row's id, a room ask's seq.
    pub ask: String,
}

impl Pending {
    pub fn new(kind: PendingKind, session: impl Into<String>, ask: impl Into<String>) -> Pending {
        Pending {
            kind,
            session: session.into(),
            ask: ask.into(),
        }
    }
}

/// The gutter mark for a set of pending asks: `!` when any is an approval, `?` for the rest, and
/// `None` when nothing is pending. An approval leads because it holds a running call.
pub fn mark(pending: &[Pending]) -> Option<char> {
    if pending.iter().any(|p| p.kind == PendingKind::Approval) {
        Some('!')
    } else if pending.is_empty() {
        None
    } else {
        Some('?')
    }
}

/// The distinct kinds, in a fixed order: what `leticode_list` and a roster line name.
pub fn kinds(pending: &[Pending]) -> Vec<PendingKind> {
    let mut k: Vec<PendingKind> = pending.iter().map(|p| p.kind).collect();
    k.sort();
    k.dedup();
    k
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_approval_outranks_a_question_in_the_mark() {
        let q = Pending::new(PendingKind::Question, "s-1", "row-1");
        let a = Pending::new(PendingKind::Approval, "s-1-sub-2", "adj-1");
        assert_eq!(mark(&[]), None);
        assert_eq!(mark(std::slice::from_ref(&q)), Some('?'));
        assert_eq!(mark(&[q.clone(), a.clone()]), Some('!'));
        assert_eq!(
            kinds(&[q, a.clone(), a]),
            vec![PendingKind::Approval, PendingKind::Question]
        );
    }

    #[test]
    fn the_wire_spells_the_kind_and_both_ids() {
        let p = Pending::new(PendingKind::of_card("permission"), "s-1", "adj-1");
        assert_eq!(
            serde_json::to_value(&p).unwrap(),
            serde_json::json!({"kind": "approval", "session": "s-1", "ask": "adj-1"})
        );
        assert_eq!(PendingKind::of_card("question"), PendingKind::Question);
        assert_eq!(PendingKind::of_card("prompt"), PendingKind::Other);
    }
}
