//! Subagents survive a daemon restart: recorded when started, brought back when the parent
//! is opened again - the operator: *"we should record them (session persist anyway) and
//! restart automatically"*.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use letibot_sessionlog::registry::Registry;
use letibot_tokencore::store::{Store, SubagentRun};
use letibot_tools::builtins::task::{
    Downgrade, Placement, RestoreMode, TaskSpec, TaskStatus, WorktreePlacement,
};
use letibot_transcript::TranscriptItem;
use serde::{Deserialize, Serialize};

/// How many restarts in a row a child may be brought back from without an orderly stop in
/// between. A child whose resume keeps taking the daemon down is resumed this many times and then
/// left, with its parent told: three crashes in a row is a pattern, not bad luck.
pub const RESUME_LIMIT: u32 = 3;

/// The spawn's own description, as it is written down - every field of [`TaskSpec`], so a
/// resumed child is spawned with exactly the configuration it started with. A struct of its
/// own rather than a `Serialize` on `TaskSpec`
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
    /// The child's name ([`TaskSpec::slug`]). Absent in a record written before children had
    /// names; [`recorded_slug`] then answers from the worktree branch or the prompt, so a child
    /// brought back is still named the way a fresh one would have been.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    /// A tool's helper ([`TaskSpec::helper`]): never resumed, never re-delivered, never listed
    /// after a restart. Absent (false) in every record written before helpers were marked
    /// [`is_helper`] recognises those by their brief.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub helper: bool,
    /// The skills the child was started with ([`TaskSpec::skills`]), so a child brought back
    /// after a restart still boots with the instructions its parent named. Absent in older
    /// records, which means none - which is what those children had.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    /// The `[perimeter.<name>]` table the spawn named ([`TaskSpec::perimeter`]). Absent in older
    /// records and in the ordinary spawn, which is the `subagent` table.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub perimeter: Option<String>,
    /// What the operator granted for the spawn ([`TaskSpec::widen`]), in the arg spelling
    /// `ro:<path>` / `rw:<path>` / `secret:<name>`, and the reason. A child brought back keeps the
    /// widening it was answered for, and nothing more.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub widen: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub widen_because: Option<String>,
}

/// Whether a recorded child is a tool's helper rather than the model's child - its record says
/// so, or (from before helpers were marked) it was briefed as `digest`'s part reader is: *"You are
/// reading part N of M of a longer body of text on somebody else's behalf"*.
pub fn is_helper(run: &SubagentRun) -> bool {
    RecordedSpec::from_json(&run.spec_json).is_ok_and(|r| r.helper)
        || letibot_tools::builtins::digest::is_part_reader_brief(&run.prompt)
}

/// Every recorded child's name, unique among its parent's children, in the store's order. A
/// name the record CARRIES is kept - it was made unique when the child was spawned. A name
/// DERIVED for an older record is made unique the way a fresh spawn's is (`-2`, `-3`, …)
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

/// The name a recorded child answers to: the slug its record carries, or - for an older record - the one its spawn would have been given: the slug its worktree branch `agent/<slug>` was cut
/// from, else [`slug_from_prompt`]. A record that does not parse is named from its prompt alone.
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
            skills: spec.skills.clone(),
            perimeter: spec.perimeter.clone(),
            widen: spec.widen.iter().map(|i| i.arg()).collect(),
            widen_because: spec.widen_because.clone(),
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
            skills: self.skills.clone(),
            perimeter: self.perimeter.clone(),
            widen: self
                .widen
                .iter()
                .filter_map(|w| letibot_tools::exec::perimeter::WidenItem::parse(w).ok())
                .collect(),
            widen_because: self.widen_because.clone(),
        })
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }

    pub fn from_json(s: &str) -> Result<RecordedSpec, String> {
        serde_json::from_str(s).map_err(|e| format!("its recorded spawn does not parse: {e}"))
    }
}

/// The store, opened on first use - shared by every record a runner writes (it is the
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

/// One child's row, as the slot that settles it holds it. See the module docs for which
/// endings write what.
#[derive(Clone)]
pub struct SubagentRecord {
    pub store: RunStore,
    pub child: String,
    /// The daemon's registry, whose `is_stopping` is what tells an orderly shutdown's stop from a
    /// person's.
    pub registry: Arc<Registry>,
    /// The row its spawn could not write, kept so that the child's own acts can write it later.
    pub pending: Option<SubagentRun>,
}

impl SubagentRecord {
    /// The row the spawn could not write is written now - see [`Self::pending`]. A no-op for
    /// every record whose row landed at the spawn, and for one that lands now: the read is what
    /// keeps it from being written twice.
    fn ensure_written(&self) {
        let Some(row) = &self.pending else { return };
        let wrote = self.store.with(|s| {
            if s.subagent_run(&self.child)
                .map_err(|e| e.to_string())?
                .is_some()
            {
                return Ok(());
            }
            s.put_subagent_run(row).map_err(|e| e.to_string())
        });
        if let Err(why) = wrote {
            eprintln!(
                "  {} · the row it was started under is still not written ({why})",
                self.child
            );
        }
    }

    /// The child's first turn settled - or was stopped. `killed` is the slot's own flag. The
    /// one judgement here is the shutdown: once the daemon has begun to stop, whatever ends the
    /// child's turn is the daemon's ending and not the child's.
    pub fn settled(&self, status: &TaskStatus, killed: bool) {
        self.ensure_written();
        let wrote = if self.registry.is_stopping() {
            // The daemon's ending, not the child's. `Registry::close` interrupts every turn in
            // flight and an interrupted turn comes back as an ANSWER, so no status here is the
            // child's last word: the row stays `running` (no crash) and the next daemon resumes it.
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
                // A row of its own: a restart must not bring this back as a child that
                // answered, and the row keeps the answer and what died under it (R173).
                TaskStatus::Killed { answer, why } => self.store.with(|s| {
                    s.settle_subagent_run(
                        &self.child,
                        "killed",
                        Some(&format!("{why}\n\n{answer}")),
                    )
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

    /// A turn of the child after the one this record settled answered `answer`: kept beside the first answer, so a daemon not running when the child spoke again still hands its parent the last word.
    /// `false` when no row was written, meaning the slot's copy is the only one.
    pub fn answered_again(&self, answer: &str) -> bool {
        // The daemon's own interruption is not the child's last word: `run_turn` hands back the text
        // it had when the stop arrived, and recording that delivers a fragment after a restart.
        if self.registry.is_stopping() {
            return false;
        }
        self.ensure_written();
        if let Err(why) = self.store.with(|s| {
            s.settle_subagent_run_again(&self.child, answer)
                .map_err(|e| e.to_string())
        }) {
            eprintln!(
                "  {} · its later answer was not recorded ({why}); a restart reads the one before \
                 it",
                self.child
            );
            return false;
        }
        true
    }

    /// A later turn of this child begins - a parent's `task_message`, a later prompt, a wake.
    /// The row goes back to `running` for the length of the turn, so a daemon that stops
    /// mid-turn leaves a child the next one resumes
    pub fn later_turn_began(&self) -> Option<String> {
        self.ensure_written();
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

    /// That later turn ended - on its own, the row goes back to `prior`. Ended by the daemon
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

    /// A person or a parent stopped this child - written at the moment the stop is accepted
    /// so a daemon that dies before the child's turn winds down does not bring back a child
    /// somebody stopped. During an orderly shutdown the stop is the daemon's: nothing is written.
    pub fn killed(&self) {
        if self.registry.is_stopping() {
            return;
        }
        self.ensure_written();
        if let Err(why) = self.store.with(|s| {
            s.settle_subagent_run(&self.child, "killed", Some("stopped"))
                .map_err(|e| e.to_string())
        }) {
            eprintln!("  {} · its stop was not recorded ({why})", self.child);
        }
    }
}

/// What a parent's open does with one recorded child - the rule, pure, so each edge case is
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
    /// A tool's helper caught mid-task ([`is_helper`]): the tool call that was waiting for its
    /// answer ended with the old daemon, so there is nobody to resume it for and nobody to tell.
    /// Marked `abandoned` with `why`, and said to no one.
    Retire { why: String },
}

/// The rule. `instance` is this daemon's; `held` is whether this daemon already has the child
/// (a slot or a live session under its id); `writer_running` is whether the daemon that wrote the
/// record is still up; `dir_exists` answers for a worktree path.
pub fn decide(
    run: &SubagentRun,
    instance: &str,
    held: bool,
    writer_running: bool,
    dir_exists: impl Fn(&str) -> bool,
    mode: RestoreMode,
) -> Decision {
    // Written by this daemon, or already here: live in this process, or settled here and
    // delivered through this process's own watchers. A parent reopened inside one daemon (a
    // parked session, a `/resume`) must not spawn a second copy of a child that is running.
    if run.instance == instance || held {
        return Decision::Leave;
    }
    // And the same for a daemon that is still beating: the store is shared by every
    // daemon on the box, so another instance's running child is running there - not a stop's.
    if writer_running {
        return Decision::Leave;
    }
    // A tool's helper is never the model's news. Settled, its answer was read by the tool that
    // started it (or that call is gone); mid-task, nothing waits for it any more. pg-noop's twelve
    // `digest` readers were handed to the model by a restart as *"12 subagents have finished"*.
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
            let status = if run.state == "done" || run.later_answer.is_some() {
                TaskStatus::Done {
                    answer: answer_said(run),
                    child: None,
                }
            } else {
                TaskStatus::Failed {
                    why: run.outcome.clone().unwrap_or_default(),
                }
            };
            return Decision::Deliver(status);
        }
        // Delivered, stopped by somebody, or already given up on.
        _ => return Decision::Leave,
    }
    let spec = match RecordedSpec::from_json(&run.spec_json).and_then(|r| r.spec()) {
        Ok(spec) => spec,
        Err(why) => return Decision::Abandon { why },
    };
    if mode == RestoreMode::Remind {
        // Past the threshold nothing comes back on its own, and the checks below are the
        // resume's - the reminder says where the child was, and `task_resume` makes them when the
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
        // the row names the tree that was copied, so "beside the copy" has a path.
        let copied = match spec.vm_source.as_deref() {
            Some(p) => format!("the tree it copied, `{}`", p.display()),
            None => "its workspace".to_string(),
        };
        return Decision::Abandon {
            why: format!(
                "it ran in a firecode VM on a copy of {copied}, and the copy did not survive the \
                 restart — a resumed child would read a conversation about edits its new copy \
                 does not have. Its work, if firecode delivered it at shutdown, is in a \
                 directory beside that copy."
            ),
        };
    }
    Decision::Resume { spec }
}

/// Whether a VM child's work outlives its VM - it worked in its worktree MOUNTED, not on a
/// copy (`letibot_tools::firecode::mounts`: a git worktree, on a Mac), so its edits are on
/// the host and a fresh VM on the same worktree is the tree its conversation is about.
fn vm_work_survives(spec: &TaskSpec) -> bool {
    spec.vm_source.is_none()
        && spec
            .worktree
            .as_ref()
            .is_some_and(|wt| letibot_tools::firecode::mounts(std::path::Path::new(&wt.path)))
}

/// The child's latest word, and whether it is a later turn's: the answer of the last turn it ran
/// after settling, else the one its first turn gave. `outcome` keeps the first either way, so a reader
/// that wants it has it.
pub fn latest_answer(run: &SubagentRun) -> (String, bool) {
    match &run.later_answer {
        Some(again) => (again.clone(), true),
        None => (run.outcome.clone().unwrap_or_default(), false),
    }
}

/// What a parent is handed as the child's answer: a later turn says so, because the same child
/// speaking twice is not the news its first answer was.
pub fn answer_said(run: &SubagentRun) -> String {
    let (answer, later) = latest_answer(run);
    if later {
        format!("a later turn of this child, after the answer it gave first:\n\n{answer}")
    } else {
        answer
    }
}

/// A recorded child's state, as `task_result` reads it - for a child of this session that has
/// no slot in this daemon. The sentences agree with [`resumable`]'s: a finished child's answer IS
/// readable, and a child that is not running says why and names the door that brings it back.
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
        // A later turn is the child's last word, and it came after the failure: the row keeps the
        // failure in `outcome` and the parent is handed what the child said since.
        "done" => TaskStatus::Done {
            answer: answer_said(run),
            child: None,
        },
        "failed" if run.later_answer.is_some() => TaskStatus::Done {
            answer: answer_said(run),
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

/// Why the store holds no row for a child, in the words a model is given - and the two
/// classes are the point of the window: one is a write that went missing, the other a store
/// that was not writing yet, and only one of them went wrong.
pub fn no_record_cause(why: letibot_tokencore::store::NoRecord) -> &'static str {
    use letibot_tokencore::store::NoRecord;
    match why {
        NoRecord::Due => "the store refused the row its spawn writes",
        NoRecord::BeforeTheStore => "this child is from before this store recorded any child",
    }
}

/// What a child with NO record reads as - the second witness, and not *nothing at all*.
pub fn a_child_with_no_record(child: &letibot_tokencore::store::UnrecordedChild) -> TaskStatus {
    let id = &child.id;
    if let Some(text) = its_answer(child.last_item.as_ref()) {
        return TaskStatus::Done {
            answer: text.to_string(),
            child: None,
        };
    }
    TaskStatus::Failed {
        why: format!(
            "nothing is running it, and no record of it was ever written — {}. No settlement of \
             it was collected and `task_resume` cannot bring it back: the spawn's own description \
             is what it would need, and that is the row that is missing. Its last row is {}. Its \
             session `{id}` and its transcript hold everything it did — `transcript` with \
             session=\"{id}\" reads it — and it worked in `{}`.{}",
            // The one clause the store decides - a child older than the store's first record
            // is not told a write was refused, because none was due for it.
            no_record_cause(child.why),
            EndedIn::of(child.last_item.as_ref()).said(),
            child.workspace_root,
            // Its brief, so a parent can recognise which child this was - the one thing left
            // that says what it was doing, and the only place a child with no record can be
            // identified at all.
            match child.title.as_deref().map(crate::jobwatch::first_line) {
                Some(line) if !line.trim().is_empty() => format!("\n  it was asked: {line}"),
                _ => String::new(),
            }
        ),
    }
}

/// The one rule that makes a child's last row an answer - an `Assistant` with no call after it
/// and something to say. Shared by [`a_child_with_no_record`] and [`how_far`], so the two doors
/// cannot disagree about whether a child with no record answered.
fn its_answer(last: Option<&TranscriptItem>) -> Option<&str> {
    match last? {
        TranscriptItem::Assistant {
            text, tool_calls, ..
        } if tool_calls.is_empty() && !text.trim().is_empty() => Some(text),
        _ => None,
    }
}

/// How a child with no record got on, as a value - one judgement with two renderings: the
/// single-handle door writes a paragraph and can afford to be exact ([`Self::said`]), a
/// list row gets a few words ([`Self::brief`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndedIn {
    /// It wrote no row of its own at all.
    Nothing,
    /// Its last row asks for a call - named here - that nothing ever answered.
    AwaitedCall(String),
    /// Its last row is an `Assistant` with no text and no call.
    EmptyAnswer,
    /// Its last row is the result of the named call: the turn went on from there.
    AfterTool(String),
    /// Its last row is the prompt it was given: it never began.
    OnPrompt,
    /// Its last row is a thought it never finished.
    MidThought,
    /// Its last row is a notice it was given.
    OnNotice,
    /// Its last row is a mark in its conversation.
    OnMark,
}

impl EndedIn {
    pub fn of(last: Option<&TranscriptItem>) -> EndedIn {
        match last {
            None => EndedIn::Nothing,
            Some(TranscriptItem::Assistant { tool_calls, .. }) if !tool_calls.is_empty() => {
                EndedIn::AwaitedCall(tool_calls[0].name.clone())
            }
            Some(TranscriptItem::Assistant { .. }) => EndedIn::EmptyAnswer,
            Some(TranscriptItem::ToolResult { name, .. }) => EndedIn::AfterTool(name.clone()),
            Some(TranscriptItem::User { .. }) => EndedIn::OnPrompt,
            Some(TranscriptItem::Reasoning { .. }) => EndedIn::MidThought,
            Some(TranscriptItem::System { .. } | TranscriptItem::Notice { .. }) => {
                EndedIn::OnNotice
            }
            Some(TranscriptItem::SegmentMark { .. } | TranscriptItem::Offer { .. }) => {
                EndedIn::OnMark
            }
        }
    }

    /// Where the child was, in the words a model reads - the long form, and the one the
    /// single-handle answer has always carried.
    pub fn said(&self) -> String {
        match self {
            EndedIn::Nothing => "nothing: it wrote no row of its own at all".into(),
            EndedIn::AwaitedCall(name) => {
                format!("a call it had just asked for (`{name}`) and never got an answer to")
            }
            EndedIn::EmptyAnswer => "an empty answer".into(),
            EndedIn::AfterTool(name) => {
                format!("the result of `{name}`, so the turn went on from there")
            }
            EndedIn::OnPrompt => "the prompt it was given".into(),
            EndedIn::MidThought => "a thought it never finished".into(),
            EndedIn::OnNotice => "a notice it was given".into(),
            EndedIn::OnMark => "a mark in its conversation".into(),
        }
    }

    /// The same class in as few words as a list row can hold - a clause that reads on from
    /// *"it stopped "*, so the row says how far the child got without spending a sentence on it.
    pub fn brief(&self) -> String {
        match self {
            EndedIn::Nothing => "before it wrote a single row".into(),
            EndedIn::AwaitedCall(name) => {
                format!("mid-`{name}`, on a call it never got an answer to")
            }
            EndedIn::EmptyAnswer => "on an empty answer".into(),
            EndedIn::AfterTool(name) => format!("after `{name}`'s result"),
            EndedIn::OnPrompt => "on the prompt it was given, before it answered".into(),
            EndedIn::MidThought => "mid-thought".into(),
            EndedIn::OnNotice => "on a notice it was given".into(),
            EndedIn::OnMark => "on a mark in its conversation".into(),
        }
    }
}

/// How far a child with no record got, as the two doors carry it - one function rather than
/// two matches, because *did it answer?* is the whole difference between the two readings
/// and two copies of it would drift
pub fn how_far(
    child: &letibot_tokencore::store::UnrecordedChild,
) -> letibot_tools::builtins::task::LostChild {
    use letibot_tools::builtins::task::LostChild;
    if its_answer(child.last_item.as_ref()).is_some() {
        LostChild::Answered
    } else {
        LostChild::Stopped(EndedIn::of(child.last_item.as_ref()).brief())
    }
}

/// Whether `task_resume` may bring this child back - the explicit, one-call door (the
/// operator *"this is what you wre running and how to restart that"*). `Ok` is its spec.
/// Refused by name. `writer_running` is whether the daemon that wrote the record is still up.
pub fn resumable(
    run: &SubagentRun,
    instance: &str,
    held: bool,
    writer_running: bool,
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
    // Another daemon still beating is running it: a second copy in the same worktree
    // is the double resume this refusal exists to stop.
    if run.state == "running" && writer_running {
        return Err(format!(
            "`{h}` is running under a daemon that is still up on this box, so it is not brought \
             back; `task_result` shows where it has got to. Nothing was spawned."
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

/// What the resumed child reads - the turn its restart continues with, in the harness's own
/// voice (`Speaker::Agent`, never the operator's words). It says what survived and nothing about
/// when or which build: neither is something the child can act on.
pub fn child_notice(_at_ms: u64) -> String {
    "[restart] You were interrupted and resumed; your conversation, worktree and plan are as you \
     left them, and background jobs you started are gone. Carry on."
        .to_string()
}

/// The one bullet the parent's model gets for a child that came back: a bullet, because it sits
/// under the notice's own `[restart]` tag, and a second tag there reads as a second notice.
pub fn parent_resumed_line(name: &str) -> String {
    format!("- subagent {name}: resumed.")
}

/// The bullet for a child that did not, with the reason and the one call that tries again.
pub fn parent_not_resumed_line(name: &str, why: &str) -> String {
    format!(
        "- subagent {name}: not resumed ({why}); start the work again with `task_start` if it is \
         still needed."
    )
}

/// The line for a child left for the model to bring back (past the restore threshold), with
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

/// Milliseconds since the epoch as `YYYY-MM-DD HH:MM:SS UTC` - civil-from-days, so no clock
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
mod tests;
