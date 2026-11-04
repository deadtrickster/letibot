//! The session log and the head protocol (W7).
//!
//! # What this crate is
//!
//! The authoritative state of a session is an append-only event log with a
//! monotonic `seq`. Everything else - the snapshot, a head's screen, a
//! stored transcript - is a projection of it. This crate is the log, the
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
//! # The nine mechanics, and where each one lives
//!
//! | mechanic | where |
//! |---|---|
//! | the mark advances over everything READ, not everything kept | [`cursor`] |
//! | ack after the client has written the frame out | [`cursor::Batch::ack_after_render`] |
//! | report what was filtered | [`protocol::Ack`], [`scrub::ScrubReport`] |
//! | one authoritative reader, shared by many heads | [`hub`] - there is structurally only one |
//! | idle is quiet, not unwatched | [`hub::Hub::detach`] |
//! | bounded scrollback that discloses the drop | [`log::SessionLog::dropped`], present-and-zero |
//! | snapshot and register under one lock | [`hub::Hub::attach`] |
//! | non-blocking fan-out; drop the slow head and tell it | [`hub::Hub::publish`] → [`hub::Delivery::Resync`] |
//! | `agentscrub` generalises | [`scrub`], and it is a projection, not a filter |
//!
//! # What this crate deliberately does not do
//!
//! - No async runtime. The turn engine is a synchronous state machine over
//!   token ids and the whole workspace is testable with no executor; putting a
//!   runtime under the log to serve at most a handful of heads would be paying a
//!   dependency for a problem we do not have. Threads and a condvar are the whole
//!   concurrency story.
//! - No transport but Unix sockets. The remote head is WebSocket + TLS
//!   over *the same frames*; [`wire`] is deliberately generic over `Read`/`Write`
//!   so that head is a transport and not a protocol. It is not built here.
//! - No ACP adapter. It would be an adapter over the same vocabulary; the
//!   vocabulary is in [`event`] (`DecisionOption`, `option_id`, the four
//!   `OptionKind`s) so the adapter is a mapping when somebody writes it.

pub mod activity;
pub mod client;
pub mod cursor;
pub mod event;
pub mod hub;
pub mod log;
pub mod metrics_tap;
/// Who a session is for - the rule every list of sessions is grouped by. See [`origin`].
pub mod origin;
// The pane contract's document (`docs/plugins-slice1.md`, C3): the shapes a `pane_<name>`
pub mod pane;
// Which process connected, from the kernel: a head and a tool call run as the same user, so
// the socket's mode decides nothing and the peer's own pid is the fact. See [`peer`].
pub mod peer;
// Where a daemon's tools run, written beside its socket (`docs/remote-hosts.md`): the one
// process fact the protocol does not carry, so a head about to reuse a daemon can tell where its
// caller's tools would run.
pub mod placement;
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

pub use activity::{Activity, Waiting};
pub use cursor::{Batch, ReadMark};
pub use event::{
    COMPACTION_SECTIONS_KEY, COMPACTION_TEMPLATE, Citation, CompactionBudget, CompactionFidelity,
    CompactionLevel, CompactionReport, CompactionSection, CompactionTail, CompactionTurn, Decider,
    DecisionOption, DecisionOutcome, DeltaTarget, Envelope, FinishReason, OnTimeout, OptionKind,
    PromptProgress, PromptReading, RESTORING_ROWS, SessionEvent, TARGET_MAX_BYTES, Timings, Usage,
    display_target,
};
pub use hub::{
    Attached, CommandKind, Delivery, Hub, JournaledPrompt, PromptJournal, QueuedCommand, Reply,
    SessionStatus,
};
pub use log::{LogBounds, SessionLog};
pub use origin::{GroupRow, MCP_IDENTITY, SessionGroup, SessionOrigin, group_of};
pub use protocol::{
    Ack, Attachment, AttachmentKind, Caps, ClientFrame, HEAD_RUN_KIND_PATH, HEAD_RUN_KIND_TEXT,
    HEAD_RUN_KIND_URL, HEAD_RUN_TOOLS, HEAD_RUN_TOOLS_KEY, HeadRunTool, NOTE_STOPPING,
    PROTOCOL_VERSION, PaneBrief, ServerFrame, TermLine, head_run_tool, head_run_verb,
    operator_line_refusal, operator_shell_command, protocol_skew, send_line, term_command,
    term_line,
};
pub use question::{AnswerDefect, QuestionAnswer};
pub use registry::{
    Bell, CreateError, NagPause, PaneBoundary, PromptDriver, Registry, RowSource, SessionBrief,
    SessionWiring, ShellSuggester, StoredEnd, TerminalDriver,
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
    // The one definition of a row's `body`, re-exported because the daemon's
    // store-backed reader answers the same question and must not answer it differently.
    body_of,
};
pub use warning::{Class, class, is_failure, is_routine};

#[cfg(feature = "turn")]
pub use lift::{LogSink, from_turn_event};

#[cfg(feature = "tools")]
pub use lift_tools::{ToolLogSink, from_tool_event};

/// How a merge-queue gatekeeper's session title begins - the first words of its brief, which a
/// child's title is cut from. A head reads it to keep the reviewer out of the subagents it counts;
/// a row rebuilt from the session list has only the title.
pub const GATEKEEPER_TITLE_PREFIX: &str = "You are the gatekeeper.";

/// How the notes keeper's session title begins - its brief's first words, on
/// [`GATEKEEPER_TITLE_PREFIX`]'s rule. Here because the session list reads it, and the spelling is
/// held together by a test in the one crate that sees both (`letibot-tui`).
pub const NOTES_KEEPER_TITLE_PREFIX: &str = "You are the notes keeper.";

/// The seat a review is recorded under - `Seat::Gatekeeper`'s store spelling. Here because a
/// crate below the daemon groups sessions too, and the mark that says *this row is a review* has to
/// be one string: `is_conversation` and `crate::origin` both read it.
pub const GATEKEEPER_SEAT: &str = "gatekeeper";

/// How every daemon's review-queue session id begins - one workspace's daemon per key
/// ([`gatekeeper_session`]), and nothing else in the tree starts with this.
pub const GATEKEEPER_SESSION_PREFIX: &str = "s-gatekeeper";

/// How many hex characters a workspace key is - `launcher::key`'s twelve, and the length
/// [`is_gatekeeper_session`] reads to tell a review-queue session from a reviewer child of one.
const WORKSPACE_KEY_LEN: usize = 12;

/// The review-queue session id of the daemon serving `workspace` - `s-gatekeeper-<key>`. It is derived
/// from the workspace key, so two daemons never append to one transcript, and a head can name it.
pub fn gatekeeper_session(workspace: &std::path::Path) -> String {
    gatekeeper_session_for_key(&workspace_key(workspace))
}

/// [`gatekeeper_session`] from a key the caller already has - the launcher and the bridge both
/// resolve a folder to its key before anything else, and a second hash of the same path is a
/// second chance to disagree about it.
pub fn gatekeeper_session_for_key(key: &str) -> String {
    format!("{GATEKEEPER_SESSION_PREFIX}-{key}")
}

/// The key a folder's daemon is named by: the first twelve hex characters of the SHA-256 of its
/// canonical path, which is what `scripts/letibot` computes. Canonicalised first, because
/// `/var/folders` and `/private/var/folders` are one directory and two strings.
pub fn workspace_key(workspace: &std::path::Path) -> String {
    use sha2::Digest;
    let path = std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    let mut h = sha2::Sha256::new();
    h.update(path.as_os_str().as_encoded_bytes());
    let digest = h.finalize();
    digest.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

/// Whether `id` is any daemon's review-queue session. Two shapes: the per-daemon id, and the
/// constant `s-gatekeeper` that a store still holds. A reviewer child's suffix is longer than a key,
/// so this reads the suffix exactly rather than by prefix.
pub fn is_gatekeeper_session(id: &str) -> bool {
    // The id every daemon shared before the ids were per-workspace is the PREFIX itself, and
    // this build never opens it: it is recognised so a store that holds one can still be read.
    if id == GATEKEEPER_SESSION_PREFIX {
        return true;
    }
    match id
        .strip_prefix(GATEKEEPER_SESSION_PREFIX)
        .and_then(|rest| rest.strip_prefix('-'))
    {
        Some(key) => key.len() == WORKSPACE_KEY_LEN && key.bytes().all(|b| b.is_ascii_hexdigit()),
        None => false,
    }
}
