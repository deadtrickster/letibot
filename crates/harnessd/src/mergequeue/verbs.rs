//! **The operator's verbs on the two queues** — `/queue approve|restart|veto|rm|reset|clean|regate`.
//!
//! Every verb is a conditional move on the row's CURRENT state (read, then moved only if still
//! there), recorded in the history under the operator's session. Two rulings shape them:
//!
//! * **Decision 5**: *"the operator may approve over the reviewer (`/queue approve` on a rejected
//!   or unreviewed entry), recorded as the operator's approval; it still runs the merge gate."* An
//!   approve binds the head the branch has NOW, and hands it to the merge queue in the same
//!   transaction; nothing here touches git or the gate.
//! * **Nothing is deleted.** `rm` and `clean` WITHDRAW rows (a terminal state, with the person's
//!   words), because the history of a row is the record R61 exists to keep; the pane shows open
//!   rows and the recent past, not every row ever.

use std::path::Path;

use letibot_sessionlog::SessionEvent;

use crate::slash::QueueScope;
use letibot_tokencore::store::{
    Approval, HealthState, MergeRow, MergeState, Move, ReviewKind, ReviewRow, ReviewState, Store,
    TargetHealth,
};

use super::gate::now_ms;
use super::git;

/// A row either queue holds, found by its id or a unique prefix of it.
enum Found {
    Review(ReviewRow),
    Merge(MergeRow),
}

fn find(store: &Store, id: &str) -> Result<Found, String> {
    let id = id.trim();
    if id.is_empty() {
        return Err("name a row: `/queue VERB ID` (the id the queue pane shows)".into());
    }
    let reviews = store.review_rows(None).map_err(|e| e.to_string())?;
    let merges = store.merge_rows(None).map_err(|e| e.to_string())?;
    if let Some(r) = reviews.iter().find(|r| r.id == id) {
        return Ok(Found::Review(r.clone()));
    }
    if let Some(m) = merges.iter().find(|m| m.id == id) {
        return Ok(Found::Merge(m.clone()));
    }
    let mut hits: Vec<Found> = reviews
        .into_iter()
        .filter(|r| r.id.starts_with(id))
        .map(Found::Review)
        .chain(
            merges
                .into_iter()
                .filter(|m| m.id.starts_with(id))
                .map(Found::Merge),
        )
        .collect();
    match hits.len() {
        1 => Ok(hits.remove(0)),
        0 => Err(format!("no row of either queue is called `{id}`")),
        n => Err(format!("`{id}` names {n} rows; give more of the id")),
    }
}

/// **The repository a `/queue` verb typed in a session at `workspace` may act in** (R92 item 5)
/// — `None` is every repository. [`QueueScope::Here`] is the session's own (the queues' key for
/// its workspace), and a session of no repository is told so with the lines that reach one.
pub fn scope_repository(workspace: &Path, scope: &QueueScope) -> Result<Option<String>, String> {
    match scope {
        QueueScope::All => Ok(None),
        QueueScope::Repo(p) => git::repository_key(Path::new(p))
            .map(Some)
            .ok_or_else(|| format!("{p} is not a git repository")),
        QueueScope::Here => super::repository_of_workspace(workspace)
            .map(Some)
            .ok_or_else(|| {
                format!(
                    "this session's workspace {} is in no repository, so it has no queue of its \
                     own; `/queue all VERB …` acts across every repository, and `/queue /REPO \
                     VERB …` in one",
                    workspace.display()
                )
            }),
    }
}

/// **Whether the row `id` names is in `repository`** (R92 item 5) — `None` is every repository.
/// A row of another repository is refused with the line that acts on it, so a verb typed in one
/// project's session never moves another project's row by accident.
pub fn in_scope(store: &Store, id: &str, repository: Option<&str>) -> Result<(), String> {
    let Some(here) = repository else {
        return Ok(());
    };
    let (its, verb_id) = match find(store, id)? {
        Found::Review(r) => (r.repository, r.id),
        Found::Merge(m) => (m.repository, m.id),
    };
    if its == here {
        return Ok(());
    }
    Err(format!(
        "`{verb_id}` is a row of {its}, not of this session's repository {here}; `/queue all \
         VERB {verb_id}` (or `/queue {its} VERB {verb_id}`) acts on it"
    ))
}

fn announce(events: &dyn Fn(SessionEvent), queue: &str, id: &str, to: &str, why: &str) {
    events(SessionEvent::QueueMoved {
        queue: queue.into(),
        id: id.into(),
        to: to.into(),
        why: why.into(),
    });
}

fn by(session: &str) -> String {
    format!("operator:{session}")
}

/// **Approve over the reviewer** (decision 5) — a review row that is waiting, under review,
/// rejected, undecided or blocked becomes `approved` at the head its branch has NOW, and a merge
/// row is born for it; the merge gate still runs.
pub fn approve(
    store: &Store,
    id: &str,
    session: &str,
    events: &dyn Fn(SessionEvent),
) -> Result<String, String> {
    let Found::Review(r) = find(store, id)? else {
        return Err(format!(
            "`{id}` is a merge row: it is approved already. `/queue restart {id}` puts a parked \
             landing back in line."
        ));
    };
    if r.kind == ReviewKind::Target {
        return Err(format!(
            "`{}` is the review of the RED target `{}`, not a branch: there is nothing to land. It \
             ends when the target is green again.",
            r.id, r.target
        ));
    }
    if !matches!(
        r.state,
        ReviewState::Waiting
            | ReviewState::Reviewing
            | ReviewState::Rejected
            | ReviewState::CannotDecide
            | ReviewState::Blocked
    ) {
        return Err(format!(
            "`{}` is `{}`; only a waiting, reviewing, rejected, undecided or blocked review can be \
             approved",
            r.id,
            r.state.as_str()
        ));
    }
    let head = git::branch_head(Path::new(&r.repository), &r.branch).ok_or_else(|| {
        format!(
            "`{}` cannot be read in {}, so there is no head to approve",
            r.branch, r.repository
        )
    })?;
    let who = by(session);
    let why = format!(
        "approved by the operator over the reviewer at {head} (was `{}`); the merge gate still runs",
        r.state.as_str()
    );
    let merge_id = format!("m-{}-{}", r.id, now_ms());
    let ok = store
        .approve_review(
            &r.id,
            &Approval {
                expect: r.state,
                expect_owner: None,
                reviewed_sha: head.clone(),
                by: who,
                reasons: vec!["approved by the operator (decision 5)".into()],
                files: vec![],
                commands: vec![],
                why: why.clone(),
                merge_id: merge_id.clone(),
            },
            now_ms(),
        )
        .map_err(|e| e.to_string())?;
    if !ok {
        return Err(format!("`{}` moved while you asked; look again", r.id));
    }
    announce(events, "review", &r.id, "approved", &why);
    announce(events, "merge", &merge_id, "queued", &why);
    Ok(format!(
        "`{}` approved at {head}; it is in the merge queue as `{merge_id}` and lands when its gate \
         passes",
        r.branch
    ))
}

/// **Try again** — a parked review back to `waiting`, a parked landing back to `queued`.
pub fn restart(
    store: &Store,
    id: &str,
    session: &str,
    events: &dyn Fn(SessionEvent),
) -> Result<String, String> {
    restart_by(store, id, &by(session), events)
}

/// [`restart`], with the mover named whole — `agent:<session>` for the `queue` tool (R76 item 2),
/// so the history says a model did it and not the operator.
pub fn restart_by(
    store: &Store,
    id: &str,
    who: &str,
    events: &dyn Fn(SessionEvent),
) -> Result<String, String> {
    let who = who.to_string();
    let actor = who.split(':').next().unwrap_or("operator").to_string();
    match find(store, id)? {
        Found::Review(r) => {
            if !matches!(
                r.state,
                ReviewState::Rejected
                    | ReviewState::CannotDecide
                    | ReviewState::Blocked
                    | ReviewState::Withdrawn
            ) {
                return Err(format!("`{}` is `{}`, not parked", r.id, r.state.as_str()));
            }
            let why = format!("restarted by the {actor} (was `{}`)", r.state.as_str());
            let ok = store
                .move_review(
                    &r.id,
                    r.state,
                    ReviewState::Waiting,
                    &Move::new(&who, why.clone())
                        .set("attempts", 0u32)
                        .set("failure", "")
                        .set("owner", "")
                        .clear("reviewer")
                        .clear("verdict")
                        .clear("verdict_by")
                        .clear("reviewed_sha"),
                    now_ms(),
                )
                .map_err(|e| e.to_string())?;
            if !ok {
                return Err(format!("`{}` moved while you asked; look again", r.id));
            }
            announce(events, "review", &r.id, "waiting", &why);
            Ok(format!("`{}` waits for a reviewer again", r.branch))
        }
        Found::Merge(m) => {
            if !matches!(
                m.state,
                MergeState::Failed
                    | MergeState::TimedOut
                    | MergeState::Conflict
                    | MergeState::Blocked
                    | MergeState::Withdrawn
            ) {
                return Err(format!("`{}` is `{}`, not parked", m.id, m.state.as_str()));
            }
            let why = format!("restarted by the {actor} (was `{}`)", m.state.as_str());
            let ok = store
                .move_merge(
                    &m.id,
                    m.state,
                    MergeState::Queued,
                    &Move::new(&who, why.clone()).set("owner", ""),
                    now_ms(),
                )
                .map_err(|e| e.to_string())?;
            if !ok {
                return Err(format!("`{}` moved while you asked; look again", m.id));
            }
            announce(events, "merge", &m.id, "queued", &why);
            Ok(format!("`{}` is back in line to land", m.branch))
        }
    }
}

/// **The operator's rejection** — a review rejected in the person's words (the complaints go back
/// with the next restart's message); a landing withdrawn.
pub fn veto(
    store: &Store,
    id: &str,
    why: &str,
    session: &str,
    deliver: &dyn Fn(&str, &str) -> Result<String, String>,
    events: &dyn Fn(SessionEvent),
) -> Result<String, String> {
    let who = by(session);
    let words = if why.trim().is_empty() {
        "no reason given".to_string()
    } else {
        why.trim().to_string()
    };
    match find(store, id)? {
        Found::Review(r) => {
            if !r.state.is_open() {
                return Err(format!("`{}` is `{}`, not open", r.id, r.state.as_str()));
            }
            let said = format!("vetoed by the operator: {words}");
            let ok = store
                .move_review(
                    &r.id,
                    r.state,
                    ReviewState::Rejected,
                    &Move::new(&who, said.clone())
                        .set("owner", "")
                        .set("verdict", "reject")
                        .set("verdict_by", who.clone())
                        .set("reviewed_sha", r.head_sha.clone())
                        .set(
                            "reasons_json",
                            serde_json::to_string(&vec![words.clone()]).unwrap_or_default(),
                        ),
                    now_ms(),
                )
                .map_err(|e| e.to_string())?;
            if !ok {
                return Err(format!("`{}` moved while you asked; look again", r.id));
            }
            announce(events, "review", &r.id, "rejected", &said);
            let told = deliver(&r.session_id, &super::notices::vetoed(&r, &words));
            Ok(match told {
                Ok(_) => format!("`{}` vetoed; the child that wrote it was told", r.branch),
                Err(e) => format!("`{}` vetoed; the child could not be told: {e}", r.branch),
            })
        }
        Found::Merge(m) => withdraw_merge(
            store,
            &m,
            &format!("vetoed by the operator: {words}"),
            &who,
            events,
        ),
    }
}

fn withdraw_merge(
    store: &Store,
    m: &MergeRow,
    why: &str,
    who: &str,
    events: &dyn Fn(SessionEvent),
) -> Result<String, String> {
    if !matches!(
        m.state,
        MergeState::Queued
            | MergeState::Blocked
            | MergeState::Failed
            | MergeState::TimedOut
            | MergeState::Conflict
    ) {
        return Err(format!(
            "`{}` is `{}` and cannot be withdrawn{}",
            m.id,
            m.state.as_str(),
            if m.state == MergeState::Gating {
                " while its gate runs"
            } else {
                ""
            }
        ));
    }
    let ok = store
        .move_merge(
            &m.id,
            m.state,
            MergeState::Withdrawn,
            &Move::new(who, why),
            now_ms(),
        )
        .map_err(|e| e.to_string())?;
    if !ok {
        return Err(format!("`{}` moved while you asked; look again", m.id));
    }
    announce(events, "merge", &m.id, "withdrawn", why);
    Ok(format!(
        "`{}` withdrawn from the merge queue; the branch stays",
        m.branch
    ))
}

/// **Take a row out** — withdrawn, with the person's words; the branch and its worktree stay.
pub fn remove(
    store: &Store,
    id: &str,
    session: &str,
    events: &dyn Fn(SessionEvent),
) -> Result<String, String> {
    withdraw_by(
        store,
        id,
        &by(session),
        "withdrawn by the operator; the branch and its worktree stay",
        events,
    )
}

/// **Take a row out, in the mover's own words** — [`remove`]'s body, and the `queue` tool's
/// `withdraw` (R76 item 2), where `who` is `agent:<session>` and `why` the model's reason.
pub fn withdraw_by(
    store: &Store,
    id: &str,
    who: &str,
    why: &str,
    events: &dyn Fn(SessionEvent),
) -> Result<String, String> {
    let who = who.to_string();
    match find(store, id)? {
        Found::Review(r) => {
            if !matches!(
                r.state,
                ReviewState::Waiting
                    | ReviewState::Reviewing
                    | ReviewState::Blocked
                    | ReviewState::Rejected
                    | ReviewState::CannotDecide
            ) {
                return Err(format!("`{}` is `{}`", r.id, r.state.as_str()));
            }
            let why = why.to_string();
            let ok = store
                .move_review(
                    &r.id,
                    r.state,
                    ReviewState::Withdrawn,
                    &Move::new(&who, why.clone()).set("owner", ""),
                    now_ms(),
                )
                .map_err(|e| e.to_string())?;
            if !ok {
                return Err(format!("`{}` moved while you asked; look again", r.id));
            }
            announce(events, "review", &r.id, "withdrawn", &why);
            Ok(format!("`{}` withdrawn from the review queue", r.branch))
        }
        Found::Merge(m) => withdraw_merge(store, &m, why, &who, events),
    }
}

/// The parked rows a sweep acts on, by id: rejected, undecided and blocked reviews; failed, timed
/// out, conflicted and blocked landings.
fn parked(store: &Store, repository: Option<&str>) -> Result<Vec<String>, String> {
    let reviews = store.review_rows(repository).map_err(|e| e.to_string())?;
    let merges = store.merge_rows(repository).map_err(|e| e.to_string())?;
    Ok(reviews
        .into_iter()
        .filter(|r| {
            r.kind == ReviewKind::Branch
                && matches!(
                    r.state,
                    ReviewState::Rejected | ReviewState::CannotDecide | ReviewState::Blocked
                )
        })
        .map(|r| r.id)
        .chain(
            merges
                .into_iter()
                .filter(|m| {
                    matches!(
                        m.state,
                        MergeState::Failed
                            | MergeState::TimedOut
                            | MergeState::Conflict
                            | MergeState::Blocked
                    )
                })
                .map(|m| m.id),
        )
        .collect())
}

/// `/queue reset [ID]` — restart one parked row, or every one in `repository` (`None`: every
/// repository's).
pub fn reset(
    store: &Store,
    id: Option<&str>,
    repository: Option<&str>,
    session: &str,
    events: &dyn Fn(SessionEvent),
) -> Result<String, String> {
    if let Some(id) = id {
        return restart(store, id, session, events);
    }
    let ids = parked(store, repository)?;
    let n = ids
        .iter()
        .filter(|id| restart(store, id, session, events).is_ok())
        .count();
    Ok(format!("{n} parked row(s) restarted"))
}

/// `/queue clean [ID]` — withdraw one parked row, or every one in `repository` (`None`: every
/// repository's). Nothing is deleted.
pub fn clean(
    store: &Store,
    id: Option<&str>,
    repository: Option<&str>,
    session: &str,
    events: &dyn Fn(SessionEvent),
) -> Result<String, String> {
    if let Some(id) = id {
        return remove(store, id, session, events);
    }
    let ids = parked(store, repository)?;
    let n = ids
        .iter()
        .filter(|id| remove(store, id, session, events).is_ok())
        .count();
    Ok(format!(
        "{n} parked row(s) withdrawn — kept in the history, gone from the queues"
    ))
}

/// **Gate the target's tip again, even when that tip was already judged** (R76 item 3, R74 #4).
///
/// Before this a target was gated only when its tip CHANGED: a red that was the machine's, a
/// timed-out gate, a gate whose setup was fixed outside the target (a corpus installed, a `.letibot`
/// path made) held every landing until somebody committed to the target. The verb writes the
/// health back to `gating` at the tip as it is NOW; the repository's queue worker sees `gating`
/// and runs the gate on its next pass — the same door the red tip's reviewer uses when it finds
/// the code sound. `who` is recorded on the move.
pub fn regate(
    store: &Store,
    repository: &str,
    target: &str,
    who: &str,
    events: &dyn Fn(SessionEvent),
) -> Result<String, String> {
    let repo = Path::new(repository);
    let tip = git::branch_head(repo, target).ok_or_else(|| {
        format!("`{target}` does not exist in {repository}, so there is no tip to gate")
    })?;
    let before = store
        .target_health(repository, target)
        .map_err(|e| e.to_string())?;
    let was = match &before {
        Some(h) if h.sha == tip => format!("it was `{}` at this tip", h.state.as_str()),
        Some(h) => format!(
            "it was `{}` at {}, and the tip is {tip} now",
            h.state.as_str(),
            h.sha
        ),
        None => "it was never gated".to_string(),
    };
    let why = format!("re-gate asked by {who}: {was}");
    store
        .set_target_health(
            &TargetHealth {
                repository: repository.to_string(),
                target: target.to_string(),
                sha: tip.clone(),
                state: HealthState::Gating,
                checked_ms: now_ms(),
                evidence: why.clone(),
                gate_steps: vec![],
            },
            who,
        )
        .map_err(|e| e.to_string())?;
    announce(events, "target", target, "gating", &why);
    // **Who will run it** — a re-gate nobody serves is a row that says `gating` for ever.
    let leases = store.queue_leases().map_err(|e| e.to_string())?;
    let served = leases.iter().any(|l| {
        l.repository == repository
            && now_ms().saturating_sub(l.heartbeat_ms) < super::lease::LEASE_TTL_MS
    });
    Ok(if served {
        format!(
            "`{target}` at {tip} is gated again ({was}); the queue's worker for {repository} runs \
             it on its next pass, and the pane shows the verdict"
        )
    } else {
        format!(
            "`{target}` at {tip} is marked for a gate ({was}) — but NO daemon holds the queue \
             lease of {repository} now, so nothing runs it until one that serves it starts"
        )
    })
}

/// How long a tip whose gate was killed at its cap `kills` times waits before a daemon taking the
/// queue gates it again: 15 minutes, doubled per kill, at most 8 hours.
pub fn killed_backoff_ms(kills: u32) -> u64 {
    const BASE: u64 = 15 * 60 * 1000;
    const CAP: u64 = 8 * 60 * 60 * 1000;
    BASE.saturating_mul(1u64 << kills.saturating_sub(1).min(16))
        .min(CAP)
}

/// **What a daemon re-gates when it takes a repository's lease** (R76 item 3) — every target
/// whose last verdict was `red`, `blocked` or `timed_out`. A restart was the one way out of a
/// timed-out or blocked target before `/queue regate`, and it did not even do that: the health
/// row said the tip was judged, so the new daemon left it alone. A green tip is not gated again
/// (nothing about it is in doubt), and a `gating` one is already going to be.
pub fn regate_at_start(store: &Store, repository: &str, who: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for h in store.target_healths().map_err(|e| e.to_string())? {
        if h.repository != repository
            || !matches!(
                h.state,
                HealthState::Red | HealthState::Blocked | HealthState::TimedOut
            )
        {
            continue;
        }
        // **A gate killed at its memory cap is retried less often each time** (R92.25): with the
        // same cap it dies the same way, and every lease take re-ran it at once. The wait doubles
        // per kill of this tip — 15 min, 30, 60, … up to 8 h — counted in the store, so a restart
        // loop is not a retry loop. `/queue regate` is the operator's way past it.
        if h.state == HealthState::Blocked
            && h.evidence.starts_with(super::gate::GATE_KILLED)
            && let Ok(Some(t)) =
                store.told(&super::serve::killed_key(repository, &h.target, &h.sha))
            && now_ms().saturating_sub(t.at_ms) < killed_backoff_ms(t.count)
        {
            continue;
        }
        let why = format!(
            "re-gated as this daemon took the queue: the last verdict was `{}` at {}",
            h.state.as_str(),
            h.sha
        );
        store
            .set_target_health(
                &TargetHealth {
                    state: HealthState::Gating,
                    checked_ms: now_ms(),
                    evidence: why,
                    gate_steps: vec![],
                    ..h.clone()
                },
                who,
            )
            .map_err(|e| e.to_string())?;
        out.push(h.target);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mergequeue::serve::tests::{TempDir, repo};
    use letibot_tokencore::store::{MergePriority, NewReview};

    fn parked_review(store: &Store, id: &str, repository: &str) {
        store
            .enqueue_review(
                &NewReview {
                    id: id.into(),
                    repository: repository.into(),
                    target: "main".into(),
                    kind: ReviewKind::Branch,
                    session_id: "s-x".into(),
                    branch: format!("agent/{id}"),
                    worktree: None,
                    base_sha: String::new(),
                    head_sha: "abc".into(),
                    brief: String::new(),
                    priority: MergePriority::Subagent,
                    fixes: None,
                    evidence: String::new(),
                },
                "test",
                1,
            )
            .unwrap();
        assert!(
            store
                .move_review(
                    id,
                    ReviewState::Waiting,
                    ReviewState::Rejected,
                    &Move::new("test", "rejected"),
                    2,
                )
                .unwrap()
        );
    }

    fn state(store: &Store, id: &str) -> ReviewState {
        store.review_row(id).unwrap().unwrap().state
    }

    /// **`/queue clean` in one project's session sweeps that project's queue only** (R92 item
    /// 5) — the operator's *"the /queue is still not project scoped"*. Before, the sweep read
    /// every repository's parked rows. A row of another repository named by id is refused with the
    /// line that reaches it; `/queue all` is the explicit way to every repository.
    #[test]
    fn the_sweeps_and_the_row_verbs_stay_in_the_sessions_repository() {
        let dir = TempDir::new("verbs-scope");
        let mine = repo(&dir.0);
        let key = git::repository_key(&mine).unwrap();
        let store = Store::open(&dir.0.join("sessions.db")).unwrap();
        parked_review(&store, "r-mine", &key);
        parked_review(&store, "r-theirs", "/elsewhere/other");

        let here = scope_repository(&mine, &QueueScope::Here).unwrap();
        assert_eq!(here.as_deref(), Some(key.as_str()));
        let said = clean(&store, None, here.as_deref(), "s-1", &|_| {}).unwrap();
        assert!(said.starts_with("1 parked"), "{said}");
        assert_eq!(state(&store, "r-mine"), ReviewState::Withdrawn);
        assert_eq!(
            state(&store, "r-theirs"),
            ReviewState::Rejected,
            "another repository's row is not this session's to sweep"
        );

        let refused = in_scope(&store, "r-theirs", here.as_deref()).unwrap_err();
        assert!(
            refused.contains("/elsewhere/other") && refused.contains("/queue all"),
            "{refused}"
        );
        assert!(in_scope(&store, "r-mine", here.as_deref()).is_ok());

        // **Explicitly every repository.**
        let all = scope_repository(&mine, &QueueScope::All).unwrap();
        assert!(in_scope(&store, "r-theirs", all.as_deref()).is_ok());
        clean(&store, None, all.as_deref(), "s-1", &|_| {}).unwrap();
        assert_eq!(state(&store, "r-theirs"), ReviewState::Withdrawn);

        // **A path names one repository** — a worktree path or a subdirectory means its repository.
        std::fs::create_dir_all(mine.join("sub")).unwrap();
        let named = QueueScope::Repo(mine.join("sub").display().to_string());
        assert_eq!(
            scope_repository(&dir.0, &named).unwrap().as_deref(),
            Some(key.as_str())
        );
    }

    /// **A session of no repository is told so, and offered `/queue all`.**
    #[test]
    fn a_session_outside_every_repository_is_told_and_offered_all() {
        let dir = TempDir::new("verbs-none");
        let empty = dir.0.join("nothing-here");
        std::fs::create_dir_all(&empty).unwrap();
        let why = scope_repository(&empty, &QueueScope::Here).unwrap_err();
        assert!(
            why.contains("in no repository") && why.contains("/queue all"),
            "{why}"
        );
    }
}
