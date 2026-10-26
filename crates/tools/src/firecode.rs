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
//! **The tree it takes is the one repository the work is in**, not the directory a session was
//! started in: a session may be seated a level above its repositories, and copying the level is
//! how 72.8 GiB of siblings reached this door (see [`FirecodeSpec::source`]). The caller decides
//! it; the ceiling refuses the wrong answer rather than copying it slowly.
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

pub mod held;

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
    firecode_available_at(firecode_bin())
}

/// [`firecode_available`] for a CLI the caller names, rather than the one `$FIRECODE_BIN`
/// names. The seam a test uses so it never has to SET `$FIRECODE_BIN`: `set_var` in one test
/// is read by every other test in the binary that is running at the same moment (R67 item 17),
/// and a parameter has no such reader.
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
///
/// **All at once, not one after another.** One `down` waits up to a minute for its guest and
/// then on the copy-out; four in a row were minutes, and a forced stop sends SIGKILL five seconds
/// after SIGTERM (`letibot --stop --force`). Whatever a SIGKILL interrupts here is left to the
/// next daemon's reaper (`harnessd`'s `vm_reap`), which finds the VM by its run record.
pub fn down_all() {
    let live = std::mem::take(&mut *LIVE.lock().unwrap_or_else(|e| e.into_inner()));
    let downs: Vec<_> = live
        .into_iter()
        .map(|(bin, project)| {
            std::thread::spawn(move || {
                let _ = std::process::Command::new(&bin)
                    .arg("down")
                    .arg("--project")
                    .arg(&project)
                    .stdin(Stdio::null())
                    .output();
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
            let _ = std::process::Command::new(self.bin)
                .arg("down")
                .arg("--project")
                .arg(self.project)
                .stdin(Stdio::null())
                .output();
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
    /// [`FirecodeSpec::verify`], kept to name the check beside its verdict.
    verify: Option<String>,
    /// **Whether this backend brought the VM up and so brings it down.** `false` for a session
    /// attached to an ancestor's VM ([`FirecodeBackend::attach`]): the VM is the ancestor's, and a
    /// child that ends must not take its parent's machine with it.
    owns: bool,
    /// **The project is the source itself, mounted, never a copy** ([`mounts`]): a git worktree
    /// on a Mac, which firecode shares into the guest read-write. Nothing of it is ever removed
    /// here, and its work is where it always was.
    mounted: bool,
    /// The project is a whole folder mounted read-write ([`FirecodeSpec::mount_rw`]), not a
    /// worktree - said differently in the answer.
    mount_rw: bool,
    /// The run of an already-running VM this backend joined instead of booting one, when it did.
    /// Joined is not owned: the VM is brought down by whoever started it.
    joined: Option<String>,
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

/// **Whether a VM placement on `source` mounts it rather than copying it.**
///
/// A git worktree on a Mac: firecode shares it into the guest read-write, the main checkout
/// read-only and the repository's `.git` read-write (virtio-fs), so the child works in the real
/// worktree beside the repository and commits to its branch as an agent on the host would -
/// nothing copied in, nothing copied out, nothing under the cache. Anything else is copied, as
/// before. A worktree, not a submodule: both have a `.git` file, and only a worktree's names a
/// directory under the repository's `worktrees/`.
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

/// **Where the VM of a session placed on `source` holds its project** — the source itself when it
/// is mounted ([`mounts`]), the session's copy under the cache otherwise. What a child attaching
/// to that VM names it by.
pub fn project_for(source: &Path, session_id: &str) -> PathBuf {
    if mounts(source) {
        source.to_path_buf()
    } else {
        FirecodeBackend::project_of(session_id)
    }
}

/// **The run of a VM already up with `folder` itself mounted read-write**, if there is one - asked
/// of firecode (`info --json`, whose `mount` says how a run holds its project), so a VM holding a
/// COPY of the folder is never mistaken for one holding the folder.
fn running_rw(bin: &Path, folder: &Path) -> Option<String> {
    let out = std::process::Command::new(bin)
        .args(["info", "--json", "--project"])
        .arg(folder)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
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
    /// **The tree to copy** — the one repository the work is in, and NOT the directory a session
    /// was started in when the two are different.
    ///
    /// The copy is the guest's whole world, so what it holds decides whether the VM can do the
    /// job at all: a reviewer runs `git diff <base>...<branch>` in it, which needs the repository
    /// the branch is in. A session seated a level above its repositories — the operator's own
    /// box: `--workspace /home/dead/Projects/letibot`, whose repository is
    /// `/home/dead/Projects/letibot/letibot` — put the FOLDER here, and the copy was 72.8 GiB of
    /// siblings, worktrees and build output that [`copy_tree`] refused three times on 2026-10-10
    /// for two different branches. The siblings are not the work and no reviewer reads them.
    ///
    /// **It is filled by `Config::vm_copy_source`, which is the ONE resolution of *what this
    /// session's VM copies*** — the caller's own `Config::vm_source` where the caller that knows
    /// named one, the repository the session's workspace is about otherwise
    /// (`mergequeue::repo_of_workspace`: git's answer, then the nested layout), and the workspace
    /// itself only when there is no repository to name at all. `Harness::open` is the only reader,
    /// so the resolution covers every session the daemon opens at once rather than one opener at a
    /// time — and the review queue, which is the caller that knows better, names the ENTRY's
    /// repository on the child's spec (`mergequeue::review_copy_source`) because a reviewer's
    /// branch is in that repository and not necessarily in the one the queue's session sits in.
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
    /// **The acceptance check**, handed to `firecode up --verify`: firecode runs it in the
    /// project when the VM is brought down, after the child has finished, and records its exit
    /// status on the run (`runs/<run>/verify-status`). [`ExecBackend::close`] reports that
    /// verdict, so a placed child's "done" is something the harness ran and not the child's word.
    pub verify: Option<String>,
    /// **The source itself, mounted read-write, never a copy** (`firecode up --mount-rw`): the
    /// folder the session works in - every repository in it - as `letibot --vm` gives it on a
    /// Mac. Edits, worktrees beside a repository and commits land on the host as they are made;
    /// nothing else of the host is reachable. A VM already running with this folder mounted is
    /// joined rather than a second booted ([`FirecodeBackend::up`]).
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
            verify: None,
            mount_rw: false,
            copy_max: std::env::var("LETIBOT_FIRECODE_COPY_MAX")
                .ok()
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(DEFAULT_COPY_MAX),
        }
    }
}

/// **8 GiB: room for one repository, and none for a folder of them.**
///
/// The number is argued against the thing being copied, and the thing is the one repository the
/// work is in — see [`FirecodeSpec::source`]. This repository's own tree measures **1.2 GiB**
/// after the excludes (measured 2026-10-10, with `target`, `.claude/worktrees`, `node_modules`
/// and `.git/worktrees` out), so 8 GiB is roughly six times the largest copy any repository of
/// this shape needs; the FOLDER above it measures **72.8 GiB**, nine times the ceiling, and that
/// is the shape the ceiling exists to refuse. It is not a budget for a copy that is going to
/// succeed: a source over it is a source that is the wrong thing, and
/// `LETIBOT_FIRECODE_COPY_MAX` is the operator's deliberate override for a repository that
/// genuinely weighs more — not a way to make a folder of repositories copy.
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
        // **The folder itself, read-write**: joined if a VM already holds it mounted, booted with
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
        // **From here every failure takes the copy with it** — and the VM, once there is
        // one. A failed spawn used to leave its copy under the cache (FIRECODE-NOTES: *"the
        // very first attempt left 286 MB"*): the `?` on a spawn error returned past the
        // `remove_dir_all` that only a refused status reached.
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

        // Layers are attached to a PATH, and the copy has a path of its own. The
        // source's toolchain layers are what the guest needs to build this tree
        // — measured 2026-09-14: two models in copies of a repository whose guest
        // lacked llama.cpp and the sqlite dev symlink each spent ten minutes
        // stubbing them. A source with no layers is an answer, not a failure.
        //
        // **And a convenience, not a requirement** (FIRECODE-NOTES item 4): a firecode without
        // the verb, or one that refuses it, still boots the copy on its base image, and the
        // backend says so rather than refusing the placement.
        //
        // Mounted, there is nothing to inherit onto: the worktree's VM inherits its main
        // checkout's layer itself, read-only, as firecode does for any worktree.
        let inherit = if mounted {
            None
        } else {
            Some(
                std::process::Command::new(&bin)
                    .arg("layer")
                    .arg("inherit")
                    .arg(&spec.source)
                    .arg("--project")
                    .arg(&project)
                    .stdin(Stdio::null())
                    .output()
                    .map_err(|e| BackendError::Io(format!("running {}: {e}", bin.display())))?,
            )
        };
        let layers = inherit.filter(|i| !i.status.success()).map(|inherit| {
            let said = String::from_utf8_lossy(&inherit.stderr);
            let why = if said.contains("usage:") && !said.contains("inherit") {
                "this firecode has no `layer inherit`".to_string()
            } else {
                format!("`layer inherit` refused: {}", said.trim())
            };
            format!("no layer inheritance ({why}); the guest builds on base-image toolchains")
        });

        let mut up = std::process::Command::new(&bin);
        up.arg("up")
            .arg("--project")
            .arg(&project)
            .args(&spec.up_args);
        if spec.mount_rw {
            up.arg("--mount-rw");
        }
        if let Some(check) = &spec.verify {
            up.arg("--verify").arg(check);
        }
        let up = up
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
            verify: spec.verify.clone(),
            owns: true,
            mounted,
            mount_rw: spec.mount_rw,
            joined: None,
        };
        // Prove the door works before handing the backend over: a VM that came
        // up and cannot take a command is a boot, not a backend.
        b.guest("true")
            .map_err(|e| BackendError::Io(format!("the VM is up but takes no command: {e}")))?;
        Ok(b)
    }

    /// **A session in a VM another session brought up** — a child of a VM-placed session runs in
    /// that same VM: the operator, *"children live in the _same_ vm"*, and the tool's own word for
    /// the default placement, *"your own boundary"*, which for a child in a VM is its VM. No copy,
    /// no boot, no layers of its own: the tree, the toolchain and what the parent installed are
    /// the parent's, and so is the memory. Never brought down by this backend ([`Self::owns`]).
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
        };
        // The parent's VM has to be up and taking commands, or there is nothing to run in.
        b.guest("true").map_err(|e| {
            BackendError::Io(format!(
                "the parent's VM takes no command (is it still up?): {e}"
            ))
        })?;
        Ok(b)
    }

    /// **Where a session's own VM's copy lives** — the project `up` names after it.
    pub fn project_of(session_id: &str) -> PathBuf {
        FirecodeSpec::new(PathBuf::new(), session_id)
            .cache
            .join(session_id)
    }

    /// Whether this backend brought its VM up — `false` when attached to an ancestor's.
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

    /// **The acceptance check's verdict, as firecode recorded it** — `runs/<run>/verify-status`,
    /// the check's exit status, or `missing` when it never reported. `None` when no check was
    /// asked for. Worded so a parent cannot read a failure as a pass: the verdict is the
    /// harness's, and it outranks whatever the child said about its own work.
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
        // **Not ours to bring down**: an attached session's VM is its ancestor's.
        if !self.owns {
            return;
        }
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
        // `--project`, always: without it `cp` asks which VM the moment a second one is up -
        // another child, the operator's own - and the write fails with "2 VMs are running".
        let cp = std::process::Command::new(&self.bin)
            .arg("cp")
            .arg("--project")
            .arg(&self.project)
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

    /// The same host, owned — what the daemon's job watcher holds so a background job in the VM
    /// is seen to settle between turns. It was missing (R92.1): the trait's default answered
    /// `None`, the harness built no watcher for a firecode session, and a finished job sat
    /// `running` until somebody typed.
    fn processes_arc(&self) -> Option<Arc<dyn crate::exec::ProcessHost>> {
        self.processes
            .clone()
            .map(|p| p as Arc<dyn crate::exec::ProcessHost>)
    }

    fn close(&self) -> Option<String> {
        self.down();
        // In a VM this session did not start: nothing is brought down, nothing is copied out.
        // It used to fall through to "neither a result directory ... appeared", which is about
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
                // Each of these used to fall through to "nothing appeared within 10 s", which
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
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// **One `find` for the whole tree, not one `list` per directory.**
    ///
    /// `list` here is one `firecode in` vsock round trip, measured at 0.645–0.688 s;
    /// the walk behind a VM-placed session's first `glob` covered 122 directories and
    /// spent 80.48 s of a 125 s boot-to-answer doing it. `find` answers the same tree
    /// in the guest in ~0.7 s, so this is the one place the trait's default loop is
    /// the wrong shape — and it is why [`ExecBackend::list_tree`] exists at all.
    ///
    /// The listing is put back into the order the loop would have produced
    /// ([`tree_in_walk_order`]) so that `glob`, `grep` and `outline` cannot tell which
    /// of the two ran.
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
            // Any other status is `find` complaining, on the merged stream, about a
            // subtree it could not read — after printing everything it could. The
            // bytes are parsed anyway: one unreadable directory is not a reason to
            // answer "no files", which is what an error here would become one layer
            // up, where `walk` turns an error into an empty walk.
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

/// Inline writes up to this many bytes ride in the command; larger go through `cp`.
const INLINE_WRITE_BYTES: usize = 48 * 1024;

/// The entries in a `find -printf '%y\t%s\t%p\n'` listing, in the order the
/// per-directory loop would have produced them.
///
/// **The order is the point, not a nicety.** `find` walks depth-first; a walk of
/// [`ExecBackend::list`] is breadth-first with each directory's children in the order
/// `list` returned them (firecode's sorts by name). `glob` and `grep` print their hits
/// in walk order, so a listing handed back in `find`'s order would reorder a model's
/// answer with nothing saying why — the same tree would answer differently depending
/// on which substrate it was on. Rebuilding the loop's order from the flat listing is
/// what makes the one-command path indistinguishable from it.
///
/// `skip` is tested before descent, exactly as the loop tests it, so a skipped
/// directory's contents are dropped even though `find` has already printed them: the
/// loop never fetched them, and the two must agree entry for entry.
///
/// `pub(crate)` so the test that pins the two paths together can drive both.
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

/// The root as `find` echoes it back in `%p`.
///
/// `find` appends a separator only when the argument does not already end in one, so
/// a **single** trailing separator is gone from every printed path and a doubled one
/// survives. Measured with findutils 4.10.0: `find t/ -printf '%p'` prints `t/sub`,
/// `find t// -printf '%p'` prints `t//sub`, `find t/. -printf '%p'` prints `t/./sub` —
/// and `Path::join`, which builds the loop's directory strings, appends the same way.
/// Getting this wrong is silent: every entry would be filed under a parent the walk
/// never visits, and the tree would answer "no files".
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

    /// **`down_all` brings down every live backend in the process**, other tests' included - so
    /// the test of it and a test that asserts a VM stays up must not run at the same time. The
    /// attach test failed about one run in five on the parent's VM being "downed" under it.
    static DOWN_ALL: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

    /// **A child in its parent's VM never brings it down** — the operator: *"children live in
    /// the _same_ vm"*. An attached backend runs in the parent's project and reports that it
    /// does not own the VM; dropping it calls no `down`, and the parent's own drop still does.
    #[test]
    fn a_child_attached_to_its_parents_vm_leaves_it_up() {
        let _alone = DOWN_ALL.lock().unwrap_or_else(|e| e.into_inner());
        let root = tmp("attach");
        let src = root.join("src");
        file(&src.join("a.txt"), 10);
        let arms = format!("{HAPPY}\ndown) touch \"$ROOT/downed\" ;;");
        let bin = stub_firecode(&root.join("fc"), &arms);
        let downed = root.join("fc").join("downed");
        let parent = FirecodeBackend::up(&spec_with(&src, &root.join("cache"), &bin))
            .expect("the parent's VM is placed");
        assert!(parent.owns_vm());
        let child = FirecodeBackend::attach(
            Some(bin.clone()),
            parent.project().to_path_buf(),
            true,
            false,
        )
        .expect("the child attaches to the parent's VM");
        assert!(!child.owns_vm(), "the VM is the parent's");
        assert_eq!(child.project(), parent.project(), "the same tree");
        drop(child);
        assert!(
            !downed.exists(),
            "the child's end took the parent's VM down"
        );
        drop(parent);
        assert!(downed.exists(), "the parent's own end still brings it down");
        let _ = std::fs::remove_dir_all(&root);
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

    /// **A VM's jobs can be watched** (R92.1): the owned process host is the same one `processes`
    /// lends. Before, `processes_arc` fell to the trait default `None`, so the harness built no
    /// job watcher for a firecode session and a finished background job was never announced.
    #[test]
    fn a_vm_that_runs_processes_hands_its_host_to_the_job_watcher() {
        // An exec VM builds its process host on a process-lifetime tree, and CI's runner has
        // none: *"cgroup mkdir failed on `/sys/fs/cgroup/system.slice/hosted-compute-agent.service/
        // letibot.17403.0`: Permission denied"* (run 39124634128, main red since 2026-10-15 on
        // this test alone). Said, and refused under `LETIBOT_REQUIRE_APPARATUS=1`, like the
        // other host-spawning tests (`exec/host.rs`).
        let Some(_) = letibot_tokencore::apparatus::present(
            "a process-lifetime tree (cgroup v2; process groups on macOS)",
            crate::host_tree().is_ok(),
        ) else {
            return;
        };
        let root = tmp("procarc");
        let src = root.join("src");
        file(&src.join("a.txt"), 10);
        let bin = stub_firecode(&root.join("fc"), HAPPY);
        let mut spec = spec_with(&src, &root.join("cache"), &bin);
        spec.exec = true;
        let b = FirecodeBackend::up(&spec).expect("the VM is placed");
        assert!(b.processes().is_some(), "an exec VM lends no process host");
        let owned = b
            .processes_arc()
            .expect("an exec VM hands no owned process host to the job watcher");
        let lent = b.processes().unwrap() as *const dyn crate::exec::ProcessHost as *const ();
        assert_eq!(
            Arc::as_ptr(&owned) as *const (),
            lent,
            "the owned host is not the one the tools run on"
        );

        let mut spec = spec_with(&src, &root.join("cache2"), &bin);
        spec.name = "child2".into();
        let b = FirecodeBackend::up(&spec).expect("the VM is placed");
        assert!(
            b.processes_arc().is_none(),
            "a no-exec VM hands out a process host"
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

    /// **A worktree is mounted, a submodule and a plain tree are copied** - both a worktree and a
    /// submodule have a `.git` file, and only a worktree's names `worktrees/`.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_worktree_is_mounted_and_a_submodule_is_not() {
        let root = tmp("mounts");
        let wt = root.join("repo-x");
        let sub = root.join("sub");
        let plain = root.join("plain");
        for d in [&wt, &sub, &plain] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(wt.join(".git"), "gitdir: /p/repo/.git/worktrees/repo-x\n").unwrap();
        std::fs::write(sub.join(".git"), "gitdir: ../.git/modules/sub\n").unwrap();
        assert!(mounts(&wt));
        assert!(!mounts(&sub));
        assert!(!mounts(&plain));
        assert_eq!(
            project_for(&wt, "s1"),
            wt,
            "a mounted VM's project is the worktree"
        );
        assert_ne!(
            project_for(&plain, "s1"),
            plain,
            "a copied one's is the copy"
        );
    }

    /// **A mounted placement copies nothing and never deletes the worktree** - the project IS the
    /// operator's worktree, so the copy-and-clean-up path must not reach it, on success or after
    /// `down`, and the answer says the work is in the worktree rather than in a copy.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_mounted_worktree_is_neither_copied_nor_removed() {
        let root = tmp("mounted");
        let wt = root.join("repo-x");
        file(&wt.join("work.txt"), 10);
        std::fs::write(wt.join(".git"), "gitdir: /p/repo/.git/worktrees/repo-x\n").unwrap();
        let fc = root.join("fc");
        let arms = format!(
            "{HAPPY}\n\
             down) mkdir -p \"$ROOT/runs/r1\"; echo 'done {w}' > \"$ROOT/runs/r1/copy_out\"; exit 0 ;;",
            w = wt.display()
        );
        let bin = stub_firecode(&fc, &arms);
        let b = FirecodeBackend::up(&spec_with(&wt, &root.join("cache"), &bin)).unwrap();
        assert_eq!(
            b.project(),
            wt.as_path(),
            "the VM's project is the worktree itself"
        );
        assert!(
            !root.join("cache").join("child").exists(),
            "nothing was copied"
        );
        let answer = b.close().unwrap();
        assert!(wt.join("work.txt").exists(), "the worktree survives down");
        assert!(answer.contains("worked in the worktree"), "{answer}");
        assert!(answer.contains("Nothing was copied"), "{answer}");
    }

    /// **`--vm`'s folder is mounted read-write and never copied or removed** - booted with
    /// `--mount-rw` on the folder itself, nothing under the cache, the folder intact after close,
    /// and the answer says the work is already there.
    #[test]
    fn a_folder_placement_mounts_it_and_copies_nothing() {
        let root = tmp("mountrw");
        let folder = root.join("org");
        file(&folder.join("repo/a.txt"), 10);
        let fc = root.join("fc");
        let argv = root.join("up-argv");
        let arms = format!(
            "up) printf '%s\\n' \"$@\" > '{a}'; exit 0 ;;\n\
             info) [ \"$2\" = --json ] && exit 1; echo '{{\"run\":\"r1\"}}' ;;\n\
             down) mkdir -p \"$ROOT/runs/r1\"; echo 'done {f}' > \"$ROOT/runs/r1/copy_out\"; exit 0 ;;",
            a = argv.display(),
            f = folder.display()
        );
        let bin = stub_firecode(&fc, &arms);
        let mut spec = spec_with(&folder, &root.join("cache"), &bin);
        spec.mount_rw = true;
        let b = FirecodeBackend::up(&spec).unwrap();
        assert!(
            std::fs::read_to_string(&argv)
                .unwrap()
                .lines()
                .any(|l| l == "--mount-rw")
        );
        assert_eq!(b.project(), folder.as_path());
        assert!(
            !root.join("cache").join("child").exists(),
            "nothing was copied"
        );
        assert!(
            b.describe().contains("mounted read-write"),
            "{}",
            b.describe()
        );
        let answer = b.close().unwrap();
        assert!(
            folder.join("repo/a.txt").exists(),
            "the folder survives down"
        );
        assert!(answer.contains("already there"), "{answer}");
    }

    /// **A VM already up with the folder mounted is joined, not doubled** - and a joiner neither
    /// boots one nor brings the joined one down. One holding only a COPY of the folder is not
    /// joined: its tree is not the folder.
    #[test]
    fn a_running_vm_with_the_folder_mounted_is_joined_and_one_with_a_copy_is_not() {
        for (mount, joins) in [("rw", true), ("copy", false)] {
            let root = tmp(&format!("join-{mount}"));
            let folder = root.join("org");
            file(&folder.join("a.txt"), 10);
            let fc = root.join("fc");
            let booted = root.join("booted");
            let downed = root.join("downed");
            let arms = format!(
                "up) touch '{b}'; exit 0 ;;\n\
                 info) printf '{{\"run\":\"r9\",\"project\":\"%s\",\"mount\":\"{mount}\"}}' '{f}' ;;\n\
                 layer) exit 0 ;;\n\
                 down) touch '{d}'; exit 0 ;;",
                b = booted.display(),
                d = downed.display(),
                f = folder.display()
            );
            let bin = stub_firecode(&fc, &arms);
            let mut spec = spec_with(&folder, &root.join("cache"), &bin);
            spec.mount_rw = true;
            let b = FirecodeBackend::up(&spec).unwrap();
            assert_eq!(
                !booted.exists(),
                joins,
                "mount {mount}: booted={}",
                booted.exists()
            );
            assert_eq!(!b.owns_vm(), joins, "a joined VM is not ours");
            if joins {
                let answer = b.close().unwrap();
                assert!(answer.contains("joined"), "{answer}");
                drop(b);
                assert!(!downed.exists(), "a joiner never brings the VM down");
            }
        }
    }

    /// **The parent's acceptance check reaches `firecode up`**, as `--verify <command>`, so it is
    /// firecode - not the child - that runs it when the VM comes down.
    #[test]
    fn the_acceptance_check_is_handed_to_firecode_up() {
        let root = tmp("verify-up");
        let src = root.join("src");
        file(&src.join("a.txt"), 10);
        let fc = root.join("fc");
        let argv = root.join("up-argv");
        let arms = format!(
            "up) printf '%s\\n' \"$@\" > '{a}'; exit 0 ;;\n\
             info) echo '{{\"run\":\"r1\"}}' ;;\n\
             layer) exit 0 ;;",
            a = argv.display()
        );
        let bin = stub_firecode(&fc, &arms);
        let mut spec = spec_with(&src, &root.join("cache"), &bin);
        spec.verify = Some("make e2e TEST=x".into());
        let _b = FirecodeBackend::up(&spec).unwrap();
        let said = std::fs::read_to_string(&argv).unwrap();
        let args: Vec<&str> = said.lines().collect();
        let at = args
            .iter()
            .position(|a| *a == "--verify")
            .expect("--verify reaches up");
        assert_eq!(
            args.get(at + 1),
            Some(&"make e2e TEST=x"),
            "and carries the command whole"
        );
    }

    /// **A failed check is the answer's headline**, beside where the work is - read from
    /// firecode's record of the run, not from anything the child said.
    #[test]
    fn the_answer_carries_the_checks_verdict_and_where_the_work_is() {
        for (status, says) in [
            ("1", "ACCEPTANCE CHECK FAILED: `make e2e` exited 1"),
            ("0", "ACCEPTANCE CHECK PASSED: `make e2e` exited 0"),
            ("missing", "ACCEPTANCE CHECK GAVE NO VERDICT"),
        ] {
            let root = tmp(&format!("verdict-{status}"));
            let src = root.join("src");
            file(&src.join("a.txt"), 10);
            let fc = root.join("fc");
            let delivered = root.join("work");
            let arms = format!(
                "{HAPPY}\n\
                 down) mkdir -p \"$ROOT/runs/r1\" '{d}'; echo '{status}' > \"$ROOT/runs/r1/verify-status\"; \
                 echo 'done {d}' > \"$ROOT/runs/r1/copy_out\"; exit 0 ;;",
                d = delivered.display()
            );
            let bin = stub_firecode(&fc, &arms);
            let mut spec = spec_with(&src, &root.join("cache"), &bin);
            spec.verify = Some("make e2e".into());
            let b = FirecodeBackend::up(&spec).unwrap();
            let answer = b.close().unwrap();
            assert!(answer.contains(says), "for status {status}: {answer}");
            assert!(
                answer.contains(&delivered.display().to_string()),
                "the delivered path is in the answer: {answer}"
            );
        }
    }

    /// **A failed copy-out is said as one**, and points at the copy that is now the only place
    /// the work is - it used to fall through to "nothing appeared within 10 s".
    #[test]
    fn a_failed_copy_out_is_reported_as_a_failure_not_as_silence() {
        let root = tmp("copy-failed");
        let src = root.join("src");
        file(&src.join("a.txt"), 10);
        let fc = root.join("fc");
        let arms = format!(
            "{HAPPY}\n\
             down) mkdir -p \"$ROOT/runs/r1\"; echo 'failed: no space left' > \"$ROOT/runs/r1/copy_out\"; exit 0 ;;"
        );
        let bin = stub_firecode(&fc, &arms);
        let b = FirecodeBackend::up(&spec_with(&src, &root.join("cache"), &bin)).unwrap();
        let answer = b.close().unwrap();
        assert!(answer.contains("failed: no space left"), "{answer}");
        assert!(answer.contains("only place the work is"), "{answer}");
        assert!(!answer.contains("within 10 s"), "{answer}");
        assert!(
            root.join("cache").join("child").exists(),
            "the copy is kept"
        );
    }

    /// **A daemon that stops brings its children's VMs down** — the backends of subagents run on
    /// threads `process::exit` does not unwind, so their `Drop` never ran and the VMs outlived
    /// the daemon (two of the acceptance suite's own runs, 2026-10-08). `down_all` is the
    /// explicit half the daemon calls on its way out.
    #[test]
    fn down_all_brings_down_a_vm_whose_backend_is_still_alive() {
        let _alone = DOWN_ALL.lock().unwrap_or_else(|e| e.into_inner());
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

    /// **One round trip for the whole tree, end to end** — the fix, and the reason
    /// [`ExecBackend::list_tree`] exists. The VM-placed session's first `glob` spent
    /// 80.48 s of a 125 s boot-to-answer on 122 per-directory `list` calls at
    /// 0.645–0.688 s each (measured 2026-10-08).
    ///
    /// The stub answers the one `find` with the guest's listing — `find`'s own order,
    /// a name with a space, a directory entry, and a `target/` whose contents the skip
    /// set must drop — and what comes back is what the per-directory loop would have
    /// produced for the same tree. The call log is the count: `up` probes the door
    /// with one `in ... true`, so the claim is the *delta* — one more round trip for
    /// the whole tree, or the tree went back to being N of them.
    #[test]
    fn the_tree_is_one_in_call_and_parses_like_the_loop() {
        let root = tmp("tree");
        let src = root.join("src");
        file(&src.join("a.txt"), 10);
        let cache = root.join("cache");
        let copy = cache.join("child");
        let calls = root.join("in-calls");
        let arms = format!(
            "{HAPPY}\n\
             in) echo \"$@\" >> '{calls}'; printf 'd\\t0\\t{c}/a b\\nf\\t7\\t{c}/a b/inner.txt\\n\
             f\\t12\\t{c}/plain.txt\\nd\\t0\\t{c}/target\\nf\\t999\\t{c}/target/junk\\n' ;;",
            calls = calls.display(),
            c = copy.display()
        );
        let bin = stub_firecode(&root.join("fc"), &arms);
        let b = FirecodeBackend::up(&spec_with(&src, &cache, &bin)).unwrap();

        let in_calls = || {
            std::fs::read_to_string(&calls)
                .unwrap_or_default()
                .lines()
                .count()
        };
        let before = in_calls();
        let (got, truncated) = crate::backend::walk(
            &b,
            &copy.display().to_string(),
            100,
            &crate::backend::default_skip,
        );
        let log = std::fs::read_to_string(&calls).unwrap_or_default();

        let at = |rel: &str, name: &str, is_dir: bool, bytes: u64| DirEntry {
            path: format!("{}/{rel}", copy.display()),
            name: name.into(),
            is_dir,
            bytes,
        };
        assert_eq!(
            got,
            vec![
                at("a b", "a b", true, 0),
                at("plain.txt", "plain.txt", false, 12),
                at("a b/inner.txt", "inner.txt", false, 7),
            ],
            "the tree came back in find's order, or with the skipped target's contents"
        );
        assert!(!truncated);
        // The tree is one round trip: exactly one `in` ran a `find`. `up` probes the
        // door with an `in ... true` of its own, so the count is of tree commands and
        // not of every call the backend made.
        let tree: Vec<&str> = log.lines().filter(|l| l.contains("find ")).collect();
        assert_eq!(
            tree.len(),
            1,
            "the tree took {} round trips, not one (in calls {before} -> {}): {log}",
            tree.len(),
            in_calls()
        );
        // And the one call really is the whole tree: no depth limit, and the path
        // form the parser needs.
        let tree_call = tree.first().copied().unwrap_or_default();
        assert!(
            tree_call.contains("-mindepth 1")
                && tree_call.contains("%p")
                && !tree_call.contains("-maxdepth"),
            "the one command is not the whole tree: {tree_call}"
        );
    }
}
