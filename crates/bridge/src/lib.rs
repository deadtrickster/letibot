//! The Claude Code bridge: leticode sessions as things Claude Code holds rather than
//! processes it runs once.
//!
//! ```text
//! Claude Code ──stdio MCP── letibot-mcp ──unix socket── harnessd (one per worktree folder)
//!      ▲                         │
//!      └── background `letibot-mcp wait …` exits when a session needs attention
//! ```
//!
//! Three crates cannot see each other and this one is where they meet. [`daemon`] is which
//! daemon belongs to which folder and how to start one the way the launcher does; [`head`] is
//! the head protocol discharged by a program; [`tools`] is the verbs and the `wait` verb whose
//! exit code is the wake-up; [`report`] parses the brief's closing report; [`rows`] serves a
//! session's transcript rows without opening the store. [`mcp`] is the wire Claude Code speaks,
//! and deliberately the thinnest layer of them all.
//!
//! Nothing here runs a model or links one: a leticode turn is the daemon's, on the other side of
//! the socket, and what the bridge does with a turn is watch it and answer for it.

pub mod browser;
pub mod daemon;
pub mod diff;
pub mod fleet;
pub mod head;
pub mod holds;
pub mod mcp;
pub mod names;
pub mod plugins;
pub mod push;
pub mod queue;
pub mod refusals;
pub mod relay;
pub mod report;
pub mod rooms;
pub mod rows;
pub mod seat;
pub mod seat_jobs;
pub mod settings;
pub mod store;
pub mod tools;
