//! **Subagents survive a daemon restart: recorded when started, brought back when the parent
//! is opened again.**
//!
//! The operator, 2026-10-12, verbatim: *"and here is one more thing about restart - we should
//! bring subagents back - i cant afford to wait till they finish. we should record them (session
//! persist anyway) and restart automatically"*.
//!
//! Before this, a child was a thread of the daemon that started it and a slot in that daemon's
//! memory. Its session was in the store, but nothing said it was mid-task: restarting the daemon
//! to pick up a new build lost every running child, so the operator either waited for them all to
//! finish or threw their work away.
//!
//! # What is recorded, and where
//!
//! One `subagent_run` row per child (`letibot_tokencore::store::subagents`, schema 26), written by
//! `HarnessTaskRunner::start` before the child's thread exists: the child's id (its handle and
//! session id), the parent's id, the subtask verbatim, the spawn's own description
//! ([`RecordedSpec`] — role, downgrade, placement, model, worktree path/branch/base, VM source and
//! check), and which daemon instance owns it. The row moves when the child's FIRST turn settles —
//! the turn `task_result` and the parent's completion notice are about — through
//! [`SubagentRecord::settled`], which is called from the one place a slot settles.
//!
//! # Which endings leave a child to be resumed
//!
//! * **A crash or a kill -9** writes nothing, so the row stays `running`: resumed.
//! * **An orderly shutdown** interrupts every turn in flight (`Registry::close`) and stops every
//!   child (`Sessions::drain_subagents`) — and that is NOT the child's ending, it is the daemon's.
//!   A settlement that arrives once the daemon has begun to stop leaves the row `running` and
//!   resets its crash count ([`SubagentRecord::settled`]): resumed.
//! * **An operator's or a parent's stop** (`job_kill`, an interrupt that walks the tree) writes
//!   `killed`: **never resumed**.
//! * **A real answer or a real failure** writes `done`/`failed` with the text. If the parent's model
//!   had not been handed it when the daemon went down (`delivered` is set by
//!   `JobWatchers::read_now`), it is handed over on the next open, and nothing is respawned.
//!
//! # How a child comes back
//!
//! When a parent session is opened (`Sessions::harness_as`, `Sessions::open_first_with_seat`, or —
//! for a child of a child — the resumed child's own open in `run_to_completion`),
//! `Harness::resume_subagents` asks its runner for the records ([`decide`] is the rule), and for
//! each child that was mid-task the runner spawns the same thread `task_start` would have, with
//! the same spec, under the SAME id: `Harness::open_with_registry` finds the session in the store
//! and resumes its transcript, in the same worktree, and its turn is continued with
//! [`child_notice`]. The slot, the parent's watcher, the journal and the parent's
//! `SessionEvent::Subagent` rows are the ones a fresh child gets, so `task_result`, the pane and
//! the completion delivery track it exactly as before.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use letibot_sessionlog::registry::Registry;
use letibot_tokencore::store::{Store, SubagentRun};
use letibot_tools::builtins::task::{
    Downgrade, Placement, RestoreMode, TaskSpec, TaskStatus, WorktreePlacement,
};
use serde::{Deserialize, Serialize};

/// **How many restarts in a row a child may be brought back from** without its daemon having
/// stopped in an orderly way in between. A child whose resume keeps taking the daemon down — a
/// tool call that OOMs the box, a crash in a path only it reaches — is resumed this many times and
/// then left, with its parent told, rather than resumed into the same crash for ever. Small on
/// purpose: three crashes in a row with the same child running is a pattern, not bad luck.
pub const RESUME_LIMIT: u32 = 3;

/// **The spawn's own description, as it is written down** — every field of [`TaskSpec`], so a
/// resumed child is spawned with exactly the configuration it was started with.
///
/// A struct of its own rather than a `Serialize` on `TaskSpec`: the record is a file format, and a
/// field added to the spec must be added here on purpose rather than appear in the store under
/// whatever name the struct happens to use.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedSpec {
    pub role: String,
    #[serde(default)]
    pub downgrade: Downgrade,
    /// `host` or `firecode`.
    pub placement: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub worktree: Option<RecordedWorktree>,
    #[serde(default)]
    pub vm_source: Option<PathBuf>,
    #[serde(default)]
    pub vm_verify: Option<String>,
}

/// [`WorktreePlacement`], as written down.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedWorktree {
    pub path: String,
    pub branch: String,
    pub base_sha: String,
    pub main_tree: bool,
}

impl RecordedSpec {
    pub fn of(spec: &TaskSpec) -> RecordedSpec {
        RecordedSpec {
            role: spec.role.clone(),
            downgrade: spec.downgrade.clone(),
            placement: spec.placement.as_str().to_string(),
            model: spec.model.clone(),
            worktree: spec.worktree.as_ref().map(|w| RecordedWorktree {
                path: w.path.clone(),
                branch: w.branch.clone(),
                base_sha: w.base_sha.clone(),
                main_tree: w.main_tree,
            }),
            vm_source: spec.vm_source.clone(),
            vm_verify: spec.vm_verify.clone(),
        }
    }

    pub fn spec(&self) -> Result<TaskSpec, String> {
        Ok(TaskSpec {
            role: self.role.clone(),
            downgrade: self.downgrade.clone(),
            placement: Placement::parse(&self.placement)?,
            model: self.model.clone(),
            worktree: self.worktree.as_ref().map(|w| WorktreePlacement {
                path: w.path.clone(),
                branch: w.branch.clone(),
                base_sha: w.base_sha.clone(),
                main_tree: w.main_tree,
            }),
            vm_source: self.vm_source.clone(),
            vm_verify: self.vm_verify.clone(),
        })
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }

    pub fn from_json(s: &str) -> Result<RecordedSpec, String> {
        serde_json::from_str(s).map_err(|e| format!("its recorded spawn does not parse: {e}"))
    }
}

/// **The store, opened on first use** — shared by every record a runner writes (it is the
/// runner's `merge_store` cell: one connection per runner, behind a mutex because a
/// `rusqlite::Connection` is `Send` and not `Sync`).
#[derive(Clone)]
pub struct RunStore {
    pub path: PathBuf,
    pub cell: Arc<Mutex<Option<Store>>>,
}

impl RunStore {
    pub fn with<T>(&self, f: impl FnOnce(&Store) -> Result<T, String>) -> Result<T, String> {
        let mut g = self.cell.lock().unwrap_or_else(|e| e.into_inner());
        if g.is_none() {
            *g = Some(
                Store::open(&self.path)
                    .map_err(|e| format!("the session store at {}: {e}", self.path.display()))?,
            );
        }
        f(g.as_ref().expect("just opened"))
    }
}

/// **One child's row, as the slot that settles it holds it.** See the module docs for which
/// endings write what.
pub struct SubagentRecord {
    pub store: RunStore,
    pub child: String,
    /// The daemon's registry, whose `is_stopping` is what tells an orderly shutdown's stop from a
    /// person's.
    pub registry: Arc<Registry>,
}

impl SubagentRecord {
    /// **The child's first turn settled** — or was stopped. `killed` is the slot's own flag.
    ///
    /// The one judgement here is the shutdown: once the daemon has begun to stop
    /// (`Registry::is_stopping`), whatever ends the child's turn is the daemon's ending and not
    /// the child's — see the arm below.
    pub fn settled(&self, status: &TaskStatus, killed: bool) {
        let wrote = if self.registry.is_stopping() {
            // **The daemon's ending, not the child's.** `Registry::close` interrupts every turn in
            // flight, and an interrupted turn comes back as an ANSWER — whatever the child had
            // written when it stopped — so no status here can be trusted to be the child's last
            // word. The row stays `running` (crash count reset: this is no crash) and the next
            // daemon resumes it. The cost, on the rare child that truly finished in the same
            // instant, is one extra turn in which it reads the restart notice beside its own
            // answer.
            self.store.with(|s| {
                s.interrupt_subagent_run(&self.child)
                    .map_err(|e| e.to_string())
            })
        } else {
            match status {
                TaskStatus::Done { .. } | TaskStatus::Failed { .. } if killed => {
                    return self.killed();
                }
                TaskStatus::Done { answer, .. } => self.store.with(|s| {
                    s.settle_subagent_run(&self.child, "done", Some(answer))
                        .map_err(|e| e.to_string())
                }),
                TaskStatus::Failed { why } => self.store.with(|s| {
                    s.settle_subagent_run(&self.child, "failed", Some(why))
                        .map_err(|e| e.to_string())
                }),
                TaskStatus::Running { .. } | TaskStatus::Unknown => return,
            }
        };
        if let Err(why) = wrote {
            eprintln!(
                "  {} · its settlement was not recorded ({why}); a restart may resume or \
                 re-deliver it",
                self.child
            );
        }
    }

    /// **A person or a parent stopped this child** — written at the moment the stop is
    /// accepted, so a daemon that dies before the child's turn winds down does not bring back a
    /// child somebody stopped. During an orderly shutdown the stop is the daemon's, not a
    /// person's, and nothing is written (see [`Self::settled`]).
    pub fn killed(&self) {
        if self.registry.is_stopping() {
            return;
        }
        if let Err(why) = self.store.with(|s| {
            s.settle_subagent_run(&self.child, "killed", Some("stopped"))
                .map_err(|e| e.to_string())
        }) {
            eprintln!("  {} · its stop was not recorded ({why})", self.child);
        }
    }
}

/// **What a parent's open does with one recorded child** — the rule, pure, so each edge case is
/// asserted without a daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Nothing to do: settled and delivered, killed, abandoned, or owned by this very daemon.
    Leave,
    /// Mid-task when its daemon went down: spawn it again under its own id.
    Resume { spec: TaskSpec },
    /// Settled, and its parent was never handed the settlement: hand it over.
    Deliver(TaskStatus),
    /// Mid-task, and it will NOT be brought back: mark it `abandoned`, tell the parent why.
    Abandon { why: String },
    /// Mid-task, and the daemon was down past the restore threshold: mark it `paused` and remind
    /// the parent, which brings it back with `task_resume` if it still matters.
    Remind { spec: TaskSpec },
}

/// **The rule.** `instance` is this daemon's; `held` is whether this daemon already has the child
/// (a slot or a live session under its id); `dir_exists` answers for a worktree path.
pub fn decide(
    run: &SubagentRun,
    instance: &str,
    held: bool,
    dir_exists: impl Fn(&str) -> bool,
    mode: RestoreMode,
) -> Decision {
    // **Written by this daemon, or already here**: live in this process, or settled here and
    // delivered through this process's own watchers. A parent reopened inside one daemon (a
    // parked session, a `/resume`) must not spawn a second copy of a child that is running.
    if run.instance == instance || held {
        return Decision::Leave;
    }
    match run.state.as_str() {
        "running" => {}
        "done" | "failed" if !run.delivered => {
            let text = run.outcome.clone().unwrap_or_default();
            return Decision::Deliver(if run.state == "done" {
                TaskStatus::Done {
                    answer: text,
                    child: None,
                }
            } else {
                TaskStatus::Failed { why: text }
            });
        }
        // Delivered, stopped by somebody, or already given up on.
        _ => return Decision::Leave,
    }
    let spec = match RecordedSpec::from_json(&run.spec_json).and_then(|r| r.spec()) {
        Ok(spec) => spec,
        Err(why) => return Decision::Abandon { why },
    };
    if mode == RestoreMode::Remind {
        // **Past the threshold nothing comes back on its own**, and the checks below are the
        // resume's — the reminder says where the child was, and `task_resume` makes them when the
        // model asks for it.
        return Decision::Remind { spec };
    }
    if run.resumes >= RESUME_LIMIT {
        return Decision::Abandon {
            why: format!(
                "it has been resumed {} times in a row and each time the daemon went down again \
                 without an orderly stop, so it is not resumed a {} time — it may be what is \
                 taking the daemon down",
                run.resumes,
                ordinal(run.resumes + 1)
            ),
        };
    }
    if let Some(wt) = &spec.worktree
        && !dir_exists(&wt.path)
    {
        return Decision::Abandon {
            why: format!(
                "its worktree `{}` (branch `{}`) is gone, so there is nothing for it to carry on \
                 in",
                wt.path, wt.branch
            ),
        };
    }
    if spec.placement == Placement::Firecode {
        return Decision::Abandon {
            why: "it ran in a firecode VM, and the VM's copy of the tree did not survive the \
                  restart — a resumed child would read a conversation about edits its new copy \
                  does not have. Its work, if firecode delivered it at shutdown, is in a \
                  directory beside the copy"
                .into(),
        };
    }
    Decision::Resume { spec }
}

/// **Whether `task_resume` may bring this child back** — the explicit, one-call door (the
/// operator: *"this is what you wre running and how to restart that"*). `Ok` is its spec.
///
/// Refused by name: a child that finished (read it, or message it), one a person or a parent
/// stopped, one that is live in this daemon, one whose worktree is gone, and a VM child.
pub fn resumable(
    run: &SubagentRun,
    instance: &str,
    held: bool,
    dir_exists: impl Fn(&str) -> bool,
) -> Result<TaskSpec, String> {
    let h = &run.child_id;
    match run.state.as_str() {
        "done" | "failed" => {
            return Err(format!(
                "`{h}` finished its task ({}); read its answer with `task_result`, or give it \
                 more with `task_message`. Nothing was spawned.",
                run.state
            ));
        }
        "killed" => {
            return Err(format!(
                "`{h}` was stopped by a person or by its parent, so it is not brought back; \
                 start the work again with `task_start` if it is still wanted. Nothing was \
                 spawned."
            ));
        }
        _ => {}
    }
    if held || (run.state == "running" && run.instance == instance) {
        return Err(format!(
            "`{h}` is running in this daemon now; `task_result` shows where it has got to. \
             Nothing was spawned."
        ));
    }
    let spec = RecordedSpec::from_json(&run.spec_json).and_then(|r| r.spec())?;
    if let Some(wt) = &spec.worktree
        && !dir_exists(&wt.path)
    {
        return Err(format!(
            "`{h}`'s worktree `{}` (branch `{}`) is gone, so there is nothing for it to carry on \
             in. Nothing was spawned; start the work again with `task_start`.",
            wt.path, wt.branch
        ));
    }
    if spec.placement == Placement::Firecode {
        return Err(format!(
            "`{h}` ran in a firecode VM whose copy of the tree did not survive the restart. \
             Nothing was spawned; start the work again with `task_start`."
        ));
    }
    Ok(spec)
}

fn ordinal(n: u32) -> String {
    match n {
        1 => "first".into(),
        2 => "second".into(),
        3 => "third".into(),
        4 => "fourth".into(),
        n => format!("{n}th"),
    }
}

/// **What the resumed child reads** — the turn its restart continues with, in the harness's own
/// voice (`Speaker::Agent`, never the operator's words).
pub fn child_notice(at_ms: u64) -> String {
    format!(
        "[restart] the daemon restarted while you were working (at {}, build {}); your \
         conversation, worktree and plan are as you left them — carry on from where you were.",
        utc(at_ms),
        build()
    )
}

/// **The one line the parent's model gets for a child that came back.**
pub fn parent_resumed_line(name: &str) -> String {
    format!("[restart] subagent {name} was running when the daemon restarted and has been resumed.")
}

/// **The line for a child that did not**, with the reason.
pub fn parent_not_resumed_line(name: &str, why: &str) -> String {
    format!(
        "[restart] subagent {name} was running when the daemon restarted and could NOT be \
         resumed: {why}. Its conversation is still in the store under that handle; start the work \
         again with `task_start` if it is still wanted."
    )
}

/// **The line for a child left for the model to bring back** (past the restore threshold), with
/// the one call that does it.
pub fn parent_reminded_line(
    name: &str,
    handle: &str,
    worktree: Option<&str>,
    branch: Option<&str>,
) -> String {
    let place = match (worktree, branch) {
        (Some(w), Some(b)) => format!(" in worktree `{w}` on branch `{b}`"),
        (Some(w), None) => format!(" in `{w}`"),
        _ => String::new(),
    };
    format!(
        "- subagent {name} was working{place} and was NOT resumed. To bring it back — same \
         conversation, worktree, role and model — call `task_resume {{\"task\": \"{handle}\"}}`."
    )
}

/// The build a notice names: the commit when the build recorded one, else the crate version.
pub fn build() -> String {
    match option_env!("LETIBOT_BUILD_SHA") {
        Some(sha) if !sha.is_empty() => sha.to_string(),
        _ => format!("v{}", env!("CARGO_PKG_VERSION")),
    }
}

/// Milliseconds since the epoch as `YYYY-MM-DD HH:MM:SS UTC` — civil-from-days, so no clock
/// crate for one sentence.
pub fn utc(ms: u64) -> String {
    let secs = ms / 1000;
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(state: &str) -> SubagentRun {
        SubagentRun {
            child_id: "s-1-sub-1".into(),
            parent_id: "s-1".into(),
            state: state.into(),
            prompt: "fix the thing".into(),
            spec_json: RecordedSpec::of(&TaskSpec {
                role: "coder".into(),
                worktree: Some(WorktreePlacement {
                    path: "/wt/agent-fix".into(),
                    branch: "agent/fix".into(),
                    base_sha: "abc".into(),
                    main_tree: false,
                }),
                model: Some("local".into()),
                ..TaskSpec::default()
            })
            .to_json(),
            instance: "old".into(),
            resumes: 0,
            outcome: None,
            delivered: false,
            started_ms: 0,
            updated_ms: 0,
        }
    }

    const NEW: &str = "new";

    #[test]
    fn a_running_child_of_a_gone_daemon_is_resumed_with_its_own_spec() {
        match decide(&run("running"), NEW, false, |_| true, RestoreMode::Resume) {
            Decision::Resume { spec } => {
                assert_eq!(spec.role, "coder");
                assert_eq!(spec.model.as_deref(), Some("local"));
                assert_eq!(spec.worktree.unwrap().path, "/wt/agent-fix");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn past_the_threshold_a_running_child_is_only_reminded() {
        assert!(matches!(
            decide(&run("running"), NEW, false, |_| true, RestoreMode::Remind),
            Decision::Remind { .. }
        ));
        // A settled result is news either way, and is still delivered.
        let mut r = run("done");
        r.outcome = Some("x".into());
        assert!(matches!(
            decide(&r, NEW, false, |_| true, RestoreMode::Remind),
            Decision::Deliver(_)
        ));
    }

    #[test]
    fn task_resume_refuses_what_it_must_not_bring_back() {
        assert!(resumable(&run("paused"), NEW, false, |_| true).is_ok());
        assert!(resumable(&run("abandoned"), NEW, false, |_| true).is_ok());
        assert!(resumable(&run("running"), NEW, false, |_| true).is_ok());
        for (state, says) in [("done", "finished"), ("killed", "stopped by a person")] {
            let why = resumable(&run(state), NEW, false, |_| true).unwrap_err();
            assert!(why.contains(says), "{why}");
        }
        assert!(
            resumable(&run("running"), "old", false, |_| true)
                .unwrap_err()
                .contains("running in this daemon")
        );
        assert!(
            resumable(&run("paused"), NEW, false, |_| false)
                .unwrap_err()
                .contains("is gone")
        );
    }

    #[test]
    fn a_child_this_daemon_owns_is_left_alone() {
        assert_eq!(
            decide(&run("running"), "old", false, |_| true, RestoreMode::Resume),
            Decision::Leave
        );
        assert_eq!(
            decide(&run("running"), NEW, true, |_| true, RestoreMode::Resume),
            Decision::Leave
        );
    }

    #[test]
    fn a_child_whose_worktree_is_gone_is_not_respawned() {
        match decide(&run("running"), NEW, false, |_| false, RestoreMode::Resume) {
            Decision::Abandon { why } => assert!(why.contains("/wt/agent-fix"), "{why}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_settled_undelivered_child_is_delivered_not_respawned() {
        let mut r = run("done");
        r.outcome = Some("all fixed".into());
        assert_eq!(
            decide(&r, NEW, false, |_| true, RestoreMode::Resume),
            Decision::Deliver(TaskStatus::Done {
                answer: "all fixed".into(),
                child: None
            })
        );
        r.delivered = true;
        assert_eq!(
            decide(&r, NEW, false, |_| true, RestoreMode::Resume),
            Decision::Leave
        );
        let mut f = run("failed");
        f.outcome = Some("it broke".into());
        assert_eq!(
            decide(&f, NEW, false, |_| true, RestoreMode::Resume),
            Decision::Deliver(TaskStatus::Failed {
                why: "it broke".into()
            })
        );
    }

    #[test]
    fn a_killed_child_is_never_resumed() {
        assert_eq!(
            decide(&run("killed"), NEW, false, |_| true, RestoreMode::Resume),
            Decision::Leave
        );
        assert_eq!(
            decide(&run("abandoned"), NEW, false, |_| true, RestoreMode::Resume),
            Decision::Leave
        );
    }

    #[test]
    fn a_child_that_keeps_crashing_the_daemon_is_given_up_on() {
        let mut r = run("running");
        r.resumes = RESUME_LIMIT - 1;
        assert!(matches!(
            decide(&r, NEW, false, |_| true, RestoreMode::Resume),
            Decision::Resume { .. }
        ));
        r.resumes = RESUME_LIMIT;
        match decide(&r, NEW, false, |_| true, RestoreMode::Resume) {
            Decision::Abandon { why } => assert!(why.contains("3 times in a row"), "{why}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_vm_child_is_not_resumed_on_a_fresh_copy() {
        let mut r = run("running");
        r.spec_json = RecordedSpec::of(&TaskSpec {
            placement: Placement::Firecode,
            ..TaskSpec::default()
        })
        .to_json();
        assert!(matches!(
            decide(&r, NEW, false, |_| true, RestoreMode::Resume),
            Decision::Abandon { .. }
        ));
    }

    #[test]
    fn the_spec_round_trips_through_its_record() {
        let spec = TaskSpec {
            role: "researcher".into(),
            downgrade: Downgrade::parse("read-only").unwrap(),
            placement: Placement::Host,
            model: Some("zai/glm".into()),
            worktree: None,
            vm_source: Some("/src".into()),
            vm_verify: None,
        };
        let back = RecordedSpec::from_json(&RecordedSpec::of(&spec).to_json())
            .unwrap()
            .spec()
            .unwrap();
        assert_eq!(back, spec);
    }

    #[test]
    fn the_notices_say_what_happened() {
        assert_eq!(utc(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(utc(1_791_806_400_000), "2026-10-12 12:00:00 UTC");
        let n = child_notice(1_791_806_400_000);
        assert!(n.starts_with("[restart] the daemon restarted while you were working (at 2026-10-12 12:00:00 UTC, build "));
        assert!(n.ends_with("carry on from where you were."));
        assert_eq!(
            parent_resumed_line("s-1-sub-1"),
            "[restart] subagent s-1-sub-1 was running when the daemon restarted and has been \
             resumed."
        );
    }
}
