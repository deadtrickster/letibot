//! The room: one project's agents, one queue per tag, and a claim that is a lease with a beat.
//! A tag is a lock, not a list of claimable items. The state machine reads no clock and opens no socket.

pub mod agent;
pub mod chat;
pub mod client;
pub mod item;
pub mod lease;
pub mod restart;
pub mod room;
pub mod server;
pub mod store;
pub mod tree;
pub mod wire;

pub use agent::{AgentLedger, TagAgent};
pub use chat::{
    Attention, Chat, DM_ROOM, HOST_ROOM, History, MAX_TEXT, Message, OPERATOR, Presence, RING, Ref,
    RoomInfo, Seat, project_room,
};
pub use item::{Abandonment, Claim, Item, Speaker, State, Wait, Why, duration};
pub use lease::{
    Acquired, COORDINATOR_LEASE_TTL_MS, CoordinatorLease, Held, Holder, LEASE_BEAT_EVERY_MS,
    Takeover, coordinator_session, host_key, host_name, lease_path, named_session,
    operator_token_path, pid_alive, socket_path, tag_of_session, tag_session,
};
pub use restart::{Cause, Marker, marker_path, read_marker, room_running, room_started_ms};
pub use room::{
    ABANDON_THRESHOLD, About, DEFAULT_BEAT_EVERY_MS, DEFAULT_LEASE_MS, Decision, Event, Facts,
    Holding, Outcome, Question, REPLAY_WINDOW, Refused, Reminder, Room, Ruling, Waiting,
    normalise_tag,
};
pub use server::Server;
pub use store::Store;
pub use tree::{
    DEFAULT_PROJECT, Group, Member, PROJECT_KEY_LEN, is_project_key, project_key, project_labels,
    room_name, room_of_name, tree_order,
};
pub use wire::{Answer, ROOM_PROTOCOL_VERSION, Request, Response};
