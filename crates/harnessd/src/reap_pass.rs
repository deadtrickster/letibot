//! The daemon's own reaper: close what is idle, past a hard TTL, or under a daemon that is gone,
//! let go of it, and free its disk (`docs/lifecycle.md`, packages G2 and G4).
//!
//! The same pass runs three ways and this is the fast one: a daemon that is up notices its own
//! owner going quiet, where `letibot gc --now` and the timer are what covers the box whose daemons
//! are not there. It reads the store and the kernel and asks no daemon anything, so an owner whose
//! daemon was SIGKILLed is closed by the one that replaced it.
//!
//! A close here is the SAME close a `leticode_stop` is: the registry entry goes, a head left
//! attached is detached, the worker serves `Work::Close` from its own queue, and the settler brings
//! down a VM the worker cannot reach (`close_settle`). What this file adds is the decision and the
//! journal, never a second way to close a session.

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
    let places = Places::here();
    let now = letibot_metrics::now_ms() as i64;
    if let Some(registry) = registry {
        renew_held(store, registry, now);
    }
    let made = match lifecycle::pass(store, &places, ops, None, now) {
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

/// Put away what settled since this daemon came up (plan 1.6, R276). Older facts are the
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
            loop {
                for line in pass_with(&store, Some(&registry), &ops) {
                    eprintln!("  reaper: {line}");
                }
                let began = std::time::Instant::now();
                while began.elapsed() < SWEEP && !LOOK_NOW.swap(false, Ordering::Relaxed) {
                    if registry.is_stopping() || registry.is_closed() {
                        return;
                    }
                    std::thread::sleep(Duration::from_secs(1));
                }
                if registry.is_stopping() || registry.is_closed() {
                    return;
                }
            }
        });
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
