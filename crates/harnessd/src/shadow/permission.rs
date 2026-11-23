//! The `permission` job beside the operator: every call the gate put in front of them is judged
//! once the card settles, and their answer is the label. It decides only as the oracle
//! (`super::oracle`), within stage 1.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use letibot_sessionlog::pending_asks::{
    Annotator, AskerGate, AskerRule, RelayedCall, RevokedGrant,
};
use letibot_tools::authorise::{CorpusRow, CorpusSink, OperatorOverride};
use letibot_tools::permission::{Action, Rule, Ruleset};

use super::Job;
use super::config::Stage;
use super::guard::Call;
use super::record::Recorder;
use super::runner::{Outcome, Shadow};
use super::verdict::{Category, Decision, Shown, Verdict};

/// The shadow a gate built at open reaches once the session's shadow is opened.
pub type Cell = Arc<Mutex<Option<(Arc<Shadow>, Arc<dyn Recorder>)>>>;

/// The data a permission check is shown: the call, where it runs, the preapproved rules, and the
/// operator's last words as the stated task. `request` ties the verdict to the operator's answer.
pub fn shown(
    request: &str,
    tool: &str,
    arguments: &serde_json::Value,
    rules: &[String],
    task: Option<&str>,
    call: Call,
) -> Shown {
    let mut d = format!(
        "request: {request}\ncall: {tool} {}\ncwd: {}\nworkspace: {}\n\npreapproved rules:\n",
        arguments,
        call.cwd.display(),
        call.workspace.display()
    );
    if rules.is_empty() {
        d.push_str("(none)\n");
    }
    for r in rules {
        d.push_str(&format!("- {r}\n"));
    }
    d.push_str("\nstated task:\n");
    d.push_str(task.unwrap_or("(none)").trim());
    d.push('\n');
    Shown {
        data: d,
        shortlist: Vec::new(),
        call: Some(call),
        rules: rules.to_vec(),
        ..Shown::default()
    }
}

/// The persistent shadow's question about one asked call. The stated task is the operator's words
/// it was already fed; the call and the rules are the harness's.
pub fn question(
    request: &str,
    tool: &str,
    arguments: &serde_json::Value,
    rules: &[String],
    call: &Call,
) -> super::conversation::Question {
    let mut d = format!(
        "request: {request}\ncall: {tool} {arguments}\ncwd: {}\nworkspace: {}\n\npreapproved rules:\n",
        call.cwd.display(),
        call.workspace.display()
    );
    if rules.is_empty() {
        d.push_str("(none)\n");
    }
    for r in rules {
        d.push_str(&format!("- {r}\n"));
    }
    d.push_str("\nThe stated task is the operator's latest own words above.\n");
    super::conversation::Question {
        job: Job::Permission,
        facts: d,
        turn_from: None,
        rules: rules.to_vec(),
    }
}

/// The corpus sink with the shadow beside it. The inner sink is unchanged; the shadow only reads.
pub struct ShadowSink {
    pub inner: Option<Arc<dyn CorpusSink>>,
    pub cell: Cell,
    pub rules: Vec<String>,
    pub workspace: PathBuf,
    pub home: PathBuf,
}

impl ShadowSink {
    /// One asked call, settled with `effect`: queued for the shadow, never waited on, and the
    /// operator's answer written beside it as the label.
    pub fn observe(
        &self,
        request: &str,
        tool: &str,
        arguments: &serde_json::Value,
        effect: &str,
        task: Option<&str>,
    ) {
        let Some((shadow, rec)) = self.cell.lock().unwrap_or_else(|e| e.into_inner()).clone()
        else {
            return;
        };
        let call = Call {
            tool: tool.to_string(),
            args: arguments.clone(),
            cwd: self.workspace.clone(),
            workspace: self.workspace.clone(),
            home: self.home.clone(),
            // A relayed card names a path this record has no view to place; the boundary it
            // runs in already answered whether the path is there.
            granted: Vec::new(),
            temp: super::guard::temp_roots(&self.home),
        };
        let q = question(request, tool, arguments, &self.rules, &call);
        // The operator's own words are the whole of "did they ask for this". The gate's trail can be
        // empty of them, and the shadow's feed is the same rows the operator and the model see, so
        // the brief asks there rather than stating "(none)".
        let task = task.map(str::to_string).or_else(|| shadow.operator_words());
        let s = shown(request, tool, arguments, &self.rules, task.as_deref(), call);
        shadow.submit_asking(Job::Permission, s, q);
        rec.row(
            &format!("permission · operator answered {effect} {request}"),
            "",
        );
    }
}

impl CorpusSink for ShadowSink {
    fn decided(&self, row: &CorpusRow) {
        if let Some(i) = &self.inner {
            i.decided(row);
        }
        if !row.asked {
            return;
        }
        let task = row
            .trail
            .utterances
            .iter()
            .rev()
            .find(|u| u.speaker == letibot_tools::authorise::Speaker::Operator)
            .map(|u| u.text.clone());
        self.observe(
            &row.request_id,
            &row.tool,
            &row.arguments,
            row.effect,
            task.as_deref(),
        );
    }

    fn ruled(&self, request_id: &str, what: &OperatorOverride) {
        if let Some(i) = &self.inner {
            i.ruled(request_id, what);
        }
        if let Some((_, rec)) = self.cell.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            rec.row(
                &format!("permission · operator {} {request_id}", what.as_str()),
                "",
            );
        }
    }

    fn decisions_recorded(&self, session_id: &str) -> u64 {
        self.inner
            .as_ref()
            .map_or(0, |i| i.decisions_recorded(session_id))
    }
}

/// Disagreements that turn answering off: an operator revoke, or a later No to a shape the
/// shadow granted.
pub const BREAKER_AT: u32 = 2;

/// How many blocks in a row, and in one session, before the operator is asked instead. Claude
/// Code's numbers, which the operator named as the thing to mirror; `[automode]` may set others.
pub const BLOCKS_IN_A_ROW: u32 = 3;
pub const BLOCKS_A_SESSION: u32 = 20;

/// How many unanswered cards' shapes a session keeps for scoring a later No, at most.
const ASKED_MAX: usize = 1024;

/// The breaker's job key in the store, beside the session whose shadow answers.
const BREAKER_JOB: &str = "permission";

/// The main session's answering state, kept across the relays built for it: the switch, the
/// breaker, the shapes granted, and the one row a tripped breaker owes the person. With a store the
/// breaker and the granted shapes outlive the daemon, as the grants they guard do.
pub struct Answering {
    on: AtomicBool,
    /// The stages as configured: what a re-arm may turn back on.
    configured: bool,
    at: Option<(crate::config::StoreAt, String)>,
    disagreements: AtomicU32,
    /// Blocks in a row, and in this session. The reviewer's block is not a card, so these are what
    /// stop a session that keeps hitting the same wall getting a sentence it never brings to anyone.
    block_run: AtomicU32,
    blocks: AtomicU32,
    block_limits: Mutex<(u32, u32)>,
    granted: Mutex<BTreeSet<String>>,
    /// Cards that went to the operator, by request, with their shape.
    asked: Mutex<BTreeMap<String, String>>,
    /// Rows owed to the main session: the breaker's, and nothing else.
    rows: Mutex<Vec<(String, String)>>,
    wake: Mutex<Option<super::Wake>>,
}

impl Answering {
    pub fn new(on: bool) -> Arc<Answering> {
        Answering::open(on, None)
    }

    /// The state as `at` keeps it: a tripped breaker stays tripped, its count stands, and the
    /// shapes the shadow granted are read back from its grants. An unreadable store answers nothing.
    pub fn open(configured: bool, at: Option<(crate::config::StoreAt, String)>) -> Arc<Answering> {
        let mut on = configured;
        let mut disagreements = 0;
        let mut granted = BTreeSet::new();
        if let Some((at, session)) = &at {
            match at.open() {
                Ok(store) => {
                    match store.breaker(session, BREAKER_JOB) {
                        Ok(Some(b)) => {
                            disagreements = b.disagreements;
                            on &= b.tripped_ms.is_none();
                        }
                        Ok(None) => {}
                        Err(_) => on = false,
                    }
                    for g in crate::grants::shadow_grants(&store).unwrap_or_default() {
                        if let letibot_tools::grant_ledger::Kept::View { path, .. } = &g.kept {
                            granted.insert(shape(Stage::Reads, &path.display().to_string()));
                        }
                    }
                }
                Err(_) => on = false,
            }
        }
        Arc::new(Answering {
            on: AtomicBool::new(on),
            configured,
            at,
            disagreements: AtomicU32::new(disagreements),
            block_run: AtomicU32::new(0),
            blocks: AtomicU32::new(0),
            block_limits: Mutex::new((BLOCKS_IN_A_ROW, BLOCKS_A_SESSION)),
            granted: Mutex::new(granted),
            asked: Mutex::default(),
            rows: Mutex::default(),
            wake: Mutex::default(),
        })
    }

    /// The limits as configured (`[automode]`), for a box that wants other numbers: blocks in a row
    /// before the operator is asked, and blocks in one session.
    pub fn with_block_limits(&self, run: u32, session: u32) {
        *self.block_limits.lock().unwrap_or_else(|e| e.into_inner()) = (run.max(1), session.max(1));
    }

    /// One block, counted. `true` when the reviewer has now blocked enough that the operator is owed
    /// the question rather than a sentence in a tool result - and a tripping block resets the run,
    /// or every later block would trip again on a session already at its limit.
    pub fn blocked(&self, category: Category) -> bool {
        let (run_max, session_max) = *self.block_limits.lock().unwrap_or_else(|e| e.into_inner());
        let run = self.block_run.fetch_add(1, Ordering::SeqCst) + 1;
        let all = self.blocks.fetch_add(1, Ordering::SeqCst) + 1;
        if run < run_max && all < session_max {
            return false;
        }
        self.block_run.store(0, Ordering::SeqCst);
        self.push_row(
            format!(
                "automode blocked {run} in a row ({all} this session); the next one is your card"
            ),
            format!(
                "the last was `{}`. A reviewer that blocks this often is usually wrong about the \
                 project rather than right about the call, so this one is going to you",
                category.as_str()
            ),
        );
        true
    }

    /// A call the reviewer allowed: the run of blocks is over.
    pub fn cleared(&self) {
        self.block_run.store(0, Ordering::SeqCst);
    }

    /// Whether the breaker is tripped, as opposed to answering never being configured.
    pub fn tripped(&self) -> bool {
        self.configured && !self.is_on()
    }

    /// `/shadow arm`: the operator's explicit re-arm, the only way a tripped breaker clears.
    pub fn arm(&self) -> Result<String, String> {
        if !self.configured {
            return Err(
                "no stage is on in [shadow] (`answers`), so there is nothing to arm".into(),
            );
        }
        self.persist(0, None, "")?;
        self.disagreements.store(0, Ordering::SeqCst);
        self.on.store(true, Ordering::SeqCst);
        Ok("the shadow answers its stages again, with 0 disagreements".into())
    }

    /// The breaker as the store keeps it. No store is nothing to keep.
    fn persist(&self, n: u32, tripped_ms: Option<i64>, why: &str) -> Result<(), String> {
        let Some((at, session)) = &self.at else {
            return Ok(());
        };
        at.open()
            .map_err(|e| e.to_string())
            .and_then(|s| {
                s.put_breaker(
                    session,
                    BREAKER_JOB,
                    &letibot_tokencore::store::BreakerRow {
                        disagreements: n,
                        tripped_ms,
                        why: why.to_string(),
                    },
                )
                .map_err(|e| e.to_string())
            })
            .map_err(|e| format!("the shadow's breaker could not be kept: {e}"))
    }

    /// Answering, and the breaker has not tripped.
    pub fn is_on(&self) -> bool {
        self.on.load(Ordering::SeqCst)
    }

    pub fn disagreements(&self) -> u32 {
        self.disagreements.load(Ordering::SeqCst)
    }

    pub fn set_wake(&self, w: super::Wake) {
        *self.wake.lock().unwrap_or_else(|e| e.into_inner()) = Some(w);
    }

    /// The main session's rows since the last take, as `(title, body)`: a breaker that tripped.
    pub fn take_rows(&self) -> Vec<(String, String)> {
        std::mem::take(&mut *self.rows.lock().unwrap_or_else(|e| e.into_inner()))
    }

    fn push_row(&self, title: String, body: String) {
        self.rows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((title, body));
        let wake = self.wake.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(w) = wake {
            w();
        }
    }

    /// A call the shadow allowed at `stage`: its shape counts toward a later No, and nothing is
    /// said. A decision the machinery took while a turn ran is not news, so it is no card; what a
    /// person may want to review later is the gate's audit row (its `by` is `shadow:<judge>`, kept
    /// in the store's corpus) and the shadow session's own record of the exchange.
    pub fn granted(&self, stage: Stage, target: &str) {
        self.granted
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(shape(stage, target));
    }

    /// A call the shadow was asked about and left to the operator, so their later No can be scored
    /// against the same shape. Bounded: a run long enough to overflow it loses that signal, which
    /// is cheaper than a map that grows with every card.
    pub fn asked(&self, req_id: &str, stage: Stage, target: &str) {
        let mut asked = self.asked.lock().unwrap_or_else(|e| e.into_inner());
        if asked.len() >= ASKED_MAX {
            asked.clear();
        }
        asked.insert(req_id.to_string(), shape(stage, target));
    }

    /// One disagreement, and the breaker at [`BREAKER_AT`]: answering stops, observe-only.
    fn disagree(&self, why: &str, rec: Option<&Arc<dyn Recorder>>) {
        let n = self.disagreements.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(r) = rec {
            r.row(&format!("permission · disagreement {n}: {why}"), "");
        }
        let trips = n >= BREAKER_AT;
        // A breaker that cannot be kept trips here: a restart must not find it armed.
        let kept = self.persist(n, trips.then(unix_ms), why);
        if (trips || kept.is_err()) && self.on.swap(false, Ordering::SeqCst) {
            let title = format!(
                "shadow stopped answering after {n} disagreements · observe-only until /shadow arm"
            );
            if let Some(r) = rec {
                r.row(&format!("permission · {title}"), why);
            }
            self.push_row(title, format!("the last disagreement: {why}"));
        }
    }
}

fn unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// The ask's shape for the breaker: the stage and the target's first three components. Any seat
/// asking the same thing is the same shape, so a shape read back from a stored grant needs no seat.
fn shape(stage: Stage, target: &str) -> String {
    let prefix: PathBuf = std::path::Path::new(target).components().take(3).collect();
    format!("{} {}", stage.as_str(), prefix.display())
}

/// The seat kind of a review-queue asker, from its session id and never its role string: a
/// role is a label the queue wrote, and a label is no membership.
fn queue_kind(asker: &str) -> Option<&'static str> {
    match letibot_tools::grant_ledger::queue_of(asker)? {
        root if root == asker => Some("gatekeeper"),
        _ => Some("reviewer"),
    }
}

/// Whether `c` is a call the shadow may answer: a review-queue seat's read of one path outside
/// its view, past the read denylist and inside the asker's repository (C8). `Ok` is the path and
/// the seat kind.
pub fn scope(c: &RelayedCall, home: &std::path::Path) -> Result<(String, &'static str), String> {
    let path = c.view_read.clone().ok_or("not a read-only view widening")?;
    if path.chars().any(char::is_control) {
        return Err("the path carries a control character".into());
    }
    let kind = queue_kind(&c.asker).ok_or("not a review-queue seat")?;
    let gate = c.gate.as_ref().ok_or("the asker's gate is not known")?;
    super::guard::read_passes(&path, home)?;
    super::guard::within_roots(&path, &super::guard::read_roots(&gate.workspace))?;
    Ok((path, kind))
}

/// Whether a relayed call may go to the main session's oracle: [`scope`], and an Allow rule of
/// the asker's own that covers the read (C7). `Ok` is the path.
pub fn relayed_read(c: &RelayedCall, home: &std::path::Path) -> Result<String, String> {
    let (path, _) = scope(c, home)?;
    let gate = c.gate.as_ref().ok_or("the asker's gate is not known")?;
    covering_rule(&path, &ruleset_of(gate))
        .map(|_| path.clone())
        .ok_or_else(|| format!("no rule of the asker's allows a read of `{path}`"))
}

/// The rule an `allow` may act on: the asker's rule that covers a read of `path` by the gate's
/// own matcher, last match wins and a Deny is seen. The model's verdict only gates it; the rule
/// it quoted is a label. Void, stale, timed out, `ask` or no covering Allow is `None`.
pub fn acting_rule(outcome: &Outcome, path: &str, rules: &Ruleset) -> Option<String> {
    let Outcome::Verdict(Verdict::Permission {
        decision: Decision::Allow { .. },
        ..
    }) = outcome
    else {
        return None;
    };
    covering_rule(path, rules)
}

/// The Allow rule in `rules` that covers a read of `path` at its canonical key, or `None`.
pub fn covering_rule(path: &str, rules: &Ruleset) -> Option<String> {
    let key = letibot_tools::view::key(std::path::Path::new(path));
    let r = letibot_tools::permission::evaluate("read", &key.to_string_lossy(), &[rules]);
    (r.action == Action::Allow).then(|| format!("{} {}", r.permission, r.pattern))
}

/// A session's gate as its relayed calls carry it.
pub fn asker_gate(rules: &[Rule], workspace: &std::path::Path) -> AskerGate {
    AskerGate {
        workspace: workspace.to_path_buf(),
        rules: rules
            .iter()
            .map(|r| AskerRule {
                permission: r.permission.clone(),
                pattern: r.pattern.clone(),
                action: r.action.as_str().to_string(),
            })
            .collect(),
    }
}

/// The asker's rules back as the gate's type. An action this build cannot read is a Deny.
pub fn ruleset_of(g: &AskerGate) -> Ruleset {
    g.rules
        .iter()
        .map(|r| {
            let action = Action::parse(&r.action).unwrap_or(Action::Deny);
            Rule::new(r.permission.clone(), r.pattern.clone(), action)
        })
        .collect()
}

/// The main session's shadow beside a relayed card: one line on it, and the operator's answer
/// beside the verdict in the record. Annotator only: a relayed read is settled by the set's
/// oracle ([`super::oracle::ShadowAdjudicator`]) or by the operator, never here.
pub struct Relay {
    pub cell: Cell,
    pub home: PathBuf,
    pub answering: Arc<Answering>,
}

impl Relay {
    fn parts(&self) -> Option<(Arc<Shadow>, Arc<dyn Recorder>)> {
        self.cell.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub(crate) fn shown_for(&self, c: &RelayedCall) -> Shown {
        let workspace = c
            .gate
            .as_ref()
            .map(|g| g.workspace.clone())
            .unwrap_or_default();
        let rules = c
            .gate
            .as_ref()
            .map(|g| rules_of(&ruleset_of(g)))
            .unwrap_or_default();
        let call = Call {
            tool: c.tool.clone(),
            args: c.arguments.clone(),
            cwd: workspace.clone(),
            workspace,
            home: self.home.clone(),
            granted: Vec::new(),
            temp: super::guard::temp_roots(&self.home),
        };
        let mut s = shown(
            &c.req_id,
            &c.tool,
            &c.arguments,
            &rules,
            c.task.as_deref(),
            call,
        );
        // The asker wrote the call, so its bytes are the asker's: never quotable as the operator's.
        let asker = super::words::Author::Peer(c.asker.clone());
        let line = format!("call: {} {}", c.tool, c.arguments);
        if let Some(at) = s.data.find(&line) {
            s.spans.push(super::verdict::Span {
                start: at,
                end: at + line.len(),
                author: asker.clone(),
                row: None,
            });
        }
        s.data.push_str(&format!(
            "\nasked by: {} (relayed)\n",
            c.role.as_deref().unwrap_or(&c.asker)
        ));
        s.push_said("layer A:", asker.clone(), None, &c.reading);
        if let Some(p) = &c.view_read {
            s.push_said(
                "asking: read access, outside the asker's view; no write, exec or network:",
                asker,
                None,
                p,
            );
        }
        s
    }
}

impl Annotator for Relay {
    fn line(&self, c: &RelayedCall) -> Option<String> {
        let (shadow, rec) = self.parts()?;
        rec.row(
            &format!("permission · relayed {} from {}", c.req_id, c.asker),
            "",
        );
        if let Ok((path, _)) = scope(c, &self.home) {
            self.answering.asked(&c.req_id, Stage::Reads, &path);
        }
        let outcome = shadow.ask(Job::Permission, self.shown_for(c))?;
        line_of(shadow.label(Job::Permission).as_deref(), &outcome)
    }

    fn answered(&self, req_id: &str, effect: &str) {
        let rec = self.parts().map(|(_, r)| r);
        if let Some(r) = &rec {
            r.row(
                &format!("permission · operator answered {effect} {req_id}"),
                "",
            );
        }
        let shape = self
            .answering
            .asked
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(req_id);
        if effect == "refuse"
            && let Some(shape) = shape
            && self
                .answering
                .granted
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&shape)
        {
            self.answering.disagree(
                &format!("operator answered No to {req_id}, a shape the shadow granted ({shape})"),
                rec.as_ref(),
            );
        }
    }

    fn revoked(&self, g: &RevokedGrant) {
        let rec = self.parts().map(|(_, r)| r);
        self.answering.disagree(
            &format!(
                "operator revoked the read of {} for {} (grant {})",
                g.path, g.session, g.id
            ),
            rec.as_ref(),
        );
    }
}

/// A verdict as the card's line; anything else is no line.
pub fn line_of(judge: Option<&str>, outcome: &Outcome) -> Option<String> {
    let Outcome::Verdict(Verdict::Permission { decision, evidence }) = outcome else {
        return None;
    };
    let who = judge.map_or_else(|| "shadow".to_string(), |j| format!("shadow ({j})"));
    let tail = |s: &str| {
        if s.trim().is_empty() {
            String::new()
        } else {
            format!(" - {}", s.trim())
        }
    };
    Some(match decision {
        Decision::Allow { rule, .. } if rule.trim().is_empty() => {
            format!("{who}: no block{}", tail(evidence))
        }
        Decision::Allow { rule, .. } => format!("{who}: allow - matches rule \"{rule}\""),
        Decision::Ask => format!("{who}: ask{}", tail(evidence)),
        Decision::Block { category, reason } => {
            format!("{who}: block ({}) - {reason}", category.as_str())
        }
    })
}

/// The preapproved rules, one line each, as a check is shown them.
pub fn rules_of(rules: &[letibot_tools::permission::Rule]) -> Vec<String> {
    rules
        .iter()
        .filter(|r| r.action == letibot_tools::permission::Action::Allow)
        .map(|r| format!("{} {}", r.permission, r.pattern))
        .collect()
}
