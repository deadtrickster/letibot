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
}

impl AdjudicationRequest {
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
        }
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

    fn next_id(&mut self) -> String {
        self.seq += 1;
        format!("adj-{}-{:04}", self.session_id, self.seq)
    }

    /// The request one call produces. Public because the head and the tests both
    /// want to see a request without a decision having been made about it.
    pub fn request_for(&mut self, call: &GateCall<'_>) -> AdjudicationRequest {
        let path = call
            .args
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("<no path argument>");
        let inside = call.path_is_inside();
        // Reversibility is a fact about the target, and the runtime stats it
        // before asking. `None` — no path argument at all — is treated as
        // reversible: claiming irreversibility about a call whose target nobody
        // looked at would be a stronger statement than the evidence supports.
        let creates = call.target_exists == Some(false);
        let class = ActionClass::host(call.access, inside, creates);
        let digest = crate::events::payload_digest(&call.args.to_string());
        let mut facts = vec![
            format!("workspace: {}", call.workspace),
            format!(
                "the path is {} the session's workspace",
                if inside { "inside" } else { "OUTSIDE" }
            ),
            "the host filesystem is not sandboxed; §11.4's boundary arrives with firecode"
                .to_string(),
        ];
        if creates {
            facts.push("this creates a file that does not exist, so there is nothing to restore".into());
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
                "`{}` wants {} access to `{path}`",
                call.name,
                call.access.as_str()
            ),
            arguments: call.args.clone(),
            arguments_digest: digest,
            boundary_facts: facts,
            kind: RequestKind::Permission,
            options: permission_options(),
            // §11.5: `Deny` for a permission, and `AgentDecides` only for a
            // question. Nothing here is a question yet.
            on_timeout: OnTimeout::Deny,
        }
    }

    fn class_key(req: &AdjudicationRequest) -> String {
        format!("{}|{}", req.tool, req.class)
    }

    fn record(&mut self, request: AdjudicationRequest, decision: AdjudicationDecision, effect: &'static str) {
        self.log.push(AdjudicationRow {
            request,
            decision,
            effect,
        });
    }
}

impl Gate for AdjudicatedGate {
    fn describe(&self) -> String {
        self.adjudicator.describe()
    }

    fn admit(&mut self, call: &GateCall<'_>) -> GateDecision {
        let req = self.request_for(call);

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
            let why = d.basis.clone();
            let id = req.id.clone();
            self.record(req, d, "refuse");
            // `deny_and_tell`: the model is told why, because a refusal it cannot
            // understand is a refusal it will retry.
            return GateDecision::refuse_and_tell(ToolOutcome::Denied { req_id: id }, why);
        }

        // 2. A class already granted this session. Recorded as a row, because a
        //    grant that stops appearing in the audit is a grant nobody can review.
        let key = Self::class_key(&req);
        if self.granted.contains(&key) {
            let d = AdjudicationDecision::selected(
                &req,
                "allow_session",
                "gate:session-grant",
                "this class was granted for the session by an earlier decision",
            );
            self.record(req, d, "admit");
            return GateDecision::Admit;
        }

        // 3. Ask.
        let decision = self.adjudicator.decide(&req);

        match &decision.outcome {
            DecisionOutcome::Selected { option_id } => {
                let kind = req.option(option_id).map(|o| o.kind);
                match kind {
                    Some(k) if k.admits() => {
                        if matches!(k, OptionKind::AllowSession | OptionKind::AllowAlways) {
                            self.granted.insert(key);
                        }
                        self.record(req, decision, "admit");
                        GateDecision::Admit
                    }
                    Some(k) => {
                        let id = req.id.clone();
                        // §11.6's two denials differ in exactly one place: whether
                        // the reason reaches the model or stops at the row.
                        let tell = if matches!(k, OptionKind::DenyAndTell) {
                            decision.basis.clone()
                        } else {
                            String::new()
                        };
                        self.record(req, decision, "refuse");
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
                        self.record(req, decision, "refuse");
                        GateDecision::refuse(ToolOutcome::NotRun { why })
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
                self.record(req, decision, "refuse");
                GateDecision::refuse(ToolOutcome::NotRun { why: msg })
            }
            DecisionOutcome::Unavailable | DecisionOutcome::Timeout | DecisionOutcome::Cancelled => {
                // §11.7: *"never a silent allow"*. Route to `on_timeout`, and note
                // that `OnTimeout::Allow` is not reachable from
                // [`AdjudicatedGate::request_for`], which always sets `Deny` — the
                // arm exists so that a W11 policy row saying `Allow` has somewhere
                // to land, and so that its absence today is visible.
                let outcome = match req.on_timeout {
                    OnTimeout::Allow => {
                        self.record(req, decision, "admit");
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
                self.record(req, decision, "refuse");
                GateDecision::refuse(outcome)
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
    if !has_write_tools {
        return (
            "N/A",
            "this session has only read-only tools, which never prompt (clause 4). \
             Nothing can reach the gate, so nothing needs an adjudicator."
                .into(),
            false,
        );
    }
    let attached = !adjudicator.starts_with("none");
    match (attached, backend_writable) {
        (false, _) => (
            "NONE",
            format!(
                "this session has WRITE TOOLS and no adjudicator ({adjudicator}). Every \
                 `write` and `edit` call will refuse with NotRun and change nothing. \
                 That is the fail-closed default and not a fault; attach an adjudicator \
                 to make them callable."
            ),
            false,
        ),
        (true, false) => (
            "GATE ONLY",
            format!(
                "an adjudicator is attached ({adjudicator}), but the execution backend was \
                 opened READ-ONLY, so an admitted write still cannot reach the disk. Two \
                 gates, and the second one is shut."
            ),
            false,
        ),
        (true, true) => (
            "",
            format!(
                "{adjudicator}. Write tools are callable: an admitted call reaches the \
                 disk. Read-only tools still never prompt (clause 4)."
            ),
            true,
        ),
    }
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
        assert!(detail.contains("reaches the disk"), "{detail}");

        // And the M1 sentence stays true for an M1 session.
        let (state, _, active) = startup_disclosure("none attached", false, false);
        assert_eq!(state, "N/A");
        assert!(!active);
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
