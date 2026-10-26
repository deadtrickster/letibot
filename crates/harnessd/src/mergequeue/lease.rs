//! **Who this daemon is to the queues, and how long a silence lasts before another takes over**
//! (R61 requirement 1, decision 1: *"a lease per repository among that repository's session
//! daemons — one holds it, with a heartbeat; another takes over when it dies; a daemon whose build
//! does not match the store never takes it. No dedicated queue process."*).
//!
//! The lease itself is a store row ([`letibot_tokencore::store::QueueLease`]); this is the
//! daemon's side of it — its identity and its clock.

use std::sync::OnceLock;
use std::time::Duration;

use letibot_tokencore::store::{LeaseClaim, QueueLease, SCHEMA_VERSION};

/// **How old a heartbeat may be before the lease is anybody's** — ninety seconds: eighteen missed
/// beats at [`HEARTBEAT_EVERY`], so a daemon held up by a slow store write is not mistaken for a
/// dead one, and a dead one is replaced within a minute and a half.
pub const LEASE_TTL_MS: u64 = 90_000;

/// How often a lease holder says it is alive — between passes. A gate runs on its own thread
/// while the passes go on (R92 item 8), so a thirty-minute gate does not lose its lease; the
/// gate's polls read what the last beat found.
pub const HEARTBEAT_EVERY: Duration = Duration::from_secs(5);

/// **A lease no claimant will ever take over, and why the supervisor gives it up** (2026-10-26).
///
/// Takeover is [`letibot_tokencore::store::Store::acquire_queue_lease`]'s, and it handles a
/// silence: a claim on a lease whose heartbeat is older than [`LEASE_TTL_MS`] takes it. What it
/// cannot do is reach a lease nobody CLAIMS — and the operator's store held `queue_lease` rows
/// from daemons that had been dead for weeks (`letim`, `fctop`), for repositories no running
/// daemon serves, so nobody ever passed to take them over and the pane went on drawing them as
/// *served by*. So a supervisor's pass releases them: the row goes, the history says why, and a
/// daemon that starts there claims a free lease.
///
/// **The pid is the second fact, and it is what makes the deletion safe.** An expired heartbeat
/// alone is enough for a claimant to take over, but deleting a LIVE process's row is a different
/// act: the daemon that owns it would find its next heartbeat answered `false` and stop serving.
/// So the pid is asked, on this host only — a lease from another box is expired by the TTL and
/// nothing else, since its pids mean nothing here. `Some(why)` is *gone*; `None` is *leave it*.
pub fn gone(l: &QueueLease, me: &LeaseClaim, now_ms: u64) -> Option<String> {
    if l.owner == me.owner {
        return None;
    }
    let age = now_ms.saturating_sub(l.heartbeat_ms);
    if age <= LEASE_TTL_MS {
        return None;
    }
    let alive_here = l.host == me.host && process_alive(l.pid);
    if alive_here {
        return None;
    }
    Some(if l.host == me.host {
        format!(
            "its heartbeat was {} s old and pid {} is not running on {}",
            age / 1000,
            l.pid,
            l.host
        )
    } else {
        format!(
            "its heartbeat was {} s old on {}, a host this daemon cannot ask about its pids",
            age / 1000,
            l.host
        )
    })
}

/// Whether `pid` names a live, non-zombie process — the same reading `pkill` and the monitors
/// take, so a pid the kernel has since reused is a judgement this does not have to make.
fn process_alive(pid: u32) -> bool {
    letibot_tools::exec::procs::read(pid).is_some_and(|p| p.state != 'Z')
}

/// **This daemon's identity on the queues** — host, pid and start time, so a pid the kernel
/// reuses is not mistaken for the daemon that held it; and the build, so the pane can say WHICH
/// binary serves a repository (the 2026-10-11 store was served by nine, of nine builds).
pub fn this_daemon() -> &'static LeaseClaim {
    static ME: OnceLock<LeaseClaim> = OnceLock::new();
    ME.get_or_init(|| {
        let pid = std::process::id();
        let host = hostname();
        let started = super::gate::now_ms();
        LeaseClaim {
            owner: format!("{host}/{pid}/{started}"),
            pid,
            host,
            build: build_name(),
        }
    })
}

/// **What this binary is**: the package version, the schema it knows, and when the executable
/// was built (its mtime) — the fact that told a stale daemon from a fresh one on 2026-10-10.
pub fn build_name() -> String {
    let built = std::env::current_exe()
        .ok()
        .and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|| "?".into());
    format!(
        "harnessd {} · schema {SCHEMA_VERSION} · built {built}",
        env!("CARGO_PKG_VERSION")
    )
}

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "localhost".into())
}
