//! One running command: its state, its captured output, and the denominator that
//! travels with every slice of it.
//!
//! # Every `bash` call is a job, including the ones that were waited on
//!
//! There is no second code path for a foreground command. A foreground `bash` is a
//! job the tool waited for, and that has two consequences worth having:
//!
//! 1. **The recovery path always exists.** `bash`'s inline result is capped; the
//!    rest is not "lost to truncation" or parked in a spill store, it is in the
//!    job's capture and `job_output` reads it. `docs/tool-survey.md` §3.5's worst
//!    finding — dsh's persistent shell clipping at 16 000 chars with *"a note that
//!    assumes the output was a file, with no path to the rest"* — is unreachable
//!    from here.
//! 2. **A timeout does not have to kill anything.** A command that outran its
//!    deadline is a job that is still running, and saying so is a better answer
//!    than either killing it or blocking the turn.
//!
//! # The capture keeps the tail, and says what it dropped
//!
//! pi's rule, and it is right: `read` keeps the head of a file, shell output keeps
//! the tail. A build that fails prints the error last.
//!
//! What is not optional is the denominator. [`OutputSlice`] carries the absolute
//! byte range, the total **produced** (not the total retained), and the count
//! dropped off the front. `0 bytes` and `0 of 0 bytes` are different facts and
//! `docs/tool-design-brief.md` §2.2 says the second one is the only one that is a
//! measurement.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, SystemTime};

/// A job's name, as the model spells it back.
///
/// A short opaque string rather than a number, because a number invites the model
/// to guess the next one and a guessed job id would be a call about somebody
/// else's process.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct JobId(pub String);

impl JobId {
    pub fn next() -> JobId {
        static N: AtomicU64 = AtomicU64::new(1);
        JobId(format!("j{}", N.fetch_add(1, Ordering::Relaxed)))
    }
}

impl std::fmt::Display for JobId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where a job is. Five states, and the last three are **not** interchangeable:
/// a command that exited non-zero, one that was killed with its scope, and one
/// that never joined its cgroup are three different things to have happened, and
/// reporting them as one "failed" is F5 — *a component's "I did not do this"
/// reported upward as success*, inverted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobState {
    Running,
    /// The process exited on its own. `code` may be non-zero; that is the
    /// command's answer, not an error in the harness.
    Exited {
        code: i32,
    },
    /// A signal ended it, and the signal is named.
    Signalled {
        signal: i32,
    },
    /// The scope reaped it, or `job_kill` did. **Never** reported as an exit code.
    Killed {
        by: String,
    },
    /// The wrapper could not put the process in its cgroup, so the command was
    /// never run. See [`super::scope::join_script`].
    NotScoped,
}

impl JobState {
    pub fn is_running(&self) -> bool {
        matches!(self, JobState::Running)
    }

    /// The word a listing shows. Deliberately not "ok"/"error": the classification
    /// a caller needs is *what happened to the process*, and success is a property
    /// of the command's own exit code.
    pub fn word(&self) -> String {
        match self {
            JobState::Running => "running".into(),
            JobState::Exited { code } => format!("exited {code}"),
            JobState::Signalled { signal } => format!("signalled {signal}"),
            JobState::Killed { by } => format!("killed by {by}"),
            JobState::NotScoped => "not run (could not join its scope)".into(),
        }
    }

    /// **Whether nothing was ever executed for this job**, as opposed to a process
    /// that ran and wrote nothing.
    ///
    /// The companion to [`JobState::word`], and it exists because the word alone cannot
    /// answer the question every reader of an empty job is asking: *is this command's
    /// output empty, or is there no command?* `NotScoped` is the second — the wrapper
    /// could not put the process in its cgroup, so nothing started — and an empty output
    /// window looks identical either way.
    ///
    /// **The operator's own rule, inverted.** R17: *a row with no output must not look
    /// like a row whose output is empty.* Both heads drew
    ///
    /// ```text
    /// not run (could not join its scope)      ← the header
    /// it wrote nothing at all.                ← and it never started
    /// ```
    ///
    /// because the sentence was chosen by `lines.is_empty()` and nothing on the wire
    /// separated the two — the window's emptiness is the same fact in both. So the fact
    /// travels: `never_ran` is the daemon's answer, it rides the job's row and the job's
    /// window, and a head chooses its line by it rather than by emptiness alone.
    ///
    /// **A bool and not a sentence** because the fact is a bool and the two readers
    /// differ: one draws a line under a state word, the other suppresses a duration it
    /// has no right to claim. The words are A's ruling, written down once on each side
    /// and paired by the tests on both sides of the wire (§11.6).
    pub fn never_ran(&self) -> bool {
        matches!(self, JobState::NotScoped)
    }
}

/// A ring over a job's output, plus the two numbers that make a slice of it a
/// measurement rather than a quantity.
#[derive(Debug)]
pub struct Capture {
    buf: VecDeque<u8>,
    cap: usize,
    /// Bytes discarded off the front to stay under `cap`.
    dropped: u64,
    /// Every byte the process ever wrote. **The denominator.**
    produced: u64,
    /// When the last byte arrived.
    ///
    /// This is the number a wait reports, and it is deliberately *not* "the
    /// process is alive". `liveness-indicators-measure-the-wrong-thing`: a
    /// publication timestamp says a dead job is live and a progress timestamp says
    /// a finished job is broken, and only the second one is about work.
    last_at: Option<SystemTime>,
}

impl Capture {
    pub fn new(cap: usize) -> Capture {
        Capture {
            buf: VecDeque::new(),
            cap,
            dropped: 0,
            produced: 0,
            last_at: None,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.produced += bytes.len() as u64;
        self.last_at = Some(SystemTime::now());
        self.buf.extend(bytes.iter().copied());
        while self.buf.len() > self.cap {
            let over = self.buf.len() - self.cap;
            self.buf.drain(..over);
            self.dropped += over as u64;
        }
    }

    pub fn produced(&self) -> u64 {
        self.produced
    }

    /// How long since the last byte, and `None` when there has never been one —
    /// which is a different fact from "a long time ago" and is reported as one.
    pub fn since_last(&self) -> Option<Duration> {
        self.last_at.map(|t| t.elapsed().unwrap_or(Duration::ZERO))
    }

    /// A slice by **absolute** byte offset — absolute so that an offset stays
    /// meaningful after the ring has moved under it, which is the whole difference
    /// between a paginator that works on a long-running job and one that silently
    /// re-reads the same window.
    pub fn slice(&self, from: u64, limit: usize) -> OutputSlice {
        let start = from.max(self.dropped);
        let idx = (start - self.dropped) as usize;
        let take = limit.min(self.buf.len().saturating_sub(idx));
        let bytes: Vec<u8> = self.buf.iter().skip(idx).take(take).copied().collect();
        OutputSlice {
            from: start,
            to: start + bytes.len() as u64,
            bytes,
            produced: self.produced,
            dropped: self.dropped,
            retained: self.buf.len() as u64,
        }
    }

    /// The last `limit` bytes, which is what a finished command's result wants.
    pub fn tail(&self, limit: usize) -> OutputSlice {
        let take = limit.min(self.buf.len());
        let from = self.dropped + (self.buf.len() - take) as u64;
        self.slice(from, take)
    }
}

/// A window onto a job's output, and everything needed to know what it is a window
/// **onto**.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputSlice {
    pub bytes: Vec<u8>,
    /// Absolute byte offset of the first byte returned.
    pub from: u64,
    /// One past the last.
    pub to: u64,
    /// Total bytes the process has written, ever.
    pub produced: u64,
    /// Bytes that fell off the front of the ring and are not recoverable.
    pub dropped: u64,
    /// Bytes currently held.
    pub retained: u64,
}

impl OutputSlice {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }

    /// The denominator, as one sentence, with every case it can be in.
    ///
    /// `docs/tool-design-brief.md` §2.2: *a count does not travel without its
    /// denominator*, and a zero denominator is a failed scope, never an answer.
    /// So a job that has written nothing says exactly that, and it does not look
    /// like a job whose output was dropped.
    pub fn denominator(&self, job: &JobId) -> String {
        if self.produced == 0 {
            return format!("`{job}` has produced 0 bytes of output so far.");
        }
        let mut s = format!(
            "bytes {}–{} of {} produced by `{job}`",
            self.from, self.to, self.produced
        );
        if self.dropped > 0 {
            s.push_str(&format!(
                "; the first {} bytes are NO LONGER RETAINED — capture keeps the last {} \
                 bytes, and this job outran that",
                self.dropped, self.retained
            ));
        }
        if self.to < self.produced {
            s.push_str(&format!(
                "; call `job_output` with job=\"{job}\" offset={} for the next window",
                self.to
            ));
        }
        s.push('.');
        s
    }

    /// Whether every byte the process wrote is inside this slice.
    pub fn complete(&self) -> bool {
        self.dropped == 0 && self.from == 0 && self.to == self.produced
    }
}

/// Where a job's processes live and what reaps them. Changed by a promotion and
/// by nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lifetime {
    /// The job's own cgroup.
    pub scope: super::scope::ScopeId,
    /// The scope that reaps it if nobody touches it.
    pub owner: super::scope::ScopeId,
    /// How it came to be in the background, once it is. `None` while it is a
    /// plain foreground command — which is a different fact from
    /// `Some(Backgrounding::Asked)` and is stored as one.
    pub background: Option<letibot_transcript::Backgrounding>,
}

/// One job, shared between the tool that started it and the threads draining it.
#[derive(Debug)]
pub struct Job {
    pub id: JobId,
    /// What the model asked for, verbatim. Never re-rendered from argv: a command
    /// echoed back in a different spelling is a command the model did not write.
    pub command: String,
    /// This job's **own** cgroup, which is what `job_kill` ends, and the scope
    /// that owns its lifetime — the turn, the session, or a named explicit
    /// scope. The distinction is the one a reader needs: `scope` is
    /// `session.job-j4`, which answers "what does killing this touch", and
    /// `owner` is `session.s12`, which answers "when does this die if nobody
    /// touches it".
    ///
    /// **Behind a lock because promotion moves both.** They were plain fields
    /// when a job's owner was decided once at spawn and never again; a promotion
    /// is exactly the event that changes the answer to "when does this die", and
    /// a `job_list` still showing the old one would be telling the model a
    /// lifetime that is no longer true.
    lifetime: Mutex<Lifetime>,
    pub cwd: String,
    pub started: SystemTime,
    /// The pid of the shell that became the command. Its descendants are in the
    /// same cgroup and are **not** tracked individually — that is the point of
    /// having a cgroup.
    pub pid: u32,
    pub state: Mutex<JobState>,
    pub finished: Condvar,
    pub capture: Mutex<Capture>,
    /// When [`Job::settle`] recorded the terminal state, or `None` while it is
    /// running. Stamped **at the settle**, not read afterwards: `elapsed` is
    /// `started.elapsed()` and keeps counting after death, so a settlement
    /// reported an hour later from this field is the job's real runtime, and one
    /// reconstructed from a later clock is a guess with a decimal point.
    settled_at: Mutex<Option<SystemTime>>,
}

impl Job {
    pub fn new(
        id: JobId,
        command: String,
        lifetime: Lifetime,
        cwd: String,
        pid: u32,
        capture_bytes: usize,
    ) -> Job {
        Job {
            id,
            command,
            lifetime: Mutex::new(lifetime),
            cwd,
            started: SystemTime::now(),
            pid,
            state: Mutex::new(JobState::Running),
            finished: Condvar::new(),
            capture: Mutex::new(Capture::new(capture_bytes)),
            settled_at: Mutex::new(None),
        }
    }

    pub fn lifetime(&self) -> Lifetime {
        self.lifetime.lock().expect("job lifetime").clone()
    }

    /// Record that this job's processes now live somewhere else.
    ///
    /// Called only after the migration is **measured**, never before it is
    /// attempted: a lifetime updated on intent rather than on effect is the
    /// announced-but-not-done failure with a job id attached.
    pub fn relocate(&self, lifetime: Lifetime) {
        *self.lifetime.lock().expect("job lifetime") = lifetime;
    }
    /// Wait for the job to finish, or for the deadline. Returns the state as it
    /// stands **after** the wait, which is `Running` when the deadline won.
    pub fn wait_until(&self, deadline: Duration) -> JobState {
        let guard = self.state.lock().expect("job state");
        let (guard, _) = self
            .finished
            .wait_timeout_while(guard, deadline, |s| s.is_running())
            .expect("job state");
        guard.clone()
    }

    /// Poll the state without waiting.
    pub fn state(&self) -> JobState {
        self.state.lock().expect("job state").clone()
    }

    pub fn settle(&self, state: JobState) {
        if !state.is_running() {
            *self.settled_at.lock().expect("job settled_at") = Some(SystemTime::now());
        }
        *self.state.lock().expect("job state") = state;
        self.finished.notify_all();
    }

    /// When the job settled, or `None` while it is running.
    pub fn settled_at(&self) -> Option<SystemTime> {
        *self.settled_at.lock().expect("job settled_at")
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed().unwrap_or(Duration::ZERO)
    }

    /// How long the job ran, from its own stamps. `None` while it is running, and
    /// **this** is the number a settlement reports — not [`Job::elapsed`], which
    /// is a live clock and keeps running after the job does not.
    pub fn ran_for(&self) -> Option<Duration> {
        let settled = self.settled_at()?;
        Some(
            settled
                .duration_since(self.started)
                .unwrap_or(Duration::ZERO),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slice_of_nothing_says_so_rather_than_returning_zero() {
        let c = Capture::new(64);
        let s = c.tail(100);
        let d = s.denominator(&JobId("j1".into()));
        // `0` and `0 of 0` are different facts and this is the one that is not a
        // measurement about content.
        assert!(d.contains("produced 0 bytes"), "{d}");
        assert!(!d.contains("bytes 0–0 of"), "{d}");
    }

    #[test]
    fn the_ring_keeps_the_tail_and_reports_what_fell_off_the_front() {
        let mut c = Capture::new(10);
        c.push(b"0123456789abcdef");
        let s = c.tail(100);
        assert_eq!(s.text(), "6789abcdef");
        assert_eq!(s.produced, 16);
        assert_eq!(s.dropped, 6);
        let d = s.denominator(&JobId("j1".into()));
        assert!(d.contains("NO LONGER RETAINED"), "{d}");
        assert!(d.contains("of 16 produced"), "{d}");
        assert!(!s.complete());
    }

    #[test]
    fn an_absolute_offset_survives_the_ring_moving_under_it() {
        let mut c = Capture::new(8);
        c.push(b"aaaaaaaa");
        let first = c.slice(0, 4);
        assert_eq!(first.text(), "aaaa");
        c.push(b"bbbbbbbb");
        // Offset 4 is gone now. The slice clamps to what is retained and SAYS the
        // window moved, rather than quietly returning a different four bytes and
        // letting the caller believe it read where it asked.
        let second = c.slice(4, 4);
        assert_eq!(second.from, 8);
        assert_eq!(second.dropped, 8);
        assert!(
            second
                .denominator(&JobId("j1".into()))
                .contains("NO LONGER RETAINED")
        );
    }

    #[test]
    fn a_window_that_is_not_the_end_names_the_next_offset() {
        let mut c = Capture::new(64);
        c.push(b"0123456789");
        let s = c.slice(0, 4);
        let d = s.denominator(&JobId("j7".into()));
        assert!(d.contains("offset=4"), "{d}");
        assert!(d.contains("job=\"j7\""), "{d}");
    }

    #[test]
    fn the_five_states_do_not_collapse_into_two() {
        // Three different things happened and they are three different sentences.
        assert_ne!(
            JobState::Exited { code: 137 }.word(),
            JobState::Killed {
                by: "turn scope".into()
            }
            .word()
        );
        assert!(JobState::NotScoped.word().contains("not run"));
        assert!(JobState::Signalled { signal: 9 }.word().contains("9"));
    }

    /// **Every state is classified, and every word is pinned here** — A.2's ruling
    /// (§11.6), and the half of it that only this crate can say.
    ///
    /// Two things are asserted and they are not the same thing. `never_ran` must place
    /// every variant on the right side of the empty/never-ran line, which is what stops a
    /// sixth state from falling through to a head's `wrote nothing` sentence. And each
    /// word must be the literal, because the other half of the pairing is a head's test
    /// that maps these strings to those sentences: a reword here breaks that test, and
    /// that is the only thing that keeps a reword from silently re-introducing the
    /// defect on one side of the wire.
    #[test]
    fn every_state_says_whether_a_process_ever_ran() {
        for st in [
            JobState::Running,
            JobState::Exited { code: 0 },
            JobState::Signalled { signal: 9 },
            JobState::Killed {
                by: "job_kill".into(),
            },
        ] {
            assert!(
                !st.never_ran(),
                "{:?} ran a process and must not be drawn as one that did not ({:?})",
                st,
                st.word()
            );
        }
        assert!(
            JobState::NotScoped.never_ran(),
            "the one state where nothing was executed"
        );
        assert_eq!(JobState::Running.word(), "running");
        assert_eq!(JobState::Exited { code: 0 }.word(), "exited 0");
        assert_eq!(JobState::Signalled { signal: 9 }.word(), "signalled 9");
        assert_eq!(
            JobState::Killed {
                by: "job_kill".into()
            }
            .word(),
            "killed by job_kill"
        );
        assert_eq!(
            JobState::NotScoped.word(),
            "not run (could not join its scope)"
        );
    }
}
