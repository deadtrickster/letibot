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
        if rank(&other) > rank(&self) {
            other
        } else {
            self
        }
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

/// The ladder for an action on [`crate::intent::ALWAYS_ASK`]: **no standing grant**.
///
/// A third mechanism for one property, and deliberately so. The tier withholds the
/// [`Adjudicable`] witness, [`AdjudicatedGate::admit`] refuses to insert a class grant
/// for one, and the prompt does not offer the option in the first place — so an
/// operator is not shown a button whose effect the gate would then decline to honour,
/// and an adjudicator cannot select one. A safety property with one mechanism ships
/// broken the first time somebody refactors the mechanism.
/// **The ladder for a path the boundary hid.** Three answers, because the choice
/// an operator actually has is not allow/deny: a path can come in readable, which
/// is enough to look at something, or writable, which is what creating a symlink
/// takes. Offering one button for both would make them pick the wider one to get
/// anything done.
///
/// Read-only is first, and is the one Enter lands on: it is the narrower of the
/// two admissions and the common case.
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
            id: "allow_always".into(),
            label: "Always allow this (written to ~/.config/letibot/permission.json)".into(),
            kind: OptionKind::AllowAlways,
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

/// The ladder at a point whose grants are [`crate::mode::GrantScope::Once`].
///
/// The ladder for an exec-class call: no session grant (the operator's rule of
/// 2026-09-11, a shell asks every time), and *Always allow* as a durable rule
/// over the program and its verb (the revision of 2026-09-14).
pub fn exec_options() -> Vec<DecisionOption> {
    vec![
        DecisionOption {
            id: "allow_once".into(),
            label: "Allow this one".into(),
            kind: OptionKind::AllowOnce,
        },
        DecisionOption {
            id: "allow_always".into(),
            label:
                "Always allow this program and verb (written to ~/.config/letibot/permission.json)"
                    .into(),
            kind: OptionKind::AllowAlways,
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

/// Same as [`permission_options`] without `allow_session`, and the label says where
/// the missing option went rather than leaving its absence to be guessed at. An
/// operator who wants to stop being asked needs a different point, not a different
/// answer, and naming it is the difference between a dead end and a next step.
pub fn once_only_options(mode_name: &'static str) -> Vec<DecisionOption> {
    vec![
        DecisionOption {
            id: "allow_once".into(),
            label: "Allow this one".into(),
            kind: OptionKind::AllowOnce,
        },
        DecisionOption {
            id: "allow_always".into(),
            label: "Always allow this (written to ~/.config/letibot/permission.json)".into(),
            kind: OptionKind::AllowAlways,
        },
        DecisionOption {
            id: "deny".into(),
            label: "Deny".into(),
            kind: OptionKind::Deny,
        },
        DecisionOption {
            id: "deny_and_tell".into(),
            label: format!(
                "Deny, and tell the model why  (mode `{mode_name}` settles one call at a \
                 time; start with `--mode writes-allowed` to allow a class for the session)"
            ),
            kind: OptionKind::DenyAndTell,
        },
    ]
}

/// One aggregated line of R11's history: what the gate did before, on actions of
/// this same task direction.
///
/// Aggregated **by the gate's own effect** — `admit` or `refuse` — because that is
/// the split the discipline cares about: *"a denial is history too"*, and a brief
/// that showed only the approvals would be telling the oracle a one-sided story
/// about its own record. The counts and the ages travel with them (*"once, three
/// weeks ago"* and *"nine times this week"* are different facts, and a bare
/// *"previously allowed"* hides which); the ages are turn distances, which is the
/// honest unit for an in-session log. There is no field here for the arguments or
/// the decision prose — history is evidence about the shape, not a transcript of
/// the calls, and the raw text must never reach the oracle through the back door.
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
    /// **The one thing the decision is about**, alone and unquoted: the command a
    /// `bash` call would run, the path a write would take, the URL a fetch reaches.
    ///
    /// Carried beside `summary` rather than only inside it, because a head has to be
    /// able to put it where the eye lands — and because a sentence with a shell line
    /// interpolated into the middle of it is the one shape a person cannot skim.
    pub target: String,
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
    /// **R11: prior answers on actions of this shape**, read back from the audit
    /// rows at decision time and carried to the brief. Set only by the gate, in
    /// [`AdjudicatedGate::request_from`], keyed the way
    /// [`crate::authorise::TaskDirection`] is — never by a caller, and never read
    /// for a tier or an admission: the four inputs history feeds is the *brief*,
    /// and the tier is layer A's alone.
    pub prior: Vec<PriorAnswer>,
    /// **What this operator answered themselves, earlier in this session.** The
    /// action and the verdict, not a count — so the guard has worked examples of
    /// this person's judgement rather than a tally. Filled here, from the audit
    /// log, exactly as `prior` is; see [`crate::authorise::DecisionExample`].
    pub examples: Vec<crate::authorise::DecisionExample>,
    /// Which wording of the guard's question this request will be briefed with.
    /// The shipping one everywhere; `--calibrate --compare` is the only caller
    /// that sets the other, and it sets it to measure the difference.
    pub brief_variant: crate::authorise::BriefVariant,
    /// **What the model already said about this**, when a model was asked first.
    ///
    /// Filled only by [`SupervisedAdjudicator`], and `None` everywhere else — at
    /// [`crate::mode::Mode::AUTO`] the model *is* the decider and there is nobody
    /// downstream to advise, at `always-ask` no model was consulted at all. Those
    /// are different facts and `None` covers neither of them alone, which is why the
    /// point that fills it is the point that says so in its name.
    ///
    /// Advice, never a decision: nothing reads this to admit anything. The gate
    /// admits on the *human's* answer and records this beside it, so a row where the
    /// two differ keeps both halves.
    pub advice: Option<ModelAdvice>,
}

/// The model's verdict, carried to the person who is about to overrule it or agree
/// with it.
///
/// > *"we literally want /mode supervised that will ask model and ask me if i agree
/// > or not"*
///
/// This is the payload of "if i agree or not": you cannot agree with something you
/// were not shown. It travels on the request rather than being re-derived by a head,
/// for [`ModelBrief`](crate::authorise::ModelBrief)'s reason one layer down — a head
/// that reconstructed the verdict would be rendering a guess about what the model
/// said.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelAdvice {
    /// **Whether an oracle was actually consulted.**
    ///
    /// `false` covers every case where a model adjudicator answered *without asking
    /// a model*: an unresolved action, an always-ask entry, an inexpressible one, an
    /// uncollected trail, an intent outside the oracle's earned scope. Those are
    /// layer A's answers arriving through layer B's door, and they are not verdicts.
    ///
    /// Load-bearing for the corpus: [`agreement`] fires only when this is `true`.
    /// Without it a row whose "verdict" was `always-ask short-circuit` would be
    /// labelled as the operator agreeing or disagreeing with an opinion nothing
    /// held — manufactured signal, and the worst kind, because it looks like data.
    pub consulted: bool,
    /// `admit`, `ask` or `unavailable` — what the model's answer would have done on
    /// its own. Not an option id: at this point the model has not selected from the
    /// ladder, it has answered *did the operator ask for this*.
    pub would: &'static str,
    /// Which oracle, in its own words.
    pub by: String,
    /// Why. The sentence a person reads before agreeing.
    pub basis: String,
    /// **Which of the operator's own utterances it relied on.** Empty is a real
    /// answer and a loud one: an authorisation that cites nothing is one the oracle
    /// could not ground, and a person should weigh it differently.
    pub cites: Vec<String>,
    /// How long it took, against the budget.
    pub latency_ms: u64,
}

impl ModelAdvice {
    /// One line for a head that has room for one line.
    ///
    /// An unconsulted oracle says so rather than presenting layer A's short-circuit
    /// as a verdict. A person reading `model says ask` about a call no model saw
    /// would weigh their own answer against nothing.
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

/// **Which rendering of [`AdjudicationRequest::brief`] a corpus row was produced
/// under.** Stamped on every row.
///
/// Bump it whenever `brief` changes what it emits. Rows carrying different values
/// are two datasets: the same decision shown under two briefs is two different
/// questions, and a trainer pooling them fits the seam between them. A date is
/// enough of an identifier and is one nobody has to look up.
pub const BRIEF_FORMAT: &str = "brief/2026-09-14";

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
    Selected {
        option_id: String,
    },
    /// First class rather than an error path (§11.4).
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

    /// **What a model said about the last call, when a model was consulted first.**
    ///
    /// [`SupervisedAdjudicator`]'s, and `None` for every other adjudicator. The
    /// same shape as `last_brief` and for the same reason: the gate builds the
    /// request and the adjudicator is what learns something about it, so an answer
    /// the adjudicator produced downstream has to be asked for rather than read off
    /// the request the gate still holds.
    ///
    /// The gate copies it into the corpus row's `model_verdict`. Without it that
    /// column holds whatever *decided*, which at a supervised point is the person —
    /// and a corpus in which the model and the operator never disagree is a corpus
    /// with no signal in it at all.
    fn last_advice(&self) -> Option<ModelAdvice> {
        None
    }

    /// **Ask the model and nobody else.**
    ///
    /// For the one path that has already been decided and still wants a verdict: a
    /// standing `allow_session` covers the call, so there is nothing to ask a person,
    /// and the model's opinion on it is a free labelled example against a ruling the
    /// operator has already made.
    ///
    /// `None` for every adjudicator that has no model in it, which is the honest
    /// answer — *nobody was consulted* — and not an empty verdict.
    ///
    /// It must not block on a person. [`SupervisedAdjudicator`] implements it by
    /// running its model half alone, and the default here runs nothing at all.
    fn advise_only(&self, _req: &AdjudicationRequest) -> Option<ModelAdvice> {
        None
    }

    /// **The glob the operator typed with their answer**, when they typed one.
    ///
    /// > *"please add globbing to my answers somehow too"*
    ///
    /// Read by the gate when an answer is `AllowAlways`, which is the only option
    /// that writes a rule; ignored on every other option id rather than quietly
    /// widening one. `None` means *derive the pattern from the call*, which is what
    /// every answer did before this existed — never *match nothing*.
    ///
    /// The same shape as `last_brief` and `last_advice`, for the same reason: this
    /// arrives with the answer, and the answer reaches the adjudicator rather than
    /// the gate. Threading it through `AdjudicationDecision` instead would have
    /// touched forty-four literals to carry a value that is `None` in forty-three
    /// of them.
    fn last_pattern(&self) -> Option<String> {
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
                        basis: format!("`{chosen}` is not one of the options this request offered"),
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
pub type TrailSource = dyn Fn(&GateCall<'_>) -> crate::authorise::AuthorisationTrail + Send + Sync;

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
    /// What a model said about this call **before** the person answered, at a
    /// supervised point. `None` everywhere else, and never the decider's own answer
    /// under another name.
    pub advice: Option<ModelAdvice>,
    /// The circuit breaker's key for this action — see
    /// [`crate::authorise::TaskDirection`]. Carried so an operator lifting a refusal
    /// they were shown does not have to reconstruct which direction it was in.
    pub direction: String,
    /// The named point in mode-space at the moment of the decision — not the
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
            baseline: self.request.baseline.clone(),
            tier: self.request.tier.as_str(),
            tool: self.request.tool.clone(),
            arguments: self.request.arguments.clone(),
            mode: self.mode.clone(),
            options: self.request.options.iter().map(|o| o.id.clone()).collect(),
            agent: self.request.agent.clone(),
            // **The model's verdict, and never the human's wearing its name.**
            //
            // At `supervised` the decision belongs to the person and the advice
            // belongs to the oracle, and writing the person's answer into
            // `model_verdict` would produce a corpus in which the two never
            // disagree — every row self-consistent, every row worthless. So when
            // advice is present it is what this column holds.
            //
            // Everywhere else the decider IS what answered, and the column holds
            // that. The two cases are one line apart and are the reason this is not
            // `Some(format!(...))` unconditionally, which is what it was.
            model_verdict: match &self.advice {
                Some(a) => Some(format!("{} by {}: {}", a.would, a.by, a.basis)),
                None => Some(format!(
                    "{} by {}: {}",
                    self.decision.outcome.as_str(),
                    self.decision.by,
                    self.decision.basis
                )),
            },
            // What actually settled the call, always — advice or no advice.
            verdict: Some(self.decision.outcome.as_str().to_string()),
            verdict_by: Some(self.decision.by.clone()),
            verdict_basis: Some(self.decision.basis.clone()),
            // Not yet produced by any oracle — see `CorpusRow::p_allow`. `None`
            // here is *no confidence was reported*, and must never be read as
            // low confidence.
            p_allow: None,
            decision_ms: self.decision.latency_ms,
            brief_format: BRIEF_FORMAT,
            effect: self.effect,
            asked: self.asked,
            operator: self.operator.clone(),
        }
    }
}

/// **Did the person agree with the model?** — in the corpus's own three words.
///
/// The vocabulary is [`crate::authorise::OperatorOverride`]'s and it is about the
/// *gate*, not about politeness: `Granted` is "it would not have run and I let it",
/// `Revoked` is "it would have run and I stopped it", `Upheld` is "we agree". So the
/// comparison is between what the model's answer would have done and what actually
/// happened, and nothing here reads the model's prose.
///
/// `would == "ask"` and `would == "unavailable"` are both *not an admission*: the
/// oracle declined to authorise. Admitting over either of them is the operator
/// supplying an authorisation the model could not find, which is `Granted`.
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

/// The `#N` sequence at the end of a turn id, if it has one.
///
/// Turn ids are `{transcript}#{n}` where the harness names them and arbitrary
/// where tests do; a tail that will not parse is an honest `None`, and the
/// renderer says the age is unknown rather than inventing a number. `rsplit`
/// because a transcript id may itself contain `#`.
fn turn_seq(turn_id: &str) -> Option<u64> {
    turn_id.rsplit('#').next()?.parse().ok()
}

/// **What a grant is keyed on**: the program the shell will actually run.
///
/// For a shell command it is the *last* stage's program name, which is a choice worth
/// naming: a pipeline's stages can differ, and the tier is already the strictest of
/// them (`Tier::strictest`), so the grant is keyed on the stage a person would name
/// when they say what the command was. A pipeline whose stages differ from a granted
/// one still falls out, because the **intent set** is the union across stages and the
/// grant covers only what it was shown.
///
/// For a path-shaped tool call there is no program, and the tool's own name is not one
/// — so it is `"<tool>"`, which matches nothing a command produces. A grant taken over
/// `bash` must not silently cover a `write` call.
fn grant_program(baseline: &crate::intent::Baseline) -> String {
    baseline
        .command
        .as_ref()
        .and_then(|n| n.stages.last())
        .and_then(|s| s.program_name())
        .map(str::to_string)
        .unwrap_or_else(|| "<tool>".to_string())
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
    /// **Where this session sits in the four-dimensional space**, as a named point.
    ///
    /// [`crate::mode::Mode::ALWAYS_ASK`] by default, which is what an unseen project
    /// gets: nothing that is not a read happens without the operator. The daemon
    /// overrides it per project root.
    mode: crate::mode::Mode,
    /// Standing permissions taken from an `allow_session` answer, keyed on
    /// **`(program, ActionClass, intents)`** rather than on `tool + class`.
    ///
    /// The old key's own comment was right about the half it argued — *"a grant keyed
    /// by the arguments would be a grant for one call, which is `allow_once`"* — and
    /// it was keyed one notch too coarse in the other direction. `tool + class` for
    /// `bash` is a grant over every command `bash` can run that lands in one class,
    /// and the program is exactly the thing a person means when they say *"stop
    /// asking me about git"*. See [`crate::grant`], and note that the intent set is in
    /// the key because that is what an execution vehicle changes.
    grants: Vec<crate::grant::Grant>,
    /// opencode's permission model, verbatim: the operator's `permission` config
    /// plus the `always` approvals, evaluated before the mode. `allow` admits,
    /// `deny` refuses, `ask` falls through to the mode and the adjudicator. See
    /// [`crate::permission`].
    permission: crate::permission::Ruleset,
    /// Where an *Always allow* answer is written down so it outlives the process
    /// — the daemon hands in `~/.config/letibot/permission.json`. Without one the
    /// answer holds for the session and the row says so.
    permission_sink: Option<
        std::sync::Arc<dyn Fn(&crate::permission::Rule) -> Result<(), String> + Send + Sync>,
    >,
    /// **opencode parity: exec follows the mode.**
    ///
    /// Off by default, which is the operator's rule (2026-09-11): no point admits an
    /// exec-class call unasked, because `bash`'s result is an arbitrary byte stream.
    /// leticode turns this on — opencode has no such carve-out; its `bash` is a
    /// permission like any other, so `bypassPermissions` admits it and `acceptEdits`
    /// still asks. This flag makes `allow-all` admit exec and leaves every other point
    /// exactly as it was.
    exec_follows_mode: bool,
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
    /// What a model said about the call in flight, at a supervised point. Taken from
    /// [`Adjudicator::last_advice`] beside `shown` and moved into the row the same
    /// way.
    advice: Option<ModelAdvice>,
    /// Set when the call in flight was settled by a **standing grant** rather than by
    /// somebody answering now: the grant's own reason. Moved into the row's operator
    /// note, so a label that came from a ruling made turns ago says which ruling.
    standing: Option<String>,
    /// **The guard model**, when one is configured. Held whether or not supervision
    /// is on, so turning it on is a flag and not a rebuild.
    advisor: Option<std::sync::Arc<dyn Adjudicator>>,
    /// Whether [`AdjudicatedGate::advisor`] gets a turn on every call.
    ///
    /// A field rather than a mode, which is the whole design:
    ///
    /// > *"supervised mode is essentially a normal mode but we always ask model"*
    ///
    /// The mode decides what asks and is fixed when a session opens, because the
    /// tools seated under it are. This decides whether the model gets a turn before
    /// the answer, and nothing about the session's shape depends on it — so
    /// `/supervise` moves it mid-session without rebuilding a gate, re-seating a
    /// tool, or deciding what happens to grants taken under a different point.
    supervise: bool,
    /// Where corpus rows go so they outlive this process. `None` means the corpus is
    /// being discarded, and the disclosure says so rather than implying it is kept.
    corpus_sink: Option<std::sync::Arc<dyn crate::authorise::CorpusSink>>,
}

impl AdjudicatedGate {
    pub fn new(adjudicator: Box<dyn Adjudicator>) -> Self {
        AdjudicatedGate {
            adjudicator,
            session_id: "session".into(),
            agent: "agent".into(),
            mode: crate::mode::UNSEEN_PROJECT,
            grants: Vec::new(),
            permission: crate::permission::Ruleset::new(),
            permission_sink: None,
            exec_follows_mode: false,
            log: Vec::new(),
            seq: 0,
            surroundings: crate::intent::Surroundings::default(),
            trail_source: None,
            denials: None,
            breaker: crate::authorise::Breaker::default(),
            shown: None,
            advice: None,
            standing: None,
            advisor: None,
            supervise: false,
            corpus_sink: None,
        }
    }

    /// **opencode parity**: make exec follow the mode instead of always asking. See
    /// [`AdjudicatedGate::exec_follows_mode`]. Off by default; leticode turns it on.
    pub fn with_exec_follows_mode(mut self, v: bool) -> Self {
        self.exec_follows_mode = v;
        self
    }

    /// **Put this session at a named point.** See [`crate::mode`].
    ///
    /// The gate does not check the point's prerequisites — the caller does, before it
    /// builds anything, because a prerequisite check that ran here would be a session
    /// that has already opened, printed a banner and seated tools by the time it finds
    /// out it cannot honour the point it named.
    pub fn with_mode(mut self, mode: crate::mode::Mode) -> Self {
        self.mode = mode;
        self
    }

    /// Install opencode's permission ruleset (the operator's `permission` config).
    /// Evaluated before the mode: `allow` admits, `deny` refuses, `ask` falls
    /// through. Later rules override earlier ones, so a config here is consulted
    /// before any `always` approvals the caller appends.
    pub fn with_permission(mut self, rules: crate::permission::Ruleset) -> Self {
        self.permission = rules;
        self
    }

    /// Where *Always allow* writes its rule. See the field.
    pub fn with_permission_sink(
        mut self,
        sink: std::sync::Arc<dyn Fn(&crate::permission::Rule) -> Result<(), String> + Send + Sync>,
    ) -> Self {
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

    /// opencode's permission ruleset in force — the config plus any `always`
    /// approvals. A subagent inherits this, so the same allow/deny/ask rules govern
    /// its calls rather than starting from scratch.
    pub fn permission(&self) -> &crate::permission::Ruleset {
        &self.permission
    }

    /// The standing permissions in force, for a listing. A grant nobody can see is a
    /// permanent widening nobody remembers making.
    pub fn grants(&self) -> &[crate::grant::Grant] {
        &self.grants
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

    /// **Install the guard model.** Held, not switched on: `/supervise` does that,
    /// and it can only do it if this was installed when the session opened.
    pub fn with_advisor(mut self, advisor: std::sync::Arc<dyn Adjudicator>) -> Self {
        self.advisor = Some(advisor);
        self
    }

    /// Start supervised. `--supervise` on the command line; `/supervise` changes it
    /// later.
    ///
    /// Named apart from [`Gate::supervising`], which asks the question this answers —
    /// a builder and a getter sharing one name is how a caller ends up reading the
    /// wrong one.
    pub fn start_supervised(mut self, on: bool) -> Self {
        self.supervise = on;
        self
    }

    /// **Install the corpus sink.** Without one every decision this gate makes is
    /// kept in `log` and lost when the process ends.
    pub fn with_corpus_sink(
        mut self,
        sink: std::sync::Arc<dyn crate::authorise::CorpusSink>,
    ) -> Self {
        self.corpus_sink = Some(sink);
        self
    }

    /// Whether decisions are being written anywhere durable. For the startup
    /// disclosure, which must not imply a corpus that is not being kept.
    pub fn corpus_is_kept(&self) -> bool {
        self.corpus_sink.is_some()
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
            // **Offer the remedy that exists.** A refusal the HARNESS made — the
            // normaliser could not read the command, the host boundary refused it —
            // is not a refusal a grant can lift: no adjudicator was consulted and
            // none would be on a retry, so telling the operator to grant it invites
            // them to grant something that changes nothing, and telling the MODEL to
            // ask for one invites a retry loop around a command whose meaning still
            // does not exist. What helps there is in the basis above: re-issue it in
            // a form the grammar can read.
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
            } else {
                format!(
                    "grant `{}` ({}) for this session, or answer the pending decision. \
                     Nothing was executed and nothing changed.",
                    req.id, req.tool
                )
            },
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

    pub fn with_identity(
        mut self,
        session_id: impl Into<String>,
        agent: impl Into<String>,
    ) -> Self {
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
            facts.push(
                "this creates a file that does not exist, so there is nothing to restore".into(),
            );
        }
        if let Some(op) = call.args.get("op").and_then(|v| v.as_str()) {
            // A tool that dispatches on an op is one tool to the gate and ten
            // actions to whoever is deciding. The op goes in the facts because the
            // routing key cannot carry it (§11.3's class has no op field), and a
            // human reading `github` alone cannot tell a listing from a merge.
            facts.push(format!("operation: `{op}`"));
        }
        // R11, before the request exists: the same key the row will be recorded
        // under, computed from the parts this function already has, so the history
        // in the brief and the history the next call reads back can never be keyed
        // differently. Reading the rows happens **here** — at decision time, per
        // §4h — not at startup and not in a batch job nobody consults.
        let direction = crate::authorise::TaskDirection::of_parts(call.name, baseline, class.scope);
        let prior = Self::prior_answers(&self.log, &direction.key(), turn_seq(call.turn_id));
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
            brief_variant: crate::authorise::BriefVariant::Follows,
            // Filled by `SupervisedAdjudicator` between the model's answer and the
            // person's, and by nothing else. The gate does not consult a model on
            // its own.
            advice: None,
            summary: format!(
                "`{}` wants {} access to `{target}`",
                call.name,
                call.access.as_str()
            ),
            target: target.clone(),
            arguments: call.args.clone(),
            arguments_digest: digest,
            boundary_facts: facts,
            kind: RequestKind::Permission,
            // **Offer only what the gate will honour.** The recording site below
            // requires BOTH that the tier is not `AlwaysAsk` AND that the mode grants
            // for the session; this used to test only the first, so at a point with
            // `GrantScope::Once` -- which `always-ask`, the default, is -- an operator
            // was shown *"Allow this class for the rest of the session"*, chose it, and
            // the grant was silently dropped.
            //
            // Reported twice before it was found here, and the comment at the recording
            // site already stated the rule this violated: *"an operator is never shown a
            // button whose effect the gate would then decline to honour."* Two guards
            // for one decision, and only one of them was kept in step.
            options: if matches!(baseline.tier, Tier::AlwaysAsk { .. }) {
                always_ask_options()
            } else if call.access == Access::Exec {
                // The operator's rule (2026-09-11): exec asks every time, so a SESSION
                // grant is never offered. Revised 2026-09-14 — *"good old Allow Always"*:
                // a durable rule, written to a file the operator reads and edits, is
                // the operator's own preapproval and is offered. `git log; rm x` under
                // a `git log*` rule still asks: the rule is tested per segment.
                exec_options()
            } else if self.mode.grants == crate::mode::GrantScope::Session {
                permission_options()
            } else {
                once_only_options(self.mode.name)
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

    /// **A standing rule settled this call. Ask the model anyway, and mark the row.**
    ///
    /// > *"supervised mode is essentially a normal mode but we always ask model. and
    /// > model has its turn regardless of glob, deny, allow, allow_session or glob
    /// > deny or allow from config"*
    ///
    /// That is the rule, and this is every place it has to be applied. `admit` has
    /// seven ways to settle a call before reaching an adjudicator; six of them call
    /// this, and each one is a place the model would otherwise never get a turn:
    ///
    /// | | settled by |
    /// |---|---|
    /// | 1a | layer A could not resolve the action |
    /// | 1b | an inexpressible tier — a secret crossing the boundary |
    /// | 1  | the never-write list |
    /// | 1.5 | a `permission` config `deny` |
    /// | 1.5 | a `permission` config `allow` |
    /// | 2  | the mode admits this class unasked |
    /// | 3  | a standing `allow_session` grant |
    ///
    /// The seventh is the circuit breaker, and it is the one deliberate exception —
    /// see the comment at its own return. A rule is the operator's standing answer,
    /// so the oracle's verdict on a call it already settled is a labelled example
    /// that costs nobody a keystroke; and a refusal is worth as much as an
    /// admission, because a model that wants to admit what the operator's rules
    /// refuse is exactly the miscalibration to find BEFORE automode.
    ///
    /// **Nothing read here changes the outcome.** Every caller has already decided
    /// by the time this runs. An oracle that is slow costs the wait, one that is down
    /// costs nothing, one that disagrees is recorded disagreeing.
    ///
    /// Off at every point but `supervised`: elsewhere there is no model to ask, and
    /// spending an oracle round trip on a call a rule already settled would be the
    /// gate paying latency for a row nobody asked for.
    fn advise_on_a_settled_call(&mut self, req: &AdjudicationRequest, why: String) {
        self.advice = self.ask_the_advisor(req);
        if self.advice.is_some() {
            self.standing = Some(why);
        }
    }

    /// The guard model's verdict on this call, or `None` when supervision is off or
    /// no advisor is installed.
    ///
    /// One function so that "is the model consulted" has one answer: every settled
    /// path and the ask path go through it, and a future path that forgets to call
    /// it is a path that visibly records no verdict rather than one that quietly
    /// half-supervises.
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
        }))
    }

    fn record(
        &mut self,
        request: AdjudicationRequest,
        decision: AdjudicationDecision,
        effect: &'static str,
        direction: String,
    ) {
        let shown = self.shown.take();
        // **Asked is read off who decided, not off the adjudicator's name.** A
        // human adjudicator that timed out did not ask anybody, and a model
        // adjudicator that escalated did. `by` is the only field that records
        // which of those happened.
        let asked = decision.by.starts_with("human");
        let mode = self.mode.name.to_string();
        let advice = self.advice.take();
        let standing = self.standing.take();
        // **At `supervised` the label is already in hand, so it is written now.**
        //
        // Everywhere else `operator` is filled later by `record_override` or by
        // `/gate`, because the person has not spoken yet. Here they just did, on
        // this exact decision, with the model's verdict in front of them — which is
        // the whole reason to sit through the point. Making them re-rule it from a
        // list afterwards would be asking the same question twice and getting a
        // worse answer the second time.
        // **The label**, when there is one to write.
        //
        // Two ways the operator's ruling is already in hand at this moment, and both
        // are recorded — the difference between them travels in `asked` and in the
        // note, never by dropping one of them:
        //
        //   `asked`     they answered THIS call, with the verdict in front of them
        //   `standing`  a grant they gave earlier covers it, and nobody was asked
        //
        // Neither is invented. With no advice there is nothing to have agreed with,
        // and the row stays unlabelled for `/gate` or `record_override` to fill.
        let operator = match (&advice, asked, &standing) {
            // **No label without a verdict.** `consulted` is false whenever a model
            // adjudicator answered without asking a model — an always-ask entry, an
            // unresolved action, an intent outside the oracle's scope. Agreeing with
            // a short-circuit is not agreeing with anything.
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

    /// **R11: the audit rows, read back.** §4h: *"the audit rows are written and
    /// never read — closing that loop is the cheapest large improvement
    /// available."*
    ///
    /// Every prior row in the same [`crate::authorise::TaskDirection`], aggregated
    /// by the gate's effect, oldest first. The discipline lives in the two rules
    /// this function cannot break and the renderer repeats: the rows are **shown
    /// to the decision, not substituted for it** (this returns data for a brief;
    /// nothing here touches `tier`, `options` or the admit path), and **denials
    /// are history too** (the filter is the direction, never the outcome).
    /// **The last few calls this operator answered themselves**, newest first.
    ///
    /// Not filtered to the shape of the call being decided, which is what `prior`
    /// does: the point is to show what this person is LIKE, and a tally of the same
    /// shape says nothing about that. It is bounded because a brief is a prompt —
    /// the operator's own rule about capping a corrective body applies to evidence
    /// too, and an unbounded list would push the trail out of the model's attention.
    ///
    /// Only rows a human actually answered. A gate:mode admit is the mode's
    /// judgement, not theirs, and showing it as "they allowed" would put words in
    /// their mouth.
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
                // **What was decided, not how layer A read it.** This was
                // `request.baseline` — "ask — intents [read_file unknown] over
                // [host_other]" — which says what class of thing it was and nothing
                // about what it WAS, so six examples rendered as six copies of the
                // same sentence. The tool and its target is the smallest thing that
                // makes an example an example. Still not the raw arguments: the rule
                // that keeps a command out of the brief keeps it out of here.
                action: format!("{} {}", row.request.tool, row.request.target),
                verdict: if row.effect == "admit" { "allowed" } else { "refused" },
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

    fn prior_answers(
        log: &[AdjudicationRow],
        key: &str,
        now_turn: Option<u64>,
    ) -> Vec<PriorAnswer> {
        let mut out: Vec<PriorAnswer> = Vec::new();
        for row in log.iter().filter(|r| r.direction == key) {
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
    /// **The question the boundary could not ask.**
    ///
    /// A real adjudication, so it lands in the corpus beside every other decision
    /// and an operator can see later what they let in and when. It is NOT a tool
    /// call — no baseline, no tier, no intents — because nothing is being run: the
    /// subject is a path and the answer widens a view. That is why it builds the
    /// request here rather than going through `request_for`, which reads a
    /// `GateCall` that does not exist.
    ///
    /// `NotAsked` when there is nobody to ask, which is a session with no
    /// adjudicator attached. Not a refusal: nobody decided, and a caller that
    /// rendered "no" would be reporting a decision that was never made.
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
            brief_variant: crate::authorise::BriefVariant::Follows,
            advice: None,
            summary: format!(
                "`{tool}` named `{}`, which is outside this session's filesystem view",
                path.display()
            ),
            arguments: serde_json::json!({ "path": path.display().to_string() }),
            arguments_digest: String::new(),
            boundary_facts: vec![
                format!(
                    "the command saw `{}` as absent — that was the boundary, not an                      answer about whether anything is there",
                    path.display()
                ),
                "granting it binds the real host path into this session's mount                  namespace for the rest of the session, and a readable path is                  readable INTO THE TRANSCRIPT"
                    .to_string(),
                "the command that hit this has already run; granting does not re-run                  it".to_string(),
            ],
            kind: RequestKind::Permission,
            options: view_grant_options(),
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

    fn describe(&self) -> String {
        match (self.supervise, &self.advisor) {
            (true, Some(a)) => format!("{} — supervised by {}", self.adjudicator.describe(), a.describe()),
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
        self.mode = mode;
        Ok(dropped)
    }

    fn set_supervision(&mut self, on: bool) -> Result<String, String> {
        // **Refuses rather than pretending.** A gate told to supervise with no guard
        // model installed, that answered "ok", would leave the operator believing
        // every later call was being measured while none were — the same lie
        // `Mode::check` refuses to tell by downgrading silently.
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
        let mut req = self.request_from(call, &baseline);
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
            // **The one settled path that does NOT ask the model**, and the exception
            // is deliberate. Every other short-circuit calls
            // `advise_on_a_settled_call`; this one is the breaker, which is open
            // precisely because the same direction has been refused three times in a
            // row — so the calls arriving here are near-duplicates of rows already in
            // the corpus, and each one would spend an oracle round trip while a retry
            // loop is running. The breaker exists to stop a loop; paying latency per
            // iteration of it is the opposite.
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
            // Asked, and it will answer `unavailable` without reaching the oracle —
            // `ModelAdjudicator` refuses on `!resolved` for the same reason the gate
            // does. That costs one call and no inference, and the row then says an
            // unresolved action produced no verdict, rather than saying nothing.
            self.advise_on_a_settled_call(&req, "layer A could not resolve the action".into());
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
            // Asked here too, and this is the row worth having: an oracle that wants
            // to admit a call touching `.ssh` is a calibration fact about the guard,
            // and it is invisible unless the never-list path records a verdict. It
            // changes nothing — the list is overridable by nobody, the oracle
            // included, and `Adjudicable` is not minted for it.
            self.advise_on_a_settled_call(&req, format!("`{hit}` is on the never-write list"));
            self.record(req, d, "refuse", direction.key());
            // The model is told why, because a refusal it cannot understand is a
            // refusal it will retry.
            return GateDecision::refuse_and_tell(ToolOutcome::Denied { req_id: id }, tell);
        }

        // 1.5. opencode's permission model, evaluated before the mode (see
        //      `crate::permission`). `deny` refuses, `allow` admits, `ask` falls
        //      through to the mode and the adjudicator. Last-rule-wins over the
        //      config plus the `always` approvals.
        if !self.permission.is_empty() {
            let pattern = permission_pattern(call.args);
            // A `bash` command is tested segment by segment against the prefix
            // rules — `git log; rm -rf ~` is not `git log` — and a compound the
            // splitter cannot read is asked, never allowed. See
            // `permission::bash_segments`.
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
                crate::permission::Action::Allow => {
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
                crate::permission::Action::Ask => {}
            }
        }

        // 2. **The point this session sits at.** See `crate::mode`.
        //
        //    It governs `Tier::MayApprove` and nothing else: `admits_unasked` answers
        //    `false` for an always-ask and for an inexpressible at every point,
        //    including automode, and there is no argument that makes it answer
        //    otherwise. So this arm cannot be the one that widens something a point is
        //    not allowed to widen — the check is in the type rather than here.
        //
        //    Recorded as a row like any other admission. A standing decision that
        //    stops appearing in the audit is one nobody can review, and the corpus
        //    wants it: *"what the operator decided"* includes the mode they put this
        //    project at.
        //    The operator's rule (2026-09-11) excepts exec: no point admits an
        //    exec-class call unasked, because `bash` is the tool whose result is an
        //    arbitrary byte stream and the operator has decided that question is
        //    settled by them, every time, and by nothing else.
        if (call.access != Access::Exec || self.exec_follows_mode)
            && self.mode.admits_unasked(&req.tier, call.access)
        {
            let d = AdjudicationDecision::selected(
                &req,
                "allow_once",
                "gate:mode",
                &format!(
                    "the `{}` mode admits {} calls without asking; nothing was consulted",
                    self.mode.name,
                    call.access.as_str()
                ),
            );
            self.breaker.admitted(&direction);
            // The mode's own admission is a settled call like any other. It cannot
            // fire at `supervised` today — every non-read disposition there is `Ask`
            // — and it is wired anyway, because a point is a value and the next one
            // somebody writes may admit something while still wanting the verdict.
            self.advise_on_a_settled_call(
                &req,
                format!("the `{}` mode admits {} unasked", self.mode.name, call.access.as_str()),
            );
            self.record(req, d, "admit", direction.key());
            return GateDecision::Admit;
        }

        // 3. A standing permission from an earlier `allow_session` in this session.
        //
        //    Keyed on `(program, ActionClass, intents)` and checked **per call against
        //    the normalisation**, never against text. That is what makes
        //    `git -c core.pager='sh -c …' log` fall out of a grant taken over
        //    `git status` without the grant having enumerated a single escape hatch:
        //    the vehicle flag changes the derived intents, and the grant covers what it
        //    was shown rather than a superset of it.
        //
        //    `Grant::covers` is the only reader, and it refuses an unresolved action,
        //    an always-ask and an inexpressible before it looks at coverage at all.
        let program = grant_program(&baseline);
        //    The operator's rule excepts exec here too: a standing permission never
        //    covers an exec-class call, so an `allow_session` taken over `cargo test`
        //    cannot quietly become a session where every later `cargo test` runs
        //    unasked. The prompt does not offer the option either (see `request_from`)
        //    and the recording site refuses one — the same three mechanisms the
        //    always-ask list uses, and for the same reason.
        if let Some(g) = self.grants.iter().find(|g| {
            call.access != Access::Exec
                && g.covers(
                    &req.tier,
                    req.resolved,
                    &program,
                    req.class,
                    &baseline.intents,
                )
                .is_ok()
        }) {
            let basis = format!(
                "a standing permission granted this session covers this call: {}",
                g.why
            );
            let why = g.why.clone();
            let d =
                AdjudicationDecision::selected(&req, "allow_session", "gate:session-grant", &basis);
            // **A grant settles the person, not the model.**
            //
            // > *"it can remember my allow_session, still ask model later, still
            // > record"*
            //
            // The grant is the operator's standing answer, so nobody is asked again —
            // that is what a grant is for. The oracle is still consulted, because its
            // verdict on a call the operator has already ruled on is a labelled
            // example that costs nobody a keystroke, and those are the only cheap ones
            // there are.
            //
            // It does not change the outcome. Nothing below this line reads the
            // advice to decide anything: the call is admitted by the grant, before and
            // regardless. An oracle that is slow or down costs the wait and nothing
            // else; an oracle that disagrees is recorded disagreeing.
            // Why the label on this row is not a fresh judgement, kept with the row
            // rather than inferred from `asked` being false. A trainer that wants only
            // decisions the operator was actually looking at filters on `asked`; one
            // that will take a standing ruling gets to see which ruling it came from.
            self.advise_on_a_settled_call(&req, why);
            self.breaker.admitted(&direction);
            self.record(req, d, "admit", direction.key());
            return GateDecision::Admit;
        }

        // 4. Ask — and when supervision is on, ask the model FIRST.
        //
        //    The ordering is load-bearing. A verdict formed after seeing which way
        //    the operator went is not a verdict worth training on, and one they never
        //    saw is advice nobody could agree with. Model, then person, then the row
        //    carrying both.
        self.advice = self.ask_the_advisor(&req);
        req.advice = self.advice.clone();

        // **At a point whose decider is the MODEL, the model's admit IS the
        // decision.** Otherwise `automode` is `supervised` wearing another name.
        //
        // `Mode::AUTO` says it in as many words — *"a model answers, within the
        // scope it has earned"* — and `Decider::Model` is the whole content of the
        // point. What actually happened was that the decider never reached the
        // adjudicator: the gate asked the advisor, threw its verdict onto the
        // request as advice, and then asked the person anyway. The operator saw
        // `model says admit` and a permission ladder under it, on a mode they chose
        // precisely so the model would answer.
        //
        // Three things still bound it, and all three predate this:
        //
        //  * the always-ask list, which is exempted here by name — the mode's own
        //    sentence promises it "still reaches you", and this is where that is
        //    kept true;
        //  * the oracle's scope, which decides whether it may answer about this
        //    action at all (it returns `ask` when it may not, and that falls
        //    through to the person below);
        //  * `Tier::Inexpressible`, refused at step 1b, before any of this.
        //
        // And it admits ONCE. The oracle mints `allow_once` and nothing here turns
        // that into a standing grant.
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
            self.record(req, d, "admit", direction.key());
            return GateDecision::Admit;
        }

        let decision = self.adjudicator.decide(&req);
        // What it was shown, verbatim, for the corpus row — and, at a supervised
        // point, what the model told the person before they answered. Both are read
        // here rather than off `req`, because the gate's own request is the one the
        // adjudicator was *handed*: whatever it learned downstream is its to report.
        self.shown = self.adjudicator.last_brief();
        let typed_pattern = self.adjudicator.last_pattern();

        match &decision.outcome {
            DecisionOutcome::Selected { option_id } => {
                let kind = req.option(option_id).map(|o| o.kind);
                match kind {
                    Some(k) if k.admits() => {
                        // An always-ask can be admitted — by a human, this turn — but
                        // never turned into a standing grant. Three separate mechanisms
                        // keep that true and this is the second: the tier mints no
                        // `Adjudicable`, this refuses to record the grant, and
                        // `always_ask_options` does not offer the option in the first
                        // place, so an operator is never shown a button whose effect
                        // the gate would then decline to honour.
                        //
                        // The grant is also refused when the mode's scope is `Once`.
                        // At `always-ask` an answer settles that call and nothing else,
                        // which is what the point means; recording a session grant
                        // there would be the point saying one thing and the gate doing
                        // another.
                        // *Always allow* is a RULE, not a grant: it goes into the
                        // permission ruleset (and the file behind it), at any point
                        // whose decider is a person, exec included — the operator's
                        // revision of 2026-09-14. For `bash` the pattern is the
                        // program and its verb (`cargo test*`), the way Claude Code's
                        // `Bash(cargo test:*)` reads; for a file tool it is the path.
                        if k == OptionKind::AllowAlways
                            && !matches!(req.tier, Tier::AlwaysAsk { .. })
                        {
                            // **The operator's own glob wins over the derived one.**
                            //
                            // The derived pattern — the exact path, or the program and
                            // its verb — is a good default and it is only ever the
                            // shape of the call in front of them. Somebody who means
                            // "any test under crates/" could not say so, and answered
                            // the same question again for every sibling.
                            //
                            // Only here, and only for `AllowAlways`: this is the one
                            // option that writes a rule, so it is the one place a
                            // pattern has a meaning. An `allow_once` carrying a glob
                            // would be a grant nobody named.
                            let pattern = match typed_pattern {
                                Some(p) => p,
                                None => match call.args.get("command").and_then(|v| v.as_str()) {
                                    Some(cmd) if call.access == Access::Exec => {
                                        crate::permission::always_pattern_for_command(cmd)
                                    }
                                    _ => permission_pattern(call.args),
                                },
                            };
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
                        } else if k == OptionKind::AllowSession
                            && !matches!(req.tier, Tier::AlwaysAsk { .. })
                            // The operator's exec rule, guarded here as well even
                            // though the option list never offers it to an exec
                            // call: a safety property with one mechanism ships
                            // broken the first time somebody refactors the mechanism.
                            && call.access != Access::Exec
                            && self.mode.grants == crate::mode::GrantScope::Session
                        {
                            self.grants.push(crate::grant::Grant {
                                written: crate::grant::Written::Enumerated {
                                    patterns: vec![format!("{program} ({})", req.class)],
                                },
                                coverage: vec![crate::grant::Coverage {
                                    program: program.clone(),
                                    class: req.class,
                                    intents: baseline.intents.clone(),
                                }],
                                why: format!(
                                    "granted for this session by an answer to {}: {}",
                                    req.id, decision.basis
                                ),
                            });
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
            DecisionOutcome::Unavailable
            | DecisionOutcome::Timeout
            | DecisionOutcome::Cancelled => {
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
            " DENIALS ARE NOT SURFACED: no denial sink is attached, so a refusal reaches the model and stops there. The operator will see a task that stopped rather than the decision that stopped it, which is the failure mode that makes a model try a variant instead of asking. Attach one with              `AdjudicatedGate::with_denial_sink`.",
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
    // `command` is first because for `bash` it IS the call: a shell line is more
    // concrete than any path inside it, and the paths are usually not arguments at
    // all. Its absence from this list is why an operator was shown *"`bash` wants
    // exec access to `<no target argument>`"* and had to approve a command the
    // prompt would not name — the one field the decision is actually about.
    for key in ["command", "path", "url", "query", "repo", "server", "pattern"] {
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
/// The pattern opencode's permission model tests a call against: the path for the
/// file tools, the command for `bash`, else `*`.
fn permission_pattern(args: &Value) -> String {
    if let Some(p) = args.get("path").and_then(|v| v.as_str()) {
        return p.to_string();
    }
    if let Some(c) = args.get("command").and_then(|v| v.as_str()) {
        return c.to_string();
    }
    "*".to_string()
}

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

    /// **`/mode` moves the session it is typed in.** The gate reads its mode when
    /// it decides, so the move is a field write — and the standing grants go with
    /// the old point, because they were answers to questions asked under it.
    #[test]
    fn a_gate_moves_to_a_new_point_and_leaves_its_grants_behind() {
        use crate::mode::Mode;
        use crate::runtime::Gate;

        // A person who allows everything for the session, so a grant gets taken.
        struct Session;
        impl Adjudicator for Session {
            fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
                AdjudicationDecision::selected(req, "allow_session", "human:test", "yes, for the session")
            }
            fn describe(&self) -> String {
                "test".into()
            }
        }
        let mut g = AdjudicatedGate::new(Box::new(Session)).with_mode(Mode::WRITES_ALLOWED);
        let args = serde_json::json!({"path": "/home/dead/Projects/letibot/notes.md"});
        let call = GateCall {
            name: "web_search",
            access: Access::Network,
            args: &serde_json::json!({"query": "x"}),
            turn_id: "t1",
            call_id: "c1",
            workspace: "/home/dead/Projects/letibot",
            target_exists: None,
        };
        assert!(matches!(g.admit(&call), GateDecision::Admit));
        assert_eq!(g.grants().len(), 1, "a session grant was taken under writes-allowed");

        // Move to always-ask: the grant does not survive the point it was taken at.
        let dropped = g.set_mode(Mode::ALWAYS_ASK).expect("the gate moves");
        assert_eq!(dropped, 1);
        assert!(g.grants().is_empty());
        assert_eq!(g.mode().name, "always-ask");

        // And the new point is the one deciding: a write that writes-allowed admits
        // unasked now reaches the adjudicator (who, here, grants — but the grant it
        // takes is refused by always-ask's `Once` scope, so nothing standing is
        // recorded).
        let write = GateCall {
            name: "write",
            access: Access::Write,
            args: &args,
            turn_id: "t1",
            call_id: "c2",
            workspace: "/home/dead/Projects/letibot",
            target_exists: Some(true),
        };
        let _ = g.admit(&write);
        assert!(
            g.grants().is_empty(),
            "always-ask must not record a standing grant, whatever the person answered"
        );
    }

    /// **`automode` means the model answers.** It advised and the person was asked
    /// anyway, which is `supervised` under another name — and the operator chose
    /// the point precisely so the model would decide.
    ///
    /// The always-ask list is the exemption the mode's own sentence promises, and
    /// it is asserted here beside the admit so the two cannot drift apart.
    #[test]
    fn at_automode_the_models_admit_is_the_decision_and_always_ask_still_asks() {
        use crate::authorise::{AuthorisationTrail, OracleAnswer, Widening};
        use crate::mode::Mode;

        /// An oracle that authorises everything it is allowed to be asked about.
        struct Yes;
        impl crate::authorise::AuthorisationOracle for Yes {
            fn authorised(&self, brief: &mut crate::authorise::ModelBrief) -> OracleAnswer {
                // The witness is the proof, and taking it is how an oracle widens.
                let Some(w) = brief.adjudicable() else {
                    return OracleAnswer::Unsure {
                        why: "no witness".into(),
                    };
                };
                OracleAnswer::Authorised(Widening::new(
                    w,
                    brief.request_id.clone(),
                    vec![0],
                    "the operator asked for exactly this",
                ))
            }
            fn describe(&self) -> String {
                "test oracle".into()
            }
        }

        /// A person who would refuse. If the ladder is reached at all, the call is
        /// denied — so an admit below can only have come from the model.
        struct Refuses;
        impl Adjudicator for Refuses {
            fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
                AdjudicationDecision::selected(req, "deny", "human:test", "the person said no")
            }
            fn describe(&self) -> String {
                "a person who refuses".into()
            }
        }

        let gate = || {
            AdjudicatedGate::new(Box::new(Refuses))
                .with_mode(Mode::AUTO)
                .with_advisor(std::sync::Arc::new(crate::ModelAdjudicator::new(
                    Box::new(Yes),
                    |_req: &AdjudicationRequest| {
                        crate::intent::Baseline::of_command("touch /tmp/x", &Default::default())
                    },
                )))
                .start_supervised(true)
                .with_trail_source(|_call: &GateCall<'_>| AuthorisationTrail {
                    utterances: vec![crate::authorise::Utterance {
                        speaker: crate::authorise::Speaker::Operator,
                        text: "make that file".into(),
                        clipped: false,
                        turns_ago: 0,
                        seconds_ago: Some(5),
                    }],
                    provenance: crate::authorise::TrailProvenance::Scanned {
                        messages_scanned: 1,
                        operator_messages: 1,
                    },
                })
        };

        let args = serde_json::json!({"path": "/home/dead/Projects/letibot/notes.md"});
        let mut g = gate();
        let d = g.admit(&GateCall {
            name: "write",
            access: Access::Write,
            args: &args,
            turn_id: "t1",
            call_id: "c1",
            workspace: "/home/dead/Projects/letibot",
            target_exists: Some(true),
        });
        assert!(
            matches!(d, GateDecision::Admit),
            "the model authorised it and the person was asked anyway: {d:?}"
        );

        // The always-ask list still reaches the person, who refuses — so the point
        // has not become allow-all by another route.
        let secret = serde_json::json!({"path": "/home/dead/.ssh/id_rsa"});
        let mut g = gate();
        let d = g.admit(&GateCall {
            name: "write",
            access: Access::Write,
            args: &secret,
            turn_id: "t1",
            call_id: "c2",
            workspace: "/home/dead/Projects/letibot",
            target_exists: Some(true),
        });
        assert!(
            !matches!(d, GateDecision::Admit),
            "an always-ask action was settled by the model: {d:?}"
        );
    }
    /// **A `bash` call is about its command**, and the prompt has to be able to say
    /// so. Measured 2026-09-15: every exec permission the operator was shown read
    /// *"`bash` wants exec access to `<no target argument>`"* — the placeholder for
    /// a call that names nothing, on the one tool whose whole argument is the
    /// thing being decided. `command` was simply not in the list.
    #[test]
    fn the_thing_a_call_is_about_includes_a_shell_command() {
        use serde_json::json;
        assert_eq!(
            super::target_of(&json!({"command": "cargo test -p letibot-tools"})),
            "cargo test -p letibot-tools"
        );
        // Still the most concrete argument first for the file tools.
        assert_eq!(super::target_of(&json!({"path": "src/main.rs"})), "src/main.rs");
        // And a call that genuinely names nothing still says so rather than
        // borrowing a word it never had.
        assert_eq!(super::target_of(&json!({"limit": 5})), "<no target argument>");
    }

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
        }
    }

    /// opencode's `permission` config governs the gate before the mode: `deny`
    /// refuses, `allow` admits, `ask` falls through.
    #[test]
    fn a_permission_config_denies_allows_and_asks() {
        let config = crate::permission::config_to_ruleset(
            &json!({
                // `findLast` semantics: the default `*` comes first, the specific
                // override last.
                "edit": { "*": "ask", "*.secret": "deny" },
                "write": "allow",
            })
            .as_object()
            .unwrap()
            .clone(),
        )
        .unwrap();
        let mut g = AdjudicatedGate::closed().with_permission(config);

        match g.admit(&call("edit", &json!({"path": "src/keys.secret"}))) {
            GateDecision::Refuse {
                outcome: ToolOutcome::Denied { .. },
                ..
            } => {}
            other => panic!("a denied permission must refuse, got {other:?}"),
        }

        match g.admit(&call("write", &json!({"path": "x.txt"}))) {
            GateDecision::Admit => {}
            other => panic!("an allowed permission must admit, got {other:?}"),
        }

        // `ask` falls through to the mode; a closed gate fails closed with NotRun.
        match g.admit(&call("edit", &json!({"path": "src/lib.rs"}))) {
            GateDecision::Refuse {
                outcome: ToolOutcome::NotRun { .. },
                ..
            } => {}
            other => {
                panic!("an asked permission must fall through (closed -> not_run), got {other:?}")
            }
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
            Fixed { id, allow, seen: std::sync::Mutex::new(None) }
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
            })
        }
        fn describe(&self) -> String {
            self.id.to_string()
        }
    }

    fn supervised_gate(model_allows: bool, human_allows: bool) -> AdjudicatedGate {
        let advisor: std::sync::Arc<dyn Adjudicator> = std::sync::Arc::new(Fixed::new("model:test", model_allows));
                AdjudicatedGate::new(Box::new(Fixed::new("human:op", human_allows)))
            .with_mode(crate::mode::Mode::ALWAYS_ASK)
            .with_advisor(advisor)
            .start_supervised(true)
    }

    /// **The person sees the model's verdict before answering.**
    ///
    /// Without this the point's name is a lie: "ask me if i agree" requires that
    /// there is something in front of me to agree with.
    #[test]
    fn the_human_is_shown_what_the_model_said() {
        let seen: std::sync::Arc<std::sync::Mutex<Option<Option<ModelAdvice>>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let captured = seen.clone();
        let human = AskAdjudicator::new("human:op", move |req: &AdjudicationRequest| {
            *captured.lock().unwrap() = Some(req.advice.clone());
            req.options
                .first()
                .map(|o| AdjudicationDecision::selected(req, &o.id, "human:op", "ok"))
        });
        let advisor: std::sync::Arc<dyn Adjudicator> = std::sync::Arc::new(Fixed::new("model:test", true));
                let mut g = AdjudicatedGate::new(Box::new(human))
            .with_mode(crate::mode::Mode::ALWAYS_ASK)
            .with_advisor(advisor)
            .start_supervised(true);
        let _ = g.admit(&call("edit", &json!({"path": "src/lib.rs"})));

        let advice = seen.lock().unwrap().clone().expect("the human was asked");
        let advice = advice.expect("carrying the model's verdict");
        assert_eq!(advice.would, "admit");
        assert_eq!(advice.by, "model:test");
    }

    /// **The model advises and never decides.** The human's answer is the outcome,
    /// whichever way the model went.
    #[test]
    fn the_person_overrules_the_model_in_both_directions() {
        // Model would allow, person refuses.
        let mut g = supervised_gate(true, false);
        assert!(matches!(
            g.admit(&call("edit", &json!({"path": "src/lib.rs"}))),
            GateDecision::Refuse { .. }
        ));
        assert_eq!(g.log[0].effect, "refuse");

        // Model would not allow, person admits.
        let mut g = supervised_gate(false, true);
        assert!(matches!(
            g.admit(&call("edit", &json!({"path": "src/lib.rs"}))),
            GateDecision::Admit
        ));
        assert_eq!(g.log[0].effect, "admit");
    }

    /// **One call, one labelled row** — the reason to sit through this point.
    ///
    /// The model's verdict and the operator's ruling are both on the row, they are
    /// different values, and the ruling did not overwrite the verdict. Nothing has
    /// to be reconstructed afterwards from a list.
    #[test]
    fn a_supervised_call_produces_a_labelled_corpus_row_by_itself() {
        let mut g = supervised_gate(true, false);
        let _ = g.admit(&call("edit", &json!({"path": "src/lib.rs"})));
        let row = g.corpus().remove(0);

        assert!(row.asked, "a person answered this one");
        // The model's, not the person's wearing its name.
        let verdict = row.model_verdict.as_deref().expect("the model was asked");
        assert!(verdict.starts_with("admit by model:test"), "{verdict}");
        // The person's, separately.
        assert_eq!(row.verdict_by.as_deref(), Some("human:op"));
        assert_eq!(row.effect, "refuse");
        // And the label, computed from the two rather than typed later.
        assert_eq!(
            row.operator.as_ref().map(crate::authorise::OperatorOverride::as_str),
            Some("revoked"),
            "the model would have admitted and the person stopped it"
        );
        assert!(row.is_disagreement());

        // The other direction is the over-refusal, and it is `granted`.
        let mut g = supervised_gate(false, true);
        let _ = g.admit(&call("edit", &json!({"path": "src/lib.rs"})));
        let row = g.corpus().remove(0);
        assert_eq!(
            row.operator.as_ref().map(crate::authorise::OperatorOverride::as_str),
            Some("granted")
        );

        // Agreement is a label too. A corpus of corrections alone teaches that
        // every decision was wrong.
        let mut g = supervised_gate(true, true);
        let _ = g.admit(&call("edit", &json!({"path": "src/lib.rs"})));
        let row = g.corpus().remove(0);
        assert_eq!(
            row.operator.as_ref().map(crate::authorise::OperatorOverride::as_str),
            Some("upheld")
        );
        assert!(!row.is_disagreement());
    }

    /// **The adviser is not load-bearing.** An oracle that cannot answer leaves the
    /// person answering exactly as they would at `always-ask`.
    #[test]
    fn a_silent_oracle_does_not_stop_the_person_being_asked() {
        struct Mute;
        impl Adjudicator for Mute {
            fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
                AdjudicationDecision {
                    request_id: req.id.clone(),
                    outcome: DecisionOutcome::Timeout,
                    by: "model:test".into(),
                    basis: "budget".into(),
                    latency_ms: 400,
                }
            }
            fn describe(&self) -> String {
                "model:test".into()
            }
        }
        let mut g = AdjudicatedGate::new(Box::new(Fixed::new("human:op", true)))
            .with_mode(crate::mode::Mode::ALWAYS_ASK)
            .with_advisor(std::sync::Arc::new(Mute))
            .start_supervised(true);
        assert!(matches!(
            g.admit(&call("edit", &json!({"path": "src/lib.rs"}))),
            GateDecision::Admit
        ));
        let row = g.corpus().remove(0);
        // No advice, so no label is invented: there is nothing for the person to
        // have agreed or disagreed with.
        assert!(row.operator.is_none());
        assert!(row.asked);
    }

    /// **A grant stops asking YOU and does not stop asking the MODEL.**
    ///
    /// > *"it can remember my allow_session, still ask model later, still record"*
    ///
    /// Three claims in one test because they are one mechanism: the person is asked
    /// once, the oracle is asked on every covered call, and every covered call is a
    /// labelled row.
    #[test]
    fn a_standing_grant_settles_the_person_and_still_asks_the_model() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        /// Counts how often the model was consulted, and how often a person was.
        struct Counted {
            id: &'static str,
            allow: bool,
            n: std::sync::Arc<AtomicUsize>,
        }
        impl Adjudicator for Counted {
            fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
                self.n.fetch_add(1, Ordering::SeqCst);
                // The person takes the SESSION grant, which is the answer this whole
                // test is about — an `allow_once` would settle one call and prove
                // nothing about what a remembered answer does.
                let want = |o: &&DecisionOption| {
                    if self.allow {
                        o.kind == OptionKind::AllowSession
                    } else {
                        !o.kind.is_allow()
                    }
                };
                match req.options.iter().find(want) {
                    Some(o) => AdjudicationDecision::selected(req, &o.id, self.id, "yes"),
                    None => AdjudicationDecision::unavailable(req, self.id, "none offered"),
                }
            }
            fn last_advice(&self) -> Option<ModelAdvice> {
                self.id.starts_with("model").then(|| ModelAdvice {
                    consulted: true,
                    would: if self.allow { "admit" } else { "ask" },
                    by: self.id.to_string(),
                    basis: "yes".into(),
                    cites: Vec::new(),
                    latency_ms: 1,
                })
            }
            fn describe(&self) -> String {
                self.id.into()
            }
        }

        let asks_model = std::sync::Arc::new(AtomicUsize::new(0));
        let asks_human = std::sync::Arc::new(AtomicUsize::new(0));
        let advisor: std::sync::Arc<dyn Adjudicator> =
            std::sync::Arc::new(Counted { id: "model:test", allow: true, n: asks_model.clone() });
        // A point that ASKS about writes and lets one answer stand for the session —
        // the shape a grant is for. Written out rather than borrowed from `NAMED`,
        // because supervision is no longer a point and this test is about the grant,
        // not about which dot on the ladder happens to have that scope today.
        let asks_and_grants = crate::mode::Mode {
            name: "test:asks-and-grants",
            grants: crate::mode::GrantScope::Session,
            ..crate::mode::Mode::ALWAYS_ASK
        };
        let mut g = AdjudicatedGate::new(Box::new(Counted {
            id: "human:op",
            allow: true,
            n: asks_human.clone(),
        }))
        .with_mode(asks_and_grants)
        .with_advisor(advisor)
        .start_supervised(true);

        let args = json!({"path": "src/lib.rs"});
        for _ in 0..3 {
            assert!(matches!(g.admit(&call("edit", &args)), GateDecision::Admit));
        }

        assert_eq!(g.grants().len(), 1, "the first answer took a session grant");
        assert_eq!(
            asks_human.load(Ordering::SeqCst),
            1,
            "the person is asked once and the grant covers the rest"
        );
        assert_eq!(
            asks_model.load(Ordering::SeqCst),
            3,
            "the oracle is asked on EVERY call, grant or no grant — that is the label"
        );

        // Every one is a row, and every one is labelled.
        let rows = g.corpus();
        assert_eq!(rows.len(), 3);
        for r in &rows {
            assert!(r.model_verdict.is_some(), "the model answered this one");
            assert!(r.operator.is_some(), "and the operator's ruling is on it");
        }

        // The first is a judgement made with the verdict on screen; the other two
        // are a standing ruling applied. `asked` separates them and the note says
        // which grant, so a trainer can weight them differently instead of
        // discovering later that it could not tell them apart.
        assert!(rows[0].asked);
        assert!(!rows[1].asked && !rows[2].asked);
        let note = match rows[1].operator.as_ref().unwrap() {
            crate::authorise::OperatorOverride::Upheld { note }
            | crate::authorise::OperatorOverride::Granted { note }
            | crate::authorise::OperatorOverride::Revoked { note } => note.clone(),
        };
        assert!(note.contains("standing decision"), "{note}");
        assert!(note.contains("granted for this session"), "{note}");
    }

    /// **A command asks every time**, whatever is granted. The operator's own rule,
    /// and supervised does not have a relaxed version of it.
    #[test]
    fn an_exec_call_is_not_covered_by_a_grant_at_a_supervised_point() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let asks = std::sync::Arc::new(AtomicUsize::new(0));
        let seen = asks.clone();
        let human = AskAdjudicator::new("human:op", move |req: &AdjudicationRequest| {
            seen.fetch_add(1, Ordering::SeqCst);
            req.options
                .iter()
                .find(|o| o.kind.is_allow())
                .map(|o| AdjudicationDecision::selected(req, &o.id, "human:op", "ok"))
        });
        let advisor: std::sync::Arc<dyn Adjudicator> = std::sync::Arc::new(Fixed::new("model:test", true));
        let mut g = AdjudicatedGate::new(Box::new(human))
            .with_mode(crate::mode::Mode::ALWAYS_ASK)
            .with_advisor(advisor)
            .start_supervised(true)
            .with_exec_follows_mode(false)
            // An absolute path and a pinned shell, so layer A resolves the action
            // and it reaches an adjudicator at all. A bare `cargo` under
            // `ShellTrust::Unknown` is `not_run` and would pass this test for the
            // wrong reason — nobody asked because nothing was decidable.
            .with_surroundings(pinned())
            .with_trail_source(|_| crate::authorise::AuthorisationTrail::from_messages(vec![], 1));

        let args = json!({"command": "/bin/cat /w/src/lib.rs"});
        for _ in 0..2 {
            assert_eq!(g.admit(&bash(&args)), GateDecision::Admit);
        }
        assert_eq!(
            asks.load(Ordering::SeqCst),
            2,
            "exec asks every time; a standing permission never covers it"
        );
        assert!(g.grants().is_empty(), "and no grant is taken over an exec call");
    }

    /// **A configured glob settles the call and the model is still asked.**
    ///
    /// > *"we have configured globs already, they can go thru model in the
    /// > supervised mode too"*
    ///
    /// Both directions. A `deny` is worth as much as an `allow` here: a model that
    /// routinely wants to admit what the operator's rules refuse is exactly the
    /// miscalibration to find before automode, and it is invisible if only the
    /// admissions are measured.
    #[test]
    fn a_permission_rule_is_still_measured_against_the_model() {
        let rules = |json: serde_json::Value| {
            crate::permission::config_to_ruleset(json.as_object().unwrap()).unwrap()
        };
        let gate = |cfg: serde_json::Value, model_allows: bool| {
            let advisor: std::sync::Arc<dyn Adjudicator> = std::sync::Arc::new(Fixed::new("model:test", model_allows));
            AdjudicatedGate::new(Box::new(NoAdjudicator))
                .with_mode(crate::mode::Mode::ALWAYS_ASK)
                .with_advisor(advisor)
                .start_supervised(true)
                .with_permission(rules(cfg))
        };

        // An `allow` rule admits, and the oracle's agreement is recorded.
        let mut g = gate(json!({ "edit": "allow" }), true);
        assert_eq!(g.admit(&call("edit", &json!({"path": "a.rs"}))), GateDecision::Admit);
        let row = g.corpus().remove(0);
        assert!(row.model_verdict.as_deref().unwrap().starts_with("admit by model:test"));
        assert!(!row.asked, "nobody was asked: a rule decided");
        assert_eq!(
            row.operator.as_ref().map(crate::authorise::OperatorOverride::as_str),
            Some("upheld")
        );

        // A `deny` rule refuses, and an oracle that would have admitted is recorded
        // disagreeing. This is the over-refusal signal and it only exists because
        // the deny path asks too.
        let mut g = gate(json!({ "edit": "deny" }), true);
        assert!(matches!(
            g.admit(&call("edit", &json!({"path": "a.rs"}))),
            GateDecision::Refuse { .. }
        ));
        let row = g.corpus().remove(0);
        assert_eq!(
            row.operator.as_ref().map(crate::authorise::OperatorOverride::as_str),
            Some("revoked"),
            "the model would have admitted and the operator's rule refused"
        );
        assert!(row.is_disagreement());

        // And nothing about the advice changed the outcome: the same rules with a
        // model that would refuse still admit and still refuse, respectively.
        let mut g = gate(json!({ "edit": "allow" }), false);
        assert_eq!(g.admit(&call("edit", &json!({"path": "a.rs"}))), GateDecision::Admit);
    }

    /// At every point but `supervised` a settled call spends no oracle round trip.
    /// There is no model to ask, and paying latency for a row nobody asked for is
    /// what this guards.
    #[test]
    fn a_rule_at_a_non_supervised_point_consults_nothing() {
        let cfg = json!({ "edit": "allow" });
        let rules = crate::permission::config_to_ruleset(cfg.as_object().unwrap()).unwrap();
        let mut g = AdjudicatedGate::new(Box::new(Fixed::new("human:op", true)))
            .with_mode(crate::mode::Mode::WRITES_ALLOWED)
            .with_permission(rules);
        assert_eq!(g.admit(&call("edit", &json!({"path": "a.rs"}))), GateDecision::Admit);
        let row = g.corpus().remove(0);
        // The verdict column holds what decided — the rule — and no label is
        // invented, because nothing was consulted to agree or disagree with.
        assert!(row.model_verdict.as_deref().unwrap().contains("gate:permission"));
        assert!(row.operator.is_none());
    }

    /// **The model has its turn regardless of what settled the call.**
    ///
    /// > *"supervised mode is essentially a normal mode but we always ask model. and
    /// > model has its turn regardless of glob, deny, allow, allow_session or glob
    /// > deny or allow from config"*
    ///
    /// One case per short-circuit in `admit`, so a path added later that forgets to
    /// ask fails here rather than quietly producing rows with no verdict. The
    /// breaker is the one deliberate exception and is asserted as such.
    #[test]
    fn every_settled_path_still_gives_the_model_its_turn() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        /// Counts every consultation, whatever the answer.
        struct Counting(std::sync::Arc<AtomicUsize>);
        impl Adjudicator for Counting {
            fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
                self.0.fetch_add(1, Ordering::SeqCst);
                AdjudicationDecision::unavailable(req, "model:test", "counted")
            }
            fn last_advice(&self) -> Option<ModelAdvice> {
                Some(ModelAdvice {
                    consulted: true,
                    would: "ask",
                    by: "model:test".into(),
                    basis: "counted".into(),
                    cites: Vec::new(),
                    latency_ms: 1,
                })
            }
            fn describe(&self) -> String {
                "model:test".into()
            }
        }

        let build = |f: &dyn Fn(AdjudicatedGate) -> AdjudicatedGate| {
            let n = std::sync::Arc::new(AtomicUsize::new(0));
            let g = AdjudicatedGate::new(Box::new(Fixed::new("human:op", true)))
                .with_mode(crate::mode::Mode::ALWAYS_ASK)
                .with_advisor(std::sync::Arc::new(Counting(n.clone())))
                .start_supervised(true);
            (f(g), n)
        };
        let rules = |j: serde_json::Value| {
            crate::permission::config_to_ruleset(j.as_object().unwrap()).unwrap()
        };

        // Each case: how to set the gate up, and the call that trips that path.
        let path = json!({"path": "src/lib.rs"});
        let secret = json!({"path": "/home/op/.ssh/id_ed25519"});

        // 1. the never-write list
        let (mut g, n) = build(&|g| g);
        let _ = g.admit(&call("edit", &secret));
        assert_eq!(n.load(Ordering::SeqCst), 1, "never-write list: model asked");
        assert_eq!(g.log[0].effect, "refuse", "and the list still refuses");

        // 1.5 a config `deny`, and a config `allow`
        for (cfg, effect) in [("deny", "refuse"), ("allow", "admit")] {
            let (mut g, n) = build(&|g| g.with_permission(rules(json!({ "edit": cfg }))));
            let _ = g.admit(&call("edit", &path));
            assert_eq!(n.load(Ordering::SeqCst), 1, "config {cfg}: model asked");
            assert_eq!(g.log[0].effect, effect, "and the rule still decides");
        }

        // 2. the mode admits unasked. `supervised` never does, so this is asserted
        //    at a point that does — the wiring is shared and the rule is the same.
        let n = std::sync::Arc::new(AtomicUsize::new(0));
        let advisor: std::sync::Arc<dyn Adjudicator> = std::sync::Arc::new(Counting(n.clone()));
                let mut g = AdjudicatedGate::new(Box::new(Fixed::new("human:op", true)))
            .with_mode(crate::mode::Mode::ALWAYS_ASK)
            .with_advisor(advisor)
            .start_supervised(true);
        let _ = g.admit(&call("edit", &path));
        let before = n.load(Ordering::SeqCst);

        // 3. a standing grant, on the SAME gate: the second call is covered.
        let _ = g.admit(&call("edit", &path));
        assert!(
            n.load(Ordering::SeqCst) > before,
            "a grant-covered call still consults the model"
        );

        // 0. the breaker is the exception, and it is one on purpose: the calls
        //    reaching it are near-duplicates of rows already recorded, and paying an
        //    oracle round trip per iteration of a retry loop is what the breaker
        //    exists to prevent.
        let (mut g, n) = build(&|g| g.with_permission(rules(json!({ "edit": "deny" }))));
        for _ in 0..5 {
            let _ = g.admit(&call("edit", &path));
        }
        let asked = n.load(Ordering::SeqCst);
        assert!(
            asked < 5,
            "the breaker must stop consulting, asked {asked} of 5"
        );
        assert!(asked >= 3, "and only after it has opened, asked {asked}");
    }

    /// **A short-circuit is not a verdict**, and a row must not record agreement
    /// with one.
    ///
    /// `ModelAdjudicator` answers five questions without asking a model — an
    /// unresolved action, an always-ask entry, an inexpressible one, an uncollected
    /// trail, an intent outside its earned scope. From outside, those produce the
    /// same `AdjudicationDecision` a real verdict does. Labelling against them would
    /// manufacture signal, which is worse than none because it looks like data.
    #[test]
    fn an_unconsulted_oracle_produces_no_label() {
        struct Silent;
        impl Adjudicator for Silent {
            fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
                AdjudicationDecision::unavailable(req, "model:test", "always-ask short-circuit")
            }
            fn last_advice(&self) -> Option<ModelAdvice> {
                Some(ModelAdvice {
                    consulted: false,
                    would: "ask",
                    by: "model:test".into(),
                    basis: "`sudo` is on the always-ask list; no oracle was consulted".into(),
                    cites: Vec::new(),
                    latency_ms: 0,
                })
            }
            fn describe(&self) -> String {
                "model:test".into()
            }
        }
        let mut g = AdjudicatedGate::new(Box::new(Fixed::new("human:op", true)))
            .with_mode(crate::mode::Mode::ALWAYS_ASK)
            .with_advisor(std::sync::Arc::new(Silent))
            .start_supervised(true);
        let _ = g.admit(&call("edit", &json!({"path": "src/lib.rs"})));

        let row = g.corpus().remove(0);
        assert!(
            row.operator.is_none(),
            "no oracle spoke, so there is nothing the operator agreed or disagreed with"
        );
        // The row still exists and still says what happened — silence is recorded,
        // not dropped.
        assert!(row.model_verdict.is_some());
        assert!(row.asked);
        // And a head renders it as an absence rather than as an opinion.
        let advice = g.log[0].advice.as_ref().unwrap();
        assert!(advice.line().starts_with("no model verdict"), "{}", advice.line());
    }

    /// **Supervision changes who is consulted and nothing else.**
    ///
    /// It used to be a mode, which meant selecting it also pinned the dispositions
    /// and the grant scope — and meant it could only change by restarting a session.
    /// It is a flag on the gate now, so this asserts the property that makes that
    /// safe: the same calls ask, the same answers travel, and only the verdict
    /// column appears.
    #[test]
    fn supervision_changes_who_is_consulted_and_nothing_else() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let asks = std::sync::Arc::new(AtomicUsize::new(0));
        let seen = asks.clone();
        let human = AskAdjudicator::new("human:op", move |req: &AdjudicationRequest| {
            seen.fetch_add(1, Ordering::SeqCst);
            req.options
                .iter()
                .find(|o| o.kind.is_allow())
                .map(|o| AdjudicationDecision::selected(req, &o.id, "human:op", "ok"))
        });
        let mut g = AdjudicatedGate::new(Box::new(human))
            .with_mode(crate::mode::Mode::ALWAYS_ASK)
            .with_advisor(std::sync::Arc::new(Fixed::new("model:test", true)));

        let args = json!({"path": "src/lib.rs"});

        // Off: the person is asked, and the row carries no verdict.
        assert!(!g.supervising());
        assert_eq!(g.admit(&call("edit", &args)), GateDecision::Admit);
        assert!(g.corpus()[0].operator.is_none());

        // **On, mid-session, without rebuilding anything.**
        let said = g.set_supervision(true).expect("an advisor is installed");
        assert!(said.contains("supervision ON"), "{said}");
        assert!(g.supervising());

        assert_eq!(g.admit(&call("edit", &args)), GateDecision::Admit);
        assert_eq!(
            asks.load(Ordering::SeqCst),
            2,
            "the same calls ask the same person; supervision adds a verdict, not a question"
        );
        let row = g.corpus().remove(1);
        assert!(row.model_verdict.as_deref().unwrap().starts_with("admit by model:test"));
        assert_eq!(row.verdict_by.as_deref(), Some("human:op"));
        assert_eq!(
            row.operator.as_ref().map(crate::authorise::OperatorOverride::as_str),
            Some("upheld")
        );

        // And off again.
        assert!(g.set_supervision(false).is_ok());
        assert!(!g.supervising());
    }

    /// **A gate with no guard model refuses to say it is supervising.**
    ///
    /// Answering "ok" and supervising nothing would leave the operator believing
    /// every later call was measured while none were — the same lie `Mode::check`
    /// refuses to tell by downgrading silently.
    #[test]
    fn supervision_without_an_advisor_refuses_by_name() {
        let mut g = AdjudicatedGate::new(Box::new(Fixed::new("human:op", true)))
            .with_mode(crate::mode::Mode::ALWAYS_ASK);
        let e = g.set_supervision(true).expect_err("nothing to supervise with");
        assert!(e.contains("gatekeeper"), "{e}");
        assert!(!g.supervising());
    }

    #[test]
    fn with_no_adjudicator_a_write_refuses_and_does_not_claim_a_decision() {
        let mut g = AdjudicatedGate::closed();
        let args = json!({"path": "src/lib.rs"});
        match g.admit(&call("edit", &args)) {
            GateDecision::Refuse {
                outcome: ToolOutcome::NotRun { why },
                ..
            } => {
                assert!(why.contains("fails closed"), "{why}");
                assert!(why.contains("nobody decided"), "{why}");
            }
            other => panic!("fail closed means refuse, got {other:?}"),
        }
        assert_eq!(g.log.len(), 1);
        assert_eq!(g.log[0].effect, "refuse");
    }

    /// An `allow_session` is not asked twice — **at a point whose grant scope is a
    /// session**.
    ///
    /// The mode is named rather than left to the default, and that is the point of the
    /// test rather than noise in it: the default is `always-ask`, whose scope is
    /// `Once`, where an answer settles that call and nothing else. A test that relied
    /// on the default here would have been asserting the old behaviour of a gate that
    /// had no notion of where it was sitting.
    ///
    /// The coordinate is one nobody named — write asks, and the answer is remembered —
    /// which is also the demonstration that the space is reachable beyond the four.
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
        let mut g = AdjudicatedGate::new(Box::new(adj)).with_mode(crate::mode::Mode {
            name: "ask about writes, remember the answer",
            write: crate::mode::Disposition::Ask,
            grants: crate::mode::GrantScope::Session,
            ..crate::mode::Mode::WRITES_ALLOWED
        });
        let args = json!({"path": "src/lib.rs"});
        assert_eq!(g.admit(&call("edit", &args)), GateDecision::Admit);
        assert_eq!(g.admit(&call("edit", &args)), GateDecision::Admit);
        assert_eq!(
            asked.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "an allow_session must not be asked again"
        );
        assert_eq!(g.log.len(), 2, "both calls are rows, grant included");
        assert_eq!(g.grants().len(), 1, "and the grant is listable");
    }

    /// **A button that cannot work is not offered.**
    ///
    /// The sibling test below asserts that an `allow_session` answer does not stand at
    /// a `Once` point. That is correct and it is not enough: for as long as the option
    /// was still *shown* there, an operator chose it, watched the next call ask again,
    /// and reported it twice as "allow_session doesn't stick".
    ///
    /// The cause was two guards for one decision. The recording site required the tier
    /// not be `AlwaysAsk` AND the mode grant for the session; the option list tested
    /// only the tier. This asserts they agree.
    #[test]
    fn a_once_scoped_point_does_not_offer_a_session_grant() {
        use std::sync::{Arc, Mutex};
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let s = seen.clone();
        let adj = AskAdjudicator::new("test", move |req: &AdjudicationRequest| {
            s.lock()
                .unwrap()
                .extend(req.options.iter().map(|o| o.id.clone()));
            Some(AdjudicationDecision::selected(
                req,
                "allow_once",
                "human:test",
                "ok",
            ))
        });
        let mut g = AdjudicatedGate::new(Box::new(adj)).with_mode(crate::mode::Mode::ALWAYS_ASK);
        let _ = g.admit(&call("edit", &json!({"path": "src/lib.rs"})));

        let ids = seen.lock().unwrap().clone();
        assert!(
            !ids.is_empty(),
            "the adjudicator was never consulted, so this proves nothing"
        );
        assert!(
            !ids.iter().any(|i| i == "allow_session"),
            "a point whose grants are `Once` offered `allow_session`, which the gate \
             then declines to record: {ids:?}"
        );
        assert!(
            ids.iter().any(|i| i == "allow_once"),
            "allowing this one call must still be offered: {ids:?}"
        );
    }

    /// **An answer at `always-ask` settles one call and nothing else.**
    ///
    /// The same adjudicator, the same two calls, and a different point: the scope is
    /// `Once`, so the second call asks again. This is the half that makes the mode
    /// mean something — a point that said *every action asks* and then quietly
    /// honoured a standing grant would be the banner and the behaviour disagreeing.
    #[test]
    fn at_always_ask_an_allow_session_answer_does_not_stand() {
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
        let mut g = AdjudicatedGate::new(Box::new(adj)).with_mode(crate::mode::Mode::ALWAYS_ASK);
        let args = json!({"path": "src/lib.rs"});

        // **Stronger than it used to be, and the change is deliberate.** This asserted
        // `Admit` twice: `allow_session` was offered here, so the answer conformed, each
        // call was admitted, and only the *standing* part was refused.
        //
        // Now the option is not offered at a `Once` point, so naming it is a
        // non-conforming answer and the gate fails closed instead of admitting. An
        // adjudicator that answers with a button it was not given is not a decision this
        // gate can act on, whatever the button meant.
        let first = g.admit(&call("edit", &args));
        assert!(
            matches!(&first, GateDecision::Refuse { outcome, .. }
                if format!("{outcome:?}").contains("did not offer")),
            "an answer naming an unoffered option must fail closed, not admit: {first:?}"
        );
        let _ = g.admit(&call("edit", &args));
        assert_eq!(
            asked.load(std::sync::atomic::Ordering::Relaxed),
            2,
            "always-ask means always ask, whatever option was chosen"
        );
        assert!(
            g.grants().is_empty(),
            "and nothing standing is ever recorded at a Once point"
        );
    }

    /// **At `writes allowed` a write is not asked at all**, and the row still exists.
    ///
    /// The second half is the one worth a test: an admission nobody was consulted
    /// about is exactly the kind that stops appearing in an audit, and §4c's corpus
    /// wants it — *what the operator decided* includes the mode they put this project
    /// at.
    #[test]
    fn at_writes_allowed_a_write_goes_through_and_is_still_a_row() {
        let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let a = asked.clone();
        let adj = AskAdjudicator::new("test", move |req: &AdjudicationRequest| {
            a.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Some(AdjudicationDecision::selected(
                req,
                "deny",
                "human:test",
                "no",
            ))
        });
        let mut g =
            AdjudicatedGate::new(Box::new(adj)).with_mode(crate::mode::Mode::WRITES_ALLOWED);
        let args = json!({"path": "src/lib.rs"});
        assert_eq!(g.admit(&call("edit", &args)), GateDecision::Admit);
        assert_eq!(
            asked.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "nothing was consulted"
        );
        assert_eq!(g.log.len(), 1, "and it is still in the audit");
        assert_eq!(g.log[0].effect, "admit");
        assert!(
            g.log[0].decision.by.contains("mode"),
            "{:?}",
            g.log[0].decision
        );
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
            GateDecision::Refuse {
                outcome: ToolOutcome::Denied { req_id },
                ..
            } => {
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
            GateDecision::Refuse {
                outcome: ToolOutcome::NotRun { why },
                ..
            } => {
                assert!(
                    why.contains("non-conforming") || why.contains("did not offer"),
                    "{why}"
                );
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
        for path in [
            ".ssh/authorized_keys",
            "home/x/.aws/credentials",
            ".git/HEAD",
        ] {
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
            GateDecision::Refuse {
                outcome: ToolOutcome::NotRun { why },
                ..
            } => {
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
            Box::new(std::io::Cursor::new(
                b"allow_once because I said so\n".to_vec(),
            )),
            Box::new(Shared(out.clone())),
        );
        let mut g = AdjudicatedGate::new(Box::new(adj));
        let args = json!({"path": "src/lib.rs", "old_string": "a", "new_string": "b"});
        assert_eq!(g.admit(&call("edit", &args)), GateDecision::Admit);

        let shown = String::from_utf8(out.lock().unwrap().clone()).unwrap();
        assert!(
            shown.contains("write,host_project,reversible,free"),
            "{shown}"
        );
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
        assert!(
            detail.contains("WRITE TOOLS and no adjudicator"),
            "{detail}"
        );

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
                vec![crate::authorise::Utterance::operator(
                    "do whatever you like",
                    0,
                )],
                1,
            )
        })
    }

    #[test]
    fn a_disclosure_is_refused_by_a_gate_that_would_otherwise_admit_everything() {
        let mut g = permissive_gate();
        let args = json!({"command": "/bin/cat /home/dead/.ssh/id_rsa"});
        match g.admit(&bash(&args)) {
            GateDecision::Refuse {
                outcome: ToolOutcome::Denied { .. },
                tell,
            } => {
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
                Some(AdjudicationDecision::selected(
                    req,
                    widest,
                    "human:test",
                    "fine",
                ))
            },
        )))
        // At a point whose grants last a session, so the second half of this test —
        // that an ORDINARY may-approve exec call is asked about twice, grant or no — is
        // actually exercising the ask path, the grant table being closed to exec. `bash` is
        // `Access::Exec`, which still asks at this point, so the always-ask half is
        // unchanged by naming it.
        .with_mode(crate::mode::Mode::WRITES_ALLOWED)
        .with_surroundings(pinned())
        .with_trail_source(|_| crate::authorise::AuthorisationTrail::from_messages(vec![], 1));
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

        // While an ordinary may-approve exec call is asked about twice now, which is
        // the operator's rule of 2026-09-11: exec asks every time, and nothing settles it.
        let ordinary = json!({"command": "/bin/rm -rf /w/target"});
        assert_eq!(g.admit(&bash(&ordinary)), GateDecision::Admit);
        assert_eq!(g.admit(&bash(&ordinary)), GateDecision::Admit);
        assert_eq!(
            asked.load(std::sync::atomic::Ordering::Relaxed),
            4,
            "an exec-class call is asked every time; no session grant settles it"
        );
        assert!(
            g.grants().is_empty(),
            "no standing permission is recorded for an exec-class call"
        );
    }

    #[test]
    /// **The operator's rule: exec and bash ask every time, and nothing settles it.**
    ///
    /// 2026-09-11, the operator: *"i want something very simple for now - exec and
    /// bash always ask me for permission."* Three mechanisms hold it, the same three
    /// the always-ask list uses: the gate skips the mode's admission and the grant
    /// table for `Access::Exec`, the prompt never offers a standing option, and the
    /// recording site refuses one. The command below is a read inside the workspace —
    /// layer A's weakest verdict — because it is exactly the call clause 4 would wave
    /// through if the access class let it.
    fn exec_class_calls_ask_every_time_and_never_take_a_standing_grant() {
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
                Some(AdjudicationDecision::selected(
                    req,
                    widest,
                    "human:test",
                    "fine",
                ))
            },
        )))
        .with_mode(crate::mode::Mode::WRITES_ALLOWED)
        .with_surroundings(pinned())
        .with_trail_source(|_| crate::authorise::AuthorisationTrail::from_messages(vec![], 1));
        let args = json!({"command": "/bin/cat /w/src/lib.rs"});
        assert_eq!(g.admit(&bash(&args)), GateDecision::Admit);
        assert_eq!(g.admit(&bash(&args)), GateDecision::Admit);
        assert_eq!(
            asked.load(std::sync::atomic::Ordering::Relaxed),
            2,
            "the second identical call asks again: no mode and no grant settles exec"
        );
        assert!(
            g.grants().is_empty(),
            "no standing permission is recorded for an exec-class call"
        );
        assert!(
            g.log
                .iter()
                .all(|r| r.request.option("allow_session").is_none()),
            "exec is never offered a session grant"
        );
        // The revision of 2026-09-14: a DURABLE rule is offered — it is the
        // operator's own preapproval, in a file they read — and it is not a grant.
        assert!(
            g.log
                .iter()
                .all(|r| r.request.option("allow_always").is_some()),
            "exec is offered Always allow, as a rule"
        );
    }

    /// *"good old Allow Always"* (the operator, 2026-09-14): the answer writes a
    /// rule over the program and its verb, the rule admits the next call before
    /// the mode is consulted, `git log; rm x` under `git log*` still asks, and
    /// the sink saw the rule once.
    #[test]
    fn always_allow_on_an_exec_call_becomes_a_prefix_rule_that_is_written_down() {
        let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let a = asked.clone();
        let written: std::sync::Arc<std::sync::Mutex<Vec<crate::permission::Rule>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let w = written.clone();
        let mut g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
            "human",
            move |req: &AdjudicationRequest| {
                a.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Some(AdjudicationDecision::selected(
                    req,
                    "allow_always",
                    "human:test",
                    "fine",
                ))
            },
        )))
        .with_mode(crate::mode::Mode::WRITES_ALLOWED)
        .with_permission(crate::permission::seed())
        .with_permission_sink(std::sync::Arc::new(move |r| {
            w.lock().unwrap().push(r.clone());
            Ok(())
        }))
        .with_surroundings(pinned())
        .with_trail_source(|_| crate::authorise::AuthorisationTrail::from_messages(vec![], 1));
        // `cargo run` is not on the shipped list, so the first call asks.
        let args = json!({"command": "cargo run --bin x"});
        assert_eq!(g.admit(&bash(&args)), GateDecision::Admit);
        assert_eq!(asked.load(std::sync::atomic::Ordering::Relaxed), 1);
        let rules = written.lock().unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].permission, "bash");
        assert_eq!(rules[0].pattern, "cargo run*");
        drop(rules);
        // The second, different `cargo run` is admitted by the rule, nobody asked.
        let again = json!({"command": "cargo run --bin y -- --flag"});
        assert_eq!(g.admit(&bash(&again)), GateDecision::Admit);
        assert_eq!(asked.load(std::sync::atomic::Ordering::Relaxed), 1);
        // A compound that hides a second command behind the prefix asks.
        let hidden = json!({"command": "cargo run; rm -rf /w"});
        assert_eq!(g.admit(&bash(&hidden)), GateDecision::Admit);
        assert_eq!(asked.load(std::sync::atomic::Ordering::Relaxed), 2);
        // And the shipped list admits a read-only git without asking at all.
        let ro = json!({"command": "git status --short"});
        assert_eq!(g.admit(&bash(&ro)), GateDecision::Admit);
        assert_eq!(asked.load(std::sync::atomic::Ordering::Relaxed), 2);
    }

    #[test]
    fn an_unresolvable_command_reaching_the_gate_is_not_run_and_never_admitted() {
        let mut g = permissive_gate();
        let args = json!({"command": "/bin/cat $FILE"});
        match g.admit(&bash(&args)) {
            GateDecision::Refuse {
                outcome: ToolOutcome::NotRun { why },
                tell,
            } => {
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
            GateDecision::Refuse {
                outcome: ToolOutcome::NotRun { why },
                ..
            } => {
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
                Some(AdjudicationDecision::selected(
                    req,
                    "deny",
                    "human:test",
                    "no",
                ))
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
            assert!(
                matches!(g.admit(&bash(&args)), GateDecision::Refuse { .. }),
                "{cmd}"
            );
        }
        assert_eq!(consulted.load(std::sync::atomic::Ordering::Relaxed), 3);

        // The fourth is not adjudicated at all.
        let args = json!({"command": "/usr/bin/less /w/fourth.txt"});
        match g.admit(&bash(&args)) {
            GateDecision::Refuse {
                outcome: ToolOutcome::NotRun { why },
                tell,
            } => {
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
                    vec![
                        crate::authorise::Utterance::operator("yeah restart it", 1).at_seconds(30),
                    ],
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
            GateDecision::Refuse {
                outcome: ToolOutcome::NotRun { why },
                ..
            } => {
                assert!(why.contains("escalated"), "{why}")
            }
            other => panic!("{other:?}"),
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

    /// §4h's loop, closed: a second call of the same shape is shown the first
    /// answer, with its count and its age. The prior rows are keyed on the task
    /// direction — `(tool, intents, scope, regions)` — so a re-spelled command in
    /// the same direction matches and a different direction does not.
    #[test]
    fn a_second_call_of_the_same_shape_shows_the_first_answer_with_its_count_and_age() {
        let mut g = AdjudicatedGate::closed().with_surroundings(pinned());
        let args = json!({"command": "/bin/ls /w"});

        // The first call: no history. (It refuses — closed gate — which is itself
        // a row, and the denial is what the second call must see.)
        let req1 = g.request_for(&bash_at(&args, "s#3"));
        assert!(
            req1.prior.is_empty(),
            "nothing has been decided yet: {:?}",
            req1.prior
        );
        let _ = g.admit(&bash_at(&args, "s#3"));

        // A re-spelling of the same direction, three turns later.
        let req2 = g.request_for(&bash_at(&json!({"command": "/bin/ls -a /w"}), "s#6"));
        assert_eq!(req2.prior.len(), 1, "{:?}", req2.prior);
        let p = &req2.prior[0];
        assert_eq!(p.effect, "refuse");
        assert_eq!(p.count, 1);
        assert_eq!(p.latest_turns_ago, Some(3));
        assert_eq!(p.first_turns_ago, Some(3));

        // A different direction does not inherit the answer: regions differ, and
        // a history for one shape is not a history for the next.
        let other = g.request_for(&bash_at(&json!({"command": "/bin/ls /elsewhere"}), "s#6"));
        assert!(
            other.prior.is_empty(),
            "a different region set is a different direction: {:?}",
            other.prior
        );
    }

    /// *"A denial is history too."* Both effects appear, denials beside approvals,
    /// each with its own count and age — and the ages differ, because *"once,
    /// three weeks ago"* and *"nine times this week"* are different facts.
    #[test]
    fn a_denial_is_history_too_and_sits_beside_the_approvals() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let n = AtomicUsize::new(0);
        let adj = AskAdjudicator::new("test", move |req: &AdjudicationRequest| {
            let i = n.fetch_add(1, Ordering::Relaxed);
            if i == 0 {
                // First: refused.
                Some(AdjudicationDecision::selected(
                    req,
                    "deny_and_tell",
                    "human:test",
                    "not today",
                ))
            } else {
                // Then: allowed.
                Some(AdjudicationDecision::selected(
                    req,
                    "allow_once",
                    "human:test",
                    "fine",
                ))
            }
        });
        let mut g = AdjudicatedGate::new(Box::new(adj)).with_surroundings(pinned());
        let args = json!({"command": "/bin/ls /w"});
        let _ = g.admit(&bash_at(&args, "s#2"));
        let _ = g.admit(&bash_at(&args, "s#4"));

        let req3 = g.request_for(&bash_at(&args, "s#9"));
        let refuse = req3
            .prior
            .iter()
            .find(|p| p.effect == "refuse")
            .expect("the denial is history too");
        assert_eq!(refuse.count, 1);
        assert_eq!(refuse.latest_turns_ago, Some(7));
        let admit = req3
            .prior
            .iter()
            .find(|p| p.effect == "admit")
            .expect("so is the approval");
        assert_eq!(admit.count, 1);
        assert_eq!(admit.latest_turns_ago, Some(5));
    }

    /// *"History is evidence, never precedent"* — and it never lifts a tier. Two
    /// approvals of the same shape do not turn an always-ask action into one the
    /// gate admits unasked: the tier is layer A's, the option list still offers
    /// only what the gate would honour, and a third ask still has to be answered
    /// by somebody.
    #[test]
    fn a_history_of_approvals_cannot_move_an_always_ask_action() {
        use std::sync::Mutex;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let n = AtomicUsize::new(0);
        let seen: std::sync::Arc<Mutex<Vec<(String, Vec<String>)>>> =
            std::sync::Arc::new(Mutex::new(Vec::new()));
        let s = seen.clone();
        let adj = AskAdjudicator::new("test", move |req: &AdjudicationRequest| {
            let i = n.fetch_add(1, Ordering::Relaxed);
            s.lock().unwrap().push((
                req.tier.as_str().to_string(),
                req.options.iter().map(|o| o.id.to_string()).collect(),
            ));
            if i < 2 {
                Some(AdjudicationDecision::selected(
                    req,
                    "allow_once",
                    "human:test",
                    "allowed once, by a human",
                ))
            } else {
                None
            }
        });
        let mut g = AdjudicatedGate::new(Box::new(adj)).with_surroundings(pinned());
        let args = json!({"command": "sudo systemctl restart foo"});
        // Two approvals in the direction — `sudo`, the privilege-escalation ask.
        assert_eq!(g.admit(&bash_at(&args, "s#1")), GateDecision::Admit);
        assert_eq!(g.admit(&bash_at(&args, "s#2")), GateDecision::Admit);

        // The third call: the history says admitted twice, and the gate asks
        // anyway — and with nobody to answer, nothing ran.
        match g.admit(&bash_at(&args, "s#3")) {
            GateDecision::Refuse {
                outcome: ToolOutcome::NotRun { .. },
                ..
            } => {}
            other => panic!("history must not admit an always-ask action: {other:?}"),
        }
        let tiers = seen.lock().unwrap();
        for (tier, options) in tiers.iter() {
            assert_eq!(tier, "always_ask", "the tier never moved: {tier}");
            assert!(
                !options.iter().any(|o| o == "allow_session"),
                "an always-ask action is never offered a standing grant: {options:?}"
            );
        }
    }
}
