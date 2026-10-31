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
    /// **The child's name** ([`TaskSpec::slug`]). Absent in a record written before children had
    /// names; [`recorded_slug`] then answers from the worktree branch or the prompt, so a child
    /// brought back is still named the way a fresh one would have been.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    /// **A tool's helper** ([`TaskSpec::helper`]): never resumed, never re-delivered, never listed
    /// after a restart. Absent (false) in every record written before helpers were marked;
    /// [`is_helper`] recognises those by their brief.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub helper: bool,
}

/// **Whether a recorded child is a tool's helper rather than the model's child** — its record
/// says so, or (for a record from before helpers were marked) it was briefed as `digest`'s part
/// reader is: *"You are reading part N of M of a longer body of text on somebody else's
/// behalf"*. pg-noop's twelve readers of 2026-10-13 are such records.
pub fn is_helper(run: &SubagentRun) -> bool {
    RecordedSpec::from_json(&run.spec_json).is_ok_and(|r| r.helper)
        || letibot_tools::builtins::digest::is_part_reader_brief(&run.prompt)
}

/// **Every recorded child's name, unique among its parent's children** — in the order given
/// (the store's: oldest first). A name the record CARRIES is kept as it is: it was made unique
/// when the child was spawned. A name DERIVED for a record from before names
/// ([`recorded_slug`]'s fallback) is made unique the way a fresh spawn's is (`-2`, `-3`, …,
/// [`letibot_tools::builtins::task::unique_slug`]): pg-noop's twelve part readers all derived
/// `you-are-reading-part` from their identical opening, and a name twelve children answer to
/// names none of them.
pub fn recorded_slugs(runs: &[SubagentRun]) -> Vec<String> {
    let carried: Vec<Option<String>> = runs
        .iter()
        .map(|r| {
            RecordedSpec::from_json(&r.spec_json)
                .ok()
                .and_then(|s| s.slug)
                .filter(|s| !s.trim().is_empty())
        })
        .collect();
    let mut taken: Vec<String> = carried.iter().flatten().cloned().collect();
    let mut out = Vec::with_capacity(runs.len());
    for (run, carried) in runs.iter().zip(carried) {
        let slug = match carried {
            Some(s) => s,
            None => {
                let s = letibot_tools::builtins::task::unique_slug(&recorded_slug(run), |s| {
                    taken.iter().any(|t| t == s)
                });
                taken.push(s.clone());
                s
            }
        };
        out.push(slug);
    }
    out
}

/// **The name a recorded child answers to**: the slug its record carries, or — for a record from
/// before children had names — the one its spawn would have been given: the slug its worktree's
/// branch `agent/<slug>` was cut from, else [`slug_from_prompt`] over what it was asked. A
/// record that does not parse is named from its prompt alone.
pub fn recorded_slug(run: &SubagentRun) -> String {
    use letibot_tools::builtins::task::slug_from_prompt;
    let spec = RecordedSpec::from_json(&run.spec_json).unwrap_or_default();
    if let Some(s) = spec.slug.filter(|s| !s.trim().is_empty()) {
        return s;
    }
    if let Some(w) = &spec.worktree
        && !w.main_tree
        && let Some(s) = w.branch.strip_prefix("agent/")
        && !s.is_empty()
    {
        return s.to_string();
    }
    slug_from_prompt(&run.prompt)
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
            slug: spec.slug.clone(),
            helper: spec.helper,
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
            slug: self.slug.clone(),
            helper: self.helper,
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
/// runner's `merge_store` cell: one handle per runner onto the process's one connection, filled
/// behind a mutex on the first write).
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
#[derive(Clone)]
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

    /// **A later turn of this child begins** — after its first answer, on its own thread: a
    /// parent's `task_message`, a later prompt, a wake, its plan-check. The row goes back to
    /// `running` for the length of the turn ([`letibot_tokencore::store::Store::reopen_subagent_run`]),
    /// so a daemon that stops mid-turn leaves a child the next one resumes. Answers the state to
    /// put back when the turn ends ([`Self::later_turn_ended`]); `None` when the row was not
    /// settled and nothing moved.
    ///
    /// pg-noop, 2026-10-14: `slt-current-number` answered at 01:57, was woken by its parent at
    /// 02:01:41 to collect its sweep, and was mid-turn when the daemon restarted at 02:06:05 —
    /// its row still said `done`, so the next daemon left it, every door then disagreed about it,
    /// and the sweep it was waiting on was never mentioned again.
    pub fn later_turn_began(&self) -> Option<String> {
        match self.store.with(|s| {
            s.reopen_subagent_run(&self.child)
                .map_err(|e| e.to_string())
        }) {
            Ok(prior) => prior,
            Err(why) => {
                eprintln!(
                    "  {} · its later turn was not recorded ({why}); a restart during it will \
                     not resume it",
                    self.child
                );
                None
            }
        }
    }

    /// **That later turn ended** — on its own, the row goes back to `prior`. Ended by the daemon
    /// stopping (`Registry::is_stopping`), it stays `running` with its crash count reset, exactly
    /// as a first turn does in [`Self::settled`], and the next daemon resumes it.
    pub fn later_turn_ended(&self, prior: &str) {
        let wrote = if self.registry.is_stopping() {
            self.store.with(|s| {
                s.interrupt_subagent_run(&self.child)
                    .map_err(|e| e.to_string())
            })
        } else {
            self.store.with(|s| {
                s.rest_subagent_run(&self.child, prior)
                    .map_err(|e| e.to_string())
            })
        };
        if let Err(why) = wrote {
            eprintln!(
                "  {} · the end of its later turn was not recorded ({why}); a restart may resume \
                 it",
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
    /// **A tool's helper caught mid-task** ([`is_helper`]): the tool call that was waiting for its
    /// answer ended with the old daemon, so there is nobody to resume it for and nobody to tell.
    /// Marked `abandoned` with `why`, and said to no one.
    Retire { why: String },
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
    // **A tool's helper is never the model's news.** Settled, its answer was read by the tool
    // that started it (or was never going to be — that call is gone); mid-task, nothing waits for
    // it any more. pg-noop, 2026-10-14: twelve `digest` readers finished the morning before were
    // handed to the model by a restart as *"12 subagents you started have finished"*.
    if is_helper(run) {
        return match run.state.as_str() {
            "running" => Decision::Retire {
                why: "a tool's helper whose call ended with the daemon that ran it — nothing \
                      waits for its answer"
                    .into(),
            },
            _ => Decision::Leave,
        };
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
    if spec.placement == Placement::Firecode && !vm_work_survives(&spec) {
        return Decision::Abandon {
            why: "it ran in a firecode VM on a copy of the tree, and the copy did not survive the \
                  restart — a resumed child would read a conversation about edits its new copy \
                  does not have. Its work, if firecode delivered it at shutdown, is in a \
                  directory beside the copy"
                .into(),
        };
    }
    Decision::Resume { spec }
}

/// **Whether a VM child's work outlives its VM** — it worked in its worktree MOUNTED, not on a
/// copy (`letibot_tools::firecode::mounts`: a git worktree, on a Mac), so every edit and commit it
/// made is in that worktree on the host, and a fresh VM booted on the same worktree is the same
/// tree its conversation is about. A child whose caller named a tree to copy (`vm_source`, a
/// reviewer's repository or `task`'s `repo`) worked on a copy, as does every VM child off a Mac.
/// The worktree's existence is checked before this, by the caller's own rule.
fn vm_work_survives(spec: &TaskSpec) -> bool {
    spec.vm_source.is_none()
        && spec
            .worktree
            .as_ref()
            .is_some_and(|wt| letibot_tools::firecode::mounts(std::path::Path::new(&wt.path)))
}

/// **A recorded child's state, as `task_result` reads it** — for a child of this session that has
/// no slot in this daemon (a restart left it without one). The one source is the record, and the
/// sentences agree with [`resumable`]'s: a finished child's answer IS readable, and a child that
/// is not running says why and names the door that brings it back.
pub fn recorded_status(run: &SubagentRun, instance: &str) -> TaskStatus {
    let outcome = run.outcome.clone().unwrap_or_default();
    let resume = format!(
        "`task_resume` with task=\"{}\" brings it back in its own session{}",
        run.child_id,
        RecordedSpec::from_json(&run.spec_json)
            .ok()
            .and_then(|r| r.worktree)
            .filter(|w| !w.main_tree)
            .map(|w| format!(" and worktree `{}`", w.path))
            .unwrap_or_default()
    );
    match run.state.as_str() {
        "done" => TaskStatus::Done {
            answer: outcome,
            child: None,
        },
        "failed" => TaskStatus::Failed { why: outcome },
        "killed" => TaskStatus::Failed {
            why: "it was stopped by a person or by its parent, and is not brought back".into(),
        },
        "running" if run.instance != instance => TaskStatus::Failed {
            why: format!(
                "it was running when the daemon that ran it stopped, and nothing is running it \
                 now; {resume}"
            ),
        },
        "running" => TaskStatus::Failed {
            why: format!(
                "its record says it is running in this daemon, but nothing here holds it; \
                 {resume}"
            ),
        },
        "paused" => TaskStatus::Failed {
            why: format!(
                "it was running when the daemon stopped and was down past the restore \
                 threshold, so it was left paused; {resume}"
            ),
        },
        "abandoned" => TaskStatus::Failed {
            why: format!("it was not brought back after a daemon restart: {outcome}"),
        },
        other => TaskStatus::Failed {
            why: format!("its record says `{other}`: {outcome}"),
        },
    }
}

/// **Whether `task_resume` may bring this child back** — the explicit, one-call door (the
/// operator: *"this is what you wre running and how to restart that"*). `Ok` is its spec.
///
/// Refused by name: a child that finished (read it, or message it), one a person or a parent
/// stopped, one that is live in this daemon, one whose worktree is gone, and a VM child that
/// worked on a copy (one that worked in its mounted worktree comes back: [`vm_work_survives`]).
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
    if spec.placement == Placement::Firecode && !vm_work_survives(&spec) {
        return Err(format!(
            "`{h}` ran in a firecode VM on a copy of the tree, and the copy did not survive the \
             restart. Nothing was spawned; start the work again with `task_start`."
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

    /// **pg-noop's twelve part readers** (2026-10-14): settled and never marked delivered,
    /// because `digest` read their answers itself — a restart handed all twelve to the model as
    /// just finished. A helper, old record or new, is left when settled and retired, silently,
    /// when caught mid-task.
    #[test]
    fn a_tools_helper_is_never_delivered_or_resumed() {
        let mut old = run("done");
        old.prompt = "You are reading part 3 of 12 of a longer body of text on somebody else's \
                      behalf. They asked:\n\n  why"
            .into();
        old.spec_json = r#"{"role":"researcher","placement":"host"}"#.into();
        old.outcome = Some("part 3 says".into());
        assert!(is_helper(&old));
        for mode in [RestoreMode::Resume, RestoreMode::Remind] {
            assert_eq!(decide(&old, NEW, false, |_| true, mode), Decision::Leave);
        }
        let mut marked = run("running");
        marked.spec_json = RecordedSpec::of(&TaskSpec {
            role: "researcher".into(),
            helper: true,
            ..TaskSpec::default()
        })
        .to_json();
        assert!(is_helper(&marked));
        assert!(matches!(
            decide(&marked, NEW, false, |_| true, RestoreMode::Resume),
            Decision::Retire { .. }
        ));
        // The model's own child with the same words in its brief, further in, is not one.
        let mut mine = run("done");
        mine.prompt = "Summarise it. You are reading part 1 of 2 of a longer body of text on \
                       somebody else's behalf."
            .into();
        mine.outcome = Some("x".into());
        assert!(!is_helper(&mine));
        assert!(matches!(
            decide(&mine, NEW, false, |_| true, RestoreMode::Resume),
            Decision::Deliver(_)
        ));
    }

    /// **Derived names are unique among a parent's children; carried names are kept** — the
    /// twelve readers that all derived `you-are-reading-part`.
    #[test]
    fn derived_names_are_made_unique_and_carried_ones_kept() {
        let reader = |n: u32| {
            let mut r = run("done");
            r.child_id = format!("s-1-sub-{n}");
            r.prompt = format!(
                "You are reading part {n} of 3 of a longer body of text on somebody else's \
                 behalf."
            );
            r.spec_json = r#"{"role":"researcher","placement":"host"}"#.into();
            r
        };
        let mut named = run("done");
        named.spec_json = RecordedSpec::of(&TaskSpec {
            slug: Some("you-are-reading-part-2".into()),
            ..TaskSpec::default()
        })
        .to_json();
        let runs = vec![reader(1), named, reader(2), reader(3)];
        assert_eq!(
            recorded_slugs(&runs),
            vec![
                "you-are-reading-part",
                "you-are-reading-part-2",
                "you-are-reading-part-3",
                "you-are-reading-part-4",
            ]
        );
    }

    /// **One state for one child, whichever door asks** — pg-noop, 2026-10-14: `task_result`
    /// listed `slt-current-number` as `unknown`, refused it as *"no subagent called … was started
    /// here"*, and `task_resume` refused it as *"finished its task (done); read its answer with
    /// `task_result`"*. The record answers `task_result` now, in words that agree with
    /// `resumable`'s.
    #[test]
    fn a_recorded_child_reads_the_same_to_task_result_and_task_resume() {
        let mut done = run("done");
        done.outcome = Some("Committed as `dce7cca`.".into());
        assert_eq!(
            recorded_status(&done, NEW),
            TaskStatus::Done {
                answer: "Committed as `dce7cca`.".into(),
                child: None
            }
        );
        assert!(
            resumable(&done, NEW, false, |_| true)
                .unwrap_err()
                .contains("read its answer with `task_result`")
        );
        // Not running anywhere: the record says why, and both doors name `task_resume`.
        for state in ["running", "paused"] {
            let r = run(state);
            match recorded_status(&r, NEW) {
                TaskStatus::Failed { why } => {
                    assert!(why.contains("task_resume"), "{state}: {why}");
                    assert!(why.contains("/wt/agent-fix"), "{state}: {why}");
                }
                other => panic!("{state}: {other:?}"),
            }
            assert!(resumable(&r, NEW, false, |_| true).is_ok(), "{state}");
        }
        match recorded_status(&run("killed"), NEW) {
            TaskStatus::Failed { why } => assert!(why.contains("stopped by a person"), "{why}"),
            other => panic!("{other:?}"),
        }
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

    /// **A VM child that worked in its worktree MOUNTED comes back; one that worked on a copy does
    /// not** — on a Mac a VM child given a worktree works in it directly (`firecode::mounts`), so its
    /// edits and commits are on the host and survive the restart, and a fresh VM on the same
    /// worktree is the tree its conversation is about. Through both doors: the restart's own
    /// decision and `task_resume`. A caller-named tree (`vm_source`) is a copy even with a
    /// worktree, and off a Mac nothing mounts.
    #[test]
    fn a_vm_child_in_its_mounted_worktree_is_resumed_and_one_on_a_copy_is_not() {
        let base = std::env::temp_dir().join(format!("letibot-vm-resume-{}", std::process::id()));
        let wt = base.join("agent-fix");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(
            wt.join(".git"),
            format!("gitdir: {}/repo/.git/worktrees/agent-fix\n", base.display()),
        )
        .unwrap();
        let record = |vm_source: Option<PathBuf>| {
            let mut r = run("running");
            r.spec_json = RecordedSpec::of(&TaskSpec {
                role: "coder".into(),
                placement: Placement::Firecode,
                worktree: Some(WorktreePlacement {
                    path: wt.display().to_string(),
                    branch: "agent/fix".into(),
                    base_sha: "abc".into(),
                    main_tree: false,
                }),
                vm_source,
                ..TaskSpec::default()
            })
            .to_json();
            r
        };
        let mounted = record(None);
        let resumed = matches!(
            decide(&mounted, NEW, false, |_| true, RestoreMode::Resume),
            Decision::Resume { .. }
        );
        assert_eq!(resumed, cfg!(target_os = "macos"), "the mounted worktree");
        assert_eq!(
            resumable(&mounted, NEW, false, |_| true).is_ok(),
            cfg!(target_os = "macos"),
            "task_resume on the mounted worktree"
        );
        let copied = record(Some(base.join("repo")));
        match decide(&copied, NEW, false, |_| true, RestoreMode::Resume) {
            Decision::Abandon { why } => assert!(why.contains("on a copy"), "{why}"),
            other => panic!("a caller-named tree is a copy: {other:?}"),
        }
        let why = resumable(&copied, NEW, false, |_| true).unwrap_err();
        assert!(why.contains("on a copy"), "{why}");
        let _ = std::fs::remove_dir_all(&base);
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
            slug: Some("survey-the-store".into()),
            helper: true,
        };
        let back = RecordedSpec::from_json(&RecordedSpec::of(&spec).to_json())
            .unwrap()
            .spec()
            .unwrap();
        assert_eq!(back, spec);
    }

    /// **A recorded child is named by its record, and an old record still gets a name** — the
    /// slug the record carries, else the branch its worktree was cut on, else its prompt's.
    #[test]
    fn a_recorded_child_is_named_by_its_record_or_as_its_spawn_would_have_been() {
        // A record from before names: the worktree branch `agent/fix` names it.
        assert_eq!(recorded_slug(&run("running")), "fix");
        // A record that carries its name: that name, whatever the branch says.
        let mut named = run("running");
        named.spec_json = RecordedSpec::of(&TaskSpec {
            slug: Some("release-notes".into()),
            ..TaskSpec::default()
        })
        .to_json();
        assert_eq!(recorded_slug(&named), "release-notes");
        // A plain `task` child from before names, with no worktree: its prompt's derivation.
        let mut plain = run("done");
        plain.spec_json = RecordedSpec::of(&TaskSpec::default()).to_json();
        assert_eq!(recorded_slug(&plain), "fix-the-thing");
        // And a record written before the field existed decodes with no name rather than failing.
        let old = r#"{"role":"coder","placement":"host"}"#;
        assert_eq!(RecordedSpec::from_json(old).unwrap().slug, None);
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
