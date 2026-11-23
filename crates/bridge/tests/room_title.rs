//! A bridge named after its Claude Code session: the title `/rename` writes into the transcript is
//! the handle the room sees, and a rename mid-session moves the label without moving the seat.
//! One test, one process: the handle, the seat and the transcript are process-wide.

use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};

use letibot_bridge::rooms::{self, RoomLink};
use letibot_room::Room;
use letibot_room::server::Server;
use serde_json::json;

/// The label the room holds for this bridge's seat: the nick it was given, else the name it is
/// drawn by. A label is display only, so this is what a reader sees and nothing is keyed on it.
fn label(server: &Server) -> Option<String> {
    server.nick_of(&rooms::seat())
}

/// Wait for the label to become `want`, which the supervisor's pass carries once the transcript
/// says so.
fn wait_label(server: &Server, want: &str) -> Option<String> {
    let until = Instant::now() + Duration::from_secs(20);
    loop {
        let now = label(server);
        if now.as_deref() == Some(want) || Instant::now() > until {
            return now;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn rename(transcript: &std::path::Path, title: &str) {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(transcript)
        .expect("the transcript");
    // Both rows Claude Code writes for one `/rename`; the later one is what wins.
    for row in [
        json!({"type": "custom-title", "customTitle": title}),
        json!({"type": "agent-name", "agentName": title}),
    ] {
        writeln!(file, "{row}").expect("a row");
    }
    file.flush().expect("flushed");
}

#[test]
fn a_seat_is_named_after_the_title_its_session_gave_itself() {
    let dir = std::env::temp_dir().join(format!("letibot-room-title-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let work = dir.join("stroppy-io");
    std::fs::create_dir_all(&work).unwrap();
    let config = dir.join("claude");
    let session = "9f0c1f7e-2a5a-4a3b-9a4e-5f6b7c8d9e0f";
    let project = letibot_bridge::titles::project_dir(&config, &work);
    std::fs::create_dir_all(&project).unwrap();
    let transcript = project.join(format!("{session}.jsonl"));
    // Two renames before the bridge starts: the folder's name first, then the one in force.
    rename(&transcript, "stroppy-io");
    rename(&transcript, "stroppy-conformity");

    // SAFETY: first thing in the only test of this file, before the handle or the title is read.
    unsafe {
        std::env::remove_var(rooms::NAME_ENV);
        std::env::set_var(rooms::SESSION_ENV, session);
        std::env::set_var("CLAUDE_CONFIG_DIR", &config);
        std::env::set_var("XDG_DATA_HOME", dir.join("data"));
    }
    std::env::set_current_dir(&work).unwrap();

    let server = Server::serve(&dir.join("room.sock"), Room::new()).expect("a coordinator");
    server.with_seat_host("lab2x1");

    let link = RoomLink::at(server.path());
    link.ensure().expect("the bridge connects");
    // The title, not the folder: `stroppy-conformity` already says which folder it is in.
    assert_eq!(rooms::handle(), "cc@stroppy-conformity");
    assert_eq!(
        label(&server).as_deref(),
        Some("stroppy-conformity"),
        "the seat is drawn by the title"
    );
    let seat = rooms::seat();

    // A `/rename` mid-session appends to the transcript, and the supervisor's pass - already there
    // for the room's liveness - carries the new label without anything polling for it.
    link.stream(Arc::new(|_: &serde_json::Value, _: bool| true));
    rename(&transcript, "stroppy-rust");
    assert_eq!(
        wait_label(&server, "stroppy-rust").as_deref(),
        Some("stroppy-rust"),
        "the label follows the transcript"
    );
    // The label moved and the address did not: the seat is what a wait and an ask are keyed on, and
    // the handle a reader addresses is still the name the seat settled as.
    assert_eq!(rooms::seat(), seat, "the seat is the address");
    assert_eq!(rooms::handle(), "cc@stroppy-conformity", "and the handle");
    let _ = std::fs::remove_dir_all(&dir);
}
