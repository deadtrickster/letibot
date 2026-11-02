//! A full-screen program on a pty the daemon owns, its bytes streamed both ways: the
//! mechanism behind `!term mc`, `!term nano`, `!term top`.
//!
//! The command line goes to `/bin/sh -c` whole, the child gets a new session and this pty as
//! its controlling terminal, and the pane lives in a session cgroup so the scope tree ends it.
//! [`TermSink`] is the only coupling to the head: raw bytes out, one ending in.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use super::Pty;
use super::scope::{Cgroup2, Reaping, ScopeId, ScopeTree, join_script};
use super::shell::set_size;

/// The shell a pane's command line is read by: the line is a line, not an argv.
const SHELL: &str = "/bin/sh";

/// The relay a confined pane's program runs under: the boundary mounts a fresh devpts, so
/// the host's pty has no name inside it and `tty` answers nothing. `-qefc` relays through a
/// pty opened inside the view, keeps the exit code and says nothing of its own.
const RELAY: &str = "/usr/bin/script";

/// The `TERM` a pane's program is given when the daemon has none, or has one that is empty:
/// a program on a pty without a usable `TERM` can neither colourise nor address the cursor.
/// The console's own value is read first, so this is only what is left to say.
const DEFAULT_TERM: &str = "xterm-256color";

/// Where a pane's program keeps its config, which is the operator's own `HOME`: a pane is
/// their program on their box. It cannot sit on [`super::console::INHERITED`], because an
/// inherited `HOME` beats the per-run one a run's own request states.
const PANE_HOME: &str = "HOME";

/// The operator's ssh agent, on the same argument as [`PANE_HOME`]: a pane's environment is
/// cleared, so without this a passphrase-protected key asks in a program whose only answer is
/// `!send`. It stays off the console's table so a confined run never inherits it.
const PANE_AGENT: &str = "SSH_AUTH_SOCK";

/// How many bytes one read from the master may carry. Not a cap on anything: a full read is
/// one [`TermSink::output`] call and the next read follows at once.
const READ_CHUNK: usize = 8192;

/// Why a pane did not start, or stopped being usable. Local rather than an
/// [`super::ExecError`], because nothing here is a tool call and no caller should have to
/// read variants about gates and adjudication.
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

/// Where a pane's bytes go, and how it says it is over. A trait rather than a channel so the
/// caller's own place is the only buffer between the pty and the reader. `output` must not
/// block: it runs on the reader thread, and a sink that waited would stop draining the pty.
pub trait TermSink: Send + Sync {
    /// Bytes the program wrote, raw: escapes, `\r`, partial UTF-8 and all.
    fn output(&self, bytes: &[u8]);
    /// The pane is over, and this is why. Called once, from the thread that read.
    fn ended(&self, reason: &str);
}

/// How to start a pane: everything a caller must decide, in one place.
#[derive(Clone)]
pub struct TermConfig {
    /// The command line, verbatim: a shell line, not an argv.
    pub command: String,
    /// The whole environment the program is given - [`TermSession::start`] clears everything
    /// else - and [`env_from`] is what a pane wants here.
    pub env: Vec<(String, String)>,
    /// Where the program starts. Nothing reads a cwd back out, because a screen program has
    /// no trailer to report one in.
    pub cwd: PathBuf,
    /// The pane's rectangle at the moment it opens, from the head that owns it.
    /// [`TermSession::resize`] moves it after that.
    pub cols: usize,
    pub rows: usize,
    /// The scope the pane lives in, and the tree that will end it. `None` is a pane nobody
    /// owns - a test's.
    pub scope: Option<ScopeId>,
    pub tree: Option<Arc<dyn ScopeTree>>,
    /// The boundary the program runs inside: the argv that goes in front of its shell, built
    /// for the session the pane belongs to. Empty is no boundary, which is what an unconfined
    /// session's own commands get.
    pub confine: Vec<String>,
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
            confine: Vec::new(),
        }
    }
}

/// The environment a pane's program is given: the console's variables and the keep-list a
/// program cannot work without, and nothing else. Separate from
/// [`super::console::env_from`], which forces `PAGER=cat` for a capture a pane may page in.
pub fn env_from(source: &[(String, String)]) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> =
        Vec::with_capacity(super::console::INHERITED.len() + super::confine::KEEP_ENV.len() + 2);
    for name in super::console::INHERITED
        .iter()
        .chain(super::confine::KEEP_ENV)
        .chain([PANE_HOME, PANE_AGENT].iter())
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

/// What to run, and with what. Pure, so the wrapper's shape is asserted without a process
/// starting. With a scope the pid we hold is a member of the cgroup and the shell `exec`s
/// into it, so a pane that could not join its scope is not run at all.
pub fn argv(cfg: &TermConfig) -> Result<(String, Vec<String>), TermError> {
    if cfg.command.trim().is_empty() {
        return Err(TermError::Config("the command is empty".to_string()));
    }
    // The boundary goes between the scope and the shell: the join writes `cgroup.procs` on the
    // host, before any namespace exists, and membership survives the crossing because a cgroup
    // is not a namespace.
    let mut line: Vec<String> = cfg.confine.clone();
    // A pty opened inside the boundary, so a program that keeps its terminal has one to name.
    // A job's argv takes the terminal away with `--new-session`, so it gets no relay.
    let relay = !cfg.confine.is_empty()
        && !cfg.confine.iter().any(|a| a == "--new-session")
        && std::path::Path::new(RELAY).is_file();
    if relay {
        line.extend([
            RELAY.to_string(),
            "-qefc".to_string(),
            cfg.command.clone(),
            "/dev/null".to_string(),
        ]);
    } else {
        line.extend([SHELL.to_string(), "-c".to_string(), cfg.command.clone()]);
    }
    match &cfg.scope {
        None => {
            let prog = line.remove(0);
            Ok((prog, line))
        }
        Some(scope) => Ok((
            SHELL.to_string(),
            [
                vec![
                    "-c".to_string(),
                    join_script().to_string(),
                    // `$0` for the wrapper, which is only ever a name in a message.
                    "letibot-term".to_string(),
                    Cgroup2::procs_path(scope).display().to_string(),
                    // `$2` is the join token the host uses as evidence it can observe. A pane's
                    // proof that it started is its own first byte, so the wrapper's write goes to
                    // the null device - named, so a reader of `host.rs` is not left looking for it.
                    "/dev/null".to_string(),
                ],
                line,
            ]
            .concat(),
        )),
    }
}

/// The pane's environment inside a boundary: [`env_from`]'s answer less the two names the
/// view decides. The view states its own `HOME` and `SSH_AUTH_SOCK` last, so passing the
/// operator's through would point the program at paths the view does not hold.
pub fn confined_env(env: &[(String, String)]) -> Vec<(String, String)> {
    env.iter()
        .filter(|(k, _)| k != PANE_HOME && k != PANE_AGENT)
        .cloned()
        .collect()
}

/// One program, one pty, one cgroup, one reader thread. The master is held twice - writes
/// here, reads from a clone - because a mutex over one `File` would put a keystroke behind a
/// read that is waiting for the program that is waiting for the key.
pub struct TermSession {
    /// The master end, for keystrokes and for `TIOCSWINSZ`.
    master: std::fs::File,
    /// The pid the scope wrapper `exec`ed into, so it is a member of the pane's cgroup.
    pid: i32,
    scope: Option<ScopeId>,
    tree: Option<Arc<dyn ScopeTree>>,
    /// Why this pane ended, when the operator is the one who ended it. Set before the kill
    /// and read by the reader thread when it composes the sentence, so the operator is told
    /// what they did and there is still one reporter.
    closing: Arc<Mutex<Option<String>>>,
    /// The program is over, set by the reader thread the moment it knows. An atomic rather
    /// than a mutex because every reader asks a yes/no question about a pane, which is no
    /// place to take a lock.
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
    /// Start the program and hand its bytes to `sink` as they arrive. Returns as soon as the
    /// child is spawned - a pane has no readiness probe, and a command that does not exist is
    /// a `sh` exiting 127 whose sentence belongs in the pane, not in the call.
    pub fn start(cfg: &TermConfig, sink: Arc<dyn TermSink>) -> Result<TermSession, TermError> {
        let (prog, args) = argv(cfg)?;
        let pty = Pty::open().map_err(|e| TermError::Start(format!("no pty: {e}")))?;
        // The size is set before the child exists, so the child is born at the pane's
        // rectangle. A child resized after the spawn gets a `SIGWINCH`, which a program that
        // traps it reads as a nudge.
        set_size(
            &pty.input()
                .map_err(|e| TermError::Start(format!("the pty master: {e}")))?,
            cfg.cols,
            cfg.rows,
        );

        let mut cmd = std::process::Command::new(&prog);
        cmd.args(&args);
        cmd.current_dir(&cfg.cwd);
        // Cleared, then stated: `cfg.env` is the whole of what the program is handed, so the
        // daemon's own variables and every token in them stop at this line.
        cmd.env_clear();
        for (k, v) in &cfg.env {
            cmd.env(k, v);
        }
        // A terminal on all three: the program reads the keys a person types at it.
        let (a, b, c) = (
            pty.stdio().map_err(|e| TermError::Start(e.to_string()))?,
            pty.stdio().map_err(|e| TermError::Start(e.to_string()))?,
            pty.stdio().map_err(|e| TermError::Start(e.to_string()))?,
        );
        cmd.stdin(a).stdout(b).stderr(c);

        // `setsid` and `TIOCSCTTY`. The mechanism lives in [`super::pty`] so this path and
        // the row path cannot drift into two copies.
        super::pty::controlling_terminal(&mut cmd, 0);

        let mut child = cmd
            .spawn()
            .map_err(|e| TermError::Start(format!("{prog}: {e}")))?;
        let pid = child.id() as i32;
        // The parent's own slave handles go now: a slave still open here is a master read
        // that never reports the child's exit.
        let master = pty.into_master();
        drop(cmd);

        let closing: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let ended = Arc::new(AtomicBool::new(false));
        let mut reader = master
            .try_clone()
            .map_err(|e| TermError::Start(format!("the pty master: {e}")))?;

        // One thread, both the reader and the waiter: the exit status is what says why the
        // pane ended, and a second reporter would race this one over the sentence.
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
                            // `EIO` is this platform's end-of-pty, not a failure: it is what a
                            // read on the master returns once the last slave is closed.
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
                    // The flag is stored before anyone is told, so a caller woken by the sink
                    // cannot find a session that still claims to be running.
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

    /// The program's pid, which is the pid the scope wrapper `exec`ed into, so it is a
    /// member of the pane's cgroup.
    pub fn pid(&self) -> i32 {
        self.pid
    }

    /// A keystroke, or a paste, or anything else the operator's terminal sent, verbatim: the
    /// program is what reads keys, and a head with a keymap in front of a terminal would eat
    /// them. The detach byte is intercepted before this, which is what makes it untrappable.
    pub fn input(&self, bytes: &[u8]) -> Result<(), TermError> {
        if self.closed {
            return Err(TermError::Gone("the pane was closed".to_string()));
        }
        let mut m = &self.master;
        m.write_all(bytes)
            .map_err(|e| TermError::Gone(format!("the pty would not take the keys: {e}")))
    }

    /// Move the pty's size. The kernel raises `SIGWINCH` for the foreground process group,
    /// so a program that redraws on a resize does.
    pub fn resize(&self, cols: usize, rows: usize) {
        set_size(&self.master, cols, rows);
    }

    /// End the pane. `why` is set before the kill so the reader thread's report carries it.
    /// With a scope this ends the cgroup and everything under it dies; without one the
    /// fallback is `SIGHUP` to the pane's process group, which is not the mechanism.
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
                // `setsid` made the child its group leader, so `-pid` is the group; the child
                // itself goes second, for a pane that somehow never got one.
                unsafe {
                    libc::kill(-self.pid, libc::SIGHUP);
                    libc::kill(self.pid, libc::SIGHUP);
                }
                None
            }
        }
    }

    /// Whether [`TermSession::close`] has been called. The reader thread outlives it - it is
    /// the thing that reports - so this is a fact about the session and not about the thread.
    pub fn closed(&self) -> bool {
        self.closed
    }

    /// The program is over, set by the reader thread before it reports the ending. The other
    /// half of [`TermSession::closed`]: a program that exits on its own ends the pane with
    /// nobody having closed it.
    pub fn ended(&self) -> bool {
        self.ended.load(Ordering::SeqCst)
    }

    /// Is there still a program in this pane - the question a caller asks before it refuses
    /// to open a second one. `closed` and `ended` are different facts and both mean no, and a
    /// caller reading only `closed` keeps the slot of a pane that is already over.
    pub fn live(&self) -> bool {
        !self.closed && !self.ended()
    }
}

impl Drop for TermSession {
    /// A pane nobody closed is still a pane that must not leak. The reader thread is not
    /// joined on purpose: it is blocked on a read that ends when the child does, so waiting
    /// would wait for the process this has just been asked to kill.
    fn drop(&mut self) {
        if !self.closed {
            self.close("the pane was dropped");
        }
    }
}

/// A sink that keeps everything, for a test. Not `#[cfg(test)]` because tests in other crates
/// want it, and a collector with a condvar is the only way to assert on a byte stream that
/// arrives on another thread.
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

    /// Wait until `needle` is in what the program wrote, or until `d` passes: `true` if it
    /// arrived. A poll for a fact, so a slow box waits longer rather than failing.
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

    /// How long a pane's bytes get before a test calls it a failure. Generous because every
    /// assertion here is on content and never on speed, so a starved reader thread must not
    /// read as a defect.
    const PATIENCE: Duration = Duration::from_secs(20);

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
            confine: Vec::new(),
        };
        let sink = Arc::new(Collected::new());
        let s = TermSession::start(&cfg, sink.clone()).expect("a pane on this box");
        (s, sink)
    }

    /// The property this module exists for: the program gets this pty as its controlling
    /// terminal, asked from inside the child as whether `/dev/tty` and fd 0 are the same
    /// device. The control is the same pty and shell without the two calls.
    #[test]
    #[cfg(target_os = "linux")] // the check reads `/proc/self/fd/0`
    fn the_program_gets_this_pty_as_its_controlling_terminal() {
        // `exec 9</dev/tty` is the question `nano` asks, and it fails with `ENXIO` where
        // there is no controlling terminal - so `no-ctty` is the control's answer alone.
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
        cmd.env_remove("SHELLOPTS").env_remove("BASH_ENV");
        cmd.arg("-c").arg(ASK);
        let (a, b, c) = (p.stdio().unwrap(), p.stdio().unwrap(), p.stdio().unwrap());
        cmd.stdin(a).stdout(b).stderr(c);
        let mut child = cmd.spawn().expect("sh starts");
        let master = p.into_master();
        // The `Command`'s own slave copies go now, or the master never reports the child's
        // exit and this read blocks for ever.
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
        // Two spellings of one answer: a redirection failure on `exec` is fatal for a
        // non-interactive shell, so it exits with its own sentence instead of reaching the
        // `|| echo no-ctty` branch.
        assert!(
            seen.contains("no-ctty") || seen.contains("cannot open /dev/tty"),
            "without the two calls the pty must NOT be the controlling terminal, saw {seen:?}"
        );
    }

    /// A keystroke reaches the program and what it writes comes back, asserted on a string the
    /// program composed rather than on the pty's own echo of our write.
    #[test]
    fn keys_reach_the_program_and_its_bytes_come_back() {
        let (s, sink) = pane("read x; printf 'GOT:%s\\n' \"$x\"");
        // Slack for the child to be up: `read` blocks until the line discipline hands the line
        // over, so a write that arrives first is buffered and not lost.
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

    /// A resize is a fact the program is told: `stty size` inside the pane reads the pty's own
    /// winsize, which is what `TIOCSWINSZ` changed and what raises `SIGWINCH`.
    #[test]
    fn a_resize_reaches_the_program() {
        // The kernel raises `WINCH` for the foreground process group when the pty's window
        // changes, and the shell re-reads its size on it.
        let (s, sink) = pane("trap 'stty size' WINCH; while :; do sleep 0.2; done");
        std::thread::sleep(Duration::from_millis(200));
        s.resize(101, 37);
        assert!(
            sink.wait_for("37 101", PATIENCE),
            "the pty must have taken the new size, saw {:?}",
            sink.text()
        );
    }

    /// A resize to the size the pty already has is not a nudge: `TIOCSWINSZ` compares the new
    /// winsize with the current one and returns before signalling, so a program that redraws on
    /// `SIGWINCH` gets nothing. The control, same program, is a size that really changed.
    #[test]
    fn a_same_size_resize_is_not_a_nudge() {
        let (s, sink) = pane("trap 'echo WINCH' WINCH; while :; do sleep 0.2; done");
        std::thread::sleep(Duration::from_millis(300));
        // The pty was opened at 80x24, `pane`'s own config, so this is the same size.
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

    /// The ending is the exit status, said once by the one thread that read: a program that
    /// exits on its own is not the operator closing it, and the sentence says which.
    #[test]
    fn the_ending_is_the_programs_own_exit_status() {
        let (_s, sink) = pane("exit 3");
        let why = sink.wait_ended(PATIENCE).expect("an ending");
        assert_eq!(why, "the program exited with 3");
        // And exactly once: a second report would overwrite the slot, so this asserts the
        // sentence did not change under a second writer.
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(sink.ended().as_deref(), Some("the program exited with 3"));
    }

    /// Closing kills the program and the operator is told they closed it. The program is
    /// proved up before liveness is asked about, because the pane being live and the program
    /// never having started are two different things a loaded box can produce.
    #[test]
    fn closing_kills_the_program_and_says_the_operator_closed_it() {
        let (mut s, sink) = pane("echo up; exec sleep 30");
        assert!(
            sink.wait_for("up", PATIENCE),
            "the pane's program must be running before the pane is asked whether it is live: {:?}",
            sink.text()
        );
        assert!(
            s.live(),
            "a program that is still running is a pane that is live"
        );
        // The sentence is the caller's - `harnessd` passes its own - so this is the plumbing
        // rather than the wording, and the literal is the real one.
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

    /// A program that exits on its own ends the pane, and the pane says so. The control is the
    /// last assertion: `closed` is still false, which is why asking only `closed` kept the slot
    /// of a pane that was already over.
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

    /// The command line is a shell line and the shell is `/bin/sh`: no splitting, no quoting,
    /// no inspection, which is what makes `FOO=1 mc`, `cd /tmp && mc` and a pipe into `less`
    /// work.
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

    /// With a scope the pid we hold is a member of the cgroup. Pure, so the wrapper's argument
    /// positions are asserted: `$1` is `cgroup.procs`, `$2` the token, then the program.
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

    /// `!term ssh` reaches the operator's own agent and a pane invents nothing: the variable is
    /// forwarded when the console has it and not invented when it does not, which is the rule
    /// an empty value keeps too.
    #[test]
    fn a_pane_gets_the_operators_agent_and_never_one_it_does_not_have() {
        let with_agent: Vec<(String, String)> = vec![
            ("TERM".into(), "xterm-256color".into()),
            ("SSH_AUTH_SOCK".into(), "/run/user/1000/keyring/ssh".into()),
        ];
        let pairs = env_from(&with_agent);
        assert!(
            pairs
                .iter()
                .any(|(k, v)| k == "SSH_AUTH_SOCK" && v == "/run/user/1000/keyring/ssh"),
            "the agent socket the operator is using must reach the pane: {pairs:?}"
        );
        let without: Vec<(String, String)> = vec![("TERM".into(), "xterm-256color".into())];
        let pairs = env_from(&without);
        assert!(
            !pairs.iter().any(|(k, _)| k == "SSH_AUTH_SOCK"),
            "a console with no agent must not be given one: {pairs:?}"
        );
        // An empty value is not a value, which is the rule every name here keeps.
        let empty: Vec<(String, String)> = vec![("SSH_AUTH_SOCK".into(), String::new())];
        assert!(
            !env_from(&empty).iter().any(|(k, _)| k == "SSH_AUTH_SOCK"),
            "`SSH_AUTH_SOCK=` says nothing, and stating it would tell ssh something false"
        );
    }

    /// A pane's environment is the console's variables plus the keep-list a program cannot
    /// work without, and not a pager forced to `cat`: a capture has nobody at the keyboard and
    /// a pane has somebody. The control is the capture's own builder in the same test.
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

    /// `TERM` is supplied when the daemon has none and when it has one that is empty: a
    /// program given `TERM=` cannot find its terminfo. The control is a `TERM` the console
    /// really has, which is passed through untouched.
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
        // And the terminfo lookup through a real program on a real pty: with the default the
        // pane's program resolves its terminal, and with `TERM=` it cannot.
        // Bound to a name, not `_`: a `TermSession` dropped at the end of the statement closes
        // the pane and kills the program before it has printed anything.
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

    /// The pane's program's environment is a decision, not an inheritance: a real program on a
    /// real pty prints its own environment, and the daemon's variables must not be in it. The
    /// other half is the pair `mc` needs, `TERM` for terminfo and `HOME` for its config.
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
            confine: Vec::new(),
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

    /// A command that does not exist is something the operator reads in the pane: the shell's
    /// own line arrives on the sink and the ending is the shell's exit status.
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

    /// A pane runs inside the session's boundary and keeps its terminal there: a file beside
    /// the project is absent, the project is writable, and `/dev/tty` still opens and still
    /// reports the pane's rectangle. The control is the job's argv, which takes it away.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_confined_pane_sees_only_its_view_and_keeps_its_terminal() {
        use super::super::confine::{Bwrap, ConfinePlan, Confinement};
        let root =
            std::env::temp_dir().join(format!("letibot-term-confine-{}", std::process::id()));
        let project = root.join("project");
        let beside = root.join("beside");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&beside).unwrap();
        std::fs::write(beside.join("secret"), "x").unwrap();
        let project = project.canonicalize().unwrap();
        let bwrap = match Bwrap::project(&project) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("SKIP: no usable boundary on this box: {e}");
                return;
            }
        };
        let env = vec![
            ("TERM".to_string(), "xterm-256color".to_string()),
            ("PATH".to_string(), "/usr/bin:/bin".to_string()),
        ];
        const ASK: &str = "if [ -e \"$1/secret\" ]; then echo seen; else echo absent; fi; \
                           touch ./made && echo wrote; \
                           if ( : </dev/tty ) 2>/dev/null; then echo \"tty=$(stty size </dev/tty)\"; \
                           else echo no-ctty; fi; echo \"name=$(tty)\"; \
                           if script -qec true /dev/null >/dev/null 2>&1; then echo nested-ok; \
                           else echo nested-refused; fi; echo done";
        let run = |wrap: Vec<String>| {
            let cfg = TermConfig {
                command: format!("set -- {}; {ASK}", beside.display()),
                env: env.clone(),
                cwd: project.clone(),
                cols: 77,
                rows: 21,
                confine: wrap,
                ..TermConfig::default()
            };
            let sink = Arc::new(Collected::new());
            let s = TermSession::start(&cfg, sink.clone()).expect("a pane on this box");
            assert!(sink.wait_for("done", PATIENCE), "{:?}", sink.text());
            drop(s);
            sink.text()
        };
        let plan = ConfinePlan {
            cwd: &project,
            env: &env,
        };
        let pane = run(bwrap.wrap_terminal(&plan).expect("a pane's argv"));
        assert!(
            pane.contains("absent"),
            "the view holds only the project: {pane:?}"
        );
        assert!(pane.contains("wrote"), "the project is writable: {pane:?}");
        assert!(
            pane.contains("tty=21 77"),
            "the pty is the program's controlling terminal inside the boundary: {pane:?}"
        );
        // `tty` names the terminal because the program's pty is one the boundary's own devpts
        // holds ([`RELAY`]), so `ttyname` finds it.
        if std::path::Path::new(RELAY).is_file() {
            assert!(
                pane.contains("name=/dev/pts/") && !pane.contains("not a tty"),
                "the pane's terminal has a name inside the boundary: {pane:?}"
            );
            assert!(
                pane.contains("nested-ok"),
                "a program in the pane can still open a terminal of its own: {pane:?}"
            );
        }
        let job = run(bwrap.wrap(&plan).expect("a job's argv"));
        assert!(
            job.contains("no-ctty"),
            "the control: a job's argv takes the terminal away: {job:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
