//! The permission job as layer B: an `AuthorisationOracle` behind `ModelAdjudicator`, so a shadow
//! answer has the gate's brief, its trail, its scope and its corpus row. Stage 1 of R316: reads
//! only, past the shadow's read denylist, while `answer_reads` is on and the breaker is armed.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::{Arc, atomic::AtomicBool, atomic::Ordering};
use std::time::Duration;

use letibot_tools::adjudicate::{
    AdjudicationDecision, AdjudicationRequest, Adjudicator, DecisionOutcome, ModelAdvice, Tier,
};
use letibot_tools::authorise::{
    AuthorisationOracle, ModelBrief, OracleAnswer, OracleScope, UnsureKind, Widening,
};
use letibot_tools::intent::Intent;

use super::Job;
use super::permission::{Answering, Cell};
use super::runner::Outcome;
use super::verdict::{Decision, Shown, Verdict};

/// The line an oracle brief opens with, so the permission prompt knows which question it is.
pub const QUESTION: &str = "question: did the operator ask for this?";

/// The stage-1 scope: reads that land outside the project at most, never off the box.
pub fn stage_one_scope() -> OracleScope {
    OracleScope::earned(
        [Intent::Inspect, Intent::ReadFile].into_iter().collect(),
        letibot_tools::adjudicate::EffectScope::HostOther,
        "R316 stage 1: reads past the shadow's read denylist, promoted by `[shadow] \
         answer_reads = true` and measured by `letibot shadow replay --job permission`",
    )
}

/// Why `req` is outside stage 1, or `Ok`: may-approve (the one always-ask predicate is the tier),
/// a resolved read, and every path it names past [`super::guard::read_passes`].
pub fn stage_one(req: &AdjudicationRequest, home: &Path, workspace: &Path) -> Result<(), String> {
    if !matches!(req.tier, Tier::MayApprove) {
        return Err(format!(
            "the tier is {}, which the operator decides",
            req.tier.as_str()
        ));
    }
    let reading = req
        .reading
        .as_ref()
        .ok_or("layer A's reading did not travel with the ask")?;
    if !reading.resolved {
        return Err("layer A could not read the call".into());
    }
    if reading.intents.is_empty()
        || !reading
            .intents
            .iter()
            .all(|i| matches!(i, Intent::Inspect | Intent::ReadFile))
    {
        return Err("not a read".into());
    }
    for s in &reading.scoped {
        let t = s.target.trim();
        if t.is_empty() {
            continue;
        }
        let p = Path::new(t);
        let abs = if p.is_absolute() {
            p.to_path_buf()
        } else {
            workspace.join(p)
        };
        super::guard::read_passes(&abs.to_string_lossy(), home)?;
    }
    Ok(())
}

/// `shadow:<judge>`: who a shadow decision is by, so the ledger and the corpus tell it apart.
fn shadow_by(cell: &Cell) -> String {
    let label = cell
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|(s, _)| s.label(Job::Permission));
    format!(
        "{}{}",
        letibot_tools::grant_ledger::SHADOW_BY,
        label.unwrap_or_else(|| "permission".into())
    )
}

/// The shadow's permission job, asked the oracle's one question under its own 2.5s deadline.
pub struct ShadowOracle {
    pub cell: Cell,
    pub answering: Arc<Answering>,
}

impl ShadowOracle {
    fn unsure(why: impl Into<String>, kind: UnsureKind) -> OracleAnswer {
        OracleAnswer::Unsure {
            why: why.into(),
            kind,
        }
    }
}

impl AuthorisationOracle for ShadowOracle {
    fn authorised(&self, brief: &mut ModelBrief) -> OracleAnswer {
        if !self.answering.is_on() {
            return Self::unsure(
                "the shadow is observe-only: answer_reads is off or its breaker tripped",
                UnsureKind::CouldNotDecide,
            );
        }
        let Some((shadow, _)) = self.cell.lock().unwrap_or_else(|e| e.into_inner()).clone() else {
            return Self::unsure("the shadow is not open yet", UnsureKind::Unreadable);
        };
        let shown = Shown {
            data: format!("{QUESTION}\n\n{}", brief.render()),
            oracle: true,
            ..Shown::default()
        };
        let label = shadow
            .label(Job::Permission)
            .unwrap_or_else(|| "permission".into());
        let outcome = match shadow.ask(Job::Permission, shown) {
            Some(o) => o,
            None => return Self::unsure("the permission job is off", UnsureKind::Unreadable),
        };
        let cites = match outcome {
            Outcome::Verdict(Verdict::Permission {
                decision: Decision::Allow { cites, .. },
                ..
            }) => cites,
            Outcome::Verdict(Verdict::Permission { evidence, .. }) => {
                return Self::unsure(
                    format!("shadow ({label}) said ask: {}", evidence.trim()),
                    UnsureKind::CouldNotDecide,
                );
            }
            Outcome::TimedOut => {
                return Self::unsure(
                    format!("shadow ({label}) gave no answer inside its deadline"),
                    UnsureKind::Unreadable,
                );
            }
            other => {
                return Self::unsure(
                    format!("shadow ({label}) gave no verdict: {other:?}"),
                    UnsureKind::Unreadable,
                );
            }
        };
        // The ALLOW-must-cite rule HttpOracle applies: a number must name operator words.
        let good = brief.trail.cited_operator_words(&cites);
        if good.is_empty() {
            return Self::unsure(
                format!(
                    "shadow ({label}) answered allow citing {cites:?}, and none of those is an \
                     operator message in the trail"
                ),
                UnsureKind::CouldNotDecide,
            );
        }
        let Some(witness) = brief.adjudicable() else {
            return Self::unsure(
                "allow on an action layer A did not mark adjudicable",
                UnsureKind::CouldNotDecide,
            );
        };
        OracleAnswer::Authorised(Widening::new(
            witness,
            brief.request_id.clone(),
            good,
            format!("shadow ({label}) read the trail as asking for this"),
        ))
    }

    fn describe(&self) -> String {
        shadow_by(&self.cell)
    }

    fn scope(&self) -> OracleScope {
        stage_one_scope()
    }

    fn budget(&self) -> Duration {
        Job::Permission.deadline()
    }
}

/// `ModelAdjudicator` over [`ShadowOracle`], asked only about stage-1 calls. Anything else
/// escalates to the person without the shadow being asked.
pub struct ShadowAdjudicator {
    model: letibot_tools::ModelAdjudicator,
    cell: Cell,
    answering: Arc<Answering>,
    home: PathBuf,
    workspace: PathBuf,
    /// A grant the shadow gave is kept where `/shadow revoke` finds it: a daemon with a store.
    revocable: bool,
    /// The last call never reached the model, so its advice is not this call's.
    skipped: AtomicBool,
    last: Mutex<Option<ModelAdvice>>,
}

impl ShadowAdjudicator {
    pub fn new(
        cell: Cell,
        answering: Arc<Answering>,
        home: PathBuf,
        workspace: PathBuf,
        revocable: bool,
    ) -> Self {
        let oracle = ShadowOracle {
            cell: cell.clone(),
            answering: answering.clone(),
        };
        ShadowAdjudicator {
            // The reading always travels on a stage-1 ask; without one it never gets here.
            model: letibot_tools::ModelAdjudicator::new(Box::new(oracle), |_| {
                letibot_tools::intent::Baseline::of_paths(
                    [],
                    false,
                    false,
                    &letibot_tools::intent::Surroundings::default(),
                )
            }),
            cell,
            answering,
            home,
            workspace,
            revocable,
            skipped: AtomicBool::new(false),
            last: Mutex::new(None),
        }
    }

    fn escalate(&self, req: &AdjudicationRequest, why: String) -> AdjudicationDecision {
        self.skipped.store(true, Ordering::SeqCst);
        AdjudicationDecision {
            request_id: req.id.clone(),
            outcome: DecisionOutcome::Escalate {
                to: "human".into(),
                why: why.clone(),
            },
            by: shadow_by(&self.cell),
            basis: why,
            latency_ms: 0,
            grant_at: None,
        }
    }
}

/// The path of a view read: arguments that are one `path` and nothing else.
fn view_path(req: &AdjudicationRequest) -> Option<String> {
    let o = req.arguments.as_object()?;
    (o.len() == 1)
        .then(|| o.get("path")?.as_str().map(str::to_string))
        .flatten()
}

impl Adjudicator for ShadowAdjudicator {
    fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
        if !self.answering.is_on() {
            return self.escalate(
                req,
                "the shadow is observe-only: answer_reads is off or its breaker tripped".into(),
            );
        }
        if !self.revocable {
            return self.escalate(
                req,
                "no store, so a grant the shadow gave could not be revoked".into(),
            );
        }
        if let Err(why) = stage_one(req, &self.home, &self.workspace) {
            return self.escalate(req, format!("outside the shadow's stage-1 scope: {why}"));
        }
        self.skipped.store(false, Ordering::SeqCst);
        let d = self.model.decide(req);
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = self.model.last_advice();
        if let Some(path) = view_path(req) {
            match &d.outcome {
                DecisionOutcome::Selected { option_id } if option_id == "allow_once" => self
                    .answering
                    .granted_read(&req.id, &req.session_id, &path, &d.basis),
                _ => self.answering.asked_read(&req.id, &path),
            }
        }
        d
    }

    fn describe(&self) -> String {
        format!(
            "{} Stage 1 only: reads past the shadow's read denylist.",
            self.model.describe()
        )
    }

    fn last_brief(&self) -> Option<String> {
        (!self.skipped.load(Ordering::SeqCst))
            .then(|| self.model.last_brief())
            .flatten()
    }

    fn last_reply(&self) -> Option<String> {
        (!self.skipped.load(Ordering::SeqCst))
            .then(|| self.model.last_reply())
            .flatten()
    }

    fn last_advice(&self) -> Option<ModelAdvice> {
        (!self.skipped.load(Ordering::SeqCst))
            .then(|| self.last.lock().unwrap_or_else(|e| e.into_inner()).clone())
            .flatten()
    }
}
