//! The firecode backend: a subagent's tools run inside a VM while the model stays on the host.
//! The VM is the boundary, so a session placed here runs at allow-all; files are the guest's,
//! a job is the `firecode in` client, and the child works on a copy of the one repository.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use crate::backend::{BackendError, Command, DirEntry, ExecBackend, Output};
use crate::exec::{Boundary, ConfinePlan, Confinement, ExecError, HostProcesses};

pub mod held;

/// Where the `firecode` CLI is. `$FIRECODE_BIN`, else `firecode` on `PATH`.
pub fn firecode_bin() -> PathBuf {
    std::env::var_os("FIRECODE_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("firecode"))
}

/// Whether this host can place a session in a firecode VM at all: the CLI that [`firecode_bin`]
/// names, found. `Err` says what was looked for and where. Asked before a child is spawned, so a
/// box without it is a refusal at the call the model can act on.
pub fn firecode_available() -> Result<PathBuf, String> {
    firecode_available_at(firecode_bin())
}

/// [`firecode_available`] for a CLI the caller names, rather than the one `$FIRECODE_BIN` names.
/// A seam a test uses so it never has to set that variable, which every other test in the binary
/// would read at the same moment.
pub fn firecode_available_at(bin: PathBuf) -> Result<PathBuf, String> {
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

/// The firecode this host would place a subagent with: where it is, and whether it can
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

/// Bring down every VM a live backend in this process still holds, the daemon's last act: a
/// subagent's backend lives on a thread `process::exit` does not unwind, so its `Drop` never runs.
/// All at once, because four `down`s in a row outlast a forced stop's five-second grace.
pub fn down_all() {
    let live = std::mem::take(&mut *LIVE.lock().unwrap_or_else(|e| e.into_inner()));
    let downs: Vec<_> = live
        .into_iter()
        .map(|(bin, project)| {
            std::thread::spawn(move || {
                // Bounded: this runs at the daemon's last act, and a `down` whose client never
                // answers would hold the stop for ever - which is the whole reason they are run
                // at once.
                let _ = crate::rpc::run_bounded(
                    &bin,
                    &[
                        "down".to_string(),
                        "--project".to_string(),
                        project.display().to_string(),
                    ],
                    CLIENT_TIMEOUT,
                );
            })
        })
        .collect();
    for d in downs {
        let _ = d.join();
    }
}

/// What the installed firecode can do, asked once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    /// `layer inherit`: a copy boots with its source's layers.
    pub inherit: bool,
}

/// Ask `bin` what it can do. The verbs are read off its own `layer` usage line, which
/// names every subcommand it has - no version number to keep in step with.
pub fn probe(bin: &Path) -> Probe {
    // Bounded ([`CLIENT_TIMEOUT`]): a client call asked once at start, and the banner it feeds
    // must not be what holds a daemon's startup.
    let usage = match crate::rpc::run_bounded(
        bin,
        &["layer".to_string(), "--help".to_string()],
        CLIENT_TIMEOUT,
    ) {
        crate::rpc::Ended::Answered(o) => {
            let mut t = String::from_utf8_lossy(&o.stdout).into_owned();
            t.push_str(&String::from_utf8_lossy(&o.stderr));
            t
        }
        _ => String::new(),
    };
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
    /// The project is a copy this backend made, and so is its to remove. `false` for a mounted
    /// worktree ([`mounts`]): the project is the operator's own worktree.
    remove: bool,
}

impl Drop for Unwind<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if self.vm {
            // Bounded, because this is an undo and an undo that never returns is a placement that
            // never fails: a client waiting on a reply that is not coming is exactly the shape
            // [`CLIENT_TIMEOUT`] exists for.
            let _ = crate::rpc::run_bounded(
                self.bin,
                &[
                    "down".to_string(),
                    "--project".to_string(),
                    self.project.display().to_string(),
                ],
                CLIENT_TIMEOUT,
            );
        }
        if self.remove {
            let _ = std::fs::remove_dir_all(self.project);
        }
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

    /// `None`, deliberately: [`Boundary`] is the namespace-shaped measurement bwrap gets, and a
    /// VM reported through that shape would be a measurement never taken. `describe` carries the
    /// VM fact.
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
    /// Why the copy has none of its source's layers, when it has none - a `layer inherit`
    /// this firecode does not have, or one that refused. Said in [`ExecBackend::describe`].
    layers: Option<String>,
    /// [`FirecodeSpec::verify`], kept to name the check beside its verdict.
    verify: Option<String>,
    /// Whether this backend brought the VM up and so brings it down. `false` for a session
    /// attached to an ancestor's VM ([`FirecodeBackend::attach`]): the VM is the ancestor's, and a
    /// child that ends must not take its parent's machine with it.
    owns: bool,
    /// The project is the source itself, mounted, never a copy ([`mounts`]): a git worktree
    /// on a Mac, which firecode shares into the guest read-write. Nothing of it is ever removed
    /// here, and its work is where it always was.
    mounted: bool,
    /// The project is a whole folder mounted read-write ([`FirecodeSpec::mount_rw`]), not a
    /// worktree - said differently in the answer.
    mount_rw: bool,
    /// The run of an already-running VM this backend joined instead of booting one, when it did.
    /// Joined is not owned: the VM is brought down by whoever started it.
    joined: Option<String>,
    /// The wall clock on one control command in the guest, a field only so a test that measures
    /// the deadline can set it to a second instead of waiting out the constant.
    guest_timeout: Duration,
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

/// Whether a VM placement on `source` mounts it rather than copying it: a git worktree on a Mac
/// is shared into the guest read-write, so the child works in the real worktree. Anything else is
/// copied. A worktree, not a submodule: only a worktree's `.git` names a `worktrees/` directory.
pub fn mounts(source: &Path) -> bool {
    if !cfg!(target_os = "macos") {
        return false;
    }
    std::fs::read_to_string(source.join(".git"))
        .map(|t| {
            t.lines()
                .next()
                .and_then(|l| l.strip_prefix("gitdir:"))
                .is_some_and(|d| d.contains("/worktrees/"))
        })
        .unwrap_or(false)
}

/// Where the VM of a session placed on `source` holds its project - the source itself when it
/// is mounted ([`mounts`]), the session's copy under the cache otherwise. What a child attaching
/// to that VM names it by.
pub fn project_for(source: &Path, session_id: &str) -> PathBuf {
    if mounts(source) {
        source.to_path_buf()
    } else {
        FirecodeBackend::project_of(session_id)
    }
}

/// The run of a VM already up with `folder` itself mounted read-write, if there is one - asked
/// of firecode (`info --json`, whose `mount` says how a run holds its project), so a VM holding a
/// COPY of the folder is never mistaken for one holding the folder.
fn running_rw(bin: &Path, folder: &Path) -> Option<String> {
    // Bounded ([`CLIENT_TIMEOUT`]): `info` is a client call, and a placement cannot wait for ever
    // on one.
    let out = match crate::rpc::run_bounded(
        bin,
        &[
            "info".to_string(),
            "--json".to_string(),
            "--project".to_string(),
            folder.display().to_string(),
        ],
        CLIENT_TIMEOUT,
    ) {
        crate::rpc::Ended::Answered(out) => out,
        _ => return None,
    };
    if !out.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    if v.get("mount").and_then(|m| m.as_str()) != Some("rw") {
        return None;
    }
    let held = v.get("project").and_then(|p| p.as_str())?;
    let same = std::fs::canonicalize(held).ok()? == std::fs::canonicalize(folder).ok()?;
    let run = v.get("run").and_then(|r| r.as_str()).unwrap_or_default();
    (same && !run.is_empty()).then(|| run.to_string())
}

/// What to boot.
#[derive(Debug, Clone)]
pub struct FirecodeSpec {
    /// The tree to copy: the one repository the work is in, and not the directory a session was
    /// started in when the two are different, because the copy is the guest's whole world. Filled
    /// by `Config::vm_copy_source`, the one resolution of what this session's VM copies.
    pub source: PathBuf,
    /// Copies live under here - `~/.cache/letibot/firecode` by default.
    pub cache: PathBuf,
    /// A name for the copy: the child session's id.
    pub name: String,
    /// Names not to copy at any depth: build output and other worktrees, because the workspace is
    /// not always the repository, so an exclude anchored at it would walk past `letibot/target`
    /// and the worktrees beside it.
    pub exclude: Vec<String>,
    /// The most the copy may weigh, in bytes, measured by a dry run before anything is written. A
    /// copy over this is refused with the size named. `LETIBOT_FIRECODE_COPY_MAX` overrides the
    /// default of 8 GiB.
    pub copy_max: u64,
    /// Handed to `firecode up` after `--project`: `--mem`, `--vcpu`, `--add-dir`, `--host-port`.
    /// The operator's, verbatim: this backend does not know firecode's option table.
    pub up_args: Vec<String>,
    /// `write` refused (a read-only downgrade).
    pub writable: bool,
    /// `processes()` present (an exec downgrade removes it).
    pub exec: bool,
    /// The firecode CLI to run, when the caller names one. `None` is [`firecode_available`]'s
    /// answer. A field and not the environment, so a test with a stub can say which without racing
    /// every other reader of `$FIRECODE_BIN` in the process.
    pub bin: Option<PathBuf>,
    /// The acceptance check, handed to `firecode up --verify`: firecode runs it in the project
    /// when the VM is brought down and records its exit status, which [`ExecBackend::close`]
    /// reports, so a placed child's "done" is not the child's word.
    pub verify: Option<String>,
    /// The source itself, mounted read-write, never a copy (`firecode up --mount-rw`): the folder
    /// the session works in, so edits, worktrees beside a repository and commits land on the host
    /// as they are made. A VM already running with it mounted is joined, not booted again.
    pub mount_rw: bool,
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
                // The registrations of the source's other worktrees. A copy has none of them,
                // and firecode's shared-tree guard counts them: a copy of this repository was
                // refused as the shared checkout of a repository with 28 worktrees.
                ".git/worktrees".into(),
            ],
            up_args: Vec::new(),
            writable: true,
            exec: true,
            bin: None,
            verify: None,
            mount_rw: false,
            copy_max: std::env::var("LETIBOT_FIRECODE_COPY_MAX")
                .ok()
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(DEFAULT_COPY_MAX),
        }
    }
}

/// 8 GiB: room for one repository, and none for a folder of them. The one repository the work is
/// in measures about 1.2 GiB after the excludes, while the folder above it measures 72.8 GiB, and
/// that is the shape the ceiling exists to refuse. `LETIBOT_FIRECODE_COPY_MAX` is the override.
pub const DEFAULT_COPY_MAX: u64 = 8 * 1024 * 1024 * 1024;

impl FirecodeBackend {
    /// Copy the workspace, boot a VM on the copy, and wait until it takes
    /// commands. Every failure names the door: the copy, the boot, or the first
    /// command.
    pub fn up(spec: &FirecodeSpec) -> Result<FirecodeBackend, BackendError> {
        // Resolved to an absolute path, once, here, so every job runs the client the placement
        // found rather than looking `firecode` up again under the job's own environment, which
        // the exec layer clears. A missing CLI fails here, by name.
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
        // The folder itself, read-write: joined if a VM already holds it mounted, booted with
        // `--mount-rw` otherwise. Never a copy, never removed - it is the operator's folder.
        if spec.mount_rw
            && let Some(run) = running_rw(&bin, &spec.source)
        {
            let mut b =
                FirecodeBackend::attach(Some(bin), spec.source.clone(), spec.writable, spec.exec)?;
            b.mounted = true;
            b.mount_rw = true;
            b.joined = Some(run);
            return Ok(b);
        }
        let mounted = spec.mount_rw || mounts(&spec.source);
        let project = if mounted {
            spec.source.clone()
        } else {
            spec.cache.join(&spec.name)
        };
        if !mounted && project.exists() {
            return Err(BackendError::Io(format!(
                "{} already exists; a child's copy is made once and named by its session",
                project.display()
            )));
        }
        if !mounted {
            std::fs::create_dir_all(&spec.cache).map_err(|e| BackendError::Io(e.to_string()))?;
        }
        // From here every failure takes the copy with it, and the VM once there is one: the `?`
        // on a spawn error would otherwise return past the `remove_dir_all` that only a refused
        // status reaches.
        let mut unwind = Unwind {
            bin: &bin,
            project: &project,
            vm: false,
            armed: true,
            remove: !mounted,
        };
        if !mounted {
            copy_tree(&spec.source, &project, &spec.exclude, spec.copy_max)?;
        }

        // Layers are attached to a PATH, and the copy has a path of its own: the source's
        // toolchain layers are what the guest needs to build this tree. A firecode without the
        // verb still boots the copy on its base image. Mounted, there is nothing to inherit onto.
        let layers = if mounted {
            None
        } else {
            let args = vec![
                "layer".to_string(),
                "inherit".to_string(),
                spec.source.display().to_string(),
                "--project".to_string(),
                project.display().to_string(),
            ];
            match crate::rpc::run_bounded(&bin, &args, LAYER_TIMEOUT) {
                crate::rpc::Ended::Answered(out) if out.status.success() => None,
                crate::rpc::Ended::Answered(out) => {
                    let said = String::from_utf8_lossy(&out.stderr).into_owned();
                    let why = if said.contains("usage:") && !said.contains("inherit") {
                        "this firecode has no `layer inherit`".to_string()
                    } else {
                        format!("`layer inherit` refused: {}", said.trim())
                    };
                    Some(format!(
                        "no layer inheritance ({why}); the guest builds on base-image toolchains"
                    ))
                }
                crate::rpc::Ended::TimedOut => Some(format!(
                    "no layer inheritance (`layer inherit` did not answer within {} s, and was \
                     killed with everything it had started); the guest builds on base-image \
                     toolchains",
                    LAYER_TIMEOUT.as_secs()
                )),
                crate::rpc::Ended::Unstartable(why) => Some(format!(
                    "no layer inheritance ({why}); the guest builds on base-image toolchains"
                )),
            }
        };

        let mut args = vec![
            "up".to_string(),
            "--project".to_string(),
            project.display().to_string(),
        ];
        args.extend(spec.up_args.iter().cloned());
        if spec.mount_rw {
            args.push("--mount-rw".to_string());
        }
        if let Some(check) = &spec.verify {
            args.push("--verify".to_string());
            args.push(check.clone());
        }
        let up = match crate::rpc::run_bounded(&bin, &args, UP_TIMEOUT) {
            crate::rpc::Ended::Answered(out) => out,
            crate::rpc::Ended::TimedOut => {
                // A boot nobody can see the end of is not a VM to hand to a session. The run
                // may exist, so the unwind below is told to bring one down - bounded since
                // [`CLIENT_TIMEOUT`] is what `down` is given - and the copy goes with it.
                unwind.vm = true;
                return Err(BackendError::Io(format!(
                    "`firecode up` did not answer within {} s: it was killed together with \
                     everything it had started, the VM it may have booted is being brought down, \
                     and the copy of {} it was booted on is removed. Nothing was placed",
                    UP_TIMEOUT.as_secs(),
                    spec.source.display()
                )));
            }
            crate::rpc::Ended::Unstartable(why) => return Err(BackendError::Io(why)),
        };
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
                // The command string goes to `firecode in` as ONE argument, which the guest runs
                // under bash -lc, so there is no shell in front of it here and no console shell
                // either: a second shell would be put in front of a command that already has one.
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
            verify: spec.verify.clone(),
            owns: true,
            mounted,
            mount_rw: spec.mount_rw,
            joined: None,
            guest_timeout: GUEST_TIMEOUT,
        };
        // Prove the door works before handing the backend over: a VM that came
        // up and cannot take a command is a boot, not a backend.
        b.guest("true")
            .map_err(|e| BackendError::Io(format!("the VM is up but takes no command: {e}")))?;
        Ok(b)
    }

    /// A session in a VM another session brought up: a child of a VM-placed session runs in that
    /// same VM. No copy, no boot, no layers of its own: the tree, the toolchain and the memory
    /// are the parent's. Never brought down by this backend ([`Self::owns`]).
    pub fn attach(
        bin: Option<PathBuf>,
        project: PathBuf,
        writable: bool,
        exec: bool,
    ) -> Result<FirecodeBackend, BackendError> {
        let bin = match bin {
            Some(b) if runnable(&b) => b,
            Some(b) => {
                return Err(BackendError::Io(format!(
                    "{} is not a program this host can run",
                    b.display()
                )));
            }
            None => firecode_available().map_err(BackendError::Io)?,
        };
        let run = run_id(&bin, &project).unwrap_or_default();
        let processes = if exec {
            let confine = FirecodeConfinement::new(bin.clone(), project.clone());
            let host = HostProcesses::confined(project.clone(), Box::new(confine))
                .map_err(|e| BackendError::Io(format!("process host for the VM: {e}")))?
                .with_shell(Vec::new())
                .with_console_shell(Vec::new());
            Some(Arc::new(host))
        } else {
            None
        };
        let b = FirecodeBackend {
            bin,
            source: project.clone(),
            project,
            processes,
            writable,
            landed: std::sync::Mutex::new(None),
            run,
            layers: None,
            verify: None,
            owns: false,
            mounted: false,
            mount_rw: false,
            joined: None,
            guest_timeout: GUEST_TIMEOUT,
        };
        // The parent's VM has to be up and taking commands, or there is nothing to run in.
        b.guest("true").map_err(|e| {
            BackendError::Io(format!(
                "the parent's VM takes no command (is it still up?): {e}"
            ))
        })?;
        Ok(b)
    }

    /// Where a session's own VM's copy lives - the project `up` names after it.
    pub fn project_of(session_id: &str) -> PathBuf {
        FirecodeSpec::new(PathBuf::new(), session_id)
            .cache
            .join(session_id)
    }

    /// Whether this backend brought its VM up - `false` when attached to an ancestor's.
    pub fn owns_vm(&self) -> bool {
        self.owns
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

    /// The deadline this backend gives one control command in the guest, as a test may set
    /// it - production never calls this, which is why the default is the constant rather than a
    /// field a caller has to fill in (the same shape as [`crate::remote::Ssh::with_bound`]).
    #[cfg(test)]
    fn with_guest_timeout(mut self, within: Duration) -> Self {
        self.guest_timeout = within;
        self
    }

    /// The sibling directory the guest's tree landed in after `down`, when known.
    pub fn landed(&self) -> Option<PathBuf> {
        self.landed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// One command in the guest, at the project root: stdout+stderr merged, and
    /// the exit status. Not through the job machinery - a file read is not a job.
    fn guest(&self, command: &str) -> Result<(Vec<u8>, i32), BackendError> {
        self.guest_in(&self.project, command)
    }

    fn guest_in(&self, cwd: &Path, command: &str) -> Result<(Vec<u8>, i32), BackendError> {
        self.guest_within(cwd, command, self.guest_timeout)
    }

    /// One command in the guest, under a wall clock: `firecode in` waits for the guest's reply
    /// with no deadline of its own, so a lost reply is indistinguishable from a slow command.
    /// [`crate::rpc::run_bounded`] kills the whole process group at the deadline.
    fn guest_within(
        &self,
        cwd: &Path,
        command: &str,
        within: Duration,
    ) -> Result<(Vec<u8>, i32), BackendError> {
        let args = vec![
            "in".to_string(),
            "--project".to_string(),
            self.project.display().to_string(),
            "--cwd".to_string(),
            cwd.display().to_string(),
            command.to_string(),
        ];
        match crate::rpc::run_bounded(&self.bin, &args, within) {
            crate::rpc::Ended::Answered(out) => {
                let code = out.status.code().unwrap_or(-1);
                let mut bytes = out.stdout;
                if !out.stderr.is_empty() {
                    bytes.extend_from_slice(&out.stderr);
                }
                Ok((bytes, code))
            }
            crate::rpc::Ended::TimedOut => Err(BackendError::Io(guest_lost_its_reply(cwd, within))),
            crate::rpc::Ended::Unstartable(why) => {
                Err(BackendError::Io(format!("firecode in: {why}")))
            }
        }
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

    /// firecode's record of this run's copy-out, `runs/<run>/copy_out`: `pending` while it runs,
    /// then `done <path>`, `none` (nothing changed) or `failed`. `None` when there is no record.
    fn copy_out(&self) -> Option<String> {
        let dir = self.run_dir()?;
        std::fs::read_to_string(dir.join("copy_out"))
            .ok()
            .map(|t| t.trim().to_string())
    }

    /// The acceptance check's verdict, as firecode recorded it: `runs/<run>/verify-status`, or
    /// `missing` when it never reported. `None` when no check was asked for. The verdict is the
    /// harness's and it outranks whatever the child said about its own work.
    fn verdict(&self) -> Option<String> {
        let check = self.verify.as_ref()?;
        let status = self
            .run_dir()
            .and_then(|d| std::fs::read_to_string(d.join("verify-status")).ok())
            .map(|t| t.trim().to_string());
        // On the run when firecode moved it there (a mounted worktree keeps only the work),
        // in the delivered tree otherwise.
        let log = self
            .run_dir()
            .map(|d| d.join("verify.log"))
            .filter(|l| l.is_file())
            .or_else(|| self.landed().map(|p| p.join(".firecode-verify.log")))
            .map(|l| l.display().to_string())
            .unwrap_or_else(|| ".firecode-verify.log in the delivered work".into());
        Some(match status.as_deref() {
            Some("0") => format!("ACCEPTANCE CHECK PASSED: `{check}` exited 0 (log: {log})."),
            Some("missing") | Some("") | None => format!(
                "ACCEPTANCE CHECK GAVE NO VERDICT: `{check}` never reported. Treat the work as \
                 NOT done, whatever the subagent said."
            ),
            Some(code) => format!(
                "ACCEPTANCE CHECK FAILED: `{check}` exited {code} (log: {log}). The work is NOT \
                 done, whatever the subagent said."
            ),
        })
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
        // Not ours to bring down: an attached session's VM is its ancestor's.
        if !self.owns {
            return;
        }
        LIVE.lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(_, p)| *p != self.project);
        let before = siblings(&self.project);
        // Bounded: `down` is a client waiting on a reply, and a teardown that never returns holds
        // the daemon's worker. A `down` killed at its deadline changes nothing about the answer
        // below, which is firecode's own copy-out record.
        let _ = crate::rpc::run_bounded(
            &self.bin,
            &[
                "down".to_string(),
                "--project".to_string(),
                self.project.display().to_string(),
            ],
            CLIENT_TIMEOUT,
        );
        // firecode's own record decides, for as long as it says the copy-out is running: while it
        // says `pending` this waits, up to COPY_OUT_CAP; `done <path>` is where the work is;
        // `none` is a guest that changed nothing; `failed` keeps the copy, which is the only copy.
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
                    self.remove_copy();
                    return;
                }
                Some(r) if r == "none" => {
                    self.remove_copy();
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
            self.remove_copy();
        }
    }

    /// The copy the VM was booted on, gone once its work has landed - and never a mounted
    /// worktree, which is the operator's own and is where the work landed.
    fn remove_copy(&self) {
        if !self.mounted {
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
        // A command the model asked for keeps its own deadline ([`GUEST_COMMAND_TIMEOUT`], the
        // `bash` tool's ceiling): this bound is a backstop for a lost reply, not a second opinion
        // about how long a build takes.
        let (bytes, exit) = self.guest_within(
            &cwd,
            &format!("{env_prefix}{joined}"),
            GUEST_COMMAND_TIMEOUT,
        )?;
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

    /// One `firecode in` for a page of files, not one per file: a repo-wide `grep` was a stream of
    /// single-file reads at about half a second apiece, so a few hundred files meant minutes with
    /// no tool result. [`batched_read_command`] prints each file behind a marker base64 cannot make.
    fn read_many(
        &self,
        paths: &[&str],
    ) -> Result<Vec<Result<Vec<u8>, BackendError>>, BackendError> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let guest: Vec<String> = paths
            .iter()
            .map(|p| self.guest_path(p).display().to_string())
            .collect();
        let asked: Vec<&str> = guest.iter().map(|s| s.as_str()).collect();
        let (bytes, exit) = self.guest(&batched_read_command(&asked))?;
        if exit != 0 {
            // The channel, not a file: the caller falls back to `read` per path.
            return Err(BackendError::Io(format!(
                "reading {} file(s) in the VM: exit {exit}: {}",
                paths.len(),
                String::from_utf8_lossy(&bytes).trim()
            )));
        }
        parse_batched_reads(&bytes, paths)
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
        // `--project`, always: without it `cp` asks which VM the moment a second one is up. Bounded
        // like every other client call, because a `cp` whose reply is lost is a write nobody can
        // tell the state of.
        let cp = match crate::rpc::run_bounded(
            &self.bin,
            &[
                "cp".to_string(),
                "--project".to_string(),
                self.project.display().to_string(),
                staged.display().to_string(),
                format!("vm:{dir}/"),
            ],
            CLIENT_TIMEOUT,
        ) {
            crate::rpc::Ended::Answered(out) => out,
            crate::rpc::Ended::TimedOut => {
                let _ = std::fs::remove_file(&staged);
                return Err(BackendError::Io(format!(
                    "`firecode cp` of {path} into the VM did not answer within {} s: it was killed \
                     together with everything it had started. The file may have arrived and may \
                     not have; nothing was renamed into place, so the path in the VM is either as \
                     it was or missing — read it before writing it again",
                    CLIENT_TIMEOUT.as_secs()
                )));
            }
            crate::rpc::Ended::Unstartable(why) => {
                let _ = std::fs::remove_file(&staged);
                return Err(BackendError::Io(format!("firecode cp: {why}")));
            }
        };
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

    /// The same host, owned, which the daemon's job watcher holds so a background job in the VM is
    /// seen to settle between turns. Without it the harness built no watcher, and a finished job
    /// sat `running` until somebody typed.
    fn processes_arc(&self) -> Option<Arc<dyn crate::exec::ProcessHost>> {
        self.processes
            .clone()
            .map(|p| p as Arc<dyn crate::exec::ProcessHost>)
    }

    fn close(&self) -> Option<String> {
        self.down();
        // In a VM this session did not start: nothing is brought down, nothing is copied out.
        // Not the "neither a result directory ... appeared" wording, which is about
        // a copy-out this session was never going to see.
        if !self.owns {
            return Some(if self.mounted {
                format!(
                    "the session ran in a VM it joined (run {}), with {} mounted read-write: \
                     everything it did is already there. The VM stays up for the session that \
                     started it",
                    self.joined.as_deref().unwrap_or(&self.run),
                    self.project.display()
                )
            } else {
                format!(
                    "the subagent ran in its parent's VM, in {}: its work is in that tree and \
                     comes out when the parent's VM does",
                    self.project.display()
                )
            });
        }
        // Said as what happened to the work, not as the VM's state: "the VM is
        // down" was read by the first model to see it as "the placement failed".
        let work = match self.landed() {
            Some(p) if self.mount_rw => format!(
                "the session's VM finished normally. It worked in {} itself - mounted \
                 read-write, not copied - so every edit, worktree and commit it made is already \
                 there. Nothing was copied",
                p.display()
            ),
            Some(p) if self.mounted => format!(
                "the subagent's VM finished normally. It worked in the worktree {} itself - \
                 mounted, not copied - so its edits are there and its commits are on that \
                 worktree's branch (`git -C {} log`). Nothing was copied",
                p.display(),
                p.display()
            ),
            Some(p) => format!(
                "the subagent's VM finished normally. Everything it wrote is in {} — a copy \
                 of {} as the guest left it. Nothing was applied to the source tree: diff \
                 it and take what you want",
                p.display(),
                self.source.display()
            ),
            None => match self.copy_out() {
                // Not "nothing appeared within 10 s", which
                // was false for both: a failed copy-out is the one case where the copy below is
                // the ONLY place the work is, and a pending one is work still arriving.
                Some(r) if r.starts_with("failed") => format!(
                    "the subagent's VM stopped, but firecode's copy-out {r}. The copy the VM was \
                     booted on is kept at {}, and it is now the only place the work is — read it \
                     from there",
                    self.project.display()
                ),
                Some(r) if r == "pending" => format!(
                    "the subagent's VM stopped and firecode is still copying its work out after \
                     {} s. It is not lost: when the copy finishes, `{}` will say `done <path>`, \
                     and the path is where the work is",
                    COPY_OUT_CAP.as_secs(),
                    self.run_dir()
                        .map(|d| d.join("copy_out").display().to_string())
                        .unwrap_or_else(|| "firecode's runs/<run>/copy_out".into())
                ),
                Some(r) if r == "none" => format!(
                    "the subagent's VM finished normally and changed nothing in its copy of {}, \
                     so firecode wrote no result directory",
                    self.source.display()
                ),
                _ if self.run_finished() => format!(
                    "the subagent's VM finished normally and changed nothing in its copy of {}, \
                     so firecode wrote no result directory",
                    self.source.display()
                ),
                _ => format!(
                    "the subagent's VM was brought down, but neither a result directory beside \
                     {} nor firecode's record of the run appeared within {} s; the run's console \
                     log under firecode's runs directory is the place to look",
                    self.project.display(),
                    LEGACY_WAIT.as_secs()
                ),
            },
        };
        Some(match self.verdict() {
            Some(v) => format!("{work}.\n\n{v}"),
            None => work,
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
                // `find -printf` here carries no time, and asking for one per entry would be a
                // second round trip for an age nobody reads.
                modified: None,
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// One `find` for the whole tree, not one `list` per directory: `list` here is one vsock round
    /// trip, so a walk of 122 directories was most of a session's boot-to-answer. The listing is
    /// put back into the order the loop would have produced, so no tool can tell which ran.
    fn list_tree(
        &self,
        root: &str,
        limit: usize,
        skip: &dyn Fn(&DirEntry) -> bool,
    ) -> Result<(Vec<DirEntry>, bool), BackendError> {
        let gp = self.guest_path(root);
        let q = shell_quote(&gp.display().to_string());
        let (bytes, exit) = self.guest(&format!(
            "if [ ! -e {q} ]; then exit 22; fi; if [ ! -d {q} ]; then exit 23; fi; \
             find {q} -mindepth 1 -printf '%y\\t%s\\t%p\\n'"
        ))?;
        match exit {
            22 => return Err(BackendError::NotFound(root.to_string())),
            23 => return Err(BackendError::NotADirectory(root.to_string())),
            // Any other status is `find` complaining on the merged stream about a subtree it could
            // not read, after printing what it could. The bytes are parsed anyway: one unreadable
            // directory is not a reason to answer "no files", which an error here would become.
            _ => {}
        }
        Ok(tree_in_walk_order(
            &bytes,
            &gp.display().to_string(),
            limit,
            skip,
        ))
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
            // The guest's `stat` is asked for the kind and the size only, so there is no time to
            // report rather than a zero to mistake for one.
            modified: None,
        })
    }

    fn describe(&self) -> String {
        if self.mount_rw {
            return format!(
                "firecode VM (run {}{}) with {} mounted read-write: every repository in it can be \
                 read and changed, new worktrees beside a repository included, and all of it \
                 lands on the host as it is made. Nothing else of the host is reachable - no other \
                 files, processes or devices. Everything is allowed inside{}",
                if self.run.is_empty() { "?" } else { &self.run },
                if self.joined.is_some() {
                    ", joined - started by another session"
                } else {
                    ""
                },
                self.project.display(),
                if self.processes.is_some() {
                    ", exec through `firecode in`"
                } else {
                    ", NO exec (downgraded)"
                },
            );
        }
        if self.mounted {
            return format!(
                "firecode VM (run {}) with the worktree {} mounted read-write: edits land in it \
                 as they are made, and commits go to its branch in the real repository. The \
                 main checkout is mounted read-only, the repository's .git read-write; nothing \
                 else of the host is reachable - no other files, processes or devices. {}{}",
                if self.run.is_empty() { "?" } else { &self.run },
                self.project.display(),
                if self.writable {
                    "Writable"
                } else {
                    "READ-ONLY (downgraded)"
                },
                if self.processes.is_some() {
                    ", exec through `firecode in`"
                } else {
                    ", NO exec (downgraded)"
                },
            );
        }
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

/// How long one control command in the guest gets before `firecode in` is killed: not a budget for
/// work, which is milliseconds, but the point past which the only explanation is a lost reply,
/// because `firecode in` has no `--timeout` of its own and `vsock-exec.py` blocks in `recv`.
pub const GUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// How long a command the model asked for gets: ten minutes, `bash`'s own ceiling for a foreground
/// call. A backstop for a lost reply, not a tighter deadline on real work, so a command that needs
/// longer is one to run with `background: true`.
pub const GUEST_COMMAND_TIMEOUT: Duration = Duration::from_secs(600);

/// The wall clock on a client verb that is not a boot and not a command in the guest (`info`,
/// `down`, `cp`): each is a local client waiting on a reply, and one that never answers is a
/// placement or a teardown held for ever.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(120);

/// The wall clock on `layer inherit`, which may carry a toolchain layer across: five minutes, and
/// a timeout is not fatal - a copy with no inherited layer still boots on its base image, and
/// [`ExecBackend::describe`] says so.
const LAYER_TIMEOUT: Duration = Duration::from_secs(300);

/// The wall clock on `firecode up`: it installs a layer and boots a kernel, which is minutes when
/// the layer is cold. Ten, and a timeout is a failed placement rather than a slow one - the copy
/// goes with it, because a boot nobody can see the end of is not a VM to hand to a session.
const UP_TIMEOUT: Duration = Duration::from_secs(600);

/// What a guest call that lost its reply says: what happened, that the outcome is unknown, and
/// which retry is safe. It never quotes the command, which may carry a file's bytes or a long path.
fn guest_lost_its_reply(cwd: &Path, within: Duration) -> String {
    format!(
        "the guest did not answer within {} s, so `firecode in` was killed together with \
         everything it had started — nothing of it is running in the VM now. The command may have \
         run and may not have: a reply lost on the vsock looks exactly like this, and it is not a \
         refusal. Retrying is safe for a read (`read`, `list`, `glob`, `grep`); a command that \
         changes something may already have changed it, so look at the state before running it \
         again. (in {})",
        within.as_secs(),
        cwd.display()
    )
}

/// Inline writes up to this many bytes ride in the command; larger go through `cp`.
const INLINE_WRITE_BYTES: usize = 48 * 1024;

/// The entries in a `find -printf '%y\t%s\t%p\n'` listing, in the order the per-directory loop
/// would have produced them, because a listing in `find`'s order would reorder a model's answer
/// with nothing saying why. `skip` is tested before descent, as the loop tests it.
pub(crate) fn tree_in_walk_order(
    listing: &[u8],
    root: &str,
    limit: usize,
    skip: &dyn Fn(&DirEntry) -> bool,
) -> (Vec<DirEntry>, bool) {
    let mut by_parent: std::collections::HashMap<String, Vec<DirEntry>> =
        std::collections::HashMap::new();
    for line in String::from_utf8_lossy(listing).lines() {
        let mut parts = line.splitn(3, '\t');
        let (Some(kind), Some(size), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            // Not a listing line. `firecode in` merges the guest's stderr into the
            // same stream, so `find`'s own complaints arrive here too, and a line
            // with no tab is not an entry.
            continue;
        };
        by_parent
            .entry(parent_of(path).to_string())
            .or_default()
            .push(DirEntry {
                path: path.to_string(),
                name: path.rsplit('/').next().unwrap_or(path).to_string(),
                is_dir: kind == "d",
                bytes: size.parse().unwrap_or(0),
                // The walk carries no times: `find -printf '%y\t%s\t%p'` was chosen for one round
                // trip over a 122-directory tree, and a time per entry would not be one.
                modified: None,
            });
    }

    let mut out = Vec::new();
    let mut truncated = false;
    let mut queue = std::collections::VecDeque::from([find_root(root).to_string()]);
    while let Some(dir) = queue.pop_front() {
        let Some(mut children) = by_parent.remove(&dir) else {
            continue;
        };
        children.sort_by(|a, b| a.name.cmp(&b.name));
        for e in children {
            if skip(&e) {
                continue;
            }
            if out.len() >= limit {
                truncated = true;
                return (out, truncated);
            }
            if e.is_dir {
                queue.push_back(e.path.clone());
            }
            out.push(e);
        }
    }
    (out, truncated)
}

/// The root as `find` echoes it back in `%p`: `find` drops a single trailing separator from every
/// printed path and keeps a doubled one, and `Path::join` appends the same way. Getting this wrong
/// is silent: every entry would be filed under a parent the walk never visits.
fn find_root(root: &str) -> &str {
    match root.len() {
        0 | 1 => root,
        n if root.ends_with('/') && !root[..n - 1].ends_with('/') => &root[..n - 1],
        _ => root,
    }
}

/// The directory part of a path `find` printed: `path` is `parent` + `name`, with a
/// separator added only when the parent did not already end in one.
fn parent_of(path: &str) -> &str {
    match path.rfind('/') {
        None => "",
        Some(0) => "/",
        Some(i) if path.as_bytes()[i - 1] == b'/' => &path[..i + 1],
        Some(i) => &path[..i],
    }
}

/// The byte that opens each file's slot in [`batched_read_command`]'s stream. ASCII `RS`, safe for
/// one reason: every byte up to the next one is base64, whose alphabet cannot produce the marker,
/// so a file containing it (or NUL, or a newline) cannot be mistaken for a boundary.
const READ_MARK: u8 = 0x1e;

/// One guest command that prints N files, the shape [`parse_batched_reads`] reads back: per file
/// `0x1e`, a header, a newline, base64 on one line - or `d` directory, `-` missing, `!` unreadable.
/// Portable so a test's `/bin/sh` matches a Linux guest; the length is trimmed as BSD `wc` pads.
pub(crate) fn batched_read_command(paths: &[&str]) -> String {
    let list = paths
        .iter()
        .map(|p| shell_quote(p))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "for f in {list}; do \
           if [ -d \"$f\" ]; then printf '\\036d\\n'; continue; fi; \
           if [ ! -e \"$f\" ]; then printf '\\036-\\n'; continue; fi; \
           n=$(wc -c < \"$f\" 2>/dev/null | tr -d '[:space:]') || n=; \
           case \"$n\" in ''|*[!0-9]*) printf '\\036!\\n'; continue ;; esac; \
           printf '\\036%s\\n' \"$n\"; \
           base64 < \"$f\" 2>/dev/null | tr -d '\\n'; \
           printf '\\n'; \
         done"
    )
}

/// [`batched_read_command`]'s stream, back into one answer per path, in order: each slot is what
/// [`ExecBackend::read`] would have answered, and a stream that does not account for exactly one
/// slot per path is an error for the CALL, which is the caller's signal to fall back.
pub(crate) fn parse_batched_reads(
    stream: &[u8],
    paths: &[&str],
) -> Result<Vec<Result<Vec<u8>, BackendError>>, BackendError> {
    // Everything before the first marker is the run-up, not a slot.
    let mut slots = stream.split(|b| *b == READ_MARK);
    slots.next();
    let slots: Vec<&[u8]> = slots.collect();
    if slots.len() != paths.len() {
        return Err(BackendError::Io(format!(
            "the VM answered {} file(s) for {} path(s)",
            slots.len(),
            paths.len()
        )));
    }
    let mut out = Vec::with_capacity(paths.len());
    for (slot, path) in slots.iter().zip(paths) {
        let (header, body) = match slot.iter().position(|b| *b == b'\n') {
            Some(nl) => (&slot[..nl], &slot[nl + 1..]),
            None => (&slot[..], &[][..]),
        };
        let header = String::from_utf8_lossy(header);
        let header = header.trim();
        out.push(match header {
            "d" => Err(BackendError::IsADirectory(path.to_string())),
            "-" => Err(BackendError::NotFound(path.to_string())),
            // There and unreadable. `NotFound` reads as *it is not there*, which is how a corpus
            // with one unreadable note was answered as a whole one.
            "!" => Err(BackendError::Io(format!(
                "{path}: it is there and could not be read"
            ))),
            said => match said.parse::<usize>() {
                Ok(len) => match decode_base64(body) {
                    // The length is the check that the slot is whole: a short body is a
                    // truncated read, and reporting it as the file's bytes would put a
                    // half file in front of the matcher.
                    Ok(b) if b.len() == len => Ok(b),
                    Ok(b) => Err(BackendError::Io(format!(
                        "{path}: read {} of {len} byte(s)",
                        b.len()
                    ))),
                    Err(e) => Err(BackendError::Io(format!("{path}: {e}"))),
                },
                Err(_) => Err(BackendError::Io(format!(
                    "{path}: the VM said {said:?} where a length belongs"
                ))),
            },
        });
    }
    Ok(out)
}

fn run_id(bin: &Path, project: &Path) -> Option<String> {
    // Bounded ([`CLIENT_TIMEOUT`]): a client call that never answers must not hold a placement.
    let out = match crate::rpc::run_bounded(
        bin,
        &[
            "info".to_string(),
            "--project".to_string(),
            project.display().to_string(),
            "--json".to_string(),
        ],
        CLIENT_TIMEOUT,
    ) {
        crate::rpc::Ended::Answered(out) => out,
        _ => return None,
    };
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

/// Copy a tree, skipping `exclude` (relative to `from`), with symlinks as links and `.git` copied
/// so the guest can commit and diff.
/// The rsync invocation both passes share, with unanchored excludes so `target` matches at any depth.
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
                "rsync: {e} (rsync is required to copy the tree a VM is booted on)"
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

/// Copy `from` under `to`, after measuring it and refusing by size: the failure this guards is not
/// an error rsync reports, because a copy that succeeds after ten minutes and 112 GB is a success.
fn copy_tree(from: &Path, to: &Path, exclude: &[String], max: u64) -> Result<(), BackendError> {
    if !from.is_dir() {
        return Err(BackendError::NotADirectory(from.display().to_string()));
    }
    let size = measure_copy(from, to, exclude)?;
    if size > max {
        return Err(BackendError::Io(format!(
            "refusing to copy {}: it weighs {} after excluding [{}], and the ceiling is {}. \
             A source this size is a folder of repositories or a tree with build output under \
             an unexpected name, and neither is the thing a VM is for: the copy source should be \
             the ONE repository the work is in — `Config::vm_copy_source`, where the session's own \
             `vm_source` and the repository its workspace is about are resolved — and not the \
             directory the session was started in. Raise LETIBOT_FIRECODE_COPY_MAX on purpose only \
             for a repository that genuinely weighs more than {}.",
            from.display(),
            human(size),
            exclude.join(", "),
            human(max),
            human(DEFAULT_COPY_MAX)
        )));
    }
    let out = rsync_cmd(from, to, exclude).output().map_err(|e| {
        BackendError::Io(format!(
            "rsync: {e} (rsync is required to copy the tree a VM is booted on)"
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

/// Delegates to the one alphabet in the tree, `letibot_transcript::media::encode_base64`, because a
/// base64 alphabet is a fact that gets spelled right in one place and subtly wrong in another. The
/// decoder below stays here: its only caller is the far side of a `firecode` read.
pub(crate) fn encode_base64(bytes: &[u8]) -> String {
    letibot_transcript::media::encode_base64(bytes)
}

/// Shared with [`crate::remote`], whose reads and writes are the same problem over a
/// different wire: a second decoder would be the same fact written twice.
pub(crate) fn decode_base64(text: &[u8]) -> Result<Vec<u8>, String> {
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
#[cfg(test)]
mod tests;
