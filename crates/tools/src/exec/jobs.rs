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

/// The next `jN` this process mints. Process-wide, and seeded from the store
/// ([`JobId::seed_above`]) so it does not start at `j1` in every daemon life.
static NEXT_JOB: AtomicU64 = AtomicU64::new(1);

impl JobId {
    pub fn next() -> JobId {
        JobId(format!("j{}", NEXT_JOB.fetch_add(1, Ordering::Relaxed)))
    }

    /// **Never mint `j{n}` or anything below it again in this process** (TODO.md R91.3).
    ///
    /// The counter used to start at `j1` in every daemon, and the store keys a job by
    /// `(session, handle)`: the first job after a restart was a pre-restart `j7` to every reader
    /// that matches by handle — `job_fate`, `RunningJobs`, the delivery sort, a row's
    /// `when: job` — and its row inherited the old job's command. The daemon seeds this from the
    /// store's highest `jN` (`Store::max_job_number`) whenever it opens a session, so handles
    /// stay short and are never repeated for a session. Only ever raises the counter: two
    /// seeds in either order leave it above both.
    pub fn seed_above(n: u64) {
        NEXT_JOB.fetch_max(n.saturating_add(1), Ordering::Relaxed);
    }
}

impl std::fmt::Display for JobId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// **How long a job's name may be** — one bound, because a name is one cell wherever it is
/// read.
///
/// 40, the number this rule has always carried: it is the same bound `task_start` caps a
/// worktree's slug at, and for the same reason — it has to read in a row that is already
/// carrying an id, a state, a byte count and a command.
///
/// **The cap can cut a long name, and that is the trade the number makes.** A name is the
/// first word of a row, not the record: what a reader needs from it is *which of these is
/// the release build*, and a name long enough to spell a whole branch would push the id and
/// the state off the line. Nothing is lost that the row does not already carry beside it.
pub const MAX_JOB_NAME: usize = 40;

/// **The one rule that turns stated words into a job's name**, or `None` when there is no
/// name in them.
///
/// `j57` is a counter: a person watching the jobs pane cannot tell which running job is the
/// release build and which is the fold's tests, and a name is what makes a row ring a bell.
/// **The words are the caller's own claim about the work** — `bash(slug: "release-build")`,
/// and the daemon's own for the work IT starts — and never a parse of a command line: a name
/// derived from `W=…; cd /tmp && cargo test …` is the machine inventing an intent, which is
/// the defect this tree refuses everywhere else.
///
/// A name and not a sentence: runs of whitespace **and control characters** become `-`, and
/// the result is capped at [`MAX_JOB_NAME`], so one stated name is one row in the listing and
/// one cell in the pane whatever was typed. Nothing else is touched — no case change, no
/// derivation, no fallback word for blank words: **blank is `None`**, which is the honest
/// answer when nobody named the job, and the command is already on the row.
///
/// **One rule and not two.** It lives here rather than at either caller because both callers
/// are the same act — `bash`'s `slug` argument and the daemon's name for the work it starts
/// (`mergequeue::gate_job_name` / `review_job_name`) — and a second spelling of *what a name
/// is* is how a row comes to be drawn one way in the pane and another in the transcript.
pub fn job_name(raw: &str) -> Option<String> {
    let mut out = String::new();
    for c in raw.chars() {
        if c.is_control() || c.is_whitespace() {
            if !out.is_empty() && !out.ends_with('-') {
                out.push('-');
            }
        } else {
            out.push(c);
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.chars().count() > MAX_JOB_NAME {
        out = out.chars().take(MAX_JOB_NAME).collect();
        while out.ends_with('-') {
            out.pop();
        }
    }
    (!out.is_empty()).then_some(out)
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
    /// **The process is gone and nothing recorded an exit.** The state of a job a DAEMON that
    /// died had started: it was adopted from its record on the next start, the record's identity
    /// said which process it was, and that process is no longer there — and the `Child` that could
    /// have reported a status died with the daemon that held it.
    ///
    /// **Deliberately not `Exited { code: 0 }` and not `Killed`.** A zero would be a success the
    /// command never reported, and `Killed` would name a killer there was none of; the operator's
    /// rule is that *it vanished* and *it was ended* are different facts, and this is the word for
    /// the first. It is what a supervisor's own `dead` means (`store::ProcState::Dead`), in the
    /// vocabulary this side speaks.
    Dead,
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
            JobState::Dead => "gone (no exit was recorded)".into(),
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
    /// **A file the whole of this job's output keeps going into**, once somebody moved the
    /// run out of the foreground. See [`Capture::spill_to`] for the report that closes.
    spill: Option<Spill>,
}

/// **Where a promoted run's output goes on living** — the file, the byte of the job's own
/// output its first byte is, and whether it is still growing.
///
/// The window ([`Capture`]) stays exactly what it was: 8 MiB of the tail, which is what
/// `job_output` pages and what the denominator counts. This is the copy that is **not**
/// bounded, because a run that went to the background is a run that may print for an hour —
/// and the operator's report was that a promoted run's output *"goes to /dev/null"*, which
/// the window becomes in three measured ways: it drops everything past
/// [`super::host::DEFAULT_CAPTURE_BYTES`], [`super::host::SETTLED_CAPTURE_BUDGET`] releases
/// a settled job's window altogether, and the whole table dies with the daemon.
///
/// `path` is a string rather than a `PathBuf` because every reader of it prints it: the
/// promotion's result, `job_output`, `job_list` and the run's own settlement notice, and a
/// sentence with a `Path` in it is the same sentence with `.display()` in four places.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputFile {
    pub path: String,
    /// **The ABSOLUTE offset in this job's output of the file's first byte.** Usually 0, and
    /// not always: a foreground run that had already outgrown the window when it was
    /// promoted has lost its head for good, and the file honestly starts where the window
    /// did rather than at a zero no reader could check.
    pub from: u64,
    /// Bytes in the file at the moment this was read.
    pub bytes: u64,
    /// **Why the file stopped growing**, when a write failed — `None` while it is keeping
    /// up. A file that quietly stopped at 4 MiB while the job ran on would read as a job
    /// that stopped printing, which is the defect the denominator rules exist against.
    pub problem: Option<String>,
}

/// The open file behind an [`OutputFile`], kept with the ring so there is **one writer** for
/// a job's bytes: the drain thread that already owns them.
#[derive(Debug)]
struct Spill {
    path: std::path::PathBuf,
    /// `None` once the job has settled and the window was released — the file is still on
    /// disk and still named, and this process has no reason to hold a descriptor open for
    /// a run that can never write again.
    file: Option<std::fs::File>,
    from: u64,
    bytes: u64,
    problem: Option<String>,
}

impl Capture {
    pub fn new(cap: usize) -> Capture {
        Capture {
            buf: VecDeque::new(),
            cap,
            dropped: 0,
            produced: 0,
            last_at: None,
            spill: None,
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
        // **After the ring, and with the bytes the ring just dropped**: the file is the copy
        // that keeps everything, so it must not inherit the window's bound.
        if let Some(spill) = &mut self.spill {
            spill.append(bytes);
        }
    }

    /// **Start a file that this job's output keeps going into, and seed it with what the job
    /// has already printed.**
    ///
    /// Called once per job, at the moment the run is moved out of the foreground — the moment
    /// its output stopped having an end in sight. The seed is the **retained window**, so the
    /// file holds everything the result the model just read held, and the two agree about
    /// where they start: [`OutputFile::from`] is the window's own absolute offset, not 0.
    ///
    /// Truncates. A second promotion of a job that already has a file is answered from
    /// [`Capture::output_file`] by the caller rather than by calling this again — truncating a
    /// file a run is writing is the one outcome worse than having none.
    pub fn spill_to(&mut self, path: impl Into<std::path::PathBuf>) -> Result<OutputFile, String> {
        let path = path.into();
        let mut file = std::fs::File::options()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let seed: Vec<u8> = self.buf.iter().copied().collect();
        file.write_all(&seed)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        self.spill = Some(Spill {
            path,
            file: Some(file),
            from: self.dropped,
            bytes: seed.len() as u64,
            problem: None,
        });
        Ok(self.output_file().expect("the spill was just set"))
    }

    /// The file this job's output is also going into, or `None`. See [`OutputFile`].
    pub fn output_file(&self) -> Option<OutputFile> {
        self.spill.as_ref().map(|s| OutputFile {
            path: s.path.display().to_string(),
            from: s.from,
            bytes: s.bytes,
            problem: s.problem.clone(),
        })
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

    /// Bytes the ring is holding right now.
    pub fn retained(&self) -> usize {
        self.buf.len()
    }

    /// **Let go of every retained byte, keeping the counts** (R68.3) — what a settled job's
    /// capture becomes when its host's budget for settled output is spent
    /// ([`super::host::SETTLED_CAPTURE_BUDGET`]). The bytes are counted as dropped off the front,
    /// exactly as the ring drops them, so a slice afterwards says *produced N, dropped N, holding
    /// 0* — a window onto nothing that says so — rather than reading as a job that wrote nothing.
    /// The memory itself is returned, not just emptied: `VecDeque::clear` keeps its allocation.
    ///
    /// **The spill's descriptor goes with it, and the file does not.** A settled job can never
    /// write again, so holding its descriptor until the host dies is a descriptor per promoted
    /// job on a box whose soft limit is 256 (see the `ulimit` rake); the path and the counts
    /// stay, because the bytes are still on disk and a reader must still be told where.
    pub fn release(&mut self) {
        self.dropped += self.buf.len() as u64;
        self.buf = VecDeque::new();
        if let Some(spill) = &mut self.spill {
            spill.file = None;
        }
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

impl Spill {
    /// Append the bytes the ring just took. A failure is recorded once and stops the writes —
    /// retrying a full disk every drain would spend the run's time on a report nobody reads, and
    /// a hole that is *said* is the same rule the denominator follows.
    fn append(&mut self, bytes: &[u8]) {
        if self.problem.is_some() {
            return;
        }
        let Some(file) = &mut self.file else {
            return;
        };
        match file.write_all(bytes) {
            Ok(()) => self.bytes += bytes.len() as u64,
            Err(e) => self.problem = Some(format!("{}: {e}", self.path.display())),
        }
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
    ///
    /// **Two spellings of one job, and they are two different jobs to do.** `label` is what a
    /// reader is SHOWN — `release-build · j65`, the rule [`crate::exec::JobView::label`] and
    /// [`crate::exec::job_label`] share — and `job` is the handle a CALL takes, because
    /// `job_output` with `job="release-build · j65"` resolves to nothing: `pick_job` matches
    /// the whole id or the whole slug and never a label. So this line names the job by its
    /// label and the call it suggests passes the id, which is the same split the sentences
    /// around it make.
    pub fn denominator(&self, job: &JobId, label: &str) -> String {
        if self.produced == 0 {
            return format!("`{label}` has produced 0 bytes of output so far.");
        }
        let mut s = format!(
            "bytes {}–{} of {} produced by `{label}`",
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
    /// **The name the caller gave this work**, or `None` when nobody said one.
    ///
    /// `j57` is a counter: a person watching the pane cannot tell which running job is
    /// the release build and which is the fold's tests, and a name is what makes a row
    /// ring a bell. The name is the **agent's own stated intent** —
    /// `bash(command = …, background: true, slug: "release-build")` — and never a parse
    /// of the command line: a slug derived from `W=…; cd /tmp && cargo test …` is the
    /// machine inventing an intent, which is the defect this tree refuses everywhere
    /// else. **Absent is the honest answer when nobody said one**; the command is
    /// already on the row, so a guessed slug earns nothing and costs the truth.
    ///
    /// **Unique within the session and fixed for the job's life.**
    /// [`super::host::ProcessHost::spawn`] takes the name the caller asked for and
    /// appends `-2`, `-3` … until it is free; nothing moves it afterwards, because a
    /// name that moves while the job runs is worse than none.
    pub slug: Option<String>,
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
    /// **The memory cap this job runs under**, decided at the spawn (TODO.md R65).
    memory_cap: MemoryCap,
    /// What the kernel counted for this job's memory, read once at the settle — before the
    /// scope that holds the counters can be ended. `None` on a tree with no memory
    /// controller, and while the job runs.
    memory_used: Mutex<Option<super::scope::MemoryUse>>,
}

/// **The cap a job was started under**, as three different facts — because *nobody asked*,
/// *capped*, and *asked and could not be capped* each need a different sentence in the
/// result, and the third must never read like the second.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryCap {
    /// No cap was asked for — the operator's own run, or a host whose caller passed none.
    NotAsked,
    /// The kernel holds this many bytes as the job's `memory.max`.
    Capped(u64),
    /// A cap was asked for and could not be set; the job runs UNCAPPED, and `why` says so.
    Uncapped { asked: u64, why: String },
}

impl Job {
    pub fn new(
        id: JobId,
        command: String,
        slug: Option<String>,
        lifetime: Lifetime,
        cwd: String,
        pid: u32,
        capture_bytes: usize,
        stdin: Stdin,
    ) -> Job {
        Job {
            id,
            command,
            slug,
            lifetime: Mutex::new(lifetime),
            cwd,
            started: SystemTime::now(),
            pid,
            state: Mutex::new(JobState::Running),
            finished: Condvar::new(),
            capture: Mutex::new(Capture::new(capture_bytes)),
            settled_at: Mutex::new(None),
            stdin,
            memory_cap: MemoryCap::NotAsked,
            memory_used: Mutex::new(None),
        }
    }

    /// The job, started under `cap`. A builder rather than a ninth argument to [`Job::new`]:
    /// the cap is decided by the host at the spawn and is nobody else's to pass.
    pub fn with_memory_cap(mut self, cap: MemoryCap) -> Job {
        self.memory_cap = cap;
        self
    }

    pub fn memory_cap(&self) -> &MemoryCap {
        &self.memory_cap
    }

    /// What the kernel counted for this job's memory — `None` until it settles, and on a tree
    /// with no memory controller.
    pub fn memory_used(&self) -> Option<super::scope::MemoryUse> {
        *self.memory_used.lock().expect("job memory")
    }

    /// Recorded by the waiter, once, just before [`Job::settle`].
    pub fn record_memory(&self, used: super::scope::MemoryUse) {
        *self.memory_used.lock().expect("job memory") = Some(used);
    }

    /// **The way in to this job's stdin.** Cloned out rather than lent, because the
    /// reader is on another thread: the run's own thread is blocked inside the very
    /// call that started this job, so an answer arrives from a thread that does not hold
    /// the `Arc<Job>`. See [`Stdin`].
    pub fn stdin(&self) -> Stdin {
        self.stdin.clone()
    }

    /// **The file this job's output is also going into**, once it was promoted out of the
    /// foreground, or `None`. Read off the capture rather than kept beside it, so there is
    /// one record of where the bytes are and not two that can disagree.
    pub fn output_file(&self) -> Option<OutputFile> {
        self.capture.lock().expect("capture").output_file()
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

    /// **A seeded counter never mints a handle at or below the seed** (R91.3), and a lower seed
    /// does not move it back. Process-wide, so this asserts bounds rather than exact values: other
    /// tests mint handles concurrently.
    #[test]
    fn a_seeded_counter_mints_above_the_store_and_never_goes_back() {
        let n = |id: JobId| id.0[1..].parse::<u64>().unwrap();
        JobId::seed_above(7_000_000);
        assert!(
            n(JobId::next()) > 7_000_000,
            "a handle at or below the seed"
        );
        JobId::seed_above(3);
        assert!(
            n(JobId::next()) > 7_000_000,
            "a lower seed moved the counter back"
        );
    }

    use super::*;

    /// **The spill is the copy that is not bounded** — the whole reason a promoted run gets a
    /// file, and the operator's report in one assertion.
    ///
    /// The window drops from the front past its cap and says so; the file must not inherit that
    /// bound, or a promoted build's first half is gone exactly when the run stopped being short
    /// enough to fit the window. The seed is the retained window — so a job whose window had
    /// already dropped bytes gets a file that says which byte it starts at, rather than a file
    /// silently pretending to be the whole output.
    #[test]
    fn a_spilled_capture_keeps_every_byte_while_the_window_drops_the_head() {
        let dir = std::env::temp_dir().join(format!("letibot-spill-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a directory to spill into");
        let path = dir.join("job-j1.out");

        let mut cap = Capture::new(16);
        cap.push(b"aaaa");
        let opened = cap.spill_to(&path).expect("the file opens");
        assert_eq!(
            (opened.from, opened.bytes),
            (0, 4),
            "the file starts with what the window held, from byte 0"
        );

        // Twenty more bytes into a sixteen-byte window: the ring drops, the file does not.
        cap.push(b"bbbbbbbbbbbbbbbbbbbb");
        assert_eq!(cap.retained(), 16, "the window keeps its cap");
        assert_eq!(cap.produced(), 24);
        let on_disk = std::fs::read(&path).expect("the file is there");
        assert_eq!(
            on_disk.len() as u64,
            cap.produced(),
            "the file has every byte the job wrote, not the window's sixteen"
        );
        assert!(
            String::from_utf8_lossy(&on_disk).starts_with("aaaabbbb"),
            "and it starts where the job did: {:?}",
            String::from_utf8_lossy(&on_disk)
        );
        assert_eq!(
            cap.output_file().expect("still named").bytes,
            24,
            "the record counts the bytes that are actually in the file"
        );

        // **A window that had already dropped bytes makes an honest file, not a whole one.**
        let late = dir.join("job-j2.out");
        let mut cap = Capture::new(8);
        cap.push(b"0123456789abcdef");
        let opened = cap.spill_to(&late).expect("the second file opens");
        assert_eq!(
            opened.from, 8,
            "the file begins at the byte its window begins at"
        );
        assert_eq!(opened.bytes, 8);
        assert_eq!(
            std::fs::read(&late).expect("the second file"),
            b"89abcdef".to_vec()
        );

        // **Releasing the window closes the descriptor and keeps the file.** A settled job can
        // never write again; the bytes and the path must outlive the handle.
        cap.release();
        let kept = cap
            .output_file()
            .expect("the file is still named after the release");
        assert_eq!(kept.bytes, 8);
        cap.push(b"nothing should reach a released file");
        assert_eq!(
            std::fs::read(&late).expect("the second file").len(),
            8,
            "a released spill takes no more writes"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// **A name is one line, capped, and blank words are no name at all.**
    ///
    /// The rule's own tests, at its own home — `bash`'s tests pin that its `slug` argument
    /// reads THIS function and not a copy of it, and this pins what the function answers.
    ///
    /// The three claims: nothing is invented (blank is `None`, not a placeholder word), a name
    /// is a name and not a sentence (whitespace and control characters break it, and a break is
    /// never doubled or left dangling), and the bound is [`MAX_JOB_NAME`] — the number a row's
    /// first cell can spend without pushing the id and the state off the line.
    #[test]
    fn a_job_name_is_one_capped_line_and_blank_words_are_no_name() {
        // Nobody named it: every spelling of *no words* is the same answer, and it is `None`
        // rather than an empty string, because the caller's fallback is the id.
        for raw in ["", "   ", "\t\n", "- -"] {
            assert_eq!(job_name(raw), None, "{raw:?}");
        }
        assert_eq!(job_name("release-build").as_deref(), Some("release-build"));
        // One line however it was typed, and no doubled or trailing separator.
        assert_eq!(
            job_name("  fold\n\t tests  ").as_deref(),
            Some("fold-tests")
        );
        assert_eq!(job_name("a\u{7}b").as_deref(), Some("a-b"));
        // The cap, and it takes the HEAD — the words a caller stated are in the order they
        // stated them, so the front is the part that names the work.
        let long = job_name(&"x".repeat(200)).unwrap();
        assert_eq!(long.chars().count(), MAX_JOB_NAME);
        // And a cap that lands on a separator does not leave one dangling.
        let words = job_name(&format!("{} tail", "word ".repeat(20))).unwrap();
        assert!(!words.ends_with('-'), "{words:?}");
        assert!(words.chars().count() <= MAX_JOB_NAME);
    }

    #[test]
    fn a_slice_of_nothing_says_so_rather_than_returning_zero() {
        let c = Capture::new(64);
        let s = c.tail(100);
        let d = s.denominator(&JobId("j1".into()), "j1");
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
        let d = s.denominator(&JobId("j1".into()), "j1");
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
                .denominator(&JobId("j1".into()), "j1")
                .contains("NO LONGER RETAINED")
        );
    }

    #[test]
    fn a_window_that_is_not_the_end_names_the_next_offset() {
        let mut c = Capture::new(64);
        c.push(b"0123456789");
        let s = c.slice(0, 4);
        let d = s.denominator(&JobId("j7".into()), "j7");
        assert!(d.contains("offset=4"), "{d}");
        assert!(d.contains("job=\"j7\""), "{d}");
    }

    /// **The name is the reader's half and the id is the caller's half, in one sentence.**
    ///
    /// The job_output header is this line, and the complaint that put a name on a job was
    /// about exactly this kind of counter. What must NOT happen is the label leaking into the
    /// `job="…"` suggestion: `release-build · j65` is not a handle `pick_job` resolves, so a
    /// suggestion carrying one would send the caller to a refusal.
    #[test]
    fn the_denominator_names_the_job_and_suggests_a_handle_that_resolves() {
        let mut c = Capture::new(64);
        c.push(b"0123456789");
        let s = c.slice(0, 4);
        let named = s.denominator(&JobId("j65".into()), "release-build · j65");
        assert!(
            named.contains("produced by `release-build · j65`"),
            "a named job is named in the header: {named}"
        );
        assert!(
            named.contains("job=\"j65\""),
            "and the argument it suggests is the resolvable one: {named}"
        );
        assert!(!named.contains("job=\"release-build"), "{named}");
        // **And a job nobody named is the id in both places**, which is the label's own
        // fallback rather than a second rule.
        let bare = s.denominator(&JobId("j65".into()), "j65");
        assert!(bare.contains("produced by `j65`"), "{bare}");
        assert!(bare.contains("job=\"j65\""), "{bare}");
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
