//! **The operator's verbs on the two queues** — `/queue approve|restart|veto|rm|reset|clean`.
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
use letibot_tokencore::store::{
    Approval, MergeRow, MergeState, Move, ReviewKind, ReviewRow, ReviewState, Store,
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
    let who = by(session);
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
            let why = format!("restarted by the operator (was `{}`)", r.state.as_str());
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
            let why = format!("restarted by the operator (was `{}`)", m.state.as_str());
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
            let told = deliver(
                &r.session_id,
                &format!(
                    "The operator VETOED `{}`: {words}. It does not land. Answer it in the branch \
                     and commit; the new head goes back to review by itself.",
                    r.branch
                ),
            );
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
    let who = by(session);
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
            let why = "withdrawn by the operator; the branch and its worktree stay".to_string();
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
        Found::Merge(m) => withdraw_merge(store, &m, "withdrawn by the operator", &who, events),
    }
}

/// The parked rows a sweep acts on, by id: rejected, undecided and blocked reviews; failed, timed
/// out, conflicted and blocked landings.
fn parked(store: &Store) -> Result<Vec<String>, String> {
    let reviews = store.review_rows(None).map_err(|e| e.to_string())?;
    let merges = store.merge_rows(None).map_err(|e| e.to_string())?;
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

/// `/queue reset [ID]` — restart one parked row, or every one.
pub fn reset(
    store: &Store,
    id: Option<&str>,
    session: &str,
    events: &dyn Fn(SessionEvent),
) -> Result<String, String> {
    if let Some(id) = id {
        return restart(store, id, session, events);
    }
    let ids = parked(store)?;
    let n = ids
        .iter()
        .filter(|id| restart(store, id, session, events).is_ok())
        .count();
    Ok(format!("{n} parked row(s) restarted"))
}

/// `/queue clean [ID]` — withdraw one parked row, or every one. Nothing is deleted.
pub fn clean(
    store: &Store,
    id: Option<&str>,
    session: &str,
    events: &dyn Fn(SessionEvent),
) -> Result<String, String> {
    if let Some(id) = id {
        return remove(store, id, session, events);
    }
    let ids = parked(store)?;
    let n = ids
        .iter()
        .filter(|id| remove(store, id, session, events).is_ok())
        .count();
    Ok(format!(
        "{n} parked row(s) withdrawn — kept in the history, gone from the queues"
    ))
}
