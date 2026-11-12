//! Cgroup-scoped process lifetime, and the record that makes its zero falsifiable.
//!
//! ```text
//!   letibot.<pid>.<n>/                  one tree's root - see `probe_under`
//!     session.<sid>/                    ScopeKind::Session
//!       turn.<tid>/                     ScopeKind::Turn - a child DIRECTORY
//!         job.<n>/                      one command
//!     explicit.<name>/                  ScopeKind::Explicit - a SIBLING of the session
//! ```
//!
//! Three scopes and no fourth: `explicit` is a sibling so ending the session does not reap
//! it. A process joins its cgroup before `exec`, so nothing can fork outside it, and every
//! end produces a [`Reaping`] that counts what was there before the kill and what survived.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use super::ExecError;

/// Who owns a process's lifetime. Three, and the enum is closed on purpose: a fourth would be
/// a scope nobody decided the reaping rule for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ScopeKind {
    /// Dies when the turn ends. The default for a command the model waits on.
    Turn,
    /// Dies when the session ends. The default for a background job: work that outlives a turn
    /// belongs to the session.
    Session,
    /// Survives the session, because somebody said so, and is listed as such.
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

/// One process a reap found, named before it was killed. The `cmdline` is what makes the record
/// usable rather than merely present: a list of pids from an hour ago identifies nothing.
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

    /// How this process should be named in a record. `cmdline` can legitimately be empty - a
    /// process caught between `fork` and `exec` has no `mm` - so the chain is the command line,
    /// then `comm`, then a sentence: never a bare `?`, which is indistinguishable from a bug.
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

/// What a scope's end actually did. Not a boolean: a zero produced by vigilance and a zero
/// produced by a mechanism are the same number and different facts, so `observed` is the
/// presence half and `survivors` is the falsifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reaping {
    pub scope: ScopeId,
    pub at: SystemTime,
    /// `cgroup.kill`, or the per-pid fallback, or `nothing to kill`. Named, because
    /// a fallback that is not visible is a fallback nobody knows they are running.
    pub mechanism: &'static str,
    /// Members read out of the cgroup before the kill. The denominator.
    pub observed: Vec<Reaped>,
    /// Members still there after the kill and the wait. Non-empty is a defect, and the record
    /// says so rather than the reaper claiming success.
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

/// What moving a scope's processes into another scope actually did. Not a boolean: a promotion
/// claims the work is now reaped elsewhere, and a process that did not move makes that claim
/// false for itself, so what was there, what moved and what was left behind all travel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Migration {
    pub from: ScopeId,
    pub to: ScopeId,
    /// Members read out of `from` before anything moved. The denominator.
    pub observed: Vec<Reaped>,
    /// Pids found in `to` afterwards.
    pub moved: Vec<u32>,
    /// Pids still in `from` afterwards. Non-empty means the promotion is partial, and those
    /// processes still die with the old scope.
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
    /// Did every process that still existed end up in the new scope? Deliberately does not
    /// require `removed`: a directory that would not go away is debris for [`Migration::note`],
    /// not a process whose lifetime is owned by the wrong scope.
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

/// The lifetime mechanism, as a seam, for the same reason [`crate::backend::ExecBackend`] is one:
/// a firecode guest owns its processes by owning the whole VM, and nothing above this line changes
/// when that is what is underneath.
pub trait ScopeTree: Send + Sync {
    /// One line for `EXPLAIN`, naming the mechanism and where its root is.
    fn describe(&self) -> String;

    /// Open a scope. `parent` is `None` for a top-level scope; a child scope's cgroup is created
    /// inside the parent's, which is what makes the parent's end reap it.
    fn open(
        &self,
        kind: ScopeKind,
        name: &str,
        parent: Option<&ScopeId>,
    ) -> Result<ScopeId, ExecError>;

    /// The pids in this scope and every scope under it. This is what replaces `pgrep`: there is
    /// no pattern, so there is nothing that can match the process asking.
    fn members(&self, scope: &ScopeId) -> Result<Vec<u32>, ExecError>;

    /// Kill everything in this scope and everything under it, and record it. Takes the
    /// observation before the kill and the survivor check after, so a caller cannot record only one.
    fn end(&self, scope: &ScopeId) -> Reaping;

    /// Move every process in `from` into `to`, so a different scope reaps them from now on. No
    /// default implementation on purpose: a silent "moved 0 of 0" would let a caller announce a
    /// promotion that never happened, so a tree that cannot migrate must say so.
    fn migrate(&self, from: &ScopeId, to: &ScopeId) -> Result<Migration, ExecError>;

    /// Every scope this tree has open, for a listing that can show what is watching and for whom.
    /// An invisible watcher is an unreapable one.
    fn list(&self) -> Vec<ScopeId>;

    /// Where this tree keeps its scopes, when that is a directory a later process can name: the
    /// fact that lets a daemon find what a dead one left behind. `None` for a tree with no place
    /// of its own, which is a tree nothing can be swept in.
    fn root(&self) -> Option<PathBuf> {
        None
    }

    /// Note a scope that already exists and that this tree did not open: a job adopted from a peer
    /// whose cgroup sits under its root. Nothing is reaped differently for knowing it, only listed.
    fn remember(&self, _scope: &ScopeId) {}

    /// Re-attach a job's process to `scope` after a restart lost the tree's memory of it, from
    /// facts the kernel keeps: the leader pid is its group id, and `identity` (`start:<n>`) tells
    /// the leader from a later process with that number. Returns how many members the scope has
    /// afterwards. A tree whose membership survives a restart needs nothing and answers 0.
    fn claim(&self, _scope: &ScopeId, _pid: u32, _identity: &str) -> usize {
        0
    }

    /// Remove the tree's own empty directories, and report how many went. Only unpopulated
    /// directories go: a populated one is somebody's live work, and the kernel refuses it anyway.
    /// Debris that holds nothing is still debris.
    fn prune(&self) -> usize {
        0
    }

    /// Whether this tree can cap a scope's memory, and if not why, in a sentence the model and
    /// the operator are shown. The default is no: a tree that cannot cap must not look like one
    /// that can, for the reason [`NoScopes`] gives.
    fn memory_caps(&self) -> Result<(), String> {
        Err("this host's lifetime mechanism has no memory controller".into())
    }

    /// Cap `scope`'s memory at `bytes`, swap included (no swap at all - a job paging out to a
    /// half-gigabyte swap is a box that stops answering before it is killed). The kernel kills the
    /// scope's processes, and only them, when they reach it.
    fn cap_memory(&self, _scope: &ScopeId, _bytes: u64) -> Result<(), String> {
        self.memory_caps()
    }

    /// What `scope`'s memory did, read from the kernel - before the scope is ended, because
    /// ending it removes the files this reads.
    fn memory_use(&self, _scope: &ScopeId) -> Option<MemoryUse> {
        None
    }
}

/// What a scope's memory did, as the kernel counted it: the cap it ran under, what it holds, the
/// most it ever held, and how many of its processes the kernel killed for reaching the cap.
/// `oom_kills` is what lets a result say killed at its memory cap rather than leaving the model
/// to read `137`; `current` is what stops a listing calling a running job killed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MemoryUse {
    /// `None` is `memory.max` reading `max`: the scope ran uncapped.
    pub cap: Option<u64>,
    /// `memory.current`, bytes: what the scope holds at the reading.
    pub current: u64,
    /// `memory.peak`, bytes.
    pub peak: u64,
    /// `memory.events`' `oom_kill`: processes the kernel killed at the cap.
    pub oom_kills: u64,
}

thread_local! {
    /// The last foreground command's memory on THIS thread - see [`note_memory`].
    static LAST: std::cell::Cell<Option<MemoryUse>> = const { std::cell::Cell::new(None) };
}

/// What a scope holds RIGHT NOW, read from the cgroup: the answer no `ps` can give a shell in
/// another PID namespace, and the reason a job whose pid its own seat cannot see is still shown to
/// be alive with its memory. Every field is the kernel's; a file that is not there is `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ScopeUse {
    /// `cgroup.events`' `populated`: anything in the subtree, the race-free liveness test.
    pub populated: bool,
    /// Processes now, from `cgroup.procs` and every directory under it.
    pub procs: u64,
    /// `memory.current`.
    pub memory_current: Option<u64>,
    /// `memory.peak`.
    pub memory_peak: Option<u64>,
    /// `memory.max`; `None` is the word `max`, which is an uncapped scope and not a missing file.
    pub memory_max: Option<u64>,
    /// `memory.events`' `oom`: times the scope hit its cap (and something survived it).
    pub oom: u64,
    /// `memory.events`' `oom_kill`: processes the kernel killed at the cap.
    pub oom_kills: u64,
    /// `cpu.stat`'s `usage_usec`.
    pub cpu_usec: Option<u64>,
}

impl ScopeUse {
    /// The one clause every listing and plan row carries: `75 processes, 17.0G now of a 32.0G cap,
    /// peak 30.1G, 591 OOM kills`. The count first because it is the fact a pid cannot give.
    pub fn clause(&self) -> String {
        let mut parts = vec![match self.procs {
            0 => "no processes".to_string(),
            1 => "1 process".to_string(),
            n => format!("{n} processes"),
        }];
        if let Some(now) = self.memory_current {
            parts.push(match self.memory_max {
                Some(cap) => format!("{} now of a {} cap", human_bytes(now), human_bytes(cap)),
                None => format!("{} now, uncapped", human_bytes(now)),
            });
        }
        if let Some(peak) = self.memory_peak {
            parts.push(format!("peak {}", human_bytes(peak)));
        }
        if self.oom_kills > 0 {
            parts.push(format!("{} OOM kills", self.oom_kills));
        } else if self.oom > 0 {
            parts.push(format!("{} OOM events", self.oom));
        }
        parts.join(", ")
    }

    /// Is anything in it? `populated` is the kernel's own answer, so this never races a fork.
    pub fn alive(&self) -> bool {
        self.populated || self.procs > 0
    }
}

/// [`ScopeUse`] for one cgroup directory, or `None` when the directory is not there at all - a
/// scope already removed, or a host with no cgroup v2. A directory with no memory controller still
/// answers its process count and `populated`, which is why the files are read one by one.
pub fn use_at(dir: &Path) -> Option<ScopeUse> {
    let events = std::fs::read_to_string(dir.join("cgroup.events")).ok()?;
    let populated = events
        .lines()
        .find_map(|l| l.strip_prefix("populated "))
        .map(|v| v.trim() == "1")
        .unwrap_or(false);
    let mut procs = 0u64;
    count_members(dir, &mut procs);
    let read = |name: &str| -> Option<u64> {
        std::fs::read_to_string(dir.join(name))
            .ok()?
            .trim()
            .parse::<u64>()
            .ok()
    };
    let memory = std::fs::read_to_string(dir.join("memory.events")).unwrap_or_default();
    let event = |name: &str| -> u64 {
        memory
            .lines()
            .find_map(|l| l.strip_prefix(name))
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0)
    };
    Some(ScopeUse {
        populated,
        procs,
        memory_current: read("memory.current"),
        memory_peak: read("memory.peak"),
        // `max` is uncapped, and `.parse` failing on that word is the answer - not a missing file.
        memory_max: read("memory.max"),
        oom: event("oom "),
        oom_kills: event("oom_kill "),
        cpu_usec: std::fs::read_to_string(dir.join("cpu.stat"))
            .ok()
            .and_then(|s| {
                s.lines()
                    .find_map(|l| l.strip_prefix("usage_usec "))
                    .and_then(|v| v.trim().parse().ok())
            }),
    })
}

/// Count the processes in one cgroup directory and every directory under it.
fn count_members(dir: &Path, out: &mut u64) {
    if let Ok(s) = std::fs::read_to_string(dir.join("cgroup.procs")) {
        *out += s.lines().filter(|l| !l.trim().is_empty()).count() as u64;
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                count_members(&e.path(), out);
            }
        }
    }
}

/// A span as a listing writes it: `900ms`, `12s`, `4m`, `2h03m`, `3d4h`.
pub fn human_millis(ms: u64) -> String {
    let secs = ms / 1000;
    match secs {
        0 => format!("{ms}ms"),
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
        s => format!("{}d{}h", s / 86_400, (s % 86_400) / 3600),
    }
}

/// A command's memory, left for whoever ran the tool call on this thread to read. A side channel
/// rather than a field on `ToolResult`, which has no other use for it: `bash` runs its foreground
/// wait on the thread that invoked it, so the invoker takes this the moment the call returns.
pub fn note_memory(u: MemoryUse) {
    LAST.with(|c| c.set(Some(u)));
}

/// What [`note_memory`] left on this thread, once.
pub fn take_memory() -> Option<MemoryUse> {
    LAST.with(|c| c.take())
}

impl MemoryUse {
    /// Whether the cap is what ended the command.
    pub fn killed_at_cap(&self) -> bool {
        self.oom_kills > 0
    }

    /// Whether this run came within a tenth of its cap: the one case besides a kill in which a
    /// result says more than the peak. A run that nearly died is worth saying, because the next
    /// one may not.
    pub fn near_its_cap(&self) -> bool {
        self.cap
            .is_some_and(|cap| cap > 0 && self.peak.saturating_mul(10) >= cap.saturating_mul(9))
    }

    /// The clause a tool result carries: `peak 37 MiB of a 64 MiB cap`.
    pub fn clause(&self) -> String {
        match self.cap {
            Some(cap) => format!(
                "peak {} of a {} cap",
                human_bytes(self.peak),
                human_bytes(cap)
            ),
            None => format!("peak {}, uncapped", human_bytes(self.peak)),
        }
    }

    /// The clause a row about a RUNNING job carries: `17 GiB now, peak 32 GiB of a 32 GiB cap`.
    /// What it holds is the fact a reader needs there, and a peak alone cannot tell a job living
    /// at its cap from one that was killed by it.
    pub fn live_clause(&self) -> String {
        let now = human_bytes(self.current);
        match self.cap {
            Some(cap) => format!(
                "{now} now, peak {} of a {} cap",
                human_bytes(self.peak),
                human_bytes(cap)
            ),
            None => format!("{now} now, peak {}, uncapped", human_bytes(self.peak)),
        }
    }

    /// Whether it is at its cap this instant, which for a running job means the kernel is killing
    /// its processes one by one and it stays alive between them.
    pub fn at_its_cap(&self) -> bool {
        self.cap
            .is_some_and(|cap| cap > 0 && self.current >= cap.saturating_mul(99) / 100)
    }
}

/// How much CPU a scope burned between two readings, and over how long: `cpu.stat` reports a
/// total for the life of the cgroup, so only a difference says whether anything is working.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuBurn {
    /// `usage_usec` now, less what it was at the previous reading.
    pub used: Duration,
    /// How long the two readings were apart.
    pub over: Duration,
}

impl CpuBurn {
    /// Whether the scope's processes were working over that interval. A tenth of one core is
    /// enough to tell a run that is thinking from one that is not - and a job of seventy fuzz
    /// workers that have written nothing is exactly the case this has to get right.
    pub fn busy(&self) -> bool {
        !self.over.is_zero() && self.used.as_micros() * 10 >= self.over.as_micros()
    }

    /// The clause a row carries: `busy — 4.2s of the last 30.0s`.
    pub fn clause(&self) -> String {
        format!(
            "{} — {} of the last {}",
            if self.busy() { "busy" } else { "idle" },
            span(self.used),
            span(self.over)
        )
    }
}

/// A duration as a row reads it: `0.4s` under a minute, `21m05s` over it.
pub(crate) fn span(d: Duration) -> String {
    let s = d.as_secs_f32();
    if s < 60.0 {
        format!("{s:.1}s")
    } else {
        format!("{:02}m{:02}s", (s / 60.0) as u64, (s % 60.0) as u64)
    }
}

/// The most command names a row names.
pub const TOP_COMMANDS: usize = 3;

/// How long a first reading watches `cpu.stat` for when there is no previous reading to subtract:
/// long enough to tell a working process from a sleeping one, short enough that one listing does
/// not stall on it.
const CPU_SAMPLE: Duration = Duration::from_millis(100);

/// How long a scope's last reading is kept for the next one to subtract. Past this the two are
/// too far apart to call an interval, and the entry is one more finished job's path in memory.
const FORGET_AFTER: Duration = Duration::from_secs(30 * 60);

/// What one scope's cgroup holds at one instant, read when a job is listed: the processes in it
/// and what they are, and the two facts a total cannot give. This is the answer to *is this job
/// alive* that a shell inside the sandbox cannot get from `ps` - the sandbox has its own PID
/// namespace, so every pid it can see is a pid that is not the job's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holding {
    /// Live processes anywhere under the scope, `cgroup.procs` counted recursively.
    pub processes: usize,
    /// Command names, most numerous first, at most [`TOP_COMMANDS`] of them.
    pub top: Vec<(String, usize)>,
    /// Whether the leader pid is a process right now. A dead leader over a populated cgroup is a
    /// job whose shell went and whose work did not, which is not a stale handle.
    pub leader_alive: bool,
    /// What the cgroup burned between this reading and the previous one.
    pub cpu: Option<CpuBurn>,
    /// How long ago `memory.events`' `oom_kill` was last seen to grow. `None` on the first
    /// reading of a scope already at its cap: the count says how many, not when.
    pub oom_since: Option<Duration>,
    /// The clock and the totals this reading was taken at, which is what makes the next one a
    /// delta. Private: a caller passes this reading back in and reads the fields above.
    seen: Seen,
}

/// The totals one reading leaves behind for the next one to subtract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Seen {
    at: Instant,
    cpu_usec: Option<u64>,
    oom_kills: u64,
    /// When this reader last saw `oom_kills` grow: a counter says how many, never how recently.
    oom_moved: Option<Instant>,
}

impl Holding {
    /// One reading of `dir`, the cgroup of a job whose leader is `pid`. `prev` is the reading
    /// this process took of the same directory last - without one, CPU comes from a short sample
    /// and the OOM count has no recency yet.
    pub fn read(dir: &Path, pid: u32, prev: Option<&Holding>) -> Holding {
        Holding::read_in(dir, Path::new("/proc"), pid, prev)
    }

    /// [`Holding::read`], with the process table's root named: `/proc` on a host, a fixture
    /// directory in a test, because a fake cgroup tree's pids are not real processes.
    pub fn read_in(dir: &Path, proc_root: &Path, pid: u32, prev: Option<&Holding>) -> Holding {
        let mut pids = Vec::new();
        members_at(dir, &mut pids);
        pids.sort_unstable();
        pids.dedup();
        let top = top_commands(&pids, proc_root);
        let leader_alive = pid != 0 && proc_root.join(pid.to_string()).exists();
        let oom_kills = oom_kills_at(dir);
        let (now, cpu_usec, cpu, oom_moved) = match prev.map(|p| p.seen) {
            Some(before) => {
                let now = Instant::now();
                // Monotonic, so a reading taken later is always the later one.
                let over = now.saturating_duration_since(before.at);
                let cpu_usec = cpu_usec_at(dir);
                let cpu = match (before.cpu_usec, cpu_usec) {
                    (Some(was), Some(is)) if !over.is_zero() => Some(CpuBurn {
                        used: Duration::from_micros(is.saturating_sub(was)),
                        over,
                    }),
                    _ => None,
                };
                // Growth happened between the two readings, so the newest kill is at most
                // `over` old: its age is reported from the previous reading, never as zero.
                let moved = if oom_kills > before.oom_kills {
                    Some(before.at)
                } else {
                    before.oom_moved
                };
                (now, cpu_usec, cpu, moved)
            }
            None => {
                let before = cpu_usec_at(dir);
                let t0 = Instant::now();
                std::thread::sleep(CPU_SAMPLE);
                let cpu_usec = cpu_usec_at(dir);
                let now = Instant::now();
                let cpu = match (before, cpu_usec) {
                    (Some(was), Some(is)) => Some(CpuBurn {
                        used: Duration::from_micros(is.saturating_sub(was)),
                        over: now.saturating_duration_since(t0),
                    }),
                    _ => None,
                };
                (now, cpu_usec, cpu, None)
            }
        };
        Holding {
            processes: pids.len(),
            top,
            leader_alive,
            cpu,
            oom_since: oom_moved.map(|m| now.saturating_duration_since(m)),
            seen: Seen {
                at: now,
                cpu_usec,
                oom_kills,
                oom_moved,
            },
        }
    }

    /// `75 processes — 73 v3rpc.test, 1 go, 1 timeout` - or `0 processes`, which is a cgroup that
    /// is empty and a leader nobody has to look for.
    pub fn processes_clause(&self) -> String {
        let mut s = if self.processes == 1 {
            "1 process".to_string()
        } else {
            format!("{} processes", self.processes)
        };
        if !self.top.is_empty() {
            s.push_str(" — ");
            let names: Vec<String> = self
                .top
                .iter()
                .map(|(name, n)| format!("{n} {name}"))
                .collect();
            s.push_str(&names.join(", "));
        }
        s
    }
}

/// The readings this process has taken, by scope directory: the other half of every delta, so a
/// listing says what a job did since the last one instead of what it has done in its life.
fn readings() -> &'static std::sync::Mutex<std::collections::HashMap<PathBuf, Holding>> {
    static READINGS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<PathBuf, Holding>>,
    > = std::sync::OnceLock::new();
    READINGS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// [`Holding::read`] for a listing, with the previous reading of this same scope as the delta's
/// other half. The map is keyed by the scope's directory, never by the job, so a job promotion
/// that moves processes does not inherit another job's totals.
pub fn holding_of(dir: &Path, pid: u32) -> Holding {
    let prev = {
        let held = readings().lock().unwrap_or_else(|e| e.into_inner());
        held.get(dir).cloned()
    };
    // Outside the lock: the first reading of a scope samples `cpu.stat` across a short sleep.
    let now = Holding::read(dir, pid, prev.as_ref());
    {
        let mut held = readings().lock().unwrap_or_else(|e| e.into_inner());
        if held.len() >= 256 {
            let stale = FORGET_AFTER;
            let at = now.seen.at;
            held.retain(|_, h| at.saturating_duration_since(h.seen.at) < stale);
        }
        held.insert(dir.to_path_buf(), now.clone());
    }
    now
}

/// The most numerous command names among `pids`, by `comm` - the kernel's own name for a process,
/// which is what `ps` would read as COMMAND and the shortest thing that says what a job is made of.
/// A pid whose name cannot be read is counted as `?`: it exited between the two reads, and a row
/// that dropped it would undercount the cgroup.
fn top_commands(pids: &[u32], proc_root: &Path) -> Vec<(String, usize)> {
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for pid in pids {
        let name = std::fs::read_to_string(proc_root.join(pid.to_string()).join("comm"))
            .map(|s| s.trim().to_string())
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "?".to_string());
        *counts.entry(name).or_default() += 1;
    }
    let mut rows: Vec<(String, usize)> = counts.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    rows.truncate(TOP_COMMANDS);
    rows
}

/// Bytes as a person reads them - `64 MiB`, `1.5 GiB`. Binary units, because `memory.max` is set
/// in them and a cap read back as `67.1 MB` would look like a different number.
pub fn human_bytes(b: u64) -> String {
    const K: f64 = 1024.0;
    let f = b as f64;
    if f >= K * K * K {
        let g = f / (K * K * K);
        if g.fract() == 0.0 {
            format!("{g:.0} GiB")
        } else {
            format!("{g:.1} GiB")
        }
    } else if f >= K * K {
        format!("{:.0} MiB", f / (K * K))
    } else if f >= K {
        format!("{:.0} KiB", f / K)
    } else {
        format!("{b} B")
    }
}

/// The default: there is no lifetime mechanism, and it says so. Not a tree that pretends to own
/// things: a mechanism that reports success while owning nothing is worse than an absent one,
/// because it reads as protection.
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

/// cgroup v2, rooted inside the harness's own cgroup. A delegated subtree is the only place an
/// unprivileged process may create cgroups, and if the harness itself is killed by something
/// above it, the whole subtree goes with it.
#[derive(Debug, Clone)]
pub struct Cgroup2 {
    root: PathBuf,
    open: std::sync::Arc<std::sync::Mutex<Vec<ScopeId>>>,
    /// Whether scopes under this root can be memory-capped, or why not. See [`delegate_memory`]:
    /// `Ok` only when the tree hangs off a delegated scope the daemon has stepped out of.
    memory: Result<(), String>,
}

/// Where a delegated cgroup v2 hierarchy is mounted.
const CGROUP_FS: &str = "/sys/fs/cgroup";

/// Distinguishes the roots of two trees in one process. See [`Cgroup2::probe_under`].
static TREE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl Cgroup2 {
    /// Find the harness's own cgroup, prove it is writable, and make a root under it. The proof is
    /// a real `mkdir`/`rmdir`, not a permission bit: on a delegated subtree the mode and the
    /// kernel's answer disagree in both directions, so guard the fact and not the proxy.
    pub fn probe() -> Result<Cgroup2, ExecError> {
        match delegate_memory() {
            Ok(base) => {
                let mut t = Self::probe_under(base)?;
                t.memory = Ok(());
                Ok(t)
            }
            Err(_) => Self::probe_under(&own_cgroup()?),
        }
    }

    /// The probe, against a chosen parent. Tests use this to build a tree that is a sibling of
    /// the harness's own rather than nested in it.
    pub fn probe_under(parent: &Path) -> Result<Cgroup2, ExecError> {
        if std::fs::metadata(CGROUP_FS).is_err() {
            return Err(ExecError::NoScopes(format!(
                "`{CGROUP_FS}` is not there, so this is not a cgroup v2 host"
            )));
        }
        // One root per tree, not one per process: a daemon runs several sessions in one process,
        // and a shared root means one session's housekeeping walks another's directories.
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
        // The kill file is the mechanism; a tree without it would fall back silently, which is the
        // thing this module exists to not do.
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
            memory: Err(match delegate_memory() {
                Err(why) => why.clone(),
                Ok(_) => {
                    "this tree was built under a chosen parent, not the daemon's delegated scope"
                        .into()
                }
            }),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `cgroup.procs`, for the shell wrapper to write its own pid into.
    pub fn procs_path(scope: &ScopeId) -> PathBuf {
        scope.path.join("cgroup.procs")
    }

    /// Whether this scope, or anything under it, holds a process. It reads `cgroup.events`, which
    /// the kernel maintains: there is no pattern, so no predicate can match its own waiter.
    pub fn populated(&self, scope: &ScopeId) -> bool {
        populated_at(&scope.path)
    }
}

/// Whether a cgroup directory, or anything under it, holds a process. `pub(crate)` because
/// [`super::monitor`] asks exactly this: the kernel's own answer cannot match its own waiter.
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
/// The leaf the daemon steps into, inside the scope the launcher started it in.
pub const DAEMON_LEAF: &str = "daemon";

/// Make this process's scope able to carry memory caps, once, or say why it cannot: step into a
/// leaf of the launcher's delegated scope and enable `memory` there, so the tree hangs beside the
/// daemon. Refused unless the daemon is alone in its cgroup, and done once - the move is final.
pub fn delegate_memory() -> &'static Result<PathBuf, String> {
    static DELEGATED: std::sync::OnceLock<Result<PathBuf, String>> = std::sync::OnceLock::new();
    DELEGATED.get_or_init(|| {
        let own = own_cgroup().map_err(|e| e.to_string())?;
        let me = std::process::id();
        let procs = std::fs::read_to_string(own.join("cgroup.procs")).map_err(|e| {
            format!(
                "`{}` could not be read: {e}",
                own.join("cgroup.procs").display()
            )
        })?;
        let others = procs
            .lines()
            .filter_map(|l| l.trim().parse::<u32>().ok())
            .filter(|p| *p != me)
            .count();
        if others > 0 {
            return Err(format!(
                "this daemon shares its cgroup `{}` with {others} other process(es) — it was not \
                 started in a scope of its own (the `letibot` launcher does that with \
                 `systemd-run --user --scope`), so tool calls run UNCAPPED",
                own.display()
            ));
        }
        let controllers =
            std::fs::read_to_string(own.join("cgroup.controllers")).unwrap_or_default();
        if !controllers.split_whitespace().any(|c| c == "memory") {
            return Err(format!(
                "the memory controller is not delegated to `{}` (it has: `{}`), so tool calls run \
                 UNCAPPED",
                own.display(),
                controllers.trim()
            ));
        }
        let leaf = own.join(DAEMON_LEAF);
        let step = std::fs::create_dir_all(&leaf)
            .and_then(|_| std::fs::write(leaf.join("cgroup.procs"), me.to_string()))
            .and_then(|_| std::fs::write(own.join("cgroup.subtree_control"), "+memory"));
        step.map_err(|e| {
            format!(
                "the daemon could not step into `{}` and enable `memory` on its scope ({e}), so \
                 tool calls run UNCAPPED",
                leaf.display()
            )
        })?;
        Ok(own)
    })
}

/// A cgroup of its own for work the daemon runs for itself, opened in the delegated scope beside
/// [`DAEMON_LEAF`] and the tool trees. A sibling, not a child, so a heavy step cannot throttle
/// every session the daemon serves. Dropping it kills what is left and removes the directory.
#[derive(Debug)]
pub struct SiblingScope {
    path: PathBuf,
}

impl SiblingScope {
    /// Open `<delegated scope>/<prefix>.<pid>.<n>` and cap it at `cap` bytes, swap forbidden (`None`
    /// sets no `memory.max`). `Err` names why there is no such scope; the caller then says uncapped.
    pub fn open(prefix: &str, cap: Option<u64>) -> Result<SiblingScope, String> {
        let base = delegate_memory().as_ref().map_err(Clone::clone)?;
        Self::open_under(base, prefix, cap)
    }

    /// [`SiblingScope::open`] under a chosen delegated base.
    pub fn open_under(base: &Path, prefix: &str, cap: Option<u64>) -> Result<SiblingScope, String> {
        let path = base.join(format!(
            "{}.{}.{}",
            sanitise(prefix),
            std::process::id(),
            TREE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path)
            .map_err(|e| format!("making the cgroup `{}` failed: {e}", path.display()))?;
        let scope = SiblingScope { path };
        if let Some(bytes) = cap {
            let write = |file: &str, value: String| {
                std::fs::write(scope.path.join(file), value).map_err(|e| {
                    format!("writing `{}` failed: {e}", scope.path.join(file).display())
                })
            };
            write("memory.max", bytes.to_string())?;
            if scope.path.join("memory.swap.max").exists() {
                write("memory.swap.max", "0".into())?;
            }
        }
        Ok(scope)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `cgroup.procs`, for [`join_script`]'s `$1`.
    pub fn procs_path(&self) -> PathBuf {
        self.path.join("cgroup.procs")
    }

    /// The live pids in it.
    pub fn members(&self) -> Vec<u32> {
        let mut out = Vec::new();
        members_at(&self.path, &mut out);
        out
    }

    /// `cgroup.kill` - every process in it, whatever group or session it put itself in.
    pub fn kill(&self) -> Result<(), String> {
        kill_tree(&self.path)
            .map_err(|e| format!("`cgroup.kill` in `{}`: {e}", self.path.display()))
    }

    /// What its memory did, read from the kernel - before [`SiblingScope::close`], which removes
    /// the files.
    pub fn memory_use(&self) -> Option<MemoryUse> {
        memory_use_at(&self.path)
    }

    /// Kill what is left, wait for the kernel to say empty, remove the directory, and say how many
    /// processes were still in it. Absence is read from `cgroup.events`, after the kill, not assumed.
    pub fn close(self) -> usize {
        self.reap()
    }

    fn reap(&self) -> usize {
        let left = self.members().len();
        if left > 0 {
            let _ = kill_tree(&self.path);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while populated_at(&self.path) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = std::fs::remove_dir(&self.path);
        left
    }
}

impl Drop for SiblingScope {
    fn drop(&mut self) {
        if self.path.exists() {
            self.reap();
        }
    }
}

/// What a scope's memory did, asked of the scope's directory with no tree instance: the job's
/// waiter thread holds the job and not the tree. `None` on a tree with no memory controller.
pub fn memory_use_of(scope: &ScopeId) -> Option<MemoryUse> {
    memory_use_at(&scope.path)
}

/// What a scope's processes have burned the CPU for, in microseconds, read from `cpu.stat`. The
/// one live signal that a job printing nothing is still working, which is what a dead check needs:
/// `None` is *this host cannot say*, never *zero*, and a caller must not end a job on it.
pub fn cpu_use_of(scope: &ScopeId) -> Option<u64> {
    cpu_usec_at(&scope.path)
}

/// `cpu.stat`'s `usage_usec` for one cgroup directory.
fn cpu_usec_at(dir: &Path) -> Option<u64> {
    let stat = std::fs::read_to_string(dir.join("cpu.stat")).ok()?;
    stat.lines()
        .find_map(|l| l.strip_prefix("usage_usec "))
        .and_then(|n| n.trim().parse().ok())
}

/// `memory.events`' `oom_kill`: how many of this cgroup's processes the kernel has killed at its
/// cap over the cgroup's whole life. Zero on a tree with no memory controller, which is the same
/// reading as a scope nothing has been killed in.
fn oom_kills_at(dir: &Path) -> u64 {
    std::fs::read_to_string(dir.join("memory.events"))
        .unwrap_or_default()
        .lines()
        .find_map(|l| l.strip_prefix("oom_kill "))
        .and_then(|n| n.trim().parse().ok())
        .unwrap_or(0)
}

/// [`MemoryUse`] for one cgroup directory. `None` when the files are not there - a tree without
/// the controller, or a scope already removed.
fn memory_use_at(dir: &Path) -> Option<MemoryUse> {
    let max = std::fs::read_to_string(dir.join("memory.max")).ok()?;
    let peak = std::fs::read_to_string(dir.join("memory.peak")).ok()?;
    let current = std::fs::read_to_string(dir.join("memory.current")).ok()?;
    Some(MemoryUse {
        cap: max.trim().parse().ok(),
        current: current.trim().parse().ok()?,
        peak: peak.trim().parse().ok()?,
        oom_kills: oom_kills_at(dir),
    })
}

fn own_cgroup() -> Result<PathBuf, ExecError> {
    let text = std::fs::read_to_string("/proc/self/cgroup")
        .map_err(|e| ExecError::NoScopes(format!("`/proc/self/cgroup` could not be read: {e}")))?;
    // cgroup v2 gives exactly one line, `0::<path>`. A v1-only host gives several and none of them
    // start `0::`, which is a fact worth reporting as itself.
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

/// Depth-first `rmdir`. A cgroup directory can only be removed when it is empty of processes and
/// of child cgroups.
fn remove_tree(dir: &Path) -> bool {
    // Retried for a bounded time: on a loaded box `rmdir` answers EBUSY for a moment after the
    // last member is reaped, and a child cgroup can appear after the first listing.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let mut ok = true;
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    ok &= remove_tree(&e.path());
                }
            }
        }
        match std::fs::remove_dir(dir) {
            Ok(()) if ok => return true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && ok => return true,
            _ if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => return false,
        }
    }
}

impl ScopeTree for Cgroup2 {
    fn describe(&self) -> String {
        format!(
            "cgroup v2, rooted at {} (lifetime only — this is NOT a sandbox)",
            self.root.display()
        )
    }

    fn root(&self) -> Option<PathBuf> {
        Some(self.root.clone())
    }

    fn open(
        &self,
        kind: ScopeKind,
        name: &str,
        parent: Option<&ScopeId>,
    ) -> Result<ScopeId, ExecError> {
        // An `explicit` scope under a session or a turn would be reaped by that scope's end, the
        // one thing it exists not to be, so it is placed at the root instead. The `p.kind` check
        // matters: without it a JOB inside an explicit scope also went to the root.
        let base = match parent {
            Some(p) if kind == ScopeKind::Explicit && p.kind != ScopeKind::Explicit => {
                self.root.clone()
            }
            Some(p) => p.path.clone(),
            None => self.root.clone(),
        };
        let dir = base.join(format!("{}.{}", kind.as_str(), sanitise(name)));
        // A capped child needs the controller in its parent's `subtree_control`, and only a cgroup
        // holding no process of its own may have one. Every directory a scope is opened under is
        // such a cgroup, since processes only ever join leaves. Written each time: idempotent.
        if self.memory.is_ok() {
            let _ = std::fs::write(base.join("cgroup.subtree_control"), "+memory");
        }
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

    fn memory_caps(&self) -> Result<(), String> {
        self.memory.clone()
    }

    fn cap_memory(&self, scope: &ScopeId, bytes: u64) -> Result<(), String> {
        self.memory.clone()?;
        let write = |file: &str, value: String| {
            std::fs::write(scope.path.join(file), value)
                .map_err(|e| format!("writing `{}` failed: {e}", scope.path.join(file).display()))
        };
        write("memory.max", bytes.to_string())?;
        // Absent on a kernel built without swap accounting, which is no swap to forbid.
        if scope.path.join("memory.swap.max").exists() {
            write("memory.swap.max", "0".into())?;
        }
        Ok(())
    }

    fn memory_use(&self, scope: &ScopeId) -> Option<MemoryUse> {
        memory_use_at(&scope.path)
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

    /// Presence, then the kill, then absence - in that order, and all three in the record.
    fn end(&self, scope: &ScopeId) -> Reaping {
        let started = Instant::now();
        let at = SystemTime::now();

        // 1. PRESENCE. Read the members and what they are, while they exist to be read. This is
        //    the denominator; without it a later `0 survivors` says nothing about whether
        //    anything was ever there.
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
                    // A fallback that is not visible is a fallback nobody knows they are running.
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

        // 3. ABSENCE, and only now. `cgroup.kill` returns before the processes are actually
        //    reaped, so a check that ran immediately would report a clean kill of a cgroup still
        //    holding three processes.
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

    /// Presence, then the move, then presence again. `ESRCH` means the process exited, which is not
    /// a failed migration, and a process can fork mid-move into the old cgroup, so the loop repeats.
    /// Membership is read back from the kernel, never counted from writes that returned `Ok`.
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
                    // ESRCH is "it exited", which is not a failure to migrate. Anything else is
                    // worth a sentence, and the LAST one is kept: a first error from a pid that
                    // then exited names the least interesting of them.
                    if e.raw_os_error() != Some(3) {
                        last_err = Some(format!("pid {pid}: {e}"));
                    }
                }
            }
            if Instant::now() >= deadline || passes >= 8 {
                break;
            }
        }

        // 3. ABSENCE, and only now - read out of the kernel rather than inferred from how many
        //    writes returned Ok.
        let mut left_behind = Vec::new();
        members_at(&from.path, &mut left_behind);
        left_behind.sort_unstable();
        left_behind.dedup();
        let mut moved = Vec::new();
        members_at(&to.path, &mut moved);
        moved.sort_unstable();
        moved.dedup();
        // Only the ones this migration is about. The target scope may already have held processes
        // of its own, and counting those as "moved" would inflate the number with somebody else's work.
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

    /// The cgroup belongs to the host that opened it, so listing it is the whole act. `prune` reads
    /// the filesystem, so a remembered scope still goes only when it holds nothing.
    fn remember(&self, scope: &ScopeId) {
        let mut open = self.open.lock().expect("scope list");
        if !open.contains(scope) {
            open.push(scope.clone());
        }
    }

    fn prune(&self) -> usize {
        let mut n = 0;
        prune_at(&self.root, &mut n);
        // The root itself, last and only if everything under it went. `remove_dir` on a populated
        // cgroup fails, and a live host always holds its session under it, so no grace is needed.
        if std::fs::remove_dir(&self.root).is_ok() {
            n += 1;
        }
        n
    }
}

/// How recently a scope must have changed for a prune to leave it. The tree is shared by every
/// host in a process, and a scope another host has just made holds no process until its job's
/// directory and process join it; removing it then failed that spawn with ENOENT.
const PRUNE_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

/// Whether `dir` changed within [`PRUNE_GRACE`]; an unreadable time is fresh, so nothing is
/// removed on a guess.
fn fresh(dir: &Path) -> bool {
    std::fs::metadata(dir)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_none_or(|age| age < PRUNE_GRACE)
}

/// Depth-first: children first, then this one if it holds no process and is not [`fresh`].
fn prune_at(dir: &Path, n: &mut usize) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                let child = e.path();
                prune_at(&child, n);
                let mut pids = Vec::new();
                members_at(&child, &mut pids);
                if pids.is_empty() && !fresh(&child) && std::fs::remove_dir(&child).is_ok() {
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
/// What a reap on this host's own tree is recorded as - the name [`host_tree`]'s kill goes by,
/// for a test or a reader that asserts on it.
pub const HOST_KILL: &str = if cfg!(target_os = "macos") {
    PGROUP_KILL
} else {
    CGROUP_KILL
};

/// The lifetime mechanism this host has: cgroup v2 on Linux, process groups on macOS. Every
/// production caller asks this, so the choice is made once.
pub fn host_tree() -> Result<Box<dyn ScopeTree>, ExecError> {
    #[cfg(target_os = "macos")]
    return Ok(Box::new(ProcessGroups::probe()?));
    #[cfg(not(target_os = "macos"))]
    return Ok(Box::new(Cgroup2::probe()?));
}

/// The live processes in a scope and every scope under it, asked the way this host's tree asks
/// it. On Linux that is `cgroup.procs`; on macOS the file holds recorded group ids that outlive
/// the kill, so only a read that asks which groups still have a member is the membership.
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

/// Whether a scope directory, or anything under it, holds a live process - the macOS reading of
/// the same question, from the recorded process groups.
#[cfg(target_os = "macos")]
pub(crate) fn populated_at(dir: &Path) -> bool {
    let mut pids = Vec::new();
    pg_members_at(dir, &mut pids);
    !pids.is_empty()
}

/// Process groups, on macOS - the same three scopes with a weaker floor. The tree keeps the
/// cgroup layout (a directory per scope, a `cgroup.procs` of group ids) and membership is read
/// from the kernel, but a process that calls `setsid` leaves its scope, and group ids are pids.
#[cfg(target_os = "macos")]
#[derive(Debug, Clone)]
pub struct ProcessGroups {
    root: PathBuf,
    open: std::sync::Arc<std::sync::Mutex<Vec<ScopeId>>>,
}

#[cfg(target_os = "macos")]
impl ProcessGroups {
    /// Make a root under the temp dir (`$TMPDIR`, per user on macOS). Fails only if it cannot be
    /// created, and says where.
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

/// Roots whose daemon is gone and which hold nothing, removed before a new one is made. A
/// directory in the temp dir outlives the process that made it, and the owner's pid is in the
/// name, so a root is debris exactly when that process is dead and no recorded group has a member.
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

/// The live processes in the groups a scope recorded, and the forgetting half: a recorded group
/// found empty is struck from its file, so its number is not held after the kernel is free to hand
/// it to somebody else.
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

/// Is this recorded group still the one that was observed? A group id is a pid, and a number the
/// kernel has handed on is not ours to signal: at least one current member has to be a process
/// this scope listed, still running what it was running.
#[cfg(target_os = "macos")]
fn still_ours(group: u32, observed: &[Reaped]) -> bool {
    super::darwin::members_of_groups(&[group])
        .into_iter()
        .filter_map(super::darwin::info)
        .any(|i| observed.iter().any(|o| o.pid == i.pid && o.comm == i.comm))
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

    fn root(&self) -> Option<PathBuf> {
        Some(self.root.clone())
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

    /// Presence, then `killpg`, then absence - the same three-part record.
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

        // Repeated, because a member can fork between one `killpg` and its exit; the child is in
        // the same group and the next pass reaches it.
        let deadline = Instant::now() + Duration::from_secs(5);
        // `pg_members` FIRST: it strikes a recorded group the kernel no longer shows a member for,
        // and a group id whose group has gone is a number the kernel may hand to somebody else.
        let _ = pg_members(&scope.path);
        let mut groups = Vec::new();
        pg_recorded_at(&scope.path, &mut groups);
        loop {
            for g in &groups {
                // And again, per pass: a group that has died since the read is not ours to signal.
                if !still_ours(*g, &observed) {
                    continue;
                }
                unsafe { libc::killpg(*g as libc::pid_t, libc::SIGKILL) };
            }
            if pg_members(&scope.path).is_empty() || Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let survivors = pg_members(&scope.path);

        let removed = survivors.is_empty() && std::fs::remove_dir_all(&scope.path).is_ok();
        let mut note = (!removed && survivors.is_empty())
            .then(|| "every process is gone but the scope directory would not be removed".into());
        // Said on every group reaping, not only when something is left: a group kill cannot reach
        // a process that called `setsid`, and the count above is of the scope, not of the machine.
        if mechanism == PGROUP_KILL {
            let escaped = "a process that called setsid or setpgid left this scope and a group kill \
                           cannot reach it, so this record does not count one";
            note = Some(match note {
                Some(n) => format!("{n}; {escaped}"),
                None => escaped.to_string(),
            });
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

    /// Moving a group is moving its number: the recorded ids go from `from`'s files to `to`'s.
    /// Membership is then read back from the kernel, as for cgroups.
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

    /// A job leads its own group, so its pid is the group id. The leader may be gone while
    /// children live on in the group: the kernel cannot reuse a pid that names a live group, so
    /// "no process has this pid and the group has members" is still the original group.
    fn claim(&self, scope: &ScopeId, pid: u32, identity: &str) -> usize {
        let recorded = pg_members(&scope.path);
        if !recorded.is_empty() {
            return recorded.len();
        }
        let leader_ok = match super::darwin::info(pid) {
            Some(i) => !identity.is_empty() && identity == format!("start:{}", i.start_us),
            None => true,
        };
        if !leader_ok || super::darwin::members_of_groups(&[pid]).is_empty() {
            return 0;
        }
        let _ = std::fs::create_dir_all(&scope.path);
        let _ = std::fs::write(scope.path.join("cgroup.procs"), format!("{pid}\n"));
        pg_members(&scope.path).len()
    }

    /// [`Cgroup2::remember`]'s reason: the group is already recorded where the giver wrote it.
    fn remember(&self, scope: &ScopeId) {
        let mut open = self.open.lock().expect("scope list");
        if !open.contains(scope) {
            open.push(scope.clone());
        }
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

/// A cgroup directory name is a filename: anything not obviously safe in one becomes `_`, and
/// the original stays in [`ScopeId::name`], so the mapping is lossy on disk and lossless in the
/// record.
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

/// The `sh` script that puts a process in its cgroup before it becomes the command: `$1` is the
/// scope's `cgroup.procs`, `$2` a path the HOST owns and touched only on the branch that joined,
/// and `|| exit 125` is the fail-closed half, since a process that could not be owned is not run.
pub fn join_script() -> &'static str {
    "echo $$ > \"$1\" || { echo 'letibot: could not join the scope cgroup; the command was NOT run' >&2; exit 125; }; : > \"$2\"; shift 2; exec \"$@\""
}

/// What the wrapper says when it could not own the process, verbatim. The classification of a
/// launcher failure rests on this string and not on the exit code (125 is legitimate), and it is
/// a `const` so the script and the reader cannot drift apart.
pub const NOT_SCOPED_MARKER: &str =
    "letibot: could not join the scope cgroup; the command was NOT run";

/// The exit code [`join_script`] uses when a process could not join its scope. Not the evidence
/// (see [`NOT_SCOPED_MARKER`]): kept because the wrapper's failure should not look like the
/// command's success, and 125 is a code a wrapped command is unlikely to choose.
pub const EXIT_NOT_SCOPED: i32 = 125;

#[cfg(test)]
mod tests {
    use super::*;

    /// A scope made moments ago holds no process until its job joins it, and the tree is shared by
    /// every host in a process: a prune leaves it, and takes an empty one that has been quiet.
    #[test]
    fn a_prune_leaves_a_scope_made_moments_ago() {
        let root = std::env::temp_dir().join(format!("letibot-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (young, old) = (root.join("young"), root.join("old"));
        std::fs::create_dir_all(&young).unwrap();
        std::fs::create_dir_all(&old).unwrap();
        let back = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
        std::fs::File::open(&old)
            .unwrap()
            .set_modified(back)
            .unwrap();
        let mut n = 0;
        prune_at(&root, &mut n);
        assert!(young.is_dir(), "a scope made moments ago was pruned");
        assert!(!old.exists(), "an empty scope that has been quiet was kept");
        assert_eq!(n, 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Memory is said on every settled run, and a kill or a run near its cap is said as such.
    #[test]
    fn memory_is_said_at_a_kill_and_near_the_cap_and_otherwise_as_a_peak() {
        let at = |peak: u64, oom_kills: u64| MemoryUse {
            cap: Some(100 << 20),
            current: peak,
            peak,
            oom_kills,
        };
        assert!(!at(50 << 20, 0).killed_at_cap());
        assert!(!at(89 << 20, 0).near_its_cap(), "89% is not near the cap");
        assert!(at(90 << 20, 0).near_its_cap(), "within a tenth of it is");
        assert!(at(10 << 20, 1).killed_at_cap(), "a kill is always said");
        assert_eq!(at(50 << 20, 0).clause(), "peak 50 MiB of a 100 MiB cap");
    }

    /// `memory.events` as the kernel writes it, read for `oom_kill` and nothing else - not
    /// `oom_group_kill`, not `oom` - and `memory.max`'s `max` is an uncapped scope.
    #[test]
    fn memory_events_are_read_for_oom_kill_alone() {
        let dir = std::env::temp_dir().join(format!("letibot-memevents-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cap: u64 = 26_735_965_388;
        let peak: u64 = 25_877_001_011;
        let current: u64 = 17_000_000_000;
        std::fs::write(dir.join("memory.max"), format!("{cap}\n")).unwrap();
        std::fs::write(dir.join("memory.peak"), format!("{peak}\n")).unwrap();
        std::fs::write(dir.join("memory.current"), format!("{current}\n")).unwrap();
        let events = |oom: u64, group: u64, kill: u64| {
            format!(
                "low 0\nhigh 0\nmax 12\noom {oom}\noom_kill {kill}\noom_group_kill {group}\n\
                 sock_throttled 0\n"
            )
        };
        std::fs::write(dir.join("memory.events"), events(2, 5, 0)).unwrap();
        let u = memory_use_at(&dir).unwrap();
        assert_eq!(
            u,
            MemoryUse {
                cap: Some(cap),
                current,
                peak,
                oom_kills: 0
            }
        );
        assert!(
            !u.killed_at_cap(),
            "`oom` and `oom_group_kill` are not `oom_kill`"
        );
        assert_eq!(u.clause(), "peak 24.1 GiB of a 24.9 GiB cap");
        std::fs::write(dir.join("memory.events"), events(1, 0, 3)).unwrap();
        assert_eq!(memory_use_at(&dir).unwrap().oom_kills, 3);
        // No events file (a tree without the controller's counters): no kill, not no reading.
        std::fs::remove_file(dir.join("memory.events")).unwrap();
        assert_eq!(memory_use_at(&dir).unwrap().oom_kills, 0);
        std::fs::write(dir.join("memory.max"), "max\n").unwrap();
        assert_eq!(memory_use_at(&dir).unwrap().cap, None, "`max` is uncapped");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The script and the marker are one string: edit either alone and this fails, which is what
    /// keeps the sentence a launcher failure is classified by from drifting away from the wrapper.
    #[test]
    fn the_script_and_the_marker_are_one_string() {
        assert!(
            join_script().contains(NOT_SCOPED_MARKER),
            "the wrapper no longer says what `launcher_failed` looks for:\n{}",
            join_script()
        );
        // And the marker is not something a successful run could produce by accident: the script
        // writes it only on the branch that also exits `EXIT_NOT_SCOPED`.
        assert!(join_script().contains(&format!("exit {EXIT_NOT_SCOPED}")));
        assert_eq!(
            join_script().matches(NOT_SCOPED_MARKER).count(),
            1,
            "the marker appears once, on the failure branch"
        );
        // And the join token is written on that branch and no other: `: > "$2"` sits after the
        // `||` group, so a wrapper that could not join never reaches it. Asserted by position
        // rather than by count, because a second occurrence later would still count as one string.
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

    /// A command that exits 125 is a command that ran. The state machine is in `host.rs`; this
    /// pins the two constants apart so neither can be mistaken for the other's evidence.
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

    /// The macOS tree, end to end through the real wrapper: a job that backgrounds a grandchild and
    /// returns is still reaped whole, and a promotion moves it so the old scope's end no longer
    /// touches it.
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

    /// A root left by a process that is gone, holding nothing, is swept by the next probe; one
    /// whose owner is alive is not touched.
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

    /// A group kill cannot reach a process that called `setsid`, so the record of one says so. The
    /// count that follows is of the scope, and a reader who took it for the machine would be told
    /// nothing is left when something may be.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_group_reaping_says_a_setsid_process_can_have_left_the_scope() {
        use std::os::unix::process::CommandExt;
        let Some(_) = letibot_tokencore::apparatus::present(
            "a process-lifetime tree (cgroup v2; process groups on macOS)",
            super::ProcessGroups::probe().is_ok(),
        ) else {
            return;
        };
        let tree = super::ProcessGroups::probe().expect("a tree");
        let scope = tree
            .open(ScopeKind::Session, "setsid-note", None)
            .expect("a scope");
        // A live member of the scope: this test's own child, in a group of its own so the kill
        // reaches it and not this process.
        let child = std::process::Command::new("/bin/sh")
            .args(["-c", "sleep 60"])
            .process_group(0)
            .spawn()
            .expect("a child in its own group");
        let pgid = child.id();
        std::fs::write(scope.path.join("cgroup.procs"), format!("{pgid}\n"))
            .expect("a recorded group");
        let reaping = tree.end(&scope);
        let _ = std::process::Command::new("kill")
            .args(["-KILL", &format!("-{pgid}")])
            .output();
        assert!(
            reaping
                .note
                .as_deref()
                .is_some_and(|n| n.contains("setsid")),
            "the record has to say what a group kill cannot reach: {reaping:?}"
        );
    }
}
