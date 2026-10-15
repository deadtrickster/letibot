//! **A child's VM does not outlive the child** — the daemon's half: whose each live firecode VM
//! is, by this store's records, and the reaper that brings down the ones whose owner is gone.
//!
//! The operator's report, 2026-10-15: pg-noop's review of `feat/memory-engine` said *"review ·
//! waiting for a VM slot · every slot busy"* and *"VM slots exhausted · waited 33 min"* for
//! hours. All four taps were held by orphaned firecracker VMs (parent pid 1), and what the store
//! says about them is the whole story:
//!
//! * three were booted on 2026-10-14 at 17:18–17:20 UTC for grandchildren of
//!   `s-1791151788256943467`. Each child's last transcript item is a tool call with no result
//!   (17:21:48, 17:22:06, 17:36:54), and their rows in `subagent_run` still say `running`, owned
//!   by daemon `d-404553`. That daemon's `daemon_life` row has no stop: its last heartbeat is
//!   18:21:01 UTC, so it ended without an orderly stop and `firecode::down_all` — the daemon's
//!   last act — never ran. Each VM's launcher (`firecode exec`, its own session leader) kept it
//!   up. Two of the three were spawned by children whose own parent had FAILED a minute earlier
//!   (`…-sub-1791998134948544726`, a store error at 17:18:21) — a child that ends on a failure
//!   left its children running (fixed in `HarnessTaskRunner::run_to_completion`);
//! * the fourth was a pg-noop great-grandchild booted on 2026-10-12 by a daemon older than
//!   `daemon_life` and `subagent_run` — no record names its owner's state at all;
//! * nothing on any later start looked at VMs, and the restart pass reads a dead daemon's child
//!   rows only when the child's PARENT is opened again. These parents had finished, so nothing
//!   was ever going to open them, and the rows say `running` to this day.
//!
//! # The record
//!
//! A child's VM is booted on its copy, `~/.cache/letibot/firecode/<child id>`
//! (`FirecodeBackend::project_of`), and firecode writes that path into the run's record
//! (`runs/<run>/project`) — so firecode's own record names the owning child, for every build
//! that ever booted one, with nothing added to the store's schema (the store is shared by daemons
//! of different builds). The child's `subagent_run` row says its state and which daemon runs it;
//! that daemon's `daemon_life` row says whether it is alive. See
//! `letibot_tools::firecode::held` for the VM half.
//!
//! # What may be reaped ([`judge`])
//!
//! Only a VM whose recorded owner is FINISHED (its row settled, longer ago than its own `down`
//! can take to deliver the work), or whose owner's row says `running` under a daemon that is
//! gone. Never a VM another live daemon's running child holds, never this daemon's own running
//! child's, and never one with no record of its owner — those are named in the capacity notice
//! and left to a person.
//!
//! # And a child whose parent is gone ends too
//!
//! A dead daemon's `running` row whose parent has finished is nobody's: no open will ever read
//! it. Its VM is reaped and the row is settled `abandoned`, with why. In THIS daemon, a running
//! child whose parent's row has been settled for [`STALL_MS`] and whose own transcript has not
//! moved for as long is interrupted and its row written `killed` — after which its VM, if it
//! still has one, is a finished owner's and is reaped like any other.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use letibot_sessionlog::registry::Registry;
use letibot_tokencore::store::{DaemonLife, Store, SubagentRun};
use letibot_tools::firecode::held::{self, HeldVm, Patience};

/// **A finished owner's VM is left this long before it is reaped** — the most its own `down` waits
/// on a copy-out (`firecode::COPY_OUT_CAP`, 600 s), so a reap never races the delivery of a
/// child that has just finished.
pub const FINISHED_GRACE_MS: u64 = 10 * 60 * 1000;

/// **A child whose parent finished this long ago, and which has written nothing for as long, is
/// stalled** — the same half hour after which a capacity wait is raised.
pub const STALL_MS: u64 = 30 * 60 * 1000;

/// **How often the reaper looks**, when nothing asks it to look sooner ([`look_now`]).
pub const SWEEP_EVERY: Duration = Duration::from_secs(5 * 60);

/// A daemon whose heartbeat is older than this is not known to be alive by its beat
/// (`restart::BEAT` is 30 s).
const BEAT_STALE_MS: u64 = 3 * 30 * 1000;

/// **The daemon a running child's row names, as far as this store and this host can tell.**
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Daemon {
    /// This daemon.
    This,
    /// Another daemon, alive: beating, or its process is still a `harnessd`.
    Live,
    /// Gone: it stopped (`stopped_ms`), or its last beat is stale and no `harnessd` has its pid.
    /// `since_ms` is when it was last known alive.
    Gone { since_ms: Option<u64> },
}

/// **Whose a VM is, and what that owner is doing.**
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Owner {
    /// No child id in the VM's record, or no `subagent_run` row for it — a VM some other tool
    /// booted, a reviewer, or a child older than the record.
    Unrecorded { child: Option<String> },
    /// Its row is settled (`done`, `failed`, `killed`, `abandoned`, `paused`) since `since_ms`.
    Finished {
        child: String,
        state: String,
        since_ms: u64,
    },
    /// Its row says `running`, under `instance`.
    Running {
        child: String,
        instance: String,
        daemon: Daemon,
    },
}

/// **May this VM be reaped, and why** — the sentence is the capacity notice's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Reap(String),
    Keep(String),
}

impl Verdict {
    pub fn reaps(&self) -> bool {
        matches!(self, Verdict::Reap(_))
    }
}

fn minutes(ms: u64) -> u64 {
    ms / 60_000
}

/// **The rule** — pure, so each owner's case is asserted without a VM. See the module docs.
pub fn judge(owner: &Owner, now_ms: u64) -> Verdict {
    match owner {
        Owner::Unrecorded { child } => Verdict::Keep(match child {
            Some(c) => format!(
                "its owner `{c}` has no record in this store, so nothing says it is finished — \
                 not this daemon's to stop"
            ),
            None => "no record names its owner — not this daemon's to stop".into(),
        }),
        Owner::Finished {
            child,
            state,
            since_ms,
        } => {
            let ago = now_ms.saturating_sub(*since_ms);
            if ago >= FINISHED_GRACE_MS {
                Verdict::Reap(format!(
                    "its owner `{child}` is `{state}` ({} min ago) and its VM is still up",
                    minutes(ago)
                ))
            } else {
                Verdict::Keep(format!(
                    "its owner `{child}` is `{state}` ({} min ago); its own `down` may still be \
                     delivering the work",
                    minutes(ago)
                ))
            }
        }
        Owner::Running {
            child,
            instance,
            daemon,
        } => match daemon {
            Daemon::This => Verdict::Keep(format!("its owner `{child}` is running in this daemon")),
            Daemon::Live => Verdict::Keep(format!(
                "its owner `{child}` is running in daemon `{instance}`, which is alive"
            )),
            Daemon::Gone { since_ms } => Verdict::Reap(format!(
                "its owner `{child}` is recorded `running` under daemon `{instance}`, which is \
                 gone{} — nothing is running it",
                since_ms
                    .map(|s| format!(
                        " (last heard {} min ago)",
                        minutes(now_ms.saturating_sub(s))
                    ))
                    .unwrap_or_default()
            )),
        },
    }
}

/// The pid an instance id carries: `d-<pid>-<nanos>-<n>`.
fn pid_of(instance: &str) -> Option<u32> {
    instance.strip_prefix("d-")?.split('-').next()?.parse().ok()
}

/// **Whether `pid` is a daemon of this build's family** — a live process whose program is
/// `harnessd` (or `letibot`). A pid reused by anything else is not the daemon.
pub fn is_a_daemon(proc_root: &Path, pid: u32) -> bool {
    if !held::alive(proc_root, pid) {
        return false;
    }
    std::fs::read(proc_root.join(pid.to_string()).join("cmdline"))
        .ok()
        .and_then(|raw| {
            raw.split(|b| *b == 0)
                .next()
                .map(|a| String::from_utf8_lossy(a).into_owned())
        })
        .is_some_and(|argv0| {
            Path::new(&argv0)
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.contains("harnessd") || n.contains("letibot"))
        })
}

/// **Is the daemon `instance` alive?** `life` is its `daemon_life` row; `daemon_alive` asks the
/// host about a pid. Conservative where the records are thin: a stale beat with a `harnessd`
/// still at that pid is alive (wedged, maybe, but somebody else's to judge).
pub fn daemon_of(
    instance: &str,
    me: &str,
    life: Option<&DaemonLife>,
    now_ms: u64,
    daemon_alive: impl Fn(u32) -> bool,
) -> Daemon {
    if instance == me {
        return Daemon::This;
    }
    if let Some(stopped) = life.and_then(|l| l.stopped_ms) {
        return Daemon::Gone {
            since_ms: Some(stopped),
        };
    }
    if life.is_some_and(|l| now_ms.saturating_sub(l.beat_ms) < BEAT_STALE_MS) {
        return Daemon::Live;
    }
    if pid_of(instance).is_some_and(&daemon_alive) {
        return Daemon::Live;
    }
    Daemon::Gone {
        since_ms: life.map(|l| l.beat_ms),
    }
}

/// **The child a VM was booted for** — the last component of its project, when the project is a
/// copy under letibot's firecode cache (`cache`).
pub fn owner_of(vm: &HeldVm, cache: &Path) -> Option<String> {
    (vm.project.parent()? == cache)
        .then(|| vm.project.file_name()?.to_str().map(str::to_string))
        .flatten()
}

/// letibot's firecode cache — where every child's copy is booted.
pub fn cache_dir() -> PathBuf {
    letibot_tools::firecode::FirecodeBackend::project_of("x")
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

/// **One VM and its owner, judged.**
#[derive(Debug, Clone)]
pub struct Holder {
    pub vm: HeldVm,
    pub owner: Owner,
    pub verdict: Verdict,
    /// The owner's row, when there is one.
    pub run: Option<SubagentRun>,
}

impl Holder {
    /// *"fccode1: firecracker pid 861706, run … started …; owner `s-…` (running under daemon
    /// d-404553, which is gone) — reaped"* — one line of the capacity notice.
    pub fn line(&self) -> String {
        let (Verdict::Reap(why) | Verdict::Keep(why)) = &self.verdict;
        format!(
            "{}; {why} — {}",
            self.vm.line(),
            if self.verdict.reaps() {
                "being reaped"
            } else {
                "left up"
            }
        )
    }
}

/// **The owner of a VM, read off the store.**
pub fn owner(
    store: &Store,
    child: Option<String>,
    me: &str,
    now_ms: u64,
) -> (Owner, Option<SubagentRun>) {
    let Some(child) = child else {
        return (Owner::Unrecorded { child: None }, None);
    };
    let Ok(Some(run)) = store.subagent_run(&child) else {
        return (Owner::Unrecorded { child: Some(child) }, None);
    };
    let owner = if run.state == "running" {
        let life = store.life(&run.instance).ok().flatten();
        Owner::Running {
            child,
            instance: run.instance.clone(),
            daemon: daemon_of(&run.instance, me, life.as_ref(), now_ms, |p| {
                is_a_daemon(Path::new("/proc"), p)
            }),
        }
    } else {
        Owner::Finished {
            child,
            state: run.state.clone(),
            since_ms: run.updated_ms,
        }
    };
    (owner, Some(run))
}

/// **Every live VM on this host, with its owner judged** — empty where there is no firecode
/// (`bin` is the CLI whose `runs/` are read; `None` reads nothing).
pub fn holders(store: &Store, me: &str, now_ms: u64, bin: Option<&Path>) -> Vec<Holder> {
    let Some(runs) = bin.and_then(held::runs_dir) else {
        return Vec::new();
    };
    let cache = cache_dir();
    held::live_vms(&runs, Path::new("/proc"))
        .into_iter()
        .map(|vm| {
            let (owner, run) = owner(store, owner_of(&vm, &cache), me, now_ms);
            let verdict = judge(&owner, now_ms);
            Holder {
                vm,
                owner,
                verdict,
                run,
            }
        })
        .collect()
}

/// **The capacity notice's lines** — what holds each busy slot (a VM with a tap), and asks the
/// reaper to look now when one of them may be reaped. Called by the review queue on a long wait.
pub fn slot_holders(store: &Store, me: &str) -> Vec<String> {
    let now = letibot_sessionlog::registry::now_ms();
    let bin = letibot_tools::firecode::firecode_available().ok();
    let held: Vec<Holder> = holders(store, me, now, bin.as_deref())
        .into_iter()
        .filter(|h| h.vm.tap.is_some())
        .collect();
    if held.iter().any(|h| h.verdict.reaps()) {
        look_now();
    }
    held.iter().map(Holder::line).collect()
}

/// **A parent that has finished for good** — answered, failed, stopped or given up on. `paused`
/// is not: a reminded parent comes back with `task_resume`, and its children with it.
fn finished(state: &str) -> bool {
    matches!(state, "done" | "failed" | "killed" | "abandoned")
}

/// **A running child of a gone daemon, whose parent has finished, is settled `abandoned`** — it is
/// nobody's: the restart pass reads a dead daemon's children only when their parent is opened,
/// and a finished parent is not. `None` when the row is not such a child.
pub fn orphan_why(
    run: &SubagentRun,
    parent: Option<&SubagentRun>,
    daemon: &Daemon,
) -> Option<String> {
    if run.state != "running" {
        return None;
    }
    let Daemon::Gone { .. } = daemon else {
        return None;
    };
    let parent = parent?;
    if !finished(&parent.state) {
        return None;
    }
    Some(format!(
        "its daemon `{}` ended without an orderly stop while it ran, and its parent `{}` had \
         already finished (`{}`), so no restart was ever going to bring it back; its VM, if it \
         had one, was reaped",
        run.instance, parent.child_id, parent.state
    ))
}

/// **A child of THIS daemon stalled under a finished parent** — `Some(why)` when the row is
/// `running` here, its parent's row has been settled for [`STALL_MS`], and its own transcript's
/// newest item is as old (`last_activity_ms`).
pub fn stalled_why(
    run: &SubagentRun,
    parent: Option<&SubagentRun>,
    me: &str,
    last_activity_ms: u64,
    now_ms: u64,
) -> Option<String> {
    if run.state != "running" || run.instance != me {
        return None;
    }
    let parent = parent?;
    if !finished(&parent.state) {
        return None;
    }
    let parent_ago = now_ms.saturating_sub(parent.updated_ms);
    let quiet = now_ms.saturating_sub(last_activity_ms.max(run.updated_ms));
    (parent_ago >= STALL_MS && quiet >= STALL_MS).then(|| {
        format!(
            "stopped: its parent `{}` finished (`{}`) {} min ago and it has made no progress for \
             {} min",
            parent.child_id,
            parent.state,
            minutes(parent_ago),
            minutes(quiet)
        )
    })
}

static LOOK_NOW: AtomicBool = AtomicBool::new(false);

/// **Ask the reaper to look now** rather than at its next sweep.
pub fn look_now() {
    LOOK_NOW.store(true, Ordering::Relaxed);
}

/// **One pass**: settle the orphans, stop this daemon's stalled children, and reap every VM the
/// records say may go (`bin` is the firecode CLI; `None` reaps nothing). Answers one line per
/// thing it did, for the daemon's log.
pub fn pass(
    store: &Store,
    registry: Option<&Registry>,
    me: &str,
    bin: Option<&Path>,
) -> Vec<String> {
    let now = letibot_sessionlog::registry::now_ms();
    let mut said = Vec::new();
    let runs = store.subagent_runs().unwrap_or_default();
    let by_id = |id: &str| runs.iter().find(|r| r.child_id == id);
    for run in runs.iter().filter(|r| r.state == "running") {
        let parent = by_id(&run.parent_id);
        if run.instance == me {
            let activity = store
                .session(&run.child_id)
                .ok()
                .flatten()
                .map(|s| s.last_activity_ms.max(0) as u64)
                .unwrap_or(0);
            if let Some(why) = stalled_why(run, parent, me, activity, now) {
                if let Some(hub) = registry.and_then(|r| r.get(&run.child_id)) {
                    let _ = hub.submit(
                        letibot_sessionlog::hub::DAEMON_SUBMITTER,
                        format!(
                            "vm-reap-{}",
                            letibot_sessionlog::registry::short_id(&run.child_id)
                        ),
                        0,
                        letibot_sessionlog::CommandKind::Interrupt {
                            reason: why.clone(),
                        },
                    );
                }
                let _ = store.settle_subagent_run(&run.child_id, "killed", Some(&why));
                said.push(format!("{} · {why}", run.child_id));
            }
            continue;
        }
        let life = store.life(&run.instance).ok().flatten();
        let daemon = daemon_of(&run.instance, me, life.as_ref(), now, |p| {
            is_a_daemon(Path::new("/proc"), p)
        });
        if let Some(why) = orphan_why(run, parent, &daemon) {
            let _ = store.settle_subagent_run(&run.child_id, "abandoned", Some(&why));
            said.push(format!("{} · abandoned: {why}", run.child_id));
        }
    }
    let Some(bin) = bin else {
        return said;
    };
    for h in holders(store, me, now, Some(bin)) {
        let Verdict::Reap(why) = &h.verdict else {
            continue;
        };
        let reaped = held::reap(bin, &h.vm, Path::new("/proc"), Patience::default());
        said.push(format!("{} · {why} — {}", h.vm.line(), reaped.said()));
    }
    said
}

/// **The reaper's thread**: a pass at start — before anything waits on a slot a dead daemon's
/// child still holds — then every [`SWEEP_EVERY`], or sooner when [`look_now`] asks, until the
/// daemon stops. Its own store connection; nothing when there is no store.
pub fn spawn(store_path: Option<PathBuf>, registry: Arc<Registry>) {
    let Some(path) = store_path else {
        return;
    };
    let _ = std::thread::Builder::new()
        .name("vm-reaper".into())
        .spawn(move || {
            let Ok(store) = Store::open(&path) else {
                return;
            };
            let me = registry.instance().to_string();
            let bin = letibot_tools::firecode::firecode_available().ok();
            loop {
                for line in pass(&store, Some(&registry), &me, bin.as_deref()) {
                    eprintln!("  vm reaper: {line}");
                }
                let began = std::time::Instant::now();
                while began.elapsed() < SWEEP_EVERY && !LOOK_NOW.swap(false, Ordering::Relaxed) {
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

#[cfg(test)]
mod tests;
