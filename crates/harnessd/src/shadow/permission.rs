//! The `permission` job, observe-only: every call the gate put in front of the operator is judged
//! by the shadow once the card settles, without waiting, and the operator's answer is the label.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use letibot_sessionlog::pending_asks::{
    Annotator, AskerGate, AskerRule, Note, RelayedCall, RevokedGrant,
};
use letibot_tools::authorise::{CorpusRow, CorpusSink, OperatorOverride};
use letibot_tools::permission::{Action, Rule, Ruleset};

use super::Job;
use super::guard::Call;
use super::record::Recorder;
use super::runner::{Outcome, Shadow};
use super::verdict::{Decision, Shown, Verdict};

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
        };
        let q = question(request, tool, arguments, &self.rules, &call);
        let s = shown(request, tool, arguments, &self.rules, task, call);
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

/// The main session's answering state, kept across the relays built for it: the switch, the
/// breaker, the shapes granted, and the rows owed to the main session.
pub struct Answering {
    on: AtomicBool,
    disagreements: AtomicU32,
    granted: Mutex<BTreeSet<String>>,
    /// Cards that went to the operator, by request, with their shape.
    asked: Mutex<BTreeMap<String, String>>,
    rows: Mutex<Vec<(String, String)>>,
    wake: Mutex<Option<super::Wake>>,
}

impl Answering {
    pub fn new(on: bool) -> Arc<Answering> {
        Arc::new(Answering {
            on: AtomicBool::new(on),
            disagreements: AtomicU32::new(0),
            granted: Mutex::default(),
            asked: Mutex::default(),
            rows: Mutex::default(),
            wake: Mutex::default(),
        })
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

    /// The main session's rows since the last take, as `(title, body)`.
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

    /// One disagreement, and the breaker at [`BREAKER_AT`]: answering stops, observe-only.
    fn disagree(&self, why: &str, rec: Option<&Arc<dyn Recorder>>) {
        let n = self.disagreements.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(r) = rec {
            r.row(&format!("permission · disagreement {n}: {why}"), "");
        }
        if n >= BREAKER_AT && self.on.swap(false, Ordering::SeqCst) {
            let title = format!(
                "shadow stopped answering reads after {n} disagreements · observe-only until restart"
            );
            if let Some(r) = rec {
                r.row(&format!("permission · {title}"), why);
            }
            self.push_row(title, format!("the last disagreement: {why}"));
        }
    }
}

/// The ask's shape for the breaker: the seat's kind and the path's first two components.
fn shape(kind: &str, path: &str) -> String {
    let prefix: PathBuf = std::path::Path::new(path).components().take(3).collect();
    format!("{kind} read {}", prefix.display())
}

/// The seat kind of a review-queue asker, or `None` for any other session.
fn queue_kind(role: Option<&str>) -> Option<&'static str> {
    let role = role?;
    if role.starts_with("reviewer of ") {
        Some("reviewer")
    } else if role.ends_with("'s gatekeeper") {
        Some("gatekeeper")
    } else {
        None
    }
}

/// Whether `c` is a call the shadow may answer: a review-queue seat's read of one path outside
/// its view, past the read denylist and inside the asker's repository (C8). `Ok` is the path and
/// the seat kind.
pub fn scope(c: &RelayedCall, home: &std::path::Path) -> Result<(String, &'static str), String> {
    let path = c.view_read.clone().ok_or("not a read-only view widening")?;
    let kind = queue_kind(c.role.as_deref()).ok_or("not a review-queue seat")?;
    let gate = c.gate.as_ref().ok_or("the asker's gate is not known")?;
    super::guard::read_passes(&path, home)?;
    super::guard::within_roots(&path, &super::guard::read_roots(&gate.workspace))?;
    Ok((path, kind))
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

/// The main session's shadow on a relayed card: asked under the permission job's deadline, its
/// verdict one line on the card and the operator's answer beside it in the record. It answers only
/// a review-queue seat's read of a path outside its view, while `answering` is on, and judges it
/// in the asker's own rules and workspace, never the main session's.
pub struct Relay {
    pub cell: Cell,
    pub home: PathBuf,
    pub answering: Arc<Answering>,
    /// Whether a grant answered here is kept where `/shadow revoke` finds it: a daemon with a
    /// store. Without one the shadow only observes.
    pub revocable: bool,
}

impl Relay {
    fn parts(&self) -> Option<(Arc<Shadow>, Arc<dyn Recorder>)> {
        self.cell.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn shown_for(&self, c: &RelayedCall) -> Shown {
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
        };
        let mut s = shown(&c.req_id, &c.tool, &c.arguments, &rules, None, call);
        s.data.push_str(&format!(
            "\nasked by: {} (relayed)\nlayer A: {}\n",
            c.role.as_deref().unwrap_or(&c.asker),
            c.reading
        ));
        if let Some(p) = &c.view_read {
            s.data.push_str(&format!(
                "asking: read access to {p}, outside the asker's view; no write, exec or network\n"
            ));
        }
        s
    }

    /// The verdict, and the answer when `may_act` and it may act.
    fn judge(&self, c: &RelayedCall, may_act: bool) -> Option<Note> {
        let (shadow, rec) = self.parts()?;
        let scoped = scope(c, &self.home);
        rec.row(
            &format!("permission · relayed {} from {}", c.req_id, c.asker),
            match &scoped {
                Ok(_) => "a read-only view widening the shadow may answer",
                Err(_) => "",
            },
        );
        let outcome = shadow.ask(Job::Permission, self.shown_for(c))?;
        let judge = shadow.label(Job::Permission);
        let line = line_of(judge.as_deref(), &outcome);
        let acting = match (&scoped, &c.gate, may_act && self.answering.is_on()) {
            (Ok((path, kind)), Some(gate), true) => {
                acting_rule(&outcome, path, &ruleset_of(gate)).map(|r| (path.clone(), *kind, r))
            }
            _ => None,
        };
        let Some((path, kind, rule)) = acting else {
            if let Ok((path, kind)) = &scoped {
                self.answering
                    .asked
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(c.req_id.clone(), shape(kind, path));
            }
            return line.map(Note::Line);
        };
        let role = c.role.as_deref().unwrap_or(&c.asker);
        let who = role.split(" (").next().unwrap_or(role);
        let row = format!(
            "shadow allowed {who} to read {path} (rule: {rule}) · alt-n to see · /shadow revoke"
        );
        let evidence = match &outcome {
            Outcome::Verdict(Verdict::Permission { evidence, .. }) => evidence.clone(),
            _ => String::new(),
        };
        let body = format!(
            "verdict: allow\njudge: {}\nrule relied on: \"{rule}\"\nevidence: \"{}\"\n\
             grant: read-only `{path}` into the view of {role} ({}), for that session\n\
             request: {}\nrevoke: /shadow revoke",
            judge.as_deref().unwrap_or("shadow"),
            evidence.trim(),
            c.asker,
            c.req_id
        );
        rec.row(
            &format!(
                "permission · answered {}: read {path} for {}",
                c.req_id, c.asker
            ),
            &body,
        );
        self.answering
            .granted
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(shape(kind, &path));
        self.answering.push_row(row.clone(), body);
        Some(Note::Answer {
            option_id: "grant_ro".into(),
            by: format!("shadow:{}", judge.as_deref().unwrap_or("permission")),
            row,
        })
    }
}

impl Annotator for Relay {
    fn line(&self, c: &RelayedCall) -> Option<String> {
        match self.judge(c, false)? {
            Note::Line(l) => Some(l),
            Note::Answer { .. } => None,
        }
    }

    fn note(&self, c: &RelayedCall) -> Option<Note> {
        self.judge(c, true)
    }

    fn may_answer(&self, c: &RelayedCall) -> bool {
        self.revocable
            && self.answering.is_on()
            && c.gate.is_some()
            && scope(c, &self.home).is_ok()
            && self.parts().is_some()
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
        Decision::Allow { rule } => format!("{who}: allow - matches rule \"{rule}\""),
        Decision::Ask => format!("{who}: ask{}", tail(evidence)),
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
