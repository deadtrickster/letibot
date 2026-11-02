//! The flowy connector, as a monitor: a seat's inbox watched across turns through the
//! [`letibot_tools::exec::monitor::Condition`] seam, with a per-room attention table, entity
//! subscriptions, and a `flowy` tool to drive them.
//!
//! | question | module |
//! |---|---|
//! | whose token, from where, and never the operator's | [`creds`] |
//! | the node's API, over the one HTTP client this tree has | [`client`] |
//! | one waiter per name, enforced against `flowy inbox` itself | [`waiter`] |
//! | what wakes a session, per room and per thread | [`attention`] |
//! | what was delivered survives the daemon dying | [`spool`] |
//! | the persistent identity that outlives every session | [`seat`] |
//! | the `Condition` a session's monitor watches | [`inbox`] |
//! | the tool the model drives all of it with | [`tool`] |
//!
//! A seat is person-shaped and minted once, held by the daemon for its life: it owns the reader, the
//! waiter claim and the spool, and there is one per name. A session attaches with its own attention
//! table and gets an [`inbox::InboxCondition`] back; detaching does not touch the seat, and nothing
//! said while no session was attached is lost, since it is spooled and handed over as a backlog. A
//! subagent never attaches: everything reaches it through its parent, because one name has one mind
//! speaking under it at a time.
//!
//! A seat that cannot reach the node has lost its encoder. It says so once as a firing, refuses to
//! `say` into the void while it lasts, retries on a short backoff and reports the reattachment with a
//! count of what arrived meanwhile. It never looks like a quiet room.

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
