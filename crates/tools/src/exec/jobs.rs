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
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
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

/// **What `by` says when the run's own deadline ended it.**
///
/// A named constant rather than a literal in two places, because it is read back: the
/// watchdog writes it into [`JobState::Killed`] and `bash` matches on it to report a
/// deadline rather than a failure. Two spellings of this string would be a timeout
/// rendered as an error, which is the one thing the deadline branch exists to avoid.
pub const DEADLINE_KILL: &str = "its deadline";

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

/// **A way to write to a running command's input** — the handle the daemon keeps for
/// the operator's own run, and the one place a person's answer can go.
///
/// # Why only the operator's run has one
///
/// Every other job's stdin is `/dev/null`, and that is not an oversight: a model's
/// `bash` call that reads stdin must get **EOF** rather than block for a person who is
/// not there. The operator's own `!` line is the one run where a person *is* there —
/// they typed the line and they are watching the bytes — so it is the one run whose
/// input somebody holds. [`super::host::SpawnRequest::tty`] is the flag, and
/// this is the fourth consequence of it.
///
/// The defect this closes, in the operator's own words: *"we need this interactivity
/// working"* — after `! sudo apt install mc`, which streamed its progress and then
/// **aborted at `Continue? [Y/n]`**, because `/dev/null` on stdin is an EOF and EOF is
/// not a `Y`.
///
/// # **It is the run's terminal, and it used to be a pipe beside it**
///
/// The first version of this wrote to a pipe on the run's fd 0. The operator's answer to
/// the two lines bash printed about job control was *"nah, i think that bash should feel
/// comfortable actually"*, so the run now has the pty on all three descriptors
/// ([`super::pty`]'s header carries the whole argument) and **this is a second handle on
/// that pty's master**: what the daemon writes goes into the same terminal the command is
/// reading, which is what every other implementation of this does. The pipe was the
/// anomaly — it is exactly what left bash without a controlling terminal on its own
/// stdin.
///
/// [`Input::Pipe`] survives for the one case that is not a choice: a box where
/// [`super::pty::Pty::open`] fails. The run then keeps today's pipe, which keeps it
/// **answerable** — the capability this type exists for — at the cost of the colour and
/// the terminal it never had on that box anyway.
///
/// # What the held pipe was for, and that the terminal carries all of it
///
/// Four things went through the pipe this replaces. Each was checked against the terminal
/// rather than assumed — the question being *can this device do what that one did*, and a `no`
/// would have meant stopping here rather than half-migrating:
///
/// 1. **The run's output capture.** Untouched, and not by luck: it is fed by the *other* two
///    descriptors, which were already this pty. The daemon's own writes do **not** enter it,
///    because [`super::pty::Pty::no_echo`] is on — measured, and pinned by `host`'s
///    `the_operators_own_run_has_a_stdin_a_line_can_be_sent_to`, which asserts the exact
///    capture rather than a substring.
/// 2. **The byte caps.** `host`'s `capture_bytes` and `bash`'s `clip_tail` both work on captured
///    *text*, and the text is the same bytes either way.
/// 3. **The `queued` echo.** Not this mechanism at all: it is the head's own echo of the
///    operator's typed line (`letibot_turn::steering`), and it never touched the run's input.
/// 4. **The `!send` addressing.** Unchanged in shape — a driver looks the run up by session id
///    and writes to whatever handle that run has, so *which* device is behind it stays this
///    type's business and nobody else's.
///
/// The one thing that genuinely differs is stated where it belongs rather than hidden here:
/// with a terminal on fd 0, a `!` command that reads stdin **waits** where `/dev/null` used to
/// hand it an EOF. `host`'s spawn note and [`super::pty`]'s header carry that trade.
///
/// # `Default` is `none`, and `none` is a fact rather than an error
///
/// A job whose stdin is `/dev/null` has a `Stdin` too — the empty one — so a caller
/// never has to carry an `Option` around a handle that is sometimes absent for a reason
/// it does not care about. [`Stdin::send_line`] says what the absence means.
#[derive(Clone, Default)]
pub struct Stdin {
    /// `Some` only for a run whose input the daemon holds. The inner `Option` is the
    /// close: an input that has been closed is not one that was never there, and the two
    /// sentences a caller reads for them differ.
    inner: Option<Arc<Mutex<Open>>>,
}

/// What the daemon is holding, and when it last wrote to it.
///
/// The clock is here rather than in the reader because **this is the only writer**: *"the
/// run is blocked reading its terminal"* is only half of what a card claims, and the other
/// half is that nobody has answered it lately. [`Stdin::since_last_input`] is that half.
struct Open {
    input: Option<Input>,
    last_sent: Option<std::time::Instant>,
}

/// **The two ways a run's input can be reachable**, and they are different devices.
enum Input {
    /// The write end of a pipe on the run's fd 0 — the fallback when the pty could not be
    /// opened. [`super::ask::InputEnd::Pipe`] is how a reader names it.
    Pipe(std::process::ChildStdin),
    /// **A second handle on the run's own terminal**, with the name of its slave end.
    ///
    /// The name travels with the handle because the reader has to tell *this* terminal
    /// from every other tty on the box, and only the slave's own name does that — see
    /// [`super::pty::Pty::slave_path`].
    Terminal {
        master: std::fs::File,
        slave_path: String,
    },
}

impl Stdin {
    /// **No input to write to** — `/dev/null`, which is every job but the operator's own.
    pub fn none() -> Stdin {
        Stdin { inner: None }
    }

    /// The write end of a pipe handed to a command, as `host::spawn` takes it out of the
    /// child it just started. The pty-less fallback; see the type's own note.
    pub fn pipe(w: std::process::ChildStdin) -> Stdin {
        Stdin {
            inner: Some(Arc::new(Mutex::new(Open {
                input: Some(Input::Pipe(w)),
                last_sent: None,
            }))),
        }
    }

    /// **The run's own terminal**, as `host::spawn` takes a second handle on the pty
    /// master it just handed to the child on all three descriptors.
    pub fn terminal(master: std::fs::File, slave_path: String) -> Stdin {
        Stdin {
            inner: Some(Arc::new(Mutex::new(Open {
                input: Some(Input::Terminal { master, slave_path }),
                last_sent: None,
            }))),
        }
    }

    /// Whether a command is out there that could be answered at all. `false` for a run
    /// whose stdin is `/dev/null` **and** for one whose input has already been closed.
    pub fn is_open(&self) -> bool {
        self.inner
            .as_ref()
            .is_some_and(|w| w.lock().map(|g| g.input.is_some()).unwrap_or(false))
    }

    /// **Which device this is**, for a reader that has to tell it apart from every other
    /// descriptor on the box.
    ///
    /// `super::ask` is the reader: *"the program is blocked reading the answer we hold"* is
    /// only a fact if the descriptor the program is blocked on is **this** one, and the
    /// identity is how the kernel lets the two be compared — an inode for a pipe, the
    /// slave's own name for a terminal. A `grep` blocked on `ls`'s pipe in `! ls | grep
    /// foo` is a read too, and without this a slow `ls` would raise a card claiming the run
    /// was waiting for a line.
    ///
    /// `None` when there is no input at all (a `/dev/null` run, or a closed one).
    pub fn input_end(&self) -> Option<super::ask::InputEnd> {
        let inner = self.inner.as_ref()?;
        let guard = inner.lock().unwrap_or_else(|e| e.into_inner());
        match guard.input.as_ref()? {
            Input::Pipe(w) => {
                use std::os::fd::AsRawFd;
                super::ask::pipe_inode(w.as_raw_fd()).map(super::ask::InputEnd::Pipe)
            }
            Input::Terminal { slave_path, .. } => {
                Some(super::ask::InputEnd::Terminal(slave_path.clone()))
            }
        }
    }

    /// **How long since the daemon last wrote a line to this run** — `None` when it never
    /// has.
    ///
    /// The reader is the prompt card, and this is the half of its claim that is about the
    /// person rather than about the process: with the terminal on fd 0, a program that has
    /// just been answered is blocked reading its terminal *again* within microseconds, and
    /// a card raised on that would be the daemon telling somebody about a question they
    /// answered a moment ago. `None` is *never written to*, which is the quietest case
    /// there is and not a missing measurement.
    pub fn since_last_input(&self) -> Option<Duration> {
        let inner = self.inner.as_ref()?;
        let guard = inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.last_sent.map(|t| t.elapsed())
    }

    /// **Send one line, with the newline that makes it a line.**
    ///
    /// An empty `line` is a bare Enter, and it is not a special case invented here: the
    /// question this exists for — `Continue? [Y/n]` — takes Enter as its default answer,
    /// so an empty line is a real answer and not a mistake to be refused.
    ///
    /// # The write blocks, and the bound is the device's own
    ///
    /// This is a blocking write, which is what both handles give and what a non-blocking
    /// version would have to replace with a thread per answer. A pipe's buffer is 64 KiB on
    /// Linux and a line is a line; a pty's input queue is smaller, and a program that has
    /// stopped reading **and** has several thousand unread lines in front of it is what
    /// blocks either of them. Named rather than discovered: the alternative is an
    /// `O_NONBLOCK` dance that would turn a full device into a silently dropped answer, and
    /// a dropped answer is the failure this whole mechanism exists to end.
    ///
    /// # Why the failure is a `String` and not an `io::Error`
    ///
    /// The reader is a person: every one of these becomes a sentence on a screen, and the
    /// three cases (no input, closed input, the far end gone) need three different ones.
    pub fn send_line(&self, line: &str) -> Result<(), String> {
        let Some(inner) = self.inner.as_ref() else {
            return Err(
                "this command's stdin is /dev/null — there is nothing to send to. \
                        Only your own `!` line is answered."
                    .to_string(),
            );
        };
        let mut guard = inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(input) = guard.input.as_mut() else {
            return Err("this command's input is already closed".to_string());
        };
        // One `write_all` for the line and its newline, so a reader sees them together:
        // a program that reads a line at a time must not be able to see a line whose
        // terminator has not arrived. On a terminal this is also what releases it: the
        // line discipline is in canonical mode, so nothing reaches the program until the
        // newline does.
        let mut bytes = Vec::with_capacity(line.len() + 1);
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
        let written = match input {
            Input::Pipe(w) => w.write_all(&bytes).and_then(|_| w.flush()),
            Input::Terminal { master, .. } => master.write_all(&bytes).and_then(|_| master.flush()),
        };
        match written {
            Ok(()) => {
                // **The beat starts here.** See [`Stdin::since_last_input`] for what reads it.
                guard.last_sent = Some(std::time::Instant::now());
                Ok(())
            }
            Err(e) => {
                // The far end is gone. Forget the handle rather than leaving one that can
                // only fail again with the same sentence.
                guard.input = None;
                Err(format!("the command is no longer reading its stdin: {e}"))
            }
        }
    }
}

impl std::fmt::Debug for Stdin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Stdin")
            .field("open", &self.is_open())
            .finish()
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

    /// **Does the retained window hold this text?**
    ///
    /// One reader and one use: the wrapper's own failure marker, which is the evidence a
    /// launcher failure is classified by (`host::launcher_failed` — the exit code alone is
    /// not it, because 125 is a legitimate code). It is a substring test over the retained
    /// bytes, and the ring is bounded, which is enough there for the reason stated at the
    /// call site: a wrapper that could not join its cgroup ran no command, so the marker is
    /// the whole of what was written.
    pub fn contains(&self, needle: &str) -> bool {
        let n = needle.as_bytes();
        if n.is_empty() || n.len() > self.buf.len() {
            return false;
        }
        let hay: Vec<u8> = self.buf.iter().copied().collect();
        hay.windows(n.len()).any(|w| w == n)
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

/// **A deadline, and the run it belongs to.**
///
/// The pair travels together because a deadline with no job is a number and a job with no
/// deadline is one nobody can end; see [`super::host::Deadlines`] for the thread that reads
/// these.
pub struct Due {
    /// When the run must be gone.
    pub at: std::time::Instant,
    /// The run. Held as an `Arc` so the watchdog can settle it and end its cgroup without
    /// taking any lock the run's own thread might be holding.
    pub job: std::sync::Arc<Job>,
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
    /// **The way in to this job's stdin**, when it has one. See [`Stdin`]: `/dev/null`
    /// for every job but the operator's own run.
    stdin: Stdin,
}

impl Job {
    pub fn new(
        id: JobId,
        command: String,
        lifetime: Lifetime,
        cwd: String,
        pid: u32,
        capture_bytes: usize,
        stdin: Stdin,
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
            stdin,
        }
    }

    /// **The way in to this job's stdin.** Cloned out rather than lent, because the
    /// reader is on another thread: the daemon's own worker is blocked inside the very
    /// call that started this job, so an answer arrives from a thread that does not hold
    /// the `Arc<Job>`. See [`Stdin`].
    pub fn stdin(&self) -> Stdin {
        self.stdin.clone()
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
