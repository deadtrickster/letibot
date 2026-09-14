//! The flowy connector, as a **monitor**: a seat's inbox watched across turns
//! through the [`letibot_tools::exec::monitor::Condition`] seam, with a
//! per-room attention table, entity subscriptions, and a `flowy` tool to drive
//! them. `docs/flowy-monitor.md` is the long form; this is the map.
//!
//! # What problem each module answers
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
//! | a todo, an artifact, a thread somebody decided to watch | [`subs`] |
//! | the fabric's skills, through the `skill` tool | [`shelf`] |
//! | the shelf and the memories as a block in the system prompt | [`context`] |
//! | the tool the model drives all of it with | [`tool`] |
//!
//! # Persistent seats, temporary minds
//!
//! On this fabric a seat is a person-shaped thing: minted once by the operator,
//! named on the roster, holding **one** inbox reader that keeps its place in the
//! log across restarts. `lubuntu3-glm` is a seat. A letibot session is not — it
//! exists while project work happens, and its subagents exist for one task
//! inside it.
//!
//! So the two are different objects here, on purpose:
//!
//! - A [`seat::Seat`] is held by the **daemon**, for the daemon's life. It owns
//!   the reader, the local waiter claim, the presence the roster reports, and the
//!   spool. There is exactly one per name, and it is the thing `GET /api/presence`
//!   says is `listening`.
//! - A session **attaches** to the seat with its own [`attention::Attention`]
//!   table and gets an [`inbox::InboxCondition`] back, which is what its
//!   `flowy` monitor watches. Detaching does not touch the seat. Nothing said
//!   while no session was attached is lost: it is spooled, acked, kept as a
//!   backlog, and handed to the next session that attaches, labelled as such.
//! - A subagent never attaches. Everything it needs from the room reaches it
//!   through its parent, and everything it says goes out through its parent —
//!   there is one name and one mind speaking under it at a time.
//!
//! # What is flowy's and what is ours
//!
//! flowy's delivery rule — `wakesFor` in `internal/flowy/inbox.go` — has three
//! levels and two scopes, and its edges were each paid for by a real failure.
//! [`attention`] keeps every one of those **definitions**. What it drops is the
//! one-flag shape: the level is a table keyed by room, with a thread override,
//! because that is what a client that holds state can do and a per-poll query
//! string cannot. The one clause that failed in both directions on the fleet — a
//! person's unaddressed broadcast — is a named switch rather than a property of
//! "addressed".
//!
//! # Closed loop, §5: a stall is a declared state
//!
//! A seat that cannot reach the node has lost its encoder. It says so — once, as
//! a firing — refuses to `say` into the void while it lasts, keeps trying to
//! reattach on a short backoff, and reports the reattachment with a count of what
//! arrived meanwhile. It never looks like a quiet room.

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
pub use context::{FabricContext, Source as FabricSource};
pub use creds::{Credentials, Onboarding, Source};
pub use inbox::InboxCondition;
pub use seat::{Seat, SeatState};
pub use shelf::FabricShelf;
pub use subs::Subscription;
pub use tool::Flowy;
