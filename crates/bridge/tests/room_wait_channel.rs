//! A session started with the development-channels flag already has its room messages pushed into it
//! as channel frames. The plugin's `room-wait` is then a second wake on a second connection of the
//! same seat, so the bridge marks the session and the waiter steps aside: exit 0, nobody connected.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use letibot_bridge::channel;

const NAME: &str = "cc@channel-test";

fn scratch(tag: &str) -> PathBuf {
    let root = match Path::new("/tmp").is_dir() {
        true => PathBuf::from("/tmp"),
        false => std::env::temp_dir(),
    };
    let root = root.join(format!(
        "bos-channel-{tag}-{}-{}",
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

#[test]
fn a_waiter_steps_aside_when_the_channel_is_live() {
    let root = scratch("live");
    let data = root.join("data");
    let folder = root.join("work");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&folder).unwrap();
    // The mark the MCP server writes when it finds the flag on its `claude` ancestor.
    channel::publish(&data.join("letibot"), NAME, true);

    let started = Instant::now();
    let out = letibot_testenv::command(env!("CARGO_BIN_EXE_letibot-mcp"))
        .arg("room-wait")
        .arg("--attention")
        .arg("to_me")
        .arg("--timeout")
        .arg("30s")
        .env("XDG_DATA_HOME", &data)
        .env("LETIBOT_ROOM_NAME", NAME)
        // A socket nothing serves: a waiter that tried to connect would answer 20, not 0.
        .env("LETIBOT_ROOM_SOCKET", root.join("no-coordinator.sock"))
        .current_dir(&folder)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("the waiter runs");
    assert_eq!(
        out.status.code(),
        Some(0),
        "the waiter stepped aside: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "and it did not sit out its timeout"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).trim().is_empty(),
        "nothing was printed as a message"
    );

    // Without the mark the waiter IS the wake: it tries to connect and says the room is not there.
    channel::publish(&data.join("letibot"), NAME, false);
    let out = letibot_testenv::command(env!("CARGO_BIN_EXE_letibot-mcp"))
        .arg("room-wait")
        .arg("--timeout")
        .arg("5s")
        .env("XDG_DATA_HOME", &data)
        .env("LETIBOT_ROOM_NAME", NAME)
        .env("LETIBOT_ROOM_SOCKET", root.join("no-coordinator.sock"))
        .current_dir(&folder)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("the waiter runs");
    assert_eq!(
        out.status.code(),
        Some(20),
        "no coordinator, so the waiter says so: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::io::stdout().flush();
    let _ = std::fs::remove_dir_all(&root);
}
