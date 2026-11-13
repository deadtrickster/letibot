//! Owners, holdings and the read-only plan (`docs/lifecycle.md`, package G1).
//!
//! Ownership is proved by a stamp on the resource itself - a cgroup directory, a run file under
//! firecode's `runs/`, an `owner.json` in the owner root - and the store's rows are the index and
//! the journal over those stamps, never the proof. So every fact here is read from the filesystem
//! and the kernel: a discovery pass re-derives what an owner holds with no daemon, no session and
//! no `ps`. That last part is why a held resource's size and liveness come from its cgroup: a seat
//! in a sandbox cannot see host pids at all.
//!
//! Nothing here reaps. `plan` judges, and G2, G3 and G4 are the hands that act on the verdict.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use letibot_tokencore::store::{
    HoldingClass, HoldingRow, HoldingState, OwnerKind, OwnerRow, Result as StoreResult, Store,
};

use crate::exec::scope::{self, ScopeUse};

mod archive;
mod config;
mod plan;
mod reap;
mod remote;
pub mod systemd;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_disks;
#[cfg(test)]
mod tests_docker;
#[cfg(test)]
mod tests_work;
pub mod work;
mod worktree;
pub use archive::{Kept, fetch_bundle, keep, namespace};
pub use config::{Settings, SharedTarget, bytes as size_bytes, config_file};
pub use plan::{
    HoldingView, LANDED_GRACE_MS, OwnerView, Plan, Stranger, UNLANDED_GRACE_MS, Verdict, judge,
    plan, stamp_of,
};
pub use reap::{
    Act, Activity, BoxHands, COPY_OUT_DEADLINE_MS, Ended, Hands, NOTICE_LEAD_MS, Ops, Pass,
    close_due, notice_at, pass,
};
pub use remote::{
    Bundle, HostPass, LeaseKeeper, LeaseRenewer, RENEW_MS, RemoteLeases, RemoteOwner,
    collect_bundle, host_pass, renew_script,
};
pub use systemd::{NoManager, Systemd, UnitManager, UnitTable};
pub use worktree::{
    Archived, OWNER_FILE, Place, Restored, STAMP_KEY, WIP_MAX_BYTES, archive_and_remove, archives,
    branch_name, drop_archive, make as make_worktree, package_dir, proved as worktree_proved,
    refs_under, restore as restore_worktree, restore_path, stamp as stamp_worktree, stamped_owner,
    under as worktrees_under,
};
/// The file an owner root carries: the owner id, this box, the kind and the lease. It is what a
/// reaper with no store reads, and the one thing that tells letibot's root from a directory that
/// merely sits beside it.
pub const OWNER_JSON: &str = "owner.json";

/// What an owner root holds, and the class each directory is (`docs/lifecycle.md` d.4). A directory
/// inside the root is the owner's because of where it is; the file names the owner.
pub const ROOT_DIRS: [(&str, &str, HoldingClass); 5] = [
    ("wt", "worktree", HoldingClass::Work),
    ("target", "target", HoldingClass::Cache),
    ("tmp", "tmp", HoldingClass::Cache),
    ("home", "home", HoldingClass::Cache),
    ("out", "copy-out", HoldingClass::Work),
];

/// A kind word for a holding, as the rows carry it.
pub const SCOPE: &str = "scope";
pub const VM: &str = "vm";
pub const DAEMON: &str = "daemon";
/// One owned worktree under an owner root's `wt/`, and the branch it is on (d.4, d.5).
pub const WORKTREE: &str = "worktree";
/// The refs an owner's work was kept in: `refs/letibot/archive/<owner>/…`. A record, so nothing
/// removes one on its own - `letibot gc --drop-archive` is the only door (d.8).
pub const ARCHIVE: &str = "archive";
/// The copy a VM was booted on, under letibot's firecode cache.
pub const COPY: &str = "copy";
/// A firecode state layer: the filesystem a project's guest writes, under `FIRECODE_STATE`.
pub const STATE: &str = "state";
/// What `firecode down` delivered beside it - the only copy of that work until it is fetched.
pub const COPY_OUT: &str = "copy-out";
/// A remote home or build directory, under that host's `.letibot-home`.
pub const REMOTE_HOME: &str = "home";

/// An owner root's own record, as `owner.json` holds it. Deliberately small: a reaper that cannot
/// open the store must still be able to read an owner's id, its box and when its lease ran out.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Stamp {
    pub owner: String,
    /// The home box this owner belongs to, so a root on another machine is not read as a local one.
    pub box_id: String,
    /// `session`, `package`, `subagent`, `gate`, `review`.
    pub kind: String,
    #[serde(default)]
    pub repo: Option<String>,
    pub created_ms: i64,
    /// When the owner last did anything, as this box's clock had it.
    pub lease_ms: i64,
}

impl Stamp {
    pub fn of(owner: &OwnerRow, box_id: &str) -> Stamp {
        Stamp {
            owner: owner.id.clone(),
            box_id: box_id.to_string(),
            kind: owner.kind.word().to_string(),
            repo: owner.repo.clone(),
            created_ms: owner.created_ms,
            lease_ms: owner.lease_ms,
        }
    }
}

/// Everything a discovery pass reads, so a test can point it at a fixture instead of the box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Places {
    /// Where owner roots live: `<gc_root>/<box>/<owner>/…`.
    pub gc_root: PathBuf,
    pub box_id: String,
    /// The `[gc]` and `[firecode]` tables this box was configured with: the shared targets, and
    /// where firecode keeps its state and its runs.
    pub settings: Settings,
    /// The directory this box's store is in - what a remote or a journal is found beside.
    pub data_dir: PathBuf,
    /// `$XDG_RUNTIME_DIR/letibot`: a folder's daemon record is `<key>.json` in it.
    pub rundir: PathBuf,
    /// firecode's `runs/`, when this box has a firecode at all.
    pub firecode_runs: Option<PathBuf>,
    /// `~/.cache/letibot/firecode`: the booted copies and the copy-outs.
    pub firecode_cache: Option<PathBuf>,
    /// `/proc`, or a fixture's.
    pub proc_root: PathBuf,
    /// `/sys/fs/cgroup`, or a fixture's.
    pub cgroup_fs: PathBuf,
    /// The docker engine this box's `[docker] engine` or `$DOCKER_HOST` names, when either does and
    /// the socket is there. `None` is a box without docker, which holds nothing of docker's.
    pub docker_engine: Option<PathBuf>,
}

impl Places {
    /// This box's own places, as every caller computes them. The owner roots are `$LETIBOT_GC_ROOT`
    /// first - a test's and a debug build's door - then `[gc] root` from the box's
    /// `leticode.toml`, then the cache directory: never the store, which a command cannot write.
    pub fn here() -> Places {
        let data_dir = data_dir();
        let settings = Settings::load_or_default();
        let gc_root = std::env::var_os("LETIBOT_GC_ROOT")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| settings.root.clone())
            .unwrap_or_else(|| cache_dir().join("gc"));
        // `[firecode] runs` first: a box that put firecode's state on a big disk put its runs
        // there too, and asking the binary where its checkout keeps them would answer the other
        // disk.
        let runs = settings.firecode_runs.clone().or_else(|| {
            crate::firecode::firecode_available()
                .ok()
                .and_then(|bin| crate::firecode::held::runs_dir(&bin))
        });
        let cache = crate::firecode::FirecodeBackend::project_of("x")
            .parent()
            .map(Path::to_path_buf);
        Places {
            gc_root,
            box_id: box_id(),
            settings,
            data_dir,
            rundir: rundir(),
            firecode_runs: runs,
            firecode_cache: cache,
            proc_root: PathBuf::from("/proc"),
            cgroup_fs: PathBuf::from("/sys/fs/cgroup"),
            docker_engine: crate::docker::engine_socket(),
        }
    }

    /// The root of one owner's things: `<gc_root>/<box>/<owner>/`.
    pub fn owner_root(&self, owner: &str) -> PathBuf {
        self.gc_root.join(&self.box_id).join(owner)
    }

    /// The box directory every owner root sits under.
    pub fn box_dir(&self) -> PathBuf {
        self.gc_root.join(&self.box_id)
    }

    /// The two directories a build writes in, made if they are not there: this is what puts a
    /// package's cargo target and its `/tmp` inside its owner root instead of on the root disk.
    pub fn build_dirs(&self, owner: &str) -> (PathBuf, PathBuf) {
        let target = self.owner_root(owner).join("target");
        let tmp = self.owner_root(owner).join("tmp");
        let _ = std::fs::create_dir_all(&target);
        let _ = std::fs::create_dir_all(&tmp);
        (target, tmp)
    }

    /// Whether an owner root may be made at all. A root under a `LETIBOT_GC_ROOT` a test named is
    /// made on demand; the default one only when its parent exists, so a box that never set the
    /// key keeps its roots where the data directory already is.
    pub fn owned_root_ready(&self) -> bool {
        self.gc_root.is_dir() || std::fs::create_dir_all(&self.gc_root).is_ok()
    }

    /// The stamp on one owner root, or `None` for a directory that is not letibot's - a missing
    /// file and an unreadable one are the same answer, and both are *look, do not act*.
    pub fn stamp_at(&self, root: &Path) -> Option<Stamp> {
        let text = std::fs::read_to_string(root.join(OWNER_JSON)).ok()?;
        serde_json::from_str(&text).ok()
    }
}

/// The pairs a build inside an owner root needs, so its target and its temp files land there and
/// not on the root disk (`docs/lifecycle.md` d.4). Only when the key is set: a box that never asked
/// for a `gc.root` keeps cargo's own default, because moving a build nobody was warned about is a
/// slower surprise than the disk filling.
pub fn owner_env(places: &Places, owner: &str) -> Vec<(String, String)> {
    if places.settings.root.is_none() && std::env::var_os("LETIBOT_GC_ROOT").is_none() {
        return Vec::new();
    }
    let (target, tmp) = places.build_dirs(owner);
    vec![
        ("CARGO_TARGET_DIR".to_string(), target.display().to_string()),
        ("TMPDIR".to_string(), tmp.display().to_string()),
        ("LETIBOT_OWNER".to_string(), owner.to_string()),
    ]
}

/// A session's two temporary directories, both under its owner root ON DISK: `spill`, where tools
/// leave artifacts too big for the transcript, and `tmp`, the `/tmp` a confined command is given.
///
/// Never the runtime directory. `$XDG_RUNTIME_DIR` is RAM, and on 2026-11-13 a Go fuzz job's 54 323
/// worker temp dirs under `<runtime>/scratch-<pid>/tmp/<session>` filled this box's 19 GiB tmpfs and
/// broke every other session's gate. The runtime dir keeps sockets and records; a session's temp
/// files are a cache of its owner's, on the disk the owner root was put on.
pub fn session_dirs(places: &Places, owner: &str) -> std::io::Result<(PathBuf, PathBuf)> {
    use std::os::unix::fs::DirBuilderExt;
    let tmp = places.owner_root(owner).join("tmp");
    let spill = tmp.join("spill");
    let session = tmp.join("session");
    for d in [&spill, &session] {
        // 0700, because what a command writes to its `/tmp` is not the next session's business.
        std::fs::DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(d)?;
    }
    Ok((spill, session))
}

/// `$XDG_CACHE_HOME/letibot`, else `~/.cache/letibot`: what can be rebuilt, temp files and build
/// targets among it, outside the store a command's view hides.
pub fn cache_dir() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .map(|d| d.join("letibot"))
        .unwrap_or_else(|| PathBuf::from(".letibot-cache"))
}

/// `$XDG_DATA_HOME/letibot`, else `~/.local/share/letibot`: the directory the store lives in.
pub fn data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share")))
        .map(|d| d.join("letibot"))
        .unwrap_or_else(|| PathBuf::from(".letibot-data"))
}

/// `$XDG_RUNTIME_DIR/letibot`, else `/run/user/<uid>/letibot`, else the temp directory - the same
/// rule the launcher and the bridge resolve, so a plan run by hand finds the daemon it is about.
pub fn rundir() -> PathBuf {
    let base = match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => {
            let uid = unsafe { libc::getuid() };
            let run = PathBuf::from(format!("/run/user/{uid}"));
            if run.is_dir() {
                run
            } else {
                std::env::temp_dir()
            }
        }
    };
    base.join("letibot")
}

/// This box's name: what a stamp says the owner belongs to, so a directory carried to another
/// machine is not read as that machine's.
pub fn box_id() -> String {
    for path in ["/proc/sys/kernel/hostname", "/etc/hostname"] {
        if let Ok(name) = std::fs::read_to_string(path) {
            let name = name.trim();
            if !name.is_empty() {
                return name.to_string();
            }
        }
    }
    std::env::var("HOSTNAME")
        .ok()
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "this-box".into())
}

/// One resource a discovery pass found on the filesystem, with the stamp that proves whose it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub owner: String,
    pub kind: String,
    pub host: Option<String>,
    pub handle: String,
    pub class: HoldingClass,
    pub stamp: String,
    pub bytes: Option<u64>,
    /// What the resource itself says about itself, when it says anything - a live VM's pid, a
    /// dead one's absence.
    pub why: Option<String>,
    /// The cgroup facts, when the resource is a cgroup: the answer no `ps` in another PID
    /// namespace can give.
    pub use_: Option<ScopeUse>,
}

impl Found {
    pub fn row(&self, now_ms: i64) -> HoldingRow {
        HoldingRow {
            owner_id: self.owner.clone(),
            kind: self.kind.clone(),
            host: self.host.clone(),
            handle: self.handle.clone(),
            class: self.class,
            state: HoldingState::Held,
            stamp: self.stamp.clone(),
            bytes: self.bytes,
            archive: None,
            why: self.why.clone(),
            created_ms: now_ms,
            checked_ms: Some(now_ms),
            attempts: 0,
        }
    }
}

// ---------------------------------------------------------------- what an owner creates

/// How long a package or a subagent may sit with no turn, no job and no input before the reaper
/// closes it (`docs/lifecycle.md` d.3). A session an operator holds is never closed for idleness.
pub const PACKAGE_IDLE_TTL_MS: i64 = 30 * 60 * 1000;
/// A gate run's own: its work is done the moment its landing ends, so it is closed sooner.
pub const GATE_IDLE_TTL_MS: i64 = 10 * 60 * 1000;
/// The hard bound on a package, whatever it is doing - the backstop a missing call bound needs.
pub const PACKAGE_HARD_TTL_MS: i64 = 24 * 60 * 60 * 1000;
/// And a gate run's.
pub const GATE_HARD_TTL_MS: i64 = 6 * 60 * 60 * 1000;

/// The owner row an open writes: a new one, or the row this id already has with the operator's
/// hold, the landing and the creation kept and its state `live` again - which is what a resume of
/// a closed owner is (`docs/lifecycle.md` d.3). The TTLs come from the kind, so a session that
/// becomes a package cannot keep a session's never.
pub fn owner_for_open(
    store: &Store,
    id: &str,
    kind: OwnerKind,
    repo: Option<String>,
    now_ms: i64,
) -> StoreResult<OwnerRow> {
    let existing = store.owner(id)?;
    let (idle_ttl_ms, hard_ttl_ms) = match kind {
        OwnerKind::Package | OwnerKind::Subagent | OwnerKind::Review => {
            (Some(PACKAGE_IDLE_TTL_MS), Some(PACKAGE_HARD_TTL_MS))
        }
        OwnerKind::Gate => (Some(GATE_IDLE_TTL_MS), Some(GATE_HARD_TTL_MS)),
        OwnerKind::Session => (None, None),
    };
    let mut row = OwnerRow::live(id, kind, repo, now_ms);
    row.idle_ttl_ms = idle_ttl_ms;
    row.hard_ttl_ms = hard_ttl_ms;
    if let Some(was) = existing {
        row.created_ms = was.created_ms;
        row.held_until_ms = was.held_until_ms;
        row.landed_sha = was.landed_sha;
        row.parent_id = was.parent_id;
        // The close is a fact, and a resume does not rewrite it: a row that lost `closed_ms` on
        // its way back to `live` would make `letibot gc --log` answer nothing about why it was
        // shut, and the plan would count a first close that never happened.
        row.closed_ms = was.closed_ms;
        row.closed_why = was.closed_why;
    }
    Ok(row)
}

/// The queue proved an owner's work is on its target: write the landing down, so the plan's landed
/// grace (1h) applies instead of the unlanded one (24h). Only a row that already exists is touched -
/// a queue entry's owner is a session id, and a landing must never invent an owner for it.
pub fn set_landed(store: &Store, owner: &str, sha: &str) -> StoreResult<bool> {
    let Some(mut row) = store.owner(owner)? else {
        return Ok(false);
    };
    row.landed_sha = Some(sha.to_string());
    store.put_owner(&row)?;
    Ok(true)
}

/// Write an owner's row and its root, then the holdings the filesystem already proves. The row
/// goes first and the root second: a row with no resource is harmless, a resource with no row is
/// the leak this whole package exists to stop.
pub fn register(store: &Store, places: &Places, owner: &OwnerRow) -> StoreResult<()> {
    store.put_owner(owner)?;
    let root = places.owner_root(&owner.id);
    std::fs::create_dir_all(&root).map_err(|e| {
        letibot_tokencore::store::StoreError::Refused(format!(
            "could not make the owner root {} ({e}): its directories are the stamp every pass \
             reads",
            root.display()
        ))
    })?;
    let stamp = Stamp::of(owner, &places.box_id);
    let text = serde_json::to_string_pretty(&stamp)?;
    std::fs::write(root.join(OWNER_JSON), text).map_err(|e| {
        letibot_tokencore::store::StoreError::Refused(format!(
            "could not write {}: {e}",
            root.join(OWNER_JSON).display()
        ))
    })?;
    // And what the filesystem already proves, written down beside it: `refresh` is the same walk a
    // plan does, so the index starts out true instead of empty.
    refresh(store, owner, places)?;
    Ok(())
}

/// One discovery pass for one owner, written down: every holding the filesystem proves is upserted
/// (so a row deleted by hand comes back), and every row the walk no longer finds is `gone`. This is
/// the pass a daemon runs; `letibot gc` runs [`discover`] and writes nothing.
pub fn refresh(store: &Store, owner: &OwnerRow, places: &Places) -> StoreResult<Vec<HoldingRow>> {
    let now = now_ms();
    let found = discover_all(store, owner, places);
    let mut written = Vec::with_capacity(found.len());
    for f in &found {
        let row = f.row(now);
        store.put_holding(&row)?;
        written.push(row);
    }
    for row in store.holdings_of(&owner.id)? {
        let still = found
            .iter()
            .any(|f| f.kind == row.kind && f.handle == row.handle && f.host == row.host);
        if !still && row.state.present() {
            store.put_holding(&HoldingRow {
                state: HoldingState::Gone,
                why: Some("the pass found no such resource on this box".into()),
                checked_ms: Some(now),
                ..row
            })?;
        }
    }
    Ok(written)
}

// ---------------------------------------------------------------- the discovery walk

/// Every resource this box can still prove is this owner's, re-derived from the filesystem and the
/// kernel alone - the owner row says what to look for, never which holdings exist.
pub fn discover(owner: &OwnerRow, places: &Places) -> Vec<Found> {
    let mut out = Vec::new();
    out.extend(from_root(owner, places));
    out.extend(from_daemon(owner, places));
    out.extend(scopes_of(owner, places));
    out.extend(from_firecode(owner, places));
    out.extend(from_docker(owner, places));
    out
}

/// The engine's own answer, through the label the proxy put on: `docs/lifecycle.md` d.4's docker
/// stamp. A container or a network is `compute` and goes at close; a volume is `work` and waits for
/// the work grace. An engine that did not answer contributes nothing - not being able to tell is a
/// reason to keep, never to reap (d.4 rule 8).
fn from_docker(owner: &OwnerRow, places: &Places) -> Vec<Found> {
    let Some(engine) = &places.docker_engine else {
        return Vec::new();
    };
    let engine = crate::docker::Socket::at(engine);
    match crate::docker::discovered(&engine, &owner.id) {
        Ok(found) => found
            .into_iter()
            .map(|d| Found {
                owner: owner.id.clone(),
                kind: d.kind.to_string(),
                host: None,
                handle: d.handle,
                class: d.class,
                stamp: format!("the label `{}={}`", crate::docker::LABEL_OWNER, owner.id),
                bytes: d.bytes,
                why: Some(d.why),
                use_: None,
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// [`discover`], plus what a row already in the store still names.
///
/// Two witnesses matter here. The scope walk starts from the DAEMON's `/proc` entry, so a daemon
/// that died takes the evidence with it while the cgroup it delegated can still hold every process
/// the owner started - the case R201 was filed on. And a WORK holding may sit outside the owner root
/// altogether: an adopted worktree (`letibot gc --adopt`), which the root walk cannot see, is found
/// by its row - checked against the disk and the repository before it is believed, and re-proved by
/// the settle step itself.
///
/// A `stuck` holding is carried, so the plan keeps saying it and nothing re-tries it in the dark.
pub fn discover_all(store: &Store, owner: &OwnerRow, places: &Places) -> Vec<Found> {
    let mut out = discover(owner, places);
    for row in store.holdings_of(&owner.id).unwrap_or_default() {
        if row.state == HoldingState::Gone {
            continue;
        }
        if out
            .iter()
            .any(|f| f.kind == row.kind && f.handle == row.handle)
        {
            continue;
        }
        if row.kind == WORKTREE || row.kind == work::BRANCH {
            // Only while the thing is still there: a row whose tree or branch is gone is the
            // refresh walk's `gone`, not a resource a second step could act on.
            let there = if row.kind == WORKTREE {
                Path::new(&row.handle).exists()
            } else {
                owner
                    .repo
                    .as_deref()
                    .is_some_and(|r| crate::git::branch_head(Path::new(r), &row.handle).is_some())
            };
            if there {
                out.push(Found {
                    owner: owner.id.clone(),
                    kind: row.kind.clone(),
                    host: row.host.clone(),
                    handle: row.handle.clone(),
                    class: HoldingClass::Work,
                    stamp: row.stamp.clone(),
                    bytes: row.bytes,
                    why: row.why.clone(),
                    use_: None,
                });
            }
            continue;
        }
        if row.kind != SCOPE {
            continue;
        }
        let path = PathBuf::from(&row.handle);
        let Some(use_) = scope::use_at(&path) else {
            continue;
        };
        out.push(Found {
            owner: owner.id.clone(),
            kind: SCOPE.to_string(),
            host: row.host.clone(),
            handle: row.handle.clone(),
            class: HoldingClass::Compute,
            stamp: format!("the kernel's own directory, {}", path.display()),
            bytes: use_.memory_current,
            why: Some(scope_clause(&use_)),
            use_: Some(use_),
        });
    }
    out
}

/// One line saying what a cgroup's facts are, for a row's `why` or a listing.
pub fn scope_clause(u: &ScopeUse) -> String {
    u.clause()
}

/// `6.1 GiB`, `38.0 GiB`, `512 KiB`: the one spelling every plan line uses.
pub fn human_bytes(n: u64) -> String {
    scope::human_bytes(n)
}

/// `12m`, `1h02m`: a span as a person reads it.
pub fn human_millis(ms: u64) -> String {
    scope::human_millis(ms)
}

/// The owner root's own directories: each one a holding because of where it is, and the file beside
/// them naming whose it is.
fn from_root(owner: &OwnerRow, places: &Places) -> Vec<Found> {
    let root = places.owner_root(&owner.id);
    let Some(stamp) = places.stamp_at(&root) else {
        return Vec::new();
    };
    if stamp.owner != owner.id || stamp.box_id != places.box_id {
        // A root that names another owner or another box is not this owner's, whatever its path
        // says: the file disagrees with the directory, and the file is the proof.
        return Vec::new();
    }
    let mut out = Vec::new();
    for (dir, kind, class) in ROOT_DIRS {
        let path = root.join(dir);
        if !path.is_dir() {
            continue;
        }
        // `wt/` is one holding per worktree, not one for the directory: a worktree is archived and
        // removed on its own, and a directory's size is not a thing a settle step can act on.
        if dir == "wt" {
            let paths = worktree::under(&root);
            if paths.is_empty() {
                let (bytes, complete) = size_of(&path);
                out.push(Found {
                    owner: owner.id.clone(),
                    kind: kind.to_string(),
                    host: None,
                    handle: path.display().to_string(),
                    class,
                    stamp: format!("{} names this owner", root.join(OWNER_JSON).display()),
                    bytes,
                    why: (!complete)
                        .then(|| "its size was not measured whole (too big to walk)".into()),
                    use_: None,
                });
                continue;
            }
            for wt in paths {
                out.extend(worktree_found(owner, &root, &wt));
            }
            continue;
        }
        let (bytes, complete) = size_of(&path);
        out.push(Found {
            owner: owner.id.clone(),
            kind: kind.to_string(),
            host: None,
            handle: path.display().to_string(),
            class,
            stamp: format!("{} names this owner", root.join(OWNER_JSON).display()),
            bytes,
            why: (!complete).then(|| "its size was not measured whole (too big to walk)".into()),
            use_: None,
        });
    }
    if let Some(repo) = owner.repo.as_deref()
        && let Ok(refs) =
            worktree::refs_under(std::path::Path::new(repo), &namespace(&owner.id, "wt"))
        && !refs.is_empty()
    {
        out.push(Found {
            owner: owner.id.clone(),
            kind: ARCHIVE.to_string(),
            host: None,
            handle: namespace(&owner.id, "wt"),
            class: HoldingClass::Record,
            stamp: format!("{} ref(s) under refs/letibot/archive", refs.len()),
            bytes: None,
            why: Some(format!(
                "{} ref(s); `letibot gc --restore {}` puts the work back",
                refs.len(),
                owner.id
            )),
            use_: None,
        });
    }
    out
}

/// One worktree of an owner root, and the branch it is on, each as its own holding: the worktree is
/// the tree, the branch is the ref, and a settle step ends both.
fn worktree_found(owner: &OwnerRow, root: &std::path::Path, wt: &std::path::Path) -> Vec<Found> {
    let mut out = Vec::new();
    let repo = owner.repo.as_deref().map(std::path::PathBuf::from);
    let branch = worktree::branch_of(wt);
    let proved = repo
        .as_deref()
        .and_then(|r| worktree::proved(r, wt, branch.as_deref(), &owner.id));
    let stamp = match (&proved, &repo) {
        (Some(why), _) => why.clone(),
        (None, Some(repo)) if worktree::stamped_owner(wt).is_some() => format!(
            "{} names an owner that is not this one, and branch.{} names nobody",
            wt.display(),
            branch.clone().unwrap_or_default()
        ),
        (None, _) => format!(
            "no stamp file in its git directory and no owner key in {}",
            repo.as_deref()
                .map(|r| r.display().to_string())
                .unwrap_or_default()
        ),
    };
    let (bytes, complete) = size_of(wt);
    out.push(Found {
        owner: owner.id.clone(),
        kind: WORKTREE.to_string(),
        host: None,
        handle: wt.display().to_string(),
        class: HoldingClass::Work,
        stamp: stamp.clone(),
        bytes,
        why: (!complete).then(|| "its size was not measured whole (too big to walk)".into()),
        use_: None,
    });
    if let Some(branch) = branch {
        out.push(Found {
            owner: owner.id.clone(),
            kind: work::BRANCH.to_string(),
            host: None,
            handle: branch,
            class: HoldingClass::Work,
            stamp,
            bytes: None,
            why: Some(format!(
                "the branch this worktree is on, under {}",
                root.display()
            )),
            use_: None,
        });
    }
    out
}

/// One daemon's runtime record: what the launcher wrote under `$RUNDIR` when it started it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonRec {
    pub path: PathBuf,
    /// The pid the launcher started. Not trusted alone (d.4): the record is the proof of whose it
    /// is, and the pid read now with its start time is the identity a signal is sent to.
    pub pid: Option<u32>,
    pub socket: String,
}

/// Every runtime record that names a workspace, in the runtime directory. A room or a forward is
/// a record of its own kind and names a directory nobody works in, so it is not here.
pub fn daemon_records(places: &Places) -> Vec<(PathBuf, String, Option<u32>, String)> {
    let Ok(entries) = std::fs::read_dir(&places.rundir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        if v.get("room").is_some() || v.get("forward").is_some() {
            continue;
        }
        let Some(workspace) = v.get("workspace").and_then(|w| w.as_str()) else {
            continue;
        };
        let pid = v
            .get("pid")
            .and_then(|p| p.as_u64())
            .and_then(|p| u32::try_from(p).ok())
            .filter(|p| *p > 0);
        let socket = v
            .get("socket")
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .to_string();
        out.push((path, workspace.to_string(), pid, socket));
    }
    out
}

/// The records of the daemons serving this owner's repository.
pub fn daemons_of(owner: &OwnerRow, places: &Places) -> Vec<DaemonRec> {
    let Some(repo) = owner.repo.as_deref() else {
        return Vec::new();
    };
    let wanted = std::fs::canonicalize(repo).unwrap_or_else(|_| PathBuf::from(repo));
    daemon_records(places)
        .into_iter()
        .filter(|(_, workspace, _, _)| {
            std::fs::canonicalize(workspace).unwrap_or_else(|_| PathBuf::from(workspace)) == wanted
        })
        .map(|(path, _, pid, socket)| DaemonRec { path, pid, socket })
        .collect()
}

/// The daemon that serves this owner: its record under the rundir, and the process the record
/// names. A record whose workspace is not this owner's is another folder's daemon.
fn from_daemon(owner: &OwnerRow, places: &Places) -> Vec<Found> {
    let Some(repo) = owner.repo.as_deref() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for rec in daemons_of(owner, places) {
        let alive = rec
            .pid
            .is_some_and(|p| crate::firecode::held::alive(&places.proc_root, p));
        let handle = match rec.pid {
            Some(p) => format!("pid:{p}"),
            None => rec.path.display().to_string(),
        };
        out.push(Found {
            owner: owner.id.clone(),
            kind: DAEMON.to_string(),
            host: None,
            handle,
            class: HoldingClass::Compute,
            stamp: format!(
                "{} and {}",
                rec.path.display(),
                places.proc_root.join("pid").display()
            ),
            bytes: None,
            why: Some(if alive {
                format!("serving {} on {}", repo, rec.socket)
            } else {
                format!(
                    "the record is here and no process is at {}: its daemon is gone",
                    rec.pid.map(|p| p.to_string()).unwrap_or_default()
                )
            }),
            use_: None,
        });
    }
    out
}

/// The cgroup subtrees of this owner's daemon: `session.<id>` under a `letibot.<pid>.<n>` root,
/// found from the daemon's own `/proc/<pid>/cgroup`. A scope is the only place a process's
/// lifetime and its memory can be read without a pid list, which is the whole reason a plan uses it.
fn scopes_of(owner: &OwnerRow, places: &Places) -> Vec<Found> {
    let mut out = Vec::new();
    for base in cgroup_bases(owner, places) {
        let Ok(entries) = std::fs::read_dir(&base) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with("letibot.") {
                continue;
            }
            let scope = entry.path().join(format!("session.{}", owner.id));
            let Some(use_) = scope::use_at(&scope) else {
                continue;
            };
            out.push(Found {
                owner: owner.id.clone(),
                kind: SCOPE.to_string(),
                host: None,
                handle: scope.display().to_string(),
                class: HoldingClass::Compute,
                stamp: format!("the kernel's own directory, {}", scope.display()),
                bytes: use_.memory_current,
                why: Some(scope_clause(&use_)),
                use_: Some(use_),
            });
        }
    }
    out
}

/// Where this owner's daemon may have put its cgroup trees: the daemon's own cgroup, and the leaf
/// it steps into to get a memory delegation (`exec::scope::Cgroup2::probe`).
fn cgroup_bases(owner: &OwnerRow, places: &Places) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for pid in daemon_pids(owner, places) {
        let Ok(text) =
            std::fs::read_to_string(places.proc_root.join(pid.to_string()).join("cgroup"))
        else {
            continue;
        };
        let Some(rel) = text.lines().find_map(|l| l.strip_prefix("0::")) else {
            continue;
        };
        let base = places.cgroup_fs.join(rel.trim().trim_start_matches('/'));
        out.push(base.clone());
        out.push(base.join(crate::exec::scope::DAEMON_LEAF));
    }
    out.sort();
    out.dedup();
    out
}

/// The pids this owner's daemons are recorded at, from the rundir records.
fn daemon_pids(owner: &OwnerRow, places: &Places) -> Vec<u32> {
    daemons_of(owner, places)
        .into_iter()
        .filter_map(|r| r.pid)
        .collect()
}

/// firecode's own records: the live run (its `owner` file, written at `up`), the copy it was booted
/// on, and the copy-out beside it - the last being the only copy of that work until it is fetched.
fn from_firecode(owner: &OwnerRow, places: &Places) -> Vec<Found> {
    let mut out = Vec::new();
    let live = places
        .firecode_runs
        .as_deref()
        .map(|runs| crate::firecode::held::live_vms(runs, &places.proc_root))
        .unwrap_or_default();
    if let Some(runs) = &places.firecode_runs {
        let owned = |run: &str| -> Option<String> {
            std::fs::read_to_string(runs.join(run).join("owner"))
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        };
        // The live VMs first: their fires are the ones the plan reports a pid for.
        for vm in &live {
            if owned(&vm.run).as_deref() != Some(owner.id.as_str()) {
                continue;
            }
            out.push(Found {
                owner: owner.id.clone(),
                kind: VM.to_string(),
                host: None,
                handle: vm.run.clone(),
                class: HoldingClass::Compute,
                stamp: format!(
                    "{} names its owner",
                    runs.join(&vm.run).join("owner").display()
                ),
                // The VM's memory is the firecracker process's, in the run's own cgroup: the same
                // two numbers a session's cgroup gives, so `holds --top` can add them to the owner
                // that boots it (G5, d.7).
                bytes: vm.cgroup.as_deref().and_then(|c| {
                    std::fs::read_to_string(c.join("memory.current"))
                        .ok()
                        .and_then(|v| v.trim().parse().ok())
                }),
                why: Some(vm.line()),
                use_: vm.cgroup.as_deref().and_then(scope::use_at),
            });
        }
        // And a run whose owner file names it but whose firecracker is gone: a record, not a VM.
        if let Ok(entries) = std::fs::read_dir(runs) {
            for entry in entries.flatten() {
                let run = entry.file_name().to_string_lossy().into_owned();
                if owned(&run).as_deref() != Some(owner.id.as_str())
                    || live.iter().any(|v| v.run == run)
                {
                    continue;
                }
                out.push(Found {
                    owner: owner.id.clone(),
                    kind: VM.to_string(),
                    host: None,
                    handle: run.clone(),
                    class: HoldingClass::Record,
                    stamp: format!("{} names its owner", runs.join(&run).join("owner").display()),
                    bytes: size_of(&entry.path()).0,
                    why: Some(
                        "its owner file is here and no firecracker carries its id: the run is over, \
                         its directory is a record"
                            .into(),
                    ),
                    use_: None,
                });
            }
        }
    }
    // A firecode state layer: the guest's own filesystem for this owner's project. It is a cache
    // (the tree it holds is rebuilt from the source), and firecode's own `gc --state` is what
    // removes one: a `rm -rf` under firecode's state root is the reap-by-pattern this design
    // refuses. Only a layer named for the owner is claimed - the project a VM is booted on is
    // named by the session id, so that is the layer this owner provably has.
    if let Some(state) = &places.settings.firecode_state {
        let layer = state.join(&owner.id);
        if layer.is_dir() {
            let (bytes, complete) = size_of(&layer);
            out.push(Found {
                owner: owner.id.clone(),
                kind: STATE.to_string(),
                host: None,
                handle: layer.display().to_string(),
                class: HoldingClass::Cache,
                stamp: format!(
                    "a state layer named for its owner under {}",
                    state.display()
                ),
                bytes,
                why: (!complete).then(|| "its size was not measured whole".into()),
                use_: None,
            });
        }
    }
    // The booted copy (a cache) and the copy-out beside it (work).
    if let Some(cache) = &places.firecode_cache {
        let copy = cache.join(&owner.id);
        if copy.is_dir() {
            let (bytes, complete) = size_of(&copy);
            out.push(Found {
                owner: owner.id.clone(),
                kind: COPY.to_string(),
                host: None,
                handle: copy.display().to_string(),
                class: HoldingClass::Cache,
                stamp: format!("a copy named for its owner under {}", cache.display()),
                bytes,
                why: (!complete).then(|| "its size was not measured whole".into()),
                use_: None,
            });
        }
        let prefix = format!("{}-", owner.id);
        if let Ok(entries) = std::fs::read_dir(cache) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if !name.starts_with(&prefix) || !entry.path().is_dir() {
                    continue;
                }
                let (bytes, complete) = size_of(&entry.path());
                out.push(Found {
                    owner: owner.id.clone(),
                    kind: COPY_OUT.to_string(),
                    host: None,
                    handle: entry.path().display().to_string(),
                    class: HoldingClass::Work,
                    stamp: format!("a copy-out named for its owner under {}", cache.display()),
                    bytes,
                    why: (!complete).then(|| "its size was not measured whole".into()),
                    use_: None,
                });
            }
        }
    }
    out
}

/// What sits in the places letibot writes and names no owner it knows: the "looks like ours" list.
/// A pattern only ever produces a row here, never a reap (`docs/lifecycle.md` c.4).
pub fn unowned(places: &Places, known: &BTreeSet<String>) -> Vec<Found> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(places.box_dir()) {
        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if known.contains(&name) {
                continue;
            }
            let stamp = places.stamp_at(&entry.path());
            let (bytes, _) = size_of(&entry.path());
            out.push(Found {
                owner: stamp
                    .as_ref()
                    .map(|s| s.owner.clone())
                    .unwrap_or_else(|| format!("unrecorded:{name}")),
                kind: "root".to_string(),
                host: None,
                handle: entry.path().display().to_string(),
                class: HoldingClass::Work,
                stamp: match &stamp {
                    Some(s) => format!(
                        "{} names `{}`",
                        entry.path().join(OWNER_JSON).display(),
                        s.owner
                    ),
                    None => format!("no {} in it", OWNER_JSON),
                },
                bytes,
                why: Some(match &stamp {
                    Some(s) => format!(
                        "it carries a stamp for `{}`, which this store does not know",
                        s.owner
                    ),
                    None => "it looks like an owner root and nothing in it says whose".into(),
                }),
                use_: None,
            });
        }
    }
    if let Some(cache) = &places.firecode_cache
        && let Ok(entries) = std::fs::read_dir(cache)
    {
        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if owner_of_cache_name(&name).is_some_and(|o| known.contains(&o)) {
                continue;
            }
            // Only the two shapes letibot writes: `<owner>` and `<owner>-<stamp>`.
            if owner_of_cache_name(&name).is_none() {
                continue;
            }
            let (bytes, _) = size_of(&entry.path());
            out.push(Found {
                owner: format!("unrecorded:{name}"),
                kind: COPY.to_string(),
                host: None,
                handle: entry.path().display().to_string(),
                class: HoldingClass::Cache,
                stamp: format!("named for an owner under {}", cache.display()),
                bytes,
                why: Some("its name says whose it is and this store has no such owner".into()),
                use_: None,
            });
        }
    }
    out
}

/// The owner a firecode cache directory is named for: `<owner>` or `<owner>-<stamp>`. `None` for a
/// name in neither shape, which is somebody else's directory and not a candidate at all.
fn owner_of_cache_name(name: &str) -> Option<String> {
    if name.starts_with("s-") {
        // `s-<digits>` is an owner; `s-<digits>-<stamp>` is a copy-out for it.
        let mut parts = name.split('-');
        parts.next();
        let digits = parts.next()?;
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        return Some(format!("s-{digits}"));
    }
    None
}

/// How big a path is, and whether the walk finished. A directory too big to walk inside the budget
/// answers the size it reached and `false`, which every caller says rather than passing off as a
/// measurement - on a box with two 38G targets, an honest partial number beats a hung plan.
pub fn size_of(path: &Path) -> (Option<u64>, bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(250);
    let mut total = 0u64;
    let mut entries = 0usize;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            entries += 1;
            if entries > 200_000 || std::time::Instant::now() > deadline {
                return (Some(total), false);
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                total += meta.len();
            }
        }
    }
    (Some(total), true)
}

/// This box's clock, as the store's rows are stamped.
pub fn now_ms() -> i64 {
    letibot_metrics::now_ms() as i64
}
