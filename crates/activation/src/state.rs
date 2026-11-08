//! A running helper's note about itself: pid, binary and that binary's mtime when it started.
//! `status` compares the mtime with the file on disk, so a rebuilt binary shows as stale.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    pub pid: u32,
    pub exe: PathBuf,
    /// Seconds since the epoch.
    pub exe_mtime: u64,
    pub started: u64,
}

pub fn mtime(path: &Path) -> Option<u64> {
    let t = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(t.duration_since(UNIX_EPOCH).ok()?.as_secs())
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl State {
    pub fn this_process() -> State {
        let exe = std::env::current_exe().unwrap_or_default();
        State {
            pid: std::process::id(),
            exe_mtime: mtime(&exe).unwrap_or(0),
            exe,
            started: now(),
        }
    }

    pub fn to_text(&self) -> String {
        format!(
            "pid={}\nexe={}\nexe_mtime={}\nstarted={}\n",
            self.pid,
            self.exe.display(),
            self.exe_mtime,
            self.started
        )
    }

    pub fn parse(text: &str) -> Option<State> {
        let get = |k: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(k)?.strip_prefix('='))
        };
        Some(State {
            pid: get("pid")?.parse().ok()?,
            exe: PathBuf::from(get("exe")?),
            exe_mtime: get("exe_mtime")?.parse().ok()?,
            started: get("started")?.parse().ok()?,
        })
    }

    /// Written whole to a temp file and renamed, so a reader never sees half of it.
    pub fn write(&self, path: &Path) -> std::io::Result<()> {
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, self.to_text())?;
        std::fs::rename(&tmp, path)
    }

    pub fn read(path: &Path) -> Option<State> {
        State::parse(&std::fs::read_to_string(path).ok()?)
    }

    /// The binary on disk changed after this process loaded it.
    pub fn stale(&self) -> bool {
        mtime(&self.exe).is_some_and(|m| m != self.exe_mtime)
    }
}

pub fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks that the pid exists and may be signalled.
    pid > 0 && unsafe { libc::kill(pid as libc::pid_t, 0) } == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_state_round_trips_and_a_touched_binary_is_stale() {
        let dir = std::env::temp_dir().join(format!("lb-act-state-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("bin");
        std::fs::write(&exe, "a").unwrap();
        let s = State {
            pid: 7,
            exe: exe.clone(),
            exe_mtime: mtime(&exe).unwrap(),
            started: 100,
        };
        let p = dir.join("x.state");
        s.write(&p).unwrap();
        assert_eq!(State::read(&p), Some(s.clone()));
        assert!(!s.stale());
        let older = State {
            exe_mtime: s.exe_mtime - 5,
            ..s
        };
        assert!(older.stale());
        assert!(alive(std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
