//! The user manager's own table, and the two bounds G5 puts on it (`docs/lifecycle.md` d.7):
//! how many units it holds against its limit, which failed `letibot-*` units are ours to reset,
//! and `letibot.slice`'s `MemoryHigh`.
//!
//! Every call is bounded (`CALL_BOUND_SECONDS`): R192 measured a manager pegged at 100% CPU, and a
//! sweep that blocks on one never runs. The door is [`UnitManager`], so a test drives the decisions
//! without a manager and nothing in a test reaches the operator's.

use std::path::PathBuf;
use std::process::Command;

/// `MANAGER_MAX_NAMES` in systemd's `manager.h`: reaching it is the `E2BIG`/"Argument list too
/// long" that made every new daemon come up without a scope (R191, R192).
pub const UNIT_LIMIT: usize = 131_072;

/// How long any one `systemctl` call may take. A manager that is pegged answers nothing, and the
/// reaper's own timer is what stops running while it waits.
pub const CALL_BOUND_SECONDS: u64 = 10;

/// The slice every daemon runs under, and so the one unit `MemoryHigh` belongs to.
pub const SLICE: &str = "letibot.slice";

/// The prefix that makes a failed unit ours to reset. Only these: a failed unit of another name is
/// somebody else's, and resetting it would erase the record of a crash this box did not cause.
pub const OURS: &str = "letibot-";

/// What the user manager holds, as the plan prints it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnitTable {
    /// Units the manager holds right now, against [`UNIT_LIMIT`].
    pub units: usize,
    /// The failed ones named `letibot-*`: this box's to reset, and nobody else's business.
    pub ours_failed: Vec<String>,
    /// Failed units that are not ours. Counted, never touched.
    pub other_failed: usize,
}

impl UnitTable {
    /// The plan's one line about the manager.
    pub fn line(&self) -> String {
        let failed = self.ours_failed.len() + self.other_failed;
        let tail = if failed == 0 {
            "; nothing failed".to_string()
        } else {
            format!(
                "; {failed} failed, {} of them letibot's and reset by `gc --now`",
                self.ours_failed.len()
            )
        };
        format!("user manager: {} units of {UNIT_LIMIT}{tail}", self.units)
    }
}

/// What `systemctl --user list-units --all --no-legend --plain` answers, read for two facts: how
/// many units the manager holds, and which failed ones are ours. Pure, so both are pinned without
/// a manager on the box.
pub fn parse_table(text: &str) -> UnitTable {
    let mut out = UnitTable::default();
    for line in text.lines() {
        let line = line.trim_start_matches(['●', '*', ' ']);
        let mut cols = line.split_whitespace();
        let Some(name) = cols.next() else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        out.units += 1;
        // The columns are `NAME LOAD ACTIVE SUB DESCRIPTION…`: `failed` is the SUB of a unit that
        // gave up, and it is what `reset-failed` clears.
        let failed = cols.clone().take(2).any(|c| c == "failed");
        if !failed {
            continue;
        }
        if name.starts_with(OURS) {
            out.ours_failed.push(name.to_string());
        } else {
            out.other_failed += 1;
        }
    }
    out
}

/// Every question this pass asks of the user manager. A trait because a test must not reach the
/// operator's manager, and because a box with none answers every one of them with the reason.
pub trait UnitManager: Send + Sync {
    /// The table, or why this box could not be asked.
    fn table(&self) -> Result<UnitTable, String>;
    /// `reset-failed` for the units named, and nothing else.
    fn reset_failed(&self, units: &[String]) -> Result<(), String>;
    /// `MemoryHigh` on [`SLICE`]: the throttle that keeps letibot from starving another project's
    /// work. `Err` names what the manager said, and a daemon goes on unbounded rather than not
    /// starting.
    fn set_slice_high(&self, bytes: u64) -> Result<(), String>;
}

/// This box's user manager, through `systemctl --user`.
pub struct Systemd;

impl Systemd {
    /// `systemctl --user ARGS…`, bounded. `timeout` is coreutils and absent on a Mac, where there
    /// is no user manager to ask either; the call is made unbounded there and fails at once.
    fn run(args: &[&str]) -> Result<String, String> {
        let systemctl = which_systemctl().ok_or_else(|| {
            "there is no `systemctl` on this PATH, so there is no user manager to ask".to_string()
        })?;
        // ONE command, whichever way it is bounded: the rlimit goes on the process actually
        // spawned, and a copy of the argv is how it silently went missing here.
        let mut cmd = match which("timeout") {
            Some(_) => {
                let mut c = Command::new("timeout");
                c.arg(CALL_BOUND_SECONDS.to_string()).arg(&systemctl);
                c
            }
            None => Command::new(&systemctl),
        };
        cmd.arg("--user").args(args);
        #[cfg(unix)]
        // No core dump from a manager probe either: a `systemctl` that dies of SIGXFSZ under a
        // file-size limit is not a crash anyone should be told about (R191).
        crate::exec::scope::keep_no_core(&mut cmd);
        let out = cmd
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("`systemctl` could not be run: {e}"))?;
        if out.status.success() {
            return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
        }
        let why = String::from_utf8_lossy(&out.stderr);
        let last = why
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("it said nothing")
            .trim();
        Err(last.to_string())
    }
}

impl UnitManager for Systemd {
    fn table(&self) -> Result<UnitTable, String> {
        // `--all` so a failed unit that unloaded is not missing from the count, `--plain` so the
        // `●` a head sees never reaches the parse, `--no-legend` so the header is not a unit.
        let text = Self::run(&["list-units", "--all", "--no-legend", "--plain"])?;
        Ok(parse_table(&text))
    }

    fn reset_failed(&self, units: &[String]) -> Result<(), String> {
        if units.is_empty() {
            return Ok(());
        }
        let mut args = vec!["reset-failed"];
        args.extend(units.iter().map(String::as_str));
        Self::run(&args).map(|_| ())
    }

    fn set_slice_high(&self, bytes: u64) -> Result<(), String> {
        Self::run(&["set-property", SLICE, &format!("MemoryHigh={bytes}")]).map(|_| ())
    }
}

/// A box with no user manager: every answer says so, in one sentence, and nothing is acted on.
pub struct NoManager;

impl UnitManager for NoManager {
    fn table(&self) -> Result<UnitTable, String> {
        Err("this box has no user manager, so it has no unit table".into())
    }

    fn reset_failed(&self, _units: &[String]) -> Result<(), String> {
        Err("this box has no user manager, so it has no failed units to reset".into())
    }

    fn set_slice_high(&self, _bytes: u64) -> Result<(), String> {
        Err("this box has no user manager, so there is no `letibot.slice` to throttle".into())
    }
}

/// Return what `PATH` resolves `name` to, without running it: the probe and the call must be the
/// same program, so the answer is a path rather than a name the kernel resolves again.
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|at| at.is_file())
}

fn which_systemctl() -> Option<PathBuf> {
    which("systemctl")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a manager holds, both facts, off the same `list-units` text: an unloaded failure counts
    /// (`--all`), a `●` line is still a unit, and only `letibot-*` is ours to reset.
    #[test]
    fn the_table_counts_units_and_keeps_the_failed_ones_ours_apart() {
        let text = "\
session-1.scope loaded active running A session
letibot-harnessd-1.scope loaded failed failed The daemon
drkonqi-coredump-launcher@1.service loaded failed failed A crash loop
letibot-gc.service loaded inactive dead The timer's unit
other.scope not-found failed failed Gone
";
        let t = parse_table(text);
        assert_eq!(t.units, 5, "every listed unit is a unit the manager holds");
        assert_eq!(
            t.ours_failed,
            vec!["letibot-harnessd-1.scope".to_string()],
            "one failed unit is ours; a crash launcher is not, and neither is an inactive one"
        );
        assert_eq!(t.other_failed, 2);
        let line = t.line();
        assert!(line.contains("5 units of 131072"), "{line}");
        assert!(line.contains("3 failed, 1 of them letibot's"), "{line}");
        assert!(parse_table("").line().contains("nothing failed"));
    }

    /// The trait's shape, driven by a stub: the reset names exactly the units handed to it, so a
    /// caller cannot widen it to a pattern or to nothing.
    #[test]
    fn a_reset_names_the_units_it_was_given_and_nothing_else() {
        struct Stub(std::sync::Mutex<Vec<String>>);
        impl UnitManager for Stub {
            fn table(&self) -> Result<UnitTable, String> {
                Ok(UnitTable::default())
            }
            fn reset_failed(&self, units: &[String]) -> Result<(), String> {
                *self.0.lock().unwrap() = units.to_vec();
                Ok(())
            }
            fn set_slice_high(&self, _bytes: u64) -> Result<(), String> {
                Ok(())
            }
        }
        let stub = Stub(std::sync::Mutex::new(Vec::new()));
        let ours = parse_table("letibot-a.scope loaded failed failed x\n").ours_failed;
        stub.reset_failed(&ours).unwrap();
        assert_eq!(*stub.0.lock().unwrap(), vec!["letibot-a.scope".to_string()]);
        // And no manager at all answers with the reason rather than pretending it acted.
        assert!(NoManager.reset_failed(&ours).is_err());
        assert!(NoManager.set_slice_high(1).is_err());
    }
}
