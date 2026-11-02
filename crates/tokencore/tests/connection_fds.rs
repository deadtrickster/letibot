//! **A closed connection does not give its descriptor back while this process still holds one to
//! the same file** - the SQLite fact [`Store::open`](letibot_tokencore::store::Store::open)'s sharing
//! exists for, measured here rather than argued. `sqlite3`'s unix VFS caches the descriptor in the
//! inode's own record and releases it when the process's *last* connection to that inode closes, so
//! a daemon keeping one connection for its whole life held a descriptor from every connection it had
//! ever opened: 253 descriptors, 210 on `sessions.db`/`-wal`, which on the Mac ended in `ENFILE`.
//! **Sequential** open/close leaves ONE descriptor behind; **concurrent** ones are each their own and
//! return nothing while the keeper lives. So the count is the high-water mark of CONCURRENT connections.

use std::path::Path;

use rusqlite::Connection;

/// **This process's descriptors on `path` and its WAL companions** - `/proc/self/fd` on Linux,
/// `/dev/fd` on Darwin. Matched on the DIRECTORY and the file name, not on the name alone: every
/// fixture here opens a `sessions.db`, and a census that counted its neighbours' would read theirs.
fn descriptors_on(path: &Path) -> usize {
    let dir = if cfg!(target_os = "linux") {
        "/proc/self/fd"
    } else {
        "/dev/fd"
    };
    let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
        return 0;
    };
    let here = path
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned());
    std::fs::read_dir(dir)
        .map(|d| {
            d.filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().to_string_lossy().parse::<i32>().ok())
                .filter(|fd| {
                    let target = target_of(*fd);
                    // `contains`, not `ends_with`: the `-wal` and `-shm` are the same store and do
                    // not end with the db's own name.
                    target.contains(&name) && here.as_deref().is_none_or(|h| target.contains(h))
                })
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

/// A WAL connection that has actually written - the shape every `Store` is.
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
    conn.execute("INSERT INTO t VALUES (1)", []).expect("a row");
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

/// **Twenty connections opened one after another and closed leave ONE descriptor behind** - the next
/// `open` takes the one the last `close` left, so the count is a high-water mark of CONCURRENT
/// connections. **The census must match the DIRECTORY**: matching the file name read a sibling's 39.
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
    eprintln!(
        "sequential: {base} with one connection, {after} after twenty more opened and closed"
    );
    assert!(
        after <= base + 1,
        "twenty connections opened and closed one at a time left {after} descriptors on the file \
         where one connection held {base} — each open takes the descriptor the last close left, so \
         a daemon that opens and closes one at a time holds one connection's worth, not twenty"
    );

    keeper.close().expect("the last close reports");
    assert_eq!(
        descriptors_on(&path),
        0,
        "the process's last connection takes every cached descriptor with it"
    );
}

/// **A closed connection's descriptor outlives it while another connection to the file lives**  -
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

    keeper.close().expect("the last close reports");
    assert_eq!(
        descriptors_on(&path),
        0,
        "the last connection to the file releases every descriptor the process cached for it"
    );
}
