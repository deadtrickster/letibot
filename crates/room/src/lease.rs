//! **A lease on the coordinator itself** — one coordinator per box, held with the merge queue's
//! discipline, because two coordinators are two rooms with one name and no arbiter can settle that.
//!
//! `docs/room.md` § *What the daemon still owes* item 2 says *"a lease on the coordinator itself …
//! otherwise two coordinators are two rooms with one name, which no arbiter can settle"*, and item
//! 3 of the same list says the room keeps **no store**. So the lease is a **file**, not a row:
//! `crate::lease::CoordinatorLease` is `mergequeue::lease`'s rules — an owner identity, a beat, a
//! ninety-second TTL, a stale takeover — written where a process with no database can write it.
//!
//! # The three rules, and they are the queue lease's
//!
//! * **The owner is an identity, not a process name**: `host/pid/<ns>`, the same spelling
//!   `Store::acquire_queue_lease` uses, because *which claim is this* has to survive a daemon
//!   restarting into the same pid.
//! * **The beat is the liveness.** [`COORDINATOR_LEASE_TTL_MS`] is the merge queue's ninety seconds
//!   and [`LEASE_BEAT_EVERY_MS`] is a tenth of it, so nine beats are missed before a takeover.
//! * **A stale lease is taken over, and it says what it took.** [`Acquired::TookOver`] carries the
//!   dead holder and *why* it was dead — a beat that stopped, or a pid that is not running — because
//!   the two are different stories a person reading the log will want to tell apart.
//!
//! # Loud when it is not alone
//!
//! [`CoordinatorLease::acquire`] against a live holder answers [`Held`], whose [`Held::sentence`]
//! names the pid, the owner and how long the beat has been running. The coordinator refuses to
//! start with that sentence rather than binding a second socket and serving a second, empty room
//! that every session would find by the same name — which is the failure this whole module exists
//! to make impossible rather than unlikely.
//!
//! # The file
//!
//! One line, tab-separated: `owner\tpid\tbeat_ms\tstarted_ms`, written to a temporary beside it and
//! renamed, so a reader never sees half a lease. [`CoordinatorLease::read`] answers `None` for a
//! file that is missing, empty or unparseable — a lease nobody can read is not a lease anybody can
//! be refused by, and pretending otherwise would be a lock that is silently open.
//!
//! # The decision is atomic, because the launcher makes races ordinary
//!
//! `scripts/letibot` starts a coordinator for the box whenever one is not already serving, so two
//! sessions opened in the same instant is not a thought experiment: it is two `harnessd --room`
//! processes inside a millisecond of each other. Read-then-write is not atomic, and MEASURED on the
//! 2026-11-01 tree, that pair produced **two** `Acquired::Fresh` — the loser then failed at the
//! SOCKET BIND, so the operator was told about an address in use rather than about the coordinator
//! that holds the room, and its `release()` deleted the winner's lease (the winner went on serving
//! with no lease until its next beat put one back). So [`CoordinatorLease::acquire`] takes a lock
//! for the length of the decision — a sibling file, never renamed, because `write` renames a
//! temporary OVER the lease and a lock on that inode would follow the file that was just replaced.
//! The lease itself is still the one claim: this lock is the atomicity of one read-modify-write,
//! not a second thing to hold.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// **Ninety seconds** — the merge queue's own lease TTL (`mergequeue::lease::LEASE_TTL_MS`), so a
/// coordinator whose box slept or whose process was stopped is replaced within a minute and a half
/// and a coordinator held up by a slow write is not mistaken for a dead one.
pub const COORDINATOR_LEASE_TTL_MS: u64 = 90_000;

/// How often the holder says it is still here — nine beats to the TTL.
pub const LEASE_BEAT_EVERY_MS: u64 = 10_000;

/// How long one `acquire` waits for its turn at the decision. The act it guards is a read, a
/// compare and one `rename` — microseconds — so this is only ever spent when the process holding
/// the lock has stopped (a `SIGSTOP`, a box that slept), and then the fall-through is deliberate.
pub const DECISION_WAIT_MS: u64 = 2_000;

/// **A coordinator that holds the lease** — who, and since when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder {
    /// `host/pid/<ns>` — the queue lease's spelling, and per CLAIM rather than per process.
    pub owner: String,
    pub pid: u32,
    /// The last time the holder said it is alive.
    pub beat_ms: u64,
    /// When this claim was taken.
    pub started_ms: u64,
}

impl fmt::Display for Holder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (pid {})", self.owner, self.pid)
    }
}

/// **Another coordinator is alive here.** The refusal, with the words the daemon says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Held {
    pub holder: Holder,
    /// `now - beat_ms`: how long the holder has been beating.
    pub age_ms: u64,
    pub path: PathBuf,
}

impl Held {
    /// **The sentence a second coordinator refuses with.** It names the pid, the owner, the file
    /// and the age, because every one of those is a thing the operator needs to act on it.
    pub fn sentence(&self) -> String {
        format!(
            "another room coordinator is alive here: {} holds {} and beat {} ago. Two coordinators \
             are two rooms with one name, so this one will not start. Stop that one (pid {}), or \
             wait for its lease to go stale ({} after its last beat).",
            self.holder,
            self.path.display(),
            crate::item::duration(self.age_ms),
            self.holder.pid,
            crate::item::duration(COORDINATOR_LEASE_TTL_MS),
        )
    }
}

impl fmt::Display for Held {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.sentence())
    }
}

impl std::error::Error for Held {}

/// **Why a stale lease was taken over** — two different stories, told apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Takeover {
    /// Nobody beat inside the TTL.
    BeatStale { age_ms: u64 },
    /// The recorded pid is not running, whatever its beat says — the ordinary case after a crash
    /// or a `kill -9`, and the one that makes a takeover immediate instead of a minute and a half.
    PidGone { pid: u32 },
}

impl fmt::Display for Takeover {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Takeover::BeatStale { age_ms } => write!(
                f,
                "its beat had stopped — the last one was {} old",
                crate::item::duration(*age_ms)
            ),
            Takeover::PidGone { pid } => write!(f, "pid {pid} is not running"),
        }
    }
}

/// What an acquire did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Acquired {
    /// There was no lease: this is the first coordinator since the file was removed.
    Fresh,
    /// There was a dead one, and it was replaced.
    TookOver { from: Holder, why: Takeover },
}

/// **The coordinator's lease on one box.**
#[derive(Debug, Clone)]
pub struct CoordinatorLease {
    path: PathBuf,
    holder: Holder,
}

impl CoordinatorLease {
    /// A lease at `path`, owned by this process: `host/pid/<ns>`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self::owned_by(
            path,
            format!("{}/{}/{}", host_name(), std::process::id(), now_ns()),
        )
    }

    /// A lease at `path` with an owner the caller spells — the fixture's door, and the one a
    /// daemon that already has an identity (a restarted coordinator keeping its name) takes.
    pub fn owned_by(path: impl Into<PathBuf>, owner: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            holder: Holder {
                owner: owner.into(),
                pid: std::process::id(),
                beat_ms: 0,
                started_ms: 0,
            },
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn owner(&self) -> &str {
        &self.holder.owner
    }

    pub fn pid(&self) -> u32 {
        self.holder.pid
    }

    /// **What the file says**, or `None` — missing, empty, or unreadable as a lease.
    pub fn read(&self) -> Option<Holder> {
        let text = std::fs::read_to_string(&self.path).ok()?;
        parse(&text)
    }

    /// **Take the lease, or be refused by name.**
    ///
    /// A live holder — a beat inside the TTL and a pid that is running — is [`Held`]. Anything else
    /// is a takeover, and it is written down as one.
    ///
    /// The decision is made under [`CoordinatorLease::decision_lock`], so two coordinators starting
    /// in the same instant answer *one* `Fresh` and *one* [`Held`] naming the winner — see the
    /// module doc for the measurement that made this necessary.
    pub fn acquire(&self, now: u64) -> Result<Acquired, Held> {
        let _turn = self.decision_lock();
        let existing = self.read();
        let acquired = match &existing {
            None => Acquired::Fresh,
            Some(h) => {
                let age = now.saturating_sub(h.beat_ms);
                if age <= COORDINATOR_LEASE_TTL_MS && pid_alive(h.pid) {
                    return Err(Held {
                        holder: h.clone(),
                        age_ms: age,
                        path: self.path.clone(),
                    });
                }
                let why = if !pid_alive(h.pid) {
                    Takeover::PidGone { pid: h.pid }
                } else {
                    Takeover::BeatStale { age_ms: age }
                };
                Acquired::TookOver {
                    from: h.clone(),
                    why,
                }
            }
        };
        self.write(now).map_err(|_| Held {
            // A file we cannot write is a lease we cannot hold: answer the holder we read, so the
            // caller's sentence is about the room rather than about the filesystem.
            holder: existing.unwrap_or_else(|| self.holder.clone()),
            age_ms: 0,
            path: self.path.clone(),
        })?;
        Ok(acquired)
    }

    /// **Say we are still here.** Rewrites the beat and leaves `started_ms` where it was.
    pub fn beat(&self, now: u64) -> io::Result<()> {
        self.write(now)
    }

    /// Give it back — the coordinator stopping in an orderly way. A lease whose file is gone is
    /// nobody's, which is what makes the next start [`Acquired::Fresh`] rather than a takeover.
    pub fn release(&self) -> io::Result<()> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// **The lock one `acquire` holds while it reads, decides and writes.**
    ///
    /// `File`'s locks are `flock` on Unix and die with the process, including on `SIGKILL` — the
    /// property a filename does not have, which is why `sessionlog::server`'s `FolderClaim` is the
    /// same shape. The path is the lease's own with `.lock` on the end and is **never unlinked**:
    /// `write` renames a temporary over the lease, so a lock taken on the lease's inode would be a
    /// lock on a file that no longer has the name.
    ///
    /// A filesystem that cannot lock, and a turn that does not come inside [`DECISION_WAIT_MS`],
    /// both fall through to the unlocked decision: refusing to start a room is a worse outcome than
    /// failing to catch a second one, and the socket bind is still behind this.
    fn decision_lock(&self) -> Option<std::fs::File> {
        let path = self.path.with_extension("lease.lock");
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .ok()?;
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_millis(DECISION_WAIT_MS);
        loop {
            match file.try_lock() {
                Ok(()) => return Some(file),
                Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                // Another `acquire` has been in there longer than the bound, or this filesystem
                // cannot lock at all. Decide unlocked rather than refuse to be a room.
                Err(_) => return None,
            }
        }
    }

    fn write(&self, now: u64) -> io::Result<()> {
        let started = match self.read() {
            Some(h) if h.owner == self.holder.owner => h.started_ms.max(1),
            _ => now.max(1),
        };
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let line = format!(
            "{}\t{}\t{}\t{}\n",
            self.holder.owner, self.holder.pid, now, started
        );
        let tmp = self
            .path
            .with_extension(format!("lease-{}", std::process::id()));
        std::fs::write(&tmp, line)?;
        std::fs::rename(&tmp, &self.path)
    }
}

fn parse(text: &str) -> Option<Holder> {
    // `trim_end` and not `trim`: a leading tab is the FIRST FIELD being empty, which is a lease
    // nobody can read — and `trim` would eat it and read the pid as the owner.
    let line = text.lines().next()?.trim_end();
    let mut f = line.split('\t');
    let owner = f.next()?.trim();
    let pid: u32 = f.next()?.trim().parse().ok()?;
    let beat_ms: u64 = f.next()?.trim().parse().ok()?;
    let started_ms: u64 = f.next().unwrap_or("0").trim().parse().unwrap_or(beat_ms);
    if owner.is_empty() {
        return None;
    }
    Some(Holder {
        owner: owner.to_string(),
        pid,
        beat_ms,
        started_ms,
    })
}

/// **Is `pid` a running process?** `kill -0`, which asks the kernel and signals nothing.
///
/// A pid we may not signal answers *not running*, and that is deliberate: the lease's holder is
/// the same user as the asker on every box this runs on, and a lease taken over from a process we
/// cannot even ask about is the safer of the two mistakes — the alternative is a room nobody can
/// restart after a uid change.
///
/// Public because *is the room there* is a question two doors ask: this one, taking over a lease,
/// and [`crate::restart::room_running`], deciding whether a restart has anything to stop.
pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn now_ns() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// **This box's name**, as `uname -n` answers it — `hostname`'s own value, which is what a person
/// comparing two boxes would type.
pub fn host_name() -> String {
    let raw = std::process::Command::new("uname")
        .arg("-n")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "localhost".to_string());
    raw
}

/// **The key a room is named by on this box** — the host's name, made safe for a filename, with a
/// short hash of the raw name appended.
///
/// The hash is there for the two cases a bare name cannot survive: two boxes that both call
/// themselves `localhost` (a container, a VM with no hostname set), and a name with characters a
/// socket path cannot hold. It is FNV-1a and written out here rather than taken from a crate,
/// because it must answer the same twelve hex characters for ever — the socket, the lease and the
/// session id are all derived from it, and a `DefaultHasher` that changed with a Rust version would
/// rename a live room.
pub fn host_key() -> String {
    host_key_of(&host_name())
}

/// **The key, from the name** — [`host_key`] without the `uname`, so a test can pin the answer and
/// `scripts/letibot` can be held to the same table.
///
/// The launcher derives this key in bash (it has to know the socket before it starts anything), and
/// the two spellings must agree or the launcher would check a path no coordinator ever binds: it
/// would start a second one every run and be refused by the lease, for ever, loudly. The shell's
/// copy is `room_host_key` in `scripts/letibot`, and both are pinned to the table in the test below.
pub fn host_key_of(raw: &str) -> String {
    let mut safe: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    safe.truncate(24);
    while safe.ends_with('-') {
        safe.pop();
    }
    if safe.is_empty() {
        safe.push_str("host");
    }
    format!("{safe}-{:08x}", fnv1a(raw.as_bytes()) as u32)
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// **The coordinator's own session id** — `s-room-<host key>`, derived and never invented.
///
/// `docs/room.md` § *The precedent* asks for exactly this: the review queue's `s-gatekeeper-<key>`
/// is derived from the workspace so two daemons cannot share one transcript, and a room is per BOX
/// rather than per folder, so the coordinator's key is the host's. It is also the name the
/// operator connects to (`letibot --session host` resolves to it), which is why it must not move.
pub fn coordinator_session(host_key: &str) -> String {
    format!("s-room-{host_key}")
}

/// The file a box's coordinator lease lives in, given the directory the runtime keeps its files in.
pub fn lease_path(runtime_dir: &Path, host_key: &str) -> PathBuf {
    runtime_dir.join(format!("room-{host_key}.lease"))
}

/// The socket a box's coordinator serves, given the same directory.
///
/// The name is derived from the HOST and not from a folder, because a room is the box's: every
/// daemon on this machine — whatever project it is serving — must find the same one socket.
pub fn socket_path(runtime_dir: &Path, host_key: &str) -> PathBuf {
    runtime_dir.join(format!("room-{host_key}.sock"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> tempdir::TempDir {
        tempdir::TempDir::new()
    }

    /// A temporary directory with no dependency: one `mkdir` under the test's own scratch.
    mod tempdir {
        use std::path::{Path, PathBuf};

        pub struct TempDir(PathBuf);

        impl TempDir {
            pub fn new() -> TempDir {
                use std::sync::atomic::{AtomicU64, Ordering};
                // A counter beside the clock: two tests can start inside one clock tick on this
                // box, and a shared directory would make one test's `Drop` another's mystery.
                static NEXT: AtomicU64 = AtomicU64::new(0);
                let n = NEXT.fetch_add(1, Ordering::SeqCst);
                let p = std::env::temp_dir()
                    .join(format!("letibot-room-lease-{}-{n}", std::process::id()));
                std::fs::create_dir_all(&p).expect("a scratch directory");
                TempDir(p)
            }

            pub fn path(&self) -> &Path {
                &self.0
            }
        }

        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    fn a_lease(t: &tempdir::TempDir) -> CoordinatorLease {
        CoordinatorLease::owned_by(t.path().join("room.lease"), "box/1/100")
    }

    #[test]
    fn a_second_coordinator_is_refused_by_name_while_the_first_beats() {
        let t = dir();
        let first = a_lease(&t);
        assert_eq!(first.acquire(1_000), Ok(Acquired::Fresh));

        let second = CoordinatorLease::owned_by(t.path().join("room.lease"), "box/2/200");
        let held = second
            .acquire(1_000 + 1)
            .expect_err("a live holder refuses");
        assert_eq!(held.holder.owner, "box/1/100");
        assert_eq!(held.holder.pid, std::process::id());
        // The sentence is the point: it names the holder, the file and what to do.
        let said = held.sentence();
        for wanted in ["box/1/100", "room.lease", "Stop that one", "1m 30s"] {
            assert!(
                said.contains(wanted),
                "the refusal must say {wanted}: {said}"
            );
        }
    }

    #[test]
    fn a_beat_inside_the_ttl_holds_and_one_past_it_is_taken_over() {
        let t = dir();
        let first = a_lease(&t);
        first.acquire(1_000).unwrap();
        first.beat(1_000 + COORDINATOR_LEASE_TTL_MS).unwrap();

        // At the boundary it is still alive — the queue lease's own comparison.
        let second = CoordinatorLease::owned_by(t.path().join("room.lease"), "box/2/200");
        assert!(second.acquire(1_000 + COORDINATOR_LEASE_TTL_MS).is_err());

        // One millisecond later the beat is stale — but the pid is this test's, so it is the BEAT
        // that decides, and the takeover says which.
        let took = second
            .acquire(1_000 + COORDINATOR_LEASE_TTL_MS * 2 + 1)
            .expect("a stale beat is taken over");
        match took {
            Acquired::TookOver { from, why } => {
                assert_eq!(from.owner, "box/1/100");
                assert!(
                    matches!(why, Takeover::BeatStale { .. }),
                    "a live pid with a stale beat is BeatStale: {why:?}"
                );
                assert!(why.to_string().contains("beat had stopped"));
            }
            other => panic!("expected a takeover, got {other:?}"),
        }
        // And the file now names the new owner.
        assert_eq!(
            second.read().map(|h| h.owner),
            Some("box/2/200".to_string())
        );
    }

    #[test]
    fn a_dead_pid_is_taken_over_at_once_however_fresh_its_beat() {
        let t = dir();
        let path = t.path().join("room.lease");
        // A pid that is certainly not running: 0 is never a process, and `kill -0 0` would mean
        // "this process group", which is why `running` answers false for it by name.
        std::fs::write(&path, "box/9/999\t0\t5000\t1000\n").unwrap();
        let lease = CoordinatorLease::owned_by(&path, "box/1/100");
        match lease.acquire(5_001).expect("a dead holder does not refuse") {
            Acquired::TookOver { from, why } => {
                assert_eq!(from.pid, 0);
                assert_eq!(why, Takeover::PidGone { pid: 0 });
            }
            other => panic!("expected a takeover, got {other:?}"),
        }
    }

    #[test]
    fn a_released_lease_is_a_fresh_start_and_not_a_takeover() {
        let t = dir();
        let first = a_lease(&t);
        first.acquire(1_000).unwrap();
        first.release().unwrap();
        assert_eq!(first.read(), None);
        // The second one is FRESH, which is the difference the pane must be able to say: nothing
        // was taken over, there was simply nothing there.
        let second = CoordinatorLease::owned_by(t.path().join("room.lease"), "box/2/200");
        assert_eq!(second.acquire(1_001), Ok(Acquired::Fresh));
        // Releasing twice is not an error: a stop is a stop.
        first.release().unwrap();
        first.release().unwrap();
    }

    #[test]
    fn a_lease_file_nobody_can_read_is_no_lease_at_all() {
        let t = dir();
        let path = t.path().join("room.lease");
        for junk in ["", "\n", "not a lease", "owner\tnotapid\t1\t2", "\t1\t2\t3"] {
            std::fs::write(&path, junk).unwrap();
            let lease = CoordinatorLease::owned_by(&path, "box/1/100");
            assert_eq!(
                lease.read(),
                None,
                "junk must not read as a holder: {junk:?}"
            );
            assert_eq!(
                lease.acquire(10),
                Ok(Acquired::Fresh),
                "and it must not refuse a start either: {junk:?}"
            );
        }
    }

    #[test]
    fn a_beat_leaves_the_start_where_it_was() {
        let t = dir();
        let lease = a_lease(&t);
        lease.acquire(1_000).unwrap();
        lease.beat(2_000).unwrap();
        lease.beat(3_000).unwrap();
        let h = lease.read().unwrap();
        assert_eq!(h.beat_ms, 3_000);
        assert_eq!(h.started_ms, 1_000, "the claim started when it was taken");
    }

    #[test]
    fn the_host_key_is_stable_and_safe_for_a_path() {
        let key = host_key();
        assert_eq!(key, host_key(), "the same box must always be the same room");
        assert!(
            key.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
            "a socket path cannot hold this: {key}"
        );
        assert!(
            key.len() <= 33,
            "a unix socket path has a length limit: {key}"
        );
        assert_eq!(coordinator_session(&key), format!("s-room-{key}"));
    }

    /// **The table `scripts/letibot`'s `room_host_key` is pinned to as well.** The launcher derives
    /// the socket's name in bash before it starts anything, so a key that disagrees with this one
    /// means the launcher checks a path no coordinator binds: it starts a second coordinator on
    /// every run and is refused by the lease, for ever. Both sides carry these four lines.
    #[test]
    fn the_host_key_matches_what_the_launcher_derives() {
        for (raw, want) in [
            ("lab2x1", "lab2x1-e666427d"),
            ("MacBook-Pro", "macbook-pro-45a1c8db"),
            ("dead's box", "dead-s-box-5f45071c"),
            ("localhost", "localhost-03ad937a"),
        ] {
            assert_eq!(
                host_key_of(raw),
                want,
                "the launcher's `room_host_key {raw}` must answer the same key"
            );
        }
        // A name longer than a filename can carry is cut, and its dashes are not left hanging.
        let long = host_key_of(&"x".repeat(40));
        assert!(long.starts_with(&"x".repeat(24)), "{long}");
        assert_eq!(long.len(), 24 + 1 + 8, "{long}");
        assert_eq!(host_key_of("---"), format!("host-{:08x}", 0xde1b_3246u32));
    }

    /// **Two coordinators starting in the same instant: one room, and the loser is told who holds
    /// it.** `scripts/letibot` starts a coordinator whenever one is not serving, so two sessions
    /// opening together is an ordinary Tuesday — and before the decision lock, both answered
    /// `Fresh` (the loser then failed at the socket bind, and its `release()` deleted the winner's
    /// lease).
    #[test]
    fn a_race_for_the_lease_gives_one_coordinator_and_names_the_holder() {
        let t = dir();
        let path = t.path().join("room.lease");
        // Eight claimants, all released at once: a barrier rather than a sleep, because the defect
        // is a window of microseconds and a slow test would step around it.
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let mut threads = Vec::new();
        for n in 0..8 {
            let path = path.clone();
            let barrier = std::sync::Arc::clone(&barrier);
            threads.push(std::thread::spawn(move || {
                let lease = CoordinatorLease::owned_by(&path, format!("box/{n}/100"));
                barrier.wait();
                lease.acquire(1_000).map(|_| format!("box/{n}/100"))
            }));
        }
        let outcomes: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        let winners: Vec<&String> = outcomes.iter().filter_map(|o| o.as_ref().ok()).collect();
        assert_eq!(
            winners.len(),
            1,
            "one coordinator per box, whatever the race: {winners:?}"
        );
        let winner = winners[0].clone();
        for outcome in &outcomes {
            if let Err(held) = outcome {
                assert_eq!(
                    held.holder.owner, winner,
                    "the loser is told, by name, who holds the room"
                );
                assert!(
                    held.sentence().contains(&winner),
                    "and the sentence it refuses with says so: {}",
                    held.sentence()
                );
            }
        }
        // And the file names the winner, so the NEXT start is refused by the same name.
        let lease = CoordinatorLease::owned_by(&path, "box/9/100");
        assert_eq!(lease.read().map(|h| h.owner), Some(winner));
    }
}
