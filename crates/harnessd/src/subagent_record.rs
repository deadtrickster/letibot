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

    /// **A turn of the child AFTER the one this record settled answered `answer`**: kept beside the
    /// first answer, which stays, so a daemon that was not running when the child spoke again still
    /// hands its parent the last word. `false` when no row was written, which is the slot's cue that
    /// its own copy is the only one there is.
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
    // **And the same for a daemon that is still beating** (R91.9): the store is shared by every
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

/// **The child's latest word**, and whether it is a later turn's: the answer of the last turn it ran
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
/// that was not writing yet, and only one of them went wrong. MEASURED 2026-11-01
pub fn no_record_cause(why: letibot_tokencore::store::NoRecord) -> &'static str {
    use letibot_tokencore::store::NoRecord;
    match why {
        NoRecord::Due => "the store refused the row its spawn writes",
        NoRecord::BeforeTheStore => "this child is from before this store recorded any child",
    }
}

/// What a child with NO record reads as - the second witness, and the answer that used to
/// be *nothing at all*.
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
/// list row gets a few words ([`Self::brief`]). MEASURED 2026-11-01
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
            Some(TranscriptItem::System { .. }) => EndedIn::OnNotice,
            Some(TranscriptItem::SegmentMark { .. }) => EndedIn::OnMark,
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
    // **Another daemon still beating is running it** (R91.9): a second copy in the same worktree
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
/// voice (`Speaker::Agent`, never the operator's words).
pub fn child_notice(at_ms: u64) -> String {
    format!(
        "[restart] the daemon restarted while you were working (at {}, build {}); your \
         conversation, worktree and plan are as you left them — carry on from where you were.",
        utc(at_ms),
        build()
    )
}

/// The one line the parent's model gets for a child that came back.
pub fn parent_resumed_line(name: &str) -> String {
    format!("[restart] subagent {name} was running when the daemon restarted and has been resumed.")
}

/// The line for a child that did not, with the reason.
pub fn parent_not_resumed_line(name: &str, why: &str) -> String {
    format!(
        "[restart] subagent {name} was running when the daemon restarted and could NOT be \
         resumed: {why}. Its conversation is still in the store under that handle; start the work \
         again with `task_start` if it is still wanted."
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

/// The build a notice names: the commit when the build recorded one, else the crate version.
pub fn build() -> String {
    match option_env!("LETIBOT_BUILD_SHA") {
        Some(sha) if !sha.is_empty() => sha.to_string(),
        _ => format!("v{}", env!("CARGO_PKG_VERSION")),
    }
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
mod tests {
    use super::*;
    use letibot_tokencore::store::NoRecord;

    /// Every record below was written by a daemon that is gone - what a restart reads. The rule about
    /// a daemon that is still beating has its own test, which calls `super::decide` to say so.
    fn decide(
        run: &SubagentRun,
        instance: &str,
        held: bool,
        dir_exists: impl Fn(&str) -> bool,
        mode: RestoreMode,
    ) -> Decision {
        super::decide(run, instance, held, false, dir_exists, mode)
    }

    fn resumable(
        run: &SubagentRun,
        instance: &str,
        held: bool,
        dir_exists: impl Fn(&str) -> bool,
    ) -> Result<TaskSpec, String> {
        super::resumable(run, instance, held, false, dir_exists)
    }

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
            later_answer: None,
            delivered: false,
            started_ms: 0,
            updated_ms: 0,
        }
    }

    const NEW: &str = "new";

    /// A spawn whose row the store refused still leaves a record when the child settles. A
    /// `database is locked` at the spawn used to print and be dropped, leaving the child with
    /// no record at all
    #[test]
    fn a_row_the_spawn_could_not_write_is_written_when_the_child_settles() {
        let path = std::env::temp_dir().join(format!(
            "letibot-record-pending-{}-{}.db",
            std::process::id(),
            letibot_sessionlog::registry::now_ms()
        ));
        for s in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{s}", path.display()));
        }
        let store = RunStore {
            path: path.clone(),
            cell: Default::default(),
        };
        let mut row = run("running");
        row.child_id = "s-1-sub-lost".into();
        let record = SubagentRecord {
            store: store.clone(),
            child: "s-1-sub-lost".into(),
            registry: Registry::new(),
            pending: Some(row),
        };
        assert!(
            store
                .with(|s| s.subagent_run("s-1-sub-lost").map_err(|e| e.to_string()))
                .unwrap()
                .is_none(),
            "the state this test is about: a spawn whose row was not written"
        );
        record.settled(
            &TaskStatus::Done {
                answer: "the sweep is in".into(),
                child: None,
            },
            false,
        );
        let written = store
            .with(|s| s.subagent_run("s-1-sub-lost").map_err(|e| e.to_string()))
            .unwrap()
            .expect("the row the spawn could not write is written at the settlement");
        assert_eq!(written.state, "done");
        assert_eq!(written.outcome.as_deref(), Some("the sweep is in"));
        assert_eq!(written.parent_id, "s-1");
        for s in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{s}", path.display()));
        }
    }

    /// **A later turn the orderly stop interrupted is not the child's answer.** `run_turn` hands back
    /// the text it had when the daemon stopped it, so recording that makes the next daemon deliver a
    /// fragment as the child's last word - the reading `settled` already refuses for the first turn.
    #[test]
    fn a_later_answer_the_shutdown_interrupted_is_not_recorded() {
        let path = std::env::temp_dir().join(format!(
            "letibot-record-stopping-{}-{}.db",
            std::process::id(),
            letibot_sessionlog::registry::now_ms()
        ));
        for s in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{s}", path.display()));
        }
        let store = RunStore {
            path: path.clone(),
            cell: Default::default(),
        };
        let mut row = run("done");
        row.outcome = Some("the answer it gave first".into());
        store
            .with(|s| s.put_subagent_run(&row).map_err(|e| e.to_string()))
            .unwrap();
        let registry = Registry::new();
        registry.close();
        let record = SubagentRecord {
            store: store.clone(),
            child: "s-1-sub-1".into(),
            registry,
            pending: None,
        };
        record.answered_again("half a sen");
        let got = store
            .with(|s| s.subagent_run("s-1-sub-1").map_err(|e| e.to_string()))
            .unwrap()
            .expect("the row");
        assert_eq!(
            got.later_answer, None,
            "the daemon's own interruption was written as the child's newest answer"
        );
        for s in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{s}", path.display()));
        }
    }

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

    /// **Another daemon still beating is not a gone one** (R91.9): the store is shared by every
    /// daemon on the box, so a running child of a live instance is left where it runs - resumed, it
    /// would be a second copy in the same worktree - and `task_resume` is refused by name.
    #[test]
    fn a_record_whose_daemon_is_still_beating_is_left_alone() {
        for mode in [RestoreMode::Resume, RestoreMode::Remind] {
            assert_eq!(
                super::decide(&run("running"), NEW, false, true, |_| true, mode),
                Decision::Leave,
                "{mode:?}"
            );
        }
        // A tool's helper is the one a restart used to RETIRE - which would kill a live daemon's read.
        let mut helper = run("running");
        helper.prompt = "You are reading part 3 of 12 of a longer body of text on somebody else's \
                         behalf. They asked:\n\n  why"
            .into();
        helper.spec_json = r#"{"role":"researcher","placement":"host"}"#.into();
        assert!(is_helper(&helper));
        assert_eq!(
            super::decide(&helper, NEW, false, true, |_| true, RestoreMode::Resume),
            Decision::Leave
        );
        let why = super::resumable(&run("running"), NEW, false, true, |_| true)
            .expect_err("a live daemon's child is not brought back");
        assert!(
            why.contains("still up") && why.contains("Nothing was spawned"),
            "{why}"
        );
        // A settled child of a live daemon is still not news here: that daemon delivers it.
        let mut done = run("done");
        done.outcome = Some("x".into());
        assert_eq!(
            super::decide(&done, NEW, false, true, |_| true, RestoreMode::Resume),
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

    /// pg-noop's twelve part readers : settled and never marked delivered, because
    /// `digest` read their answers itself - a restart handed all twelve to the model as just
    /// finished. A helper, old record or new, is left when settled and retired silently mid-task.
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

    /// **A child that failed and then answered again reads as its last word, at every door.** The row
    /// keeps the failure in `outcome`, but the later turn is what the parent is handed as news - so a
    /// reader that reported the old failure would contradict the notice it just received.
    #[test]
    fn a_failed_child_that_answered_again_reads_as_that_answer() {
        let mut again = run("failed");
        again.outcome = Some("it blew up on the second read".into());
        again.later_answer = Some("the answer it gave after".into());
        match recorded_status(&again, NEW) {
            TaskStatus::Done { answer, .. } => {
                assert!(answer.contains("the answer it gave after"), "{answer}");
            }
            other => panic!("a child that spoke again read as {other:?}"),
        }
        assert_eq!(
            decide(&again, NEW, false, |_| true, RestoreMode::Resume),
            Decision::Deliver(TaskStatus::Done {
                answer: answer_said(&again),
                child: None,
            }),
            "the delivery after a restart says what `task_result` says"
        );
        // A failure with no later turn is still a failure.
        let mut once = run("failed");
        once.outcome = Some("it blew up on the second read".into());
        assert_eq!(
            recorded_status(&once, NEW),
            TaskStatus::Failed {
                why: "it blew up on the second read".into()
            }
        );
    }

    /// Derived names are unique among a parent's children; carried names are kept - the
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

    /// One state for one child, whichever door asks - pg-noop, 2026-10-14: `task_result` listed
    /// `slt-current-number` as `unknown`, refused it as *"no subagent called … was started here"*
    /// and `task_resume` refused it as *"finished its task"*. The record answers both now.
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

    /// **A child that answered again is read as its later turn**, because after a restart the row is
    /// all `task_result` has: the answer it hands over is the last word, it says which turn that came
    /// from, and the first answer stays in the row for whoever reads `outcome`.
    #[test]
    fn a_settled_child_with_a_later_answer_reads_as_that_later_turn() {
        let mut again = run("done");
        again.outcome = Some("the first answer".into());
        again.later_answer = Some("the second answer".into());
        match recorded_status(&again, NEW) {
            TaskStatus::Done { answer, .. } => {
                assert!(answer.contains("the second answer"), "{answer}");
                assert!(!answer.contains("the first answer"), "{answer}");
                assert!(
                    answer.contains("a later turn of this child"),
                    "a parent has to be able to tell this from the answer the child gave first: \
                     {answer}"
                );
            }
            other => panic!("a settled child with a later answer read as {other:?}"),
        }
        // **A restart's delivery at open says the same thing**, or the notice would carry the first
        // answer while `task_result` hands over the second.
        match decide(&again, NEW, false, |_| true, RestoreMode::Resume) {
            Decision::Deliver(TaskStatus::Done { answer, .. }) => {
                assert!(answer.contains("the second answer"), "{answer}");
            }
            other => panic!("a settled, undelivered child is {other:?}"),
        }
        // And a child that answered once is unchanged: no sentence, no later answer.
        let mut once = run("done");
        once.outcome = Some("the first answer".into());
        assert_eq!(
            recorded_status(&once, NEW),
            TaskStatus::Done {
                answer: "the first answer".into(),
                child: None,
            }
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

    /// A VM child that worked in its worktree MOUNTED comes back; one that worked on a copy
    /// does not - a Mac VM child given a worktree works in it directly (`firecode::mounts`), so
    /// its edits survive the restart. Through both doors.
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
            skills: vec!["release".into()],
        };
        let back = RecordedSpec::from_json(&RecordedSpec::of(&spec).to_json())
            .unwrap()
            .spec()
            .unwrap();
        assert_eq!(back, spec);
    }

    /// A recorded child is named by its record, and an old record still gets a name - the
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

    /// A child with no record reads two ways, and the difference is its last row - an answer is
    /// returned as an answer, and anything else is said to be gone rather than dressed up as
    /// one. The state is not hypothetical
    #[test]
    fn a_child_with_no_record_reads_its_own_last_row() {
        let child = |why, last: Option<TranscriptItem>| letibot_tokencore::store::UnrecordedChild {
            id: "s-1-sub-lost".into(),
            title: Some("the-notices-stay-collapsed".into()),
            role: Some("coder".into()),
            workspace_root: "/wt/agent-lost".into(),
            created_ms: 1,
            why,
            last_item: last,
        };
        // An answer with no call after it is a finished turn: its text IS the answer.
        let answered = a_child_with_no_record(&child(
            NoRecord::Due,
            Some(TranscriptItem::Assistant {
                text: "the sweep is in".into(),
                tool_calls: vec![],
                truncated: false,
            }),
        ));
        assert_eq!(
            answered,
            TaskStatus::Done {
                answer: "the sweep is in".into(),
                child: None,
            }
        );
        // A turn that was still going when the rows stopped has NO answer, and the sentence says
        // where the child was and where its work still is.
        let gone = a_child_with_no_record(&child(
            NoRecord::Due,
            Some(TranscriptItem::ToolResult {
                call_id: "c1".into(),
                name: "bash".into(),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload: "ok".into(),
                edit: None,
                origin: None,
                media: None,
            }),
        ));
        let TaskStatus::Failed { why } = gone else {
            panic!("a child mid-turn has no answer: {gone:?}");
        };
        assert!(why.contains("no record of it was ever written"), "{why}");
        assert!(why.contains("the result of `bash`"), "{why}");
        assert!(
            why.contains("`transcript` with session=\"s-1-sub-lost\""),
            "{why}"
        );
        assert!(why.contains("/wt/agent-lost"), "{why}");
        assert!(
            !why.contains("task_resume` with task="),
            "nothing can resume it: {why}"
        );
        // And a child that wrote no row of its own at all.
        let TaskStatus::Failed { why } = a_child_with_no_record(&child(NoRecord::Due, None)) else {
            panic!("a child that wrote nothing has no answer");
        };
        assert!(why.contains("it wrote no row of its own at all"), "{why}");
    }

    /// How far a child with no record got, in the two sizes the two doors need - both from ONE
    /// judgement, because a child whose text [`a_child_with_no_record`] returns as an ANSWER
    /// must be the child [`how_far`] calls `Answered`. The defect this closes
    #[test]
    fn how_far_says_where_a_child_with_no_record_stopped() {
        use letibot_tools::builtins::task::{LostChild, TaskStatus};

        let child = |last: Option<TranscriptItem>| letibot_tokencore::store::UnrecordedChild {
            id: "s-1-sub-lost".into(),
            title: Some("the sweep".into()),
            role: Some("coder".into()),
            workspace_root: "/wt/agent-lost".into(),
            created_ms: 1,
            why: NoRecord::Due,
            last_item: last,
        };
        let answer = TranscriptItem::Assistant {
            text: "the sweep is in".into(),
            tool_calls: vec![],
            truncated: false,
        };
        let asked = TranscriptItem::Assistant {
            text: String::new(),
            tool_calls: vec![letibot_transcript::ToolCall {
                id: "c1".into(),
                name: "bash".into(),
                arguments: "{}".into(),
            }],
            truncated: false,
        };
        let result = TranscriptItem::ToolResult {
            call_id: "c1".into(),
            name: "bash".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload: "ok".into(),
            edit: None,
            origin: None,
            media: None,
        };

        // Two children that stopped somewhere different read differently - this is the whole
        // point, and the sentence the list printed for both was one.
        let mid_call = how_far(&child(Some(asked)));
        let mid_turn = how_far(&child(Some(result.clone())));
        let never = how_far(&child(None));
        assert_eq!(
            mid_call,
            LostChild::Stopped("mid-`bash`, on a call it never got an answer to".into())
        );
        assert_eq!(mid_turn, LostChild::Stopped("after `bash`'s result".into()));
        assert_eq!(
            never,
            LostChild::Stopped("before it wrote a single row".into())
        );
        assert_ne!(mid_call, mid_turn, "two classes, one clause");

        // A last row that IS an answer is the other class, and the two doors agree about it.
        assert_eq!(how_far(&child(Some(answer.clone()))), LostChild::Answered);
        assert!(
            matches!(
                a_child_with_no_record(&child(Some(answer))),
                TaskStatus::Done { .. }
            ),
            "the class the list calls `Answered` is the one the answer door returns as an answer"
        );

        // Every shape of last row, in both sizes - the clause reads on from *"it stopped "*
        // and the paragraph never loses the long form a model has been reading.
        for (item, said, brief) in [
            (
                None,
                "nothing: it wrote no row of its own at all",
                "before it wrote a single row",
            ),
            (
                Some(result.clone()),
                "the result of `bash`, so the turn went on from there",
                "after `bash`'s result",
            ),
            (
                Some(TranscriptItem::User {
                    parts: vec![],
                    speaker: Default::default(),
                }),
                "the prompt it was given",
                "on the prompt it was given, before it answered",
            ),
            (
                Some(TranscriptItem::Reasoning {
                    text: "hm".into(),
                    field: letibot_transcript::ReasoningField::Inline,
                    truncated: false,
                }),
                "a thought it never finished",
                "mid-thought",
            ),
        ] {
            let ended = EndedIn::of(item.as_ref());
            assert_eq!(ended.said(), said);
            assert_eq!(ended.brief(), brief);
            assert_eq!(
                how_far(&child(item)),
                LostChild::Stopped(brief.into()),
                "{brief}"
            );
        }
        // And nothing in either rendering guesses WHY the row is missing - that is
        // `no_record_cause`'s, from the store's own class, and the list is not told it at all.
        for why in [NoRecord::Due, NoRecord::BeforeTheStore] {
            let c = letibot_tokencore::store::UnrecordedChild { why, ..child(None) };
            let said = match how_far(&c) {
                LostChild::Stopped(words) => words,
                other => panic!("{other:?}"),
            };
            assert!(
                !said.contains("refused") && !said.contains("store"),
                "{said}"
            );
        }
    }

    /// A child older than the store's first record is not told a write was refused - the class
    /// the window exists for, and the sentence a model reads about it. MEASURED 2026-11-01
    #[test]
    fn a_child_from_before_the_record_is_told_that_and_not_a_refused_write() {
        let old = letibot_tokencore::store::UnrecordedChild {
            id: "s-1-sub-old".into(),
            title: Some("the sweep".into()),
            role: Some("coder".into()),
            workspace_root: "/wt/agent-old".into(),
            created_ms: 1,
            why: NoRecord::BeforeTheStore,
            last_item: None,
        };
        let TaskStatus::Failed { why } = a_child_with_no_record(&old) else {
            panic!("a child with no answer is not an answer");
        };
        assert!(
            why.contains("this child is from before this store recorded any child"),
            "{why}"
        );
        assert!(
            !why.contains("refused the row"),
            "no write of it was refused, so the sentence may not say one was: {why}"
        );
        // The rest of what a parent needs is still there: where it worked, how to read it, and
        // what it was asked.
        assert!(why.contains("/wt/agent-old"), "{why}");
        assert!(
            why.contains("`transcript` with session=\"s-1-sub-old\""),
            "{why}"
        );
        assert!(why.contains("it was asked: the sweep"), "{why}");
        // And the class that IS a lost write still says so.
        assert_eq!(
            no_record_cause(NoRecord::Due),
            "the store refused the row its spawn writes"
        );
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
