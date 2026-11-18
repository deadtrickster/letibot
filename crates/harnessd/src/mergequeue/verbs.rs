//! The operator's verbs on the two queues - `/queue
//! approve|restart|veto|rm|reset|clean|regate`.

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

/// The repository a `/queue` verb typed in a session at `workspace` may act in: `None` is every
/// repository. [`QueueScope::Here`] is the session's own (the queues' key for its workspace), and a
/// session of no repository is told so with the lines that reach one.
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

/// Every repository a `/queue` verb may act in - [`scope_repository`] for several: `[None]`
/// is every repository, a session of one repository is `[Some(it)]`, and a session of a
/// folder that is one project of several (`git::repos_of_workspace`) is each of them.
pub fn scope_repositories(
    workspace: &Path,
    scope: &QueueScope,
) -> Result<Vec<Option<String>>, String> {
    match scope {
        QueueScope::Here => {
            let many = super::repositories_of_workspace(workspace);
            if many.len() > 1 {
                return Ok(many.into_iter().map(Some).collect());
            }
            scope_repository(workspace, scope).map(|r| vec![r])
        }
        _ => scope_repository(workspace, scope).map(|r| vec![r]),
    }
}

/// The repository the row `id` is a row of - for a verb in a session of several to act in the
/// right one. `None` for a row no queue holds, which the verb itself refuses by name.
pub fn row_repository(store: &Store, id: &str) -> Option<String> {
    match find(store, id).ok()? {
        Found::Review(r) => Some(r.repository),
        Found::Merge(m) => Some(m.repository),
    }
}

/// Whether the row `id` names is in `repository` (`None` is every repository). A row of another
/// repository is refused with the line that acts on it, so a verb typed in one project's session
/// never moves another project's row by accident.
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

/// The mover's kind, `operator` for `operator:<session>` and `bridge` for `bridge:<handle>`.
fn actor(who: &str) -> &str {
    who.split(':').next().unwrap_or("operator")
}

/// A verb the table rules on. The others only take rows out of line, which any mover may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verb {
    Approve,
    Restart,
    /// Kill the gate of a row: the row's own, not somebody else's, and the operator's any.
    Stop,
}

/// What the table reads about a row: who wrote it, who put it where it is, and whether it is out.
pub(crate) struct Facts<'a> {
    /// The session that enqueued it: `bridge:<handle>` for the bridge, a session id for an agent.
    pub author: &'a str,
    /// Who moved it into its current state, from its last transition.
    pub parked_by: &'a str,
    pub withdrawn: bool,
}

/// The one verb x actor x state rule. The operator may do anything; a bridge may not approve
/// what a bridge enqueued; an agent may restart only its own rows; nobody but the operator undoes
/// the operator's park or any withdrawal.
pub(crate) fn may(verb: Verb, who: &str, row: &Facts) -> Result<(), String> {
    let (kind, name) = who.split_once(':').unwrap_or((who, ""));
    if kind == "operator" {
        return Ok(());
    }
    match verb {
        Verb::Approve if kind == "bridge" && row.author.starts_with("bridge:") => Err(format!(
            "a row the bridge enqueued (`{}`) is approved by the operator, not by `{who}`: \
             `/queue approve ID` in a session",
            row.author
        )),
        Verb::Approve if kind == "bridge" => Ok(()),
        Verb::Approve => Err(format!(
            "only the operator approves over a reviewer, not `{who}`"
        )),
        Verb::Restart if row.parked_by.starts_with("operator:") => Err(format!(
            "the operator parked it (`{}`), so only the operator restarts it",
            row.parked_by
        )),
        Verb::Restart if row.withdrawn => Err(format!(
            "a withdrawn row is restarted by the operator, not by `{who}`"
        )),
        Verb::Restart if kind == "agent" && row.author != name => Err(format!(
            "`{who}` may restart only its own rows; this one is `{}`'s",
            row.author
        )),
        Verb::Restart => Ok(()),
        // A stop is the model's answer to a gate that is going nowhere, and it is its OWN gate's:
        // an agent or a bridge stops the gate of a row it enqueued, or one its own `regate` started.
        Verb::Stop if row.author != name && row.author != who => Err(format!(
            "`{who}` may stop only the gate of its own row; this one is `{}`'s",
            row.author
        )),
        Verb::Stop => Ok(()),
    }
}

/// The last mover of a row into `state`, from its history.
fn parked_by(store: &Store, queue: &str, id: &str, state: &str) -> String {
    store
        .transitions(queue, id)
        .ok()
        .and_then(|t| t.into_iter().rev().find(|t| t.to_state == state))
        .map(|t| t.by)
        .unwrap_or_default()
}

/// Approve over the reviewer: a review row that is waiting, under review, rejected, undecided or
/// blocked becomes `approved` at the head its branch has NOW, and a merge row is born for it; the
/// merge gate still runs.
pub fn approve(
    store: &Store,
    id: &str,
    session: &str,
    events: &dyn Fn(SessionEvent),
) -> Result<String, String> {
    approve_by(store, id, &by(session), events)
}

/// [`approve`], with the mover named whole (`bridge:<handle>` for the Claude Code bridge).
pub fn approve_by(
    store: &Store,
    id: &str,
    who: &str,
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
    may(
        Verb::Approve,
        who,
        &Facts {
            author: &r.session_id,
            parked_by: "",
            withdrawn: false,
        },
    )?;
    let head = git::branch_head(Path::new(&r.repository), &r.branch).ok_or_else(|| {
        format!(
            "`{}` cannot be read in {}, so there is no head to approve",
            r.branch, r.repository
        )
    })?;
    let actor = actor(who);
    let why = format!(
        "approved by the {actor} over the reviewer at {head} (was `{}`); the merge gate still runs",
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
                by: who.to_string(),
                reasons: vec![format!("approved by the {actor} (decision 5)")],
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

/// Try again - a parked review back to `waiting`, a parked landing back to `queued`.
pub fn restart(
    store: &Store,
    id: &str,
    session: &str,
    events: &dyn Fn(SessionEvent),
) -> Result<String, String> {
    restart_by(store, id, &by(session), events)
}

/// [`restart`], with the mover named whole - `agent:<session>` for the `queue` tool, so the history
/// says a model did it and not the operator.
pub fn restart_by(
    store: &Store,
    id: &str,
    who: &str,
    events: &dyn Fn(SessionEvent),
) -> Result<String, String> {
    let who = who.to_string();
    let actor = actor(&who).to_string();
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
            may(
                Verb::Restart,
                &who,
                &Facts {
                    author: &r.session_id,
                    parked_by: &parked_by(store, "review", &r.id, r.state.as_str()),
                    withdrawn: r.state == ReviewState::Withdrawn,
                },
            )?;
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
            let author = store
                .review_row(&m.review_id)
                .ok()
                .flatten()
                .map(|r| r.session_id)
                .unwrap_or_default();
            may(
                Verb::Restart,
                &who,
                &Facts {
                    author: &author,
                    parked_by: &parked_by(store, "merge", &m.id, m.state.as_str()),
                    withdrawn: m.state == MergeState::Withdrawn,
                },
            )?;
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

/// The operator's rejection - a review rejected in the person's words (the complaints go back with
/// the next restart's message); a landing withdrawn.
pub fn veto(
    store: &Store,
    id: &str,
    why: &str,
    session: &str,
    deliver: &dyn Fn(&str, &str) -> Result<String, String>,
    events: &dyn Fn(SessionEvent),
) -> Result<String, String> {
    veto_by(store, id, why, &by(session), deliver, events)
}

/// [`veto`], with the mover named whole.
pub fn veto_by(
    store: &Store,
    id: &str,
    why: &str,
    who: &str,
    deliver: &dyn Fn(&str, &str) -> Result<String, String>,
    events: &dyn Fn(SessionEvent),
) -> Result<String, String> {
    let actor = actor(who);
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
            let said = format!("vetoed by the {actor}: {words}");
            let ok = store
                .move_review(
                    &r.id,
                    r.state,
                    ReviewState::Rejected,
                    &Move::new(who, said.clone())
                        .set("owner", "")
                        .set("verdict", "reject")
                        .set("verdict_by", who.to_string())
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
        Found::Merge(m) => {
            // A running gate is taken out by `rm`, not by `veto` - `veto` is *rejected in your words
            // sent back to the child*, and a landing has no child left to send it to.
            if m.state == MergeState::Gating {
                return Err(format!(
                    "`{}` is `gating`; `/queue rm {}` takes it out of line while its gate runs",
                    m.id, m.id
                ));
            }
            withdraw_merge(
                store,
                &m,
                &format!("vetoed by the {actor}: {words}"),
                who,
                events,
            )
        }
    }
}

fn withdraw_merge(
    store: &Store,
    m: &MergeRow,
    why: &str,
    who: &str,
    events: &dyn Fn(SessionEvent),
) -> Result<String, String> {
    // `gating` is withdrawable too: *take it out of line* is the person's answer to a gate
    // that is going nowhere, and refusing it left the pane no way out of the one state a queue
    // gets stuck in.
    if !matches!(
        m.state,
        MergeState::Queued
            | MergeState::Gating
            | MergeState::Blocked
            | MergeState::Failed
            | MergeState::TimedOut
            | MergeState::Conflict
    ) {
        return Err(format!(
            "`{}` is `{}` and cannot be withdrawn",
            m.id,
            m.state.as_str()
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
    // A row out of line has no gate: one still running for it would hold the box's build lock and
    // judge a branch nobody waits for. The verdict it then writes is `stopped`, and a row that has
    // left `gating` is not moved by it.
    let stopped = stop_gate_of(store, &m.repository, &[&m.id], who, why, events)?;
    Ok(format!(
        "`{}` withdrawn from the merge queue; the branch stays{stopped}",
        m.branch
    ))
}

/// Take down the gate of `ids`, if that is the one running - answered with the clause a caller adds to
/// its own sentence, empty when no gate of those rows was running.
fn stop_gate_of(
    store: &Store,
    repository: &str,
    ids: &[&str],
    who: &str,
    why: &str,
    events: &dyn Fn(SessionEvent),
) -> Result<String, String> {
    let Some(gate) = store.live_gate(repository).map_err(|e| e.to_string())? else {
        return Ok(String::new());
    };
    let carries_one = gate.entries().iter().any(|e| ids.contains(e));
    if !carries_one || gate.stopping() {
        return Ok(String::new());
    }
    // `false` is a gate that ended between the read and the ask: it judged nothing about this row,
    // which is out of line either way.
    let _ = store
        .ask_gate_stop(repository, &gate.run_id, who, why)
        .map_err(|e| e.to_string())?;
    announce(
        events,
        "gate",
        &gate.run_id,
        "stopping",
        &format!("{} by {who}: {why}", super::serve::STOPPED),
    );
    Ok("; the gate it was under is stopped too".to_string())
}

/// Take a row out - withdrawn, with the person's words; the branch and its worktree stay.
pub fn remove(
    store: &Store,
    id: &str,
    session: &str,
    events: &dyn Fn(SessionEvent),
) -> Result<String, String> {
    remove_by(store, id, &by(session), events)
}

/// [`remove`], with the mover named whole.
pub fn remove_by(
    store: &Store,
    id: &str,
    who: &str,
    events: &dyn Fn(SessionEvent),
) -> Result<String, String> {
    let why = format!(
        "withdrawn by the {}; the branch and its worktree stay",
        actor(who)
    );
    withdraw_by(store, id, who, &why, events)
}

/// Take a row out, in the mover's own words - [`remove`]'s body, and the `queue` tool's
/// `withdraw`, where `who` is `agent:<session>` and `why` the model's reason.
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

/// Stop the gate this repository is running - `queue`'s `stop`, and what withdrawing a gated row does
/// first. The worker notices on its next pass, kills the run, and the verdict it applies is `stopped`:
/// the rows go back in line, with this request's words on them.
pub fn stop_by(
    store: &Store,
    repository: &str,
    who: &str,
    why: &str,
    events: &dyn Fn(SessionEvent),
) -> Result<String, String> {
    let Some(gate) = store.live_gate(repository).map_err(|e| e.to_string())? else {
        return Err(format!(
            "no gate is running in {repository} now. What is gated, and what it is doing, is what \
             `queue` with `action: \"status\"` prints"
        ));
    };
    may_stop(store, &gate, who)?;
    if gate.stopping() {
        return Err(format!(
            "{} already asked this gate to stop ({}), and its worker has not killed it yet",
            gate.stop_by, gate.stop_why
        ));
    }
    let asked = store
        .ask_gate_stop(repository, &gate.run_id, who, why)
        .map_err(|e| e.to_string())?;
    if !asked {
        return Err(format!(
            "the gate of {repository} ended while you asked; `queue` with `action: \"status\"` \
             shows what is there now"
        ));
    }
    let said = format!("{} by {who}: {why}", super::serve::STOPPED);
    announce(events, "gate", &gate.run_id, "stopping", &said);
    Ok(format!(
        "the gate of {repository} is asked to stop: {}. Its worker kills it on its next pass, and \
         the verdict it then writes is `stopped` - nothing is judged about the code, and what the \
         gate was for goes back in line with your words on it",
        super::desk::gate_said(&gate, now_ms(), "")
    ))
}

/// Whether `who` may stop this gate: the operator any; an agent or a bridge its own row's, or a
/// target it re-gated itself (`regate` asks for that gate, and the target's history says so).
fn may_stop(
    store: &Store,
    gate: &letibot_tokencore::store::LiveGate,
    who: &str,
) -> Result<(), String> {
    if gate.entries().is_empty() {
        // A target's own tip: who asked for the gate on it is the last move INTO `gating`, and a gate
        // the queue started for itself was asked for by the queue - the operator's, not an agent's.
        let by = store
            .transitions("target", &gate.target)
            .map_err(|e| e.to_string())?
            .into_iter()
            .rev()
            .find(|t| t.to_state == HealthState::Gating.as_str())
            .map(|t| t.by)
            .unwrap_or_default();
        return may(
            Verb::Stop,
            who,
            &Facts {
                author: &by,
                parked_by: "",
                withdrawn: false,
            },
        );
    }
    for id in gate.entries() {
        let row = store.merge_row(id).map_err(|e| e.to_string())?;
        let author = row
            .and_then(|m| store.review_row(&m.review_id).ok().flatten())
            .map(|r| r.session_id)
            .unwrap_or_default();
        may(
            Verb::Stop,
            who,
            &Facts {
                author: &author,
                parked_by: "",
                withdrawn: false,
            },
        )?;
    }
    Ok(())
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

/// `/queue reset [ID]` - restart one parked row, or every one in `repository` (`None`: every
/// repository's).
pub fn reset(
    store: &Store,
    id: Option<&str>,
    repository: Option<&str>,
    session: &str,
    events: &dyn Fn(SessionEvent),
) -> Result<String, String> {
    reset_by(store, id, repository, &by(session), events)
}

/// [`reset`], with the mover named whole.
pub fn reset_by(
    store: &Store,
    id: Option<&str>,
    repository: Option<&str>,
    who: &str,
    events: &dyn Fn(SessionEvent),
) -> Result<String, String> {
    if let Some(id) = id {
        return restart_by(store, id, who, events);
    }
    let ids = parked(store, repository)?;
    let n = ids
        .iter()
        .filter(|id| restart_by(store, id, who, events).is_ok())
        .count();
    Ok(format!("{n} parked row(s) restarted"))
}

/// `/queue clean [ID]` - withdraw one parked row, or every one in `repository` (`None`: every
/// repository's). Nothing is deleted.
pub fn clean(
    store: &Store,
    id: Option<&str>,
    repository: Option<&str>,
    session: &str,
    events: &dyn Fn(SessionEvent),
) -> Result<String, String> {
    clean_by(store, id, repository, &by(session), events)
}

/// [`clean`], with the mover named whole.
pub fn clean_by(
    store: &Store,
    id: Option<&str>,
    repository: Option<&str>,
    who: &str,
    events: &dyn Fn(SessionEvent),
) -> Result<String, String> {
    if let Some(id) = id {
        return remove_by(store, id, who, events);
    }
    let ids = parked(store, repository)?;
    let n = ids
        .iter()
        .filter(|id| remove_by(store, id, who, events).is_ok())
        .count();
    Ok(format!(
        "{n} parked row(s) withdrawn — kept in the history, gone from the queues"
    ))
}

/// Gate the target's tip again, even when that tip was already judged.
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
    let was_red = before
        .as_ref()
        .is_some_and(|h| h.sha == tip && super::serve::halts_reviews(h));
    let why = match was_red {
        true => format!(
            "re-gate asked by {who}: {was}; {}",
            super::serve::STILL_HALTED
        ),
        false => format!("re-gate asked by {who}: {was}"),
    };
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
    // Who will run it: a re-gate nobody serves is a row that says `gating` for ever.
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

/// What a daemon re-gates when it takes a repository's lease: every target whose last
/// verdict was `red`, `blocked` or `timed_out`.
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
        // A gate killed at its memory cap is retried less often each time: with the same cap it
        // dies the same way, and every lease take re-ran it at once.
        if h.state == HealthState::Blocked
            && h.evidence.starts_with(super::gate::GATE_KILLED)
            && let Ok(Some(t)) =
                store.told(&super::serve::killed_key(repository, &h.target, &h.sha))
            && now_ms().saturating_sub(t.at_ms) < killed_backoff_ms(t.count)
        {
            continue;
        }
        let mut why = format!(
            "re-gated as this daemon took the queue: the last verdict was `{}` at {}",
            h.state.as_str(),
            h.sha
        );
        if h.state == HealthState::Red {
            why = format!("{why}; {}", super::serve::STILL_HALTED);
        }
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

    /// `/queue clean` in one project's session sweeps that project's queue only - before, the sweep
    /// read every repository's parked rows. A row of another repository named by id is refused with
    /// the line that reaches it; `/queue all` is the explicit way to every repository.
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

        // Explicitly every repository.
        let all = scope_repository(&mine, &QueueScope::All).unwrap();
        assert!(in_scope(&store, "r-theirs", all.as_deref()).is_ok());
        clean(&store, None, all.as_deref(), "s-1", &|_| {}).unwrap();
        assert_eq!(state(&store, "r-theirs"), ReviewState::Withdrawn);

        // A path names one repository - a worktree path or a subdirectory means its repository.
        std::fs::create_dir_all(mine.join("sub")).unwrap();
        let named = QueueScope::Repo(mine.join("sub").display().to_string());
        assert_eq!(
            scope_repository(&dir.0, &named).unwrap().as_deref(),
            Some(key.as_str())
        );
    }

    /// A session of no repository is told so, and offered `/queue all`.
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

    /// A session of a one-org project folder acts in each of its repositories. Both repositories
    /// are the session's scope; a row of either is in scope in its own; a row of a repository
    /// outside the folder is still refused.
    #[test]
    fn a_one_org_folder_scopes_a_verb_to_each_of_its_repositories() {
        let dir = TempDir::new("verbs-many");
        let org = dir.0.join("stroppy-io");
        let child = |name: &str, url: &str| {
            let p = org.join(name);
            std::fs::create_dir_all(&p).unwrap();
            git::git(&p, &["init", "-q", "-b", "main"]).unwrap();
            git::git(&p, &["remote", "add", "origin", url]).unwrap();
            std::fs::canonicalize(p).unwrap()
        };
        let a = child("a", "https://github.com/stroppy-io/a.git");
        let b = child("b", "git@github.com:stroppy-io/b.git");
        let (ka, kb) = (
            git::repository_key(&a).unwrap(),
            git::repository_key(&b).unwrap(),
        );
        let store = Store::open(&dir.0.join("sessions.db")).unwrap();
        parked_review(&store, "r-a", &ka);
        parked_review(&store, "r-b", &kb);
        parked_review(&store, "r-theirs", "/elsewhere/other");

        let scopes = scope_repositories(&org, &QueueScope::Here).unwrap();
        assert_eq!(scopes.len(), 2, "{scopes:?}");
        assert!(scopes.contains(&Some(ka.clone())) && scopes.contains(&Some(kb.clone())));
        assert_eq!(row_repository(&store, "r-b").as_deref(), Some(kb.as_str()));
        assert!(in_scope(&store, "r-b", Some(&kb)).is_ok());
        let theirs = row_repository(&store, "r-theirs").unwrap();
        assert!(
            !scopes.contains(&Some(theirs)),
            "a row of a repository outside the folder is in none of its scopes"
        );
        assert!(
            scope_repository(&org, &QueueScope::Here).is_err(),
            "no single one"
        );
    }

    /// A merge row of `s-x`'s review, approved by a reviewer.
    fn landing(store: &Store, id: &str) -> String {
        parked_review(store, id, "/r");
        assert!(
            store
                .move_review(
                    id,
                    ReviewState::Rejected,
                    ReviewState::Reviewing,
                    &Move::new("test", "back"),
                    3,
                )
                .unwrap()
        );
        let merge_id = format!("m-{id}");
        assert!(
            store
                .approve_review(
                    id,
                    &Approval {
                        expect: ReviewState::Reviewing,
                        expect_owner: None,
                        reviewed_sha: "abc".into(),
                        by: "reviewer:r1".into(),
                        reasons: vec![],
                        files: vec![],
                        commands: vec![],
                        why: "fine".into(),
                        merge_id: merge_id.clone(),
                    },
                    4,
                )
                .unwrap()
        );
        merge_id
    }

    fn merge_state(store: &Store, id: &str) -> MergeState {
        store
            .merge_rows(None)
            .unwrap()
            .into_iter()
            .find(|m| m.id == id)
            .unwrap()
            .state
    }

    /// The operator's veto stands against the author's restart; only the operator undoes it.
    #[test]
    fn agent_restart_cannot_undo_operator_veto() {
        let dir = TempDir::new("verbs-veto");
        let store = Store::open(&dir.0.join("sessions.db")).unwrap();
        let m = landing(&store, "r-v");
        let no_line = |_: &str, _: &str| -> Result<String, String> { Ok(String::new()) };
        veto(&store, &m, "not this", "s-op", &no_line, &|_| {}).unwrap();
        assert_eq!(merge_state(&store, &m), MergeState::Withdrawn);
        let e = restart_by(&store, &m, "agent:s-x", &|_| {}).expect_err("the author's restart");
        assert!(e.contains("only the operator restarts it"), "{e}");
        let e = restart_by(&store, &m, "bridge:cc", &|_| {}).expect_err("the bridge's restart");
        assert!(e.contains("only the operator"), "{e}");
        assert_eq!(merge_state(&store, &m), MergeState::Withdrawn);
        restart(&store, &m, "s-op", &|_| {}).expect("the operator's own");
        assert_eq!(merge_state(&store, &m), MergeState::Queued);

        // A review the operator vetoed is the operator's to restart too.
        parked_review(&store, "r-w", "/r");
        assert!(
            store
                .move_review(
                    "r-w",
                    ReviewState::Rejected,
                    ReviewState::Waiting,
                    &Move::new("test", "back"),
                    3,
                )
                .unwrap()
        );
        veto(&store, "r-w", "no", "s-op", &no_line, &|_| {}).unwrap();
        assert!(restart_by(&store, "r-w", "agent:s-x", &|_| {}).is_err());
        assert_eq!(state(&store, "r-w"), ReviewState::Rejected);
    }

    /// An agent restarts its own parked rows and nobody else's; a withdrawn row is the operator's.
    #[test]
    fn an_agent_restarts_only_its_own_rows() {
        let dir = TempDir::new("verbs-own");
        let store = Store::open(&dir.0.join("sessions.db")).unwrap();
        parked_review(&store, "r-o", "/r");
        let e = restart_by(&store, "r-o", "agent:s-other", &|_| {}).expect_err("not its row");
        assert!(e.contains("only its own rows"), "{e}");
        restart_by(&store, "r-o", "agent:s-x", &|_| {}).expect("its own row");
        assert_eq!(state(&store, "r-o"), ReviewState::Waiting);
        let m = landing(&store, "r-p");
        withdraw_by(&store, &m, "agent:s-x", "stale", &|_| {}).unwrap();
        let e = restart_by(&store, &m, "agent:s-x", &|_| {}).expect_err("withdrawn");
        assert!(e.contains("a withdrawn row"), "{e}");
    }

    /// Every verb x actor pair the table rules on, against one row of each kind.
    #[test]
    fn the_verb_actor_state_table_is_exhaustive() {
        let row = |author, parked_by, withdrawn| Facts {
            author,
            parked_by,
            withdrawn,
        };
        let agents = row("s-x", "queue:d", false);
        let bridges = row("bridge:cc", "queue:d", false);
        for verb in [Verb::Approve, Verb::Restart] {
            for r in [&agents, &bridges, &row("s-x", "operator:s", true)] {
                assert!(may(verb, "operator:s", r).is_ok());
            }
        }
        assert!(may(Verb::Approve, "bridge:cc", &agents).is_ok());
        assert!(may(Verb::Approve, "bridge:cc", &bridges).is_err());
        assert!(may(Verb::Approve, "bridge:other", &bridges).is_err());
        assert!(may(Verb::Approve, "agent:s-x", &agents).is_err());
        assert!(may(Verb::Restart, "agent:s-x", &agents).is_ok());
        assert!(may(Verb::Restart, "agent:s-y", &agents).is_err());
        assert!(may(Verb::Restart, "bridge:cc", &bridges).is_ok());
        assert!(may(Verb::Restart, "bridge:cc", &row("s-x", "operator:s", false)).is_err());
        assert!(may(Verb::Restart, "agent:s-x", &row("s-x", "agent:s-x", true)).is_err());
    }
}
