//! The review queue's reviewers - the half that runs on the queue's own SESSION: a reviewer
//! is a `task` child of the gatekeeper session.

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

/// What the reviewers' host needs to serve its claimed rows.
pub struct Pool<'a> {
    pub store: &'a Store,
    pub runner: &'a dyn TaskRunner,
    pub desk: &'a ReviewDesk,
    /// The session the reviewers are children of.
    pub host: &'a str,
    /// This daemon's lease identity - the owner the queue's thread claimed the rows as.
    pub me: &'a str,
    pub placement: ReviewerPlacement,
    pub events: &'a dyn Fn(SessionEvent),
    /// A repository's alert, published only where that repository's sessions are
    /// ([`super::publish_for_repository`] in the daemon) - never on every hub.
    pub warn: &'a dyn Fn(&str, SessionEvent),
    /// Send a refusal's complaints to a session (`TaskRunner::send`).
    pub deliver: &'a dyn Fn(&str, &str) -> Result<String, String>,
    /// Whether a session is live in this daemon now - [`super::serve::QueueDoor::session_live`]'s
    /// answer, asked when a refusal has nobody in its own tree to go to and a live session of the
    /// same workspace is looked for ([`live_session_in_workspace`]); nothing else here asks it.
    pub live: &'a dyn Fn(&str) -> bool,
    pub settings: &'a dyn Fn(&Path) -> Result<QueueSettings, String>,
    /// Where a VM reviewer's fresh clone is made.
    pub clones: PathBuf,
    /// The skills every reviewer is started with, from `[queue] reviewer_skills` for the repository
    /// being served. Empty is the ordinary case.
    pub reviewer_skills: &'a [String],
    /// What holds each busy VM slot, one line per tap (`crate::vm_reap::slot_holders`), read when a
    /// capacity wait is raised as long, so the notice says what to free instead of only that all are
    /// busy.
    pub slots: &'a dyn Fn() -> Vec<String>,
}

/// What one pass of [`serve`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Served {
    pub started: usize,
    pub moved: bool,
    /// Refusals nobody heard that this pass handed to a session which had opened since - see
    /// [`redeliver_refusals`].
    pub redelivered: usize,
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
        (self.warn)(repository, SessionEvent::warning(code, detail));
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

    /// Why a review is blocked, in one sentence a person can act on - every counted attempt at
    /// this head, by its own gist (firecode's `error:` line, not its banner), oldest first
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

    /// firecode had no free slot for the reviewer - not an attempt, and not a block: the row goes
    /// back to `waiting` with a not-before time ([`capacity::retry_after_ms`]), says so once in the
    /// routine register, and raises once when the wait is long. See [`capacity`].
    fn wait_for_slot(&self, r: &ReviewRow, cap: &capacity::VmCapacity) -> Result<bool, StoreError> {
        let history = self.store.transitions("review", &r.id)?;
        let streak = capacity::streak(&history);
        // The wait is the branch's, not the row's. A red branch gets a new row per red tip
        // (`red-<branch>-<sha>`), and each row's own streak raised its own two notices for one
        // wait that never ended.
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

    /// The reviewer's tree could not be MADE - the clone failed, or the finished clone could
    /// not be put at its name. Not an attempt, and not a block
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

    /// How many times in a row this row has waited for a tree - newest back to the last other
    /// outcome, counted the way [`capacity::streak`] counts a slot wait: a claim, a reviewer started
    /// or a note on a waiting row does not end the run of them.
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

    /// The capacity streaks of the OTHER rows of `r`'s branch that are still in line (`waiting` or
    /// `reviewing`) - what [`capacity::branch_wait`] reads.
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

/// One pass of the reviewers' host - start, collect, kill. See the module docs.
pub fn serve(p: &Pool) -> Result<Served, StoreError> {
    let mut out = Served::default();
    // The trees no row is on any more, before anything is started - see [`sweep_clones`].
    sweep_clones(p);
    // The other end of a refusal nobody heard - before the rows are read, so a session that
    // opened since the verdict is told in the same pass that starts the next reviewer.
    out.redelivered = redeliver_refusals(p)?;
    out.moved |= out.redelivered > 0;
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
            // A tree that could not be made is a WAIT, not an attempt: a clone that failed on a busy
            // disk, on a filesystem, or on another daemon's clone of the same row says nothing about
            // the branch. See [`Pool::wait_for_tree`].
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
                // A reviewer that cannot start RAISES.
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
    // branch it opened): the same rule a reviewer follows - a child working for a row nobody
    // will act on is stopped, not waited out.
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
            verdict(p, &r, &child, &open.req.cwd, said)?;
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
                    // No free slot is a wait, not an attempt (see `capacity`).
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

/// The directory a review's clone lives in, under the temp dir - one per review, named
/// `review-<row id>` ([`start_reviewer`]). A `const` because two places spell it
pub const CLONES_DIR: &str = "letibot-review-clones";

/// The clause a review waiting for its TREE leads its move (and `failure`) with - what the queue's
/// claim reads to hold the row until its not-before time, and what [`Pool::tree_waits`] counts. It is
/// a wait of its own and never [`capacity::WAITING_FOR_SLOT`]: a busy disk is not a busy VM.
pub const WAITING_FOR_TREE: &str = "waiting for the reviewer's tree";

/// How long a tree in the clone directory is left alone once its row is dead - see
/// [`sweep_clones`].
pub const CLONE_GRACE: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// The row a tree in the clone directory belongs to, or `None` for a name that is not a
/// review's at all - the three spellings [`git::clone_branch`] and its history write *
/// `review-<id>-<pid>-<n>` - the tree ONE ATTEMPT works in (this build) *
fn review_id(name: &str) -> Option<&str> {
    let name = name.strip_suffix(".tmp").unwrap_or(name);
    let name = name.strip_prefix('.').unwrap_or(name);
    let rest = name.strip_prefix("review-")?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let mut fields = rest.rsplitn(3, '-');
    let n = fields.next().unwrap_or_default();
    let pid = fields.next().unwrap_or_default();
    match fields.next() {
        Some(id) if !id.is_empty() && digits(pid) && digits(n) => Some(id),
        _ => Some(rest),
    }
}

/// The trees no reviewer can be in.
fn sweep_clones(p: &Pool) {
    sweep_clones_with(p, CLONE_GRACE)
}

/// [`sweep_clones`] with the grace named - so a test can say *the row rule* and *the grace* apart
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
        let Some(id) = review_id(&name) else {
            continue;
        };
        if live.contains(id) {
            continue;
        }
        let path = e.path();
        if !path.is_dir() {
            continue;
        }
        // Younger than the grace is kept, and an age that cannot be read is kept too: the store's
        // word about a row is not worth a tree that may still be being written.
        if age_of(&path).is_none_or(|age| age < grace) {
            continue;
        }
        let _ = std::fs::remove_dir_all(&path);
    }
}

/// How long ago `path`'s own entry last changed, or `None` when that cannot be read (an mtime in
/// the future included - a clock that went backwards is not evidence of age).
fn age_of(path: &Path) -> Option<std::time::Duration> {
    let at = std::fs::metadata(path).ok()?.modified().ok()?;
    std::time::SystemTime::now().duration_since(at).ok()
}

/// Start one reviewer: where it runs, on what tree, with which prompt. Both placements get
/// a tree of their own, made here once - a detached clone of the entry's repository at the
/// head the row names. What differs is what is done with it
fn start_reviewer(p: &Pool, r: &ReviewRow) -> Result<(String, ReviewRequest), CouldNotStart> {
    let repo = PathBuf::from(&r.repository);
    let mut spec = TaskSpec {
        role: "gatekeeper".into(),
        placement: p.placement.placement(),
        // The reviewer's declared skills, from `[queue] reviewer_skills` - the one seat nobody
        // types a brief for, so its instructions are configured rather than asked for.
        skills: p.reviewer_skills.to_vec(),
        ..Default::default()
    };
    // The branch the row is about - the target itself for a red tip, the child's branch for a review
    // - and the head as the queue read it when it claimed the row.
    let what = if r.kind == ReviewKind::Target {
        r.target.clone()
    } else {
        r.branch.clone()
    };
    let stem = p.clones.join(format!("review-{}", r.id));
    let dir = git::clone_branch(&repo, &what, &r.head_sha, &stem).map_err(CouldNotStart::Tree)?;
    // The tree is proved before a child is given it.
    let at = match git::rev_parse(&dir, "HEAD") {
        Some(sha) if sha == r.head_sha => sha,
        other => {
            return Err(CouldNotStart::Tree(format!(
                "the tree `{}` is {}, and the row is about {}",
                dir.display(),
                match other {
                    Some(sha) => format!("at {sha}"),
                    None => "not a checkout git can name".to_string(),
                },
                r.head_sha
            )));
        }
    };
    debug_assert_eq!(at, r.head_sha);
    let cwd = match p.placement {
        ReviewerPlacement::Vm => {
            // A VM is booted on the clone; inside it there is no absolute path to name.
            spec.vm_source = Some(dir);
            String::new()
        }
        ReviewerPlacement::Host | ReviewerPlacement::NoVm { .. } => {
            // A host reviewer works in the checkout, so the child's workspace and the `cwd` its
            // prompt names are one path - the branch at the head it judges, never the operator's tree.
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
    let handle = p
        .runner
        .start(&prompt, &spec)
        .map_err(CouldNotStart::Runner)?;
    Ok((handle, req))
}

/// Why a reviewer could not be started - and the two arms are different KINDS of fact
/// which is the whole of it: one is a machine that answered, the other a judgement about
/// the branch. A [`CouldNotStart::Tree`] waits ([`Pool::wait_for_tree`])
enum CouldNotStart {
    /// The tree could not be MADE - a clone that failed, or a finished clone that could not be put
    /// at its name: a busy disk, a filesystem, another daemon cloning the same row.
    Tree(String),
    /// The runner would not start the child - placement, firecode, the seat.
    Runner(String),
}

/// The red target's reviewer is told what its verdict MEANS - the tip is broken, so
/// `reject` is *the code is broken*, which opens a fix branch through the queue when the
/// project's policy is `fix` `accept` is *the red is not in the code* (a flake, the
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

/// A verdict handed in - moved onto the row conditionally, and acted on. `req_cwd` is the
/// tree the child was GIVEN (the desk's own record of what it was asked with), and it is
/// read before the verdict is: see the check below.
fn verdict(
    p: &Pool,
    r: &ReviewRow,
    child: &str,
    req_cwd: &str,
    said: letibot_tools::gatekeeper::Submission,
) -> Result<(), StoreError> {
    // A verdict about a tree that is not there is not a verdict about the branch. A reviewer
    // whose clone was removed under it reads a fraction of the artifact, knows it, and answers
    // `cannot_decide`
    let tree = Path::new(&req_cwd);
    let there = tree.is_dir()
        && std::fs::read_dir(tree)
            .map(|mut d| d.next().is_some())
            .unwrap_or(false);
    if !there {
        let why = format!(
            "the review environment went away under the reviewer: the tree it was given (`{}`) is \
             {} and the row is about {}. Nothing about the branch was judged — this is our \
             machinery, not the code — so the review is tried again in a fresh tree",
            tree.display(),
            if tree.is_dir() {
                "empty".to_string()
            } else {
                "gone".to_string()
            },
            r.head_sha
        );
        p.wait_for_tree(r, why)?;
        return Ok(());
    }
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

/// A review that stops because its reviewer could not decide says THAT - the head, the
/// sentence the verdict turned on (the reviewer's own words, chosen, not paraphrased), how
/// many times reviewers could not decide on this branch and at which heads, and what the
fn undecided_sentence(p: &Pool, r: &ReviewRow, by: &str, reasons: &[String]) -> String {
    let heads = p
        .store
        .transitions("review", &r.id)
        .map(|h| capacity::undecided_heads(&h))
        .unwrap_or_default();
    notices::needs_human(r, by, &capacity::decisive_reason(reasons), &heads)
}

/// The reviewer of the red tip opens a fix branch, through the queue like any other, and the operator
/// is told - or, under a project's `red_target = "raise"`, only told.
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
    // A fix is for the tip that is red NOW: a branch cut from a tip the target no longer has can
    // never land, so nothing is opened when the target has moved - and the reason is written on the
    // row, because a refusal nobody records is a review that looks like it did nothing.
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

/// The word a refusal nobody heard is written down under, in the store's `queue_told` table: the row
/// whose complaints are owed to the first session that opens later. The head it judged is in the key,
/// so a re-review of a new head is a new debt.
pub const REFUSAL_OWED: &str = "refusal:";

/// How long a refusal nobody heard is owed. A branch rejected a week ago and never touched again is
/// the row's evidence, not a message: the door looks for a session still in the story.
pub const REFUSAL_OWED_MS: u64 = 24 * 60 * 60 * 1000;

fn owed_key(r: &ReviewRow) -> String {
    format!("{REFUSAL_OWED}{}:{}", r.head_sha, r.id)
}

/// The refusal goes back to the child that wrote the branch, or its root when the child is gone, then
/// to a live session of the branch's workspace; when nobody can be told it is written down, and
/// [`redeliver_refusals`] hands it to whoever opens next.
fn deliver_refusal(p: &Pool, r: &ReviewRow, reasons: &[String]) {
    let Err(tried) = hand_refusal(p, r, reasons) else {
        return;
    };
    // A refusal is the child's news, and a child whose daemon is gone comes back. The write is
    // the fact a later pass reads; `tell_once` is also what makes the operator's warning one row
    // per undelivered head rather than one per pass.
    let first = p.store.tell_once(&owed_key(r), now_ms()).unwrap_or(true);
    if first {
        p.raise(
            &r.repository,
            "refusal_undelivered",
            notices::refusal_undelivered(r, &tried.join("; ")),
        );
    }
}

/// Hand the refusal to the first session that will take it, and answer the session that did - or
/// every party that did not, in its own words, which is the evidence *told nobody* has to carry.
fn hand_refusal(p: &Pool, r: &ReviewRow, reasons: &[String]) -> Result<String, Vec<String>> {
    let to_child = notices::refused(r, reasons);
    let child = r.session_id.clone();
    let mut tried: Vec<String> = Vec::new();
    if !child.is_empty() {
        match (p.deliver)(&child, &to_child) {
            Ok(_) => return Ok(child),
            Err(e) => tried.push(format!("`{child}`: {e}")),
        }
    }
    let root = super::root_session(p.store, &child);
    if !root.is_empty() && root != child {
        let to_root = notices::refused_to_root(r, reasons, &child, &tried.join("; "));
        match (p.deliver)(&root, &to_root) {
            Ok(_) => return Ok(root),
            Err(e) => tried.push(format!("`{root}`: {e}")),
        }
    }
    match live_session_in_workspace(p, r, &child, &root) {
        Some((to, workspace)) => {
            let to_them =
                notices::refused_to_workspace(r, reasons, &child, &workspace, &tried.join("; "));
            match (p.deliver)(&to, &to_them) {
                Ok(_) => return Ok(to),
                Err(e) => tried.push(format!("`{to}`: {e}")),
            }
        }
        None => tried.push(format!(
            "no live top-level session of `{}` is open in this daemon",
            r.repository
        )),
    }
    Err(tried)
}

/// A refusal nobody heard is owed to whoever opens next: `deliver_refusal` wrote the debt down at
/// the verdict, and this is the other end, a pass that hands it to a session opened since. A row that
/// is no longer the verdict it was rejected with is owed nothing, and neither is a day-old one.
pub fn redeliver_refusals(p: &Pool) -> Result<usize, StoreError> {
    let mut handed = 0;
    let now = now_ms();
    for (key, told) in p.store.told_with_prefix(REFUSAL_OWED)? {
        let Some((head, id)) = key.trim_start_matches(REFUSAL_OWED).split_once(':') else {
            // Not a key this function wrote, and not one it can pay: forgotten, so a malformed
            // key cannot live here for ever.
            p.store.untell_prefix(&key)?;
            continue;
        };
        let Some(r) = p.store.review_row(id)? else {
            // A row the store does not have cannot be owed anything.
            p.store.untell_prefix(&key)?;
            continue;
        };
        let stands = r.state == ReviewState::Rejected && r.head_sha == head;
        if !stands || now.saturating_sub(told.at_ms) > REFUSAL_OWED_MS {
            p.store.untell_prefix(&key)?;
            continue;
        }
        if hand_refusal(p, &r, &r.reasons).is_ok() {
            p.store.untell_prefix(&key)?;
            handed += 1;
        }
    }
    Ok(handed)
}

/// The live session of the branch's workspace a refusal falls back to - the newest
/// top-level session of the repository `r` names, that this daemon holds, with its
/// workspace.
fn live_session_in_workspace(
    p: &Pool,
    r: &ReviewRow,
    child: &str,
    root: &str,
) -> Option<(String, String)> {
    let mut best: Option<(String, String, i64)> = None;
    for s in p.store.list_sessions().unwrap_or_default() {
        if s.parent_session_id.is_some_and(|parent| !parent.is_empty()) {
            continue;
        }
        if s.id == child || s.id == root || letibot_sessionlog::is_gatekeeper_session(&s.id) {
            continue;
        }
        // A row with no workspace cannot be said to be in the branch's.
        if s.workspace_root.is_empty() || !(p.live)(&s.id) {
            continue;
        }
        let in_workspace = super::repositories_of_workspace_cached(Path::new(&s.workspace_root))
            .iter()
            .any(|repo| repo == &r.repository);
        if !in_workspace {
            continue;
        }
        // Newest, by the store's own reading of a session's last activity - the same order the picker
        // lists them in.
        if best
            .as_ref()
            .is_none_or(|(_, _, ms)| s.last_activity_ms > *ms)
        {
            best = Some((s.id.clone(), s.workspace_root.clone(), s.last_activity_ms));
        }
    }
    best.map(|(id, workspace, _)| (id, workspace))
}

/// What an enqueue did: the row, and whether it is news - born, or put back in line by a head
/// nobody has judged. A row that was already in line answers `false`, so a second finish of one
/// child says nothing twice.
pub struct Enqueued {
    /// The row, or `None` for a child that left no branch of its own to land.
    pub row: Option<ReviewRow>,
    /// The row is new, or it was withdrawn and the branch has since moved past that head.
    pub news: bool,
}

/// A finished `task_start` child's branch, put up for review - the enqueue. The repository
/// is the worktree's own, the target is the project's, and the head is read now.
pub fn enqueue_finished(
    store: &Store,
    handle: &str,
    brief: &str,
    placement: &letibot_tools::builtins::task::WorktreePlacement,
    settings: &dyn Fn(&Path) -> Result<QueueSettings, String>,
    now: u64,
) -> Result<Enqueued, String> {
    if placement.main_tree {
        return Ok(Enqueued {
            row: None,
            news: false,
        });
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
    let head = match git::branch_head(&repo, &placement.branch) {
        Some(head) => head,
        None => {
            return Err(format!(
                "the branch `{}` is not in {} — `git rev-parse refs/heads/{}` answered nothing, \
                 so there is no head to review and nothing to land",
                placement.branch,
                repo.display(),
                placement.branch
            ));
        }
    };
    if !placement.base_sha.is_empty() && head == placement.base_sha {
        return Err(format!(
            "the branch `{}` has no commit of its own: its head is `{head}`, the base it was cut \
             from, so there is no change to review and nothing to land. A child placed in a VM \
             commits inside the VM's copy of the repository and this branch stays at its base; a \
             child whose work is not committed leaves the same branch. Commit on this host (or \
             re-run the child here) and enqueue it again",
            placement.branch
        ));
    }
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
    let born = store
        .enqueue_review(&new, handle, now)
        .map_err(|e| format!("the review row for `{handle}` could not be written: {e}"))?;
    let Some(row) = store.review_row(handle).map_err(|e| e.to_string())? else {
        return Err(format!(
            "the review row for `{handle}` was written and read back as nothing"
        ));
    };
    // A row already there is not rewritten by the insert, and a WITHDRAWN one records a branch
    // that had nothing on it: a later commit is a head no reviewer has judged, and the row is
    // terminal, so this is the only door that can put the work back in line.
    if row.state == ReviewState::Withdrawn && row.head_sha != new.head_sha {
        let why = format!(
            "`{}` moved to {} after the row was withdrawn ({}), so the new head is put up for \
             review",
            new.branch, new.head_sha, row.evidence
        );
        store
            .move_review(
                handle,
                ReviewState::Withdrawn,
                ReviewState::Waiting,
                &Move::new("enqueue", why)
                    .set("head_sha", new.head_sha.clone())
                    .clear("verdict")
                    .clear("verdict_by")
                    .clear("reviewed_sha")
                    .set("attempts", 0u32),
                now,
            )
            .map_err(|e| {
                format!("the review row for `{handle}` could not be put back in line: {e}")
            })?;
        let row = store.review_row(handle).map_err(|e| e.to_string())?;
        return Ok(Enqueued { row, news: true });
    }
    Ok(Enqueued {
        row: Some(row),
        news: born,
    })
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod capacity_tests;
