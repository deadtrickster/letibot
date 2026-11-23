//! An action the operator DENIED is refused for the session's tree until it is lifted: the shape
//! and the target they refused, read back from the ledger, refused with no card and without telling
//! them again - and it never narrows a point that admits everything.

use super::*;
use crate::grant_ledger::GrantLedger as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

/// The person, refusing and counting what they were asked.
struct Person {
    asks: Arc<AtomicU32>,
    by: &'static str,
}

impl Person {
    fn counter() -> Arc<AtomicU32> {
        Arc::new(AtomicU32::new(0))
    }
}

impl Adjudicator for Person {
    fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
        self.asks.fetch_add(1, Ordering::SeqCst);
        match req.options.iter().find(|o| !o.kind.admits()) {
            Some(o) => AdjudicationDecision::selected(req, &o.id, self.by, "no"),
            None => AdjudicationDecision::unavailable(req, self.by, "no refusing rung was offered"),
        }
    }
    fn describe(&self) -> String {
        "the person, in a fixture".into()
    }
}

/// A ledger in memory: the one row a denial is, and the generation a revoke moves.
#[derive(Default)]
struct Denied {
    rows: Mutex<Vec<crate::grant_ledger::Stored>>,
}

impl crate::grant_ledger::GrantLedger for Denied {
    fn keep(
        &self,
        scope: &crate::grant_ledger::Scope,
        kept: &crate::grant_ledger::Kept,
        why: &str,
        by: &str,
    ) -> Result<(), String> {
        let mut rows = self.rows.lock().unwrap();
        // The ledger's own rule: one row per key, so a repeated refusal is not a longer list.
        if let Some(i) = rows
            .iter()
            .position(|r| r.scope == *scope && r.kept.key() == kept.key())
        {
            rows[i].why = why.into();
            rows[i].by = by.into();
            return Ok(());
        }
        let id = rows.len() as i64 + 1;
        rows.push(crate::grant_ledger::Stored {
            id,
            scope: scope.clone(),
            kept: kept.clone(),
            why: why.into(),
            by: by.into(),
        });
        Ok(())
    }

    fn load(
        &self,
        scope: &crate::grant_ledger::Scope,
    ) -> Result<Vec<crate::grant_ledger::Stored>, String> {
        Ok(self
            .rows
            .lock()
            .unwrap()
            .iter()
            .filter(|r| &r.scope == scope)
            .cloned()
            .collect())
    }

    fn clear(&self, scope: &crate::grant_ledger::Scope, _kind: &str) -> Result<usize, String> {
        let mut rows = self.rows.lock().unwrap();
        let before = rows.len();
        rows.retain(|r| r.scope != *scope);
        Ok(before - rows.len())
    }

    fn generation(&self) -> Result<String, String> {
        Ok(self.rows.lock().unwrap().len().to_string())
    }
}

/// A session at `always-ask` with the person behind its cards and a ledger beside it. The counter
/// is the fixture's: how many cards were raised, which is the fact every test here is about.
fn gate_at(
    mode: &str,
    who: &'static str,
    ledger: Arc<Denied>,
) -> (AdjudicatedGate, Arc<AtomicU32>) {
    let asks = Person::counter();
    let person = Person {
        asks: asks.clone(),
        by: who,
    };
    let g = AdjudicatedGate::new(Box::new(person))
        .with_identity("s-main", "agent")
        .with_mode(crate::mode::Mode::parse(mode).unwrap())
        .with_surroundings(crate::intent::Surroundings {
            home: Some("/home/op".into()),
            workspace: Some("/w".into()),
            shell: crate::intent::ShellTrust::Pinned { how: "test".into() },
            ..Default::default()
        })
        .with_tree_root("s-tree-root")
        .with_grant_ledger(ledger, None);
    (g, asks)
}

/// The two commands the tests refuse: one action, two targets.
fn a() -> Value {
    json!({ "command": "touch /w/a.txt" })
}

fn b() -> Value {
    json!({ "command": "touch /w/b.txt" })
}

/// A person's No refuses the action again without a card, and the row is one row: they are told
/// once when they say it, not once per retry.
#[test]
fn a_denial_refuses_the_same_action_again_with_no_card() {
    let ledger = Arc::new(Denied::default());
    let (mut g, asks) = gate_at("always-ask", "human:op", ledger.clone());
    assert!(matches!(
        g.admit(&call("bash", &a())),
        GateDecision::Refuse { .. }
    ));
    assert_eq!(asks.load(Ordering::SeqCst), 1);
    {
        let rows = ledger.rows.lock().unwrap();
        assert_eq!(rows.len(), 1, "the denial is kept: {rows:?}");
        assert!(
            matches!(rows[0].kept, crate::grant_ledger::Kept::Deny { .. }),
            "{:?}",
            rows[0].kept
        );
        assert_eq!(rows[0].by, "human:op", "the row names who refused");
        assert_eq!(
            rows[0].scope,
            crate::grant_ledger::Scope::Session("s-tree-root".into()),
            "kept at the session's TREE, so a child is refused too"
        );
    }

    match g.admit(&call("bash", &a())) {
        GateDecision::Refuse { outcome, tell } => {
            assert!(matches!(outcome, ToolOutcome::Denied { .. }), "{outcome:?}");
            assert!(tell.contains("FORBIDDEN"), "{tell}");
            assert!(tell.contains("human:op"), "told by whom: {tell}");
        }
        other => panic!("the second call asked again: {other:?}"),
    }
    assert_eq!(
        asks.load(Ordering::SeqCst),
        1,
        "nobody was asked the same question twice"
    );
    assert_eq!(ledger.rows.lock().unwrap().len(), 1);
}

/// The key is the PAIR: an action shaped like the refused one that names something else is a new
/// question, and the person is asked it.
#[test]
fn a_different_target_of_the_refused_shape_is_a_new_question() {
    let ledger = Arc::new(Denied::default());
    let (mut g, asks) = gate_at("always-ask", "human:op", ledger.clone());
    assert!(matches!(
        g.admit(&call("bash", &a())),
        GateDecision::Refuse { .. }
    ));
    assert!(matches!(
        g.admit(&call("bash", &b())),
        GateDecision::Refuse { .. }
    ));
    assert_eq!(
        asks.load(Ordering::SeqCst),
        2,
        "`touch /w/b.txt` is not the action they refused"
    );
    assert_eq!(ledger.rows.lock().unwrap().len(), 2);
}

/// Lifting it - the row leaves the ledger, as `/permissions revoke` does - reaches the next call.
#[test]
fn a_lifted_denial_asks_again() {
    let ledger = Arc::new(Denied::default());
    let (mut g, asks) = gate_at("always-ask", "human:op", ledger.clone());
    g.admit(&call("bash", &a()));
    assert_eq!(asks.load(Ordering::SeqCst), 1);
    g.admit(&call("bash", &a()));
    assert_eq!(asks.load(Ordering::SeqCst), 1, "refused from the row");

    ledger
        .clear(
            &crate::grant_ledger::Scope::Session("s-tree-root".into()),
            "deny",
        )
        .unwrap();
    crate::grant_ledger::bump();
    g.admit(&call("bash", &a()));
    assert_eq!(
        asks.load(Ordering::SeqCst),
        2,
        "the question is open again after a lift"
    );
}

/// A refusal never narrows a point that admits everything: the operator's allow-all is literal.
#[test]
fn an_allow_all_point_is_not_narrowed_by_a_refusal() {
    let ledger = Arc::new(Denied::default());
    // The person refuses `touch /w/a.txt` at a point that asks.
    let (mut asking, _) = gate_at("always-ask", "human:op", ledger.clone());
    asking.admit(&call("bash", &a()));
    // The same tree at allow-all: the record does not narrow it, and nobody is asked.
    let (mut all, never) = gate_at("allow-all", "human:op", ledger);
    assert!(
        matches!(all.admit(&call("bash", &a())), GateDecision::Admit),
        "allow-all admits it whatever the record says"
    );
    assert_eq!(never.load(Ordering::SeqCst), 0, "nobody was asked");
}

/// A reviewer's refusal is a decision about THIS call: it is not kept, and the person still decides
/// the next one.
#[test]
fn a_reviewers_refusal_is_not_sticky() {
    let ledger = Arc::new(Denied::default());
    let (mut g, asks) = gate_at("always-ask", "shadow:deepseek", ledger.clone());
    assert!(matches!(
        g.admit(&call("bash", &a())),
        GateDecision::Refuse { .. }
    ));
    assert!(
        ledger.rows.lock().unwrap().is_empty(),
        "the reviewer's No is not the operator's"
    );
    g.admit(&call("bash", &a()));
    assert_eq!(asks.load(Ordering::SeqCst), 2, "the person is still asked");
}
