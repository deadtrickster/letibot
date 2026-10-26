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

pub mod activity;
pub mod client;
pub mod cursor;
pub mod event;
pub mod hub;
pub mod log;
pub mod metrics_tap;
// **The pane contract's document** (`docs/plugins-slice1.md`, C3): the shapes a `pane_<name>`
pub mod pane;
// **Where a daemon's tools run, written beside its socket** (`docs/remote-hosts.md`): one process
// fact the protocol does not carry, kept next to the claim that already says *this folder's daemon
// is me* — so a head about to reuse a daemon can tell whether it would run its caller's tools
// where the caller asked.
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
    COMPACTION_SECTIONS_KEY, COMPACTION_TEMPLATE, Citation, CompactionReport, CompactionSection,
    CompactionTail, CompactionTurn, Decider, DecisionOption, DecisionOutcome, DeltaTarget,
    Envelope, FinishReason, OnTimeout, OptionKind, PromptProgress, PromptReading, RESTORING_ROWS,
    SessionEvent, TARGET_MAX_BYTES, Timings, Usage, display_target,
};
pub use hub::{Attached, CommandKind, Delivery, Hub, QueuedCommand, Reply, SessionStatus};
pub use log::{LogBounds, SessionLog};
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
    // **The one definition of a row's `body`** (R19.2b), re-exported because the daemon's
    // store-backed reader answers the same question and must not answer it differently.
    body_of,
};
pub use warning::{Class, class, is_failure, is_routine};

#[cfg(feature = "turn")]
pub use lift::{LogSink, from_turn_event};

#[cfg(feature = "tools")]
pub use lift_tools::{ToolLogSink, from_tool_event};

/// **How a merge-queue gatekeeper's session title begins** — the first words of its brief
/// (`letibot_tools::gatekeeper::review_prompt`, asserted equal in `harnessd`), which a child's
/// title is cut from. A head reads it to keep the reviewer out of the subagents it counts: the
/// gatekeeper is the queue's, not a child the operator started. The role is the better mark and
/// a live `Subagent` event carries it; a row rebuilt from the session list has only the title.
pub const GATEKEEPER_TITLE_PREFIX: &str = "You are the gatekeeper.";

/// **How every daemon's review-queue session id begins** — one workspace's daemon per key
/// ([`gatekeeper_session`]), and nothing else in the tree starts with this.
pub const GATEKEEPER_SESSION_PREFIX: &str = "s-gatekeeper";

/// How many hex characters a workspace key is — `launcher::key`'s twelve, and the length
/// [`is_gatekeeper_session`] reads to tell a review-queue session from a reviewer child of one.
const WORKSPACE_KEY_LEN: usize = 12;

/// **The review-queue session id of the daemon serving `workspace`** — `s-gatekeeper-<key>`.
///
/// Session ids are `s-<now_ns>` — unique per creation and meaningless. This one is the opposite
/// within a folder and never across them: a daemon that restarts comes back to the SAME id (so
/// its board and its review rows survive the restart), and a second folder's daemon gets an id of
/// its own. The last part is the whole of the defect this replaces: the id used to be the
/// constant `s-gatekeeper` in every daemon, every daemon on the box shares one store, and two of
/// them appending to one transcript is silent corruption — the store's append-only trigger knows a
/// row must follow the one before it, and two writers do not agree about which row that is
/// (MEASURED 2026-10-26: `s-gatekeeper#t0`, `seq must be the next one`).
///
/// **It is derived from the workspace KEY** — the twelve hex characters `scripts/letibot` names
/// a folder's socket, its lock and its log with ([`workspace_key`]) — and not from the path
/// itself, because the key is already the box's one name for a folder's daemon and a second
/// spelling of *which folder* is exactly how two daemons come to disagree.
///
/// **It is spelled here, one crate below the daemon that owns the session**, because a session id
/// is the wire's business and a HEAD has to be able to name it. The bridge is that head:
/// `docs/claude-code-bridge.md`'s B6 watches the review queue from Claude Code, and it must say
/// *which session* without linking `harnessd` — which is the daemon, its providers and its wasm
/// JIT — to learn one string.
pub fn gatekeeper_session(workspace: &std::path::Path) -> String {
    gatekeeper_session_for_key(&workspace_key(workspace))
}

/// [`gatekeeper_session`] from a key the caller already has — the launcher and the bridge both
/// resolve a folder to its key before anything else, and a second hash of the same path is a
/// second chance to disagree about it.
pub fn gatekeeper_session_for_key(key: &str) -> String {
    format!("{GATEKEEPER_SESSION_PREFIX}-{key}")
}

/// **The key a folder's daemon is named by**: the first twelve hex characters of the SHA-256 of
/// its path, with nothing appended — `sha256sum | cut -c1-12`, which is what `scripts/letibot`
/// computes and what the socket, the lock and the log are named with.
///
/// Canonicalised first, because the daemon is started by the shell (`cd … && pwd -P`, then the
/// git toplevel) and a head may name the same folder by any path that reaches it; `/var/folders`
/// and `/private/var/folders` are one directory and two strings, and two strings are two ids.
pub fn workspace_key(workspace: &std::path::Path) -> String {
    use sha2::Digest;
    let path = std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    let mut h = sha2::Sha256::new();
    h.update(path.as_os_str().as_encoded_bytes());
    let digest = h.finalize();
    digest.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

/// **Whether `id` is any daemon's review-queue session.**
///
/// Two shapes, and only two: the per-daemon id ([`gatekeeper_session`]), and the constant
/// `s-gatekeeper` that every daemon shared before it. The second is recognised because a store
/// still holds one — the session and its transcripts are never rewritten or deleted
/// (`harnessd`'s `sessions::gatekeeper` says so at length) — and a reader that draws the queue's
/// board, or that names the session whose rows are a repo's queue, has to be able to see it for
/// what it was.
///
/// **A reviewer child of a review-queue session is not one of these** — a reviewer is
/// `s-gatekeeper-<key>-sub-<ns>`, whose suffix is longer than a key, and this reads the suffix
/// exactly rather than by prefix for that reason.
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
