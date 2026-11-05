//! Where a `host` reviewer runs: this machine, or a declared host named by `[queue]
//! reviewer_hosts` (opt-in, D18), chosen by spare cores. Facts come from a cache refreshed off the
//! queue's thread. [`ReviewHosts::keep`] frees a place against the desk's live children.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use letibot_tools::remote::{Host, Hosts, Paths};

use crate::mergequeue::capacity;

/// How long a snapshot of the pool is good for. Spare cores move slowly, and a down host is
/// caught by the placement itself.
pub const STALE_AFTER: Duration = Duration::from_secs(30);

/// The bound on one host's probe - under [`PROBE_TOTAL`] so a slow machine's own timeout fires
/// first and the report names *that machine* rather than the whole probe.
pub const HOST_BOUND: Duration = Duration::from_secs(8);

/// The bound on the whole refresh. A refresh that has not finished by then answers for the hosts it
/// heard from and leaves the rest as *not asked yet*, which is a wait, not a verdict.
pub const PROBE_TOTAL: Duration = Duration::from_secs(10);

/// **The name this machine answers to in a pool** - `[queue] reviewer_hosts = ["local"]`.
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
    /// `None` is this machine - the one the daemon is running on.
    pub name: Option<String>,
    /// `cores - load - reserve`, or `None` when the machine did not say. `None` is not zero: a
    /// machine nobody asked yet may take reviewers.
    pub spare: Option<f64>,
    /// Why it cannot take one, when it cannot: the probe's refusal, or *no spare cores*.
    pub why: Option<String>,
}

impl Machine {
    /// How many more reviewers this machine takes, given what this daemon already has there.
    /// A machine nobody has asked yet takes none, so no row is claimed for a host that may be off.
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

/// The machines a `host` reviewer may be placed on, what each last said, and what this daemon
/// has running where. One per daemon: the per-host cap counts all its work.
#[derive(Debug, Default)]
pub struct ReviewHosts {
    inner: Mutex<Inner>,
    /// A refresh is in flight - one at a time, so a queue that looks twice a second does not spawn
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
    /// The reviewers this daemon has placed, by child handle. Pruned by [`ReviewHosts::keep`].
    placed: BTreeMap<String, String>,
    /// **The machines that refused a placement in THIS pass** - skipped until the pass ends, so
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

    /// The machines this project's reviewers may go to. `pool` is `reviewer_hosts`: `None` is
    /// every declared host, a list is exactly those. Forbidden and undeclared names are dropped, and
    /// [`ReviewHosts::unknown`] reports them.
    pub fn machines(&self, pool: Option<&[String]>) -> Vec<Machine> {
        let inner = self.lock();
        self.machines_locked(&inner, pool)
    }

    /// **The names a project asked for that nobody declared** - the sentence a pass says, rather
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

    /// **How many reviewers the pool can take this pass** - the sum of what each machine has room
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

    /// Which machine the next reviewer goes to, or `None` when the pool is full this pass. Most
    /// spare cores wins and a tie stays on this machine, which costs no transport. This machine
    /// alone is never gated on spare cores.
    pub fn choose(&self, pool: Option<&[String]>) -> Option<Machine> {
        let inner = self.lock();
        // This machine alone is never gated on spare cores, as before the pool existed.
        if let [only] = self.machines_locked(&inner, pool).as_slice() {
            return Some(only.clone());
        }
        let mut best: Option<Machine> = None;
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
            let spare = m.spare.unwrap_or(0.0);
            let better = match &best {
                None => true,
                // **More spare wins; equal spare is this machine's.** `>` and not `>=` on the spare
                // alone, because the machines are walked local-first and a later machine with the
                // same spare must not displace it.
                Some(b) => spare > b.spare.unwrap_or(0.0),
            };
            if better {
                best = Some(m);
            }
        }
        best
    }

    /// This machine, if it has room - where a refused placement falls back. `None` is a wait.
    pub fn choose_local(&self) -> Option<Machine> {
        let inner = self.lock();
        let me = self.machines_locked(&inner, Some(&[])).into_iter().next()?;
        (me.room(taken(&inner, None)) > 0).then_some(me)
    }

    /// **Record that a reviewer went to a machine** - the per-host cap counts this daemon's own
    /// work, and [`ReviewHosts::keep`] frees it when the reviewer is over.
    pub fn started(&self, child: &str, machine: &Machine) {
        let mut inner = self.lock();
        inner
            .placed
            .insert(child.to_string(), machine.name.clone().unwrap_or_default());
    }

    /// Frees the place of every reviewer no longer in `live`, the desk's own list of children.
    /// One reconciliation instead of a call at each ending, so none can be missed. A restart
    /// forgets `placed`, which over-admits until the old reviewers end.
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

    /// **This machine is having a bad day** - it refused a placement in this pass, so nothing else
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

    /// **The pass is over** - a machine that failed a minute ago is tried again next time, because
    /// a reboot is minutes and a queue that never retried would run here for ever.
    pub fn end_pass(&self) {
        self.lock().down.clear();
    }

    /// Ask every machine how it is, off the caller's thread. A no-op while a refresh is in
    /// flight or the snapshot is younger than [`STALE_AFTER`].
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

    /// **Block until a refresh is done** - for a test, and for nothing in the queue.
    pub fn wait(&self, handle: Option<std::thread::JoinHandle<()>>) {
        if let Some(h) = handle {
            let _ = h.join();
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Install a snapshot without a network, for tests of what the pool does with an answer.
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

/// This machine's cores, with nothing subtracted: the daemon's box is the operator's own, and
/// its load is mostly the queue's. Floored at one reviewer's worth so a one-core box still runs
/// a reviewer.
pub fn local_spare() -> Option<f64> {
    let cores = std::thread::available_parallelism().ok()?.get() as f64;
    Some(cores.max(capacity::CORES_PER_REVIEWER))
}

/// **Ask every host at once**, each bounded, and the whole thing bounded - the shape
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
