//! One long-lived shell per session: one pty, an interactive shell on the far end of it,
//! and a `!` line typed at that shell rather than a fresh `/bin/sh -c`. Once it exists the
//! shell is the authority for the cwd, the environment and the exit status, and it lives in
//! the session's cgroup, so a background job started in it dies with the conversation.
//!
//! Framing is a sentinel, not a prompt: a prompt is not a protocol, because it can appear
//! inside a command's own output. `run` writes the line and then a marked trailer and reads
//! until the trailer arrives. The line written for `run("ls")` is two lines:
//!
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
//! The trailer is one OSC sequence, so a command cannot forge it, `$?` and `$PWD` are the
//! shell's own answer read in one place, and a screen consumes it whole. A timeout
//! desynchronises the session, so `run` refuses afterwards and `resync` is the way back.
//!
//! Two rules: `run` executes a line a person submitted and never a model's turn, and nothing
//! the trailer carries (`$PWD`, `$?`, the environment) is ever printed.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::Pty;
use super::scope::{Cgroup2, Reaping, ScopeId, ScopeTree, join_script};

/// The OSC introducer every trailer starts with. `OSC 1337` is the range terminals reserve
/// for private use, and a screen consumes such a sequence whole while a program cannot emit
/// one by accident.
const MARK_OPEN: &str = "\x1b]1337;letibot;";

/// What ends a trailer: BEL, the short spelling of an OSC terminator.
const MARK_CLOSE: u8 = 0x07;

/// How long a session waits for the shell to come up and answer its first trailer.
pub const READY: Duration = Duration::from_secs(10);

/// How long a line gets before the session is called desynchronised. A caller may override it
/// per line; this default matches a tool call's budget, because a build is a command a person
/// typed.
pub const WAIT: Duration = Duration::from_secs(300);

/// How many bytes one turn may accumulate before it is called a runaway: a bound on what this
/// module holds while scanning for a trailer that may never come, not a cap on what a command
/// may print.
const MAX_TURN: usize = 32 * 1024 * 1024;

/// Why a session did not answer. Local to this module on purpose: nothing here is a tool
/// call, so a caller should not have to read a variant that talks about commands and gates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellError {
    /// The shell is not there: it exited, or the pty's far end closed.
    Gone(String),
    /// The trailer did not arrive inside the deadline. The session is desynchronised: the
    /// trailer for this line is still coming, and the next `run` would read it as its own.
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

/// How to start a session: everything a caller must decide, with the defaults a `!` line
/// wants. `Clone` and not `Debug`, because the scope tree is a trait object with no `Debug`.
#[derive(Clone)]
pub struct ShellConfig {
    /// The shell and its flags. [`super::console::shell`] is the operator's; tests pass
    /// `--norc` so an assertion is about this module and not about what an rc prints.
    pub shell: Vec<String>,
    /// Pairs put in front of the shell's environment: what a *session* adds on top of the
    /// console's own, `TERM` above all, because a shell on a pty without one cannot colourise.
    pub env: Vec<(String, String)>,
    /// Where the session starts. A startup fact, not a report: once the shell runs,
    /// [`ShellSession::cwd`] is the answer and this is history.
    pub cwd: PathBuf,
    /// The pty's size in columns and rows: the pane's rectangle at opening.
    /// [`ShellSession::resize`] moves it after that.
    pub cols: usize,
    pub rows: usize,
    /// The scope the session lives in, and the tree that will end it. `None` is a session
    /// nobody owns, which a test uses and [`ShellSession::close`] says so.
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
    /// the trailer's own line removed. The pane's feed: raw bytes, escapes and all.
    pub bytes: Vec<u8>,
    /// `$?`, read at the top of the trailer's function body, where it is still the status of
    /// the line that was sent.
    pub status: i32,
    /// `$PWD`, the shell's own answer. A caller may not report a cwd it remembered; only this
    /// field is a report.
    pub cwd: String,
    /// The exported environment the shell held when it wrote this trailer, as `NAME=value;`
    /// pairs; [`trailer_definition`] is the filter. Never printed and never on the wire: a
    /// credential can be in it, so the one reader masks it wherever it is shown.
    pub env: String,
}

impl std::fmt::Debug for Turn {
    /// A `Turn` may hold a credential in [`Turn::env`], so this prints the status and the cwd
    /// and says the environment is there without printing it. `derive(Debug)` would put every
    /// exported token into whatever formatted the turn.
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

/// A nonce, unique per session on this box: the pid separates two sessions, the clock two
/// sessions in one process, and the counter two in one nanosecond, which is what a test does.
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

/// The name of the function that writes the trailer, which carries the nonce. In the *name*
/// and not only in the environment, so the echoed line carries a name [`strip_echo`] can remove
/// exactly rather than a value it would have to guess at.
fn end_fn(mark: &str) -> String {
    format!("__lt_end_{mark}")
}

/// How many names one trailer may carry. The shell enforces this as it emits and
/// `intent::env::SessionEnv` on the way in: a report over either cap is refused whole rather
/// than truncated, because half an environment silently resolved is worse than none.
pub const ENV_PAIRS: usize = 64;

/// How many bytes of value one trailer may carry, for the reason [`ENV_PAIRS`] gives: four of
/// the longest value `intent::env::ENV_VALUE` carries, so a long `PATH` fits beside the rest.
pub const ENV_BYTES: usize = 16384;

/// The line written once at session start, defining the trailer's functions. `$?` is read at
/// the top of the body, still the status of the line before the call, and only a value that is
/// already one plain word is carried, so the report needs no de-quoter and no escaping.
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

/// The characters a value may hold and still be carried: the set
/// `intent::parameters::as_one_word` splices back. The shell function is generated from it, so
/// both filters are one list, and the hyphen is last so it is a character and not a range.
pub const PLAIN: &str = "A-Za-z0-9_./:@%,-";

/// What to run, and with what. Pure, so the wrapper's ordering is asserted without a process
/// starting. With a scope it is `/bin/sh -c <join_script> ...` in front of the shell, so the pid
/// we hold is a member of the cgroup and the shell `exec`s into that same pid.
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
                // `$2` is the join token `host.rs` uses as evidence it can observe. The
                // trailer is this module's answer, so the wrapper's `: > "$2"` goes to the
                // null device, named rather than silently passed.
                "/dev/null".to_string(),
            ];
            args.push(prog);
            args.extend(rest);
            Ok(("/bin/sh".to_string(), args))
        }
    }
}

/// Find a trailer in `buf` and take the turn out of it. `Ok(None)` means not yet: the marker is
/// absent, or its terminator has not arrived, which is the same instruction to read more. Pure,
/// so the scan is testable against hand-written bytes.
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
    // `<status>;<cwd>` then a newline then `<env>`. The split is on the FIRST newline because
    // neither the status nor a plain-word value can hold one, so a cwd that contains `;` still
    // survives; a trailer with no newline answers with no environment rather than mis-splitting.
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

/// Take the echo of the trailer's own line out of a turn: the operator's line stays, because a
/// terminal shows what was typed, and this one is machinery nobody typed that would otherwise
/// print `__lt_end_<nonce>` above every command's output. The nonce makes the match exact.
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

/// One shell, one pty, one cgroup.
pub struct ShellSession {
    /// The master end. Reads are the shell's output; writes are its input.
    master: std::fs::File,
    /// The shell's pid, which is the pid the scope wrapper `exec`ed into, so it is a member of
    /// the session's cgroup.
    pid: i32,
    mark: String,
    fn_name: String,
    /// Bytes read and not yet consumed by a trailer scan. Carried between turns, so a marker
    /// split across two reads is one marker and a prompt after a turn belongs to the next one.
    pending: Vec<u8>,
    /// The last cwd the SHELL reported. `None` until a trailer has been read, because a
    /// starting directory is not an answer.
    cwd: Option<String>,
    /// The last environment the SHELL reported, as the trailer's `NAME=value;` pairs. `None`
    /// until a trailer is read, and `None` for a report this build refused. It replaces rather
    /// than accumulates, so an `unset` needs no rule of its own.
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
            // A count, never the report: a credential can be in it. See [`Turn::env`].
            .field("env", &self.env.as_ref().map(|e| e.matches('=').count()))
            .field("scope", &self.scope)
            .field("blocked", &self.blocked)
            .finish()
    }
}

impl ShellSession {
    /// Start the session: one pty, one shell on the far end, one cgroup if the caller named
    /// one, and a first trailer read before this returns, so a caller never holds a session that
    /// has not answered.
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
        // No inherited shell options: `SHELLOPTS=xtrace` turns tracing on before the rc runs, and
        // the trace of the trailer's own functions then lands inside the trailer. Removed from
        // `cfg.env` too, so a caller naming one cannot re-arm what this removes.
        cmd.env_remove("SHELLOPTS").env_remove("BASHOPTS");
        for (k, v) in &cfg.env {
            if k == "SHELLOPTS" || k == "BASHOPTS" {
                continue;
            }
            cmd.env(k, v);
        }
        // A terminal on all three: the shell reads the lines a person types at it.
        let (a, b, c) = (
            pty.stdio().map_err(|e| ShellError::Start(e.to_string()))?,
            pty.stdio().map_err(|e| ShellError::Start(e.to_string()))?,
            pty.stdio().map_err(|e| ShellError::Start(e.to_string()))?,
        );
        cmd.stdin(a).stdout(b).stderr(c);

        // `setsid` and `TIOCSCTTY` here rather than in `pty.rs`, because a shell session wants
        // job control and a row command does not. Failure is not fatal: a shell without a
        // controlling terminal still runs, it just has no job control and says so in two lines.
        unsafe {
            use std::os::unix::process::CommandExt;
            cmd.pre_exec(|| {
                if libc::setsid() < 0 {
                    // Already a group leader, which a forked child is not, so this is a
                    // failure we do not understand and the shell still runs.
                }
                let _ = libc::ioctl(0, libc::TIOCSCTTY as _, 0);
                Ok(())
            });
        }

        let child = cmd
            .spawn()
            .map_err(|e| ShellError::Start(format!("{prog}: {e}")))?;
        let pid = child.id() as i32;
        // The parent's own slave handles go now: a slave this process still holds open is a
        // master read that never reports the child's exit.
        let master = pty.into_master();
        drop(cmd);

        // The size before the first byte, so `$COLUMNS` and any program that asks are right
        // from the start rather than from the first resize.
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
        // The session is not open until it has answered: defining the function and calling it is
        // also the readiness probe, so a shell that never comes up is a `Start` failure here
        // rather than a mystery on the first `run`.
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

    /// The shell's pid. A member of the session's cgroup, because the wrapper joined before
    /// `exec`.
    pub fn pid(&self) -> i32 {
        self.pid
    }

    /// The last cwd the shell reported, or `None` before it has reported one. `None` is not
    /// "assume the workspace": it is this session has not answered yet, and a caller holding one
    /// has a [`ShellConfig::cwd`] and a startup fact rather than a directory.
    pub fn cwd(&self) -> Option<&str> {
        self.cwd.as_deref()
    }

    /// The last environment the shell reported, as `NAME=value;` pairs: the input
    /// `intent::env::SessionEnv::observe` takes. `None` before the first trailer and `None` again
    /// if a report was refused, which is the fail-closed answer. See [`Turn::env`].
    pub fn env(&self) -> Option<&str> {
        self.env.as_deref()
    }

    /// The report from a turn, dropped whole when it is empty: a trailer with no third field (a
    /// shell this build did not generate) must not wipe an environment that was observed.
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

    /// Run one line, and answer with what the shell wrote and what the shell said. `line` is
    /// written verbatim, because a second grammar in front of the shell is a second answer to
    /// what a line means, and it must be a line a person submitted, never a model's turn.
    pub fn run(&mut self, line: &str, wait: Duration) -> Result<Turn, ShellError> {
        // A shell that has left outranks anything this session was told before: a line that
        // timed out blocks as desynchronised, and if the shell then exited, "there is no shell"
        // is the truer answer for the next call.
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

    /// Read the trailer that is still in flight, after a timeout. A timeout does not lose it:
    /// the line is still running, so the session is behind rather than broken, and the turn this
    /// returns is the one that timed out. `false` means it is still not here.
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

    /// Move the pty's size. The kernel raises `SIGWINCH` for the foreground process group, so a
    /// program that redraws on a resize does.
    pub fn resize(&mut self, cols: usize, rows: usize) {
        set_size(&self.master, cols, rows);
    }

    /// End the session. With a scope this is `ScopeTree::end`: the cgroup is killed, everything
    /// under it dies with it, and the record says what was seen before and after. Without one the
    /// fallback is `SIGHUP` to the shell, which reaches its jobs but nothing daemonised away.
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

    /// Read until a trailer, or until the deadline. The whole of the framing is here: scan what
    /// has arrived, and if there is no trailer, wait for more bytes with a bounded `poll`.
    fn read_turn(&mut self, wait: Duration) -> Result<Turn, ShellError> {
        let deadline = Instant::now() + wait;
        let mut buf = [0u8; 8192];
        loop {
            if let Some(turn) = take_turn(&mut self.pending, &self.mark)? {
                return Ok(turn);
            }
            if self.pending.len() > MAX_TURN {
                let n = self.pending.len();
                // Blocked as behind, not as this call failed: what the next call needs to know
                // is that a trailer is in flight.
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
                // `EIO` is this platform's end-of-pty, not a failure: it is what a read on the
                // master returns once the last slave is closed.
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
    /// [`ShellSession::close`].
    fn drop(&mut self) {
        if self.blocked.is_none() {
            self.close();
        }
    }
}

/// `TIOCSWINSZ` on the master. Failure is ignored: a pty that will not take a size shows the
/// default, which is cosmetic. `pub(crate)` because [`super::term`] is the second caller and two
/// `ioctl` wrappers would be two places for the clamp to be wrong.
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

/// Is there anything to read, inside `d`? `Ok(false)` is a timeout and not an error: the caller
/// loops, because the deadline is the caller's and not the poll's.
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

    /// The shell the tests drive, with `--norc --noprofile` so an assertion here is about this
    /// module's framing and not about what an operator's rc prints.
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

    /// A scratch directory, per test and per process.
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

    /// A session answers a line, and the status is the shell's own: the `42` is the half that
    /// matters, because a module that always answered zero would pass a `0`.
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

    /// One session, two lines, and the state carried between them. `cd` is the case a fresh
    /// `sh -c` per line cannot do however many flags it is given.
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

        // The head's notion follows the shell because it IS the shell's answer: the assertion
        // that matters is the negative one, that the session is not reporting its startup
        // directory.
        assert_eq!(t.cwd, sub.to_str().unwrap());
        assert_eq!(s.cwd(), Some(sub.to_str().unwrap()));
        assert_ne!(
            s.cwd(),
            Some(dir.to_str().unwrap()),
            "the session is still reporting its startup directory"
        );
    }

    /// `export`, a function and an alias survive too: an environment variable, a shell function,
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
        // And the environment came out in a trailer: the adjudicator reads a command against
        // what the shell really holds, so the shell says what that is after every line.
        let report = s.env().expect("every trailer carries the environment");
        assert!(
            report.split(';').any(|p| p == "LT_CARRIED=carried"),
            "the exported value is not in the trailer: {report}"
        );
    }

    /// A shell option asked for cannot trace the trailer. It goes through `cfg.env` rather than
    /// the process environment, so this test cannot do to its neighbours what a process-wide
    /// `SHELLOPTS=xtrace` does.
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

    /// The filter the shell applies is the set the resolver accepts: the trailer drops every
    /// value that is not one plain word ([`PLAIN`]) and `as_one_word` writes back only such a
    /// value, so this asks a real shell about a battery of them.
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
            // A tilde is not in the plain set, and it is quoted so the shell keeps it as a
            // character rather than expanding it. A tab cannot be sent at all: an interactive
            // bash reads through readline, where TAB is completion.
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

    /// A credential is carried and the session does not print it: the report holds the value
    /// because the shell really holds it, and every spelling a reader could reach masks it.
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

    /// A prompt is not a protocol: a command whose output is a prompt must not end the turn, or
    /// a framing that watched for one would hand back half a line.
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

    /// A command cannot forge the trailer: the introducer is never in the text it could print,
    /// because the nonce is in the function's name and in `$LETIBOT_MARK`, and a name is not the
    /// sequence.
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

    /// The echo of the trailer's own line is removed and the echo of the operator's stays: the
    /// first is machinery nobody typed, the second is what a terminal shows.
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

    /// A deadline does not answer the wrong line: the trailer for the line that timed out is
    /// still coming, so the next `run` would read it as its own. The session says so instead,
    /// and `resync` is the way back.
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

        // And it is not dead: catching up puts the session back in step, and the turn it
        // returns is the one that timed out.
        assert!(
            s.resync(LONG),
            "the trailer that was in flight never arrived"
        );
        let t = s.run("printf 'back'", LONG).expect("a turn");
        assert!(text(&t).contains("back"), "{:?}", text(&t));
    }

    /// The shell has a terminal, and a controlling one, which is what makes job control work and
    /// what silences bash's two lines about it.
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
        // `(exec 9</dev/tty)` and not `test -r /dev/tty`: that is a mode check on a device node
        // and never an open, so it answers yes on a session with no controlling terminal at all.
        // The open is the question `nano` asks, and the one that fails without one.
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

    /// Two sessions do not share a nonce: one mark for two sessions would make the trailer
    /// forgeable and the echo unremovable at once.
    #[test]
    fn two_sessions_have_two_marks() {
        let dir = tmp("marks");
        let a = ShellSession::open(&config(&dir)).expect("a shell session");
        let b = ShellSession::open(&config(&dir)).expect("a second shell session");
        assert_ne!(a.mark(), b.mark());
        assert_ne!(a.pid(), b.pid());
    }

    /// The scope wrapper goes in front of the shell and the shell is last. Pure, so the ordering
    /// is asserted without a cgroup: [`join_script`] writes `$$` into `cgroup.procs` and only
    /// then `exec`s, so the pid this module holds is a member of the scope.
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

    /// The scan on hand-written bytes: the split marker, the two facts, and a cwd that contains
    /// the separator. Pure, so this is the framing's own test rather than a test of bash.
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

        // A trailer with no terminator yet is not yet, not a failure.
        let mut buf = b"\x1b]1337;letibot;m;0;/w".to_vec();
        assert!(take_turn(&mut buf, "m").expect("a scan").is_none());
    }

    /// A trailer this module cannot read is a refusal, not a guess: a guess would be a status or
    /// a directory no shell ever reported.
    #[test]
    fn a_malformed_trailer_is_refused_rather_than_guessed() {
        let mut buf = b"\x1b]1337;letibot;m;nope;/w\x07".to_vec();
        assert!(
            take_turn(&mut buf, "m").is_err(),
            "a status that is not a number"
        );
        let mut buf = b"\x1b]1337;letibot;m;0\x07".to_vec();
        assert!(take_turn(&mut buf, "m").is_err(), "no separator at all");
        // And a trailer for a different session's nonce is not this session's trailer.
        let mut buf = b"\x1b]1337;letibot;other;0;/w\x07".to_vec();
        assert!(take_turn(&mut buf, "m").expect("a scan").is_none());
    }

    /// The echo removal takes the name and not the text around it, so a name that appeared twice
    /// does not take the command's own output with it.
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

    /// A session that is closed says so, rather than blocking a read on a pty whose far end is
    /// gone.
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

    /// A shell that exits is a fact and not a zombie: the reaper makes the next `run` say the
    /// shell exited rather than wait out a deadline on a pty whose far end has closed.
    #[test]
    fn a_shell_that_exits_is_reported_and_not_waited_on() {
        let dir = tmp("exits");
        let mut s = ShellSession::open(&config(&dir)).expect("a shell session");
        // The trailer is written by the function before the shell processes `exit`, so this turn
        // can succeed or fail depending on which line the shell read first, and either ending is
        // honest. What must not happen is a hang.
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
