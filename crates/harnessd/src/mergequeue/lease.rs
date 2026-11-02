//! Who this daemon is to the queues, and how long a silence lasts before another takes over
//! .
//! The lease is a store row; this is the daemon's side of it - its identity and its clock.

use std::sync::OnceLock;
use std::time::Duration;

use letibot_tokencore::store::{LeaseClaim, QueueLease, SCHEMA_VERSION};

/// How old a heartbeat may be before the lease is anybody's - ninety seconds, eighteen missed
/// beats at [`HEARTBEAT_EVERY`]: a daemon held up by a slow store write is not mistaken for a dead
/// one, and a dead one is replaced within a minute and a half.
pub const LEASE_TTL_MS: u64 = 90_000;

/// How often a lease holder says it is alive, between passes. A gate runs on its own thread while
/// the passes go on, so a thirty-minute gate does not lose its lease.
pub const HEARTBEAT_EVERY: Duration = Duration::from_secs(5);

/// A lease no claimant will ever take over : takeover only reaches a lease somebody
/// CLAIMS, so rows from daemons dead for weeks sat there naming repositories nobody serves. The pid
/// is asked on this host only - elsewhere it means nothing. `Some(why)` is *gone*; `None` is *leave*.
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

/// Whether `pid` names a live, non-zombie process - the same reading `pkill` and the monitors
/// take, so a pid the kernel has reused is a judgement this does not make.
fn process_alive(pid: u32) -> bool {
    letibot_tools::exec::procs::read(pid).is_some_and(|p| p.state != 'Z')
}

/// This daemon's identity on the queues - host, pid and start time, so a reused pid is not
/// mistaken for the daemon that held it; and the build, so the pane can say WHICH binary serves.
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

/// What this binary is: the package version, the schema it knows, and when the executable was
/// built (its mtime) - the fact that tells a stale daemon from a fresh one.
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
