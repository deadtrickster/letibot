//! **The adjudicated gate's tests**, one file per theme, and the fixtures they share. Each theme
//! file reaches the fixtures, the gate and the imports below through `use super::*`.

use super::*;

use serde_json::json;

use std::collections::BTreeSet;

fn call<'a>(name: &'a str, args: &'a Value) -> GateCall<'a> {
    GateCall {
        name,
        access: Access::Write,
        args,
        turn_id: "t1",
        call_id: "c1",
        workspace: "/w",
        target_exists: Some(true),
        scripts: &[],
    }
}

// ---------------------------------------------------------------- supervised

/// An adjudicator that always picks the first option whose kind matches, and
/// records what it was shown.
struct Fixed {
    id: &'static str,
    allow: bool,
    seen: std::sync::Mutex<Option<Option<ModelAdvice>>>,
}

impl Fixed {
    fn new(id: &'static str, allow: bool) -> Self {
        Fixed {
            id,
            allow,
            seen: std::sync::Mutex::new(None),
        }
    }
}

impl Adjudicator for Fixed {
    fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
        *self.seen.lock().unwrap() = Some(req.advice.clone());
        let want = |o: &&DecisionOption| o.kind.is_allow() == self.allow;
        match req.options.iter().find(want) {
            Some(o) => AdjudicationDecision::selected(req, &o.id, self.id, "because"),
            None => AdjudicationDecision::unavailable(req, self.id, "no such option"),
        }
    }
    /// Stands in for a model adjudicator, so it reports like one. A double that
    /// answered without saying whether an oracle was consulted would exercise
    /// the `consulted: false` fallback in every test and never the real path.
    fn last_advice(&self) -> Option<ModelAdvice> {
        self.id.starts_with("model").then(|| ModelAdvice {
            consulted: true,
            would: if self.allow { "admit" } else { "ask" },
            by: self.id.to_string(),
            basis: "because".into(),
            cites: Vec::new(),
            latency_ms: 1,
            unsure: None,
        })
    }
    fn describe(&self) -> String {
        self.id.to_string()
    }

    /// **The fixture now carries both halves of an exchange** (R11), because the
    /// corpus columns have to come from the adjudicator that asked an oracle.
    fn last_brief(&self) -> Option<String> {
        Some(format!(
            "brief — {}\ntrail: 1 operator message(s) of 1 scanned",
            self.id
        ))
    }

    fn last_reply(&self) -> Option<String> {
        Some("It follows from what was asked.\nALLOW 0".to_string())
    }
}

// -----------------------------------------------------------------------
// The four outcome classes, at the gate.
// -----------------------------------------------------------------------

fn pinned() -> crate::intent::Surroundings {
    crate::intent::Surroundings {
        scratch: None,
        home: Some("/home/dead".into()),
        workspace: Some("/w".into()),
        shell: crate::intent::ShellTrust::Pinned {
            how: "test fixture".into(),
        },
        seen_hosts: BTreeSet::new(),
    }
}

fn bash<'a>(args: &'a Value) -> GateCall<'a> {
    GateCall {
        name: "bash",
        access: Access::Exec,
        args,
        turn_id: "t1",
        call_id: "c1",
        workspace: "/w",
        target_exists: None,
        scripts: &[],
    }
}

// -------------------------------------------------------------------------
// R11 — the audit rows, read back at decision time
// -------------------------------------------------------------------------

/// The same call with a turn id that carries a sequence, so ages are
/// computable. `bash` names its turn "t1", which parses to nothing and is the
/// honest `None` everywhere else.
fn bash_at<'a>(args: &'a Value, turn_id: &'a str) -> GateCall<'a> {
    GateCall {
        turn_id,
        ..bash(args)
    }
}

mod breaker;
mod gate;
mod grants;
mod history;
mod memory;
mod outcomes;
mod supervised;
