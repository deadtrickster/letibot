//! **One daemon for one folder, and what says so** — the operator's 2026-10-04 entry.
//!
//! The symptom they met, in their own words: *"a session that could not be written to at all.
//! Every append refused: `row 483 (…): sqlite: transcript_item seq must be the next one`"* —
//! not the context wall, a BRICKED session, because saying anything is a write. The store's
//! trigger was doing its job; the cause was **two daemons for one folder**, and the way the
//! second one got in is the defect this file is about:
//!
//! > `ServerHandle::shutdown` and `Drop` both unlink the socket file. A daemon asked to stop
//! > can unlink it and keep listening. The next start finds no file, probes nothing, and binds
//! > a second socket at the same path — the check that would have refused it is gated on
//! > `path.exists()`.
//!
//! > **Liveness is being inferred from a filename, and an unlinked socket is indistinguishable
//! > from a dead daemon.**
//!
//! The guard is now a held lock beside the socket (`FolderClaim`), which the kernel releases
//! when the process goes — including on `SIGKILL`, which no `Drop` survives — and which no
//! shutdown path can remove. These tests drive the three consequences, and each one is a thing
//! that was measured going wrong on the box:
//!
//! 1. **The second daemon is refused BY NAME**, with the pid of the one serving, because a
//!    refusal the operator cannot act on is a refusal they will route around.
//! 2. **The refusal does not depend on the socket file existing** — this is the defect, in
//!    three lines: start, unlink, start again.
//! 3. **A daemon going away removes its own socket and nobody else's.** That one was not in
//!    the entry and is worse than what is: the dying daemon unlinked the *current holder's*
//!    file, which makes the live daemon the one the next start cannot see, so the loop
//!    repeats.

use std::sync::Arc;

use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_sessionlog::server::{ServerHandle, serve_registry};

/// A path no other test and no live daemon is using. The lock file lives beside it, so this
/// is one name for the whole fixture.
fn socket_path(tag: &str) -> std::path::PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    // **Short**, because macOS caps a socket path at 104 bytes and its temp dir alone is
    // about 50; the nanosecond clock in decimal and a long tag overflowed it (`path must be
    // shorter than SUN_LEN`). The tag is for a human reading `ls`, so ten characters do.
    std::env::temp_dir().join(format!(
        "lb-1fold-{:.10}-{}-{:x}.sock",
        tag,
        std::process::id(),
        n as u32
    ))
}

fn wiring() -> SessionWiring {
    SessionWiring {
        model: "qwen-3.8-flash-next".into(),
        dialect: "qwen3.8".into(),
        endpoint: "127.0.0.1:8080".into(),
        workspace: "/home/dead/Projects/letibot".into(),
    }
}

fn start(path: &std::path::Path) -> std::io::Result<ServerHandle> {
    let r = Registry::new();
    r.create("a", "the socket question", wiring()).unwrap();
    serve_registry(r.clone(), path)
}

fn cleanup(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(path.with_extension("lock"));
}

/// **The refusal, by name, from the lock rather than the file.**
#[test]
fn a_second_daemon_for_one_folder_is_refused_and_the_refusal_names_the_live_pid() {
    let p = socket_path("by-name");
    let first = start(&p).expect("the first daemon binds");
    let ours = std::process::id();

    // The claim is a file beside the socket, and it holds the pid of the process serving —
    // which is what makes the refusal actionable rather than merely correct.
    let lock = p.with_extension("lock");
    let written: u32 = std::fs::read_to_string(&lock)
        .expect("the claim file is written when the claim is taken")
        .trim()
        .parse()
        .expect("and it is the pid, not prose");
    assert_eq!(
        written, ours,
        "the pid in the claim is the claiming process"
    );

    let second = start(&p);
    let err = match second {
        Ok(_) => panic!("a second daemon took a folder that already had one"),
        Err(e) => e,
    };
    assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse, "{err}");
    let msg = err.to_string();
    assert!(
        msg.contains(&ours.to_string()),
        "the refusal does not name the live pid, so nothing can be done about it: {msg}"
    );
    assert!(
        msg.contains("lock beside the socket"),
        "the refusal does not say what refused it, which is the whole point of it not being \
         the file: {msg}"
    );

    first.shutdown();
    cleanup(&p);
}

/// **The defect itself: unlink the socket file under a live daemon and start a second one.**
///
/// MEASURED before this guard existed, on a scratch socket: `ss -xlH` showed **two LISTEN rows
/// on one path** while `ls` said the file did not exist, and the second daemon started with no
/// refusal at all. Both daemons then append to one store, and the transcript trigger refuses
/// every row — the session is bricked and nothing on any screen said why.
#[test]
fn the_refusal_does_not_depend_on_the_socket_file_existing() {
    let p = socket_path("unlinked");
    let first = start(&p).expect("the first daemon binds");

    // Exactly what a stopping daemon's `shutdown`/`Drop` does to the path, with the process
    // still up: `remove_file`, and the listener keeps its nameless inode.
    std::fs::remove_file(&p).expect("the socket file is there to unlink");
    assert!(!p.exists(), "the premise: the file is gone");

    let second = start(&p);
    let err = match second {
        Ok(_) => panic!(
            "a second daemon bound a path whose file was unlinked under a live one — this is \
             the defect the test is named after"
        ),
        Err(e) => e,
    };
    assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse, "{err}");
    assert!(
        err.to_string().contains(&std::process::id().to_string()),
        "{err}"
    );

    first.shutdown();
    cleanup(&p);
}

/// **And the ordinary path still works**: when the daemon that held the folder is gone, the
/// next start takes it. A guard that cannot be released is a folder nobody can serve again,
/// which is the failure mode on the other side of this one.
#[test]
fn once_the_daemon_is_gone_the_folder_can_be_served_again() {
    let p = socket_path("again");
    let first = start(&p).expect("the first daemon binds");
    first.shutdown();
    assert!(!p.exists(), "shutdown removes its own socket file");

    let second = start(&p).expect("the claim died with the daemon that held it");
    second.shutdown();
    cleanup(&p);
}

/// **The consequence the entry does not name, and the reason the loop repeated.**
///
/// `shutdown`, `Drop` and the accept thread all removed *the path*. The path is shared state:
/// once a second daemon could be at it, the old daemon's `Drop` unlinked the NEW daemon's
/// socket, so the live daemon became the one with no file — and the next start, inferring
/// liveness from a filename, invited a third.
///
/// The interleaving needs the claim to have been taken twice, which the guard now prevents
/// except when the claim FILE itself is removed by hand (a hand on the box, not a code path).
/// That is exactly the state this drives, and the assertion is the one that matters: the
/// newer daemon's socket survives the older daemon's `shutdown`.
#[test]
fn a_daemon_going_away_does_not_unlink_a_newer_daemons_socket() {
    let p = socket_path("newer");
    let first = start(&p).expect("the first daemon binds");

    // The state a hand produces: the socket unlinked (as a stopping daemon does) and the claim
    // file removed (as nothing in the code does).
    std::fs::remove_file(&p).expect("the first socket");
    let lock = p.with_extension("lock");
    std::fs::remove_file(&lock).expect("the first claim file");

    // A second daemon takes the folder. Its socket is a NEW inode at the same path.
    let second = start(&p).expect("with the claim file gone, a second daemon starts");
    assert!(p.exists(), "the second daemon's socket file is there");

    // **And the first one goes.** Its removal is by (dev, ino) now, so it leaves the file that
    // is no longer its own alone.
    first.shutdown();
    assert!(
        p.exists(),
        "the older daemon's shutdown unlinked the newer daemon's socket: the live daemon is \
         now invisible to every liveness test on the box"
    );

    second.shutdown();
    cleanup(&p);
}

/// The lock is released by the process DYING, not by a tidy exit — which is the property the
/// socket file does not have and the whole reason the guard is a lock.
///
/// Driven here the way it can be driven in one process: a claim taken by another `File` is
/// released when that file is closed, and it is the kernel that does it, so nothing about the
/// holder's exit path can leave the folder claimed forever.
#[test]
fn the_claim_is_released_by_the_holder_going_not_by_a_tidy_exit() {
    let p = socket_path("released");
    let lock = p.with_extension("lock");

    // A bare holder, standing in for a daemon that dies without running a line of its own
    // shutdown code — `SIGKILL` cannot run `Drop`, and this does not either.
    {
        let holder = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock)
            .unwrap();
        holder.try_lock().expect("nothing holds it yet");
        assert!(
            start(&p).is_err(),
            "a claim held by something else must refuse a start"
        );
        // `holder` is closed here: no `unlock`, no removal, no `Drop` we wrote.
    }

    let after = start(&p).expect("the claim went when its holder did");
    after.shutdown();
    cleanup(&p);
}

/// **A second daemon in ONE process, without a claim file at all**, is still refused by the
/// probe — the belt under the braces, kept so an older build's daemon (or any other program
/// that bound the path) is not started over.
#[test]
fn a_live_socket_with_no_lock_is_still_refused() {
    let p = socket_path("nolock");
    // A plain listener, standing in for a daemon from a build older than this one.
    let _listener = std::os::unix::net::UnixListener::bind(&p).expect("bind");

    let err = match start(&p) {
        Ok(_) => panic!("something is listening on that path and it was started over"),
        Err(e) => e,
    };
    assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse, "{err}");
    assert!(
        err.to_string().contains("does not hold the folder lock"),
        "the refusal should say it is the older/foreign case: {err}"
    );

    cleanup(&p);
}

/// The claim is taken before the socket is bound, so a folder whose socket file is a leftover
/// of a crash — file present, nobody listening, no claim — still starts, and starts once.
#[test]
fn a_stale_socket_file_does_not_stop_a_start() {
    let p = socket_path("stale");
    // A crashed daemon's leftovers: the socket file with nothing behind it.
    {
        let l = std::os::unix::net::UnixListener::bind(&p).expect("bind");
        drop(l);
    }
    assert!(p.exists(), "the stale file is the premise");

    let h = start(&p).expect("a stale file is not a daemon");
    assert!(Arc::strong_count(&h.registry()) >= 1);
    h.shutdown();
    cleanup(&p);
}
