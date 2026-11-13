//! Background jobs that outlive a daemon restart (`docs/lifecycle.md`, "Jobs across a restart").
//! A job writes its own `job-<id>.raw` under [`WRAPPER`], which leaves `job-<id>.status` at its end,
//! so the next daemon proves it by cgroup and stamp and reads on, or reads how it ended.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use super::jobs::{Job, JobState};

/// The `sh` script a background job runs under: `$1` is the status path, the rest the command.
/// The pipe through `cat` keeps the job running until every writer of its output is gone, as a
/// pipe to the daemon did, and the status is renamed into place so a reader never sees half of it.
pub const WRAPPER: &str = "s=$1; shift; { \"$@\"; echo $? > \"$s.tmp\"; } 2>&1 | cat; \
     r=$(cat \"$s.tmp\" 2>/dev/null); mv -f \"$s.tmp\" \"$s\" 2>/dev/null; exit \"${r:-127}\"";

/// `$0` of the wrapper, which is what `ps` shows for it.
pub const WRAPPER_NAME: &str = "letibot-job";

/// How long a reader with nothing to read sleeps, at first and at most. Short at first so a
/// command's first line is not late; capped so fifty quiet jobs do not spin.
const IDLE_FIRST: Duration = Duration::from_millis(5);
const IDLE_MOST: Duration = Duration::from_millis(100);

/// How long a detach waits for the reader to put down what it holds.
const DETACH_BOUND: Duration = Duration::from_secs(5);

/// A job's own output file and how far this daemon has read it.
#[derive(Debug)]
pub struct Follow {
    raw: PathBuf,
    status: PathBuf,
    read: AtomicU64,
    /// The wrapper has exited: read to the end once more, then stop.
    last: AtomicBool,
    /// Stop reading now, wherever the file is.
    stop: AtomicBool,
    /// Kept across a restart: the waiter must not settle it, and nothing may end its scope.
    detached: AtomicBool,
    done: Mutex<bool>,
    done_cv: Condvar,
}

impl Follow {
    pub fn new(raw: PathBuf, status: PathBuf, read: u64) -> Arc<Follow> {
        Arc::new(Follow {
            raw,
            status,
            read: AtomicU64::new(read),
            last: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            detached: AtomicBool::new(false),
            done: Mutex::new(false),
            done_cv: Condvar::new(),
        })
    }

    pub fn raw(&self) -> &Path {
        &self.raw
    }

    pub fn status(&self) -> &Path {
        &self.status
    }

    /// Raw bytes this daemon has taken into the job's capture.
    pub fn read(&self) -> u64 {
        self.read.load(Ordering::SeqCst)
    }

    pub fn is_detached(&self) -> bool {
        self.detached.load(Ordering::SeqCst)
    }

    /// The wrapper exited, so nothing writes the file any more.
    pub fn finish(&self) {
        self.last.store(true, Ordering::SeqCst);
    }

    /// Stop reading and hand the job to whoever opens the session next. Answers how far the file
    /// was read, after the capture has written out everything it held.
    pub fn detach(&self, job: &Job) -> u64 {
        self.detached.store(true, Ordering::SeqCst);
        self.stop.store(true, Ordering::SeqCst);
        let deadline = Instant::now() + DETACH_BOUND;
        let mut done = self.done.lock().unwrap_or_else(|e| e.into_inner());
        while !*done {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            done = self
                .done_cv
                .wait_timeout(done, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        drop(done);
        job.close_spill();
        self.read()
    }

    /// The files a settled job leaves behind: its output is in the masked spill by now.
    pub fn discard(&self) {
        let _ = std::fs::remove_file(&self.raw);
        let _ = std::fs::remove_file(&self.status);
    }

    fn mark_done(&self) {
        *self.done.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.done_cv.notify_all();
    }
}

/// Follow `f`'s file into `job`'s capture until the wrapper exits or a detach stops it.
pub fn spawn_reader(job: Arc<Job>, f: Arc<Follow>) -> Option<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name(format!("letibot-follow-{}", job.id))
        .spawn(move || {
            read_until_done(&job, &f);
            f.mark_done();
        })
        .ok()
}

fn read_until_done(job: &Job, f: &Follow) {
    let Ok(mut file) = std::fs::File::open(&f.raw) else {
        return;
    };
    if file.seek(SeekFrom::Start(f.read())).is_err() {
        return;
    }
    let mut buf = vec![0u8; 64 * 1024];
    let mut idle = IDLE_FIRST;
    loop {
        // Read before deciding: a `last` seen before the read guarantees the read reached the end.
        let last = f.last.load(Ordering::SeqCst);
        let mut got = false;
        loop {
            if f.stop.load(Ordering::SeqCst) {
                return;
            }
            match file.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    job.capture.lock().expect("capture").push(&buf[..n]);
                    f.read.fetch_add(n as u64, Ordering::SeqCst);
                    got = true;
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return,
            }
        }
        if last {
            return;
        }
        if got {
            idle = IDLE_FIRST;
        } else {
            std::thread::sleep(idle);
            idle = (idle * 2).min(IDLE_MOST);
        }
    }
}

/// The exit code a wrapper left, or `None` when there is no whole status file.
pub fn read_status(path: &Path) -> Option<i32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// `job-<id>.raw` and its two siblings, which is all a record has to keep.
pub fn raw_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("job-{id}.raw"))
}

pub fn status_path(raw: &Path) -> PathBuf {
    raw.with_extension("status")
}

pub fn out_path(raw: &Path) -> PathBuf {
    raw.with_extension("out")
}

/// One job a stopping daemon left running for the next one: what the `process` row keeps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detached {
    pub handle: String,
    pub command: String,
    pub slug: Option<String>,
    pub cwd: String,
    /// The stamp: the wrapper's pid and its `start:<n>`.
    pub pid: u32,
    pub identity: String,
    /// The owner scope's name, `session.<name>` or `explicit.<name>`.
    pub scope: String,
    /// The job's own cgroup directory, which proves it is running.
    pub cgroup: String,
    pub raw: String,
    /// Raw bytes already in the masked `.out` file.
    pub raw_read: u64,
}

impl Detached {
    /// The reaping scope's directory: the parent of the job's own.
    pub fn owner_dir(&self) -> String {
        Path::new(&self.cgroup)
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    }
}

/// What the kernel and the status file prove about a detached job. Nothing else is evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Proof {
    /// Its cgroup holds processes and the pid is still the one stamped.
    Running,
    /// It ended while no daemon watched, and its wrapper said how.
    Ended(i32),
    /// Neither: the sentence says what was looked at.
    Nothing(String),
}

/// Whether `pid` is still the process stamped `identity` (`start:<n>`).
pub fn stamp_matches(pid: u32, identity: &str) -> bool {
    super::procs::read(pid)
        .is_some_and(|p| identity == format!("start:{}", p.start) && p.state != 'Z')
}

/// Look at a detached job's cgroup, stamp and status file, and say what they prove.
pub fn prove(d: &Detached) -> Proof {
    let scope = super::scope::ScopeId {
        kind: super::scope::ScopeKind::Session,
        name: format!("job-{}", d.handle),
        path: PathBuf::from(&d.cgroup),
    };
    let populated = !super::scope::live_members(&scope).is_empty();
    let stamped = stamp_matches(d.pid, &d.identity);
    let status = read_status(&status_path(Path::new(&d.raw)));
    match (populated, stamped, status) {
        (true, true, _) => Proof::Running,
        // A process that let go of its output can outlive the wrapper, as it outlived the pipe.
        (_, _, Some(code)) => Proof::Ended(code),
        (true, false, None) => Proof::Nothing(format!(
            "its cgroup still holds processes, but pid {} is not the process stamped `{}`, so \
             nothing proves they are this job",
            d.pid, d.identity
        )),
        (false, _, None) => Proof::Nothing(
            "its cgroup is empty and its wrapper left no status file, so how it ended is not \
             known"
                .into(),
        ),
    }
}

/// How an adoption came out, for the restart notice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reattached {
    Running,
    Ended(JobState),
}
