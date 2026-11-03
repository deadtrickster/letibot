//! The tool side of the adjudication seam: one request, one decision, fail closed.
//! [`NoAdjudicator`] answers `Unavailable` and the refusal carries `NotRun` rather
//! than `Denied`, because nobody decided; a write also needs a writable backend.

use std::sync::Mutex;

use letibot_transcript::ToolOutcome;
use serde_json::Value;

use crate::authorise::UnsureKind;
use crate::runtime::{Gate, GateCall, GateDecision};
use crate::schema::Access;

/// Which data-flow rule an action breaks. The rule is about flow, not access: `ssh`
/// reads the private key legitimately, so what separates the rows is where the bytes
/// end up. See [`crate::intent`] for the derivation and its limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FlowRule {
    /// Into a tool result, therefore the transcript, therefore the model's context
    /// and the store.
    SecretToTranscript,
    /// Into a location with weaker protection, and from there anywhere.
    SecretToWeakerLocation,
    /// Off the machine.
    SecretOffBox,
    /// A secret is an operand of a program whose data flow the harness cannot state.
    /// Refused rather than guessed at: an unrecognised program is not a program that
    /// keeps secrets.
    SecretFlowUnknown,
    /// A write into a secret store, kept here so both mechanisms exist.
    WriteIntoSecretStore,
}

impl FlowRule {
    pub fn as_str(&self) -> &'static str {
        match self {
            FlowRule::SecretToTranscript => "secret_to_transcript",
            FlowRule::SecretToWeakerLocation => "secret_to_weaker_location",
            FlowRule::SecretOffBox => "secret_off_box",
            FlowRule::SecretFlowUnknown => "secret_flow_unknown",
            FlowRule::WriteIntoSecretStore => "write_into_secret_store",
        }
    }
}

/// The four outcome classes, decided by layer A alone. The axis is who decides: `Auto`
/// nobody, `MayApprove` the model, `AlwaysAsk` the operator every time, `Blocked` nobody
/// ever. Only the middle two are adjudicable, and `Blocked` sits above every adjudicator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tier {
    /// Clause 4: a read-only action inside the boundary never prompts.
    Auto,
    /// The model may widen this into an admission, within the scope it has earned.
    MayApprove,
    /// The operator decides, every time. `rule` names the entry on
    /// [`crate::intent::ALWAYS_ASK`] and `why` is that entry's reason, so an operator
    /// reading a prompt can see what they are trading.
    AlwaysAsk { rule: &'static str, why: String },
    /// Nothing promotes this. `evidence` is the sentence the audit row carries.
    Blocked { rule: FlowRule, evidence: String },
}

impl Tier {
    pub fn as_str(&self) -> &'static str {
        match self {
            Tier::Auto => "auto",
            Tier::MayApprove => "may_approve",
            Tier::AlwaysAsk { .. } => "always_ask",
            Tier::Blocked { .. } => "blocked",
        }
    }

    pub fn is_blocked(&self) -> bool {
        matches!(self, Tier::Blocked { .. })
    }

    /// Whether an oracle may be consulted: `MayApprove` and `AlwaysAsk` both, since the
    /// cards a fixed always-ask list produces are what teach an operator to press
    /// allow-all. `Auto` needs nobody and `Blocked` is nobody's to move.
    pub fn is_adjudicable_by_model(&self) -> bool {
        matches!(self, Tier::MayApprove | Tier::AlwaysAsk { .. })
    }

    /// The stricter of two tiers. Layer A combines findings across the stages of one
    /// command, and combining must never relax: a pipeline with one disclosing stage
    /// is a disclosing pipeline.
    pub fn strictest(self, other: Tier) -> Tier {
        fn rank(t: &Tier) -> u8 {
            match t {
                Tier::Auto => 0,
                Tier::MayApprove => 1,
                Tier::AlwaysAsk { .. } => 2,
                Tier::Blocked { .. } => 3,
            }
        }
        if rank(&other) > rank(&self) {
            other
        } else {
            self
        }
    }
}

/// The witness that layer A found this action adjudicable. The field is private, so
/// only [`AdjudicationRequest::adjudicable`] mints one, and it answers `None` for a
/// `Blocked` tier or an unresolved command. Not `Clone`: a witness must not travel.
#[derive(Debug)]
pub struct Adjudicable(());

/// Where an action's effect lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EffectScope {
    /// Inside a firecode run: a copy of one project and nothing else of the host.
    InRun,
    /// The host's project directory: the session's own workspace.
    HostProject,
    /// The host, outside the workspace.
    HostOther,
    /// Off the box.
    External,
}

impl EffectScope {
    pub fn as_str(&self) -> &'static str {
        match self {
            EffectScope::InRun => "in_run",
            EffectScope::HostProject => "host_project",
            EffectScope::HostOther => "host_other",
            EffectScope::External => "external",
        }
    }

    /// The inverse of [`EffectScope::as_str`], derived from it.
    pub fn parse(name: &str) -> Option<EffectScope> {
        [
            EffectScope::InRun,
            EffectScope::HostProject,
            EffectScope::HostOther,
            EffectScope::External,
        ]
        .into_iter()
        .find(|s| s.as_str() == name.trim())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Reversibility {
    Reversible,
    Irreversible,
}

impl Reversibility {
    pub fn as_str(&self) -> &'static str {
        match self {
            Reversibility::Reversible => "reversible",
            Reversibility::Irreversible => "irreversible",
        }
    }

    /// The inverse of [`Reversibility::as_str`], derived from it.
    pub fn parse(name: &str) -> Option<Reversibility> {
        [Reversibility::Reversible, Reversibility::Irreversible]
            .into_iter()
            .find(|r| r.as_str() == name.trim())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Cost {
    Free,
    Metered,
}

impl Cost {
    pub fn as_str(&self) -> &'static str {
        match self {
            Cost::Free => "free",
            Cost::Metered => "metered",
        }
    }

    /// The inverse of [`Cost::as_str`], derived from it.
    pub fn parse(name: &str) -> Option<Cost> {
        [Cost::Free, Cost::Metered]
            .into_iter()
            .find(|c| c.as_str() == name.trim())
    }
}

/// The routing key: `(tool.access, effect_scope, reversibility, cost)`, derived from
/// four facts the harness already has. Its `Display` is the spelling a policy table
/// would match on, so the log and the table share one vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ActionClass {
    pub access: Access,
    pub scope: EffectScope,
    pub reversibility: Reversibility,
    pub cost: Cost,
}

impl std::fmt::Display for ActionClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{},{},{},{}",
            self.access.as_str(),
            self.scope.as_str(),
            self.reversibility.as_str(),
            self.cost.as_str()
        )
    }
}

impl ActionClass {
    /// The inverse of [`ActionClass`]'s `Display`. Every field must parse or the whole
    /// thing refuses: a class missing one of its four facts is unknown, not narrower,
    /// and a `None` here means the row is skipped, which is asking.
    pub fn parse(s: &str) -> Option<ActionClass> {
        let mut parts = s.split(',');
        let (a, sc, r, c) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
        if parts.next().is_some() {
            return None;
        }
        Some(ActionClass {
            access: Access::parse(a)?,
            scope: EffectScope::parse(sc)?,
            reversibility: Reversibility::parse(r)?,
            cost: Cost::parse(c)?,
        })
    }

    /// The class of a host-local tool call. `cost` is always `Free` because no tool here
    /// spends credits or touches the network. `reversibility` is `Reversible` only for an
    /// edit or overwrite, meaning the prior bytes went to the head, not that anything undoes it.
    pub fn host(access: Access, inside_workspace: bool, creates: bool) -> Self {
        ActionClass {
            access,
            scope: if inside_workspace {
                EffectScope::HostProject
            } else {
                EffectScope::HostOther
            },
            reversibility: if creates {
                Reversibility::Irreversible
            } else {
                Reversibility::Reversible
            },
            cost: Cost::Free,
        }
    }

    /// The class of a call that leaves the box. A network call has no `path` and
    /// `path_is_inside` answers true for its absence, so [`ActionClass::host`] would log it
    /// as landing in the operator's project. Always `Irreversible`: it cannot be recalled.
    pub fn external(access: Access, cost: Cost) -> Self {
        ActionClass {
            access,
            scope: EffectScope::External,
            reversibility: Reversibility::Irreversible,
            cost,
        }
    }
}

/// A permission and a question are one mechanism, differing in `kind` and in what
/// happens when nobody answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestKind {
    Permission,
    Question,
}

/// What happens when nobody answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnTimeout {
    Deny,
    Allow,
    AgentDecides,
}

/// One option an adjudicator may select.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionOption {
    pub id: String,
    pub label: String,
    pub kind: OptionKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionKind {
    AllowOnce,
    /// Admit this call and every later call of the same class in this session.
    AllowSession,
    /// As `AllowSession`, and nothing here persists it past the process: it behaves as
    /// `AllowSession` and says so in the row, because claiming a durable grant the harness
    /// does not store would be a lie in the audit.
    AllowAlways,
    Deny,
    /// Deny, and put the reason in front of the model rather than only in the log.
    DenyAndTell,
}

impl OptionKind {
    /// Whether choosing this option lets the call run. The three allow shapes differ
    /// in how far the answer travels, not in whether it admits.
    pub fn is_allow(self) -> bool {
        matches!(
            self,
            OptionKind::AllowOnce | OptionKind::AllowSession | OptionKind::AllowAlways
        )
    }
}

impl OptionKind {
    pub fn admits(&self) -> bool {
        matches!(
            self,
            OptionKind::AllowOnce | OptionKind::AllowSession | OptionKind::AllowAlways
        )
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            OptionKind::AllowOnce => "allow_once",
            OptionKind::AllowSession => "allow_session",
            OptionKind::AllowAlways => "allow_always",
            OptionKind::Deny => "deny",
            OptionKind::DenyAndTell => "deny_and_tell",
        }
    }
}

/// The ladder for a path the boundary hid: readable, read and write, or no. One button
/// for both would make an operator pick the wider to get anything done. Read-only is
/// first and is where Enter lands.
pub fn view_grant_options() -> Vec<DecisionOption> {
    vec![
        DecisionOption {
            id: "grant_ro".into(),
            label: "Let this session READ it (for the rest of the session)".into(),
            kind: OptionKind::AllowOnce,
        },
        DecisionOption {
            id: "grant_rw".into(),
            label: "Let this session READ AND WRITE it (for the rest of the session)".into(),
            kind: OptionKind::AllowOnce,
        },
        DecisionOption {
            id: "deny".into(),
            label: "No — leave it outside the view".into(),
            kind: OptionKind::Deny,
        },
    ]
}

/// The memory card's answers: grant what was asked once, or for the session. The
/// session grant is bounded by the amount on this card and by what is free when a later
/// ask arrives; anything larger asks again ([`AdjudicatedGate::grant_memory`]).
pub fn memory_grant_options() -> Vec<DecisionOption> {
    vec![
        DecisionOption {
            id: "grant_memory".into(),
            label: "Let this command have it".into(),
            kind: OptionKind::AllowOnce,
        },
        DecisionOption {
            id: "grant_memory_session".into(),
            label: "Let this session have up to this — ask again above it".into(),
            kind: OptionKind::AllowSession,
        },
        DecisionOption {
            id: "deny".into(),
            label: "No — it does not start".into(),
            kind: OptionKind::Deny,
        },
    ]
}

pub fn always_ask_options() -> Vec<DecisionOption> {
    vec![
        DecisionOption {
            id: "allow_once".into(),
            label: "Allow this one".into(),
            kind: OptionKind::AllowOnce,
        },
        DecisionOption {
            id: "deny".into(),
            label: "Deny".into(),
            kind: OptionKind::Deny,
        },
        DecisionOption {
            id: "deny_and_tell".into(),
            label: "Deny, and tell the model why".into(),
            kind: OptionKind::DenyAndTell,
        },
    ]
}

/// The durable rung, when one can be offered at all. `None` from [`derived_pattern`]
/// means the matcher would ask again whatever rule were written, so the rung is left off
/// rather than shown and then quietly ignored. One helper for all three ladders.
fn durable_rung(derived: Option<&str>) -> Option<DecisionOption> {
    derived.map(|d| DecisionOption {
        id: "allow_always".into(),
        label: format!("Always allow `{d}` (written to ~/.config/letibot/permission.json)"),
        kind: OptionKind::AllowAlways,
    })
}

/// The session rung, when a grant taken now could cover a later call: `Grant::covers`
/// refuses an unresolved action before it looks at coverage, so a session grant over an
/// unreadable command would cover nothing. The condition is [`Offer::session`].
fn session_rung(offer: &Offer, label: String) -> Option<DecisionOption> {
    offer.session.then(|| DecisionOption {
        id: "allow_session".into(),
        label,
        kind: OptionKind::AllowSession,
    })
}

pub fn permission_options(offer: &Offer) -> Vec<DecisionOption> {
    let mut out = vec![DecisionOption {
        id: "allow_once".into(),
        label: "Allow this one".into(),
        kind: OptionKind::AllowOnce,
    }];
    out.extend(session_rung(
        offer,
        "Allow this class for the rest of the session".into(),
    ));
    out.extend(durable_rung(offer.derived.as_deref()));
    out.push(DecisionOption {
        id: "deny".into(),
        label: "Deny".into(),
        kind: OptionKind::Deny,
    });
    out.push(DecisionOption {
        id: "deny_and_tell".into(),
        label: "Deny, and tell the model why".into(),
        kind: OptionKind::DenyAndTell,
    });
    out
}

/// What a ladder may offer for one call, and why anything is missing from it. Computed once
/// in `AdjudicatedGate::request_from` from the facts the recording site reads, so the list
/// and what an answer does cannot drift. `note` says why a rung is absent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Offer {
    pub derived: Option<String>,
    pub program: String,
    pub session: bool,
    pub note: Option<String>,
}

impl Offer {
    /// The line to draw, or nothing: one spelling of *is there anything to say*. A card that
    /// asked this and got `Some("")` would push a blank row under its ladder.
    pub fn note_line(&self) -> Option<&str> {
        self.note.as_deref().filter(|n| !n.trim().is_empty())
    }
}

/// The ladder for an exec-class call. The session rung is offered whenever the durable one
/// is: a rule that outlives the session is stronger than a grant that dies with the daemon,
/// so offering only the stronger is backwards. See [`Offer`] for what may be left off.
pub fn exec_options(offer: &Offer) -> Vec<DecisionOption> {
    // A call with no command has no program to name, so the label names the class.
    // `NO_PROGRAM` is a key and must match nothing a command produces, so printing it
    // would put a placeholder where a noun belongs, on the surface where a person decides.
    let session_label = if offer.program == NO_PROGRAM {
        "Allow this class for the rest of the session".to_string()
    } else {
        format!(
            "Allow `{}` (this class) for the rest of the session",
            offer.program
        )
    };
    let mut out = vec![DecisionOption {
        id: "allow_once".into(),
        label: "Allow this one".into(),
        kind: OptionKind::AllowOnce,
    }];
    out.extend(session_rung(offer, session_label));
    out.extend(durable_rung(offer.derived.as_deref()));
    out.push(DecisionOption {
        id: "deny".into(),
        label: "Deny".into(),
        kind: OptionKind::Deny,
    });
    out.push(DecisionOption {
        id: "deny_and_tell".into(),
        label: "Deny, and tell the model why".into(),
        kind: OptionKind::DenyAndTell,
    });
    out
}

/// The ladder at a point whose grants are [`crate::mode::GrantScope::Once`]. The session
/// rung is here too, because an explicit answer is the operator overriding the point's
/// default, and a durable rule was already on the ladder.
pub fn once_only_options(mode_name: &'static str, offer: &Offer) -> Vec<DecisionOption> {
    let mut out = vec![DecisionOption {
        id: "allow_once".into(),
        label: "Allow this one".into(),
        kind: OptionKind::AllowOnce,
    }];
    out.extend(session_rung(
        offer,
        "Allow this class for the rest of the session".into(),
    ));
    out.extend(durable_rung(offer.derived.as_deref()));
    out.push(DecisionOption {
        id: "deny".into(),
        label: "Deny".into(),
        kind: OptionKind::Deny,
    });
    out.push(DecisionOption {
        id: "deny_and_tell".into(),
        label: format!(
            "Deny, and tell the model why  (mode `{mode_name}` asks once per call by \
                 default; `allow_session` above lifts that for this class until the \
                 daemon stops)"
        ),
        kind: OptionKind::DenyAndTell,
    });
    out
}

/// One aggregated line of history: what the gate did before, on actions of this same task
/// direction. Aggregated by effect, because a denial is history too, and carrying counts
/// and turn distances - never the arguments, which must not reach the oracle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriorAnswer {
    /// The gate's effect on the prior call: `admit` or `refuse`, as the row spells it.
    pub effect: &'static str,
    pub count: usize,
    /// Turn distance to the most recent and to the first answer of this effect,
    /// when both turn ids carry a `#N` sequence. `None` on either side means the
    /// renderer says so rather than inventing a number.
    pub latest_turns_ago: Option<u64>,
    pub first_turns_ago: Option<u64>,
}

/// The request, minus the fields no adjudicator on this box can use yet. `boundary_facts`
/// holds what the host backend can honestly state; there is no `deadline`, because this
/// seam is synchronous and an adjudicator that has to wait owns its own.
#[derive(Debug, Clone, PartialEq)]
pub struct AdjudicationRequest {
    pub id: String,
    pub session_id: String,
    pub turn_id: String,
    pub call_id: String,
    pub agent: String,
    /// The tool as declared, not as spelled by the model.
    pub tool: String,
    pub class: ActionClass,
    /// One line a human can decide from without reading the arguments.
    pub summary: String,
    /// The one thing the decision is about, alone and unquoted: the command, the path, the
    /// URL. Beside `summary` rather than inside it, because a sentence with a shell line in
    /// the middle is the one shape a person cannot skim.
    pub target: String,
    pub arguments: Value,
    pub arguments_digest: String,
    /// What the backend can truthfully say about where this lands.
    pub boundary_facts: Vec<String>,
    /// The code this call will run, when it names a script file. An interpreter is
    /// `ExecuteCode` and nothing else to layer A, so `python3 deploy.py` arrives with its
    /// program unopened. Empty when no script is named, which the brief states.
    pub scripts: Vec<crate::runtime::ScriptSource>,
    pub kind: RequestKind,
    pub options: Vec<DecisionOption>,
    pub on_timeout: OnTimeout,

    /// The tier, derived by [`crate::intent`] and never chosen by a caller: the gate
    /// computes it the way it computes `class`. A field a caller filled in could be filled
    /// in wrongly.
    pub tier: Tier,
    /// Whether the grammar resolved the whole action. `false` is `NotRun` - nobody could
    /// decide - and no adjudicator is consulted.
    pub resolved: bool,
    /// Layer A's one-line reading: the verdict, the intents, the regions, the tier.
    pub baseline: String,
    /// The authorisation trail: the same command is authorised or not depending on what the
    /// operator just said, so a stateless classifier cannot be correct.
    pub trail: crate::authorise::AuthorisationTrail,
    /// Prior answers on actions of this shape, read back from the audit rows and carried to
    /// the brief. Set only by the gate, and never read for a tier or an admission: history
    /// feeds the brief, and the tier is layer A's alone.
    pub prior: Vec<PriorAnswer>,
    /// What this operator answered themselves, earlier in this session: the action and the
    /// verdict, not a count, so the guard has worked examples of this person's judgement.
    pub examples: Vec<crate::authorise::DecisionExample>,
    /// Which wording of the guard's question this request will be briefed with.
    /// The shipping one everywhere; `--calibrate --compare` is the only caller
    /// that sets the other, and it sets it to measure the difference.
    pub brief_variant: crate::authorise::BriefVariant,
    /// What the agent says it is doing, for the guard to connect the operator's
    /// words to this call. Never an authorisation and never citable - see
    /// [`crate::authorise::ModelBrief::agent_claim`].
    pub agent_claim: Option<String>,
    /// Layer A's finding as a value rather than prose. `baseline` is the sentence a human
    /// reads; this is what an adjudicator reads, because re-deriving it from
    /// `arguments["command"]` works for `bash` and for nothing else.
    pub reading: Option<crate::intent::Baseline>,
    /// The command's shape: the parse with its literals replaced by holes. `None` for a call
    /// that is not a command. See [`shape_of`].
    pub shape: Option<String>,
    /// Why a rung a person would expect is not on this ladder, in one line; `None` when the
    /// ladder is complete. A rung that disappears unexplained leaves a person wondering
    /// whether they misremembered. Written by the gate, drawn by the head verbatim.
    pub options_note: Option<String>,
    /// What the model already said about this, when a model was asked first. Filled only by
    /// [`SupervisedAdjudicator`]. Advice, never a decision: nothing reads it to admit, and a
    /// row where the two differ keeps both halves.
    pub advice: Option<ModelAdvice>,
}

/// The model's verdict, carried to the person about to overrule it or agree: you cannot
/// agree with something you were not shown. It travels on the request rather than being
/// re-derived by a head, which would be rendering a guess about what the model said.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelAdvice {
    /// Whether an oracle was actually consulted. `false` covers every answer given without
    /// asking a model - unresolved, always-ask, blocked - which are layer A's, not verdicts.
    /// Load-bearing for [`agreement`], which fires only when this is true.
    pub consulted: bool,
    /// `admit`, `ask` or `unavailable` - what the model's answer would have done on
    /// its own. Not an option id: at this point the model has not selected from the
    /// ladder, it has answered *did the operator ask for this*.
    pub would: &'static str,
    /// Which oracle, in its own words.
    pub by: String,
    /// Why. The sentence a person reads before agreeing.
    pub basis: String,
    /// Which of the operator's own utterances it relied on. Empty is a real answer and a loud
    /// one: an authorisation that cites nothing is one the oracle could not ground, and a
    /// person should weigh it differently.
    pub cites: Vec<String>,
    /// How long it took, against the budget.
    pub latency_ms: u64,
    /// Which `Unsure` this was, when it was one, and `None` for every other verdict: `basis`
    /// says it in words, this says it in a token so the corpus can count it with a `GROUP BY`
    /// instead of a regex. Only consulted answers carry it.
    pub unsure: Option<UnsureKind>,
}

impl ModelAdvice {
    /// One line for a head that has room for one line. An unconsulted oracle says so rather
    /// than presenting layer A's short-circuit as a verdict, which a person would weigh
    /// against nothing.
    pub fn line(&self) -> String {
        if !self.consulted {
            return format!("no model verdict — {}", self.basis);
        }
        format!(
            "model ({}) says {} in {} ms: {}",
            self.by, self.would, self.latency_ms, self.basis
        )
    }
}

/// Which rendering of [`AdjudicationRequest::brief`] a corpus row was produced under, stamped
/// on every row. Bump it whenever `brief` changes what it emits: the same decision under two
/// briefs is two different questions, and pooling them fits the seam between them.
pub const BRIEF_FORMAT: &str = "brief/2026-09-14";

impl AdjudicationRequest {
    /// The witness that layer A found this adjudicable and readable, and the only way
    /// anything can be admitted by an oracle. `resolved` is not part of it: an unresolved
    /// action is the one whose reading layer A could not produce, so the oracle is asked.
    pub fn adjudicable(&self) -> Option<Adjudicable> {
        self.tier
            .is_adjudicable_by_model()
            .then_some(Adjudicable(()))
    }
    /// The card a person reads, not the brief a model is shown. Rendered here rather than in
    /// each caller because a human over flowy and a console prompt need the same bytes, and
    /// three renderings of one request is how they stop agreeing.
    pub fn brief(&self) -> String {
        let mut out = String::new();
        // No request id: it is high-entropy, carries nothing about the call, and on a
        // boundary case the spelling alone tipped the verdict. It still keys the audit row
        // and the answer; this is the one place it is dropped, where it was never information.
        out.push_str(&format!("decision — {}\n", self.summary));
        out.push_str(&format!("tool: {}\nclass: {}\n", self.tool, self.class));
        for f in &self.boundary_facts {
            out.push_str(&format!("fact: {f}\n"));
        }
        out.push_str(&format!(
            "arguments ({}):\n{}\n",
            self.arguments_digest,
            arguments_preview(&self.arguments)
        ));
        out.push_str(&scripts_section(&self.scripts));
        out.push_str("options:\n");
        for o in &self.options {
            out.push_str(&format!("  {} — {}\n", o.id, o.label));
        }
        out
    }

    pub fn option(&self, id: &str) -> Option<&DecisionOption> {
        self.options.iter().find(|o| o.id == id)
    }
}

/// The code the command will run, for the adjudicator that can read it. Its own section
/// because it is the call itself, not a statement about it. A script that could not be read
/// says so: telling nothing apart from "runs no script" would invert the right answer.
fn scripts_section(scripts: &[crate::runtime::ScriptSource]) -> String {
    use crate::runtime::ScriptBody;
    if scripts.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "the program(s) this command will run, read from disk — judge THIS, not the \
         filename:\n",
    );
    for s in scripts {
        match &s.body {
            ScriptBody::Read(text) => {
                out.push_str(&format!("--- {} ---\n{text}\n", s.path));
            }
            ScriptBody::Truncated { head, omitted } => {
                out.push_str(&format!(
                    "--- {} (first {} bytes; {omitted} NOT SHOWN) ---\n{head}\n\
                     --- the rest of {} was not read; anything could be in it ---\n",
                    s.path,
                    head.len(),
                    s.path
                ));
            }
            ScriptBody::Unreadable(why) => {
                out.push_str(&format!(
                    "--- {} COULD NOT BE READ: {why} ---\n\
                     This call runs a program nobody here has seen.\n",
                    s.path
                ));
            }
        }
    }
    out.push('\n');
    out
}

/// The arguments, verbatim, with any value over 2 KiB spilled to a digest and a head/tail:
/// an adjudicator made to read a 40 KB body to approve a one-line edit is one nobody reads.
const ARG_PREVIEW_BYTES: usize = 2048;

fn arguments_preview(args: &Value) -> String {
    let mut out = String::new();
    let Some(obj) = args.as_object() else {
        return format!("  {args}");
    };
    for (k, v) in obj {
        let rendered = match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        if rendered.len() <= ARG_PREVIEW_BYTES {
            out.push_str(&format!("  {k} = {rendered:?}\n"));
        } else {
            let head = floor_char_boundary(&rendered, ARG_PREVIEW_BYTES / 2);
            let tail = ceil_char_boundary(&rendered, rendered.len() - ARG_PREVIEW_BYTES / 2);
            out.push_str(&format!(
                "  {k} = {:?} … [{} bytes omitted, sha {}] … {:?}\n",
                &rendered[..head],
                rendered.len() - ARG_PREVIEW_BYTES,
                crate::spill::content_hash(rendered.as_bytes()),
                &rendered[tail..]
            ));
        }
    }
    out
}

fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_char_boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// The closed outcome vocabulary: a throwing or non-conforming answerer becomes
/// `unavailable`, never silently opens the gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecisionOutcome {
    Selected {
        option_id: String,
    },
    /// First class rather than an error path.
    Escalate {
        to: String,
        why: String,
    },
    Unavailable,
    Cancelled,
    Timeout,
}

impl DecisionOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            DecisionOutcome::Selected { .. } => "selected",
            DecisionOutcome::Escalate { .. } => "escalate",
            DecisionOutcome::Unavailable => "unavailable",
            DecisionOutcome::Cancelled => "cancelled",
            DecisionOutcome::Timeout => "timeout",
        }
    }
}

/// The decision. `basis` is the reason, always present and never optional, so it is a
/// `String` and not an `Option<String>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdjudicationDecision {
    pub request_id: String,
    pub outcome: DecisionOutcome,
    /// The adjudicator id: `"boundary:firecode"`, `"human:deadtrickster"`,
    /// `"model:qwen-3.8-27b"`.
    pub by: String,
    pub basis: String,
    pub latency_ms: u64,
}

impl AdjudicationDecision {
    pub fn selected(req: &AdjudicationRequest, option_id: &str, by: &str, basis: &str) -> Self {
        AdjudicationDecision {
            request_id: req.id.clone(),
            outcome: DecisionOutcome::Selected {
                option_id: option_id.to_string(),
            },
            by: by.to_string(),
            basis: basis.to_string(),
            latency_ms: 0,
        }
    }

    pub fn unavailable(req: &AdjudicationRequest, by: &str, basis: &str) -> Self {
        AdjudicationDecision {
            request_id: req.id.clone(),
            outcome: DecisionOutcome::Unavailable,
            by: by.to_string(),
            basis: basis.to_string(),
            latency_ms: 0,
        }
    }
}

/// One adjudicator. The boundary, a person and a model are three implementations of this,
/// and the design's job is to make them interchangeable rather than to rank them.
pub trait Adjudicator: Send + Sync {
    fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision;

    /// For `EXPLAIN` and for the daemon's startup disclosures. An operator who
    /// cannot see which adjudicator is attached cannot see that none is.
    fn describe(&self) -> String;

    /// The bytes this adjudicator was actually shown for the last call, copied into
    /// [`AdjudicationRow::shown`]: a corpus whose input was reconstructed afterwards is a
    /// corpus of guesses. `None` for an adjudicator that shows nothing.
    fn last_brief(&self) -> Option<String> {
        None
    }

    /// What an oracle answered, verbatim, on the last call, when one answered. The twin of
    /// `last_brief`, and asked for rather than returned with the verdict because the reply is
    /// not part of the decision - the corpus is what needs it, a layer up.
    fn last_reply(&self) -> Option<String> {
        None
    }

    /// What a model said about the last call, when a model was consulted first: the gate
    /// copies it into the row's `model_verdict`. Without it that column holds whatever
    /// decided, and a corpus where model and operator never disagree has no signal.
    fn last_advice(&self) -> Option<ModelAdvice> {
        None
    }

    /// Ask the model and nobody else: for a call a standing grant already covers, where the
    /// verdict is a free labelled example and there is nothing to ask a person. `None` means
    /// nobody was consulted, and it must not block on a person.
    fn advise_only(&self, _req: &AdjudicationRequest) -> Option<ModelAdvice> {
        None
    }

    /// The glob the operator typed with their answer, when they typed one. Read only for
    /// `AllowAlways`, the one option that writes a rule. `None` means derive the pattern from
    /// the call, never match nothing.
    fn last_pattern(&self) -> Option<String> {
        None
    }
}

/// The default, and it refuses. Not "deny": nobody decided, so it answers `Unavailable`,
/// which [`AdjudicatedGate`] routes to the request's `on_timeout` - `Deny` over a write,
/// which becomes [`ToolOutcome::NotRun`] because no decision was made.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoAdjudicator;

impl Adjudicator for NoAdjudicator {
    fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
        AdjudicationDecision::unavailable(
            req,
            "none",
            "no adjudicator is attached to this session, so nothing can decide this; \
             the gate fails closed",
        )
    }

    fn describe(&self) -> String {
        "none attached — every gated call refuses (fail closed)".into()
    }
}

/// An adjudicator that is a function of the request: the seam a head, a flowy connector or a
/// model adjudicator plugs into. `None` from the function means nobody answered, which
/// becomes `Unavailable` - never an allow.
pub struct AskAdjudicator<F> {
    pub ask: F,
    pub id: String,
}

impl<F> AskAdjudicator<F>
where
    F: Fn(&AdjudicationRequest) -> Option<AdjudicationDecision> + Send + Sync,
{
    pub fn new(id: impl Into<String>, ask: F) -> Self {
        AskAdjudicator { ask, id: id.into() }
    }
}

impl<F> Adjudicator for AskAdjudicator<F>
where
    F: Fn(&AdjudicationRequest) -> Option<AdjudicationDecision> + Send + Sync,
{
    fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
        let started = std::time::Instant::now();
        match (self.ask)(req) {
            Some(mut d) => {
                if d.latency_ms == 0 {
                    d.latency_ms = started.elapsed().as_millis() as u64;
                }
                // A conforming answer names an option this request offered. One that does
                // not is `Unavailable`: a non-conforming answerer never opens the gate.
                if let DecisionOutcome::Selected { option_id } = &d.outcome
                    && req.option(option_id).is_none()
                {
                    return AdjudicationDecision {
                        request_id: req.id.clone(),
                        outcome: DecisionOutcome::Unavailable,
                        by: d.by,
                        basis: format!(
                            "the answer selected `{option_id}`, which this request did not \
                             offer; a non-conforming answer never opens the gate"
                        ),
                        latency_ms: d.latency_ms,
                    };
                }
                d
            }
            None => AdjudicationDecision {
                request_id: req.id.clone(),
                outcome: DecisionOutcome::Timeout,
                by: self.id.clone(),
                basis: "nobody answered".into(),
                latency_ms: started.elapsed().as_millis() as u64,
            },
        }
    }

    fn describe(&self) -> String {
        format!("ask via {}", self.id)
    }
}

/// A real ask over a pair of streams: write the brief, read an option id. The constructor
/// takes the streams so a test can hand it a `Cursor` and assert on the exact bytes a human
/// would have seen. Not the flowy human adjudicator: this one holds the call.
pub struct ConsoleAdjudicator {
    io: Mutex<ConsoleIo>,
    who: String,
}

struct ConsoleIo {
    input: Box<dyn std::io::BufRead + Send>,
    output: Box<dyn std::io::Write + Send>,
}

impl ConsoleAdjudicator {
    pub fn new(
        who: impl Into<String>,
        input: Box<dyn std::io::BufRead + Send>,
        output: Box<dyn std::io::Write + Send>,
    ) -> Self {
        ConsoleAdjudicator {
            io: Mutex::new(ConsoleIo { input, output }),
            who: who.into(),
        }
    }

    pub fn stdio(who: impl Into<String>) -> Self {
        Self::new(
            who,
            Box::new(std::io::BufReader::new(std::io::stdin())),
            Box::new(std::io::stderr()),
        )
    }
}

impl Adjudicator for ConsoleAdjudicator {
    fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
        use std::io::Write;
        let started = std::time::Instant::now();
        let mut io = match self.io.lock() {
            Ok(g) => g,
            // A poisoned mutex means a previous ask panicked: Unavailable, not a guess.
            // `by` names who DECIDED, and nobody did here - only `Selected` is the person,
            // so a timeout is never written into the corpus as a human's refusal.
            Err(_) => {
                return AdjudicationDecision::unavailable(
                    req,
                    "gate:unavailable",
                    &format!(
                        "the console adjudicator's stream is poisoned (asked {})",
                        self.who
                    ),
                );
            }
        };
        let io = &mut *io;
        let _ = write!(io.output, "{}\nchoose: ", req.brief());
        let _ = io.output.flush();

        let mut line = String::new();
        match io.input.read_line(&mut line) {
            // EOF: the stream closed without an answer. Silence is not absence here, because
            // the stream told us. Timeout, which routes to `on_timeout`.
            Ok(0) => AdjudicationDecision {
                request_id: req.id.clone(),
                outcome: DecisionOutcome::Timeout,
                by: "gate:timeout".into(),
                basis: format!(
                    "the ask channel closed without an answer (asked {})",
                    self.who
                ),
                latency_ms: started.elapsed().as_millis() as u64,
            },
            Ok(_) => {
                let (chosen, basis) = match line.trim().split_once(char::is_whitespace) {
                    Some((id, rest)) => (id.trim().to_string(), rest.trim().to_string()),
                    None => (line.trim().to_string(), String::new()),
                };
                let basis = if basis.is_empty() {
                    format!("{} chose `{chosen}` at the console", self.who)
                } else {
                    basis
                };
                if req.option(&chosen).is_none() {
                    return AdjudicationDecision {
                        request_id: req.id.clone(),
                        outcome: DecisionOutcome::Unavailable,
                        by: "gate:unavailable".into(),
                        basis: format!(
                            "{} chose `{chosen}`, which is not one of the options this request offered",
                            self.who
                        ),
                        latency_ms: started.elapsed().as_millis() as u64,
                    };
                }
                AdjudicationDecision {
                    request_id: req.id.clone(),
                    outcome: DecisionOutcome::Selected { option_id: chosen },
                    by: format!("human:{}", self.who),
                    basis,
                    latency_ms: started.elapsed().as_millis() as u64,
                }
            }
            Err(e) => AdjudicationDecision::unavailable(
                req,
                "gate:unavailable",
                &format!("the ask channel failed: {e} (asked {})", self.who),
            ),
        }
    }

    fn describe(&self) -> String {
        format!("console ask, answered by {}", self.who)
    }
}

/// The closure a session loop installs to supply the authorisation trail. A named alias
/// because the inline type is unreadable, and the seam
/// [`AdjudicatedGate::with_trail_source`] needs.
pub type TrailSource = dyn Fn(&GateCall<'_>) -> crate::authorise::AuthorisationTrail + Send + Sync;

/// One row of the audit: every request, answer and timeout is an immutable row, log-only and
/// never in the model's transcript. Deliberately unreachable from
/// [`crate::result::ToolResult`]: the model sees the outcome, not the deliberation.
#[derive(Debug, Clone, PartialEq)]
pub struct AdjudicationRow {
    pub request: AdjudicationRequest,
    pub decision: AdjudicationDecision,
    /// What the gate did with the decision, in one word: `admit` or `refuse`.
    pub effect: &'static str,
    /// What the operator decided afterwards, if they said anything. A separate field that
    /// never overwrites `decision`, because the disagreement is the training signal; a row
    /// keeping only the final state has thrown the label away.
    pub operator: Option<crate::authorise::OperatorOverride>,
    /// The bytes an oracle was actually shown - the brief - when one was consulted, verbatim
    /// rather than reconstructed. Holds the card when a person was asked and no oracle was,
    /// so a new row is the brief or nothing.
    pub shown: Option<String>,
    /// What the oracle answered, verbatim. `None` when none was consulted or the one that was
    /// did not answer; the parsed reading is `verdict_basis`, a rendering of these bytes.
    pub reply: Option<String>,
    /// What a model said about this call before the person answered, at a
    /// supervised point. `None` everywhere else, and never the decider's own answer
    /// under another name.
    pub advice: Option<ModelAdvice>,
    /// The circuit breaker's key for this action - see
    /// [`crate::authorise::TaskDirection`]. Carried so an operator lifting a refusal
    /// they were shown does not have to reconstruct which direction it was in.
    pub direction: String,
    /// The named point in mode-space at the moment of the decision, not the
    /// gate's mode when the row is read, which a later `/mode` would have changed.
    pub mode: String,
    /// Whether a human was put in front of this decision as it was taken. Distinct
    /// from `operator`, which is what they said *afterwards* and is `None` until
    /// they do.
    pub asked: bool,
}

impl AdjudicationRow {
    /// The row as one fine-tuning example.
    pub fn corpus(&self) -> crate::authorise::CorpusRow {
        crate::authorise::CorpusRow {
            request_id: self.request.id.clone(),
            session_id: self.request.session_id.clone(),
            turn_id: self.request.turn_id.clone(),
            action: self.request.summary.clone(),
            trail: self.request.trail.clone(),
            shown: self.shown.clone(),
            reply: self.reply.clone(),
            shape: self.request.shape.clone(),
            shape_class: self
                .request
                .shape
                .as_ref()
                .map(|_| self.request.class.to_string()),
            baseline: self.request.baseline.clone(),
            tier: self.request.tier.as_str(),
            tool: self.request.tool.clone(),
            arguments: self.request.arguments.clone(),
            mode: self.mode.clone(),
            options: self.request.options.iter().map(|o| o.id.clone()).collect(),
            agent: self.request.agent.clone(),
            // The model's verdict, never the human's wearing its name. At `supervised` the
            // person decides and the oracle advises, and writing the person's answer here
            // would give a corpus where the two never disagree - every row worthless.
            model_verdict: match &self.advice {
                Some(a) => Some(format!("{} by {}: {}", a.would, a.by, a.basis)),
                None => Some(format!(
                    "{} by {}: {}",
                    self.decision.outcome.as_str(),
                    self.decision.by,
                    self.decision.basis
                )),
            },
            // What actually settled the call, always - advice or no advice.
            verdict: Some(self.decision.outcome.as_str().to_string()),
            verdict_by: Some(self.decision.by.clone()),
            verdict_basis: Some(self.decision.basis.clone()),
            // Not yet produced by any oracle - see `CorpusRow::p_allow`. `None`
            // here is *no confidence was reported*, and must never be read as
            // low confidence.
            p_allow: None,
            decision_ms: self.decision.latency_ms,
            brief_format: BRIEF_FORMAT,
            // The flag, not an inference. `advice` is `None` for an adjudicator that consults
            // nothing, and `consulted: false` inside a `Some` is a model saying it
            // short-circuited: both mean no oracle spoke.
            consulted: self.advice.as_ref().is_some_and(|a| a.consulted),
            // The reading, not a reading of the prose. `None` on every answered
            // verdict, and on every row where the advice is a short-circuit rather than a
            // model's answer: a label there would name a reply nobody made.
            oracle_reading: self
                .advice
                .as_ref()
                .filter(|a| a.consulted)
                .and_then(|a| a.unsure)
                .map(UnsureKind::as_str),
            effect: self.effect,
            asked: self.asked,
            operator: self.operator.clone(),
        }
    }
}

/// Did the person agree with the model, in the corpus's own three words: `Granted` is "it
/// would not have run and I let it", `Revoked` is "it would have run and I stopped it",
/// `Upheld` is "we agree". `ask` and `unavailable` are both not an admission.
fn agreement(
    advice: &ModelAdvice,
    effect: &'static str,
    note: String,
) -> crate::authorise::OperatorOverride {
    use crate::authorise::OperatorOverride;
    let model_would_admit = advice.would == "admit";
    let gate_admitted = effect == "admit";
    match (model_would_admit, gate_admitted) {
        (true, true) | (false, false) => OperatorOverride::Upheld { note },
        // The model found no authorisation and the operator admitted anyway. The
        // single most valuable row in the corpus: an over-refusal, caught.
        (false, true) => OperatorOverride::Granted { note },
        // The model authorised it and the operator said no.
        (true, false) => OperatorOverride::Revoked { note },
    }
}

/// The `#N` sequence at the end of a turn id, if it has one: `rsplit` because a transcript id
/// may itself contain `#`, and a tail that will not parse is an honest `None` rather than an
/// invented age.
fn turn_seq(turn_id: &str) -> Option<u64> {
    turn_id.rsplit('#').next()?.parse().ok()
}

/// The sentinel key for a call with no program - which is not a label. It matches nothing a
/// command produces, so a grant over `bash` cannot silently cover a `write`, and rendering it
/// into an option would put a placeholder where a noun belongs.
pub const NO_PROGRAM: &str = "<tool>";

/// What a grant is keyed on: the program doing the work, as the shell will run it. It reads
/// [`crate::intent::Baseline::grant_program`], chosen where every stage is read, so a grant
/// names the program that works rather than `export` or `head`. [`NO_PROGRAM`] with no command.
fn grant_program(baseline: &crate::intent::Baseline) -> String {
    baseline
        .grant_program
        .clone()
        .unwrap_or_else(|| NO_PROGRAM.to_string())
}

/// The refuse-list, evaluated before any adjudicator and overridable by none of them: a
/// `deny` row is checked before an adjudicator is consulted, so no listener can bypass it.
/// `.git/` is ours, not the clause's: rewriting a ref is not something a tool can undo.
pub const NEVER_WRITE: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".aws",
    ".kube",
    ".config/gh",
    ".password-store",
    ".mozilla",
    ".config/google-chrome",
    ".config/chromium",
    ".git",
    // The clause said "or a browser profile", and three of them is not the
    // clause. Same reasoning as the read-side list in `intent::SECRET_DIRS`,
    // which these are kept in step with.
    ".config/BraveSoftware",
    ".config/microsoft-edge",
    ".config/vivaldi",
    ".config/opera",
    ".thunderbird",
    "Library/Application Support/Google/Chrome",
    "Library/Application Support/Firefox",
    "AppData/Local/Google/Chrome",
    "AppData/Roaming/Mozilla",
    ".local/share/keyrings",
    ".gnome2/keyrings",
    ".config/gcloud",
    ".azure",
    ".config/op",
    ".config/Bitwarden",
];

/// Where an *Always allow* answer is written down; see
/// [`AdjudicatedGate::with_permission_sink`].
pub type PermissionSink =
    std::sync::Arc<dyn Fn(&crate::permission::Rule) -> Result<(), String> + Send + Sync>;

/// The gate that consults an adjudicator.
///
/// Constructed with one; the default one is [`NoAdjudicator`] and it refuses.
pub struct AdjudicatedGate {
    adjudicator: Box<dyn Adjudicator>,
    session_id: String,
    agent: String,
    /// Where this session sits in mode-space, as a named point. `ALWAYS_ASK` by default,
    /// which is what an unseen project gets; the daemon overrides it per project root.
    mode: crate::mode::Mode,
    /// Standing permissions from an `allow_session` answer, keyed on
    /// `(program, ActionClass, intents)` rather than `tool + class`: the program is what a
    /// person means by "stop asking me about git", and the intent set is what a vehicle changes.
    grants: Vec<crate::grant::Grant>,
    /// The memory the operator let this session have without asking: `Some(Some(bytes))` up
    /// to that size, `Some(None)` uncapped, `None` nothing granted. Cleared with the other
    /// session grants when the mode changes.
    memory_session: Option<Option<u64>>,
    /// Shapes the operator approved themselves, with the class they approved them at. Seeded
    /// from their past approvals under this workspace and outliving the process on purpose: a
    /// session restarted after a fault should not be asked about every shape again.
    approved_shapes: std::collections::HashMap<String, ActionClass>,
    /// What the agent says it is doing right now, supplied by the layer that has the
    /// transcript. A closure for the reason the trail source is one: this type must not learn
    /// to read a conversation.
    agent_claim: Box<dyn Fn() -> Option<String> + Send + Sync>,
    /// The operator's `permission` config plus the `always` approvals, evaluated before the
    /// mode: `allow` admits, `deny` refuses, `ask` falls through to the mode and the
    /// adjudicator.
    permission: crate::permission::Ruleset,
    /// Where an *Always allow* answer is written down so it outlives the process: the daemon
    /// hands in `~/.config/letibot/permission.json`. Without one the answer holds for the
    /// session and the row says so.
    permission_sink: Option<PermissionSink>,
    /// The boundary this session's commands run inside, read off the backend at open. `None` is
    /// *no boundary*, and `boundary_refusal` says whether that is a choice or a host that could
    /// not confine. A verdict reads it - [`AdjudicatedGate::runs_project_code_unconfined`].
    boundary: Option<crate::exec::Boundary>,
    boundary_refusal: Option<String>,
    /// Whether exec follows the mode. Off by default: no point admits an exec-class call
    /// unasked, because `bash`'s result is an arbitrary byte stream. leticode turns it on, so
    /// `allow-all` admits exec and every other point is unchanged.
    exec_follows_mode: bool,
    /// The rows, in memory: the durable journal is `letibot-sessionlog`'s, and wiring this into
    /// it is not done here.
    pub log: Vec<AdjudicationRow>,
    /// A monotonic counter so two requests in one turn have different ids. Not a ULID: this
    /// crate has no clock and no ULID dependency, and an id unique within a session is what the
    /// log and the refusal text need.
    seq: u64,
    /// What layer A needs to know about where it is standing. The default's
    /// [`crate::intent::ShellTrust`] is `Unknown`, so a gate nobody configured refuses bare
    /// command names, which is the fail-closed direction.
    surroundings: crate::intent::Surroundings,
    /// Where the authorisation trail comes from. See
    /// [`crate::authorise::AuthorisationTrail`] for what must install it.
    trail_source: Option<Box<TrailSource>>,
    /// Where denials go so the operator sees them when they happen.
    denials: Option<Box<dyn crate::authorise::DenialSink>>,
    /// The consecutive-denial circuit breaker.
    pub breaker: crate::authorise::Breaker,
    /// The bytes an oracle was shown for the call in flight, moved into the row
    /// by [`AdjudicatedGate::record`].
    shown: Option<String>,
    /// What that oracle answered, from the same adjudicator and the same call as `shown`: the
    /// reply is the input a corpus row's label was read out of, and a row whose verdict cannot be
    /// traced back to its bytes is a row nobody can check.
    reply: Option<String>,
    /// What a model said about the call in flight, at a supervised point. Taken from
    /// [`Adjudicator::last_advice`] beside `shown` and moved into the row the same
    /// way.
    advice: Option<ModelAdvice>,
    /// Set when the call in flight was settled by a standing grant rather than by
    /// somebody answering now: the grant's own reason. Moved into the row's operator
    /// note, so a label that came from a ruling made turns ago says which ruling.
    standing: Option<String>,
    /// The guard model, when one is configured. Held whether or not supervision
    /// is on, so turning it on is a flag and not a rebuild.
    advisor: Option<std::sync::Arc<dyn Adjudicator>>,
    /// Whether [`AdjudicatedGate::advisor`] gets a turn on every call. A field rather than a
    /// mode, so `/supervise` moves it mid-session: the mode is fixed when a session opens
    /// because the tools seated under it are, and this changes nothing about that.
    supervise: bool,
    /// Where corpus rows go so they outlive this process. `None` means the corpus is
    /// being discarded, and the disclosure says so rather than implying it is kept.
    corpus_sink: Option<std::sync::Arc<dyn crate::authorise::CorpusSink>>,
    /// Set for the one call that is adjudicated and may not ask - [`Gate::admit_unasked`],
    /// which is how a look a plugin layer declared meets this gate. The ladder runs and the ask
    /// does not: a step that would put the call to somebody answers *would need your approval*.
    unasked: bool,
    /// Where an admission nobody was asked about goes, so the operator sees it happened. The
    /// opposite channel to `denials`, and not for symmetry: a mode change that stops asking has
    /// to be visible while the work happens, or the only evidence is that the cards stopped.
    admissions: Option<Box<dyn AdmissionSink>>,
}

/// A call the gate admitted without asking anybody, and why. Deliberately not a denial with a
/// different sign: a denial leaves the model's call unanswered and the operator is owed a
/// remedy, while an admission happened, and what they are owed is to be told what happened.
pub struct AdmissionNotice {
    pub request_id: String,
    pub turn_id: String,
    pub tool: String,
    /// The one argument a person reads: a command line, a path.
    pub summary: String,
    /// Why nobody was asked, in the words the audit row carries.
    pub basis: String,
    /// The point the session sits at (`writes allowed`), so a reader can tell an admission
    /// this rule earned from one `allow-all` made unconditional.
    pub mode: String,
    /// What measured the boundary: `bubblewrap 0.11.1` here, a VM's mechanism there.
    pub boundary: String,
}

/// The gate's other exit, as a seam. `letibot-tools` holds no hub, no store and no
/// conversation, so a caller that has one supplies this, exactly as it supplies
/// [`crate::authorise::DenialSink`] for the channel in the other direction.
pub trait AdmissionSink: Send + Sync {
    fn admitted(&self, notice: &AdmissionNotice);
}

/// The rule, split where a corpus can see the halves: [`admits_inside_the_boundary`] is what the
/// gate calls, and this is the same rule with the boundary's half left out, for the replay's
/// instrument. One implementation, two callers, so a replay cannot restate the condition.
pub fn a_command_that_crosses_nothing(
    mode: crate::mode::Mode,
    access: Access,
    baseline: &crate::intent::Baseline,
) -> bool {
    access == Access::Exec
        && mode == crate::mode::Mode::WRITES_ALLOWED
        // The always-ask list survives the boundary, and as a positive match rather than as
        // *not always-ask* so a fourth tier is a compile error here: `sudo -n true` and
        // `shutdown now` carry no network intent and no region, and must still ask.
        && matches!(baseline.tier, Tier::Auto | Tier::MayApprove)
        && !baseline.intents.contains(&crate::intent::Intent::Network)
        && !baseline.regions.iter().any(|r| r.is_outside())
}

/// The whole rule: the call crosses nothing and the boundary it runs inside was measured and
/// holds - see [`crate::exec::Boundary::encloses`].
pub fn admits_inside_the_boundary(
    boundary: Option<&crate::exec::Boundary>,
    mode: crate::mode::Mode,
    access: Access,
    baseline: &crate::intent::Baseline,
) -> bool {
    a_command_that_crosses_nothing(mode, access, baseline) && boundary.is_some_and(|b| b.encloses())
}

impl AdjudicatedGate {
    pub fn new(adjudicator: Box<dyn Adjudicator>) -> Self {
        AdjudicatedGate {
            adjudicator,
            session_id: "session".into(),
            agent: "agent".into(),
            mode: crate::mode::UNSEEN_PROJECT,
            grants: Vec::new(),
            memory_session: None,
            approved_shapes: std::collections::HashMap::new(),
            agent_claim: Box::new(|| None),
            permission: crate::permission::Ruleset::new(),
            permission_sink: None,
            exec_follows_mode: false,
            boundary: None,
            boundary_refusal: None,
            log: Vec::new(),
            seq: 0,
            surroundings: crate::intent::Surroundings::default(),
            trail_source: None,
            denials: None,
            breaker: crate::authorise::Breaker::default(),
            shown: None,
            reply: None,
            advice: None,
            standing: None,
            advisor: None,
            supervise: false,
            corpus_sink: None,
            unasked: false,
            admissions: None,
        }
    }

    /// opencode parity: make exec follow the mode instead of always asking. See
    /// [`AdjudicatedGate::exec_follows_mode`]. Off by default; leticode turns it on.
    pub fn with_exec_follows_mode(mut self, v: bool) -> Self {
        self.exec_follows_mode = v;
        self
    }

    /// Where this session's commands actually run: the boundary that was measured, or `None` for a
    /// host that has none. **The one fact the gate may read about the platform**, and it is handed
    /// in rather than probed, so the Mac's case (`confine::probe` answers nothing there) is a Linux
    /// test's to construct. `refusal` is the words the probe refused in, which a card repeats.
    pub fn with_boundary(
        mut self,
        boundary: Option<crate::exec::Boundary>,
        refusal: Option<String>,
    ) -> Self {
        self.boundary = boundary;
        self.boundary_refusal = refusal;
        self
    }

    /// Where an admission nobody was asked about is told, so the operator sees it happened. See
    /// [`AdmissionSink`], and `denials` for the channel in the other direction.
    pub fn with_admission_sink(mut self, sink: Box<dyn AdmissionSink>) -> Self {
        self.admissions = Some(sink);
        self
    }

    /// Put this session at a named point. The gate does not check the point's prerequisites:
    /// the caller does, before it builds anything, because a check here would run after a
    /// session had already opened, printed a banner and seated tools.
    pub fn with_mode(mut self, mode: crate::mode::Mode) -> Self {
        self.mode = mode;
        self
    }

    /// Install opencode's permission ruleset (the operator's `permission` config), evaluated
    /// before the mode. Later rules override earlier ones, so a config here is consulted before
    /// any `always` approvals the caller appends.
    pub fn with_permission(mut self, rules: crate::permission::Ruleset) -> Self {
        self.permission = rules;
        self
    }

    /// Where *Always allow* writes its rule. See the field.
    pub fn with_permission_sink(mut self, sink: PermissionSink) -> Self {
        self.permission_sink = Some(sink);
        self
    }

    /// The rules in force, for a disclosure.
    pub fn permission_rules(&self) -> &crate::permission::Ruleset {
        &self.permission
    }

    /// Which point this gate is at, for the disclosure.
    pub fn mode(&self) -> crate::mode::Mode {
        self.mode
    }

    /// opencode's permission ruleset in force: the config plus any `always` approvals. A
    /// subagent inherits this, so the same allow/deny/ask rules govern its calls rather than
    /// starting from scratch.
    pub fn permission(&self) -> &crate::permission::Ruleset {
        &self.permission
    }

    /// Warm-start the shape cache from approvals the operator already gave. A pair whose class
    /// does not parse is dropped rather than guessed at, so the next call of that shape asks.
    /// The caller owes the bounds; the lookup re-checks what it can, so a seed is never a licence.
    pub fn seed_shapes(&mut self, seen: impl IntoIterator<Item = (String, String)>) -> usize {
        let mut added = 0;
        for (shape, class) in seen {
            let Some(class) = ActionClass::parse(&class) else {
                continue;
            };
            if let std::collections::hash_map::Entry::Vacant(e) = self.approved_shapes.entry(shape)
            {
                e.insert(class);
                added += 1;
            }
        }
        added
    }

    pub fn grants(&self) -> &[crate::grant::Grant] {
        &self.grants
    }

    /// Tell layer A where it is standing, including whether the shell that will run a command
    /// is fixed. See [`crate::intent::ShellTrust`]: undeclared means bare command names are
    /// unresolved.
    pub fn with_surroundings(mut self, s: crate::intent::Surroundings) -> Self {
        self.surroundings = s;
        self
    }

    /// Install the authorisation trail source: the seam this crate cannot fill itself, since the
    /// tool runtime sees one call at a time and has no transcript. Until a session loop installs
    /// one the trail is `NotCollected`, and a model adjudicator refuses rather than decide blind.
    pub fn with_trail_source(
        mut self,
        f: impl Fn(&GateCall<'_>) -> crate::authorise::AuthorisationTrail + Send + Sync + 'static,
    ) -> Self {
        self.trail_source = Some(Box::new(f));
        self
    }

    /// Where "what the agent says it is doing" comes from. Installed by the harness, the only
    /// layer holding the transcript, and `None` by default so a gate nobody wired says nothing
    /// rather than inventing a claim.
    pub fn with_agent_claim(
        mut self,
        f: impl Fn() -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        self.agent_claim = Box::new(f);
        self
    }

    /// Install the guard model. Held, not switched on: `/supervise` does that, and it can only
    /// do it if this was installed when the session opened.
    pub fn with_advisor(mut self, advisor: std::sync::Arc<dyn Adjudicator>) -> Self {
        self.advisor = Some(advisor);
        self
    }

    /// Start supervised. `--supervise` on the command line; `/supervise` changes it later.
    /// Named apart from [`Gate::supervising`], which asks the question this answers: a builder
    /// and a getter sharing one name is how a caller reads the wrong one.
    pub fn start_supervised(mut self, on: bool) -> Self {
        self.supervise = on;
        self
    }

    /// Install the corpus sink. Without one every decision this gate makes is
    /// kept in `log` and lost when the process ends.
    pub fn with_corpus_sink(
        mut self,
        sink: std::sync::Arc<dyn crate::authorise::CorpusSink>,
    ) -> Self {
        // Seed the request counter from what the sink already holds: `next_id` runs off an
        // in-memory `seq` and a resumed session keeps its id, so every restart re-emitted ids
        // the table had, and `INSERT OR IGNORE` dropped them silently.
        self.seq = self.seq.max(sink.decisions_recorded(&self.session_id));
        self.corpus_sink = Some(sink);
        self
    }

    /// Whether decisions are being written anywhere durable. For the startup
    /// disclosure, which must not imply a corpus that is not being kept.
    pub fn corpus_is_kept(&self) -> bool {
        self.corpus_sink.is_some()
    }

    /// Install the denial sink. Every refusal is delivered to it at the moment it
    /// is decided, so the operator is never left inferring a decision from a task
    /// that stopped.
    pub fn with_denial_sink(mut self, sink: Box<dyn crate::authorise::DenialSink>) -> Self {
        self.denials = Some(sink);
        self
    }

    /// Emit the notice, and produce the prose the model is given. One function so the two
    /// cannot drift: the sentence the operator sees and the sentence the model sees are built
    /// from one [`crate::authorise::DenialNotice`].
    fn surface(
        &self,
        req: &AdjudicationRequest,
        decision: &AdjudicationDecision,
        outcome: &'static str,
        repeat: crate::authorise::BreakerState,
    ) -> String {
        let notice = crate::authorise::DenialNotice {
            request_id: req.id.clone(),
            session_id: req.session_id.clone(),
            turn_id: req.turn_id.clone(),
            call_id: req.call_id.clone(),
            tool: req.tool.clone(),
            summary: req.summary.clone(),
            baseline: req.baseline.clone(),
            by: decision.by.clone(),
            basis: decision.basis.clone(),
            tier: req.tier.as_str(),
            outcome,
            repeat,
            // Offer the remedy that exists. A refusal the HARNESS made is not one a grant can
            // lift - no adjudicator was consulted and none would be on a retry - so what helps
            // is re-issuing the command in a form the grammar can read.
            grant: if decision.by == "boundary:normaliser" {
                "Nothing was executed and nothing changed. No grant applies: the \
                 refusal is before any adjudicator and a standing permission is \
                 tested after it, so granting this changes nothing on a retry. What \
                 lifts it is re-issuing the command in a form that resolves — the \
                 reason above names each construct."
                    .to_string()
            } else if decision.by.starts_with("boundary:") {
                "Nothing was executed and nothing changed. No grant applies and no \
                 rewording does either: this is refused by the harness itself, \
                 before any adjudicator and overridable by none."
                    .to_string()
            } else if decision.by.starts_with("model") {
                // The guard decided this one, and there is no pending decision: an oracle's
                // refusal is a decision in its own right, so no card was raised and the remedy
                // that exists is the operator's grant, since the row is on `/gate`.
                format!(
                    "the guard refused this one — no card was raised for it, and nothing is \
                     waiting for an answer. Nothing was executed and nothing changed. The \
                     row is on `/gate`, where the operator can lift it: `grant {}` ({}).",
                    req.id, req.tool
                )
            } else {
                format!(
                    "grant `{}` ({}) for this session. Nothing was executed and nothing \
                     changed.",
                    req.id, req.tool
                )
            },
        };
        // What the sink says it reached is what the model is told: the sentence about the
        // operator knowing is a claim about a screen, and only the sink holds that fact. A
        // person who decided HAS seen it, and a console operator is no head at all.
        let by_a_person = decision.by.starts_with("human:");
        let mut reached = match &self.denials {
            Some(sink) => sink.denied(&notice),
            None => crate::authorise::Reach::Nobody,
        };
        if by_a_person {
            reached = crate::authorise::Reach::OnScreen;
        }
        crate::authorise::refusal_text(&notice, reached)
    }

    /// The fail-closed gate, named so that constructing one reads as a decision.
    pub fn closed() -> Self {
        Self::new(Box::new(NoAdjudicator))
    }

    pub fn with_identity(
        mut self,
        session_id: impl Into<String>,
        agent: impl Into<String>,
    ) -> Self {
        self.session_id = session_id.into();
        self.agent = agent.into();
        self
    }

    /// What the daemon should print at startup: an operator who cannot see which adjudicator is
    /// attached cannot see that none is.
    pub fn describe(&self) -> String {
        self.adjudicator.describe()
    }

    /// Whether denials reach the operator. Feeds [`startup_disclosure_with_surfacing`].
    pub fn surfaces_denials(&self) -> bool {
        self.denials.is_some()
    }

    fn next_id(&mut self) -> String {
        self.seq += 1;
        format!("adj-{}-{:04}", self.session_id, self.seq)
    }

    /// The access this call is JUDGED at. A shell call is `Exec` because the tool that carries it
    /// is, whatever the program inside does, so this narrows to `Read` or `Write` from layer A's
    /// reading. Narrowing only, and only when the tool declared `Exec`; the gate is still asked.
    fn judged_access(call: &GateCall<'_>, baseline: &crate::intent::Baseline) -> Access {
        if call.access == Access::Exec && baseline.reads_only() {
            Access::Read
        } else if call.access == Access::Exec && baseline.writes_only() {
            // The class the edit path already knows how to judge; `path_is_inside` is asked of
            // the write targets, and the tier was settled by layer A before this.
            Access::Write
        } else {
            call.access
        }
    }

    /// The whole of what this gate knows about the platform, and it was handed in at open rather
    /// than probed here: a Mac's host shell answers `None` at `confine::probe`, so a test on a
    /// Linux box constructs that case by handing this gate the same `None`.
    fn has_measured_boundary(&self) -> bool {
        self.boundary.is_some()
    }

    /// Does this call run the project's own code on a host with no boundary, and what does it run?
    /// One function because a card that offers a grant the matcher refuses is the defect `Offer`
    /// exists to prevent. An unconfined host asks every time: the file changes after the answer.
    fn runs_project_code_unconfined(
        &self,
        access: Access,
        baseline: &crate::intent::Baseline,
    ) -> Option<String> {
        if access != Access::Exec || self.has_measured_boundary() {
            return None;
        }
        crate::project_code::runs_project_code(baseline)
    }

    /// What the ladder may offer for this call - one computation for the rungs and for the note
    /// that explains an absent one. Both halves come from mechanisms that already exist rather
    /// than from a restatement of their rules.
    fn offer_for(
        &self,
        call: &GateCall<'_>,
        baseline: &crate::intent::Baseline,
        class: ActionClass,
    ) -> Offer {
        let program = grant_program(baseline);
        let derived = derived_pattern(call, baseline, &self.permission);
        // `Err` is *a grant taken now would cover nothing*, and its own vocabulary says which
        // of the three refusals it was, in the short register a card has room for.
        let refused = Self::session_grant(baseline, class, &program, String::new()).err();
        let note = refused.as_ref().map(|why| {
            format!(
                "not offered for the session: {}{}",
                why.short(),
                unresolved_names(baseline)
            )
        });
        // On a host with no boundary a call that runs the project's code offers neither standing
        // rung: the durable rule outlives the session, and the file it approves is not the file
        // that will run, so the next run is a different program.
        if let Some(runs) =
            self.runs_project_code_unconfined(Self::judged_access(call, baseline), baseline)
        {
            return Offer {
                derived: None,
                program,
                session: false,
                note: Some(format!(
                    "asks every time: it runs code the agent wrote ({runs}), and this host has \
                     no boundary — a grant cannot stand in for the file that will exist by the \
                     next run"
                )),
            };
        }
        Offer {
            derived,
            program,
            session: refused.is_none(),
            note,
        }
    }

    /// The grant an `allow_session` answer over this call would record, or why one taken now
    /// could cover nothing. Both the card and the recording site read this one answer, so the
    /// card asks the matcher rather than offering a rung on a rule of its own.
    fn session_grant(
        baseline: &crate::intent::Baseline,
        class: ActionClass,
        program: &str,
        why: String,
    ) -> Result<crate::grant::Grant, crate::grant::NotCovered> {
        let grant = crate::grant::Grant {
            written: crate::grant::Written::Enumerated {
                patterns: vec![format!("{program} ({class})")],
            },
            coverage: vec![crate::grant::Coverage {
                program: program.to_string(),
                class,
                intents: baseline.intents.clone(),
            }],
            why,
        };
        grant.covers(
            &baseline.tier,
            baseline.resolved,
            program,
            class,
            &baseline.intents,
        )?;
        Ok(grant)
    }

    /// Layer A's deterministic reading of this call, before any adjudicator. A `command`
    /// argument is normalised through the grammar; anything else is read as path arguments, so
    /// the flow rule applies to `read` exactly as it applies to `cat`.
    pub fn baseline_for(&self, call: &GateCall<'_>) -> crate::intent::Baseline {
        if let Some(cmd) = call.args.get("command").and_then(|v| v.as_str()) {
            // The scripts this command will run travel with it, because the runtime already
            // read them through the backend for the brief. Handing layer A the same bytes is
            // what stops the classifier and the oracle disagreeing about one file.
            return crate::intent::Baseline::of_command_with(cmd, &self.surroundings, call.scripts);
        }
        let paths: Vec<&str> = call
            .args
            .as_object()
            .map(|o| {
                o.iter()
                    .filter(|(k, _)| {
                        matches!(
                            k.as_str(),
                            "path" | "paths" | "file" | "src" | "dest" | "to" | "from"
                        )
                    })
                    .filter_map(|(_, v)| v.as_str())
                    .collect()
            })
            .unwrap_or_default();
        crate::intent::Baseline::of_paths(
            paths,
            call.access == Access::Write,
            // A read tool returns the bytes it read, which is the transcript edge.
            call.access == Access::Read,
            &self.surroundings,
        )
    }

    /// The request one call produces. Public because the head and the tests both want to see a
    /// request without a decision having been made about it.
    pub fn request_for(&mut self, call: &GateCall<'_>) -> AdjudicationRequest {
        let baseline = self.baseline_for(call);
        self.request_from(call, &baseline)
    }

    fn request_from(
        &mut self,
        call: &GateCall<'_>,
        baseline: &crate::intent::Baseline,
    ) -> AdjudicationRequest {
        let network = call.access == Access::Network;
        // What this call is about, for a human deciding in one line. A path for a file tool;
        // for a network tool there is none, and naming the absence would describe the request
        // by what it is not.
        let target = target_of(call.args);
        let inside = call.path_is_inside();
        // Reversibility is a fact about the target, and the runtime stats it before asking.
        // `None` - no path argument at all - counts as reversible: claiming otherwise would be
        // a stronger statement than the evidence supports.
        let creates = call.target_exists == Some(false);
        // The class is the work's, not the vehicle's. `baseline` is the same reading the facts
        // and the intents come from, so the class, the headline and the intents cannot disagree
        // about what this call is.
        let access = Self::judged_access(call, baseline);
        let class = if network {
            ActionClass::external(access, Cost::Free)
        } else {
            ActionClass::host(access, inside, creates)
        };
        let digest = crate::events::payload_digest(&call.args.to_string());
        let mut facts = if network {
            vec![
                format!(
                    "workspace: {} — and this call does not touch it",
                    call.workspace
                ),
                "this call LEAVES THE BOX: it reaches a third party, who learns that \
                 somebody here asked"
                    .to_string(),
                "a request that has been sent cannot be recalled, which is why the class \
                 says irreversible even for a read"
                    .to_string(),
                "what comes back is text this harness did not write, and it enters the \
                 model's context"
                    .to_string(),
            ]
        } else {
            let mut f = vec![format!("workspace: {}", call.workspace)];
            // A call with no path argument is judged on what it DOES. `path_is_inside` answers
            // `true` when the key is absent, so printing the fact unconditionally asserted that
            // a path nobody named was inside the workspace. The guard is its own first line.
            let names_a_path = call
                .args
                .get("path")
                .and_then(|v| v.as_str())
                .is_some_and(|p| !p.trim().is_empty());
            if names_a_path {
                f.push(format!(
                    "the path is {} the session's workspace",
                    if inside { "inside" } else { "OUTSIDE" }
                ));
            } else {
                f.push(
                    "this call names no path, so it is judged on what it DOES rather \
                     than on where it points"
                        .to_string(),
                );
            }
            // The fact about the boundary is the measured one: a boundary that bounds this
            // call, one that was wanted and could not be built (with its reason), or no
            // question asked at all.
            f.push(match (&self.boundary, &self.boundary_refusal) {
                (Some(b), _) => format!(
                    "this call runs INSIDE a measured boundary on the host that owns these \
                     files: {}. A path outside that view is ABSENT from it — not denied, not \
                     there — so what the command cannot see, it cannot leak either{}",
                    b.view.summary().replace('\n', "; "),
                    if b.egress.allowed() {
                        "; the network namespace is the host's, by the session's own rule"
                    } else {
                        "; it has no network at all"
                    }
                ),
                (None, Some(why)) => format!(
                    "the host filesystem is NOT sandboxed for this call, and confinement was \
                     asked for and refused — {why}"
                ),
                (None, None) => {
                    "the host filesystem is not sandboxed; a confinement boundary arrives with \
                     firecode"
                        .to_string()
                }
            });
            f
        };
        if creates && !network {
            facts.push(
                "this creates a file that does not exist, so there is nothing to restore".into(),
            );
        }
        if let Some(op) = call.args.get("op").and_then(|v| v.as_str()) {
            // A tool that dispatches on an op is one tool to the gate and ten actions to
            // whoever decides. The op goes in the facts because the class cannot carry it, and
            // a person reading `github` alone cannot tell a listing from a merge.
            facts.push(format!("operation: `{op}`"));
        }
        // The key the row will be recorded under, computed from the parts this function has,
        // so the history in the brief and the history the next call reads back are keyed the
        // same. The rows are read here, at decision time.
        let direction = crate::authorise::TaskDirection::of_parts(call.name, baseline, class.scope);
        let prior = Self::prior_answers(&self.log, &direction.key(), turn_seq(call.turn_id));
        // What the ladder may offer, computed once for both halves of the card: the rungs and
        // the note that explains a missing one are the same fact, and computing it twice is two
        // chances for a card to say a rung was left off while offering it.
        let offer = self.offer_for(call, baseline, class);
        AdjudicationRequest {
            id: self.next_id(),
            session_id: self.session_id.clone(),
            turn_id: call.turn_id.to_string(),
            call_id: call.call_id.to_string(),
            agent: self.agent.clone(),
            tool: call.name.to_string(),
            class,
            prior,
            examples: Self::operator_examples(&self.log, turn_seq(call.turn_id)),
            brief_variant: crate::authorise::BriefVariant::AskedForIt,
            agent_claim: (self.agent_claim)(),
            reading: Some(baseline.clone()),
            shape: shape_of(baseline),
            // Filled by `SupervisedAdjudicator` between the model's answer and the
            // person's, and by nothing else. The gate does not consult a model on
            // its own.
            advice: None,
            // The headline names the access the call is JUDGED at, so a shell that
            // only reads says `read access` rather than `exec access` over a list of
            // intents that already said read.
            summary: format!(
                "`{}` wants {} access to `{target}`",
                call.name,
                access.as_str()
            ),
            target: target.clone(),
            arguments: call.args.clone(),
            arguments_digest: digest,
            boundary_facts: facts,
            scripts: call.scripts.to_vec(),
            kind: RequestKind::Permission,
            // Offer only what the gate will honour: the recording site requires a tier that is
            // not `AlwaysAsk` and a point that grants for the session, and every narrowing here
            // moves with them, or an operator is shown a button whose effect is dropped.
            options: {
                // One computation, and every rung reads it: `derived` is what *Always allow*
                // would write, `session` is whether a grant taken now could cover a later call,
                // and `note` says why whichever is missing is missing.
                if matches!(baseline.tier, Tier::AlwaysAsk { .. }) {
                    always_ask_options()
                } else if call.access == Access::Exec {
                    exec_options(&offer)
                } else if self.mode.grants == crate::mode::GrantScope::Session {
                    permission_options(&offer)
                } else {
                    once_only_options(self.mode.name, &offer)
                }
            },
            // `None` when nothing was left off, so a complete ladder says nothing rather than
            // saying "nothing".
            options_note: offer.note_line().map(str::to_string),
            // `Deny` for a permission, and `AgentDecides` only for a question. Nothing here is
            // a question yet.
            on_timeout: OnTimeout::Deny,
            // Layer A's, never the caller's.
            tier: baseline.tier.clone(),
            // Layer A's, never the caller's, and carried rather than read off the verdict:
            // an unresolvable construct is a widened scope rather than a `NotRun`. See
            // `AuthorisationRequest::resolved`.
            resolved: baseline.resolved,
            baseline: baseline.summary(),
            trail: match &self.trail_source {
                Some(f) => f(call),
                None => crate::authorise::AuthorisationTrail::default(),
            },
        }
    }

    /// A standing rule settled this call; ask the model anyway and mark the row. Every caller
    /// has already decided by the time this runs, so nothing read here changes the outcome - an
    /// oracle that disagrees is recorded disagreeing. Off at every point but `supervised`.
    fn advise_on_a_settled_call(&mut self, req: &AdjudicationRequest, why: String) {
        // An unasked call consults no model, even here: a look a plugin declared is polled on a
        // watch's clock, and an inference per firing on somebody's `exec` look is a cost nobody
        // chose.
        if self.unasked {
            return;
        }
        self.advice = self.ask_the_advisor(req);
        if self.advice.is_some() {
            self.standing = Some(why);
        }
    }

    /// The guard model's verdict on this call, or `None` when supervision is off or no advisor
    /// is installed. One function so "is the model consulted" has one answer, and a path that
    /// forgets to call it records no verdict rather than half-supervising.
    fn ask_the_advisor(&self, req: &AdjudicationRequest) -> Option<ModelAdvice> {
        if !self.supervise {
            return None;
        }
        let advisor = self.advisor.as_ref()?;
        // Its verdict, never its decision: `decide` is called for the answer and the
        // outcome is thrown away. Nothing downstream reads this to admit anything.
        let d = advisor.decide(req);
        advisor.last_advice().or(Some(ModelAdvice {
            // An advisor that does not report its own advice cannot say whether an
            // oracle was asked, and assuming one was is the assumption `consulted`
            // exists to refuse.
            consulted: false,
            would: "unavailable",
            by: d.by,
            basis: d.basis,
            cites: Vec::new(),
            latency_ms: d.latency_ms,
            // No oracle reported an advice, so there is no reading to label: `None` for the
            // same reason `consulted` is `false`, since a `kind` here would be a reading of a
            // reply that does not exist.
            unsure: None,
        }))
    }

    /// What the oracle was shown and what it answered, from whichever adjudicator actually
    /// consulted one: the advisor when it was put to this call, else the decider when that is
    /// itself a model, and nothing otherwise, because a card is not an oracle's input.
    fn file_exchange(&mut self) {
        let asked = self.advice.as_ref().is_some_and(|a| a.consulted);
        let decided = self.adjudicator.last_advice().is_some_and(|a| a.consulted);
        let source: Option<&dyn Adjudicator> = if asked {
            self.advisor.as_deref()
        } else if decided {
            Some(self.adjudicator.as_ref())
        } else {
            None
        };
        self.shown = source.and_then(|a| a.last_brief());
        self.reply = source.and_then(|a| a.last_reply());
    }

    fn record(
        &mut self,
        request: AdjudicationRequest,
        decision: AdjudicationDecision,
        effect: &'static str,
        direction: String,
    ) {
        let shown = self.shown.take();
        // The other half of the exchange, taken beside `shown` and from the same
        // adjudicator, so the two cannot describe different calls.
        let reply = self.reply.take();
        // An admitted contact makes the host seen: the always-ask rule is about an UNSEEN host,
        // and nothing called `saw_host`, so every contact was a first contact and the asks never
        // tapered. Recorded here, where every decision passes, and only for an admit.
        if effect == "admit"
            && let Some(b) = request.reading.as_ref()
        {
            for r in &b.regions {
                if let crate::intent::Region::Remote(h) = r {
                    self.surroundings.saw_host(h.clone());
                }
            }
        }
        // Asked is read off who decided, not off the adjudicator's name: a human adjudicator
        // that timed out asked nobody, and a model that escalated did. `by` records which.
        let asked = decision.by.starts_with("human");
        let mode = self.mode.name.to_string();
        let advice = self.advice.take();
        let standing = self.standing.take();
        // At `supervised` the label is already in hand, so it is written now: they just
        // answered this call with the model's verdict in front of them, and re-ruling it from a
        // list afterwards would ask the same question twice. No advice, no label.
        let operator = match (&advice, asked, &standing) {
            // No label without a verdict: `consulted` is false whenever a model adjudicator
            // answered without asking a model, and agreeing with a short-circuit is agreeing
            // with nothing.
            (Some(a), _, _) if !a.consulted => None,
            (Some(a), true, _) => Some(agreement(a, effect, String::new())),
            (Some(a), false, Some(why)) => Some(agreement(
                a,
                effect,
                format!("standing decision, not a fresh answer: {why}"),
            )),
            _ => None,
        };
        self.log.push(AdjudicationRow {
            request,
            decision,
            effect,
            operator,
            shown,
            reply,
            advice,
            direction,
            mode,
            asked,
        });
        // Write through immediately, not at shutdown: a daemon that is killed is the
        // ordinary end of a session, and a corpus flushed on a clean exit is a corpus
        // that keeps exactly the runs nothing went wrong in.
        if let Some(sink) = &self.corpus_sink {
            // `last` is the row just pushed; building a second one would be a second
            // chance to diverge from what the log holds.
            sink.decided(&self.log[self.log.len() - 1].corpus());
        }
    }

    /// The last few calls this operator answered themselves, newest first. Not filtered to the
    /// shape being decided - the point is what this person is like - and bounded because a brief
    /// is a prompt. Only rows a human answered: a mode's admit is not theirs.
    fn operator_examples(
        log: &[AdjudicationRow],
        now_turn: Option<u64>,
    ) -> Vec<crate::authorise::DecisionExample> {
        const MAX: usize = 6;
        let mut out: Vec<crate::authorise::DecisionExample> = Vec::new();
        for row in log.iter().rev() {
            if !row.decision.by.starts_with("human") {
                continue;
            }
            out.push(crate::authorise::DecisionExample {
                // What was decided, not how layer A read it: six examples of `request.baseline`
                // rendered as six copies of one sentence. Still not the raw arguments - the rule
                // that keeps a command out of the brief keeps it out of here.
                action: format!("{} {}", row.request.tool, row.request.target),
                verdict: if row.effect == "admit" {
                    "allowed"
                } else {
                    "refused"
                },
                turns_ago: match (now_turn, turn_seq(&row.request.turn_id)) {
                    (Some(n), Some(t)) => Some(n.saturating_sub(t)),
                    _ => None,
                },
            });
            if out.len() >= MAX {
                break;
            }
        }
        out
    }

    /// The audit rows read back: every prior row in the same task direction, aggregated by the
    /// gate's effect, oldest first. Shown to the decision and never substituted for it, and
    /// denials are history too - the filter is the direction, never the outcome.
    fn prior_answers(
        log: &[AdjudicationRow],
        key: &str,
        now_turn: Option<u64>,
    ) -> Vec<PriorAnswer> {
        let mut out: Vec<PriorAnswer> = Vec::new();
        // A HUMAN's answers, never the gate's own: an admit by the mode, the permission ruleset
        // or the oracle itself would come back as "admit - 3 time(s)" on the next call of that
        // shape, which is a ratchet. Only what a person decided is evidence.
        for row in log
            .iter()
            .filter(|r| r.direction == key && r.decision.by.starts_with("human"))
        {
            let ago = match (now_turn, turn_seq(&row.request.turn_id)) {
                (Some(n), Some(t)) => Some(n.saturating_sub(t)),
                _ => None,
            };
            match out.iter_mut().find(|p| p.effect == row.effect) {
                Some(p) => {
                    p.count += 1;
                    // Rows are appended in decision order, so the last one seen in
                    // this effect is the most recent.
                    p.latest_turns_ago = ago;
                }
                None => out.push(PriorAnswer {
                    effect: row.effect,
                    count: 1,
                    latest_turns_ago: ago,
                    first_turns_ago: ago,
                }),
            }
        }
        out
    }

    /// Record what the operator did with a decision that had already been made: a denial they
    /// can see is one they can lift, and the lift is the labelled example. Never overwrites the
    /// verdict, and answers whether the row was found.
    pub fn record_override(
        &mut self,
        request_id: &str,
        what: crate::authorise::OperatorOverride,
    ) -> bool {
        let Some(row) = self.log.iter_mut().find(|r| r.request.id == request_id) else {
            return false;
        };
        // A grant lifts the breaker for that direction too: a human answered, which is
        // the only thing that closes an open one.
        let key = row.direction.clone();
        let granted = matches!(what, crate::authorise::OperatorOverride::Granted { .. });
        row.operator = Some(what.clone());
        if let Some(sink) = &self.corpus_sink {
            sink.ruled(request_id, &what);
        }
        // A grant lifts the breaker for that direction: a human answered, and that is
        // the only thing that closes an open one.
        if granted {
            self.breaker.reset_key(&key);
        }
        true
    }

    /// Every row that is a labelled disagreement: the corpus a fine-tune is for.
    pub fn corpus(&self) -> Vec<crate::authorise::CorpusRow> {
        self.log.iter().map(AdjudicationRow::corpus).collect()
    }
}

impl Gate for AdjudicatedGate {
    /// The ladder, stopped where a card would be raised. The same body as [`Gate::admit`] with
    /// the adjudicator, the guard model and the circuit breaker switched off, so a look a plugin
    /// declared can never block on a head or close the session's gate to its own model.
    fn admit_unasked(&mut self, call: &GateCall<'_>) -> GateDecision {
        let breaker = std::mem::take(&mut self.breaker);
        self.unasked = true;
        let decision = self.admit(call);
        self.unasked = false;
        self.breaker = breaker;
        decision
    }

    /// How this session stands, for the `on_tool_call` hook: the mode's own word and the session
    /// id. Read from here because the mode is this gate's field and it moves at runtime, so a
    /// copy elsewhere would be a second answer.
    fn standing(&mut self) -> crate::runtime::Standing {
        crate::runtime::Standing {
            session: self.session_id.clone(),
            mode: self.mode.name.to_string(),
        }
    }
    /// The question the boundary could not ask. A real adjudication, so it lands in the corpus
    /// beside every other decision, but not a tool call: nothing runs, the subject is a path and
    /// the answer widens a view. `NotAsked` when there is nobody to ask, never a refusal.
    fn grant_view(&mut self, path: &std::path::Path, tool: &str) -> crate::runtime::ViewGrant {
        use crate::runtime::ViewGrant;
        if self.adjudicator.describe().starts_with("none") {
            return ViewGrant::NotAsked;
        }
        let req = AdjudicationRequest {
            id: self.next_id(),
            session_id: self.session_id.clone(),
            turn_id: String::new(),
            call_id: String::new(),
            agent: self.agent.clone(),
            tool: tool.to_string(),
            target: path.display().to_string(),
            class: ActionClass::host(Access::Read, false, false),
            prior: Vec::new(),
            // A path is not a tool call, so this session's record of tool calls is
            // not evidence about it; an empty list here says exactly that.
            examples: Vec::new(),
            brief_variant: crate::authorise::BriefVariant::AskedForIt,
            agent_claim: None,
            reading: None,
            shape: None,
            advice: None,
            summary: format!(
                "`{tool}` named `{}`, which is outside this session's filesystem view",
                path.display()
            ),
            arguments: serde_json::json!({ "path": path.display().to_string() }),
            arguments_digest: String::new(),
            // A view grant is about showing the operator something, not about
            // running a program: there is no command and so no script.
            scripts: Vec::new(),
            boundary_facts: vec![
                format!(
                    "the command saw `{}` as absent — that was the boundary, not an answer about whether anything is there",
                    path.display()
                ),
                "granting it binds the real host path into this session's mount namespace for the rest of the session, and a readable path is readable INTO THE TRANSCRIPT"
                    .to_string(),
                "the command that hit this has already run; granting does not re-run it".to_string(),
            ],
            kind: RequestKind::Permission,
            options: view_grant_options(),
            // A view grant is offered as it always was: it is a fact about the session's
            // filesystem view rather than about a command's class, and nothing here is
            // left off the ladder.
            options_note: None,
            on_timeout: OnTimeout::Deny,
            tier: Tier::MayApprove,
            resolved: true,
            baseline: format!("a path outside the view: {}", path.display()),
            trail: crate::authorise::AuthorisationTrail::default(),
        };
        let d = self.adjudicator.decide(&req);
        match &d.outcome {
            DecisionOutcome::Selected { option_id } if option_id == "grant_ro" => {
                ViewGrant::Granted { writable: false }
            }
            DecisionOutcome::Selected { option_id } if option_id == "grant_rw" => {
                ViewGrant::Granted { writable: true }
            }
            DecisionOutcome::Selected { .. } => ViewGrant::Refused(d.basis.clone()),
            // A deadline is not an answer, and neither is an adjudicator that could
            // not be reached. Both are "nobody decided", which is what the caller
            // has to be told so it does not report a refusal.
            _ => ViewGrant::NotAsked,
        }
    }

    /// The memory ask above the ceiling, as a card the OPERATOR answers. `AlwaysAsk`, not
    /// `MayApprove`: a guard model widening it into an admission would be the model granting
    /// itself the box.
    fn grant_memory(
        &mut self,
        ask: &crate::exec::memory::MemoryAsk,
    ) -> crate::exec::memory::MemoryGrant {
        use crate::exec::memory::MemoryGrant;
        // A session grant covers an ask no larger than it and no larger than what is free.
        // Uncapped is covered only by an uncapped grant, and an uncapped ask has no size to hold
        // against what is free, so the grant itself is the operator's word on it.
        let covered = match (self.memory_session, ask.bytes) {
            (Some(None), _) => true,
            (Some(Some(up_to)), Some(b)) => b <= up_to && b <= ask.available,
            _ => false,
        };
        if covered {
            return MemoryGrant::Granted;
        }
        if self.adjudicator.describe().starts_with("none") {
            return MemoryGrant::NotAsked;
        }
        let amount = ask
            .bytes
            .map(crate::exec::human_bytes)
            .unwrap_or_else(|| "uncapped".into());
        let req = AdjudicationRequest {
            id: self.next_id(),
            session_id: self.session_id.clone(),
            turn_id: String::new(),
            call_id: String::new(),
            agent: self.agent.clone(),
            tool: "bash".to_string(),
            target: ask.command.clone(),
            class: ActionClass::host(Access::Exec, false, false),
            prior: Vec::new(),
            examples: Vec::new(),
            brief_variant: crate::authorise::BriefVariant::AskedForIt,
            agent_claim: None,
            reading: None,
            shape: None,
            advice: None,
            summary: ask.summary(),
            arguments: serde_json::json!({ "command": ask.command, "memory": amount }),
            arguments_digest: String::new(),
            scripts: Vec::new(),
            boundary_facts: vec![
                format!(
                    "{} of {} is free now; the model may take up to {} without asking",
                    crate::exec::human_bytes(ask.available),
                    crate::exec::human_bytes(ask.total),
                    crate::exec::human_bytes(ask.ceiling)
                ),
                "the command has NOT started; it starts only if you agree".to_string(),
                "under its cap the command dies alone when it reaches it; uncapped, a command \
                 that eats the box is killed by the kernel along with whatever else it picks, \
                 and that has taken whole terminal windows"
                    .to_string(),
            ],
            kind: RequestKind::Permission,
            options: memory_grant_options(),
            options_note: None,
            on_timeout: OnTimeout::Deny,
            tier: Tier::AlwaysAsk {
                rule: "memory_above_ceiling",
                why: "the operator decides any memory ask above the ceiling".into(),
            },
            resolved: true,
            baseline: format!("a memory ask above the ceiling: {amount}"),
            trail: crate::authorise::AuthorisationTrail::default(),
        };
        let d = self.adjudicator.decide(&req);
        match &d.outcome {
            DecisionOutcome::Selected { option_id } if option_id == "grant_memory" => {
                MemoryGrant::Granted
            }
            DecisionOutcome::Selected { option_id } if option_id == "grant_memory_session" => {
                // The larger of what was granted before and this, so a second, bigger session
                // grant widens it and a smaller one never narrows what the operator already gave.
                self.memory_session = Some(match (self.memory_session, ask.bytes) {
                    (Some(None), _) | (_, None) => None,
                    (Some(Some(was)), Some(b)) => Some(was.max(b)),
                    (None, Some(b)) => Some(b),
                });
                MemoryGrant::Granted
            }
            DecisionOutcome::Selected { .. } => MemoryGrant::Refused(d.basis.clone()),
            _ => MemoryGrant::NotAsked,
        }
    }

    fn describe(&self) -> String {
        match (self.supervise, &self.advisor) {
            (true, Some(a)) => format!(
                "{} — supervised by {}",
                self.adjudicator.describe(),
                a.describe()
            ),
            _ => self.adjudicator.describe(),
        }
    }

    fn supervising(&self) -> bool {
        self.supervise && self.advisor.is_some()
    }

    fn attach_advisor(&mut self, advisor: std::sync::Arc<dyn Adjudicator>) -> Result<(), String> {
        self.advisor = Some(advisor);
        Ok(())
    }

    fn set_mode(&mut self, mode: crate::mode::Mode) -> Result<usize, String> {
        let dropped = self.grants.len();
        self.grants.clear();
        self.memory_session = None;
        self.mode = mode;
        Ok(dropped)
    }

    fn set_supervision(&mut self, on: bool) -> Result<String, String> {
        // Refuses rather than pretending: a gate told to supervise with no guard model
        // installed that answered "ok" would leave the operator believing every later call was
        // being measured while none were.
        let Some(advisor) = self.advisor.as_ref() else {
            return Err(
                "no guard model is configured, so there is nothing to supervise WITH. \
                 Put its address in ~/.config/letibot/providers.toml:\n\
                 \x20 [gatekeeper]\n\
                 \x20 endpoint = \"HOST:PORT\"\n\
                 a llama.cpp /completion endpoint. Or `/supervise HOST:PORT` for this \
                 session only."
                    .into(),
            );
        };
        if self.supervise == on {
            return Ok(format!(
                "already {}",
                if on { "supervised" } else { "unsupervised" }
            ));
        }
        self.supervise = on;
        Ok(if on {
            format!(
                "supervision ON from the next gated call: {} answers first, you answer \
                 second, and both land on the same corpus row. Nothing else changed — \
                 the same calls ask that asked before.",
                advisor.describe()
            )
        } else {
            "supervision OFF. Calls still ask whoever the mode says; the guard model \
             is no longer consulted and rows stop carrying a verdict."
                .into()
        })
    }

    fn admit(&mut self, call: &GateCall<'_>) -> GateDecision {
        use crate::authorise::{BreakerState, TaskDirection};
        use crate::intent::BaselineVerdict;

        let baseline = self.baseline_for(call);
        // The access this call is judged at. Computed here and passed to the three places that
        // would otherwise read the tool's declared one; the gate is still consulted on the
        // declared access, so this changes what it says, not whether it is asked.
        let access = Self::judged_access(call, &baseline);
        // The first half, computed once: `Some(cargo test)` when this call runs the project's
        // own code and the host has no boundary. Read by the three arms that would otherwise let
        // a standing answer settle it, and by the two that would record one.
        let unconfined_code = self.runs_project_code_unconfined(access, &baseline);
        let mut req = self.request_from(call, &baseline);
        let direction = TaskDirection::of(&req, &baseline);
        let breaker_state = self.breaker.state(&direction);

        // 0. The breaker, before anything else asks anything. Three consecutive refusals in one
        //    direction and the loop stops: the fallback is a human, and another verdict would be
        //    the one-call-at-a-time behaviour the breaker exists to stop.
        if let BreakerState::Open { consecutive } = breaker_state {
            let d = AdjudicationDecision {
                request_id: req.id.clone(),
                outcome: DecisionOutcome::Escalate {
                    to: "human".into(),
                    why: format!("{consecutive} consecutive refusals in this task direction"),
                },
                by: "breaker".into(),
                basis: format!(
                    "the circuit breaker is open for `{}` after {consecutive} consecutive \
                     refusals. No adjudicator is consulted; only the operator lifts this",
                    direction.key()
                ),
                latency_ms: 0,
            };
            let tell = self.surface(&req, &d, "not_run", breaker_state.clone());
            let why = d.basis.clone();
            // The one settled path that does NOT ask the model, deliberately: the breaker is
            // open because this direction was refused three times in a row, so each call here is
            // a near-duplicate and an oracle round trip per retry is the opposite of stopping it.
            self.record(req, d, "refuse", direction.key());
            return GateDecision::refuse_and_tell(ToolOutcome::NotRun { why }, tell);
        }

        // 1a. Layer A's own refusals, before any adjudicator. `NotRun` first: an action nobody
        //     could read is not one anybody can decide about. Except at a point that admits
        //     everything, where an unreadable command goes on and the row says it ran unread.
        let ran_unread = match &baseline.verdict {
            BaselineVerdict::NotRun { why } if self.mode.admits_everything() => {
                Some(why.lines().next().unwrap_or("").to_string())
            }
            _ => None,
        };
        if ran_unread.is_none()
            && let BaselineVerdict::NotRun { why } = &baseline.verdict
        {
            let d = AdjudicationDecision {
                request_id: req.id.clone(),
                outcome: DecisionOutcome::Unavailable,
                by: "boundary:normaliser".into(),
                basis: why.clone(),
                latency_ms: 0,
            };
            let tell = self.surface(&req, &d, "not_run", breaker_state.clone());
            self.breaker.refused(&direction);
            let why = why.clone();
            // Asked, and it answers `unavailable` without reaching the oracle, so the row says
            // an unresolved action produced no verdict rather than saying nothing.
            self.advise_on_a_settled_call(&req, "layer A could not resolve the action".into());
            self.record(req, d, "refuse", direction.key());
            return GateDecision::refuse_and_tell(ToolOutcome::NotRun { why }, tell);
        }
        // 1b. The tier: the one thing layer A refuses outright. Narrow on purpose -
        //      irreversible disclosure of a secret across the boundary. There is no destructive
        //      block list here: `rm -rf /` is allowed if it is the intent.
        if let Tier::Blocked { rule, evidence } = &req.tier {
            let d = AdjudicationDecision {
                request_id: req.id.clone(),
                outcome: DecisionOutcome::Selected {
                    option_id: "deny_and_tell".into(),
                },
                by: "boundary:flow".into(),
                basis: format!(
                    "{evidence} ({}). The consequence of a disclosure does not land on the person who would be consenting to it — it lands on every host that credential opens and on whoever reads the transcript later — and it cannot be undone afterwards. So no context, no classifier verdict and no operator instruction promotes it",
                    rule.as_str()
                ),
                latency_ms: 0,
            };
            let tell = self.surface(&req, &d, "denied", breaker_state.clone());
            self.breaker.refused(&direction);
            let id = req.id.clone();
            self.advise_on_a_settled_call(
                &req,
                format!("layer A refuses `{}` outright", rule.as_str()),
            );
            self.record(req, d, "refuse", direction.key());
            return GateDecision::refuse_and_tell(ToolOutcome::Denied { req_id: id }, tell);
        }

        // 1. The `never` rows, overridable by none. Two questions, one rule: `never_hit` asks it
        // of the SPELLING and [`ssh_resolution_hit`] of the PATH - what the argument names after
        // every symlink, for a file that may not exist yet.
        let refused = never_hit(call.args).map(|hit| {
            format!(
                "`{hit}` is on the never-write list: no adjudicator is consulted and none \
                 can override it"
            )
        });
        let refused = refused.or_else(|| {
            let home = self.surroundings.home.as_deref()?;
            let real = ssh_resolution_hit(call, std::path::Path::new(home))?;
            Some(crate::remote::dot_ssh_refusal(
                &real,
                std::path::Path::new(home),
            ))
        });
        if let Some(basis) = refused {
            let d = AdjudicationDecision {
                request_id: req.id.clone(),
                outcome: DecisionOutcome::Selected {
                    option_id: "deny_and_tell".into(),
                },
                by: "boundary:host".into(),
                basis: basis.clone(),
                latency_ms: 0,
            };
            let tell = self.surface(&req, &d, "denied", breaker_state.clone());
            let id = req.id.clone();
            self.breaker.refused(&direction);
            // Asked here too: an oracle that wants to admit a call touching the operator's ssh
            // material is a calibration fact, invisible unless this path records a verdict. It
            // changes nothing, and the note is the decision's own basis, so no two stories.
            self.advise_on_a_settled_call(&req, basis);
            self.record(req, d, "refuse", direction.key());
            // The model is told why, because a refusal it cannot understand is a
            // refusal it will retry.
            return GateDecision::refuse_and_tell(ToolOutcome::Denied { req_id: id }, tell);
        }

        // 1.5. opencode's permission model, evaluated before the mode: `deny` refuses, `allow`
        //      admits, `ask` falls through. Last rule wins over the config plus `always`.
        if !self.permission.is_empty() {
            let pattern = permission_pattern(call.args);
            // A `bash` command is tested segment by segment against the prefix rules, and a
            // compound the splitter cannot read is asked, never allowed.
            let action = if call.args.get("command").and_then(|v| v.as_str()).is_some()
                && call.access == Access::Exec
            {
                crate::permission::evaluate_bash(&pattern, &[&self.permission])
            } else {
                crate::permission::evaluate(call.name, &pattern, &[&self.permission]).action
            };
            match action {
                crate::permission::Action::Deny => {
                    let d = AdjudicationDecision::selected(
                        &req,
                        "deny_and_tell",
                        "gate:permission",
                        &format!(
                            "the `permission` config denies `{}` for `{pattern}`; no \
                             adjudicator is consulted and none can override it",
                            call.name
                        ),
                    );
                    let tell = self.surface(&req, &d, "denied", breaker_state.clone());
                    self.breaker.refused(&direction);
                    let id = req.id.clone();
                    self.advise_on_a_settled_call(
                        &req,
                        format!("`permission` config denies `{}` for `{pattern}`", call.name),
                    );
                    self.record(req, d, "refuse", direction.key());
                    return GateDecision::refuse_and_tell(ToolOutcome::Denied { req_id: id }, tell);
                }
                crate::permission::Action::Allow if unconfined_code.is_none() => {
                    let d = AdjudicationDecision::selected(
                        &req,
                        "allow_once",
                        "gate:permission",
                        &format!(
                            "the `permission` config allows `{}` for `{pattern}`; nothing \
                             was consulted",
                            call.name
                        ),
                    );
                    self.breaker.admitted(&direction);
                    self.advise_on_a_settled_call(
                        &req,
                        format!("`permission` config allows `{}` for `{pattern}`", call.name),
                    );
                    self.record(req, d, "admit", direction.key());
                    return GateDecision::Admit;
                }
                // A rule is a standing answer, and this call is not covered by one: an `allow`
                // keyed on a program and a pattern was written about a file that no longer
                // exists. It is not deleted, it simply does not settle this call.
                crate::permission::Action::Allow => {}
                crate::permission::Action::Ask => {}
            }
        }

        // Inside a measured boundary a command is not asked about: at `writes-allowed`, where
        // the confinement holds, a per-command ruling protects nothing a write the mode already
        // admits could not do. What still asks is what crosses the boundary or widens the view.
        if !self.unasked
            && admits_inside_the_boundary(self.boundary.as_ref(), self.mode, access, &baseline)
            && let Some(boundary) = &self.boundary
        {
            let basis = format!(
                "admitted: inside the measured boundary. {} entered the namespaces that \
                 ARE the boundary, both seals were tested and held, egress is denied, this call \
                 crosses nothing (it names no network intent and no path outside the checkout), \
                 and it is not on the always-ask list. A ruling about this command would \
                 protect nothing a write this mode already admits could not do instead — the \
                 boundary is the defence, so nobody was asked",
                boundary.mechanism
            );
            let d = AdjudicationDecision::selected(&req, "allow_once", "gate:boundary", &basis);
            self.breaker.admitted(&direction);
            // Told, not only recorded. The audit row is read by `/gate` and by the corpus,
            // and neither scrolls while the work happens: a mode that stops asking has to say
            // so where the operator is looking, or the only evidence is the silence.
            if let Some(sink) = &self.admissions {
                sink.admitted(&AdmissionNotice {
                    request_id: req.id.clone(),
                    turn_id: req.turn_id.clone(),
                    tool: req.tool.clone(),
                    summary: req.summary.clone(),
                    basis: d.basis.clone(),
                    mode: self.mode.name.to_string(),
                    boundary: boundary.mechanism.clone(),
                });
            }
            self.advise_on_a_settled_call(&req, "inside a measured boundary".into());
            self.record(req, d, "admit", direction.key());
            return GateDecision::Admit;
        }

        // 2. The point this session sits at. It governs `Tier::MayApprove` and nothing else, so
        //    this arm cannot widen what a point may not. Exec is excepted unless the mode follows
        //    it, and `ran_unread` widens nothing: an unreadable exec asks where a readable one does.
        if (access != Access::Exec || self.exec_follows_mode)
            && self.mode.admits_unasked(&req.tier, access)
        {
            let d = AdjudicationDecision::selected(
                &req,
                "allow_once",
                "gate:mode",
                &format!(
                    "the `{}` mode admits {} calls without asking; nothing was consulted{}",
                    self.mode.name,
                    access.as_str(),
                    ran_unread
                        .as_deref()
                        .map(|w| format!(
                            ". **It ran unread** — the grammar could not read it ({w}), and \
                             this point admits it anyway"
                        ))
                        .unwrap_or_default()
                ),
            );
            self.breaker.admitted(&direction);
            // The mode's own admission is a settled call like any other. It cannot fire at
            // `supervised` today, and it is wired because a point is a value and the next one may
            // admit something while still wanting the verdict.
            self.advise_on_a_settled_call(
                &req,
                format!(
                    "the `{}` mode admits {} unasked",
                    self.mode.name,
                    access.as_str()
                ),
            );
            self.record(req, d, "admit", direction.key());
            return GateDecision::Admit;
        }

        // 3. A standing permission from an earlier `allow_session`, keyed on
        //    `(program, ActionClass, intents)` and checked per call against the normalisation,
        //    never against text, so a vehicle flag falls out of a grant taken over `git status`.
        let program = grant_program(&baseline);
        // A shape the operator already approved, this session. The key is the parse with its
        // literals holed, so two greps are one approval and `sed -n` is not `sed -i`; flags
        // stay. Bounded: a human approved it, the tier is re-checked here, and destruction asks.
        if let Tier::MayApprove = req.tier
            && unconfined_code.is_none()
            && let Some(shape) = &req.shape
            && let Some(seen) = self.approved_shapes.get(shape)
            && *seen == req.class
            && !baseline.intents.contains(&crate::intent::Intent::Destroy)
        {
            let basis = format!("you approved this shape before, in this project: `{shape}`");
            let d = AdjudicationDecision::selected(&req, "allow_once", "gate:shape", &basis);
            self.advise_on_a_settled_call(&req, "a shape the operator approved".into());
            self.breaker.admitted(&direction);
            self.record(req, d, "admit", direction.key());
            return GateDecision::Admit;
        }

        if unconfined_code.is_none()
            && let Some(g) = self.grants.iter().find(|g| {
                g.covers(
                    &req.tier,
                    req.resolved,
                    &program,
                    req.class,
                    &baseline.intents,
                )
                .is_ok()
            })
        {
            let basis = format!(
                "a standing permission granted this session covers this call: {}",
                g.why
            );
            let why = g.why.clone();
            let d =
                AdjudicationDecision::selected(&req, "allow_session", "gate:session-grant", &basis);
            // A grant settles the person, not the model: nobody is asked again, and the oracle
            // is still consulted because its verdict on a call already ruled on is a cheap
            // labelled example. It changes no outcome - the grant admits regardless.
            self.advise_on_a_settled_call(&req, why);
            self.breaker.admitted(&direction);
            self.record(req, d, "admit", direction.key());
            return GateDecision::Admit;
        }

        // 4.0 - the call that is adjudicated and may not ask. Everything above is the ladder,
        // everything below is the person, and a plugin's look stops here because a card would
        // park the monitor thread. `NotRun`, not `Denied`: nobody decided.
        if self.unasked {
            let why = format!("would need your approval: {}", req.summary);
            let tell = format!(
                "{why}\n\nThe gate puts this to the operator before running it, and nothing is \
                 put to anybody from here: this is a look a plugin layer declared, adjudicated \
                 without asking, on the thread that polls the watch. Nothing ran."
            );
            return GateDecision::refuse_and_tell(ToolOutcome::NotRun { why }, tell);
        }

        // 4. Ask - and when supervision is on, ask the model FIRST. A verdict formed after
        // seeing which way the operator went is not worth training on, and one they never saw is
        // advice nobody could agree with.
        self.advice = self.ask_the_advisor(&req);
        req.advice = self.advice.clone();

        // At a point whose decider is the MODEL, the model's admit IS the decision: otherwise
        // `automode` is `supervised` under another name. Still bounded by the always-ask list,
        // the oracle's earned scope, and `Blocked` refused at 1b - and it admits once.
        if self.mode.decider == crate::mode::Decider::Model
            && !matches!(req.tier, Tier::AlwaysAsk { .. })
            && let Some(a) = self.advice.clone()
            && a.consulted
            && a.would == "admit"
        {
            let d = AdjudicationDecision {
                request_id: req.id.clone(),
                outcome: DecisionOutcome::Selected {
                    option_id: "allow_once".into(),
                },
                by: a.by.clone(),
                basis: a.basis.clone(),
                latency_ms: a.latency_ms,
            };
            self.breaker.admitted(&direction);
            // This path consulted the oracle too, so it files the exchange: without it the brief
            // and the reply were dropped and `shown` held whatever the previous call put there.
            self.file_exchange();
            self.record(req, d, "admit", direction.key());
            return GateDecision::Admit;
        }

        let decision = self.adjudicator.decide(&req);
        self.file_exchange();
        let typed_pattern = self.adjudicator.last_pattern();

        match &decision.outcome {
            DecisionOutcome::Selected { option_id } => {
                let kind = req.option(option_id).map(|o| o.kind);
                match kind {
                    Some(k) if k.admits() => {
                        // The shape, remembered - but only from a person, and only where a shape
                        // may stand in: `MayApprove` only, since a shape recorded on an always-ask
                        // would be the standing grant three other mechanisms exist to refuse.
                        if matches!(req.tier, Tier::MayApprove)
                            && decision.by.starts_with("human")
                            && !baseline.intents.contains(&crate::intent::Intent::Destroy)
                            && unconfined_code.is_none()
                            && let Some(shape) = req.shape.clone()
                        {
                            self.approved_shapes.insert(shape, req.class);
                        }
                        // An always-ask can be admitted by a human this turn, but never turned
                        // into a standing grant: no `Adjudicable`, no grant recorded here, and the
                        // option is not offered. *Always allow* is a rule, not a grant.
                        if k == OptionKind::AllowAlways
                            && !matches!(req.tier, Tier::AlwaysAsk { .. })
                            // And not for a call that runs the project's code on a host with no
                            // boundary: the ladder did not offer this rung, and an answer from an
                            // older head or a test must not write a rule the gate would not honour.
                            && unconfined_code.is_none()
                        {
                            // The operator's own glob wins over the derived one, which is only ever
                            // the shape in front of them; and a rule that could never match is not
                            // written, since `derived_pattern` answers `None` for a heredoc.
                            let pattern = match typed_pattern {
                                Some(p) => Some(p),
                                None => derived_pattern(call, &baseline, &self.permission),
                            };
                            if let Some(pattern) = pattern {
                                let rule = crate::permission::Rule::new(
                                    call.name,
                                    pattern,
                                    crate::permission::Action::Allow,
                                );
                                if let Some(sink) = &self.permission_sink
                                    && let Err(e) = sink(&rule)
                                {
                                    // The rule holds for this session either way; the
                                    // audit row carries why it will not outlive it.
                                    eprintln!(
                                        "letibot: the always-allow rule `{}` for `{}` was not \
                                     written down: {e}",
                                        rule.pattern, rule.permission
                                    );
                                }
                                self.permission.push(rule);
                            }
                        } else if k == OptionKind::AllowSession
                            && !matches!(req.tier, Tier::AlwaysAsk { .. })
                            && unconfined_code.is_none()
                        {
                            // The grant is scoped, not blanket: keyed on the program and the
                            // class with the intents a subset of what the call was shown to carry,
                            // and one that could never cover anything is not recorded at all.
                            let why = format!(
                                "granted for this session by an answer to {}: {}",
                                req.id, decision.basis
                            );
                            if let Ok(grant) =
                                Self::session_grant(&baseline, req.class, &program, why)
                            {
                                self.grants.push(grant);
                            }
                        }
                        // The loop closed, so the error signal is gone.
                        self.breaker.admitted(&direction);
                        self.record(req, decision, "admit", direction.key());
                        GateDecision::Admit
                    }
                    Some(k) => {
                        let id = req.id.clone();
                        // `DenyAndTell` decides how much of the adjudicator's own basis travels;
                        // the fact of a refusal, and that it is a refusal rather than a failure,
                        // always does - a model told only "denied" tries a variant.
                        let mut tell =
                            self.surface(&req, &decision, "denied", breaker_state.clone());
                        if !matches!(k, OptionKind::DenyAndTell) {
                            tell.push_str(
                                "\n\nThe adjudicator's own reasoning stayed in the audit \
                                 row; the operator can read it there.",
                            );
                        }
                        self.breaker.refused(&direction);
                        self.record(req, decision, "refuse", direction.key());
                        GateDecision::refuse_and_tell(ToolOutcome::Denied { req_id: id }, tell)
                    }
                    // Unreachable via `AskAdjudicator` and `ConsoleAdjudicator`,
                    // which both check; reachable via a hand-written adjudicator,
                    // and it must not open the gate.
                    None => {
                        let why = format!(
                            "the adjudicator selected `{option_id}`, which this request did \
                             not offer; nothing was executed"
                        );
                        let tell = self.surface(&req, &decision, "not_run", breaker_state.clone());
                        self.breaker.refused(&direction);
                        self.record(req, decision, "refuse", direction.key());
                        GateDecision::refuse_and_tell(ToolOutcome::NotRun { why }, tell)
                    }
                }
            }
            DecisionOutcome::Escalate { to, why } => {
                // Escalation is first class, but there is one adjudicator attached, so there is
                // nowhere to escalate to - and saying so is the honest answer.
                let msg = format!(
                    "`{}` escalated this to `{to}` ({why}), and this session has one \
                     adjudicator and no routing table, so there is nobody to escalate to. \
                     Nothing was executed.",
                    decision.by
                );
                let tell = self.surface(&req, &decision, "not_run", breaker_state.clone());
                self.breaker.refused(&direction);
                self.record(req, decision, "refuse", direction.key());
                GateDecision::refuse_and_tell(ToolOutcome::NotRun { why: msg }, tell)
            }
            DecisionOutcome::Unavailable
            | DecisionOutcome::Timeout
            | DecisionOutcome::Cancelled => {
                // Never a silent allow: route to `on_timeout`. `OnTimeout::Allow` is not
                // reachable from `request_for`, which always sets `Deny`; the arm exists so a
                // policy row saying `Allow` has somewhere to land.
                let outcome = match req.on_timeout {
                    OnTimeout::Allow => {
                        self.breaker.admitted(&direction);
                        self.record(req, decision, "admit", direction.key());
                        return GateDecision::Admit;
                    }
                    OnTimeout::Deny | OnTimeout::AgentDecides => ToolOutcome::NotRun {
                        why: format!(
                            "`{}` is a {} tool, and the adjudicator answered `{}`: {}. \
                             The gate fails closed, so nothing was executed and nothing \
                             on disk changed. This is not a denial — nobody decided.",
                            req.tool,
                            req.class.access.as_str(),
                            decision.outcome.as_str(),
                            decision.basis
                        ),
                    },
                };
                let tell = self.surface(&req, &decision, "not_run", breaker_state.clone());
                self.breaker.refused(&direction);
                self.record(req, decision, "refuse", direction.key());
                GateDecision::refuse_and_tell(outcome, tell)
            }
        }
    }
}

/// ```text
/// let (state, detail, active) =
///     letibot_tools::adjudicate::startup_disclosure(gate_describes, backend_writable, has_write_tools);
/// out.push(if active { Disclosure::on("adjudication", detail) }
///          else       { Disclosure::off("adjudication", state, detail) });
/// ```
pub fn startup_disclosure(
    adjudicator: &str,
    backend_writable: bool,
    has_write_tools: bool,
) -> (&'static str, String, bool) {
    startup_disclosure_with_surfacing(adjudicator, backend_writable, has_write_tools, true)
}

/// As [`startup_disclosure`], and it also says whether denials reach the operator: a session
/// whose denials go nowhere lets the model try a variant and dies with the operator seeing only
/// a dead task, which is not a state to be in silently.
pub fn startup_disclosure_with_surfacing(
    adjudicator: &str,
    backend_writable: bool,
    has_write_tools: bool,
    denials_surfaced: bool,
) -> (&'static str, String, bool) {
    startup_disclosure_for(
        adjudicator,
        backend_writable,
        if has_write_tools { &["write"] } else { &[] },
        denials_surfaced,
    )
}

/// As [`startup_disclosure_with_surfacing`], and it says which classes can reach the gate rather
/// than assuming they are writes. `classes` is the set of non-unattended access names actually
/// seated, read off the schemas by the caller; empty means nothing can reach the gate.
pub fn startup_disclosure_for(
    adjudicator: &str,
    backend_writable: bool,
    classes: &[&str],
    denials_surfaced: bool,
) -> (&'static str, String, bool) {
    let (state, mut detail, active) =
        startup_disclosure_inner(adjudicator, backend_writable, classes);
    if !classes.is_empty() && !denials_surfaced {
        detail.push_str(
            " DENIALS ARE NOT SURFACED: no denial sink is attached, so a refusal reaches the model and stops there. The operator will see a task that stopped rather than the decision that stopped it, which is the failure mode that makes a model try a variant instead of asking. Attach one with `AdjudicatedGate::with_denial_sink`.",
        );
        return (state, detail, false);
    }
    (state, detail, active)
}

fn startup_disclosure_inner(
    adjudicator: &str,
    backend_writable: bool,
    classes: &[&str],
) -> (&'static str, String, bool) {
    if classes.is_empty() {
        return (
            "N/A",
            "this session has only read-only tools, which never prompt (clause 4). \
             Nothing can reach the gate, so nothing needs an adjudicator."
                .into(),
            false,
        );
    }
    // What the session actually seated, in its own words. `WRITE` for a coder, `EXEC` for a
    // runner, `WRITE + NETWORK` for a planner: never the first of those about all three.
    let shout = classes
        .iter()
        .map(|c| c.to_uppercase())
        .collect::<Vec<_>>()
        .join(" + ");
    let quiet = classes.join(" + ");
    let writes = classes.contains(&"write");
    let attached = !adjudicator.starts_with("none");
    match (attached, backend_writable) {
        (false, _) => (
            "NONE",
            format!(
                "this session has {shout} TOOLS and no adjudicator ({adjudicator}). Every \
                 {quiet} call will refuse with NotRun and change nothing. That is the \
                 fail-closed default and not a fault; attach an adjudicator to make them \
                 callable."
            ),
            false,
        ),
        // Only a *write* class is stopped by a read-only backend. An `exec` or `network` tool
        // over one is a different, and worse, mismatch, so this arm says which it is rather than
        // describing every seat as a blocked write.
        (true, false) if writes => (
            "GATE ONLY",
            format!(
                "an adjudicator is attached ({adjudicator}), but the execution backend was \
                 opened READ-ONLY, so an admitted write still cannot reach the disk. Two \
                 gates, and the second one is shut."
            ),
            false,
        ),
        (true, false) => (
            "GATE ONLY",
            format!(
                "an adjudicator is attached ({adjudicator}) over {quiet} tools, and the \
                 backend was opened READ-ONLY. Nothing here can change a file; whether the \
                 tools work at all depends on what each needs from the backend, and a tool \
                 that needs more than it has refuses naming the backend."
            ),
            false,
        ),
        (true, true) => (
            "",
            format!(
                "{adjudicator}. {shout} tools are callable: an admitted call takes effect. \
                 Read-only tools still never prompt (clause 4)."
            ),
            true,
        ),
    }
}

/// What a call is about, in one string, for the summary a human decides from. The order is the
/// order of specificity: a path, then an address, then a query. A call naming none says so
/// rather than borrowing the word `path`.
fn target_of(args: &Value) -> String {
    // `command` is first because for `bash` it IS the call: a shell line is more concrete than
    // any path inside it, and its absence is why an operator was shown a prompt that would not
    // name the command it was about.
    for key in [
        "command", "path", "url", "query", "repo", "server", "pattern",
    ] {
        if let Some(v) = args.get(key).and_then(|v| v.as_str())
            && !v.trim().is_empty()
        {
            return v.to_string();
        }
    }
    "<no target argument>".to_string()
}

/// The pattern the permission model tests a call against: the path for a file tool, the command
/// for `bash`, else `*`.
fn permission_pattern(args: &Value) -> String {
    if let Some(p) = args.get("path").and_then(|v| v.as_str()) {
        return p.to_string();
    }
    if let Some(c) = args.get("command").and_then(|v| v.as_str()) {
        return c.to_string();
    }
    "*".to_string()
}

/// The rule an *Always allow* answer would write, or `None` when none could be: `None` means the
/// matcher will ask again whatever is written, so the caller drops the option rather than offer
/// it. One helper for the label and the recording site, so the rule cannot drift.
fn derived_pattern(
    call: &GateCall<'_>,
    baseline: &crate::intent::Baseline,
    rules: &crate::permission::Ruleset,
) -> Option<String> {
    let Some(cmd) = call.args.get("command").and_then(|v| v.as_str()) else {
        return Some(permission_pattern(call.args));
    };
    if call.access != Access::Exec {
        return Some(permission_pattern(call.args));
    }
    if !crate::permission::a_durable_rule_can_apply(cmd) {
        return None;
    }
    // A stage that sets state for a later one makes no durable rule honest: the only rule that
    // would silence it admits every variable a later program trusts, which `the_rule_that_admits`
    // refuses too.
    if let Some(n) = &baseline.command
        && n.stages.iter().any(|s| {
            s.program_name()
                .is_some_and(crate::permission::sets_state_for_later)
        })
    {
        return None;
    }
    if !rules.is_empty() {
        return crate::permission::the_rule_that_admits(cmd, &[rules]);
    }
    // The parse when there is one, and nothing when there is not: a command layer A could
    // not read gets no offer rather than the raw string's first token.
    baseline
        .command
        .as_ref()
        .and_then(|n| n.stages.last())
        .and_then(|stage| {
            stage.program_name().map(|program| {
                let first = stage.argv.first().and_then(|w| w.literal());
                crate::permission::always_pattern_for_stage(program, first)
            })
        })
}

/// A baseline's command as a shape, when it has one. One place, so the key a decision is
/// recorded under and the key a later call is looked up by cannot differ.
pub fn shape_of(b: &crate::intent::Baseline) -> Option<String> {
    b.command.as_ref().map(letibot_code::shell::shape)
}

/// The words nobody could read, for the one line a card has room for: the words are what the
/// person would have to rewrite. The word as written, not the construct's name. Nothing when the
/// parse is missing every word, so the sentence does not trail a dangling dash.
fn unresolved_names(b: &crate::intent::Baseline) -> String {
    const SHOWN: usize = 3;
    let Some(n) = &b.command else {
        return String::new();
    };
    let mut names: Vec<String> = Vec::new();
    for u in &n.unresolved {
        let word: String = u.text.trim().chars().take(24).collect();
        if word.is_empty() || names.contains(&word) {
            continue;
        }
        names.push(word);
    }
    if names.is_empty() {
        return String::new();
    }
    let more = names.len().saturating_sub(SHOWN);
    names.truncate(SHOWN);
    let list = names
        .iter()
        .map(|n| format!("`{n}`"))
        .collect::<Vec<_>>()
        .join(", ");
    match more {
        0 => format!(" — {list}"),
        n => format!(" — {list} and {n} more"),
    }
}

/// The arguments that are content and not a path, so the never-write list is not asked about
/// them: a write's body, a replacement's text and a search pattern name no path. A path argument
/// still is, and so is `bash`'s `command`, which is where a string is the only evidence there is.
const CONTENT_ARGUMENTS: &[&str] = &[
    "content",
    "old_string",
    "new_string",
    "pattern",
    "text",
    "abstract",
    "query",
];

/// The arguments that name a path, for [`ssh_resolution_hit`]: the file tools all spell it
/// `path`, and the rest are here because a tool that takes a path does not always call it that.
const PATH_ARGUMENTS: &[&str] = &[
    "path",
    "paths",
    "file",
    "file_path",
    "cwd",
    "src",
    "dest",
    "to",
    "from",
    "dir",
    "directory",
    "target",
    "worktree",
    "workspace",
];

/// A path argument whose real target is the operator's ssh directory. The literal check above
/// answers a spelling; this answers the path, so a link in the workspace named `keys` and pointing
/// into it is caught. A `command` is only best effort: its words are all a matcher can see.
fn ssh_resolution_hit(call: &GateCall<'_>, home: &std::path::Path) -> Option<std::path::PathBuf> {
    let obj = call.args.as_object()?;
    let workspace = std::path::Path::new(call.workspace);
    let resolved = |raw: &str| -> Option<std::path::PathBuf> {
        let p = crate::remote::expand(std::path::Path::new(raw), home);
        let p = if p.is_absolute() {
            p
        } else {
            workspace.join(p)
        };
        if crate::remote::inside_user_ssh(&p, home) {
            Some(crate::remote::real(&p))
        } else {
            None
        }
    };
    for (key, value) in obj {
        if CONTENT_ARGUMENTS.contains(&key.as_str()) {
            continue;
        }
        let Value::String(s) = value else { continue };
        if key == "command" {
            for word in s.split_whitespace() {
                let word = word
                    .trim_matches(|c: char| matches!(c, '"' | '\'' | ';' | '(' | ')' | ',' | '`'));
                if (word.starts_with('/') || word.starts_with('~'))
                    && let Some(hit) = resolved(word)
                {
                    return Some(hit);
                }
            }
            continue;
        }
        if PATH_ARGUMENTS.contains(&key.as_str())
            && let Some(hit) = resolved(s)
        {
            return Some(hit);
        }
    }
    None
}

/// Any string argument that names a path on the never-write list. Every string is checked, not
/// only `path`: a check that looks at the well-known name tests the spelling rather than the fact.
fn never_hit(args: &Value) -> Option<String> {
    let obj = args.as_object()?;
    for (key, v) in obj {
        // Content is not a path: see [`CONTENT_ARGUMENTS`] for the measurement that made this a
        // rule rather than a tolerance.
        if CONTENT_ARGUMENTS.contains(&key.as_str()) {
            continue;
        }
        let Value::String(s) = v else { continue };
        for seg in s.split(['/', '\\']) {
            for never in NEVER_WRITE {
                // `.config/gh` is two segments; compare against the whole string
                // for those and against segments for the rest.
                if never.contains('/') {
                    if s.contains(never) {
                        return Some((*never).to_string());
                    }
                } else if seg == *never {
                    return Some((*never).to_string());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod scripts_brief_tests {
    use super::scripts_section;
    use crate::runtime::{ScriptBody, ScriptSource};

    /// The code goes in front of the adjudicator, not the filename.
    #[test]
    fn a_read_script_is_shown_whole_and_labelled_as_the_thing_to_judge() {
        let out = scripts_section(&[ScriptSource {
            path: "deploy.py".into(),
            body: ScriptBody::Read("import os\nos.remove('/home/dead/.ssh/id_rsa')\n".into()),
        }]);
        assert!(out.contains("judge THIS, not the filename"), "{out}");
        assert!(out.contains("--- deploy.py ---"), "{out}");
        assert!(out.contains("os.remove('/home/dead/.ssh/id_rsa')"), "{out}");
    }

    /// A script that could not be read says so, which matters more than the ones that could: told
    /// nothing, an adjudicator cannot tell a command that runs no script from one whose script it
    /// was not shown, and those are the two cases where the right answers are opposite.
    #[test]
    fn an_unreadable_script_is_named_rather_than_passed_over() {
        let out = scripts_section(&[ScriptSource {
            path: "setup.sh".into(),
            body: ScriptBody::Unreadable("no such file".into()),
        }]);
        assert!(out.contains("COULD NOT BE READ: no such file"), "{out}");
        assert!(
            out.contains("runs a program nobody here has seen"),
            "and says what that means: {out}"
        );
    }

    /// A truncated script says how much is missing, so nobody reads part of a program as all of
    /// one: the middle is exactly where something would hide.
    #[test]
    fn a_truncated_script_says_what_was_not_read() {
        let out = scripts_section(&[ScriptSource {
            path: "big.py".into(),
            body: ScriptBody::Truncated {
                head: "print(1)".into(),
                omitted: 40_000,
            },
        }]);
        assert!(out.contains("40000 NOT SHOWN"), "{out}");
        assert!(out.contains("anything could be in it"), "{out}");
    }

    /// No scripts is no section: a command that runs none must not grow a heading implying it does.
    #[test]
    fn a_command_with_no_script_gets_no_section() {
        assert_eq!(scripts_section(&[]), "");
    }
}
