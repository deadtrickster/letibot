//! The Claude Code bridge: leticode sessions as things Claude Code holds rather than
//! processes it runs once.
//!
//! ```text
//! Claude Code ──stdio MCP── letibot-mcp ──unix socket── harnessd (one per worktree folder)
//!      ▲                         │
//!      └── background `letibot-mcp wait …` exits when a session needs attention
//! ```
//!
//! Where [`daemon`] (which daemon serves a folder), [`head`] (the head protocol), [`tools`] (the
//! verbs, and `wait` whose exit is the wake-up) and [`mcp`] (the thin wire) meet. Nothing here runs
//! a model: the turn is the daemon's, and the bridge watches it and answers for it.

pub mod browser;
pub mod daemon;
pub mod diff;
pub mod exe_stamp;
pub mod fleet;
pub mod head;
pub mod holds;
pub mod mcp;
pub mod names;
pub mod plugins;
pub mod push;
pub mod queue;
pub mod queue_desk;
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
