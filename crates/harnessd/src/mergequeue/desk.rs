//! **The `queue` tool's daemon half** (R76 item 2) — what a session's model can do about its
//! project's queues: read them, re-gate the target, restart or withdraw a row, and PROPOSE the
//! branch the project lands on.
//!
//! The operator, on pg-noop's red `main`: *"it should be able to get out of it itself
//! obviously"*. Every verb here is one the operator already had (`mergequeue::verbs`), moved under
//! the session's own name (`agent:<session>`) so the history says a model did it. One is not:
//! **setting `[queue] target` is the operator's decision**, so `propose_target` raises it on every
//! hub, naming the exact `leticode.toml` line, records the proposal in the target's history, and
//! writes no file.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::registry::Registry;
use letibot_tokencore::store::{HealthState, Store};
use letibot_tools::builtins::queue::QueueDesk;

use super::gate::now_ms;
use super::{git, notices, settings, verbs};

/// **The repository a session's queue verbs are about, and its target** — the one `workspace`
/// means (`repo_of_workspace`), keyed the way the queues key it, unless `named` is a path; and
/// the project's `[queue] target` (else `main`/`master`) when no target is named.
pub fn resolve(
    workspace: &Path,
    named: Option<&str>,
    target: Option<&str>,
) -> Result<(String, String), String> {
    let repo = match named {
        Some(p) => git::repo_of_worktree(Path::new(p))
            .ok_or_else(|| format!("{p} is not a git repository"))?,
        None => git::repo_of_workspace(workspace)
            .and_then(|r| git::repo_of_worktree(&r))
            .ok_or_else(|| {
                // **A project of several repositories names them**, so the one to name is a
                // choice among the right ones rather than a path to go and find.
                let many = git::repos_of_workspace(workspace);
                if many.len() > 1 {
                    let names: Vec<String> =
                        many.iter().map(|r| format!("`{}`", r.display())).collect();
                    return format!(
                        "the workspace {} is one project of {} repositories — {} — so which one is \
                         not something this can know; name it (`/queue regate /path/to/repo \
                         [TARGET]`, or the tool's `repository`)",
                        workspace.display(),
                        many.len(),
                        names.join(", ")
                    );
                }
                format!(
                    "the workspace {} names no one git repository; name it (`/queue regate \
                     /path/to/repo [TARGET]`, or the tool's `repository`)",
                    workspace.display()
                )
            })?,
    };
    let target = match target {
        Some(t) => t.to_string(),
        None => {
            let s = settings::for_repository(&repo).unwrap_or_default();
            settings::target_for(&repo, &s).ok_or_else(|| {
                format!("{} has no target branch to gate; name one", repo.display())
            })?
        }
    };
    Ok((repo.display().to_string(), target))
}

/// **One session's desk** — its store, its workspace, and the hubs a move is announced on.
pub struct SessionQueue {
    pub store_path: PathBuf,
    pub session: String,
    pub workspace: PathBuf,
    pub registry: Arc<Registry>,
}

impl SessionQueue {
    fn store(&self) -> Result<Store, String> {
        Store::open(&self.store_path).map_err(|e| e.to_string())
    }

    fn who(&self) -> String {
        format!("agent:{}", self.session)
    }

    fn events(&self) -> impl Fn(SessionEvent) + '_ {
        move |e| {
            self.registry.broadcast(e);
        }
    }
}

impl QueueDesk for SessionQueue {
    fn status(&self, all: bool) -> Result<String, String> {
        let store = self.store()?;
        let mine = resolve(&self.workspace, None, None).ok().map(|(r, _)| r);
        status(&store, if all { None } else { mine.as_deref() })
    }

    fn regate(&self, repository: Option<&str>, target: Option<&str>) -> Result<String, String> {
        let (repo, target) = resolve(&self.workspace, repository, target)?;
        verbs::regate(&self.store()?, &repo, &target, &self.who(), &self.events())
    }

    fn restart(&self, id: &str) -> Result<String, String> {
        verbs::restart_by(&self.store()?, id, &self.who(), &self.events())
    }

    fn withdraw(&self, id: &str, why: &str) -> Result<String, String> {
        let words = format!("withdrawn by {}: {why}", self.who());
        verbs::withdraw_by(&self.store()?, id, &self.who(), &words, &self.events())
    }

    fn propose_target(&self, target: &str, why: &str) -> Result<String, String> {
        propose_target(
            &self.store()?,
            &self.workspace,
            target,
            why,
            &self.who(),
            &self.events(),
        )
    }
}

/// **Put `[queue] target` to the operator** — raised on every hub as `queue_target_proposed`
/// with the exact lines and the file, recorded in the target's history; nothing is written.
pub fn propose_target(
    store: &Store,
    workspace: &Path,
    target: &str,
    why: &str,
    who: &str,
    events: &dyn Fn(SessionEvent),
) -> Result<String, String> {
    let (repo, now) = resolve(workspace, None, None)?;
    let repo_path = Path::new(&repo);
    if git::branch_head(repo_path, target).is_none() {
        return Err(format!("`{target}` is not a branch of {repo}"));
    }
    if now == target {
        return Err(format!("the queue already lands {repo} on `{target}`"));
    }
    let file = settings::for_repository(repo_path)
        .ok()
        .and_then(|s| s.path)
        .unwrap_or_else(|| repo_path.join("leticode.toml"));
    let detail = format!(
        "{who} PROPOSES that {repo} lands on `{target}` (the queue gates `{now}` now): {why}. \
         It is your decision and nothing was written. To take it, put in {}:\n\n[queue]\ntarget = \
         \"{target}\"\n\nThe queue reads it on its next pass; `/queue regate {repo} {target}` \
         then gates the new target's tip.",
        file.display()
    );
    events(SessionEvent::Warning {
        code: "queue_target_proposed".into(),
        detail: detail.clone(),
        compaction: None,
    });
    store
        .record_transition(
            "target",
            target,
            &repo,
            None,
            "proposed",
            who,
            &format!("proposed as the target instead of `{now}`: {why}"),
            now_ms(),
        )
        .map_err(|e| e.to_string())?;
    Ok(format!(
        "Put to the operator (nothing was written): `[queue] target = \"{target}\"` in {}. Until \
         they take it, the queue gates `{now}`.",
        file.display()
    ))
}

/// **Both queues and the targets, as a model reads them** — `repository` narrows to one.
pub fn status(store: &Store, repository: Option<&str>) -> Result<String, String> {
    let here = |r: &str| repository.is_none_or(|k| k == r);
    let mut out = String::new();
    for h in store.target_healths().map_err(|e| e.to_string())? {
        if !here(&h.repository) {
            continue;
        }
        out.push_str(&format!(
            "target `{}` in {}: {} at {}\n  {}\n",
            h.target,
            h.repository,
            h.state.as_str().to_uppercase(),
            h.sha.chars().take(12).collect::<String>(),
            h.evidence.lines().next().unwrap_or_default()
        ));
        if h.state != HealthState::Green
            && let Some((n, of, s, tail)) = notices::failing_step(&h.gate_steps)
        {
            out.push_str(&format!(
                "  failing step {n} of {of}: `{}`; the end of its output:\n{}\n",
                s.command,
                tail.lines()
                    .map(|l| format!("    | {l}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
        }
    }
    let reviews = store.review_rows(None).map_err(|e| e.to_string())?;
    let open_reviews: Vec<_> = reviews
        .iter()
        .filter(|r| here(&r.repository))
        .filter(|r| {
            r.state.is_open()
                || matches!(
                    r.state,
                    letibot_tokencore::store::ReviewState::Rejected
                        | letibot_tokencore::store::ReviewState::CannotDecide
                )
        })
        .collect();
    out.push_str(&format!(
        "review queue: {} row(s) open or parked\n",
        open_reviews.len()
    ));
    for r in open_reviews {
        out.push_str(&format!(
            "  `{}` {} `{}` → `{}` at {}: {}\n",
            r.id,
            r.state.as_str(),
            r.branch,
            r.target,
            r.head_sha.chars().take(12).collect::<String>(),
            if r.failure.is_empty() {
                &r.evidence
            } else {
                &r.failure
            }
        ));
    }
    let merges = store.merge_rows(None).map_err(|e| e.to_string())?;
    let open_merges: Vec<_> = merges
        .iter()
        .filter(|m| here(&m.repository))
        .filter(|m| {
            m.state.is_open()
                || matches!(
                    m.state,
                    letibot_tokencore::store::MergeState::Failed
                        | letibot_tokencore::store::MergeState::TimedOut
                        | letibot_tokencore::store::MergeState::Conflict
                )
        })
        .collect();
    out.push_str(&format!(
        "merge queue: {} row(s) open or parked\n",
        open_merges.len()
    ));
    for m in open_merges {
        out.push_str(&format!(
            "  `{}` {} `{}` → `{}`: {}\n",
            m.id,
            m.state.as_str(),
            m.branch,
            m.target,
            m.evidence.lines().next().unwrap_or_default()
        ));
    }
    if let Ok(leases) = store.queue_leases() {
        for l in leases.iter().filter(|l| here(&l.repository)) {
            let age = now_ms().saturating_sub(l.heartbeat_ms) / 1000;
            out.push_str(&format!(
                "served by {} ({}, heartbeat {age}s ago{})\n",
                l.owner,
                l.build,
                if age * 1000 >= super::lease::LEASE_TTL_MS {
                    " — STALE: nobody is serving it"
                } else {
                    ""
                }
            ));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mergequeue::serve::tests::{TempDir, commit, repo, sh};
    use letibot_tokencore::store::{
        MergeGateOutcome, MergeGateStep, MergePriority, NewReview, ReviewKind, ReviewState,
        TargetHealth,
    };
    use std::sync::Mutex;

    /// **The model's way out, end to end against a real repository and a temp store** (R76 item
    /// 2): it reads the red target with the failing step's tail, re-gates it, withdraws a stale
    /// row under its own name, and PROPOSES the target — which writes no file and puts the exact
    /// line to the operator. FAILS before R76: there was no such door at all.
    #[test]
    fn the_model_reads_regates_withdraws_and_only_proposes_the_target() {
        let dir = TempDir::new("desk");
        let repo = repo(&dir.0);
        sh(&repo, &["branch", "feat/memory-engine"]);
        let tip = commit(&repo, "b.txt", "b\n", "tip");
        let key = repo.display().to_string();
        let db = dir.0.join("sessions.db");
        let store = Store::open(&db).unwrap();
        store
            .set_target_health(
                &TargetHealth {
                    repository: key.clone(),
                    target: "main".into(),
                    sha: tip.clone(),
                    state: HealthState::Red,
                    checked_ms: 1,
                    evidence: "`scripts/merge_gate.sh` failed".into(),
                    gate_steps: vec![MergeGateStep {
                        command: "scripts/merge_gate.sh".into(),
                        outcome: MergeGateOutcome::Failed,
                        output: "cargo build ok\nFAIL: no binary at target/release/letidb".into(),
                        started_ms: 0,
                        elapsed_ms: 0,
                    }],
                },
                "test",
            )
            .unwrap();
        store
            .enqueue_review(
                &NewReview {
                    id: "red-main-ec3cb01e8a6f".into(),
                    repository: key.clone(),
                    target: "main".into(),
                    kind: ReviewKind::Target,
                    session_id: String::new(),
                    branch: "main".into(),
                    worktree: None,
                    base_sha: String::new(),
                    head_sha: "ec3cb01e8a6f".into(),
                    brief: "red".into(),
                    priority: MergePriority::Urgent,
                    fixes: None,
                    evidence: "red".into(),
                },
                "test",
                1,
            )
            .unwrap();
        let desk = SessionQueue {
            store_path: db.clone(),
            session: "s-project".into(),
            workspace: dir.0.clone(),
            registry: letibot_sessionlog::registry::Registry::new(),
        };

        let said = desk.status(false).unwrap();
        assert!(
            said.contains("target `main`") && said.contains("RED"),
            "{said}"
        );
        assert!(
            said.contains("`scripts/merge_gate.sh`") && said.contains("| FAIL: no binary"),
            "the failing step and its tail: {said}"
        );
        assert!(said.contains("red-main-ec3cb01e8a6f"), "{said}");

        let said = desk.regate(None, None).unwrap();
        assert!(said.contains("`main`"), "{said}");
        let h = store.target_health(&key, "main").unwrap().unwrap();
        assert_eq!(h.state, HealthState::Gating);
        assert!(h.evidence.contains("agent:s-project"), "{}", h.evidence);

        desk.withdraw("red-main-ec3", "its commit was amended away")
            .unwrap();
        let r = store.review_row("red-main-ec3cb01e8a6f").unwrap().unwrap();
        assert_eq!(r.state, ReviewState::Withdrawn);
        let moves = store.transitions("review", &r.id).unwrap();
        let last = moves.last().unwrap();
        assert_eq!(last.by, "agent:s-project");
        assert!(last.why.contains("amended away"), "{}", last.why);

        // **Proposed, not set**: the operator is shown the exact line, and no file is written.
        let raised = Mutex::new(Vec::new());
        let said = propose_target(
            &store,
            &dir.0,
            "feat/memory-engine",
            "main builds pgnoop; the gate names letidb",
            "agent:s-project",
            &|e| raised.lock().unwrap().push(e),
        )
        .unwrap();
        assert!(said.contains("nothing was written"), "{said}");
        assert!(!repo.join("leticode.toml").exists(), "no config written");
        let raised = raised.lock().unwrap();
        let detail = raised
            .iter()
            .find_map(|e| match e {
                SessionEvent::Warning { code, detail, .. } if code == "queue_target_proposed" => {
                    Some(detail.clone())
                }
                _ => None,
            })
            .expect("put to the operator on the hubs");
        assert!(
            detail.contains("[queue]\ntarget = \"feat/memory-engine\"")
                && detail.contains("leticode.toml"),
            "{detail}"
        );
        assert!(
            propose_target(&store, &dir.0, "no-such-branch", "x", "a", &|_| {}).is_err(),
            "a branch that does not exist is not proposed"
        );
    }
}
