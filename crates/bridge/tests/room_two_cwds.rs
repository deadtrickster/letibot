//! One Claude Code session is ONE seat, whatever directory each of its processes started in: the
//! plugin's monitor may run in a subdirectory of the repository its MCP server was started in, and a
//! handle or a room minted from whichever directory a process happened to start in split one seat
//! into two rows of two projects.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use letibot_bridge::rooms;
use letibot_room::server::Server;
use letibot_room::{Room, Seat};

const SESSION: &str = "2E4A9C1B-9F3D-4A21-8B77-5C1D0E9A1234";

/// A short root: a socket path has a length limit and a Mac's temp dir is a long one.
fn scratch(tag: &str) -> PathBuf {
    let root = match Path::new("/tmp").is_dir() {
        true => PathBuf::from("/tmp"),
        false => std::env::temp_dir(),
    };
    let root = root.join(format!(
        "bos-two-cwd-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            % 100_000
    ));
    std::fs::create_dir_all(&root).expect("a scratch root");
    root
}

fn waiter(socket: &Path, folder: &Path, data: &Path, timeout: &str) -> Child {
    letibot_testenv::command(env!("CARGO_BIN_EXE_letibot-mcp"))
        .arg("room-wait")
        .arg("--attention")
        .arg("to_me")
        .arg("--timeout")
        .arg(timeout)
        .env("LETIBOT_ROOM_SOCKET", socket)
        .env("XDG_DATA_HOME", data)
        .env(rooms::SESSION_ENV, SESSION)
        .env_remove(rooms::NAME_ENV)
        .current_dir(folder)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the waiter starts")
}

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !cond() {
        assert!(Instant::now() < deadline, "never happened: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The external seats the host room's roster draws, by name and by the project room they are in.
fn external(server: &Server) -> Vec<(String, String)> {
    server
        .roster_of("host")
        .into_iter()
        .filter(|r| r.kind == Seat::External)
        .map(|r| (r.name, r.project))
        .collect()
}

#[test]
fn two_processes_of_one_session_in_two_folders_are_one_seat() {
    let root = scratch("seat");
    // The repository, and a directory inside it: the two folders one session's processes begin in.
    let repo = root.join("letibot");
    let inner = repo.join("letibot");
    let data = root.join("data");
    std::fs::create_dir_all(&inner).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    let socket = root.join("room.sock");
    let server = Server::serve(&socket, Room::new()).expect("a coordinator");

    // The MCP server's process: the repository.
    let mut first = waiter(&socket, &repo, &data, "60s");
    wait_until("the first process is in the room", || {
        !external(&server).is_empty()
    });
    let one = external(&server);
    assert_eq!(one.len(), 1, "{one:?}");

    // The monitor's: a directory inside it, and the same session id.
    let mut second = waiter(&socket, &inner, &data, "2s");
    let _ = second.wait();

    let rows = external(&server);
    assert_eq!(rows.len(), 1, "one seat, not one row per folder: {rows:?}");
    assert_eq!(rows[0], one[0], "the second process joined the first's row");
    // And it is the seat's room, not the second folder's.
    assert_eq!(
        rows[0].1, one[0].1,
        "the row did not move project: {rows:?}"
    );

    let _ = first.kill();
    let _ = first.wait();
    server.stop();
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::io::stdout().flush();
}

/// The record the seat is kept under is one per Claude Code session, and a second process reads the
/// name and the room the first settled: that is what makes the row above one row.
#[test]
fn a_sessions_seat_is_kept_by_the_session_not_by_a_folder() {
    let root = scratch("record");
    let data = root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let dir = data.join("letibot");
    rooms::remember_session(&dir, SESSION, "cc@letibot", "abc123def456/p-one").expect("kept");
    assert_eq!(
        rooms::settled_session(&dir, SESSION),
        Some(("cc@letibot".to_string(), "abc123def456/p-one".to_string()))
    );
    // Another session is another seat, and a folder's own name is not this file's business.
    assert_eq!(rooms::settled_session(&dir, "not-this-session"), None);
    let _ = std::fs::remove_dir_all(&root);
}

/// A bridge of a session that has already settled keeps the seat's name even when its own folder
/// would derive another: the seat id decides, not the directory.
#[test]
fn a_bridge_reads_the_seat_its_session_already_settled() {
    let root = scratch("handle");
    let data = root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    // SAFETY: the only test of this file that touches the environment, and it sets it before any
    // bridge cell is read.
    unsafe {
        std::env::set_var("XDG_DATA_HOME", &data);
        std::env::set_var(rooms::SESSION_ENV, SESSION);
        std::env::set_var(rooms::NAME_ENV, "cc@my-own-folder");
    }
    rooms::remember_session(&data.join("letibot"), SESSION, "cc@letibot", "")
        .expect("the seat is kept");
    assert_eq!(
        rooms::settled_session(&data.join("letibot"), SESSION),
        Some(("cc@letibot".to_string(), String::new()))
    );
    // The first read of the name happens here, and the session's settled one wins.
    assert_eq!(rooms::handle(), "cc@letibot");
    let _ = std::fs::remove_dir_all(&root);
}
