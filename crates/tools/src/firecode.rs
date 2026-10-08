//! The firecode backend: a subagent's tools run inside a VM, the model stays on
//! the host. W10, filled on 2026-09-14. `docs/subagents.md` §3 has the measured
//! contract this is built on; the short form:
//!
//! | door | what it is |
//! |---|---|
//! | `firecode up --project P` | boots a VM holding a **copy** of `P` at the same path; ~6–12 s |
//! | `firecode in --project P --cwd D 'cmd'` | one `bash -lc` command in the guest, ~0.5–0.8 s; exit status propagates; stdout and stderr arrive merged, text-filtered |
//! | `firecode cp` | files either way over vsock |
//! | `firecode down --project P` | stops it; the guest's copy lands in a sibling `P-<stamp>` |
//!
//! # The VM is the boundary
//!
//! Nothing a command does in there reaches the host except through `cp` and
//! `down`, so a session placed here runs at allow-all and the gate asks no
//! question. That is §5 of the design brief made concrete, and it is why this
//! backend's [`Confinement`] describes a boundary the host ones cannot. What it
//! does **not** confine, said in the same breath: the guest has the network
//! unless firecode was told `--no-net`, and it has whatever the project's
//! `firecode.layer` installed.
//!
//! # Files are the guest's
//!
//! There is no shared filesystem — Firecracker has no virtio-fs — so every
//! `read`, `write`, `list` and `stat` is one round trip. A read is `base64
//! -w0 < path`, decoded here, which is what makes it binary-safe through the
//! guest's terminal filter. A write is `cp` to a temp name in the guest then
//! `mv -f`: atomic, as the trait requires. `root_path` is `None`, as the trait's
//! own doc says a firecode guest must be — a host path here would make
//! `path_is_inside` answer confidently about a filesystem it is not looking at.
//!
//! # Processes are host-side clients
//!
//! A job is the `firecode in` client, spawned by the ordinary [`HostProcesses`]
//! under the session's cgroup with a [`FirecodeConfinement`] whose `wrap` is
//! `firecode in --project P --cwd D` and an empty shell, so the job's command
//! string reaches the guest as one command. Jobs, scopes, monitors, promotions
//! and the reap log are the host implementation's, unchanged. What differs, and
//! `describe` says: killing a job kills the client; the guest command may run
//! on until `down`.
//!
//! # The child works on a copy
//!
//! firecode's shared-tree guard refuses to pack a main checkout that has other
//! worktrees and uncommitted tracked changes — which is exactly the state a
//! parent is in when it spawns a child. And a copy is firecode's own rule for a
//! child anyway ("a fresh workspace per child"). So [`FirecodeBackend::up`] takes
//! the workspace to copy and the directory to copy it under — never `/tmp`,
//! which is tmpfs and evaporates — and the guest's path is the copy's path.
//!
//! # Cold boots only, for now
//!
//! Checkpoint/restore is not dependable on the host that measured it (a
//! restored guest resumes in 4 ms and then exits cleanly — claude-host-lab's
//! note `01M2FR5A1VAJJ13M7S2XRRZK33`). The operator's direction is hierarchical
//! image caches so a boot is milliseconds; that is firecode's side of this
//! seam, and this backend will not notice when it lands.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use crate::backend::{BackendError, Command, DirEntry, ExecBackend, Output};
use crate::exec::{Boundary, ConfinePlan, Confinement, ExecError, HostProcesses};

/// Where the `firecode` CLI is. `$FIRECODE_BIN`, else `firecode` on `PATH`.
pub fn firecode_bin() -> PathBuf {
    std::env::var_os("FIRECODE_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("firecode"))
}

/// **Whether this host can place a session in a firecode VM at all**: the CLI that
/// [`firecode_bin`] names, found. `Err` says what was looked for and where.
///
/// Asked BEFORE a child is spawned, so `where: firecode` on a box without it is a refusal at
/// the call the model can act on, not a child that dies at its start with `running firecode:
/// No such file or directory`. MEASURED 2026-10-08 on the operator's Mac.
pub fn firecode_available() -> Result<PathBuf, String> {
    let bin = firecode_bin();
    let runnable = |p: &std::path::Path| {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    };
    if bin.components().count() > 1 {
        return if runnable(&bin) {
            Ok(bin)
        } else {
            Err(format!(
                "$FIRECODE_BIN names {}, which is not a program here",
                bin.display()
            ))
        };
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|d| d.join(&bin))
        .find(|p| runnable(p))
        .ok_or_else(|| "there is no `firecode` on this host's PATH (nor $FIRECODE_BIN)".to_string())
}

/// **The firecode this host would place a subagent with**: where it is, and whether it can
/// give a copy its source's layers. The daemon asks once at start and says it in the banner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub path: PathBuf,
    pub inherit: bool,
}

/// [`firecode_available`] and [`probe`] together, or `None` when there is no firecode.
pub fn installed() -> Option<Installed> {
    let path = firecode_available().ok()?;
    let inherit = probe(&path).inherit;
    Some(Installed { path, inherit })
}

/// The VMs this process's live backends hold, as `(firecode, copy)`. A backend registers when
/// its VM is up and leaves when it is brought down.
static LIVE: std::sync::Mutex<Vec<(PathBuf, PathBuf)>> = std::sync::Mutex::new(Vec::new());

/// **Bring down every VM a live backend in this process still holds** — the daemon's last act.
/// A subagent's backend lives on a thread that `process::exit` does not unwind, so its `Drop`
/// never runs and its VM outlived the daemon. firecode's own `down` delivers the work; the copy
/// is left where it is (the work may be in it), and a backend that is dropped later finds its
/// VM already down, which `down` takes.
pub fn down_all() {
    let live = std::mem::take(&mut *LIVE.lock().unwrap_or_else(|e| e.into_inner()));
    for (bin, project) in live {
        let _ = std::process::Command::new(&bin)
            .arg("down")
            .arg("--project")
            .arg(&project)
            .stdin(Stdio::null())
            .output();
    }
}

/// What the installed firecode can do, asked once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    /// `layer inherit`: a copy boots with its source's layers.
    pub inherit: bool,
}

/// **Ask `bin` what it can do.** The verbs are read off its own `layer` usage line, which
/// names every subcommand it has — no version number to keep in step with.
pub fn probe(bin: &Path) -> Probe {
    let usage = std::process::Command::new(bin)
        .args(["layer", "--help"])
        .stdin(Stdio::null())
        .output()
        .map(|o| {
            let mut t = String::from_utf8_lossy(&o.stdout).into_owned();
            t.push_str(&String::from_utf8_lossy(&o.stderr));
            t
        })
        .unwrap_or_default();
    Probe {
        inherit: usage.contains("inherit"),
    }
}

fn runnable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// The undo for a placement that did not finish: the copy goes, and the VM with it once one
/// was started. Disarmed by the one path that succeeds.
struct Unwind<'a> {
    bin: &'a Path,
    project: &'a Path,
    vm: bool,
    armed: bool,
}

impl Drop for Unwind<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if self.vm {
            let _ = std::process::Command::new(self.bin)
                .arg("down")
                .arg("--project")
                .arg(self.project)
                .stdin(Stdio::null())
                .output();
        }
        let _ = std::fs::remove_dir_all(self.project);
    }
}

/// The `Confinement` a firecode job runs under: the VM. `wrap` is the client
/// invocation, and the shell is empty because `firecode in` takes the command
/// string itself and runs it under `bash -lc` in the guest.
pub struct FirecodeConfinement {
    bin: PathBuf,
    project: PathBuf,
}

impl FirecodeConfinement {
    pub fn new(bin: PathBuf, project: PathBuf) -> Self {
        FirecodeConfinement { bin, project }
    }

    /// The argv in front of the command, for a listing or a test.
    pub fn prefix(&self, cwd: &Path) -> Vec<String> {
        vec![
            self.bin.display().to_string(),
            "in".into(),
            "--project".into(),
            self.project.display().to_string(),
            "--cwd".into(),
            cwd.display().to_string(),
        ]
    }
}

impl Confinement for FirecodeConfinement {
    fn describe(&self) -> String {
        format!(
            "a firecode VM holding a copy of {}: no host filesystem, no host processes, no \
             host devices. NOT confined: the guest's network (unless firecode was told \
             --no-net), and whatever firecode.layer installed. Killing a job kills the \
             `firecode in` client on the host; the guest command may run on until the VM \
             is brought down",
            self.project.display()
        )
    }

    /// `None`, deliberately. [`Boundary`] is the namespace-shaped measurement
    /// bwrap gets; a VM is not namespaces, and reporting one through that shape
    /// would be a measurement that was never taken. The VM fact is in
    /// `describe`, and the harness records the placement as confined by
    /// decision rather than by this reading.
    fn boundary(&self) -> Option<&Boundary> {
        None
    }

    fn wrap(&self, plan: &ConfinePlan<'_>) -> Result<Vec<String>, ExecError> {
        Ok(self.prefix(plan.cwd))
    }
}

/// One VM, one project copy, for the life of the backend.
pub struct FirecodeBackend {
    bin: PathBuf,
    /// The copy the guest holds, at this same path inside.
    project: PathBuf,
    /// Where the copy came from.
    source: PathBuf,
    processes: Option<Arc<HostProcesses>>,
    writable: bool,
    /// The sibling directory `down` will leave the guest's tree in, once known.
    landed: std::sync::Mutex<Option<PathBuf>>,
    /// The run id, from `firecode info --json`, for a listing.
    run: String,
    /// Why the copy has none of its source's layers, when it has none — a `layer inherit`
    /// this firecode does not have, or one that refused. Said in [`ExecBackend::describe`].
    layers: Option<String>,
}

impl std::fmt::Debug for FirecodeBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "FirecodeBackend({} from {})",
            self.project.display(),
            self.source.display()
        )
    }
}

/// What to boot.
#[derive(Debug, Clone)]
pub struct FirecodeSpec {
    /// The workspace to copy.
    pub source: PathBuf,
    /// Copies live under here — `~/.cache/letibot/firecode` by default.
    pub cache: PathBuf,
    /// A name for the copy: the child session's id.
    pub name: String,
    /// Names not to copy, **at any depth**: build output, other worktrees.
    ///
    /// Any depth, because the workspace is not always the repository. Measured
    /// 2026-09-16: the daemon's workspace was the folder ABOVE the checkout, the
    /// excludes were anchored at that folder, and `letibot/target` (38 GB) and
    /// `letibot/.claude/worktrees` (63 GB of agent worktrees, each with its own
    /// `target`) walked straight past them — 112 GB copied into the cache while
    /// the operator watched a yellow `task` card and read it as blocked.
    pub exclude: Vec<String>,
    /// The most the copy may weigh, in bytes, measured by a dry run before
    /// anything is written. A copy over this is refused with the size named,
    /// because a child that spends ten minutes copying build output is a child
    /// nobody asked for. `LETIBOT_FIRECODE_COPY_MAX` (bytes) overrides the
    /// default of 8 GiB.
    pub copy_max: u64,
    /// Handed to `firecode up` after `--project`: `--mem`, `--vcpu`, `--add-dir`
    /// for a toolchain the guest image lacks, `--host-port` for a server on the
    /// host's loopback. The operator's, verbatim — this backend does not know
    /// firecode's option table and does not pretend to.
    pub up_args: Vec<String>,
    /// `write` refused (a read-only downgrade).
    pub writable: bool,
    /// `processes()` present (an exec downgrade removes it).
    pub exec: bool,
    /// **The firecode CLI to run, when the caller names one.** `None` is [`firecode_available`]'s
    /// answer (`$FIRECODE_BIN`, else `firecode` on PATH). A field and not the environment so a
    /// caller — a test with a stub — can say which without racing every other reader of
    /// `$FIRECODE_BIN` in the process.
    pub bin: Option<PathBuf>,
}

impl FirecodeSpec {
    pub fn new(source: impl Into<PathBuf>, name: impl Into<String>) -> Self {
        let cache = std::env::var_os("HOME")
            .map(|h| {
                PathBuf::from(h)
                    .join(".cache")
                    .join("letibot")
                    .join("firecode")
            })
            .unwrap_or_else(|| PathBuf::from(".letibot-firecode"));
        FirecodeSpec {
            source: source.into(),
            cache,
            name: name.into(),
            exclude: vec![
                "target".into(),
                ".claude/worktrees".into(),
                "node_modules".into(),
                // The registrations of the source's other worktrees. A copy has
                // none of them, and firecode's shared-tree guard counts them:
                // measured 2026-09-14, a leticode subagent's copy of this
                // repository was refused as "the shared checkout of a repository
                // with 28 worktrees" — the checkout was a copy, the 28 were the
                // original's, and every one of them pointed back at a tree the
                // guest could not see.
                ".git/worktrees".into(),
            ],
            up_args: Vec::new(),
            writable: true,
            exec: true,
            bin: None,
            copy_max: std::env::var("LETIBOT_FIRECODE_COPY_MAX")
                .ok()
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(DEFAULT_COPY_MAX),
        }
    }
}

/// 8 GiB: room for any source tree, none for a build directory.
pub const DEFAULT_COPY_MAX: u64 = 8 * 1024 * 1024 * 1024;

impl FirecodeBackend {
    /// Copy the workspace, boot a VM on the copy, and wait until it takes
    /// commands. Every failure names the door: the copy, the boot, or the first
    /// command.
    pub fn up(spec: &FirecodeSpec) -> Result<FirecodeBackend, BackendError> {
        // **Resolved to an absolute path, once, here**, so every job runs the client the
        // placement found rather than looking `firecode` up again under the job's own
        // environment — which the exec layer clears. That clearing is what the tmux
        // acceptance spec `local/subagent-firecode.sh` caught on 2026-10-08: firecode
        // derived HOME with `getent`, which macOS lacks, and every command in the VM came
        // back `exit 127` (fixed in firecode). A missing CLI fails here, by name.
        let bin = match &spec.bin {
            Some(b) if runnable(b) => b.clone(),
            Some(b) => {
                return Err(BackendError::Io(format!(
                    "{} is not a program this host can run",
                    b.display()
                )));
            }
            None => firecode_available().map_err(BackendError::Io)?,
        };
        let project = spec.cache.join(&spec.name);
        if project.exists() {
            return Err(BackendError::Io(format!(
                "{} already exists; a child's copy is made once and named by its session",
                project.display()
            )));
        }
        std::fs::create_dir_all(&spec.cache).map_err(|e| BackendError::Io(e.to_string()))?;
        // **From here every failure takes the copy with it** — and the VM, once there is
        // one. A failed spawn used to leave its copy under the cache (FIRECODE-NOTES: *"the
        // very first attempt left 286 MB"*): the `?` on a spawn error returned past the
        // `remove_dir_all` that only a refused status reached.
        let mut unwind = Unwind {
            bin: &bin,
            project: &project,
            vm: false,
            armed: true,
        };
        copy_tree(&spec.source, &project, &spec.exclude, spec.copy_max)?;

        // Layers are attached to a PATH, and the copy has a path of its own. The
        // source's toolchain layers are what the guest needs to build this tree
        // — measured 2026-09-14: two models in copies of a repository whose guest
        // lacked llama.cpp and the sqlite dev symlink each spent ten minutes
        // stubbing them. A source with no layers is an answer, not a failure.
        //
        // **And a convenience, not a requirement** (FIRECODE-NOTES item 4): a firecode without
        // the verb, or one that refuses it, still boots the copy on its base image, and the
        // backend says so rather than refusing the placement.
        let inherit = std::process::Command::new(&bin)
            .arg("layer")
            .arg("inherit")
            .arg(&spec.source)
            .arg("--project")
            .arg(&project)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| BackendError::Io(format!("running {}: {e}", bin.display())))?;
        let layers = (!inherit.status.success()).then(|| {
            let said = String::from_utf8_lossy(&inherit.stderr);
            let why = if said.contains("usage:") && !said.contains("inherit") {
                "this firecode has no `layer inherit`".to_string()
            } else {
                format!("`layer inherit` refused: {}", said.trim())
            };
            format!("no layer inheritance ({why}); the guest builds on base-image toolchains")
        });

        let up = std::process::Command::new(&bin)
            .arg("up")
            .arg("--project")
            .arg(&project)
            .args(&spec.up_args)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| BackendError::Io(format!("running {}: {e}", bin.display())))?;
        if !up.status.success() {
            return Err(BackendError::Io(format!(
                "firecode up refused ({}): {}",
                up.status,
                String::from_utf8_lossy(&up.stderr).trim()
            )));
        }
        unwind.vm = true;
        let run = run_id(&bin, &project).unwrap_or_default();

        let processes = if spec.exec {
            let confine = FirecodeConfinement::new(bin.clone(), project.clone());
            let host = HostProcesses::confined(project.clone(), Box::new(confine))
                .map_err(|e| BackendError::Io(format!("process host for the VM: {e}")))?
                // The command string goes to `firecode in` as ONE argument, which the
                // guest runs under bash -lc. No shell in front of it here — **and so no
                // console shell either**: the operator's own run is handed to this host's
                // shell, and this host has none. The guest's `bash -lc` is what reads the
                // guest's own rc, and making the local argv interactive would put a second
                // shell in front of a command that already has one.
                .with_shell(Vec::new())
                .with_console_shell(Vec::new());
            Some(Arc::new(host))
        } else {
            None
        };
        unwind.armed = false;
        drop(unwind);
        LIVE.lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((bin.clone(), project.clone()));
        let b = FirecodeBackend {
            bin,
            project,
            source: spec.source.clone(),
            processes,
            writable: spec.writable,
            landed: std::sync::Mutex::new(None),
            run,
            layers,
        };
        // Prove the door works before handing the backend over: a VM that came
        // up and cannot take a command is a boot, not a backend.
        b.guest("true")
            .map_err(|e| BackendError::Io(format!("the VM is up but takes no command: {e}")))?;
        Ok(b)
    }

    pub fn project(&self) -> &Path {
        &self.project
    }

    /// The host-side process host, for the monitor registry it carries.
    pub fn host_processes(&self) -> Option<&Arc<HostProcesses>> {
        self.processes.as_ref()
    }

    pub fn run_id(&self) -> &str {
        &self.run
    }

    /// The sibling directory the guest's tree landed in after `down`, when known.
    pub fn landed(&self) -> Option<PathBuf> {
        self.landed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// One command in the guest, at the project root: stdout+stderr merged, and
    /// the exit status. Not through the job machinery — a file read is not a job.
    fn guest(&self, command: &str) -> Result<(Vec<u8>, i32), BackendError> {
        self.guest_in(&self.project, command)
    }

    fn guest_in(&self, cwd: &Path, command: &str) -> Result<(Vec<u8>, i32), BackendError> {
        let out = std::process::Command::new(&self.bin)
            .arg("in")
            .arg("--project")
            .arg(&self.project)
            .arg("--cwd")
            .arg(cwd)
            .arg(command)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| BackendError::Io(format!("firecode in: {e}")))?;
        let code = out.status.code().unwrap_or(-1);
        let mut bytes = out.stdout;
        if !out.stderr.is_empty() {
            bytes.extend_from_slice(&out.stderr);
        }
        Ok((bytes, code))
    }

    /// A path as the guest sees it: absolute stays, relative is under the copy.
    fn guest_path(&self, path: &str) -> PathBuf {
        let p = Path::new(path);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.project.join(path)
        }
    }

    /// firecode's own record of the run, when its runs directory can be found:
    /// `<checkout>/runs/<run>/result` is written when the guest's tree was copied
    /// out — or when there was nothing to copy.
    /// **firecode's record of this run's copy-out**, `runs/<run>/copy_out` beside the CLI:
    /// `pending` while it runs, then `done <path>`, `none` (nothing changed) or `failed …`.
    /// `None` when there is no record — a firecode older than the record, or no run id.
    fn copy_out(&self) -> Option<String> {
        let dir = self.run_dir()?;
        std::fs::read_to_string(dir.join("copy_out"))
            .ok()
            .map(|t| t.trim().to_string())
    }

    fn run_dir(&self) -> Option<PathBuf> {
        if self.run.is_empty() {
            return None;
        }
        let real = std::fs::canonicalize(&self.bin).ok()?;
        Some(real.parent()?.parent()?.join("runs").join(&self.run))
    }

    fn run_finished(&self) -> bool {
        if self.run.is_empty() {
            return false;
        }
        let Ok(real) = std::fs::canonicalize(&self.bin).or_else(|_| {
            // `firecode` on PATH: resolve it the way the shell would.
            std::env::var_os("PATH")
                .and_then(|p| {
                    std::env::split_paths(&p)
                        .map(|d| d.join(&self.bin))
                        .find(|c| c.is_file())
                })
                .ok_or(std::io::Error::other("not on PATH"))
                .and_then(std::fs::canonicalize)
        }) else {
            return false;
        };
        real.parent()
            .and_then(|bin| bin.parent())
            .map(|root| root.join("runs").join(&self.run).join("result").is_file())
            .unwrap_or(false)
    }

    /// Bring the VM down. Idempotent; called on drop. The copy the VM was booted
    /// on is removed afterwards: the result directory beside it is the record,
    /// and a copy per child would otherwise accumulate under the cache.
    pub fn down(&self) {
        LIVE.lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(_, p)| *p != self.project);
        let before = siblings(&self.project);
        let _ = std::process::Command::new(&self.bin)
            .arg("down")
            .arg("--project")
            .arg(&self.project)
            .stdin(Stdio::null())
            .output();
        // **firecode's own record decides, for as long as it says the copy-out is running.**
        // FIRECODE-NOTES item 1: the copy-out is asynchronous, and a fixed 10 s wait reported
        // a run's work as missing while it was still being delivered — it appeared two minutes
        // later. So while the record says `pending` this waits, up to COPY_OUT_CAP; `done
        // <path>` is where the work is; `none` is a guest that changed nothing; `failed …`
        // keeps the copy, which is then the only place the work is.
        //
        // A firecode with no record (older than it) is the old wait: the sibling appears a few
        // seconds after `down` returns, or firecode's run record says the run finished.
        let started = std::time::Instant::now();
        let mut recorded = false;
        loop {
            match self.copy_out() {
                Some(r) if r == "pending" => {
                    recorded = true;
                    if started.elapsed() >= COPY_OUT_CAP {
                        break;
                    }
                }
                Some(r) if r.starts_with("done ") => {
                    *self.landed.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(PathBuf::from(r["done ".len()..].trim()));
                    let _ = std::fs::remove_dir_all(&self.project);
                    return;
                }
                Some(r) if r == "none" => {
                    let _ = std::fs::remove_dir_all(&self.project);
                    return;
                }
                Some(_failed) => return,
                None if recorded => return,
                None => {
                    let now = siblings(&self.project);
                    if let Some(new) = now.into_iter().find(|s| !before.contains(s)) {
                        *self.landed.lock().unwrap_or_else(|e| e.into_inner()) = Some(new);
                        break;
                    }
                    if self.run_finished() || started.elapsed() >= LEGACY_WAIT {
                        break;
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        if self.landed().is_some() || self.run_finished() {
            let _ = std::fs::remove_dir_all(&self.project);
        }
    }
}

impl Drop for FirecodeBackend {
    fn drop(&mut self) {
        self.down();
    }
}

impl ExecBackend for FirecodeBackend {
    fn run(&self, cmd: &Command) -> Result<Output, BackendError> {
        if self.processes.is_none() {
            return Err(BackendError::Unsupported(
                "this subagent was downgraded to no exec, so nothing runs in its VM",
            ));
        }
        let joined = cmd
            .argv
            .iter()
            .map(|a| shell_quote(a))
            .collect::<Vec<_>>()
            .join(" ");
        let env_prefix = cmd
            .env
            .iter()
            .map(|(k, v)| format!("{}={} ", shell_quote(k), shell_quote(v)))
            .collect::<String>();
        let cwd = self.guest_path(&cmd.cwd);
        let (bytes, exit) = self.guest_in(&cwd, &format!("{env_prefix}{joined}"))?;
        Ok(Output {
            stdout: bytes,
            stderr: Vec::new(),
            exit,
        })
    }

    fn read(&self, path: &str) -> Result<Vec<u8>, BackendError> {
        let gp = self.guest_path(path);
        let q = shell_quote(&gp.display().to_string());
        let (bytes, exit) = self.guest(&format!(
            "if [ -d {q} ]; then echo __DIR__; exit 21; fi; base64 -w0 < {q}"
        ))?;
        match exit {
            0 => decode_base64(&bytes).map_err(|e| BackendError::Io(format!("{path}: {e}"))),
            21 => Err(BackendError::IsADirectory(path.to_string())),
            _ => Err(BackendError::NotFound(path.to_string())),
        }
    }

    fn write(&self, path: &str, bytes: &[u8]) -> Result<(), BackendError> {
        if !self.writable {
            return Err(BackendError::Unsupported(
                "this subagent was downgraded to no write; nothing in its VM changed",
            ));
        }
        let gp = self.guest_path(path);
        let dir = gp
            .parent()
            .map(|d| d.display().to_string())
            .unwrap_or_else(|| "/".into());
        let tmp = format!("{}.letibot-{}", gp.display(), std::process::id());
        // Small files go inline, base64 in the command; large ones through `cp`
        // into the temp name. Either way the final step is one rename.
        if bytes.len() <= INLINE_WRITE_BYTES {
            let b64 = encode_base64(bytes);
            let (out, exit) = self.guest(&format!(
                "mkdir -p {} && printf %s {} | base64 -d > {} && mv -f {} {}",
                shell_quote(&dir),
                shell_quote(&b64),
                shell_quote(&tmp),
                shell_quote(&tmp),
                shell_quote(&gp.display().to_string())
            ))?;
            if exit != 0 {
                return Err(BackendError::Io(format!(
                    "writing {path} in the VM: {}",
                    String::from_utf8_lossy(&out).trim()
                )));
            }
            return Ok(());
        }
        let staged = std::env::temp_dir().join(format!(
            "letibot-fc-{}-{}",
            std::process::id(),
            Path::new(&tmp)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        ));
        std::fs::write(&staged, bytes).map_err(|e| BackendError::Io(e.to_string()))?;
        let (out, exit) = self.guest(&format!("mkdir -p {}", shell_quote(&dir)))?;
        if exit != 0 {
            let _ = std::fs::remove_file(&staged);
            return Err(BackendError::Io(String::from_utf8_lossy(&out).into_owned()));
        }
        let cp = std::process::Command::new(&self.bin)
            .arg("cp")
            .arg(&staged)
            .arg(format!("vm:{dir}/"))
            .stdin(Stdio::null())
            .output()
            .map_err(|e| BackendError::Io(format!("firecode cp: {e}")))?;
        let _ = std::fs::remove_file(&staged);
        if !cp.status.success() {
            return Err(BackendError::Io(format!(
                "firecode cp into the VM: {}",
                String::from_utf8_lossy(&cp.stderr).trim()
            )));
        }
        let staged_name = Path::new(&staged)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let (out, exit) = self.guest(&format!(
            "mv -f {} {}",
            shell_quote(&format!("{dir}/{staged_name}")),
            shell_quote(&gp.display().to_string())
        ))?;
        if exit != 0 {
            return Err(BackendError::Io(String::from_utf8_lossy(&out).into_owned()));
        }
        Ok(())
    }

    fn processes(&self) -> Option<&dyn crate::exec::ProcessHost> {
        self.processes
            .as_deref()
            .map(|p| p as &dyn crate::exec::ProcessHost)
    }

    fn close(&self) -> Option<String> {
        self.down();
        // Said as what happened to the work, not as the VM's state: "the VM is
        // down" was read by the first model to see it as "the placement failed".
        Some(match self.landed() {
            Some(p) => format!(
                "the subagent's VM finished normally. Everything it wrote is in {} — a copy \
                 of {} as the guest left it. Nothing was applied to the source tree: diff \
                 it and take what you want",
                p.display(),
                self.source.display()
            ),
            None if self.run_finished() => format!(
                "the subagent's VM finished normally and changed nothing in its copy of {}, \
                 so firecode wrote no result directory",
                self.source.display()
            ),
            None => format!(
                "the subagent's VM was brought down, but neither a result directory beside \
                 {} nor firecode's record of the run appeared within 10 s; the run's console \
                 log under firecode's runs directory is the place to look",
                self.project.display()
            ),
        })
    }

    fn is_writable(&self) -> bool {
        self.writable
    }

    fn list(&self, path: &str) -> Result<Vec<DirEntry>, BackendError> {
        let gp = self.guest_path(path);
        let q = shell_quote(&gp.display().to_string());
        let (bytes, exit) = self.guest(&format!(
            "if [ ! -e {q} ]; then exit 22; fi; if [ ! -d {q} ]; then exit 23; fi; \
             find {q} -mindepth 1 -maxdepth 1 -printf '%y\\t%s\\t%f\\n'"
        ))?;
        match exit {
            0 => {}
            22 => return Err(BackendError::NotFound(path.to_string())),
            23 => return Err(BackendError::NotADirectory(path.to_string())),
            _ => {
                return Err(BackendError::Io(
                    String::from_utf8_lossy(&bytes).into_owned(),
                ));
            }
        }
        let mut out = Vec::new();
        for line in String::from_utf8_lossy(&bytes).lines() {
            let mut parts = line.splitn(3, '\t');
            let (Some(kind), Some(size), Some(name)) = (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            out.push(DirEntry {
                path: gp.join(name).display().to_string(),
                name: name.to_string(),
                is_dir: kind == "d",
                bytes: size.parse().unwrap_or(0),
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    fn stat(&self, path: &str) -> Option<DirEntry> {
        let gp = self.guest_path(path);
        let q = shell_quote(&gp.display().to_string());
        let (bytes, exit) = self.guest(&format!("stat --printf '%F\\t%s' {q}")).ok()?;
        if exit != 0 {
            return None;
        }
        let text = String::from_utf8_lossy(&bytes);
        let (kind, size) = text.trim().split_once('\t')?;
        Some(DirEntry {
            path: gp.display().to_string(),
            name: gp
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            is_dir: kind == "directory",
            bytes: size.parse().unwrap_or(0),
        })
    }

    fn describe(&self) -> String {
        format!(
            "firecode VM (run {}) holding a copy of {} at {}: the VM is the boundary — \
             no host filesystem, processes or devices; the network is the guest's. \
             {}{}. Files are the guest's and reach the host as a sibling of the copy at \
             down",
            if self.run.is_empty() { "?" } else { &self.run },
            self.source.display(),
            self.project.display(),
            if self.writable {
                "writable"
            } else {
                "READ-ONLY (downgraded)"
            },
            if self.processes.is_some() {
                ", exec through `firecode in`"
            } else {
                ", NO exec (downgraded)"
            },
        ) + &self
            .layers
            .as_deref()
            .map(|l| format!(". {l}"))
            .unwrap_or_default()
    }
}

/// How long `down` waits on a copy-out firecode says is still `pending`. Long, because the
/// work is in it; bounded, because a teardown that never returns holds the daemon's worker.
const COPY_OUT_CAP: std::time::Duration = std::time::Duration::from_secs(600);
/// The wait for a firecode that keeps no copy-out record: the sibling, or the run's record.
const LEGACY_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// Inline writes up to this many bytes ride in the command; larger go through `cp`.
const INLINE_WRITE_BYTES: usize = 48 * 1024;

fn run_id(bin: &Path, project: &Path) -> Option<String> {
    let out = std::process::Command::new(bin)
        .arg("info")
        .arg("--project")
        .arg(project)
        .arg("--json")
        .stdin(Stdio::null())
        .output()
        .ok()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    v.get("run")
        .or_else(|| v.get("FIRECODE_RUN"))
        .and_then(|r| r.as_str())
        .map(String::from)
}

/// `P-<stamp>` directories beside `P`.
fn siblings(project: &Path) -> Vec<PathBuf> {
    let Some(parent) = project.parent() else {
        return Vec::new();
    };
    let Some(name) = project
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
    else {
        return Vec::new();
    };
    let prefix = format!("{name}-");
    std::fs::read_dir(parent)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.is_dir()
                        && p.file_name()
                            .map(|n| n.to_string_lossy().starts_with(&prefix))
                            .unwrap_or(false)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Copy a tree, skipping `exclude` (relative to `from`). Symlinks are copied as
/// links; permissions are kept. `.git` is copied so the guest can commit and
/// diff, which is what a coding subagent does.
/// The rsync invocation both passes share, so the dry run measures exactly the
/// copy that would follow it. Excludes are **unanchored**: `target` matches a
/// `target` at any depth, `.claude/worktrees` any `.claude/worktrees`. A leading
/// slash would pin them to the transfer root, which is the mistake this replaces.
fn rsync_cmd(from: &Path, to: &Path, exclude: &[String]) -> std::process::Command {
    let mut cmd = std::process::Command::new("rsync");
    cmd.arg("-a");
    for e in exclude {
        cmd.arg("--exclude").arg(e.trim_start_matches('/'));
    }
    cmd.arg(format!("{}/", from.display())).arg(to);
    cmd.stdin(Stdio::null());
    cmd
}

/// What the copy would weigh, from `rsync --dry-run --stats`: the "Total file
/// size" line, digits only, so a locale that groups thousands does not matter.
fn measure_copy(from: &Path, to: &Path, exclude: &[String]) -> Result<u64, BackendError> {
    let out = rsync_cmd(from, to, exclude)
        .arg("--dry-run")
        .arg("--stats")
        .output()
        .map_err(|e| {
            BackendError::Io(format!(
                "rsync: {e} (rsync is required to copy the workspace)"
            ))
        })?;
    if !out.status.success() {
        return Err(BackendError::Io(format!(
            "measuring {}: {}",
            from.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    stdout
        .lines()
        .find_map(|l| l.trim_start().strip_prefix("Total file size:"))
        .map(|rest| {
            rest.chars()
                .filter(char::is_ascii_digit)
                .collect::<String>()
        })
        .and_then(|d| d.parse().ok())
        .ok_or_else(|| BackendError::Io("rsync --stats printed no `Total file size` line".into()))
}

fn human(bytes: u64) -> String {
    const G: f64 = 1024.0 * 1024.0 * 1024.0;
    const M: f64 = 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= G {
        format!("{:.1} GiB", b / G)
    } else if b >= M {
        format!("{:.0} MiB", b / M)
    } else {
        format!("{bytes} bytes")
    }
}

/// Copy `from` under `to`, after measuring it.
///
/// Measured first, and refused by size, because the failure this guards is not
/// an error rsync would ever report: a copy that succeeds after ten minutes and
/// 112 GB is, to rsync, a success.
fn copy_tree(from: &Path, to: &Path, exclude: &[String], max: u64) -> Result<(), BackendError> {
    if !from.is_dir() {
        return Err(BackendError::NotADirectory(from.display().to_string()));
    }
    let size = measure_copy(from, to, exclude)?;
    if size > max {
        return Err(BackendError::Io(format!(
            "refusing to copy {}: it weighs {} after excluding [{}], and the ceiling is {}. \
             A workspace this size is a folder of repositories or a tree with build output \
             under an unexpected name; point the session at the one repository, or raise \
             LETIBOT_FIRECODE_COPY_MAX on purpose",
            from.display(),
            human(size),
            exclude.join(", "),
            human(max)
        )));
    }
    let out = rsync_cmd(from, to, exclude).output().map_err(|e| {
        BackendError::Io(format!(
            "rsync: {e} (rsync is required to copy the workspace)"
        ))
    })?;
    if !out.status.success() {
        return Err(BackendError::Io(format!(
            "copying {} to {}: {}",
            from.display(),
            to.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}

/// POSIX single-quote quoting, the same policy the host backend uses.
fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./=:,+@%".contains(&b))
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// **Shared with `crate::media`**, which needs the same encoder for an image's `data:` URI.
///
/// `pub(crate)` rather than a second copy: this tree's whole ledger of defects tonight is one fact
/// written twice, and a base64 alphabet is exactly the kind of fact that can be spelled right in one
/// place and wrong in another. The decoder stays private — the only caller is the far side of a
/// `firecode` read.
/// **Delegates to the one alphabet in the tree** — `letibot_transcript::media::encode_base64`.
///
/// It was a second copy until the image channel needed the same encoder, and a base64 alphabet is
/// exactly the kind of fact that gets spelled right in one place and subtly wrong in another. The
/// decoder below stays here: its only caller is the far side of a `firecode` read.
fn encode_base64(bytes: &[u8]) -> String {
    letibot_transcript::media::encode_base64(bytes)
}

fn decode_base64(text: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut buf = 0u32;
    let mut bits = 0u32;
    for &c in text {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' | b'\n' | b'\r' | b' ' => continue,
            other => return Err(format!("not base64: byte {other:#x}")),
        };
        buf = (buf << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("letibot-firecode-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn file(p: &Path, bytes: usize) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, vec![b'x'; bytes]).unwrap();
    }

    /// **A stub firecode**: `<root>/bin/firecode`, a shell script whose `case "$1"` arms are
    /// `arms`, with `<root>/runs/` beside it the way the real checkout lays them out (the
    /// backend finds a run's record there through the binary's own path). Returns the binary.
    fn stub_firecode(root: &Path, arms: &str) -> PathBuf {
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::create_dir_all(root.join("runs")).unwrap();
        let bin = root.join("bin").join("firecode");
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\nROOT='{}'\ncase \"$1\" in\n{arms}\nesac\n",
                root.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        bin
    }

    /// The arms every stub needs: `up` boots, `info` names run `r1`, `layer inherit` works.
    const HAPPY: &str = "up) exit 0 ;;\n\
                         info) echo '{\"run\":\"r1\"}' ;;\n\
                         layer) [ \"$2\" = inherit ] && exit 0; echo 'usage: firecode layer [add <image>|ls|rm <image>|inherit <source-dir>]' >&2; exit 1 ;;";

    fn spec_with(src: &Path, cache: &Path, bin: &Path) -> FirecodeSpec {
        let mut spec = FirecodeSpec::new(src, "child");
        spec.cache = cache.to_path_buf();
        spec.exec = false;
        spec.bin = Some(bin.to_path_buf());
        spec
    }

    /// **A placement that fails leaves no copy behind** — FIRECODE-NOTES: *"A failed spawn leaks
    /// its copy. The very first attempt left 286 MB in ~/.cache/letibot/firecode/; the error
    /// path remove_dir_alls on a refused status but not on a spawn error."*
    #[test]
    fn a_firecode_that_cannot_be_run_leaves_no_copy_behind() {
        let root = tmp("unrunnable");
        let src = root.join("src");
        file(&src.join("a.txt"), 10);
        // A directory where the program should be: running it is a spawn error.
        let bin = root.join("not-a-program");
        std::fs::create_dir_all(&bin).unwrap();
        let cache = root.join("cache");
        let r = FirecodeBackend::up(&spec_with(&src, &cache, &bin));
        assert!(r.is_err(), "a firecode that cannot run placed a VM");
        assert!(
            !cache.join("child").exists(),
            "the copy outlived the failed placement: {:?}",
            r.err()
        );
    }

    /// **No `layer inherit` is a missing convenience, not a missing VM** — FIRECODE-NOTES item 4:
    /// *"degrade where only a convenience is missing ("no layer inheritance; the guest builds on
    /// base-image toolchains") and refuse only what is genuinely required."*
    #[test]
    fn a_firecode_without_layer_inherit_still_places_the_vm_and_says_so() {
        let root = tmp("noinherit");
        let src = root.join("src");
        file(&src.join("a.txt"), 10);
        let bin = stub_firecode(
            &root.join("fc"),
            "up) exit 0 ;;\n\
             info) echo '{\"run\":\"r1\"}' ;;\n\
             layer) echo 'usage: firecode layer [add <image>|ls|rm <image>] [--project DIR]' >&2; exit 1 ;;",
        );
        let b = FirecodeBackend::up(&spec_with(&src, &root.join("cache"), &bin))
            .expect("the VM is placed without inherited layers");
        assert!(
            b.describe().contains("no layer inheritance"),
            "the missing layers are not said: {}",
            b.describe()
        );
    }

    /// **`down` waits while firecode says the copy-out is still running, however long that is**
    /// — FIRECODE-NOTES item 1: *"a child answered … wrote 'neither a result directory … nor
    /// firecode's record appeared within 10 s' — and the directory appeared about two minutes
    /// later."* The stub's copy-out takes 12 s; the old wait gave up at 10.
    #[test]
    fn down_waits_while_the_copy_out_is_pending() {
        let root = tmp("pending");
        let src = root.join("src");
        file(&src.join("a.txt"), 10);
        let fc = root.join("fc");
        let delivered = root.join("delivered");
        let arms = format!(
            "{HAPPY}\n\
             down) mkdir -p \"$ROOT/runs/r1\"; echo pending > \"$ROOT/runs/r1/copy_out\"; \
             (sleep 12; mkdir -p '{d}'; echo 'done {d}' > \"$ROOT/runs/r1/copy_out.t\"; \
             mv \"$ROOT/runs/r1/copy_out.t\" \"$ROOT/runs/r1/copy_out\") >/dev/null 2>&1 & exit 0 ;;",
            d = delivered.display()
        );
        let bin = stub_firecode(&fc, &arms);
        let b = FirecodeBackend::up(&spec_with(&src, &root.join("cache"), &bin)).unwrap();
        b.down();
        assert_eq!(
            b.landed().as_deref(),
            Some(delivered.as_path()),
            "down gave up on a copy-out firecode said was still running"
        );
    }

    /// **The record names where the work went**, so the backend does not have to guess from
    /// the directory names beside the copy.
    #[test]
    fn down_takes_the_delivered_path_from_the_run_record() {
        let root = tmp("record");
        let src = root.join("src");
        file(&src.join("a.txt"), 10);
        let fc = root.join("fc");
        let delivered = root.join("elsewhere").join("work");
        let arms = format!(
            "{HAPPY}\n\
             down) mkdir -p \"$ROOT/runs/r1\" '{d}'; echo 'done {d}' > \"$ROOT/runs/r1/copy_out\"; exit 0 ;;",
            d = delivered.display()
        );
        let bin = stub_firecode(&fc, &arms);
        let b = FirecodeBackend::up(&spec_with(&src, &root.join("cache"), &bin)).unwrap();
        b.down();
        assert_eq!(b.landed().as_deref(), Some(delivered.as_path()));
        assert!(
            !root.join("cache").join("child").exists(),
            "the copy is removed once the work landed"
        );
    }

    /// **A daemon that stops brings its children's VMs down** — the backends of subagents run on
    /// threads `process::exit` does not unwind, so their `Drop` never ran and the VMs outlived
    /// the daemon (two of the acceptance suite's own runs, 2026-10-08). `down_all` is the
    /// explicit half the daemon calls on its way out.
    #[test]
    fn down_all_brings_down_a_vm_whose_backend_is_still_alive() {
        let root = tmp("downall");
        let src = root.join("src");
        file(&src.join("a.txt"), 10);
        let marker = root.join("down-was-called");
        let arms = format!(
            "{HAPPY}\n\
             down) touch '{m}'; mkdir -p \"$ROOT/runs/r1\"; echo none > \"$ROOT/runs/r1/copy_out\"; exit 0 ;;",
            m = marker.display()
        );
        let bin = stub_firecode(&root.join("fc"), &arms);
        let b = FirecodeBackend::up(&spec_with(&src, &root.join("cache"), &bin)).unwrap();
        down_all();
        assert!(
            marker.exists(),
            "the VM of a live backend was not brought down"
        );
        drop(b);
    }

    /// **What the installed firecode can do, asked once** — for the daemon's start-up banner.
    #[test]
    fn the_probe_says_whether_layer_inherit_is_there() {
        let root = tmp("probe");
        let with = stub_firecode(&root.join("with"), HAPPY);
        let without = stub_firecode(
            &root.join("without"),
            "layer) echo 'usage: firecode layer [add <image>|ls|rm <image>] [--project DIR]' >&2; exit 1 ;;",
        );
        assert!(probe(&with).inherit, "{:?}", probe(&with));
        assert!(!probe(&without).inherit, "{:?}", probe(&without));
    }

    /// The 2026-09-16 shape: the workspace is the folder ABOVE the checkout, so
    /// the build output and the agent worktrees are one level down. Every one of
    /// them must be left behind, at that depth and deeper.
    #[test]
    fn excludes_apply_at_any_depth_not_only_at_the_root() {
        let src = tmp("src");
        let dst = tmp("dst").join("copy");
        file(&src.join("repo/crates/a/src/lib.rs"), 10);
        file(&src.join("repo/target/release/big"), 5000);
        file(&src.join("repo/.claude/worktrees/agent-1/target/x"), 5000);
        file(&src.join("repo/.claude/worktrees/agent-1/src/y.rs"), 10);
        file(&src.join("repo/web/node_modules/pkg/index.js"), 5000);
        file(&src.join("target/root-level"), 5000);

        let exclude = FirecodeSpec::new(&src, "t").exclude;
        copy_tree(&src, &dst, &exclude, u64::MAX).unwrap();

        assert!(dst.join("repo/crates/a/src/lib.rs").exists());
        assert!(
            !dst.join("repo/target").exists(),
            "nested target was copied"
        );
        assert!(
            !dst.join("repo/.claude/worktrees").exists(),
            "nested worktrees were copied"
        );
        assert!(
            !dst.join("repo/web/node_modules").exists(),
            "nested node_modules was copied"
        );
        assert!(!dst.join("target").exists(), "root target was copied");
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(dst.parent().unwrap());
    }

    /// A copy over the ceiling is refused BEFORE anything is written, with the
    /// size and the ceiling named.
    #[test]
    fn a_copy_over_the_ceiling_is_refused_with_its_size_named_and_nothing_written() {
        let src = tmp("big");
        let dst = tmp("bigdst").join("copy");
        file(&src.join("repo/data/blob"), 200_000);
        let exclude = FirecodeSpec::new(&src, "t").exclude;

        let err = copy_tree(&src, &dst, &exclude, 100_000)
            .unwrap_err()
            .to_string();
        assert!(err.contains("refusing to copy"), "{err}");
        assert!(
            err.contains("195 MiB") || err.contains("200000 bytes"),
            "{err}"
        );
        assert!(err.contains("LETIBOT_FIRECODE_COPY_MAX"), "{err}");
        assert!(!dst.exists(), "the refusal wrote the copy anyway");

        // The same tree under the ceiling copies.
        copy_tree(&src, &dst, &exclude, 1_000_000).unwrap();
        assert!(dst.join("repo/data/blob").exists());
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(dst.parent().unwrap());
    }

    /// The measurement counts what the copy would carry, not what the excludes
    /// leave behind — otherwise the ceiling would refuse the very copies the
    /// excludes made small.
    #[test]
    fn the_measurement_honours_the_excludes() {
        let src = tmp("meas");
        let dst = tmp("measdst").join("copy");
        file(&src.join("repo/src/main.rs"), 100);
        file(&src.join("repo/target/huge"), 900_000);
        let exclude = FirecodeSpec::new(&src, "t").exclude;
        let size = measure_copy(&src, &dst, &exclude).unwrap();
        assert!(size < 10_000, "excluded build output was measured: {size}");
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(dst.parent().unwrap());
    }

    #[test]
    fn base64_round_trips_binary_and_the_paddings() {
        for input in [
            &b""[..],
            b"f",
            b"fo",
            b"foo",
            b"foob",
            b"fooba",
            b"foobar",
            &[0u8, 255, 10, 13, 128][..],
        ] {
            let e = encode_base64(input);
            assert_eq!(decode_base64(e.as_bytes()).unwrap(), input, "{e}");
        }
        assert_eq!(encode_base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(encode_base64(b"fo"), "Zm8=");
    }

    #[test]
    fn the_confinement_wraps_a_job_as_firecode_in_with_the_cwd() {
        let c =
            FirecodeConfinement::new(PathBuf::from("/usr/bin/firecode"), PathBuf::from("/w/copy"));
        let plan = ConfinePlan {
            cwd: Path::new("/w/copy/sub"),
            env: &[],
        };
        assert_eq!(
            c.wrap(&plan).unwrap(),
            vec![
                "/usr/bin/firecode",
                "in",
                "--project",
                "/w/copy",
                "--cwd",
                "/w/copy/sub"
            ]
        );
        assert!(c.describe().contains("NOT confined"));
        assert!(c.boundary().is_none());
    }

    #[test]
    fn quoting_leaves_plain_words_and_wraps_the_rest() {
        assert_eq!(shell_quote("cargo"), "cargo");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote(""), "''");
    }
}
