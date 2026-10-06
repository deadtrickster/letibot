//! The session log and the head protocol (W7): §4.5, §13.2, §13.2b.
//!
//! # What this crate is
//!
//! The authoritative state of a session is an **append-only event log with a
//! monotonic `seq`** (§13.2). Everything else — the snapshot, a head's screen, a
//! stored transcript — is a projection of it. This crate is the log, the
//! projections, the fan-out and the frames; it is not a daemon and it holds no
//! model.
//!
//! ```text
//!   turn engine ──emit──> LogSink ──publish──> Hub ─┬─> head (TUI)
//!                                                │   ├─> head (remote)
//!                                          SessionLog└─> head (flowy)
//!                                                │
//!                                          SessionView ──snapshot──> a late head
//! ```
//!
//! # The nine mechanics §13.2b insists on, and where each one lives
//!
//! | mechanic | where |
//! |---|---|
//! | the mark advances over everything READ, not everything kept | [`cursor`] |
//! | ack after the client has written the frame out | [`cursor::Batch::ack_after_render`] |
//! | report what was filtered | [`protocol::Ack`], [`scrub::ScrubReport`] |
//! | one authoritative reader, shared by many heads | [`hub`] — there is structurally only one |
//! | idle is quiet, not unwatched | [`hub::Hub::detach`] |
//! | bounded scrollback that discloses the drop | [`log::SessionLog::dropped`], present-and-zero |
//! | snapshot and register under one lock | [`hub::Hub::attach`] |
//! | non-blocking fan-out; drop the slow head and tell it | [`hub::Hub::publish`] → [`hub::Delivery::Resync`] |
//! | `agentscrub` generalises | [`scrub`], and it is a projection, not a filter |
//!
//! # What this crate deliberately does not do
//!
//! - **No async runtime.** The turn engine is a synchronous state machine over
//!   token ids and the whole workspace is testable with no executor; putting a
//!   runtime under the log to serve at most a handful of heads would be paying a
//!   dependency for a problem we do not have. Threads and a condvar are the whole
//!   concurrency story.
//! - **No transport but Unix sockets.** §13.4's remote head is WebSocket + TLS
//!   over *the same frames*; [`wire`] is deliberately generic over `Read`/`Write`
//!   so that head is a transport and not a protocol. It is not built here.
//! - **No ACP adapter.** §13.4 calls it an adapter over the same vocabulary; the
//!   vocabulary is in [`event`] (`DecisionOption`, `option_id`, the four
//!   `OptionKind`s) so the adapter is a mapping when somebody writes it.

pub mod client;
pub mod cursor;
pub mod event;
pub mod hub;
pub mod log;
pub mod protocol;
pub mod question;
pub mod registry;
pub mod scrub;
pub mod server;
pub mod suggest;
pub mod testing;
pub mod view;
pub mod warning;
pub mod wire;

#[cfg(feature = "turn")]
pub mod lift;

#[cfg(feature = "tools")]
pub mod lift_tools;

pub use cursor::{Batch, ReadMark};
pub use event::{
    COMPACTION_SECTIONS_KEY, COMPACTION_TEMPLATE, CompactionReport, CompactionSection,
    CompactionTail, CompactionTurn, Decider, DecisionOption, DecisionOutcome, DeltaTarget,
    Envelope, FinishReason, OnTimeout, OptionKind, PromptProgress, SessionEvent, TARGET_MAX_BYTES,
    Timings, Usage, display_target,
};
pub use hub::{Attached, CommandKind, Delivery, Hub, QueuedCommand, Reply, SessionStatus};
pub use log::{LogBounds, SessionLog};
pub use protocol::{
    Ack, Caps, ClientFrame, HEAD_RUN_KIND_PATH, HEAD_RUN_KIND_TEXT, HEAD_RUN_KIND_URL,
    HEAD_RUN_TOOLS, HEAD_RUN_TOOLS_KEY, HeadRunTool, NOTE_STOPPING, PROTOCOL_VERSION, ServerFrame,
    head_run_tool, head_run_verb, operator_shell_command, protocol_skew, term_command,
};
pub use question::{AnswerDefect, QuestionAnswer};
pub use registry::{
    Bell, CreateError, Registry, RowSource, SessionBrief, SessionWiring, ShellSuggester,
    TerminalDriver,
};
pub use scrub::{Projection, ScrubReport, StoredProjection, is_interactive};
pub use view::{
    CallState,
    CallView,
    OpenDecision,
    SessionView,
    SettledDecision,
    Snapshot,
    SnapshotItem,
    TurnState,
    TurnView,
    ViewBounds,
    // **The one definition of a row's `body`** (R19.2b), re-exported because the daemon's
    // store-backed reader answers the same question and must not answer it differently.
    body_of,
};
pub use warning::{Class, class, is_failure, is_routine};

#[cfg(feature = "turn")]
pub use lift::{LogSink, from_turn_event};

#[cfg(feature = "tools")]
pub use lift_tools::{ToolLogSink, from_tool_event};
