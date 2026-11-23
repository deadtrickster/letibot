//! The permission job as layer B: an `AuthorisationOracle` behind `ModelAdjudicator`, so a shadow
//! answer has the gate's brief, its trail, its scope and its corpus row. It answers the stages the
//! operator promoted (`[shadow] answers`) - reads past the shadow's denylist, writes inside the
//! workspace AND inside the roots the operator granted writable, exec in either - while the breaker
//! is armed, and never the always-ask list.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::{Arc, atomic::AtomicBool, atomic::Ordering};
use std::time::Duration;

use letibot_tools::adjudicate::{
    AdjudicationDecision, AdjudicationRequest, Adjudicator, DecisionOutcome, EffectScope,
    ModelAdvice, Tier,
};
use letibot_tools::authorise::{
    AuthorisationOracle, ModelBrief, OracleAnswer, OracleScope, UnsureKind, Widening,
};
use letibot_tools::intent::Intent;

use super::Job;
use super::config::Stage;
use super::guard::Call;
use super::permission::{Answering, Cell};
use super::runner::Outcome;
use super::verdict::{Decision, Shown, Verdict};

/// The scope the stages in force earn. `reads` reaches outside the project at most; exec reaches
/// the host when the call only looks at it (see [`stage_of`]) and the workspace plus the operator's
/// writable grants when it writes, which one scope cannot say per call.
pub fn stage_scope(stages: &BTreeSet<Stage>) -> OracleScope {
    let mut intents: BTreeSet<Intent> = BTreeSet::new();
    let mut reach = EffectScope::HostProject;
    for stage in stages {
        match stage {
            Stage::Reads => {
                intents.extend([Intent::Inspect, Intent::ReadFile]);
                reach = reach.max(EffectScope::HostOther);
            }
            Stage::WorkspaceWrites => {
                intents.extend([Intent::Inspect, Intent::ReadFile, Intent::WriteFile]);
            }
            Stage::Exec => {
                intents.extend([
                    Intent::Inspect,
                    Intent::ReadFile,
                    Intent::WriteFile,
                    Intent::ExecuteCode,
                    Intent::Destroy,
                ]);
                // A diagnostic that only reads the host is the exec stage's cheapest case, so the
                // scope has to reach that far for `stage_of`'s per-call answer to be reachable.
                reach = reach.max(EffectScope::HostOther);
            }
        }
    }
    OracleScope::earned(
        intents,
        reach,
        format!(
            "R316 stages {}: reads past the shadow's read denylist, writes and exec inside the \
             workspace and the operator's writable grants, and an exec that only looks at the host \
             as far as `host_other`, promoted by `[shadow] answers` and measured by `letibot shadow \
             replay --job permission`",
            stages
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    )
}

/// The stage a call belongs to, or why no stage answers it. A call is one stage's: the narrowest
/// that covers every intent it carries, and a stage the operator promoted is checked by the caller.
/// Tier, resolution, a secret flow and a path that leaves the reach are refusals here, so the
/// person is asked without the model being consulted. `granted` is the writable half of the
/// session's view, so a denylist check treats a granted folder as the workspace it was made into.
pub fn stage_of(
    req: &AdjudicationRequest,
    home: &Path,
    workspace: &Path,
    granted: &[PathBuf],
) -> Result<Stage, String> {
    match &req.tier {
        Tier::MayApprove => {}
        Tier::AlwaysAsk { rule, .. } => {
            return Err(format!(
                "`{rule}` is on the always-ask list, which is the operator's whatever stages are on"
            ));
        }
        other => {
            return Err(format!(
                "the tier is {}, which the operator decides",
                other.as_str()
            ));
        }
    }
    let reading = req
        .reading
        .as_ref()
        .ok_or("layer A's reading did not travel with the ask")?;
    if !reading.resolved {
        return Err("layer A could not read the call".into());
    }
    if let Some(flow) = reading.flows.first() {
        return Err(format!(
            "it moves a secret out of {} - credentials are never a stage's",
            flow.store
        ));
    }
    if reading.intents.is_empty() {
        return Err("layer A found no intent, so there is nothing to answer".into());
    }
    let stage = stage_for(&reading.intents)?;
    // Reach is per call because a scope carries one reach for every intent it holds. Only the
    // intents decide it: `Region::Device` is a place inside the host, so `/sys`, `ip neigh` and
    // `networkctl status` land at `host_other`, while a class reaches `external` only through the
    // network intent, and such a call is refused below.
    let looks_only = !reading.intents.contains(&Intent::WriteFile)
        && !reading.intents.contains(&Intent::Destroy);
    let allowed = match stage {
        Stage::Reads => EffectScope::HostOther,
        // Writing or destroying at exec keeps the workspace and the operator's grants: the host is
        // not a place this session writes to, and what may be written is the class's own answer.
        Stage::Exec if looks_only => EffectScope::HostOther,
        _ => EffectScope::HostProject,
    };
    if req.class.scope > allowed {
        return Err(format!(
            "it lands on `{}`, and stage `{}` reaches `{}`",
            req.class.scope.as_str(),
            stage.as_str(),
            allowed.as_str()
        ));
    }
    let call = Call {
        tool: req.tool.clone(),
        args: req.arguments.clone(),
        cwd: workspace.to_path_buf(),
        workspace: workspace.to_path_buf(),
        home: home.to_path_buf(),
        granted: granted.to_vec(),
    };
    if stage != Stage::Reads {
        // The lexical screen a shadow allow meets anyway: privilege, a push, a write, a redirect
        // or a file-tool path outside the workspace, and the key store.
        super::guard::allow_passes(&call)?;
    }
    for s in &reading.scoped {
        let t = s.target.trim();
        if t.is_empty() || t == letibot_tools::intent::UNRESOLVED_TARGET {
            return Err(format!(
                "{} names no path this build can place",
                s.intent.as_str()
            ));
        }
        let p = Path::new(t);
        let abs = if p.is_absolute() {
            p.to_path_buf()
        } else {
            workspace.join(p)
        };
        // A secret store is no stage's, and this is the lexical half of that rule. Where a write
        // or an exec LANDS past the workspace is the class's answer above and
        // `allow_passes`'s below; a program's own path is not a place the call reaches.
        super::guard::read_passes(&abs.to_string_lossy(), home)?;
    }
    Ok(stage)
}

/// The narrowest stage that covers every intent the call carries.
fn stage_for(intents: &BTreeSet<Intent>) -> Result<Stage, String> {
    let within = |allowed: &[Intent]| intents.iter().all(|i| allowed.contains(i));
    let reads = [Intent::Inspect, Intent::ReadFile];
    if within(&reads) {
        return Ok(Stage::Reads);
    }
    let writes = [Intent::Inspect, Intent::ReadFile, Intent::WriteFile];
    if within(&writes) {
        return Ok(Stage::WorkspaceWrites);
    }
    let exec = [
        Intent::Inspect,
        Intent::ReadFile,
        Intent::WriteFile,
        Intent::ExecuteCode,
        Intent::Destroy,
        // A program this build never heard of - `ip`, `networkctl`, `nmcli` - is exec, and the
        // model is what judges it; refusing it here made the shadow refuse the read-only
        // diagnostics it exists for. `OracleScope::covers` skips it too, for the same reason.
        Intent::Unknown,
    ];
    if within(&exec) {
        return Ok(Stage::Exec);
    }
    Err(format!(
        "it carries {}, and no stage answers that",
        intents
            .iter()
            .map(|i| i.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    ))
}

/// `shadow:<judge>`: who a shadow decision is by, so the ledger and the corpus tell it apart.
fn shadow_by(cell: &Cell) -> String {
    format!(
        "{}{}",
        letibot_tools::grant_ledger::SHADOW_BY,
        judge_of(cell).unwrap_or_else(|| "permission".into())
    )
}

/// The model the permission job runs on, as the shadow was built with it.
fn judge_of(cell: &Cell) -> Option<String> {
    cell.lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|(s, _)| s.label(Job::Permission))
}

/// The shadow's permission job, asked the oracle's one question under its own 2.5s deadline.
pub struct ShadowOracle {
    pub cell: Cell,
    pub answering: Arc<Answering>,
    pub stages: BTreeSet<Stage>,
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
                "the shadow is observe-only: no stage is on, or its breaker tripped",
                UnsureKind::CouldNotDecide,
            );
        }
        let Some((shadow, _)) = self.cell.lock().unwrap_or_else(|e| e.into_inner()).clone() else {
            return Self::unsure("the shadow is not open yet", UnsureKind::Unreadable);
        };
        // The caller's own flag says which question this is: the data carries no marker, so no
        // transcript line can choose the prompt. See `runner::Inner::system_for`.
        let shown = Shown {
            data: brief.render(),
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
        stage_scope(&self.stages)
    }

    fn budget(&self) -> Duration {
        Job::Permission.deadline()
    }
}

/// `ModelAdjudicator` over [`ShadowOracle`], asked only about calls inside a promoted stage.
/// Anything else escalates to the person without the shadow being asked.
pub struct ShadowAdjudicator {
    model: letibot_tools::ModelAdjudicator,
    cell: Cell,
    answering: Arc<Answering>,
    stages: BTreeSet<Stage>,
    home: PathBuf,
    workspace: PathBuf,
    /// The roots the session's view holds beyond the workspace, read at each ask: a revoked grant
    /// must stop counting before the next call, so this is the boundary's live set and not a copy.
    granted: letibot_tools::view::GrantedRoots,
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
        stages: BTreeSet<Stage>,
        home: PathBuf,
        workspace: PathBuf,
        revocable: bool,
        granted: letibot_tools::view::GrantedRoots,
    ) -> Self {
        let oracle = ShadowOracle {
            cell: cell.clone(),
            answering: answering.clone(),
            stages: stages.clone(),
        };
        ShadowAdjudicator {
            // The reading always travels on a stage ask; without one it never gets here.
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
            stages,
            home,
            workspace,
            granted,
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

    /// What the card says about who answered: the model and the stage, because a person reading
    /// "allowed" has to know how far the shadow's authority reached for it.
    fn said(&self, d: &mut AdjudicationDecision, stage: Stage) {
        let Some(label) = judge_of(&self.cell) else {
            return;
        };
        d.basis = match &d.outcome {
            DecisionOutcome::Selected { option_id } if option_id == "allow_once" => format!(
                "allowed by the shadow ({label}, {}): {}",
                stage.as_str(),
                d.basis
            ),
            _ => d.basis.clone(),
        };
    }
}

/// The one thing this call is about, as the breaker's shape key and the card's target: the single
/// `path` argument of a file tool, else the request's own target.
fn target_of(req: &AdjudicationRequest) -> String {
    if let Some(o) = req.arguments.as_object()
        && o.len() == 1
        && let Some(p) = o.get("path").and_then(|p| p.as_str())
    {
        return p.to_string();
    }
    req.target.clone()
}

impl Adjudicator for ShadowAdjudicator {
    fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
        if !self.answering.is_on() {
            return self.escalate(
                req,
                "the shadow is observe-only: no stage is on, or its breaker tripped \
                 (`/shadow arm` re-arms it)"
                    .into(),
            );
        }
        if !self.revocable {
            return self.escalate(
                req,
                "no store, so a grant the shadow gave could not be revoked".into(),
            );
        }
        let stage = match stage_of(req, &self.home, &self.workspace, &self.granted.writable()) {
            Ok(s) => s,
            Err(why) => return self.escalate(req, format!("the shadow does not answer it: {why}")),
        };
        if !self.stages.contains(&stage) {
            return self.escalate(
                req,
                format!(
                    "`{}` is not a stage the shadow answers (it answers {})",
                    stage.as_str(),
                    self.stages
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            );
        }
        self.skipped.store(false, Ordering::SeqCst);
        let mut d = self.model.decide(req);
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = self.model.last_advice();
        self.said(&mut d, stage);
        let target = target_of(req);
        match &d.outcome {
            DecisionOutcome::Selected { option_id } if option_id == "allow_once" => {
                self.answering.granted(stage, &target)
            }
            _ => self.answering.asked(&req.id, stage, &target),
        }
        d
    }

    fn describe(&self) -> String {
        format!(
            "{} Stages: {}. Never the always-ask list, and never a path outside them.",
            self.model.describe(),
            self.stages
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
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
