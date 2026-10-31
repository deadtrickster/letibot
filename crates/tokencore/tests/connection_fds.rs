//! **A closed connection does not give its descriptor back while this process still holds one to
//! the same file** — the SQLite fact [`Store::open`](letibot_tokencore::store::Store::open)'s
//! sharing exists for, measured here rather than argued.
//!
//! # What the daemon's `ENFILE` was
//!
//! MEASURED 2026-10-30 on the operator's Mac and again on lab2x1: `sqlite3`'s unix VFS caches the
//! descriptor it opened for a file in the inode's own record, and releases it when the *last*
//! connection to that inode in the process closes — not when the connection that opened it does.
//! A daemon keeps one connection for its whole life (its registry's), so a descriptor from every
//! connection it had ever opened was held until it exited. `Harness::open` opened the store four
//! times per session, and the operator's daemon reached 253 descriptors with 210 of them on
//! `sessions.db`/`-wal`, which on the Mac ended in `ENFILE` — the whole box out of file table.
//!
//! # The two facts, and the second is why "close it properly" was never the fix
//!
//! * **Sequential** open/close leaves the count where it was: the next `open` takes the descriptor
//!   the last close left behind, so a daemon that opens and closes one at a time never grew.
//! * **Concurrent** connections are each their own descriptor, and closing them returns nothing
//!   while the keeper lives. So the count is the *high-water mark* of concurrent connections, and
//!   the only way to bound it is to stop making so many.
//!
//! These are measured on raw `rusqlite::Connection`s, not on `Store`: they are facts about
//! `sqlite3`, and the fix is a decision about how many connections to make.

use std::path::Path;

use rusqlite::Connection;

/// **This process's descriptors on `path` and its WAL companions** — `/proc/self/fd` on Linux,
/// `/dev/fd` on Darwin, both of which list one entry per open descriptor.
fn descriptors_on(path: &Path) -> usize {
    let dir = if cfg!(target_os = "linux") {
        "/proc/self/fd"
    } else {
        "/dev/fd"
    };
    let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
        return 0;
    };
    std::fs::read_dir(dir)
        .map(|d| {
            d.filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().to_string_lossy().parse::<i32>().ok())
                .filter(|fd| target_of(*fd).contains(&name))
                .count()
        })
        .unwrap_or(0)
}

#[cfg(target_os = "macos")]
fn target_of(fd: i32) -> String {
    let mut buf = [0u8; 1024];
    if unsafe { libc::fcntl(fd, libc::F_GETPATH, buf.as_mut_ptr()) } < 0 {
        return String::new();
    }
    let s = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr() as *const libc::c_char) };
    s.to_string_lossy().into_owned()
}

#[cfg(not(target_os = "macos"))]
fn target_of(fd: i32) -> String {
    std::fs::read_link(format!("/proc/self/fd/{fd}"))
        .map(|p| p.display().to_string())
        .unwrap_or_default()
}

/// A WAL connection that has actually written — the shape every `Store` is.
fn open(path: &Path) -> Connection {
    let conn = Connection::open(path).expect("a connection");
    conn.busy_timeout(std::time::Duration::from_secs(30))
        .expect("a busy timeout");
    conn.pragma_update(None, "journal_mode", "WAL")
        .expect("WAL");
    conn.pragma_update(None, "synchronous", "FULL")
        .expect("synchronous");
    conn.execute_batch("CREATE TABLE IF NOT EXISTS t (x)")
        .expect("a table");
    conn.execute("INSERT INTO t VALUES (1)", [])
        .expect("a row");
    conn
}

/// A directory of this test's own, removed when it goes.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!("{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("the scratch dir");
        Self(path)
    }

    fn db(&self) -> std::path::PathBuf {
        self.0.join("sessions.db")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// **The descriptor count is a high-water mark, not a debt**: twenty connections opened one after
/// another and closed leave the count where one connection put it, because each open takes the
/// descriptor the last close left in the inode's cache.
#[test]
fn sequential_open_and_close_reuses_the_descriptor() {
    let scratch = Scratch::new("letibot-fds-sequential");
    let path = scratch.db();
    let keeper = open(&path);
    let base = descriptors_on(&path);
    assert!(
        base >= 1,
        "the keeper's own descriptors are on the file: {base}"
    );

    for _ in 0..20 {
        let conn = open(&path);
        conn.close().expect("a close");
    }
    let after = descriptors_on(&path);
    eprintln!("sequential: {base} with one connection, {after} after twenty more opened and closed");
    assert!(
        after <= base + 1,
        "twenty sequential open/close left {after} descriptors on the file where one connection \
         held {base} — the next open takes the descriptor the last close left, so a daemon that \
         opens and closes one at a time does not grow"
    );

    drop(keeper);
    assert_eq!(
        descriptors_on(&path),
        0,
        "the process's last connection takes every cached descriptor with it"
    );
}

/// **A closed connection's descriptor outlives it while another connection to the file lives** —
/// so a session that closes its store gives the store back and not the descriptor.
#[test]
fn a_closed_connection_holds_its_descriptor_until_the_last_one_goes() {
    let scratch = Scratch::new("letibot-fds-closed");
    let path = scratch.db();
    let keeper = open(&path);
    let base = descriptors_on(&path);

    let held: Vec<Connection> = (0..20).map(|_| open(&path)).collect();
    let while_open = descriptors_on(&path);
    for conn in held {
        conn.close().expect("a close");
    }
    let after = descriptors_on(&path);
    eprintln!(
        "closed: {base} with one connection, {while_open} with twenty-one open, {after} after \
         closing the twenty"
    );
    assert!(
        while_open > base,
        "twenty more connections are twenty more descriptors: {while_open} against {base}"
    );
    assert!(
        after > base,
        "closing twenty connections returned the file to {after} descriptors where one connection \
         held {base} — each of them left its descriptor in the inode's cache, and the process's \
         last connection is what takes them away. This is why a daemon's descriptor count was a \
         function of how many sessions it had ever served, and why the fix is fewer connections \
         rather than better closes"
    );

    drop(keeper);
    assert_eq!(
        descriptors_on(&path),
        0,
        "the last connection to the file releases every descriptor the process cached for it"
    );
}
