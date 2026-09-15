//! **Where the gate's decisions go so they outlive the daemon.**
//!
//! > *"we should assemble my decisions and model decisions into corpus across our
//! > letibot/leticode runs. and if gatekeeper is unsure it should say so and i still
//! > asked - goes to corpus too."*
//!
//! `AdjudicatedGate::log` is a `Vec` in one process. It was always the right shape —
//! the model's verdict and the operator's answer in separate fields, `shown` kept
//! verbatim — and it was always discarded. Everything this harness ever decided died
//! with the daemon that decided it.
//!
//! This is the write-through. One row per decision at the moment it is made; one
//! update when the operator rules, which is a different event and often a later turn.
//!
//! # Its own connection, on purpose
//!
//! [`letibot_tokencore::store::Store`] holds a `rusqlite::Connection`, which is `Send`
//! but not `Sync`, and a [`CorpusSink`] is shared across the gate's lifetime. So this
//! opens a second connection to the same file behind a `Mutex`. That is the store's
//! declared design — WAL, a 5 s busy timeout, and the comment in `from_connection`
//! naming two connections as expected — not a workaround.
//!
//! # A lost example is never a refused call
//!
//! Every failure here is swallowed into a counter. The gate has already decided by
//! the time this runs; turning a full disk into a denial would make the corpus a
//! thing that can *change* what the harness does, and a guard whose behaviour depends
//! on the health of its own telemetry is worse than one with no telemetry.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use letibot_tokencore::store::Store;
use letibot_tools::{CorpusRow, CorpusSink, OperatorOverride};

pub struct StoreCorpus {
    store: Mutex<Store>,
    /// Decisions this sink could not write. Read by the disclosure: a corpus that
    /// is quietly dropping rows must not read as a corpus that is being kept.
    lost: AtomicU64,
    /// Rulings that found no row. Counted separately because it means something
    /// different — the decision was never written, or it belongs to a store this
    /// daemon is not the one holding.
    orphan_rulings: AtomicU64,
}

impl StoreCorpus {
    /// Open a second connection to the session store.
    pub fn open(path: &std::path::Path) -> Result<Self, letibot_tokencore::store::StoreError> {
        Ok(StoreCorpus {
            store: Mutex::new(Store::open(path)?),
            lost: AtomicU64::new(0),
            orphan_rulings: AtomicU64::new(0),
        })
    }

    /// What the corpus holds, and how much of it did not arrive.
    ///
    /// Reported together because a count shown without its losses is the denominator
    /// defect in the one place it matters most: a corpus quietly dropping rows must
    /// not read as one that is being kept.
    pub fn counts(&self) -> (letibot_tokencore::store::CorpusCounts, u64, u64) {
        let c = self
            .store
            .lock()
            .ok()
            .and_then(|s| s.corpus_counts().ok())
            .unwrap_or_default();
        (
            c,
            self.lost.load(Ordering::Relaxed),
            self.orphan_rulings.load(Ordering::Relaxed),
        )
    }
}

impl CorpusSink for StoreCorpus {
    fn decided(&self, row: &CorpusRow) {
        // The trail is stored as JSON rather than as `AuthorisationTrail::render`'s
        // prose: the prose is what the *model* was shown and is already in `shown`,
        // and a corpus reader that wants to re-render it under a changed brief
        // format needs the structure. A trail that will not serialise is recorded
        // as the reason it would not, never as an empty trail — `{}` and "nobody
        // looked" are the two facts this whole type exists to keep apart.
        let trail_json = serde_json::to_string(&row.trail)
            .unwrap_or_else(|e| format!(r#"{{"unserialisable":{}}}"#, json_string(&e.to_string())));
        // Same rule for the arguments: a value that will not serialise is recorded
        // as that, never as `{}`. An empty argument object is a real call shape.
        let arguments_json = serde_json::to_string(&row.arguments)
            .unwrap_or_else(|e| format!(r#"{{"unserialisable":{}}}"#, json_string(&e.to_string())));
        let options_json = serde_json::to_string(&row.options).unwrap_or_else(|_| "[]".into());

        let rec = letibot_tokencore::store::NewAdjudication {
            request_id: row.request_id.clone(),
            session_id: row.session_id.clone(),
            turn_id: row.turn_id.clone(),
            action: row.action.clone(),
            baseline: row.baseline.clone(),
            tier: row.tier.to_string(),
            trail_json,
            shown: row.shown.clone(),
            tool: row.tool.clone(),
            arguments_json,
            mode: row.mode.clone(),
            options_json,
            agent: row.agent.clone(),
            model_verdict: row.model_verdict.clone(),
            verdict: row.verdict.clone(),
            verdict_by: row.verdict_by.clone(),
            verdict_basis: row.verdict_basis.clone(),
            p_allow: row.p_allow,
            oracle_ms: Some(row.decision_ms as i64),
            // The oracle's own id is inside `verdict_by` (`model:<id>`) and is not
            // separately reported today. Left NULL rather than parsed out of that
            // string: a column filled by splitting prose is the column that breaks
            // when the prose changes.
            oracle_model: None,
            // Carried from the row rather than re-derived: the key a decision is
            // recorded under and the key a later call is looked up by must be one
            // derivation.
            shape: row.shape.clone(),
            brief_sha: Some(row.brief_format.to_string()),
            effect: row.effect.to_string(),
            asked: row.asked,
        };

        let ok = self
            .store
            .lock()
            .ok()
            .map(|s| s.record_adjudication(&rec).is_ok())
            .unwrap_or(false);
        if !ok {
            self.lost.fetch_add(1, Ordering::Relaxed);
            return;
        }
        // **At `supervised` the ruling arrives WITH the decision**, because the
        // person answered this call with the model's verdict in front of them. Two
        // statements rather than an eleven-argument insert with two more optional
        // columns: `record_operator_ruling` is the one place the label is written,
        // and a second path into those columns is a second place for them to drift.
        if let Some(what) = &row.operator {
            self.ruled(&row.request_id, what);
        }
    }

    fn ruled(&self, request_id: &str, what: &OperatorOverride) {
        let note = match what {
            OperatorOverride::Granted { note }
            | OperatorOverride::Upheld { note }
            | OperatorOverride::Revoked { note } => note.as_str(),
        };
        let found = self
            .store
            .lock()
            .ok()
            .and_then(|s| s.record_operator_ruling(request_id, what.as_str(), note).ok())
            .unwrap_or(false);
        if !found {
            self.orphan_rulings.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// A JSON string literal, for the one place above that builds JSON by hand.
fn json_string(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}
