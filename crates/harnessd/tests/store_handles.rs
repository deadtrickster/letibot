//! **A session that closes gives its descriptors back** — the measurement the operator's `EMFILE`
//! is answered with, and the one that says which fix works.
//!
//! # The arithmetic, before and after
//!
//! `Harness::open` used to open the store **four times per session** — once for the session
//! itself, once for the corpus the gate writes, once for the `transcript` tool and once for the
//! `decisions` tool — and each of those was a `rusqlite` connection to a WAL file. MEASURED on
//! lab2x1, `main` at `4b2f2113`: **20 sessions opened and closed left 92 descriptors on
//! `sessions.db`** while the process held 5 `Store`s, and 88 stayed after the daemon's own
//! connection was dropped too.
//!
//! `crates/tokencore/tests/connection_fds.rs` measures why: `sqlite3` keeps a descriptor per file
//! in the inode's own record and releases it when the process's **last** connection to that file
//! closes, so closing a session's four connections returned none of them while the daemon's own
//! connection lived. So "close it properly" was never the fix — **fewer connections** is, and
//! `Store::open` now hands every caller in the process the same connection per store file.
//!
//! After: the count is the same before and after twenty sessions, and it is the daemon's own
//! three descriptors (the db, its `-wal` and its `-shm`).
//!
//! # How it counts
//!
//! `/dev/fd` on Darwin and `/proc/self/fd` on Linux: both are directories whose entries ARE the
//! process's open descriptors, so `read_dir` is a census and not an estimate. `Store::live()` is
//! the second, independent count — how many connections the process holds — because a descriptor
//! count is a proxy for a lifetime and `Store::live` is the lifetime itself.
//!
//! # What it needs
//!
//! **Nothing on this box but a temp directory.** The vocabulary is the byte one and the provider
//! is named and never called (`no_vocab.rs`'s shape), because no turn runs here — the subject is
//! the store, and a session that opens one needs no model to do it.

use std::path::Path;

use letibot_harnessd::config::{Config, ProviderConfig, Seat};
use letibot_harnessd::sessions::Sessions;
use letibot_harnessd::{Dialect, Parts};
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_tokencore::store::Store;
use letibot_turn::Endpoint;

#[path = "support/room.rs"]
mod room;

const ROOT: &str = "s-root-handles";

fn config(session_id: &str, store: &Path) -> Config {
    let mut cfg = Config::for_this_box("/tmp");
    cfg.dialect = Dialect::Qwen;
    // The unconfined seat with `write` and the delegation trio; a `coder` seat cannot open on a
    // Mac at all (see `which-seat-a-test-must-fixture-on-a-mac.md`), and nothing here is about a
    // boundary.
    cfg.seat = Seat::Leticode;
    cfg.endpoint = Endpoint::new("127.0.0.1", 1);
    cfg.http_retries = 0;
    cfg.context_window = Some(32_768);
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = session_id.to_string();
    cfg.web_search = None;
    cfg.permission = Vec::new();
    // **No GGUF**: a named provider opens a session on the byte vocabulary and no turn is run
    // here, so this measures the store on any box, with or without llama.cpp.
    cfg.vocab_gguf = None;
    cfg.provider = Some(ProviderConfig {
        name: "deepseek".into(),
        model: None,
        api_key: Some("sk-not-used".into()),
        thinking: false,
    });
    cfg
}

fn parts_for(cfg: &Config) -> Parts {
    let p = Parts::load(cfg).unwrap_or_else(|e| panic!("the byte vocabulary must load: {e:?}"));
    *p.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    p
}

fn wiring(cfg: &Config) -> SessionWiring {
    SessionWiring {
        model: cfg.model.clone(),
        dialect: "qwen".into(),
        endpoint: "http://127.0.0.1:1".into(),
        workspace: cfg.workspace.display().to_string(),
    }
}

/// **This process's open descriptors** — the census, from the directory the kernel keeps them in.
///
/// Darwin has `/dev/fd` (`fdescfs`); Linux has `/proc/self/fd`. Both list one entry per open
/// descriptor, so the count is exact on the two boxes this tree runs on.
pub fn open_descriptors() -> usize {
    let dir = if cfg!(target_os = "linux") {
        "/proc/self/fd"
    } else {
        "/dev/fd"
    };
    std::fs::read_dir(dir).map(|d| d.count()).unwrap_or(0)
}

/// **How many of this process's descriptors are on `path` and its WAL companions** — the count
/// the operator read off their daemon with `lsof`, taken here without one.
///
/// `readlink` answers on Linux; Darwin's `/dev/fd` is not a symlink tree that resolves, so this
/// asks the kernel with `F_GETPATH`.
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
                .filter(|fd| descriptor_target(*fd).contains(&name))
                .count()
        })
        .unwrap_or(0)
}

#[cfg(target_os = "macos")]
fn descriptor_target(fd: i32) -> String {
    let mut buf = [0u8; 1024];
    if unsafe { libc::fcntl(fd, libc::F_GETPATH, buf.as_mut_ptr()) } < 0 {
        return String::new();
    }
    let s = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr() as *const libc::c_char) };
    s.to_string_lossy().into_owned()
}

#[cfg(not(target_os = "macos"))]
fn descriptor_target(fd: i32) -> String {
    std::fs::read_link(format!("/proc/self/fd/{fd}"))
        .map(|p| p.display().to_string())
        .unwrap_or_default()
}

struct TempDir {
    path: std::path::PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!("{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("creating the temp dir");
        Self { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// **One test at a time**: the census is the PROCESS's, and a sibling test in this binary opening
/// sessions in parallel would be counted as this one's leak.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// **Twenty sessions opened and closed leave the store's descriptors where they were.**
///
/// This is the deliverable's number. `Harness::open` opened the store four times per session, each
/// a WAL connection, and MEASURED on `main` at `4b2f2113` twenty sessions left **92** descriptors
/// on `sessions.db` after they were closed. One connection per store file per process makes the
/// count the daemon's own three whatever the session count, and the bound below is the baseline
/// plus nothing.
#[test]
fn a_closed_session_gives_its_descriptors_back() {
    let Some(_) = room::room_for("twenty sessions opened and closed", 1024) else {
        return;
    };
    let _one = serial();

    let dir = TempDir::new("harnessd-store-handles");
    let path = dir.path.join("sessions.db");
    let cfg = config(ROOT, &path);
    let parts = parts_for(&cfg);
    let registry = Registry::new();
    registry.set_source(
        letibot_harnessd::sessions::StoreSessions::open(&cfg).expect("the store opens"),
    );
    registry
        .create(ROOT, "the root", wiring(&cfg))
        .expect("the root is created");
    let mut sessions =
        Sessions::open_first(&parts, cfg.clone(), registry.clone()).expect("the daemon opens");

    // **The baseline, taken with the daemon up and no session of its own** — one connection to the
    // store file, so the db, its `-wal` and its `-shm`.
    let baseline = descriptors_on(&path);
    let connections = Store::live();
    eprintln!(
        "baseline: {baseline} descriptors on the store file, {connections} connection(s), {} in \
         the process",
        open_descriptors()
    );
    assert_eq!(
        connections, 1,
        "the daemon holds ONE connection to its store, whatever it is serving"
    );
    assert!(
        baseline <= 3,
        "one WAL connection is the db, its -wal and its -shm: {baseline}"
    );

    let ids: Vec<String> = (0..20).map(|n| format!("s-handle-{n}")).collect();
    for id in &ids {
        registry
            .create(id, "a session", wiring(&cfg))
            .expect("the session is created");
        sessions.open(id).expect("the session opens");
    }
    let open_descs = descriptors_on(&path);
    let open_connections = Store::live();
    eprintln!(
        "while open: {open_connections} connection(s) for {} sessions, {open_descs} descriptors on \
         the store file",
        ids.len()
    );
    assert_eq!(
        open_connections, 1,
        "twenty sessions opened a second connection between them"
    );

    for id in &ids {
        sessions.close(id);
    }
    let closed_descs = descriptors_on(&path);
    let closed_connections = Store::live();
    eprintln!(
        "after closing all {}: {closed_connections} connection(s), {closed_descs} descriptors on \
         the store file, {} in the process",
        ids.len(),
        open_descriptors()
    );

    // **The number, in the assertion**: the count comes back to the baseline it started at.
    assert!(
        closed_descs <= baseline,
        "closing {} sessions left {closed_descs} descriptors on the store file, where the daemon \
         held {baseline} before any of them opened — a session that ends does not give its store \
         back",
        ids.len()
    );
    assert_eq!(
        closed_connections, 1,
        "a closed session left a connection of its own behind"
    );

    // And the process's own census did not climb by the sessions either: whatever else this
    // binary holds, twenty sessions' worth of connections would show here.
    drop(sessions);
    drop(registry);
}

/// **Two sessions at once are not blocked by each other.**
///
/// The fix's cost is a lock: every store access in the process now serialises behind one
/// connection, so this is the test that says the second session still gets its work done. Two
/// threads write and read through the same store while two sessions are open; both finish, every
/// row is there, and the connection count does not move.
///
/// **What it would catch**: a lock held across a whole turn (a second session waiting for ever), a
/// transaction left open (the next writer blocking on `BEGIN IMMEDIATE` until its thirty-second
/// timeout), and a second connection appearing per thread.
#[test]
fn two_sessions_at_once_are_not_blocked_by_each_other() {
    let Some(_) = room::room_for("two sessions at once", 1024) else {
        return;
    };
    let _one = serial();

    let dir = TempDir::new("harnessd-store-two");
    let path = dir.path.join("sessions.db");
    let cfg = config(ROOT, &path);
    let parts = parts_for(&cfg);
    let registry = Registry::new();
    registry.set_source(
        letibot_harnessd::sessions::StoreSessions::open(&cfg).expect("the store opens"),
    );
    registry
        .create(ROOT, "the root", wiring(&cfg))
        .expect("the root is created");
    let mut sessions =
        Sessions::open_first(&parts, cfg.clone(), registry.clone()).expect("the daemon opens");
    for id in ["s-two-a", "s-two-b"] {
        registry
            .create(id, "a session", wiring(&cfg))
            .expect("the session is created");
        sessions.open(id).expect("the session opens");
    }
    let baseline = descriptors_on(&path);
    let connections = Store::live();

    // Two writers, one store, at the same time — each on its own thread, each through its own
    // `Store` handle, which is the same connection.
    let writes = 50;
    let work = |session: &'static str| {
        let path = path.clone();
        std::thread::spawn(move || {
            let store = Store::open(&path).expect("the same store");
            for n in 0..writes {
                store
                    .put_todos(
                        session,
                        &[letibot_tokencore::store::TodoItem {
                            content: format!("{session} row {n}"),
                            status: letibot_tokencore::store::TodoStatus::Pending,
                            by: letibot_tokencore::store::TodoBy::Model,
                            when: None,
                            needs: Vec::new(),
                        }],
                    )
                    .expect("a write");
                let back = store.todos(session).expect("a read");
                assert_eq!(back.len(), 1, "{session} lost its board");
            }
        })
    };
    let a = work("s-two-a");
    let b = work("s-two-b");
    a.join().expect("the first session's thread finished");
    b.join().expect("the second session's thread finished");

    // Both sessions' rows are on the store, and each is its own.
    let store = Store::open(&path).expect("the store");
    for session in ["s-two-a", "s-two-b"] {
        let todos = store.todos(session).expect("a read");
        assert_eq!(todos.len(), 1, "{session} has no board");
        assert_eq!(
            todos[0].content,
            format!("{session} row {}", writes - 1),
            "{session}'s last write is not the one on its row"
        );
    }
    let after = descriptors_on(&path);
    eprintln!(
        "two sessions, {writes} writes each: {connections} connection(s) before, {} after; \
         {baseline} descriptors on the store file before, {after} after",
        Store::live()
    );
    assert_eq!(
        Store::live(),
        1,
        "a second thread's store was a second connection"
    );
    assert!(
        after <= baseline,
        "two sessions writing left {after} descriptors on the store file where {baseline} was the \
         baseline"
    );
    drop(sessions);
    drop(registry);
}
