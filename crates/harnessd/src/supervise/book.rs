//! **Where a fanout's and a scenario's processes are written down** — the daemon's half of
//! `letibot_tools::fleet::ProcessBook`.
//!
//! A branch of `fanout`, a role of `scenario` and their readiness checks are jobs of the *host's*
//! table, not of the session's own process host — the session is placed on one machine and a
//! fanout deliberately reaches the others. [`crate::jobwatch::JobRecorder`] writes the supervisor's
//! record for a job of *this* host, and nothing wrote one for a job of another, which would have
//! left two holes: a daemon that restarted mid-fanout would find no process to adopt and would
//! report nothing, and a session's close would kill nothing on the machines it reached.
//!
//! So the tool reports its own starts and endings through this, and the supervisor does the rest —
//! one liveness check per host per tick, the record's own deadline, and RH7's teardown that follows
//! ownership. **The store is the truth here as it is there**: nothing in this file remembers a pid
//! or a connection, so a daemon that starts an hour later has the same list.
//!
//! # Two rules borrowed from `JobRecorder`, because they are the same rules
//!
//! * **A process that cannot be identified is not written down at all.** A record whose identity is
//!   empty cannot be told from a stranger's pid, and the supervisor would signal the wrong process
//!   with it — so the write is skipped and the reason is said.
//! * **A write that fails is said, and kills nothing.** The processes are running either way; what
//!   is missing is the row, and a missing row with no sentence is a mystery.

use std::path::PathBuf;
use std::sync::Mutex;

use letibot_tokencore::store::{Check, ProcState, ProcessRecord, Store};
use letibot_tools::fleet::{ProcessBook, ProcessNote};

/// One session's process table, opened on first use.
pub struct StoreBook {
    path: PathBuf,
    session: String,
    /// **Opened lazily**, and kept: a session that never fans out pays nothing, and one that fans
    /// out twice pays one open. A second connection to a WAL store is ordinary here rather than a
    /// workaround — see `Store::path`.
    store: Mutex<Option<Store>>,
}

impl StoreBook {
    pub fn new(path: impl Into<PathBuf>, session: impl Into<String>) -> StoreBook {
        StoreBook {
            path: path.into(),
            session: session.into(),
            store: Mutex::new(None),
        }
    }

    /// Run `f` against the store, opening it once — and say when it could not be done.
    fn with(&self, handle: &str, what: &str, f: impl FnOnce(&Store) -> Result<(), String>) {
        let Ok(mut g) = self.store.lock() else {
            return;
        };
        if g.is_none() {
            match Store::open(&self.path) {
                Ok(s) => *g = Some(s),
                Err(e) => {
                    drop(g);
                    self.said(handle, what, &format!("the store did not open: {e}"));
                    return;
                }
            }
        }
        let out = match g.as_ref() {
            Some(store) => f(store),
            None => return,
        };
        drop(g);
        if let Err(why) = out {
            self.said(handle, what, &why);
        }
    }

    /// The sentence a failed write leaves, in the shape `JobRecorder` writes one.
    fn said(&self, handle: &str, what: &str, why: &str) {
        eprintln!(
            "  {} · process {handle}: {what} was not recorded: {why}",
            self.session
        );
    }
}

impl ProcessBook for StoreBook {
    fn started(&self, p: &ProcessNote) {
        // **An identity that is empty is a pid and nothing else**, so the record is not written —
        // see the module header. A job on this box whose `/proc` could not be read, or a host that
        // gave no marker, is that case.
        if p.identity.trim().is_empty() {
            self.said(
                &p.handle,
                "its process record",
                &format!(
                    "pid {} cannot be told apart from a reused pid, so the record would name a \
                     number rather than a process",
                    p.pid
                ),
            );
            return;
        }
        let mut rec = ProcessRecord::starting(&self.session, &p.handle, p.host.clone(), p.pid);
        // **`starting`, not `running`**: nobody has looked at this process yet, and the first tick
        // is what says it is there. `running` written by the starter would be a claim the starter
        // is not in a position to make — it knows the pid, not the process.
        rec.state = ProcState::Starting;
        rec.identity = p.identity.clone();
        rec.command = p.command.clone();
        rec.cwd = p.cwd.clone();
        rec.scope = if p.scope.trim().is_empty() {
            format!("session.{}", self.session)
        } else {
            p.scope.clone()
        };
        rec.started_ms = p.started_ms;
        rec.deadline_ms = p.deadline_ms;
        rec.log_path = p.log.clone();
        // **The pid is written as the group it leads, and the reader decides.** On a host, a job's
        // launch is `setsid`'d, so the pid really is its group; here, `LocalProbe` re-asks before a
        // signal (`supervise::signal`) and falls back to the pid alone when it does not lead one.
        rec.pgid = Some(p.pid);
        self.with(&p.handle, "its process record", |store| {
            store.put_process(&rec).map_err(|e| e.to_string())
        });
    }

    fn ended(&self, handle: &str, state: ProcState, exit: Option<i32>) {
        let mut check = Check::new(state, crate::supervise::now_ms(), 0, "fleet");
        if let Some(code) = exit {
            check = check.with_exit(code);
        }
        self.with(handle, "its ending", |store| {
            store
                .check_process(&self.session, handle, &check)
                .map_err(|e| e.to_string())
        });
    }
}
