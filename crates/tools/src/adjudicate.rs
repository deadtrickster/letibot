//! §11's adjudication seam, from the tool side — the minimum that genuinely
//! decides.
//!
//! # Why this exists at all, and what it is not
//!
//! W9 shipped [`crate::runtime::Gate`] as *"the smallest adjudication seam the
//! runtime needs"* and `TODO.md` T16.3 recorded the consequence: **W11 should
//! absorb it rather than build a parallel one.** W10 is the first caller that
//! needs the gate to actually decide something, because `edit` and `write` are
//! the first tools that can change the operator's tree. So this module grows the
//! seam to §11.2's shape without building §11.3's policy table or §11.7's auto
//! mode, both of which are W11's.
//!
//! What is here:
//!
//! | §11 | here |
//! |---|---|
//! | 11.2's one decision shape | [`AdjudicationRequest`], [`AdjudicationDecision`] |
//! | 11.3's action class, *derived* | [`ActionClass`] — the routing key, not a policy table |
//! | 11.4's boundary refuse-list, evaluated first | [`NEVER_WRITE`] |
//! | 11.5's "every request and answer is a row" | [`AdjudicatedGate::log`] |
//! | 11.6's permission/question split | [`RequestKind`] |
//! | 11.9's "no adjudicator can override a `deny` row" | the precheck runs **before** [`Adjudicator::decide`] |
//!
//! What is deliberately not here: the routing table (one adjudicator is attached
//! per session, not a table of them), auto mode, shadow mode, persistence of
//! `allow_always` past the process, and the flowy delivery mechanics of §11.5.
//! Each is named where a reader would look for it.
//!
//! # Fail closed, and why that is the whole point
//!
//! > *"A `Gate` that returns 'allowed' because nothing is wired is worse than no
//! > gate, because it looks like protection."*
//!
//! [`NoAdjudicator`] is the default and it answers [`DecisionOutcome::Unavailable`]
//! — never a selection. [`AdjudicatedGate`] maps `Unavailable` to a refusal, and
//! the refusal carries [`ToolOutcome::NotRun`] rather than
//! [`ToolOutcome::Denied`], because `Denied` claims somebody decided and nobody
//! did. That distinction is §8.2's discipline applied to the outcome vocabulary
//! itself, and W9 already made it once for [`crate::runtime::NoBoundary`].
//!
//! There is a **second, independent** gate below this one:
//! [`crate::backend::HostBackend`] is read-only unless it was opened with
//! [`crate::backend::HostBackend::writable`]. So a write reaching the disk needs
//! both an adjudicator that admitted it and a backend that was opened to be
//! written to, and neither is the default. A safety property with one mechanism
//! behind it is a safety property that ships broken the first time somebody
//! refactors the mechanism.

use std::collections::BTreeSet;
use std::sync::Mutex;

use letibot_transcript::ToolOutcome;
use serde_json::Value;

use crate::runtime::{Gate, GateCall, GateDecision};
use crate::schema::Access;

// ---------------------------------------------------------------------------
// §3's two tiers, and the witness that makes the second one unspellable.
// ---------------------------------------------------------------------------

/// Which of `docs/boundary-and-adjudication.md` §3's data-flow rules an action
/// breaks.
///
/// The rule is about **flow**, not access: `ssh` reads the private key, so "never
/// read `~/.ssh/id_rsa`" would forbid the authorised case. What separates the rows is
/// where the bytes end up. See [`crate::intent`] for the derivation and its limits.
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
    /// A write *into* a secret store. Not §3's own row — §11.4's, which
    /// [`NEVER_WRITE`] also covers — and kept here so both mechanisms exist.
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

/// **The four outcome classes**, decided by layer A and by nothing else.
///
/// Not two, and not "high risk"/"low risk". The axis is *who decides*, and the four
/// answers do not collapse into each other:
///
/// | | who decides | when |
/// |---|---|---|
/// | [`Tier::Auto`] | nothing is consulted | a read inside the boundary — clause 4 |
/// | [`Tier::MayApprove`] | the model, if the authorisation is clear | everything else |
/// | [`Tier::AlwaysAsk`] | **the operator, every time** | layer A's fixed list, whatever anything else concludes |
/// | [`Tier::Inexpressible`] | nobody, ever | secret bytes crossing the boundary, and nothing else |
///
/// # The two the model cannot move
///
/// > *"i think A should have a fixed list of things we ask a user about anyway. the
/// > problem as i see it is that not layer a not layer b are ideal. we can[not] be
/// > sure intent is clearly distilled and calibrated by model on step b"*
///
/// [`Tier::AlwaysAsk`] is **not** the destructive block list that used to live in
/// [`crate::intent`] and was deleted. A block list says *never* and is wrong —
/// `rm -rf /` can be the intent. An always-ask list says *the human decides this one,
/// every time, no matter how confident anything is*. It preserves the operator's
/// authority to authorise anything while removing the model's authority to authorise
/// it on their behalf, and **the classifier cannot shrink it** — only the operator
/// can, and each removal is a recorded decision like any other override.
///
/// [`Tier::Inexpressible`] is narrower still: irreversible disclosure of a secret
/// across the boundary. Not "dangerous", not "destructive". The asymmetry is who
/// bears the consequence and whether they can consent in-session — the operator owns
/// a deletion and chose it, and cannot un-disclose a key afterwards.
///
/// # Why this is a type and not a check
///
/// [`Adjudicable`] is minted only for [`Tier::MayApprove`], so a function that could
/// return an admission for the other two cannot be written — see
/// [`crate::authorise::Widening`]. Layer B widens `MayApprove` from ask to admit and
/// can do nothing else, in either direction.
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
    Inexpressible { rule: FlowRule, evidence: String },
}

impl Tier {
    pub fn as_str(&self) -> &'static str {
        match self {
            Tier::Auto => "auto",
            Tier::MayApprove => "may_approve",
            Tier::AlwaysAsk { .. } => "always_ask",
            Tier::Inexpressible { .. } => "inexpressible",
        }
    }

    pub fn is_inexpressible(&self) -> bool {
        matches!(self, Tier::Inexpressible { .. })
    }

    /// Whether an oracle may be consulted at all. False for both of the tiers it
    /// cannot move, and false for `Auto`, which needs nobody.
    pub fn is_adjudicable_by_model(&self) -> bool {
        matches!(self, Tier::MayApprove)
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
                Tier::Inexpressible { .. } => 3,
            }
        }
        if rank(&other) > rank(&self) { other } else { self }
    }
}

/// **The witness that layer A found this action adjudicable and readable.**
///
/// A tuple struct with a private field: nothing outside this module can construct
/// one, and the only public way to obtain one is
/// [`AdjudicationRequest::adjudicable`], which answers `None` when the tier is
/// [`Tier::Inexpressible`] **or** when the command did not resolve.
///
/// [`crate::authorise::Widening`] — the sole value that can turn an ask into an
/// admit — takes one by value. So both of the failures this design is most afraid of
/// are unspellable rather than merely checked:
///
/// - *admit an inexpressible action* — the witness does not exist for one.
/// - *promote out of always-ask* — nor for one of those. The operator decides those
///   every time, and "the model was very sure" is not a way around it.
/// - *unresolved, therefore proceed* — measured as one `if` apart in the survey:
///   one harness turns a parse failure into a prompt, another returns before the ask
///   and lets the raw string reach the shell. There is no branch here to invert,
///   because there is no value to branch on.
///
/// It is deliberately **not** `Clone`, `Copy` or `Default`: a witness that could be
/// duplicated is a witness that could be moved from a request that earned it to one
/// that did not.
#[derive(Debug)]
pub struct Adjudicable(());

// ---------------------------------------------------------------------------
// §11.3's action class — derived, never free text.
// ---------------------------------------------------------------------------

/// Where an action's effect lands. §11.3's `effect_scope`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EffectScope {
    /// Inside a firecode run: a copy of one project and nothing else of the host.
    InRun,
    /// The host's project directory — the session's own workspace.
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
}

/// §11.3's routing key: `(tool.access, effect_scope, reversibility, cost)`.
///
/// Derived from four facts the harness already has, which is why it is a struct
/// of enums and not a string an operator types. Its `Display` is the pattern
/// §11.3's table matches on, so a policy table W11 writes later reads the same
/// spelling this log already carries.
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
    /// The class of a host-local tool call.
    ///
    /// Two of the four facts are honest simplifications and both are stated rather
    /// than hidden:
    ///
    /// - `cost` is always `Free`. No tool in this crate spends API credits or
    ///   touches the network, and §11.4 says explicitly that the boundary does not
    ///   protect metered actions — so `metered` will arrive with the tool that is
    ///   metered, not before.
    /// - `reversibility` is `Reversible` for an edit or an overwrite of a file
    ///   that exists, and `Irreversible` for the creation of one, because a
    ///   creation has no prior content to restore. That is a weaker claim than it
    ///   looks: "reversible" here means *the harness handed the prior bytes to the
    ///   head in the same result* ([`crate::edit::FileEdit::before`]), not that
    ///   anything will automatically undo it.
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

    /// The class of a call that **leaves the box**.
    ///
    /// This exists because [`ActionClass::host`] was the only constructor and
    /// `Access::Network` had no user, so every derivation ran through the path
    /// that asks whether a `path` argument is inside the workspace. A network call
    /// has no `path`, `path_is_inside` answers `true` for the absence of one, and
    /// the first `web_fetch` would therefore have been logged and routed as
    /// `network,host_project` — a class that says the effect lands in the
    /// operator's project directory. The audit would have been wrong in the
    /// direction that matters, and nothing would have said so.
    ///
    /// `reversibility` is **always `Irreversible`**, and that is a claim rather
    /// than a hedge: a request that has left this box cannot be recalled. Even a
    /// read is a row in somebody's access log and a fact about who is looking at
    /// what. §11.3 routes `*,external,irreversible,*` to a human, which is the
    /// conservative row, and the alternative — calling a `GET` reversible because
    /// nothing here changed — would be reasoning about the wrong side of the wire.
    ///
    /// `cost` is the caller's to state. It is `Free` for everything today because
    /// nothing is attached and nothing is billed; the first metered provider brings
    /// [`Cost::Metered`] with it, and §11.4 is explicit that the boundary does not
    /// protect metered actions.
    pub fn external(access: Access, cost: Cost) -> Self {
        ActionClass {
            access,
            scope: EffectScope::External,
            reversibility: Reversibility::Irreversible,
            cost,
        }
    }
}

// ---------------------------------------------------------------------------
// §11.2's one decision shape.
// ---------------------------------------------------------------------------

/// §11.6: a permission and a question are one mechanism, differing in `kind` and
/// in what happens when nobody answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestKind {
    Permission,
    Question,
}

/// What happens when nobody answers. §11.5.
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
    /// As `AllowSession`, and W11 persists it past the process. Here it behaves as
    /// `AllowSession` and says so in the row, because claiming a durable grant the
    /// harness does not store would be a lie in the audit.
    AllowAlways,
    Deny,
    /// Deny, and put the reason in front of the model rather than only in the log.
    DenyAndTell,
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

/// The ladder for an action on [`crate::intent::ALWAYS_ASK`]: **no standing grant**.
///
/// A third mechanism for one property, and deliberately so. The tier withholds the
/// [`Adjudicable`] witness, [`AdjudicatedGate::admit`] refuses to insert a class grant
/// for one, and the prompt does not offer the option in the first place — so an
/// operator is not shown a button whose effect the gate would then decline to honour,
/// and an adjudicator cannot select one. A safety property with one mechanism ships
/// broken the first time somebody refactors the mechanism.
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

/// The allow/deny ladder every permission carries. opencode's permission *model*
/// is the part §11.9 says is worth vendoring, and this is it.
pub fn permission_options() -> Vec<DecisionOption> {
    vec![
        DecisionOption {
            id: "allow_once".into(),
            label: "Allow this one".into(),
            kind: OptionKind::AllowOnce,
        },
        DecisionOption {
            id: "allow_session".into(),
            label: "Allow this class for the rest of the session".into(),
            kind: OptionKind::AllowSession,
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

/// §11.2's request, minus the fields no adjudicator on this box can use yet.
///
/// Dropped from §11.2 and why: `session_id`/`agent` are carried (the gate is
/// constructed with them); `run_id` and `boundary_facts` are firecode's and
/// arrive with the firecode backend, so [`AdjudicationRequest::boundary_facts`]
/// holds what the *host* backend can honestly state instead; `deadline` is not
/// here because this seam is synchronous — an adjudicator that has to wait owns
/// its own deadline and reports [`DecisionOutcome::Timeout`], which is the same
/// fact one layer in.
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
    pub arguments: Value,
    pub arguments_digest: String,
    /// What the backend can truthfully say about where this lands.
    pub boundary_facts: Vec<String>,
    pub kind: RequestKind,
    pub options: Vec<DecisionOption>,
    pub on_timeout: OnTimeout,

    // --- layer A's findings, and the authorisation trail. -------------------
    /// §3's tier, derived by [`crate::intent`] and **never** chosen by a caller: the
    /// gate computes it in [`AdjudicatedGate::request_for`], the way it computes
    /// `class`. A field a caller filled in would be a field a caller could fill in
    /// wrongly.
    pub tier: Tier,
    /// Whether the grammar resolved the whole action. `false` is `NotRun` — *nobody
    /// could decide* — and no adjudicator is consulted.
    pub resolved: bool,
    /// Layer A's one-line reading: the verdict, the intents, the regions, the tier.
    pub baseline: String,
    /// **The authorisation trail.** §2: *"a stateless command classifier cannot be
    /// correct, because the same command is authorised or not depending on what the
    /// operator just said."*
    pub trail: crate::authorise::AuthorisationTrail,
}

impl AdjudicationRequest {
    /// The witness that layer A found this adjudicable **and** readable.
    ///
    /// The only way to obtain an [`Adjudicable`], and therefore the only way anything
    /// can be admitted by an oracle. `None` for an inexpressible action and `None`
    /// for an unresolved one — see [`Adjudicable`] for why that is a type rather than
    /// a check.
    pub fn adjudicable(&self) -> Option<Adjudicable> {
        (self.resolved && self.tier.is_adjudicable_by_model()).then_some(Adjudicable(()))
    }
    /// The brief §11.7 says a model adjudicator sees: the class, the tool, the
    /// arguments, the boundary facts and the options — and **never the
    /// transcript**.
    ///
    /// It is rendered here rather than in the model adjudicator because a human
    /// over flowy and a console prompt need exactly the same bytes, and having
    /// three renderings of one request is how they stop agreeing.
    pub fn brief(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("decision {} — {}\n", self.id, self.summary));
        out.push_str(&format!("tool: {}\nclass: {}\n", self.tool, self.class));
        for f in &self.boundary_facts {
            out.push_str(&format!("fact: {f}\n"));
        }
        out.push_str(&format!(
            "arguments ({}):\n{}\n",
            self.arguments_digest,
            arguments_preview(&self.arguments)
        ));
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

/// §11.7 asks for *"the arguments, verbatim, with any value over 2 KiB spilled
/// to a digest and a head/tail"*. The same number, and the same reason: an
/// adjudicator that has to read a 40 KB file body to approve a one-line edit is
/// an adjudicator nobody will read.
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

/// §11.2's closed outcome vocabulary. dsh's property: *a throwing or
/// non-conforming answerer becomes `unavailable`, never silently opens the gate.*
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecisionOutcome {
    Selected { option_id: String },
    /// First class rather than an error path (§11.4).
    Escalate { to: String, why: String },
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

/// §11.2's decision. `basis` is *"short text — the reason, always present, never
/// optional"*, so it is a `String` and not an `Option<String>`.
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

// ---------------------------------------------------------------------------
// The adjudicator seam.
// ---------------------------------------------------------------------------

/// One adjudicator. §11.1's three — boundary, human, model — are three
/// implementations of this, *"and the design's job is to make them
/// interchangeable rather than to rank them"*.
pub trait Adjudicator: Send + Sync {
    fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision;

    /// For `EXPLAIN` and for the daemon's startup disclosures. An operator who
    /// cannot see which adjudicator is attached cannot see that none is.
    fn describe(&self) -> String;

    /// The bytes this adjudicator was **actually shown** for the last call, when it
    /// shows anything.
    ///
    /// The gate copies it into [`AdjudicationRow::shown`], and that field is the reason
    /// this method exists: a fine-tuning corpus whose input was reconstructed
    /// afterwards is a corpus of guesses about what the model saw. `None` for an
    /// adjudicator that shows nothing — [`NoAdjudicator`] — which is a different fact
    /// from an empty brief.
    fn last_brief(&self) -> Option<String> {
        None
    }
}

/// **The default, and it refuses.**
///
/// Not "deny": nobody decided. It answers `Unavailable`, which
/// [`AdjudicatedGate`] routes to the request's `on_timeout` — `Deny` for a
/// permission over a write, which becomes [`ToolOutcome::NotRun`] because the
/// harness will not claim a decision that was not made.
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

/// An adjudicator that is a function of the request.
///
/// This is the seam a head, a flowy connector or §11.7's model adjudicator plugs
/// into: all three are *"post a brief somewhere and come back with an option id"*.
/// `None` from the function means **nobody answered**, which becomes
/// `Unavailable` — never an allow, which is dsh's rule about a non-conforming
/// answerer.
pub struct AskAdjudicator<F> {
    pub ask: F,
    pub id: String,
}

impl<F> AskAdjudicator<F>
where
    F: Fn(&AdjudicationRequest) -> Option<AdjudicationDecision> + Send + Sync,
{
    pub fn new(id: impl Into<String>, ask: F) -> Self {
        AskAdjudicator {
            ask,
            id: id.into(),
        }
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
                // A conforming answer names an option this request offered. One
                // that does not is `Unavailable`, not a guess: §11.2's whole point
                // is that a non-conforming answerer never opens the gate.
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

/// A real ask over a pair of streams: write the brief, read an option id.
///
/// This is the `--ask` path in its smallest honest form, and it is here so that
/// "an ask path exists and is real" is a thing a test can drive rather than a
/// thing a README claims. `ConsoleAdjudicator::stdio()` is what a foreground CLI
/// uses; the constructor takes the streams so a test can hand it a `Cursor` and
/// assert on the exact bytes a human would have seen.
///
/// **Not** the flowy human adjudicator of §11.5: this one holds the call, and
/// §11.5 says a pending decision should be a visible row rather than a held
/// connection. It composes with firecode's `--ask`, which does block, and it is
/// the right shape inside a guest that has nothing else.
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
            // A poisoned mutex means a previous ask panicked. Unavailable, not a
            // guess.
            Err(_) => {
                return AdjudicationDecision::unavailable(
                    req,
                    &format!("human:{}", self.who),
                    "the console adjudicator's stream is poisoned",
                );
            }
        };
        let io = &mut *io;
        let _ = write!(io.output, "{}\nchoose: ", req.brief());
        let _ = io.output.flush();

        let mut line = String::new();
        match io.input.read_line(&mut line) {
            // EOF: the stream closed without an answer. §11.5's *"silence is
            // indistinguishable from absence"* — except here it is not, because
            // the stream told us. Timeout, which routes to `on_timeout`.
            Ok(0) => AdjudicationDecision {
                request_id: req.id.clone(),
                outcome: DecisionOutcome::Timeout,
                by: format!("human:{}", self.who),
                basis: "the ask channel closed without an answer".into(),
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
                        by: format!("human:{}", self.who),
                        basis: format!(
                            "`{chosen}` is not one of the options this request offered"
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
                &format!("human:{}", self.who),
                &format!("the ask channel failed: {e}"),
            ),
        }
    }

    fn describe(&self) -> String {
        format!("console ask, answered by {}", self.who)
    }
}

// ---------------------------------------------------------------------------
// The gate proper.
// ---------------------------------------------------------------------------

/// The closure a session loop installs to supply the authorisation trail.
///
/// A named alias because the inline type is unreadable and because it is the seam
/// `docs/boundary-and-adjudication.md` §2 asks for: see
/// [`AdjudicatedGate::with_trail_source`] for what must call it.
pub type TrailSource =
    dyn Fn(&GateCall<'_>) -> crate::authorise::AuthorisationTrail + Send + Sync;

/// One row of the audit. §11.5: *"Every request, fan-out, ack, answer and timeout
/// is a row. Immutable. The audit is log-only and never enters the model
/// transcript."*
///
/// So this is deliberately not reachable from [`crate::result::ToolResult`]: the
/// model sees the derived outcome, not the deliberation.
#[derive(Debug, Clone, PartialEq)]
pub struct AdjudicationRow {
    pub request: AdjudicationRequest,
    pub decision: AdjudicationDecision,
    /// What the gate did with the decision, in one word: `admit` or `refuse`.
    pub effect: &'static str,
    /// **What the operator decided afterwards**, if they said anything.
    ///
    /// A separate field, and it **never overwrites** `decision`. The disagreement is
    /// the training signal:
    ///
    /// > *"my inputs on model decision should be together with some prior context a
    /// > fine tuning input."*
    ///
    /// A row that recorded only the final state would have thrown the label away, and a
    /// corpus assembled later from such rows is not recoverable. Set by
    /// [`AdjudicatedGate::record_override`].
    pub operator: Option<crate::authorise::OperatorOverride>,
    /// The bytes an oracle was actually shown, when one was consulted. Verbatim rather
    /// than reconstructed, for the same reason.
    pub shown: Option<String>,
    /// The circuit breaker's key for this action — see
    /// [`crate::authorise::TaskDirection`]. Carried so an operator lifting a refusal
    /// they were shown does not have to reconstruct which direction it was in.
    pub direction: String,
}

impl AdjudicationRow {
    /// The row as one fine-tuning example.
    pub fn corpus(&self) -> crate::authorise::CorpusRow {
        crate::authorise::CorpusRow {
            request_id: self.request.id.clone(),
            session_id: self.request.session_id.clone(),
            turn_id: self.request.turn_id.clone(),
            action: self.request.baseline.clone(),
            trail: self.request.trail.clone(),
            shown: self.shown.clone(),
            baseline: self.request.baseline.clone(),
            tier: self.request.tier.as_str(),
            model_verdict: Some(format!(
                "{} by {}: {}",
                self.decision.outcome.as_str(),
                self.decision.by,
                self.decision.basis
            )),
            effect: self.effect,
            operator: self.operator.clone(),
        }
    }
}

/// §11.4's refuse-list, evaluated **before any adjudicator** and overridable by
/// none of them.
///
/// §11.9's rule, from dsh: *"the `never` policy is enforced inside the service
/// before dispatch, so no listener can bypass it … a `deny` row in §11.3's table
/// is evaluated before any adjudicator is consulted, and no adjudicator can
/// override it."*
///
/// The first six are §11.4's own list, quoted: `--add-dir` and `--workdir`
/// *"refuse outright to carry `.ssh`, `.gnupg`, `.aws`, `.kube`, `.config/gh`,
/// `.password-store` or a browser profile out of the home directory"*. The
/// browser profiles are that clause spelled out. `.git/` is **ours** and not
/// §11.4's: a write into `.git/` is not a source edit, and rewriting a ref is not
/// something the tool that did it can undo — which is exactly the property
/// [`ActionClass::host`] calls `reversibility` and the only one this gate can
/// actually check.
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
];

/// The gate that consults an adjudicator.
///
/// Constructed with one; the default one is [`NoAdjudicator`] and it refuses.
pub struct AdjudicatedGate {
    adjudicator: Box<dyn Adjudicator>,
    session_id: String,
    agent: String,
    /// Classes granted for the rest of the session by an `allow_session` or
    /// `allow_always` selection. Keyed by `tool + class`, which is the finest key
    /// this seam can honestly offer: a grant keyed by the *arguments* would be a
    /// grant for one call, which is `allow_once`.
    granted: BTreeSet<String>,
    /// §11.5's rows. In memory: the durable journal is `letibot-sessionlog`'s, and
    /// wiring this into it is W11's, not W10's.
    pub log: Vec<AdjudicationRow>,
    /// A monotonic counter so two requests in one turn have different ids. Not a
    /// ULID: §11.2 asks for one and this crate has no clock dependency and no ULID
    /// crate. W11 replaces it; until then an id is unique within a session, which
    /// is what the log and the refusal text need.
    seq: u64,
    /// What layer A needs to know about where it is standing. Defaults to
    /// [`Default::default`], whose [`crate::intent::ShellTrust`] is `Unknown` — so a
    /// gate nobody configured refuses bare command names, which is the fail-closed
    /// direction.
    surroundings: crate::intent::Surroundings,
    /// Where the authorisation trail comes from. See
    /// [`crate::authorise::AuthorisationTrail`] for what must install it.
    trail_source: Option<Box<TrailSource>>,
    /// Where denials go so the operator sees them **when they happen**.
    denials: Option<Box<dyn crate::authorise::DenialSink>>,
    /// The consecutive-denial circuit breaker.
    pub breaker: crate::authorise::Breaker,
    /// The brief the adjudicator was shown for the call in flight, moved into the row
    /// by [`AdjudicatedGate::record`].
    shown: Option<String>,
}

impl AdjudicatedGate {
    pub fn new(adjudicator: Box<dyn Adjudicator>) -> Self {
        AdjudicatedGate {
            adjudicator,
            session_id: "session".into(),
            agent: "agent".into(),
            granted: BTreeSet::new(),
            log: Vec::new(),
            seq: 0,
            surroundings: crate::intent::Surroundings::default(),
            trail_source: None,
            denials: None,
            breaker: crate::authorise::Breaker::default(),
            shown: None,
        }
    }

    /// Tell layer A where it is standing, including whether the shell that will run a
    /// command is fixed. See [`crate::intent::ShellTrust`] — undeclared means bare
    /// command names are unresolved.
    pub fn with_surroundings(mut self, s: crate::intent::Surroundings) -> Self {
        self.surroundings = s;
        self
    }

    /// **Install the authorisation trail source.**
    ///
    /// This is the seam §2 needs and the one this crate cannot fill itself: the tool
    /// runtime sees one call at a time and has no transcript, deliberately. The
    /// session loop that owns the conversation installs a closure here.
    ///
    /// What must call it: whoever constructs the gate for a session — today
    /// `crates/harnessd`'s session setup — passing a closure that walks the
    /// transcript backwards from the current turn, collects
    /// `TranscriptItem::User` text as [`crate::authorise::Utterance`]s with their
    /// distance in turns, and returns
    /// [`crate::authorise::AuthorisationTrail::from_messages`] with the number of
    /// messages it looked at. Until that happens the trail is
    /// `NotCollected`, and [`crate::authorise::ModelAdjudicator`] refuses rather than
    /// deciding without it.
    pub fn with_trail_source(
        mut self,
        f: impl Fn(&GateCall<'_>) -> crate::authorise::AuthorisationTrail + Send + Sync + 'static,
    ) -> Self {
        self.trail_source = Some(Box::new(f));
        self
    }

    /// **Install the denial sink.** Every refusal is delivered to it at the moment it
    /// is decided, so the operator is never left inferring a decision from a task
    /// that stopped.
    pub fn with_denial_sink(mut self, sink: Box<dyn crate::authorise::DenialSink>) -> Self {
        self.denials = Some(sink);
        self
    }

    /// Emit the notice, and produce the prose the model is given.
    ///
    /// One function so the two cannot drift: the sentence the operator sees and the
    /// sentence the model sees are built from one [`crate::authorise::DenialNotice`].
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
            grant: format!(
                "grant `{}` ({}) for this session, or answer the pending decision. \
                 Nothing was executed and nothing changed.",
                req.id, req.tool
            ),
        };
        if let Some(sink) = &self.denials {
            sink.denied(&notice);
        }
        crate::authorise::refusal_text(&notice)
    }

    /// The fail-closed gate, named so that constructing one reads as a decision.
    pub fn closed() -> Self {
        Self::new(Box::new(NoAdjudicator))
    }

    pub fn with_identity(mut self, session_id: impl Into<String>, agent: impl Into<String>) -> Self {
        self.session_id = session_id.into();
        self.agent = agent.into();
        self
    }

    /// What the daemon should print at startup. See the module docs on why an
    /// operator who cannot see this cannot see that nothing is attached.
    pub fn describe(&self) -> String {
        self.adjudicator.describe()
    }

    /// Whether denials reach the operator. Feeds
    /// [`startup_disclosure_with_surfacing`].
    pub fn surfaces_denials(&self) -> bool {
        self.denials.is_some()
    }

    fn next_id(&mut self) -> String {
        self.seq += 1;
        format!("adj-{}-{:04}", self.session_id, self.seq)
    }

    /// **Layer A's deterministic reading of this call**, before any adjudicator.
    ///
    /// A `command` argument is normalised through the grammar; anything else is read
    /// as a set of path arguments, so §3's flow rule applies to
    /// `read({path: "~/.ssh/id_rsa"})` exactly as it applies to `cat ~/.ssh/id_rsa`.
    /// A rule that only looked at shell commands would be routed around by the first
    /// tool that takes a path.
    pub fn baseline_for(&self, call: &GateCall<'_>) -> crate::intent::Baseline {
        if let Some(cmd) = call.args.get("command").and_then(|v| v.as_str()) {
            return crate::intent::Baseline::of_command(cmd, &self.surroundings);
        }
        let paths: Vec<&str> = call
            .args
            .as_object()
            .map(|o| {
                o.iter()
                    .filter(|(k, _)| {
                        matches!(k.as_str(), "path" | "paths" | "file" | "src" | "dest" | "to" | "from")
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

    /// The request one call produces. Public because the head and the tests both
    /// want to see a request without a decision having been made about it.
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
        // What this call is *about*, for a human who is deciding in one line. A
        // path for a file tool; for a network tool there is none, and printing
        // `<no path argument>` about a `web_fetch` would describe the request by
        // what it is not.
        let target = target_of(call.args);
        let inside = call.path_is_inside();
        // Reversibility is a fact about the target, and the runtime stats it
        // before asking. `None` — no path argument at all — is treated as
        // reversible: claiming irreversibility about a call whose target nobody
        // looked at would be a stronger statement than the evidence supports.
        let creates = call.target_exists == Some(false);
        let class = if network {
            ActionClass::external(call.access, Cost::Free)
        } else {
            ActionClass::host(call.access, inside, creates)
        };
        let digest = crate::events::payload_digest(&call.args.to_string());
        let mut facts = if network {
            vec![
                format!("workspace: {} — and this call does not touch it", call.workspace),
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
            vec![
                format!("workspace: {}", call.workspace),
                format!(
                    "the path is {} the session's workspace",
                    if inside { "inside" } else { "OUTSIDE" }
                ),
                "the host filesystem is not sandboxed; §11.4's boundary arrives with firecode"
                    .to_string(),
            ]
        };
        if creates && !network {
            facts.push("this creates a file that does not exist, so there is nothing to restore".into());
        }
        if let Some(op) = call.args.get("op").and_then(|v| v.as_str()) {
            // A tool that dispatches on an op is one tool to the gate and ten
            // actions to whoever is deciding. The op goes in the facts because the
            // routing key cannot carry it (§11.3's class has no op field), and a
            // human reading `github` alone cannot tell a listing from a merge.
            facts.push(format!("operation: `{op}`"));
        }
        AdjudicationRequest {
            id: self.next_id(),
            session_id: self.session_id.clone(),
            turn_id: call.turn_id.to_string(),
            call_id: call.call_id.to_string(),
            agent: self.agent.clone(),
            tool: call.name.to_string(),
            class,
            summary: format!(
                "`{}` wants {} access to `{target}`",
                call.name,
                call.access.as_str()
            ),
            arguments: call.args.clone(),
            arguments_digest: digest,
            boundary_facts: facts,
            kind: RequestKind::Permission,
            options: if matches!(baseline.tier, Tier::AlwaysAsk { .. }) {
                always_ask_options()
            } else {
                permission_options()
            },
            // §11.5: `Deny` for a permission, and `AgentDecides` only for a
            // question. Nothing here is a question yet.
            on_timeout: OnTimeout::Deny,
            // Layer A's, never the caller's.
            tier: baseline.tier.clone(),
            resolved: !matches!(
                baseline.verdict,
                crate::intent::BaselineVerdict::NotRun { .. }
            ),
            baseline: baseline.summary(),
            trail: match &self.trail_source {
                Some(f) => f(call),
                None => crate::authorise::AuthorisationTrail::default(),
            },
        }
    }

    fn class_key(req: &AdjudicationRequest) -> String {
        format!("{}|{}", req.tool, req.class)
    }

    fn record(
        &mut self,
        request: AdjudicationRequest,
        decision: AdjudicationDecision,
        effect: &'static str,
        direction: String,
    ) {
        let shown = self.shown.take();
        self.log.push(AdjudicationRow {
            request,
            decision,
            effect,
            operator: None,
            shown,
            direction,
        });
    }

    /// **Record what the operator did with a decision that had already been made.**
    ///
    /// The other half of surfacing: a denial the operator can see is a denial they can
    /// lift, and the lift is the labelled example. Never overwrites the verdict — see
    /// [`AdjudicationRow::operator`].
    ///
    /// Returns whether the row was found, because silently recording an override
    /// against nothing would be the same defect one level down.
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
        row.operator = Some(what);
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
    fn describe(&self) -> String {
        self.adjudicator.describe()
    }

    fn admit(&mut self, call: &GateCall<'_>) -> GateDecision {
        use crate::authorise::{BreakerState, TaskDirection};
        use crate::intent::BaselineVerdict;

        let baseline = self.baseline_for(call);
        let req = self.request_from(call, &baseline);
        let direction = TaskDirection::of(&req, &baseline);
        let breaker_state = self.breaker.state(&direction);

        // 0. The breaker, before anything else asks anything. Three consecutive
        //    refusals in one direction and the loop stops: the fallback is a human,
        //    and producing another verdict would be exactly the argued-down-one-call-
        //    at-a-time behaviour the breaker exists to stop.
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
            self.record(req, d, "refuse", direction.key());
            return GateDecision::refuse_and_tell(ToolOutcome::NotRun { why }, tell);
        }

        // 1a. Layer A's own refusals, before any adjudicator and overridable by none.
        //     `NotRun` first: an action nobody could read is not an action anybody can
        //     decide about, and there is no branch here that could turn "unresolved"
        //     into "proceed".
        if let BaselineVerdict::NotRun { why } = &baseline.verdict {
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
            self.record(req, d, "refuse", direction.key());
            return GateDecision::refuse_and_tell(ToolOutcome::NotRun { why }, tell);
        }
        // 1b. §3's tier: the one thing layer A refuses outright. Narrow on purpose —
        //      irreversible disclosure of a secret across the boundary, and nothing
        //      else. There is no destructive block list here and there never will be:
        //      `rm -rf /` is allowed if it is the intent, and what makes it safe or not
        //      is a mismatch with what was authorised rather than a property of the
        //      string.
        if let Tier::Inexpressible { rule, evidence } = &req.tier {
            let d = AdjudicationDecision {
                request_id: req.id.clone(),
                outcome: DecisionOutcome::Selected {
                    option_id: "deny_and_tell".into(),
                },
                by: "boundary:flow".into(),
                basis: format!(
                    "{evidence} ({}). The consequence of a disclosure does not land on                      the person who would be consenting to it — it lands on every host                      that credential opens and on whoever reads the transcript later —                      and it cannot be undone afterwards. So no context, no classifier                      verdict and no operator instruction promotes it",
                    rule.as_str()
                ),
                latency_ms: 0,
            };
            let tell = self.surface(&req, &d, "denied", breaker_state.clone());
            self.breaker.refused(&direction);
            let id = req.id.clone();
            self.record(req, d, "refuse", direction.key());
            return GateDecision::refuse_and_tell(ToolOutcome::Denied { req_id: id }, tell);
        }

        // 1. The `never` rows, before any adjudicator and overridable by none.
        if let Some(hit) = never_hit(call.args) {
            let d = AdjudicationDecision {
                request_id: req.id.clone(),
                outcome: DecisionOutcome::Selected {
                    option_id: "deny_and_tell".into(),
                },
                by: "boundary:host".into(),
                basis: format!(
                    "`{hit}` is on the never-write list (§11.4); no adjudicator is \
                     consulted and none can override it"
                ),
                latency_ms: 0,
            };
            let tell = self.surface(&req, &d, "denied", breaker_state.clone());
            let id = req.id.clone();
            self.breaker.refused(&direction);
            self.record(req, d, "refuse", direction.key());
            // The model is told why, because a refusal it cannot understand is a
            // refusal it will retry.
            return GateDecision::refuse_and_tell(ToolOutcome::Denied { req_id: id }, tell);
        }

        // 2. A class already granted this session. Recorded as a row, because a
        //    grant that stops appearing in the audit is a grant nobody can review.
        //
        //    **Not for an always-ask.** A class grant is a standing permission, and a
        //    standing permission over `sudo` or a deletion outside the project is
        //    exactly the authority the fixed list withholds. An earlier yes to one of
        //    those was a yes to that one.
        let key = Self::class_key(&req);
        if self.granted.contains(&key) && !matches!(req.tier, Tier::AlwaysAsk { .. }) {
            let d = AdjudicationDecision::selected(
                &req,
                "allow_session",
                "gate:session-grant",
                "this class was granted for the session by an earlier decision",
            );
            self.record(req, d, "admit", direction.key());
            return GateDecision::Admit;
        }

        // 3. Ask.
        let decision = self.adjudicator.decide(&req);
        // What it was shown, verbatim, for the corpus row.
        self.shown = self.adjudicator.last_brief();

        match &decision.outcome {
            DecisionOutcome::Selected { option_id } => {
                let kind = req.option(option_id).map(|o| o.kind);
                match kind {
                    Some(k) if k.admits() => {
                        // An always-ask can be admitted — by a human, this turn — but
                        // never turned into a standing grant.
                        if matches!(k, OptionKind::AllowSession | OptionKind::AllowAlways)
                            && !matches!(req.tier, Tier::AlwaysAsk { .. })
                        {
                            self.granted.insert(key);
                        }
                        // The loop closed, so the error signal is gone.
                        self.breaker.admitted(&direction);
                        self.record(req, decision, "admit", direction.key());
                        GateDecision::Admit
                    }
                    Some(k) => {
                        let id = req.id.clone();
                        // §11.6's two denials used to differ in whether the reason
                        // reached the model at all. They no longer do, and the reason is
                        // the operator's: a model told only "denied" infers that the
                        // APPROACH was wrong and tries a variant, which is the
                        // routing-around behaviour induced by the design rather than by
                        // the model. `DenyAndTell` now decides how much of the
                        // adjudicator's own basis travels; the fact of a refusal, and
                        // that it is a refusal rather than a failure, always does.
                        let mut tell = self.surface(&req, &decision, "denied", breaker_state.clone());
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
                // §11.4: escalation is first class. There is one adjudicator
                // attached, so there is nowhere to escalate *to* — and saying so
                // is the honest answer. W11's routing table is where this becomes
                // a second lookup rather than a refusal.
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
            DecisionOutcome::Unavailable | DecisionOutcome::Timeout | DecisionOutcome::Cancelled => {
                // §11.7: *"never a silent allow"*. Route to `on_timeout`, and note
                // that `OnTimeout::Allow` is not reachable from
                // [`AdjudicatedGate::request_for`], which always sets `Deny` — the
                // arm exists so that a W11 policy row saying `Allow` has somewhere
                // to land, and so that its absence today is visible.
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

/// What the daemon must print at startup, when a session has write tools.
///
/// `crates/harnessd`'s `Config::disclosures()` already tells the truth about
/// spill, store, retrieval and adjudication, and its `adjudication` line
/// currently reads *"M1 is read-only tools, which never prompt (clause 4). There
/// is no boundary and no human in the loop."* That sentence stops being true the
/// moment `write` and `edit` are registered, and a disclosure that has quietly
/// gone stale is worse than none — it is the harness asserting a safety property
/// it no longer has.
///
/// So the text lives here, next to the code whose behaviour it describes, and
/// the daemon wraps it in its own `Disclosure`. Returned as
/// `(state, detail, active)` because that is the shape harnessd's type takes;
/// `subject` is always `"adjudication"`.
///
/// # What the daemon must do
///
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

/// As [`startup_disclosure`], and it also says whether **denials reach the operator**.
///
/// A separate function so the older signature keeps working, and a disclosure at all
/// because a session whose denials go nowhere has the defect the operator named: the
/// model infers the approach was wrong, tries a variant, and the task dies with the
/// operator seeing only a dead task. That is not a state to be in silently.
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

/// As [`startup_disclosure_with_surfacing`], and it says **which** classes can reach
/// the gate rather than assuming they are writes.
///
/// # The defect this exists to stop, which is the one this whole seam exists to stop
///
/// The two functions above take `has_write_tools: bool`, and a caller with a session
/// whose gated tools are `Access::Exec` — the runner role's five job verbs and
/// `monitor`, no `write` anywhere — has to pass `true` to get the on/off logic right,
/// and then the sentence says *"Write tools are callable"* about a session with no
/// write tools. The banner was correct about the boundary and wrong about the
/// session, which is the same shape as the four instances this crate's `GateWiring`
/// sibling already paid for: a disclosure that reads a wiring and then paraphrases it
/// from memory.
///
/// So the classes travel. `classes` is the set of non-unattended
/// [`Access`](crate::schema::Access) names actually seated — `["write"]`, `["exec"]`,
/// `["write", "network"]` — read off the schemas by the caller. Empty means nothing
/// can reach the gate.
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
            " DENIALS ARE NOT SURFACED: no denial sink is attached, so a refusal reaches              the model and stops there. The operator will see a task that stopped rather              than the decision that stopped it, which is the failure mode that makes a              model try a variant instead of asking. Attach one with              `AdjudicatedGate::with_denial_sink`.",
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
    // What the session actually seated, in its own words. `WRITE` for a coder,
    // `EXEC` for a runner, `WRITE + NETWORK` for a planner — never the first of
    // those about all three.
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
        // Only a *write* class is stopped by a read-only backend. An `exec` or
        // `network` tool over one is a different, and worse, mismatch — so this arm
        // says which it is rather than describing every seat as a blocked write.
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

/// What a call is about, in one string, for the summary a human decides from.
///
/// The order is the order of specificity, not of preference: a `path` is the most
/// concrete thing a call can name, then an address, then a query. A call that names
/// none of them says so rather than borrowing the word `path`, because *"no path
/// argument"* on a `web_search` describes the request by something it was never
/// going to have.
fn target_of(args: &Value) -> String {
    for key in ["path", "url", "query", "repo", "server", "pattern"] {
        if let Some(v) = args.get(key).and_then(|v| v.as_str())
            && !v.trim().is_empty()
        {
            return v.to_string();
        }
    }
    "<no target argument>".to_string()
}

/// Any string argument that names a path on the never-write list.
///
/// Every string is checked, not only `path`: a tool argument that is a path is
/// not always spelled `path`, and a check that only looks at the well-known name
/// is a check that tests the spelling rather than the fact.
fn never_hit(args: &Value) -> Option<String> {
    let obj = args.as_object()?;
    for v in obj.values() {
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
mod tests {
    /// `Gate::describe` defaults to naming the absence, and an attached gate names
    /// its adjudicator. The daemon's banner is computed from this, so a gate that
    /// answered vaguely would put a vague sentence in front of the operator.
    #[test]
    fn a_gate_says_who_is_adjudicating_or_that_nobody_is() {
        use crate::runtime::{Gate, NoBoundary};
        assert!(
            NoBoundary.describe().starts_with("none"),
            "the default must name the absence: {}",
            NoBoundary.describe()
        );
        let gate = AdjudicatedGate::new(Box::new(NoAdjudicator));
        assert_eq!(gate.describe(), NoAdjudicator.describe());
    }

    use super::*;
    use serde_json::json;

    fn call<'a>(name: &'a str, args: &'a Value) -> GateCall<'a> {
        GateCall {
            name,
            access: Access::Write,
            args,
            turn_id: "t1",
            call_id: "c1",
            workspace: "/w",
            target_exists: Some(true),
        }
    }

    #[test]
    fn with_no_adjudicator_a_write_refuses_and_does_not_claim_a_decision() {
        let mut g = AdjudicatedGate::closed();
        let args = json!({"path": "src/lib.rs"});
        match g.admit(&call("edit", &args)) {
            GateDecision::Refuse { outcome: ToolOutcome::NotRun { why }, .. } => {
                assert!(why.contains("fails closed"), "{why}");
                assert!(why.contains("nobody decided"), "{why}");
            }
            other => panic!("fail closed means refuse, got {other:?}"),
        }
        assert_eq!(g.log.len(), 1);
        assert_eq!(g.log[0].effect, "refuse");
    }

    #[test]
    fn an_allow_admits_and_a_session_grant_is_not_asked_twice() {
        let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let a = asked.clone();
        let adj = AskAdjudicator::new("test", move |req: &AdjudicationRequest| {
            a.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Some(AdjudicationDecision::selected(
                req,
                "allow_session",
                "human:test",
                "fine",
            ))
        });
        let mut g = AdjudicatedGate::new(Box::new(adj));
        let args = json!({"path": "src/lib.rs"});
        assert_eq!(g.admit(&call("edit", &args)), GateDecision::Admit);
        assert_eq!(g.admit(&call("edit", &args)), GateDecision::Admit);
        assert_eq!(
            asked.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "an allow_session must not be asked again"
        );
        assert_eq!(g.log.len(), 2, "both calls are rows, grant included");
    }

    #[test]
    fn a_deny_is_denied_and_carries_the_request_id() {
        let adj = AskAdjudicator::new("test", |req: &AdjudicationRequest| {
            Some(AdjudicationDecision::selected(
                req,
                "deny",
                "human:test",
                "not that file",
            ))
        });
        let mut g = AdjudicatedGate::new(Box::new(adj));
        let args = json!({"path": "src/lib.rs"});
        match g.admit(&call("edit", &args)) {
            GateDecision::Refuse { outcome: ToolOutcome::Denied { req_id }, .. } => {
                assert!(req_id.starts_with("adj-"), "{req_id}");
            }
            other => panic!("a deny is a decision, got {other:?}"),
        }
    }

    #[test]
    fn an_answer_naming_an_option_that_was_not_offered_never_opens_the_gate() {
        let adj = AskAdjudicator::new("test", |req: &AdjudicationRequest| {
            Some(AdjudicationDecision::selected(
                req,
                "yes_obviously",
                "model:small",
                "looks fine",
            ))
        });
        let mut g = AdjudicatedGate::new(Box::new(adj));
        let args = json!({"path": "src/lib.rs"});
        match g.admit(&call("edit", &args)) {
            GateDecision::Refuse { outcome: ToolOutcome::NotRun { why }, .. } => {
                assert!(why.contains("non-conforming") || why.contains("did not offer"), "{why}");
            }
            other => panic!("a non-conforming answer must not admit: {other:?}"),
        }
    }

    #[test]
    fn the_never_list_is_checked_before_the_adjudicator_and_it_cannot_be_overridden() {
        let consulted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c = consulted.clone();
        let adj = AskAdjudicator::new("test", move |req: &AdjudicationRequest| {
            c.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Some(AdjudicationDecision::selected(
                req,
                "allow_always",
                "human:test",
                "yes to everything",
            ))
        });
        let mut g = AdjudicatedGate::new(Box::new(adj));
        for path in [".ssh/authorized_keys", "home/x/.aws/credentials", ".git/HEAD"] {
            let args = json!({"path": path});
            assert!(
                matches!(g.admit(&call("write", &args)), GateDecision::Refuse { .. }),
                "{path} must be refused"
            );
        }
        assert_eq!(
            consulted.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "the never rows are evaluated before dispatch"
        );
    }

    #[test]
    fn nobody_answering_is_a_timeout_and_the_gate_still_refuses() {
        let adj = AskAdjudicator::new("test", |_: &AdjudicationRequest| None);
        let mut g = AdjudicatedGate::new(Box::new(adj));
        let args = json!({"path": "src/lib.rs"});
        match g.admit(&call("edit", &args)) {
            GateDecision::Refuse { outcome: ToolOutcome::NotRun { why }, .. } => {
                assert!(why.contains("timeout"), "{why}")
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_console_ask_is_a_real_ask_and_the_brief_is_what_a_human_reads() {
        let out = std::sync::Arc::new(Mutex::new(Vec::<u8>::new()));
        struct Shared(std::sync::Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Shared {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let adj = ConsoleAdjudicator::new(
            "deadtrickster",
            Box::new(std::io::Cursor::new(b"allow_once because I said so\n".to_vec())),
            Box::new(Shared(out.clone())),
        );
        let mut g = AdjudicatedGate::new(Box::new(adj));
        let args = json!({"path": "src/lib.rs", "old_string": "a", "new_string": "b"});
        assert_eq!(g.admit(&call("edit", &args)), GateDecision::Admit);

        let shown = String::from_utf8(out.lock().unwrap().clone()).unwrap();
        assert!(shown.contains("write,host_project,reversible,free"), "{shown}");
        assert!(shown.contains("allow_once"), "{shown}");
        assert!(shown.contains("old_string"), "{shown}");
        assert_eq!(g.log[0].decision.basis, "because I said so");
    }

    #[test]
    fn a_long_argument_is_summarised_in_the_brief_rather_than_pasted() {
        let big = "x".repeat(10_000);
        let args = json!({"path": "a.txt", "content": big});
        let mut g = AdjudicatedGate::closed();
        let req = g.request_for(&call("write", &args));
        let brief = req.brief();
        assert!(brief.len() < 4_000, "{} bytes", brief.len());
        assert!(brief.contains("bytes omitted"), "{brief}");
    }

    #[test]
    fn the_startup_disclosure_names_whichever_gate_is_shut() {
        // Three states, and only one of them is "on". A daemon that printed the
        // same line in all three would be asserting a property it does not have
        // in two of them.
        let (state, detail, active) = startup_disclosure("none attached", true, true);
        assert_eq!(state, "NONE");
        assert!(!active);
        assert!(detail.contains("WRITE TOOLS and no adjudicator"), "{detail}");

        let (state, detail, active) = startup_disclosure("console ask", false, true);
        assert_eq!(state, "GATE ONLY");
        assert!(!active);
        assert!(detail.contains("READ-ONLY"), "{detail}");

        let (_, detail, active) = startup_disclosure("console ask", true, true);
        assert!(active);
        assert!(detail.contains("takes effect"), "{detail}");

        // And the M1 sentence stays true for an M1 session.
        let (state, _, active) = startup_disclosure("none attached", false, false);
        assert_eq!(state, "N/A");
        assert!(!active);
    }

    /// **The sentence names the classes the session actually seated.**
    ///
    /// The defect this replaced: a caller whose gated tools were `Access::Exec` —
    /// the runner role's five job verbs and `monitor`, no `write` anywhere — had to
    /// pass `has_write_tools: true` to get the on/off logic right, and the banner
    /// then said *"Write tools are callable"* about a session with no write tools.
    /// Correct about the boundary and wrong about the session, which is the same
    /// shape as the four instances `harnessd`'s `GateWiring` already paid for.
    ///
    /// Nothing here pins a wording. What it pins is that the wording **moves** with
    /// the classes, and that a class the session does not have never appears.
    #[test]
    fn the_disclosure_names_the_classes_seated_and_not_write_by_default() {
        let exec = startup_disclosure_for("console ask", true, &["exec"], true).1;
        assert!(exec.contains("EXEC"), "{exec}");
        assert!(
            !exec.to_lowercase().contains("write"),
            "a session with no write tools was told its write tools are callable: {exec}"
        );

        let both = startup_disclosure_for("console ask", true, &["write", "network"], true).1;
        assert!(both.contains("WRITE"), "{both}");
        assert!(both.contains("NETWORK"), "{both}");
        assert_ne!(both, exec);

        // A read-only backend stops a *write* and says so in those terms; for an
        // exec or network seat it is a different mismatch and must not be described
        // as a blocked write.
        let shut_write = startup_disclosure_for("console ask", false, &["write"], true).1;
        assert!(shut_write.contains("cannot reach the disk"), "{shut_write}");
        let shut_exec = startup_disclosure_for("console ask", false, &["exec"], true).1;
        assert_ne!(
            shut_exec, shut_write,
            "an exec seat over a read-only backend is not a blocked write"
        );

        // Empty classes is still the M1 sentence, and it is the only state that
        // may claim nothing can reach the gate.
        assert_eq!(
            startup_disclosure_for("none attached", false, &[], true).0,
            "N/A"
        );
    }


    // -----------------------------------------------------------------------
    // The four outcome classes, at the gate.
    // -----------------------------------------------------------------------

    fn pinned() -> crate::intent::Surroundings {
        crate::intent::Surroundings {
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
        }
    }

    /// A gate that would admit anything an adjudicator is consulted about, so that a
    /// refusal in these tests can only have come from layer A.
    fn permissive_gate() -> AdjudicatedGate {
        AdjudicatedGate::new(Box::new(AskAdjudicator::new(
            "yes-to-everything",
            |req: &AdjudicationRequest| {
                Some(AdjudicationDecision::selected(
                    req,
                    "allow_once",
                    "human:test",
                    "yes to everything",
                ))
            },
        )))
        .with_surroundings(pinned())
        .with_trail_source(|_| {
            crate::authorise::AuthorisationTrail::from_messages(
                vec![crate::authorise::Utterance::operator("do whatever you like", 0)],
                1,
            )
        })
    }

    #[test]
    fn a_disclosure_is_refused_by_a_gate_that_would_otherwise_admit_everything() {
        let mut g = permissive_gate();
        let args = json!({"command": "/bin/cat /home/dead/.ssh/id_rsa"});
        match g.admit(&bash(&args)) {
            GateDecision::Refuse { outcome: ToolOutcome::Denied { .. }, tell } => {
                assert!(tell.contains("REFUSED"), "{tell}");
                assert!(tell.contains("Do not retry a variant"), "{tell}");
            }
            other => panic!("a disclosure must not be admitted: {other:?}"),
        }
        assert_eq!(g.log[0].request.tier.as_str(), "inexpressible");
    }

    #[test]
    fn rm_rf_slash_is_not_blocked_by_the_gate_and_reaches_a_decision() {
        // The operator's case: `rm -rf /` can be allowed if it is the intent. What the
        // gate must NOT do is refuse it on the strength of the string.
        let mut g = permissive_gate();
        let args = json!({"command": "/bin/rm -rf /"});
        assert_eq!(g.admit(&bash(&args)), GateDecision::Admit);
        assert_eq!(g.log[0].request.tier.as_str(), "always_ask");
        assert_eq!(g.log[0].effect, "admit");
    }

    #[test]
    fn an_always_ask_is_never_turned_into_a_standing_grant() {
        // The adjudicator asks for the widest grant it is allowed to name each time. A
        // class grant over a deletion outside the project is the standing authority the
        // fixed list withholds, so the second call must be asked about again.
        let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let a = asked.clone();
        let mut g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
            "human",
            move |req: &AdjudicationRequest| {
                a.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let widest = if req.option("allow_session").is_some() {
                    "allow_session"
                } else {
                    "allow_once"
                };
                Some(AdjudicationDecision::selected(req, widest, "human:test", "fine"))
            },
        )))
        .with_surroundings(pinned())
        .with_trail_source(|_| {
            crate::authorise::AuthorisationTrail::from_messages(vec![], 1)
        });
        let args = json!({"command": "/bin/rm -rf /home/dead/elsewhere"});
        assert_eq!(g.admit(&bash(&args)), GateDecision::Admit);
        assert_eq!(g.admit(&bash(&args)), GateDecision::Admit);
        assert_eq!(
            asked.load(std::sync::atomic::Ordering::Relaxed),
            2,
            "an always-ask is asked every time"
        );
        // And it was never even OFFERED a standing grant.
        assert!(g.log[0].request.option("allow_session").is_none());
        assert!(g.log[0].request.option("allow_always").is_none());

        // While an ordinary may-approve class grant still works, so this is a
        // restriction on the fixed list and not on grants in general.
        let ordinary = json!({"command": "/bin/rm -rf /w/target"});
        assert_eq!(g.admit(&bash(&ordinary)), GateDecision::Admit);
        assert_eq!(g.admit(&bash(&ordinary)), GateDecision::Admit);
        assert_eq!(asked.load(std::sync::atomic::Ordering::Relaxed), 3);
    }

    #[test]
    fn an_unresolvable_command_reaching_the_gate_is_not_run_and_never_admitted() {
        let mut g = permissive_gate();
        let args = json!({"command": "/bin/cat $FILE"});
        match g.admit(&bash(&args)) {
            GateDecision::Refuse { outcome: ToolOutcome::NotRun { why }, tell } => {
                assert!(why.contains("$FILE"), "{why}");
                assert!(tell.contains("nobody decided"), "{tell}");
            }
            other => panic!("unresolved is NotRun, never admit: {other:?}"),
        }
        assert!(!g.log[0].request.resolved);
        assert!(g.log[0].request.adjudicable().is_none());
    }

    #[test]
    fn a_bare_command_name_is_not_run_when_the_shell_was_not_declared_fixed() {
        // The alias defeat: `AdjudicatedGate::new` leaves `ShellTrust::Unknown`, so a
        // gate nobody configured refuses a name that something upstream could redefine.
        let mut g = AdjudicatedGate::new(Box::new(NoAdjudicator));
        let args = json!({"command": "ls -la"});
        match g.admit(&bash(&args)) {
            GateDecision::Refuse { outcome: ToolOutcome::NotRun { why }, .. } => {
                assert!(why.contains("alias"), "{why}")
            }
            other => panic!("{other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Surfacing, and the breaker.
    // -----------------------------------------------------------------------

    #[test]
    fn every_denial_reaches_the_operator_with_the_grant_path_attached() {
        // The operator's complaint, closed: they are told when it happens, and the
        // notice carries what they can do about it.
        let sink = std::sync::Arc::new(crate::authorise::RecordingDenialSink::default());
        struct Shared(std::sync::Arc<crate::authorise::RecordingDenialSink>);
        impl crate::authorise::DenialSink for Shared {
            fn denied(&self, n: &crate::authorise::DenialNotice) {
                self.0.denied(n);
            }
        }
        let mut g = AdjudicatedGate::closed()
            .with_surroundings(pinned())
            .with_denial_sink(Box::new(Shared(sink.clone())));
        let args = json!({"command": "/bin/rm /w/x.rs"});
        assert!(matches!(g.admit(&bash(&args)), GateDecision::Refuse { .. }));
        assert_eq!(sink.len(), 1);
        let n = sink.last().unwrap();
        assert_eq!(n.outcome, "not_run");
        assert!(n.grant.contains("grant"), "{}", n.grant);
        assert!(n.grant.contains("Nothing was executed"), "{}", n.grant);
        assert!(n.headline().starts_with("REFUSED bash"));
    }

    #[test]
    fn three_refusals_in_one_direction_open_the_breaker_and_stop_asking() {
        // What stops a classifier being argued down one call at a time: after three,
        // the gate does not produce another verdict at all.
        let consulted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c = consulted.clone();
        let mut g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
            "no",
            move |req: &AdjudicationRequest| {
                c.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Some(AdjudicationDecision::selected(req, "deny", "human:test", "no"))
            },
        )))
        .with_surroundings(pinned())
        .with_trail_source(|_| crate::authorise::AuthorisationTrail::from_messages(vec![], 1));

        // Three re-spellings of one intention. The counter must not be reset by the
        // re-spelling, which is the whole point of the direction key.
        for cmd in [
            "/bin/cat /w/secret-ish.txt",
            "/usr/bin/head /w/other.txt",
            "/usr/bin/tail /w/third.txt",
        ] {
            let args = json!({"command": cmd});
            assert!(matches!(g.admit(&bash(&args)), GateDecision::Refuse { .. }), "{cmd}");
        }
        assert_eq!(consulted.load(std::sync::atomic::Ordering::Relaxed), 3);

        // The fourth is not adjudicated at all.
        let args = json!({"command": "/usr/bin/less /w/fourth.txt"});
        match g.admit(&bash(&args)) {
            GateDecision::Refuse { outcome: ToolOutcome::NotRun { why }, tell } => {
                assert!(why.contains("circuit breaker is open"), "{why}");
                assert!(tell.contains("breaker is OPEN"), "{tell}");
                assert!(tell.contains("only the operator can lift it"), "{tell}");
            }
            other => panic!("the fourth must not reach the adjudicator: {other:?}"),
        }
        assert_eq!(
            consulted.load(std::sync::atomic::Ordering::Relaxed),
            3,
            "an open breaker consults nobody"
        );
    }

    #[test]
    fn an_admission_clears_the_direction_so_the_breaker_is_not_a_permanent_ban() {
        let answers = std::sync::Arc::new(Mutex::new(vec!["deny", "deny", "allow_once", "deny"]));
        let a = answers.clone();
        let mut g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
            "mixed",
            move |req: &AdjudicationRequest| {
                let mut q = a.lock().unwrap();
                let pick = if q.is_empty() { "deny" } else { q.remove(0) };
                Some(AdjudicationDecision::selected(req, pick, "human:test", "…"))
            },
        )))
        .with_surroundings(pinned())
        .with_trail_source(|_| crate::authorise::AuthorisationTrail::from_messages(vec![], 1));
        let args = json!({"command": "/bin/cat /w/a.txt"});
        assert!(matches!(g.admit(&bash(&args)), GateDecision::Refuse { .. }));
        assert!(matches!(g.admit(&bash(&args)), GateDecision::Refuse { .. }));
        assert_eq!(g.admit(&bash(&args)), GateDecision::Admit);
        // The loop closed, so the count is gone and the next refusal starts over.
        assert!(matches!(g.admit(&bash(&args)), GateDecision::Refuse { .. }));
        assert!(g.breaker.open_directions().is_empty());
    }

    #[test]
    fn a_second_attempt_at_the_same_thing_is_named_as_one_rather_than_read_as_fresh() {
        let mut g = AdjudicatedGate::closed().with_surroundings(pinned());
        let args = json!({"command": "/bin/rm /w/x.rs"});
        let _ = g.admit(&bash(&args));
        match g.admit(&bash(&args)) {
            GateDecision::Refuse { tell, .. } => {
                assert!(tell.contains("attempt 2"), "{tell}");
                assert!(tell.contains("go to a human"), "{tell}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_trail_source_is_what_puts_the_operators_words_in_front_of_an_adjudicator() {
        // The seam §2 needs. Without it the trail says NOT COLLECTED, which is a
        // different fact from the operator having said nothing.
        let mut bare = AdjudicatedGate::closed().with_surroundings(pinned());
        let args = json!({"command": "/bin/ls /w"});
        let req = bare.request_for(&bash(&args));
        assert!(!req.trail.was_collected());
        assert!(req.trail.render().contains("NOT COLLECTED"));

        let mut wired = AdjudicatedGate::closed()
            .with_surroundings(pinned())
            .with_trail_source(|_| {
                crate::authorise::AuthorisationTrail::from_messages(
                    vec![crate::authorise::Utterance::operator("yeah restart it", 1).at_seconds(30)],
                    18,
                )
            });
        let req = wired.request_for(&bash(&args));
        assert!(req.trail.was_collected());
        assert!(req.trail.render().contains("1 operator message(s) of 18"));
        assert!(req.trail.render().contains("yeah restart it"));
    }

    #[test]
    fn escalation_is_refused_with_the_reason_and_not_treated_as_an_allow() {
        let adj = AskAdjudicator::new("test", |req: &AdjudicationRequest| {
            Some(AdjudicationDecision {
                request_id: req.id.clone(),
                outcome: DecisionOutcome::Escalate {
                    to: "human".into(),
                    why: "I am not sure".into(),
                },
                by: "model:small".into(),
                basis: "unsure".into(),
                latency_ms: 1,
            })
        });
        let mut g = AdjudicatedGate::new(Box::new(adj));
        let args = json!({"path": "src/lib.rs"});
        match g.admit(&call("edit", &args)) {
            GateDecision::Refuse { outcome: ToolOutcome::NotRun { why }, .. } => {
                assert!(why.contains("escalated"), "{why}")
            }
            other => panic!("{other:?}"),
        }
    }
}
