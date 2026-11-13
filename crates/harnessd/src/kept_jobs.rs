//! Host jobs kept across `letibot --restart`: the rows a stopping daemon writes for each, and how
//! the daemon that opens the session next takes them back (`docs/lifecycle.md`, "Jobs across a
//! restart"). The proof is the job's cgroup, its stamp and its status file, never the row.

use letibot_tokencore::store::{
    Check, HoldingClass, HoldingRow, HoldingState, JobContext, JobRecord, ProcState, ProcessRecord,
    Store,
};
use letibot_tools::exec::ProcessHost;
use letibot_tools::exec::detach::{Detached, Reattached};

/// The job row's state while no daemon watches it.
pub const DETACHED: &str = "detached";

/// The job row's state when nothing proves what became of it.
pub const LOST_NO_PROOF: &str = "lost: no proof";

/// Write each kept job down: its process row `detached` with the cgroup and the stamp, and its
/// job row `detached`. Answers the sentences of the writes that failed.
pub fn record_detached(store: &Store, session: &str, kept: &[Detached]) -> Vec<String> {
    let now = letibot_metrics::now_ms() as i64;
    let mut failed = Vec::new();
    for d in kept {
        let mut rec = store
            .process(session, &d.handle)
            .ok()
            .flatten()
            .unwrap_or_else(|| ProcessRecord::starting(session, &d.handle, None, d.pid));
        rec.pid = d.pid;
        rec.pgid = Some(d.pid);
        rec.identity = d.identity.clone();
        rec.command = d.command.clone();
        rec.cwd = d.cwd.clone();
        rec.scope = d.scope.clone();
        rec.state = ProcState::Detached;
        rec.log_path = Some(d.raw.clone());
        rec.log_read = d.raw_read;
        rec.cgroup = Some(d.cgroup.clone());
        rec.check_ms = Some(now);
        rec.check_how = Some("restart".into());
        if let Err(e) = store.put_process(&rec) {
            failed.push(format!("{}: its process row: {e}", d.handle));
        }
        let mut job = job_row(store, session, d);
        job.state = DETACHED.into();
        if let Err(e) = store.put_job(session, &job) {
            failed.push(format!("{}: its job row: {e}", d.handle));
        }
        // The box-wide index `letibot holds` and the reaper read: the process row is per project.
        if let Err(e) = store.put_holding(&holding(session, d, HoldingState::Held, now)) {
            failed.push(format!("{}: its holding: {e}", d.handle));
        }
    }
    failed
}

fn holding(session: &str, d: &Detached, state: HoldingState, now: i64) -> HoldingRow {
    let mut row = HoldingRow::new(
        session,
        letibot_tools::lifecycle::DETACHED_JOB,
        HoldingClass::Compute,
        &d.handle,
        format!("pid {} {} in {}", d.pid, d.identity, d.cgroup),
        now,
    );
    row.state = state;
    row.why = Some(format!("detached across a restart: `{}`", d.command));
    row
}

fn job_row(store: &Store, session: &str, d: &Detached) -> JobRecord {
    store
        .jobs(session)
        .unwrap_or_default()
        .into_iter()
        .find(|j| j.handle == d.handle)
        .unwrap_or_else(|| JobRecord {
            handle: d.handle.clone(),
            command: d.command.clone(),
            how: String::new(),
            state: DETACHED.into(),
            produced: 0,
            elapsed_ms: 0,
            redirect: None,
        })
}

/// What a restarting daemon's run ender does with the jobs its host kept: write them down, and
/// say each on the log the launcher relays. A job nothing could write down is said too.
pub fn keep_for_the_next_daemon(
    at: Option<&crate::config::StoreAt>,
    session: &str,
    kept: &[Detached],
) {
    if kept.is_empty() {
        return;
    }
    let failed = match at.map(|a| a.open_existing()) {
        Some(Ok(store)) => record_detached(&store, session, kept),
        Some(Err(e)) => vec![format!("the store did not open: {e}")],
        None => vec!["this session has no store".to_string()],
    };
    for d in kept {
        eprintln!(
            "  stopping: {session} keeps job `{}` (pid {}) running for the next daemon",
            d.handle, d.pid
        );
    }
    for why in failed {
        eprintln!(
            "  stopping: {session}: a kept job was not written down, so the next daemon will not \
             find it: {why}"
        );
    }
}

/// A restart of a VM-placed session: its jobs cannot outlive the VM, so they end as a plain stop
/// ends a host's, and the answer is the one sentence that says so. `None` when nothing ran.
pub fn end_in_vm(host: &dyn ProcessHost) -> Option<String> {
    let ended = host.end_running("the daemon stopping");
    (!ended.is_empty()).then(|| vm_not_kept(ended.len()))
}

/// The one sentence a restart of a VM-placed session says about its jobs.
pub fn vm_not_kept(n: usize) -> String {
    format!(
        "runs in a VM, so its {n} running job(s) cannot be kept across the restart: they were \
         ended with the daemon, as a plain stop ends them"
    )
}

/// A `detached` row as the host takes it back. `None` for a row a build before this one wrote.
pub fn detached_of(rec: &ProcessRecord, slug: Option<String>) -> Option<Detached> {
    Some(Detached {
        handle: rec.handle.clone(),
        command: rec.command.clone(),
        slug,
        cwd: rec.cwd.clone(),
        pid: rec.pid,
        identity: rec.identity.clone(),
        scope: rec.scope.clone(),
        cgroup: rec.cgroup.clone()?,
        raw: rec.log_path.clone()?,
        raw_read: rec.log_read,
    })
}

/// What became of one kept job at this open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fate {
    Reattached,
    /// It ended while detached; its ending is delivered as a settlement.
    Ended,
    Lost,
}

/// One kept job as the restart notice says it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Taken {
    pub handle: String,
    pub fate: Fate,
    pub line: String,
}

/// Take back every job of `session` a restart kept. Answers what each came to and the handles a
/// job watcher should follow: a running one, or one whose ending is owed to the model.
pub fn take_back(
    procs: &dyn ProcessHost,
    store: &Store,
    session: &str,
    instance: &str,
) -> (Vec<Taken>, Vec<String>) {
    let mut taken = Vec::new();
    let mut watch = Vec::new();
    for rec in store.detached_processes_of(session).unwrap_or_default() {
        let ctx = store.job_context(session, &rec.handle).ok().flatten();
        let label = label(&rec.handle, ctx.as_ref().and_then(|c| c.slug.as_deref()));
        let detached = detached_of(&rec, ctx.as_ref().and_then(|c| c.slug.clone()));
        let outcome = match &detached {
            Some(d) => procs.reattach(d).map(|(_, r)| r),
            None => Err("its row names no cgroup or output file to look at".to_string()),
        };
        // Detached no longer, whatever it came to: running here, ended, or said lost.
        if let Some(d) = &detached {
            let now = letibot_metrics::now_ms() as i64;
            let _ = store.put_holding(&holding(session, d, HoldingState::Gone, now));
        }
        // This daemon owns the job now: the restart pass must not remind of it or say its
        // ending a second time.
        let own = |store: &Store| {
            let ctx = ctx.clone().unwrap_or(JobContext {
                handle: rec.handle.clone(),
                slug: None,
                cwd: rec.cwd.clone(),
                instance: String::new(),
                started_ms: rec.started_ms.max(0) as u64,
            });
            let _ = store.put_job_context(
                session,
                &JobContext {
                    instance: instance.to_string(),
                    ..ctx
                },
            );
        };
        let mut job = store
            .jobs(session)
            .unwrap_or_default()
            .into_iter()
            .find(|j| j.handle == rec.handle)
            .unwrap_or_else(|| JobRecord {
                handle: rec.handle.clone(),
                command: rec.command.clone(),
                how: String::new(),
                state: String::new(),
                produced: 0,
                elapsed_ms: 0,
                redirect: None,
            });
        let now = letibot_metrics::now_ms() as i64;
        match outcome {
            Ok(Reattached::Running) => {
                own(store);
                let _ = store.check_process(
                    session,
                    &rec.handle,
                    &Check::new(ProcState::Running, now, 0, "reattach"),
                );
                job.state = "running".into();
                let _ = store.put_job(session, &job);
                watch.push(rec.handle.clone());
                taken.push(Taken {
                    handle: rec.handle.clone(),
                    fate: Fate::Reattached,
                    line: reattached_line(&label, &rec),
                });
            }
            Ok(Reattached::Ended(state)) => {
                own(store);
                job.state = state.word();
                let _ = store.put_job(session, &job);
                watch.push(rec.handle.clone());
                taken.push(Taken {
                    handle: rec.handle.clone(),
                    fate: Fate::Ended,
                    line: ended_line(&label, &rec, &state.word()),
                });
            }
            Err(why) => {
                own(store);
                let _ = store.check_process(
                    session,
                    &rec.handle,
                    &Check::new(ProcState::Dead, now, 0, "no proof"),
                );
                job.state = LOST_NO_PROOF.into();
                let _ = store.put_job(session, &job);
                // Said in the notice, so it is not an ending the next open announces again.
                let _ = store.deliver_job(session, &rec.handle);
                taken.push(Taken {
                    handle: rec.handle.clone(),
                    fate: Fate::Lost,
                    line: lost_line(&label, &rec, &why),
                });
            }
        }
    }
    (taken, watch)
}

fn label(handle: &str, slug: Option<&str>) -> String {
    letibot_tools::exec::job_label(slug, handle)
}

/// `- job `fuzz` (`./fuzz.sh`): still running, reattached ...`.
pub fn reattached_line(label: &str, rec: &ProcessRecord) -> String {
    let out = rec
        .log_path
        .as_deref()
        .map(|raw| {
            letibot_tools::exec::detach::out_path(std::path::Path::new(raw))
                .display()
                .to_string()
        })
        .unwrap_or_default();
    format!(
        "- job `{label}` (`{}`): still running, reattached. Its output goes on in `{out}`, and \
         its ending will be delivered as usual.",
        rec.command
    )
}

/// `- job `fuzz` (`./fuzz.sh`): ended while detached ...`.
pub fn ended_line(label: &str, rec: &ProcessRecord, word: &str) -> String {
    format!(
        "- job `{label}` (`{}`): ended while no daemon watched it ({word}); its result is \
         delivered on its own.",
        rec.command
    )
}

/// `- job `fuzz` (`./fuzz.sh`): lost: no proof ...`.
pub fn lost_line(label: &str, rec: &ProcessRecord, why: &str) -> String {
    format!(
        "- job `{label}` (`{}`): {LOST_NO_PROOF} - {why}. It was NOT reattached, and nothing \
         says whether it finished.",
        rec.command
    )
}

/// The lines a restart notice gives the jobs kept across it, `None` when it owes them none.
pub fn block(taken: &[Taken]) -> Option<String> {
    let lines: Vec<&str> = taken.iter().map(|t| t.line.as_str()).collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}
