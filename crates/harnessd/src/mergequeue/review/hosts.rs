//! **Where a reviewer runs when the placement is `host`** — this machine, or one the operator
//! declared in `hosts.toml`, chosen by how much of it is left.
//!
//! The operator, 2026-10-29: *"well i guess for host placement we can just kinda what? fan out per
//! global host limit"*, and then, when asked who chooses the machines: *"yeah ok, use what is in
//! hosts"*. So there is no list a person has to fill in before the queue gets faster: the pool IS
//! `hosts.toml`, it grows and shrinks with that file, and this machine is always in it.
//!
//! # What this module decides, and what it does not
//!
//! It answers three questions and nothing else:
//!
//! * **how many reviewers the pool can take** ([`ReviewHosts::capacity`]) — what
//!   [`super::super::capacity::review_limit`] uses for the queue's claim limit;
//! * **which machine the next reviewer goes to** ([`ReviewHosts::choose`]) — the one with the most
//!   spare cores, among those with room under the per-host cap;
//! * **which machine is having a bad day** ([`ReviewHosts::mark_down`]) — a host that refused a
//!   placement in this pass is skipped for the rest of it, so fourteen rows do not each spend a
//!   timeout discovering the same reboot.
//!
//! The placement itself is not invented here: a reviewer on a host is a `task` child with
//! `Placement::Remote(name)` — the same door `where: "host:NAME"` takes — and the tree, the
//! checkout, the confinement probe and the toolchain wrapper are all `SshBackend::place`'s.
//!
//! # Never stall on the network
//!
//! **The facts are a cache, and the cache is refreshed off the caller's thread.** A probe is one
//! bounded ssh call per host, up to four of them in parallel, and a machine that is rebooting takes
//! its own timeout to say so — so doing it inline would stop the queue's pass for seconds every
//! time it looked. [`ReviewHosts::capacity`] and [`ReviewHosts::choose`] therefore answer from the
//! last snapshot (or from this machine alone, before there is one) and *ask* for a refresh; the
//! thread that answers it is the only thing that ever waits.
//!
//! A stale snapshot is fine for what it is used for: spare cores move slowly, and a host that has
//! gone down between two probes is caught by the placement itself, which falls back to this
//! machine and says so ([`super::Pool::start_reviewer`]).
//!
//! # How many run at once, on the operator's box (MEASURED 2026-10-29)
//!
//! | machine | cores | load | reserve | spare | reviewers |
//! |---|---|---|---|---|---|
//! | `lab2x1` | 32 | 3.8 | 0 | 28.2 | `min(4, 7) = 4` |
//! | `lubuntu1`, `lubuntu2`, `lubuntu3` | 32 | 0.1–0.7 | 0 | ~31 | `min(4, 7) = 4` each |
//! | this machine | 18 | — | 0 | 18 | `min(4, 4) = 4` |
//!
//! **Twenty reviewers at once**, which is the default `reviewers_at_once` for a `host` queue. The
//! fourteen rows waiting on the operator's box on 2026-10-29 are therefore **one wave instead of
//! the seven** a limit of two made them; what one wave costs is [`super::start_reviewer`]'s doc
//! (20.2 MB and ~3.5 s of transport, 22.5 s and 1.1 GB if the reviewer builds) plus the
//! reviewer's own model time, which no measurement here covers.
//!
//! **The number is CPU-shaped and the model endpoint is the other resource**: twenty concurrent
//! reviewers share one server, and `[queue] reviewers_at_once` is the knob if the server, rather
//! than the machines, is what fills up.
//!
//! # The per-host cap is released by the DESK, not by a bookkeeper
//!
//! `placed` records who this daemon has where, and it is pruned by [`ReviewHosts::keep`] at the end
//! of every pass against the children still on the desk — **the same list, in the same place, as
//! the desk's own `keep`**. That is not tidiness: an earlier cut freed a place at each place a
//! reviewer ENDED, and the normal ending — a verdict handed in ([`super::verdict`]) — was missed, so
//! every completed review kept its place for the life of the daemon and the pool ran down to
//! nothing. A rule that has to be repeated at every ending is a rule with a missing case; a
//! reconciliation against the one list of live children has none.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use letibot_tools::remote::{Host, Hosts, Paths};

use crate::mergequeue::capacity;

/// **How long a snapshot of the pool is good for** — thirty seconds.
///
/// Long enough that a queue pass every two seconds does not spend its life in ssh, short enough
/// that a machine the operator just freed is used in the same minute. The *choice* between two
/// healthy hosts does not need to be fresher than this; the *fallback* when one is down does not
/// depend on it at all.
pub const STALE_AFTER: Duration = Duration::from_secs(30);

/// The bound on one host's probe — under [`PROBE_TOTAL`] so a slow machine's own timeout fires
/// first and the report names *that machine* rather than the whole probe.
pub const HOST_BOUND: Duration = Duration::from_secs(8);

/// The bound on the whole refresh. A refresh that has not finished by then answers for the hosts it
/// heard from and leaves the rest as *not asked yet*, which is a wait, not a verdict.
pub const PROBE_TOTAL: Duration = Duration::from_secs(10);

/// **The name this machine answers to in a pool** — `[queue] reviewer_hosts = ["local"]`.
pub const LOCAL: &str = "local";

/// Hosts a reviewer is never placed on, even when `hosts.toml` declares them or the pool is `*`.
pub const FORBIDDEN: &[&str] = &["lubuntu3"];

/// Whether `name` is a host reviewers are never placed on.
pub fn is_forbidden(name: &str) -> bool {
    FORBIDDEN.contains(&name)
}

/// **One machine, as this pass sees it.**
#[derive(Debug, Clone, PartialEq)]
pub struct Machine {
    /// `None` is this machine — the one the daemon is running on.
    pub name: Option<String>,
    /// `cores − load − reserve`, or `None` when the machine did not say (a host that was not
    /// reached, or one that has not been asked yet). **`None` is not zero**: a machine nobody asked
    /// is a machine that might take four reviewers, and treating it as full is how a pool never
    /// opens.
    pub spare: Option<f64>,
    /// Why it cannot take one, when it cannot: the probe's refusal, or *no spare cores*.
    pub why: Option<String>,
}

impl Machine {
    /// **How many more reviewers this machine takes**, given what this daemon already has there.
    ///
    /// A machine nobody has asked yet takes none: the *capacity* this feeds is the number of rows
    /// the queue claims, and claiming a row for a machine that turns out to be off leaves it
    /// `reviewing` with nobody on it until the start grace raises it.
    pub fn room(&self, taken: usize) -> usize {
        match self.spare {
            Some(spare) => capacity::reviewers_fit(spare).saturating_sub(taken),
            None => 0,
        }
    }

    /// The name a sentence uses: the host's, or *this machine*.
    pub fn label(&self) -> String {
        self.name.clone().unwrap_or_else(|| LOCAL.to_string())
    }
}

/// **The pool of machines a `host` reviewer may be placed on**, with the last thing each said about
/// itself and what this daemon already has running where.
///
/// One of these per daemon, shared by the queue's workers and the review pool (it rides
/// `Config::review_hosts`), because the per-host cap is a fact about *this daemon's* work: two
/// `RepoQueue` threads and the review pool all count against the same four places on `lab2x1`.
#[derive(Debug, Default)]
pub struct ReviewHosts {
    inner: Mutex<Inner>,
    /// A refresh is in flight — one at a time, so a queue that looks twice a second does not spawn
    /// a probe per look.
    refreshing: AtomicBool,
}

#[derive(Debug, Default)]
struct Inner {
    /// What each host last said, by name. Kept across a narrowing of the pool: the operator may
    /// narrow and widen `reviewer_hosts` while the daemon runs, and a fact about `lab2x1` is not
    /// invalidated by a project that stopped naming it.
    facts: BTreeMap<String, Fact>,
    /// When the snapshot was taken.
    at: Option<Instant>,
    /// **The reviewers this daemon has placed, by child handle** — the machine each went to, so the
    /// per-host cap counts this daemon's own work. Pruned by [`ReviewHosts::keep`], which is the
    /// desk's own list of live children: a place is freed by the reviewer being over, however it
    /// ended.
    placed: BTreeMap<String, String>,
    /// **The machines that refused a placement in THIS pass** — skipped until the pass ends, so
    /// fourteen rows do not each pay the same timeout. Cleared by [`ReviewHosts::end_pass`].
    down: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
struct Fact {
    spare: Option<f64>,
    why: Option<String>,
}

impl ReviewHosts {
    pub fn new() -> Arc<ReviewHosts> {
        Arc::new(ReviewHosts::default())
    }

    /// **The machines this project's reviewers may go to** — `pool` is
    /// [`super::super::settings::QueueSettings::reviewer_hosts`], where `None` is *every host the
    /// operator declared, plus this machine*.
    ///
    /// A name that is not declared is NOT silently dropped from the file's meaning: the pool keeps
    /// this machine (a queue that stops because a project named a machine that does not exist would
    /// be worse than one that runs here) and the caller is told by [`ReviewHosts::unknown`], which
    /// [`super::Pool::start_reviewer`] raises once.
    pub fn machines(&self, pool: Option<&[String]>) -> Vec<Machine> {
        let inner = self.lock();
        self.machines_locked(&inner, pool)
    }

    /// **The names a project asked for that nobody declared** — the sentence a pass says, rather
    /// than a pool that is quietly smaller than the file says.
    pub fn unknown(&self, pool: Option<&[String]>) -> Vec<String> {
        let Some(list) = pool else {
            return Vec::new();
        };
        let declared = Hosts::load();
        list.iter()
            .filter(|n| n.as_str() != LOCAL && n.as_str() != "*")
            .filter(|n| is_forbidden(n) || declared.get(n).is_none())
            .cloned()
            .collect()
    }

    /// **How many reviewers the pool can take this pass** — the sum of what each machine has room
    /// for, which is what the queue claims by. A machine that refused a placement this pass counts
    /// for nothing: claiming a row for it would leave that row `reviewing` with nobody on it.
    pub fn capacity(&self, pool: Option<&[String]>) -> usize {
        let inner = self.lock();
        // This machine alone is the default (D18): the old fixed limit, not a spare-core budget.
        if self.machines_locked(&inner, pool).len() == 1 {
            return crate::mergequeue::serve::MAX_REVIEWING;
        }
        self.machines_locked(&inner, pool)
            .iter()
            .filter(|m| {
                !m.name
                    .as_deref()
                    .is_some_and(|n| inner.down.contains_key(n))
            })
            .map(|m| m.room(taken(&inner, m.name.as_deref())))
            .sum()
    }

    /// **Which machine the next reviewer goes to**, or `None` when the pool is full this pass.
    ///
    /// The rule is the operator's: *chosen by spare capacity*. Ties go to this machine, because a
    /// reviewer that runs here costs no transport at all — and the per-host cap is already applied
    /// by [`Machine::room`], so a 32-core box does not take the whole queue.
    pub fn choose(&self, pool: Option<&[String]>) -> Option<Machine> {
        let inner = self.lock();
        // This machine alone is never gated on spare cores, as before the pool existed.
        if let [only] = self.machines_locked(&inner, pool).as_slice() {
            return Some(only.clone());
        }
        let mut best: Option<(usize, f64, Machine)> = None;
        for m in self.machines_locked(&inner, pool) {
            if m.room(taken(&inner, m.name.as_deref())) == 0 {
                continue;
            }
            if m.name
                .as_deref()
                .is_some_and(|n| inner.down.contains_key(n))
            {
                continue;
            }
            let rank = (m.name.is_some() as usize, m.spare.unwrap_or(0.0));
            let better = match &best {
                None => true,
                Some((remote, spare, _)) => (*remote, *spare) < rank,
            };
            if better {
                best = Some((rank.0, rank.1, m));
            }
        }
        best.map(|(_, _, m)| m)
    }

    /// **This machine, if it has room** — the fallback a host's refusal lands on.
    ///
    /// It ignores the pool entirely: a reviewer whose machine refused it is started again here, and
    /// `None` means this machine is full too, which is a wait and not a failure.
    pub fn choose_local(&self) -> Option<Machine> {
        let inner = self.lock();
        let me = self.machines_locked(&inner, Some(&[])).into_iter().next()?;
        (me.room(taken(&inner, None)) > 0).then_some(me)
    }

    /// **Record that a reviewer went to a machine** — the per-host cap counts this daemon's own
    /// work, and [`ReviewHosts::keep`] frees it when the reviewer is over.
    pub fn started(&self, child: &str, machine: &Machine) {
        let mut inner = self.lock();
        inner
            .placed
            .insert(child.to_string(), machine.name.clone().unwrap_or_default());
    }

    /// **Every reviewer that is no longer on the desk is over, and its place is free again.**
    ///
    /// `live` is the children the desk still holds — `ReviewDesk::keep`'s own list, computed in the
    /// same pass and for the same reason. **One reconciliation, not a call at each ending**: a
    /// reviewer ends in five places (a verdict, a death, a vanished child, a row that moved under
    /// it, a row that is gone), and the first cut freed the place at four of them and missed the
    /// one every successful review takes. That is how a pool runs down to nothing while the queue
    /// goes on claiming: each completed review held a place for the life of the daemon.
    ///
    /// It cannot drift, either: an ending added later is covered without being told about this, and
    /// a `?` that returns early out of the collect loop cannot skip it.
    ///
    /// **The one window it cannot see is a daemon restart**: `placed` is this process's, so after a
    /// restart the pool believes the machines are empty until the reviewers the old process left
    /// are collected or time out. It over-admits for that window rather than under-admitting for
    /// ever — the rows themselves are in the store, and `recover_queue` and the review deadline are
    /// what end the ones that outlived their daemon.
    pub fn keep(&self, live: &[String]) {
        let mut inner = self.lock();
        inner.placed.retain(|child, _| live.contains(child));
    }

    /// **Which machine a child of this daemon is on**, when this pool placed it. `Some(None)` is
    /// *this machine*, and `None` is *this pool did not place it* (a reviewer from before a
    /// restart, or a VM one).
    pub fn where_is(&self, child: &str) -> Option<Option<String>> {
        self.lock()
            .placed
            .get(child)
            .map(|n| (!n.is_empty()).then(|| n.clone()))
    }

    /// **This machine is having a bad day** — it refused a placement in this pass, so nothing else
    /// is sent there until the pass ends. The sentence is the host's own.
    pub fn mark_down(&self, name: Option<&str>, why: String) {
        self.lock()
            .down
            .insert(name.unwrap_or_default().to_string(), why);
    }

    /// Why a machine is being skipped this pass, if it is.
    pub fn down(&self, name: Option<&str>) -> Option<String> {
        self.lock().down.get(name.unwrap_or_default()).cloned()
    }

    /// **The pass is over** — a machine that failed a minute ago is tried again next time, because
    /// a reboot is minutes and a queue that never retried would run here for ever.
    pub fn end_pass(&self) {
        self.lock().down.clear();
    }

    /// **Ask every machine how it is, off the caller's thread** — a no-op when a refresh is already
    /// in flight, or when the snapshot is younger than [`STALE_AFTER`].
    ///
    /// The thread is not joined by the queue and holds no lock across a call: a machine that is
    /// rebooting costs its own bound, in a thread nobody is waiting on. [`ReviewHosts::wait`] is
    /// how a test collects it.
    pub fn refresh(
        self: &Arc<Self>,
        pool: Option<&[String]>,
    ) -> Option<std::thread::JoinHandle<()>> {
        if self.lock().at.is_some_and(|at| at.elapsed() < STALE_AFTER) {
            return None;
        }
        if self
            .refreshing
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return None;
        }
        let declared = Hosts::load();
        let hosts: Vec<Host> = self
            .machines(pool)
            .into_iter()
            .filter_map(|m| m.name)
            .filter_map(|n| declared.get(&n).cloned())
            .collect();
        let me = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("review-hosts".into())
            .spawn(move || {
                let began = Instant::now();
                let facts = probe_all(&hosts, PROBE_TOTAL, HOST_BOUND);
                {
                    let mut inner = me.lock();
                    for (name, fact) in facts {
                        inner.facts.insert(name, fact);
                    }
                    inner.at = Some(began);
                }
                me.refreshing.store(false, Ordering::SeqCst);
            });
        match spawned {
            Ok(h) => Some(h),
            Err(_) => {
                self.refreshing.store(false, Ordering::SeqCst);
                None
            }
        }
    }

    /// **Block until a refresh is done** — for a test, and for nothing in the queue.
    pub fn wait(&self, handle: Option<std::thread::JoinHandle<()>>) {
        if let Some(h) = handle {
            let _ = h.join();
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// **Install a snapshot without a network** — a test's door to the one field the probe writes.
    ///
    /// The alternative is a fake `ssh` and a real `hosts.toml`, which is how
    /// `letibot_tools::builtins::hosts` tests its own probe; here the question is never *did the
    /// probe work* but *what does the pool do with the answer*, and a test that had to fake a
    /// machine to ask that would be measuring the fake.
    #[cfg(test)]
    pub(crate) fn set_spare(&self, name: Option<&str>, spare: Option<f64>, why: Option<String>) {
        let mut inner = self.lock();
        match name {
            Some(n) => {
                inner.facts.insert(n.to_string(), Fact { spare, why });
            }
            None => {
                // This machine's own facts are read live (`local_spare`), so a test that wants
                // them fixed says so through the same map, keyed by the empty name.
                inner.facts.insert(String::new(), Fact { spare, why });
            }
        }
        inner.at = Some(Instant::now());
    }

    fn machines_locked(&self, inner: &Inner, pool: Option<&[String]>) -> Vec<Machine> {
        let declared = Hosts::load();
        let mut out = vec![match inner.facts.get("") {
            Some(f) => Machine {
                name: None,
                spare: f.spare,
                why: f.why.clone(),
            },
            None => Machine {
                name: None,
                spare: local_spare(),
                why: None,
            },
        }];
        let names: Vec<String> = match pool {
            None => declared.names(),
            Some(list) => list
                .iter()
                .filter(|n| n.as_str() != LOCAL && n.as_str() != "*")
                .filter(|n| declared.get(n).is_some())
                .cloned()
                .collect(),
        }
        .into_iter()
        .filter(|n| !is_forbidden(n))
        .collect();
        for name in names {
            let fact = inner.facts.get(&name);
            out.push(Machine {
                spare: fact.and_then(|f| f.spare),
                why: fact.and_then(|f| f.why.clone()),
                name: Some(name),
            });
        }
        out
    }
}

fn taken(inner: &Inner, name: Option<&str>) -> usize {
    let key = name.unwrap_or_default();
    inner.placed.values().filter(|v| v.as_str() == key).count()
}

/// **`cores` on this machine, and nothing subtracted** — the number a reviewer here is counted
/// against.
///
/// **No load and no reserve, and both are deliberate.** `reserve` in `hosts.toml` is the operator's
/// statement about a machine they SHARE with other work; the machine the daemon runs on is theirs
/// alone, and there is no file to say otherwise. And the load average here is mostly *the queue's
/// own work plus the operator's*: subtracting it would make the queue throttle itself exactly when
/// the box is busy — a self-reference that reads as a capacity limit. On a declared host the load
/// is somebody else's and is subtracted; here it is ours and is not.
///
/// **Floored at one reviewer's worth of cores**, which is the *"or a fixed number if cores lie"*
/// half: a two-core box, a container with a core budget, or a VM with `available_parallelism`
/// answering 1 would otherwise compute `floor(1 / 4) = 0` and a queue that never runs a reviewer
/// on the one machine it can always reach.
pub fn local_spare() -> Option<f64> {
    let cores = std::thread::available_parallelism().ok()?.get() as f64;
    Some(cores.max(capacity::CORES_PER_REVIEWER))
}

/// **Ask every host at once**, each bounded, and the whole thing bounded — the shape
/// `letibot_tools::builtins::hosts` already uses for the `hosts` tool's `probe: true`, one number
/// over: the spare cores, or the refusal.
fn probe_all(hosts: &[Host], total: Duration, per_host: Duration) -> BTreeMap<String, Fact> {
    let began = Instant::now();
    let (tx, rx) = std::sync::mpsc::channel::<(String, Fact)>();
    for host in hosts {
        let tx = tx.clone();
        let host = host.clone();
        std::thread::spawn(move || {
            let answer = letibot_tools::remote::probe(&host, None, Paths::from_env(), per_host);
            let fact = match answer {
                Ok(f) => match host.spare_cores(f.cores, &f.load) {
                    Some(spare) => Fact {
                        spare: Some(spare),
                        why: None,
                    },
                    // It answered, but not with a number this side can subtract from: `spare_cores`
                    // is `None` for a machine that did not say its cores or its load. Said as
                    // unknown rather than as a zero, because a zero is *full*.
                    None => Fact {
                        spare: None,
                        why: Some(
                            "the host answered without its cores or its load, so there is no spare \
                             to count"
                                .to_string(),
                        ),
                    },
                },
                Err(why) => Fact {
                    spare: None,
                    why: Some(why),
                },
            };
            let _ = tx.send((host.name.clone(), fact));
        });
    }
    drop(tx);
    let mut out = BTreeMap::new();
    while out.len() < hosts.len() {
        let left = total.saturating_sub(began.elapsed());
        if left.is_zero() {
            break;
        }
        match rx.recv_timeout(left) {
            Ok((name, fact)) => {
                out.insert(name, fact);
            }
            Err(_) => break,
        }
    }
    out
}

#[cfg(test)]
pub(crate) mod tests;
