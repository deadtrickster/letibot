//! The room: one project's agents, one queue per tag, and a claim that is a lease with a beat.
//!
//! A tag is a LOCK, not a list of claimable items, so two sessions cannot hold one scarce thing.
//! The state machine reads no clock and opens no socket: the semantics are what must be right
//! before the coordinator is wired to them.

pub mod agent;
pub mod client;
pub mod item;
pub mod lease;
pub mod restart;
pub mod room;
pub mod server;
pub mod tree;
pub mod wire;

pub use agent::{AgentLedger, TagAgent};
pub use item::{Abandonment, Claim, Item, Speaker, State, Wait, Why, duration};
pub use lease::{
    Acquired, COORDINATOR_LEASE_TTL_MS, CoordinatorLease, Held, Holder, LEASE_BEAT_EVERY_MS,
    Takeover, coordinator_session, host_key, host_name, lease_path, named_session, pid_alive,
    socket_path, tag_of_session, tag_session,
};
pub use restart::{Cause, Marker, marker_path, read_marker, room_running, room_started_ms};
pub use room::{
    ABANDON_THRESHOLD, About, DEFAULT_BEAT_EVERY_MS, DEFAULT_LEASE_MS, Decision, Event, Facts,
    Holding, Outcome, Question, REPLAY_WINDOW, Refused, Reminder, Room, Ruling, Waiting,
    normalise_tag,
};
pub use server::Server;
pub use tree::{
    DEFAULT_PROJECT, Group, Member, PROJECT_KEY_LEN, is_project_key, project_key, room_name,
    room_of_name, tree_order,
};
pub use wire::{Answer, ROOM_PROTOCOL_VERSION, Request, Response};
