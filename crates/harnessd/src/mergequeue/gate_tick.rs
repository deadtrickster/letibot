//! The gate's tick, in one direction: the daemon pokes a step it started, the step's own launcher
//! watches for the pokes, and a step that stops being poked ends itself. The far side never calls
//! back, so a machine with no egress is watched exactly as one on the same box - and nothing is
//! written down for a daemon that comes later, because there is nothing to reap.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// How often the daemon pokes a running step. This is the gate's liveness, and it replaces the
/// poll that used to ask: a step's output arrives on the same round.
pub const TICK: Duration = Duration::from_secs(5);

/// How many pokes a step may miss before it is down - Erlang counts `net_ticktime` this way, and
/// four rather than one because a single late tick is a machine that is busy, not one that is gone.
pub const MISSES: u32 = 4;

/// How long the group gets after the watchdog's `SIGTERM` before `SIGKILL`, in tenths of a second:
/// the same five seconds the daemon's own `kill` gives a step.
const GRACE_TENTHS: u32 = 50;

/// How often a running step is poked, and how many misses make it down. A value rather than two
/// constants so a test can watch a real step for a tenth of a second instead of a real 20 s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tick {
    pub every: Duration,
    pub miss: u32,
}

impl Default for Tick {
    fn default() -> Tick {
        Tick {
            every: TICK,
            miss: MISSES,
        }
    }
}

impl Tick {
    pub const fn new(every: Duration, miss: u32) -> Tick {
        Tick { every, miss }
    }

    /// The window a silent daemon is given: what a reader is told, and the number
    /// [`crate::mergequeue::gate_host::POLL_BOUND`] has to fit inside.
    pub fn window(&self) -> Duration {
        self.every * self.miss
    }

    /// The tick as a word for `sleep`: `5` when it is whole seconds, `0.200` when it is not.
    fn seconds(&self) -> String {
        if self.every.subsec_nanos() == 0 {
            self.every.as_secs().to_string()
        } else {
            format!("{:.3}", self.every.as_secs_f64())
        }
    }
}

/// The beat file beside another of a step's files: `run/step-3.pid` reads its pokes in
/// `run/step-3.beat`, so everything one step keeps is still one name apart.
pub fn beat_beside(what: &Path) -> PathBuf {
    what.with_extension("beat")
}

/// The daemon's poke, where both ends are in this process's reach: the local gate's step. A counter
/// and not an instant, so the two sides compare words rather than clocks, and written through a
/// rename so a watchdog reading it mid-poke sees the old number rather than half of a new one.
pub fn poke(beat: &Path) -> std::io::Result<()> {
    let was = std::fs::read_to_string(beat).unwrap_or_default();
    let n: u64 = was.trim().parse().unwrap_or(0);
    let new = PathBuf::from(format!("{}.new", beat.display()));
    std::fs::write(&new, format!("{}\n", n + 1))?;
    std::fs::rename(&new, beat)
}

/// The poke as text for the far side of an ssh call: the poll that asks how a step is doing is what
/// pokes it, so one round trip both fetches the output and says the daemon is here.
pub fn poke_script(beat: &str) -> String {
    let (beat, new) = (quote(beat), quote(&format!("{beat}.new")));
    format!(
        "b=$(cat {beat} 2>/dev/null); case \"$b\" in ''|*[!0-9]*) b=0;; esac; b=$((b + 1)); \
         printf '%s\\n' \"$b\" > {new} && mv -f {new} {beat}"
    )
}

/// The watchdog, as launcher text: a subshell started beside the step, which ends the step's whole
/// process group when the pokes stop. `kill 0` is that group - the one the watcher was forked into,
/// so it reaches every process the step forked whether or not the host has `setsid` (the local gate
/// puts the launcher in a group of its own, and a `nohup` host has only the ssh session's). `$$` is
/// the launcher's own pid and is what says the step is over: a leader that is gone is nothing to
/// watch, and killing a group after that pid is handed out again would be somebody else's step.
pub fn watchdog_script(beat: &str, tick: &Tick) -> String {
    let beat = quote(beat);
    let (every, miss, grace) = (tick.seconds(), tick.miss, GRACE_TENTHS);
    format!(
        "( trap '' TERM\n\
         seen=$(cat {beat} 2>/dev/null)\n\
         missed=0\n\
         while kill -0 $$ 2>/dev/null; do\n\
         \tsleep {every}\n\
         \tnow=$(cat {beat} 2>/dev/null)\n\
         \tif [ -n \"$now\" ] && [ \"$now\" != \"$seen\" ]; then seen=$now; missed=0; \
         else missed=$((missed + 1)); fi\n\
         \tif [ \"$missed\" -ge {miss} ]; then\n\
         \t\tkill -0 $$ 2>/dev/null || exit 0\n\
         \t\tkill -TERM 0 2>/dev/null\n\
         \t\ti=0; while [ \"$i\" -lt {grace} ] && kill -0 $$ 2>/dev/null; do \
         sleep 0.1; i=$((i + 1)); done\n\
         \t\tkill -KILL 0 2>/dev/null\n\
         \t\texit 0\n\
         \tfi\n\
         done\n\
         ) >/dev/null 2>&1"
    )
}

/// A step's command with the watchdog in front of it: the one shell line the local gate spawns, so
/// the watchdog is part of the launcher here as it is on a declared host.
pub fn watched(command: &str, beat: &Path, tick: &Tick) -> String {
    let wd = watchdog_script(&beat.display().to_string(), tick);
    format!("{wd} &\n{command}")
}

/// One word, single-quoted for a shell: `'it'\''s'`. The launcher's own text is never quoted, but
/// the paths around it are this daemon's and a worktree's name is not this daemon's to trust.
pub fn quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
}

/// What "the step left nothing behind" means when it is measured rather than said: the pids still in
/// a process group. A leader's own pid is gone the moment it is killed, so the group is the fact.
#[cfg(test)]
pub(crate) mod probe {
    use std::time::{Duration, Instant};

    pub(crate) fn in_group(pgid: u32) -> Vec<u32> {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
            .filter(|pid| pgrp_of(*pid) == Some(pgid))
            .collect()
    }

    fn pgrp_of(pid: u32) -> Option<u32> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let rest = &stat[stat.rfind(')')? + 1..];
        let mut f = rest.split_whitespace();
        // After `)`: state ppid pgrp. A zombie is not a process the step left running.
        if f.next()? == "Z" {
            return None;
        }
        f.next()?;
        f.next()?.parse().ok()
    }

    /// Wait for the group to empty, and say whether it did.
    pub(crate) fn gone_within(pgid: u32, within: Duration) -> bool {
        let until = Instant::now() + within;
        while Instant::now() < until {
            if in_group(pgid).is_empty() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }
}

#[cfg(test)]
mod tests;
