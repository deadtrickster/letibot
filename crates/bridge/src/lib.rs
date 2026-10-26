//! **The Claude Code bridge** (`docs/claude-code-bridge.md`): leticode sessions as things
//! Claude Code holds rather than processes it runs once.
//!
//! ```text
//! Claude Code ──stdio MCP── letibot-mcp ──unix socket── harnessd (one per worktree folder)
//!      ▲                         │
//!      └── background `letibot-mcp wait …` exits when a session needs attention
//! ```
//!
//! Three crates cannot see each other and this one is where they meet:
//!
//! * [`daemon`] — which daemon belongs to which folder, and how to start one the way
//!   `scripts/letibot` does. The launcher's crate is a binary and has no library target, so
//!   the three facts it owns are spelled again here and asserted against its own values.
//! * [`head`] — the head protocol, discharged by a program: attach, ack, collect the turn,
//!   the last answer, the cards still open, the todos and the meter.
//! * [`tools`] — the ten verbs from the design's table, and the `wait` verb whose exit code
//!   is the wake-up.
//! * [`report`] — the brief's closing report, parsed into the fields a reviewer reads.
//!
//! [`mcp`] is the wire Claude Code speaks, and it is deliberately the thinnest layer of
//! them all: JSON-RPC over lines, three methods, no library.
//!
//! **Nothing here runs a model or links one.** A leticode turn is the daemon's, on the other
//! side of the socket; what the bridge does with a turn is watch it and answer for it.
//!
//! **And a session the bridge did not start is a session it can watch** (B6):
//! `leticode_attach` joins a folder's running daemon as a read-only head — the review queue's
//! own `s-gatekeeper`, most of the time — so the orchestrator sees which reviewer is on which
//! branch without reading the store.

pub mod daemon;
pub mod diff;
pub mod head;
pub mod mcp;
pub mod report;
pub mod tools;
