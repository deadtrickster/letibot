//! A seat's OWN background jobs: the command's process, its log, and the handle a row's
//! `wait_on {"job": …}` takes. A seat is not a session, so there is no daemon process host to run
//! it (`docs/seat-todo.md`); the bridge runs it, keeps the row in the shared store under the seat's
//! handle, and reports its ending on the seat's channel.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use letibot_tokencore::store::SeatJob;
use letibot_tools::exec::jobs::JobId;
use letibot_tools::exec::procs;
use serde_json::{Value, json};

use crate::push::Outbox;

/// How often the reaper looks. Not a budget: a job's ending is noticed within this, and its row
/// and its channel frame move with it.
const PASS: Duration = Duration::from_millis(250);

/// One job this process is running, and the row the store was written with.
struct Live {
    record: SeatJob,
    child: Mutex<Child>,
    started: Instant,
    log: PathBuf,
}

/// A seat's jobs, by handle. One per bridge process, shared with the seat and its watcher.
pub struct SeatJobs {
    /// The handle the store keys them under: `cc@<project>`.
    seat: String,
    store: PathBuf,
    /// Where the logs go - the bridge's rundir, one directory per seat.
    dir: PathBuf,
    live: Mutex<BTreeMap<String, Arc<Live>>>,
    /// The seat's channel, once a client has attached it: a job's ending is pushed there, and
    /// recorded before it is sent so a reconnect is told what it missed.
    outbox: Mutex<Option<Arc<Outbox>>>,
}

impl SeatJobs {
    /// The seat's jobs, with the rows a dead bridge left `running` settled by the process table
    /// rather than by a guess.
    pub fn new(
        seat: impl Into<String>,
        store: impl Into<PathBuf>,
        dir: impl Into<PathBuf>,
    ) -> Arc<SeatJobs> {
        let jobs = Arc::new(SeatJobs {
            seat: seat.into(),
            store: store.into(),
            dir: dir.into(),
            live: Mutex::new(BTreeMap::new()),
            outbox: Mutex::new(None),
        });
        jobs.bury_the_lost();
        jobs
    }

    /// Start the reaper: one thread per process, which ends with the `SeatJobs` it watches.
    pub fn reap_in_background(jobs: &Arc<SeatJobs>) {
        let weak: Weak<SeatJobs> = Arc::downgrade(jobs);
        let _ = std::thread::Builder::new()
            .name("leticode-seat-jobs".into())
            .spawn(move || {
                while let Some(jobs) = weak.upgrade() {
                    jobs.reap();
                    drop(jobs);
                    std::thread::sleep(PASS);
                }
            });
    }

    /// Point the job endings at the seat's channel.
    pub fn attach(&self, outbox: Arc<Outbox>) {
        *self.outbox.lock().unwrap_or_else(|e| e.into_inner()) = Some(outbox);
    }

    /// Every job this seat has recorded, oldest first - what `seat_jobs` answers with.
    pub fn listing(&self) -> Vec<SeatJob> {
        // A bridge with no store yet has no jobs: the file is the daemon's, and there is
        // nothing for one to be written to until it exists.
        if !crate::store::exists(&self.store) {
            return Vec::new();
        }
        match crate::store::open(&self.store, "seat_job", None) {
            Ok(store) => store.seat_jobs(&self.seat).unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }

    /// The jobs still running: what `wait_on {"job": …}` resolves against, and what keeps a row
    /// that waits on one quiet. A job THIS process holds is read from its own child handle, because
    /// the process table's answer needs a start time it may disagree about - and a running job read
    /// as ended is a row asked about work that is still going. The table is the witness only for a
    /// row a dead bridge left `running`.
    pub fn open(&self) -> Vec<SeatJob> {
        let live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<SeatJob> = live
            .values()
            .filter(|job| {
                matches!(
                    job.child
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .try_wait(),
                    Ok(None)
                )
            })
            .map(|job| job.record.clone())
            .collect();
        out.extend(self.listing().into_iter().filter(|j| {
            j.state == "running" && !live.contains_key(&j.handle) && procs::alive(j.pid, j.start)
        }));
        out.sort_by(|a, b| a.handle.cmp(&b.handle));
        out
    }

    /// Run one command as this seat's job: `argv` verbatim, no shell, output to a file under the
    /// rundir. Answers the handle, the pid and where the output went.
    pub fn start(
        &self,
        argv: &[Value],
        cwd: Option<&str>,
        name: Option<&str>,
    ) -> Result<Value, String> {
        let argv: Vec<String> = argv
            .iter()
            .map(|a| a.as_str().map(str::to_string))
            .collect::<Option<Vec<String>>>()
            .ok_or_else(|| {
                "`argv` is a list of strings: the command and its arguments".to_string()
            })?;
        let Some((program, args)) = argv.split_first() else {
            return Err("`argv` is empty: name the command to run".into());
        };
        let cwd = cwd.map(str::to_string).unwrap_or_else(|| {
            std::env::current_dir()
                .map(|d| d.display().to_string())
                .unwrap_or_else(|_| ".".into())
        });
        if !Path::new(&cwd).is_dir() {
            return Err(format!("`cwd` `{cwd}` is not a directory on this host"));
        }
        std::fs::create_dir_all(&self.dir).map_err(|e| {
            format!(
                "the seat's job directory {} cannot be made: {e}",
                self.dir.display()
            )
        })?;
        let handle = JobId::next().to_string();
        let log = self.dir.join(format!("{handle}.log"));
        let out = std::fs::File::create(&log)
            .map_err(|e| format!("the log {} cannot be made: {e}", log.display()))?;
        let err = out.try_clone().map_err(|e| {
            format!(
                "the log {} cannot be shared with stderr: {e}",
                log.display()
            )
        })?;
        let child = Command::new(program)
            .args(args)
            .current_dir(&cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(err))
            .spawn()
            .map_err(|e| format!("`{program}` did not start: {e}"))?;
        let pid = child.id();
        // The start time is the half of a pid that survives reuse: without it a wait on this job
        // would settle a stranger's process after the number came round again.
        let start = procs::read(pid).map(|p| p.start).unwrap_or(0);
        let record = SeatJob {
            handle: handle.clone(),
            command: argv.join(" "),
            cwd: cwd.clone(),
            name: name.unwrap_or("").trim().to_string(),
            pid,
            start,
            log: log.display().to_string(),
            state: "running".into(),
            started_ms: now_ms(),
            ended_ms: 0,
            elapsed_ms: 0,
            produced: 0,
        };
        self.record(&record);
        let live = Live {
            record: record.clone(),
            child: Mutex::new(child),
            started: Instant::now(),
            log,
        };
        self.live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(handle.clone(), Arc::new(live));
        let log = record.log.clone();
        Ok(json!({
            "job": handle,
            "pid": pid,
            "start": start,
            "cwd": cwd,
            "log": log.clone(),
            "state": "running",
            "said": format!("job `{handle}` started on pid {pid}; its output goes to {log}."),
        }))
    }

    /// End one job of this seat's. The killing is now; its ending - the row and the channel frame -
    /// is the reaper's, so one place writes a settlement.
    pub fn kill(&self, handle: &str) -> Result<Value, String> {
        let live = self
            .live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(handle)
            .cloned();
        let (pid, start) = match live {
            Some(live) => (live.record.pid, live.record.start),
            // A job this process does not hold: one a dead bridge started, and the process table
            // still knows the pair.
            None => {
                let row = self
                    .listing()
                    .into_iter()
                    .find(|j| j.handle == handle && j.state == "running");
                let Some(row) = row else {
                    return Err(format!("no running job of this seat is `{handle}`"));
                };
                (row.pid, row.start)
            }
        };
        procs::kill_exact(pid, start, procs::Signal::Kill).map_err(|why| match why {
            procs::KillRefused::Gone => format!("`{handle}` had already ended"),
            procs::KillRefused::Self_ => {
                "that is this bridge or one of its own parents".to_string()
            }
            procs::KillRefused::Os(why) => why,
        })?;
        Ok(json!({
            "job": handle,
            "pid": pid,
            "state": "killing",
            "said": format!(
                "killed `{handle}` (pid {pid}); its end follows on the seat's channel."
            ),
        }))
    }

    /// Settle every job whose process has ended: the store row first, then the channel frame.
    fn reap(&self) {
        let live: Vec<(String, Arc<Live>)> = self
            .live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(h, l)| (h.clone(), l.clone()))
            .collect();
        for (handle, job) in live {
            let status = job
                .child
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .try_wait();
            let Ok(Some(status)) = status else { continue };
            let state = match (status.code(), status.success()) {
                (Some(0), _) => "exited 0".to_string(),
                (Some(code), _) => format!("exited {code}"),
                (None, _) => "killed by a signal".to_string(),
            };
            self.live
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&handle);
            self.settle(&job, state);
        }
    }

    /// One job's ending, written once: the store row, then the frame - and only when a channel has
    /// attached, because a record nobody can take is a file nobody reads.
    fn settle(&self, job: &Live, state: String) {
        let mut record = job.record.clone();
        record.state = state;
        record.ended_ms = now_ms();
        record.elapsed_ms = job.started.elapsed().as_millis() as u64;
        record.produced = std::fs::metadata(&job.log).map(|m| m.len()).unwrap_or(0);
        self.record(&record);
        let outbox = self
            .outbox
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(outbox) = outbox {
            let (key, note) = crate::push::seat_job_event(&self.seat, &record);
            outbox.record(key, note);
        }
    }

    /// A `running` row left by a bridge that died is settled by the process table: the job is over
    /// if the pid and its start time are not there any more.
    fn bury_the_lost(&self) {
        for mut row in self.listing() {
            if row.state != "running" || procs::alive(row.pid, row.start) {
                continue;
            }
            row.state = "lost".into();
            row.ended_ms = now_ms();
            self.record(&row);
        }
    }

    fn record(&self, job: &SeatJob) {
        // The store is the daemon's file, so a seat that runs before one exists keeps its job in
        // this process rather than making a store of its own beside it.
        let opened = match crate::store::exists(&self.store) {
            true => crate::store::open(&self.store, "seat_job", None),
            false => Err("no store yet".to_string()),
        };
        let Ok(store) = opened else {
            eprintln!(
                "seat jobs: there is no store at {} yet, so `{}` lives in this process only",
                self.store.display(),
                job.handle
            );
            return;
        };
        if let Err(e) = store.put_seat_job(&self.seat, job) {
            eprintln!(
                "seat jobs: `{}` could not be written to the store ({e}); it lives in this process \
                 only",
                job.handle
            );
        }
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_tokencore::store::Store;

    /// A job this process holds is running whatever its store row says about its start time. The
    /// table's answer needs that time to match, and a job read as ended puts its row back on the
    /// check while the work is still going - the handle is the fact here.
    #[test]
    fn a_job_this_process_holds_is_open_even_when_the_store_row_disagrees_about_its_start() {
        let dir = std::env::temp_dir().join(format!("seat-jobs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch dir");
        let store = dir.join("sessions.db");
        Store::open(&store).expect("the store the daemon's first start makes");
        let jobs = SeatJobs::new("cc@test", &store, dir.join("jobs"));
        let started = jobs
            .start(
                &[serde_json::json!("sleep"), serde_json::json!("60")],
                None,
                None,
            )
            .expect("a job of the seat's own");
        let handle = started["job"].as_str().expect("a handle").to_string();
        assert!(
            jobs.open().iter().any(|j| j.handle == handle),
            "a job this process started is open"
        );

        // The row a bridge that died leaves behind, with a start time this process disagrees with -
        // which is the shape that read a running job as one that had ended.
        let mut row = jobs
            .listing()
            .into_iter()
            .find(|j| j.handle == handle)
            .expect("the row is on disk");
        row.start = row.start.wrapping_add(1);
        Store::open(&store)
            .expect("the store")
            .put_seat_job("cc@test", &row)
            .expect("the row is written as a dead bridge left it");
        assert!(
            jobs.open().iter().any(|j| j.handle == handle),
            "its own handle is the fact: {:?}",
            jobs.open()
        );

        jobs.kill(&handle).expect("killed");
        // Reap the ending here: no reaper thread runs in this test, and a zombie is not a job.
        let _ = jobs.open();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
