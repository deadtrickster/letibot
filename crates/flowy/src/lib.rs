//! The flowy connector, as a monitor: a seat's inbox watched across turns through
//! the [`letibot_tools::exec::monitor::Condition`] seam, with a per-room attention
//! table, entity subscriptions, and a `flowy` tool to drive them.
//!
//! A seat is persistent and person-shaped - one inbox reader keeping its place in
//! the log - while a session is temporary and attaches with its own attention
//! table. Detaching does not touch the seat, and nothing said meanwhile is lost.
//! A stall is declared: the seat says so once and refuses to `say` into the void.

pub mod attention;
pub mod client;
pub mod context;
pub mod creds;
pub mod inbox;
pub mod render;
pub mod seat;
pub mod shelf;
pub mod spool;
pub mod subs;
pub mod tool;
pub mod waiter;

pub use attention::{Attention, Level};
pub use client::{Event, Node, NodeError};
pub use context::{FabricContext, Reach as FabricReach, Source as FabricSource};
pub use creds::{Credentials, Onboarding, Source};
pub use inbox::InboxCondition;
pub use seat::{Seat, SeatState};
pub use shelf::FabricShelf;
pub use subs::Subscription;
pub use tool::Flowy;
