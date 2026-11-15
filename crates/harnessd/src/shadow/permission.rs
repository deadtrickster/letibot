//! The `permission` job, observe-only: every call the gate put in front of the operator is judged
//! by the shadow once the card settles, without waiting, and the operator's answer is the label.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use letibot_sessionlog::pending_asks::{Annotator, RelayedCall};
use letibot_tools::authorise::{CorpusRow, CorpusSink, OperatorOverride};

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

/// The main session's shadow on a relayed card: asked under the permission job's deadline, its
/// verdict one line on the card and the operator's answer beside it in the record. It never acts.
pub struct Relay {
    pub cell: Cell,
    pub rules: Vec<String>,
    pub workspace: PathBuf,
    pub home: PathBuf,
}

impl Annotator for Relay {
    fn line(&self, c: &RelayedCall) -> Option<String> {
        let (shadow, rec) = self
            .cell
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()?;
        let call = Call {
            tool: c.tool.clone(),
            args: c.arguments.clone(),
            cwd: self.workspace.clone(),
            workspace: self.workspace.clone(),
            home: self.home.clone(),
        };
        let mut s = shown(&c.req_id, &c.tool, &c.arguments, &self.rules, None, call);
        s.data.push_str(&format!(
            "\nasked by: {} (relayed)\nlayer A: {}\n",
            c.asker, c.reading
        ));
        rec.row(
            &format!("permission · relayed {} from {}", c.req_id, c.asker),
            "",
        );
        let outcome = shadow.ask(Job::Permission, s)?;
        line_of(shadow.label(Job::Permission).as_deref(), &outcome)
    }

    fn answered(&self, req_id: &str, effect: &str) {
        if let Some((_, rec)) = self.cell.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            rec.row(
                &format!("permission · operator answered {effect} {req_id}"),
                "",
            );
        }
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
