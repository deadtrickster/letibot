//! **The memory card's session grant** (R65): the operator, getting the asks one after another —
//! *"i started to get memory asks, session_allow option is missing"*. A session grant covers a later
//! ask no larger than what was granted and no larger than what is free; anything else asks again.

use super::*;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::exec::memory::{MemoryAsk, MemoryGrant};
use crate::runtime::Gate;

/// Answers every card with one option and counts how many cards it was shown.
struct Answers {
    option: &'static str,
    asked: Arc<AtomicUsize>,
}

impl Adjudicator for Answers {
    fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
        self.asked.fetch_add(1, Ordering::SeqCst);
        AdjudicationDecision::selected(req, self.option, "operator", "test")
    }

    fn describe(&self) -> String {
        "the test's operator".into()
    }
}

const GIB: u64 = 1 << 30;

fn ask(bytes: Option<u64>, available: u64) -> MemoryAsk {
    MemoryAsk {
        command: "cargo test --workspace".into(),
        bytes,
        ceiling: 8 * GIB,
        available,
        total: 128 * GIB,
    }
}

fn gate(option: &'static str) -> (AdjudicatedGate, Arc<AtomicUsize>) {
    let asked = Arc::new(AtomicUsize::new(0));
    let g = AdjudicatedGate::new(Box::new(Answers {
        option,
        asked: asked.clone(),
    }));
    (g, asked)
}

#[test]
fn the_card_offers_the_session_and_says_it_is_bounded() {
    let opts = memory_grant_options();
    let session = opts
        .iter()
        .find(|o| o.id == "grant_memory_session")
        .expect("the session option is on the card");
    assert_eq!(session.kind, OptionKind::AllowSession);
    assert!(session.label.contains("up to this"), "{}", session.label);
    assert!(opts.iter().any(|o| o.id == "grant_memory"));
    assert!(opts.iter().any(|o| o.id == "deny"));
}

#[test]
fn a_session_grant_covers_a_later_ask_no_larger_and_no_larger_than_what_is_free() {
    let (mut g, asked) = gate("grant_memory_session");
    assert!(matches!(
        g.grant_memory(&ask(Some(16 * GIB), 60 * GIB)),
        MemoryGrant::Granted
    ));
    assert_eq!(asked.load(Ordering::SeqCst), 1, "the first ask is a card");

    // Smaller, and free enough: granted without a card.
    assert!(matches!(
        g.grant_memory(&ask(Some(12 * GIB), 60 * GIB)),
        MemoryGrant::Granted
    ));
    assert_eq!(
        asked.load(Ordering::SeqCst),
        1,
        "covered by the session grant"
    );

    // Larger than granted: asks again.
    g.grant_memory(&ask(Some(20 * GIB), 60 * GIB));
    assert_eq!(
        asked.load(Ordering::SeqCst),
        2,
        "above the grant asks again"
    );

    // Within the (now widened, 20 GiB) grant but more than is free right now: asks again.
    g.grant_memory(&ask(Some(16 * GIB), 10 * GIB));
    assert_eq!(
        asked.load(Ordering::SeqCst),
        3,
        "more than is free asks again"
    );

    // Uncapped is never covered by a sized grant.
    g.grant_memory(&ask(None, 60 * GIB));
    assert_eq!(asked.load(Ordering::SeqCst), 4, "uncapped asks");
}

#[test]
fn a_once_grant_covers_nothing_after_it_and_a_mode_change_drops_the_session_grant() {
    let (mut g, asked) = gate("grant_memory");
    g.grant_memory(&ask(Some(16 * GIB), 60 * GIB));
    g.grant_memory(&ask(Some(1 * GIB), 60 * GIB));
    assert_eq!(asked.load(Ordering::SeqCst), 2, "once is once");

    let (mut g, asked) = gate("grant_memory_session");
    g.grant_memory(&ask(Some(16 * GIB), 60 * GIB));
    g.set_mode(crate::mode::Mode::ALWAYS_ASK).expect("a mode");
    g.grant_memory(&ask(Some(1 * GIB), 60 * GIB));
    assert_eq!(
        asked.load(Ordering::SeqCst),
        2,
        "a mode change clears session grants, this one with them"
    );
}
