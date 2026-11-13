//! `[boundary] firecode_only`: a shell call that runs a listed program (`npm`) runs in a firecode
//! VM the daemon holds for the session, never in the seat. Detection reads the command through the
//! shell grammar, so nested shells, wrappers and paths are seen; what it cannot read is said.

use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use letibot_code::shell::{self, Word};

use crate::backend::{Command, ExecBackend, Output};
use crate::exec::boundary::Entry;
use crate::intent::{script_argument, unwrap_wrapper};

pub mod guard;

const SHELLS: &[&str] = &["bash", "sh", "zsh", "dash", "ksh"];
const MAX_DEPTH: usize = 6;
const MAX_SCRIPT: usize = 256 * 1024;

/// What a file named by a command holds, as far as the session can read it.
pub enum Script {
    Text(String),
    /// A program, not a script.
    Binary,
    Absent,
    /// Present and not readable here, with why.
    Unreadable(String),
}

/// The listed program a command runs, and the places the analysis could not see into.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Seen {
    pub hit: Option<Hit>,
    pub unseen: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Hit {
    pub program: String,
    pub listed: Entry<String>,
}

/// Which listed program `command` runs, if any. `read` opens a script a shell or a shebang would
/// run, so `sh build.sh` is judged by what `build.sh` holds.
pub fn examine(command: &str, only: &[Entry<String>], read: &dyn Fn(&str) -> Script) -> Seen {
    let mut w = Walk {
        only,
        read,
        seen: Seen::default(),
    };
    w.text(command, 0);
    w.seen
}

struct Walk<'a> {
    only: &'a [Entry<String>],
    read: &'a dyn Fn(&str) -> Script,
    seen: Seen,
}

impl Walk<'_> {
    fn done(&self) -> bool {
        self.seen.hit.is_some()
    }

    fn unseen(&mut self, why: String) {
        if !self.seen.unseen.contains(&why) {
            self.seen.unseen.push(why);
        }
    }

    fn check(&mut self, program: &str) -> bool {
        let base = program.rsplit('/').next().unwrap_or(program);
        match self.only.iter().find(|e| e.value == base) {
            Some(e) => {
                self.seen.hit = Some(Hit {
                    program: base.to_string(),
                    listed: e.clone(),
                });
                true
            }
            None => false,
        }
    }

    fn text(&mut self, text: &str, depth: usize) {
        if depth > MAX_DEPTH {
            self.unseen("shell text nested deeper than the analysis follows".into());
            return;
        }
        let n = shell::normalise(text);
        for stage in &n.stages {
            if self.done() {
                return;
            }
            let Some(program) = stage.program.literal() else {
                self.unseen(format!(
                    "a command name it cannot read in `{}`",
                    clip(&n.source, stage.span.start, 60)
                ));
                continue;
            };
            self.program(program, &stage.argv, depth);
        }
    }

    fn program(&mut self, program: &str, argv: &[Word], depth: usize) {
        if self.check(program) {
            return;
        }
        let base = program.rsplit('/').next().unwrap_or(program);
        let lits = |ws: &[Word]| -> Vec<String> {
            ws.iter()
                .filter_map(|w| w.literal().map(str::to_string))
                .collect()
        };
        if let Some((inner, at)) = unwrap_wrapper(base, argv) {
            let raw = argv[at].literal().unwrap_or(&inner).to_string();
            return self.program(&raw, &argv[(at + 1).min(argv.len())..], depth + 1);
        }
        match base {
            "exec" | "builtin" => {
                let mut i = 0;
                while let Some(t) = argv.get(i).and_then(|w| w.literal()) {
                    if t == "-a" {
                        i += 2;
                    } else if t.starts_with('-') {
                        i += 1;
                    } else {
                        break;
                    }
                }
                if let Some(p) = argv.get(i).and_then(|w| w.literal()) {
                    self.program(p, &argv[i + 1..], depth + 1);
                }
            }
            "find" => {
                let mut i = 0;
                while i < argv.len() {
                    if matches!(
                        argv[i].literal(),
                        Some("-exec" | "-execdir" | "-ok" | "-okdir")
                    ) && let Some(p) = argv.get(i + 1).and_then(|w| w.literal())
                    {
                        self.program(p, &argv[i + 2..], depth + 1);
                        i += 1;
                    }
                    i += 1;
                }
            }
            "eval" | "trap" | "watch" => {
                let text = lits(argv).join(" ");
                self.text(&text, depth + 1);
            }
            b if SHELLS.contains(&b) => self.shell(b, argv, depth),
            _ if program.contains('/') => self.file(program, depth),
            _ => {}
        }
    }

    /// `sh -c TEXT`, `bash -lc TEXT`, `sh FILE`.
    fn shell(&mut self, shell: &str, argv: &[Word], depth: usize) {
        let lits: Vec<Option<&str>> = argv.iter().map(|w| w.literal()).collect();
        let c_at = lits.iter().position(|t| {
            t.is_some_and(|t| t.starts_with('-') && !t.starts_with("--") && t.contains('c'))
        });
        if let Some(i) = c_at {
            match lits.get(i + 1).copied().flatten() {
                Some(body) => self.text(body, depth + 1),
                None => self.unseen(format!("the text `{shell} -c` is given is not a literal")),
            }
            return;
        }
        let strings: Vec<String> = lits.iter().flatten().map(|s| s.to_string()).collect();
        if let Some(path) = script_argument(shell, &strings) {
            self.script(&path, depth);
        }
    }

    /// A program named by path: a script with a shebang is read, a binary is not.
    fn file(&mut self, path: &str, depth: usize) {
        match (self.read)(path) {
            Script::Text(body) => {
                let Some(first) = body.lines().next().and_then(|l| l.strip_prefix("#!")) else {
                    return;
                };
                let mut words = first.split_whitespace();
                let mut interp = words.next().unwrap_or("");
                if interp.rsplit('/').next() == Some("env") {
                    interp = words.find(|w| !w.starts_with('-')).unwrap_or("");
                }
                if self.check(interp) {
                    return;
                }
                if SHELLS.contains(&interp.rsplit('/').next().unwrap_or("")) {
                    self.text(&body, depth + 1);
                }
            }
            Script::Binary | Script::Absent => {}
            Script::Unreadable(why) => self.unreadable(path, &why),
        }
    }

    fn script(&mut self, path: &str, depth: usize) {
        match (self.read)(path) {
            Script::Text(body) => self.text(&body, depth + 1),
            Script::Binary | Script::Absent => {}
            Script::Unreadable(why) => self.unreadable(path, &why),
        }
    }

    fn unreadable(&mut self, path: &str, why: &str) {
        self.unseen(format!(
            "`{path}` is run and could not be read ({why}), so whether it runs a listed program is unknown"
        ));
    }
}

fn clip(s: &str, at: usize, n: usize) -> String {
    s.get(at.min(s.len())..)
        .unwrap_or("")
        .chars()
        .take(n)
        .collect()
}

/// Turn a backend read of a script into [`Script`], bounded.
pub fn script_of(read: Result<Vec<u8>, crate::backend::BackendError>) -> Script {
    use crate::backend::BackendError;
    match read {
        Ok(b) if b.len() > MAX_SCRIPT => {
            Script::Unreadable("larger than the analysis reads".into())
        }
        Ok(b) => match String::from_utf8(b) {
            Ok(t) => Script::Text(t),
            Err(_) => Script::Binary,
        },
        Err(BackendError::NotFound(_) | BackendError::IsADirectory(_)) => Script::Absent,
        Err(e) => Script::Unreadable(e.to_string()),
    }
}

/// A routed call's result and what the route did around it.
#[derive(Debug)]
pub struct Routed {
    pub output: Output,
    pub notes: Vec<String>,
}

/// The VM a routed call runs in. A trait so a test records the commands without booting one.
pub trait Vm: Send + Sync {
    fn run(&self, command: &str, cwd: &str) -> Result<Routed, String>;
}

/// A repository's VM, made on its first routed call and kept for the next: boxed because the
/// closure that makes it has to outlive the call that handed it over.
type VmFor = Box<dyn Fn(&std::path::Path) -> Arc<dyn Vm> + Send + Sync>;

/// Runs a call between a snapshot of its repository and a restore of everything but
/// `node_modules/` and the manifests. The repository is the nearest one holding the call's `cwd`
/// inside the session's folder, and its VM mounts that repository alone, so a workspace of many
/// repositories routes each call to the one it names. A call no repository holds is not routed.
pub struct GuardedVm {
    vm_for: VmFor,
    pub root: std::path::PathBuf,
    vms: Mutex<std::collections::HashMap<std::path::PathBuf, Arc<dyn Vm>>>,
}

impl GuardedVm {
    /// `vm_for` makes a repository's VM on its first routed call; it is kept for the next.
    pub fn new(
        root: std::path::PathBuf,
        vm_for: impl Fn(&std::path::Path) -> Arc<dyn Vm> + Send + Sync + 'static,
    ) -> GuardedVm {
        GuardedVm {
            vm_for: Box::new(vm_for),
            root,
            vms: Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// One VM for every repository: a test's.
    pub fn one(root: std::path::PathBuf, inner: Arc<dyn Vm>) -> GuardedVm {
        GuardedVm::new(root, move |_| inner.clone())
    }
}

/// The nearest directory with a `.git` from `cwd` up to `root`, both included; `None` when the
/// walk leaves `root` first. A relative `cwd` is under `root`.
pub fn repo_of(root: &std::path::Path, cwd: &str) -> Option<std::path::PathBuf> {
    let mut at = root.join(cwd);
    loop {
        if !at.starts_with(root) {
            return None;
        }
        if at.join(".git").exists() {
            return Some(at);
        }
        if at == root || !at.pop() {
            return None;
        }
    }
}

impl Vm for GuardedVm {
    fn run(&self, command: &str, cwd: &str) -> Result<Routed, String> {
        let repo = repo_of(&self.root, cwd).ok_or_else(|| {
            format!(
                "no git repository holds `{cwd}` inside {}, so there is nothing to snapshot and \
                 restore around the call, and a call is not routed unguarded. Run it from inside \
                 the repository it is for (`cwd`).",
                self.root.display()
            )
        })?;
        let snap = guard::snapshot(&repo).map_err(|e| {
            format!(
                "{} cannot be snapshotted before the call ({e}), and a call is not routed unguarded",
                repo.display()
            )
        })?;
        let vm = {
            let mut vms = self.vms.lock().unwrap_or_else(|e| e.into_inner());
            vms.entry(repo.clone())
                .or_insert_with(|| (self.vm_for)(&repo))
                .clone()
        };
        let ran = vm.run(command, cwd);
        let report = guard::restore(&snap);
        let mut routed = ran?;
        routed.notes.extend(report.sentences());
        Ok(routed)
    }
}

/// The session's routing: the listed programs and the VM they go to.
#[derive(Clone)]
pub struct FirecodeRoute {
    pub only: Vec<Entry<String>>,
    pub vm: Arc<dyn Vm>,
}

impl std::fmt::Debug for FirecodeRoute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FirecodeRoute({} programs)", self.only.len())
    }
}

type Start = Box<dyn Fn() -> Result<Arc<dyn ExecBackend>, String> + Send + Sync>;

struct Held {
    backend: Option<Arc<dyn ExecBackend>>,
    last: Instant,
}

/// One VM per session: started by the first routed call, kept for the next, dropped (which brings
/// it down) when the session ends or it has been idle for `idle`.
pub struct SessionVm {
    start: Start,
    held: Arc<Mutex<Held>>,
    idle: Duration,
}

pub const IDLE: Duration = Duration::from_secs(600);

impl SessionVm {
    pub fn new(start: Start, idle: Duration) -> SessionVm {
        SessionVm {
            start,
            held: Arc::new(Mutex::new(Held {
                backend: None,
                last: Instant::now(),
            })),
            idle,
        }
    }

    /// The idle sweeper holds only a weak handle, so it ends with the session.
    fn sweep(held: Weak<Mutex<Held>>, idle: Duration) {
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(
                    (idle / 4).clamp(Duration::from_millis(50), Duration::from_secs(30)),
                );
                let Some(h) = held.upgrade() else { return };
                let gone = {
                    let mut g = h.lock().unwrap_or_else(|e| e.into_inner());
                    if g.backend.is_some() && g.last.elapsed() >= idle {
                        g.backend.take()
                    } else {
                        None
                    }
                };
                let ended = gone.is_some();
                drop(gone);
                if ended {
                    return;
                }
            }
        });
    }
}

impl Vm for SessionVm {
    fn run(&self, command: &str, cwd: &str) -> Result<Routed, String> {
        let backend = {
            let mut g = self.held.lock().unwrap_or_else(|e| e.into_inner());
            g.last = Instant::now();
            if g.backend.is_none() {
                g.backend = Some((self.start)()?);
                Self::sweep(Arc::downgrade(&self.held), self.idle);
            }
            g.backend.clone().expect("just set")
        };
        let out = backend
            .run(&Command {
                argv: vec!["bash".into(), "-c".into(), command.into()],
                cwd: cwd.into(),
                env: Vec::new(),
            })
            .map_err(|e| e.to_string());
        self.held.lock().unwrap_or_else(|e| e.into_inner()).last = Instant::now();
        out.map(|output| Routed {
            output,
            notes: Vec::new(),
        })
    }
}

/// Linux: `firecode run` sends the changed files in and copies nothing back, so the manifests a
/// package manager rewrites are fetched one by one with `firecode cp`. Not exercised on a Mac.
pub struct RunVm {
    pub bin: std::path::PathBuf,
    pub project: std::path::PathBuf,
}

impl Vm for RunVm {
    fn run(&self, command: &str, cwd: &str) -> Result<Routed, String> {
        use crate::rpc::{Ended, run_bounded};
        let wait = Duration::from_secs(600);
        let line = format!("cd '{}' && {{ {command}\n}}", cwd.replace('\'', "'\\''"));
        let args: Vec<String> = ["run", "--project"]
            .iter()
            .map(|s| s.to_string())
            .chain([
                self.project.display().to_string(),
                "--".into(),
                "bash".into(),
                "-c".into(),
                line,
            ])
            .collect();
        let o = match run_bounded(&self.bin, &args, wait) {
            Ended::Answered(o) => o,
            Ended::TimedOut => return Err("firecode run did not answer in time".into()),
            Ended::Unstartable(e) => return Err(format!("firecode run: {e}")),
        };
        let mut notes = Vec::new();
        for name in guard::MANIFESTS {
            let host = std::path::Path::new(cwd).join(name);
            let tmp =
                std::env::temp_dir().join(format!("fc-manifest-{}-{name}", std::process::id()));
            let cp = [
                "cp".to_string(),
                "--project".into(),
                self.project.display().to_string(),
                format!("vm:{}/{name}", cwd),
                tmp.display().to_string(),
            ];
            if let Ended::Answered(c) = run_bounded(&self.bin, &cp, Duration::from_secs(60))
                && c.status.success()
                && let Ok(new) = std::fs::read(&tmp)
                && std::fs::read(&host).ok().as_deref() != Some(&new)
                && std::fs::write(&host, new).is_ok()
            {
                notes.push(format!("copied back from the VM: {}", host.display()));
            }
            let _ = std::fs::remove_file(&tmp);
        }
        let mut stdout = o.stdout;
        stdout.extend(o.stderr);
        Ok(Routed {
            output: Output {
                stdout,
                stderr: Vec::new(),
                exit: o.status.code().unwrap_or(-1),
            },
            notes,
        })
    }
}

/// The route a session gets in production: a mounted-read-write VM on a Mac, `firecode run` on
/// Linux, started by the first routed call. Unavailable firecode is the call's refusal, not a
/// startup failure.
pub fn production(
    only: Vec<Entry<String>>,
    workspace: std::path::PathBuf,
    session_id: String,
) -> FirecodeRoute {
    let vm_for = move |repo: &std::path::Path| -> Arc<dyn Vm> {
        let repo = repo.to_path_buf();
        if cfg!(target_os = "macos") {
            let name = format!(
                "{session_id}-fconly-{}",
                repo.file_name().and_then(|n| n.to_str()).unwrap_or("repo")
            );
            let start: Start = Box::new(move || {
                let mut spec = crate::firecode::FirecodeSpec::for_box(repo.clone(), name.clone());
                spec.mount_rw = true;
                let b = crate::firecode::FirecodeBackend::up(&spec).map_err(|e| e.to_string())?;
                Ok(Arc::new(b) as Arc<dyn ExecBackend>)
            });
            Arc::new(SessionVm::new(start, IDLE))
        } else {
            match crate::firecode::firecode_available() {
                Ok(bin) => Arc::new(RunVm { bin, project: repo }),
                Err(why) => Arc::new(Unavailable(why)),
            }
        }
    };
    FirecodeRoute {
        only,
        vm: Arc::new(GuardedVm::new(workspace, vm_for)),
    }
}

struct Unavailable(String);

impl Vm for Unavailable {
    fn run(&self, _: &str, _: &str) -> Result<Routed, String> {
        Err(self.0.clone())
    }
}

#[cfg(test)]
mod tests;
