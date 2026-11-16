//! The daemon's own reaper: close what is idle, past a hard TTL, or under a daemon that is gone,
//! let go of it, and free its disk (`docs/lifecycle.md`).
//!
//! The same pass also runs as `letibot gc --now` and on a timer; this is the fast one, in a daemon
//! that is up. It reads the store and the kernel and asks no daemon anything, so an owner whose
//! daemon was SIGKILLed is closed by the one that replaced it.
//!
//! Every daemon runs the idle half each [`SWEEP`]. The disk half, which walks caches, shared
//! targets and remote hosts over ssh, is one daemon's per box at `[gc] disk_every` ([`DiskTurn`]).
//!
//! A close here is the same close a `leticode_stop` is (registry entry, head detach, `Work::Close`,
//! `close_settle`). This file adds the decision and the journal, never a second way to close.

use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use letibot_sessionlog::registry::Registry;
use letibot_tokencore::store::{ReapAct, Store};
use letibot_tools::lifecycle::{self, Places};

use crate::config::StoreAt;

/// How often the daemon looks, when nothing asks it to look sooner. The idle TTLs are half an hour
/// and the notice five minutes, so a minute is fine grain enough for both.
pub const SWEEP: Duration = Duration::from_secs(60);

/// How often the disk half runs when `[gc] disk_every` names no period. It walks every owner's
/// disks and every remote host over ssh, so one daemon per box runs it, and not every minute.
pub const DISK_EVERY: Duration = Duration::from_secs(600);

static LOOK_NOW: AtomicBool = AtomicBool::new(false);

/// Ask the pass to look at its next second rather than its next sweep.
pub fn look_now() {
    LOOK_NOW.store(true, Ordering::Relaxed);
}

/// One pass, with what the daemon does about each act it made. `by` is the journal's word for who
/// ran it. Answers the lines the daemon's log says.
pub fn pass(store: &Store, registry: Option<&Registry>, by: &str) -> Vec<String> {
    pass_with(store, registry, &lifecycle::Ops::here(by))
}

/// The daemon's own hands: [`lifecycle::Ops::here`] with when it came up, for the start grace.
fn daemon_ops(registry: &Registry) -> lifecycle::Ops {
    lifecycle::Ops {
        daemon_up_ms: Some(registry.started_ms() as i64),
        ..lifecycle::Ops::here(format!("daemon {}", registry.instance()))
    }
}

/// [`pass`] with hands the caller keeps, so the ssh connections a remote host's pass opens are
/// reused from one sweep to the next.
pub fn pass_with(store: &Store, registry: Option<&Registry>, ops: &lifecycle::Ops) -> Vec<String> {
    sweep(store, registry, ops, true)
}

/// One sweep: the idle half always, the disk half only when `disk` says this daemon's turn.
fn sweep(
    store: &Store,
    registry: Option<&Registry>,
    ops: &lifecycle::Ops,
    disk: bool,
) -> Vec<String> {
    let places = Places::here();
    let now = letibot_metrics::now_ms() as i64;
    if let Some(registry) = registry {
        renew_held(store, registry, now);
    }
    let made = match lifecycle::pass_halves(store, &places, ops, None, now, disk) {
        Ok(made) => made,
        Err(e) => return vec![format!("the pass stopped on the store: {e}")],
    };
    let mut said = Vec::new();
    for act in &made.acts {
        said.push(act.line());
        let hub = registry.and_then(|r| r.get(&act.owner));
        match (act.act, act.kind.as_str()) {
            // The heads are told a close is coming; the conversation keeps no row for it, because
            // this pass holds no ledger and a row is what the model is charged for.
            (ReapAct::Plan, "notice") => {
                if let Some(hub) = hub {
                    hub.warn("idle_closing", act.said.clone());
                }
            }
            // And the close is the daemon's own: the registry entry goes, a head is detached and
            // the worker serves `Work::Close` when it next can. Nothing here reaches into the
            // worker's map from a second thread.
            (ReapAct::Close, _) => {
                let hub = registry.and_then(|r| r.get(&act.owner));
                if let Some(hub) = hub {
                    hub.warn("idle_closed", act.said.clone());
                }
                if let Some(registry) = registry
                    && registry.close_session(&act.owner)
                    && let Some(settler) = registry.close_settler()
                {
                    settler.closed(&act.owner);
                }
            }
            _ => {}
        }
    }
    if let Some(registry) = registry {
        said.extend(auto_archive(store, registry, now));
    }
    said
}

/// Put away what settled since this daemon came up (plan 1.6). Older facts are the
/// backfill's, `letibot gc --archive-settled`, which lists before it writes.
pub fn auto_archive(store: &Store, registry: &Registry, now: i64) -> Vec<String> {
    use letibot_tools::lifecycle::autoarchive;
    let live = |id: &str| live_hold(registry, id);
    // The row only: an idle session this daemon serves stays served, so a parent can still
    // send to the child it collected, and the open that follows takes it back into the list.
    let act = autoarchive::write_to(store);
    autoarchive::sweep(
        store,
        &autoarchive::Here::this_process(),
        &live,
        registry.started_ms() as i64,
        now,
        Some(&act),
    )
    .into_iter()
    .filter(|d| d.archived)
    .map(|d| d.line())
    .collect()
}

/// What this daemon's own hub says holds a session: a head, a turn or an open card.
pub fn live_hold(registry: &Registry, id: &str) -> Option<Option<String>> {
    let hub = registry.get(id)?;
    Some(if hub.attached_heads() > 0 {
        Some("a head is attached".into())
    } else if hub.turn_in_flight() {
        Some("a turn is in flight".into())
    } else if !hub.open_ask_ids().is_empty() {
        Some("an ask is open".into())
    } else {
        None
    })
}

/// Renew the lease of every owner this daemon serves that a head, a turn or a job still holds:
/// the lease is in `shared.db`, so every reaper on the box reads it. A landed package's head
/// does not count, so a finished package still idles out with its seat attached.
fn renew_held(store: &Store, registry: &Registry, now: i64) {
    for id in registry.ids() {
        let Some(hub) = registry.get(&id) else {
            continue;
        };
        let Ok(Some(owner)) = store.owner(&id) else {
            continue;
        };
        if owner.state != letibot_tokencore::store::OwnerState::Live {
            continue;
        }
        let head = hub.attached_heads() > 0 && owner.landed_sha.is_none();
        let job = store
            .processes_of(&id)
            .is_ok_and(|v| v.iter().any(|p| p.state.is_open()));
        if head || hub.turn_in_flight() || job {
            let _ = store.touch_owner(&id, now);
        }
    }
}

/// The reaper's thread: a pass at start, then every [`SWEEP`] or sooner when [`look_now`] asks,
/// until the daemon stops. Its own store connection; nothing at all when there is no store.
pub fn spawn(store_at: Option<impl Into<StoreAt>>, registry: Arc<Registry>) {
    let Some(at) = store_at.map(Into::into) else {
        return;
    };
    let _ = std::thread::Builder::new()
        .name("owner-reaper".into())
        .spawn(move || {
            let Ok(store) = at.open() else {
                return;
            };
            let ops = daemon_ops(&registry);
            let every = lifecycle::Settings::load_or_default()
                .disk_every_ms
                .map_or(DISK_EVERY, |ms| Duration::from_millis(ms as u64));
            let mut turn = DiskTurn::for_store(&at, every);
            loop {
                let now = letibot_metrics::now_ms() as i64;
                let disk = turn.due(now);
                for line in sweep(&store, Some(&registry), &ops, disk) {
                    eprintln!("  reaper: {line}");
                }
                if disk {
                    turn.ran(now);
                }
                let began = std::time::Instant::now();
                while began.elapsed() < SWEEP && !LOOK_NOW.swap(false, Ordering::Relaxed) {
                    if registry.is_closing() || registry.is_closed() {
                        return;
                    }
                    std::thread::sleep(Duration::from_secs(1));
                }
                if registry.is_closing() || registry.is_closed() {
                    return;
                }
            }
        });
}

/// Which daemon on a box runs the disk half: the one holding `reaper.lock` beside the store. The
/// lock is `flock`, so a daemon that stops or is killed hands it over with no lease to expire.
/// The file holds when the box's last disk pass began, so a new holder keeps the period.
pub struct DiskTurn {
    path: PathBuf,
    every_ms: i64,
    held: Option<std::fs::File>,
}

impl DiskTurn {
    pub fn new(path: impl Into<PathBuf>, every: Duration) -> DiskTurn {
        DiskTurn {
            path: path.into(),
            every_ms: every.as_millis() as i64,
            held: None,
        }
    }

    /// The lock beside a store: one per data directory, which is one per box.
    pub fn for_store(at: &StoreAt, every: Duration) -> DiskTurn {
        DiskTurn::new(lock_path(at), every)
    }

    /// Whether this daemon runs the disk half at `now_ms`: it holds the lock, and the box's last
    /// disk pass is a period old. A clock that went back counts as due rather than stalling.
    pub fn due(&mut self, now_ms: i64) -> bool {
        if self.held.is_none() {
            self.held = take_lock(&self.path);
        }
        let Some(file) = self.held.as_mut() else {
            return false;
        };
        let mut text = String::new();
        let last = file
            .rewind()
            .and_then(|_| file.read_to_string(&mut text))
            .ok()
            .and_then(|_| text.trim().parse::<i64>().ok());
        last.is_none_or(|last| now_ms < last || now_ms - last >= self.every_ms)
    }

    /// Record that a disk pass began at `now_ms`.
    pub fn ran(&mut self, now_ms: i64) {
        if let Some(file) = self.held.as_mut() {
            let _ = file
                .set_len(0)
                .and_then(|_| file.rewind())
                .and_then(|_| writeln!(file, "{now_ms}"));
        }
    }

    pub fn holds(&self) -> bool {
        self.held.is_some()
    }
}

fn lock_path(at: &StoreAt) -> PathBuf {
    match at {
        StoreAt::Set { dir, .. } => dir.join("reaper.lock"),
        StoreAt::File(path) => path.with_extension("reaper.lock"),
    }
}

fn take_lock(path: &Path) -> Option<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .ok()?;
    file.try_lock().ok()?;
    Some(file)
}

/// The owner's lease renewed on a clock while a turn runs. A round renews it once, and one tool
/// call can outlast the idle TTL inside a round; a turn in flight is never idle (d.3).
pub struct LeaseBeat {
    stop: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl LeaseBeat {
    /// Start beating for `owner`, a quarter of its idle TTL apart, on a connection of its own.
    /// Nothing starts for an owner with no row or no idle TTL: there is no idle close to hold off.
    pub fn start(at: Option<&StoreAt>, owner: &str) -> LeaseBeat {
        let none = LeaseBeat {
            stop: None,
            thread: None,
        };
        let Some(at) = at.cloned() else {
            return none;
        };
        let Some(ttl) = at
            .open_existing()
            .ok()
            .and_then(|s| s.owner(owner).ok().flatten())
            .and_then(|o| o.idle_ttl_ms)
        else {
            return none;
        };
        let every = Duration::from_millis((ttl / 4).clamp(50, 60_000) as u64);
        let (stop, stopped) = std::sync::mpsc::channel::<()>();
        let owner = owner.to_string();
        let thread = std::thread::Builder::new()
            .name("lease-beat".into())
            .spawn(move || {
                let Ok(store) = at.open_existing() else {
                    return;
                };
                loop {
                    let _ = store.touch_owner(&owner, letibot_metrics::now_ms() as i64);
                    match stopped.recv_timeout(every) {
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                        _ => return,
                    }
                }
            })
            .ok();
        LeaseBeat {
            stop: Some(stop),
            thread,
        }
    }

    /// Whether a beat is running, for a test to tell an owner with no idle TTL from a failed start.
    pub fn beating(&self) -> bool {
        self.thread.is_some()
    }
}

impl Drop for LeaseBeat {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lock_in(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("reap-turn-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("reaper.lock")
    }

    /// Three daemons sweep every minute for an hour: the disk half runs once per period, by one
    /// of them, and when that one stops another takes over without running early.
    #[test]
    fn one_daemon_per_box_runs_the_disk_half() {
        let path = lock_in("three");
        let every = Duration::from_secs(600);
        let mut daemons: Vec<Option<DiskTurn>> =
            (0..3).map(|_| Some(DiskTurn::new(&path, every))).collect();
        let minute = 60_000i64;
        let mut runs: Vec<(i64, usize)> = Vec::new();
        for m in 0..60 {
            let now = 1_000_000 + m * minute;
            if m == 25 {
                let holder = daemons
                    .iter()
                    .position(|d| d.as_ref().is_some_and(DiskTurn::holds))
                    .expect("someone holds the turn");
                daemons[holder] = None;
                // A fork elsewhere in this binary can hold a copy of the lock's fd until its exec;
                // wait for the release, since an hour of minutes here passes in microseconds.
                for _ in 0..500 {
                    if let Some(probe) = take_lock(&path) {
                        drop(probe);
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            for (i, d) in daemons.iter_mut().enumerate() {
                let Some(d) = d else { continue };
                if d.due(now) {
                    d.ran(now);
                    runs.push((now, i));
                }
            }
        }
        let at: Vec<i64> = runs.iter().map(|(t, _)| (t - 1_000_000) / minute).collect();
        assert_eq!(at, vec![0, 10, 20, 30, 40, 50], "{runs:?}");
        let first = runs[0].1;
        assert!(runs[..3].iter().all(|(_, i)| *i == first), "{runs:?}");
        let next = runs[3].1;
        assert_ne!(next, first, "{runs:?}");
        assert!(runs[3..].iter().all(|(_, i)| *i == next), "{runs:?}");
    }

    /// A fresh box runs the disk half at once, and a lock file it cannot read is no stall.
    #[test]
    fn the_first_holder_runs_at_once() {
        let path = lock_in("first");
        std::fs::write(&path, "not a time\n").unwrap();
        let mut d = DiskTurn::new(&path, Duration::from_secs(600));
        assert!(d.due(5));
        d.ran(5);
        assert!(!d.due(6));
        assert!(d.due(3), "a clock that went back is due");
    }
}
