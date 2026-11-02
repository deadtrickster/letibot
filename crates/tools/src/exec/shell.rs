//! One long-lived shell per session, with a terminal, whose state is the shell's: a `!` line
//! is a line typed at that shell, not a `/bin/sh -c` for it, and the shell answers for the
//! cwd, the status and the environment - [`ShellSession::cwd`] never remembers one.
//!
//! A prompt is not a protocol: it is a string the operator's rc chooses, it can appear inside
//! a command's own output, and an interactive shell prints one before it reads each line. So
//! the framing is a sentinel - the line sent for `run("ls")` is the command and then this:
//! ```text
//!   ls
//!   __lt_end_<nonce>
//! ```
//!
//! and `__lt_end_<nonce>` is a shell function defined once at session start:
//!
//! ```text
//!   __lt_end_<nonce>() {
//!       printf '\033]1337;letibot;%s;%s;%s\007' "$LETIBOT_MARK" "$?" "$PWD"
//!   }
//! ```
//!
//! The model may propose a line but never run one: a line here is one a person typed.
//! A secret stays a person's card: the trailer's `$PWD` and `$?` are read back, and the
//! exported environment is never printed, never on the wire and never in `Debug`.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::Pty;
use super::scope::{Cgroup2, Reaping, ScopeId, ScopeTree, join_script};

/// The OSC introducer every trailer starts with. `OSC 1337` is the range terminals
/// reserve for private use, and the bytes are chosen because a screen consumes them whole
/// and a program cannot produce them by accident.
const MARK_OPEN: &str = "\x1b]1337;letibot;";

/// What ends a trailer — BEL, the short spelling of an OSC's terminator.
const MARK_CLOSE: u8 = 0x07;

/// How long a session waits for the shell to come up and answer its first trailer.
pub const READY: Duration = Duration::from_secs(10);

/// How long a line gets before the session is called desynchronised. The caller may override
/// it per line; this is what a `!` line uses, and it is the same order as the 300 s budget a
/// tool call gets because a build is a command a person typed.
pub const WAIT: Duration = Duration::from_secs(300);

/// How many bytes of shell output one turn may accumulate before it is called a runaway: a
/// bound on what this module holds while it scans for a trailer that may never come, not a
/// cap on what a command may print. Hitting it desynchronises the session.
const MAX_TURN: usize = 32 * 1024 * 1024;

/// Why a session did not answer. Small and local on purpose: this is not an
/// [`ExecError`], because nothing here is a tool call and a caller should not have to read a
/// variant that talks about commands and gates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellError {
    /// The shell is not there — it exited, or the pty's far end closed.
    Gone(String),
    /// The trailer did not arrive inside the deadline. The session is desynchronised: the
    /// trailer for this line is still coming, and the next `run` would read it and call it
    /// the next line's answer.
    Timeout(Duration),
    /// A previous timeout, or a runaway turn, left a trailer in flight.
    Desynchronised(String),
    /// The turn grew past [`MAX_TURN`] with no trailer in it.
    Runaway(usize),
    /// The session could not be started, or the scope could not be joined.
    Start(String),
    /// The caller asked for something a session cannot do.
    Config(String),
}

impl std::fmt::Display for ShellError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShellError::Gone(w) => write!(f, "the shell session is gone: {w}"),
            ShellError::Timeout(d) => write!(
                f,
                "the shell did not reach its trailer within {d:?}; the session is \
                 desynchronised and must be resynced or replaced"
            ),
            ShellError::Desynchronised(w) => write!(f, "the shell session is desynchronised: {w}"),
            ShellError::Runaway(n) => write!(
                f,
                "the shell wrote {n} bytes with no trailer in them; the session is \
                 desynchronised"
            ),
            ShellError::Start(w) => write!(f, "the shell session could not be started: {w}"),
            ShellError::Config(w) => write!(f, "{w}"),
        }
    }
}

impl std::error::Error for ShellError {}

/// How to start a session. Everything a caller must decide, in one place, with the
/// defaults a `!` line wants. `Clone` and not `Debug`: the scope tree is a trait object that
/// carries no `Debug`.
#[derive(Clone)]
pub struct ShellConfig {
    /// The shell and its flags. [`super::console::shell`] is the operator's — `/bin/bash -ic`
    /// — and the tests pass `--norc` so an assertion is about this module and not about
    /// whatever the operator's rc prints.
    pub shell: Vec<String>,
    /// Pairs put in front of the shell's environment. The daemon's own environment is the
    /// console's (`super::console`'s argument), so this is what a *session* adds — `TERM`
    /// above all, because a shell on a pty without one is a shell that cannot colourise.
    pub env: Vec<(String, String)>,
    /// Where the session starts. A startup fact, not a report — see the module header:
    /// once the shell is running, [`ShellSession::cwd`] is the answer and this is history.
    pub cwd: PathBuf,
    /// The pty's size, in columns and rows. The head's pane rectangle at the moment of
    /// opening; [`ShellSession::resize`] moves it after that.
    pub cols: usize,
    pub rows: usize,
    /// The scope the session lives in, and the tree that will end it. `None` is a session
    /// nobody owns — a test's, and named as such by [`ShellSession::close`].
    pub scope: Option<ScopeId>,
    pub tree: Option<Arc<dyn ScopeTree>>,
    /// How long the first trailer gets.
    pub ready: Duration,
}

impl Default for ShellConfig {
    fn default() -> ShellConfig {
        ShellConfig {
            shell: super::console::shell(),
            env: Vec::new(),
            cwd: PathBuf::from("."),
            cols: 80,
            rows: 24,
            scope: None,
            tree: None,
            ready: READY,
        }
    }
}

/// One line's answer, and the facts the shell is the authority for.
#[derive(Clone, PartialEq, Eq)]
pub struct Turn {
    /// Everything the shell wrote between the previous trailer and this one, with the echo of
    /// the trailer's own line removed. This is the pane's feed — raw bytes, escapes and
    /// all, because a screen is what consumes them.
    pub bytes: Vec<u8>,
    /// `$?`, read at the top of the trailer's function body, where it is still the status of
    /// the line that was sent.
    pub status: i32,
    /// `$PWD`, the shell's own answer. A caller may not report a cwd it remembered.
    pub cwd: String,
    /// The exported environment the shell held when it wrote this trailer, as
    /// `NAME=value;` pairs - see [`trailer_definition`] for the filter. It is never printed
    /// and never travels: the one reader is `intent::env::SessionEnv::observe`.
    pub env: String,
}

impl std::fmt::Debug for Turn {
    /// A `Turn` may hold a credential in [`Turn::env`], so this prints the status and the cwd
    /// and says the environment is there without printing it: `derive(Debug)` would put
    /// every exported token into whatever formatted the turn.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Turn")
            .field("bytes", &format_args!("{} byte(s)", self.bytes.len()))
            .field("status", &self.status)
            .field("cwd", &self.cwd)
            .field(
                "env",
                &format_args!("{} name(s), not printed", self.env.matches('=').count()),
            )
            .finish()
    }
}

/// A nonce, unique per session on this box. The pid makes two sessions on one host differ; the
/// clock makes two sessions in one process differ; the counter makes two sessions in one
/// nanosecond differ, which is what a test does.
fn nonce() -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(
        "{:x}{:x}{:x}",
        std::process::id(),
        t,
        N.fetch_add(1, Ordering::Relaxed)
    )
}

/// The name of the function that writes the trailer, which carries the nonce. In the
/// name and not only in the environment, because the shell echoes the line we send: an echo
/// carrying `$LETIBOT_MARK` unexpanded is harmless, and the name makes [`strip_echo`] exact.
fn end_fn(mark: &str) -> String {
    format!("__lt_end_{mark}")
}

/// How many names one trailer may carry. The shell enforces this as it emits and
/// `intent::env::SessionEnv` on the way in: a report over the cap is refused whole, because
/// half an environment silently resolved is worse than none.
pub const ENV_PAIRS: usize = 64;

/// How many bytes of value one trailer may carry, for the reason [`ENV_PAIRS`] gives.
/// Four of the longest value `intent::env::ENV_VALUE` carries, so a long `PATH` fits and
/// still leaves room for the rest.
pub const ENV_BYTES: usize = 16384;

/// The line written once, at session start, that defines the trailer's functions.
/// `$?` is read at the top of the body, where it is still the status of the line before the
/// call. The environment half prints only a value that is already one plain word ([`PLAIN`]).
pub fn trailer_definition(fn_name: &str) -> String {
    let env_fn = format!("{fn_name}_env");
    format!(
        "{env_fn}() {{ command -v compgen >/dev/null 2>&1 || return; local n v c=0 b=0; \
         for n in $(compgen -e); do \
         case \"${{!n}}\" in ''|*[!{plain}]*) continue;; esac; v=${{!n}}; \
         [ ${{#v}} -gt {value} ] && continue; \
         printf '%s=%s;' \"$n\" \"$v\"; c=$((c+1)); b=$((b+${{#n}}+${{#v}}+2)); \
         [ $c -ge {pairs} ] && return; [ $b -ge {bytes} ] && return; done; }}\n\
         {fn_name}() {{ printf '\\033]1337;letibot;%s;%s;%s\n' \"$LETIBOT_MARK\" \"$?\" \"$PWD\"; \
         {env_fn}; printf '\\007'; }}",
        plain = PLAIN,
        pairs = ENV_PAIRS,
        bytes = ENV_BYTES,
        // The same per-value cap `intent::env` holds: a value the daemon would drop is a
        // value not worth carrying up a pty.
        value = crate::intent::env::ENV_VALUE,
    )
}

/// The characters a value may hold and still be carried - the set
/// `intent::parameters::as_one_word` splices back, generated into the shell function so the
/// two filters are one list. `-` is last: a bracket expression reads it as a character there.
pub const PLAIN: &str = "A-Za-z0-9_./:@%,-";

/// What to run, and with what. With a scope it is `/bin/sh -c <join_script>` in front of
/// the shell, so the pid we hold is a member of the cgroup and the shell `exec`s into it;
/// a session that could not join is not run ([`join_script`]'s `exit 125`). Pure.
pub fn argv(cfg: &ShellConfig) -> Result<(String, Vec<String>), ShellError> {
    let mut it = cfg.shell.iter().cloned();
    let prog = it
        .next()
        .filter(|p| !p.is_empty())
        .ok_or_else(|| ShellError::Config("the shell is empty".to_string()))?;
    let rest: Vec<String> = it.collect();
    match &cfg.scope {
        None => Ok((prog, rest)),
        Some(scope) => {
            let mut args = vec![
                "-c".to_string(),
                join_script().to_string(),
                // `$0` for the wrapper, which is only ever a name in a message.
                "letibot-shell".to_string(),
                Cgroup2::procs_path(scope).display().to_string(),
                // `$2` is the join token the host uses as *evidence* it can observe; this
                // module has the trailer instead, so the wrapper's `: > "$2"` is pointed at
                // the null device - named rather than silently passed.
                "/dev/null".to_string(),
            ];
            args.push(prog);
            args.extend(rest);
            Ok(("/bin/sh".to_string(), args))
        }
    }
}

/// Find a trailer in `buf` and take the turn out of it.
/// `Ok(None)` means *not yet*: no marker, or a marker whose terminator has not arrived - both
/// are the same instruction, to read more. Pure, so the scan is testable on hand-written bytes.
pub fn take_turn(buf: &mut Vec<u8>, mark: &str) -> Result<Option<Turn>, ShellError> {
    let open = format!("{MARK_OPEN}{mark};");
    let open = open.as_bytes();
    let Some(at) = find(buf, open) else {
        return Ok(None);
    };
    let body = at + open.len();
    let Some(rel) = buf[body..].iter().position(|b| *b == MARK_CLOSE) else {
        // The introducer is here and the terminator is not: one more read, and it will be.
        return Ok(None);
    };
    let raw = String::from_utf8_lossy(&buf[body..body + rel]).into_owned();
    // `<status>;<cwd>` then a newline then `<env>`: only the cwd can contain a newline,
    // so the split is on the FIRST one and the head parses as it did before the third field
    // existed. No newline is a shell this build did not generate - cwd and status, no env.
    let (head, env) = match raw.split_once('\n') {
        Some((head, env)) => (head, env),
        None => (raw.as_str(), ""),
    };
    let (status, cwd) = head
        .split_once(';')
        .ok_or_else(|| ShellError::Start(format!("a trailer with no `;` in it: {head:?}")))?;
    let status: i32 = status.trim().parse().map_err(|_| {
        ShellError::Start(format!(
            "a trailer whose status is not a number: {status:?}"
        ))
    })?;
    let mut bytes = buf[..at].to_vec();
    strip_echo(&mut bytes, &end_fn(mark));
    buf.drain(..body + rel + 1);
    Ok(Some(Turn {
        bytes,
        status,
        cwd: cwd.to_string(),
        env: env.to_string(),
    }))
}

/// Take the echo of the trailer's own line out of a turn. The operator's line stays: a
/// terminal shows the command that ran. The trailer's line is machinery nobody typed, and the
/// name carries the nonce, so this removes exactly one string.
pub fn strip_echo(bytes: &mut Vec<u8>, fn_name: &str) {
    let name = fn_name.as_bytes();
    let mut i = 0usize;
    while i + name.len() <= bytes.len() {
        if &bytes[i..i + name.len()] == name {
            let mut end = i + name.len();
            // The echo's newline, in either spelling: readline emits `\r\n` and a plain line
            // discipline emits `\n`.
            if end < bytes.len() && bytes[end] == b'\r' {
                end += 1;
            }
            if end < bytes.len() && bytes[end] == b'\n' {
                end += 1;
            }
            bytes.drain(i..end);
            continue;
        }
        i += 1;
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|i| &hay[*i..*i + needle.len()] == needle)
}

/// One shell, one pty, one cgroup. The pty is the terminal, the cgroup is the lifetime.
pub struct ShellSession {
    /// The master end. Reads are the shell's output; writes are its input.
    master: std::fs::File,
    /// The shell's pid — which is the pid the scope wrapper `exec`ed into, so it is a member
    /// of the session's cgroup.
    pid: i32,
    mark: String,
    fn_name: String,
    /// Bytes read and not yet consumed by a trailer scan. Carried between turns, so a
    /// marker split across two reads is one marker and the prompt that follows a turn belongs
    /// to the next one.
    pending: Vec<u8>,
    /// The last cwd the SHELL reported. `None` until a trailer has been read, because a
    /// starting directory is not an answer.
    cwd: Option<String>,
    /// The last environment the SHELL reported, as the trailer's `NAME=value;` pairs.
    /// `None` until a trailer is read, and `None` for a report this build refused. It
    /// replaces rather than accumulates: the report is the whole environment.
    env: Option<String>,
    scope: Option<ScopeId>,
    tree: Option<Arc<dyn ScopeTree>>,
    /// What the reaper thread saw, once it has seen it.
    exit: Arc<Mutex<Option<i32>>>,
    /// Why this session must not be used, if it must not.
    blocked: Option<ShellError>,
}

impl std::fmt::Debug for ShellSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellSession")
            .field("pid", &self.pid)
            .field("mark", &self.mark)
            .field("cwd", &self.cwd)
            // A count, never the report. See [`Turn::env`]: a credential can be in it.
            .field("env", &self.env.as_ref().map(|e| e.matches('=').count()))
            .field("scope", &self.scope)
            .field("blocked", &self.blocked)
            .finish()
    }
}

impl ShellSession {
    /// Start the session. One pty, one shell on the far end of it, one cgroup if the
    /// caller named one, and a first trailer read before this returns — so a caller never
    /// holds a session that has not answered anything.
    pub fn open(cfg: &ShellConfig) -> Result<ShellSession, ShellError> {
        let (prog, args) = argv(cfg)?;
        let pty = Pty::open().map_err(|e| ShellError::Start(format!("no pty: {e}")))?;
        let mark = nonce();
        let fn_name = end_fn(&mark);

        let mut cmd = std::process::Command::new(&prog);
        cmd.args(&args);
        cmd.current_dir(&cfg.cwd);
        // The mark the trailer's function reads. In the environment rather than in the
        // definition, so the line the shell echoes carries the *name* and not the value.
        cmd.env("LETIBOT_MARK", &mark);
        // No inherited shell options. `SHELLOPTS=xtrace` traces the trailer's own
        // functions INTO the trailer, so the report is `+ for n in ...` instead of
        // `NAME=value;`. Filtered from `cfg.env` too: a caller must not re-arm it.
        cmd.env_remove("SHELLOPTS").env_remove("BASHOPTS");
        for (k, v) in &cfg.env {
            if k == "SHELLOPTS" || k == "BASHOPTS" {
                continue;
            }
            cmd.env(k, v);
        }
        // A terminal on all three, which is the difference between this and the row path:
        // the shell reads the lines a person types at it.
        let (a, b, c) = (
            pty.stdio().map_err(|e| ShellError::Start(e.to_string()))?,
            pty.stdio().map_err(|e| ShellError::Start(e.to_string()))?,
            pty.stdio().map_err(|e| ShellError::Start(e.to_string()))?,
        );
        cmd.stdin(a).stdout(b).stderr(c);

        // `setsid` and `TIOCSCTTY`, here rather than in `pty.rs` on purpose: this is the
        // one path that wants a session leader with a controlling terminal. Failure is not
        // fatal - a shell without one still runs, it just has no job control.
        unsafe {
            use std::os::unix::process::CommandExt;
            cmd.pre_exec(|| {
                if libc::setsid() < 0 {
                    // Already a group leader, which a forked child is not — so this is a
                    // failure we do not understand, and the shell still runs.
                }
                let _ = libc::ioctl(0, libc::TIOCSCTTY as _, 0);
                Ok(())
            });
        }

        let child = cmd
            .spawn()
            .map_err(|e| ShellError::Start(format!("{prog}: {e}")))?;
        let pid = child.id() as i32;
        // The parent's own slave handles go now. A slave this process still holds open is
        // a master read that never reports the child's exit — `pty.rs`'s own test measures
        // thirty seconds of that.
        let master = pty.into_master();
        drop(cmd);

        // The size before the first byte, so the shell's `$COLUMNS` and any program that
        // asks are right from the start rather than right from the first resize.
        set_size(&master, cfg.cols, cfg.rows);

        // A reaper, so a shell that exits is a fact rather than a zombie.
        let exit = Arc::new(Mutex::new(None));
        {
            let slot = Arc::clone(&exit);
            let mut child = child;
            std::thread::spawn(move || {
                let code = child.wait().ok().map(|s| s.code().unwrap_or(-1));
                if let Ok(mut slot) = slot.lock() {
                    *slot = code;
                }
            });
        }

        let mut session = ShellSession {
            master,
            pid,
            mark,
            fn_name,
            pending: Vec::new(),
            cwd: None,
            env: None,
            scope: cfg.scope.clone(),
            tree: cfg.tree.clone(),
            exit,
            blocked: None,
        };
        // The session is not open until it has answered. Defining the function and
        // calling it is also the readiness probe: a shell that never comes up is a `Start`
        // here rather than a mystery on the first `run`, and the two lines go together.
        let setup = format!(
            "{}\n{}\n",
            trailer_definition(&session.fn_name),
            session.fn_name
        );
        session.write(setup.as_bytes())?;
        match session.read_turn(cfg.ready) {
            Ok(t) => {
                session.take_env(&t);
                session.cwd = Some(t.cwd);
                Ok(session)
            }
            Err(e) => {
                session.close();
                Err(e)
            }
        }
    }

    /// The nonce this session's trailer carries. Unique on this box, and in the function's
    /// name so an echo of it can be removed exactly.
    pub fn mark(&self) -> &str {
        &self.mark
    }

    /// The shell's pid. A member of the session's cgroup, because the wrapper joined
    /// before `exec`.
    pub fn pid(&self) -> i32 {
        self.pid
    }

    /// The last cwd the shell reported, or `None` before it has reported one.
    /// `None` is not "unknown, assume the workspace": it is *this session has not answered
    /// yet*, and a caller with a `None` has a [`ShellConfig::cwd`], a startup fact.
    pub fn cwd(&self) -> Option<&str> {
        self.cwd.as_deref()
    }

    /// The last environment the shell reported, as `NAME=value;` pairs - the input
    /// `intent::env::SessionEnv::observe` takes. `None` before the first trailer and `None`
    /// again if a report was refused: fail-closed. See [`Turn::env`].
    pub fn env(&self) -> Option<&str> {
        self.env.as_deref()
    }

    /// The report from a turn, dropped whole when it is empty — a trailer with no third field
    /// (a shell this build did not generate) must not wipe an environment that was observed.
    fn take_env(&mut self, turn: &Turn) {
        if !turn.env.is_empty() {
            self.env = Some(turn.env.clone());
        }
    }

    /// Whether the shell has exited. `None` while the reaper is still waiting.
    pub fn exited(&self) -> Option<i32> {
        self.exit.lock().ok().and_then(|e| *e)
    }

    /// Whether this session may be used. `Err` says why not, in the words a caller can show.
    pub fn usable(&self) -> Result<(), ShellError> {
        match &self.blocked {
            Some(e) => Err(e.clone()),
            None => Ok(()),
        }
    }

    /// Run one line, and answer with what the shell wrote and what the shell said.
    /// `line` is written verbatim: a grammar in front of the shell would be a second answer
    /// to what a line means. It must be one a person submitted; there is no timer here.
    pub fn run(&mut self, line: &str, wait: Duration) -> Result<Turn, ShellError> {
        // A shell that has left outranks everything this session was told before. A line
        // that timed out sets `blocked` to `Desynchronised`, and if the shell then *exited*
        // the truth is simpler and stronger: there is no shell.
        if let Some(code) = self.exited() {
            let e = ShellError::Gone(format!("the shell exited with {code}"));
            self.blocked = Some(e.clone());
            return Err(e);
        }
        self.usable()?;
        let mut write = String::with_capacity(line.len() + self.fn_name.len() + 2);
        write.push_str(line.trim_end_matches('\n'));
        write.push('\n');
        write.push_str(&self.fn_name);
        write.push('\n');
        self.write(write.as_bytes())?;
        let turn = self.read_turn(wait)?;
        self.cwd = Some(turn.cwd.clone());
        self.take_env(&turn);
        Ok(turn)
    }

    /// Read the trailer that is still in flight, after a timeout. The session is not
    /// broken, it is *behind*, and the turn returned is the one that timed out. `false` means
    /// it is still not here.
    pub fn resync(&mut self, wait: Duration) -> bool {
        match self.read_turn(wait) {
            Ok(t) => {
                self.take_env(&t);
                self.cwd = Some(t.cwd);
                self.blocked = None;
                true
            }
            Err(_) => false,
        }
    }

    /// Move the pty's size. The kernel raises `SIGWINCH` for the pty's foreground process
    /// group, so a program that redraws on a resize does. Nothing yet carries the head's pane
    /// rectangle to this call.
    pub fn resize(&mut self, cols: usize, rows: usize) {
        set_size(&self.master, cols, rows);
    }

    /// End the session. With a scope, `ScopeTree::end` kills the cgroup, everything
    /// under it dies with it, and the record carries what survived. Without one the fallback
    /// is `SIGHUP` to the shell, which is not the mechanism: it reaches no daemonised job.
    pub fn close(&mut self) -> Option<Reaping> {
        match (&self.tree, &self.scope) {
            (Some(tree), Some(scope)) => {
                let reaping = tree.end(scope);
                // The shell is dead by now; the master read would block for ever, so the
                // descriptor goes.
                self.blocked = Some(ShellError::Gone("the session was closed".to_string()));
                Some(reaping)
            }
            _ => {
                unsafe {
                    libc::kill(self.pid, libc::SIGHUP);
                }
                self.blocked = Some(ShellError::Gone("the session was closed".to_string()));
                None
            }
        }
    }

    fn write(&mut self, bytes: &[u8]) -> Result<(), ShellError> {
        self.master
            .write_all(bytes)
            .map_err(|e| ShellError::Gone(format!("the pty would not take the line: {e}")))
    }

    /// Read until a trailer, or until the deadline. The whole of the framing: scan what
    /// has arrived, and if there is no trailer, wait for more bytes with a bounded `poll` and
    /// append them. Nothing about a prompt, and nothing about how many reads it takes.
    fn read_turn(&mut self, wait: Duration) -> Result<Turn, ShellError> {
        let deadline = Instant::now() + wait;
        let mut buf = [0u8; 8192];
        loop {
            if let Some(turn) = take_turn(&mut self.pending, &self.mark)? {
                return Ok(turn);
            }
            if self.pending.len() > MAX_TURN {
                let n = self.pending.len();
                // Blocked as *behind*, not as *this call failed*. The two are different
                // facts and only one of them is about the caller's line: what the next call
                // needs to know is that a trailer is in flight.
                self.blocked = Some(ShellError::Desynchronised(format!(
                    "a turn grew to {n} bytes with no trailer in it"
                )));
                return Err(ShellError::Runaway(n));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                self.blocked = Some(ShellError::Desynchronised(format!(
                    "a line timed out after {wait:?}; its trailer is still in flight"
                )));
                return Err(ShellError::Timeout(wait));
            }
            match wait_readable(&self.master, left) {
                Ok(false) => continue,
                Ok(true) => {}
                Err(e) => {
                    let e = ShellError::Gone(e.to_string());
                    self.blocked = Some(e.clone());
                    return Err(e);
                }
            }
            match self.master.read(&mut buf) {
                Ok(0) => {
                    let e = ShellError::Gone(self.gone_why());
                    self.blocked = Some(e.clone());
                    return Err(e);
                }
                Ok(n) => self.pending.extend_from_slice(&buf[..n]),
                // `EIO` is this platform's end-of-pty, not a failure: it is what a read
                // on the master returns once the last slave is closed, and `pty.rs`'s own
                // drain treats it the same way.
                Err(e) if e.raw_os_error() == Some(libc::EIO) => {
                    let e = ShellError::Gone(self.gone_why());
                    self.blocked = Some(e.clone());
                    return Err(e);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    let e = ShellError::Gone(e.to_string());
                    self.blocked = Some(e.clone());
                    return Err(e);
                }
            }
        }
    }

    fn gone_why(&self) -> String {
        match self.exited() {
            Some(code) => format!("the shell exited with {code}"),
            None => "the pty's far end closed".to_string(),
        }
    }
}

impl Drop for ShellSession {
    /// A session nobody closed is still a session that must not leak. See
    /// [`ShellSession::close`] for what the two paths do.
    fn drop(&mut self) {
        if self.blocked.is_none() {
            self.close();
        }
    }
}

/// `TIOCSWINSZ` on the master. Failure is ignored: a pty that will not take a size is one
/// whose programs see the default, which is cosmetic and not a reason to lose a session.
/// `pub(crate)` because [`super::term`] is the second caller: one answer, one clamp.
pub(crate) fn set_size(master: &std::fs::File, cols: usize, rows: usize) {
    use std::os::fd::AsRawFd;
    let ws = libc::winsize {
        ws_row: rows.clamp(1, u16::MAX as usize) as u16,
        ws_col: cols.clamp(1, u16::MAX as usize) as u16,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    unsafe {
        libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &ws);
    }
}

/// Is there anything to read, inside `d`? `Ok(false)` is a timeout and not an error — the
/// caller loops, because the deadline is the caller's and not the poll's.
fn wait_readable(f: &std::fs::File, d: Duration) -> std::io::Result<bool> {
    use std::os::fd::AsRawFd;
    let mut p = libc::pollfd {
        fd: f.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let ms = d.as_millis().min(i32::MAX as u128) as i32;
    let rc = unsafe { libc::poll(&mut p, 1, ms.max(1)) };
    if rc < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(rc > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use super::super::scope::ScopeKind;

    /// The shell the tests drive. `--norc --noprofile` on purpose: an assertion here is
    /// about this module's framing, and the operator's rc is a file that prints things. The
    /// production default is [`super::super::console::shell`], the same argv shape.
    fn shell() -> Vec<String> {
        if Path::new("/bin/bash").exists() {
            vec![
                "/bin/bash".to_string(),
                "--norc".to_string(),
                "--noprofile".to_string(),
                "-i".to_string(),
            ]
        } else {
            vec!["/bin/sh".to_string(), "-i".to_string()]
        }
    }

    fn config(cwd: &Path) -> ShellConfig {
        ShellConfig {
            shell: shell(),
            cwd: cwd.to_path_buf(),
            ..ShellConfig::default()
        }
    }

    /// A scratch directory, per test and per process — the tree's own pattern.
    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("letibot-shell-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("a scratch directory");
        d
    }

    fn text(t: &Turn) -> String {
        String::from_utf8_lossy(&t.bytes).into_owned()
    }

    /// Long enough that a loaded box does not fail the test, short enough that a hang does
    /// not hold the suite: every wait ends when the trailer arrives.
    const LONG: Duration = Duration::from_secs(30);

    /// A session answers a line, and the status is the shell's own answer. The two halves
    /// are the whole of the framing: the bytes between two trailers, and `$?` read where it is
    /// still the status of the line sent. The `42` is the half that matters.
    #[test]
    fn a_session_answers_a_line_and_reports_the_shells_status() {
        let dir = tmp("status");
        let mut s = ShellSession::open(&config(&dir)).expect("a shell session");
        let t = s.run("printf 'hi\\n'", LONG).expect("a turn");
        assert!(text(&t).contains("hi"), "{:?}", text(&t));
        assert_eq!(t.status, 0);
        let t = s.run("(exit 42)", LONG).expect("a turn");
        assert_eq!(
            t.status, 42,
            "the status is the shell's, and it is not always zero"
        );
        // And the session is still usable after a failing line, which is what makes the next
        // test meaningful rather than lucky.
        let t = s.run("printf 'after'", LONG).expect("a turn");
        assert!(text(&t).contains("after"));
        assert_eq!(t.status, 0);
    }

    /// The slice: one session, two lines, and the state carried between them. `cd` is the
    /// case (`! cd /x` then `! ls` lists `/x`), and the one a fresh `sh -c` per line cannot do
    /// however many flags it is given.
    #[test]
    fn the_working_directory_survives_between_two_lines() {
        let dir = tmp("cwd");
        std::fs::create_dir_all(dir.join("sub")).expect("a subdirectory");
        // The shell answers with the physical path, and on macOS the temp dir is a link
        // (`/var` is `/private/var`), so the expectation is canonical too.
        let dir = dir.canonicalize().expect("the directory exists");
        let sub = dir.join("sub");
        let mut s = ShellSession::open(&config(&dir)).expect("a shell session");
        assert_eq!(
            s.cwd(),
            Some(dir.to_str().unwrap()),
            "the first trailer is the shell's own answer about where it started"
        );

        s.run(&format!("cd {}", sub.display()), LONG)
            .expect("a turn");
        let t = s.run("pwd", LONG).expect("a turn");
        assert!(
            text(&t).contains(sub.to_str().unwrap()),
            "`pwd` after a `cd` on the previous line: {:?}",
            text(&t)
        );

        // And the head's notion follows the shell, because it IS the shell's answer. The
        // assertion that matters is the negative: the session is not reporting the directory
        // it was opened in.
        assert_eq!(t.cwd, sub.to_str().unwrap());
        assert_eq!(s.cwd(), Some(sub.to_str().unwrap()));
        assert_ne!(
            s.cwd(),
            Some(dir.to_str().unwrap()),
            "the session is still reporting its startup directory"
        );
    }

    /// `export`, a function and an alias survive too — the other three things the operator
    /// named, and each is a different mechanism: an environment variable, a shell function,
    /// and a name the shell expands at read time.
    #[test]
    fn an_export_a_function_and_an_alias_survive() {
        let dir = tmp("state");
        let mut s = ShellSession::open(&config(&dir)).expect("a shell session");

        s.run("export LT_CARRIED=carried", LONG).expect("a turn");
        let t = s.run("printf '%s' \"$LT_CARRIED\"", LONG).expect("a turn");
        assert!(text(&t).contains("carried"), "{:?}", text(&t));

        s.run("lt_fn() { printf 'from-the-function'; }", LONG)
            .expect("a turn");
        let t = s.run("lt_fn", LONG).expect("a turn");
        assert!(text(&t).contains("from-the-function"), "{:?}", text(&t));

        s.run("alias lt_alias='printf aliased'", LONG)
            .expect("a turn");
        let t = s.run("lt_alias", LONG).expect("a turn");
        assert!(
            text(&t).contains("aliased"),
            "an alias is read at the next line, and the next line is a new read: {:?}",
            text(&t)
        );
        // And the environment came out in a trailer. The adjudicator reads a command
        // against what the shell really holds, so the shell says what that is after every
        // line. `LT_CARRIED` was set two lines above.
        let report = s.env().expect("every trailer carries the environment");
        assert!(
            report.split(';').any(|p| p == "LT_CARRIED=carried"),
            "the exported value is not in the trailer: {report}"
        );
    }

    /// A shell option asked for cannot trace the trailer. Through `cfg.env` rather than
    /// the process environment, so this test cannot do to its neighbours what `host.rs`'s
    /// `SHELLOPTS=xtrace` did to `an_export_a_function_and_an_alias_survive`.
    #[test]
    fn shellopts_xtrace_does_not_reach_the_trailer() {
        let dir = tmp("xtrace");
        let mut cfg = config(&dir);
        cfg.env.push(("SHELLOPTS".into(), "xtrace".into()));
        cfg.env.push(("LT_PLAIN".into(), "plain".into()));
        let mut s = ShellSession::open(&cfg).expect("a shell session");

        let t = s.run("printf 'ok'", LONG).expect("a turn");
        assert!(!text(&t).contains("+ printf"), "traced: {:?}", text(&t));
        let report = s.env().expect("a report");
        assert!(
            !report.contains('+'),
            "the trace is in the trailer: {report}"
        );
        assert!(
            report.split(';').any(|p| p == "LT_PLAIN=plain"),
            "the report lost the plain value: {report}"
        );
    }

    /// The filter the shell applies is the set the resolver accepts, measured both ways.
    /// [`PLAIN`] and `intent::parameters::as_one_word` must be the same set, or the daemon
    /// would resolve with values the trailer could never send. This runs the real shell.
    #[test]
    fn the_plain_class_is_exactly_the_set_the_resolver_accepts() {
        let dir = tmp("plain");
        let mut s = ShellSession::open(&config(&dir)).expect("a shell session");
        // Carried: every character `as_one_word` allows.
        let carried = [
            ("LT_A", "/home/op/Projects/x"),
            ("LT_B", "/a/b-c_d"),
            ("LT_C", "a@b:c%d,e"),
            ("LT_D", "1729"),
        ];
        // Not carried: whitespace, a glob, a shell metacharacter, an empty value.
        let dropped = [
            ("LT_E", "a b"),
            ("LT_F", "a*b"),
            ("LT_G", "a;b"),
            ("LT_H", ""),
            // A tilde is not in the plain set, and it is quoted here so the shell keeps it
            // as a character rather than expanding it. (A tab cannot be sent at all: an
            // interactive bash reads the line through readline, where TAB is completion.)
            ("LT_I", "~"),
        ];
        for (n, v) in carried.iter().chain(dropped.iter()) {
            s.run(&format!("export {n}='{v}'"), LONG).expect("a turn");
        }
        let report = s.env().expect("a report").to_string();
        for (n, v) in carried {
            assert!(
                report.split(';').any(|p| p == format!("{n}={v}")),
                "`{n}={v}` is one plain word and must be carried: {report}"
            );
        }
        for (n, _) in dropped {
            assert!(
                !report.split(';').any(|p| p.starts_with(&format!("{n}="))),
                "`{n}` is not one plain word and must not be carried: {report}"
            );
        }
    }

    /// A credential is carried, and the session does not print it. The report holds the
    /// value, because the shell really holds it, and every spelling a reader could reach
    /// masks it.
    #[test]
    fn a_credential_is_carried_and_never_printed() {
        let dir = tmp("credential");
        let mut s = ShellSession::open(&config(&dir)).expect("a shell session");
        s.run("export LT_TOKEN=ghp_0123456789abcdef4c", LONG)
            .expect("a turn");
        let report = s.env().expect("a report").to_string();
        assert!(
            report.contains("LT_TOKEN=ghp_0123456789abcdef4c"),
            "the shell holds it and the report is the shell's answer: {report}"
        );
        // And nothing a person or a log reads prints it.
        assert!(
            !format!("{s:?}").contains("ghp_"),
            "the session's Debug: {s:?}"
        );
        let t = s.run("true", LONG).expect("a turn");
        assert!(
            !format!("{t:?}").contains("ghp_"),
            "the turn's Debug: {t:?}"
        );
        assert!(
            !text(&t).contains("ghp_"),
            "and it is not in the pane's feed: {:?}",
            text(&t)
        );
    }

    /// A prompt is not a protocol, and this is the test that says so. A command whose
    /// *output* is a prompt must not end the turn: a framing that watched for one would stop
    /// here and hand back half a line.
    #[test]
    fn output_that_looks_like_a_prompt_is_not_a_boundary() {
        let dir = tmp("prompt");
        let mut s = ShellSession::open(&config(&dir)).expect("a shell session");
        let t = s.run("printf 'user@host:~$ '", LONG).expect("a turn");
        assert_eq!(t.status, 0);
        // The turn ended because of the trailer, and the next line still lands.
        let t = s.run("printf 'second'", LONG).expect("a turn");
        assert!(text(&t).contains("second"), "{:?}", text(&t));
    }

    /// A command cannot forge the trailer, because the introducer is never in the text it
    /// could print: the nonce is in the function's *name* and in `$LETIBOT_MARK`, and a name
    /// is not the sequence.
    #[test]
    fn a_command_cannot_forge_a_trailer() {
        let dir = tmp("forge");
        let mut s = ShellSession::open(&config(&dir)).expect("a shell session");
        let t = s
            .run(
                "printf 'ESC]1337;letibot;%s;0;/forged' \"$LETIBOT_MARK\"",
                LONG,
            )
            .expect("a turn");
        assert!(
            !text(&t).contains("\u{1b}]1337;letibot;"),
            "the command produced the introducer: {:?}",
            text(&t)
        );
        assert_eq!(t.status, 0, "and the real trailer still arrived");
        let t = s.run("printf 'still here'", LONG).expect("a turn");
        assert!(text(&t).contains("still here"), "{:?}", text(&t));
    }

    /// The echo of the trailer's own line is removed, and the echo of the operator's line
    /// stays. The first is machinery nobody typed; the second is what a terminal shows, and
    /// the pane is a terminal.
    #[test]
    fn the_trailer_lines_echo_is_removed_and_the_operators_is_kept() {
        let dir = tmp("echo");
        let mut s = ShellSession::open(&config(&dir)).expect("a shell session");
        let fn_name = end_fn(s.mark());
        let t = s.run("printf 'echoed'", LONG).expect("a turn");
        let seen = text(&t);
        assert!(
            !seen.contains(&fn_name),
            "the trailer's own line was echoed into the turn: {seen:?}"
        );
        assert!(
            seen.contains("printf 'echoed'"),
            "the line the operator sent is not echoed, so the pane would not show it: {seen:?}"
        );
    }

    /// A deadline does not answer the wrong line. The trailer for the line that timed out
    /// is still coming, so the next `run` would read it and attribute it to the wrong command
    /// - an answer that looks exactly like a right one. `resync` is the way back.
    #[test]
    fn a_timeout_desynchronises_the_session_rather_than_answering_the_wrong_line() {
        let dir = tmp("timeout");
        let mut s = ShellSession::open(&config(&dir)).expect("a shell session");
        let e = s
            .run("sleep 2", Duration::from_millis(150))
            .expect_err("a two-second sleep inside a 150 ms wait");
        assert!(matches!(e, ShellError::Timeout(_)), "{e}");
        let e = s
            .run("printf 'never'", Duration::from_millis(150))
            .expect_err("a desynchronised session must refuse");
        assert!(matches!(e, ShellError::Desynchronised(_)), "{e}");

        // And it is not dead. The trailer that was in flight arrives, and catching up
        // puts the session back in step — the turn it returns is the one that timed out.
        assert!(
            s.resync(LONG),
            "the trailer that was in flight never arrived"
        );
        let t = s.run("printf 'back'", LONG).expect("a turn");
        assert!(text(&t).contains("back"), "{:?}", text(&t));
    }

    /// The shell has a terminal, and a controlling one. The second is what makes job
    /// control work and what silences bash's two lines about it; [`super::super::pty`] carries
    /// the mechanism.
    #[test]
    fn the_shell_has_a_terminal_and_a_controlling_one() {
        let dir = tmp("tty");
        let mut s = ShellSession::open(&config(&dir)).expect("a shell session");
        let t = s
            .run(
                "test -t 0 && echo stdin-is-a-tty || echo stdin-is-not-a-tty",
                LONG,
            )
            .expect("a turn");
        assert!(
            text(&t).contains("stdin-is-a-tty"),
            "the shell is not reading a terminal: {:?}",
            text(&t)
        );
        // `(exec 9</dev/tty)` and NOT `test -r /dev/tty`, which measures nothing: that is
        // a mode check on a device node and never an open, so it answers *yes* on a session
        // with no controlling terminal. The open is the question `nano` asks.
        let t = s
            .run(
                "if (exec 9</dev/tty) 2>/dev/null; then echo has-controlling; else echo none; fi",
                LONG,
            )
            .expect("a turn");
        assert!(
            text(&t).contains("has-controlling"),
            "no controlling terminal, so job control and `/dev/tty` are both absent: {:?}",
            text(&t)
        );
        assert!(
            !text(&t).contains("job control"),
            "bash's own two lines about job control reached the session: {:?}",
            text(&t)
        );
    }

    /// Two sessions do not share a nonce. The mark is what makes a trailer unforgeable
    /// and an echo removable, and one mark for two sessions would make both of those false at
    /// once.
    #[test]
    fn two_sessions_have_two_marks() {
        let dir = tmp("marks");
        let a = ShellSession::open(&config(&dir)).expect("a shell session");
        let b = ShellSession::open(&config(&dir)).expect("a second shell session");
        assert_ne!(a.mark(), b.mark());
        assert_ne!(a.pid(), b.pid());
    }

    /// The scope wrapper goes in front of the shell, and the shell is last. Pure, so the
    /// ordering is asserted without a cgroup existing: [`join_script`] writes `$$` into
    /// `cgroup.procs` and only then `exec`s, so the pid we hold is a member of the scope.
    #[test]
    fn the_scope_wrapper_goes_in_front_of_the_shell() {
        let scope = ScopeId {
            kind: ScopeKind::Session,
            name: "s1".to_string(),
            path: PathBuf::from("/sys/fs/cgroup/letibot.1/session.s1"),
        };
        let cfg = ShellConfig {
            shell: vec!["/bin/bash".to_string(), "-ic".to_string()],
            scope: Some(scope.clone()),
            ..ShellConfig::default()
        };
        let (prog, args) = argv(&cfg).expect("an argv");
        assert_eq!(prog, "/bin/sh", "the wrapper is a shell and not the shell");
        assert!(args.contains(&join_script().to_string()), "{args:?}");
        assert!(
            args.iter().any(|a| a.ends_with("session.s1/cgroup.procs")),
            "the scope's `cgroup.procs` is not in the argv: {args:?}"
        );
        assert_eq!(
            &args[args.len() - 2..],
            &["/bin/bash".to_string(), "-ic".to_string()],
            "the shell and its flags are the last words, which is what `exec \"$@\"` runs"
        );
        assert!(
            args.windows(2).any(|w| w[0] == "letibot-shell"),
            "the wrapper's `$0` is missing: {args:?}"
        );

        // Without a scope the shell is the program, and there is no wrapper at all.
        let cfg = ShellConfig {
            shell: vec!["/bin/bash".to_string(), "-ic".to_string()],
            ..ShellConfig::default()
        };
        let (prog, args) = argv(&cfg).expect("an argv");
        assert_eq!(prog, "/bin/bash");
        assert_eq!(args, vec!["-ic".to_string()]);
    }

    /// An empty shell is a refusal and not a spawn of nothing.
    #[test]
    fn an_empty_shell_is_refused_by_name() {
        let cfg = ShellConfig {
            shell: Vec::new(),
            ..ShellConfig::default()
        };
        assert!(matches!(argv(&cfg), Err(ShellError::Config(_))));
    }

    /// The scan, on hand-written bytes — the split marker, the two facts, and the cwd
    /// that contains the separator. Pure, so this is the framing's own test rather than a
    /// test of bash.
    #[test]
    fn the_scan_waits_for_a_split_marker_and_takes_both_facts() {
        let mark = "abc";
        let mut buf = b"out \x1b]1337;letibot;ab".to_vec();
        assert!(
            take_turn(&mut buf, mark).expect("a scan").is_none(),
            "half a marker is not a marker"
        );
        buf.extend_from_slice(b"c;0;/tmp\x07tail");
        let t = take_turn(&mut buf, mark).expect("a scan").expect("a turn");
        assert_eq!(t.status, 0);
        assert_eq!(t.cwd, "/tmp");
        assert_eq!(t.bytes, b"out ");
        assert_eq!(
            buf, b"tail",
            "what follows the trailer is the next turn's prefix"
        );

        // A path with the separator in it survives, because the split is on the FIRST `;`.
        let mut buf = b"\x1b]1337;letibot;m;3;/a;b\x07".to_vec();
        let t = take_turn(&mut buf, "m").expect("a scan").expect("a turn");
        assert_eq!(t.status, 3);
        assert_eq!(t.cwd, "/a;b");

        // A trailer with no terminator yet is *not yet*, not a failure.
        let mut buf = b"\x1b]1337;letibot;m;0;/w".to_vec();
        assert!(take_turn(&mut buf, "m").expect("a scan").is_none());
    }

    /// A trailer this module cannot read is a refusal, not a guess. The two malformed
    /// shapes are the two fields, and a guess here would be a status or a directory that no
    /// shell ever reported.
    #[test]
    fn a_malformed_trailer_is_refused_rather_than_guessed() {
        let mut buf = b"\x1b]1337;letibot;m;nope;/w\x07".to_vec();
        assert!(
            take_turn(&mut buf, "m").is_err(),
            "a status that is not a number"
        );
        let mut buf = b"\x1b]1337;letibot;m;0\x07".to_vec();
        assert!(take_turn(&mut buf, "m").is_err(), "no separator at all");
        // And a trailer for a *different* session's nonce is not this session's trailer.
        let mut buf = b"\x1b]1337;letibot;other;0;/w\x07".to_vec();
        assert!(take_turn(&mut buf, "m").expect("a scan").is_none());
    }

    /// The echo removal takes the name and not the text around it. A name that appeared
    /// twice would otherwise take the command's own output with it.
    #[test]
    fn the_echo_removal_is_exact() {
        let fn_name = end_fn("beef");
        let mut bytes = format!("printf 'x'\r\n{fn_name}\r\nout\n{fn_name}\nout2\n").into_bytes();
        strip_echo(&mut bytes, &fn_name);
        assert_eq!(
            String::from_utf8_lossy(&bytes),
            "printf 'x'\r\nout\nout2\n",
            "the name went and the two newlines it sat on went with it, once each"
        );
    }

    /// A session that is closed says so, rather than blocking a read on a pty whose far
    /// end is gone.
    #[test]
    fn a_closed_session_refuses_rather_than_hanging() {
        let dir = tmp("closed");
        let mut s = ShellSession::open(&config(&dir)).expect("a shell session");
        s.run("printf 'alive'", LONG).expect("a turn");
        s.close();
        assert!(matches!(s.usable(), Err(ShellError::Gone(_))));
        let e = s.run("printf 'no'", LONG).expect_err("a closed session");
        assert!(matches!(e, ShellError::Gone(_)), "{e}");
        // Dropping it again is not a second kill: `Drop` sees the block and does nothing.
    }

    /// A shell that exits is a fact and not a zombie. The reaper thread is what makes the
    /// next `run` say *the shell exited with N* rather than waiting out a deadline on a pty
    /// whose far end has closed.
    #[test]
    fn a_shell_that_exits_is_reported_and_not_waited_on() {
        let dir = tmp("exits");
        let mut s = ShellSession::open(&config(&dir)).expect("a shell session");
        // The trailer is written by the function *before* the shell processes `exit`, so this
        // turn can succeed or fail depending on which line the shell read first — and either
        // ending is honest. What must not happen is a hang.
        let _ = s.run("exit 3", Duration::from_secs(5));
        let deadline = Instant::now() + Duration::from_secs(10);
        while s.exited().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(s.exited(), Some(3), "the reaper never saw the shell leave");
        let e = s
            .run("printf 'no'", Duration::from_secs(5))
            .expect_err("a dead session");
        assert!(matches!(e, ShellError::Gone(_)), "{e}");
    }
}
