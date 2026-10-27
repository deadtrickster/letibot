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

/// **A boundary that encloses, for the fixtures whose subject is something a boundary then
/// admits.** Every namespace entered, both seals tested and held, egress denied — what
/// `Bwrap::probe` refuses to return anything less than, plus the seals being *held* rather than
/// merely not-open. See `boundary.rs` for what each clause is about.
fn measured_boundary() -> crate::exec::Boundary {
    use crate::exec::{Egress, Namespace, NsState, Seal, SealKind, ViewSpec};
    let mut ns = std::collections::BTreeMap::new();
    for n in Namespace::ALL {
        ns.insert(
            n,
            NsState::Entered {
                inode: format!("{}:[1]", n.proc_name()),
            },
        );
    }
    let mut seals = std::collections::BTreeMap::new();
    for k in SealKind::ALL {
        seals.insert(
            k,
            Seal::Held {
                how: "the fixture says it was tried and did not take".into(),
            },
        );
    }
    crate::exec::Boundary {
        mechanism: "bubblewrap 0.11.1 (fixture)".into(),
        platform: crate::exec::confine::Mechanism::Namespaces,
        view: ViewSpec::project_only("/w"),
        egress: Egress::Denied,
        ns,
        seals,
        props: std::collections::BTreeMap::new(),
        measured_at: std::time::SystemTime::now(),
    }
}

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

/// **The ladder's inputs for a call nothing was left off**: a durable rule when one is
/// named, and a session grant that would be honoured.
///
/// The ladder functions take an [`Offer`] rather than loose arguments because the rungs
/// and the note explaining a missing one are one fact — see [`Offer`]. A test that wants
/// a missing rung asks the gate for it (`AdjudicatedGate::request_for`) instead of
/// assembling an `Offer` by hand, so the only way to see the two disagree is a real call.
fn offered(derived: Option<&str>, program: &str) -> Offer {
    Offer {
        derived: derived.map(str::to_string),
        program: program.to_string(),
        session: true,
        note: None,
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

mod boundary;
mod breaker;
mod card;
mod gate;
mod grants;
mod history;
mod memory;
mod outcomes;
mod supervised;
mod unasked;
