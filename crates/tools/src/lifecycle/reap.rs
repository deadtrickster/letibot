//! The reaper: one pass that ends compute and then frees disk (`docs/lifecycle.md`, G2 and G4).
//!
//! G1 judged; this acts. The compute half (this file) closes an owner and ends its cgroups, VMs
//! and daemons; the disk half ([`disks`]) then removes the caches of an owner whose compute is
//! gone, retires its run directories, keeps shared targets under quota and asks remote hosts for
//! their own pass. Work is never removed without an archive, so `leticode_send` still reopens a
//! closed owner where it stopped.
//!
//! Every decision reads the store and the kernel, never a daemon and never `ps`: this pass is what
//! runs when the daemon that owned the session was SIGKILLed, so nothing may be asked of it. A
//! job's liveness comes from its cgroup (`cgroup.events` populated, `cgroup.procs`, `memory.events`
//! oom_kill) or, where no cgroup could be made, from the `process` row's `(pid, identity)`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use letibot_tokencore::store::{
    Check, HoldingRow, HoldingState, OwnerKind, OwnerRow, OwnerState, ProcState, ProcessRecord,
    ReapAct, ReapEvent, Result as StoreResult, Store,
};

use super::plan::{HoldingView, Verdict};
use super::{
    DAEMON, Places, SCOPE, VM, daemons_of, discover_all, human_bytes, human_millis, scope_clause,
    stamp_of,
};

mod disks;
use crate::exec::procs::{self, Signal};
use crate::exec::scope;
use crate::firecode::held::{self, Patience};
pub use disks::{BoxHands, COPY_OUT_DEADLINE_MS, Hands};

/// How long before an idle close the owner is told (`docs/lifecycle.md` d.9). It is a heads-up and
/// nothing depends on it: the close happens whether or not anybody read it.
pub const NOTICE_LEAD_MS: i64 = 5 * 60 * 1000;

/// A scope's processes get this long between SIGTERM and `cgroup.kill` (d.5). A closed owner is
/// resumable, which is what makes ten seconds enough.
pub const SIGTERM_GRACE: Duration = Duration::from_secs(10);

/// After `cgroup.kill`, how long the kernel is given to show the subtree empty.
pub const AFTER_KILL: Duration = Duration::from_secs(5);

/// A daemon gets SIGTERM and then, after this, SIGKILL by `(pid, start)` (d.5).
pub const DAEMON_GRACE: Duration = Duration::from_secs(5);

/// How many times a step may fail before its holding is `stuck` instead of retried in silence.
pub const ATTEMPTS_BEFORE_STUCK: i64 = 2;

/// Who is running a pass, and what it may use to act. `by` is what the journal records.
#[derive(Clone)]
pub struct Ops {
    /// `gc timer`, `letibot gc --now`, `daemon d-…`.
    pub by: String,
    /// The firecode CLI, when this box has one. `None` leaves a VM alone and says so: a VM nobody
    /// can bring down is not one to write off as gone.
    pub firecode: Option<PathBuf>,
    pub grace: Duration,
    pub after_kill: Duration,
    pub daemon_grace: Duration,
    /// What the disk half reaches through: firecode's verbs, the build lock, remote hosts.
    pub hands: Arc<dyn Hands>,
    /// This box's `[docker]` and `[gc.docker]` settings. They are on `Ops` rather than read inside
    /// the pass so the one thing that removes something docker owns can be a test's value.
    pub docker: crate::docker::Settings,
}

impl std::fmt::Debug for Ops {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ops")
            .field("by", &self.by)
            .field("firecode", &self.firecode)
            .field("grace", &self.grace)
            .field("after_kill", &self.after_kill)
            .field("daemon_grace", &self.daemon_grace)
            .finish_non_exhaustive()
    }
}

impl Ops {
    /// A pass on this box, run by `by`, with the box's own firecode, bounds and hands.
    pub fn here(by: impl Into<String>) -> Ops {
        let hands = BoxHands::new();
        Ops {
            by: by.into(),
            firecode: hands.firecode.clone(),
            grace: SIGTERM_GRACE,
            after_kill: AFTER_KILL,
            daemon_grace: DAEMON_GRACE,
            hands: Arc::new(hands),
            docker: crate::docker::Settings::load_or_default(),
        }
    }
}

/// What the records and the kernel say an owner is doing - the whole input a close decision has.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Activity {
    /// A turn in flight or a job running. Read from the cgroup where there is one: a session whose
    /// leader is gone can still hold a hundred processes.
    pub busy: bool,
    /// Its parent's state, when it has a parent row.
    pub parent: Option<OwnerState>,
    /// Its daemon: `None` no record at all, `Some(true)` a process at the recorded pid,
    /// `Some(false)` a record whose daemon is gone.
    pub daemon: Option<bool>,
}

impl Activity {
    /// The clock an idle TTL is measured from: the owner row's own lease, which a turn renews.
    pub fn lease(&self, o: &OwnerRow) -> i64 {
        o.lease_ms
    }
}

/// Is this owner due to be closed, and in whose words? `None` is *leave it*, which is the answer
/// for everything this pass cannot prove.
///
/// The order is the design's (d.3): a parent that closed takes its children with it, a hard TTL is
/// the backstop that fires whatever the owner is doing, a daemon that is gone means nothing is
/// running it, and only then does idleness get a say - and idleness never fires with a turn in
/// flight or a job running. An interactive `session` owner is never closed by any of them: the
/// operator's own conversation is theirs to end.
pub fn close_due(o: &OwnerRow, a: &Activity, now_ms: i64) -> Option<String> {
    if o.state != OwnerState::Live || o.held(now_ms) {
        return None;
    }
    if let Some(parent) = &o.parent_id
        && let Some(state) = a.parent
        && state.compute_gone()
    {
        return Some(format!(
            "its parent `{parent}` is `{}`: a child's compute does not outlive it",
            state.word()
        ));
    }
    if o.kind != OwnerKind::Session
        && let Some(ttl) = o.hard_ttl_ms
        && now_ms - o.created_ms >= ttl
    {
        return Some(format!(
            "hard TTL: a {} is closed {} after it was created, whatever it is doing",
            o.kind.noun(),
            human_millis(ttl as u64)
        ));
    }
    if o.kind != OwnerKind::Session
        && let Some(false) = a.daemon
    {
        return Some(
            "its daemon's record is there and no process is behind it: nothing is running it"
                .to_string(),
        );
    }
    if a.busy {
        return None;
    }
    if o.kind != OwnerKind::Session
        && let Some(ttl) = o.idle_ttl_ms
    {
        let idle = now_ms - a.lease(o);
        if idle >= ttl {
            return Some(format!(
                "idle {}: no turn, no job and nothing said to it for {}",
                human_millis(idle as u64),
                human_millis(ttl as u64)
            ));
        }
    }
    None
}

/// The instant a notice is due for this owner, if its idle clock is running: `NOTICE_LEAD_MS`
/// before the close the idle TTL would make. `None` for an owner with no idle TTL, a hold, or a
/// turn in flight - and for one whose notice is already in the journal.
pub fn notice_at(o: &OwnerRow, a: &Activity, now_ms: i64) -> Option<i64> {
    if o.state != OwnerState::Live || o.held(now_ms) || a.busy || o.kind == OwnerKind::Session {
        return None;
    }
    let ttl = o.idle_ttl_ms?;
    let closes_at = a.lease(o) + ttl;
    (now_ms >= closes_at - NOTICE_LEAD_MS && now_ms < closes_at).then_some(closes_at)
}

/// One thing a pass did, as its line - the journal row and the operator's answer are the same fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Act {
    pub owner: String,
    pub kind: String,
    /// The machine it was on, when that is not this box.
    pub host: Option<String>,
    pub handle: String,
    pub act: ReapAct,
    pub said: String,
    /// What a removal freed; 0 for everything else.
    pub bytes: u64,
}

impl Act {
    pub fn line(&self) -> String {
        format!(
            "{} {}{} {} — {}",
            self.owner,
            self.kind,
            self.host
                .as_deref()
                .map(|h| format!(" on {h}"))
                .unwrap_or_default(),
            self.act.word(),
            self.said
        )
    }
}

/// What one pass did, in order, and what it refused to do and why.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pass {
    pub now_ms: i64,
    pub owners: usize,
    pub acts: Vec<Act>,
    /// Holdings on a host this box could not reach: marked `lost`, removed nothing, retried.
    pub lost: Vec<(String, String, String)>,
    /// What this pass left exactly as it was, with why - a hold, a live turn, a held build lock.
    pub kept: Vec<String>,
}

impl Pass {
    pub fn did_anything(&self) -> bool {
        !self.acts.is_empty()
    }

    /// The one line a pass that acted posts to the box's room (d.9).
    pub fn summary(&self) -> Option<String> {
        if self.acts.is_empty() {
            return None;
        }
        let closes = self.acts.iter().filter(|a| a.act == ReapAct::Close).count();
        let parts: Vec<String> = self.acts.iter().map(Act::line).collect();
        Some(format!(
            "gc: closed {closes} idle or finished owner(s); {}",
            parts.join("; ")
        ))
    }

    /// The bytes this pass freed: removes only, since a keep frees nothing.
    pub fn freed(&self) -> u64 {
        self.acts
            .iter()
            .filter(|a| matches!(a.act, ReapAct::Remove | ReapAct::Down))
            .map(|a| a.bytes)
            .sum()
    }

    /// `gc: removed 3, freed 2.1 GiB, 1 host unreachable` - the one line a pass posts.
    pub fn line(&self) -> String {
        if self.acts.is_empty() && self.lost.is_empty() {
            return "gc: nothing to reap".into();
        }
        let mut parts: Vec<String> = Vec::new();
        let mut by: BTreeMap<&'static str, usize> = BTreeMap::new();
        for a in &self.acts {
            *by.entry(a.act.word()).or_default() += 1;
        }
        for (act, n) in by {
            parts.push(format!("{act} {n}"));
        }
        if !self.acts.is_empty() {
            parts.push(format!("freed {}", human_bytes(self.freed())));
        }
        if !self.lost.is_empty() {
            parts.push(format!(
                "{} host(s) unreachable, their holdings `lost`",
                self.lost.len()
            ));
        }
        format!("gc: {}", parts.join(", "))
    }

    /// The operator's report: one line per act, then the keeps and the losses.
    pub fn render(&self) -> String {
        let at = stamp_of(self.now_ms);
        let mut out = String::new();
        for a in &self.acts {
            out.push_str(&format!(
                "{at} {} {}{} — {} ({})\n",
                a.owner,
                a.kind,
                a.host
                    .as_deref()
                    .map(|h| format!(" on {h}"))
                    .unwrap_or_default(),
                a.said,
                human_bytes(a.bytes)
            ));
        }
        for k in &self.kept {
            out.push_str(&format!("{at} keep — {k}\n"));
        }
        for (owner, host, said) in &self.lost {
            out.push_str(&format!(
                "{at} {owner} on {host} — lost: {said} (nothing of it was touched here; the next \
                 pass tries again)\n"
            ));
        }
        out.push_str(&self.line());
        out.push('\n');
        out
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "now_ms": self.now_ms,
            "owners": self.owners,
            "freed": self.freed(),
            "acts": self.acts.iter().map(|a| serde_json::json!({
                "owner": a.owner,
                "kind": a.kind,
                "host": a.host,
                "handle": a.handle,
                "act": a.act.word(),
                "said": a.said,
                "bytes": a.bytes,
            })).collect::<Vec<_>>(),
            "kept": self.kept,
            "lost": self.lost.iter().map(|(o, h, s)| serde_json::json!({
                "owner": o, "host": h, "said": s
            })).collect::<Vec<_>>(),
        })
    }
}

// ---------------------------------------------------------------- the reading half

/// What this owner is doing, read from the store and the kernel - no daemon is asked anything.
pub fn activity(store: &Store, o: &OwnerRow, places: &Places) -> Activity {
    let parent = o
        .parent_id
        .as_ref()
        .and_then(|p| store.owner(p).ok().flatten())
        .map(|p| p.state);
    Activity {
        busy: busy(store, o),
        parent,
        daemon: daemon_state(o, places),
    }
}

/// Is a turn in flight or a job running? The cgroup answers it, because the seat's own `ps` cannot
/// see host pids and the daemon may not be there to be asked. A store row that says `running` for
/// a process whose cgroup is empty is a row the supervisor has not caught up with, not work.
fn busy(store: &Store, o: &OwnerRow) -> bool {
    let mut populated = false;
    for row in store.holdings_of(&o.id).unwrap_or_default() {
        if row.kind != SCOPE || row.state == HoldingState::Gone {
            continue;
        }
        if scope::use_at(Path::new(&row.handle)).is_some_and(|u| u.alive()) {
            populated = true;
        }
    }
    if populated {
        return true;
    }
    store
        .processes_of(&o.id)
        .unwrap_or_default()
        .iter()
        .any(|p| p.state.is_open())
}

/// What the runtime records say about the daemon serving this owner: `None` when no record names
/// one, `Some(true)` while a process is at the recorded pid, `Some(false)` once it is not.
fn daemon_state(o: &OwnerRow, places: &Places) -> Option<bool> {
    let recs = daemons_of(o, places);
    if recs.is_empty() {
        return None;
    }
    Some(
        recs.iter()
            .any(|r| r.pid.is_some_and(|p| held::alive(&places.proc_root, p))),
    )
}

// ---------------------------------------------------------------- the acting half

/// One pass over every owner, or the one named: compute first, then disk. Closing an owner and
/// freeing its caches happen in the same pass, and the shared targets and remote hosts follow.
/// Idempotent end to end - a kill of an empty cgroup, a `down` of a VM that is down and a remove of
/// a missing path are all answers, not failures (d.10).
pub fn pass(
    store: &Store,
    places: &Places,
    ops: &Ops,
    only: Option<&str>,
    now: i64,
) -> StoreResult<Pass> {
    let owners: Vec<OwnerRow> = match only {
        Some(id) => store.owner(id)?.into_iter().collect(),
        None => store.owners()?,
    };
    let mut out = Pass {
        now_ms: now,
        owners: owners.len(),
        ..Pass::default()
    };
    for owner in &owners {
        compute(store, places, ops, owner, now, &mut out);
        // The row as the compute half left it: an owner it closed just now loses its caches here.
        let owner = store
            .owner(&owner.id)
            .ok()
            .flatten()
            .unwrap_or_else(|| owner.clone());
        if owner.held(now) {
            out.kept.push(format!(
                "{}: the operator holds it, so nothing of it is closed or removed",
                owner.id
            ));
            continue;
        }
        // The work half: a closed owner past its grace has its worktrees and branches kept in
        // `refs/letibot/archive/<owner>/` and then removed (G3). It runs between compute and the
        // caches, because a process may still be writing in the tree until the compute half ends.
        if let Err(e) = super::work::settle(store, places, ops, &owner, now, &mut out) {
            out.kept.push(format!(
                "{}: its work was left, the store said {e}",
                owner.id
            ));
        }
        // A store error in the disk half is said and the pass goes on: the closes already made
        // are in `acts`, and the daemon acts on them only if the pass returns them.
        if disks::past_compute(&owner) {
            // Docker's `work` class - a named volume - after the close and its caches: it waits for
            // the work grace, and `judge` is what says when that is.
            docker_work(store, places, ops, &owner, now, &mut out);
            if let Err(e) = disks::reap_owner(store, places, ops, &owner, now, &mut out) {
                out.kept.push(format!(
                    "{}: its disks were left, the store said {e}",
                    owner.id
                ));
            }
        }
    }
    disks::shared_targets(places, ops, now, &mut out);
    if let Err(e) = disks::remote_hosts(store, places, ops, now, only, &mut out) {
        out.kept
            .push(format!("remote hosts were left, the store said {e}"));
    }
    prune_docker(store, ops, now, &mut out);
    Ok(out)
}

/// The opt-in `[gc.docker]` prune: off unless the operator wrote labels into it. It is the only
/// place a pass removes something docker owns that is not an owner's, and it is why the setting is
/// `prune_label` and not a switch: a prune with no label is how another project's images go.
fn prune_docker(store: &Store, ops: &Ops, at_ms: i64, out: &mut Pass) {
    if !ops.docker.prunes() {
        return;
    }
    let engine = HandsEngine(ops.hands.as_ref());
    match crate::docker::prune(&engine, &ops.docker) {
        Ok(said) if said.is_empty() => {}
        Ok(said) => {
            let sentence = format!(
                "`[gc.docker] {}`: {}",
                ops.docker.prune_label.join(", "),
                said.join(", ")
            );
            store
                .put_reap_event(&ReapEvent {
                    at_ms,
                    owner_id: "box".into(),
                    kind: Some("docker".into()),
                    host: None,
                    handle: None,
                    act: ReapAct::Remove.word().into(),
                    said: sentence.clone(),
                    by: ops.by.clone(),
                })
                .ok();
            out.acts.push(Act {
                owner: "box".into(),
                kind: "docker".into(),
                host: None,
                handle: ops.docker.prune_label.join(","),
                act: ReapAct::Remove,
                said: sentence,
                bytes: 0,
            });
        }
        Err(why) => out
            .kept
            .push(format!("the opt-in docker prune was left: {why}")),
    }
}

/// The compute half for one owner: judge a live one, and end the compute of one that is closing.
fn compute(
    store: &Store,
    places: &Places,
    ops: &Ops,
    owner: &OwnerRow,
    at_ms: i64,
    out: &mut Pass,
) {
    match owner.state {
        OwnerState::Live => {
            let a = activity(store, owner, places);
            if let Some(closes_at) = notice_at(owner, &a, at_ms) {
                note_idle_close(store, owner, closes_at, ops, at_ms, out);
            }
            let Some(why) = close_due(owner, &a, at_ms) else {
                // A hold is said once, by the pass, for both halves.
                if !owner.held(at_ms) {
                    out.kept.push(format!(
                        "{}: it is live and nothing says close it",
                        owner.id
                    ));
                }
                return;
            };
            store
                .set_owner_state(&owner.id, OwnerState::Closing, &why, at_ms)
                .ok();
            out.acts.push(journal(
                store,
                at_ms,
                owner,
                "",
                "",
                ReapAct::Signal,
                &format!("closing: {why}"),
                &ops.by,
            ));
        }
        OwnerState::Closing => {}
        _ => return,
    }
    // The row as the CLOSE left it: `closed_why` is the sentence the journal - and so
    // `letibot gc --log` - reads back, and the copy this loop holds was read before the state
    // moved.
    let owner = &store
        .owner(&owner.id)
        .ok()
        .flatten()
        .unwrap_or_else(|| owner.clone());
    reap_compute(store, places, ops, owner, at_ms, out);
}

/// The heads-up before an idle close. It is journaled and said on the owner's status line, and it
/// is **not a conversation row**: a row carries the tokens the model is charged for, and this pass
/// runs where nobody is holding the session's ledger. The conversation stays exactly the model's
/// (AGENTS.md's first rule); what the operator and the model can both read back is the journal.
fn note_idle_close(
    store: &Store,
    owner: &OwnerRow,
    closes_at: i64,
    ops: &Ops,
    at_ms: i64,
    out: &mut Pass,
) {
    let already = store
        .reap_events_of(&owner.id)
        .unwrap_or_default()
        .iter()
        .any(|e| e.kind.as_deref() == Some("notice") && e.at_ms >= closes_at - NOTICE_LEAD_MS);
    if already {
        return;
    }
    let said = format!(
        "closing in {} for idleness; `letibot hold {}` keeps it. Nothing is lost: the rows stay and \
         `leticode_send` reopens it.",
        human_millis((closes_at - at_ms).max(0) as u64),
        owner.id
    );
    out.acts.push(journal(
        store,
        at_ms,
        owner,
        "notice",
        "",
        ReapAct::Plan,
        &said,
        &ops.by,
    ));
}

/// Reap one `closing` owner's compute, in the design's order, and move it to `closed` when nothing
/// of class `compute` is left. Caches are the disk half's, once the owner is closed; work is G3's.
fn reap_compute(
    store: &Store,
    places: &Places,
    ops: &Ops,
    owner: &OwnerRow,
    at_ms: i64,
    out: &mut Pass,
) {
    let mut left = 0usize;
    for f in discover_all(store, owner, places) {
        if f.class != letibot_tokencore::store::HoldingClass::Compute || f.host.is_some() {
            continue;
        }
        let said = match f.kind.as_str() {
            SCOPE => end_scope(Path::new(&f.handle), ops),
            VM => end_vm(places, ops, &f.handle),
            DAEMON => end_daemon(places, ops, owner, &f.handle),
            // A container or a network the label proves: gone by `docker rm -f` / `network rm`
            // through the engine, which is G6's half of compute.
            crate::docker::reap::CONTAINER | crate::docker::reap::NETWORK => {
                end_docker(ops, &f.kind, &f.handle)
            }
            // Nothing else is compute: said, not guessed at.
            other => Ended::Kept(format!("`{other}` is not this package's to reap")),
        };
        let (state, why) = match &said {
            Ended::Gone(why) => (HoldingState::Gone, why.clone()),
            Ended::Kept(why) => (HoldingState::Kept, why.clone()),
            Ended::Refused(why) => (HoldingState::Stuck, why.clone()),
        };
        let state = write_holding(store, owner, &f.kind, &f.handle, state, &why, at_ms);
        out.acts.push(journal(
            store,
            at_ms,
            owner,
            &f.kind,
            &f.handle,
            if state == HoldingState::Gone {
                ReapAct::Kill
            } else if state == HoldingState::Stuck {
                ReapAct::Stuck
            } else {
                ReapAct::Keep
            },
            &why,
            &ops.by,
        ));
        if state != HoldingState::Gone {
            left += 1;
        }
    }
    // And the processes themselves, for a box that could make no cgroup: the `process` row's
    // `(pid, identity)` is the proof a bare pid never is (d.4, R192's fallback).
    left += end_processes(store, owner, ops, at_ms, out);
    if left == 0 {
        let why = owner
            .closed_why
            .clone()
            .unwrap_or_else(|| "closed by the reaper".into());
        store
            .set_owner_state(&owner.id, OwnerState::Closed, &why, at_ms)
            .ok();
        out.acts.push(journal(
            store,
            at_ms,
            owner,
            "",
            "",
            ReapAct::Close,
            &format!("closed with no compute left; {why}"),
            &ops.by,
        ));
    }
}

/// Kill every open process this owner has a row for, by `(pid, start)`, and write down what each
/// one answered. Answers how many are still running.
fn end_processes(store: &Store, owner: &OwnerRow, ops: &Ops, at_ms: i64, out: &mut Pass) -> usize {
    let mut still = 0usize;
    for rec in store.processes_of(&owner.id).unwrap_or_default() {
        if !rec.state.is_open() {
            continue;
        }
        match end_process(&rec, ops) {
            Ended::Gone(why) => {
                store
                    .check_process(
                        &owner.id,
                        &rec.handle,
                        &Check::new(ProcState::Stopped, at_ms, 0, "gc"),
                    )
                    .ok();
                out.acts.push(journal(
                    store,
                    at_ms,
                    owner,
                    "process",
                    &rec.handle,
                    ReapAct::Kill,
                    &why,
                    &ops.by,
                ));
            }
            Ended::Kept(why) | Ended::Refused(why) => {
                still += 1;
                out.acts.push(journal(
                    store,
                    at_ms,
                    owner,
                    "process",
                    &rec.handle,
                    ReapAct::Keep,
                    &why,
                    &ops.by,
                ));
            }
        }
    }
    still
}

/// How one resource's end went. `Gone` is only ever written after looking: a step is done when the
/// thing is absent, not when the command returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    /// Absent now, with the sentence the journal keeps.
    Gone(String),
    /// Left, deliberately, with why - a hold, a host this box cannot see, no tool to do it with.
    Kept(String),
    /// Tried and it did not work: `stuck` after two of these, and nothing further removed.
    Refused(String),
}

impl Ended {
    pub fn said(&self) -> &str {
        match self {
            Ended::Gone(why) | Ended::Kept(why) | Ended::Refused(why) => why,
        }
    }
}

/// End one cgroup: SIGTERM to what it lists, a grace, then `cgroup.kill`, then a look. The kernel
/// is the authority throughout - a directory that is gone is `Gone`, one that still says
/// `populated 1` is not.
pub fn end_scope(dir: &Path, ops: &Ops) -> Ended {
    let Some(use_) = scope::use_at(dir) else {
        return Ended::Gone(format!("{} is not there: nothing in it", dir.display()));
    };
    if !use_.alive() {
        let _ = std::fs::remove_dir(dir);
        return Ended::Gone(format!(
            "{} held no process; the directory is gone",
            dir.display()
        ));
    }
    let before = scope_clause(&use_);
    let pids = members(dir);
    for pid in &pids {
        // SAFETY: a pid the cgroup itself lists, and the kernel puts a fork in its parent's
        // cgroup, so these are this scope's processes and not somebody's reused number.
        unsafe {
            libc::kill(*pid as i32, libc::SIGTERM);
        }
    }
    let deadline = Instant::now() + ops.grace;
    while scope::use_at(dir).is_some_and(|u| u.alive()) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    if scope::use_at(dir).is_some_and(|u| u.alive()) {
        if std::fs::write(dir.join(scope::CGROUP_KILL), b"1").is_err() {
            return Ended::Refused(format!(
                "it still holds {before} and {} could not be written",
                dir.join(scope::CGROUP_KILL).display()
            ));
        }
        let deadline = Instant::now() + ops.after_kill;
        while scope::use_at(dir).is_some_and(|u| u.alive()) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    match scope::use_at(dir) {
        None => Ended::Gone(format!("ended by cgroup.kill; {} is gone", dir.display())),
        Some(after) if !after.alive() => {
            let _ = std::fs::remove_dir(dir);
            Ended::Gone(format!(
                "ended by cgroup.kill; it held {before} and holds nothing now"
            ))
        }
        Some(after) => Ended::Refused(format!(
            "it still holds {} after SIGTERM and cgroup.kill (it held {before})",
            scope_clause(&after)
        )),
    }
}

/// Every pid one cgroup directory lists, in the directory and under it.
fn members(dir: &Path) -> Vec<u32> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        if let Ok(text) = std::fs::read_to_string(d.join("cgroup.procs")) {
            out.extend(text.lines().filter_map(|l| l.trim().parse::<u32>().ok()));
        }
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    stack.push(e.path());
                }
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// End one process by its record: `(pid, start)` proven before any signal, SIGTERM, a grace, then
/// SIGKILL. A pid whose start time no longer matches is somebody else's now, and is not signalled.
pub fn end_process(rec: &ProcessRecord, ops: &Ops) -> Ended {
    if rec.host.is_some() {
        return Ended::Kept(format!(
            "{} runs on {}: that host's own pass ends it (G4), and this one cannot see it",
            rec.label(),
            rec.host.clone().unwrap_or_default()
        ));
    }
    let Some(start) = rec
        .identity
        .strip_prefix("start:")
        .and_then(|s| s.parse().ok())
    else {
        return Ended::Refused(format!(
            "{}'s record carries no start time, so a pid is all there is to signal and a pid is \
             never enough",
            rec.label()
        ));
    };
    if !procs::alive(rec.pid, start) {
        return Ended::Gone(format!("{} (pid {}) is already gone", rec.label(), rec.pid));
    }
    let _ = procs::kill_exact(rec.pid, start, Signal::Term);
    let deadline = Instant::now() + ops.grace;
    while procs::alive(rec.pid, start) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    if procs::alive(rec.pid, start) {
        let _ = procs::kill_exact(rec.pid, start, Signal::Kill);
        let deadline = Instant::now() + ops.after_kill;
        while procs::alive(rec.pid, start) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    if procs::alive(rec.pid, start) {
        Ended::Refused(format!(
            "{} (pid {}) is still there after SIGTERM and SIGKILL",
            rec.label(),
            rec.pid
        ))
    } else {
        Ended::Gone(format!(
            "{} (pid {}) was ended by the reaper",
            rec.label(),
            rec.pid
        ))
    }
}

/// The pass's door to the engine: the same [`crate::docker::Engine`] a plan uses, reached through
/// [`Hands`] so a test can answer the whole reap without a socket.
struct HandsEngine<'a>(&'a dyn Hands);

impl crate::docker::Engine for HandsEngine<'_> {
    fn call(&self, req: &crate::docker::Request) -> Result<crate::docker::Response, String> {
        self.0.docker(req)
    }

    fn name(&self) -> String {
        "the docker engine this box's settings name".into()
    }
}

/// End one container or network by its label (`docs/lifecycle.md` d.5): compute, so it goes at close
/// and needs no grace - the row stays, and a reopen makes the objects again.
fn end_docker(ops: &Ops, kind: &str, handle: &str) -> Ended {
    let ended = crate::docker::remove(&HandsEngine(ops.hands.as_ref()), kind, handle);
    if ended.gone {
        Ended::Gone(ended.said)
    } else {
        Ended::Refused(ended.said)
    }
}

/// A volume is `work` (`docs/lifecycle.md` d.5): it survives the close and goes at settle. The
/// decision is the plan's own `judge`, so what `letibot gc` prints and what the pass does cannot
/// drift - and a volume discovered with its size is a plan line with a size.
fn docker_work(
    store: &Store,
    places: &Places,
    ops: &Ops,
    owner: &OwnerRow,
    at_ms: i64,
    out: &mut Pass,
) {
    for row in store.holdings_of(&owner.id).unwrap_or_default() {
        if row.kind != crate::docker::reap::VOLUME
            || row.state == HoldingState::Gone
            || row.host.is_some()
        {
            continue;
        }
        let view = HoldingView {
            kind: row.kind.clone(),
            class: row.class,
            host: row.host.clone(),
            handle: row.handle.clone(),
            state: HoldingState::Held,
            stamp: row.stamp.clone(),
            bytes: row.bytes,
            archive: row.archive.clone(),
            why: row.why.clone(),
            verdict: Verdict::Keep(String::new()),
            memory: None,
        };
        // Only `reap now` acts: `reap at` is the plan's sentence for a grace that has not
        // elapsed, and a pass that acted on it would be the reap the grace exists to stop.
        let verdict = super::plan::judge(owner, &view, places, at_ms);
        if !matches!(verdict, Verdict::Now(_)) {
            out.kept.push(format!(
                "{} volume {}: {} — {}",
                owner.id,
                row.handle,
                verdict.word(),
                verdict.said()
            ));
            continue;
        }
        let ended = crate::docker::remove(
            &HandsEngine(ops.hands.as_ref()),
            crate::docker::reap::VOLUME,
            &row.handle,
        );
        let (state, why) = if ended.gone {
            (HoldingState::Gone, ended.said)
        } else {
            (HoldingState::Stuck, ended.said)
        };
        let state = write_holding(store, owner, &row.kind, &row.handle, state, &why, at_ms);
        let mut act = journal(
            store,
            at_ms,
            owner,
            &row.kind,
            &row.handle,
            if state == HoldingState::Gone {
                ReapAct::Remove
            } else {
                ReapAct::Stuck
            },
            &why,
            &ops.by,
        );
        act.bytes = ended.bytes;
        out.acts.push(act);
    }
}

/// Bring one VM down, with the backend's own ladder - `firecode down`, the run's `cgroup.kill`, then
/// SIGKILL to the firecracker - and only believe it when `/proc` agrees.
fn end_vm(places: &Places, ops: &Ops, run: &str) -> Ended {
    let Some(bin) = &ops.firecode else {
        return Ended::Kept(format!(
            "no firecode on this box's PATH, so the VM of run `{run}` cannot be brought down here"
        ));
    };
    let Some(runs) = &places.firecode_runs else {
        return Ended::Kept(format!(
            "firecode's `runs/` is not where this box looks, so run `{run}` cannot be read"
        ));
    };
    let Some(vm) = held::live_vms(runs, &places.proc_root)
        .into_iter()
        .find(|v| v.run == run)
    else {
        return Ended::Gone(format!(
            "run `{run}` has no firecracker: its VM is already down"
        ));
    };
    if vm.project.parent() != Some(places.firecode_cache.as_deref().unwrap_or(Path::new("/x"))) {
        return Ended::Kept(format!(
            "run `{run}` was booted on {}, which is not under letibot's cache: not this box's to stop",
            vm.project.display()
        ));
    }
    let reaped = held::reap(bin, &vm, &places.proc_root, patience(ops));
    if reaped.gone() {
        Ended::Gone(format!("{} · {}", vm.line(), reaped.said()))
    } else {
        Ended::Refused(format!("{} · {}", vm.line(), reaped.said()))
    }
}

fn patience(ops: &Ops) -> Patience {
    Patience {
        down: Duration::from_secs(60).min(ops.grace.max(Duration::from_secs(1)) * 6),
        after_down: ops.after_kill,
        after_kill: ops.after_kill,
    }
}

/// Stop one daemon by its runtime record: SIGTERM, a grace, then SIGKILL by `(pid, start)`. Never
/// this process, and never a daemon whose record names another workspace (d.4 rule 6).
fn end_daemon(places: &Places, ops: &Ops, owner: &OwnerRow, handle: &str) -> Ended {
    let recs = daemons_of(owner, places);
    let rec = recs
        .iter()
        .find(|r| match r.pid {
            Some(p) => handle == format!("pid:{p}"),
            None => handle == r.path.display().to_string(),
        })
        .or_else(|| recs.first());
    let Some(rec) = rec else {
        return Ended::Gone("no runtime record names a daemon for this owner".into());
    };
    let Some(pid) = rec.pid else {
        return Ended::Gone(format!(
            "{} names no pid, so there is no daemon to stop",
            rec.path.display()
        ));
    };
    if pid == std::process::id() || pid <= 1 {
        return Ended::Kept(format!(
            "pid {pid} is the process running this pass: a reaper never reaps itself"
        ));
    }
    let Some(info) = procs::read(pid) else {
        return Ended::Gone(format!(
            "no process is at {pid}, so the daemon {} named is gone",
            rec.path.display()
        ));
    };
    if !procs::alive(pid, info.start) {
        return Ended::Gone(format!("no process is at {pid}: its daemon is gone"));
    }
    let _ = procs::kill_exact(pid, info.start, Signal::Term);
    let deadline = Instant::now() + ops.daemon_grace;
    while procs::alive(pid, info.start) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    if procs::alive(pid, info.start) {
        let _ = procs::kill_exact(pid, info.start, Signal::Kill);
        let deadline = Instant::now() + ops.after_kill;
        while procs::alive(pid, info.start) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    if procs::alive(pid, info.start) {
        let _ = std::fs::remove_file(&rec.path);
        return Ended::Refused(format!(
            "the daemon at pid {pid} is still there after SIGTERM and SIGKILL"
        ));
    }
    // The record goes with it: it is what every reader uses to say a daemon is there, and a record
    // with no process behind it is the stale handle this whole package exists to stop believing.
    let _ = std::fs::remove_file(&rec.path);
    Ended::Gone(format!(
        "the daemon at pid {pid} was stopped and its record {} removed",
        rec.path.display()
    ))
}

/// Write a holding's new state, counting the attempts a refused step has cost it: a step that has
/// failed `ATTEMPTS_BEFORE_STUCK` times is `stuck`, and nothing further is removed for it (d.10).
fn write_holding(
    store: &Store,
    owner: &OwnerRow,
    kind: &str,
    handle: &str,
    state: HoldingState,
    why: &str,
    at_ms: i64,
) -> HoldingState {
    let existing = store
        .holdings_of(&owner.id)
        .unwrap_or_default()
        .into_iter()
        .find(|h| h.kind == kind && h.handle == handle && h.host.is_none());
    let attempts = existing.as_ref().map(|h| h.attempts).unwrap_or(0);
    let (state, attempts) = match state {
        HoldingState::Gone => (HoldingState::Gone, attempts),
        HoldingState::Kept => (HoldingState::Kept, attempts),
        _ if attempts + 1 >= ATTEMPTS_BEFORE_STUCK => (HoldingState::Stuck, attempts + 1),
        _ => (HoldingState::Reaping, attempts + 1),
    };
    store
        .put_holding(&HoldingRow {
            owner_id: owner.id.clone(),
            kind: kind.to_string(),
            host: None,
            handle: handle.to_string(),
            class: existing
                .as_ref()
                .map(|h| h.class)
                .unwrap_or(letibot_tokencore::store::HoldingClass::Compute),
            state,
            stamp: existing
                .as_ref()
                .map(|h| h.stamp.clone())
                .unwrap_or_else(|| "the pass that reaped it".into()),
            bytes: existing.as_ref().and_then(|h| h.bytes),
            archive: existing.as_ref().and_then(|h| h.archive.clone()),
            why: Some(why.to_string()),
            created_ms: existing.as_ref().map(|h| h.created_ms).unwrap_or(at_ms),
            checked_ms: Some(at_ms),
            attempts,
        })
        .ok();
    state
}

/// One journal line, and the `Act` that says the same thing.
#[allow(clippy::too_many_arguments)]
fn journal(
    store: &Store,
    at_ms: i64,
    owner: &OwnerRow,
    kind: &str,
    handle: &str,
    act: ReapAct,
    said: &str,
    by: &str,
) -> Act {
    let kind = (!kind.is_empty()).then(|| kind.to_string());
    let handle = (!handle.is_empty()).then(|| handle.to_string());
    store
        .put_reap_event(&ReapEvent {
            at_ms,
            owner_id: owner.id.clone(),
            kind: kind.clone(),
            host: None,
            handle: handle.clone(),
            act: act.word().to_string(),
            said: said.to_string(),
            by: by.to_string(),
        })
        .ok();
    Act {
        owner: owner.id.clone(),
        kind: kind.unwrap_or_default(),
        host: None,
        handle: handle.unwrap_or_default(),
        act,
        said: said.to_string(),
        bytes: 0,
    }
}

#[cfg(test)]
mod tests;
