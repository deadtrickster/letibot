//! Settling an owner's work: keep it in `refs/letibot/archive/<owner>/`, then remove it, then put
//! the session away (`docs/lifecycle.md` d.3's settle triggers and d.8's procedure).
//!
//! The order inside one owner is the design's: compute (G2), then work, then caches and records
//! (G4). This file is the middle step, and it is the only place a `work` holding is removed - with
//! the archive written and read back in the same pass, or not at all. Nothing here acts on a live
//! owner, and nothing here acts inside a grace.

use std::path::PathBuf;

use letibot_tokencore::store::{
    HoldingClass, HoldingRow, HoldingState, OwnerRow, OwnerState, ReapAct, ReapEvent,
    Result as StoreResult, Store, StoreError,
};

use super::plan::{LANDED_GRACE_MS, UNLANDED_GRACE_MS, stamp_of};
use super::reap::{ATTEMPTS_BEFORE_STUCK, Ops, Pass};
use super::{Found, Places, WORKTREE, discover_all, human_millis, worktree};
use crate::git;

/// The kind word a settled BRANCH holding carries: a branch whose worktree is already gone.
pub const BRANCH: &str = "branch";

/// When an owner's work may go: the grace d.3 puts after a close, counted from the close. `None`
/// for a state this step never acts on - a live owner, or one whose step already failed.
pub fn due_at(owner: &OwnerRow) -> Option<i64> {
    match owner.state {
        OwnerState::Settling | OwnerState::Settled => Some(i64::MIN),
        OwnerState::Closed => {
            let base = owner.closed_ms.unwrap_or(owner.created_ms);
            Some(match &owner.landed_sha {
                Some(_) => base + LANDED_GRACE_MS,
                None => base + UNLANDED_GRACE_MS,
            })
        }
        _ => None,
    }
}

/// Why a closed owner's work is not gone yet, for a plan line.
pub fn kept_until(owner: &OwnerRow) -> Option<String> {
    if owner.state != OwnerState::Closed {
        return None;
    }
    let at = due_at(owner)?;
    let why = match &owner.landed_sha {
        Some(sha) => format!("its work landed at {}", worktree::short(sha)),
        None => "its work is not proved landed".to_string(),
    };
    Some(format!("{why}; the grace ends at {}", stamp_of(at)))
}

/// Move a closed owner to `settling` now - what `letibot gc --settle OWNER` does, so the next pass
/// acts without waiting out the grace.
pub fn set_settling(store: &Store, owner: &OwnerRow, now: i64, why: &str) -> StoreResult<()> {
    store.set_owner_state(&owner.id, OwnerState::Settling, why, now)
}

/// The work half of one owner, called from the pass between compute and caches: a process may
/// still be writing in the tree until the compute half is done.
pub(super) fn settle(
    store: &Store,
    places: &Places,
    ops: &Ops,
    owner: &OwnerRow,
    now: i64,
    report: &mut Pass,
) -> StoreResult<()> {
    let Some(at) = due_at(owner) else {
        return Ok(());
    };
    if owner.state == OwnerState::Closed {
        if now < at {
            if let Some(why) = kept_until(owner) {
                report
                    .kept
                    .push(format!("{}: its work is kept - {why}", owner.id));
            }
            return Ok(());
        }
        let why = match &owner.landed_sha {
            Some(sha) => format!(
                "its work landed at {} and {} has passed",
                worktree::short(sha),
                human_millis(LANDED_GRACE_MS as u64)
            ),
            None => format!(
                "it was closed {} ago and is not proved landed; its work is archived before it is \
                 removed",
                human_millis(UNLANDED_GRACE_MS as u64)
            ),
        };
        store.set_owner_state(&owner.id, OwnerState::Settling, &why, now)?;
        let said = format!("settling: {why}");
        report.acts.push(journal(
            store,
            ops,
            owner,
            "",
            "",
            ReapAct::Plan,
            &said,
            now,
        )?);
    }
    // The row as the state move left it: `settling` and `settled` both act.
    let owner = &store.owner(&owner.id)?.unwrap_or_else(|| owner.clone());
    let rows = store.holdings_of(&owner.id)?;
    let mut left = 0usize;
    let mut failed = 0usize;
    // `discover_all`, not `discover`: an adopted worktree sits outside the owner root and only
    // its row can find it.
    for found in discover_all(store, owner, places) {
        if found.class != HoldingClass::Work || found.host.is_some() {
            continue;
        }
        let row = rows
            .iter()
            .find(|r| r.kind == found.kind && r.handle == found.handle && r.host == found.host)
            .cloned();
        let settled = match found.kind.as_str() {
            WORKTREE => settle_worktree(store, places, ops, owner, &found, &row, now, report),
            BRANCH => settle_branch(store, ops, owner, &found, &row, now, report),
            other => {
                left += 1;
                report.kept.push(format!(
                    "{} {}: a `{other}` holding is not this step's to remove",
                    owner.id, found.handle
                ));
                Ok(true)
            }
        };
        match settled {
            Ok(true) => {}
            Ok(false) => left += 1,
            Err(e) => {
                failed += 1;
                stuck(store, ops, owner, &found, &row, &e.to_string(), now, report)?;
            }
        }
    }
    // `Settled` is the state a previous pass reached: it said its one line then, and a pass that
    // said it again every minute would fill the journal with the same sentence.
    if left == 0 && failed == 0 && owner.state == OwnerState::Settling {
        settled(store, ops, owner, now, report)?;
    }
    Ok(())
}

/// One worktree: prove it is the owner's, archive it, and only then remove it. `Ok(false)` is
/// *kept*; `Err` is *tried and it did not work*.
// Every argument is one of the pass's own (store, places, hands, owner, what was found, the row,
// the clock, the report): a struct for them would be a second spelling of `Pass`.
#[allow(clippy::too_many_arguments)]
fn settle_worktree(
    store: &Store,
    places: &Places,
    ops: &Ops,
    owner: &OwnerRow,
    found: &Found,
    row: &Option<HoldingRow>,
    now: i64,
    report: &mut Pass,
) -> StoreResult<bool> {
    let path = PathBuf::from(&found.handle);
    if !path.exists() {
        return Ok(true);
    }
    // The owner root's own `wt/` directory is not a worktree: it goes when nothing is left in it.
    let wt = places.owner_root(&owner.id).join("wt");
    if path == wt {
        if worktrees_under(&wt).is_empty() {
            let _ = std::fs::remove_dir(&wt);
            return Ok(true);
        }
        report.kept.push(format!(
            "{} {}: it holds {} worktree(s), each settled on its own",
            owner.id,
            found.handle,
            worktrees_under(&wt).len()
        ));
        return Ok(false);
    }
    let Some(repo) = owner.repo.as_deref().map(PathBuf::from) else {
        report.kept.push(format!(
            "{} {}: this owner names no repository to archive it into",
            owner.id, found.handle
        ));
        return Ok(false);
    };
    let branch = worktree::branch_of(&path);
    let Some(proved) = worktree::proved(&repo, &path, branch.as_deref(), &owner.id) else {
        report.kept.push(format!(
            "{} {}: nothing in it names this owner (a stamp file in its git directory, a \
             `branch.<name>.letibot-owner` key, or its own HEAD log), so this step does not touch it",
            owner.id, found.handle
        ));
        return Ok(false);
    };
    let kept = worktree::archive_and_remove(places, &repo, &path, &owner.id, branch.as_deref())
        .map_err(StoreError::Refused)?;
    let ns = super::namespace(&owner.id, "wt");
    let said = format!(
        "its worktree is in {ns} ({}) - `letibot gc --restore {} --worktree` brings it back; \
         {proved}",
        kept.said(),
        owner.id
    );
    write_gone(store, found, row, &ns, &said, now)?;
    report.acts.push(journal(
        store,
        ops,
        owner,
        &found.kind,
        &found.handle,
        ReapAct::Archive,
        &said,
        now,
    )?);
    Ok(true)
}

/// One branch whose worktree is already gone: its commits go to the archive, then the ref.
#[allow(clippy::too_many_arguments)]
fn settle_branch(
    store: &Store,
    ops: &Ops,
    owner: &OwnerRow,
    found: &Found,
    row: &Option<HoldingRow>,
    now: i64,
    report: &mut Pass,
) -> StoreResult<bool> {
    let Some(repo) = owner.repo.as_deref().map(PathBuf::from) else {
        report.kept.push(format!(
            "{} {}: this owner names no repository, so a branch of it cannot be proved or kept",
            owner.id, found.handle
        ));
        return Ok(false);
    };
    let branch = &found.handle;
    if git::branch_head(&repo, branch).is_none() {
        return Ok(true);
    }
    if worktree::proved(&repo, &repo, Some(branch), &owner.id).is_none()
        && worktree::stamped_owner(&repo).as_deref() != Some(&owner.id)
    {
        report.kept.push(format!(
            "{} {branch}: nothing proves this branch is the owner's (no \
             `branch.{branch}.letibot-owner` key), so it stays",
            owner.id
        ));
        return Ok(false);
    }
    let kept = worktree::archive_branch(&repo, &owner.id, branch).map_err(StoreError::Refused)?;
    let ns = super::namespace(&owner.id, "wt");
    let said = format!(
        "the branch `{branch}` is in {ns} ({}) and gone - `letibot gc --restore {}` puts it back",
        kept.said(),
        owner.id
    );
    write_gone(store, found, row, &ns, &said, now)?;
    report.acts.push(journal(
        store,
        ops,
        owner,
        &found.kind,
        &found.handle,
        ReapAct::Archive,
        &said,
        now,
    )?);
    Ok(true)
}

/// The holding row after its work is archived: `gone`, with where it is, which is what the plan
/// and `--drop-archive` read.
fn write_gone(
    store: &Store,
    found: &Found,
    row: &Option<HoldingRow>,
    ns: &str,
    said: &str,
    now: i64,
) -> StoreResult<()> {
    let mut wrote = row.clone().unwrap_or_else(|| found.row(now));
    wrote.state = HoldingState::Gone;
    wrote.archive = Some(ns.to_string());
    wrote.why = Some(said.to_string());
    wrote.checked_ms = Some(now);
    store.put_holding(&wrote)
}

/// A step that failed: the holding counts the attempt, and the second one is `stuck` - said in the
/// journal and once in the plan, and nothing further removed for it.
#[allow(clippy::too_many_arguments)]
fn stuck(
    store: &Store,
    ops: &Ops,
    owner: &OwnerRow,
    found: &Found,
    row: &Option<HoldingRow>,
    why: &str,
    now: i64,
    report: &mut Pass,
) -> StoreResult<()> {
    let already = row.as_ref().is_some_and(|r| r.state == HoldingState::Stuck);
    let mut wrote = row.clone().unwrap_or_else(|| found.row(now));
    wrote.attempts += 1;
    wrote.state = if wrote.attempts >= ATTEMPTS_BEFORE_STUCK {
        HoldingState::Stuck
    } else {
        HoldingState::Held
    };
    wrote.why = Some(why.to_string());
    wrote.checked_ms = Some(now);
    store.put_holding(&wrote)?;
    let said = format!(
        "{} was NOT archived and is still there ({} attempt(s)): {why}",
        found.handle, wrote.attempts
    );
    report.kept.push(format!("{} {said}", owner.id));
    // One line when the holding becomes stuck, and no more: the plan keeps saying it, and a pass
    // that journalled the same failure every minute is a journal nobody can read.
    if already {
        return Ok(());
    }
    let what = if wrote.state == HoldingState::Stuck {
        ReapAct::Stuck
    } else {
        ReapAct::Keep
    };
    report.acts.push(journal(
        store,
        ops,
        owner,
        &found.kind,
        &found.handle,
        what,
        &said,
        now,
    )?);
    Ok(())
}

/// The owner has no work left: the session goes out of sight with the reason, and its rows are
/// never touched (R184's remainder).
fn settled(
    store: &Store,
    ops: &Ops,
    owner: &OwnerRow,
    now: i64,
    report: &mut Pass,
) -> StoreResult<()> {
    let why = match &owner.landed_sha {
        Some(sha) => format!("settled: landed {}", worktree::short(sha)),
        None => "settled: archived unlanded".to_string(),
    };
    store.set_owner_state(&owner.id, OwnerState::Settled, &why, now)?;
    // The session goes out of sight with the reason; its rows and transcripts stay (R184). An
    // owner id that is not a session row (a gate run) simply has nothing to archive.
    let archived =
        store.set_archived(&owner.id, true, Some(&why)).is_ok() && archived_ok(store, &owner.id);
    let tail = if archived {
        ", which is archived and out of the list"
    } else {
        ", which this store keeps as it was"
    };
    let said = format!(
        "settled: its work is under {}/ - `leticode_send` reopens the conversation{tail}",
        super::namespace(&owner.id, "wt")
    );
    report.acts.push(journal(
        store,
        ops,
        owner,
        "session",
        &owner.id,
        ReapAct::Archive,
        &said,
        now,
    )?);
    Ok(())
}

/// Whether the session row really carries the archive stamp now - an act is done when the thing is
/// there, not when the call returned.
fn archived_ok(store: &Store, id: &str) -> bool {
    store
        .session(id)
        .ok()
        .flatten()
        .is_some_and(|r| r.archived_ms.is_some())
}

/// The worktrees under one `wt/` directory.
fn worktrees_under(wt: &std::path::Path) -> Vec<PathBuf> {
    worktree::under(wt.parent().unwrap_or(wt))
}

/// One journal line. A removal nobody can read back afterwards is one nobody can argue with, so
/// every act goes through here.
#[allow(clippy::too_many_arguments)]
fn journal(
    store: &Store,
    ops: &Ops,
    owner: &OwnerRow,
    kind: &str,
    handle: &str,
    what: ReapAct,
    said: &str,
    now: i64,
) -> StoreResult<super::Act> {
    store.put_reap_event(&ReapEvent {
        at_ms: now,
        owner_id: owner.id.clone(),
        kind: (!kind.is_empty()).then(|| kind.to_string()),
        host: None,
        handle: (!handle.is_empty()).then(|| handle.to_string()),
        act: what.word().into(),
        said: said.to_string(),
        by: ops.by.clone(),
    })?;
    Ok(super::Act {
        owner: owner.id.clone(),
        kind: kind.to_string(),
        host: None,
        handle: handle.to_string(),
        act: what,
        said: said.to_string(),
        bytes: 0,
    })
}
