//! Why each queue entry is where it is, every lease and what it guards, and the next thing that
//! will happen. A session reads this in `queue status` instead of asking a model.

use std::collections::BTreeMap;
use std::path::Path;

use letibot_tokencore::store::{
    HealthState, MergeRow, MergeState, QueueLease, ReviewKind, ReviewRow, ReviewState, Store,
    TargetHealth,
};

use super::desk::{dur_human, parked, parked_merge, short_sha};
use super::gate::now_ms;
use super::lease::LEASE_TTL_MS;
use super::{capacity, git, serve};

/// Everything the explanation reads, taken once so no line contradicts another.
pub struct Facts {
    pub reviews: Vec<ReviewRow>,
    pub merges: Vec<MergeRow>,
    pub healths: Vec<TargetHealth>,
    pub leases: Vec<QueueLease>,
    /// The current tip of each target branch, by name.
    pub tips: BTreeMap<String, String>,
    pub now: u64,
}

impl Facts {
    pub fn read(store: &Store, repo: &str) -> Result<Facts, String> {
        let err = |e: letibot_tokencore::store::StoreError| e.to_string();
        let reviews: Vec<ReviewRow> = store
            .review_rows(Some(repo))
            .map_err(err)?
            .into_iter()
            .collect();
        let merges: Vec<MergeRow> = store
            .merge_rows(None)
            .map_err(err)?
            .into_iter()
            .filter(|m| m.repository == repo)
            .collect();
        let healths: Vec<TargetHealth> = store
            .target_healths()
            .map_err(err)?
            .into_iter()
            .filter(|h| h.repository == repo)
            .collect();
        let leases = store
            .queue_leases()
            .unwrap_or_default()
            .into_iter()
            .filter(|l| l.repository == repo)
            .collect();
        let mut tips = BTreeMap::new();
        let targets = healths
            .iter()
            .map(|h| h.target.clone())
            .chain(reviews.iter().map(|r| r.target.clone()))
            .chain(merges.iter().map(|m| m.target.clone()));
        for t in targets {
            if let Some(sha) = git::branch_head(Path::new(repo), &t) {
                tips.insert(t, sha);
            }
        }
        Ok(Facts {
            reviews,
            merges,
            healths,
            leases,
            tips,
            now: now_ms(),
        })
    }

    fn health(&self, target: &str) -> Option<&TargetHealth> {
        self.healths.iter().find(|h| h.target == target)
    }

    fn served(&self) -> bool {
        self.leases
            .iter()
            .any(|l| self.now.saturating_sub(l.heartbeat_ms) < LEASE_TTL_MS)
    }
}

/// The whole block: entries with their reasons, leases, and what happens next.
pub fn explain(f: &Facts, cap: Option<usize>) -> String {
    let mut out = String::from("why each entry is where it is\n");
    let mut lines: Vec<String> = Vec::new();
    for r in f.reviews.iter().filter(|r| parked(r.state)) {
        lines.push(format!(
            "  review `{}` ({}): {}\n",
            r.id,
            r.state.as_str(),
            review_why(f, r)
        ));
    }
    for m in f.merges.iter().filter(|m| parked_merge(m.state)) {
        lines.push(format!(
            "  landing `{}` ({}): {}\n",
            m.id,
            m.state.as_str(),
            merge_why(f, m)
        ));
    }
    if lines.is_empty() {
        out.push_str("  no entry is open or parked\n");
    }
    let shown = cap.unwrap_or(lines.len()).min(lines.len());
    for l in &lines[..shown] {
        out.push_str(l);
    }
    if shown < lines.len() {
        out.push_str(&format!(
            "  +{} more - `queue` `status` with `repository: \".\"` explains every entry\n",
            lines.len() - shown
        ));
    }
    out.push_str(&leases(f));
    out.push_str(&next(f));
    out
}

fn since(f: &Facts, ms: u64) -> String {
    format!("{} ago", dur_human(f.now.saturating_sub(ms)))
}

fn until(f: &Facts, ms: u64) -> String {
    if ms > f.now {
        format!("in {}", dur_human(ms - f.now))
    } else {
        format!("{} ago (past)", dur_human(f.now - ms))
    }
}

fn review_why(f: &Facts, r: &ReviewRow) -> String {
    let target = &r.target;
    match r.state {
        ReviewState::Waiting => {
            let health = f.health(target);
            if r.kind == ReviewKind::Branch
                && r.fixes.is_none()
                && health.is_some_and(|h| h.state == HealthState::Red)
            {
                return format!(
                    "held: `{target}` is red and a red target halts its review queue. It is \
                     reviewed once `{target}` is green, or after a fix branch lands."
                );
            }
            if capacity::held_until(&r.failure, r.deadline_ms, f.now) {
                return format!(
                    "no reviewer slot is free: {}. Retried {}.",
                    capacity::gist(&r.failure),
                    until(f, r.deadline_ms.unwrap_or(f.now))
                );
            }
            if !f.served() {
                return "nobody serves this repository (no live queue lease), so nothing claims \
                        it until a daemon in it starts."
                    .into();
            }
            let running = f
                .reviews
                .iter()
                .filter(|x| x.state == ReviewState::Reviewing)
                .count();
            let ahead = f
                .reviews
                .iter()
                .filter(|x| x.state == ReviewState::Waiting && x.id != r.id)
                .filter(|x| (x.kind != ReviewKind::Target, x.created_ms) < (r.kind != ReviewKind::Target, r.created_ms))
                .count();
            let tried = if r.attempts > 0 {
                format!(
                    " Attempt {} of {} is spent: {}.",
                    r.attempts,
                    serve::MAX_REVIEW_ATTEMPTS,
                    capacity::gist(&r.failure)
                )
            } else {
                String::new()
            };
            format!(
                "waiting for a reviewer since {}. {running} review(s) running, {ahead} waiting \
                 ahead; the queue claims it on a pass once a slot is free.{tried}",
                since(f, r.state_ms)
            )
        }
        ReviewState::Reviewing => format!(
            "leased to {} since {}, until {} (the reviewer's deadline). Reviewer: {}. Attempt {} \
             of {}. The reviewer's own verdict (`review_verdict`) is what ends it.",
            r.owner,
            since(f, r.state_ms),
            r.deadline_ms
                .map(|d| until(f, d))
                .unwrap_or_else(|| "no deadline".into()),
            r.reviewer.as_deref().unwrap_or("not started yet"),
            r.attempts + 1,
            serve::MAX_REVIEW_ATTEMPTS
        ),
        ReviewState::Blocked => format!(
            "the review could not run: {}. `/queue restart {}` (operator) puts it back in line.",
            capacity::gist(if r.failure.is_empty() { &r.evidence } else { &r.failure }),
            r.id
        ),
        ReviewState::Rejected => format!(
            "refused{}: {}. It returns to the branch's author, and re-enters review when the \
             branch moves past {} or the operator restarts it.",
            by(r),
            r.reasons.first().map(String::as_str).unwrap_or("no reason recorded"),
            short_sha(r.reviewed_sha.as_deref().unwrap_or(&r.head_sha))
        ),
        ReviewState::CannotDecide => format!(
            "the reviewer asked for a person{}. The operator decides: `/queue approve {}` or \
             `/queue veto {} WHY`.",
            by(r),
            r.id,
            r.id
        ),
        other => other.as_str().to_string(),
    }
}

fn by(r: &ReviewRow) -> String {
    r.verdict_by
        .as_deref()
        .filter(|b| !b.is_empty())
        .map(|b| format!(" by {b}"))
        .unwrap_or_default()
}

fn merge_why(f: &Facts, m: &MergeRow) -> String {
    match m.state {
        MergeState::Queued => {
            let tip = f.tips.get(&m.target);
            let held = match f.health(&m.target) {
                None => Some(format!(
                    "held for gating: `{}` has no gate verdict yet.",
                    m.target
                )),
                Some(h) if h.state == HealthState::Red && m.fixes.is_none() => Some(format!(
                    "held for gating: `{}` is red at {}; only a fix for it lands.",
                    m.target,
                    short_sha(&h.sha)
                )),
                Some(h) if h.state == HealthState::Gating => Some(format!(
                    "held for gating: the tip of `{}` is being gated ({}).",
                    m.target,
                    h.evidence.lines().next().unwrap_or_default()
                )),
                Some(h) if matches!(h.state, HealthState::Blocked | HealthState::TimedOut) => {
                    Some(format!(
                        "held for gating: `{}` could not be judged ({}): {}",
                        m.target,
                        h.state.as_str(),
                        h.evidence.lines().next().unwrap_or_default()
                    ))
                }
                Some(h) if h.state == HealthState::Green && tip.is_some_and(|t| *t != h.sha) => {
                    Some(format!(
                        "held for gating: `{}` moved past the gated tip {}; a regate of the new \
                         tip is pending.",
                        m.target,
                        short_sha(&h.sha)
                    ))
                }
                _ => None,
            };
            if let Some(why) = held {
                return why;
            }
            if !f.served() {
                return "nobody serves this repository (no live queue lease), so it is not \
                        taken until a daemon in it starts."
                    .into();
            }
            let ahead = f
                .merges
                .iter()
                .filter(|x| x.state == MergeState::Queued && x.target == m.target && x.id != m.id)
                .filter(|x| (x.fixes.is_none(), x.created_ms) < (m.fixes.is_none(), m.created_ms))
                .count();
            if ahead == 0 {
                format!(
                    "next to land: queued {}, the queue rebases it onto `{}`, gates it and \
                     lands it on its next pass.",
                    since(f, m.state_ms),
                    m.target
                )
            } else {
                format!("{ahead} landing(s) ahead of it on `{}`; one gate runs at a time.", m.target)
            }
        }
        MergeState::Gating => format!(
            "leased to {} since {}: rebasing onto `{}` and running the gate. Green lands and \
             pushes it, red fails it.",
            if m.owner.is_empty() { "the queue" } else { &m.owner },
            since(f, m.state_ms),
            m.target
        ),
        MergeState::Failed => format!(
            "the gate failed on the rebased branch: {}. It goes back to its author; new commits \
             and a new review put it back.",
            m.evidence.lines().next().unwrap_or_default()
        ),
        MergeState::TimedOut => {
            "the gate ran past its deadline and was killed. `/queue restart` (operator) or a \
             `regate` once the cause is gone."
                .into()
        }
        MergeState::Conflict => format!(
            "the rebase onto `{}` conflicts. The author rebases the branch and it is reviewed \
             again; the queue never resolves conflicts.",
            m.target
        ),
        MergeState::Blocked => format!(
            "the queue could not run the landing: {}. Nothing about the branch is judged.",
            m.evidence.lines().next().unwrap_or_default()
        ),
        MergeState::LandedLocal => {
            "landed on the target locally; the push failed, so a person pushes it.".into()
        }
        other => other.as_str().to_string(),
    }
}

fn leases(f: &Facts) -> String {
    let mut out = String::from("leases\n");
    if f.leases.is_empty() {
        out.push_str(
            "  repository: no queue lease - no daemon serves this repository, so nothing is \
             claimed, gated or landed\n",
        );
    }
    for l in &f.leases {
        let expiry = l.heartbeat_ms + LEASE_TTL_MS;
        out.push_str(&format!(
            "  repository: held by {} (pid {}, {}) since {}, last beat {}, expires {} unless it \
             beats again. Guards: claiming reviews, the one gate at a time and landings for this \
             repository.{}\n",
            l.owner,
            l.pid,
            l.host,
            since(f, l.acquired_ms),
            since(f, l.heartbeat_ms),
            until(f, expiry),
            if f.now >= expiry { " STALE: another daemon may take it." } else { "" }
        ));
    }
    for r in f.reviews.iter().filter(|r| r.state == ReviewState::Reviewing) {
        out.push_str(&format!(
            "  review claim `{}`: {} since {}, until {}; guards this entry from a second reviewer\n",
            r.id,
            r.owner,
            since(f, r.state_ms),
            r.deadline_ms.map(|d| until(f, d)).unwrap_or_else(|| "no deadline".into())
        ));
    }
    for m in f.merges.iter().filter(|m| m.state == MergeState::Gating) {
        out.push_str(&format!(
            "  landing claim `{}`: {} since {}; guards the repository's one gate\n",
            m.id,
            if m.owner.is_empty() { "the queue" } else { &m.owner },
            since(f, m.state_ms)
        ));
    }
    out
}

fn next(f: &Facts) -> String {
    let health: Vec<String> = f
        .healths
        .iter()
        .map(|h| {
            format!(
                "`{}` {} at {} ({})",
                h.target,
                h.state.as_str(),
                short_sha(&h.sha),
                since(f, h.checked_ms)
            )
        })
        .collect();
    let mut out = format!(
        "target health: {}\n",
        if health.is_empty() { "never gated".into() } else { health.join("; ") }
    );
    let now = if !f.served() && (f.reviews.iter().any(|r| parked(r.state) && r.state.is_open())
        || f.merges.iter().any(|m| m.state.is_open()))
    {
        "nothing: no daemon holds the lease. Starting `letibot` in this repository takes it; a \
         stale lease is taken over after 90 s."
            .to_string()
    } else if let Some(m) = f.merges.iter().find(|m| m.state == MergeState::Gating) {
        format!(
            "the gate of `{}` finishes; green lands it on `{}`, red fails it.",
            m.id, m.target
        )
    } else if let Some(h) = f.healths.iter().find(|h| h.state == HealthState::Gating) {
        format!(
            "the gate of `{}`'s tip finishes; landings resume if it is green.",
            h.target
        )
    } else if let Some(h) = f.healths.iter().find(|h| h.state == HealthState::Red) {
        format!(
            "nothing lands on `{}` until a fix does. It needs a fix branch reviewed and landed, \
             or `queue` `regate` once the cause is gone.",
            h.target
        )
    } else if let Some(m) = f
        .merges
        .iter()
        .filter(|m| m.state == MergeState::Queued)
        .min_by_key(|m| (m.fixes.is_none(), m.created_ms))
    {
        format!("`{}` is gated and landed on `{}` next pass, if its target is green.", m.id, m.target)
    } else if let Some(r) = f
        .reviews
        .iter()
        .filter(|r| r.state == ReviewState::Waiting)
        .min_by_key(|r| (r.kind != ReviewKind::Target, r.created_ms))
    {
        format!("`{}` is claimed for review as soon as a reviewer slot is free.", r.id)
    } else if f.reviews.iter().any(|r| r.state == ReviewState::Reviewing) {
        "the reviewers' verdicts arrive; approval hands the entry to the landing queue.".into()
    } else if f.reviews.iter().any(|r| matches!(r.state, ReviewState::Rejected | ReviewState::CannotDecide | ReviewState::Blocked)) {
        "nothing runs by itself: the parked entries wait for their author or the operator.".into()
    } else {
        "nothing: the queue is idle. A new branch enters with `queue` `enqueue`.".into()
    };
    out.push_str(&format!("next: {now}\n"));
    out
}

/// The explanation for `repo`, read from the store now.
pub fn read(store: &Store, repo: &str, cap: Option<usize>) -> Result<String, String> {
    Ok(explain(&Facts::read(store, repo)?, cap))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mergequeue::serve::tests::TempDir;
    use letibot_tokencore::store::{
        Approval, LeaseClaim, MergePriority, Move, NewReview,
    };

    fn review(store: &Store, id: &str, state: ReviewState) {
        store
            .enqueue_review(
                &NewReview {
                    id: id.into(),
                    repository: "/r".into(),
                    target: "main".into(),
                    kind: ReviewKind::Branch,
                    session_id: "s-x".into(),
                    branch: format!("agent/{id}"),
                    worktree: None,
                    base_sha: "base".into(),
                    head_sha: "head".into(),
                    brief: "a branch".into(),
                    priority: MergePriority::Subagent,
                    fixes: None,
                    evidence: "put up".into(),
                },
                "test",
                1,
            )
            .unwrap();
        if state != ReviewState::Waiting {
            store
                .move_review(id, ReviewState::Waiting, ReviewState::Reviewing, &Move::new("me", "claimed").set("owner", "box/1/2"), 2)
                .unwrap();
        }
    }

    #[test]
    fn status_explains_a_lease_a_held_landing_and_a_waiting_review() {
        let dir = TempDir::new("explain");
        let store = Store::open(&dir.0.join("sessions.db")).unwrap();
        review(&store, "waits", ReviewState::Waiting);
        review(&store, "lands", ReviewState::Reviewing);
        store
            .approve_review(
                "lands",
                &Approval {
                    expect: ReviewState::Reviewing,
                    expect_owner: None,
                    reviewed_sha: "head".into(),
                    by: "reviewer:one".into(),
                    reasons: vec![],
                    files: vec![],
                    commands: vec![],
                    why: "ok".into(),
                    merge_id: "m-lands".into(),
                },
                3,
            )
            .unwrap();
        store
            .set_target_health(
                &TargetHealth {
                    repository: "/r".into(),
                    target: "main".into(),
                    sha: "ec3cb01e8a6f4d2c".into(),
                    state: HealthState::Red,
                    checked_ms: 4,
                    evidence: "gate red".into(),
                    gate_steps: vec![],
                },
                "test",
            )
            .unwrap();
        let me = LeaseClaim {
            owner: "box/9/9".into(),
            pid: 9,
            host: "box".into(),
            build: "b".into(),
        };
        store
            .acquire_queue_lease("/r", &me, super::now_ms(), LEASE_TTL_MS)
            .unwrap();

        let mut f = Facts::read(&store, "/r").unwrap();
        f.tips.insert("main".into(), "ec3cb01e8a6f4d2c".into());
        let said = explain(&f, None);
        assert!(
            said.contains("held for gating: `main` is red") && said.contains("only a fix"),
            "{said}"
        );
        assert!(
            said.contains("held: `main` is red and a red target halts its review queue"),
            "{said}"
        );
        assert!(
            said.contains("repository: held by box/9/9") && said.contains("expires in"),
            "{said}"
        );
        assert!(said.contains("target health: `main` red"), "{said}");
        assert!(said.contains("next: nothing lands on `main` until a fix does"), "{said}");
    }
}
