//! **The review queue's reviewers** — the half that runs on the review queue's own SESSION
//! (`sessions::gatekeeper::session`), because a reviewer is a `task` child of it (the operator:
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
//!
//! **Except when the tree could not be MADE.** A clone that failed is a disk's answer, not a
//! judgement of the branch, so it is a WAIT: the row goes back to `waiting` with the clone's own
//! words on it and spends no attempt ([`Pool::wait_for_tree`]), the way a review with no free VM
//! slot does ([`capacity`]). Decision 4's three attempts are for a review that cannot RUN.

use std::path::{Path, PathBuf};

use letibot_sessionlog::SessionEvent;
use letibot_tokencore::store::{
    Approval, MergePriority, Move, NewReview, ReviewKind, ReviewRow, ReviewState, Store, StoreError,
};
use letibot_tools::builtins::task::{
    TaskRunner, TaskSpec, TaskStatus, WorktreePlacement, WorktreeSpec,
};
use letibot_tools::builtins::verdict::ReviewDesk;
use letibot_tools::gatekeeper::{Decision, ReviewRequest};

use super::capacity;
use super::gate::now_ms;
use super::git;
use super::notices;
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
    /// **A repository's alert**, published only where that repository's sessions are
    /// ([`super::publish_for_repository`] in the daemon) — never on every hub.
    pub warn: &'a dyn Fn(&str, SessionEvent),
    /// Send a refusal's complaints to a session (`TaskRunner::send`).
    pub deliver: &'a dyn Fn(&str, &str) -> Result<String, String>,
    pub settings: &'a dyn Fn(&Path) -> Result<QueueSettings, String>,
    /// Where a VM reviewer's fresh clone is made.
    pub clones: PathBuf,
    /// **What holds each busy VM slot**, one line per tap (`crate::vm_reap::slot_holders`): the
    /// firecracker pid, when its run started, and its owner's handle and state — and the reaper
    /// is asked to look now when an owner is known to be finished or gone. Read when a capacity
    /// wait is raised as long, so the notice says what to free instead of only that all are busy.
    pub slots: &'a dyn Fn() -> Vec<String>,
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

    fn raise(&self, repository: &str, code: &str, detail: String) {
        eprintln!("  review queue: {code}: {detail}");
        (self.warn)(
            repository,
            SessionEvent::Warning {
                code: code.into(),
                detail,
                compaction: None,
            },
        );
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
    ///
    /// **What "3 attempts" means** (R61 defects 10 and 11, requirement 4): attempts to RUN a
    /// review of one head that ended without a verdict — a reviewer that died, vanished, answered
    /// without handing a verdict in, ran past its deadline, or could not be placed for a real
    /// reason. It bounds infrastructure retries (148 reviewer sessions for 21 rows) so that a
    /// review that cannot run is raised instead of retried in silence. It is NOT a limit on
    /// verdicts — a `needs_human` stops the row for a person by itself, and a new head starts
    /// the count again — and a reviewer firecode had no free slot for is not an attempt at all
    /// ([`Pool::wait_for_slot`]).
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
            let said = self.blocked_sentence(r, attempts, &why);
            let ok = self.mv(r, ReviewState::Blocked, base.set("failure", said.clone()))?;
            if ok {
                self.raise(
                    &r.repository,
                    "queue_blocked",
                    notices::blocked_after(r, &said),
                );
            }
            Ok(ok)
        } else {
            self.mv(r, ReviewState::Waiting, base)
        }
    }

    /// **Why a review is blocked, in one sentence a person can act on** — every counted attempt
    /// at this head, by its own gist (firecode's `error:` line, not its banner), oldest first;
    /// and, when every one was the reviewer's VM failing to start, that it was placement and not
    /// a judgement of the branch.
    fn blocked_sentence(&self, r: &ReviewRow, attempts: u32, last: &str) -> String {
        let history = self.store.transitions("review", &r.id).unwrap_or_default();
        // The counted failures are the moves from `reviewing` back to `waiting` that were not a
        // wait of their own (a capacity wait, a tree that could not be made), since the count was
        // last reset (a restart, a new head: a move INTO `waiting` from anywhere but `reviewing`).
        let mut earlier: Vec<String> = Vec::new();
        for t in history.iter().rev() {
            let from = t.from_state.as_deref().unwrap_or("");
            if t.to_state == "waiting" && from == "reviewing" {
                if !t.why.starts_with(capacity::WAITING_FOR_SLOT)
                    && !t.why.starts_with(WAITING_FOR_TREE)
                {
                    earlier.push(t.why.clone());
                }
            } else if (t.to_state == "waiting" && from != "waiting") || t.from_state.is_none() {
                break;
            }
        }
        earlier.reverse();
        let keep = (attempts as usize).saturating_sub(1);
        let skip = earlier.len().saturating_sub(keep);
        let mut all: Vec<String> = earlier.into_iter().skip(skip).collect();
        all.push(last.to_string());
        let placement = all
            .iter()
            .all(|w| w.contains("placing this session in a firecode VM"));
        let gists: Vec<String> = all.iter().map(|w| capacity::gist(w)).collect();
        notices::attempts_spent(attempts, &r.head_sha, &gists, placement)
    }

    /// **firecode had no free slot for the reviewer** — not an attempt, and not a block: the row
    /// goes back to `waiting` with a not-before time ([`capacity::retry_after_ms`]), says so once
    /// in the routine register, and raises once when the wait is long. See [`capacity`].
    fn wait_for_slot(&self, r: &ReviewRow, cap: &capacity::VmCapacity) -> Result<bool, StoreError> {
        let history = self.store.transitions("review", &r.id)?;
        let streak = capacity::streak(&history);
        // **The wait is the branch's, not the row's.** A red branch gets a new row per red tip
        // (`red-<branch>-<sha>`), and each row's own streak raised its own two notices: pg-noop's
        // `feat/memory-engine`, 2026-10-15, drew a fresh "waiting for a VM slot" at 16:10, 16:16,
        // 16:34, 16:48, … and a fresh "VM slots exhausted" half an hour after each, for one wait
        // that never ended. So the rows of the same branch that are in a capacity wait of their own
        // are read too ([`capacity::branch_wait`]), and the wait is said once while any of them is.
        let others = self.branch_streaks(r)?;
        let wait = capacity::branch_wait(streak, &others);
        let nth = streak.refusals + 1;
        let now = now_ms();
        let since = wait.since_ms.unwrap_or(now);
        let waited = now.saturating_sub(since);
        let delay = capacity::retry_after_ms(nth);
        let long = waited >= capacity::LONG_WAIT_MS && !wait.raised;
        let why = notices::capacity_wait(
            r,
            &cap.busy_line(),
            delay / 60_000,
            nth,
            long.then_some(waited / 60_000),
        );
        let ok = self.mv(
            r,
            ReviewState::Waiting,
            Move::new(self.me, why.clone())
                .owned_by(self.me)
                .set("owner", "")
                .clear("reviewer")
                .set("deadline_ms", now + delay)
                .set("failure", why),
        )?;
        if !ok {
            return Ok(false);
        }
        if wait.first {
            self.raise(
                &r.repository,
                "queue_waiting_for_vm",
                notices::waiting_for_vm(r, &cap.busy_line()),
            );
        }
        if long {
            self.raise(
                &r.repository,
                "queue_vm_slots_exhausted",
                notices::vm_slots_exhausted(
                    r,
                    waited / 60_000,
                    &cap.busy_line(),
                    &cap.raise_ceiling,
                    &(self.slots)(),
                ),
            );
        }
        Ok(true)
    }

    /// **The reviewer's tree could not be MADE** — the clone failed, or the finished clone could
    /// not be put at its name. Not an attempt, and not a block: the row goes back to `waiting`
    /// with the clone's own words on it and a not-before time ([`capacity::retry_after_ms`], the
    /// same ladder a VM slot wait climbs), so the queue claims it again and the pass clones again.
    ///
    /// **Why this is a wait and not a raise.** Decision 4's three attempts are for a review that
    /// cannot RUN — a reviewer that died, vanished, or answered without a verdict — and the rule
    /// exists so a review that cannot run is raised instead of retried in silence. A clone that
    /// failed is neither: a busy disk, a filesystem, and another daemon cloning the same row all
    /// answer a second time, and spending one of three attempts on them is what turned a lost race
    /// on 2026-10-29 into a branch that a person had to look at. MEASURED: the notice
    /// *"a reviewer for `agent/a-one-line-citation-is-filled` could not start: git clone … fatal:
    /// could not open '…/review-s-…-sub-1793298319066201000/.git/objects/pack/tmp_pack_cgbKfF'"*
    /// came out of the raise path, for a tree that another process had removed.
    ///
    /// **It is unbounded, and that is the trade.** A row whose tree cannot be made for a reason
    /// that will not pass (a repository that is gone) waits with the clone's words on it, visible
    /// in the queue's own pane, rather than being blocked as *a review that could not run* — which
    /// is a claim about the branch, and this is a claim about a disk. The ladder is what keeps it
    /// from being a spin: a minute, then two, then five.
    fn wait_for_tree(&self, r: &ReviewRow, why: String) -> Result<bool, StoreError> {
        let nth = self.tree_waits(r) + 1;
        // The clause leads, because that is what the queue's claim reads (`capacity::held_until`)
        // to hold the row until its not-before time, and what [`Pool::tree_waits`] counts.
        let said = format!(
            "{WAITING_FOR_TREE}: the reviewer's tree could not be made, so the review waits and \
             is tried again — no attempt is spent on it, and nothing about the branch was judged: \
             {why}"
        );
        self.mv(
            r,
            ReviewState::Waiting,
            Move::new(self.me, said.clone())
                .owned_by(self.me)
                .set("owner", "")
                .clear("reviewer")
                .set("deadline_ms", now_ms() + capacity::retry_after_ms(nth))
                .set("failure", said),
        )
    }

    /// **How many times in a row this row has waited for a tree** — newest back to the last other
    /// outcome, counted the way [`capacity::streak`] counts a slot wait: a claim, a reviewer
    /// started, or a note on a waiting row does not end the run of them.
    fn tree_waits(&self, r: &ReviewRow) -> u32 {
        let history = self.store.transitions("review", &r.id).unwrap_or_default();
        let mut n = 0;
        for t in history.iter().rev() {
            let from = t.from_state.as_deref().unwrap_or("");
            if t.to_state == "waiting" && t.why.starts_with(WAITING_FOR_TREE) {
                n += 1;
            } else if (t.to_state == "reviewing" && matches!(from, "waiting" | "reviewing"))
                || (t.to_state == "waiting" && from == "waiting")
            {
                continue;
            } else {
                break;
            }
        }
        n
    }

    /// **The capacity streaks of the OTHER rows of `r`'s branch** that are still in line
    /// (`waiting` or `reviewing`) — what [`capacity::branch_wait`] reads.
    fn branch_streaks(&self, r: &ReviewRow) -> Result<Vec<capacity::Streak>, StoreError> {
        let mut out = Vec::new();
        for o in self.store.review_rows(None)? {
            if o.id == r.id
                || o.repository != r.repository
                || o.branch != r.branch
                || !matches!(o.state, ReviewState::Waiting | ReviewState::Reviewing)
            {
                continue;
            }
            let s = capacity::streak(&self.store.transitions("review", &o.id)?);
            if s.refusals > 0 {
                out.push(s);
            }
        }
        Ok(out)
    }
}

/// **One pass of the reviewers' host** — start, collect, kill. See the module docs.
pub fn serve(p: &Pool) -> Result<Served, StoreError> {
    let mut out = Served::default();
    // **The trees no row is on any more, before anything is started** — see [`sweep_clones`].
    sweep_clones(p);
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
            // **A tree that could not be made is a WAIT, not an attempt** — a clone that failed
            // on a busy disk, on a filesystem, or on another daemon's clone of the same row says
            // nothing about the branch. See [`Pool::wait_for_tree`].
            Err(CouldNotStart::Tree(why)) => {
                p.wait_for_tree(r, why)?;
                out.moved = true;
            }
            Err(CouldNotStart::Runner(why)) if capacity::vm_capacity(&why).is_some() => {
                let cap = capacity::vm_capacity(&why).unwrap_or_default();
                p.wait_for_slot(r, &cap)?;
                out.moved = true;
            }
            Err(CouldNotStart::Runner(why)) => {
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
                        &r.repository,
                        "queue_blocked",
                        notices::reviewer_could_not_start(r, &why),
                    );
                }
                out.moved = true;
            }
        }
    }

    // ---- Stop a fixer whose red tip the target has moved past (`RepoQueue` withdraws the fix
    // branch it opened): the same rule a reviewer follows — a child working for a row nobody will
    // act on is stopped, not waited out. `Running` is the whole test, so a fixer that has finished
    // is not asked about again on the next pass.
    for r in rows
        .iter()
        .filter(|r| r.kind == ReviewKind::Target && !r.state.is_open())
    {
        let Some(fixer) = r.fixes.as_deref() else {
            continue;
        };
        let Some(tip) = git::branch_head(Path::new(&r.repository), &r.target) else {
            continue;
        };
        if tip == r.head_sha {
            // Still the red tip it was opened for: the fixer has something to fix.
            continue;
        }
        if matches!(
            p.runner.collect(fixer, std::time::Duration::ZERO),
            TaskStatus::Running { .. }
        ) {
            let _ = p.runner.kill(fixer);
            out.moved = true;
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
                match capacity::vm_capacity(&why) {
                    // **No free slot is a wait, not an attempt** (see `capacity`).
                    Some(cap) => {
                        p.wait_for_slot(&r, &cap)?;
                    }
                    None => {
                        p.attempt_failed(&r, format!("the reviewer `{child}` failed: {why}"))?;
                    }
                }
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

/// **The directory a review's clone lives in**, under the temp dir — one per review, named
/// `review-<row id>` ([`start_reviewer`]).
///
/// A `const` because two places spell it: the daemon builds [`Pool::clones`] from it
/// (`Harness::serve_reviews`), and [`sweep_clones`] refuses to touch any directory not called
/// this — which is the guard that keeps a fixture from pointing a sweep at a real directory.
pub const CLONES_DIR: &str = "letibot-review-clones";

/// **The clause a review waiting for its TREE leads its move (and `failure`) with** — what the
/// queue's claim reads to hold the row until its not-before time, and what [`Pool::tree_waits`]
/// counts. It is a wait of its own and never [`capacity::WAITING_FOR_SLOT`]: a busy disk is not a
/// busy VM, and the two ladders are counted apart.
pub const WAITING_FOR_TREE: &str = "waiting for the reviewer's tree";

/// **How long a tree in the clone directory is left alone whatever the store says** — see
/// [`sweep_clones`]. Ten minutes.
///
/// The number is the answer to one question: *how long can a tree be being written while the row
/// it belongs to does not read as live?* A clone of this repository at a branch head MEASURED
/// **1.2 s** (`git clone --no-local --single-branch`, 52 MB of tree, 226 MiB of `.git`, warm
/// cache, 2026-10-29); on a box with two reviewers building, or with a cold cache, it is tens of
/// seconds. Ten minutes is ~500× the measurement, and it is twice
/// [`crate::mergequeue::serve::REVIEWER_START_GRACE_MS`] — the queue's own answer to *a reviewer
/// should have started by now* — so anything still being written after it is a tree no reviewer
/// will ever be started on. What the grace costs is only how long an orphan lingers: the pass runs
/// every 30 s ([`crate::sessions::REVIEW_PASS_EVERY`]), so an orphan is reclaimed within twenty
/// passes of being certain.
pub const CLONE_GRACE: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// **The per-review trees nobody is on any more** — [`start_reviewer`] makes one clone per review
/// under [`Pool::clones`] (`review-<row id>`), for BOTH placements, and nothing used to remove one:
/// MEASURED on the operator's box 2026-10-29, **eleven clones, 530 MB, the oldest four days old**.
/// The firecode copy that *is* removed (`FirecodeBackend::remove_copy`) is firecode's own project
/// directory, not this clone.
///
/// **Why a sweep and not a teardown at the verdict.** A review ends in three places — a verdict
/// ([`verdict`]), a failed attempt ([`Pool::attempt_failed`]), and a WITHDRAWAL by a person or a
/// deadline, which lives in `mergequeue::serve` — and the two that leak most (a withdrawal, and
/// everything a daemon that died left behind) are exactly the ones a per-point removal in this
/// file could not reach. So the rule is the one Package K's resource sweep uses, one directory
/// over: a clone is removed once its row is no longer `waiting` or `reviewing`. Package K's sweep
/// reclaims terminal rows' WORKTREES in the queue's pass; this reclaims reviewer CLONES in the
/// review pass, because neither can see the other's directory.
///
/// **Every row on the store, not this daemon's** — the store is shared by every daemon on the box
/// and `clones` is one global directory, so a sweep scoped to `p.me` would delete a live clone of
/// another daemon's review.
///
/// # What makes it safe by construction, and why the first cut was not
///
/// MEASURED 2026-10-29: a reviewer's `git clone` died on *"could not open
/// '…/review-s-…/.git/objects/pack/tmp_pack_cgbKfF' for reading: No such file or directory"* — a
/// clone whose directory this sweep had removed WHILE git was writing into it. The row was not
/// live at that instant (a withdrawal, a block, a target that moved), and the tree was half-made.
/// Three rules, and each of them alone would have stopped it:
///
/// * **A tree in flight is not a `review-<id>`.** [`git::clone_branch`] clones beside the name and
///   renames into it, so the only name this sweep can ever see is a COMPLETE clone. The remainder
///   after `review-` is the id only up to its first `.`, so `review-<id>.tmp-…` and
///   `review-<id>.old-…` are read as trees of `<id>` and are never touched by the row rule.
/// * **A tree younger than [`CLONE_GRACE`] is left alone**, whatever the store says — which is the
///   belt under the brace: a row can leave `waiting`/`reviewing` while its clone is still being
///   made (a person's veto, a blocked row, a target that moved), and the store's answer at that
///   instant is not evidence that the tree is nobody's yet.
/// * **Only a DIRECTORY is removed, a name that does not parse is left alone, and a store that
///   will not answer is not evidence that a tree is nobody's** — as before.
///
/// **Four refusals, each of which is a real way this could destroy something:**
/// * a directory that is not named [`CLONES_DIR`] is never swept — that refuses `std::env::temp_dir()`
///   itself (which three fixtures used to pass), a parent of it, and any directory a test or a
///   future caller might be pointed at;
/// * a path whose age cannot be read is kept;
/// * a tree that is not a directory is left alone;
/// * a store that will not answer is not evidence that a tree is nobody's.
fn sweep_clones(p: &Pool) {
    sweep_clones_with(p, CLONE_GRACE)
}

/// [`sweep_clones`] with the grace named — so a test can say *the row rule* and *the grace* apart,
/// which is the only way either of them can be asserted on its own.
fn sweep_clones_with(p: &Pool, grace: std::time::Duration) {
    if p.clones.file_name().is_none_or(|n| n != CLONES_DIR) {
        return;
    }
    let Ok(rows) = p.store.review_rows(None) else {
        return;
    };
    let live: std::collections::HashSet<&str> = rows
        .iter()
        .filter(|r| matches!(r.state, ReviewState::Waiting | ReviewState::Reviewing))
        .map(|r| r.id.as_str())
        .collect();
    let Ok(entries) = std::fs::read_dir(&p.clones) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(rest) = name.strip_prefix("review-") else {
            continue;
        };
        // **The id is what stands before the first dot** — `review-<id>.tmp-<pid>-<n>` is a tree
        // being written and `review-<id>.old-…` one being replaced, and neither is swept by the
        // row rule ([`git::clone_branch`]).
        let id = rest.split('.').next().unwrap_or(rest);
        if id.is_empty() || live.contains(id) {
            continue;
        }
        let path = e.path();
        if !path.is_dir() {
            continue;
        }
        // **Younger than the grace is kept, and an age that cannot be read is kept too**: the
        // store's word about a row is not worth a tree that may still be being written.
        if age_of(&path).is_none_or(|age| age < grace) {
            continue;
        }
        let _ = std::fs::remove_dir_all(&path);
    }
}

/// How long ago `path`'s own entry last changed, or `None` when that cannot be read (an mtime in
/// the future included — a clock that went backwards is not evidence of age).
fn age_of(path: &Path) -> Option<std::time::Duration> {
    let at = std::fs::metadata(path).ok()?.modified().ok()?;
    std::time::SystemTime::now().duration_since(at).ok()
}

/// Start one reviewer: where it runs, on what tree, with which prompt.
///
/// **Both placements get a tree of their own, and it is made here, once** — a detached clone of the
/// entry's repository at the head the row names (`git::clone_branch`, decision 3). What differs is
/// what is done with it: a VM is booted ON the clone (`TaskSpec::vm_source`, so the guest's whole
/// world is the branch), and a host reviewer WORKS IN it (`TaskSpec::worktree`, which is the
/// field that makes a child's workspace something other than its parent's — see
/// `Harness::run_to_completion`). The `cwd` the review prompt names is that same path, so the
/// reviewer's `read`, `grep`, `glob` and `bash` all look at `head_sha`.
///
/// **The host arm used to hand the reviewer `r.repository`** — the operator's own checkout, on
/// whatever branch they had out — and its workspace was the DAEMON's, so a review judged a tree
/// nobody had asked it about. MEASURED as a defect by the operator on 2026-10-29: *"a reviewer that
/// judges the wrong tree is worse than a slow one"*.
///
/// **Two consequences of `spec.worktree` that are deliberate and not defects.** A child with a
/// worktree gets `CARGO_TARGET_DIR` from `worktree_build_env`, which for a clone is the clone's OWN
/// `target/` (its `.git` is its own) — so a reviewer that builds builds hermetically. And it gets a
/// [`deliverable`](crate::harness::deliverable): if the reviewer leaves files in its tree, its
/// completion says so. Nothing can be lost that way — the clone is thrown away — and suppressing
/// the check would need a field on `TaskSpec` that says *a tree the child may not commit into*,
/// which is a change to the spawn and not to this decision.
fn start_reviewer(p: &Pool, r: &ReviewRow) -> Result<(String, ReviewRequest), CouldNotStart> {
    let repo = PathBuf::from(&r.repository);
    let mut spec = TaskSpec {
        role: "gatekeeper".into(),
        placement: p.placement.placement(),
        ..Default::default()
    };
    // **The branch the row is about** — the target itself for a red tip, the child's branch for a
    // review — and the head as the queue read it when it claimed the row.
    let what = if r.kind == ReviewKind::Target {
        r.target.clone()
    } else {
        r.branch.clone()
    };
    let dir = p.clones.join(format!("review-{}", r.id));
    git::clone_branch(&repo, &what, &r.head_sha, &dir).map_err(CouldNotStart::Tree)?;
    let cwd = match p.placement {
        ReviewerPlacement::Vm => {
            // A VM is booted on the clone; inside it there is no absolute path to name.
            spec.vm_source = Some(dir);
            String::new()
        }
        ReviewerPlacement::Host | ReviewerPlacement::NoVm { .. } => {
            // **A host reviewer works in the checkout**, so the child's workspace and the `cwd`
            // its prompt names are one path — the branch at the head it judges, and never the
            // operator's tree.
            let at = dir.display().to_string();
            spec.worktree = Some(WorktreePlacement {
                path: at.clone(),
                branch: what,
                base_sha: r.base_sha.clone(),
                main_tree: false,
            });
            at
        }
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
    let handle = p.runner.start(&prompt, &spec).map_err(CouldNotStart::Runner)?;
    Ok((handle, req))
}

/// **Why a reviewer could not be started** — and the two arms are different KINDS of fact, which
/// is the whole of it: one is a machine that answered, the other is a judgement about the branch.
/// A [`CouldNotStart::Tree`] waits ([`Pool::wait_for_tree`]); a [`CouldNotStart::Runner`] either
/// waits for a free VM slot ([`capacity`]) or is raised (decision 4).
enum CouldNotStart {
    /// The tree could not be MADE — a clone that failed, or a finished clone that could not be put
    /// at its name: a busy disk, a filesystem, another daemon cloning the same row.
    Tree(String),
    /// The runner would not start the child — placement, firecode, the seat.
    Runner(String),
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
                    &r.repository,
                    "queue_needs_human",
                    undecided_sentence(p, r, &by, &said.reasons),
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

/// **A review that stops because its reviewer could not decide says THAT** — the head, the
/// sentence the verdict turned on (the reviewer's own words, chosen, not paraphrased), how many
/// times reviewers could not decide on this branch and at which heads, and what the operator can
/// do with the verbs the queue has. A failure to place or run a reviewer is not mentioned: it is
/// not why this row stopped (the hub row of 2026-10-12 named a placement error for a branch whose
/// history was three `could not decide` verdicts — see `capacity`). The words are
/// [`notices::needs_human`]'s.
fn undecided_sentence(p: &Pool, r: &ReviewRow, by: &str, reasons: &[String]) -> String {
    let heads = p
        .store
        .transitions("review", &r.id)
        .map(|h| capacity::undecided_heads(&h))
        .unwrap_or_default();
    notices::needs_human(r, by, &capacity::decisive_reason(reasons), &heads)
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
            &r.repository,
            "target_red",
            format!(
                "EMERGENCY: {why}. This project's policy is `red_target = \"raise\"`: no fix branch \
                 was opened — the target needs a person's fix."
            ),
        );
        return Ok(());
    }
    // **A fix is for the tip that is red NOW** (the operator, 2026-10-26: four fix branches were
    // opened in one morning for tips `main` had left, and all four ended empty). The reviewer
    // judges the tip it was sent at, and the target may have moved by the time it answers: a
    // branch cut from a tip the target no longer has can never land, so nothing is opened — and
    // the reason is written on the row, because a refusal nobody records is a review that looks
    // like it did nothing.
    if let Some(tip) = git::branch_head(&repo, &r.target)
        && tip != r.head_sha
    {
        let why = format!(
            "no fix branch was opened: `{}` has moved from {} to {tip} since this reviewer was \
             sent at it, so the red it judged is superseded by {tip} and a branch cut from it could \
             never land. The tip at {tip} is gated, and reviewed if it is red too",
            r.target, r.head_sha
        );
        p.mv(
            &ReviewRow {
                state: ReviewState::Rejected,
                ..r.clone()
            },
            ReviewState::Rejected,
            Move::new(p.me, why),
        )?;
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
            &r.repository,
                "target_red",
                format!(
                    "EMERGENCY: {why}. A fix branch `{}` was opened by {} and goes through the \
                     queue; it is the only branch that lands while `{}` is red.",
                    h.placement.branch, h.handle, r.target
                ),
            );
        }
        Err(e) => p.raise(
            &r.repository,
            "target_red",
            format!("EMERGENCY: {why}. A fix branch could NOT be opened: {e}. A person has to fix `{}`.", r.target),
        ),
    }
    Ok(())
}

/// **The refusal goes back to the child that wrote the branch**, or its root when the child is
/// gone; when nobody can be told, the operator is (it stays on the row either way).
fn deliver_refusal(p: &Pool, r: &ReviewRow, reasons: &[String]) {
    let to_child = notices::refused(r, reasons);
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
        let to_root = notices::refused_to_root(r, reasons, &child, &missed.join("; "));
        match (p.deliver)(&root, &to_root) {
            Ok(_) => return,
            Err(e) => missed.push(format!("`{root}`: {e}")),
        }
    }
    p.raise(
        &r.repository,
        "refusal_undelivered",
        notices::refusal_undelivered(r, &missed.join("; ")),
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

#[cfg(test)]
mod capacity_tests;
