//! A restart leaves a note: when the room began again, and why. The room's items are memory only (chat has a store), so a
//! note on the socket's own directory is the only place a person can read what happened.

use std::io;
use std::path::{Path, PathBuf};

use crate::item::duration;

/// Why the room is a new one - three stories a person can be told, and each is a different thing to do
/// about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    /// A person pressed restart in the room's pane. The marker names the session whose pane it was.
    Asked,
    /// The daemon restarted it and found nothing running: the key did not empty the queue, the room
    /// had already gone, and what happened was a start.
    Daemon,
    /// Nobody here asked, and the room's instance changed: it died and came back - or it is gone and
    /// the marker is the last thing it left.
    Crashed,
}

impl Cause {
    /// The word the file carries.
    pub fn as_str(self) -> &'static str {
        match self {
            Cause::Asked => "asked",
            Cause::Daemon => "daemon",
            Cause::Crashed => "crashed",
        }
    }

    fn parse(text: &str) -> Option<Cause> {
        match text {
            "asked" => Some(Cause::Asked),
            "daemon" => Some(Cause::Daemon),
            "crashed" => Some(Cause::Crashed),
            _ => None,
        }
    }
}

/// The note a restart leaves: when it was asked or seen, which room it is about, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marker {
    /// When the act was asked for, or when the observation was made. Milliseconds since the epoch,
    /// the room's one clock.
    pub at_ms: u64,
    /// Which room this is about: the lease's `started_ms`, or `0` when the daemon could not read it (a
    /// marker written before the new coordinator had taken its lease).
    pub room_started_ms: u64,
    pub cause: Cause,
    /// The session whose pane it was, for [`Cause::Asked`]. Empty otherwise.
    pub by: String,
    /// The writer's own clause, when it has one - *the room that started 14m ago went away*.
    pub says: String,
}

impl Marker {
    /// A marker about the room that begins at `room_started_ms`.
    pub fn new(at_ms: u64, room_started_ms: u64, cause: Cause) -> Marker {
        Marker {
            at_ms,
            room_started_ms,
            cause,
            by: String::new(),
            says: String::new(),
        }
    }

    pub fn by(mut self, session: impl Into<String>) -> Marker {
        self.by = session.into();
        self
    }

    pub fn saying(mut self, says: impl Into<String>) -> Marker {
        self.says = says.into();
        self
    }

    /// Does this marker already account for the room that began at `instance_ms`? The guard an
    /// observation asks before it writes: a fact already recorded is not recorded again, and the act
    /// that emptied the room keeps its own sentence.
    pub fn claims(&self, instance_ms: u64) -> bool {
        self.room_started_ms == instance_ms
    }

    /// The sentence a pane prints. The age is measured from the act, so a restart asked for a minute ago
    /// and finished a second ago reads as a minute - the identity sentence already says how long the
    /// room has been up. Every arm ends with *a restart begins with no items at all*.
    pub fn sentence(&self, now: u64) -> String {
        let age = duration(now.saturating_sub(self.at_ms));
        let mut said = match self.cause {
            Cause::Asked => format!(
                "restarted {age} ago: {} pressed restart in the room pane",
                self.who()
            ),
            Cause::Daemon => format!(
                "restarted {age} ago by the daemon, and nothing was running to stop — this is a \
                 start, so the key did not empty the queue, the room had already gone"
            ),
            Cause::Crashed => {
                format!("restarted {age} ago and nobody asked — the room before it went away")
            }
        };
        if !self.says.is_empty() {
            said.push_str(&format!(" ({})", self.says));
        }
        said.push_str(". A restart begins with no items at all.");
        said
    }

    fn who(&self) -> String {
        if self.by.is_empty() {
            "a session".to_string()
        } else {
            format!("`{}`", self.by)
        }
    }

    /// The one line the file holds.
    pub fn line(&self) -> String {
        format!(
            "{}\t{}\t{}\t{}\t{}\n",
            self.at_ms,
            self.room_started_ms,
            self.cause.as_str(),
            one_line(&self.by),
            one_line(&self.says),
        )
    }

    /// The marker at `path`, or `None` - missing, empty, or not a marker.
    pub fn read(path: &Path) -> Option<Marker> {
        let text = std::fs::read_to_string(path).ok()?;
        parse(&text)
    }

    /// Write it, beside itself and renamed into place - the lease's own discipline, so a reader never
    /// sees half a note.
    pub fn write(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension(format!("restart-{}", std::process::id()));
        std::fs::write(&tmp, self.line())?;
        std::fs::rename(&tmp, path)
    }
}

/// A field of the file is one line: a tab or a newline in a session id or a sentence would split the
/// record, and a marker that cannot be read is worse than one that has lost a space.
fn one_line(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c == '\t' || c == '\n' || c == '\r' {
                ' '
            } else {
                c
            }
        })
        .collect::<String>()
        .trim()
        .to_string()
}

fn parse(text: &str) -> Option<Marker> {
    // `trim_end` and not `trim`, for the lease's reason: a leading tab is the FIRST FIELD being
    // empty, and `trim` would eat it and read the room's instance as the moment of the act.
    let line = text.lines().next()?.trim_end();
    let mut f = line.split('\t');
    let at_ms: u64 = f.next()?.trim().parse().ok()?;
    let room_started_ms: u64 = f.next().unwrap_or("0").trim().parse().unwrap_or(0);
    let cause = Cause::parse(f.next().unwrap_or("").trim())?;
    Some(Marker {
        at_ms,
        room_started_ms,
        cause,
        by: f.next().unwrap_or("").trim().to_string(),
        says: f.next().unwrap_or("").trim().to_string(),
    })
}

/// Where a box's restart marker lives - beside the socket it belongs to, so a box that moves its room
/// with `$LETIBOT_ROOM_SOCKET` moves the note with it.
pub fn marker_path(socket: &Path) -> PathBuf {
    socket.with_extension("restart")
}

/// The marker beside `socket`, if there is one.
pub fn read_marker(socket: &Path) -> Option<Marker> {
    Marker::read(&marker_path(socket))
}

/// When the room behind `socket` began, from the lease it holds - `None` when there is no lease, which
/// is a room that is not there. The lease and not the identity sentence: `started_ms` is a number two
/// readers can compare, and the sentence is prose whose age moves every second.
pub fn room_started_ms(socket: &Path) -> Option<u64> {
    let holder = crate::lease::CoordinatorLease::owned_by(socket.with_extension("lease"), "reader")
        .read()?;
    Some(holder.started_ms)
}

/// Is a room running behind `socket`? The lease names a pid and that pid is alive - the launcher's own
/// test, asked from this side. Not a connection test on purpose: the daemon asking may be the one whose
/// socket is fine and whose coordinator is wedged, and *wedged* is not *gone*.
pub fn room_running(socket: &Path) -> bool {
    let Some(holder) =
        crate::lease::CoordinatorLease::owned_by(socket.with_extension("lease"), "reader").read()
    else {
        return false;
    };
    crate::lease::pid_alive(holder.pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let p =
            std::env::temp_dir().join(format!("letibot-room-restart-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).expect("a scratch directory");
        p
    }

    /// A marker round-trips, and it says all five fields - the record is the file, so a field the writer
    /// drops is a fact the pane can never print.
    #[test]
    fn a_marker_round_trips_through_the_file() {
        let d = dir();
        let path = marker_path(&d.join("room.sock"));
        assert_eq!(path.file_name().unwrap(), "room.restart");
        let marker = Marker::new(1_000, 900, Cause::Asked)
            .by("s-a")
            .saying("the pane's key");
        marker.write(&path).expect("the marker is written");
        assert_eq!(Marker::read(&path), Some(marker));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A marker nobody can parse is `None` rather than a guess: a note that cannot be read is not one a
    /// reader can be misled by.
    #[test]
    fn an_unreadable_marker_is_not_a_marker() {
        let d = dir();
        let path = d.join("room.restart");
        for junk in ["", "\n", "not a marker\n", "1000\t900\tnonsense\t\t\n"] {
            std::fs::write(&path, junk).unwrap();
            assert_eq!(Marker::read(&path), None, "{junk:?}");
        }
        assert_eq!(Marker::read(&d.join("nothing.restart")), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A sentence in a field does not split the record: a tab or a newline in what a writer said would
    /// make the fifth field a sixth, and the marker would come back as `None`.
    #[test]
    fn a_tab_in_a_field_does_not_break_the_line() {
        let d = dir();
        let path = d.join("room.restart");
        Marker::new(1, 2, Cause::Daemon)
            .saying("it said\tstop\nnow")
            .write(&path)
            .unwrap();
        let read = Marker::read(&path).expect("still one record");
        assert_eq!(read.says, "it said stop now");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The three causes read differently, and only the words tell *restarted clean* from *restarted
    /// empty*.
    #[test]
    fn the_three_causes_are_three_sentences() {
        let asked = Marker::new(10_000, 9_000, Cause::Asked)
            .by("s-a")
            .sentence(14_000);
        assert!(asked.contains("`s-a` pressed restart"), "{asked}");
        assert!(asked.contains("4s ago"), "{asked}");

        let daemon = Marker::new(10_000, 9_000, Cause::Daemon).sentence(14_000);
        assert!(daemon.contains("by the daemon"), "{daemon}");
        assert!(daemon.contains("already gone"), "{daemon}");
        assert!(
            !daemon.contains("pressed"),
            "nobody pressed this one: {daemon}"
        );

        let crashed = Marker::new(10_000, 9_000, Cause::Crashed).sentence(14_000);
        assert!(crashed.contains("nobody asked"), "{crashed}");
        assert!(crashed.contains("went away"), "{crashed}");

        for said in [&asked, &daemon, &crashed] {
            assert!(
                said.contains("A restart begins with no items at all"),
                "every one says what survives: {said}"
            );
        }
        assert_ne!(asked, daemon);
        assert_ne!(daemon, crashed);
    }

    /// A marker claims the room it is about, and no other - the guard that keeps an observation from
    /// overwriting the record of an act.
    #[test]
    fn a_marker_claims_the_instance_it_names() {
        let m = Marker::new(10, 900, Cause::Asked).by("s-a");
        assert!(m.claims(900));
        assert!(!m.claims(901), "a later room is not this marker's");
        assert!(!m.claims(0), "and neither is an unreadable instance");
    }

    /// The instance and the liveness come from the lease, the only place a room that is not answering
    /// still leaves a trace.
    #[test]
    fn the_room_started_ms_and_liveness_come_from_the_lease() {
        let d = dir();
        let socket = d.join("room.sock");
        assert_eq!(room_started_ms(&socket), None);
        assert!(!room_running(&socket), "no lease is no room");

        // A lease held by a pid that is not running: the room is gone, and its start is still readable
        // - the two facts a restart marker is built from.
        let lease =
            crate::lease::CoordinatorLease::owned_by(socket.with_extension("lease"), "box/1/1");
        std::fs::write(lease.path(), "box/1/1\t999999\t5000\t7000\n").unwrap();
        assert_eq!(room_started_ms(&socket), Some(7000));
        assert!(!room_running(&socket), "pid 999999 is not running");

        // This process's own pid: alive, so the room is.
        std::fs::write(
            lease.path(),
            format!("box/1/1\t{}\t5000\t7000\n", std::process::id()),
        )
        .unwrap();
        assert!(room_running(&socket));
        let _ = std::fs::remove_dir_all(&d);
    }
}
