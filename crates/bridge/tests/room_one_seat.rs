//! One folder is ONE seat, whichever process speaks for it: the plugin's waiter and the MCP bridge
//! of one Claude Code session join the same name, and only another SEAT - a different seat id - is
//! suffixed. The waiter is a reader and names no id of its own, because the two processes agree on
//! the folder and not on an id: Claude Code's session id reaches its MCP server, and a monitor's
//! environment is not the same thing.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use letibot_bridge::rooms::{self, RoomLink};
use letibot_room::server::Server;
use letibot_room::{Room, Seat};

/// The name both processes ask for: the folder's own, which the room's `base_name` would derive
/// from the directory this test runs in.
const NAME: &str = "cc@mac-findings";

fn scratch(tag: &str) -> PathBuf {
    // A socket path has a length limit, and a Mac's temp dir is a long `/var/folders/...`: the
    // shortest writable root this box has is used first.
    let root = std::path::Path::new("/tmp");
    let root = match root.is_dir() {
        true => root.to_path_buf(),
        false => std::env::temp_dir(),
    };
    let root = root.join(format!(
        "bos-{tag}-{}-{}",
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

/// One `room-wait`, in a process of its own.
fn waiter(
    socket: &Path,
    folder: &Path,
    data: &Path,
    session: Option<&str>,
    timeout: &str,
) -> Child {
    let mut cmd = letibot_testenv::command(env!("CARGO_BIN_EXE_letibot-mcp"));
    cmd.arg("room-wait")
        .arg("--attention")
        .arg("to_me")
        .arg("--timeout")
        .arg(timeout)
        .env("LETIBOT_ROOM_SOCKET", socket)
        .env(rooms::NAME_ENV, NAME)
        .env("XDG_DATA_HOME", data)
        .current_dir(folder)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    match session {
        Some(id) => {
            cmd.env(rooms::SESSION_ENV, id);
        }
        None => {
            cmd.env_remove(rooms::SESSION_ENV);
        }
    }
    cmd.spawn().expect("the waiter starts")
}

/// The external seats the host room's roster draws.
fn external(server: &Server) -> Vec<String> {
    server
        .roster_of("host")
        .into_iter()
        .filter(|r| r.kind == Seat::External)
        .map(|r| r.name)
        .collect()
}

/// Wait on a fact, never on a fixed time.
fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !cond() {
        assert!(Instant::now() < deadline, "never happened: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn the_waiter_and_the_bridge_of_one_folder_are_one_seat() {
    let root = scratch("seat");
    let folder = root.join("letibot");
    let data = root.join("data");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    // SAFETY: first thing in the only test of this file, before the bridge reads its name.
    unsafe {
        std::env::set_var(rooms::NAME_ENV, NAME);
        std::env::set_var("XDG_DATA_HOME", &data);
    }
    let socket = root.join("room.sock");
    let server = Server::serve(&socket, Room::new()).expect("a coordinator");

    // The waiter connects first, as the plugin's monitor does, and names no seat.
    let mut first = waiter(&socket, &folder, &data, None, "60s");
    wait_until("the waiter is in the room", || {
        !external(&server).is_empty()
    });
    assert_eq!(external(&server), vec![NAME.to_string()]);

    // The bridge of the same Claude Code session: a real id, and still one seat with the reader.
    let bridge = RoomLink::at(&socket);
    bridge.as_seat("lab2x1/cc-2e4a9c1b");
    bridge.ensure().expect("the bridge connects");
    assert_eq!(rooms::handle(), NAME, "the bridge took no suffixed name");
    assert_eq!(
        external(&server),
        vec![NAME.to_string()],
        "one name, one seat in the roster"
    );
    assert_eq!(bridge.seat(), "lab2x1/cc-2e4a9c1b");

    // A second process of the same Claude Code session joins them, and takes no twin either.
    let mut second = waiter(&socket, &folder, &data, Some("2E4A9C1B-9F3D"), "2s");
    let _ = second.wait();
    assert_eq!(
        external(&server),
        vec![NAME.to_string()],
        "a second process of one session is one seat"
    );

    let _ = first.kill();
    let _ = first.wait();
    server.stop();
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::io::stdout().flush();
}
