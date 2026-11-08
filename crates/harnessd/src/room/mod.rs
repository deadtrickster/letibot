//! The room, from the daemon's side - the `--room` coordinator, a session's connection to it
//! and the bell that turns an event into a turn; `docs/room.md` holds the split between the files.
//! Every name here is DERIVED from the box, and a box with no coordinator is not an error.

use std::path::PathBuf;

pub mod activation;
pub mod agents;
pub mod asks;
pub mod coordinator;
pub mod link;
pub mod operator;
pub mod remind;
pub mod roster;
pub mod seat;
pub mod supervise;
pub mod wake;

/// The environment variable that puts the room's socket somewhere else - a test's door, and
/// the reason no fixture has to touch the box's real data directory.
pub const SOCKET_ENV: &str = "LETIBOT_ROOM_SOCKET";

/// The environment variable that shortens the grace an ask's other end gets before the operator is
/// asked to restart it - [`letibot_room::server::ASK_GRACE_MS`] unless a test says otherwise, so a
/// run of this box's own coordinator is five minutes and a test's is seconds.
pub const ASK_GRACE_ENV: &str = "LETIBOT_ROOM_ASK_GRACE_MS";

/// The ask grace this box runs with, or `None` to take the room's own default.
pub fn ask_grace_ms() -> Option<u64> {
    std::env::var(ASK_GRACE_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
}

/// This box's room - where the coordinator serves, or `None` when there is no data directory
/// and no override, which is a box with nowhere to put a socket.
pub fn socket_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(SOCKET_ENV).filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(p));
    }
    let dir = crate::mergequeue::git::letibot_data_dir()?;
    Some(letibot_room::socket_path(&dir, &letibot_room::host_key()))
}

/// The coordinator's lease on this box - the file that makes *one coordinator* true.
pub fn lease_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(SOCKET_ENV).filter(|v| !v.is_empty()) {
        // The lease lives beside the socket it belongs to, so a test that moves one moves both.
        return Some(PathBuf::from(p).with_extension("lease"));
    }
    let dir = crate::mergequeue::git::letibot_data_dir()?;
    Some(letibot_room::lease_path(&dir, &letibot_room::host_key()))
}

/// The chat store beside the socket: one append-only file per host, so a coordinator restart keeps
/// history, sequence numbers and inbox cursors.
pub fn chat_path() -> Option<PathBuf> {
    socket_path().map(|p| p.with_extension("chat"))
}

/// The queues' store beside the socket: the same one append-only file for every project's room, so
/// a coordinator restart keeps the items, the leases and the open questions, and takes back a
/// claim whose beat stopped while it was down.
pub fn items_path() -> Option<PathBuf> {
    socket_path().map(|p| p.with_extension("items"))
}

/// The coordinator's own session id - `s-room-<host key>`, derived and never invented, so
/// `letibot --session host` always reaches the one agent that arbitrates this machine.
pub fn session_id() -> String {
    letibot_room::coordinator_session(&letibot_room::host_key())
}

/// The session a person named by a derived spelling - `host`, or `host:<tag>` for a tag's
/// agent - or `None` for an ordinary session id. One function, so three callers cannot disagree.
pub fn session_named(name: &str) -> Option<String> {
    letibot_room::named_session(&letibot_room::host_key(), name)
}

/// The tag a session is the agent of, when it is one - the derivation read backwards, against
/// this box's own key.
pub fn tag_of(session: &str) -> Option<String> {
    letibot_room::tag_of_session(&letibot_room::host_key(), session)
}

/// Is there a coordinator to talk to? A socket file that exists is the whole test; whether
/// anything is behind it is the tool's sentence to say.
pub fn socket_exists() -> bool {
    socket_path().map(|p| p.exists()).unwrap_or(false)
}

/// Whether a session is a top-level service, the kind that joins the room. A subagent speaks
/// through its parent, and so does a package a Claude Code seat started (`--parent-seat`).
/// A tag's agent is the exception: a child that holds a seat of its own.
pub fn is_top_level(cfg: &crate::config::Config) -> bool {
    if cfg.parent_seat.is_some() {
        return false;
    }
    match cfg.parent_session_id {
        None => true,
        Some(_) => tag_of(&cfg.session_id).is_some(),
    }
}

#[cfg(test)]
mod top_level_tests {
    use super::is_top_level;

    #[test]
    fn a_package_of_a_seat_is_not_top_level() {
        let mut cfg = crate::config::Config::for_this_box("/tmp/w");
        assert!(is_top_level(&cfg));
        cfg.parent_seat = Some("cc@letibot".into());
        assert!(!is_top_level(&cfg));
    }

    #[test]
    fn a_subagent_is_not_top_level() {
        let mut cfg = crate::config::Config::for_this_box("/tmp/w");
        cfg.parent_session_id = Some("s-parent".into());
        cfg.session_id = "s-child".into();
        assert!(!is_top_level(&cfg));
    }
}
