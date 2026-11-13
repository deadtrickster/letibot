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

/// [`pass`] with hands the caller keeps, so the ssh connections a remote host's pass opens are
/// reused from one sweep to the next.
pub fn pass_with(store: &Store, registry: Option<&Registry>, ops: &lifecycle::Ops) -> Vec<String> {
    let places = Places::here();
    let now = letibot_metrics::now_ms() as i64;
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
    said
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
            let ops = lifecycle::Ops::here(format!("daemon {}", registry.instance()));
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
