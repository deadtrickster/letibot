//! The disk half of the pass: caches removed at close, shared targets under their quota, firecode's
//! run directories retired, and the remote hosts' own pass (`docs/lifecycle.md`, package G4).
//!
//! A step is done when the thing is ABSENT, not when a command returned, and nothing of class
//! `work` is removed unless its archive was written in the same pass and read back. Everything that
//! is not a local `remove_dir_all` goes through [`Hands`], so a test drives it against a fixture.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use letibot_tokencore::store::{
    HoldingRow, HoldingState, OwnerRow, OwnerState, ReapAct, ReapEvent, Result as StoreResult,
    Store,
};

use super::{Act, Ops, Pass};
use crate::lifecycle::archive::{self, Kept};
use crate::lifecycle::remote::{self, HostPass};
use crate::lifecycle::{COPY, COPY_OUT, Places, STATE, VM, discover, human_bytes, stamp_of};

/// What a pass reaches through. One trait rather than direct calls so a test can point the pass at
/// a fixture: the lock is a real `flock` in this process, `firecode` is a stub script, and `host`
/// is a shell on this machine standing in for another.
pub trait Hands: Send + Sync {
    /// One firecode verb, bounded. `Err` is the verb this firecode does not have as well as one that
    /// failed, because the caller's answer to both is the same: the fallback, said out loud.
    fn firecode(&self, args: &[String]) -> Result<String, String>;
    /// Whether `<target>.rrun.lock` is held by a live process - the lock `rrun-runner` takes beside
    /// the target dir it builds in. `Err` when the box cannot be asked, which keeps the target.
    fn lock_held(&self, target: &Path) -> Result<bool, String>;
    /// One command on `host`, one round trip.
    fn host_command(&self, host: &str, script: &str) -> Result<String, String>;
    /// A file's bytes from `host`, for the one thing that must come home whole: a bundle.
    fn host_read(&self, host: &str, path: &str) -> Result<Vec<u8>, String>;
    /// One request to this box's docker engine, when it has one. The default is a box with none:
    /// a pass that cannot see docker reaps nothing of it, which is the fail-closed direction.
    fn docker(&self, req: &crate::docker::Request) -> Result<crate::docker::Response, String> {
        Err(format!(
            "no docker engine is configured here: {req:?} was not sent - set `[docker] engine` in \
             ~/.config/letibot/leticode.toml, or $DOCKER_HOST"
        ))
    }
}

/// The kind words a cache may have, as the discovery walk gives them: a build target, a session's
/// `/tmp`, a confined home, the tree a VM was booted on, and a firecode state layer.
const CACHE_KINDS: [&str; 5] = ["target", "tmp", "home", COPY, "state"];

/// Is this owner past its compute? The compute half runs first in the same pass, so an owner still
/// `closing` here has a process left that may be writing in its caches, and they wait for it.
pub(super) fn past_compute(owner: &OwnerRow) -> bool {
    matches!(
        owner.state,
        OwnerState::Closed | OwnerState::Settling | OwnerState::Settled | OwnerState::Swept
    )
}

/// Everything one owner holds that this package removes, and a sentence for everything it holds
/// that it does not.
pub(super) fn reap_owner(
    store: &Store,
    places: &Places,
    ops: &Ops,
    owner: &OwnerRow,
    now: i64,
    report: &mut Pass,
) -> StoreResult<()> {
    let rows = store.holdings_of(&owner.id)?;
    let found_all = discover(owner, places);
    let copy_out = copy_out_of(places, owner, &found_all, now);
    for found in found_all {
        if found.host.is_some() {
            // A remote holding is that host's own pass's business: this box cannot see it.
            continue;
        }
        let row = rows
            .iter()
            .find(|r| r.kind == found.kind && r.handle == found.handle && r.host == found.host)
            .cloned()
            .unwrap_or_else(|| found.row(now));
        if let Some(why) = copy_out.keeps(&found.kind) {
            if let CopyOut::Stuck(_) = &copy_out
                && matches!(found.kind.as_str(), VM | COPY)
                && row.state != HoldingState::Stuck
            {
                journal(store, ops, &row.owner_id, &row, ReapAct::Stuck, &why, now)?;
                store.put_holding(&HoldingRow {
                    state: HoldingState::Stuck,
                    why: Some(why.clone()),
                    checked_ms: Some(now),
                    ..row.clone()
                })?;
            }
            report
                .kept
                .push(format!("{} {}: {why}", owner.id, found.handle));
            continue;
        }
        let kept = match (found.class.word(), found.kind.as_str()) {
            ("cache", kind) if CACHE_KINDS.contains(&kind) => {
                remove_cache(store, ops, owner, &row, now, report)?
            }
            ("work", COPY_OUT) => remove_copy_out(store, ops, owner, &row, now, report)?,
            ("record", _) if found.kind == crate::lifecycle::VM => {
                retire_run(store, places, ops, owner, &row, now, report)?
            }
            ("work", _) => Some(format!(
                "its owner is `{}`, and a `work` holding goes only in the pass that archived it \
                 into refs/letibot/archive/{}",
                owner.state.word(),
                owner.id
            )),
            ("record", _) => Some(format!(
                "a record outlives its owner: `{}` stays until its owner is settled",
                found.handle
            )),
            _ => None,
        };
        if let Some(why) = kept {
            report
                .kept
                .push(format!("{} {}: {why}", owner.id, found.handle));
        }
    }
    Ok(())
}

/// How long a closed owner's copy-out may stay `pending` with no process left to finish it.
pub const COPY_OUT_DEADLINE_MS: i64 = 30 * 60 * 1000;

/// Where an owner's VM copy-outs stand. A run whose copy-out is not finished is still compute: the
/// launcher reads the copy and writes into the run dir and the destination beside it.
#[derive(Debug)]
enum CopyOut {
    Finished,
    Running(String),
    Stuck(String),
}

impl CopyOut {
    /// Why a holding of `kind` stays this pass, or `None` when the copy-out does not hold it.
    fn keeps(&self, kind: &str) -> Option<String> {
        if !matches!(kind, VM | COPY | COPY_OUT | STATE) {
            return None;
        }
        match self {
            CopyOut::Finished => None,
            CopyOut::Running(why) => Some(format!("kept: copy-out still running ({why})")),
            CopyOut::Stuck(why) => Some(format!(
                "kept: copy-out did not finish ({why}), so the copy and its run stay until the \
                 operator looks"
            )),
        }
    }
}

/// The owner's runs read off their `copy_out` record (`pending`, `done <path>`, `none`, `failed
/// <why>`) and the processes that may still write it. Any running run outranks a stuck one.
fn copy_out_of(
    places: &Places,
    owner: &OwnerRow,
    found: &[crate::lifecycle::Found],
    now: i64,
) -> CopyOut {
    let Some(runs) = places.firecode_runs.as_deref() else {
        return CopyOut::Finished;
    };
    let fires = crate::firecode::held::firecrackers(&places.proc_root);
    let mut stuck = None;
    for f in found.iter().filter(|f| f.kind == VM && f.host.is_none()) {
        let dir = runs.join(&f.handle);
        let record = dir.join("copy_out");
        let state = std::fs::read_to_string(&record)
            .ok()
            .map(|s| s.trim().to_string());
        let state_word = state.as_deref().unwrap_or("not written");
        if state_word == "none" || state_word.starts_with("done") {
            continue;
        }
        let launcher = std::fs::read_to_string(dir.join("launcher"))
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .filter(|p| crate::firecode::held::alive(&places.proc_root, *p));
        let fire = fires.iter().find(|(r, _)| *r == f.handle).map(|(_, p)| *p);
        if let Some(pid) = launcher.or(fire) {
            return CopyOut::Running(format!(
                "run {}: copy_out `{state_word}`, pid {pid} alive",
                f.handle
            ));
        }
        if state_word.starts_with("failed") {
            stuck.get_or_insert(format!("run {}: copy_out `{state_word}`", f.handle));
            continue;
        }
        if state.is_none() {
            // No record and nothing alive: the run never reached its copy-out.
            continue;
        }
        let since = owner.closed_ms.or_else(|| {
            std::fs::metadata(&record)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
        });
        let past = since.is_some_and(|t| now.saturating_sub(t) >= COPY_OUT_DEADLINE_MS);
        if !past {
            return CopyOut::Running(format!(
                "run {}: copy_out `{state_word}` and no process left yet; it counts as failed {} \
                 min after close",
                f.handle,
                COPY_OUT_DEADLINE_MS / 60_000
            ));
        }
        stuck.get_or_insert(format!(
            "run {}: copy_out still `{state_word}` {} min after close and no process left to \
             finish it",
            f.handle,
            COPY_OUT_DEADLINE_MS / 60_000
        ));
    }
    match stuck {
        Some(why) => CopyOut::Stuck(why),
        None => CopyOut::Finished,
    }
}

/// Remove one cache and verify it is absent. A cache is rebuilt, so a failed removal costs bytes
/// and never work; the second failure is `stuck` and said rather than retried in silence.
fn remove_cache(
    store: &Store,
    ops: &Ops,
    owner: &OwnerRow,
    row: &HoldingRow,
    now: i64,
    report: &mut Pass,
) -> StoreResult<Option<String>> {
    let path = Path::new(&row.handle);
    if !path.exists() {
        return Ok(None);
    }
    // A firecode state layer is firecode's own to remove: it made that filesystem, and an `rm -rf`
    // under its state root is the reap-by-pattern this design refuses.
    if row.kind == "state" {
        let args = vec!["gc".to_string(), "--state".to_string(), row.handle.clone()];
        return Ok(match ops.hands.firecode(&args) {
            Ok(_) if !path.exists() => {
                report
                    .acts
                    .push(act(owner, row, ReapAct::Remove, "its state layer is gone"));
                journal(
                    store,
                    ops,
                    &row.owner_id,
                    row,
                    ReapAct::Remove,
                    "its state layer is gone",
                    now,
                )?;
                None
            }
            Ok(_) => Some(format!(
                "`firecode gc --state {}` answered and the layer is still there: it is untouched \
                 until firecode says what it did",
                row.handle
            )),
            Err(why) => Some(format!(
                "{} is a firecode state layer and this firecode cannot remove it ({why}); that is a \
                 firecode row to file, and the layer is kept",
                row.handle
            )),
        });
    }
    let bytes = row.bytes.unwrap_or(0);
    if std::fs::remove_dir_all(path).is_ok() && !path.exists() {
        let said = format!("its {} is gone", row.kind);
        journal(store, ops, &row.owner_id, row, ReapAct::Remove, &said, now)?;
        store.put_holding(&HoldingRow {
            state: HoldingState::Gone,
            why: Some(said.clone()),
            checked_ms: Some(now),
            ..row.clone()
        })?;
        report
            .acts
            .push(act(owner, row, ReapAct::Remove, &said).with_bytes(bytes));
        return Ok(None);
    }
    if !path.exists() {
        return Ok(None);
    }
    let attempts = row.attempts + 1;
    let said = format!(
        "{} could not be removed ({attempts} attempt(s)); a cache is rebuilt, so this costs bytes \
         and nothing else",
        row.handle
    );
    store.put_holding(&HoldingRow {
        state: if attempts >= 2 {
            HoldingState::Stuck
        } else {
            HoldingState::Held
        },
        attempts,
        why: Some(said.clone()),
        checked_ms: Some(now),
        ..row.clone()
    })?;
    if attempts >= 2 {
        journal(store, ops, &row.owner_id, row, ReapAct::Stuck, &said, now)?;
    }
    Ok(Some(said))
}

/// A copy-out is the only copy of that work until it is in the archive namespace, so: fetch it,
/// read the refs back, and only then remove it. A copy-out that is not a checkout stays, because a
/// `work` holding is never removed on a guess.
fn remove_copy_out(
    store: &Store,
    ops: &Ops,
    owner: &OwnerRow,
    row: &HoldingRow,
    now: i64,
    report: &mut Pass,
) -> StoreResult<Option<String>> {
    let path = Path::new(&row.handle);
    if !path.exists() {
        return Ok(None);
    }
    let Some(repo) = owner.repo.as_deref().map(PathBuf::from) else {
        return Ok(Some(format!(
            "{} is a copy-out and this owner names no repository to archive it into",
            row.handle
        )));
    };
    let tag = copy_out_tag(path);
    let kept: Kept = match archive::keep(&repo, path, &owner.id, &tag) {
        Ok(k) => k,
        Err(why) => {
            return Ok(Some(format!(
                "{} is a copy-out and it was NOT archived, so the only copy stays: {why}",
                row.handle
            )));
        }
    };
    let namespace = archive::namespace(&owner.id, &tag);
    journal(
        store,
        ops,
        &row.owner_id,
        row,
        ReapAct::Archive,
        &format!("its copy-out is in {namespace}: {}", kept.said()),
        now,
    )?;
    let bytes = row.bytes.unwrap_or(0);
    let archived = HoldingRow {
        archive: Some(namespace.clone()),
        checked_ms: Some(now),
        ..row.clone()
    };
    if std::fs::remove_dir_all(path).is_err() && path.exists() {
        store.put_holding(&archived)?;
        return Ok(Some(format!(
            "{} is archived in {namespace} and could not be removed; the archive is already the \
             copy that matters and the next pass tries again",
            row.handle
        )));
    }
    store.put_holding(&HoldingRow {
        state: HoldingState::Gone,
        why: Some(format!("its work is in {namespace}")),
        ..archived
    })?;
    report.acts.push(
        act(
            owner,
            row,
            ReapAct::Remove,
            &format!("its copy-out is in {namespace} and the directory is gone"),
        )
        .with_bytes(bytes),
    );
    Ok(None)
}

/// `s-1-20261112-115500` -> `copy-out/20261112-115500`, so two copy-outs of one owner never
/// overwrite each other's namespace. The owner id is `s-<digits>`, so the stamp is what follows the
/// SECOND dash.
fn copy_out_tag(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stamp = name
        .split_once('-')
        .and_then(|(owner, rest)| owner.starts_with('s').then_some(rest))
        .and_then(|rest| rest.split_once('-').map(|(_, s)| s))
        .filter(|s| !s.is_empty());
    match stamp {
        Some(s) => format!("copy-out/{s}"),
        None => format!("copy-out/{name}"),
    }
}

/// Retire a run directory: `firecode gc --run <id>` when this firecode has the verb, the design's
/// fallback move into `runs-archive/<yyyy-mm>/` otherwise. Both end by looking at `runs/`: a run is
/// retired when firecode no longer has it there to probe, not when a command returned.
fn retire_run(
    store: &Store,
    places: &Places,
    ops: &Ops,
    owner: &OwnerRow,
    row: &HoldingRow,
    now: i64,
    report: &mut Pass,
) -> StoreResult<Option<String>> {
    let Some(runs) = places.firecode_runs.as_deref() else {
        return Ok(None);
    };
    let dir = runs.join(&row.handle);
    if !dir.exists() {
        return Ok(None);
    }
    let live = crate::firecode::held::live_vms(runs, &places.proc_root);
    if live.iter().any(|v| v.run == row.handle) {
        return Ok(Some(format!(
            "run {} still has a firecracker carrying its id: its compute ends first, and this pass \
             does not remove a live VM's directory",
            row.handle
        )));
    }
    let args = vec!["gc".to_string(), "--run".to_string(), row.handle.clone()];
    let verb = ops.hands.firecode(&args);
    let mut said = if !dir.exists() {
        "`firecode gc --run` retired it".to_string()
    } else {
        let Some(parent) = runs.parent() else {
            return Ok(Some(format!(
                "{}: firecode's runs directory has no parent",
                row.handle
            )));
        };
        let month = stamp_of(now).get(..7).unwrap_or("unknown").to_string();
        let into = parent.join("runs-archive").join(&month);
        if let Err(e) = std::fs::create_dir_all(&into) {
            return Ok(Some(format!("{}: {e}", row.handle)));
        }
        let to = into.join(&row.handle);
        if std::fs::rename(&dir, &to).is_err() {
            return Ok(Some(format!(
                "{} could not be moved out of {}: the archive would have to be on another \
                 filesystem, and this pass does not copy a run's gigabytes by hand",
                row.handle,
                runs.display()
            )));
        }
        format!(
            "this firecode has no working `gc --run` ({}), so the run was moved to {}",
            verb.err()
                .unwrap_or_else(|| "it answered and left the directory".into())
                .lines()
                .next()
                .unwrap_or("no answer"),
            to.display()
        )
    };
    if dir.exists() {
        said = format!(
            "{} is still under {}: nothing was retired",
            row.handle,
            runs.display()
        );
        return Ok(Some(said));
    }
    journal(store, ops, &row.owner_id, row, ReapAct::Remove, &said, now)?;
    store.put_holding(&HoldingRow {
        state: HoldingState::Gone,
        why: Some(said.clone()),
        checked_ms: Some(now),
        ..row.clone()
    })?;
    report.acts.push(act(
        owner,
        row,
        ReapAct::Remove,
        &format!("its run directory is retired: {said}"),
    ));
    Ok(None)
}

/// The shared targets, under the lock `rrun-runner` takes beside each. Over quota is a removal;
/// under it, `idle_days` with nothing written is one too. A held lock keeps it and says so, because
/// a build reading that target is not a cache nobody is using.
pub(super) fn shared_targets(places: &Places, ops: &Ops, now: i64, report: &mut Pass) {
    for shared in &places.settings.shared {
        let path = &shared.path;
        if !path.is_dir() {
            continue;
        }
        let (bytes, complete) = crate::lifecycle::size_of(path);
        let bytes = bytes.unwrap_or(0);
        let over = shared.quota.is_some_and(|q| bytes > q);
        let idle = shared
            .idle_days
            .filter(|d| *d > 0)
            .map(|d| {
                let touched = newest_write(path).unwrap_or(0);
                now.saturating_sub(touched) >= (d as i64) * 24 * 60 * 60 * 1000
            })
            .unwrap_or(false);
        if !over && !idle {
            continue;
        }
        let why = if over {
            format!(
                "it is over its quota ({}){}",
                human_bytes(shared.quota.unwrap_or(0)),
                if complete {
                    ""
                } else {
                    ", and its size was not measured whole"
                }
            )
        } else {
            format!(
                "nothing has been written in it for {} day(s)",
                shared.idle_days.unwrap_or(0)
            )
        };
        match ops.hands.lock_held(path) {
            Err(cannot_tell) => report.kept.push(format!(
                "shared target {}: kept, because whether a build holds its lock could not be told \
                 ({cannot_tell}) — not being able to tell is a reason to keep",
                path.display()
            )),
            Ok(true) => report.kept.push(format!(
                "shared target {}: kept, because a build holds its lock ({}.rrun.lock); {why}",
                path.display(),
                path.display()
            )),
            Ok(false) => {
                if std::fs::remove_dir_all(path).is_ok() && !path.exists() {
                    report.acts.push(Act {
                        owner: "shared".into(),
                        kind: "target".into(),
                        host: None,
                        handle: path.display().to_string(),
                        act: ReapAct::Remove,
                        said: format!("{why}, and no build held its lock"),
                        bytes,
                    });
                } else {
                    report.kept.push(format!(
                        "shared target {}: {why} and the lock was free, but it could not be removed",
                        path.display()
                    ));
                }
            }
        }
    }
}

/// The newest write in `path` or one level under it, in unix ms. One level, because cargo touches
/// `debug/` and `release/` on every build and walking a target dir whole is minutes.
fn newest_write(path: &Path) -> Option<i64> {
    let at = |p: &Path| -> Option<i64> {
        let t = std::fs::metadata(p).ok()?.modified().ok()?;
        t.duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_millis() as i64)
    };
    let mut newest = at(path);
    let Ok(rd) = std::fs::read_dir(path) else {
        return newest;
    };
    for entry in rd.flatten() {
        if let Some(t) = at(&entry.path()) {
            newest = Some(newest.map(|n: i64| n.max(t)).unwrap_or(t));
        }
    }
    newest
}

/// Every host this store has holdings on, and its own pass (`docs/lifecycle.md` d.10's remote rows).
/// A host this box cannot reach marks its holdings `lost` and removes nothing of it here; the next
/// pass tries again, which is the whole of the retry.
pub(super) fn remote_hosts(
    store: &Store,
    places: &Places,
    ops: &Ops,
    now: i64,
    only: Option<&str>,
    report: &mut Pass,
) -> StoreResult<()> {
    let mut by_host: BTreeMap<String, Vec<HoldingRow>> = BTreeMap::new();
    for row in store.holdings()? {
        let Some(host) = &row.host else { continue };
        if host == &places.box_id || only.is_some_and(|n| row.owner_id != n) {
            continue;
        }
        by_host.entry(host.clone()).or_default().push(row);
    }
    for (host, rows) in by_host {
        let root = remote::root_of(&rows).unwrap_or_default();
        match remote::host_pass(&host, &root, &*ops.hands, now) {
            Ok(pass) => {
                absorb(store, &pass, now, report)?;
                // The bundle comes home in the same pass when the owner's repository is known
                // here: it is the only copy of that work, and a bundle left on a host is a copy
                // nobody has read. A fetch that fails leaves it there, said.
                let mut collected: Vec<String> = Vec::new();
                for b in &pass.bundles {
                    let Some(repo) = store.owner(&b.owner)?.and_then(|o| o.repo) else {
                        report.kept.push(format!(
                            "{} on {host}: its bundle at {} is the only copy of that work, and this \
                             owner names no repository here to fetch it into",
                            b.owner, b.bundle
                        ));
                        continue;
                    };
                    match remote::collect_bundle(&*ops.hands, &host, Path::new(&repo), b) {
                        Ok(said) => {
                            collected.push(b.owner.clone());
                            report.acts.push(Act {
                                owner: b.owner.clone(),
                                kind: "bundle".into(),
                                host: Some(host.clone()),
                                handle: b.bundle.clone(),
                                act: ReapAct::Archive,
                                said: said.clone(),
                                bytes: 0,
                            });
                            journal(
                                store,
                                ops,
                                &b.owner,
                                &HoldingRow::new(
                                    b.owner.clone(),
                                    "bundle",
                                    letibot_tokencore::store::HoldingClass::Work,
                                    b.bundle.clone(),
                                    "a bundle the host's own pass wrote",
                                    now,
                                ),
                                ReapAct::Archive,
                                &said,
                                now,
                            )?;
                        }
                        Err(why) => report.kept.push(format!(
                            "{} on {host}: the bundle at {} could not be fetched ({why}), so it \
                             stays on that host",
                            b.owner, b.bundle
                        )),
                    }
                }
                for row in rows {
                    let moved = pass.touched(&row.owner_id) || collected.contains(&row.owner_id);
                    store.put_holding(&HoldingRow {
                        state: if moved {
                            HoldingState::Gone
                        } else {
                            HoldingState::Held
                        },
                        why: Some(if moved {
                            "the host's own pass bundled its tree and removed its caches".into()
                        } else {
                            "the host answered: its lease is fresh, or its work is still there"
                                .into()
                        }),
                        checked_ms: Some(now),
                        ..row
                    })?;
                }
            }
            Err(why) => {
                for row in &rows {
                    store.put_holding(&HoldingRow {
                        state: HoldingState::Lost,
                        why: Some(format!("{host} could not be asked: {why}")),
                        checked_ms: Some(now),
                        attempts: row.attempts + 1,
                        ..row.clone()
                    })?;
                    journal(
                        store,
                        ops,
                        &row.owner_id,
                        row,
                        ReapAct::Keep,
                        &format!("{host} could not be asked ({why}), so nothing of it was touched"),
                        now,
                    )?;
                    report
                        .lost
                        .push((row.owner_id.clone(), host.clone(), why.clone()));
                }
            }
        }
    }
    Ok(())
}

/// One host pass, folded into this pass's report and the journal.
fn absorb(store: &Store, pass: &HostPass, now: i64, report: &mut Pass) -> StoreResult<()> {
    for a in &pass.acts {
        report.acts.push(a.clone());
        store.put_reap_event(&ReapEvent {
            at_ms: now,
            owner_id: a.owner.clone(),
            kind: Some(a.kind.clone()),
            host: Some(pass.host.clone()),
            handle: Some(a.handle.clone()),
            act: a.act.word().into(),
            said: a.said.clone(),
            by: format!("the host's own pass on {}", pass.host),
        })?;
    }
    report.kept.extend(pass.kept.iter().cloned());
    for b in &pass.bundles {
        report.kept.push(format!(
            "{}: its tree on {} is bundled at {} — the home box fetches it into \
             refs/letibot/archive/{}/ and the bundle goes after that",
            b.owner, pass.host, b.bundle, b.owner
        ));
    }
    Ok(())
}

fn act(owner: &OwnerRow, row: &HoldingRow, what: ReapAct, said: &str) -> Act {
    Act {
        owner: owner.id.clone(),
        kind: row.kind.clone(),
        host: row.host.clone(),
        handle: row.handle.clone(),
        act: what,
        said: said.to_string(),
        bytes: row.bytes.unwrap_or(0),
    }
}

impl Act {
    fn with_bytes(mut self, bytes: u64) -> Act {
        self.bytes = bytes;
        self
    }
}

/// One journal line. A removal nobody can read back afterwards is one nobody can argue with, so
/// every act goes through here.
fn journal(
    store: &Store,
    ops: &Ops,
    owner: &str,
    row: &HoldingRow,
    act: ReapAct,
    said: &str,
    now: i64,
) -> StoreResult<()> {
    store.put_reap_event(&ReapEvent {
        at_ms: now,
        owner_id: owner.to_string(),
        kind: Some(row.kind.clone()),
        host: row.host.clone(),
        handle: Some(row.handle.clone()),
        act: act.word().into(),
        said: said.to_string(),
        by: ops.by.clone(),
    })
}

/// What a production pass reaches through: the box's own firecode, `/proc/locks`, and one ssh
/// connection per host, cached the way the supervisor caches its probes.
pub struct BoxHands {
    pub firecode: Option<PathBuf>,
    pub proc_locks: PathBuf,
    /// This box's docker engine, when it has one.
    pub docker: Option<PathBuf>,
    /// What this firecode's own `gc --help` says it can do, asked at the first verb and cached:
    /// asking a `gc --run` a CLI does not have costs a round trip and answers nothing.
    probe: std::sync::OnceLock<crate::firecode::Probe>,
    ssh: std::sync::Mutex<BTreeMap<String, std::sync::Arc<crate::remote::Ssh>>>,
}

impl Default for BoxHands {
    fn default() -> Self {
        BoxHands::new()
    }
}

impl BoxHands {
    pub fn new() -> BoxHands {
        BoxHands {
            firecode: crate::firecode::firecode_available().ok(),
            proc_locks: PathBuf::from("/proc/locks"),
            docker: crate::docker::engine_socket(),
            probe: std::sync::OnceLock::new(),
            ssh: std::sync::Mutex::new(BTreeMap::new()),
        }
    }

    /// This firecode's capabilities, asked once: three `--help`s is cheaper than a verb it does not
    /// have, and it is the detection the brief asks for - a CLI without them falls back, said.
    fn probe(&self) -> Option<&crate::firecode::Probe> {
        let bin = self.firecode.as_deref()?;
        Some(self.probe.get_or_init(|| crate::firecode::probe(bin)))
    }

    fn connection(&self, host: &str) -> Result<std::sync::Arc<crate::remote::Ssh>, String> {
        if let Some(s) = self.ssh.lock().expect("ssh connections").get(host) {
            return Ok(std::sync::Arc::clone(s));
        }
        let hosts = crate::remote::Hosts::load();
        let named = hosts
            .get(host)
            .ok_or_else(|| hosts.refusal(host))
            .cloned()?;
        let ssh = std::sync::Arc::new(crate::remote::Ssh::connect(
            named,
            None,
            crate::remote::Paths::from_env(),
        )?);
        self.ssh
            .lock()
            .expect("ssh connections")
            .insert(host.to_string(), std::sync::Arc::clone(&ssh));
        Ok(ssh)
    }
}

impl Hands for BoxHands {
    fn firecode(&self, args: &[String]) -> Result<String, String> {
        let Some(bin) = &self.firecode else {
            return Err(
                "this box has no `firecode` on its PATH, so no firecode verb can be asked".into(),
            );
        };
        // Detected before it is asked: an older firecode answers an unknown flag with its usage and
        // a non-zero status, and the caller's fallback then reads as a failure rather than as the
        // version it is.
        if args.first().is_some_and(|a| a == "gc")
            && let Some(caps) = self.probe()
        {
            if args.iter().any(|a| a == "--state") && !caps.gc_state {
                return Err(
                    "this firecode's `gc --help` names no `--state`, so it cannot retire a state \
                     layer yet"
                        .into(),
                );
            }
            if args.iter().any(|a| a == "--run") && !caps.gc_run {
                return Err(
                    "this firecode's `gc --help` names no `--run`, so it cannot retire a run \
                     directory yet"
                        .into(),
                );
            }
        }
        match crate::rpc::run_bounded(bin, args, std::time::Duration::from_secs(300)) {
            crate::rpc::Ended::Answered(out) if out.status.success() => {
                Ok(String::from_utf8_lossy(&out.stdout).into_owned())
            }
            crate::rpc::Ended::Answered(out) => Err(String::from_utf8_lossy(&out.stderr)
                .lines()
                .next()
                .unwrap_or("firecode refused")
                .to_string()),
            crate::rpc::Ended::TimedOut => Err("firecode did not answer within 300 s".into()),
            crate::rpc::Ended::Unstartable(why) => Err(why),
        }
    }

    /// A read of `/proc/locks`, never `flock -n`: taking the lock to test it is the opposite of
    /// testing it. The kernel answers about the INODE, which is what makes this immune to the window
    /// in which a build has opened its lock file and not yet taken the lock.
    fn lock_held(&self, target: &Path) -> Result<bool, String> {
        use std::os::unix::fs::MetadataExt;
        let lock = PathBuf::from(format!("{}.rrun.lock", target.display()));
        if !lock.exists() {
            return Ok(false);
        }
        let ino = std::fs::metadata(&lock)
            .map_err(|e| format!("{}: {e}", lock.display()))?
            .ino();
        let text = std::fs::read_to_string(&self.proc_locks)
            .map_err(|e| format!("{}: {e}", self.proc_locks.display()))?;
        let key = format!(":{ino}");
        Ok(text.lines().any(|l| {
            let mut f = l.split_whitespace();
            f.next().is_some()
                && f.next() == Some("FLOCK")
                && f.nth(3).is_some_and(|d| d.ends_with(&key))
        }))
    }

    fn host_command(&self, host: &str, script: &str) -> Result<String, String> {
        let ssh = self.connection(host)?;
        match ssh.sh_once("the reaper's pass", script) {
            Ok((said, _, 0)) => Ok(said),
            Ok((said, stderr, code)) => Err(format!(
                "the pass exited {code}: {}",
                String::from_utf8_lossy(&stderr)
                    .lines()
                    .next()
                    .or_else(|| said.lines().next())
                    .unwrap_or("no answer")
            )),
            Err(e) => Err(e.to_string()),
        }
    }

    fn host_read(&self, host: &str, path: &str) -> Result<Vec<u8>, String> {
        let ssh = self.connection(host)?;
        let script = format!("cat {}", crate::remote::quote(path));
        match ssh.sh_once("reading a bundle", &script) {
            Ok((said, _, 0)) => Ok(said.into_bytes()),
            Ok((_, stderr, code)) => Err(format!(
                "reading {path} exited {code}: {}",
                String::from_utf8_lossy(&stderr).trim()
            )),
            Err(e) => Err(e.to_string()),
        }
    }

    fn docker(&self, req: &crate::docker::Request) -> Result<crate::docker::Response, String> {
        let Some(engine) = &self.docker else {
            return Err(
                "this box has no docker engine socket: set `[docker] engine` in \
                 ~/.config/letibot/leticode.toml, or $DOCKER_HOST"
                    .into(),
            );
        };
        crate::docker::call(engine, req, crate::docker::reap::BOUND)
    }
}
