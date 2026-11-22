//! A VM boot's admission: how many `firecode up`s run at once on this box, and how long one is
//! given. What grows with load is the host-side preparation (the config drive, packing the copy),
//! and two preparations at once compound, so boots queue for a slot and the deadline scales.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The deadline on `firecode up` at or under [`KNEE_PER_CORE`].
pub const BASE_UP: Duration = Duration::from_secs(600);
/// The most `firecode up` is ever given. firecode's own wait gives up near 14 minutes anyway.
pub const MAX_UP: Duration = Duration::from_secs(1800);
/// The load per core past which a deadline grows in proportion to it.
pub const KNEE_PER_CORE: f64 = 4.0;
/// Boots that prepare at once on one box, unless `LETIBOT_VM_BOOT_SLOTS` says otherwise.
pub const DEFAULT_SLOTS: usize = 2;
/// How long a boot waits for a slot before it is refused.
pub const DEFAULT_QUEUE_BUDGET: Duration = Duration::from_secs(30 * 60);

/// The box's load when a boot was admitted.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LoadSample {
    pub load1: f64,
    pub load5: f64,
    pub cores: usize,
}

impl LoadSample {
    /// `/proc/loadavg` and the cores this process may use, else the same two numbers off `sysctl`:
    /// a Mac has no `/proc`, and `sysctl -n vm.loadavg` answers `{ 1.23 4.56 7.89 }`.
    pub fn read() -> Option<LoadSample> {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        if let Ok(text) = std::fs::read_to_string("/proc/loadavg") {
            return Self::parse(&text, cores);
        }
        let out = std::process::Command::new("sysctl")
            .args(["-n", "vm.loadavg"])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let text = text.trim().trim_start_matches('{').trim_end_matches('}');
        Self::parse(text.trim(), cores)
    }

    pub fn parse(loadavg: &str, cores: usize) -> Option<LoadSample> {
        let mut it = loadavg.split_whitespace();
        let load1 = it.next()?.parse().ok()?;
        let load5 = it.next()?.parse().ok()?;
        Some(LoadSample {
            load1,
            load5,
            cores: cores.max(1),
        })
    }

    /// The larger of the 1- and 5-minute loads: a boot lasts minutes, so a dip is not room.
    pub fn load(&self) -> f64 {
        self.load1.max(self.load5)
    }

    pub fn per_core(&self) -> f64 {
        self.load() / self.cores as f64
    }

    pub fn said(&self) -> String {
        format!("load {:.0} on {} cores", self.load(), self.cores)
    }
}

/// `base`, grown in proportion to load per core past [`KNEE_PER_CORE`], capped at `cap`.
pub fn scaled(base: Duration, load: Option<&LoadSample>, cap: Duration) -> Duration {
    let factor = load
        .map(|l| l.per_core() / KNEE_PER_CORE)
        .filter(|f| f.is_finite())
        .unwrap_or(1.0)
        .max(1.0);
    base.mul_f64(factor).min(cap.max(base))
}

/// The deadline `firecode up` gets at this load.
pub fn up_deadline(load: Option<&LoadSample>) -> Duration {
    scaled(BASE_UP, load, MAX_UP)
}

/// A counting semaphore across processes: `slots` lock files in `dir`, each held with an
/// exclusive file lock for the length of one boot. A process that dies releases its slot.
#[derive(Debug, Clone)]
pub struct BootQueue {
    pub dir: PathBuf,
    pub slots: usize,
    pub budget: Duration,
    pub poll: Duration,
}

impl BootQueue {
    /// Every daemon of this user shares one queue: `$XDG_RUNTIME_DIR/letibot/vm-boot`.
    pub fn for_box() -> BootQueue {
        let base = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(std::env::temp_dir);
        let slots = std::env::var("LETIBOT_VM_BOOT_SLOTS")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .filter(|n: &usize| *n > 0)
            .unwrap_or(DEFAULT_SLOTS);
        BootQueue {
            dir: base.join("letibot").join("vm-boot"),
            slots,
            budget: DEFAULT_QUEUE_BUDGET,
            poll: Duration::from_secs(1),
        }
    }

    /// Wait for a free slot, at most [`Self::budget`]. A refusal names the load and who holds
    /// every slot, so a caller that starts the work on the host instead can say why.
    pub fn admit(
        &self,
        owner: &str,
        load: &dyn Fn() -> Option<LoadSample>,
    ) -> Result<Admitted, String> {
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| format!("the VM boot queue at {}: {e}", self.dir.display()))?;
        let started = Instant::now();
        loop {
            for i in 0..self.slots.max(1) {
                let path = self.dir.join(format!("slot-{i}"));
                if let Some(file) = take(&path, owner) {
                    let waited = started.elapsed();
                    return Ok(Admitted::new(Some(file), waited, self.slots, load()));
                }
            }
            let waited = started.elapsed();
            if waited >= self.budget {
                let holders = self.holders();
                let load = load()
                    .map(|l| l.said())
                    .unwrap_or_else(|| "load not measured".into());
                return Err(format!(
                    "no VM boot slot came free in {}: {} of {} boots on this box are still \
                     preparing ({}), at {load}. Nothing was placed; a host session instead runs \
                     without the VM boundary",
                    human(waited),
                    holders.len(),
                    self.slots,
                    if holders.is_empty() {
                        "holders unknown".to_string()
                    } else {
                        holders.join(", ")
                    },
                ));
            }
            std::thread::sleep(self.poll.min(self.budget - waited));
        }
    }

    /// Who holds each taken slot and for how long, from what [`take`] wrote into it.
    pub fn holders(&self) -> Vec<String> {
        let now = epoch_ms();
        (0..self.slots.max(1))
            .filter_map(|i| {
                let path = self.dir.join(format!("slot-{i}"));
                let probe = File::open(&path).ok()?;
                if probe.try_lock().is_ok() {
                    return None;
                }
                let text = std::fs::read_to_string(&path).ok()?;
                let (who, since) = text.trim().rsplit_once(' ')?;
                let since: u64 = since.parse().ok()?;
                let held = Duration::from_millis(now.saturating_sub(since));
                Some(format!("{who} for {}", human(held)))
            })
            .collect()
    }
}

fn take(path: &Path, owner: &str) -> Option<File> {
    let mut f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .ok()?;
    f.try_lock().ok()?;
    let _ = f.set_len(0);
    let _ = write!(f, "{owner} {}", epoch_ms());
    Some(f)
}

fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// `2m 05s`, `40s`.
pub fn human(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 60 {
        format!("{}m {:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

/// A boot that may start: the slot it holds until it is dropped, and the deadlines it gets.
#[derive(Debug)]
pub struct Admitted {
    _slot: Option<File>,
    pub waited: Duration,
    pub slots: usize,
    pub load: Option<LoadSample>,
    /// The deadline on `firecode up`.
    pub up: Duration,
    /// The deadline on the first command in the new guest.
    pub probe: Duration,
}

impl Admitted {
    fn new(slot: Option<File>, waited: Duration, slots: usize, load: Option<LoadSample>) -> Self {
        Admitted {
            _slot: slot,
            waited,
            slots,
            up: up_deadline(load.as_ref()),
            probe: scaled(super::GUEST_TIMEOUT, load.as_ref(), MAX_UP),
            load,
        }
    }

    /// A boot that takes no slot: the deadlines still follow the load.
    pub fn unqueued(load: Option<LoadSample>) -> Self {
        Admitted::new(None, Duration::ZERO, 0, load)
    }

    /// The sentence a session shows when its boot waited or was given more than the base
    /// deadline. `None` for an ordinary boot.
    pub fn note(&self) -> Option<String> {
        let load = self
            .load
            .map(|l| l.said())
            .unwrap_or_else(|| "load not measured".into());
        let waited = self.waited >= Duration::from_secs(1);
        let longer = self.up > BASE_UP;
        if !waited && !longer {
            return None;
        }
        let mut s = if waited {
            format!(
                "the VM boot waited {} for one of {} boot slots, at {load}",
                human(self.waited),
                self.slots
            )
        } else {
            format!("the VM boot started at {load}")
        };
        if longer {
            s.push_str(&format!(
                "; `firecode up` was given {} s",
                self.up.as_secs()
            ));
        }
        Some(s)
    }

    /// What a deadline refusal adds: the load and the deadline it was given.
    pub fn said(&self) -> String {
        let load = self
            .load
            .map(|l| l.said())
            .unwrap_or_else(|| "load not measured".into());
        if self.waited >= Duration::from_secs(1) {
            format!("{load}, after {} in the boot queue", human(self.waited))
        } else {
            load
        }
    }
}

#[cfg(test)]
mod tests;
