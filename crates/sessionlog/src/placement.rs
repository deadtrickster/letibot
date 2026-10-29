//! **Where a daemon's tools run, written down beside its socket.**
//!
//! `docs/remote-hosts.md` places a session's TOOLS on another machine — `--host lab2x1` — while
//! its model, its store and its gate stay here. That is a fact of the **daemon**, one per process:
//! every session it opens runs its `bash`, `read`, `write` and jobs in the same place. A head that
//! attaches has no way to ask. The protocol carries the session's model, its endpoint and its
//! dialect, all of them per session and all of them on the wire; the backend is not among them, and
//! inventing a frame for one process-wide fact would be the wrong place to put it.
//!
//! So the daemon writes it down: the spelling and the socket it belongs to, in a file named after
//! that socket, beside the claim ([`crate::server`]'s `FolderClaim`) that already says *this folder
//! has a daemon and it is me*. The bridge reads it to answer `leticode_status`'s placement, and —
//! the reason this matters — to refuse a `leticode_start { host }` that would otherwise join a
//! daemon running the caller's tools somewhere else.
//!
//! # Why the socket's identity is in the record
//!
//! **A record that outlived its daemon must never be read as the present one's.** A daemon killed
//! with `SIGKILL` leaves its `.placement` behind, and the next daemon to take the folder — a local
//! one, now — writes its own. But between that bind and that write there is a window, and a reader
//! that took the stale line in it would answer *remote* about a daemon running the tools here: the
//! caller's package would run locally while it believed it was placed, which is the defect the
//! bridge refuses rather than risks. So a record names the `(dev, ino)` of the socket it belongs
//! to (the same pair `ServerHandle` keeps to unlink *its own* file and no other), and a reader that
//! finds a different inode at the socket than the record names answers *nothing known* — which
//! every caller treats as a refusal rather than an answer.
//!
//! **Nothing removes the record on the way out.** The socket's absence is what says nobody is
//! here, and it is the one fact every reader checks first (`connect`, not `stat` — see
//! `crates/bridge/src/daemon.rs`'s `is_up`); a file that outlives its daemon can only be read by a
//! caller that has just been answered by a listener, and then the identity check settles it.

use std::path::{Path, PathBuf};

/// `<socket>.placement` — beside the socket and beside its `.lock`, named after the same key.
pub fn path(socket: &Path) -> PathBuf {
    socket.with_extension("placement")
}

/// **Record where this daemon's tools run**, as the spelling `Placement::parse` reads back
/// (`host`, `host:lab2x1`, `firecode`).
///
/// Called by the daemon once it holds the folder — after the bind, so only the daemon that is
/// actually serving writes here, and a second start that was refused for a live daemon cannot
/// overwrite the live one's answer.
///
/// Written to a sibling and renamed, so a reader never sees half a record; the sibling is named
/// after the record so two daemons racing cannot share a temp file.
pub fn write(socket: &Path, spelling: &str) -> std::io::Result<()> {
    let (dev, ino) = socket_id(socket)?;
    let target = path(socket);
    let mut tmp = target.clone();
    tmp.set_extension("placement.tmp");
    std::fs::write(
        &tmp,
        format!("socket {dev}:{ino}\nplacement {}\n", spelling.trim()),
    )?;
    std::fs::rename(&tmp, &target)
}

/// **Where the daemon holding this socket says its tools run**, or `None` when there is no record
/// or the record is not this socket's.
///
/// `None` is *nothing is known*, never *local*: the caller that has to decide something from this
/// (the bridge, before it reuses a daemon) must fail closed on it.
pub fn read(socket: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path(socket)).ok()?;
    let mut recorded: Option<(u64, u64)> = None;
    let mut spelling: Option<String> = None;
    for line in text.lines() {
        let Some((key, value)) = line.split_once(' ') else {
            continue;
        };
        match key {
            "socket" => {
                recorded = value
                    .trim()
                    .split_once(':')
                    .and_then(|(d, i)| Some((d.parse().ok()?, i.parse().ok()?)));
            }
            "placement" => {
                let value = value.trim();
                if !value.is_empty() {
                    spelling = Some(value.to_string());
                }
            }
            // A key this build does not know is not a fault: the record is a fact of the build
            // that wrote it, and a reader that refused the whole file over a line it does not
            // use would break on the next field added.
            _ => {}
        }
    }
    let recorded = recorded?;
    // The identity check: a record whose socket is not the socket at this path belongs to a
    // daemon that is gone (see this module's doc).
    (socket_id(socket).ok()? == recorded).then_some(spelling)?
}

/// The `(dev, ino)` of a path — the pair that says *this file, not another one at the same name*.
///
/// A stat and not a connect: the caller has already established that something is listening, and
/// what is being asked here is which file the listener is bound to.
fn socket_id(path: &Path) -> std::io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(path)?;
    Ok((m.dev(), m.ino()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A socket path in a scratch directory, with a real socket file at it — the record is about a
    /// socket's identity, so a test that wrote one beside a name that is not a socket would be
    /// testing a file.
    ///
    /// The listener is kept alive by the caller: closing it unlinks nothing, but the inode is what
    /// the assertions are about and a rebound socket is this module's whole other test.
    ///
    /// [`crate::testing::scratch_root`] and not `$TMPDIR`: a unix socket path is capped at
    /// `SUN_LEN`, and a per-user temp directory is most of the budget before a name is added.
    fn a_socket(tag: &str) -> (PathBuf, std::os::unix::net::UnixListener) {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = crate::testing::scratch_root().join(format!(
            "letibot-placement-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        let path = dir.join("daemon.sock");
        let l = std::os::unix::net::UnixListener::bind(&path).expect("a socket binds");
        (path, l)
    }

    /// **The round trip**: what the daemon wrote is what a head reads, at the same spelling.
    #[test]
    fn what_the_daemon_recorded_is_what_a_reader_gets() {
        let (socket, _l) = a_socket("round-trip");
        assert_eq!(read(&socket), None, "nothing has been written yet");
        write(&socket, "host:lab2x1").expect("the record is written");
        assert_eq!(read(&socket).as_deref(), Some("host:lab2x1"));
        // And a daemon on this box records `host`, which is a placement and not an absence.
        write(&socket, "host").expect("the record is rewritten");
        assert_eq!(read(&socket).as_deref(), Some("host"));
        let _ = std::fs::remove_dir_all(socket.parent().expect("a parent"));
    }

    /// **A record from a daemon that is gone is not this daemon's answer.** The socket at the path
    /// is a new file — a new inode — and the record still names the old one, so the reader answers
    /// nothing known rather than a stale placement.
    ///
    /// This is the shape the bridge refuses on: *there is a daemon here and I cannot say where it
    /// runs the tools* is a refusal, while a stale *remote* read as the present truth is a package
    /// running locally when its caller asked for a host.
    ///
    /// **The successor takes the name by a rename, not by a rebind at the path.** Unlinking and
    /// rebinding asks the filesystem to hand out a fresh inode, and only tmpfs does: on ext4 the
    /// freed inode comes straight back (measured 30 times out of 30), so a rebinding fixture passes
    /// wherever `TMPDIR` is on tmpfs and fails wherever it is on a real disk — which is what a gate
    /// that moves its scratch onto one exposes. Two files that exist at the same moment cannot share
    /// an inode, so binding the successor beside the old socket and moving it onto the name makes
    /// *a different file at the same path* the fixture's guarantee instead of the filesystem's
    /// favour, and leaves the assertion about identity exactly as strong.
    #[test]
    fn a_record_of_a_socket_that_is_gone_is_not_an_answer() {
        use std::os::unix::fs::FileTypeExt;

        let (socket, first) = a_socket("replaced");
        write(&socket, "host:lab2x1").expect("the record is written");
        assert_eq!(read(&socket).as_deref(), Some("host:lab2x1"));
        let gone = socket_id(&socket).expect("the socket the record names");
        // The daemon goes, and a new one takes the same path: same name, a different file.
        let successor = socket.with_file_name("successor.sock");
        let _second =
            std::os::unix::net::UnixListener::bind(&successor).expect("a new socket binds");
        drop(first);
        std::fs::rename(&successor, &socket).expect("the new socket takes the old name");
        assert!(
            std::fs::metadata(&socket)
                .expect("the new socket")
                .file_type()
                .is_socket(),
            "the fixture needs a real socket at the path, not a file that took its name"
        );
        assert_ne!(
            socket_id(&socket).expect("the new socket"),
            gone,
            "the fixture needs a different file at the path for this to be about identity"
        );
        assert_eq!(
            read(&socket),
            None,
            "the record names the socket that is gone, so nothing is known about this one"
        );
        let _ = std::fs::remove_dir_all(socket.parent().expect("a parent"));
    }
}
