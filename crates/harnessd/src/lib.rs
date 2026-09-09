//! `harnessd` (T17): the daemon that assembles the parts into a working harness.
//!
//! Nine crates built the pieces. This one closes the loop between two ends that
//! were already tested and had nothing between them —
//! `ToolRuntime::transcript_item` and `ToolLogSink` — and gives the result a
//! socket, a store and a shutdown.
//!
//! ```text
//!         ┌──────────────────────── harnessd ────────────────────────┐
//!         │                                                          │
//!  head ──┼─ sessionlog::serve ── Hub ── take_command ── worker ──────┼── llama.cpp
//!         │                        │                       │         │   /completion
//!         │                     LogSink                 Harness      │
//!         │                   ToolLogSink        engine · tools      │
//!         │                                       ledger · store     │
//!         └──────────────────────────────────────────────────────────┘
//! ```
//!
//! # What it does
//!
//! * Opens one session over a content-addressed stable prefix, on the
//!   `/completion` token-array fallback (§5.6). No control channel: S0 is
//!   deliberately off the critical path.
//! * Runs §5.1's loop — render, tokenize, append, submit, stream, parse, execute,
//!   append, resubmit — with the append-only token ledger underneath it.
//! * Serves heads over a Unix socket, with the full §13.2 protocol (snapshot,
//!   resync, fan-out, scrub) because `letibot-sessionlog` already is that.
//! * Persists the transcript, the ledger rows and the token blobs to SQLite, where
//!   the append-only property is a database trigger rather than a convention.
//! * Acts on `finish_reason: length` (§5.7) and runs §18.1-I1's post-flight prefix
//!   assertion, gated on `BackendCaps::may_assert_structural_prefix()`.
//!
//! # What it does not do, and says so at startup
//!
//! Every one of these is printed by `Config::disclosures()` when the daemon comes
//! up, because a capability that is off and silent is indistinguishable from one
//! that is on and broken:
//!
//! * **Spill is off unless configured.** `NoBudget` is correct per D6 — an unset
//!   budget is a genuine no-op — so nothing spills until `--spill-inline` is given.
//! * **Retrieval is inert.** `ask_code` and `ask_corpus` abstain and nothing is
//!   behind them (T16.6, checked). No stub, because a stub that answered is
//!   precisely the failure §8.2 exists to prevent.
//! * **No adjudication.** Read-only tools never prompt (clause 4), so there is no
//!   boundary and no human in the loop. `Gate` is a parameter, not a constant, so
//!   W11 has a seam to absorb rather than a hard-coded `NoBoundary`.
//! * **No compaction, no fork, no subagents.** M2 and later.
//! * **One session per process.** `Hub` is per-session by construction and §13.2's
//!   multi-session daemon is M5's problem, not a thing to half-build here.
//!
//! # Two known gaps it works around rather than hides
//!
//! * **T13.1** — `TranscriptAppended` carries no content and `EventSink` has no
//!   channel for one, so a head cannot rebuild a conversation from the log alone.
//!   `Hub::record_item` is the out-of-band path and [`harness`] wires it, reading
//!   the item id off the event rather than recomputing it. The event is not
//!   widened here: that is a contract change and not this strand's call.
//! * **T13.5** — `DeltaTarget` has no channel for tool-call argument text, so
//!   arguments stream to a head as `Text` while the committed row puts them in a
//!   `ToolCall`. A head therefore shows the raw `<function=…>` block scrolling past
//!   and then a tidy row. Unfixed and visible; not papered over by filtering the
//!   deltas, which would make the live view and the stored view disagree in the
//!   other direction — the exact thing T12 fixed.

pub mod config;
pub mod daemon;
pub mod dialect;
pub mod harness;

pub use config::{Config, SpillPolicy, SpillStorage};
pub use daemon::{Daemon, Outcome};
pub use dialect::{Dialect, Wiring};
pub use harness::{Harness, HarnessError, HubSteering, Parts, Reply};
