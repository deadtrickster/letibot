//! The flowy connector as a monitor: a seat's inbox watched across turns through the
//! [`letibot_tools::exec::monitor::Condition`] seam, with per-room attention, subscriptions and a `flowy` tool.
//! The seat is persistent; a session attaches with its own table. A stall is declared once.

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
