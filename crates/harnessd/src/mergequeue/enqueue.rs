//! A session puts a branch it holds up for review. The row is the one a finished child gets
//! (`review::enqueue_finished`), and the checks are the ones the queue would apply to it later,
//! said at the door.

use std::path::Path;

use letibot_tokencore::store::{
    MergePriority, NewReview, ReviewKind, ReviewRow, ReviewState, Store,
};

use super::{gate, git, settings};

fn slug(branch: &str) -> String {
    let s: String = branch
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    s.trim_matches('-').chars().take(40).collect()
}

/// Put `branch` of `repo` up for review onto `target` (the project's when `None`), for `brief`.
/// Every refusal names what is missing and changes nothing.
pub fn enqueue_branch(
    store: &Store,
    repo: &Path,
    branch: &str,
    target: Option<&str>,
    brief: &str,
    session: &str,
    now: u64,
) -> Result<ReviewRow, String> {
    let key = repo.display().to_string();
    let branch = branch.trim();
    if brief.trim().is_empty() {
        return Err("say what the work is for (`for`): the reviewer judges the branch against it".into());
    }
    let configured = settings::for_repository(repo)
        .ok()
        .and_then(|s| settings::target_for(repo, &s));
    let Some(target) = target.map(str::to_string).or(configured.clone()) else {
        return Err(format!(
            "{key} has no target branch: name one with `target`"
        ));
    };
    let Some(tip) = git::branch_head(repo, &target) else {
        return Err(format!(
            "target `{target}` is not a branch of {key}{}",
            configured
                .map(|c| format!(" (the project lands on `{c}`)"))
                .unwrap_or_default()
        ));
    };
    if branch == target {
        return Err(format!("`{branch}` is the target itself; name the branch that is to land on it"));
    }
    match gate::declared_gate(repo, &tip) {
        Ok(Some(_)) => {}
        Ok(None) => {
            return Err(format!(
                "`{target}` has no `## Merge gate` section in AGENTS.md at {tip}, so nothing \
                 could judge or land the branch. Propose one to the operator first (`merge_gate`)"
            ));
        }
        Err(e) => return Err(format!("the gate could not be read from `{target}`: {e}")),
    }
    let Some(head) = git::branch_head(repo, branch) else {
        return Err(format!(
            "branch `{branch}` is not in {key}: the queue reads local branches, so fetch or \
             create it there first"
        ));
    };
    let remotes = git::git(repo, &["remote"]).unwrap_or_default();
    if !remotes.trim().is_empty() {
        let held = git::git(repo, &["branch", "-r", "--contains", &head]).unwrap_or_default();
        if held.trim().is_empty() {
            return Err(format!(
                "branch `{branch}` at {head} is not pushed: no remote branch contains it. Push \
                 it, then enqueue"
            ));
        }
    }
    if git::is_ancestor(repo, &head, &tip).unwrap_or(false) {
        return Err(format!(
            "`{branch}` is already on `{target}` (its head {head} is in the target's history), \
             so there is nothing to review"
        ));
    }
    let id = format!("{}-{}", slug(branch), head.chars().take(8).collect::<String>());
    let existing = store.review_rows(Some(&key)).map_err(|e| e.to_string())?;
    if let Some(r) = existing
        .iter()
        .find(|r| r.id == id || (r.branch == branch && r.state.is_open()))
    {
        return Err(format!(
            "`{branch}` is already held as `{}` ({}): {}. `queue` with `action: \"status\"` shows \
             why it is where it is",
            r.id,
            r.state.as_str(),
            r.evidence.lines().next().unwrap_or_default()
        ));
    }
    if let Some(r) = existing
        .iter()
        .find(|r| r.branch == branch && r.state == ReviewState::Approved)
    {
        return Err(format!(
            "`{branch}` is approved as `{}` and waiting to land",
            r.id
        ));
    }
    let base = git::git(repo, &["merge-base", &tip, &head])
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let new = NewReview {
        id: id.clone(),
        repository: key,
        target: target.clone(),
        kind: ReviewKind::Branch,
        session_id: session.to_string(),
        branch: branch.to_string(),
        worktree: None,
        base_sha: base,
        head_sha: head,
        brief: brief.trim().to_string(),
        priority: MergePriority::Subagent,
        fixes: None,
        evidence: format!("put up for review onto `{target}` by {session}"),
    };
    let born = store
        .enqueue_review(&new, session, now)
        .map_err(|e| format!("the review row could not be written: {e}"))?;
    if !born {
        return Err(format!("`{id}` is already in the queue"));
    }
    let row = store
        .review_row(&id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("the review row `{id}` was written and read back as nothing"))?;
    Ok(row)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mergequeue::serve::tests::{TempDir, commit, repo, sh};

    fn go(store: &Store, repo: &Path, branch: &str, target: Option<&str>) -> Result<ReviewRow, String> {
        enqueue_branch(store, repo, branch, target, "wire the thing", "s-me", 5)
    }

    fn fixture(name: &str) -> (TempDir, std::path::PathBuf, Store) {
        let dir = TempDir::new(name);
        let repo = repo(&dir.0);
        let store = Store::open(&dir.0.join("sessions.db")).unwrap();
        (dir, repo, store)
    }

    #[test]
    fn a_branch_with_commits_is_put_up_and_each_missing_thing_is_named() {
        let (_dir, repo, store) = fixture("enqueue");
        sh(&repo, &["checkout", "-q", "-b", "feat/x"]);
        let head = commit(&repo, "x.txt", "x\n", "work");
        sh(&repo, &["checkout", "-q", "main"]);

        let q = go(&store, &repo, "feat/x", None).expect("queued");
        assert_eq!(q.state, ReviewState::Waiting);
        assert_eq!(q.target, "main");
        assert_eq!(q.head_sha, head);
        assert_eq!(q.session_id, "s-me");
        assert_eq!(q.brief, "wire the thing");

        let held = go(&store, &repo, "feat/x", None).unwrap_err();
        assert!(held.contains("already held") && held.contains(&q.id), "{held}");

        let none = enqueue_branch(&store, &repo, "feat/x", None, " ", "s-me", 5).unwrap_err();
        assert!(none.contains("what the work is for"), "{none}");

        let target = go(&store, &repo, "feat/x", Some("nope")).unwrap_err();
        assert!(target.contains("`nope` is not a branch"), "{target}");

        let missing = go(&store, &repo, "feat/none", None).unwrap_err();
        assert!(missing.contains("`feat/none` is not in"), "{missing}");

        let landed = go(&store, &repo, "main", Some("feat/x")).unwrap_err();
        assert!(landed.contains("already on"), "{landed}");
    }

    #[test]
    fn a_target_without_a_merge_gate_is_refused_by_name() {
        let (_dir, repo, store) = fixture("enqueue-nogate");
        sh(&repo, &["checkout", "-q", "-b", "ungated"]);
        commit(&repo, "AGENTS.md", "# agents\n", "drop the gate");
        sh(&repo, &["checkout", "-q", "-b", "feat/y", "main"]);
        commit(&repo, "y.txt", "y\n", "work");
        let why = go(&store, &repo, "feat/y", Some("ungated")).unwrap_err();
        assert!(why.contains("no `## Merge gate` section"), "{why}");
    }

    #[test]
    fn an_unpushed_branch_is_refused_until_a_remote_holds_it() {
        let (dir, repo, store) = fixture("enqueue-push");
        let remote = dir.0.join("remote.git");
        sh(&dir.0, &["init", "-q", "--bare", &remote.to_string_lossy()]);
        sh(&repo, &["remote", "add", "origin", &remote.to_string_lossy()]);
        sh(&repo, &["push", "-q", "origin", "main"]);
        sh(&repo, &["checkout", "-q", "-b", "feat/z"]);
        commit(&repo, "z.txt", "z\n", "work");
        sh(&repo, &["checkout", "-q", "main"]);
        let why = go(&store, &repo, "feat/z", None).unwrap_err();
        assert!(why.contains("not pushed"), "{why}");
        sh(&repo, &["push", "-q", "origin", "feat/z"]);
        go(&store, &repo, "feat/z", None).expect("pushed, so queued");
    }
}
