//! **The review queue's reviewers** — the half that runs on the review queue's own SESSION
//! (`sessions::gatekeeper::SESSION`), because a reviewer is a `task` child of it (the operator:
//! *"reviews are subagents managed by the main gatekeeper session … gatekeepers are subagents"*,
//! and *"model reviews stay, yes"*).
//!
//! The queue's thread claims a review (`waiting → reviewing`, owner = this daemon, with a
//! deadline) and rings the session; [`serve`] starts a reviewer for every claimed row this daemon
//! owns, reads the verdict the reviewer HANDS IN (`review_verdict`, the daemon's desk), and moves
//! the row — conditionally, on the row's state and owner, so a deadline or a person that moved the
//! row first wins and the reviewer is killed rather than heard.
//!
//! # The verdict is bound to the head it judged (decision 6)
//!
//! The reviewer is shown `base...<head_sha>` — the head the queue read when it claimed the row —
//! and an accept writes that sha as the merge row's `approved_sha` in the same transaction
//! (`Store::approve_review`). A commit made on the branch while the review ran is not what was
//! judged; the merge queue sees the difference and sends the branch back.
//!
//! # Where a reviewer runs (decision 3)
//!
//! In a firecode VM booted on a FRESH CLONE of the branch, when the box can boot one — never on
//! the workspace (72.8 GiB against an 8 GiB ceiling: 8 of 21 reviews on 2026-10-11 died on that
//! copy). Otherwise on the host, in the read-only `gatekeeper` seat, told the repository to run
//! its commands in. A reviewer that cannot start is RAISED (decision 4) — `blocked`, at high
//! severity — and never handed to a model to merge without the gate (defect 11).

use std::path::{Path, PathBuf};

use letibot_sessionlog::SessionEvent;
use letibot_tokencore::store::{
    Approval, MergePriority, Move, NewReview, ReviewKind, ReviewRow, ReviewState, Store, StoreError,
};
use letibot_tools::builtins::task::{TaskRunner, TaskSpec, TaskStatus, WorktreeSpec};
use letibot_tools::builtins::verdict::ReviewDesk;
use letibot_tools::gatekeeper::{Decision, ReviewRequest};

use super::gate::now_ms;
use super::git;
use super::serve::MAX_REVIEW_ATTEMPTS;
use super::settings::{QueueSettings, RedTarget};
use crate::sessions::gatekeeper::ReviewerPlacement;

/// **What the reviewers' host needs to serve its claimed rows.**
pub struct Pool<'a> {
    pub store: &'a Store,
    pub runner: &'a dyn TaskRunner,
    pub desk: &'a ReviewDesk,
    /// The session the reviewers are children of.
    pub host: &'a str,
    /// This daemon's lease identity — the owner the queue's thread claimed the rows as.
    pub me: &'a str,
    pub placement: ReviewerPlacement,
    pub events: &'a dyn Fn(SessionEvent),
    /// Send a refusal's complaints to a session (`TaskRunner::send`).
    pub deliver: &'a dyn Fn(&str, &str) -> Result<String, String>,
    pub settings: &'a dyn Fn(&Path) -> Result<QueueSettings, String>,
    /// Where a VM reviewer's fresh clone is made.
    pub clones: PathBuf,
}

/// What one pass of [`serve`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Served {
    pub started: usize,
    pub moved: bool,
}

impl Pool<'_> {
    fn announce(&self, queue: &str, id: &str, to: &str, why: &str) {
        (self.events)(SessionEvent::QueueMoved {
            queue: queue.into(),
            id: id.into(),
            to: to.into(),
            why: why.into(),
        });
    }

    fn raise(&self, code: &str, detail: String) {
        eprintln!("  review queue: {code}: {detail}");
        (self.events)(SessionEvent::Warning {
            code: code.into(),
            detail,
            compaction: None,
        });
    }

    fn mv(&self, r: &ReviewRow, to: ReviewState, mv: Move) -> Result<bool, StoreError> {
        let why = mv.why.clone();
        let ok = self.store.move_review(&r.id, r.state, to, &mv, now_ms())?;
        if ok {
            self.announce("review", &r.id, to.as_str(), &why);
        }
        Ok(ok)
    }

    /// An attempt that reached no verdict: back to `waiting`, counted; the third is a raise.
    fn attempt_failed(&self, r: &ReviewRow, why: String) -> Result<bool, StoreError> {
        let attempts = r.attempts + 1;
        let base = Move::new(self.me, why.clone())
            .owned_by(self.me)
            .set("owner", "")
            .clear("reviewer")
            .clear("deadline_ms")
            .set("attempts", attempts)
            .set("failure", why.clone());
        if attempts >= MAX_REVIEW_ATTEMPTS {
            let ok = self.mv(r, ReviewState::Blocked, base)?;
            if ok {
                self.raise(
                    "queue_blocked",
                    format!(
                        "the review of `{}` (`{}`) is BLOCKED after {attempts} attempts without a \
                         verdict: {why} `/queue restart {}` asks again; `/queue approve {}` \
                         approves it over the reviewer (the merge gate still runs).",
                        r.branch, r.id, r.id, r.id
                    ),
                );
            }
            Ok(ok)
        } else {
            self.mv(r, ReviewState::Waiting, base)
        }
    }
}

/// **One pass of the reviewers' host** — start, collect, kill. See the module docs.
pub fn serve(p: &Pool) -> Result<Served, StoreError> {
    let mut out = Served::default();
    let rows = p.store.review_rows(None)?;

    // ---- Start a reviewer for every row this daemon claimed and nobody is on.
    for r in rows
        .iter()
        .filter(|r| r.state == ReviewState::Reviewing && r.owner == p.me && r.reviewer.is_none())
    {
        if p.desk.is_open_for(&r.id) {
            continue;
        }
        match start_reviewer(p, r) {
            Ok((handle, req)) => {
                if p.mv(
                    r,
                    ReviewState::Reviewing,
                    Move::new(p.me, format!("reviewer {handle} started on {}", r.head_sha))
                        .owned_by(p.me)
                        .set("reviewer", handle.clone()),
                )? {
                    p.desk.opened(p.host, &handle, &r.id, req);
                    out.started += 1;
                } else {
                    // The row moved under us (a deadline, a person): this reviewer is nobody's.
                    let _ = p.runner.kill(&handle);
                }
                out.moved = true;
            }
            Err(why) => {
                // **A reviewer that cannot start RAISES** (decision 4).
                let why = format!("a reviewer for `{}` could not start: {why}", r.branch);
                if p.mv(
                    r,
                    ReviewState::Blocked,
                    Move::new(p.me, why.clone())
                        .owned_by(p.me)
                        .set("owner", "")
                        .set("failure", why.clone())
                        .set("attempts", r.attempts + 1),
                )? {
                    p.raise(
                        "queue_blocked",
                        format!(
                            "{why}. The review is BLOCKED (`{}`): `/queue restart {}` asks again, \
                             `/queue approve {}` approves it over the reviewer — the merge gate \
                             still runs.",
                            r.id, r.id, r.id
                        ),
                    );
                }
                out.moved = true;
            }
        }
    }

    // ---- Collect: verdicts, deaths, and reviewers whose row moved on without them.
    let mut still: Vec<String> = Vec::new();
    for open in p.desk.open(p.host) {
        let child = open.child.clone();
        let Some(r) = p.store.review_row(&open.entry_id)? else {
            let _ = p.runner.kill(&child);
            out.moved = true;
            continue;
        };
        let ours = r.state == ReviewState::Reviewing
            && r.owner == p.me
            && r.reviewer.as_deref() == Some(child.as_str());
        if !ours {
            // A deadline, a person, or a recovery moved the row: the reviewer judges nothing now.
            let _ = p.runner.kill(&child);
            out.moved = true;
            continue;
        }
        if let Some(said) = p.desk.take_verdict(&child) {
            out.moved = true;
            verdict(p, &r, &child, said)?;
            continue;
        }
        match p.runner.collect(&child, std::time::Duration::ZERO) {
            TaskStatus::Running { .. } => still.push(child),
            TaskStatus::Done { .. } => {
                out.moved = true;
                p.attempt_failed(
                    &r,
                    format!(
                        "the reviewer `{child}` answered without handing a verdict in \
                         (`review_verdict` is the only door one goes through); its reply is in its \
                         own session and is not evidence"
                    ),
                )?;
            }
            TaskStatus::Failed { why } => {
                out.moved = true;
                p.attempt_failed(&r, format!("the reviewer `{child}` failed: {why}"))?;
            }
            TaskStatus::Unknown => {
                out.moved = true;
                p.attempt_failed(
                    &r,
                    format!("the reviewer `{child}` vanished without a verdict"),
                )?;
            }
        }
    }
    p.desk.keep(p.host, &still);
    Ok(out)
}

/// Start one reviewer: where it runs, on what tree, with which prompt.
fn start_reviewer(p: &Pool, r: &ReviewRow) -> Result<(String, ReviewRequest), String> {
    let repo = PathBuf::from(&r.repository);
    let mut spec = TaskSpec {
        role: "gatekeeper".into(),
        placement: p.placement.placement(),
        ..Default::default()
    };
    let cwd = match p.placement {
        ReviewerPlacement::Vm => {
            // **A fresh clone of the branch** (decision 3), not the workspace.
            let dir = p.clones.join(format!("review-{}", r.id));
            let what = if r.kind == ReviewKind::Target {
                r.target.clone()
            } else {
                r.branch.clone()
            };
            git::clone_branch(&repo, &what, &r.head_sha, &dir)?;
            spec.vm_source = Some(dir);
            String::new()
        }
        ReviewerPlacement::Host | ReviewerPlacement::NoVm { .. } => r.repository.clone(),
    };
    let req = ReviewRequest {
        brief: r.brief.clone(),
        branch: r.branch.clone(),
        base_sha: r.base_sha.clone(),
        head_sha: r.head_sha.clone(),
        cwd,
    };
    let prompt = match r.kind {
        ReviewKind::Branch => letibot_tools::gatekeeper::review_prompt(&req),
        ReviewKind::Target => red_target_prompt(&req, &r.target),
    };
    let handle = p.runner.start(&prompt, &spec)?;
    Ok((handle, req))
}

/// **The red target's reviewer is told what its verdict MEANS** — the tip is broken; `reject`
/// is *the code is broken*, which opens a fix branch through the queue when the project's policy
/// is `fix` (decision 11); `accept` is *the red is not in the code* (a flake, the machine), which
/// gates the tip again; `needs_human` asks the operator.
pub fn red_target_prompt(req: &ReviewRequest, target: &str) -> String {
    format!(
        "{}\n\nTHIS REVIEW IS DIFFERENT: it is not a branch asking to land. `{target}` itself is RED \
         at {head}: its own merge gate fails there, and the merge queue lands nothing until it is \
         green again. What failed, and how, is the ask above. Find what in `{target}`'s recent \
         history broke it — `git log --oneline -20 {head}` and `git show SHA` are your tools — \
         and hand your verdict in with `review_verdict`, where the words mean:\n\
         - `reject`: the code at {head} is broken. Say which commit and why in `reasons`; a fix \
         branch is opened from your reasons and goes through the queue like any other.\n\
         - `accept`: the code is sound and the red is not in it (a flaky test, the machine). The \
         tip is gated again.\n\
         - `needs_human`: you cannot tell; the operator is asked.",
        letibot_tools::gatekeeper::review_prompt(req),
        head = req.head_sha,
    )
}

/// **A verdict handed in** — moved onto the row conditionally, and acted on.
fn verdict(
    p: &Pool,
    r: &ReviewRow,
    child: &str,
    said: letibot_tools::gatekeeper::Submission,
) -> Result<(), StoreError> {
    let by = format!("reviewer:{child}");
    let reasons = serde_json::to_string(&said.reasons).unwrap_or_else(|_| "[]".into());
    let files = serde_json::to_string(&said.looked_at.files).unwrap_or_else(|_| "[]".into());
    let commands = serde_json::to_string(&said.looked_at.commands).unwrap_or_else(|_| "[]".into());
    let judged = |word: &str, why: String| {
        Move::new(&by, why)
            .owned_by(p.me)
            .set("owner", "")
            .clear("deadline_ms")
            .set("verdict", word)
            .set("verdict_by", by.clone())
            .set("reviewed_sha", r.head_sha.clone())
            .set("reasons_json", reasons.clone())
            .set("files_json", files.clone())
            .set("commands_json", commands.clone())
    };
    let reasons_line = said.reasons.join("; ");
    match (r.kind, said.decision) {
        (ReviewKind::Branch, Decision::Accept) => {
            let merge_id = format!("m-{}-{}", r.id, now_ms());
            let why = format!(
                "accepted at {} by {by}{}",
                r.head_sha,
                if reasons_line.is_empty() {
                    String::new()
                } else {
                    format!(": {reasons_line}")
                }
            );
            if p.store.approve_review(
                &r.id,
                &Approval {
                    expect: ReviewState::Reviewing,
                    expect_owner: Some(p.me.to_string()),
                    reviewed_sha: r.head_sha.clone(),
                    by: by.clone(),
                    reasons: said.reasons.clone(),
                    files: said.looked_at.files.clone(),
                    commands: said.looked_at.commands.clone(),
                    why: why.clone(),
                    merge_id: merge_id.clone(),
                },
                now_ms(),
            )? {
                p.announce("review", &r.id, "approved", &why);
                p.announce("merge", &merge_id, "queued", &why);
            }
        }
        (ReviewKind::Branch, Decision::Reject) => {
            let why = format!("rejected at {} by {by}: {reasons_line}", r.head_sha);
            if p.mv(r, ReviewState::Rejected, judged("reject", why))? {
                deliver_refusal(p, r, &said.reasons);
            }
        }
        (_, Decision::NeedsHuman) => {
            let why = format!("{by} could not decide at {}: {reasons_line}", r.head_sha);
            if p.mv(
                r,
                ReviewState::CannotDecide,
                judged("needs_human", why.clone()),
            )? {
                p.raise(
                    "queue_needs_human",
                    format!(
                        "the review of `{}` (`{}`) cannot decide — {why}. `/queue approve {}` \
                         approves it (the merge gate still runs), `/queue veto {} WHY` sends it \
                         back, `/queue rm {}` drops it.",
                        r.branch, r.id, r.id, r.id, r.id
                    ),
                );
            }
        }
        (ReviewKind::Target, Decision::Accept) => {
            let why = format!(
                "{by} finds `{}` sound at {} — the red is not in the code ({reasons_line}); the tip \
                 is gated again",
                r.target, r.head_sha
            );
            if p.mv(r, ReviewState::Withdrawn, judged("accept", why))? {
                // Force a fresh gate of the tip on the queue's next pass.
                p.store.set_target_health(
                    &letibot_tokencore::store::TargetHealth {
                        repository: r.repository.clone(),
                        target: r.target.clone(),
                        sha: r.head_sha.clone(),
                        state: letibot_tokencore::store::HealthState::Gating,
                        checked_ms: now_ms(),
                        evidence: "the red tip's reviewer finds the code sound: gating it again"
                            .into(),
                        gate_steps: vec![],
                    },
                    &by,
                )?;
            }
        }
        (ReviewKind::Target, Decision::Reject) => {
            let why = format!(
                "{by} finds `{}` broken at {}: {reasons_line}",
                r.target, r.head_sha
            );
            if p.mv(r, ReviewState::Rejected, judged("reject", why.clone()))? {
                open_fix_branch(p, r, &said.reasons, &why)?;
            }
        }
    }
    Ok(())
}

/// **Decision 11**: the reviewer of the red tip opens a fix branch, through the queue like any
/// other, and the operator is told — or, under a project's `red_target = "raise"`, only told.
fn open_fix_branch(
    p: &Pool,
    r: &ReviewRow,
    reasons: &[String],
    why: &str,
) -> Result<(), StoreError> {
    let repo = PathBuf::from(&r.repository);
    let policy = (p.settings)(&repo)
        .map(|s| s.red_target)
        .unwrap_or(RedTarget::Fix);
    if policy == RedTarget::Raise {
        p.raise(
            "target_red",
            format!(
                "EMERGENCY: {why}. This project's policy is `red_target = \"raise\"`: no fix branch \
                 was opened — the target needs a person's fix."
            ),
        );
        return Ok(());
    }
    let short: String = r.head_sha.chars().take(8).collect();
    let brief = format!(
        "Fix `{target}`: its merge gate is RED at {head} in {repo}. The reviewer of the red tip \
         found: {reasons}\n\nThe gate's own words, at the end of its output:\n\n{ask}\n\nMake the \
         smallest change that turns the gate green, commit it on this branch, and finish. It goes \
         through the review queue and the merge queue like any other branch; while `{target}` is \
         red it is the only branch the merge queue lands.",
        target = r.target,
        head = r.head_sha,
        repo = r.repository,
        reasons = reasons.join("; "),
        ask = r.brief,
    );
    let spec = TaskSpec {
        role: "coder".into(),
        ..Default::default()
    };
    let wt = WorktreeSpec {
        slug: format!("fix-{}-{short}", r.target.replace('/', "-")),
        base: Some(r.head_sha.clone()),
        main_tree: false,
        repo: Some(r.repository.clone()),
    };
    match p.runner.start_worktree(&brief, &spec, &wt) {
        Ok(h) => {
            let mv = Move::new(p.me, format!("fix branch `{}` opened by {}", h.placement.branch, h.handle))
                .set("fixes", h.handle.clone());
            p.mv(&ReviewRow { state: ReviewState::Rejected, ..r.clone() }, ReviewState::Rejected, mv)?;
            p.raise(
                "target_red",
                format!(
                    "EMERGENCY: {why}. A fix branch `{}` was opened by {} and goes through the \
                     queue; it is the only branch that lands while `{}` is red.",
                    h.placement.branch, h.handle, r.target
                ),
            );
        }
        Err(e) => p.raise(
            "target_red",
            format!("EMERGENCY: {why}. A fix branch could NOT be opened: {e}. A person has to fix `{}`.", r.target),
        ),
    }
    Ok(())
}

/// **The refusal goes back to the child that wrote the branch**, or its root when the child is
/// gone; when nobody can be told, the operator is (it stays on the row either way).
fn deliver_refusal(p: &Pool, r: &ReviewRow, reasons: &[String]) {
    let list = reasons
        .iter()
        .map(|x| format!("  - {x}"))
        .collect::<Vec<_>>()
        .join("\n");
    let wt = r.worktree.as_deref().unwrap_or("its worktree");
    let to_child = format!(
        "Your branch `{branch}` was REFUSED by the review at {head}, so it does not land. The \
         reviewer's reasons:\n\n{list}\n\nThe branch is still in {wt}. Answer the reasons there \
         and commit: a new head goes back to review by itself (the verdict is bound to the head it \
         judged).",
        branch = r.branch,
        head = r.head_sha,
    );
    let child = r.session_id.clone();
    let mut missed: Vec<String> = Vec::new();
    if !child.is_empty() {
        match (p.deliver)(&child, &to_child) {
            Ok(_) => return,
            Err(e) => missed.push(format!("`{child}`: {e}")),
        }
    }
    let root = super::root_session(p.store, &child);
    if !root.is_empty() && root != child {
        let to_root = format!(
            "The review of `{branch}` — the branch of a child you started (`{child}`), which could \
             not be told ({why}) — REFUSED it at {head}, so it does not land. The reviewer's \
             reasons:\n\n{list}\n\nThe branch is in {wt}, and it is yours to move: answer the \
             reasons there and commit (a new head goes back to review by itself), or ask the \
             operator for `/queue approve {id}` or `/queue rm {id}`.",
            branch = r.branch,
            head = r.head_sha,
            why = missed.join("; "),
            id = r.id,
        );
        match (p.deliver)(&root, &to_root) {
            Ok(_) => return,
            Err(e) => missed.push(format!("`{root}`: {e}")),
        }
    }
    p.raise(
        "refusal_undelivered",
        format!(
            "the review of `{}` (`{}`) refused it, and the complaints reached nobody ({}). The \
             verdict stands and is on the row; `/queue restart {}`, `/queue approve {}` or \
             `/queue rm {}`.",
            r.branch,
            r.id,
            missed.join("; "),
            r.id,
            r.id,
            r.id
        ),
    );
}

/// **A finished `task_start` child's branch, put up for review** — the enqueue. The repository
/// is the worktree's own; the target is the project's (decision 12); the head is read now. A
/// child opened to fix a red target carries `fixes`, and only such a branch lands while the
/// target is red.
pub fn enqueue_finished(
    store: &Store,
    handle: &str,
    brief: &str,
    placement: &letibot_tools::builtins::task::WorktreePlacement,
    settings: &dyn Fn(&Path) -> Result<QueueSettings, String>,
    now: u64,
) -> Result<Option<ReviewRow>, String> {
    if placement.main_tree {
        return Ok(None);
    }
    let wt = Path::new(&placement.path);
    let repo = git::repo_of_worktree(wt).ok_or_else(|| {
        format!(
            "the worktree {} is not in a git repository, so `{}` has nowhere to land",
            placement.path, placement.branch
        )
    })?;
    let settings = settings(&repo).unwrap_or_default();
    let target = super::settings::target_for(&repo, &settings).unwrap_or_default();
    let head = git::branch_head(&repo, &placement.branch).unwrap_or_default();
    let fixes = store
        .review_rows(Some(&repo.display().to_string()))
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|r| r.kind == ReviewKind::Target && r.fixes.as_deref() == Some(handle))
        .map(|r| r.head_sha);
    let new = NewReview {
        id: handle.to_string(),
        repository: repo.display().to_string(),
        target: target.clone(),
        kind: ReviewKind::Branch,
        session_id: handle.to_string(),
        branch: placement.branch.clone(),
        worktree: Some(placement.path.clone()),
        base_sha: placement.base_sha.clone(),
        head_sha: head,
        brief: brief.to_string(),
        priority: if fixes.is_some() {
            MergePriority::Urgent
        } else {
            MergePriority::Subagent
        },
        fixes: fixes.clone(),
        evidence: match &fixes {
            Some(red) => format!("put up for review onto `{target}` — it fixes the red tip {red}"),
            None => format!("put up for review onto `{target}`"),
        },
    };
    store
        .enqueue_review(&new, handle, now)
        .map_err(|e| format!("the review row for `{handle}` could not be written: {e}"))?;
    store.review_row(handle).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests;
