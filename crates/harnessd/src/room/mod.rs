//! **The room, from the daemon's side** — the `--room` coordinator mode, the session's own
//! connection to it, and the bell that turns an event into a turn.
//!
//! `docs/room.md` § *What the daemon still owes* is the list this module answers, and the split
//! between the files is that list's own order:
//!
//! * **[`coordinator`]** — the `--room` mode: a whole `harnessd` with no project, its own socket
//!   under the data directory, the lease that makes it the only one on the box, the tie-breaker
//!   session prompted with `open_questions`, and the refusal to start at all when there is no
//!   model to arbitrate with.
//! * **[`link`]** — one connection per top-level session, the tool's verbs behind it, the beat run
//!   on the session's behalf while its turn runs, and the callback that turns a room event into a
//!   wake.
//! * **[`wake`]** — the queue between the link's reader thread and `Harness::wake`, because the
//!   thread that hears the bell is not the thread that runs the turn.
//!
//! # The names are derived, and they must not move
//!
//! A room is per **box**, so every name in it comes from the host and from nothing else: the
//! socket, the lease, and the coordinator's own session id (`s-room-<host key>`). That is the
//! review queue's `s-gatekeeper-<key>` rule one level out — a session id that a restart, a second
//! folder and the operator's own `--session host` all have to agree about.
//!
//! # A box with no coordinator is not an error
//!
//! Every daemon on this box looks for the same socket and most boxes have nothing there. That is
//! not a failure and must not be one: the `room` tool then refuses by name (the room is a daemon of
//! its own) and everything else about the session is exactly what it was. What IS a failure is a
//! coordinator that starts and cannot arbitrate — see [`coordinator::configure`].

use std::path::PathBuf;

pub mod coordinator;
pub mod link;
pub mod wake;

/// **The environment variable that puts the room's socket somewhere else** — a test's door, and
/// the reason no fixture has to touch the box's real data directory.
pub const SOCKET_ENV: &str = "LETIBOT_ROOM_SOCKET";

/// **This box's room** — where the coordinator serves, or `None` when there is no data directory
/// and no override, which is a box with nowhere to put a socket.
pub fn socket_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(SOCKET_ENV).filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(p));
    }
    let dir = crate::mergequeue::git::letibot_data_dir()?;
    Some(letibot_room::socket_path(
        &dir,
        &letibot_room::host_key(),
    ))
}

/// **The coordinator's lease on this box** — the file that makes *one coordinator* true.
pub fn lease_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(SOCKET_ENV).filter(|v| !v.is_empty()) {
        // The lease lives beside the socket it belongs to, so a test that moves one moves both.
        return Some(PathBuf::from(p).with_extension("lease"));
    }
    let dir = crate::mergequeue::git::letibot_data_dir()?;
    Some(letibot_room::lease_path(&dir, &letibot_room::host_key()))
}

/// **The coordinator's own session id** — `s-room-<host key>`, derived from the box and never
/// invented. `letibot --session host` resolves to it (`cli`'s own resolution), so the operator can
/// always reach the one agent that arbitrates this machine.
pub fn session_id() -> String {
    letibot_room::coordinator_session(&letibot_room::host_key())
}

/// **Is there a coordinator to talk to?** A socket file that exists is the whole test — connecting
/// is what finds out whether anything is behind it, and that answer is a sentence the tool shows.
pub fn socket_exists() -> bool {
    socket_path().map(|p| p.exists()).unwrap_or(false)
}
