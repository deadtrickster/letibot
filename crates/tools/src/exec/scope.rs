//! Cgroup-scoped process lifetime, per `TODO.md` T24, and the record that makes
//! its zero falsifiable.
//!
//! # Three scopes and no fourth
//!
//! ```text
//!   letibot.<pid>.<n>/                  one tree's root — see `probe_under`
//!     session.<sid>/                    ScopeKind::Session
//!       turn.<tid>/                     ScopeKind::Turn — a child DIRECTORY
//!         job.<n>/                      one command
//!     explicit.<name>/                  ScopeKind::Explicit — a SIBLING of the session
//! ```
//!
//! [`ScopeKind::Explicit`] is a sibling and not a child, and that placement is the
//! whole meaning of the scope: ending the session must **not** reap it. `.78`
//! supplied the case — four git worktrees that were not a leak at all, three of
//! them live builds under active comparison. *"A long-lived resource with a
//! declared owner is correct."* So `explicit` exists to be declared, and
//! [`ScopeTree::list`] shows it as surviving.
//!
//! # Two kernel facts this depends on, written down because they are the traps
//!
//! 1. **No controller is ever enabled in `cgroup.subtree_control`.** cgroup v2's
//!    "no internal processes" rule only binds a cgroup that has controllers
//!    delegated to its children; everything here needs is `cgroup.procs`,
//!    `cgroup.kill` and `cgroup.events`, which are **core** files present in every
//!    cgroup. Enabling a controller would make the harness's own cgroup illegal.
//! 2. **A process joins its cgroup before `exec`, not after `fork`.** See
//!    [`join_script`]: the wrapper writes its own pid to `cgroup.procs` and *then*
//!    `exec`s, so there is no window in which a descendant could be forked outside
//!    the scope. If the write fails the command **is not run** — a process that
//!    could not be owned is the leak, so it is refused rather than orphaned.
//!
//! # The record
//!
//! [`Reaping`] carries what was observed before the kill, what mechanism did it,
//! and what was still there after. `docs/closed-loop.md` §3's ordering — *only
//! count absence after presence* — is the shape of the struct, not a comment on
//! it: a reap that observed nothing and a reap that killed three both produce a
//! record, and they do not look alike.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use super::ExecError;

/// Who owns a process's lifetime. Three, and the enum is closed on purpose:
/// T24 says *"three scopes and no fourth"*, and a fourth would be a scope nobody
/// decided the reaping rule for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ScopeKind {
    /// Dies when the turn ends. The default for a command the model waits on.
    Turn,
    /// Dies when the session ends. The default for a background job — D4's *"if a
    /// vm is temporary then it is a session cgroup"*.
    Session,
    /// **Survives the session, because somebody said so**, and is listed as such.
    Explicit,
}

impl ScopeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ScopeKind::Turn => "turn",
            ScopeKind::Session => "session",
            ScopeKind::Explicit => "explicit",
        }
    }

    /// What a caller is told about this scope's end, in the words `job_list` uses.
    pub fn reaped_when(&self) -> &'static str {
        match self {
            ScopeKind::Turn => "reaped when this turn ends",
            ScopeKind::Session => "reaped when this session ends",
            ScopeKind::Explicit => "SURVIVES this session; nothing reaps it but an explicit kill",
        }
    }

    pub fn parse(s: &str) -> Option<ScopeKind> {
        match s {
            "turn" => Some(ScopeKind::Turn),
            "session" => Some(ScopeKind::Session),
            "explicit" => Some(ScopeKind::Explicit),
            _ => None,
        }
    }
}

/// A scope's identity: what it is called, what kind it is, and where its cgroup is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeId {
    pub kind: ScopeKind,
    pub name: String,
    /// The cgroup directory. Absolute, under `/sys/fs/cgroup`.
    pub path: PathBuf,
}

impl std::fmt::Display for ScopeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.kind.as_str(), self.name)
    }
}

/// One process a reap found, named before it was killed.
///
/// The `cmdline` is what makes the record usable rather than merely present: a
/// list of pids from an hour ago identifies nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reaped {
    pub pid: u32,
    pub comm: String,
    pub cmdline: String,
}

impl Reaped {
    /// Read what a pid is, from `/proc`, while it is still there to read.
    #[cfg(target_os = "macos")]
    pub fn observe(pid: u32) -> Reaped {
        Reaped {
            pid,
            comm: super::darwin::info(pid).map(|i| i.comm).unwrap_or_default(),
            cmdline: super::darwin::cmdline(pid),
        }
    }

    #[cfg(not(target_os = "macos"))]
    pub fn observe(pid: u32) -> Reaped {
        let comm = std::fs::read_to_string(format!("/proc/{pid}/comm"))
            .unwrap_or_default()
            .trim()
            .to_string();
        let cmdline = std::fs::read(format!("/proc/{pid}/cmdline"))
            .map(|b| cmdline_of(&b))
            .unwrap_or_default();
        Reaped { pid, comm, cmdline }
    }

    /// How this process should be named in a record.
    ///
    /// **`cmdline` can legitimately be empty**, and this cost a test: a process
    /// caught between `fork` and `exec` has no `mm` yet, so `/proc/<pid>/cmdline`
    /// reads as zero bytes and the record said `pid 566758 ?` about a `sleep 60`
    /// somebody would later need to identify. So the fallback chain is stated:
    /// the command line, then `comm`, then a sentence saying the read lost the
    /// race — never a bare `?`, which is indistinguishable from a bug.
    pub fn label(&self) -> String {
        if !self.cmdline.is_empty() {
            return self.cmdline.clone();
        }
        if !self.comm.is_empty() {
            return format!(
                "[{}] (no command line: caught between fork and exec)",
                self.comm
            );
        }
        "(gone before it could be identified: /proc had neither a command line nor a name)"
            .to_string()
    }
}

/// `/proc/<pid>/cmdline` is NUL-separated and NUL-terminated.
#[cfg(any(not(target_os = "macos"), test))]
pub(crate) fn cmdline_of(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .split('\0')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// What a scope's end actually did. **Not a boolean.**
///
/// The fleet's finding, one layer up: a zero produced by vigilance and a zero
/// produced by a mechanism are the same number and different facts. This struct
/// is the difference — `observed` is the presence half and `survivors` is the
/// falsifier, so a reaper that silently did nothing cannot be mistaken for a
/// scope that was empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reaping {
    pub scope: ScopeId,
    pub at: SystemTime,
    /// `cgroup.kill`, or the per-pid fallback, or `nothing to kill`. Named, because
    /// a fallback that is not visible is a fallback nobody knows they are running.
    pub mechanism: &'static str,
    /// Members read out of the cgroup **before** the kill. The denominator.
    pub observed: Vec<Reaped>,
    /// Members still there **after** the kill and the wait. Non-empty is a defect
    /// and the record says so rather than the reaper claiming success.
    pub survivors: Vec<u32>,
    /// How long the wait for `populated 0` took.
    pub waited: Duration,
    /// Whether the cgroup directory itself is gone.
    pub removed: bool,
    /// Anything that needs a sentence: a fallback taken, a directory that would
    /// not go away.
    pub note: Option<String>,
}

impl Reaping {
    /// One line for a listing, and it never says "ok".
    pub fn summary(&self) -> String {
        let mut s = format!(
            "{}: observed {} process(es), killed by {}, {} survivor(s)",
            self.scope,
            self.observed.len(),
            self.mechanism,
            self.survivors.len()
        );
        if !self.observed.is_empty() {
            s.push_str(&format!(
                " [{}]",
                self.observed
                    .iter()
                    .map(|r| format!("{} {}", r.pid, first_word(&r.label())))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if let Some(n) = &self.note {
            s.push_str(&format!(" — {n}"));
        }
        s
    }

    /// Did this reap leave anything behind? The only honest success test.
    pub fn clean(&self) -> bool {
        self.survivors.is_empty() && self.removed
    }
}

fn first_word(s: &str) -> &str {
    s.split_whitespace().next().unwrap_or("(unidentified)")
}

/// What moving a scope's processes into another scope actually did. **Not a
/// boolean**, and for the same reason [`Reaping`] is not one.
///
/// A promotion is a claim about *lifetime*: after it, this work is reaped by a
/// different scope. If some process did not move, the claim is false for that
/// process — it will still die when the old scope ends — and a `promoted: true`
/// that hid it would be the unfalsifiable zero in the other direction. So the
/// three numbers travel together: what was there, what is in the new scope, and
/// what is still in the old one.
///
/// `observed.len() != moved.len() + left_behind.len()` is a real and benign case:
/// a process can exit between the read and the write. It is stated in [`Migration::note`]
/// rather than papered over, because "vanished" and "would not move" are different
/// facts and only the second is a defect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Migration {
    pub from: ScopeId,
    pub to: ScopeId,
    /// Members read out of `from` **before** anything moved. The denominator.
    pub observed: Vec<Reaped>,
    /// Pids found in `to` afterwards.
    pub moved: Vec<u32>,
    /// Pids still in `from` afterwards. Non-empty means the promotion is
    /// **partial**, and those processes still die with the old scope.
    pub left_behind: Vec<u32>,
    /// How many passes it took. More than one means something forked mid-move,
    /// which is the case a single pass gets wrong.
    pub passes: usize,
    pub waited: Duration,
    /// Whether `from`'s directory is gone afterwards.
    pub removed: bool,
    pub mechanism: &'static str,
    pub note: Option<String>,
}

impl Migration {
    /// Did every process that still existed end up in the new scope?
    ///
    /// Deliberately does **not** require `removed`: a cgroup directory that
    /// would not go away is debris, and debris is [`Migration::note`]'s business,
    /// but it is not a process whose lifetime is now owned by the wrong scope.
    pub fn complete(&self) -> bool {
        self.left_behind.is_empty()
    }

    /// Processes that were there at the start and are in neither scope now.
    pub fn vanished(&self) -> usize {
        self.observed
            .len()
            .saturating_sub(self.moved.len() + self.left_behind.len())
    }

    pub fn summary(&self) -> String {
        let mut s = format!(
            "{} → {}: observed {} process(es), {} moved, {} left behind, {} pass(es)",
            self.from,
            self.to,
            self.observed.len(),
            self.moved.len(),
            self.left_behind.len(),
            self.passes
        );
        if self.vanished() > 0 {
            s.push_str(&format!(
                ", {} exited while the move was happening",
                self.vanished()
            ));
        }
        if let Some(n) = &self.note {
            s.push_str(&format!(" — {n}"));
        }
        s
    }
}

/// The lifetime mechanism, as a seam.
///
/// A trait for the same reason [`crate::backend::ExecBackend`] is one: a firecode
/// guest owns its processes by owning the whole VM, and the tools above this line
/// do not change when that is what is underneath. What the tools require is only
/// that a scope can be opened, that its membership can be *counted* without a
/// pattern, and that ending it produces a [`Reaping`].
pub trait ScopeTree: Send + Sync {
    /// One line for `EXPLAIN`, naming the mechanism and where its root is.
    fn describe(&self) -> String;

    /// Open a scope. `parent` is `None` for a top-level scope; a child scope's
    /// cgroup is created **inside** the parent's, which is what makes the parent's
    /// end reap it.
    fn open(
        &self,
        kind: ScopeKind,
        name: &str,
        parent: Option<&ScopeId>,
    ) -> Result<ScopeId, ExecError>;

    /// The pids in this scope and every scope under it. **This is what replaces
    /// `pgrep`**: there is no pattern, so there is nothing that can match the
    /// process asking.
    fn members(&self, scope: &ScopeId) -> Result<Vec<u32>, ExecError>;

    /// Kill everything in this scope and everything under it, and record it.
    ///
    /// Takes the observation before the kill and the survivor check after, so the
    /// caller cannot accidentally record only one of them.
    fn end(&self, scope: &ScopeId) -> Reaping;

    /// **Move every process in `from` into `to`**, so that a different scope
    /// reaps them from now on. This is what promotion is, underneath.
    ///
    /// No default implementation on purpose. A tree that cannot migrate must say
    /// so ([`NoScopes`] does), because a default that silently returned "moved 0
    /// of 0" would let a caller announce a promotion that never happened — and
    /// the process would then die with the turn while the model held a handle it
    /// believed outlived one.
    fn migrate(&self, from: &ScopeId, to: &ScopeId) -> Result<Migration, ExecError>;

    /// Every scope this tree has open, for a listing that can show what is
    /// watching and for whom. *"An invisible watcher is an unreapable one."*
    fn list(&self) -> Vec<ScopeId>;

    /// Remove the tree's own empty directories, and report how many went.
    ///
    /// **Found by looking rather than by a failing test**, which is the half of
    /// T24 that is easy to skip: after a green run, zero processes had leaked and
    /// six empty `letibot.<pid>/` directories had. Nothing was holding a process,
    /// so no reap record was wrong — and the count still went up by one per
    /// session, which is *precisely* the shape of the thirteen abandoned worktrees
    /// this entry was opened about. Debris that holds nothing is still debris.
    ///
    /// Only unpopulated directories go. A populated one is somebody's live work,
    /// including a declared [`ScopeKind::Explicit`] scope, and removing it is not
    /// possible anyway — the kernel refuses.
    fn prune(&self) -> usize {
        0
    }
}

/// The default: **there is no lifetime mechanism**, and it says so.
///
/// Deliberately not a tree that pretends to own things. Same discipline as
/// [`crate::runtime::NoBoundary`]: a mechanism that reports success while owning
/// nothing is worse than an absent one, because it reads as protection.
#[derive(Debug, Clone)]
pub struct NoScopes {
    /// Why there is none. Filled by [`Cgroup2::probe`]'s failure so a caller is
    /// told what was missing rather than that something was.
    pub why: String,
}

impl NoScopes {
    pub fn new(why: impl Into<String>) -> Self {
        NoScopes { why: why.into() }
    }
}

impl ScopeTree for NoScopes {
    fn describe(&self) -> String {
        format!("none — {}", self.why)
    }
    fn open(&self, _k: ScopeKind, _n: &str, _p: Option<&ScopeId>) -> Result<ScopeId, ExecError> {
        Err(ExecError::NoScopes(self.why.clone()))
    }
    fn members(&self, _s: &ScopeId) -> Result<Vec<u32>, ExecError> {
        Err(ExecError::NoScopes(self.why.clone()))
    }
    fn end(&self, scope: &ScopeId) -> Reaping {
        Reaping {
            scope: scope.clone(),
            at: SystemTime::now(),
            mechanism: "nothing — there is no scope mechanism",
            observed: vec![],
            survivors: vec![],
            waited: Duration::ZERO,
            removed: false,
            note: Some(self.why.clone()),
        }
    }
    fn list(&self) -> Vec<ScopeId> {
        vec![]
    }
    fn migrate(&self, _from: &ScopeId, _to: &ScopeId) -> Result<Migration, ExecError> {
        Err(ExecError::NoScopes(self.why.clone()))
    }
}

/// cgroup v2, rooted inside the harness's **own** cgroup.
///
/// Rooting it there rather than at `/sys/fs/cgroup` is not a convenience: a
/// delegated subtree is the only place an unprivileged process may create
/// cgroups, and it also means that if the harness itself is killed by something
/// above it — a systemd scope going away, a `byobu` session ending — the whole
/// subtree goes with it. The mechanism is the same one, one level up.
#[derive(Debug, Clone)]
pub struct Cgroup2 {
    root: PathBuf,
    open: std::sync::Arc<std::sync::Mutex<Vec<ScopeId>>>,
}

/// Where a delegated cgroup v2 hierarchy is mounted.
const CGROUP_FS: &str = "/sys/fs/cgroup";

/// Distinguishes the roots of two trees in one process. See [`Cgroup2::probe_under`].
static TREE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl Cgroup2 {
    /// Find the harness's own cgroup, prove it is writable, and make a root under
    /// it. Every failure names what was missing.
    ///
    /// The proof is a real `mkdir`/`rmdir`, not a permission bit: on a delegated
    /// subtree the directory can be mode 755 and owned by the user while the
    /// kernel still refuses, and the reverse. **Guard the fact, not the proxy.**
    pub fn probe() -> Result<Cgroup2, ExecError> {
        Self::probe_under(&own_cgroup()?)
    }

    /// The probe, against a chosen parent. Tests use this to build a tree that is
    /// a sibling of the harness's own rather than nested in it.
    pub fn probe_under(parent: &Path) -> Result<Cgroup2, ExecError> {
        if std::fs::metadata(CGROUP_FS).is_err() {
            return Err(ExecError::NoScopes(format!(
                "`{CGROUP_FS}` is not there, so this is not a cgroup v2 host"
            )));
        }
        // **One root per tree, not one per process.** A daemon runs several
        // sessions in one process, and a shared root means one session's
        // housekeeping walks another's directories — which showed up first as two
        // parallel tests racing each other's `rmdir` and reporting *"every process
        // is gone but the cgroup directory would not be removed"* about a scope
        // that had been removed, by somebody else, correctly.
        let root = parent.join(format!(
            "letibot.{}.{}",
            std::process::id(),
            TREE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).map_err(|e| ExecError::Cgroup {
            op: "mkdir",
            path: root.display().to_string(),
            why: format!(
                "{e} — a delegated cgroup v2 subtree is needed; \
                 `{}` is the harness's own cgroup and it is not writable",
                parent.display()
            ),
        })?;
        // The kill file is the mechanism; a tree without it would fall back
        // silently, which is the thing this module exists to not do.
        if std::fs::metadata(root.join("cgroup.procs")).is_err() {
            return Err(ExecError::Cgroup {
                op: "stat cgroup.procs",
                path: root.display().to_string(),
                why: "the directory was created but has no cgroup core files".into(),
            });
        }
        Ok(Cgroup2 {
            root,
            open: Default::default(),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `cgroup.procs`, for the shell wrapper to write its own pid into.
    pub fn procs_path(scope: &ScopeId) -> PathBuf {
        scope.path.join("cgroup.procs")
    }

    /// Whether this scope, or anything under it, holds a process.
    ///
    /// **This is the replacement for `until pgrep -f X`.** It reads
    /// `cgroup.events`, which the kernel maintains; there is no pattern, so there
    /// is no predicate that can match its own waiter.
    pub fn populated(&self, scope: &ScopeId) -> bool {
        populated_at(&scope.path)
    }
}

/// Whether a cgroup directory, or anything under it, holds a process.
///
/// `pub(crate)` because [`super::monitor`] is the other caller: a scope monitor
/// asks exactly this question, and asking it through the kernel's own
/// `cgroup.events` is what makes the monitor's predicate unable to match its own
/// waiter.
#[cfg(not(target_os = "macos"))]
pub(crate) fn populated_at(dir: &Path) -> bool {
    match std::fs::read_to_string(dir.join("cgroup.events")) {
        Ok(s) => s
            .lines()
            .find_map(|l| l.strip_prefix("populated "))
            .map(|v| v.trim() == "1")
            .unwrap_or(false),
        Err(_) => false,
    }
}

/// The harness's own cgroup path, from `/proc/self/cgroup`.
fn own_cgroup() -> Result<PathBuf, ExecError> {
    let text = std::fs::read_to_string("/proc/self/cgroup")
        .map_err(|e| ExecError::NoScopes(format!("`/proc/self/cgroup` could not be read: {e}")))?;
    // cgroup v2 gives exactly one line, `0::<path>`. A v1-only host gives several
    // and none of them start `0::`, which is a fact worth reporting as itself.
    let rel = text
        .lines()
        .find_map(|l| l.strip_prefix("0::"))
        .ok_or_else(|| {
            ExecError::NoScopes(
                "`/proc/self/cgroup` has no `0::` line, so this host is running \
                 cgroup v1 only and there is no unified hierarchy to delegate"
                    .into(),
            )
        })?;
    Ok(PathBuf::from(CGROUP_FS).join(rel.trim().trim_start_matches('/')))
}

/// Read the pids in one cgroup directory and every directory under it.
fn members_at(dir: &Path, out: &mut Vec<u32>) {
    if let Ok(s) = std::fs::read_to_string(dir.join("cgroup.procs")) {
        for line in s.lines() {
            if let Ok(pid) = line.trim().parse::<u32>() {
                out.push(pid);
            }
        }
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                members_at(&e.path(), out);
            }
        }
    }
}

/// Depth-first `rmdir`. A cgroup directory can only be removed when it is empty
/// of processes *and* of child cgroups.
fn remove_tree(dir: &Path) -> bool {
    let mut ok = true;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                ok &= remove_tree(&e.path());
            }
        }
    }
    ok && std::fs::remove_dir(dir).is_ok()
}

impl ScopeTree for Cgroup2 {
    fn describe(&self) -> String {
        format!(
            "cgroup v2, rooted at {} (lifetime only — this is NOT a sandbox)",
            self.root.display()
        )
    }

    fn open(
        &self,
        kind: ScopeKind,
        name: &str,
        parent: Option<&ScopeId>,
    ) -> Result<ScopeId, ExecError> {
        // An `explicit` scope under a SESSION or a TURN would be reaped by that
        // scope's end, which is the one thing it exists not to be — so it is placed
        // at the root instead, and the rule lives in the placement rather than in a
        // comment asking for it.
        //
        // The qualifier matters and cost a test: `(Explicit, _) => root` also sent
        // every *job* cgroup inside an explicit scope to the root, so ending that
        // scope found it empty and the reap record said `observed 0` about three
        // live processes. An unfalsifiable zero, produced by the code written to
        // make zeroes falsifiable.
        let base = match parent {
            Some(p) if kind == ScopeKind::Explicit && p.kind != ScopeKind::Explicit => {
                self.root.clone()
            }
            Some(p) => p.path.clone(),
            None => self.root.clone(),
        };
        let dir = base.join(format!("{}.{}", kind.as_str(), sanitise(name)));
        std::fs::create_dir_all(&dir).map_err(|e| ExecError::Cgroup {
            op: "mkdir",
            path: dir.display().to_string(),
            why: e.to_string(),
        })?;
        let id = ScopeId {
            kind,
            name: name.to_string(),
            path: dir,
        };
        let mut open = self.open.lock().expect("scope list");
        if !open.contains(&id) {
            open.push(id.clone());
        }
        Ok(id)
    }

    fn members(&self, scope: &ScopeId) -> Result<Vec<u32>, ExecError> {
        if std::fs::metadata(&scope.path).is_err() {
            return Err(ExecError::NoSuchScope(scope.to_string()));
        }
        let mut out = Vec::new();
        members_at(&scope.path, &mut out);
        out.sort_unstable();
        out.dedup();
        Ok(out)
    }

    /// Presence, then the kill, then absence — in that order, and all three in the
    /// record.
    fn end(&self, scope: &ScopeId) -> Reaping {
        let started = Instant::now();
        let at = SystemTime::now();

        // 1. PRESENCE. Read the members and what they are, while they exist to be
        //    read. This is the denominator; without it a later `0 survivors` says
        //    nothing about whether anything was ever there.
        let mut pids = Vec::new();
        members_at(&scope.path, &mut pids);
        pids.sort_unstable();
        pids.dedup();
        let observed: Vec<Reaped> = pids.iter().map(|p| Reaped::observe(*p)).collect();

        // 2. THE KILL.
        let mut note = None;
        let mechanism = if observed.is_empty() {
            "nothing to kill"
        } else {
            match kill_tree(&scope.path) {
                Ok(()) => CGROUP_KILL,
                Err(e) => {
                    // A fallback that is not visible is a fallback nobody knows
                    // they are running.
                    note = Some(format!(
                        "`cgroup.kill` was unusable ({e}); fell back to SIGKILL"
                    ));
                    for p in &pids {
                        let _ = std::process::Command::new("kill")
                            .arg("-KILL")
                            .arg(p.to_string())
                            .status();
                    }
                    "SIGKILL per pid"
                }
            }
        };

        // 3. ABSENCE, and only now. `cgroup.kill` returns before the processes are
        //    actually reaped, so a check that ran immediately would report a clean
        //    kill of a cgroup still holding three processes.
        let deadline = Instant::now() + Duration::from_secs(5);
        while populated_at(&scope.path) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut survivors = Vec::new();
        members_at(&scope.path, &mut survivors);
        survivors.sort_unstable();
        survivors.dedup();

        let removed = remove_tree(&scope.path);
        if !removed && survivors.is_empty() && note.is_none() {
            note = Some(
                "every process is gone but the cgroup directory would not be removed".to_string(),
            );
        }
        self.open.lock().expect("scope list").retain(|s| s != scope);

        Reaping {
            scope: scope.clone(),
            at,
            mechanism,
            observed,
            survivors,
            waited: started.elapsed(),
            removed,
            note,
        }
    }

    /// **Presence, then the move, then presence again — and both counts kept.**
    ///
    /// cgroup v2 moves a process by writing its pid into the target's
    /// `cgroup.procs`, one pid per write. Two things follow and both are handled
    /// here rather than assumed away:
    ///
    /// 1. **A write can fail for a benign reason.** `ESRCH` means the process
    ///    exited between the read and the write, which is not a failed migration;
    ///    it is a process that is no longer anybody's to own. Counted as
    ///    [`Migration::vanished`], not as `left_behind`.
    /// 2. **A process can fork while the move is happening.** A child forked
    ///    before its parent moved is in the OLD cgroup and stays there, so one
    ///    pass is not enough. The loop repeats until a pass finds the source
    ///    empty or the budget runs out — and if the budget runs out it reports
    ///    what is still there rather than claiming the move finished.
    ///
    /// Membership is checked by reading the cgroups afterwards, never by counting
    /// successful writes: a write that returned `Ok` and a process that is in the
    /// new cgroup are two facts, and only the second is the one being claimed.
    fn migrate(&self, from: &ScopeId, to: &ScopeId) -> Result<Migration, ExecError> {
        if std::fs::metadata(&from.path).is_err() {
            return Err(ExecError::NoSuchScope(from.to_string()));
        }
        if std::fs::metadata(&to.path).is_err() {
            return Err(ExecError::NoSuchScope(to.to_string()));
        }
        let started = Instant::now();

        // 1. PRESENCE, before anything moves. The denominator.
        let mut observed_pids = Vec::new();
        members_at(&from.path, &mut observed_pids);
        observed_pids.sort_unstable();
        observed_pids.dedup();
        let observed: Vec<Reaped> = observed_pids.iter().map(|p| Reaped::observe(*p)).collect();

        // 2. THE MOVE, repeated until the source is empty or the budget is spent.
        let procs = to.path.join("cgroup.procs");
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut passes = 0usize;
        let mut last_err: Option<String> = None;
        loop {
            let mut pids = Vec::new();
            members_at(&from.path, &mut pids);
            pids.sort_unstable();
            pids.dedup();
            if pids.is_empty() {
                break;
            }
            passes += 1;
            for pid in &pids {
                let w = std::fs::OpenOptions::new()
                    .write(true)
                    .open(&procs)
                    .and_then(|mut f| writeln!(f, "{pid}"));
                if let Err(e) = w {
                    // ESRCH is "it exited", which is not a failure to migrate.
                    // Anything else is worth a sentence, and the LAST one is kept
                    // rather than the first: a first error from a pid that then
                    // exited would name the least interesting of them.
                    if e.raw_os_error() != Some(3) {
                        last_err = Some(format!("pid {pid}: {e}"));
                    }
                }
            }
            if Instant::now() >= deadline || passes >= 8 {
                break;
            }
        }

        // 3. ABSENCE, and only now — read out of the kernel rather than inferred
        //    from how many writes returned Ok.
        let mut left_behind = Vec::new();
        members_at(&from.path, &mut left_behind);
        left_behind.sort_unstable();
        left_behind.dedup();
        let mut moved = Vec::new();
        members_at(&to.path, &mut moved);
        moved.sort_unstable();
        moved.dedup();
        // Only the ones this migration is about. The target scope may already have
        // held processes of its own, and counting those as "moved" would inflate
        // the number with work somebody else did.
        moved.retain(|p| observed_pids.contains(p));

        let mut note = last_err.map(|e| format!("a pid could not be written into the target: {e}"));
        let removed = if left_behind.is_empty() {
            remove_tree(&from.path)
        } else {
            false
        };
        if !left_behind.is_empty() && note.is_none() {
            note = Some(format!(
                "{} process(es) would not move after {passes} pass(es); they are STILL \
                 owned by `{from}` and die when it does",
                left_behind.len()
            ));
        }
        self.open.lock().expect("scope list").retain(|s| s != from);

        Ok(Migration {
            from: from.clone(),
            to: to.clone(),
            observed,
            moved,
            left_behind,
            passes,
            waited: started.elapsed(),
            removed,
            mechanism: "write pid to cgroup.procs",
            note,
        })
    }

    fn list(&self) -> Vec<ScopeId> {
        self.open.lock().expect("scope list").clone()
    }

    fn prune(&self) -> usize {
        let mut n = 0;
        prune_at(&self.root, &mut n);
        // The root itself, last and only if everything under it went. `remove_dir`
        // on a populated cgroup fails, so this cannot take a live scope by mistake.
        if std::fs::remove_dir(&self.root).is_ok() {
            n += 1;
        }
        n
    }
}

/// Depth-first: children first, then this one if it holds no process.
fn prune_at(dir: &Path, n: &mut usize) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                let child = e.path();
                prune_at(&child, n);
                let mut pids = Vec::new();
                members_at(&child, &mut pids);
                if pids.is_empty() && std::fs::remove_dir(&child).is_ok() {
                    *n += 1;
                }
            }
        }
    }
}

/// The [`Reaping::mechanism`] a cgroup reap records.
pub const CGROUP_KILL: &str = "cgroup.kill";
/// The [`Reaping::mechanism`] a process-group reap records (macOS).
pub const PGROUP_KILL: &str = "killpg SIGKILL per process group";
/// **What a reap on this host's own tree is recorded as** — the name [`host_tree`]'s kill
/// goes by, for a test or a reader that asserts on it.
pub const HOST_KILL: &str = if cfg!(target_os = "macos") {
    PGROUP_KILL
} else {
    CGROUP_KILL
};

/// **The lifetime mechanism this host has**: cgroup v2 on Linux, process groups on
/// macOS. Every production caller that used to say `Cgroup2::probe()` asks this,
/// so the choice is made once.
pub fn host_tree() -> Result<Box<dyn ScopeTree>, ExecError> {
    #[cfg(target_os = "macos")]
    return Ok(Box::new(ProcessGroups::probe()?));
    #[cfg(not(target_os = "macos"))]
    return Ok(Box::new(Cgroup2::probe()?));
}

/// **The live processes in a scope and every scope under it**, asked the way this host's
/// tree asks it — with no tree instance needed, because both answers are a function of
/// the scope's directory. On Linux that is `cgroup.procs`, which the kernel keeps to the
/// live members. On macOS `cgroup.procs` holds the recorded **group ids**, which stay
/// until the reaper removes the directory — so a reader of the file sees a group a kill
/// has already emptied, and only this, which asks which of those groups still has a
/// member, is the membership.
pub fn live_members(scope: &ScopeId) -> Vec<u32> {
    #[cfg(target_os = "macos")]
    return pg_members(&scope.path);
    #[cfg(not(target_os = "macos"))]
    {
        let mut out = Vec::new();
        members_at(&scope.path, &mut out);
        out.sort_unstable();
        out.dedup();
        out
    }
}

/// Whether a scope directory, or anything under it, holds a live process — the
/// macOS reading of the same question, from the recorded process groups.
#[cfg(target_os = "macos")]
pub(crate) fn populated_at(dir: &Path) -> bool {
    let mut pids = Vec::new();
    pg_members_at(dir, &mut pids);
    !pids.is_empty()
}

/// **Process groups, on macOS** — the same three scopes with a weaker floor.
///
/// macOS has no cgroups. What it has is the process group: a number every process
/// carries, inherited across `fork`, and `killpg` to signal all of them at once. So
/// the tree keeps the cgroup *layout* — one directory per scope, a `cgroup.procs`
/// file per job — in the temp dir, and what [`join_script`] writes into that file
/// is the wrapper's own pid, which is its **process group id** because every
/// spawn makes the wrapper a group leader (`setsid` on the pty paths,
/// `process_group(0)` on the plain one). Membership is then "every live process
/// whose group is one this scope recorded", read from the kernel, never from a
/// pattern.
///
/// **What is weaker, said rather than discovered:**
///
/// - A process can leave its group (`setsid`, `setpgid`). A cgroup cannot be left.
///   Ordinary tools do not do this; daemons that double-fork do, and such a process
///   is outside the scope and is not reaped.
/// - A group id is a pid, and pids are reused. A group with no members is
///   forgotten as soon as any read notices it is empty, so the window is between
///   the last member's exit and the next read — but it is a window, where a cgroup
///   has none.
/// - There is no `cgroup.kill`: the kill is `killpg(SIGKILL)` per recorded group,
///   repeated until the members are gone, and the record names that mechanism.
#[cfg(target_os = "macos")]
#[derive(Debug, Clone)]
pub struct ProcessGroups {
    root: PathBuf,
    open: std::sync::Arc<std::sync::Mutex<Vec<ScopeId>>>,
}

#[cfg(target_os = "macos")]
impl ProcessGroups {
    /// Make a root under the temp dir (`$TMPDIR`, per user on macOS). Fails only if
    /// it cannot be created, and says where.
    pub fn probe() -> Result<ProcessGroups, ExecError> {
        Self::probe_under(&std::env::temp_dir().join("letibot-scopes"))
    }

    pub fn probe_under(parent: &Path) -> Result<ProcessGroups, ExecError> {
        sweep_orphans(parent);
        let root = parent.join(format!(
            "letibot.{}.{}",
            std::process::id(),
            TREE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).map_err(|e| ExecError::Cgroup {
            op: "mkdir",
            path: root.display().to_string(),
            why: format!("{e} — the process-group scope tree needs a writable temp dir"),
        })?;
        Ok(ProcessGroups {
            root,
            open: Default::default(),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// **Roots whose daemon is gone and which hold nothing**, removed before a new one is made.
///
/// A cgroup root is a child of its daemon's own cgroup and goes when that does; a directory
/// in the temp dir goes when somebody removes it, and a daemon that was killed, or a test
/// binary that never pruned, does not. The owner's pid is in the name (`letibot.<pid>.<n>`),
/// so a root is debris exactly when that process is not alive **and** no recorded group still
/// has a member — a root still holding a live process is somebody's work, whoever made it.
#[cfg(target_os = "macos")]
fn sweep_orphans(parent: &Path) {
    let Ok(rd) = std::fs::read_dir(parent) else {
        return;
    };
    for e in rd.flatten() {
        let name = e.file_name();
        let Some(owner) = name
            .to_str()
            .and_then(|n| n.strip_prefix("letibot."))
            .and_then(|r| r.split('.').next())
            .and_then(|p| p.parse::<u32>().ok())
        else {
            continue;
        };
        if owner == std::process::id() || super::darwin::alive(owner) {
            continue;
        }
        if pg_members(&e.path()).is_empty() {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// The group ids recorded in one scope directory and every directory under it.
#[cfg(target_os = "macos")]
fn pg_recorded_at(dir: &Path, out: &mut Vec<u32>) {
    members_at(dir, out);
}

/// The live processes in the groups a scope recorded, and — the forgetting half —
/// any recorded group found empty is struck from its file, so its number is not
/// held after the kernel is free to hand it to somebody else.
#[cfg(target_os = "macos")]
fn pg_members_at(dir: &Path, out: &mut Vec<u32>) {
    if let Ok(s) = std::fs::read_to_string(dir.join("cgroup.procs")) {
        let recorded: Vec<u32> = s.lines().filter_map(|l| l.trim().parse().ok()).collect();
        let live = super::darwin::members_of_groups(&recorded);
        let groups: Vec<u32> = live
            .iter()
            .filter_map(|p| super::darwin::info(*p).map(|i| i.pgid))
            .collect();
        let kept: Vec<u32> = recorded
            .iter()
            .copied()
            .filter(|g| groups.contains(g))
            .collect();
        if kept.len() != recorded.len() {
            let body: String = kept.iter().map(|g| format!("{g}\n")).collect();
            let _ = std::fs::write(dir.join("cgroup.procs"), body);
        }
        out.extend(live);
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                pg_members_at(&e.path(), out);
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn pg_members(dir: &Path) -> Vec<u32> {
    let mut v = Vec::new();
    pg_members_at(dir, &mut v);
    v.sort_unstable();
    v.dedup();
    v
}

#[cfg(target_os = "macos")]
impl ScopeTree for ProcessGroups {
    fn describe(&self) -> String {
        format!(
            "process groups, rooted at {} (lifetime only — this is NOT a sandbox; \
             a process that calls setsid/setpgid leaves its scope)",
            self.root.display()
        )
    }

    fn open(
        &self,
        kind: ScopeKind,
        name: &str,
        parent: Option<&ScopeId>,
    ) -> Result<ScopeId, ExecError> {
        // The same placement rule as `Cgroup2::open`, for the same reason.
        let base = match parent {
            Some(p) if kind == ScopeKind::Explicit && p.kind != ScopeKind::Explicit => {
                self.root.clone()
            }
            Some(p) => p.path.clone(),
            None => self.root.clone(),
        };
        let dir = base.join(format!("{}.{}", kind.as_str(), sanitise(name)));
        std::fs::create_dir_all(&dir).map_err(|e| ExecError::Cgroup {
            op: "mkdir",
            path: dir.display().to_string(),
            why: e.to_string(),
        })?;
        let id = ScopeId {
            kind,
            name: name.to_string(),
            path: dir,
        };
        let mut open = self.open.lock().expect("scope list");
        if !open.contains(&id) {
            open.push(id.clone());
        }
        Ok(id)
    }

    fn members(&self, scope: &ScopeId) -> Result<Vec<u32>, ExecError> {
        if std::fs::metadata(&scope.path).is_err() {
            return Err(ExecError::NoSuchScope(scope.to_string()));
        }
        Ok(pg_members(&scope.path))
    }

    /// Presence, then `killpg`, then absence — the same three-part record.
    fn end(&self, scope: &ScopeId) -> Reaping {
        let started = Instant::now();
        let at = SystemTime::now();

        let pids = pg_members(&scope.path);
        let observed: Vec<Reaped> = pids.iter().map(|p| Reaped::observe(*p)).collect();

        let mechanism = if observed.is_empty() {
            "nothing to kill"
        } else {
            PGROUP_KILL
        };

        // Repeated, because a member can fork between one `killpg` and its exit; the
        // child is in the same group and the next pass reaches it.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut groups = Vec::new();
        pg_recorded_at(&scope.path, &mut groups);
        loop {
            for g in &groups {
                unsafe { libc::killpg(*g as libc::pid_t, libc::SIGKILL) };
            }
            if pg_members(&scope.path).is_empty() || Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let survivors = pg_members(&scope.path);

        let removed = survivors.is_empty() && std::fs::remove_dir_all(&scope.path).is_ok();
        let note = (!removed && survivors.is_empty())
            .then(|| "every process is gone but the scope directory would not be removed".into());
        self.open.lock().expect("scope list").retain(|s| s != scope);

        Reaping {
            scope: scope.clone(),
            at,
            mechanism,
            observed,
            survivors,
            waited: started.elapsed(),
            removed,
            note,
        }
    }

    /// Moving a group is moving its number: the recorded ids go from `from`'s files
    /// to `to`'s. Membership is then read back from the kernel, as for cgroups.
    fn migrate(&self, from: &ScopeId, to: &ScopeId) -> Result<Migration, ExecError> {
        if std::fs::metadata(&from.path).is_err() {
            return Err(ExecError::NoSuchScope(from.to_string()));
        }
        if std::fs::metadata(&to.path).is_err() {
            return Err(ExecError::NoSuchScope(to.to_string()));
        }
        let started = Instant::now();
        let observed_pids = pg_members(&from.path);
        let observed: Vec<Reaped> = observed_pids.iter().map(|p| Reaped::observe(*p)).collect();

        let mut groups = Vec::new();
        pg_recorded_at(&from.path, &mut groups);
        let mut note = None;
        let w = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(to.path.join("cgroup.procs"))
            .and_then(|mut f| {
                for g in &groups {
                    writeln!(f, "{g}")?;
                }
                Ok(())
            });
        if let Err(e) = w {
            note = Some(format!(
                "the group ids could not be written into the target: {e}"
            ));
        }
        let removed = note.is_none() && std::fs::remove_dir_all(&from.path).is_ok();

        let left_behind = if removed {
            Vec::new()
        } else {
            pg_members(&from.path)
        };
        let mut moved = pg_members(&to.path);
        moved.retain(|p| observed_pids.contains(p));
        if !left_behind.is_empty() && note.is_none() {
            note = Some(format!(
                "{} process(es) would not move; they are STILL owned by `{from}` and die when it does",
                left_behind.len()
            ));
        }
        self.open.lock().expect("scope list").retain(|s| s != from);

        Ok(Migration {
            from: from.clone(),
            to: to.clone(),
            observed,
            moved,
            left_behind,
            passes: 1,
            waited: started.elapsed(),
            removed,
            mechanism: "move process-group ids",
            note,
        })
    }

    fn list(&self) -> Vec<ScopeId> {
        self.open.lock().expect("scope list").clone()
    }

    fn prune(&self) -> usize {
        fn at(dir: &Path, n: &mut usize) {
            if let Ok(rd) = std::fs::read_dir(dir) {
                for e in rd.flatten() {
                    if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        let child = e.path();
                        at(&child, n);
                        if pg_members(&child).is_empty() && std::fs::remove_dir_all(&child).is_ok()
                        {
                            *n += 1;
                        }
                    }
                }
            }
        }
        let mut n = 0;
        at(&self.root, &mut n);
        if pg_members(&self.root).is_empty() && std::fs::remove_dir_all(&self.root).is_ok() {
            n += 1;
        }
        n
    }
}

fn kill_tree(dir: &Path) -> Result<(), std::io::Error> {
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .open(dir.join("cgroup.kill"))?;
    f.write_all(b"1")
}

/// A cgroup directory name is a filename. Anything that is not obviously safe in
/// one becomes `_`, and the original stays in [`ScopeId::name`], so the mapping is
/// lossy in the filesystem and lossless in the record.
fn sanitise(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() { "unnamed".into() } else { s }
}

/// The `sh` script that puts a process in its cgroup **before** it becomes the
/// command.
///
/// Read in order:
///
/// - `$1` is the scope's `cgroup.procs`; `$$` is this shell's own pid.
/// - the write happens **first**, so every descendant the command later forks is
///   a member by inheritance and there is no window to escape through;
/// - `$2` is a path the HOST owns, and it is touched **only on the branch that joined**;
/// - `exec` replaces this shell, so the argv a later `/proc/<pid>/cmdline` shows
///   is the command's own and not a wrapper's;
/// - **`|| exit 125` is the fail-closed half**: a process that could not be owned
///   is exactly the orphan T24 exists to stop, so it is not run at all.
///
/// Everything after the script is a separate argv element, so nothing here is a
/// quoting question: the command text never passes through a second parser.
///
/// # The evidence, three times over, and why the third was needed
///
/// **The code is not the evidence** (R21's sibling). 125 is a legitimate exit code, so
/// `Some(EXIT_NOT_SCOPED) => JobState::NotScoped` called `bash -c "exit 125"` a command that
/// never ran.
///
/// **The marker sentence is not the evidence either** (2026-09-23, leticl's measurement).
/// It is a string a command can *print*: `echo 'letibot: could not join…' >&2; exit 125`
/// reproduced the whole misclassification, because the check reads the command's own output.
/// *An exit code is the process's own answer, and neither it nor a sentence the process wrote
/// can be the evidence that there was no process.*
///
/// **`$2` is the evidence, and the host owns it.** The path is the daemon's, the file is
/// created on the branch that joined and on no other, and the command's output cannot reach
/// it — so `joined` is a fact the daemon observed rather than a sentence it read. It is a
/// file and not a second exit code for the same reason `NOT_SCOPED_MARKER` is a string and
/// not a number: one channel can only carry one meaning at a time.
pub fn join_script() -> &'static str {
    "echo $$ > \"$1\" || { echo 'letibot: could not join the scope cgroup; the command was NOT run' >&2; exit 125; }; : > \"$2\"; shift 2; exec \"$@\""
}

/// **What the wrapper says when it could not own the process**, verbatim.
///
/// The classification of a launcher failure rests on this string and not on the exit code,
/// for the reason in [`join_script`]'s own note, and it is a `const` so that the script and
/// the reader cannot drift apart: `the_script_and_the_marker_are_one_string` fails if either
/// side is edited alone.
pub const NOT_SCOPED_MARKER: &str =
    "letibot: could not join the scope cgroup; the command was NOT run";

/// The exit code [`join_script`] uses when a process could not join its scope.
///
/// **Not the evidence** — see [`NOT_SCOPED_MARKER`]. Kept because the wrapper's failure
/// should not look like the command's success, and 125 is what the reference launcher uses
/// too (`deepseek-harness`'s `entry/src/main.c`) for the same reason: a code the wrapped
/// command is unlikely to choose. *Unlikely* is not *never*, which is the whole defect.
pub const EXIT_NOT_SCOPED: i32 = 125;

#[cfg(test)]
mod tests {
    use super::*;

    /// **The script and the marker are one string** — R21's sibling.
    ///
    /// The classification of a launcher failure rests on the sentence the wrapper writes,
    /// and the exit code is no longer evidence at all (125 is a legitimate code:
    /// `bash -c "exit 125"` was listed as `not run (could not join its scope)`). This is the
    /// test that keeps the two ends of that string from drifting: edit the script and this
    /// fails, edit the const and this fails. **Falsified by editing either one.**
    #[test]
    fn the_script_and_the_marker_are_one_string() {
        assert!(
            join_script().contains(NOT_SCOPED_MARKER),
            "the wrapper no longer says what `launcher_failed` looks for:\n{}",
            join_script()
        );
        // And the marker is not something a *successful* run could produce by accident:
        // the script writes it only on the branch that also exits `EXIT_NOT_SCOPED`.
        assert!(join_script().contains(&format!("exit {EXIT_NOT_SCOPED}")));
        assert_eq!(
            join_script().matches(NOT_SCOPED_MARKER).count(),
            1,
            "the marker appears once, on the failure branch"
        );
        // **And the join token is written on that branch and no other.** `: > "$2"` sits
        // after the `||` group, so a wrapper that could not join never reaches it — which is
        // what makes its absence a fact the host can read. Asserted by position rather than
        // by count, because a second occurrence anywhere after would still count as one
        // string while making the token meaningless.
        let script = join_script();
        let token = script.find(": > \"$2\"").expect("the token write is gone");
        let failure = script.find("exit 125").expect("the failure branch is gone");
        assert!(
            token > failure,
            "the token is written BEFORE the failure branch exits, so a wrapper that could \
             not join would touch it anyway:\n{script}"
        );
        assert_eq!(
            script.matches(": > \"$2\"").count(),
            1,
            "one write, so `present` and `joined` cannot come apart"
        );
    }

    /// **A command that exits 125 is a command that ran.** The state machine is in
    /// `host.rs`; this pins the two constants apart so neither can be mistaken for the
    /// other's evidence.
    #[test]
    fn the_exit_code_is_not_the_evidence() {
        assert_eq!(EXIT_NOT_SCOPED, 125);
        assert!(
            !NOT_SCOPED_MARKER.contains("125"),
            "a marker that carried the number would put the code back in the evidence"
        );
    }

    #[test]
    fn the_three_scopes_round_trip_and_there_is_no_fourth() {
        for k in [ScopeKind::Turn, ScopeKind::Session, ScopeKind::Explicit] {
            assert_eq!(ScopeKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(ScopeKind::parse("forever"), None);
        // Only `explicit` promises to outlive the session, and it says so in the
        // words a listing shows.
        assert!(ScopeKind::Explicit.reaped_when().contains("SURVIVES"));
        assert!(!ScopeKind::Turn.reaped_when().contains("SURVIVES"));
    }

    #[test]
    fn no_scopes_refuses_and_names_what_is_missing() {
        let n = NoScopes::new("this host has no cgroup v2");
        let e = n.open(ScopeKind::Turn, "t1", None).unwrap_err();
        assert!(format!("{e}").contains("no cgroup v2"), "{e}");
        // And its reap is a record that admits it did nothing, rather than a
        // clean-looking zero.
        let scope = ScopeId {
            kind: ScopeKind::Turn,
            name: "t1".into(),
            path: PathBuf::from("/nowhere"),
        };
        let r = n.end(&scope);
        assert!(!r.clean());
        assert!(
            r.summary().contains("no scope mechanism"),
            "{}",
            r.summary()
        );
    }

    #[test]
    fn a_reaping_of_an_empty_scope_is_not_the_same_record_as_a_reaping_of_three() {
        // The fleet's point, as a type-level fact: the two zeroes are different
        // facts and the record shows which one happened.
        let scope = ScopeId {
            kind: ScopeKind::Turn,
            name: "t".into(),
            path: PathBuf::from("/tmp/none"),
        };
        let empty = Reaping {
            scope: scope.clone(),
            at: SystemTime::now(),
            mechanism: "nothing to kill",
            observed: vec![],
            survivors: vec![],
            waited: Duration::ZERO,
            removed: true,
            note: None,
        };
        let three = Reaping {
            observed: (1..=3)
                .map(|p| Reaped {
                    pid: p,
                    comm: "x".into(),
                    cmdline: "sleep 100".into(),
                })
                .collect(),
            ..empty.clone()
        };
        assert!(empty.summary().contains("observed 0"));
        assert!(three.summary().contains("observed 3"));
        assert!(three.summary().contains("sleep"));
    }

    #[test]
    fn a_cmdline_is_nul_separated() {
        assert_eq!(cmdline_of(b"sleep\x00100\x00"), "sleep 100");
    }

    #[test]
    fn the_join_script_refuses_rather_than_orphaning() {
        // The fail-closed half, asserted as text because it is the half that is
        // easy to delete during a refactor and impossible to notice missing.
        assert!(join_script().contains("exit 125"));
        assert!(join_script().contains("was NOT run"));
        assert!(join_script().contains("exec"));
    }

    /// The macOS tree, end to end through the real wrapper: a job that backgrounds a
    /// grandchild and returns is still reaped whole, the record carries both, and a
    /// promotion moves them so the old scope's end no longer touches them.
    #[cfg(target_os = "macos")]
    #[test]
    fn process_groups_reap_a_backgrounded_grandchild_and_promotion_moves_it() {
        use std::os::unix::process::CommandExt;
        let t = ProcessGroups::probe().unwrap();
        let session = t.open(ScopeKind::Session, "s", None).unwrap();
        let turn = t.open(ScopeKind::Turn, "t", Some(&session)).unwrap();
        let job = t.open(ScopeKind::Turn, "job.1", Some(&turn)).unwrap();

        let mut wrapper = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(join_script())
            .arg("letibot-scope")
            .arg(Cgroup2::procs_path(&job))
            .arg("/dev/null")
            .arg("/bin/sh")
            .arg("-c")
            .arg("sleep 30 & sleep 30")
            .process_group(0)
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while t.members(&turn).unwrap().len() < 3 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(t.members(&turn).unwrap().len(), 3, "sh and two sleeps");
        assert!(populated_at(&session.path));

        // Promote the job to the session: the turn's end must now find nothing.
        let m = t.migrate(&job, &session).unwrap();
        assert!(m.complete(), "{}", m.summary());
        assert_eq!(m.moved.len(), 3, "{}", m.summary());
        let turn_end = t.end(&turn);
        assert!(turn_end.observed.is_empty(), "{}", turn_end.summary());
        assert_eq!(t.members(&session).unwrap().len(), 3);

        let r = t.end(&session);
        let _ = wrapper.wait();
        assert_eq!(r.observed.len(), 3, "{}", r.summary());
        assert_eq!(r.mechanism, PGROUP_KILL);
        assert!(r.clean(), "{}", r.summary());
        assert!(
            r.observed.iter().any(|o| o.label().contains("sleep 30")),
            "{}",
            r.summary()
        );
        t.prune();
    }

    /// A root left by a process that is gone, holding nothing, is swept by the next probe;
    /// one whose owner is alive is not touched.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_dead_daemons_empty_root_is_swept_and_a_live_ones_is_kept() {
        let parent = std::env::temp_dir().join(format!("letibot-sweep-{}", std::process::id()));
        // pid 0 is never a user process; this process is certainly alive.
        let dead = parent.join("letibot.0.0/session.s");
        let live = parent.join(format!("letibot.{}.99/session.s", std::process::id()));
        std::fs::create_dir_all(&dead).unwrap();
        std::fs::create_dir_all(&live).unwrap();
        let t = ProcessGroups::probe_under(&parent).unwrap();
        assert!(!parent.join("letibot.0.0").exists(), "the dead root stayed");
        assert!(live.exists(), "a live owner's root was removed");
        drop(t);
        let _ = std::fs::remove_dir_all(&parent);
    }
}
