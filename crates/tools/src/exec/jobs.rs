//! One running command: state, captured output, and the denominator that travels with every slice.
//! A foreground `bash` call is the same job, so its output stays reachable through `job_output`.
//! The capture keeps the tail; [`OutputSlice`] says what it dropped.

use std::collections::VecDeque;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime};

/// A job's name, as the model spells it back. An opaque string, not a number: a number invites
/// the model to guess the next one, and a guessed id would be a call about somebody else's
/// process.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct JobId(pub String);

/// The next `jN` this process mints. Process-wide, and seeded from the store
/// ([`JobId::seed_above`]) so it does not start at `j1` in every daemon life.
static NEXT_JOB: AtomicU64 = AtomicU64::new(1);

impl JobId {
    pub fn next() -> JobId {
        JobId(format!("j{}", NEXT_JOB.fetch_add(1, Ordering::Relaxed)))
    }

    /// Never mint `j{n}` or anything below it again in this process. The store keys a job by
    /// `(session, handle)`, so a counter restarted at `j1` gives a new job the handle, and the row,
    /// of a pre-restart one. Only ever raises: two seeds in either order leave it above both.
    pub fn seed_above(n: u64) {
        NEXT_JOB.fetch_max(n.saturating_add(1), Ordering::Relaxed);
    }
}

impl std::fmt::Display for JobId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// How long a job's name may be - one bound, because a name is one cell wherever it is
/// read, and the row already carries an id, a state, a byte count and a command. The cap
/// can cut a name; the front is what says which job this is, and the row says the rest.
pub const MAX_JOB_NAME: usize = 40;

/// The one rule that turns stated words into a job's name, or `None` when there are none:
/// whitespace and control characters become `-`, capped at [`MAX_JOB_NAME`]. The words are the
/// caller's claim, never a parse of a command line, and blank is `None` - the command is on the row.
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

/// What `by` says when the run's own deadline ended it. A named constant because it is read
/// back: `bash` matches on it to report a deadline rather than a failure, so two spellings
/// would render a timeout as an error.
pub const DEADLINE_KILL: &str = "its deadline";

/// What `by` says when the harness's own dead check ended the run: no output and no CPU progress
/// for the whole window. A different fact from a deadline, which somebody asked for.
pub const DEAD_KILL: &str = "the dead check — no output and no CPU progress";

/// What a kill leaves the work, said to whoever reads it. A kill ends the work and not the output,
/// and a killed job read as a finished one is a model building on half an answer. One constant for
/// every reader (a result, a settlement notice, a channel push), so they cannot drift apart.
pub const PARTIAL: &str = "its output is partial — it was ended, not finished. `job_output` reads \
                          what it wrote; `job_extend` gives a running job a longer deadline, and \
                          `job_kill` ends one you no longer want.";

/// The partial sentence for a settlement, whose only surviving fact is the state WORD: the job has
/// been reaped by the time a notice is built. `None` for anything that was not killed.
pub fn partial_of(word: &str) -> Option<&'static str> {
    word.starts_with("killed by ").then_some(PARTIAL)
}

/// Where a job is. Five states, and the last three are not interchangeable: a non-zero
/// exit, a kill, and a command that never joined its cgroup are three different facts, and
/// one "failed" for all three reports an absence as a success.
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
    /// The scope reaped it, or `job_kill` did. Never reported as an exit code.
    Killed {
        by: String,
    },
    /// The wrapper could not put the process in its cgroup, so the command was
    /// never run. See [`super::scope::join_script`].
    NotScoped,
    /// The process is gone and nothing recorded an exit - a job a daemon that died had
    /// started. Deliberately not `Exited { code: 0 }` (a success nobody reported) and not
    /// `Killed` (a killer there was none of): vanished and ended are different facts.
    Dead,
}

impl JobState {
    pub fn is_running(&self) -> bool {
        matches!(self, JobState::Running)
    }

    /// The word a listing shows. Not "ok"/"error": what a caller needs is what happened to
    /// the process, and success is a property of the command's own exit code.
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

    /// Whether nothing was ever executed for this job, as opposed to a process that ran and
    /// wrote nothing. `NotScoped` is the only such state, and an empty output window cannot
    /// tell the two apart on its own.
    ///
    /// ```text
    /// not run (could not join its scope)      ← the header
    /// it wrote nothing at all.                ← and it never started
    /// ```
    ///
    /// A bool rather than a sentence because the two readers draw different things from it:
    /// one a line under a state word, the other a duration it may not claim.
    pub fn never_ran(&self) -> bool {
        matches!(self, JobState::NotScoped)
    }

    /// The partial sentence a killed job owes its reader, or `None` for a state that is a finish.
    /// See [`PARTIAL`].
    pub fn partial(&self) -> Option<&'static str> {
        matches!(self, JobState::Killed { .. }).then_some(PARTIAL)
    }
}

/// A way to write to a running command's input - the handle the daemon keeps for the
/// operator's own run, the only run where a person is there to answer. Every other job's
/// stdin is `/dev/null`, so a model's `bash` that reads it gets EOF rather than blocking.
#[derive(Clone, Default)]
pub struct Stdin {
    /// `Some` only for a run whose input the daemon holds. The inner `Option` is the
    /// close: an input that has been closed is not one that was never there, and the two
    /// sentences a caller reads for them differ.
    inner: Option<Arc<Mutex<Open>>>,
}

/// What the daemon is holding, and when it last wrote to it. The clock lives here because
/// this is the only writer: [`Stdin::since_last_input`] is the half of a prompt card's claim
/// that is about the person rather than the process.
struct Open {
    input: Option<Input>,
    last_sent: Option<std::time::Instant>,
}

/// The two ways a run's input can be reachable, and they are different devices.
enum Input {
    /// The write end of a pipe on the run's fd 0 - the fallback when the pty could not be
    /// opened. [`super::ask::InputEnd::Pipe`] is how a reader names it.
    Pipe(std::process::ChildStdin),
    /// A second handle on the run's own terminal, with the name of its slave end. The name
    /// travels with the handle because a reader must tell this terminal from every other tty:
    /// see [`super::pty::Pty::slave_path`].
    Terminal {
        master: std::fs::File,
        slave_path: String,
    },
}

impl Stdin {
    /// No input to write to - `/dev/null`, which is every job but the operator's own.
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

    /// The run's own terminal, as `host::spawn` takes a second handle on the pty master it
    /// just handed to the child on all three descriptors.
    pub fn terminal(master: std::fs::File, slave_path: String) -> Stdin {
        Stdin {
            inner: Some(Arc::new(Mutex::new(Open {
                input: Some(Input::Terminal { master, slave_path }),
                last_sent: None,
            }))),
        }
    }

    /// Whether a command is out there that could be answered at all. `false` for a run whose
    /// stdin is `/dev/null` and for one whose input has already been closed.
    pub fn is_open(&self) -> bool {
        self.inner
            .as_ref()
            .is_some_and(|w| w.lock().map(|g| g.input.is_some()).unwrap_or(false))
    }

    /// Which device this is, for a reader telling it from every other descriptor on the box:
    /// a program blocked on some other pipe is not waiting for us. `None` when there is no
    /// input at all (a `/dev/null` run, or a closed one).
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

    /// How long since the daemon last wrote a line to this run - `None` when it never has. The
    /// prompt card reads it so a program answered a moment ago, blocked on its terminal again,
    /// is not announced as waiting on the person.
    pub fn since_last_input(&self) -> Option<Duration> {
        let inner = self.inner.as_ref()?;
        let guard = inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.last_sent.map(|t| t.elapsed())
    }

    /// Send one line, with the newline that makes it a line; an empty `line` is a bare Enter,
    /// a real answer at the `Continue? [Y/n]` prompt this exists for. The write blocks on the
    /// device's own buffer, because a dropped answer is the failure this mechanism ends.
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
        // One `write_all` for the line and its newline, so a reader sees them together: a
        // program reading a line at a time must not see a terminator that has not arrived. On a
        // terminal the canonical line discipline also needs the newline to release the line.
        let mut bytes = Vec::with_capacity(line.len() + 1);
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
        let written = match input {
            Input::Pipe(w) => w.write_all(&bytes).and_then(|_| w.flush()),
            Input::Terminal { master, .. } => master.write_all(&bytes).and_then(|_| master.flush()),
        };
        match written {
            Ok(()) => {
                // The beat starts here. See [`Stdin::since_last_input`] for what reads it.
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
    /// Every byte the process ever wrote. The denominator.
    produced: u64,
    /// When the last byte arrived. This is the number a wait reports, and it is deliberately
    /// not "the process is alive": a progress timestamp is about work, while a publication
    /// timestamp would call a dead job live.
    last_at: Option<SystemTime>,
    /// A file this job's whole output keeps going into, for a run with no deadline. See
    /// [`Capture::spill_to`].
    spill: Option<Spill>,
    /// Secrets taken out where the bytes arrive, so the window and the spill file are the same
    /// bytes. `None` for a session carrying none.
    masker: Option<Masker>,
    /// The trailer this job's output gives up, when its command was appended one. `None` for
    /// every job that was not armed.
    trailer: Option<TrailerTaker>,
}

/// Takes one job's OSC trailer out of its output as the bytes arrive, and keeps the report it
/// carried for the harness. Holds back only the suffix that could still grow into the marker, so
/// the window, the spill file and the denominator carry nothing the command did not print.
struct TrailerTaker {
    mark: String,
    held: Vec<u8>,
    /// `Some(Ok(pairs))` once a whole trailer was read, `Some(Err)` for one this build refuses
    /// to believe, `None` while there is neither.
    report: Option<Result<String, String>>,
}

impl std::fmt::Debug for TrailerTaker {
    /// The report may hold a credential, so this says it is there without printing it - the
    /// same rule `shell::Turn`'s `Debug` keeps for the same reason.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrailerTaker")
            .field("mark", &self.mark)
            .field("held", &self.held.len())
            .field("report", &self.report.is_some())
            .finish()
    }
}

impl TrailerTaker {
    fn new(mark: &str) -> TrailerTaker {
        TrailerTaker {
            mark: mark.to_string(),
            held: Vec::new(),
            report: None,
        }
    }

    /// What may be published of `bytes` plus whatever was held, the trailer taken out.
    fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.held.extend_from_slice(bytes);
        let mut out = Vec::new();
        loop {
            match super::shell::take_turn(&mut self.held, &self.mark) {
                Ok(Some(turn)) => {
                    out.extend_from_slice(&turn.bytes);
                    // The first trailer is the report; a second with this mark is a forgery,
                    // and it is stripped without overwriting what the shell said.
                    if self.report.is_none() {
                        self.report = Some(Ok(turn.env));
                    }
                }
                Ok(None) => break,
                Err(why) => {
                    // A marker we cannot believe is still machinery: it comes out of the
                    // stream, and the report says why rather than guessing a value.
                    match take_marker(&mut self.held, &self.mark) {
                        Some(before) => {
                            out.extend_from_slice(&before);
                            if self.report.is_none() {
                                self.report = Some(Err(why.to_string()));
                            }
                        }
                        None => break,
                    }
                }
            }
        }
        // Not a whole trailer yet: publish what cannot be part of one. Once the opening has arrived,
        // everything from it is held until its BEL - a marker is not something the rest of the
        // stream can dilute - and before that, only bytes that could grow INTO an opening are held.
        let open = super::shell::opening(&self.mark);
        let publish = match self
            .held
            .windows(open.len())
            .position(|w| w == open.as_slice())
        {
            Some(at) => at,
            None => self.held.len() - self.carry(),
        };
        out.extend(self.held.drain(..publish));
        out
    }

    /// The stream is over, so nothing held can still grow into a marker: it was output that
    /// merely resembled one, and it goes out.
    fn finish(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.held)
    }

    /// How many trailing bytes could still be the start of the marker: the longest suffix of
    /// the hold that is a proper prefix of it.
    fn carry(&self) -> usize {
        let open = super::shell::opening(&self.mark);
        (1..open.len())
            .rev()
            .find(|&k| self.held.ends_with(&open[..k]))
            .unwrap_or(0)
    }
}

/// Take a marker we refuse to parse out of `buf`, through its BEL, and answer with what stood
/// before it. `None` when even the BEL has not arrived: still reading.
fn take_marker(buf: &mut Vec<u8>, mark: &str) -> Option<Vec<u8>> {
    let open = super::shell::opening(mark);
    let at = buf.windows(open.len()).position(|w| w == open.as_slice())?;
    let rel = buf[at + open.len()..].iter().position(|b| *b == 0x07)?;
    let before = buf[..at].to_vec();
    buf.drain(..at + open.len() + rel + 1);
    Some(before)
}

/// Takes secret values out of a stream as it arrives. Bytes a secret could still complete are
/// held back, because a token split across two reads is otherwise half published and half masked.
#[derive(Debug, Default)]
struct Masker {
    needles: Vec<Vec<u8>>,
    held: Vec<u8>,
}

impl Masker {
    fn new(needles: Vec<Vec<u8>>) -> Masker {
        Masker {
            needles,
            held: Vec::new(),
        }
    }

    /// How many trailing bytes could still be the start of a needle: the longest suffix of the hold
    /// that is a proper prefix of one. Anything before it is decided, so ordinary output (a prompt
    /// of a few bytes) is published at once and only a real partial token waits.
    fn carry(&self) -> usize {
        self.needles
            .iter()
            .flat_map(|n| (1..n.len()).rev().find(|&k| self.held.ends_with(&n[..k])))
            .max()
            .unwrap_or(0)
    }

    /// What may be published of `bytes` plus whatever was held, masked.
    fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.held.extend_from_slice(bytes);
        let publish = self.held.len().saturating_sub(self.carry());
        self.take(publish)
    }

    /// Nothing can arrive after this, so the hold goes out - masked, since a needle may sit in it
    /// whole and simply have been undecidable against what followed.
    fn finish(&mut self) -> Vec<u8> {
        let all = self.held.len();
        self.take(all)
    }

    fn take(&mut self, publish: usize) -> Vec<u8> {
        // Masked over the WHOLE hold before the prefix leaves: a needle that starts in the
        // published part and ends in the kept part is otherwise published with its head showing.
        for n in &self.needles {
            let mut i = 0;
            while i + n.len() <= self.held.len() {
                if &self.held[i..i + n.len()] == n.as_slice() {
                    self.held[i..i + n.len()].fill(b'*');
                    i += n.len();
                } else {
                    i += 1;
                }
            }
        }
        self.held.drain(..publish).collect()
    }
}

/// Where a run's output goes on living outside the window: the file, the byte of the job's
/// output its first byte is, and whether it is still growing. Unbounded on purpose, because a
/// run with no deadline may print for an hour. `path` is a `String` because every reader prints it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputFile {
    pub path: String,
    /// The absolute offset in this job's output of the file's first byte. Not always 0: a run
    /// promoted after outgrowing the window has lost its head for good, and the file honestly
    /// starts where the window did rather than at a zero no reader could check.
    pub from: u64,
    /// Bytes in the file at the moment this was read.
    pub bytes: u64,
    /// Why the file stopped growing, when a write failed - `None` while it is keeping up. A file
    /// that quietly stopped at 4 MiB while the job ran on would read as a job that stopped
    /// printing, which is the defect the denominator rules exist against.
    pub problem: Option<String>,
}

/// The open file behind an [`OutputFile`], kept with the ring so there is one writer for a
/// job's bytes: the drain thread that already owns them.
#[derive(Debug)]
struct Spill {
    path: std::path::PathBuf,
    /// `None` once nothing can write again - the drains have joined or the settled window was
    /// released. The file stays on disk and stays named; this process has no reason to hold a
    /// descriptor for a run that can never write again.
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
            masker: None,
            trailer: None,
        }
    }

    /// Take this job's own trailer out of everything it writes, from the next byte on. The
    /// spawn arms this when it appended one, with the mark it minted for this call.
    pub fn strip_trailer(&mut self, mark: &str) {
        self.trailer = Some(TrailerTaker::new(mark));
    }

    /// The report the trailer took: `Some(Ok(pairs))` once a whole trailer arrived, `Some(Err)`
    /// for one this build refuses to believe, `None` while there is neither.
    pub fn env_report(&self) -> Option<Result<String, String>> {
        self.trailer.as_ref().and_then(|t| t.report.clone())
    }

    /// Take these values out of everything this job writes, from the next byte on.
    pub fn mask(&mut self, needles: Vec<Vec<u8>>) {
        if !needles.is_empty() {
            self.masker = Some(Masker::new(needles));
        }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        // The trailer is taken out first, so the report's values never pass the masker and a
        // secret the shell exported is masked only where the card shows it, by `intent::env`.
        let arrived: Vec<u8> = match &mut self.trailer {
            Some(t) => t.feed(bytes),
            None => bytes.to_vec(),
        };
        // Counted on arrival and without the trailer: that is machinery this harness appended,
        // not output the command produced, and the denominator must agree with the window.
        self.produced += arrived.len() as u64;
        self.last_at = Some(SystemTime::now());
        match &mut self.masker {
            None => self.keep(&arrived),
            Some(masker) => {
                let published = masker.feed(&arrived);
                self.keep(&published);
            }
        }
    }

    /// The stream is over: what the masker held back goes out, and the window and the file have
    /// the whole of it. Called where the drains have joined, which is the last byte there is.
    pub fn flush_tail(&mut self) {
        // The trailer's hold first, and through the masker: those bytes were only ever held
        // back against a marker that never completed, and the masker must see every byte once.
        let held = self
            .trailer
            .as_mut()
            .map(|t| t.finish())
            .unwrap_or_default();
        if !held.is_empty() {
            match &mut self.masker {
                None => self.keep(&held),
                Some(masker) => {
                    let published = masker.feed(&held);
                    self.keep(&published);
                }
            }
        }
        let Some(masker) = &mut self.masker else {
            return;
        };
        let tail = masker.finish();
        self.keep(&tail);
    }

    fn keep(&mut self, bytes: &[u8]) {
        self.buf.extend(bytes.iter().copied());
        while self.buf.len() > self.cap {
            let over = self.buf.len() - self.cap;
            self.buf.drain(..over);
            self.dropped += over as u64;
        }
        // After the ring, and with the bytes the ring just dropped: the file is the copy that
        // keeps everything, so it must not inherit the window's bound.
        if let Some(spill) = &mut self.spill {
            spill.append(bytes);
        }
    }

    /// Start a file this job's output keeps going into, seeded with what the job has already
    /// printed: nothing at the spawn, the retained window at a promotion, so `from` is the
    /// window's own offset. Truncates, so a caller must not re-open a file already spilling.
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

    /// Keep appending to a file an earlier daemon spilled into, for a job adopted after a restart.
    /// The window starts empty at the file's end, so offsets stay those of the job's whole output.
    pub fn resume_spill(&mut self, path: impl Into<std::path::PathBuf>) -> Result<(), String> {
        let path = path.into();
        let file = std::fs::File::options()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let bytes = file
            .metadata()
            .map_err(|e| format!("{}: {e}", path.display()))?
            .len();
        self.produced = bytes;
        self.dropped = bytes;
        self.buf.clear();
        self.spill = Some(Spill {
            path,
            file: Some(file),
            from: 0,
            bytes,
            problem: None,
        });
        Ok(())
    }

    /// The last byte is in, so the descriptor goes and the file stays: called where the drains
    /// have joined, and from [`Capture::release`]. The path, offset and byte count stay,
    /// because every reader of a settled job is still owed the place.
    pub fn close_spill(&mut self) {
        self.flush_tail();
        if let Some(spill) = &mut self.spill {
            spill.file = None;
        }
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

    /// Does the retained window hold this text? One reader: the wrapper's own failure marker,
    /// which classifies a launcher failure (the exit code alone cannot, 125 being legitimate).
    /// A wrapper that could not join its cgroup ran no command, so the ring holds all it wrote.
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

    /// Let go of every retained byte, keeping the counts, so a slice afterwards says produced
    /// N, dropped N, holding 0 rather than reading as a job that wrote nothing. The allocation
    /// goes back too, and the spill's descriptor: a settled job can never write again.
    pub fn release(&mut self) {
        self.flush_tail();
        self.dropped += self.buf.len() as u64;
        self.buf = VecDeque::new();
        self.close_spill();
    }

    /// How long since the last byte, and `None` when there has never been one - a different
    /// fact from "a long time ago", and reported as one.
    pub fn since_last(&self) -> Option<Duration> {
        self.last_at.map(|t| t.elapsed().unwrap_or(Duration::ZERO))
    }

    /// A slice by absolute byte offset, absolute so an offset stays meaningful after the ring has
    /// moved under it - the difference between a paginator that works on a long-running job and
    /// one that silently re-reads the same window.
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
    /// Append the bytes the ring just took. A failure is recorded once and stops the writes:
    /// retrying a full disk every drain spends the run's time on a report nobody reads, and a
    /// hole that is said is the same rule the denominator follows.
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

/// A window onto a job's output, and everything needed to know what it is a window onto.
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

    /// The denominator, as one sentence, with every case it can be in: a job that has written
    /// nothing says exactly that, and does not look like a job whose output was dropped. `label`
    /// is what a reader is shown; the suggestion passes the id, because a label is not a handle.
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

/// Where a job's processes live and what reaps them. Changed by a promotion and by nothing
/// else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lifetime {
    /// The job's own cgroup.
    pub scope: super::scope::ScopeId,
    /// The scope that reaps it if nobody touches it.
    pub owner: super::scope::ScopeId,
    /// How it came to be in the background, once it is. `None` while it is a plain foreground
    /// command - a different fact from `Some(Backgrounding::Asked)`, and stored as one.
    pub background: Option<letibot_transcript::Backgrounding>,
}

/// A deadline, and the run it belongs to. The pair travels together: a deadline with no job is
/// a number, and a job with no deadline is one nobody can end. [`super::host::Deadlines`] is
/// the thread that reads these.
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
    /// The name the caller gave this work, or `None` when nobody said one: an id alone cannot
    /// tell which running job is the release build, and the name is the agent's stated intent,
    /// never a parse of a command line. Fixed for the job's life; `spawn` de-duplicates it.
    pub slug: Option<String>,
    /// This job's own cgroup, which `job_kill` ends, and the scope that owns its lifetime -
    /// the answer to "what does killing this touch" and to "when does this die if nobody does".
    /// Behind a lock because a promotion moves both, and a stale lifetime is a lie to the model.
    lifetime: Mutex<Lifetime>,
    pub cwd: String,
    pub started: SystemTime,
    /// The pid of the shell that became the command. Its descendants are in the same cgroup and
    /// are not tracked individually - that is the point of having a cgroup.
    pub pid: u32,
    pub state: Mutex<JobState>,
    pub finished: Condvar,
    pub capture: Mutex<Capture>,
    /// When [`Job::settle`] recorded the terminal state, or `None` while it runs. Stamped at the
    /// settle and not read afterwards: `elapsed` keeps counting after death, so a settlement
    /// reconstructed from a later clock is a guess with a decimal point.
    settled_at: Mutex<Option<SystemTime>>,
    /// The way in to this job's stdin, when it has one. See [`Stdin`]: `/dev/null` for every
    /// job but the operator's own run.
    stdin: Stdin,
    /// The memory cap this job runs under, decided at the spawn.
    memory_cap: MemoryCap,
    /// What the kernel counted for this job's memory, read once at the settle, before the scope
    /// holding the counters can be ended. `None` on a tree with no memory controller, and while
    /// the job runs.
    memory_used: Mutex<Option<super::scope::MemoryUse>>,
    /// `start:<n>` of the leader at spawn or adoption: what tells this process from a later one
    /// with its number. Empty when unread.
    identity: Mutex<String>,
    /// The raw output file and exit status of a job that can outlive this daemon. `None` for a
    /// job whose output comes through pipes, which dies with the daemon that reads them.
    follow: Option<Arc<super::detach::Follow>>,
}

/// The cap a job was started under, as three different facts: nobody asked, capped, and asked
/// and could not be capped each need a different sentence, and the third must never read like
/// the second.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryCap {
    /// No cap was asked for - the operator's own run, or a host whose caller passed none.
    NotAsked,
    /// The kernel holds this many bytes as the job's `memory.max`.
    Capped(u64),
    /// A cap was asked for and could not be set; the job runs UNCAPPED, and `why` says so.
    Uncapped { asked: u64, why: String },
}

impl Job {
    #[allow(clippy::too_many_arguments)] // one argument per SpawnRequest field the Job records
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
            identity: Mutex::new(String::new()),
            follow: None,
        }
    }

    /// The job, writing to a file of its own rather than to this daemon's pipes.
    pub fn with_follow(mut self, follow: Arc<super::detach::Follow>) -> Job {
        self.follow = Some(follow);
        self
    }

    pub fn follow(&self) -> Option<&Arc<super::detach::Follow>> {
        self.follow.as_ref()
    }

    pub fn set_identity(&self, identity: String) {
        *self.identity.lock().expect("job identity") = identity;
    }

    pub fn identity(&self) -> String {
        self.identity.lock().expect("job identity").clone()
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

    /// The secrets this job's output must never carry, taken out where the bytes arrive so the
    /// window and the spill file are one set of masked bytes rather than two renderings.
    pub fn with_secret_mask(self, secrets: &super::seat::SecretEnv) -> Job {
        // A value shorter than eight bytes matches ordinary text too often, and a mask that eats
        // a path or a word is a reader that stops trusting the mask.
        let needles: Vec<Vec<u8>> = secrets
            .0
            .iter()
            .filter(|(_, v)| v.len() >= 8)
            .map(|(_, v)| v.as_bytes().to_vec())
            .collect();
        self.capture.lock().expect("capture").mask(needles);
        self
    }

    /// Arm the trailer this job's command was appended, so its output gives the trailer up
    /// where the bytes arrive. `mark` is the nonce the spawn minted for this one call, and
    /// `&self` because the spawn holds the job behind an `Arc` before any byte can arrive.
    pub fn arm_env_trailer(&self, mark: &str) {
        self.capture.lock().expect("capture").strip_trailer(mark);
    }

    /// The environment report this job's trailer took, for the harness to observe once the job
    /// has settled. `None` while there is none: a killed run and a call with no `bash` answer
    /// the same honest nothing.
    pub fn env_report(&self) -> Option<Result<String, String>> {
        self.capture.lock().expect("capture").env_report()
    }

    /// What the kernel counted for this job's memory - `None` until it settles, and on a tree
    /// with no memory controller.
    pub fn memory_used(&self) -> Option<super::scope::MemoryUse> {
        *self.memory_used.lock().expect("job memory")
    }

    /// Recorded by the waiter, once, just before [`Job::settle`].
    pub fn record_memory(&self, used: super::scope::MemoryUse) {
        *self.memory_used.lock().expect("job memory") = Some(used);
    }

    /// The way in to this job's stdin. Cloned out rather than lent because the reader is on
    /// another thread: the run's own thread is blocked inside the call that started this job, so
    /// the answer arrives from a thread that does not hold the `Arc<Job>`.
    pub fn stdin(&self) -> Stdin {
        self.stdin.clone()
    }

    /// The file this job's output is also going into, for a run with no deadline or one whose
    /// deadline was lifted. Read off the capture rather than kept beside it, so there is one
    /// record of where the bytes are and not two that can disagree.
    pub fn output_file(&self) -> Option<OutputFile> {
        self.capture.lock().expect("capture").output_file()
    }

    /// The last byte is in, so the file's descriptor goes: called by the waiter once both drains
    /// have joined, after which nothing can push a byte. See [`Capture::close_spill`].
    pub fn close_spill(&self) {
        self.capture.lock().expect("capture").close_spill();
    }

    pub fn lifetime(&self) -> Lifetime {
        self.lifetime.lock().expect("job lifetime").clone()
    }

    /// Record that this job's processes now live somewhere else. Called only after the migration
    /// is measured, never on intent: a lifetime updated before the move is the
    /// announced-but-not-done failure with a job id attached.
    pub fn relocate(&self, lifetime: Lifetime) {
        *self.lifetime.lock().expect("job lifetime") = lifetime;
    }
    /// Wait for the job to finish, or for the deadline. Returns the state as it stands after the
    /// wait, which is `Running` when the deadline won.
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

    /// Undo a settlement that a kill could not make true: the process is still there.
    pub fn reopen(&self) {
        *self.settled_at.lock().expect("job settled_at") = None;
        *self.state.lock().expect("job state") = JobState::Running;
    }

    /// When the job settled, or `None` while it is running.
    pub fn settled_at(&self) -> Option<SystemTime> {
        *self.settled_at.lock().expect("job settled_at")
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed().unwrap_or(Duration::ZERO)
    }

    /// How long the job ran, from its own stamps. `None` while it is running, and this is the
    /// number a settlement reports - not [`Job::elapsed`], which is a live clock and keeps
    /// running after the job does not.
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
    /// A prompt no secret begins with is published whole and at once, and a token split across two
    /// reads is still held until it can be decided.
    #[test]
    fn a_masker_holds_only_a_partial_secret_and_not_a_fixed_tail() {
        let mut m = Masker::new(vec![b"ghp_0123456789abcdef".to_vec()]);
        assert_eq!(m.feed(b"Continue? [Y/n]\n"), b"Continue? [Y/n]\n");
        assert_eq!(m.feed(b"token ghp_0123"), b"token ");
        let mut want = vec![b'*'; 20];
        want.push(b'!');
        assert_eq!(m.feed(b"456789abcdef!"), want);
    }

    /// A seeded counter never mints a handle at or below the seed, and a lower seed does not move
    /// it back. Process-wide, so this asserts bounds rather than exact values: other tests mint
    /// handles concurrently.
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

    /// A kill ends the work and not the output, and every reader of one says so: the state word is
    /// what a settlement keeps after the job is reaped, so `partial_of` reads that and nothing else.
    #[test]
    fn a_killed_job_says_its_output_is_partial_and_a_finished_one_does_not() {
        let killed = JobState::Killed {
            by: DEADLINE_KILL.to_string(),
        };
        assert_eq!(killed.partial(), Some(PARTIAL));
        assert!(PARTIAL.contains("its output is partial"), "{PARTIAL}");
        assert!(PARTIAL.contains("job_extend"), "{PARTIAL}");
        // The word a settlement carries round-trips to the same sentence.
        assert_eq!(partial_of(&killed.word()), Some(PARTIAL));
        let dead = JobState::Killed {
            by: DEAD_KILL.to_string(),
        };
        assert_eq!(dead.partial(), Some(PARTIAL));
        assert!(
            dead.word().contains("no output and no CPU"),
            "{} — the reason is the point, not a label",
            dead.word()
        );
        // Every way a job can finish is silent about partiality.
        for finished in [
            JobState::Exited { code: 0 },
            JobState::Exited { code: 1 },
            JobState::Signalled { signal: 9 },
            JobState::Dead,
            JobState::NotScoped,
        ] {
            assert_eq!(finished.partial(), None, "{} is a finish", finished.word());
            assert_eq!(partial_of(&finished.word()), None);
        }
    }

    /// The spill is the copy that is not bounded: the window drops from the front past its cap,
    /// the file must not. The seed is the retained window, so a job whose window had already
    /// dropped bytes gets a file that says which byte it starts at.
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

        // A window that had already dropped bytes makes an honest file, not a whole one.
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

        // Releasing the window closes the descriptor and keeps the file. A settled job can never
        // write again; the bytes and the path must outlive the handle.
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

    /// The token arrives in two reads, and neither the window nor the file may hold either half: a
    /// pipe splits on its own byte boundaries, so a slice-per-output mask leaves the tail behind.
    #[test]
    fn a_token_split_across_two_reads_is_masked_in_the_window_and_in_the_file() {
        let dir = std::env::temp_dir().join(format!("letibot-split-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a directory to spill into");
        let path = dir.join("job-j3.out");
        let token = "gho_fixtureFIXTUREfixture0000000000000000";
        let (head, tail) = token.split_at(12);

        let mut cap = Capture::new(4096);
        cap.mask(vec![token.as_bytes().to_vec()]);
        cap.spill_to(&path).expect("the file opens");
        cap.push(format!("a={head}").as_bytes());
        cap.push(format!("{tail}=b").as_bytes());
        cap.close_spill();

        let window = String::from_utf8(cap.slice(0, 4096).bytes).expect("ascii");
        assert!(
            !window.contains(head),
            "the window kept the token's head: {window}"
        );
        assert!(
            !window.contains(tail),
            "the window kept the token's tail: {window}"
        );
        assert!(
            window.contains("a=***"),
            "the mask is the token's own width: {window}"
        );
        assert!(
            window.ends_with("**=b"),
            "and the bytes after it are untouched: {window}"
        );

        let on_disk = std::fs::read_to_string(&path).expect("the file is there");
        assert!(
            !on_disk.contains(token),
            "the file holds the token: {on_disk}"
        );
        assert_eq!(
            on_disk.len(),
            window.len(),
            "masking keeps every offset: the file and the window are one length"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A hold that never gets its second read still comes out: the tail of a run is where a
    /// half-written token would otherwise sit for ever, and `close_spill` is the last byte.
    #[test]
    fn the_hold_goes_out_masked_when_nothing_more_arrives() {
        let token = "gho_fixtureFIXTUREfixture0000000000000000";
        let mut cap = Capture::new(4096);
        cap.mask(vec![token.as_bytes().to_vec()]);
        cap.push(token.as_bytes());
        assert!(
            !String::from_utf8_lossy(&cap.slice(0, 4096).bytes).contains(token),
            "not even while it is held"
        );
        cap.close_spill();
        let out = String::from_utf8_lossy(&cap.slice(0, 4096).bytes).to_string();
        assert_eq!(out.len(), token.len(), "every byte came out: {out}");
        assert!(!out.contains(token), "and masked: {out}");
    }
    /// A name is one line, capped, and blank words are no name at all: nothing is invented, a name
    /// is not a sentence (breaks are never doubled or dangling), and the bound is [`MAX_JOB_NAME`].
    /// `bash`'s tests pin that its `slug` reads this function and not a copy of it.
    #[test]
    fn a_job_name_is_one_capped_line_and_blank_words_are_no_name() {
        // Nobody named it: every spelling of no words is the same answer, and it is `None`
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
        // The cap, and it takes the HEAD: the words a caller stated are in the order they stated
        // them, so the front is the part that names the work.
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

    /// The name is the reader's half and the id is the caller's half, in one sentence. The label
    /// must not leak into the `job="..."` suggestion: a label is not a handle `pick_job` resolves,
    /// so a suggestion carrying one would send the caller to a refusal.
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
        // And a job nobody named is the id in both places, which is the label's own fallback
        // rather than a second rule.
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

    /// Every state is classified, and every word is pinned: `never_ran` must place each variant
    /// on the right side of the empty/never-ran line, and each word must be the literal a head's
    /// test maps to its sentences, so a reword here cannot silently re-split the two sides.
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

    /// A trailer split across reads is taken out whole: the marker held until its BEL, the
    /// command's own bytes published either side of it, and the report kept for the harness.
    #[test]
    fn a_trailer_split_across_reads_is_taken_out_and_its_report_kept() {
        let mut cap = Capture::new(1024);
        cap.strip_trailer("m1");
        let stream = format!(
            "before\n{}",
            "\x1b]1337;letibot;m1;3;/a;b/cwd\np=/w;q=/q;\x07"
        );
        let bytes = stream.as_bytes();
        // Three reads, none aligned with anything: 7, then one that ends inside the marker,
        // then the rest.
        cap.push(&bytes[..7]);
        cap.push(&bytes[7..20]);
        cap.push(&bytes[20..]);
        let window = String::from_utf8_lossy(&cap.slice(0, usize::MAX).bytes).into_owned();
        assert_eq!(window, "before\n", "the trailer stayed in: {window:?}");
        assert_eq!(
            cap.produced(),
            7,
            "the trailer is not output the command made"
        );
        assert_eq!(
            cap.env_report(),
            Some(Ok("p=/w;q=/q;".to_string())),
            "the pairs the trailer carried"
        );
    }

    /// Output that merely resembles the start of a marker is not lost: it was held against a
    /// trailer that never came, and the end of the stream publishes it.
    #[test]
    fn output_that_resembles_a_marker_goes_out_at_the_end() {
        let mut cap = Capture::new(1024);
        cap.strip_trailer("m2");
        cap.push(b"trailing \x1b]13");
        assert_eq!(
            cap.slice(0, usize::MAX).bytes.len(),
            9,
            "the partial marker is held"
        );
        cap.flush_tail();
        let window = String::from_utf8_lossy(&cap.slice(0, usize::MAX).bytes).into_owned();
        assert_eq!(window, "trailing \x1b]13", "held is not lost: {window:?}");
        assert!(cap.env_report().is_none(), "no trailer ever arrived");
    }

    /// A marker this build refuses to parse still comes out of the stream, and the report says
    /// why: a status that is not a number is not a value anybody should splice into a reading.
    #[test]
    fn a_marker_this_build_refuses_to_believe_is_stripped_and_named() {
        let mut cap = Capture::new(1024);
        cap.strip_trailer("m3");
        cap.push(b"out\x1b]1337;letibot;m3;zero;/cwd\np=/w;\x07after");
        let window = String::from_utf8_lossy(&cap.slice(0, usize::MAX).bytes).into_owned();
        assert_eq!(
            window, "outafter",
            "the refused marker stayed in: {window:?}"
        );
        let Some(Err(why)) = cap.env_report() else {
            panic!("a refused report, not none: {:?}", cap.env_report());
        };
        assert!(why.contains("not a number"), "{why}");
    }
}
