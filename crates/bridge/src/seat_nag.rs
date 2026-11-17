//! A seat's todo nag as a MONITOR's work, not a channel push. Only the serving bridge's clock knows
//! when the check is due - the seat's idleness is what arms it - so the bridge writes the notice
//! into a log beside the seat and `letibot-mcp todo-wait` reports it. Claude Code delivers a channel
//! frame only from a server named in `--dangerously-load-development-channels`, and drops the nag
//! silently when the session was started without it; a monitor exits and wakes the session either
//! way. The frame is still used when nobody is waiting, so a seat with the flag loses nothing.

use std::fs::{File, OpenOptions};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// How often a waiter looks at the log. The file is a few lines, and a nag is worth a wake within a
/// second of the bridge deciding it.
const POLL: Duration = Duration::from_millis(200);

/// How many notices the log keeps. A waiter's mark is an id, so the oldest lines are only ever ones
/// already read; this is what bounds a file a long session would otherwise grow for ever.
const KEEP: usize = 32;

/// Exit codes, `room-wait`'s conventions: a notice is on stdout, or the reason is on stderr.
pub const REPORTED: i32 = 0;
pub const UNREADABLE: i32 = 20;
pub const TIMEOUT: i32 = 30;

/// One seat's nag, as files in the folder's runtime directory. The bridge writes the log; a waiter
/// reads the log and keeps the mark; the claim is the lock that decides which of the two paths a
/// nag takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nag {
    /// The store the seat's board is in, so a waiter can say the seat is not readable yet.
    store: PathBuf,
    /// The notices, one JSON line each: `{"id": n, "text": "[todo check] ..."}`.
    log: PathBuf,
    /// The last id a waiter reported.
    mark: PathBuf,
    /// Held (flock) by the process that is waiting on this seat.
    claim: PathBuf,
}

/// The seat `handle`'s nag in `store`'s folder, as the channel's outbox already names itself.
pub fn for_seat(store: &Path, handle: &str) -> Nag {
    in_dir(crate::daemon::rundir(), store, handle)
}

/// [`for_seat`] with the directory named - the tests' door, because the runtime directory is one
/// process-wide fact and a test that set it would be moving another test's files.
pub fn in_dir(dir: PathBuf, store: &Path, handle: &str) -> Nag {
    let key = dir_key(store);
    let name = handle.replace(['/', '@', ':'], "-");
    Nag {
        store: store.to_path_buf(),
        log: dir.join(format!("{key}.seat-nag-log-{name}")),
        mark: dir.join(format!("{key}.seat-nag-mark-{name}")),
        claim: dir.join(format!("{key}.seat-nag-wait-{name}")),
    }
}

/// The key a seat's files are named by: the store's folder. Both sides derive it from the store
/// they resolved, which is the one fact a bridge and a waiter cannot disagree about.
pub fn dir_key(store: &Path) -> String {
    crate::daemon::key(store.parent().unwrap_or(store))
}

/// A held claim. Dropping it - or the process exiting, however it exits - releases the lock.
pub struct Claim(File);

impl Drop for Claim {
    fn drop(&mut self) {
        // Closing the descriptor would do it; unlocking says so where a reader looks.
        let _ = flock(&self.0, libc::LOCK_UN);
    }
}

impl Nag {
    /// Take this seat if nobody else waits on it. `Ok(None)` is a standby: another waiter holds it,
    /// and reporting beside the holder would wake the session twice.
    pub fn claim(&self) -> Result<Option<Claim>, String> {
        if let Some(dir) = self.claim.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.claim)
            .map_err(|e| format!("cannot wait on {}: {e}", self.claim.display()))?;
        Ok(flock(&file, libc::LOCK_EX | libc::LOCK_NB).then_some(Claim(file)))
    }

    /// Is a waiter on this seat right now? The bridge asks before it decides a nag's one delivery.
    pub fn claimed(&self) -> bool {
        let Ok(file) = File::open(&self.claim) else {
            return false;
        };
        // A shared lock is refused while a waiter holds the exclusive one, and closing the file
        // releases this probe: the question is asked, not the seat taken.
        !flock(&file, libc::LOCK_SH | libc::LOCK_NB)
    }

    /// Record `text` for the waiter that holds this seat, answering whether there was one. This is
    /// the bridge's one delivery decision: a nag is a log line or a channel frame, never both.
    pub fn note(&self, text: &str) -> bool {
        if !self.claimed() {
            return false;
        }
        let mut rows = self.rows();
        let id = rows.last().map_or(1, |(id, _)| id + 1);
        rows.push((id, text.to_string()));
        let keep = rows.split_off(rows.len().saturating_sub(KEEP));
        // A log this host cannot write is the channel's nag after all, never a lost one.
        write_rows(&self.log, &keep).is_ok()
    }

    /// The first notice the waiter has not reported. A mark past the log's last id is a log that
    /// was reset under a mark that outlived it, so it reads as nothing read rather than as all of it.
    pub fn pending(&self, mark: u64) -> Option<(u64, String)> {
        let rows = self.rows();
        let last = rows.last()?.0;
        let mark = if mark > last { 0 } else { mark };
        rows.into_iter().find(|(id, _)| *id > mark)
    }

    /// The id the last waiter reported, or 0.
    pub fn mark(&self) -> u64 {
        std::fs::read_to_string(&self.mark)
            .ok()
            .and_then(|t| t.trim().parse().ok())
            .unwrap_or(0)
    }

    /// Remember `id` as reported. Written BEFORE the notice is printed, `room-wait`'s order: a
    /// crash between the two loses a nag, and the other order would say one twice.
    pub fn reported(&self, id: u64) {
        if let Some(dir) = self.mark.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&self.mark, format!("{id}\n"));
    }

    fn rows(&self) -> Vec<(u64, String)> {
        let Ok(text) = std::fs::read_to_string(&self.log) else {
            return Vec::new();
        };
        text.lines()
            .filter_map(|line| {
                let row: Value = serde_json::from_str(line).ok()?;
                Some((row["id"].as_u64()?, row["text"].as_str()?.to_string()))
            })
            .collect()
    }
}

/// `todo-wait`: block until this seat's nag comes due and print it. The bridge's clock owns "due" -
/// a waiter that computed it would be a second clock, and two clocks is two nags.
pub fn wait(deadline: Instant) -> (i32, String) {
    let here = match std::env::current_dir() {
        Ok(here) => here,
        Err(e) => return (UNREADABLE, format!("cannot read the working folder: {e}")),
    };
    let daemon = match crate::daemon::resolve(&here) {
        Ok(daemon) => daemon,
        Err(e) => return (UNREADABLE, e),
    };
    wait_on(&for_seat(&daemon.store, &crate::rooms::handle()), deadline)
}

/// [`wait`]'s body, over a seat whose files are already resolved - the tests' door, because a
/// waiter's own [handle](crate::rooms::handle) is process-wide and a fixture cannot choose it.
pub fn wait_on(nag: &Nag, deadline: Instant) -> (i32, String) {
    if !crate::store::exists(&nag.store) {
        return (
            UNREADABLE,
            format!(
                "there is no store at {} yet: the seat's board is the daemon's file, so \
                 `leticode_start` is what opens one",
                nag.store.display()
            ),
        );
    }
    let mut held = None;
    loop {
        if held.is_none() {
            match nag.claim() {
                Ok(claim) => held = claim,
                Err(e) => return (UNREADABLE, e),
            }
        }
        // A standby reports nothing: the holder already has the same log and the same mark.
        if held.is_some()
            && let Some((id, text)) = nag.pending(nag.mark())
        {
            nag.reported(id);
            return (REPORTED, text);
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return (TIMEOUT, String::new());
        }
        std::thread::sleep(left.min(POLL));
    }
}

/// `flock(2)` as a question: `true` when the lock was taken.
fn flock(file: &File, how: libc::c_int) -> bool {
    // SAFETY: `file` owns a live descriptor for the length of the call, and `how` is a flock flag.
    unsafe { libc::flock(file.as_raw_fd(), how) == 0 }
}

/// Replace the log in one step, so a waiter reading it sees the old lines or the new ones.
fn write_rows(path: &Path, rows: &[(u64, String)]) -> Result<(), String> {
    let text: String = rows
        .iter()
        .map(|(id, text)| format!("{}\n", json!({ "id": id, "text": text })))
        .collect();
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp = path.with_file_name(format!("{name}.tmp"));
    std::fs::write(&tmp, text).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("cannot keep {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let root = letibot_sessionlog::testing::scratch_root()
            .join(format!("letibot-seat-nag-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("dir");
        root
    }

    /// A log line a waiter has not read is reported once, and the same id is never news again -
    /// the mark, not the notice text, is what makes a repeat a repeat.
    #[test]
    fn a_notice_is_reported_once_and_the_mark_is_what_says_so() {
        let root = scratch("mark");
        let nag = in_dir(root.clone(), &root.join("sessions.db"), "cc@mark");
        let _held = nag
            .claim()
            .expect("the claim is made")
            .expect("nothing holds it");
        assert!(
            nag.claimed(),
            "the holder is the waiter the bridge asks about"
        );
        assert!(nag.note("[todo check] Not done:\n  - ship it — yours"));
        assert!(nag.note("[todo check] Not done:\n  - ship it — yours, again"));

        assert_eq!(nag.mark(), 0);
        let (id, text) = nag.pending(nag.mark()).expect("the first notice");
        assert_eq!(
            (id, text.as_str()),
            (1, "[todo check] Not done:\n  - ship it — yours")
        );
        nag.reported(id);
        assert_eq!(nag.pending(nag.mark()).map(|(id, _)| id), Some(2));
        nag.reported(2);
        assert_eq!(nag.pending(nag.mark()), None);
        // A mark from a log that was reset under it is not a seat that said everything.
        assert_eq!(nag.pending(99).map(|(id, _)| id), Some(1));
    }

    /// With no waiter holding the seat the bridge is told so, and nothing is written: the channel
    /// is the one delivery, so the log may not hold a copy of it.
    #[test]
    fn no_waiter_means_no_log_line() {
        let root = scratch("free");
        let nag = in_dir(root.clone(), &root.join("sessions.db"), "cc@free");
        assert!(!nag.claimed(), "nobody is waiting");
        assert!(
            !nag.note("[todo check] something"),
            "so the channel says it"
        );
        drop(nag.claim().expect("the claim is made"));
        {
            let _held = nag.claim().expect("the claim is made");
            assert!(nag.note("[todo check] something else"), "a waiter is there");
        }
        // Released, so the bridge goes back to the frame.
        assert!(!nag.claimed());
    }
}
