//! **A program that owns the screen, on a pty the daemon owns, streamed as raw bytes** —
//! the mechanism behind `!term mc`, `!term nano`, `!term top`.
//!
//! # The defect, and the two things this module is between
//!
//! [`super::terminal`] refuses `nano`, `mc`, `top` and `less` by name on the `!` line, and
//! says why: the operator's run *does* get a pty (so `ls --color=auto` colourises), but the
//! pty is a **capture** — its output is folded into one transcript row and its input is
//! `/dev/null` — so a program that draws a screen draws cursor-addressing escapes into that
//! row and waits for a keystroke that cannot arrive. Its own doc names the fix: *"a `!term`
//! verb and a head that is a terminal emulator."*
//!
//! [`super::shell`] is the other neighbour, and the difference is the whole of this module.
//! That one frames a **line**: it appends a marked trailer and reads until the trailer
//! arrives, so one `run` returns one [`super::shell::Turn`] with a status and a cwd. That
//! framing is exactly wrong here and `shell.rs` says so in its own TODOs — *"a full-screen
//! program never returns to the shell, so its trailer never arrives and `run` waits out its
//! deadline."* A screen program is not a line and has no exit status to report while it
//! runs. What it has is **a byte stream in both directions, for as long as it lives**, and
//! that is what this module is.
//!
//! | | [`super::shell`] | here |
//! |---|---|---|
//! | the far end | an interactive shell, one per session | the program the operator named, one per pane |
//! | what comes back | one `Turn` per line, at the trailer | every byte, as it is written |
//! | what goes in | a line, plus this module's own trailer line | the operator's keystrokes, verbatim |
//! | the end | the shell exits | the program exits, or the operator leaves |
//!
//! **It is not a screen.** Nothing here parses an escape, holds a cell or knows what a
//! rectangle is: the bytes go out as they arrived and `letibot_vt::Screen` is what
//! turns them into rows. That is the seam the pane's coupling is kept to — [`TermSink`] in
//! one direction and the screen's own `feed` in the other — so the two ends can be
//! changed independently.
//!
//! # A controlling terminal, and why this path wants the one the row path declined
//!
//! [`super::pty`] records the decision that an operator's `!` run gets a pty that is **not**
//! its controlling terminal: `setsid` plus `TIOCSCTTY` was rejected there because it makes
//! `/dev/tty` *openable*, and a program reached **indirectly** — git's editor, `gpg`'s
//! pinentry — would then wait for a keystroke that cannot arrive.
//!
//! **That hazard does not exist here.** In a pane, keystrokes arrive: the head forwards the
//! operator's keys down this pty. So the child is given a new session ([`libc::setsid`])
//! and this pty as its controlling terminal ([`libc::TIOCSCTTY`]) — which is also what
//! makes `/dev/tty` the right device for `nano` and `mc`, what makes job control work, and
//! what silences bash's two lines about job control that a capture's terminal produces on
//! every row today. `the_program_gets_this_pty_as_its_controlling_terminal` is the test, and
//! it asserts the two devices are the *same one* rather than that a controlling terminal
//! merely exists.
//!
//! # The command line is the shell's to read
//!
//! `!term mc /etc` is a **shell line**, not an argv. Splitting it here would be a second
//! grammar in front of the shell — the argument [`super::shell::ShellSession::run`] makes
//! for the lines it writes — and it would break `!term FOO=1 mc`, `!term cd /tmp && mc` and
//! `!term git log | less` on the first day. So the line goes to `/bin/sh -c`, and **that is
//! the only thing this module does with it**: no quoting, no rewriting, no inspection.
//!
//! `/bin/sh` and not [`super::console::shell`]'s `/bin/bash -ic`: the operator's rc is a
//! file full of aliases and prompts, and a pane's program is one the operator *named*. An
//! operator who wants their shell types `!term bash`, which is one word and is honest.
//!
//! # The environment
//!
//! **The pane's program is given the console's environment and nothing else** — the
//! decision this section is, and the one that was missing. Until now the environment was
//! whatever `TermSession::start`'s child *inherited*, which is the daemon's whole
//! environment: measured through the real driver on 2026-09-26, `!term env` inside the
//! pane printed `CARGO_MANIFEST_DIR`, `RUSTUP_TOOLCHAIN`, `LD_LIBRARY_PATH`,
//! `SUDO_ASKPASS`, `LETIBOT_SOCKET` and `LETIBOT_SESSION` — the harness's own variables,
//! including every token the daemon's environment carries, handed to a program the
//! operator runs. `TERM` and `HOME` were in there too, and **by accident**: a daemon
//! started without them gave a pane neither, and `mc` needs both (terminfo for the first,
//! `~/.config/mc` for the second) — a program that prints one line and exits, which is
//! the flash this whole defect is.
//!
//! So the environment is now **cleared and then stated**, exactly the shape
//! [`super::host`]'s spawn uses (R10 layer 1), and it is stated from two tables that
//! already exist plus one name a pane has to state for itself — nothing invented here:
//!
//! | group | pairs | why |
//! |---|---|---|
//! | the terminal the console is | [`super::console::INHERITED`] — `TERM`, `COLORTERM`, `LS_COLORS` | the row path's own table, so a console variable is stated once |
//! | what a program cannot work without | [`super::confine::KEEP_ENV`] — `PATH`, `TERM`, `LANG`, `LC_ALL`, `TZ` | a confined command's keep-list, because both answer *what must a program have to run at all* |
//! | where its config lives | [`PANE_HOME`] — `HOME` | named here and not on the console's table, because a run's `HOME` is a decision about *that run*: see [`super::console::INHERITED`] |
//!
//! The difference from [`super::console::env_from`] stays what it was, and it is the reason
//! there are two functions: that one **forces** `PAGER=cat`, which is right for a capture —
//! nobody can press a key, so a pager would hang — and wrong for a pane, where somebody can.
//! The pane's table does not name a pager at all, so a pane's `git log` execs its own
//! default (`less`), which is what a terminal is for. `TERM` is supplied when the daemon has
//! none **or has one that is empty** ([`super::console::value`]: an empty value is not a
//! value), because a program on a pty with no `TERM` cannot colourise or address the cursor
//! and one with `TERM=` cannot find its terminfo.
//!
//! **What the pane's program therefore does not get, said rather than discovered.**
//! `SSH_AUTH_SOCK` (so `!term ssh` asks for a passphrase in the pane rather than using the
//! operator's agent — a grant is what that would be, and `confine`'s
//! `Grant::AgentSocket` is where it lives), `XDG_*`, and every token in the daemon's
//! environment. Each is a *variable a program may want* and not a hole: the answer to any
//! of them is a pair on one of the two tables, which is a decision somebody makes, where
//! inheriting the daemon's environment was one nobody made.
//!
//! # The lifetime is the cgroup's, and the vocabulary is [`super::scope`]'s
//!
//! A program that outlives the pane that drew it is a leak, and this tree already has the
//! answer rather than needing a second one: the command is started **inside a
//! [`ScopeKind::Session`] cgroup** through [`super::scope::join_script`] — the wrapper writes
//! its own pid into `cgroup.procs` and *then* `exec`s, so there is no window in which the
//! program or any of its children could be forked outside the scope — and the pane ends by
//! [`ScopeTree::end`], which kills the tree and records what it killed. **Nothing here
//! invents a lifetime**: no pid file, no `pkill`, no signal to a remembered number. The one
//! fallback — a session opened with no scope at all, which is what a unit test does — is
//! named in [`TermSession::close`] and says what it is.
//!
//! # The end, and there is one reporter
//!
//! [`TermSession::start`] spawns **one thread**, and it does the reading and the waiting:
//! it reads the master until the far end goes, then `wait`s for the child, then calls
//! [`TermSink::ended`] once with the exit status in the reason. One reporter, so there is
//! no race between *the pty closed* and *the program exited* over which sentence a head
//! prints — and the sentence is the specific fact, from the process that knows it.
//!
//! **That thread is also what says the pane is over** ([`TermSession::ended`], and
//! [`TermSession::live`] which reads it). The flag is set *before* the sink is told, so a
//! caller that learns of the ending from the sink — which is every caller, and the daemon
//! is the one that matters — cannot look at the session afterwards and be told it is still
//! running. It was, and that is the ghost: a program that exits at once left
//! `TermSession::closed` false (nothing had *closed* it) and the daemon holding its slot,
//! so the next `!term` in that session was refused with *"a pane is already open"* about a
//! pane that had not existed for a minute.
//!
//! **What it will not do.** A program that closes its own descriptors and keeps running
//! leaves the master readable-for-ever, so the read does not end and the pane does not
//! report; the operator's way out — `Ctrl-\`, intercepted by the head before it reaches
//! this pty, and documented there — still kills it through the scope. A program that forks a
//! daemon away from the pty is reported as ended the moment the *pane's* program exits, which
//! is the honest answer to *is this pane still live*, and the daemon it left behind is the
//! cgroup's business and not this module's.
//!
//! # Provenance
//!
//! Nothing here is taken from grok-build or opencode, and no terminal emulator was read.
//! The lifetime mechanism is [`super::scope`]'s and the pty is [`super::pty`]'s; both are
//! reused rather than copied, which is why this file is mostly a reader thread and a
//! comment.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use super::Pty;
use super::scope::{Cgroup2, Reaping, ScopeId, ScopeTree, join_script};
use super::shell::set_size;

/// **The shell a pane's command line is read by.** See the module header: the line is a
/// line and the shell is the thing that reads it.
const SHELL: &str = "/bin/sh";

/// **The `TERM` a pane's program is given when the daemon has none — or has one that is
/// empty.**
///
/// Not a policy about the operator's terminal — [`super::console::INHERITED`] is read first,
/// so a daemon started from their console passes their own value through. This is what is
/// left when there is nothing to pass: a program on a pty with no `TERM` cannot colourise,
/// cannot address the cursor, and is exactly the *"nano draws nothing"* failure this whole
/// module exists to remove. *Nothing to pass* includes `TERM=`, because an empty value is
/// not a value — see [`super::console::value`], and the flash it caused.
const DEFAULT_TERM: &str = "xterm-256color";

/// **Where a pane's program keeps its config** — the one name a pane's environment has that
/// neither of the two tables it reads carries.
///
/// **Not on [`super::console::INHERITED`], and that is a measurement rather than a taste.**
/// A run's `HOME` is a decision about *that run*: the operator's row path is handed one on
/// the daemon's standing environment, a confined run is handed the view's tmpfs home by
/// [`super::confine`], and a pair on the request's own environment is applied **last** — so
/// an inherited `HOME` beats both. The first was measured the day this was written
/// (`crates/tools/tests/exec.rs`'s rc test stopped reading the rc the test wrote) and the
/// second is the same mechanism with a boundary on it.
///
/// **A pane is neither of those.** It is the operator's program, on the operator's own box,
/// with no view and no standing environment — so the console's home is the honest one, and
/// it is the one `mc` writes `~/.config/mc` into and `git` reads `~/.gitconfig` from. A
/// program that exits because it cannot find either is the flash this strand is about.
const PANE_HOME: &str = "HOME";

/// How many bytes one read from the master may carry. The same order as
/// [`super::shell`]'s, and not a cap on anything: a read that fills it is one call to
/// [`TermSink::output`] and the next read follows immediately.
const READ_CHUNK: usize = 8192;

/// Why a pane did not start, or stopped being usable. Small and local on purpose: this is
/// not an [`super::ExecError`], because nothing here is a tool call and a caller should not
/// have to read a variant that talks about gates and adjudication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TermError {
    /// The program could not be started, or the scope could not be joined.
    Start(String),
    /// The pty's far end is gone, so keystrokes have nowhere to go.
    Gone(String),
    /// The caller asked for something a pane cannot do.
    Config(String),
}

impl std::fmt::Display for TermError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TermError::Start(w) => write!(f, "the terminal could not be started: {w}"),
            TermError::Gone(w) => write!(f, "the terminal is gone: {w}"),
            TermError::Config(w) => write!(f, "{w}"),
        }
    }
}

impl std::error::Error for TermError {}

/// **Where a pane's bytes go, and how it says it is over.**
///
/// The two methods are the whole coupling between this module and the head: bytes out as
/// they arrive, and one ending. A trait rather than a channel because the caller already has
/// the place they belong — the daemon's hub, a test's `Vec` — and a second buffer between
/// the pty and that place would be a second place for bytes to be lost.
///
/// **`output` must not block.** It is called from the pane's reader thread, and a sink that
/// waited on a socket write would stop reading the pty — which is the pty's own buffer
/// filling up, and then the program blocking in `write`. The daemon's implementation hands
/// the bytes to a queue that is never allowed to wait.
pub trait TermSink: Send + Sync {
    /// Bytes the program wrote. **Raw**: escapes, `\r`, partial UTF-8 and all, because a
    /// screen is what consumes them.
    fn output(&self, bytes: &[u8]);
    /// **The pane is over, and this is why** — *"the program exited with 0"*, or the
    /// operator's own departure. Called exactly once, from the one thread that read.
    fn ended(&self, reason: &str);
}

/// **How to start a pane.** Everything a caller must decide, in one place.
#[derive(Clone)]
pub struct TermConfig {
    /// **The command line, verbatim.** A shell line: see the module header for why it is
    /// not split here.
    pub command: String,
    /// **The whole environment the program is given.** Not "pairs put in front of" one:
    /// [`TermSession::start`] clears the environment first, so this is all of it. See the
    /// module header for why the pane's environment is a decision rather than an
    /// inheritance, and [`env_from`] for what a pane wants here.
    pub env: Vec<(String, String)>,
    /// Where the program starts. A startup fact — nothing reads a cwd back out of a pane,
    /// because a screen program has no trailer to report one in.
    pub cwd: PathBuf,
    /// The pane's rectangle at the moment it opens, from the head that owns it.
    /// [`TermSession::resize`] moves it after that.
    pub cols: usize,
    pub rows: usize,
    /// **The scope the pane lives in**, and the tree that will end it. `None` is a pane
    /// nobody owns — a test's, and named as such by [`TermSession::close`].
    pub scope: Option<ScopeId>,
    pub tree: Option<Arc<dyn ScopeTree>>,
}

impl Default for TermConfig {
    fn default() -> TermConfig {
        TermConfig {
            command: String::new(),
            env: Vec::new(),
            cwd: PathBuf::from("."),
            cols: 80,
            rows: 24,
            scope: None,
            tree: None,
        }
    }
}

/// **The environment a pane's program is given**: the console's own variables and the
/// keep-list a program cannot work without — and **nothing else**.
///
/// A second function beside [`super::console::env_from`] rather than a flag on it, because
/// the two disagree about the pagers and the disagreement is the whole reason: that one
/// **forces** `PAGER=cat` because a capture has nobody at the keyboard, and a pane has
/// somebody. `!term git log` is allowed to page — it is a terminal, and that is what a
/// terminal is for.
///
/// The two tables it reads are [`super::console::INHERITED`] (the row path's own) and
/// [`super::confine::KEEP_ENV`] (a confined command's keep-list), plus [`PANE_HOME`] — which
/// is named here rather than on the console's table for the reason its own doc gives. **No
/// third table**, because the question *what environment does a program get* already has two
/// answers in this tree and a third would be the defect. `TERM` is on both, so the tables are
/// walked in order with the first pair for a name winning, and an empty value is no value at
/// all — see [`super::console::value`].
///
/// See the module header for what this deliberately leaves out and why.
pub fn env_from(source: &[(String, String)]) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> =
        Vec::with_capacity(super::console::INHERITED.len() + super::confine::KEEP_ENV.len() + 2);
    for name in super::console::INHERITED
        .iter()
        .chain(super::confine::KEEP_ENV)
        .chain([PANE_HOME].iter())
    {
        // `TERM` is on both tables; the first one to name it is the console's own.
        if pairs.iter().any(|(k, _)| k == name) {
            continue;
        }
        if let Some(value) = super::console::value(source, name) {
            pairs.push(((*name).to_string(), value.to_string()));
        }
    }
    if !pairs.iter().any(|(k, _)| k == "TERM") {
        pairs.push(("TERM".to_string(), DEFAULT_TERM.to_string()));
    }
    pairs
}

/// [`env_from`] over this process's own environment, which is the console's.
pub fn env() -> Vec<(String, String)> {
    env_from(&std::env::vars().collect::<Vec<_>>())
}

/// **What to run, and with what.** Pure, so the scope wrapper can be asserted without a
/// process starting — the same discipline [`super::shell::argv`] keeps.
///
/// With no scope this is `/bin/sh -c <line>`. With a scope it is `/bin/sh -c <join_script>
/// … /bin/sh -c <line>`, so that **the pid we hold is a member of the cgroup** and the shell
/// `exec`s into that same pid: a pane that could not join its scope is not run, which is
/// [`join_script`]'s own `exit 125` and its own sentence.
pub fn argv(cfg: &TermConfig) -> Result<(String, Vec<String>), TermError> {
    if cfg.command.trim().is_empty() {
        return Err(TermError::Config("the command is empty".to_string()));
    }
    match &cfg.scope {
        None => Ok((
            SHELL.to_string(),
            vec!["-c".to_string(), cfg.command.clone()],
        )),
        Some(scope) => Ok((
            SHELL.to_string(),
            vec![
                "-c".to_string(),
                join_script().to_string(),
                // `$0` for the wrapper, which is only ever a name in a message.
                "letibot-term".to_string(),
                Cgroup2::procs_path(scope).display().to_string(),
                // `$2` is the join token the host uses as *evidence* it can observe. This
                // module has no use for one — a pane's proof that it started is its own
                // first byte — so the wrapper's `: > "$2"` is pointed at the null device.
                // Named rather than silently passed, because a reader who knows `host.rs`
                // will look for a real file here.
                "/dev/null".to_string(),
                SHELL.to_string(),
                "-c".to_string(),
                cfg.command.clone(),
            ],
        )),
    }
}

/// **One program, one pty, one cgroup, one reader thread.**
///
/// The master is held twice on purpose: this struct writes keystrokes to it, and the reader
/// thread reads the program's bytes from a clone. Two descriptors on one pty is what a
/// terminal is — the alternative, a mutex over one `File`, would put a keystroke behind a
/// blocked read.
pub struct TermSession {
    /// The master end, for keystrokes and for `TIOCSWINSZ`.
    master: std::fs::File,
    /// The pid the scope wrapper `exec`ed into, so it is a member of the pane's cgroup.
    pid: i32,
    scope: Option<ScopeId>,
    tree: Option<Arc<dyn ScopeTree>>,
    /// **Why this pane ended, when the operator is the one who ended it.** Set by
    /// [`TermSession::close`] before it kills anything, and read by the reader thread when
    /// it composes the sentence for [`TermSink::ended`] — so the operator who confirmed
    /// `!term close` is told *"you closed the terminal"* rather than *"the program exited with
    /// 137"*, and there is still exactly one reporter.
    closing: Arc<Mutex<Option<String>>>,
    /// **The program is over**, set by the reader thread the moment it knows — see
    /// [`TermSession::live`]. An [`AtomicBool`] and not a `Mutex<bool>` because the reader
    /// sets it on a thread nobody waits for and every reader of it is asking a yes/no
    /// question about a pane, which is not a place to take a lock.
    ended: Arc<AtomicBool>,
    closed: bool,
}

impl std::fmt::Debug for TermSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TermSession")
            .field("pid", &self.pid)
            .field("scope", &self.scope)
            .field("closed", &self.closed)
            .field("live", &self.live())
            .finish()
    }
}

impl TermSession {
    /// **Start the program, and hand its bytes to `sink` as they arrive.**
    ///
    /// Returns as soon as the child is spawned: a pane has no readiness probe and needs
    /// none, because the first thing a screen program does is draw. A command that does not
    /// exist is a `sh` that exits 127, whose bytes and whose ending both arrive on the sink
    /// like any other program's — which is the right shape, because *"command not found"* is
    /// something the operator should read in the pane and not as a failed call.
    pub fn start(cfg: &TermConfig, sink: Arc<dyn TermSink>) -> Result<TermSession, TermError> {
        let (prog, args) = argv(cfg)?;
        let pty = Pty::open().map_err(|e| TermError::Start(format!("no pty: {e}")))?;

        let mut cmd = std::process::Command::new(&prog);
        cmd.args(&args);
        cmd.current_dir(&cfg.cwd);
        // **Cleared, then stated — the whole environment, and not an inheritance.** See
        // the module header: the child used to inherit the daemon's own environment, which
        // is the harness's variables and every token in them, and which gave a pane `TERM`
        // and `HOME` only where the daemon happened to have them. `cfg.env` is
        // [`env_from`]'s answer and it is the whole of what the program is handed.
        cmd.env_clear();
        for (k, v) in &cfg.env {
            cmd.env(k, v);
        }
        // **A terminal on all three**, which is the difference between this and the row
        // path: the program reads the keys a person types at it.
        let (a, b, c) = (
            pty.stdio().map_err(|e| TermError::Start(e.to_string()))?,
            pty.stdio().map_err(|e| TermError::Start(e.to_string()))?,
            pty.stdio().map_err(|e| TermError::Start(e.to_string()))?,
        );
        cmd.stdin(a).stdout(b).stderr(c);

        // **`setsid` and `TIOCSCTTY`, and here rather than in `pty.rs` on purpose.** The row
        // path declines both (see the module header); a pane wants both. Failure is not
        // fatal: a program without a controlling terminal still runs on this pty, it just
        // cannot open `/dev/tty` — which is a program that draws and does not answer, so the
        // test that says it works is `the_program_gets_this_pty_as_its_controlling_terminal`.
        unsafe {
            use std::os::unix::process::CommandExt;
            cmd.pre_exec(|| {
                if libc::setsid() < 0 {
                    // Already a group leader, which a forked child is not — so this is a
                    // failure we do not understand, and the program still runs.
                }
                let _ = libc::ioctl(0, libc::TIOCSCTTY, 0);
                Ok(())
            });
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| TermError::Start(format!("{prog}: {e}")))?;
        let pid = child.id() as i32;
        // **The parent's own slave handles go now.** A slave this process still holds open is
        // a master read that never reports the child's exit — `pty.rs`'s own test measures
        // thirty seconds of that.
        let master = pty.into_master();
        drop(cmd);

        // **The size before the first byte**, so a program that asks on startup — which is
        // every full-screen program — gets the pane's rectangle rather than the default.
        set_size(&master, cfg.cols, cfg.rows);

        let closing: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let ended = Arc::new(AtomicBool::new(false));
        let mut reader = master
            .try_clone()
            .map_err(|e| TermError::Start(format!("the pty master: {e}")))?;

        // **One thread, and it is both the reader and the waiter.** See the module header:
        // the reason a pane ended is the exit status, the process that knows it is the child
        // this thread owns, and a second thread reporting the pty's end would race this one
        // over which sentence a head prints.
        {
            let sink = Arc::clone(&sink);
            let closing = Arc::clone(&closing);
            let ended = Arc::clone(&ended);
            let thread = std::thread::Builder::new()
                .name("term-pane".into())
                .spawn(move || {
                    let mut buf = [0u8; READ_CHUNK];
                    loop {
                        match reader.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => sink.output(&buf[..n]),
                            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                            // **`EIO` is this platform's end-of-pty**, not a failure: it is
                            // what a read on the master returns once the last slave is
                            // closed. `pty.rs`'s own drain treats it the same way.
                            Err(_) => break,
                        }
                    }
                    let code = child.wait().ok().and_then(|s| s.code());
                    let why = match closing.lock().ok().and_then(|c| c.clone()) {
                        Some(said) => said,
                        None => match code {
                            Some(0) => "the program exited".to_string(),
                            Some(c) => format!("the program exited with {c}"),
                            None => "the program was killed by a signal".to_string(),
                        },
                    };
                    // **The pane is over before anyone is told**, so a caller woken by the
                    // sink cannot find a session that still claims to be running — see
                    // [`TermSession::live`] and the module header's *the ghost*.
                    ended.store(true, Ordering::SeqCst);
                    sink.ended(&why);
                });
            if let Err(e) = thread {
                return Err(TermError::Start(format!("no thread for the pane: {e}")));
            }
        }

        Ok(TermSession {
            master,
            pid,
            scope: cfg.scope.clone(),
            tree: cfg.tree.clone(),
            closing,
            ended,
            closed: false,
        })
    }

    /// **The program's pid** — which is the pid the scope wrapper `exec`ed into, so it is a
    /// member of the pane's cgroup.
    pub fn pid(&self) -> i32 {
        self.pid
    }

    /// **A keystroke, or a paste, or anything else the operator's terminal sent.**
    ///
    /// Verbatim: this module does not decode, translate or filter, because the program is
    /// the thing that reads keys and a head that understood a key the program did not get
    /// would be a head with a keymap in front of a terminal. **The way out is intercepted
    /// before this is called** — see the head's own module — which is what makes it
    /// untrappable: a byte that never arrives cannot be caught.
    pub fn input(&self, bytes: &[u8]) -> Result<(), TermError> {
        if self.closed {
            return Err(TermError::Gone("the pane was closed".to_string()));
        }
        let mut m = &self.master;
        m.write_all(bytes)
            .map_err(|e| TermError::Gone(format!("the pty would not take the keys: {e}")))
    }

    /// **Move the pty's size.** The kernel raises `SIGWINCH` for the pty's foreground
    /// process group, so a program that redraws on a resize does — this is the half of
    /// *"the conversation's rectangle given to the program"* that a screen alone cannot do.
    pub fn resize(&self, cols: usize, rows: usize) {
        set_size(&self.master, cols, rows);
    }

    /// **End the pane.**
    ///
    /// `why` is the sentence the operator is told, and it is set *before* the kill so the
    /// reader thread's report carries it: the operator who confirmed `!term close` is told what
    /// they did, and the operator whose program exited on its own is told what it said.
    ///
    /// With a scope this is [`ScopeTree::end`]: the cgroup is killed, **everything under it
    /// dies with it**, and the record carries what was observed before and what survived
    /// after. Without one — a unit test, or a box where cgroups are unavailable — the
    /// fallback is `SIGHUP` to the pane's process group, which reaches the program and not
    /// what it daemonised away. **The fallback is not the mechanism** and says so.
    pub fn close(&mut self, why: &str) -> Option<Reaping> {
        if self.closed {
            return None;
        }
        self.closed = true;
        if let Ok(mut c) = self.closing.lock() {
            *c = Some(why.to_string());
        }
        match (&self.tree, &self.scope) {
            (Some(tree), Some(scope)) => Some(tree.end(scope)),
            _ => {
                // The pane's own process group first — `setsid` made the child its leader,
                // so `-pid` is the group — and the child itself second, for a pane that
                // somehow never got one.
                unsafe {
                    libc::kill(-self.pid, libc::SIGHUP);
                    libc::kill(self.pid, libc::SIGHUP);
                }
                None
            }
        }
    }

    /// Whether [`TermSession::close`] has been called. The reader thread outlives it — it
    /// is the thing that reports — so this is a fact about the session and not about the
    /// thread.
    pub fn closed(&self) -> bool {
        self.closed
    }

    /// **The program is over**, set by the reader thread before it reports the ending.
    ///
    /// The other half of [`TermSession::closed`], and the half that was missing: `closed`
    /// is about *somebody ending this pane*, and a program that exits on its own ends the
    /// pane without anybody doing anything. A caller asking *is there a program in this
    /// pane* wants both — see [`TermSession::live`].
    pub fn ended(&self) -> bool {
        self.ended.load(Ordering::SeqCst)
    }

    /// **Is there still a program in this pane?** — the question a caller asks before it
    /// refuses to open a second one.
    ///
    /// `closed || ended`, and the two are different facts that are both *no*: a pane the
    /// operator closed, and a pane whose program finished. Before this, the daemon asked
    /// `!closed` and a program that exited instantly left a **ghost** — the slot held by a
    /// pane that was over, so the next `!term` in that session was refused with *"a pane is
    /// already open"* and there was nothing on the screen to leave.
    pub fn live(&self) -> bool {
        !self.closed && !self.ended()
    }
}

impl Drop for TermSession {
    /// **A pane nobody closed is still a pane that must not leak.** The reader thread is
    /// deliberately not joined: it is blocked on a master read that ends when the child
    /// does, and a `drop` that waited for it would wait for the very process it has just
    /// been asked to kill.
    fn drop(&mut self) {
        if !self.closed {
            self.close("the pane was dropped");
        }
    }
}

/// **A sink that keeps everything, for a test.** Not `#[cfg(test)]` because the daemon's
/// own tests in other crates want it too, and because a collector with a `Condvar` in it is
/// the only honest way to assert on a byte stream that arrives on another thread.
#[derive(Default)]
pub struct Collected {
    bytes: Mutex<Vec<u8>>,
    ended: Mutex<Option<String>>,
    cv: std::sync::Condvar,
}

impl Collected {
    pub fn new() -> Collected {
        Collected::default()
    }

    /// Everything the program has written so far, as bytes.
    pub fn bytes(&self) -> Vec<u8> {
        self.bytes.lock().map(|b| b.clone()).unwrap_or_default()
    }

    /// Everything the program has written so far, lossily, for a readable assertion.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes()).into_owned()
    }

    /// Wait until `needle` is in what the program wrote, or until `d` passes. `true` if it
    /// arrived — a poll for a *fact*, so a slow box is a longer wait and not a failure.
    pub fn wait_for(&self, needle: &str, d: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + d;
        loop {
            if self.text().contains(needle) {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// Wait for the ending, or until `d` passes.
    pub fn wait_ended(&self, d: std::time::Duration) -> Option<String> {
        let guard = self.ended.lock().ok()?;
        if guard.is_some() {
            return guard.clone();
        }
        let (guard, _) = self.cv.wait_timeout_while(guard, d, |e| e.is_none()).ok()?;
        guard.clone()
    }

    /// The ending, if it has happened.
    pub fn ended(&self) -> Option<String> {
        self.ended.lock().ok().and_then(|e| e.clone())
    }
}

impl TermSink for Collected {
    fn output(&self, bytes: &[u8]) {
        if let Ok(mut b) = self.bytes.lock() {
            b.extend_from_slice(bytes);
        }
    }

    fn ended(&self, reason: &str) {
        if let Ok(mut e) = self.ended.lock() {
            *e = Some(reason.to_string());
        }
        self.cv.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// How long a pane's bytes get before a test calls it a failure. Generous: this is a
    /// poll for a fact on a loaded box, and the assertions are on content and not on speed.
    const PATIENCE: Duration = Duration::from_secs(10);

    fn pane(command: &str) -> (TermSession, Arc<Collected>) {
        let cfg = TermConfig {
            command: command.to_string(),
            env: vec![
                ("TERM".to_string(), "xterm-256color".to_string()),
                ("PATH".to_string(), "/usr/bin:/bin".to_string()),
            ],
            cwd: std::env::temp_dir(),
            cols: 80,
            rows: 24,
            scope: None,
            tree: None,
        };
        let sink = Arc::new(Collected::new());
        let s = TermSession::start(&cfg, sink.clone()).expect("a pane on this box");
        (s, sink)
    }

    /// **The property this module exists for: a program that owns the screen gets the pty
    /// as its controlling terminal, and it is THIS one.**
    ///
    /// Not *"a controlling terminal exists"* — which a test run from an operator's terminal
    /// would satisfy without any of this working — but *"`/dev/tty` and fd 0 are the same
    /// device"*, asked from inside the child. That is the question `nano` asks when it opens
    /// `/dev/tty`, and the reason [`super::super::pty`]'s own doc says the row path declines
    /// `setsid`/`TIOCSCTTY`.
    ///
    /// **Two probes, and neither of the obvious ones works.** `tty < /dev/tty` prints
    /// `/dev/tty` — the name it was *given* — and `stat -L /proc/self/fd/9` follows the
    /// descriptor's own name to the `5:0` device node rather than to the pty behind it.
    /// Measured, both. What does answer is `ps -o tty= -p $$`, which reads the process's
    /// **controlling terminal** out of `/proc/<pid>/stat` rather than out of a descriptor,
    /// against `readlink /proc/self/fd/0`, which names the device the child is actually on.
    /// Two different questions, and *same device* is the only answer that means `TIOCSCTTY`
    /// worked.
    ///
    /// The control is the same pty and the same shell **without** the two calls, and it is
    /// in the same test so a green run cannot be a `stat` that answered `same` for another
    /// reason.
    #[test]
    fn the_program_gets_this_pty_as_its_controlling_terminal() {
        // `exec 9</dev/tty` is the question `nano` asks. Without a controlling terminal the
        // open fails with `ENXIO` — measured on this box — so `no-ctty` is the control's
        // answer and never the pane's.
        const ASK: &str = "exec 9</dev/tty 2>/dev/null || { echo no-ctty; exit 0; }; \
                           a=$(ps -o tty= -p $$ 2>/dev/null); \
                           b=$(readlink /proc/self/fd/0 2>/dev/null); \
                           if [ \"$a\" != \"?\" ] && [ -n \"$a\" ] && [ \"/dev/$a\" = \"$b\" ]; \
                           then echo same; else echo \"diff:a=$a:b=$b\"; fi";

        let (_s, sink) = pane(ASK);
        assert!(
            sink.wait_for("same", PATIENCE),
            "the program must get this pty as its controlling terminal, saw {:?}",
            sink.text()
        );

        // The control: the same pair, the same shell, no `setsid`, no `TIOCSCTTY`.
        let p = Pty::open().expect("a pty");
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.arg("-c").arg(ASK);
        let (a, b, c) = (p.stdio().unwrap(), p.stdio().unwrap(), p.stdio().unwrap());
        cmd.stdin(a).stdout(b).stderr(c);
        let mut child = cmd.spawn().expect("sh starts");
        let master = p.into_master();
        // **The `Command`'s own slave copies go now**, or the master never reports the
        // child's exit and this read blocks for ever — `pty.rs`'s own test measures exactly
        // that, and it is why `TermSession::start` drops its `Command` too.
        drop(cmd);
        let mut seen = Vec::new();
        let mut buf = [0u8; 512];
        {
            use std::io::Read;
            let mut m = &master;
            while let Ok(n) = m.read(&mut buf) {
                if n == 0 {
                    break;
                }
                seen.extend_from_slice(&buf[..n]);
            }
        }
        let _ = child.wait();
        let seen = String::from_utf8_lossy(&seen);
        // **Two spellings of the same answer, because `sh` chooses one of them.** A
        // redirection failure on `exec` is fatal for a non-interactive shell, so a `sh` that
        // cannot open `/dev/tty` exits with its own sentence instead of reaching the
        // `|| echo no-ctty` branch — and the sentence is the more useful half anyway.
        assert!(
            seen.contains("no-ctty") || seen.contains("cannot open /dev/tty"),
            "without the two calls the pty must NOT be the controlling terminal, saw {seen:?}"
        );
    }

    /// **A keystroke reaches the program, and what it writes comes back.** Both directions
    /// of the pane, asserted as bytes: the program echoes what it read, so the assertion is
    /// on a string the *program* composed and not on the pty's own echo of our write.
    #[test]
    fn keys_reach_the_program_and_its_bytes_come_back() {
        let (s, sink) = pane("read x; printf 'GOT:%s\\n' \"$x\"");
        // A little slack: the child has to be up before a line written to the pty is read
        // by it. `read` blocks until the line discipline hands the line over, so a write
        // that arrives first is buffered and not lost.
        std::thread::sleep(Duration::from_millis(150));
        s.input(b"hello\r").expect("the pty takes keys");
        assert!(
            sink.wait_for("GOT:hello", PATIENCE),
            "the program must have read the keys we wrote, saw {:?}",
            sink.text()
        );
        assert!(
            sink.wait_ended(PATIENCE).is_some(),
            "the program exited, so the pane must have reported an end"
        );
    }

    /// **A resize is a fact the program is told, not a fact this head keeps.** `stty size`
    /// inside the pane reads the pty's own `winsize`, so this asserts `TIOCSWINSZ` reached
    /// the device the program is on — which is also what raises its `SIGWINCH`.
    #[test]
    fn a_resize_reaches_the_program() {
        // The shell re-reads its size on every `WINCH`, which the kernel raises for the
        // foreground process group when the pty's window changes.
        let (s, sink) = pane("trap 'stty size' WINCH; while :; do sleep 0.2; done");
        std::thread::sleep(Duration::from_millis(200));
        s.resize(101, 37);
        assert!(
            sink.wait_for("37 101", PATIENCE),
            "the pty must have taken the new size, saw {:?}",
            sink.text()
        );
    }

    /// **A resize to the size the pty already has is not a nudge, and this is the measurement
    /// that says so.**
    ///
    /// It exists because *"a `TermResize` to its own size forces a redraw in every TUI"* is a
    /// sentence somebody will believe — it is the obvious way to make a pane that came back
    /// redraw itself — and it is **false on this platform**: `TIOCSWINSZ` compares the new
    /// `winsize` with the current one and returns before it signals, so a program that redraws
    /// on `SIGWINCH` gets nothing at all. A daemon that relied on it to prove an attach would be
    /// relying on a no-op, which is why the attach is proved by the daemon **replaying the
    /// screen it holds** (`letibot_harnessd`'s `term` module) and why this test is here: a
    /// future edit that replaces the replay with a nudge fails here rather than in a person's
    /// empty rectangle.
    ///
    /// **The control is in the same test**: the very same program, the very same trap, resized
    /// to a *different* size, prints — so a green run cannot be a `trap` that never fired.
    #[test]
    fn a_same_size_resize_is_not_a_nudge() {
        let (s, sink) = pane("trap 'echo WINCH' WINCH; while :; do sleep 0.2; done");
        std::thread::sleep(Duration::from_millis(300));
        // The pty was opened at 80×24 — `pane`'s own config — so this is the same size.
        s.resize(80, 24);
        assert!(
            !sink.wait_for("WINCH", Duration::from_millis(700)),
            "a `TIOCSWINSZ` that changes nothing must raise no SIGWINCH, and this one did: {:?}",
            sink.text()
        );
        // The control: a real change, on the same program, is a real signal.
        s.resize(81, 24);
        assert!(
            sink.wait_for("WINCH", PATIENCE),
            "a size that really changed must reach the program, or this test measures the trap \
             and not the ioctl: {:?}",
            sink.text()
        );
    }

    /// **The ending is the exit status, said once, by the one thread that read.** A program
    /// that exits on its own is not the operator closing it, and the sentence says which.
    #[test]
    fn the_ending_is_the_programs_own_exit_status() {
        let (_s, sink) = pane("exit 3");
        let why = sink.wait_ended(PATIENCE).expect("an ending");
        assert_eq!(why, "the program exited with 3");
        // And exactly once: the sink's slot is a single `Option`, so a second report would
        // overwrite it — this asserts the sentence did not change under a second writer.
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(sink.ended().as_deref(), Some("the program exited with 3"));
    }

    /// **Closing kills the program and the operator is told they closed it.**
    ///
    /// The program is `sleep 30`, so a close that did not kill would leave the ending
    /// unwritten for half a minute and this test would time out — the wait is the assertion,
    /// and the sentence is the other half.
    #[test]
    fn closing_kills_the_program_and_says_the_operator_closed_it() {
        let (mut s, sink) = pane("sleep 30");
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            s.live(),
            "a program that is still running is a pane that is live"
        );
        // The sentence is the CALLER's — `harnessd` passes `Terminals::CLOSED` — so this is the
        // plumbing rather than the wording. The literal is the real one, because a test that
        // proved this with a made-up string would keep passing through a rename.
        s.close("you closed the terminal");
        let why = sink.wait_ended(PATIENCE).expect("an ending");
        assert_eq!(why, "you closed the terminal");
        assert!(s.closed());
        assert!(
            !s.live(),
            "a pane the operator left is not live, whether or not the reader thread has \
             got round to saying so"
        );
    }

    /// **A program that exits on its own ends the pane, and the pane says so.**
    ///
    /// This is the ghost the operator hit: `!term mc` printed one line and exited, the pane
    /// was over, and `TermSession::closed` was still false — because nobody had *closed* it —
    /// so the daemon kept the slot and the next `!term` was refused with *"a pane is already
    /// open in this session"* about a pane that had been gone for a minute.
    ///
    /// **The control is the second assertion**: `closed` is false at the end of this test,
    /// which is what makes it evidence that the question the daemon asked was the wrong one.
    /// The ordering that matters for the daemon — the flag is stored *before* the sink is
    /// told, so a caller woken by the report cannot find a session still claiming to be live
    /// — is by construction in `start`, one line above the call, and the end-to-end half of it
    /// is `crates/harnessd/tests/term_pane_live.rs`'s
    /// `a_pane_whose_program_exited_frees_its_slot`.
    #[test]
    fn a_program_that_exits_on_its_own_leaves_no_ghost_of_a_pane() {
        let (s, sink) = pane("printf 'bye\\n'; exit 7");
        assert_eq!(
            sink.wait_ended(PATIENCE).as_deref(),
            Some("the program exited with 7"),
            "the ending is the status, and it is what the daemon reads"
        );
        assert!(
            sink.text().contains("bye"),
            "and its last bytes are the head's"
        );
        assert!(!s.live(), "a program that exited leaves no live pane");
        assert!(
            !s.closed(),
            "and nobody closed it — which is exactly why `closed` alone was the wrong \
             question to refuse a second pane on"
        );
    }

    /// **The command line is a shell line, and the shell is `/bin/sh`.**
    ///
    /// Pure, so the shape is asserted without a process: no splitting, no quoting, no
    /// inspection — which is what makes `!term FOO=1 mc`, `!term cd /tmp && mc` and
    /// `!term git log | less` work on the first day.
    #[test]
    fn the_command_line_is_handed_to_the_shell_whole() {
        let cfg = TermConfig {
            command: "FOO=1 mc /etc && echo done".to_string(),
            ..TermConfig::default()
        };
        let (prog, args) = argv(&cfg).expect("argv");
        assert_eq!(prog, "/bin/sh");
        assert_eq!(args, vec!["-c", "FOO=1 mc /etc && echo done"]);

        // And an empty one is refused by name rather than starting a shell that reads EOF.
        let empty = TermConfig::default();
        assert!(matches!(argv(&empty), Err(TermError::Config(_))));
    }

    /// **With a scope the pid we hold is a member of the cgroup.** Pure, so the wrapper's
    /// argument positions are asserted rather than discovered: `$1` is `cgroup.procs`, `$2`
    /// the evidence token, and everything after `shift 2` is the program.
    #[test]
    fn a_scoped_pane_joins_its_cgroup_before_it_execs() {
        use super::super::scope::ScopeKind;
        let scope = ScopeId {
            kind: ScopeKind::Session,
            name: "s1".to_string(),
            path: PathBuf::from("/sys/fs/cgroup/letibot/s1"),
        };
        let cfg = TermConfig {
            command: "mc".to_string(),
            scope: Some(scope.clone()),
            ..TermConfig::default()
        };
        let (prog, args) = argv(&cfg).expect("argv");
        assert_eq!(prog, "/bin/sh");
        assert_eq!(args[0], "-c");
        assert_eq!(args[1], join_script());
        assert_eq!(args[2], "letibot-term");
        assert_eq!(args[3], Cgroup2::procs_path(&scope).display().to_string());
        assert_eq!(args[4], "/dev/null");
        assert_eq!(&args[5..], &["/bin/sh", "-c", "mc"]);
    }

    /// **A pane's environment is the console's own variables plus the keep-list a program
    /// cannot work without — and NOT a pager forced to `cat`.**
    ///
    /// The contrast with [`super::super::console::env_from`] is one reason there are two
    /// functions: a capture has nobody at the keyboard so `git log` must not page, and a
    /// pane has somebody at the keyboard so it may. Asserted without a process, which is the
    /// only way to hold the whole of it rather than the one variable a test thought to print.
    #[test]
    fn a_panes_environment_keeps_the_pager_the_console_would_have_forbidden() {
        let source: Vec<(String, String)> = vec![
            ("TERM".into(), "screen-256color".into()),
            ("COLORTERM".into(), "truecolor".into()),
            ("PAGER".into(), "less".into()),
            ("HOME".into(), "/home/nobody".into()),
            ("PATH".into(), "/usr/bin".into()),
            ("LANG".into(), "en_GB.UTF-8".into()),
            ("CARGO_MANIFEST_DIR".into(), "/build".into()),
        ];
        assert_eq!(
            env_from(&source),
            vec![
                ("TERM".to_string(), "screen-256color".to_string()),
                ("COLORTERM".to_string(), "truecolor".to_string()),
                ("PATH".to_string(), "/usr/bin".to_string()),
                ("LANG".to_string(), "en_GB.UTF-8".to_string()),
                ("HOME".to_string(), "/home/nobody".to_string()),
            ],
            "the pane's environment is the console's terminal variables, the keep-list a \
             program cannot work without, and the console's HOME — and nothing else: no pager \
             forced, and nothing of the harness's own"
        );
        // The control, in the same test: the capture's own builder does force it.
        assert!(
            super::super::console::env_from(&source)
                .iter()
                .any(|(k, v)| k == "PAGER" && v == "cat"),
            "the capture's environment must still forbid paging, or this test measures \
             nothing about the difference"
        );
    }

    /// **`TERM` is supplied when the daemon has none — and when it has one that is empty.**
    ///
    /// The second half is the flash the operator hit: a daemon whose environment carries
    /// `TERM=` (set, and saying nothing) handed the pane a *terminal type of the empty
    /// string*, which every ncurses program answers by printing one line and exiting. `mc`
    /// is one of them; `nano` says `Error opening terminal: unknown.` and is the one this
    /// test can run.
    ///
    /// The control is the pair of cases around it: a `TERM` the console really has is passed
    /// through untouched, so this is not a rule about the default winning.
    #[test]
    fn a_pane_is_given_a_usable_term_even_when_the_daemon_has_an_empty_one() {
        assert_eq!(
            env_from(&[]),
            vec![("TERM".to_string(), DEFAULT_TERM.to_string())]
        );
        assert_eq!(
            env_from(&[("TERM".to_string(), String::new())]),
            vec![("TERM".to_string(), DEFAULT_TERM.to_string())],
            "`TERM=` is not a terminal type, and a pane given one cannot find its terminfo"
        );
        assert_eq!(
            env_from(&[("TERM".to_string(), "st-256color".to_string())]),
            vec![("TERM".to_string(), "st-256color".to_string())],
            "the console's own TERM is the console's, and is passed through"
        );
        // And the terminfo lookup this is about, through a real program on a real pty: with
        // the default the pane's program can resolve its terminal, and with `TERM=` it cannot.
        // **`_s` and not `_`**: a `TermSession` bound to a bare `_` is dropped at the end of
        // the statement, which closes the pane and kills the program before it has printed
        // anything — measured here as an empty sink and *"the pane was dropped"*.
        let (_s, sink) = pane("printf 'tput:%s\n' \"$(tput cols 2>&1)\"");
        assert!(
            sink.wait_for("tput:", PATIENCE),
            "the pane's program must have run: {:?}",
            sink.text()
        );
        let said = sink.text();
        assert!(
            !said.contains("unknown") && !said.contains("No such file"),
            "a pane with a usable TERM must be able to find its terminfo: {said:?}"
        );
    }

    /// **The pane's program's environment is a decision, not an inheritance.**
    ///
    /// This is the test the fix is for, and it is a real program on a real pty printing its
    /// own environment: `env()` is what the daemon hands a pane, and the daemon's own
    /// variables — `CARGO_MANIFEST_DIR` is this test process's, and is the shape of every
    /// variable the harness runs with — must not be in it. Measured before the fix, they
    /// were: the child inherited the daemon's whole environment, tokens and all.
    ///
    /// The other half is the pair the operator's `mc` needed: `TERM` for terminfo and `HOME`
    /// to write its config, both of which used to arrive only where the daemon happened to
    /// have them.
    #[test]
    fn the_panes_program_gets_the_console_and_nothing_of_the_harnesss_own() {
        let cfg = TermConfig {
            command: "printf 'TERM=[%s] HOME=[%s] PATH=[%s] MANIFEST=[%s]\n' \"$TERM\" \
                      \"$HOME\" \"$PATH\" \"$CARGO_MANIFEST_DIR\""
                .to_string(),
            env: env(),
            cwd: std::env::temp_dir(),
            cols: 80,
            rows: 24,
            scope: None,
            tree: None,
        };
        let sink = Arc::new(Collected::new());
        let _s = TermSession::start(&cfg, sink.clone()).expect("a pane on this box");
        assert!(
            sink.wait_for("MANIFEST=", PATIENCE),
            "the pane's program must have printed its environment: {:?}",
            sink.text()
        );
        let said = sink.text();
        let value = |name: &str| -> String {
            said.split_whitespace()
                .find_map(|w| w.strip_prefix(name))
                .unwrap_or("")
                .trim_matches(|c| c == '[' || c == ']')
                .to_string()
        };
        assert!(
            !value("TERM=").is_empty(),
            "a pane's program needs a TERM to colourise and address the cursor: {said:?}"
        );
        assert!(
            value("HOME=").starts_with('/'),
            "`mc` writes its config under HOME and exits when it has none: {said:?}"
        );
        assert!(
            !value("PATH=").is_empty(),
            "a pane runs a program by name, so it needs the console's PATH: {said:?}"
        );
        assert!(
            value("MANIFEST=").is_empty(),
            "the daemon's own environment must not reach a program the operator runs, and \
             this is the variable that says it did: {said:?}"
        );
    }

    /// **A command that does not exist is something the operator reads in the pane**, not a
    /// failed call: the shell's own `not found` line arrives on the sink like any program's
    /// output, and the ending is the shell's exit status.
    #[test]
    fn a_command_that_does_not_exist_says_so_in_the_pane() {
        let (_s, sink) = pane("definitely-not-a-program-xyz");
        assert!(
            sink.wait_for("not found", PATIENCE),
            "the shell's own sentence must reach the pane, saw {:?}",
            sink.text()
        );
        assert_eq!(
            sink.wait_ended(PATIENCE).as_deref(),
            Some("the program exited with 127")
        );
    }
}
