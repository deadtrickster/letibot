//! The TUI head's state machine: frames in, a screen out.
//!
//! Deliberately separated from the terminal. [`App`] touches no file descriptor,
//! so the whole head — snapshot handling, resync, the read mark, the filter
//! accounting, the incremental markdown — is testable by feeding it frames and
//! reading `screen()`. The terminal ([`crate::term`]) is fifty lines of `termios`
//! on top.
//!
//! # The three §13.2b obligations a head owns
//!
//! 1. **Ack after rendering.** [`App::apply`] classifies; the driver writes the
//!    screen; only then does it ack. The type helps: `apply` returns a
//!    [`Disposition`] and has no way to send anything.
//! 2. **The mark covers everything read.** The driver acks
//!    `batch.last_seq()`, not "the last event I drew". A head at verbosity
//!    `Terse` displays almost nothing and still advances.
//! 3. **Say what was filtered.** The status line carries `filtered N`, and it is
//!    a running total, not a per-frame flash. *"Busy, and none of it was for me"*
//!    has to be readable, or a filter that suppresses everything looks exactly
//!    like an idle session.
//!
//! # Verbosity is this head's filter
//!
//! §12.2b makes per-room levels a harness concern; the same shape applies to a
//! head. [`Verbosity`] decides what reaches the transcript, and everything it
//! rejects is counted. That is what makes the counter meaningful rather than
//! decorative: there is a key that changes it, so the number moves.

use letibot_sessionlog::event::{DeltaTarget, Envelope, SessionEvent, Timings, Usage};
use letibot_sessionlog::protocol::ServerFrame;
use letibot_sessionlog::registry::{SessionBrief, SessionWiring, short_id};
use letibot_sessionlog::view::{
    CallState, OpenDecision, SettledDecision, Snapshot, SnapshotItem, TurnState, Warned,
};
use letibot_transcript::{TranscriptItem, UserPart};

use letibot_ui::editor::{Editor, Reaction};
use letibot_ui::style::{Painter, Role};
use letibot_ui::{card, diff::DiffConfig, progress, sidediff, width};

use crate::markdown::IncrementalMarkdown;
use crate::render::{
    BlockCache, Decor, RenderConfig, bytes_human, dur_human, sgr, trim_to, visible_width, wrap,
};

/// How much of the stream reaches the transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verbosity {
    /// Assistant text and tool outcomes only.
    Terse,
    /// Plus reasoning.
    Normal,
    /// Plus warnings, head arrivals, and who issued which command.
    Loud,
}

impl Verbosity {
    pub fn next(self) -> Verbosity {
        match self {
            Verbosity::Terse => Verbosity::Normal,
            Verbosity::Normal => Verbosity::Loud,
            Verbosity::Loud => Verbosity::Terse,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Verbosity::Terse => "terse",
            Verbosity::Normal => "normal",
            Verbosity::Loud => "loud",
        }
    }
}

/// What `apply` did with a frame. The driver counts these into the ack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// It changed the screen.
    Rendered,
    /// It was read and deliberately not shown. **Counted.**
    Filtered,
    /// Not an event: a Hello, a Resync, a command reply.
    Control,
}

/// Something the head wants the daemon to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Prompt(String),
    Interrupt(String),
    /// Move the running command to the background (Ctrl+B), like Claude Code.
    Promote,
    Answer {
        req_id: String,
        option_id: String,
        /// The glob typed after the option id, for an *always allow*. `None` means
        /// the daemon derives the pattern from the call, which is what every answer
        /// did before this existed.
        pattern: Option<String>,
    },
    Resync,
    /// Ask the daemon what sessions it holds.
    ListSessions,
    /// Ask for this session's todo list — the pane's bootstrap read.
    ListTodos,
    /// Make one. The head switches to it when the daemon says which id it minted;
    /// see [`App::apply`]'s `Sessions` arm.
    NewSession(String),
    /// Move this connection to another session.
    Switch(String),
    /// Read a subagent's output without leaving this session: the daemon answers
    /// with `Peeked`, and the pane the tree's Enter opens is built from it.
    /// Lazy — nothing is read until this is sent.
    Peek(String),
    /// Bring a session that is in the store but not in this daemon back to life.
    /// The head switches to it on the same `Sessions` reply a `NewSession` produces.
    ResumeSession(String),
    /// Name a session, or clear its name with an empty title.
    Rename { session_id: String, title: String },
    /// Compact the session this head is in: one summary turn, then the history
    /// is replaced by that summary through a transcript fork.
    Compact,
    /// Rebuild this conversation's prompt from the tools seated now, forking onto
    /// it. The only thing that changes a live session's tool list.
    Reseat,
    /// Move this session's project to a named point, persisted by the daemon.
    /// See `D13`.
    Mode { name: String },
    /// A command the daemon handles: `flowy …`, `models …`. The line minus `/`.
    Slash { line: String },
    /// A password for `sudo`, or a refusal. Never logged by anything on the way.
    Secret {
        req_id: String,
        secret: Option<String>,
    },
    Quit,
}

/// A key, decoded from the terminal.
///
/// Everything down to [`Key::Eof`] is one of `letibot_ui::editor::Key`'s and is
/// forwarded to the composer verbatim; the five below it are the head's own and
/// never reach it. Two enums rather than one because the composer is a library
/// that knows nothing about folds, and the head is a program that must not own a
/// keymap for word motion.
///
/// No longer `Copy`: [`Key::Paste`] carries the paste, because the whole point of
/// bracketed paste is that three thousand characters are **one** key and not
/// three thousand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    Char(char),
    /// A bracketed paste, arriving whole.
    Paste(String),
    Enter,
    /// Alt+Enter: a newline that does not submit.
    SoftEnter,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    WordLeft,
    WordRight,
    Home,
    End,
    KillToEnd,
    KillToStart,
    KillWordBack,
    Yank,
    Undo,
    Redo,
    Esc,
    CtrlC,
    Eof,
    /// Fold or unfold the model's reasoning.
    CtrlR,
    /// Fold or unfold tool output.
    CtrlT,
    /// Show or hide the raw, unparsed text of tool calls.
    CtrlX,
    /// Repaint from scratch.
    CtrlL,
    /// Open or close the session picker.
    CtrlS,
    /// Open or close the todos pane: the session's plan and the repo's queue.
    CtrlP,
    /// Open or close the subagent tree: the subagents this session spawned.
    CtrlG,
    /// Move the running command to the background (Ctrl+O, like Claude Code's
    /// Ctrl+B — B is the readline left-arrow here).
    CtrlO,
    /// Open or close the background-jobs pane: the jobs this session started.
    CtrlQ,
    PageUp,
    PageDown,
    /// Mouse wheel up, decoded from the SGR mouse protocol. Scrolls the
    /// transcript back; drags and motion are decoded and dropped, because the
    /// terminal's own Shift+drag is what selects.
    WheelUp,
    WheelDown,
    /// Tab: complete the `/command` being typed.
    Tab,
    /// A left-button press, 0-based screen coordinates. An open picker takes
    /// it: the row under the pointer becomes the selected row, and Enter still
    /// does the switching — select and confirm stay two acts.
    Click { x: u16, y: u16 },
}

impl Key {
    /// The composer's key, when this is one of its.
    fn composer(&self) -> Option<letibot_ui::editor::Key> {
        use letibot_ui::editor::Key as E;
        Some(match self {
            Key::Char(c) => E::Char(*c),
            Key::Paste(s) => E::Paste(s.clone()),
            Key::Enter => E::Enter,
            Key::SoftEnter => E::SoftEnter,
            Key::Backspace => E::Backspace,
            Key::Delete => E::Delete,
            Key::Left => E::Left,
            Key::Right => E::Right,
            Key::Up => E::Up,
            Key::Down => E::Down,
            Key::WordLeft => E::WordLeft,
            Key::WordRight => E::WordRight,
            Key::Home => E::Home,
            Key::End => E::End,
            Key::KillToEnd => E::KillToEnd,
            Key::KillToStart => E::KillToStart,
            Key::KillWordBack => E::KillWordBack,
            Key::Yank => E::Yank,
            Key::Undo => E::Undo,
            Key::Redo => E::Redo,
            Key::Esc => E::Esc,
            Key::CtrlC => E::CtrlC,
            Key::Eof => E::Eof,
            Key::CtrlR
            | Key::CtrlT
            | Key::CtrlX
            | Key::CtrlL
            | Key::CtrlS
            | Key::CtrlP
            | Key::CtrlG
            | Key::CtrlO
            | Key::CtrlQ
            | Key::PageUp
            | Key::PageDown
            | Key::WheelUp
            | Key::WheelDown
            | Key::Tab
            | Key::Click { .. } => {
                return None;
            }
        })
    }
}

/// How much of a foldable thing is on the screen.
///
    /// Two states and a key that flips them, rather than a per-item toggle: the
    /// pointer here is a wheel, not a cursor — it scrolls and selects nothing — so a
    /// per-item affordance would still need a cursor mode, and a cursor mode is a
    /// second keymap for a head whose whole input surface is one line. The
    /// **discoverability** is bought instead by the fold's own header
    /// naming its key — `▸ thinking · 18 lines · ctrl-r` — which is on the screen at
    /// the moment the operator wants it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fold {
    /// A title and a count. The default for reasoning, because the reasoning is
    /// working-out and the answer is the answer.
    Folded,
    /// Everything, bounded only by the render budget.
    Open,
}

impl Fold {
    fn flip(self) -> Fold {
        match self {
            Fold::Folded => Fold::Open,
            Fold::Open => Fold::Folded,
        }
    }

    fn is_open(self) -> bool {
        self == Fold::Open
    }
}

/// One tool call as the head watches it happen.
///
/// A tuple until now, and the three things it did not keep are the three a person
/// looking at a running call wants: **how long it has been going**, **what it
/// last said**, and **how much came out**. All three were derivable —
/// `Envelope::ts` is on every event and `ToolProgress { note }` was being read and
/// dropped — and none of them had anywhere to go while a call rendered as one
/// A subagent this session spawned, as the latest `Subagent` event reported it.
/// The event is durable and replayed, so a late head rebuilds the same tree.
#[derive(Debug, Clone)]
struct SubagentState {
    session_id: String,
    /// `running` | `done` | `failed`.
    state: String,
    /// The subtask's first line, the same derivation the subagent's title uses.
    prompt: String,
    role: String,
}

/// A background job this session started, as the events report it.
///
/// The **start** comes off the `bash` call's own finish — a `ToolOutcome::
/// Backgrounded` names the handle as a field, not as text to parse — and the
/// command shown is the call's §4.1 display target, joined at draw time through
/// the call id, because the arguments reach a head with the transcript and not
/// with the event. The **end** comes off the durable `JobSettled` event the
/// daemon publishes when the job settles between turns. A row with no
/// settlement yet is running; that is the whole reason the event exists.
#[derive(Debug, Clone)]
struct JobRow {
    job: String,
    /// The call that backgrounded it, for the join to the command text. Empty
    /// when the start is beyond this head's window and only the settlement
    /// replayed — the row then says the command is unknown rather than guessing.
    call_id: String,
    /// How it came to be in the background: `asked`, `promoted`, or
    /// `promoted by NAME`. Empty on a settlement-only row, for the same reason.
    how: String,
    /// `JobState::word` once settled — `exited 0`, `killed by job_kill` — and
    /// empty while running. Deliberately the process's word, never "ok"/"error".
    state: String,
    produced: u64,
    elapsed_ms: u64,
}

/// What one subagent's Enter opens: its tool output, read out of the subagent's
/// own scrollback by a `Peek`, shown without moving the head out of the session
/// it is in. The pane behaves like a terminal — the tail shows by default,
/// arrows walk back toward the beginning — and the whole view is spilled to a
/// file, because a cap on the pane must not be a cap on the record.
#[derive(Debug, Clone)]
struct SubOut {
    session_id: String,
    /// One line per rendered row: a `· name — outcome` header per tool result,
    /// the payload verbatim under it, and the spill locators at the end.
    lines: Vec<String>,
    /// Lines hidden off the bottom. Zero is "following the tail"; the pane draw
    /// clamps it, because only the draw knows the visible height.
    scroll: usize,
    /// Where the whole view was spilled, when it was written.
    spill: Option<String>,
    /// Events that fell off the daemon's scrollback before this read — the same
    /// disclosure a `Hello` makes, because a peek is a replay.
    dropped: u64,
}

/// line. See `crates/ui/DESIGN.md` §2.3.
#[derive(Debug, Clone)]
struct CallRow {
    call_id: String,
    name: String,
    /// The §4.1 display target: the path, pattern or command line the call is
    /// about. Empty when the event carried none — a log recorded before the field
    /// existed, or a call first seen as `ToolStarted` — and the card then renders
    /// the verb alone rather than a guess.
    target: String,
    state: CallState,
    /// `Envelope::ts` of the proposal or the start, and of the finish. Zero means
    /// this call came out of a snapshot, which has no timestamps — and a duration
    /// invented from a clock the events were not measured against is worse than
    /// no duration, so that case renders as `card::Phase::Replayed`.
    started_ms: u64,
    ended_ms: u64,
    /// The most recent `ToolProgress { note }`.
    note: Option<String>,
}

#[derive(Debug, Default)]
struct TurnPane {
    turn_id: String,
    model: String,
    text: IncrementalMarkdown,
    reasoning: IncrementalMarkdown,
    text_cache: BlockCache,
    reasoning_cache: BlockCache,
    calls: Vec<CallRow>,
    /// The raw `<function=…>` markup of this turn's tool calls, as it arrives on
    /// `DeltaTarget::ToolCall`.
    ///
    /// Kept, never shown by default. The default view shows a pending affordance
    /// while it is being written and the settled card afterwards; this is what the
    /// raw chord reveals, and it is the reason the chord can tell the truth
    /// instead of re-deriving markup the head never saw.
    raw_call: String,
    /// True between `<tool_call>` and the `ToolCallProposed` that settles it.
    ///
    /// A separate fact from `raw_call.is_empty()`: a turn that has written one call
    /// and is now writing prose has a non-empty `raw_call` and is not in a call.
    writing_call: bool,
    progress: Option<letibot_sessionlog::event::PromptProgress>,
    state: Option<TurnState>,
    /// Transcript rows appended while this turn ran.
    ///
    /// The pane is the *live* view of a turn. Once the turn has ended and those
    /// rows carry their content, the transcript is authoritative and the pane is a
    /// duplicate of it — so the pane stands down and only its summary line
    /// survives. Without this the answer is on the screen twice, once in the wrong
    /// order, which is what the first run of `--demo` showed.
    appended: Vec<String>,
    /// How many of `calls` the transcript has already taken over.
    ///
    /// A round's tool-result rows are appended **in call order**, after every call
    /// in the round has been invoked (`harnessd::harness`, the `for call in &calls`
    /// loop, then one `append_items`). So the *n*th `tool_result` row of this turn
    /// is about `calls[n-1]`, exactly, with no id matching involved — which is the
    /// point, because the ids repeat.
    ///
    /// Everything below this index is on the screen already as a settled card with
    /// its payload under it, and drawing it a second time in the live pane is the
    /// wall the operator was looking at: eight `● Read …` rows above eight
    /// `▸ Read … · ok · N lines` rows, no added fact between them.
    settled_calls: usize,
    /// The `ts` of `TurnStarted`, and of the last event seen for this turn. The
    /// difference is how long the turn has been going, taken from the log's own
    /// clock rather than from a wall clock in the head — a head that reads a
    /// recorded session must show the same elapsed time as the one that watched it.
    started_ms: u64,
    last_ms: u64,
    /// Characters of visible answer so far. Not tokens: this head never sees a
    /// token count until `TurnFinished`, and printing a character count as though
    /// it were tokens is the kind of number that gets quoted back later.
    out_chars: usize,
    /// The `ts` of the first and last `Delta { target: Reasoning }`.
    ///
    /// `card::reasoning` renders `Thought for 4.2s`, and this is where the 4.2
    /// comes from — no engine change needed, only a head that keeps the two
    /// timestamps it was already being handed. Zero means the reasoning arrived
    /// in a snapshot and has no honest duration.
    think_started_ms: u64,
    think_last_ms: u64,
}

/// The head.
pub struct App {
    pub cfg: RenderConfig,
    pub verbosity: Verbosity,
    /// The two-panel before/after view for file-edit cards, on when the pane
    /// is wide enough to hold both. `/diff` flips it; the unified renderer is
    /// the fallback at every width, which is what makes the toggle safe to
    /// flip on a narrow terminal.
    pub diff_split: bool,
    session_id: String,
    head_id: String,
    /// The head id the daemon just handed out, for the driver to give the client.
    ///
    /// A `Switch` seats this connection as a *different head* in the new session,
    /// and a client that kept the old id would ack into a session it had left —
    /// which the hub would silently ignore, so the mark would stop advancing and
    /// nothing would say why.
    seated: Option<String>,
    /// What this session is talking to: `model · dialect · endpoint · workspace`.
    /// §4.4, arriving on `Hello`.
    wiring: SessionWiring,
    /// Every session the daemon holds, as of the last `Hello` or `Sessions` frame.
    sessions: Vec<SessionBrief>,
    /// Subagents this session has spawned, folded from the durable `Subagent`
    /// events. Keyed by session id: a `running` row becomes its `done` row.
    subagents: Vec<SubagentState>,
    /// Background jobs this session started, folded from the `Backgrounded`
    /// outcome on a tool finish and the durable `JobSettled` event. In the order
    /// they were backgrounded; a settlement folds into its row.
    jobs: Vec<JobRow>,
    /// Which picker row the cursor is on. Arrows move it, Enter takes it; it starts
    /// on the session this head is already in, so an untouched list answers Enter
    /// with a no-op rather than a surprise.
    picker_sel: usize,
    /// How many rows the picker block actually drew on the last screen: the
    /// two title lines plus the sessions that survived `truncate(room)`. A
    /// click is only trusted for a row this count proves was on screen — a
    /// click into the blank space below a truncated list must not select a
    /// session nobody can see.
    picker_rows_drawn: usize,
    /// The terminal height the last screen was composed for, so a click can
    /// redo the header-row arithmetic the screen did without a repaint.
    screen_rows: usize,
    /// A Tab-driven completion in progress: the prefix as typed, the candidate
    /// names it matched, and which one is current. Re-derived whenever the
    /// text no longer starts with the cached prefix; any other key leaves it
    /// alone, and the render only trusts a prefix that is still being typed.
    completion: Option<(String, Vec<&'static str>, usize)>,
    /// Actions produced by a *frame* rather than by a key: the switch that follows
    /// a session being created. Drained by the driver, which is the only thing that
    /// can send.
    queued: Vec<Action>,
    /// Prompts this head has sent that the transcript does not hold yet.
    ///
    /// A prompt sent while a turn runs is **queued as a follow-up user item**
    /// (§13.2), and the item is appended only at the next step boundary — which for
    /// a turn with no tool calls is the turn's end. Between the enter press and
    /// that append the words existed nowhere on the screen: the composer had
    /// handed them off, the hub had accepted them, and the operator was looking at
    /// a conversation that had swallowed a sentence they had just typed. It comes
    /// back at the boundary, so nothing is lost — but "not lost" and "visible" are
    /// different requirements, and this is the second one.
    ///
    /// Each entry renders at the tail of the body, marked `queued`, until a user
    /// row lands carrying exactly its text ([`App::record_item`]) or the session
    /// changes ([`App::load`]). It is this head's own queue, not the hub's: the
    /// hub's queue is not in a snapshot, and a `CommandIssued` carries no text, so
    /// a second head cannot show it — this is the one place the words are still
    /// held by the party that typed them.
    pending_prompts: Vec<String>,
    /// Set when this head asked for a session and is waiting to be told its id.
    want_new_session: bool,
    /// The last turn's `usage`, kept past the end of the turn so the header can
    /// say how much context this session is carrying while nothing is running.
    usage: Option<Usage>,
    /// The last turn's `timings`, kept for the same reason and shown beside it:
    /// the decode rate and the wall time the turn footer used to carry. They
    /// moved because the footer repeated the header's context and cache numbers
    /// next to them, and one fact on one screen twice is one fact rendered as a
    /// question — see `turn_footer` for what the footer kept.
    last_timings: Option<Timings>,
    items: Vec<SnapshotItem>,
    hist_lines: Vec<String>,
    hist_upto: usize,
    /// Where the walk stood just before it rendered each item: `hist_marks[k]` is
    /// the state at the top of the iteration that drew `items[k]`, so there is one
    /// per rendered row and `hist_marks.len() == hist_upto`.
    ///
    /// This is what makes "the history from row k on is stale" expressible. It was
    /// only ever sayable as "all of it": a row whose body arrived, and every
    /// turn-state transition, threw the whole rendered session away and re-lexed
    /// it — 135 full rebuilds of an 89-row session, measured on one replay — which
    /// is the §13.3 rule this head is built around, broken at the level above the
    /// lexer that was careful about it.
    hist_marks: Vec<HistMark>,
    hist_width: usize,
    /// The class of the last row the walk actually drew, so the next one knows
    /// whether a blank line belongs between them. The walk is incremental across
    /// frames, so this has to survive the frame that set it.
    hist_class: Option<RowClass>,
    turn: Option<TurnPane>,
    open: Vec<OpenDecision>,
    /// `sudo` in the session wants a password: the request, and what has been
    /// typed for it so far. Kept OUT of the composer, so it is never in the
    /// composer's history, never completed, never shown: the composer draws a
    /// dot per character while this is `Some`.
    secret: Option<SecretAsk>,
    secret_buf: String,
    /// Screen requests this head has not answered yet. Answered by the DRIVER,
    /// after the frame is built, with the rows it actually drew.
    screen_requests: Vec<String>,
    /// The terminal's full width at the last render, gutter included. See
    /// [`App::screen`].
    term_cols: usize,
    /// Which option of `open[0]` is highlighted.
    ///
    /// A permission prompt used to be answered by TYPING an option id or its first
    /// letter into the composer. That is a keymap the operator has to remember and a
    /// word they can mistype, on a prompt that appears mid-thought — reported twice as
    /// *"it wasn't a choice but something I have to type (and mistype) myself"*.
    ///
    /// Up/Down move this; Enter on an empty composer answers it. Typing still works,
    /// because a head driven by a script and the tests both use it, and because the
    /// first letter is faster than two arrow presses once you know the ladder.
    sel: usize,
    /// Things that happened *between* transcript rows and belong in the
    /// conversation: a guard that fired, a decision that settled.
    ///
    /// Each is anchored to the number of rows that existed when it arrived, so the
    /// history rebuild puts it back where it happened. They used to be pinned to
    /// the bottom of the body — the last three warnings sat above the status line
    /// forever, so a warning about turn three was still shoving turn nine up the
    /// screen — and a settled decision was recorded and then never rendered at all,
    /// which is the silence §13.2b says a refusal must not become.
    notes: Vec<(usize, Note)>,
    /// How many of those are already in `hist_lines`.
    note_upto: usize,
    heads: usize,
    /// Counters. Every one of these is on the status line, because a number a head
    /// keeps and does not show is a number nobody can act on.
    /// Transcript rows the history walk has rendered, ever — not rows in the
    /// session, rows *drawn*, so a row re-rendered ten times counts ten.
    ///
    /// The encoder for [`App::invalidate_history_from`]. "Did that turn re-render
    /// the whole session" is unanswerable after the fact, and a wall time is not
    /// something a test can assert on; a count is. On a session of `n` rows this
    /// is `O(n)`, and it was `O(n²)`.
    pub hist_renders: u64,
    pub seq: u64,
    pub dropped: u64,
    pub scrubbed: u64,
    pub resyncs: u64,
    pub rendered: u64,
    pub filtered: u64,
    /// Scroll offset from the bottom, in lines. 0 is "following the stream".
    pub scroll: usize,
    /// The composer. `letibot_ui::editor::Editor` — multi-line, with history, a
    /// kill ring, undo batching, a paste ledger and the two interrupt double-taps.
    /// It was a `String` and a character index, which is why there was no way to
    /// write a two-line prompt, recall the last one, or paste a stack trace
    /// without losing bytes.
    editor: Editor,
    /// Folds. Reasoning starts folded; tool output starts folded.
    pub reasoning: Fold,
    pub tools: Fold,
    /// Show tool calls in the raw, unparsed form the model wrote them in.
    ///
    /// **Off, and it is not a fold.** A fold hides something the reader already
    /// knows is there; this reveals markup that the default view is required never
    /// to show. The operator asked for both halves in one sentence — *"I want to
    /// save the ability to see raw tool calls but it should be behind some chord"*
    /// — and they are two different obligations: the raw form must be reachable,
    /// and it must not be what anybody sees by accident.
    pub raw_calls: bool,
    notice: Option<String>,
    /// Frames the notice has left. A notice that never expires becomes furniture,
    /// and the old one replaced the input line for the rest of the session.
    notice_ttl: u32,
    help: bool,
    /// The session picker, which is a screen like `help` rather than a mode with a
    /// cursor. Same argument as the folds: there is one input surface here and it
    /// is a line, so the affordance is *typing the number you can see* — which also
    /// means the picker needs no keymap of its own and works over a pipe.
    picker: bool,
    /// The todos pane, a screen like the picker: the session's plan (what the
    /// model last wrote through `todo_write`) and the repo's own queue
    /// (`TODO.md`, read-only here — an agent's plan and the operator's queue are
    /// different lists, and the pane says which is which).
    todos_pane: bool,
    /// The subagent tree pane, a screen like `todos`: the subagents this session
    /// spawned, their state and their prompt. `ctrl-g`.
    subagents_pane: bool,
    /// The background-jobs pane, a screen like the other two: the jobs this
    /// session started, running and settled. `ctrl-q`.
    jobs_pane: bool,
    /// Which subagent row the cursor is on. Arrows move it, Enter switches to that
    /// subagent's session — the same two acts the picker keeps separate.
    subagents_sel: usize,
    /// The output view one subagent's Enter opens, until Esc closes it.
    sub_out: Option<SubOut>,
    /// The subagent whose output was asked for and not yet answered. Esc cancels.
    sub_out_pending: Option<String>,
    /// The session's todo list, as the last `TodosUpdated` said it was. Seeded by
    /// the `Todos` reply when the pane first opens; carried forward by the events.
    todos: Vec<letibot_sessionlog::event::TodoEntry>,
    /// The repo's `TODO.md` as a section map, read once per pane-open. The file
    /// can be longer than the pane and is the operator's to edit; the map is what
    /// a pane can honestly show.
    repo_todos: Option<Vec<String>>,
    /// The head's own instrumentation, as a screen: `/status`.
    ///
    /// Every counter it shows was added because something was measured going
    /// wrong, and every one of them used to live on the **bottom border of the
    /// chat window** — `seq 907 · rendered 900 · filtered 1 (normal) · dropped 0 ·
    /// scrubbed 0 · resync 0 · s-1789023464202470853 h3`, in the operator's frame,
    /// on every frame, next to the thing they are typing into. That is the wrong
    /// place for a number that is zero: it costs a row of attention for ever in
    /// exchange for being noticed once.
    ///
    /// So the border keeps only the alarm — the counters that are *not* zero, in
    /// the attention role — and this screen keeps everything, with a line under
    /// each counter saying what it means. Reachable, which is the obligation, and
    /// not resident, which was never part of it.
    stats: bool,
    quit: bool,
    /// Set whenever a full repaint is wanted regardless of the diff.
    redraw: bool,
    /// Wall clock, fed in by the driver, and when this head last had anything from
    /// the daemon.
    ///
    /// **Received-at, not the event's `ts`.** The difference is what a stall is,
    /// and taking it from the event's own clock would measure the daemon's opinion
    /// of how long it had been quiet — which is exactly the number that is missing
    /// when the daemon has stopped talking. Zero means nobody has told this head
    /// what time it is, and then it says nothing about stalls rather than guessing.
    now_ms: u64,
    last_event_at: u64,
    /// The model this session is talking to, kept past the end of a turn.
    ///
    /// It lives on `TurnPane` because that is where the event carries it, and the
    /// composer's own line has to say what it is talking to when nothing is
    /// running — which is most of the time a person is looking at it. Since §4.4 it
    /// is also on `Hello`, so a head with no turn yet has an answer too.
    model: String,
    /// §4.1's display target, by call id, **for the round the history walk is
    /// currently inside** — and for no other.
    ///
    /// # Why this is not session-scoped, which is what it used to be
    ///
    /// A call id is positional *within one round of one turn*:
    /// `letibot_turn::items` assigns `format!("call_{}", calls.len())` when the
    /// wire format carries no id, so every round of every turn starts again at
    /// `call_0`. This table was keyed on the id alone and kept for the life of the
    /// session, which means the fourteen rounds of a long turn all wrote to the
    /// same three keys — and every settled card then read back whichever round
    /// happened to write last.
    ///
    /// What that looked like on the screen, from the operator's capture:
    /// `▸ Read */Cargo.toml · ok · 3 lines` above a body reading
    /// `pub fn longest_common_prefix(…)`. The payload was the right one; the label
    /// was another round's. A head that names a file the tool never opened is
    /// telling the operator something false about what a tool returned, which is
    /// the defect class this repo exists against — so the fix is not to widen the
    /// key but to stop the table outliving the thing it describes.
    ///
    /// It is therefore **replaced wholesale** every time the walk reaches an
    /// `Assistant` row, in transcript order, and a `ToolResult` that finds no
    /// entry renders its correlation id rather than a neighbour's path.
    call_targets: std::collections::HashMap<String, String>,
    /// How long the call behind a settled `tool_result` row took, by **item id**.
    ///
    /// The one fact the live card had that the transcript row does not: a
    /// `TranscriptItem::ToolResult` carries no timestamps at all. Without this the
    /// only way to keep "that grep took 4.1 s" on the screen was to keep the live
    /// card beside the settled one, which is the duplication being removed.
    ///
    /// Keyed by item id, which is unique per row — unlike the call id, which is
    /// not. Absent for a row this head did not watch run (a snapshot, a `--replay`
    /// of a log recorded elsewhere), and the card then shows no duration rather
    /// than a fabricated one, which is the same rule as `card::Phase::Replayed`.
    call_ms: std::collections::HashMap<String, u64>,
    /// Both sides of the file a settled `edit`/`write` row changed, by **item
    /// id** — carried across the takeover exactly as `call_ms` is, and for the
    /// same reason: the transcript row has the tool's prose and not the pair.
    ///
    /// This is what puts a diff on the screen at all. The two-panel view used to
    /// be drawn only by the LIVE card, and the transcript takes a call over the
    /// moment its result row lands — so the diff existed for the milliseconds
    /// between `ToolFinished` and `TranscriptAppended`, and the operator, who
    /// asked for it twice, reported *"nothing really shown"*. Absent for a row
    /// this head did not watch run, and the row then shows the tool's own
    /// text, which is the `Replayed` rule again.
    call_edits: std::collections::HashMap<String, letibot_sessionlog::event::ToolEdit>,
    /// The total body length of the last frame, so `Up` can be clamped to it.
    body_len: usize,
    /// Where the terminal's caret belongs, from the last frame.
    cursor: Option<(usize, usize)>,
}

/// A run of body lines: history is **borrowed** from the head's own buffer, the
/// live tail is owned and rebuilt. See [`App::screen`] for why this is not one
/// `Vec<String>`.
enum Seg<'a> {
    Borrowed(&'a [String]),
    Owned(Vec<String>),
}

impl Seg<'_> {
    fn len(&self) -> usize {
        match self {
            Seg::Borrowed(s) => s.len(),
            Seg::Owned(v) => v.len(),
        }
    }

    fn get(&self, i: usize) -> &str {
        match self {
            Seg::Borrowed(s) => &s[i],
            Seg::Owned(v) => &v[i],
        }
    }
}

/// Lines `[start, end)` of the concatenation, and only those.
fn take_window(segs: &[Seg<'_>], start: usize, end: usize) -> Vec<String> {
    let mut out = Vec::with_capacity(end.saturating_sub(start));
    let mut base = 0usize;
    for s in segs {
        let n = s.len();
        let lo = start.saturating_sub(base);
        if base + n > start && base < end {
            let hi = (end - base).min(n);
            for i in lo..hi {
                out.push(s.get(i).to_string());
            }
        }
        base += n;
        if base >= end {
            break;
        }
    }
    out
}

/// The commands the composer completes, in the order Tab offers them. Aliases
/// (`s`, `q`, `h`, …) are deliberately absent: this list is what Tab offers
/// and what the live line shows, and offering both spellings doubles the list
/// to teach the same actions. `command()` still takes the short forms.
/// The opening delimiter of a screen sent by `/cells`, and its closing one.
///
/// A marker rather than a sentence because two readers need the edges: the model,
/// to know where the operator's words stop and the picture starts, and this head,
/// to fold a copy of its own screen out of its own transcript — see
/// [`fold_cells`]. Kept here beside the command that writes them so the pair
/// cannot drift.
const CELLS_OPEN: &str = "\u{27e6}screen ";
const CELLS_MARK_END: &str = "\u{27e7}";
const CELLS_CLOSE: &str = "\u{27e6}end screen\u{27e7}";

const SLASH_COMMANDS: &[(&str, &str)] = &[
    ("new", "TITLE — start a fresh session"),
    ("sessions", "the session picker"),
    ("switch", "ID — go to another session"),
    ("rename", "NAME — name the session you are in"),
    ("help", "the key and command reference"),
    ("status", "the bottom border's telemetry, full screen"),
    ("think", "fold or unfold the model's reasoning"),
    ("tools", "fold or unfold tool output"),
    ("verbosity", "cycle the event-stream detail"),
    ("diff", "toggle the two-panel file-edit diff"),
    ("jobs", "open or close the background-jobs pane"),
    ("cells", "MESSAGE — send it with a copy of this screen"),
    ("compact", "summarise this session and fork it"),
    ("reseat", "rebuild the prompt from the tools seated now"),
    ("interrupt", "stop the running turn"),
    ("quit", "leave the head"),
];

impl App {
    pub fn new(cfg: RenderConfig) -> Self {
        App {
            cfg,
            verbosity: Verbosity::Normal,
            diff_split: true,
            session_id: String::new(),
            head_id: String::new(),
            seated: None,
            wiring: SessionWiring::default(),
            sessions: Vec::new(),
            subagents: Vec::new(),
            jobs: Vec::new(),
            picker_sel: 0,
            picker_rows_drawn: 0,
            screen_rows: 0,
            completion: None,
            queued: Vec::new(),
            pending_prompts: Vec::new(),
            want_new_session: false,
            usage: None,
            last_timings: None,
            items: Vec::new(),
            hist_lines: Vec::new(),
            hist_upto: 0,
            hist_marks: Vec::new(),
            note_upto: 0,
            hist_width: 0,
            hist_class: None,
            turn: None,
            open: Vec::new(),
            secret: None,
            secret_buf: String::new(),
            screen_requests: Vec::new(),
            term_cols: 0,
            sel: 0,
            notes: Vec::new(),
            heads: 0,
            hist_renders: 0,
            seq: 0,
            dropped: 0,
            scrubbed: 0,
            resyncs: 0,
            rendered: 0,
            filtered: 0,
            scroll: 0,
            editor: Editor::new(),
            model: String::new(),
            call_targets: std::collections::HashMap::new(),
            call_ms: std::collections::HashMap::new(),
            call_edits: std::collections::HashMap::new(),
            reasoning: Fold::Folded,
            tools: Fold::Folded,
            raw_calls: false,
            notice: None,
            notice_ttl: 0,
            help: false,
            picker: false,
            todos_pane: false,
            subagents_pane: false,
            jobs_pane: false,
            subagents_sel: 0,
            sub_out: None,
            sub_out_pending: None,
            todos: Vec::new(),
            repo_todos: None,
            stats: false,
            quit: false,
            redraw: false,
            now_ms: 0,
            last_event_at: 0,
            body_len: 0,
            cursor: None,
        }
    }

    /// Tell the head what time it is. The driver calls this once a tick; nothing
    /// else in `App` reads a clock, so a test drives time by hand.
    pub fn clock(&mut self, now_ms: u64) {
        self.now_ms = now_ms;
    }

    /// Whether the head wants the terminal repainted from scratch (Ctrl-L, or a
    /// fold that changed every cached line). Reading it clears it.
    pub fn take_redraw(&mut self) -> bool {
        std::mem::take(&mut self.redraw)
    }

    pub fn should_quit(&self) -> bool {
        self.quit
    }

    pub fn head_id(&self) -> &str {
        &self.head_id
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// The head id the daemon just seated this connection with, once.
    ///
    /// The driver hands it to the client. It is `take`n rather than read because a
    /// client that re-applied a stale one would ack into the session it left.
    pub fn take_seated(&mut self) -> Option<String> {
        self.seated.take()
    }

    /// Actions a *frame* produced, for the driver to send. Empty almost always.
    pub fn take_actions(&mut self) -> Vec<Action> {
        std::mem::take(&mut self.queued)
    }

    /// How this session should be named on a screen: its title, or a short id.
    ///
    /// **Never the full id.** `s-1788987496351498881` is twenty-one characters of
    /// which the first thirteen are the same for every session minted on the same
    /// afternoon — it costs a fifth of an eighty-column header to say almost nothing,
    /// and the part that distinguishes two sessions is the part that gets cut when
    /// the header runs out of room. The last eight characters are where they differ,
    /// so that is what is shown.
    ///
    /// The full id is still reachable: it is on its own line under every named row in
    /// the picker, and `letibot --sessions` prints it in full. A label is for
    /// recognising a session; an id is for naming one to a command, and those are
    /// different jobs done in different places.
    fn session_label(&self, id: &str) -> String {
        self.sessions
            .iter()
            .find(|s| s.session_id == id)
            .filter(|s| !s.title.is_empty())
            .map(|s| s.title.clone())
            .unwrap_or_else(|| short_id(id))
    }

    /// Ask the daemon for a session as soon as this head is attached: resume it out
    /// of the store if it is not live, and switch to it either way.
    ///
    /// What `letibot --continue` and `letibot --session ID` turn into. It cannot be
    /// an `Attach` naming the id, because the daemon refuses an attach to a session
    /// it does not hold — correctly, since a typo must not seat you somewhere — and
    /// "not held yet" is exactly the state a resume is for. So the head attaches to
    /// wherever the daemon puts it and then asks, which is the same two steps the
    /// picker takes.
    pub fn request_session(&mut self, id: &str) {
        if id.is_empty() {
            return;
        }
        self.want_new_session = true;
        self.queued.push(Action::ResumeSession(id.to_string()));
    }

    /// Make a session and go there, as soon as this head is attached.
    ///
    /// `letibot --new [TITLE]` against a daemon that is already running.
    pub fn request_new_session(&mut self, title: &str) {
        self.want_new_session = true;
        self.queued.push(Action::NewSession(title.to_string()));
    }

    pub fn open_decisions(&self) -> &[OpenDecision] {
        &self.open
    }

    /// Apply one frame. Never sends anything; see the module note on acking.
    pub fn apply(&mut self, frame: ServerFrame) -> Disposition {
        match frame {
            // Once per *attachment*, which is now more than once per connection: a
            // `Switch` is answered with a second `Hello`, and this arm is the whole
            // of the head's switch path. That is the reason the daemon answers a
            // switch with a `Hello` rather than a frame of its own — the late-join
            // path is the best-tested path in this head, and a second one that
            // "also seats you somewhere" is a second one to keep in step.
            ServerFrame::Hello {
                session_id,
                head_id,
                dropped,
                snapshot,
                scrubbed,
                wiring,
                sessions,
                ..
            } => {
                let moved = !self.session_id.is_empty() && self.session_id != session_id;
                self.head_id = head_id.clone();
                self.seated = Some(head_id);
                self.wiring = wiring;
                // Subagents are not sessions a picker lists: they are children of this
                // session, shown in the subagent tree (`ctrl-g`), and reached by
                // `/switch id` rather than by cluttering the flat list.
                self.sessions = sessions
                    .into_iter()
                    .filter(|s| s.parent_session_id.is_none())
                    .collect();
                self.dropped += dropped;
                self.scrubbed += scrubbed.total();
                // `session_id` is assigned by `load` and **not before it**: `load`
                // decides whether this is the same session by comparing the two,
                // and assigning first made that comparison always true — so a
                // switch kept the previous session's token count and model on the
                // header, over the new session's empty transcript. Seen under tmux:
                // a brand-new session claiming `4470 ctx · 34% cached`.
                match snapshot {
                    Some(s) => self.load(*s),
                    // A resume served from the scrollback: no snapshot, and the
                    // state that is already here is this session's.
                    None => self.session_id = session_id,
                }
                if moved {
                    // The picker is closed by arriving, not by the key that opened
                    // it: the switch is the answer to the question the picker
                    // asked, and leaving it up over the session you just joined is
                    // a screen the operator has to dismiss for no reason.
                    self.picker = false;
                    self.say(&format!("switched to {}", self.session_label(&self.session_id)));
                }
                Disposition::Control
            }
            ServerFrame::Sessions {
                sessions,
                current,
                created,
            } => {
                self.sessions = sessions
                    .into_iter()
                    .filter(|s| s.parent_session_id.is_none())
                    .collect();
                self.session_id = current;
                match created {
                    // A session was made *because this head asked*. Going there is
                    // what was meant — `/new` that leaves you where you were is a
                    // command whose effect is invisible.
                    Some(id) if self.want_new_session => {
                        self.want_new_session = false;
                        self.queued.push(Action::Switch(id));
                    }
                    Some(id) => self.say(&format!("session {id} created")),
                    // **Not** `self.picker = true`. A `Sessions` frame is the answer
                    // to three different questions — a list, a rename, and a switch
                    // to the session you are already in — and only the first of them
                    // wants a picker. Opening it here put the session list over the
                    // screen after `/rename`, which the operator then had to dismiss
                    // to see the header they had just changed. The key and the
                    // command that ask for a list already open it themselves.
                    None => {}
                }
                self.redraw = true;
                Disposition::Control
            }
            // The bootstrap read for the todos pane. The session named is the one
            // the daemon answered for; a head that has since switched keeps what
            // it has until the pane is opened again, which re-asks.
            ServerFrame::Todos { session_id, todos } => {
                if session_id == self.session_id {
                    self.todos = todos;
                    self.redraw = true;
                }
                Disposition::Control
            }
            // The answer to the tree's Enter: the named subagent's scrollback,
            // scrubbed as a replay. Read, never folded — these events are not
            // this session's history, and folding them would lie about whose
            // turn is whose. The pane shows the tool results; the whole view is
            // spilled to a file so no cap on the pane is a cap on the record.
            ServerFrame::Peeked {
                session_id,
                dropped,
                events,
            } => {
                self.sub_out_pending = None;
                let mut lines = subagent_out_lines(&events);
                if lines.is_empty() {
                    lines.push("    no tool output in this subagent's scrollback.".to_string());
                }
                let spill = spill_sub_out(&session_id, &lines);
                self.sub_out = Some(SubOut {
                    session_id,
                    lines,
                    scroll: 0,
                    spill,
                    dropped,
                });
                self.redraw = true;
                Disposition::Rendered
            }
            // Only ever written to an `askpass` head; a TUI that sees one has a
            // daemon confused about who it is talking to.
            ServerFrame::Secret { .. } => Disposition::Control,
            ServerFrame::Resync {
                reason,
                dropped,
                snapshot,
                scrubbed,
            } => {
                self.resyncs += 1;
                self.dropped += dropped;
                self.scrubbed += scrubbed.total();
                self.notice = Some(format!("resync: {reason}"));
                self.load(*snapshot);
                Disposition::Control
            }
            ServerFrame::Event(env) => {
                self.seq = env.seq;
                self.last_event_at = self.now_ms;
                let ts = env.ts;
                let d = self.event(env.event, ts);
                match d {
                    Disposition::Rendered => self.rendered += 1,
                    Disposition::Filtered => self.filtered += 1,
                    Disposition::Control => {}
                }
                d
            }
            ServerFrame::Accepted { note, .. } => {
                // Telling the person who just pressed enter that their prompt was
                // accepted is not information — and the old head left exactly that
                // sitting on the input line for the rest of the session. Anything
                // *other* than the routine acceptance still gets said.
                if note != letibot_sessionlog::protocol::NOTE_PROMPT_QUEUED {
                    self.say(&note);
                }
                Disposition::Control
            }
            ServerFrame::Rejected {
                reason,
                expected_seq,
                actual_seq,
                ..
            } => {
                // Both numbers, so the operator can see what they were looking at.
                // A peek answered with one of these also ends its waiting.
                self.sub_out_pending = None;
                self.say(&format!(
                    "rejected: {reason} (you saw {expected_seq}, the session is at {actual_seq})"
                ));
                Disposition::Control
            }
            ServerFrame::Bye { reason } => {
                self.say(&format!("daemon: {reason}"));
                self.quit = true;
                Disposition::Control
            }
        }
    }

    /// Replace all state from a snapshot. This is the late-join path and the
    /// resync path; they are the same path, which is why resync is not special.
    fn load(&mut self, s: Snapshot) {
        // Everything session-scoped goes, not just the transcript. A snapshot is a
        // *replacement*, and this is also the switch path: carrying the previous
        // session's model name or a tool target keyed by a call id that only
        // existed over there is how a switched head shows the right conversation
        // with the wrong facts attached to it.
        if self.session_id != s.session_id {
            self.call_targets.clear();
            self.call_ms.clear();
            self.call_edits.clear();
            self.usage = None;
            self.last_timings = None;
            self.model.clear();
            self.turn = None;
            self.heads = 0;
            // The subagent tree is the PARENT's fact. Carried across a switch it
            // put "1 subagent running" on the composer of the very subagent being
            // looked at (measured 2026-09-16), and Enter in the pane there would
            // have switched to itself.
            self.subagents.clear();
            self.subagents_sel = 0;
            // The queue is the old session's. Whatever was queued there stays
            // queued *there* — the hub drains it into that session's transcript —
            // but this head is no longer looking at that session, and an echo of
            // words belonging to a conversation that is no longer on the screen is
            // the same lie a carried-over model name is.
            self.pending_prompts.clear();
        }
        self.session_id = s.session_id;
        self.seq = s.seq;
        self.dropped = self.dropped.max(s.dropped);
        // A resync of the *same* session keeps the queue — the hub's command
        // queue survives a resync, and a prompt queued behind a running turn is
        // still behind that turn — but anything the snapshot's transcript already
        // holds has landed, and its echo stands down the way `record_item` would
        // have stood it down had the row arrived live.
        for it in &s.items {
            if let Some(TranscriptItem::User { parts }) = &it.item
                && let Some(text) = parts.iter().find_map(|p| match p {
                    UserPart::Text { text } => Some(text.clone()),
                    _ => None,
                })
                && let Some(at) = self.pending_prompts.iter().position(|p| *p == text)
            {
                self.pending_prompts.remove(at);
            }
        }
        // The snapshot's in-flight calls are **not** seeded into `call_targets`.
        // They reach the screen as `TurnPane::calls`, which carries each call's own
        // target on the row that is about to draw it; putting them in an id-keyed
        // table as well is how a live `call_0` came to relabel a settled one.
        if let Some(TurnState::Finished { usage, timings, .. }) = s.turn.as_ref().map(|t| &t.state) {
            self.usage = Some(*usage);
            self.last_timings = Some(*timings);
        }
        self.items = s.items;
        self.invalidate_history();
        self.open = s.open_decisions;
        // A snapshot can replace the open set wholesale; keep the highlight in range.
        self.sel = 0;
        // Everything in a snapshot is history and none of it is anchored, so it
        // goes at the top rather than being invented a position among the rows.
        self.notes = s
            .warnings
            .into_iter()
            // Same rule as the live arm: `turn_failed` is the log's record of what
            // the turn's own terminal state already says on the screen. Filtering
            // it here as well is what stops a *snapshot* from putting it back —
            // which is exactly what happened the first time, and is the reason the
            // live path and the snapshot path have to agree about every filter.
            .filter(|w| w.code != "turn_failed")
            .map(|w| (0, Note::Warned(w)))
            .chain(s.settled_decisions.into_iter().map(|d| (0, Note::Decided(d))))
            .collect();
        self.note_upto = 0;
        self.heads = s.heads.len();
        self.turn = s.turn.map(|t| {
            self.model = t.model.clone();
            let mut pane = TurnPane {
                turn_id: t.turn_id,
                model: t.model,
                // No timestamps in a snapshot, so every one of these renders
                // without a duration rather than with a fabricated one.
                calls: t
                    .calls
                    .into_iter()
                    .map(|c| CallRow {
                        call_id: c.call_id,
                        name: c.name,
                        target: c.target,
                        state: c.state,
                        started_ms: 0,
                        ended_ms: 0,
                        note: None,
                    })
                    .collect(),
                progress: t.progress,
                state: Some(t.state),
                // Which rows this turn produced. Without it a head that joined late
                // cannot tell that the transcript already holds the answer, and
                // renders it twice — measured on a second head attached to a
                // finished turn, where the whole reply appeared above itself.
                appended: t.appended,
                out_chars: t.text.chars().count(),
                ..TurnPane::default()
            };
            // The snapshot carries the accumulated text **once**. Everything after
            // this is an increment. That is §13.3's wire half, arriving.
            pane.text.push(&t.text);
            pane.reasoning.push(&t.reasoning);
            // A head joining mid-call gets the markup too, so the raw chord shows
            // the same thing on a reattach as it does on the head that watched it.
            // `writing_call` stays false: a snapshot cannot say whether the block
            // is still open, and inventing a spinner that never stops is worse
            // than not showing one.
            pane.raw_call = t.raw_calls;
            pane
        });
        self.scroll = 0;
        self.redraw = true;
    }

    fn event(&mut self, e: SessionEvent, ts: u64) -> Disposition {
        if let Some(t) = self.turn.as_mut() {
            t.last_ms = ts.max(t.last_ms);
        }
        match e {
            // The name of the session this head is *in*. Folded into the row this
            // head already holds rather than triggering a `ListSessions` round trip:
            // the event carries the whole of the change, and asking the daemon to
            // resend a list to learn something it just told us is how a head ends up
            // one frame behind its own screen.
            SessionEvent::SessionRenamed { title } => {
                let id = self.session_id.clone();
                if let Some(row) = self.sessions.iter_mut().find(|s| s.session_id == id) {
                    row.title = title.clone();
                }
                self.redraw = true;
                // Said out loud, because the header changes under the operator and an
                // unexplained change of the one label that identifies where you are
                // is worse than no label.
                self.say(&format!("this session is now called {title:?}"));
                Disposition::Control
            }
            // The model revised its plan. The whole list, not a delta — keep the
            // latest and let the pane show it. Said only when the pane is open:
            // a line in the scrollback for every todo write would bury the work
            // the todos exist to organize, and the pane is where this state
            // lives.
            SessionEvent::TodosUpdated { todos } => {
                self.todos = todos;
                self.redraw = true;
                if self.todos_pane {
                    Disposition::Rendered
                } else {
                    Disposition::Filtered
                }
            }
            // A subagent spawn/finish. Fold into the tree, replacing the row with the
            // same session id, so `running` becomes `done` rather than a second line.
            SessionEvent::Subagent {
                subagent_id,
                state,
                prompt,
                role,
            } => {
                if let Some(row) = self
                    .subagents
                    .iter_mut()
                    .find(|s| s.session_id == subagent_id)
                {
                    row.state = state;
                    row.prompt = prompt;
                    row.role = role;
                } else {
                    self.subagents.push(SubagentState {
                        session_id: subagent_id,
                        state,
                        prompt,
                        role,
                    });
                }
                self.redraw = true;
                Disposition::Filtered
            }
            // A background job settled — the daemon publishes this between turns,
            // which is exactly when nothing else could say it. Fold into the row
            // the backgrounded finish pushed; a settlement whose start is beyond
            // this head's window still gets its row, with the command unknown,
            // because a job that ran is a fact even when its beginning scrolled
            // off.
            SessionEvent::JobSettled {
                job,
                state,
                produced,
                elapsed_ms,
            } => {
                if let Some(row) = self.jobs.iter_mut().find(|j| j.job == job) {
                    row.state = state;
                    row.produced = produced;
                    row.elapsed_ms = elapsed_ms;
                } else {
                    self.jobs.push(JobRow {
                        job,
                        call_id: String::new(),
                        how: String::new(),
                        state,
                        produced,
                        elapsed_ms,
                    });
                }
                self.redraw = true;
                Disposition::Filtered
            }
            SessionEvent::TurnStarted {
                turn_id,
                model,
                ledger_head: _,
            } => {
                // Asked *before* the pane is replaced. The rows that stop being
                // drawn live are the previous turn's, and once its pane is gone
                // there is nothing left to ask which they were.
                let stale_from = self.turn_first_row();
                self.model = model.clone();
                self.turn = Some(TurnPane {
                    turn_id,
                    model,
                    state: Some(TurnState::Running),
                    started_ms: ts,
                    last_ms: ts,
                    ..TurnPane::default()
                });
                // A new turn takes the pane away from the previous one, so the
                // previous one's rows now own everything they proposed. Same
                // reason as the terminal states above: the history is cached —
                // from the first row that pane owned, which is the only part of
                // it that can render differently now.
                if let Some(k) = stale_from {
                    self.invalidate_history_from(k);
                }
                Disposition::Rendered
            }
            SessionEvent::PromptProgress { progress, .. } => {
                if let Some(t) = self.turn.as_mut() {
                    t.progress = Some(progress);
                }
                // Shown on the status line, not in the transcript — which is what a
                // progress frame is for. It counts as rendered because it does
                // change the screen.
                Disposition::Rendered
            }
            SessionEvent::Delta {
                target,
                text,
                turn_id,
            } => {
                let Some(t) = self.turn.as_mut() else {
                    return Disposition::Filtered;
                };
                if t.turn_id != turn_id {
                    return Disposition::Filtered;
                }
                match target {
                    DeltaTarget::Text => {
                        t.out_chars += text.chars().count();
                        t.text.push(&text);
                        Disposition::Rendered
                    }
                    DeltaTarget::Reasoning => {
                        if self.verbosity >= Verbosity::Normal {
                            if t.think_started_ms == 0 {
                                t.think_started_ms = ts;
                            }
                            t.think_last_ms = ts;
                            t.reasoning.push(&text);
                            Disposition::Rendered
                        } else {
                            Disposition::Filtered
                        }
                    }
                    // Never `t.text`. This is the markup, and the whole point of
                    // the channel is that the default view does not show it — see
                    // `letibot_sessionlog::event::DeltaTarget`.
                    DeltaTarget::ToolCall => {
                        t.raw_call.push_str(&text);
                        t.writing_call = true;
                        Disposition::Rendered
                    }
                }
            }
            SessionEvent::ToolCallProposed {
                call_id,
                name,
                target,
                ..
            } => {
                // Not kept beyond the turn, and not put in `call_targets`. It used
                // to be, on the argument that a settled `Assistant { tool_calls }`
                // row needs the word and the proposal is where the head saw it —
                // but the row carries the arguments the word is derived from, and
                // the id it would be filed under is reused by the next round. The
                // proposal's target lives on the `CallRow` below, which is the row
                // that draws it, and dies with the turn that made it.
                if let Some(t) = self.turn.as_mut() {
                    // The proposal is the settled form of whatever was being
                    // written, so the pending affordance stands down here.
                    t.writing_call = false;
                    t.calls.push(CallRow {
                        call_id,
                        name,
                        target,
                        state: CallState::Proposed,
                        started_ms: ts,
                        ended_ms: 0,
                        note: None,
                    });
                }
                Disposition::Rendered
            }
            SessionEvent::ToolStarted { call_id, name, .. } => {
                if let Some(t) = self.turn.as_mut() {
                    match open_call(&mut t.calls, &call_id) {
                        Some(c) => {
                            c.state = CallState::Running;
                            // The clock starts when the tool starts, not when the
                            // model asked for it: a call that waited on a decision
                            // did not spend that time running.
                            c.started_ms = ts;
                        }
                        None => t.calls.push(CallRow {
                            call_id,
                            name,
                            // `ToolStarted` carries no target and none is invented.
                            target: String::new(),
                            state: CallState::Running,
                            started_ms: ts,
                            ended_ms: 0,
                            note: None,
                        }),
                    }
                }
                Disposition::Rendered
            }
            // Still not accumulated — partial tool output has no durable form, and
            // this event is interactive-only and scrubbed for a late head. But the
            // *latest* note is a fact about right now and a running card has a
            // place for it, which a one-line renderer did not: the old comment
            // ("nowhere to put it, by design") was true of the renderer, not of
            // the event.
            SessionEvent::ToolProgress { call_id, note, .. } => {
                match self
                    .turn
                    .as_mut()
                    .and_then(|t| open_call(&mut t.calls, &call_id))
                {
                    Some(c) => {
                        c.note = Some(note);
                        Disposition::Rendered
                    }
                    None => Disposition::Filtered,
                }
            }
            SessionEvent::ToolFinished {
                call_id,
                outcome,
                payload_digest,
                inline_bytes,
                full_bytes,
                spill,
                edit,
                ..
            } => {
                // A backgrounded call leaves a job behind, and the row the jobs
                // pane shows starts here: the handle is the outcome's own field,
                // not a parse of the result text. The command joins at draw time,
                // through the call's §4.1 target.
                if let letibot_transcript::ToolOutcome::Backgrounded { handle, how, .. } = &outcome
                {
                    self.jobs.retain(|j| j.job != *handle);
                    self.jobs.push(JobRow {
                        job: handle.clone(),
                        call_id: call_id.clone(),
                        how: how_word(how),
                        state: String::new(),
                        produced: 0,
                        elapsed_ms: 0,
                    });
                    self.redraw = true;
                }
                if let Some(t) = self.turn.as_mut()
                    && let Some(c) = open_call(&mut t.calls, &call_id)
                {
                    c.ended_ms = ts;
                    c.note = None;
                    c.state = CallState::Finished {
                        outcome,
                        payload_digest,
                        inline_bytes,
                        full_bytes,
                        spill,
                        edit,
                    };
                }
                Disposition::Rendered
            }
            SessionEvent::DecisionRequested {
                req_id,
                kind,
                call_id,
                summary,
                target,
                detail,
                options,
                choices,
                because,
                advice,
                deadline,
                on_timeout,
            } => {
                self.open.retain(|d| d.req_id != req_id);
                // A fresh question starts at the top of its ladder rather than
                // wherever the last one was left: the highlight must never be
                // somewhere the operator did not put it when Enter is one key away.
                self.sel = 0;
                self.open.push(OpenDecision {
                    req_id,
                    kind,
                    call_id,
                    summary,
                    target,
                    detail,
                    options,
                    choices,
                    because,
                    advice,
                    deadline,
                    on_timeout,
                    // Not `ts`. A head renders how long a decision has been waiting
                    // from the view's own stamp, and this arm is the live one — the
                    // snapshot path at `apply` carries the real `asked_ts`.
                    asked_ts: 0,
                });
                Disposition::Rendered
            }
            SessionEvent::DecisionAnswered {
                req_id,
                outcome,
                by,
                basis,
                late,
            } => {
                let summary = self
                    .open
                    .iter()
                    .find(|d| d.req_id == req_id)
                    .map(|d| d.summary.clone())
                    .unwrap_or_default();
                self.open.retain(|d| d.req_id != req_id);
                self.note(Note::Decided(SettledDecision {
                    req_id,
                    summary,
                    outcome,
                    by,
                    basis,
                    late,
                }));
                Disposition::Rendered
            }
            SessionEvent::TurnFinished {
                finish_reason,
                usage,
                timings,
                ..
            } => {
                // Kept on the head, not only on the pane: the session header says
                // how much context this conversation is carrying, and that question
                // is asked between turns, when the pane may have been superseded by
                // the transcript. The timings are kept with it — the header now
                // carries the turn's rate and duration too, which is why the footer
                // no longer does.
                self.usage = Some(usage);
                self.last_timings = Some(timings);
                if let Some(t) = self.turn.as_mut() {
                    t.progress = None;
                    t.state = Some(TurnState::Finished {
                        finish_reason,
                        usage,
                        timings,
                    });
                }
                // A terminal state can hand a call back to the transcript.
                // While the pane is drawing a turn, that turn's assistant rows do
                // not draw their own unsettled calls; once it stands down they
                // must, or a call the turn was interrupted in the middle of leaves
                // the screen with nothing said about it. The rendered history is
                // cached, so it has to be told — from the first row this pane
                // owns, which is the only part of it that can render differently
                // now.
                self.invalidate_turn_rows();
                Disposition::Rendered
            }
            // §4.5's terminal event, which did not exist. The head used to be told
            // a `Warning` and nothing else, so `TurnState` stayed `Running` and the
            // spinner span at a dead turn until somebody closed the window; the
            // "nothing received for 17.0s" line below is the *disclosure* that
            // covered for it, and it stays as the backstop for a genuine stall.
            SessionEvent::TurnFailed {
                turn_id,
                error,
                partial_kept,
            } => {
                if let Some(t) = self.turn.as_mut()
                    && (t.turn_id == turn_id || turn_id.is_empty())
                {
                    t.progress = None;
                    t.state = Some(TurnState::Failed {
                        error,
                        partial_kept,
                    });
                }
                // A terminal state can hand a call back to the transcript.
                // While the pane is drawing a turn, that turn's assistant rows do
                // not draw their own unsettled calls; once it stands down they
                // must, or a call the turn was interrupted in the middle of leaves
                // the screen with nothing said about it. The rendered history is
                // cached, so it has to be told — from the first row this pane
                // owns, which is the only part of it that can render differently
                // now.
                self.invalidate_turn_rows();
                Disposition::Rendered
            }
            SessionEvent::TurnInterrupted {
                reason,
                partial_kept,
                ..
            } => {
                if let Some(t) = self.turn.as_mut() {
                    t.progress = None;
                    t.state = Some(TurnState::Interrupted {
                        reason,
                        partial_kept,
                    });
                }
                // A terminal state can hand a call back to the transcript.
                // While the pane is drawing a turn, that turn's assistant rows do
                // not draw their own unsettled calls; once it stands down they
                // must, or a call the turn was interrupted in the middle of leaves
                // the screen with nothing said about it. The rendered history is
                // cached, so it has to be told — from the first row this pane
                // owns, which is the only part of it that can render differently
                // now.
                self.invalidate_turn_rows();
                Disposition::Rendered
            }
            SessionEvent::TranscriptAppended {
                item_id,
                kind,
                ledger_head,
            } => {
                // A tool-result row hands one live card over to the transcript.
                // Positional, not by id: the engine invokes a round's calls in
                // order and appends their rows in the same order, and the ids
                // repeat every round so there is nothing to match on. The duration
                // is carried across here because it is the only fact the live card
                // had that the row does not.
                let mut carried: Option<u64> = None;
                let mut carried_edit: Option<letibot_sessionlog::event::ToolEdit> = None;
                if let Some(t) = self.turn.as_mut() {
                    t.appended.push(item_id.clone());
                    if kind == "tool_result" {
                        let c = t.calls.get(t.settled_calls);
                        carried = c
                            .filter(|c| c.started_ms > 0 && c.ended_ms > c.started_ms)
                            .map(|c| c.ended_ms - c.started_ms);
                        // The pair rides across with the duration: same card, same
                        // moment, same positional match.
                        carried_edit = c.and_then(|c| match &c.state {
                            CallState::Finished { edit: Some(e), .. } => Some(e.clone()),
                            _ => None,
                        });
                        t.settled_calls += 1;
                    }
                }
                if let Some(ms) = carried {
                    self.call_ms.insert(item_id.clone(), ms);
                }
                if let Some(e) = carried_edit {
                    self.call_edits.insert(item_id.clone(), e);
                }
                self.items.push(SnapshotItem {
                    item_id,
                    kind,
                    ledger_head,
                    // When it happened, from the log's own clock. A head reading a
                    // recorded session must show the same times as the one that
                    // watched it, so this is never `now`.
                    ts,
                    item: None,
                });
                Disposition::Rendered
            }
            // The body for a row already announced. Before this existed, a head
            // that was attached when the row landed had no route to the content at
            // all and rendered `[kind id — content not loaded]` for the rest of the
            // session — including for the operator's own prompt.
            SessionEvent::TranscriptContent { item_id, item } => {
                self.record_item(&item_id, *item);
                Disposition::Rendered
            }
            SessionEvent::HeadAttached { .. } => {
                self.heads += 1;
                if self.verbosity >= Verbosity::Loud {
                    Disposition::Rendered
                } else {
                    Disposition::Filtered
                }
            }
            SessionEvent::HeadDetached { .. } => {
                self.heads = self.heads.saturating_sub(1);
                if self.verbosity >= Verbosity::Loud {
                    Disposition::Rendered
                } else {
                    Disposition::Filtered
                }
            }
            // Into the transcript, where it happened.
            //
            // It used to be pinned to the bottom of the body: the last three
            // warnings sat above the status line forever, so a warning about turn
            // three was still shoving turn nine up the screen, and the operator had
            // no way to say "seen". A warning is an event with a place in the
            // conversation, and putting it there is what makes it scroll away like
            // one — and still be there when you scroll back.
            SessionEvent::ScreenRequested { req_id } => {
                // Queued, not answered here: the answer is the rows this head
                // DRAWS, and they do not exist until the frame is built. The
                // driver takes these after `screen()` and sends exactly what it
                // put on the terminal — anything rendered here instead would be
                // a second rendering, which is the reconstruction this whole
                // frame exists to avoid.
                self.screen_requests.push(req_id);
                self.redraw = true;
                Disposition::Control
            }
            SessionEvent::SecretRequested {
                req_id,
                prompt,
                command,
                deadline,
            } => {
                self.secret = Some(SecretAsk {
                    req_id,
                    prompt,
                    command,
                    deadline,
                });
                self.secret_buf.clear();
                self.redraw = true;
                Disposition::Rendered
            }
            SessionEvent::SecretSettled { req_id, given, by } => {
                if self.secret.as_ref().is_some_and(|s| s.req_id == req_id) {
                    self.secret = None;
                    self.secret_buf.clear();
                }
                self.note(Note::Warned(Warned {
                    code: "sudo".into(),
                    detail: if given {
                        format!("password given by {by}")
                    } else {
                        format!("no password given ({by})")
                    },
                    ts,
                }));
                Disposition::Rendered
            }
            SessionEvent::Warning { code, detail } => {
                // `turn_failed` is the log's grep-able record of the same fact
                // `TurnFailed` puts under the turn, and the daemon publishes both
                // on purpose — one is state, the other is history. On a *screen*
                // they are the same sentence twice, three lines apart, so this head
                // renders the terminal state and counts the warning as filtered.
                // Counted, not dropped: the status line's `filtered` is what makes
                // "I chose not to show this" different from "nothing happened".
                if code == "turn_failed" {
                    return Disposition::Filtered;
                }
                self.note(Note::Warned(Warned { code, detail, ts }));
                Disposition::Rendered
            }
            SessionEvent::CommandIssued {
                head_id,
                identity,
                command,
                note,
                ..
            } => {
                // Two humans in one session: seeing who did what is the point — and
                // seeing *yourself* do what you just did is not. Our own routine
                // acceptances are already covered by `Accepted`.
                if head_id != self.head_id {
                    self.say(&format!("{identity} · {command}: {note}"));
                }
                if self.verbosity >= Verbosity::Loud {
                    Disposition::Rendered
                } else {
                    Disposition::Filtered
                }
            }
            // §6's plan is a document; the transcript is not where it goes.
            SessionEvent::Explain { .. } => Disposition::Filtered,
            // **Always rendered, at every verbosity.**
            //
            // `docs/boundary-and-adjudication.md` §4b: a denial the operator cannot
            // see manufactures the workaround, so there is no verbosity at which
            // hiding this is correct — it is a decision taken on their behalf and
            // only they can lift it. It goes into the transcript, in the place it
            // happened, beside the tool call it refused.
            //
            // This reuses `Note::Warned` rather than growing a note kind of its own:
            // making a denial *look* different from a warning is presentation, this
            // head is somebody else's this session, and a placeholder that rendered
            // nothing would be the invisible denial again with a different cause.
            // The `code` carries the distinction a reader needs, and
            // `repeat_count`/`breaker_open` are on the event for a head that later
            // wants to collapse repeats.
            SessionEvent::DenialRaised {
                request_id,
                tool,
                summary,
                by,
                basis,
                outcome,
                repeat_count,
                breaker_open,
                grant,
                ..
            } => {
                let repeat = if breaker_open {
                    format!(
                        " · breaker OPEN after {repeat_count} consecutive refusals; only you \
                         can lift it"
                    )
                } else if repeat_count > 1 {
                    format!(" · attempt {repeat_count} at the same task direction")
                } else {
                    String::new()
                };
                // **A refusal the harness made is not a question for the operator.**
                //
                // `boundary:` signs the deterministic ones — the normaliser could not
                // read the command, the host boundary refused it. Nobody was asked,
                // no grant lifts it, and the whole explanation is already on the
                // screen as the tool's own result one row above. Rendering it again
                // in red, in full, is the same wall twice: measured at 20 lines for
                // one shell one-liner, and the operator's answer to it was *"it just
                // throws up on my chat"*.
                //
                // A refusal somebody DECIDED still gets the loud register and the
                // request id, because granting it is a thing the operator can do.
                if by.starts_with("boundary:") {
                    // **And only when it has something the row does not.**
                    //
                    // The refused call is already a row in the transcript, one line
                    // below this, carrying the same first sentence — so on a single
                    // refusal this note is the same words twice, which is the noise
                    // it was just cut down from. What the row cannot say is that
                    // this is the SECOND attempt at the same direction, or that the
                    // breaker has closed the direction for the session: that is a
                    // fact about the shape of the session and not about one call,
                    // and it is the one an operator wants to be told.
                    if !repeat.is_empty() {
                        self.note(Note::NotRun(Warned {
                            code: format!("denied:{request_id}"),
                            // The gist, not the transcript of it: layer A's
                            // explanation runs to a paragraph per unresolved
                            // construct and every word of it is for the model.
                            detail: format!(
                                "{tool} {outcome} — {}{repeat}",
                                first_sentence(&basis)
                            ),
                            ts,
                        }));
                    }
                } else {
                    self.note(Note::Warned(Warned {
                        code: format!("denied:{request_id}"),
                        detail: format!(
                            "REFUSED {tool} — {summary}. {outcome} by {by}: {basis}{repeat}. {grant}"
                        ),
                        ts,
                    }));
                }
                Disposition::Rendered
            }
        }
    }

    /// A key. Returns an action for the driver to send, if any.
    ///
    /// # Who owns which key
    ///
    /// Five keys are the head's: the two folds, the redraw, and the two page
    /// keys that move the transcript. **Everything else goes to the composer**,
    /// which is the whole reason `letibot_ui::editor` exists — word motion, undo
    /// batching, the kill ring, the paste ledger and the two double-tap windows
    /// are interaction policy, and interaction policy in a match arm in a head is
    /// how it ends up subtly different in the second head.
    ///
    /// # Up and Down
    ///
    /// The composer's, and only then the transcript's. `Editor::vertical` moves
    /// by **visual** row inside a wrapped prompt and walks history at the edges,
    /// which is what every shell does and what a person pressing Up after
    /// sending something expects. When it refuses — nothing to recall, or a
    /// recalled entry has been edited and moving would destroy the edit — the
    /// press falls through to scrolling by a line, so an empty composer with no
    /// history still scrolls with the arrows it always did.
    ///
    /// # Ctrl+C no longer interrupts
    ///
    /// It cleared nothing and quit an idle head on one press, so there was no way
    /// to abandon a half-typed prompt and a stray Ctrl+C killed the head. The
    /// composer's rule (opencode's) is: Ctrl+C on a non-empty composer **clears
    /// it**, twice within a second on an empty one quits, and **Esc twice within
    /// five seconds interrupts the turn**. The hint bar says which, and changes
    /// after the first press — that is the entire mechanism by which anyone
    /// discovers a double-tap exists.
    pub fn key(&mut self, k: Key) -> Option<Action> {
        // Any key is an acknowledgement of whatever the notice said.
        if !matches!(k, Key::Up | Key::Down | Key::PageUp | Key::PageDown) {
            self.notice_ttl = self.notice_ttl.min(1);
        }
        // **A password field owns the keyboard.** While `sudo` is waiting, every
        // key is the password's: characters and pastes go into the buffer, Enter
        // sends it, Esc or Ctrl+C refuses. Nothing reaches the composer, the
        // ladder or the scrollback, so a password cannot land in a prompt.
        if let Some(ask) = &self.secret {
            let req_id = ask.req_id.clone();
            match k {
                Key::Char(c) => self.secret_buf.push(c),
                Key::Paste(s) => self.secret_buf.push_str(s.trim_end_matches(['\n', '\r'])),
                Key::Backspace => {
                    self.secret_buf.pop();
                }
                Key::KillToStart | Key::KillToEnd => self.secret_buf.clear(),
                Key::Enter => {
                    let secret = std::mem::take(&mut self.secret_buf);
                    self.secret = None;
                    self.redraw = true;
                    return Some(Action::Secret {
                        req_id,
                        secret: Some(secret),
                    });
                }
                Key::Esc | Key::CtrlC => {
                    self.secret_buf.clear();
                    self.secret = None;
                    self.redraw = true;
                    return Some(Action::Secret {
                        req_id,
                        secret: None,
                    });
                }
                _ => {}
            }
            self.redraw = true;
            return None;
        }
        match k {
            Key::CtrlR => {
                self.reasoning = self.reasoning.flip();
                self.refold();
                return None;
            }
            Key::CtrlT => {
                self.tools = self.tools.flip();
                self.refold();
                return None;
            }
            // Ctrl+X, and the reason it is not one of the obvious letters is worth
            // writing down. Ctrl+R is taken (thinking) and the operator ruled it
            // out by name. Ctrl+C, Ctrl+D, Ctrl+Z, Ctrl+S and Ctrl+Q are the
            // terminal's own — two of them are flow control that would freeze a
            // pane. Ctrl+L, Ctrl+T and Ctrl+S are already this head's, and
            // Ctrl+A/E/W/U/Y/K/B/F are the composer's readline keys, which are
            // muscle memory and not available. Alt+R would read better in the hint
            // bar and is not safe: a lone Esc followed by a typed `r` arrives in
            // the same read as `ESC r`, and the composer's interrupt is Esc twice.
            //
            // What is left and is mnemonic: **x for the XML-ish markup** —
            // `<function=…><parameter=…>` — which is exactly what the chord shows.
            // 0x18 is unbound here, is not one of the tty's control characters, and
            // readline uses it only as a prefix, so nothing is waiting for a second
            // byte.
            Key::CtrlX => {
                self.raw_calls = !self.raw_calls;
                self.refold();
                return None;
            }
            Key::CtrlL => {
                self.redraw = true;
                return None;
            }
            Key::CtrlS => {
                self.picker = !self.picker;
                self.redraw = true;
                // Opening it asks for a fresh list rather than drawing the one from
                // the attach: sessions are a shared thing, and a picker showing what
                // was true when this head connected is a picker that hides the
                // session somebody else just started.
                if self.picker {
                    // The cursor starts where you are, so Enter on an untouched list
                    // is a no-op and the arrows move from a row that means something.
                    self.picker_sel = self
                        .sessions
                        .iter()
                        .position(|s| s.session_id == self.session_id)
                        .unwrap_or(0);
                }
                return self.picker.then_some(Action::ListSessions);
            }
            Key::CtrlP => {
                self.todos_pane = !self.todos_pane;
                self.redraw = true;
                if self.todos_pane {
                    // The repo's queue, read at open: the file is the operator's
                    // to edit between opens, and a pane showing yesterday's read
                    // of it is a pane that lies quietly.
                    self.repo_todos = Some(repo_todos_map(&self.wiring.workspace));
                }
                // Opening asks for the session's list rather than drawing the one
                // from the last event: the bootstrap read, for a head that
                // attached after the model last wrote. Later changes arrive as
                // `TodosUpdated` and need no asking.
                return self.todos_pane.then_some(Action::ListTodos);
            }
            // Ctrl+G for the subagent tree: R/T/X/L/S/P are taken, A/E/W/U/Y/K/B/F
            // are the composer's readline keys, and the subagent tree is a *view*,
            // not a thing the composer needs a letter for.
            Key::CtrlG => {
                self.subagents_pane = !self.subagents_pane;
                self.redraw = true;
                return None;
            }
            // Ctrl+Q for the background jobs. J would have been the mnemonic and
            // is line-feed; Q is XON, dead the same way Ctrl+S's XOFF would be —
            // and fixed the same way: cfmakeraw clears IXON, so nothing is
            // listening for flow control and the byte arrives like any other.
            Key::CtrlQ => {
                self.jobs_pane = !self.jobs_pane;
                self.redraw = true;
                return None;
            }
            // Ctrl+O: move the running command to the background. Meaningless when
            // nothing is running, so a bare press says so rather than asking.
            Key::CtrlO => {
                if self.turn_running() {
                    self.say("moving the running command to the background");
                    return Some(Action::Promote);
                }
                self.say("nothing is running to move to the background");
                return None;
            }
            Key::PageUp => {
                self.scroll = (self.scroll + 10).min(self.body_len);
                return None;
            }
            Key::PageDown => {
                self.scroll = self.scroll.saturating_sub(10);
                return None;
            }
            // Three lines a notch: a wheel notch is a row at a time in a pager,
            // but a transcript row can be two screen rows after wrapping, and a
            // notch that moves one wrapped row reads as nothing happened.
            Key::WheelUp => {
                self.scroll = (self.scroll + 3).min(self.body_len);
                return None;
            }
            Key::WheelDown => {
                self.scroll = self.scroll.saturating_sub(3);
                return None;
            }
            _ => {}
        }

        // **The subagent output view owns the keys while it is open.** Arrows
        // scroll it like a terminal — up toward the beginning, down back to the
        // tail — Enter reads the same subagent again, because a running one has
        // new output, and Esc goes back to the tree. This sits ahead of the
        // generic Esc below on purpose: Esc here means "back to the tree", not
        // "close everything".
        if self.sub_out.is_some() {
            match k {
                Key::Up => {
                    self.sub_out.as_mut().unwrap().scroll += 1;
                    self.redraw = true;
                    return None;
                }
                Key::Down => {
                    let v = self.sub_out.as_mut().unwrap();
                    v.scroll = v.scroll.saturating_sub(1);
                    self.redraw = true;
                    return None;
                }
                Key::Enter if self.editor.text().is_empty() => {
                    let id = self.sub_out.as_ref().unwrap().session_id.clone();
                    self.sub_out_pending = Some(id.clone());
                    return Some(Action::Peek(id));
                }
                Key::Esc | Key::CtrlC => {
                    self.sub_out = None;
                    self.redraw = true;
                    return None;
                }
                _ => {}
            }
        }

        // Help and the picker are screens, and the two keys that mean "go back"
        // close them before the composer ever sees them.
        if (self.help || self.picker || self.stats || self.todos_pane || self.subagents_pane
            || self.jobs_pane)
            && matches!(k, Key::Esc | Key::CtrlC)
        {
            self.help = false;
            self.picker = false;
            self.stats = false;
            self.todos_pane = false;
            self.subagents_pane = false;
            self.jobs_pane = false;
            self.sub_out_pending = None;
            self.redraw = true;
            return None;
        }
        // Esc while parked in the scrollback means "follow the stream again",
        // which is what the scrollback banner says it means. Only then does Esc
        // start arming an interrupt.
        if matches!(k, Key::Esc) && self.scroll > 0 {
            self.scroll = 0;
            return None;
        }

        // **An open decision owns Up/Down and a bare Enter.**
        //
        // Before the composer, because while a prompt is on the screen those keys mean
        // the ladder and cannot sensibly mean anything else -- the same argument the
        // picker arm above already makes for a bare row number.
        //
        // Only with an EMPTY composer, so nothing is taken away: a half-typed line
        // still scrolls, still edits, and Enter still sends it. That keeps `/command`,
        // a typed option id and the tests working unchanged.
        if !self.open.is_empty() && self.editor.text().is_empty() {
            let n = self.open[0].options.len();
            match k {
                Key::Up if n > 0 => {
                    self.sel = if self.sel == 0 { n - 1 } else { self.sel - 1 };
                    self.redraw = true;
                    return None;
                }
                Key::Down if n > 0 => {
                    self.sel = (self.sel + 1) % n;
                    self.redraw = true;
                    return None;
                }
                Key::Enter if n > 0 => {
                    let d = &self.open[0];
                    let opt = d.options[self.sel.min(n - 1)].option_id.clone();
                    let req_id = d.req_id.clone();
                    return Some(Action::Answer {
                        req_id,
                        option_id: opt,
                        // Enter on the ladder is the no-glob path by construction:
                        // there is nothing typed to read one from. A glob is given
                        // by typing `allow_always <pattern>` on the line.
                        pattern: None,
                    });
                }
                _ => {}
            }
        }

        // **An open session picker owns Up and Down, and Enter on an empty line.**
        //
        // After the decision ladder, which keeps precedence while a prompt is up. The
        // number path is untouched — digits still land in the composer and Enter
        // still answers them — but the list is on the screen, so the arrows move the
        // cursor on it rather than the caret in a composer the picker is covering.
        // The empty-composer rule is the decision ladder's own: a half-typed id's
        // Enter still means the id.
        if self.picker && !self.sessions.is_empty() {
            let n = self.sessions.len();
            match k {
                Key::Up => {
                    self.picker_sel = if self.picker_sel == 0 {
                        n - 1
                    } else {
                        self.picker_sel - 1
                    };
                    self.redraw = true;
                    return None;
                }
                Key::Down => {
                    self.picker_sel = (self.picker_sel + 1) % n;
                    self.redraw = true;
                    return None;
                }
                Key::Enter if self.editor.text().is_empty() => {
                    let id = self.sessions[self.picker_sel.min(n - 1)].session_id.clone();
                    return self.switch_to(id);
                }
                Key::Click { y, .. } => {
                    // The same arithmetic the screen did: the optional session
                    // header takes a row, then the picker's title and a blank,
                    // then the sessions. Only a row the last render actually
                    // drew is trusted — `picker_rows_drawn` knows where
                    // `truncate(room)` cut the list off, so a click into the
                    // blank space under a truncated list moves nothing.
                    let header_rows =
                        usize::from(self.screen_rows >= 6 && !self.session_id.is_empty());
                    let first = header_rows + 2;
                    let row = usize::from(y).saturating_sub(first);
                    if row < self.picker_rows_drawn.saturating_sub(2) {
                        self.picker_sel = row.min(n - 1);
                        self.redraw = true;
                    }
                    return None;
                }
                _ => {}
            }
        }

        // **An open subagent pane owns Up and Down, and Enter switches into the
        // subagent.** The same two acts the picker keeps separate — arrows move the
        // cursor, Enter confirms — so the tree is a screen you can act on, not just
        // a list you read.
        if self.subagents_pane && !self.subagents.is_empty() {
            let n = self.subagents.len();
            match k {
                Key::Up => {
                    self.subagents_sel = if self.subagents_sel == 0 {
                        n - 1
                    } else {
                        self.subagents_sel - 1
                    };
                    self.redraw = true;
                    return None;
                }
                Key::Down => {
                    self.subagents_sel = (self.subagents_sel + 1) % n;
                    self.redraw = true;
                    return None;
                }
                Key::Enter if self.editor.text().is_empty() => {
                    // Reading, not moving: the output pane opens on the `Peeked`
                    // reply, and this head never leaves the session it is in.
                    let row = &self.subagents[self.subagents_sel.min(n - 1)];
                    if row.state == "opening" {
                        // Nothing to read yet, and the daemon would refuse the peek
                        // by name anyway; saying it here keeps the operator in the
                        // pane they were using rather than bouncing them through a
                        // rejection.
                        self.say("that subagent is still opening — nothing to read yet");
                        self.redraw = true;
                        return None;
                    }
                    let id = row.session_id.clone();
                    self.sub_out_pending = Some(id.clone());
                    return Some(Action::Peek(id));
                }
                // Switching is still here, one key over: Enter reads, `o` opens
                // the subagent's session for good.
                Key::Char('o') if self.editor.text().is_empty() => {
                    let row = &self.subagents[self.subagents_sel.min(n - 1)];
                    if row.state == "opening" {
                        self.say("that subagent is still opening — nothing to attach to yet");
                        self.redraw = true;
                        return None;
                    }
                    let id = row.session_id.clone();
                    self.subagents_pane = false;
                    return self.switch_to(id);
                }
                _ => {}
            }
        }

        // Tab: slash-command completion. The composer's own keys run after it
        // because Tab means nothing to the editor — its byte used to be eaten
        // by the decoder — and every other key leaves a running completion
        // cycle alone: it re-validates its prefix the next time Tab is
        // pressed, so there is nothing to reset in each arm here.
        if let Key::Tab = k {
            self.complete_slash();
            self.redraw = true;
            return None;
        }

        let now = self.now_ms;
        let cols = self.composer_cols();
        let reaction = match k {
            Key::Up => self.editor.vertical(true, cols),
            Key::Down => self.editor.vertical(false, cols),
            ref other => self.editor.key(other.composer()?, now),
        };
        match reaction {
            Reaction::Submit(text) => self.submit(text),
            Reaction::Interrupt => {
                if self.turn_running() {
                    // Interrupt is not quit. A shared session's interrupt is
                    // announced with the issuer, so it must be a deliberate act —
                    // and two presses of Esc inside five seconds is one.
                    Some(Action::Interrupt("operator pressed esc twice".into()))
                } else {
                    self.say("nothing is running");
                    None
                }
            }
            Reaction::Quit => {
                self.quit = true;
                Some(Action::Quit)
            }
            Reaction::Changed => None,
            // The composer had no use for it. Up and Down then belong to the
            // transcript; see the note above.
            Reaction::Idle => {
                match k {
                    Key::Up => self.scroll = (self.scroll + 1).min(self.body_len),
                    Key::Down => self.scroll = self.scroll.saturating_sub(1),
                    _ => {}
                }
                None
            }
        }
    }

    /// What a submitted line means: a command, an answer to an open decision, or
    /// a prompt.
    fn submit(&mut self, text: String) -> Option<Action> {
        if let Some(rest) = text.strip_prefix('/') {
            return self.command(rest.trim());
        }
        // The picker takes the line as a row number or an id prefix. It is checked
        // before the decision arm and before the prompt arm, because while a picker
        // is on the screen a bare `2` means the second session and cannot sensibly
        // mean anything else.
        if self.picker {
            return self.pick(text.trim());
        }
        // An open decision takes the line as an option id or its first letter, so
        // answering does not require a second keymap.
        if let Some(d) = self.open.first().cloned()
            && let Some((opt, pattern)) = match_option(&d, text.trim())
        {
            return Some(Action::Answer {
                req_id: d.req_id,
                option_id: opt,
                pattern,
            });
        }
        // Sending scrolls back to the tail: the answer is about to arrive at the
        // bottom, and staying parked in the scrollback while it does looks exactly
        // like nothing happening.
        self.scroll = 0;
        // Held here, visibly, until the transcript takes the words over. When the
        // session is idle the user row lands within a tick and this is a one-frame
        // acknowledgement; when a turn is running it is the whole fix — the hub
        // queues the prompt as a follow-up user item and appends it at the next
        // step boundary, and until then this is the only place the sentence exists
        // where the person who typed it can see it.
        self.pending_prompts.push(text.clone());
        Some(Action::Prompt(text))
    }

    /// Columns the composer's text has, inside the box.
    ///
    /// One function, because the wrap width the editor is *drawn* at and the one
    /// vertical motion is *computed* at have to be the same number — a cursor
    /// that moves by a row the renderer did not draw lands somewhere the person
    /// was not looking.
    fn composer_cols(&self) -> usize {
        self.cfg.width.saturating_sub(4).max(8)
    }

    /// Screen requests to answer with the frame just drawn. Drains.
    pub fn take_screen_requests(&mut self) -> Vec<String> {
        std::mem::take(&mut self.screen_requests)
    }

    /// What the composer holds, for a test and for a head that wants to prefill it.
    pub fn input(&self) -> &str {
        self.editor.text()
    }

    /// A fold changes how many lines every cached block renders to, so the history
    /// buffer and every block cache are stale at once.
    fn refold(&mut self) {
        self.invalidate_history();
        self.scroll = 0;
        self.redraw = true;
        self.say(&format!(
            "thinking {} · tool output {} · raw tool calls {}",
            fold_word(self.reasoning),
            fold_word(self.tools),
            if self.raw_calls { "shown" } else { "hidden" }
        ));
    }

    /// Answer the session picker: a row number, or enough of an id to be unique.
    ///
    /// An ambiguous prefix is **refused with the count**, not resolved to the first
    /// match. Switching to the wrong session is not a keystroke you can take back —
    /// the prompt you type next lands there.
    fn pick(&mut self, typed: &str) -> Option<Action> {
        if typed.is_empty() {
            self.picker = false;
            self.redraw = true;
            return None;
        }
        if let Ok(n) = typed.parse::<usize>()
            && n >= 1
            && n <= self.sessions.len()
        {
            let id = self.sessions[n - 1].session_id.clone();
            return self.switch_to(id);
        }
        let hits: Vec<&SessionBrief> = self
            .sessions
            .iter()
            .filter(|s| {
                s.session_id.starts_with(typed)
                    || (!s.title.is_empty()
                        && s.title.to_ascii_lowercase().contains(&typed.to_ascii_lowercase()))
            })
            .collect();
        match hits.len() {
            1 => {
                let id = hits[0].session_id.clone();
                self.switch_to(id)
            }
            0 => {
                self.say(&format!("no session matches {typed:?} — esc closes the list"));
                None
            }
            n => {
                self.say(&format!(
                    "{n} sessions match {typed:?}; type the number on the left instead"
                ));
                None
            }
        }
    }

    /// Take Tab on a `/`-prefixed line.
    ///
    /// A fresh prefix starts a cycle at its first match; a further Tab walks
    /// the cycle, but only while the line is exactly what the cycle last
    /// wrote — a character typed on, or an edit away, starts a fresh match
    /// next time, so the cycle can never clobber what someone typed after it.
    /// A prefix nothing matches says so in the notice line and leaves the
    /// line alone, because deleting what someone typed to explain why nothing
    /// happened would be the completion acting like a decision.
    fn complete_slash(&mut self) {
        let text = self.editor.text().to_string();
        if !text.starts_with('/') || text.contains(char::is_whitespace) {
            return;
        }
        if let Some((_, names, idx)) = &mut self.completion {
            let live = names
                .get(*idx)
                .is_some_and(|current| text == format!("/{current}"));
            if live && !names.is_empty() {
                *idx = (*idx + 1) % names.len();
                let word = names[*idx];
                self.set_composer(&format!("/{word}"));
                return;
            }
        }
        let needle = &text[1..];
        let names: Vec<&'static str> = SLASH_COMMANDS
            .iter()
            .filter(|(name, _)| name.starts_with(needle))
            .map(|(name, _)| *name)
            .collect();
        match names.first() {
            Some(&first) => {
                self.completion = Some((text.clone(), names, 0));
                self.set_composer(&format!("/{first}"));
            }
            None => {
                self.completion = None;
                self.say(&format!("no /command starts with {text:?}"));
            }
        }
    }

    /// Replace the whole composer line. Completion words are single tokens, so
    /// Home + kill-to-end + insert is the honest way there: the editor has no
    /// text setter, and the three public ops keep its undo and history exactly
    /// as true as any typed edit.
    fn set_composer(&mut self, text: &str) {
        self.editor.key(letibot_ui::editor::Key::Home, self.now_ms);
        self.editor
            .key(letibot_ui::editor::Key::KillToEnd, self.now_ms);
        self.editor.insert(text);
    }

    /// The live completion row shown above the composer while a `/command` is
    /// being typed: every match, name plus its hint, joined with `·`. A bare
    /// `/` lists everything; a prefix nothing matches shows nothing, because
    /// an empty line that appears and disappears is noise, and Tab will say
    /// what went wrong when it is asked.
    fn completions_line(&self, w: usize) -> Option<String> {
        let text = self.editor.text();
        if !text.starts_with('/') || text.contains(char::is_whitespace) {
            return None;
        }
        let needle = &text[1..];
        let parts: Vec<String> = SLASH_COMMANDS
            .iter()
            .filter(|(name, _)| name.starts_with(needle))
            .map(|(name, hint)| format!("/{name} {hint}"))
            .collect();
        if parts.is_empty() {
            return None;
        }
        let cfg = &self.cfg;
        Some(dim(cfg, &trim_to(&format!("  {}", parts.join("  ·  ")), w)))
    }

    fn switch_to(&mut self, id: String) -> Option<Action> {
        if id == self.session_id {
            self.picker = false;
            self.redraw = true;
            self.say("already here");
            return None;
        }
        // A session that is on disk and not in this daemon has to be brought in
        // before it can be switched to. Two frames, and the head sends the second
        // one when the daemon answers the first — the same two steps `/new` takes,
        // reusing `want_new_session` because "go to the session the daemon just told
        // me about" is one behaviour and a second flag for it would be a second
        // behaviour that drifts.
        if self.sessions.iter().any(|b| b.session_id == id && !b.live) {
            self.want_new_session = true;
            self.say(&format!(
                "resuming {} from the store…",
                self.session_label(&id)
            ));
            return Some(Action::ResumeSession(id));
        }
        // `since_seq` is not sent: this head has no state for the session it is
        // going to, so a snapshot is the only honest ask. Coming *back* to a
        // session it was watching would be a resume, and this head does not keep
        // per-session marks — it would be a cache with no invalidation rule.
        Some(Action::Switch(id))
    }

    fn command(&mut self, cmd: &str) -> Option<Action> {
        // **`/cells MESSAGE` — the message, and what is on this screen with it.**
        //
        // `harness what=screen` lets the model ASK; this is the operator pointing.
        // Same rows, same bytes, and captured here rather than a moment later on
        // purpose: the screen being talked about is the one that was there when
        // Enter was pressed, and a turn takes seconds during which it moves.
        //
        // Rendered from this head, at this head's size, escape codes intact — the
        // whole point is what is actually painted, not a description of it.
        if let Some(rest) = cmd.strip_prefix("cells") {
            let message = rest.trim().to_string();
            let (w, h) = (self.term_cols, self.screen_rows);
            if w == 0 || h == 0 {
                // Nothing has been drawn yet, so there is nothing to send. Said
                // rather than sending an empty block that reads as a blank screen.
                self.say("nothing has been drawn on this head yet — no cells to send");
                return None;
            }
            let rows = self.screen(w, h);
            let mut text = if message.is_empty() {
                String::new()
            } else {
                format!("{message}\n\n")
            };
            // Delimited rather than introduced by a sentence, so both readers can
            // find the edges: the model knows where the screen stops and the
            // operator's words end, and the head knows which part of its own
            // transcript is a picture of itself and folds it away. A sentence would
            // do the first job and not the second.
            text.push_str(&format!(
                "{CELLS_OPEN}{w}x{h} — my terminal exactly as this head drew it, ANSI \
                 escape codes included, so what you are reading IS the rendering and \
                 not a description of it{CELLS_MARK_END}\n"
            ));
            for r in &rows {
                text.push_str(r);
                text.push('\n');
            }
            text.push_str(CELLS_CLOSE);
            text.push('\n');
            self.scroll = 0;
            // **The same string that was sent.** The pending row is cleared by
            // matching the user item the daemon appends, so an abbreviation here
            // never matches and the `queued` line never leaves. The screen is taken
            // out at RENDER time instead, by `queued_lines` and by `user_block`,
            // which is where a decision about what to show belongs.
            self.pending_prompts.push(text.clone());
            return Some(Action::Prompt(text));
        }
        if let Some(title) = cmd.strip_prefix("new") {
            self.want_new_session = true;
            self.say("making a session…");
            return Some(Action::NewSession(title.trim().to_string()));
        }
        if matches!(cmd, "sessions" | "s") {
            self.picker = true;
            self.redraw = true;
            return Some(Action::ListSessions);
        }
        if let Some(id) = cmd.strip_prefix("switch ") {
            return self.pick(id.trim());
        }
        // Renames the session this head is **in**. Not an arbitrary one: the picker
        // is where another session is on screen, and a `/rename` that could reach a
        // row you were only looking at is one typo away from renaming the wrong
        // conversation.
        if let Some(title) = cmd.strip_prefix("rename") {
            let title = title.trim().to_string();
            if self.session_id.is_empty() {
                self.say("not attached to a session yet");
                return None;
            }
            if title.is_empty() {
                self.say("/rename NAME — or /rename with nothing clears the name");
            }
            return Some(Action::Rename {
                session_id: self.session_id.clone(),
                title,
            });
        }
        if let Some(name) = cmd.strip_prefix("mode") {
            let name = name.trim().to_string();
            if self.session_id.is_empty() {
                self.say("not attached to a session yet");
                return None;
            }
            if name.is_empty() {
                self.say(
                    "/mode NAME — read-only, always-ask, writes-allowed, supervised, \
                     automode, allow-all (or the opencode names plan/default/\
                     acceptEdits/bypassPermissions). Moves THIS session from its \
                     next call, and every later session in this project.",
                );
                return None;
            }
            return Some(Action::Mode { name });
        }
        match cmd {
            "quit" | "q" => {
                self.quit = true;
                Some(Action::Quit)
            }
            "resync" => Some(Action::Resync),
            "help" | "h" | "?" => {
                self.help = !self.help;
                self.redraw = true;
                None
            }
            // Where the bottom border's telemetry went. See `App::stats`.
            "status" | "stats" => {
                self.stats = !self.stats;
                self.redraw = true;
                None
            }
            "think" | "r" => {
                self.reasoning = self.reasoning.flip();
                self.refold();
                None
            }
            "tools" | "t" => {
                self.tools = self.tools.flip();
                self.refold();
                None
            }
            "verbosity" | "v" => {
                self.verbosity = self.verbosity.next();
                self.say(&format!(
                    "verbosity {} — {} events filtered so far",
                    self.verbosity.as_str(),
                    self.filtered
                ));
                None
            }
            "diff" => {
                self.diff_split = !self.diff_split;
                // The settled rows are cached; a toggle that changes how they
                // draw has to say so or it only reaches the live pane.
                self.invalidate_history();
                self.redraw = true;
                self.say(&format!(
                    "file edits render {}",
                    if self.diff_split { "side by side (unified below 100 columns)" } else { "as a unified diff" }
                ));
                None
            }
            "jobs" => {
                self.jobs_pane = !self.jobs_pane;
                self.redraw = true;
                None
            }
            "interrupt" | "i" => Some(Action::Interrupt("operator typed /interrupt".into())),
            "compact" => {
                // The session this head is **in**, for the same reason /rename
                // refuses an arbitrary id: a compaction that could reach a row you
                // were only looking at is one typo away from summarising the wrong
                // conversation.
                if self.session_id.is_empty() {
                    self.say("not attached to a session yet");
                    return None;
                }
                Some(Action::Compact)
            }
            // **The tool list is in the prompt, and a prompt is fixed for a
            // conversation.** So this is the only way a session that opened without
            // a shell ever gets one: summarise, and continue under a prompt built
            // from what is seated now. Same refusal as `/compact` for the same
            // reason — it acts on the session this head is in, never one you are
            // only looking at.
            "reseat" => {
                if self.session_id.is_empty() {
                    self.say("not attached to a session yet");
                    return None;
                }
                self.say("re-seating: summarising, then rebuilding the prompt…");
                Some(Action::Reseat)
            }
            other => {
                // The daemon's verbs. The head does not know them and does not
                // need to: the line goes over as typed and the answer comes back
                // on the session log.
                let verb = other.split_whitespace().next().unwrap_or("");
                if matches!(
                    verb,
                    "flowy" | "models" | "model" | "login" | "supervise" | "supervised" | "gate"
                ) {
                    if self.session_id.is_empty() {
                        self.say("not attached to a session yet");
                        return None;
                    }
                    return Some(Action::Slash {
                        line: other.trim().to_string(),
                    });
                }
                self.say(&format!("unknown command /{other} — try /help"));
                None
            }
        }
    }

    /// Post a transient line. It lives for a few frames and then gets out of the
    /// way; it does **not** take the input line's place, which is what the old one
    /// did — after the first prompt of a session there was nowhere to see what you
    /// were typing, for the rest of the session.
    fn say(&mut self, text: &str) {
        self.notice = Some(text.to_string());
        self.notice_ttl = 60;
    }

    fn turn_running(&self) -> bool {
        matches!(
            self.turn.as_ref().and_then(|t| t.state.as_ref()),
            Some(TurnState::Running)
        )
    }

    /// Attach content to a transcript row, from whatever route the daemon offers.
    pub fn record_item(&mut self, item_id: &str, item: TranscriptItem) {
        let prose = matches!(item, TranscriptItem::Assistant { .. });
        // A user row with body is the transcript taking a queued prompt over. The
        // steering path appends the operator's words verbatim
        // (`SteeringMessage::to_item`: "a plain `User` item with exactly its own
        // text"), so the text is the match — and one row retires one entry, so two
        // prompts that say the same thing stay queued separately until each of
        // their rows lands.
        if let TranscriptItem::User { parts } = &item
            && let Some(text) = parts.iter().find_map(|p| match p {
                UserPart::Text { text } => Some(text.clone()),
                _ => None,
            })
            && let Some(at) = self.pending_prompts.iter().position(|p| *p == text)
        {
            self.pending_prompts.remove(at);
        }
        let Some(idx) = self.items.iter().position(|r| r.item_id == item_id) else {
            return;
        };
        self.items[idx].item = Some(item);
        // The row's rendered form changed, so the history cache from that row
        // on is stale. From that row on, and not from row zero: this is the
        // hottest of the invalidations — one per transcript row, so one per
        // row per session — and re-rendering the rows above a row whose body
        // just arrived is the whole session, again, for every row in it. From
        // the head of its ROUND, because a round is what renders as a unit; see
        // `round_head`.
        let k = self.round_head(idx);
        self.invalidate_history_from(k);
        // The pane's accumulated prose, handed over the same way its calls are.
        //
        // `TurnPane::text` is every `Delta { target: Text }` of the whole turn, and
        // a turn's prose is committed to the transcript one ROUND at a time. So
        // once a round's assistant row has its body, the sentence the model wrote
        // before its first tool call is on the screen twice — in history where it
        // belongs and again in the pane below the cards. Measured at 60x34 on the
        // operator's session: "I'll take a look at what's in the tree first."
        // appearing above the round's cards and again under them.
        //
        // Clearing rather than counting bytes, because that is what "the
        // transcript has taken this over" means, and because `IncrementalMarkdown`
        // is a frozen-prefix lexer — slicing it would mean re-lexing what it has
        // already frozen, which is the §13.3 rule this head is built around.
        //
        // Safe against a race only because the engine appends a round's assistant
        // row before generating the next round (`harnessd::harness`), so no delta
        // of round N+1 can arrive before round N's row.
        if prose
            && let Some(t) = self.turn.as_mut()
            && t.appended.iter().any(|a| a == item_id)
        {
            t.text = IncrementalMarkdown::new();
            t.text_cache = BlockCache::new();
        }
    }

    /// Throw the rendered history away; it is rebuilt from `items` and `notes`
    /// on the next frame. One place, because forgetting one of the two cursors
    /// duplicates or loses everything after it.
    ///
    /// For the three callers that really do mean *all of it*: a snapshot replaced
    /// `items` wholesale, a fold changed how many lines every cached block renders
    /// to, and a width change moved every wrap. Everything else means
    /// [`App::invalidate_history_from`].
    fn invalidate_history(&mut self) {
        self.invalidate_history_from(0);
    }

    /// The rendered history is stale **from row `k` on**. Rows above it are
    /// settled: nothing this head is told can change what they render to.
    ///
    /// # Why this is not `invalidate_history`
    ///
    /// It was, at every call site, and the comment at the largest one already said
    /// what the code did not do — *"the row's rendered form changed, so the history
    /// cache from that row on is stale"*. Measured over one replay of a real
    /// 89-row session: 89 row bodies arriving and 46 turn-state transitions, each
    /// re-rendering the whole transcript ahead of the row that moved, so the cost
    /// of a session grows as its square.
    ///
    /// **It is not a repaint.** [`crate::term::paint_full`] diffs every frame
    /// against the glass and writes only the rows whose text changed, so a rebuild
    /// that produces the same lines writes no bytes. Measured on the same replay,
    /// with all four turn-state invalidations removed: 1,253,922 bytes against
    /// 1,256,038, and the same final screen to the byte. This is the cost of the
    /// *render*, and nothing about what reaches the terminal.
    fn invalidate_history_from(&mut self, k: usize) {
        if k == 0 {
            self.hist_lines.clear();
            self.hist_marks.clear();
            self.hist_upto = 0;
            self.note_upto = 0;
            self.hist_class = None;
            // The target table is the walk's own state — the round it is currently
            // inside — so it is thrown away with the lines it labelled. Leaving it
            // behind is what let a rebuild start at row 0 holding round 14's paths.
            self.call_targets.clear();
            return;
        }
        // A row the walk has not reached yet has nothing rendered to throw away,
        // and rewinding to it would rewind past rows that are fine.
        let Some(mark) = self.hist_marks.get(k).copied() else {
            return;
        };
        self.hist_lines.truncate(mark.lines);
        self.hist_marks.truncate(k);
        self.hist_upto = k;
        self.note_upto = mark.note_upto;
        self.hist_class = mark.class;
        self.retarget_before(k);
    }

    /// Put `call_targets` back to what it held when the walk was about to draw
    /// row `k`: the calls of the nearest assistant row above it that has a body.
    ///
    /// Derived rather than stored, and it has to match the walk exactly — the walk
    /// **replaces** the table at every assistant row with a body, including one
    /// that proposed no calls at all, because `call_0` is positional within a
    /// round and a merge is how round 4's `call_0` came to wear round 1's path.
    /// So the scan stops at the first such row rather than accumulating.
    fn retarget_before(&mut self, k: usize) {
        self.call_targets.clear();
        for r in self.items[..k].iter().rev() {
            if let Some(TranscriptItem::Assistant { tool_calls, .. }) = r.item.as_ref() {
                for c in tool_calls {
                    self.call_targets.insert(
                        c.id.clone(),
                        letibot_sessionlog::display_target(&c.arguments),
                    );
                }
                return;
            }
        }
    }

    /// The first row whose rendering a change to row `idx` can reach.
    ///
    /// **A row is not rendered in isolation, and this is the trap in narrowing an
    /// invalidation.** An assistant row asks which of the calls it proposed have
    /// come back, and that answer lives in the rows *after* it, up to the next
    /// assistant or user row — [`round_results`]. So the unit that has to be
    /// re-rendered is the ROUND, not the row: a tool result's body arriving
    /// changes what the assistant row above it draws, and rewinding only to the
    /// result leaves the proposal beside its own answer. Measured as exactly that
    /// — `TODO.md` on the screen twice — by
    /// `a_settled_call_is_one_row_and_the_row_is_the_one_with_the_result_on_it`,
    /// and against a real session by
    /// `a_settled_call_is_one_row_when_the_round_does_not_start_at_row_zero`,
    /// which is the one that exercises a rewind rather than a rebuild.
    ///
    /// Keyed on `kind` rather than on the body, because [`round_results`] breaks on
    /// an *announced* assistant row whose content has not arrived yet, and two
    /// answers to "where does this round start" is one too many.
    fn round_head(&self, idx: usize) -> usize {
        self.items[..=idx]
            .iter()
            .rposition(|r| r.kind == "assistant" || r.kind == "user")
            .unwrap_or(0)
    }

    /// The first history row this turn's pane is drawing, or `None` if it is
    /// drawing none.
    ///
    /// A turn-state transition changes one input to the walk — `drawn_live`, which
    /// is true only for a row in `TurnPane::appended` — so it can change what those
    /// rows render to and nothing above the first of them.
    fn turn_first_row(&self) -> Option<usize> {
        let t = self.turn.as_ref()?;
        if t.appended.is_empty() {
            return None;
        }
        let ids: std::collections::HashSet<&str> = t.appended.iter().map(String::as_str).collect();
        self.items
            .iter()
            .position(|r| ids.contains(r.item_id.as_str()))
    }

    /// The history is stale from the first row the live pane owns. A pane that
    /// owns no rows changes no history at all, and then this does nothing.
    fn invalidate_turn_rows(&mut self) {
        if let Some(k) = self.turn_first_row() {
            let k = self.round_head(k);
            self.invalidate_history_from(k);
        }
    }

    /// File something that happened between rows, at the row it happened at.
    fn note(&mut self, n: Note) {
        let at = self.items.len();
        self.notes.push((at, n));
        if self.notes.len() > 64 {
            self.notes.remove(0);
            self.note_upto = self.note_upto.saturating_sub(1);
            // Every mark holds a `note_upto`, and dropping the oldest note shifts
            // every index in `notes` down by one. A mark that is not shifted with
            // them rewinds to the wrong note and re-renders it — which is the
            // "duplicates or loses everything after it" failure, one cursor along.
            for m in &mut self.hist_marks {
                m.note_upto = m.note_upto.saturating_sub(1);
            }
        }
    }

    /// One frame: `h` lines of at most `w` columns.
    ///
    /// # The cost of a frame does not grow with the session
    ///
    /// §13.3's rule is about the whole render path, not only the lexer, and the
    /// previous shape broke it downstream of the part that was careful: the frozen
    /// prefix was lexed once and rendered once, and then **copied in full on every
    /// frame** — `hist_lines.clone()`, plus a `stable_lines.clone()` inside each
    /// block cache — so drawing at 10 Hz cost O(everything said so far), ten times a
    /// second, to put `h` lines on a screen.
    ///
    /// So the body is assembled as a list of [`Seg`]s — the history borrowed, the
    /// live tail owned and freshly rendered — and only the visible window is
    /// materialised. A frame costs O(live tail + window). The history's length
    /// reaches the frame only as an integer.
    ///
    /// # The bottom of the screen
    ///
    /// ```text
    ///   ╭────────────────────────────────────────────── 1 subagent running ─╮
    ///   │ › why did the cache miss                                               │
    ///   ╰────────────────────────────── ⚠ · ⠹ Responding · 4.2s · 1.2k chars ─╯
    ///   enter send · alt+enter newline · esc esc interrupt · ctrl-r thinking
    /// ```
    ///
    /// **A box, not an accent bar.** Both were on the table and the box wins on
    /// three counts, none of them taste:
    ///
    /// 1. It is *structure*, not colour. This head has a `color: false` mode that
    ///    is not a monochrome theme — it is `--replay`, a pipe to a file, and CI —
    ///    and an accent bar plus a raised background is exactly nothing there. It
    ///    is also nothing in a light-theme terminal, where a dark block is either
    ///    invisible or unreadable depending on which half of the pair lands.
    /// 2. **The border rows carry the content that would otherwise need rows of
    ///    its own.** The top edge is what the session is talking to; the bottom
    ///    edge is §13.2b's disclosure counters, which used to be a line. So the
    ///    box costs one net row over the old two-line chrome, not two, and every
    ///    row on the screen says something.
    /// 3. It degrades by *deletion* rather than by becoming wrong: at 40 columns
    ///    the legends truncate and the box is still a box, and when the terminal
    ///    is too short for it the borders go and the input keeps its `›`.
    ///
    /// What is deliberately **not** in the field: any prose. The old input line
    /// read `ask something · /help · ctrl-r thinking · ctrl-t tool output` — four
    /// jobs in one line, which is why it read as a status message and not as a
    /// place to type. Neither surveyed project puts anything inside the input.
    /// The affordance is the caret and the container.
    pub fn screen(&mut self, term_w: usize, h: usize) -> Vec<String> {
        // The gutter, applied to the *whole* frame rather than to the transcript.
        // The operator's report was "no margins for the main output — things are
        // hard left with literally zero space"; inseting only the body would have
        // fixed that sentence and left the header and the composer's box a
        // different distance from the edge, which is the thing a reader notices.
        let gutter = Self::gutter(term_w);
        let w = term_w - 2 * gutter;
        self.cfg.width = w;
        // The TERMINAL's width, kept beside the frame's. `cfg.width` is the inner
        // one — the gutter already taken off — so anything that re-renders from a
        // stored size has to start from this one or the frame narrows by two
        // columns every time it is asked for.
        self.term_cols = term_w;
        let h = h.max(1);
        // Click mapping has to redo this frame's arithmetic without a repaint;
        // the height the frame was composed for is the fact it needed.
        self.screen_rows = h;
        if self.notice_ttl > 0 {
            self.notice_ttl -= 1;
            if self.notice_ttl == 0 {
                self.notice = None;
            }
        }

        let dec: Vec<String> = match (&self.secret, self.open.first()) {
            (Some(ask), _) => self.secret_lines(ask, w),
            (None, Some(d)) => self.decision_lines(d, w),
            (None, None) => Vec::new(),
        };
        let stuck = self.stuck_line(w);
        let notice = self
            .notice
            .clone()
            .map(|n| colour(&self.cfg, sgr::MAGENTA, &trim_to(&format!("· {n}"), w)));
        // Live slash-command matches, one dim row above the composer. It is a
        // typing aid, not a message — which is why it is the first thing the
        // ladder gives up.
        let completions = self.completions_line(w);

        // How many rows the composer wants, and then what actually fits. The
        // ladder deletes the most expendable row first and stops as soon as the
        // whole thing fits with a line of transcript left over. The old code
        // drained the chrome from the *front*, which for a box would have eaten
        // the top border and left the bottom one — a container with one side is
        // worse than none. The turn's own status costs no row at all any more:
        // it is inlaid in the bottom border, which is there anyway.
        let mut rows = self.editor.height(self.composer_cols(), h);
        let mut hint = true;
        let mut show_notice = notice.is_some();
        let mut show_stuck = stuck.is_some();
        let mut show_completions = completions.is_some();
        let mut boxed = true;
        let mut dec_rows = dec.len();
        loop {
            let n = dec_rows
                + usize::from(show_stuck)
                + usize::from(show_notice)
                + usize::from(show_completions)
                // Unboxed costs one row **only when there is an alarm to show**:
                // the counters move off the border and back onto a line of their
                // own, and a counter that has moved is not what a narrow screen
                // gives up. A clean head owes that row to the transcript.
                + if boxed { 2 } else { usize::from(self.alarmed()) }
                + rows
                + usize::from(hint);
            if n < h {
                break;
            }
            if show_completions {
                show_completions = false;
            } else if hint {
                hint = false;
            } else if show_notice {
                show_notice = false;
            } else if rows > 1 {
                rows -= 1;
            } else if show_stuck {
                show_stuck = false;
            } else if boxed {
                boxed = false;
            } else if dec_rows > 1 {
                dec_rows -= 1;
            } else {
                break;
            }
        }

        let (input_rows, caret_row, caret_col) = self.composer_rows(w, rows, boxed);
        let mut chrome: Vec<String> = Vec::new();
        chrome.extend(dec.into_iter().take(dec_rows));
        if show_stuck && let Some(l) = stuck {
            chrome.push(l);
        }
        if show_notice && let Some(l) = notice {
            chrome.push(l);
        }
        if show_completions && let Some(l) = completions {
            chrome.push(l);
        }
        if boxed {
            // The top edge carries exactly one fact, pinned right, and only when
            // it is true: a subagent this session spawned is still running. The
            // legend that used to live here — model, dialect, endpoint,
            // verbosity — was a row of attention paid for ever for facts read
            // once; this one is a fact that exists only while it does.
            let running = self
                .subagents
                .iter()
                .filter(|s| s.state == "running")
                .count();
            let top = if running > 0 {
                self.cfg.palette().paint(
                    Role::Pending,
                    &format!(
                        "{running} subagent{} running",
                        if running == 1 { "" } else { "s" }
                    ),
                )
            } else {
                String::new()
            };
            chrome.push(self.box_edge(w, '╭', '╮', "", &top));
        }
        let caret_at = chrome.len() + caret_row;
        chrome.extend(input_rows);
        if boxed {
            // The bottom edge, pinned right: the alarm as a triangle — the
            // counters behind it are /status's, and were never worth a resident
            // sentence of bright yellow — and the turn's own status beside it.
            let mut right: Vec<String> = Vec::new();
            if self.alarmed() {
                right.push(self.cfg.palette().paint(Role::Attention, "⚠"));
            }
            let status = self.turn_status(w);
            if !status.is_empty() {
                right.push(status);
            }
            chrome.push(self.box_edge(w, '╰', '╯', "", &right.join(" · ")));
        } else if self.alarmed() {
            chrome.push(self.status_line(w));
        }
        if hint {
            chrome.push(self.hint_bar(w));
        }
        // Backstop. The ladder above cannot always win — `h` can be 2 — and a head
        // that returns more lines than the terminal has scrolls its own composer
        // off the bottom.
        if chrome.len() >= h {
            chrome.drain(..chrome.len() - h.max(1));
        }

        // The session header, pinned above everything. One row, and it is the row
        // both surveyed heads spend first: opencode puts the title left and
        // `39,413  20% ($0.29)` right, grok-build puts the cwd left and `9.5K /
        // 500K` right. What is here is the same shape with this harness's own
        // numbers — see `header_line` for which of theirs are deliberately absent.
        //
        // It costs a row of transcript and it is worth it because the question it
        // answers ("which session am I in, and how big has it got") is otherwise
        // answered by scrolling.
        let header = (h >= 6 && !self.session_id.is_empty()).then(|| self.header_line(w));
        let room = h
            .saturating_sub(chrome.len() + usize::from(header.is_some()))
            .max(1);
        let mut out = if self.help {
            let mut help = help_lines(&self.cfg, w);
            help.truncate(room);
            help
        } else if self.stats {
            let mut rows = self.status_lines(w);
            rows.truncate(room);
            rows
        } else if self.picker {
            let mut rows = self.picker_lines(w);
            rows.truncate(room);
            self.picker_rows_drawn = rows.len();
            rows
        } else if self.todos_pane {
            let mut rows = self.todos_lines(w);
            rows.truncate(room);
            rows
        } else if self.sub_out.is_some() {
            let mut rows = self.sub_out_lines(room);
            rows.truncate(room);
            rows
        } else if self.subagents_pane {
            let mut rows = self.subagents_lines(w);
            rows.truncate(room);
            rows
        } else if self.jobs_pane {
            let mut rows = self.jobs_lines(w);
            rows.truncate(room);
            rows
        } else {
            self.body_window(room)
        };
        while out.len() < room {
            out.push(String::new());
        }
        out.truncate(room);
        let mut body_rows = out.len();
        if let Some(l) = header {
            out.insert(0, l);
            body_rows += 1;
        }
        out.extend(chrome);
        out.truncate(h);
        // The caret is the affordance. It goes where the composer says, and the
        // terminal draws it as a steady block because `term::enter` asked for one.
        self.cursor = Some((
            (body_rows + caret_at).min(out.len().saturating_sub(1)),
            caret_col.min(w.saturating_sub(1)) + gutter,
        ));
        let pad = " ".repeat(gutter);
        out.into_iter()
            .map(|l| {
                let l = trim_to(&l, w);
                if gutter == 0 || l.is_empty() {
                    l
                } else {
                    format!("{pad}{l}")
                }
            })
            .collect()
    }

    /// Columns of empty space down each side of the frame.
    ///
    /// **Two**, matching the transcript container both surveyed heads use —
    /// opencode's session view is one box with `paddingLeft={2} paddingRight={2}`
    /// around the message list *and* the prompt, which is why its header, its
    /// answers and its input all start in the same column.
    ///
    /// It also has to agree with [`card::REASONING_RAIL_WIDTH`], which is the one
    /// indent already on the screen: the rail is two columns, so reasoning text
    /// lands exactly one gutter further in than body text and the page reads as a
    /// single two-column step rather than as two unrelated indents.
    pub const GUTTER: usize = 2;

    /// The gutter this terminal can afford. It is the first thing given up on a
    /// very narrow screen, before any content is: four columns out of forty is a
    /// tenth of the line, and out of twenty it is a fifth.
    fn gutter(w: usize) -> usize {
        if w >= 40 { Self::GUTTER } else { 0 }
    }

    /// Where the terminal's own caret belongs, from the last [`App::screen`].
    pub fn cursor(&self) -> Option<(usize, usize)> {
        self.cursor
    }

    /// The composer's rows, and the caret's `(row, column)` within them.
    ///
    /// The editor lays out the text; this puts a wall on each side of it and pads
    /// to the full width, so the row is a *field* and not a line of text that
    /// happens to be at the bottom. Padding matters for more than looks:
    /// `term::paint` erases each row it rewrites with `\x1b[K`, and a row that
    /// stops early leaves the field's right wall hanging in space.
    fn composer_rows(&self, w: usize, max_rows: usize, boxed: bool) -> (Vec<String>, usize, usize) {
        let inner = self.composer_cols();
        let (lines, (crow, ccol)) = if self.secret.is_some() {
            // A dot per character, and the caret after the last one. The text
            // itself is never rendered, not even to compute a width.
            let n = self.secret_buf.chars().count();
            (vec!["•".repeat(n)], (0, n))
        } else {
            self.editor.render(inner, self.cfg.palette())
        };
        let n = lines.len().max(1);
        let show = max_rows.clamp(1, n);
        // Scroll to the row being edited, never to the top: a composer taller than
        // the rows it was given must still show the caret, or the person is typing
        // somewhere they cannot see.
        let start = crow.saturating_sub(show - 1).min(n - show);
        let mut out = Vec::with_capacity(show);
        for i in start..start + show {
            let body = lines.get(i).cloned().unwrap_or_default();
            out.push(if boxed {
                // `Role::Faint`, not `sgr::GREY`. 90 is the theme's *bright
                // black*, which `style.rs` measured landing within a hair of the
                // background on several light themes; the attribute de-emphasises
                // whatever foreground the reader has already chosen.
                let wall = self.cfg.palette().paint(Role::Faint, "│");
                format!("{wall} {}{wall}", width::fit(&body, inner + 1))
            } else {
                trim_to(&body, w)
            });
        }
        let col = if boxed { ccol + 2 } else { ccol };
        (out, crow.saturating_sub(start), col)
    }

    /// One edge of the box, with a legend inlaid at the left and one pinned to
    /// the right.
    ///
    /// `╰────────── ⚠ · ⠹ Responding · 4.2s ─╯`. A legend rather than a
    /// decoration **when there is something to say**: an edge with nothing to
    /// say renders plain, because a row of attention paid for ever for a fact
    /// read once is the mistake the composer's top border already made once.
    /// The right legend yields room to the left one, yields itself by
    /// truncation next, and is dropped before the border is allowed to wrap.
    fn box_edge(&self, w: usize, open: char, close: char, left: &str, right: &str) -> String {
        let w = w.max(4);
        let inner = w - 2;
        // A legend may arrive already painted — the alarm is in the attention
        // role, the turn's spinner in pending — and `Palette::paint` closes with
        // a plain reset, which restores the *terminal default* and not the grey
        // of the border it is inlaid into. So the border reopens itself on the
        // far side of each legend. Same defect and same fix as
        // `style::Painter::inside`, one layer up: a reset is not a restore.
        let reopen = self.cfg.palette().open(Role::Faint);
        let mut left_text = String::new();
        if !left.is_empty() && inner >= 10 {
            left_text = format!("─ {}{reopen} ", trim_to(left, inner - 4));
        }
        let left_cols = visible_width(&left_text);
        let mut right_text = String::new();
        if !right.is_empty() && inner >= 10 {
            let room = inner.saturating_sub(left_cols + 2);
            if room >= 4 {
                right_text = format!(" {}{reopen} ─", trim_to(right, room));
            }
        }
        let fill = inner.saturating_sub(left_cols + visible_width(&right_text));
        self.cfg.palette().paint(
            Role::Faint,
            &format!("{open}{left_text}{}{right_text}{close}", "─".repeat(fill)),
        )
    }

    /// The bottom bar: what the keys do, right now.
    ///
    /// The composer owns the first half and changes it after the first Esc or
    /// Ctrl+C — that is how anyone finds out a double-tap exists. The head owns
    /// the second half, which is its own keys.
    fn hint_bar(&self, w: usize) -> String {
        let p = self.cfg.palette();
        let mut s = self.editor.hint(self.turn_running(), self.now_ms, p);
        let tail = if self.help || self.stats {
            "esc closes this"
        } else if self.picker {
            "type a number to switch · /new [title] · esc closes"
        } else if self.todos_pane {
            "the model's plan above, the repo's queue below · esc closes"
        } else if self.subagents_pane {
            "subagents this session spawned · esc closes"
        } else if self.jobs_pane {
            "background jobs this session started · esc closes"
        } else if !self.open.is_empty() {
            "type an option above to answer · /help"
        } else {
            "ctrl-s sessions · ctrl-p todos · ctrl-g subagents · ctrl-r thinking · ctrl-t tool output · ctrl-q jobs · tab completes /commands · /help"
        };
        s.push_str(&p.paint(Role::Faint, &format!(" · {tail}")));
        trim_to(&s, w)
    }

    /// The visible `room` lines of the body, and nothing else built.
    fn body_window(&mut self, room: usize) -> Vec<String> {
        let cfg = self.cfg.clone();
        let (think, tool) = (self.reasoning, self.tools);
        let raw = self.raw_calls;
        if self.hist_width != cfg.width {
            self.hist_width = cfg.width;
            self.invalidate_history();
        }
        // Rows and notes, interleaved in the order they happened. A note anchored
        // at row N renders between row N-1 and row N, which is where it was when it
        // arrived.
        //
        // Destructured rather than indexed through `self`, so a row can be rendered
        // *while* the rendered lines are being appended and the tool-target table
        // is being read — three disjoint fields, one borrow each, no clone of a row
        // per frame.
        // Does the transcript already own this turn's content? If so the live pane
        // is a duplicate of history and only its summary line survives — otherwise
        // the answer is on the screen twice, once in the wrong order.
        //
        // Computed before the walk, because the walk needs it: it is the
        // difference between "the pane below is drawing this call" and "nothing
        // is".
        let superseded = !matches!(
            self.turn.as_ref().and_then(|t| t.state.as_ref()),
            Some(TurnState::Running) | None
        ) && self.turn.as_ref().is_some_and(|t| {
            !t.appended.is_empty()
                && t.appended.iter().all(|id| {
                    self.items
                        .iter()
                        .find(|r| &r.item_id == id)
                        .is_some_and(|r| r.item.is_some())
                })
        });
        // The rows the live pane is still drawing. An assistant row in this set
        // does **not** draw its own unsettled calls: the pane below is drawing
        // them, with a spinner and a running clock, and `→ Read foo.rs · no result`
        // above a `◐ Reading foo.rs` is both a duplicate and, while the call is
        // still running, false.
        //
        // Empty once the pane has stood down, which is what stops a call the turn
        // was interrupted in the middle of from vanishing off the screen entirely:
        // nothing is drawing it, so the assistant row draws it, and says it never
        // came back.
        let in_flight: std::collections::HashSet<&str> = if superseded {
            std::collections::HashSet::new()
        } else {
            self.turn
                .as_ref()
                .map(|t| t.appended.iter().map(String::as_str).collect())
                .unwrap_or_default()
        };
        {
            let App {
                hist_lines,
                hist_upto,
                hist_marks,
                note_upto,
                items,
                notes,
                call_targets,
                call_ms,
                call_edits,
                hist_class,
                hist_renders,
                diff_split,
                ..
            } = self;
            let diff_split = *diff_split;
            loop {
                let note_next = notes
                    .get(*note_upto)
                    .is_some_and(|(at, _)| *at <= *hist_upto);
                if note_next {
                    if !hist_lines.is_empty() {
                        hist_lines.push(String::new());
                    }
                    hist_lines.extend(note_lines(&cfg, &notes[*note_upto].1));
                    *hist_class = Some(RowClass::Other);
                    *note_upto += 1;
                } else if *hist_upto < items.len() {
                    // Where the walk stands before this row, so a later "from row
                    // k on" can come back to exactly here. Recorded for every row,
                    // including one that renders to nothing, because the mark is
                    // indexed by row and a gap would misalign every mark after it.
                    if hist_marks.len() == *hist_upto {
                        hist_marks.push(HistMark {
                            lines: hist_lines.len(),
                            note_upto: *note_upto,
                            class: *hist_class,
                        });
                    }
                    // An assistant row carries the arguments for the calls it
                    // proposed, and the tool-result rows that follow it want the
                    // same label. Learning them here, in transcript order, is what
                    // lets a head that attached *after* a turn still say which file
                    // was read — the proposal event is long gone and the row is the
                    // only place the arguments survive.
                    //
                    // **Replaced, not merged.** `call_0` is round-positional, so a
                    // merge is how round 4's `call_0` came to be labelled with
                    // round 1's path. An assistant row opens a new round and its
                    // calls are the only ones the rows after it can be about; one
                    // with no calls at all opens a round with no calls, and a
                    // stray result then has to say so rather than borrow.
                    let mut answered: std::collections::HashSet<String> =
                        std::collections::HashSet::new();
                    if let Some(TranscriptItem::Assistant { tool_calls, .. }) =
                        items[*hist_upto].item.as_ref()
                    {
                        call_targets.clear();
                        for c in tool_calls {
                            call_targets.insert(
                                c.id.clone(),
                                letibot_sessionlog::display_target(&c.arguments),
                            );
                        }
                        answered = round_results(items, *hist_upto);
                    }
                    *hist_renders += 1;
                    let (class, rows) = item_lines(
                        &items[*hist_upto],
                        &ItemCtx {
                            cfg: &cfg,
                            think,
                            tools: tool,
                            raw,
                            targets: call_targets,
                            answered: &answered,
                            drawn_live: in_flight.contains(items[*hist_upto].item_id.as_str()),
                            elapsed_ms: call_ms.get(&items[*hist_upto].item_id).copied(),
                            edit: call_edits.get(&items[*hist_upto].item_id),
                            diff_split,
                        },
                    );
                    // A row that rendered nothing gets no separator either. An
                    // assistant row whose text is `"\n\n\n"` and whose every call
                    // is drawn by its own result row is a real and common shape —
                    // it is what a tool-calling round looks like — and paying two
                    // blank lines for it puts a hole in the transcript.
                    if !rows.iter().all(|l| l.trim().is_empty()) {
                        // Air where the KIND changes, not between every pair of
                        // rows. Two tool cards in a row are one block and read as
                        // one; a blank between each of them was a third of the
                        // vertical budget spent separating things a glyph in the
                        // first column already separates.
                        let pack = *hist_class == Some(RowClass::Activity)
                            && class == RowClass::Activity;
                        if !hist_lines.is_empty() && !pack {
                            hist_lines.push(String::new());
                        }
                        hist_lines.extend(rows);
                        *hist_class = Some(class);
                    }
                    *hist_upto += 1;
                } else {
                    break;
                }
            }
        }

        // Disjoint field borrows, so the history can be lent to the frame while the
        // block caches are still being written to.
        let App {
            hist_lines, turn, ..
        } = self;
        let mut segs: Vec<Seg<'_>> = vec![Seg::Borrowed(hist_lines)];
        // The history no longer ends with a blank — separators go *before* a row
        // now, so the last row of the transcript is the last line of it. The live
        // pane therefore brings its own.
        let gap = vec![String::new()];
        if !hist_lines.is_empty() {
            segs.push(Seg::Borrowed(&gap));
        }

        if let Some(t) = turn {
            let running = matches!(t.state, Some(TurnState::Running));
            let think_elapsed = if t.think_started_ms == 0 {
                None
            } else {
                Some(t.think_last_ms.saturating_sub(t.think_started_ms))
            };
            let now_ms = t.last_ms;
            let TurnPane {
                text,
                reasoning,
                text_cache,
                reasoning_cache,
                calls,
                settled_calls,
                raw_call,
                writing_call,
                state,
                ..
            } = t;
            let ind = activity_indent(cfg.width);
            if !superseded && !reasoning.is_empty() {
                // Narrower by the rail and by the step it is set in. Getting this
                // wrong makes the block one row taller than the space reserved for
                // it, which moves everything below it by a line every frame — which
                // is one of the things being called flicker.
                let rcfg = reasoning_cfg(&cfg);
                reasoning_cache.set_decor(reasoning_decor(&cfg));
                segs.push(Seg::Owned(step_in(
                    vec![thinking_header(
                        &cfg,
                        reasoning.raw(),
                        think.is_open(),
                        running,
                        think_elapsed,
                    )],
                    ind,
                )));
                if think.is_open() {
                    let (stable, tail) =
                        reasoning_cache.split(reasoning, &rcfg, cfg.budget.reasoning_lines);
                    segs.push(Seg::Borrowed(stable));
                    segs.push(Seg::Owned(tail));
                } else {
                    // Folded, but a *running* turn still shows the last line, so
                    // "it is thinking" and "it is stuck" do not look the same.
                    let d = reasoning_decor(&cfg);
                    segs.push(Seg::Owned(vec![
                        d.apply(&last_line(reasoning.raw(), &rcfg)),
                    ]));
                }
                segs.push(Seg::Owned(vec![String::new()]));
            }
            if !superseded {
                // Only the calls the transcript has NOT taken over yet. The rest
                // are already on the screen above as settled cards with their
                // output under them, and drawing them here as well was the second
                // half of the doubling: a turn eight calls deep showed eight live
                // rows under eight settled ones, in the same order, saying less.
                let live = calls.get(*settled_calls..).unwrap_or(&[]);
                if !live.is_empty() {
                    let mut owned: Vec<String> = Vec::new();
                    for c in live.iter() {
                        owned.extend(step_in(call_card(c, &cfg, now_ms, tool, self.diff_split), ind));
                    }
                    owned.push(String::new());
                    segs.push(Seg::Owned(owned));
                }
                if !text.is_empty() {
                    let (stable, tail) = text_cache.split(text, &cfg, cfg.budget.body_lines);
                    segs.push(Seg::Borrowed(stable));
                    segs.push(Seg::Owned(tail));
                    // The same air the reasoning block and the call cards already
                    // carry. Without it the last line of a running decode touches
                    // the top border of the composer, and the blank appears only
                    // when the turn ends and the pane stands down, so the screen
                    // grows by a line at the moment the reader finally has time to
                    // look. Padding while running is also the shape the transcript
                    // row takes over, so nothing reflows at the handoff.
                    segs.push(Seg::Owned(vec![String::new()]));
                }
                // The call the model is writing right now. The markup itself is
                // never here: what is on the screen is that a call is being
                // written, which is the fact the raw text was accidentally
                // conveying and the only part of it a reader wanted.
                if *writing_call {
                    segs.push(Seg::Owned(step_in(
                        vec![writing_call_line(&cfg, now_ms)],
                        ind,
                    )));
                }
                if raw && !raw_call.is_empty() {
                    segs.push(Seg::Owned(raw_call_lines(&cfg, raw_call)));
                }
            }
            if let Some(s) = state {
                segs.push(Seg::Owned(turn_footer(&cfg, s)));
            }
        }

        // The prompts this head has sent that the transcript does not hold yet, at
        // the tail — the place their rows will land — so a message typed while a
        // turn runs stays on the screen until the step boundary appends it. See
        // `pending_prompts` for why this is the head's own queue and not the hub's.
        if !self.pending_prompts.is_empty() {
            let mut owned: Vec<String> = vec![String::new()];
            for q in &self.pending_prompts {
                owned.extend(queued_lines(q, &cfg));
            }
            segs.push(Seg::Owned(owned));
        }

        // Nothing has happened yet. An empty screen with a status line under it is
        // indistinguishable from a head that attached to the wrong socket.
        let opening;
        if segs.iter().all(|s| s.len() == 0) {
            opening = vec![
                colour(&cfg, sgr::BOLD, "letibot"),
                String::new(),
                dim(
                    &cfg,
                    "attached, and this session has said nothing yet. Type a question and \
                     press enter.",
                ),
                dim(
                    &cfg,
                    "The turn runs in the daemon: closing this window does not stop it, and \
                     reattaching picks it up.",
                ),
                String::new(),
                dim(&cfg, "/help lists the keys."),
            ];
            segs.push(Seg::Borrowed(&opening));
        }

        let total: usize = segs.iter().map(Seg::len).sum();
        self.body_len = total;
        // Clamped so the window stays **full**, not so the last line stays on
        // screen. It was `total - 1`, which meant scrolling to the top left a
        // one-line window — and since the banner below overwrites the last line of
        // the window, the whole screen went blank with `── scrolled back · 56 lines
        // below` at the top of it. Found by pressing PageUp six times under tmux,
        // which is a thing a person does and no test did.
        self.scroll = self.scroll.min(total.saturating_sub(room.max(1)));
        let end = total.saturating_sub(self.scroll);
        let start = end.saturating_sub(room);
        let mut out = take_window(&segs, start, end);
        if self.scroll > 0 {
            let behind = total - end;
            let last = out.len().saturating_sub(1);
            out[last] = colour(
                &self.cfg,
                sgr::YELLOW,
                &format!(
                    "── scrolled back · {behind} lines below · ↓ or esc to follow · \
                     wheel scrolls · shift+drag selects"
                ),
            );
        }
        out
    }

    /// The session header: which session, what it is talking to, and how big it
    /// has got.
    ///
    /// ```text
    ///   ▌ the cache question  ~/Projects/letibot   2/4 · glm-5.3-flash · 41.2k ctx · 92% cached · 45 tok/s · 12.3s · 1.2k out
    /// ```
    ///
    /// The model name is here and not on the composer's border, where it used to
    /// sit with the dialect and the endpoint beside it: the border is the row the
    /// eye crosses on every return to the field, and a socket address is not part
    /// of a sentence. The last three fields are the last turn's decode rate, wall
    /// time and output, and this is their only home: they used to close the turn
    /// footer, on a line that also repeated the context and cache numbers
    /// already above — one fact, two places, and the reader stops to check
    /// whether they agree. The footer keeps only the ending that is news; an
    /// ordinary one leaves no footer line at all.
    ///
    /// **What is deliberately not on it.** opencode's right-hand side reads
    /// `39,413  20% ($0.29)` — tokens, context *used as a percentage*, and money.
    /// The percentage needs the context window and the price needs a tariff, and
    /// this harness has neither: nothing on the wire carries `n_ctx`, and the model
    /// is on the other side of a Unix socket on this box and costs nothing per
    /// token. Rendering `20%` against a denominator nobody sent would be the same
    /// move as rendering `0.0s` for a call that was never timed.
    ///
    /// What is here instead is the number this harness exists to move and neither
    /// surveyed head can show at all: **how much of the prompt was cached**. It is
    /// `f_sim` — cached over *this* prompt — and it is labelled `cached`, never
    /// `f_keep`, which needs the previous turn's entry as its denominator.
    /// # It degrades by deletion, one field at a time
    ///
    /// The first version handed the two halves to `split_row`, which drops the
    /// **whole** right half when they do not both fit — correct for the in-flight
    /// line, where the left half is what is happening, and wrong here, where the
    /// right half is the part you cannot get any other way. Measured under tmux at
    /// 110 columns: an 82-column path plus a 27-column tail is 111, and the entire
    /// tail vanished with nothing to say it had. So the tail is built in priority
    /// order and the path is shortened from its left before anything is dropped —
    /// a path is recognisable from its end, and a token count is not recoverable
    /// from anywhere else on the screen.
    fn header_line(&self, w: usize) -> String {
        let p = self.cfg.palette();
        let name = self.session_label(&self.session_id);

        // Most valuable first: which of several sessions this is, then how big the
        // prompt has got, then how much of it the cache saved.
        let mut right: Vec<String> = Vec::new();
        // Shown for one session too. It used to be gated on `len() > 1`, and the
        // effect was that the *only* case with no session identity anywhere on the
        // screen — one untitled session, whose name is therefore an opaque id — was
        // also the case with no position indicator. Two absences do not add up to a
        // fact, and "1/1" is a fact: this daemon holds one session and you are in it.
        let at = self
            .sessions
            .iter()
            .position(|s| s.session_id == self.session_id)
            .map(|i| i + 1)
            .unwrap_or(0);
        right.push(format!("{at}/{}", self.sessions.len().max(1)));
        // What this session is talking to: the daemon's own word from `Hello`, or
        // — before that has arrived — the model the running turn named. It lived
        // on the composer's top border, the one row the eye crosses on every
        // return to the field; the header is where this session's facts live now.
        // The dialect and the endpoint do not ride along: the dialect's name is
        // the model's name whenever the two differ at all, and the endpoint is a
        // socket path, which is the daemon's business and not the sentence's.
        let model = if !self.wiring.model.is_empty() {
            self.wiring.model.clone()
        } else {
            self.model.clone()
        };
        if !model.is_empty() {
            right.push(model);
        }
        // Live prefill numbers win over the last turn's: while a turn is running,
        // "how big is this prompt" is a question about the prompt being sent.
        let usage = match self.turn.as_ref().and_then(|t| t.progress.as_ref()) {
            Some(pp) if pp.total > 0 => Some((pp.total, pp.cache)),
            _ => self
                .usage
                .filter(|u| u.prompt_tokens > 0)
                .map(|u| (u.prompt_tokens, u.cached_tokens)),
        };
        if let Some((total, cached)) = usage {
            right.push(format!("{} ctx", progress::thousands(total)));
            right.push(format!("{:.0}% cached", cached as f64 * 100.0 / total as f64));
        }
        // The last turn's speed and duration, measured when it ended. A rate nobody
        // measured is refused, the rule the footer's rate was held to when it lived
        // there: a turn that decoded nothing has no `predicted_ms`, and `0 tok/s`
        // would be a number nobody took. Dropped first on a narrow screen — the
        // context numbers are the ones this header exists for.
        if let (Some(u), Some(tm)) = (self.usage, self.last_timings) {
            if tm.predicted_ms > 0.0 {
                right.push(format!(
                    "{:.0} tok/s",
                    u.predicted_tokens as f64 * 1000.0 / tm.predicted_ms
                ));
            }
            if tm.wall_ms > 0 {
                right.push(dur_human(tm.wall_ms));
            }
            // And how much the answer was — the last of the turn's numbers, and
            // the reason an ordinary ending leaves the body with no footer line
            // at all.
            if u.predicted_tokens > 0 {
                right.push(format!("{} out", progress::thousands(u.predicted_tokens)));
            }
        }
        // Drop from the end until it leaves room for the name.
        let name_cols = visible_width(&name) + 2;
        while right.len() > 1 && name_cols + right.join(" · ").chars().count() + 2 > w {
            right.pop();
        }
        let tail = right.join(" · ");
        let tail_cols = if tail.is_empty() { 0 } else { tail.chars().count() + 2 };

        let mut left = String::new();
        left.push_str(&p.paint(Role::UserAccent, "▌ "));
        left.push_str(&p.paint(Role::Strong, &name));
        let mut left_cols = 2 + visible_width(&name);
        // The workspace fills whatever is left, shortened from its *left*: the end
        // of a path is the part that identifies it.
        if !self.wiring.workspace.is_empty() {
            let path = tilde(&self.wiring.workspace);
            let room = w.saturating_sub(left_cols + tail_cols + 2);
            if room >= 8 {
                let shown = ellipsise_left(&path, room);
                left.push_str(&p.paint(Role::Faint, &format!("  {shown}")));
                left_cols += 2 + visible_width(&shown);
            }
        }
        let pad = w.saturating_sub(left_cols + tail.chars().count());
        trim_to(
            &format!("{left}{}{}", " ".repeat(pad), p.paint(Role::Faint, &tail)),
            w,
        )
    }

    /// The session picker: every session this daemon holds, and how to go there.
    ///
    /// A screen and not a mode. There is no pointer in this head and no selection,
    /// so a cursor here would be a second keymap for a program whose whole input
    /// surface is one line — the same argument the folds settled. The affordance is
    /// the number in the left column, which you type into the composer that is
    /// still there under the list.
    /// The todos pane: two lists that are deliberately not one.
    ///
    /// The first is this session's plan — what the model last wrote through
    /// `todo_write`, and the only list here that anything in this session can
    /// change. The second is the repo's `TODO.md`, the **operator's** queue,
    /// shown as a section map and read-only on purpose: a pane that let a model
    /// tick the operator's boxes would let a plan edit its own backlog.
    fn todos_lines(&self, w: usize) -> Vec<String> {
        let mut out = vec![colour(&self.cfg, sgr::BOLD, "todos")];
        out.push(String::new());
        out.push(dim(
            &self.cfg,
            "  this session — the model's plan, live:",
        ));
        if self.todos.is_empty() {
            out.push(dim(
                &self.cfg,
                "    none written yet. The model writes them with todo_write.",
            ));
        }
        for t in &self.todos {
            let mark = match t.status {
                letibot_sessionlog::event::TodoStatus::Pending => "[ ]",
                letibot_sessionlog::event::TodoStatus::InProgress => "[~]",
                letibot_sessionlog::event::TodoStatus::Completed => "[x]",
            };
            out.push(format!("    {mark} {}", t.content));
        }
        out.push(String::new());
        out.push(dim(
            &self.cfg,
            "  the repo's TODO.md — the operator's queue, read-only here:",
        ));
        match &self.repo_todos {
            None => out.push(dim(&self.cfg, "    not read yet — close and reopen the pane.")),
            Some(lines) => {
                if lines.is_empty() {
                    out.push(dim(&self.cfg, "    no sections found."));
                }
                out.extend(lines.iter().cloned());
            }
        }
        out.push(String::new());
        out.push(dim(
            &self.cfg,
            "  the file itself is in the workspace; this pane never writes it.",
        ));
        out.into_iter()
            .map(|l| trim_to(&l, w))
            .collect()
    }

    /// The subagent tree: the subagents this session spawned, their state and their
    /// prompt. A subagent is also a session, so the last line points at `ctrl-s`.
    fn subagents_lines(&self, w: usize) -> Vec<String> {
        let mut out = vec![colour(&self.cfg, sgr::BOLD, "subagents")];
        out.push(String::new());
        if self.subagents.is_empty() {
            out.push(dim(
                &self.cfg,
                "    none spawned yet. The model spawns them with the task tool.",
            ));
        }
        for (i, s) in self.subagents.iter().enumerate() {
            let (mark, state_colour) = match s.state.as_str() {
                // Not a session yet: the child is copying its workspace or booting.
                // Enter does nothing here, and the row says so below.
                "opening" => ("[…]", ""),
                "running" => ("[~]", sgr::YELLOW),
                "done" => ("[x]", sgr::GREEN),
                "failed" => ("[!]", sgr::RED),
                _ => ("[ ]", ""),
            };
            let picked = i == self.subagents_sel.min(self.subagents.len().saturating_sub(1));
            let left = format!(
                "{} {} {}",
                if picked { "▸" } else { " " },
                colour(&self.cfg, state_colour, mark),
                s.prompt
            );
            let left = if picked {
                format!("{}{}{}", sgr::REVERSE, left, sgr::RESET)
            } else {
                left
            };
            out.push(left);
            out.push(dim(
                &self.cfg,
                &format!(
                    "       {} · role {} · {}{}",
                    short_id(&s.session_id),
                    s.role,
                    s.state,
                    if s.state == "opening" {
                        " — not attachable yet"
                    } else {
                        ""
                    }
                ),
            ));
        }
        out.push(String::new());
        out.push(dim(
            &self.cfg,
            "    arrows move, Enter reads the subagent's output, o switches into it — subagents are hidden from ctrl-s.",
        ));
        out.into_iter()
            .map(|l| trim_to(&l, w))
            .collect()
    }

    /// The output view: a terminal, not a document. The tail shows by default;
    /// arrows walk back toward the beginning; `scroll` counts lines hidden off
    /// the bottom and is clamped here, where the visible height is actually
    /// known — a key handler cannot clamp what it cannot see.
    fn sub_out_lines(&mut self, room: usize) -> Vec<String> {
        let Some(v) = self.sub_out.as_mut() else {
            return Vec::new();
        };
        let mut out = vec![colour(
            &self.cfg,
            sgr::BOLD,
            &format!("subagent output — {}", short_id(&v.session_id)),
        )];
        if v.dropped > 0 {
            out.push(dim(
                &self.cfg,
                &format!(
                    "    {} earlier event{} fell off the daemon's scrollback before this read",
                    v.dropped,
                    if v.dropped == 1 { "" } else { "s" }
                ),
            ));
        }
        out.push(String::new());
        let footer = 1;
        let visible = room.saturating_sub(out.len() + footer).max(1);
        let max_scroll = v.lines.len().saturating_sub(visible);
        v.scroll = v.scroll.min(max_scroll);
        let end = v.lines.len() - v.scroll;
        let start = end.saturating_sub(visible);
        for l in &v.lines[start..end] {
            out.push(l.clone());
        }
        while out.len() < room.saturating_sub(footer) {
            out.push(String::new());
        }
        let spill = v.spill.as_deref().unwrap_or("not written");
        out.push(dim(
            &self.cfg,
            &format!("    arrows scroll, Enter re-reads, Esc back — full: {spill}"),
        ));
        out.truncate(room);
        out
    }

    fn jobs_lines(&self, w: usize) -> Vec<String> {
        let mut out = vec![colour(&self.cfg, sgr::BOLD, "background jobs")];
        out.push(String::new());
        if self.jobs.is_empty() {
            out.push(dim(
                &self.cfg,
                "    none. The model backgrounds a command with bash's `background: \
                 true`; ctrl-o moves the running one.",
            ));
        }
        for j in &self.jobs {
            let (mark, state_colour) = if j.state.is_empty() {
                ("[~]", sgr::YELLOW)
            } else if j.state.starts_with("exited 0") {
                ("[x]", sgr::GREEN)
            } else {
                ("[!]", sgr::RED)
            };
            // The command is the call's §4.1 display target, joined at draw time:
            // the arguments reach a head with the transcript, which for a
            // backgrounded call is the same moment as the finish. A settlement
            // replayed without its start has no call to join, and the row then
            // says so rather than guessing.
            let command = self
                .turn
                .as_ref()
                .and_then(|t| t.calls.iter().find(|c| c.call_id == j.call_id))
                .map(|c| c.target.clone())
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| "(command not in this head's window)".to_string());
            out.push(format!(
                "{} {} {}",
                colour(&self.cfg, state_colour, mark),
                j.job,
                command
            ));
            let tail = if j.state.is_empty() {
                "running".to_string()
            } else {
                format!(
                    "{} · {} out · ran {}.{:01}s",
                    j.state,
                    bytes_human(j.produced),
                    j.elapsed_ms / 1000,
                    (j.elapsed_ms % 1000) / 100,
                )
            };
            let how = if j.how.is_empty() {
                "how: not in this head's window".to_string()
            } else {
                j.how.clone()
            };
            out.push(dim(&self.cfg, &format!("       {} · {}", how, tail)));
        }
        out.push(String::new());
        out.push(dim(
            &self.cfg,
            "    a job still shows running until the daemon says it settled — between \
             turns, that saying is the daemon's alone.",
        ));
        out.into_iter()
            .map(|l| trim_to(&l, w))
            .collect()
    }

    fn picker_lines(&self, w: usize) -> Vec<String> {
        let p = self.cfg.palette();
        let mut out = vec![
            colour(&self.cfg, sgr::BOLD, "sessions in this daemon"),
            String::new(),
        ];
        if self.sessions.is_empty() {
            out.push(dim(
                &self.cfg,
                "  none listed yet — the daemon has not answered, or this head is \
                 replaying a recorded log and has no daemon to ask.",
            ));
        }
        for (i, s) in self.sessions.iter().enumerate() {
            let here = s.session_id == self.session_id;
            // The same ladder the decision prompt draws: the mark IS the thing Enter
            // takes, and the row it sits on is inverse. The session this head is in
            // keeps its bold name, so "where am I" and "what Enter takes" stay two
            // readable facts even when they are different rows.
            let picked = i == self.picker_sel.min(self.sessions.len().saturating_sub(1));
            let mark = if picked { "▸" } else { " " };
            let name = if s.title.is_empty() {
                short_id(&s.session_id)
            } else {
                s.title.clone()
            };
            let left = format!(
                "{mark} {:>2}  {}",
                i + 1,
                p.paint(if here { Role::Strong } else { Role::Plain }, &name),
            );
            let left = if picked {
                format!("{}{}{}", sgr::REVERSE, left, sgr::RESET)
            } else {
                left
            };
            // Busy is the fact a picker exists to show: switching away from a
            // running turn is fine — the daemon keeps generating — and switching
            // *into* one is how you go back and watch it.
            let mut facts: Vec<String> = Vec::new();
            if s.status.running {
                facts.push("generating".into());
            }
            if !s.live {
                // The whole difference between a row that costs one keystroke and a
                // row that costs a resume. Said in a word rather than implied by an
                // absent "generating".
                facts.push("on disk".into());
            }
            // The store's count when there is one: the view's `items` is bounded by
            // `ViewBounds` and is the length of what a head is *holding*, not the
            // length of the conversation. Reporting the smaller number as "rows"
            // makes a long session look short.
            let rows = s.stored_items as usize;
            let rows = if rows > 0 { rows } else { s.status.items };
            if rows > 0 {
                facts.push(format!("{rows} rows"));
            }
            if s.status.heads > 0 {
                facts.push(format!(
                    "{} head{}",
                    s.status.heads,
                    if s.status.heads == 1 { "" } else { "s" }
                ));
            }
            if !s.wiring.model.is_empty() {
                facts.push(s.wiring.model.clone());
            }
            let right = p.paint(
                if s.status.running {
                    Role::Pending
                } else {
                    Role::Faint
                },
                &facts.join(" · "),
            );
            out.push(trim_to(&split_row(&left, &right, w), w));
            // The full id under **every** row, not only the named ones. It used to be
            // printed only when a title had displaced it, so the sessions whose id
            // you might actually need to type — the unnamed ones, the ones you would
            // pass to `letibot --session` — were the ones showing a truncation.
            //
            // The workspace goes on the same line. For a stored session it is the
            // only thing on the row that says what the conversation was *about*: an
            // unnamed session shows a short id and a model alias every other row also
            // has, and two of those are indistinguishable until you switch into one.
            let under = if s.wiring.workspace.is_empty() {
                format!("      {}", s.session_id)
            } else {
                format!("      {}  {}", s.session_id, tilde(&s.wiring.workspace))
            };
            out.push(trim_to(&dim(&self.cfg, &under), w));
        }
        out.push(String::new());
        out.push(dim(
            &self.cfg,
            "  ↑↓ moves · enter switches · or type a number or part of a name and press \
             enter · /new [title] makes one · /rename NAME names this one · esc closes",
        ));
        out.push(dim(
            &self.cfg,
            "  switching does not stop anything: a turn keeps running in the session you \
             left, and it is still there when you come back.",
        ));
        out
    }

    /// The password card: what is asking, for which command, and the two keys.
    fn secret_lines(&self, ask: &SecretAsk, w: usize) -> Vec<String> {
        let left = ask.deadline.saturating_sub(self.now_ms) / 1000;
        let mut out = Vec::new();
        out.push(colour(
            &self.cfg,
            sgr::YELLOW,
            &trim_to(&format!("sudo wants a password — {}", ask.prompt.trim()), w),
        ));
        for l in wrap(&format!("for: {}", ask.command), w) {
            out.push(l);
        }
        out.push(self.cfg.palette().paint(
            Role::Faint,
            &trim_to(
                &format!(
                    "type it below (shown as dots), Enter sends it once to sudo and nowhere \
                     else; Esc refuses · {left}s left"
                ),
                w,
            ),
        ));
        out
    }

    fn decision_lines(&self, d: &OpenDecision, w: usize) -> Vec<String> {
        // (helper below the method, so the rendering reads top to bottom)
        // **The question, then the thing itself, then the evidence.**
        //
        // One line used to carry all three — the sentence with the target
        // interpolated into it, layer A's verdict and intent list, and the kind —
        // and the operator's reading of it was *"no possible to see wtf was the
        // command i supposed to approve"*. A shell line inside a sentence inside a
        // taxonomy is not skimmable, and a permission an operator cannot evaluate is
        // one they approve out of fatigue, which is the whole mechanism this gate
        // exists to interrupt.
        //
        // So: the ask in yellow, the target alone and indented under it, the
        // deterministic reading dim below that. The target keeps its own lines even
        // when it wraps — a command is the one thing here worth the rows.
        let headline = match ask_without_target(&d.summary, &d.target) {
            Some(ask) => ask,
            None => d.summary.clone(),
        };
        let mut out = vec![colour(
            &self.cfg,
            sgr::YELLOW,
            &format!("? {headline} [{}]", d.kind),
        )];
        if !d.target.is_empty() {
            for l in wrap(&format!("    {}", d.target), w) {
                // Bold rather than yellow: the question is yellow, and the thing
                // being asked about is not a second question.
                out.push(colour(&self.cfg, sgr::BOLD, &l));
            }
        }
        if !d.detail.is_empty() {
            for l in wrap(&format!("  {}", d.detail), w) {
                out.push(colour(&self.cfg, sgr::DIM, &l));
            }
        }
        // **The model's verdict, above the ladder.**
        //
        // At `/mode supervised` the question is not *should this run* but *do you
        // agree with the model*, and a person cannot agree with something they were
        // not shown. It sits above the options rather than below because it is read
        // before the choice is made, and it is dim rather than yellow so it reads as
        // evidence beside the question rather than as a second question.
        //
        // Nothing here preselects an option. `self.sel` is untouched: the verdict
        // informs the answer and must never supply it, or the corpus fills with rows
        // recording a keystroke rather than a judgement.
        if let Some(a) = &d.advice {
            for l in wrap(&format!("  model says {}: {}", a.would, a.basis), w) {
                out.push(colour(&self.cfg, sgr::DIM, &l));
            }
            // **Said out loud when it is a fact, omitted when it is not one.**
            //
            // An oracle that AUTHORISED something while citing none of your words
            // is the case most worth a second look, and a blank line there reads as
            // "no note" rather than as "it could not ground this". That is the only
            // case this sentence is true about.
            //
            // It was printed unconditionally, and the citations never arrived from
            // the layer below — so a screen carried `the operator authorised this:
            // … (citing trail entry 0)` and `cites nothing from your words` one
            // line apart. The operator read the second: *"it also told that i didnt
            // mention anything while it was clear that i instructed the model to
            // use worktrees"*. An oracle that did not authorise has nothing to cite
            // and saying so about it claims a search that was never the question.
            let grounds = if !a.cites.is_empty() {
                Some(format!("cites {}", a.cites.join(" · ")))
            } else if a.would == "admit" {
                Some("cites nothing from your words".to_string())
            } else {
                None
            };
            let tail = match &grounds {
                Some(g) => format!("  {} · {g} · {} ms", a.by, a.latency_ms),
                None => format!("  {} · {} ms", a.by, a.latency_ms),
            };
            for l in wrap(&tail, w) {
                out.push(colour(&self.cfg, sgr::DIM, &l));
            }
        }
        // **One option per line, with the highlighted one marked.**
        //
        // They used to be joined with `·` onto one wrapped line, which is readable but
        // is not a control: there was nothing to move and nothing to press, so the only
        // way in was to type the id. A ladder the eye can walk is also a ladder Up/Down
        // can walk, and the two have to agree -- the marker IS the thing Enter takes.
        for (i, o) in d.options.iter().enumerate() {
            let picked = i == self.sel.min(d.options.len().saturating_sub(1));
            // The id stays on the line. Typing it still works, a script still uses it,
            // and a reader learning the ladder sees both spellings of the same choice.
            let body = format!(
                "{} {}  ({})",
                if picked { "▸" } else { " " },
                o.label,
                o.option_id
            );
            for l in wrap(&format!("  {body}"), w) {
                out.push(if picked {
                    // Inverse video rather than another colour: the prompt is already
                    // yellow, and a highlight that is a second hue reads as a second
                    // kind of thing rather than as "this one".
                    format!("{}{}{}", sgr::REVERSE, l, sgr::RESET)
                } else {
                    colour(&self.cfg, sgr::YELLOW, &l)
                });
            }
        }
        // The glob line is only shown when an *always allow* is actually on offer.
        // A hint for an option this request does not have is an affordance that does
        // nothing, which teaches the operator to stop reading the hints.
        let hint = if d
            .options
            .iter()
            .any(|o| o.kind == letibot_sessionlog::event::OptionKind::AllowAlways)
        {
            "  ↑↓ to choose · Enter to answer · or type the id ·              `allow_always <glob>` to set what the rule covers"
        } else {
            "  ↑↓ to choose · Enter to answer · or type the id"
        };
        out.push(colour(&self.cfg, sgr::YELLOW, hint));
        out
    }

    /// The turn's status, inlaid in the composer's bottom border and pinned
    /// right: the spinner, what phase the turn is in, and — once anything has
    /// arrived — how much. Empty when nothing is running.
    ///
    /// §5.6's prefill progress is the thing nothing surveyed reports, and it is
    /// not a small difference: both projects read for `letibot-ui` talk to a
    /// metered API and have **no prefill number at all**, so their in-flight line
    /// can only say `Responding… 15s`. `PromptProgress { total, cache, processed,
    /// time_ms }` supports a three-segment bar that answers *how far along* and
    /// *how much of this did the prefix cache save me* at once, which is the
    /// number §10 says the whole prompt pipeline exists to move.
    ///
    /// The arithmetic is `progress::Prefill`'s and the module header is worth
    /// reading before touching it: `processed` **includes** `cache`, so the
    /// fraction is `processed / total` and the work done is `processed - cache`.
    /// Read the other way a 90 %-cached prompt shows as 10 % done and then jumps
    /// to 100 %, which is the classic progress-bar lie; and the throughput has to
    /// divide by the computed tokens or it reports a cache hit as a speed in the
    /// hundreds of thousands.
    ///
    /// # The spinner runs on this head's clock
    ///
    /// It used to be keyed off `t.last_ms`, the last event's timestamp, and the
    /// defect was visible: a spinner that only moves when a token or a prefill
    /// batch arrives is not a spinner, it is a snapshot of one — a tool running
    /// thirty silent seconds froze it on one glyph. The phase is `now_ms` now,
    /// which the driver advances every tick whether or not anything arrived.
    /// The duration is measured against the same clock: head and daemon share
    /// the machine, which is the assumption the stuck line below already makes
    /// when it diffs `now_ms` against an event timestamp.
    fn turn_status(&self, w: usize) -> String {
        let t = match self.turn.as_ref() {
            Some(t) if matches!(t.state, Some(TurnState::Running)) => t,
            _ => return String::new(),
        };
        // **`started_ms == 0` means the turn came out of a snapshot**, which has no
        // timestamps — the same case `Phase::Replayed` exists for on a tool card.
        // `last_ms` is then an epoch millisecond and the difference is one, so the
        // line read `Responding · 496940h16m`. Found by switching into a session
        // that was mid-turn, which is the case the whole switch feature is for.
        // `Responding · 4.2s` when the duration was measured, and `Responding
        // since you attached` when it was not — never a number nobody took.
        let since = match t.started_ms {
            0 => " · started before this head attached".to_string(),
            started => format!(
                " · {}",
                progress::duration(self.now_ms.saturating_sub(started))
            ),
        };
        let p = self.cfg.palette();
        let spin = p.paint(Role::Pending, &progress::spinner(self.now_ms).to_string());
        match &t.progress {
            Some(pp) if pp.total > 0 && pp.processed < pp.total => {
                let pf = progress::Prefill {
                    total: pp.total,
                    cache: pp.cache,
                    processed: pp.processed,
                    time_ms: pp.time_ms,
                };
                format!(
                    "{spin} {}",
                    progress::prefill_line(&pf, w.saturating_sub(6), p),
                )
            }
            // Prefill finished, generation running. The prompt's size and cache
            // are on the header — live prefill numbers win there, and they win
            // for the whole turn, not only while prefill runs — so this carries
            // only what it alone knows: how much has arrived. Nothing yet is no
            // field at all: `0 chars` is a zero field wearing a measurement's
            // clothes. One **compact** string, for the border to pin right —
            // this used to `split_row` into a justified full-width line, which
            // as a legend put `Responding` at the left edge and clipped the
            // count it was carrying.
            Some(pp) if pp.total > 0 => {
                let mut s = p.paint(Role::Pending, &format!("{spin} Responding{since}"));
                if t.out_chars > 0 {
                    s.push_str(&p.paint(
                        Role::Faint,
                        &format!(" · {} chars", progress::thousands(t.out_chars as u64)),
                    ));
                }
                s
            }
            _ => p.paint(Role::Pending, &format!("{spin} Responding{since}")),
        }
    }

    /// A turn that is running and silent. The daemon sends prefill progress
    /// while it prefills and a delta per chunk while it generates, so a gap this
    /// long is a real gap and not a slow model — and the case that produced this
    /// line is one a head cannot otherwise show: when a turn *fails*, the engine
    /// publishes a `Warning` and nothing else, so `TurnState` stays `Running`
    /// and the old head span its spinner at a dead session indefinitely. See the
    /// report: `TurnFinished`/`TurnInterrupted` on failure is the daemon's to
    /// fix, and a head saying "nothing for 40s" is not a substitute for it.
    ///
    /// A row of its own, above the border, and not inlaid: it is a disclosure
    /// with a sentence in it, and a sentence truncated to fit a border is a
    /// disclosure that lost the words that mattered.
    fn stuck_line(&self, w: usize) -> Option<String> {
        let t = self.turn.as_ref()?;
        if !matches!(t.state, Some(TurnState::Running)) {
            return None;
        }
        let quiet = if self.last_event_at == 0 {
            0
        } else {
            self.now_ms.saturating_sub(self.last_event_at)
        };
        if quiet > 15_000 {
            return Some(colour(
                &self.cfg,
                sgr::YELLOW,
                &trim_to(
                    &format!(
                        "{} — nothing received for {}. The turn is still marked running; \
                         esc esc interrupts it.",
                        t.model,
                        dur_human(quiet)
                    ),
                    w,
                ),
            ));
        }
        None
    }

    /// The disclosure line: the read mark, what this head suppressed, what the
    /// daemon will never send, and what it stripped on the way.
    ///
    /// Ordered by how likely it is to matter, and truncated from the right, because
    /// on an 80-column terminal the old line lost `dropped`, `scrubbed` and
    /// `resync` to the ellipsis — the three numbers whose whole purpose is to be
    /// impossible to miss. Anything nonzero is promoted to the front.
    /// True when a §13.2b disclosure counter is non-zero, i.e. when the bottom
    /// border has something to say at all.
    fn alarmed(&self) -> bool {
        self.dropped + self.scrubbed + self.resyncs > 0
    }

    /// The alarm line for the **unboxed** composer — the degenerate short-screen
    /// path, where there is no border to pin a triangle to. The boxed path says
    /// it with a `⚠` in the bottom edge's right corner and leaves the numbers to
    /// `/status`; this names them, because on a screen this small the triangle
    /// alone would be a fact with no way to read it.
    ///
    /// It used to be all of them, plus the sequence numbers, plus the verbosity,
    /// plus the twenty-one-character session id and the head id, on every frame:
    ///
    /// ```text
    /// ╰─ seq 907 · rendered 900 · filtered 1 (normal) · dropped 0 · scrubbed 0 ·
    ///    resync 0 · s-1789023464202470853 h3 ─╯
    /// ```
    ///
    /// Every one of those was added because something was measured going wrong,
    /// and none of that is an argument for keeping them resident. §13.2b's rule is
    /// that *an absent field and a zero field must not look the same when the
    /// field is the disclosure* — which is a rule about the moment the field is
    /// **read**, not about where it lives the rest of the time. `/status` is where
    /// it is read, it says `dropped 0` explicitly, and it says what the counter
    /// means, which the border never had room to.
    ///
    /// What stays here is the case a person must not have to go looking for: a
    /// counter that has moved. In [`Role::Attention`], not the border's grey,
    /// because a second colour inside a border reads as damage and this *is*
    /// damage — that was the argument for painting it grey and it was the wrong
    /// way round.
    fn status_line(&self, w: usize) -> String {
        if !self.alarmed() {
            return String::new();
        }
        let p = self.cfg.palette();
        trim_to(
            &p.paint(
                Role::Attention,
                &format!(
                    "⚠ dropped {} · scrubbed {} · resync {} · /status",
                    self.dropped, self.scrubbed, self.resyncs
                ),
            ),
            w,
        )
    }

    /// `/status`: this head's own instrumentation, with what each number means.
    ///
    /// The gloss is the part the border could never carry, and it is the reason
    /// the counters are worth keeping at all — `scrubbed 4` is not actionable
    /// unless you know that scrubbing is what a *late* head does to an
    /// interactive-only frame, at which point it is the answer to "why is this
    /// head quieter than the one next to it".
    fn status_lines(&self, w: usize) -> Vec<String> {
        let p = self.cfg.palette();
        let mut out = vec![p.paint(Role::Strong, "this head"), String::new()];
        let mut row = |k: &str, v: String, why: &str| {
            let head = format!("  {k:<12}");
            out.push(format!(
                "{}{}",
                p.paint(Role::Faint, &head),
                p.paint(Role::Plain, &v)
            ));
            for l in wrap(why, w.saturating_sub(16)) {
                out.push(format!("{:14}{}", "", p.paint(Role::Faint, &l)));
            }
            out.push(String::new());
        };

        if !self.session_id.is_empty() {
            row(
                "session",
                self.session_id.clone(),
                "In full, because this is the form a command takes. \
                 The header shows the last eight characters, which is the part \
                 two sessions differ in.",
            );
        }
        if !self.head_id.is_empty() {
            row(
                "head",
                format!("{} · {} attached", self.head_id, self.heads.max(1)),
                "Every head on this session sees the same stream from its own \
                 read mark. Closing one does not stop the turn.",
            );
        }
        row(
            "seq",
            format!("{} · {} rendered", self.seq, self.rendered),
            "The log's monotonic, gap-free position, and how many of those events \
             reached the screen. Both counted by this head, not by the daemon.",
        );
        row(
            "filtered",
            format!("{} ({})", self.filtered, self.verbosity.as_str()),
            "Events this head chose not to show at the current verbosity. \
             /verbosity walks terse → normal → loud.",
        );
        row(
            "dropped",
            self.dropped.to_string(),
            "Events the daemon's bounded scrollback threw away before this head \
             asked for them. Not a rendering choice: they are gone.",
        );
        row(
            "scrubbed",
            self.scrubbed.to_string(),
            "Interactive-only frames withheld from a head that attached late — \
             partial tool output and the like, which has no durable form.",
        );
        row(
            "resync",
            self.resyncs.to_string(),
            "Times this head threw its state away and took a fresh snapshot, \
             because the gap since its read mark was past the daemon's bound.",
        );
        row(
            "verbosity",
            self.verbosity.as_str().to_string(),
            "What reaches the transcript at the current filter. /verbosity walks \
             terse → normal → loud. It used to sit on the composer's border, \
             which was a row of attention paid for ever for a fact read once.",
        );
        if !self.wiring.workspace.is_empty() {
            row(
                "workspace",
                tilde(&self.wiring.workspace),
                "Where the daemon is standing. Tools resolve relative paths here.",
            );
        }
        out.push(p.paint(Role::Faint, "  /status or esc closes this"));
        out
    }

    /// Drop the transient notice, once the operator has had a frame to see it.
    pub fn clear_notice(&mut self) {
        self.notice = None;
        self.notice_ttl = 0;
    }
}

/// The option a typed line names, and the glob the operator put after it.
///
/// > *"please add globbing to my answers somehow too"*
///
/// `allow_always crates/**/tests/*.rs` answers the permission AND says what the
/// rule should cover, instead of accepting the pattern the gate derives from the
/// one call in front of you. The two halves split on the first space; everything
/// after it is the pattern, verbatim and un-lowercased — a glob is a path and
/// `Cargo.toml` is not `cargo.toml`.
///
/// A pattern is only meaningful with `allow_always`, which is the only option that
/// writes a rule. Typed after anything else it is **refused** rather than dropped:
/// somebody who wrote `allow_once src/**` meant the rule to cover `src/**`, and
/// silently granting one call instead is the answer they did not give. Returning
/// `None` leaves the line in the composer, where they can see it.
fn match_option(d: &OpenDecision, typed: &str) -> Option<(String, Option<String>)> {
    let line = typed.trim();
    let (word, rest) = match line.split_once(char::is_whitespace) {
        Some((w, r)) => (w, r.trim()),
        None => (line, ""),
    };
    let t = word.to_ascii_lowercase();
    let id = d
        .options
        .iter()
        .find(|o| o.option_id.eq_ignore_ascii_case(&t) || o.label.to_ascii_lowercase() == t)
        .or_else(|| {
            d.options
                .iter()
                .find(|o| o.option_id.to_ascii_lowercase().starts_with(&t) && !t.is_empty())
        })?;
    if rest.is_empty() {
        return Some((id.option_id.clone(), None));
    }
    if id.kind != letibot_sessionlog::event::OptionKind::AllowAlways {
        return None;
    }
    Some((id.option_id.clone(), Some(rest.to_string())))
}

/// The row a `ToolStarted` / `ToolProgress` / `ToolFinished` is about: the
/// **last** call with that id that has not finished yet.
///
/// Not the first, which is what this used to be. A call id is positional within a
/// round (`call_0`, `call_1`, …), so a turn that makes fourteen rounds of calls
/// has fourteen rows called `call_0` in one `TurnPane`, and `find` handed every
/// one of those events to the first of them. Measured on the operator's own
/// session, replayed: round one's card was re-finished eight times and wore the
/// last round's duration, while rounds two onward sat at `○ Reading README.md ·
/// proposed` for the rest of the turn — a call that had returned twenty seconds
/// earlier, drawn as one that had not started.
///
/// Searching from the back for a row that is still open is exact rather than
/// heuristic: within a turn the engine proposes and settles in order, so the only
/// row a start or a finish can be about is the newest unfinished one.
fn open_call<'a>(calls: &'a mut [CallRow], call_id: &str) -> Option<&'a mut CallRow> {
    calls
        .iter_mut()
        .rev()
        .find(|c| c.call_id == call_id && !matches!(c.state, CallState::Finished { .. }))
}

/// The ask with its target taken off the end: `` `bash` wants exec access `` from
/// `` `bash` wants exec access to `cargo test` ``.
///
/// The daemon sends both the sentence and the target, and the sentence is the one
/// every other reader of the log already has — the audit row, the denial notice, a
/// second head. Rather than change what that sentence is, the head that wants to
/// lay the two out separately takes the target back off. `None` when the sentence
/// does not end in the target, which is the honest answer for a summary some other
/// builder wrote: then the whole sentence is shown and nothing is lost.
fn ask_without_target(summary: &str, target: &str) -> Option<String> {
    if target.is_empty() {
        return None;
    }
    let head = summary.strip_suffix(&format!("`{target}`"))?;
    // " to " is the joint in every sentence this builder writes; trimming it is what
    // makes the remainder read as a heading rather than as a clipped sentence.
    let head = head.trim_end();
    Some(head.strip_suffix(" to").unwrap_or(head).to_string())
}

/// The pane's word for how a job came to be in the background — the three causes
/// `Backgrounding` names, as a person reads them. The distinction is the one the
/// outcome already draws: who wanted it there.
fn how_word(how: &letibot_transcript::Backgrounding) -> String {
    match how {
        letibot_transcript::Backgrounding::Asked => "asked".into(),
        letibot_transcript::Backgrounding::Promoted => "promoted".into(),
        letibot_transcript::Backgrounding::Operator { identity } => {
            format!("promoted by {identity}")
        }
    }
}

fn colour(cfg: &RenderConfig, code: &str, s: &str) -> String {
    if cfg.color {
        format!("{code}{s}{}", sgr::RESET)
    } else {
        s.to_string()
    }
}

/// The first sentence of a refusal's reasoning, capped.
///
/// Layer A's `basis` is written for the model: it names every construct it could
/// not resolve, one indented paragraph each, and ends with the instruction to
/// re-issue. The operator needs the first clause of that — *what happened* — and
/// nothing else, because the rest is already in front of them as the tool result.
///
/// Cut at the first sentence end, then hard-capped: a "sentence" written without a
/// full stop is still not a paragraph a status line should carry.
fn first_sentence(basis: &str) -> String {
    let line = basis.lines().next().unwrap_or("").trim();
    let end = line.find(". ").map(|i| i + 1).unwrap_or(line.len());
    let s = &line[..end];
    const CAP: usize = 140;
    if s.chars().count() <= CAP {
        return s.to_string();
    }
    let cut: String = s.chars().take(CAP).collect();
    format!("{}…", cut.trim_end())
}

/// A result envelope's marker line: `<<<TOOL_ERROR 5ebfdef6>>>`, `<<<END_OK …>>>`.
///
/// Matched by SHAPE rather than against a list of kinds, so a kind added to
/// `letibot_tools::result::Envelope` does not start leaking here on the day it
/// lands. A body line that happens to look like one cannot exist: the envelope
/// rewrites every `<<<` in a payload to `< < <` precisely so its own markers are
/// unforgeable.
fn is_envelope(line: &str) -> bool {
    let l = line.trim();
    l.starts_with("<<<") && l.ends_with(">>>") && l.len() > 6
}

fn dim(cfg: &RenderConfig, s: &str) -> String {
    colour(cfg, sgr::DIM, s)
}

fn warn_line(cfg: &RenderConfig, s: &str) -> String {
    colour(cfg, sgr::RED, s)
}

fn fold_word(f: Fold) -> &'static str {
    match f {
        Fold::Folded => "folded",
        Fold::Open => "open",
    }
}

/// The output pane's lines from a peeked scrollback: one block per tool result,
/// in order, the payload verbatim — that payload is the stdout and stderr the
/// tool produced, as the model received it. A `ToolFinished`'s spill locator is
/// the full output on disk when the inline payload was bounded; those paths are
/// named at the end, because *"there is more"* without a *where* is a dead end.
fn subagent_out_lines(events: &[Envelope]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut spills: Vec<String> = Vec::new();
    for env in events {
        match &env.event {
            SessionEvent::TranscriptContent { item, .. } => {
                if let TranscriptItem::ToolResult {
                    name,
                    outcome,
                    payload,
                    ..
                } = &**item
                {
                    out.push(format!("· {name} — {}", outcome_word(outcome)));
                    for line in payload.lines() {
                        out.push(format!("  {line}"));
                    }
                    out.push(String::new());
                }
            }
            SessionEvent::ToolFinished {
                spill: Some(path), ..
            } => spills.push(path.clone()),
            _ => {}
        }
    }
    if !spills.is_empty() {
        out.push("full output on disk:".to_string());
        for s in spills {
            out.push(format!("  {s}"));
        }
        out.push(String::new());
    }
    out
}

/// The whole view, spilled: the pane caps like a terminal, the file does not
/// cap. One name per subagent, overwritten on each read, so the path is stable
/// enough to open twice.
/// Where a head keeps files of its own: `$XDG_RUNTIME_DIR/letibot`, where the
/// socket already lives — per-user, mode 0700, tmpfs. Not `/tmp`: a subagent's
/// tool output is whatever the model read, and a world-readable file at a name
/// anyone can predict is both a disclosure and the classic symlink target. The
/// fallback is the shape the sudo shims use when there is no runtime dir.
fn head_runtime_dir() -> std::path::PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(d) => std::path::PathBuf::from(d).join("letibot"),
        None => std::env::temp_dir().join(format!("letibot-{}", unsafe { libc::getuid() })),
    }
}

fn spill_sub_out(session_id: &str, lines: &[String]) -> Option<String> {
    spill_sub_out_under(&head_runtime_dir(), session_id, lines)
}

fn spill_sub_out_under(
    dir: &std::path::Path,
    session_id: &str,
    lines: &[String],
) -> Option<String> {
    if std::fs::create_dir_all(dir).is_err() {
        return None;
    }
    let path = dir.join(format!("subagent-{session_id}.log"));
    let mut body = String::new();
    for l in lines {
        body.push_str(l);
        body.push('\n');
    }
    std::fs::write(&path, body)
        .ok()
        .map(|_| path.display().to_string())
}

/// The rail every line of the model's reasoning carries.
///
/// Three independent signals, because each one is lost somewhere: the **word**
/// (`Thinking…` / `Thought for 4.2s`), the **rail** (`┃`), and the **dim-italic
/// attribute**. Colour is a no-op under a terminal-native palette; the rail is
/// what survives a copy-paste; the attribute is what survives a `--replay` diff.
/// Handing it to the `BlockCache` as a `Decor` rather than mapping over the lines
/// per frame is what keeps §13.3: the rail is applied once, when the line enters
/// the cache, not once per line per frame.
/// The width the reasoning body wraps to, and the style it wraps inside.
///
/// One place, because two call sites computing it and one of them forgetting the
/// step makes the block a row taller than the space reserved for it, which moves
/// everything below it every frame.
fn reasoning_cfg(cfg: &RenderConfig) -> RenderConfig {
    let mut r = cfg.inside(Role::Reasoning);
    r.width = cfg
        .width
        .saturating_sub(activity_indent(cfg.width) + card::REASONING_RAIL_WIDTH)
        .max(20);
    r
}

fn reasoning_decor(cfg: &RenderConfig) -> Decor {
    let p = cfg.palette();
    // The step the whole of the model's working is set in, carried on the same
    // prefix as the rail so it is applied once per line as the line enters the
    // cache — not once per line per frame, which is what §13.3 forbids.
    let step = " ".repeat(activity_indent(cfg.width));
    // The rail is painted **inside** the block too, so that the row obeys one
    // invariant end to end: every reset in a reasoning row either ends the row or
    // hands the reasoning style straight back. That is what the test asserts, and
    // an invariant with an exception at column 0 is an invariant nobody can check.
    // The cost is the block's opening sequence twice at the head of each row,
    // which a terminal collapses to nothing.
    let rail = Painter::inside(p, Role::Reasoning);
    Decor {
        prefix: format!("{step}{} ", rail.paint(Role::Faint, "┃")),
        open: p.open(Role::Reasoning).to_string(),
    }
}

/// The fold's own header, which is also where its key is advertised.
///
/// `card::reasoning` supplies the word and the tense; this adds the two things
/// only the head knows — how much of the terminal opening it would cost, and
/// which key opens it. There is no pointer here and no selection, so the fold's
/// own header naming its key is the whole discoverability mechanism, and it is on
/// the screen at the moment the operator wants it.
///
/// The count is **screen** lines, not source lines: the model writes its
/// working-out as a handful of very long paragraphs, so "3 lines" beside a fold
/// that opens to half a screen is a number that answers the wrong question. What
/// the reader wants to know is how much of the terminal this is about to cost.
fn thinking_header(
    cfg: &RenderConfig,
    raw: &str,
    open: bool,
    running: bool,
    elapsed_ms: Option<u64>,
) -> String {
    let w = cfg.width.max(20);
    let lines: usize = raw
        .lines()
        .map(|l| visible_width(l).div_ceil(w).max(1))
        .sum::<usize>()
        .max(1);
    let mark = if open { "▾" } else { "▸" };
    let word = card::reasoning(&[], running, elapsed_ms, &card_cfg(cfg, Fold::Folded))
        .into_iter()
        .next()
        .unwrap_or_default();
    trim_to(
        &format!(
            "{mark} {word}{}",
            dim(
                cfg,
                &format!(
                    " · {lines} line{} · ctrl-r",
                    if lines == 1 { "" } else { "s" }
                )
            )
        ),
        w,
    )
}

/// A `card::CardConfig` from this head's own config. One place, so the width, the
/// palette and the fold cannot drift between the live pane and the transcript.
fn card_cfg(cfg: &RenderConfig, fold: Fold) -> card::CardConfig {
    card::CardConfig {
        width: cfg.width,
        palette: cfg.palette(),
        mode: match fold {
            Fold::Open => card::DisplayMode::Expanded,
            Fold::Folded => card::DisplayMode::Truncated,
        },
        budget: card::Budget::GENERIC,
        show_id: false,
    }
}

/// A left half and a right half of one row, with the gap between them.
///
/// Falls back to the left half alone when both do not fit, because the left half
/// is the one that says what is happening.
fn split_row(left: &str, right: &str, w: usize) -> String {
    let (lw, rw) = (visible_width(left), visible_width(right));
    if lw + rw + 2 <= w {
        format!("{left}{}{right}", " ".repeat(w - lw - rw))
    } else {
        trim_to(left, w)
    }
}

/// The last non-empty line of a growing document, trimmed to fit.
fn last_line(raw: &str, cfg: &RenderConfig) -> String {
    let l = raw.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("");
    trim_to(l.trim(), cfg.width.saturating_sub(4))
}

/// How a turn ended, said in words rather than in the wire's vocabulary.
///
/// The footer carries **only the ending that is news**. Its stats line —
/// prompt, cache, rate, wall time, output — moved to the session header, where
/// the rest of the turn's numbers live; an ordinary ending (`eos`, `word`) now
/// leaves the body with no footer line at all, because `── 1.2k out` hovering
/// above the composer was a settled fact occupying the row a live fact used to
/// have to earn. What stays is the case §5.7's rule is about: `length` is not a
/// normal ending, it means the answer was cut off mid-sentence, and truncation
/// is never folded into success — a display that lets it read like `eos` folds
/// it at the last possible moment.
fn turn_footer(cfg: &RenderConfig, state: &TurnState) -> Vec<String> {
    match state {
        TurnState::Running => Vec::new(),
        TurnState::Finished { finish_reason, .. } => {
            match finish_reason {
                letibot_sessionlog::event::FinishReason::Length => vec![colour(
                    cfg,
                    sgr::YELLOW,
                    &format!(
                        "── CUT SHORT — it hit the output limit mid-answer; \
                         ask it to continue"
                    ),
                )],
                letibot_sessionlog::event::FinishReason::Aborted => vec![colour(
                    cfg,
                    sgr::YELLOW,
                    &"── stopped early (aborted)".to_string(),
                )],
                // `eos` and `word` are ordinary endings and read as ordinary:
                // no line at all.
                letibot_sessionlog::event::FinishReason::Eos
                | letibot_sessionlog::event::FinishReason::Word => Vec::new(),
                // A reason nobody recognises is shown, never normalised.
                letibot_sessionlog::event::FinishReason::Other(s) => vec![colour(
                    cfg,
                    sgr::YELLOW,
                    &format!("── ended for an unrecognised reason: {s}"),
                )],
            }
        }
        TurnState::Interrupted {
            reason,
            partial_kept,
        } => vec![colour(
            cfg,
            sgr::YELLOW,
            &format!(
                "── interrupted: {reason} ({})",
                if *partial_kept {
                    "what it had written is kept"
                } else {
                    "nothing kept"
                }
            ),
        )],
        // §4.5. A failure is not an ending a turn is allowed to have, so it does
        // not read like one: red, shouted, and wrapped rather than truncated,
        // because the reason is the whole content of the event.
        TurnState::Failed {
            error,
            partial_kept,
        } => {
            let kept = if *partial_kept {
                "what it had written is kept"
            } else {
                "nothing was recorded"
            };
            wrap(&format!("── FAILED — {error} ({kept})"), cfg.width)
                .into_iter()
                .map(|l| warn_line(cfg, &l))
                .collect()
        }
    }
}

/// The repo's `TODO.md` as a section map: one line per `##` section with its
/// open and done checkbox counts. Read fresh on every pane-open — the file is
/// the operator's to edit, and a cached map is a cache of somebody else's
/// intention. Errors name themselves; a missing file is a fact about the
/// workspace, not a panic in a pane.
fn repo_todos_map(workspace: &str) -> Vec<String> {
    let path = std::path::Path::new(workspace).join("TODO.md");
    let body = match std::fs::read_to_string(&path) {
        Ok(b) => b,
        Err(e) => {
            return vec![format!(
                "    (no TODO.md in {workspace}: {e})"
            )];
        }
    };
    let mut out: Vec<String> = Vec::new();
    let mut open = 0usize;
    let mut done = 0usize;
    let mut section: Option<String> = None;
    let flush = |out: &mut Vec<String>, section: &Option<String>, open: usize, done: usize| {
        if let Some(name) = section {
            out.push(format!(
                "    {name} — {open} open, {done} done"
            ));
        }
    };
    for line in body.lines() {
        if let Some(name) = line.strip_prefix("## ") {
            flush(&mut out, &section, open, done);
            section = Some(name.trim().to_string());
            open = 0;
            done = 0;
        } else {
            let t = line.trim_start();
            if t.starts_with("- [ ]") {
                open += 1;
            } else if t.starts_with("- [x]") || t.starts_with("- [X]") {
                done += 1;
            }
        }
    }
    flush(&mut out, &section, open, done);
    out
}

fn help_lines(cfg: &RenderConfig, w: usize) -> Vec<String> {
    let rows = [
        ("enter", "send what you typed; while a turn runs it is queued as a follow-up"),
        ("alt+enter", "a newline inside the prompt, without sending it"),
        ("esc esc", "interrupt the running turn — twice, within five seconds"),
        ("ctrl-c", "clear what you typed; twice on an empty prompt, within a second, quits"),
        ("↑ ↓", "move inside the prompt, then walk the prompts you have sent"),
        ("pgup pgdn", "scroll the transcript; esc returns to following the stream"),
        ("wheel", "scroll the transcript; shift+drag selects text"),
        ("ctrl-a ctrl-e", "start and end of the line; ctrl-w and ctrl-u kill, ctrl-y yanks"),
        ("ctrl-z", "undo — a word at a time, and a kill is always its own step"),
        ("paste", "five lines or more collapses to a marker and is sent in full"),
        ("ctrl-s", "the session list: type a number or part of a name to switch"),
        ("tab", "complete the /command being typed; more tabs walk the matches"),
        ("click", "in the session list, picks the row under the pointer; enter still switches"),
        ("ctrl-p", "the todos pane: the model's plan, and the repo's TODO.md read-only"),
        ("/new [title]", "start a session in this daemon and go there"),
        ("/switch WHAT", "go to a session by number, id or part of its name"),
        ("ctrl-r", "fold or unfold the model's thinking"),
        ("ctrl-t", "fold or unfold tool output"),
        ("ctrl-x", "show the raw <function=…> text of tool calls, as the model wrote it"),
        ("ctrl-l", "repaint the screen"),
        ("/status", "this head's counters — dropped, scrubbed, resync — and what each means"),
        ("/verbosity", "terse → normal → loud; /status counts what has been filtered"),
        ("/interrupt", "interrupt, when a key is awkward"),
        ("/compact", "summarize this session down to one record; the old transcript is forked, not lost"),
        ("/mode", "move this project to a point: read-only, always-ask, writes-allowed, automode, allow-all (next session)"),
        ("/supervise", "the guard model answers every gated call before you do, from the next call — on, off, status"),
        ("/gate", "what the gate decided, and rule on it afterwards: recent, todo, corpus, ok|grant|revoke ID"),
        ("/flowy", "the seat on the fabric: /flowy status · /flowy login [SEAT] [--token T] · /flowy logout"),
        ("/models", "which model answers: /models lists them with their auth; /models deepseek/deepseek-chat switches and sticks; /models local"),
        ("/resync", "throw this head's state away and take a fresh snapshot"),
        ("/quit", "detach. The turn keeps running: idle means quiet, not unwatched"),
    ];
    let mut out = vec![
        colour(cfg, sgr::BOLD, "keys and commands"),
        String::new(),
    ];
    for (k, v) in rows {
        let head = format!("  {k:<16}");
        for (i, l) in wrap(v, w.saturating_sub(19)).into_iter().enumerate() {
            out.push(if i == 0 {
                format!("{}{}", colour(cfg, sgr::CYAN, &head), l)
            } else {
                format!("{:19}{l}", "")
            });
        }
    }
    out.push(String::new());
    out.push(dim(cfg, "  /help or esc closes this"));
    out
}

/// The one mapping from engine types to display types.
///
/// `crates/ui/DESIGN.md` §1: the display crate deliberately does not depend on
/// `letibot-transcript`, so a rendering primitive can be tested without the
/// transcript and a head that wants to draw something the transcript does not
/// model does not have to fork it. The cost is this function, and it is the right
/// place for the cost to land — it is four lines and it is where the two
/// vocabularies are reconciled *once*.
fn display_outcome(o: &letibot_transcript::ToolOutcome) -> card::Outcome {
    use letibot_transcript::ToolOutcome as O;
    match o {
        O::Ok => card::Outcome::Ok,
        // §8.2: abstention is not a flavour of success, and `card::Outcome`
        // keeps the distinction for exactly that reason.
        O::Abstained { reason } => card::Outcome::Abstained(reason.clone()),
        O::Failed { reason } => card::Outcome::Failed(reason.clone()),
        O::Denied { req_id } => card::Outcome::Denied(format!("denied, {req_id}")),
        O::Timeout => card::Outcome::Failed("timed out".into()),
        // Not a failure of the tool and not a success either; it never ran. The
        // word is kept in the reason rather than mapped onto one that would read
        // as something else.
        O::NotRun { why } => card::Outcome::Failed(format!("not run — {why}")),
        // Not `Failed`, which would put a retry in front of the operator for a
        // command that is still working, and not `Ok`, which would read as a
        // finish. The handle is in the reason because the handle is what makes
        // it reachable.
        O::Backgrounded {
            handle, ran_for_ms, ..
        } => card::Outcome::Backgrounded(format!(
            "in the background as `{handle}` after {:.1}s",
            *ran_for_ms as f64 / 1000.0
        )),
    }
}

/// One live tool call, as a card.
///
/// This was `call_line`, which produced `● edit(call_7) — ok · 214 B` and could
/// produce nothing else. A card keeps the disclosure and adds the three things a
/// person watching a call is actually looking for: how long it has been running,
/// what it last said, and — for a spill — what happened to the rest of the
/// output.
///
/// **The body is empty while a call is in flight, and that is deliberate.**
/// `ToolFinished` carries digests and byte counts, never a payload; the payload
/// reaches a head only through `TranscriptItem::ToolResult` in a snapshot or a
/// reconciliation. See `crates/ui/DESIGN.md` §4.2 — it is a defensible design (an
/// event fans out to every head; a 480 KB payload should not) and it is why
/// `Phase` distinguishes running from settled at all.
/// The affordance that stands in for a tool call while the model is writing it.
///
/// The defect this replaces: *"tool calls — i see `<function…` like strings first,
/// then closing tag arrives and it becomes a toolcall."* The markup was being
/// rendered as prose because it arrived as prose, which is fixed one layer down
/// (`DeltaTarget::ToolCall`). What is left is the question that markup was
/// accidentally answering — *is something happening?* — and this answers it
/// without showing anybody a half-written `<parameter=`.
///
/// The spinner is driven off the log's own clock, like every other moving thing
/// here, so a replayed session animates the same way the live one did.
fn writing_call_line(cfg: &RenderConfig, now_ms: u64) -> String {
    let p = cfg.palette();
    let spin = letibot_ui::progress::spinner(now_ms).to_string();
    trim_to(
        &format!(
            "{} {}{}",
            p.paint(Role::Pending, &spin),
            p.paint(Role::Pending, "writing a tool call"),
            p.paint(Role::Faint, " · ctrl-x for the raw form")
        ),
        cfg.width,
    )
}

/// The raw, unparsed text of a tool call, behind `ctrl-x`.
///
/// Rendered as a labelled block rather than inline, because the whole point is
/// that this is *not* the assistant speaking. Faint and fenced: it is evidence,
/// and evidence that looks like prose is how the defect started.
fn raw_call_lines(cfg: &RenderConfig, raw: &str) -> Vec<String> {
    let p = cfg.palette();
    let mut out = vec![p.paint(Role::Faint, "┌─ raw tool call · ctrl-x")];
    for l in raw.lines() {
        for w in wrap(l, cfg.width.saturating_sub(2)) {
            out.push(format!("{}{}", p.paint(Role::Faint, "│ "), p.paint(Role::Code, &w)));
        }
    }
    out.push(p.paint(Role::Faint, "└─"));
    out
}

fn call_card(c: &CallRow, cfg: &RenderConfig, now_ms: u64, fold: Fold, diff_split: bool) -> Vec<String> {
    let mut card = card::Card::new(&c.name, &c.call_id);
    // §4.1, fixed. `ToolCallProposed` now carries a bounded display target beside
    // the digest — the path, the pattern, the command line — so a call that is
    // still running says `Running "cargo test --workspace"` rather than `Running
    // bash`. Empty is still possible (a call first seen as `ToolStarted`, or a log
    // recorded before the field existed) and is still rendered as nothing: a digest
    // is not a display string and a guess is worse than a blank.
    card.target = c.target.clone();
    let mut body: Vec<String> = Vec::new();
    // Both sides of the file this call changed, when it changed one and the
    // event carried them. Bound in the arm, used after it: the phase match
    // decides what the header says, and the body decision needs both.
    let mut edit_excerpt: Option<letibot_sessionlog::event::ToolEdit> = None;
    card.phase = match &c.state {
        CallState::Proposed => card::Phase::Proposed { note: c.note.clone() },
        CallState::Running => card::Phase::Running {
            elapsed_ms: now_ms.saturating_sub(c.started_ms),
            note: c.note.clone(),
        },
        CallState::Finished {
            outcome,
            inline_bytes,
            full_bytes,
            spill,
            edit,
            ..
        } => {
            // §8.3's disclosure, as prose and in units a person reads. It goes in
            // the body rather than the header tail because the header tail is
            // dropped whole when it does not fit, and "there is more, and here is
            // how to get it" is not a line that may vanish on a narrow terminal.
            if let Some(hash) = spill {
                body.push(format!(
                    "{} of {} went to the model, the rest is kept — read_spill hash={hash}",
                    bytes_human(*inline_bytes),
                    bytes_human(*full_bytes),
                ));
            } else {
                card.bytes = Some((*inline_bytes, *inline_bytes));
                body.push(bytes_human(*inline_bytes));
            }
            edit_excerpt = edit.clone();
            let outcome = display_outcome(outcome);
            if c.started_ms == 0 || c.ended_ms == 0 {
                // A snapshot has no timestamps, and `0.0s` is a measurement that
                // was never taken rendered as one that was.
                card::Phase::Replayed { outcome }
            } else {
                card::Phase::Finished {
                    outcome,
                    elapsed_ms: Some(c.ended_ms.saturating_sub(c.started_ms)),
                }
            }
        }
    };
    // The two-panel before/after view. It replaces the byte-count body when
    // this call edited a file, the operator has it switched on, and the pane
    // is wide enough for both panels (opencode's gate, and for the same
    // reason); every other case keeps exactly what the card already said,
    // which is what makes the toggle safe to flip at any width.
    if matches!(card.verb, card::Verb::Edit | card::Verb::Write)
        && let Some(e) = edit_excerpt
    {
        let view = sidediff::edit_view(diff_split, cfg.width.saturating_sub(2));
        let dcfg = DiffConfig {
            // The card indents its body by two, so the panels are built for
            // the width the body actually has, or the card truncates the
            // right panel's tail to fit and the diff lies by omission.
            width: cfg.width.saturating_sub(2),
            palette: cfg.palette(),
            // The excerpt already carries ±3 lines of context around the
            // change; re-diffing with the same keeps it intact.
            context: 3,
            line_numbers: true,
            intra_line: false,
            max_rows: 60,
        };
        body = sidediff::render_edit_view(
            &e.path, &e.before, &e.after, e.before_start, e.after_start, &dcfg, view,
        );
        if e.truncated {
            body.push(cfg.palette().paint(
                Role::Faint,
                &format!(
                    "… the excerpt was capped; the file is {} lines now",
                    e.after_lines
                ),
            ));
        }
    }
    card.body = body;
    let verb = card.verb.clone();
    card.render(&card::CardConfig {
        width: cfg.width,
        palette: cfg.palette(),
        // Never `Collapsed`: the spill disclosure lives in the body and a fold is
        // not a licence to hide it.
        mode: match fold {
            Fold::Open => card::DisplayMode::Expanded,
            Fold::Folded => card::DisplayMode::Truncated,
        },
        budget: card::Budget::for_verb(&verb),
        show_id: false,
    })
}

/// How a call ended, in one word.
///
/// Split from its reason on purpose. The two used to be one string on the card's
/// header, and a header is trimmed from the right — so a `not run` whose reason
/// ran to a hundred and forty characters pushed **the word itself** off the end
/// of the line and the row read `▸ ask_code "Give an overview of the crate…`,
/// with no sign anywhere on it that the call had not run. A reason is prose and
/// belongs on a line that wraps; the word is the fact and must not be able to
/// vanish.
fn outcome_word(o: &letibot_transcript::ToolOutcome) -> &'static str {
    use letibot_transcript::ToolOutcome as O;
    match o {
        O::Ok => "ok",
        // §8.2: abstention is not a flavour of success and must not read like one.
        O::Abstained { .. } => "ABSTAINED",
        O::Failed { .. } => "failed",
        O::Denied { .. } => "REFUSED",
        O::Timeout => "timeout",
        O::NotRun { .. } => "not run",
        O::Backgrounded { .. } => "STILL RUNNING",
    }
}

/// Why it ended that way, when there is a why. Goes in the body, where it wraps.
fn outcome_why(o: &letibot_transcript::ToolOutcome) -> Option<String> {
    use letibot_transcript::ToolOutcome as O;
    match o {
        O::Ok | O::Timeout => None,
        O::Abstained { reason } | O::Failed { reason } => Some(reason.clone()),
        O::Denied { req_id } => Some(format!("the call was denied ({req_id})")),
        O::NotRun { why } => Some(why.clone()),
        O::Backgrounded { handle, next, .. } => Some(format!("as `{handle}` — {next}")),
    }
}

/// The user's own message: an accent bar, a raised block, and the time it was sent.
///
/// The three things opencode and grok-build both do and this head did not. It used
/// to be `› {line}` in bold, which is a *prefix* rather than a block: at a glance
/// down a long conversation the operator's own words had the same shape as
/// everything else, and finding "what did I actually ask" meant reading.
///
/// - **The bar** (`▌`) is the signal that survives with no colour at all and
///   survives a copy-paste, which is the same argument the reasoning rail makes.
/// - **The block** sets a background *and* a foreground. The head's own note on
///   the composer rejects a raised background because "a dark block is either
///   invisible or unreadable depending on which half of the pair lands" — which is
///   true of a background set alone, and is fixed by setting both.
/// - **The timestamp** is right-aligned, from the log's own clock, and is
///   **omitted entirely when the row carries no `ts`** — a snapshot from a log
///   recorded before the field existed. The same rule as a replayed tool call
///   showing no duration.
fn user_block(text: &str, ts: u64, cfg: &RenderConfig) -> Vec<String> {
    let folded = fold_cells(text);
    let text: &str = folded.as_deref().unwrap_or(text);
    let p = cfg.palette();
    let w = cfg.width.max(20);
    let bar = p.paint(Role::UserAccent, "▌");
    let stamp = clock_time(ts);
    // The first row shares its width with the timestamp; the rest have the row.
    let head_w = w.saturating_sub(2 + visible_width(&stamp) + usize::from(!stamp.is_empty()));
    let mut lines = wrap(text, head_w.max(8));
    if lines.is_empty() {
        lines.push(String::new());
    }
    let mut out = Vec::with_capacity(lines.len());
    for (i, l) in lines.iter().enumerate() {
        // Padded to the full width so the block is a block: `term::paint` erases
        // each row it rewrites with `\x1b[K`, and a background that stops early
        // leaves a ragged right edge that reads as damage.
        let tail = if i == 0 && !stamp.is_empty() {
            let pad = w
                .saturating_sub(2)
                .saturating_sub(visible_width(l))
                .saturating_sub(visible_width(&stamp));
            format!("{}{stamp}", " ".repeat(pad))
        } else {
            " ".repeat(w.saturating_sub(2).saturating_sub(visible_width(l)))
        };
        out.push(format!("{bar} {}", p.paint(Role::UserBlock, &format!("{l}{tail}"))));
    }
    out
}

/// **A `/cells` message, with the screen taken back out of it.**
///
/// The rows are sent for the model and they are a photograph of this head, so
/// rendering them inside this head is a picture of the terminal inside the
/// terminal — re-wrapped to a narrower body, which breaks every box it drew. Worse,
/// it is permanent: the transcript is scrolled back through for the rest of the
/// session.
///
/// So the transcript keeps the operator's words and one line saying what went with
/// them. Nothing is hidden that the line does not name, and the model still has
/// every row. `None` when there is no screen in the text, which is every other
/// message.
fn fold_cells(text: &str) -> Option<String> {
    let at = text.find(CELLS_OPEN)?;
    let rest = &text[at..];
    let size = rest
        .strip_prefix(CELLS_OPEN)
        .and_then(|r| r.split_once(' '))
        .map(|(size, _)| size)
        .unwrap_or("");
    // The rows between the two markers; the delimiter lines are not screen.
    let rows = rest
        .lines()
        .skip(1)
        .take_while(|l| !l.starts_with(CELLS_CLOSE))
        .count();
    let words = text[..at].trim_end();
    let note = format!("· {rows} rows of this screen ({size}) went with this message");
    Some(if words.is_empty() {
        note
    } else {
        format!("{words}\n{note}")
    })
}

/// A prompt this head has sent that the transcript does not hold yet: the shape a
/// settled user row gets, dimmed, with `queued` where the timestamp goes.
///
/// ```text
///   ▌ queued · also bump the retry budget
/// ```
///
/// The block sits at the tail of the body — the place its row will occupy the
/// moment the step boundary appends it — so a message typed mid-turn never leaves
/// the screen: it changes from `queued` to a timestamped row in place. Dim text
/// rather than the raised block, because the raised block says "this is in the
/// conversation" and until the boundary it is not; the tag is what says what is
/// true instead, in [`Role::Pending`], the colour the spinner already uses for
/// something in flight.
fn queued_lines(text: &str, cfg: &RenderConfig) -> Vec<String> {
    // Folded here as well as in `user_block`, and it has to be the same text going
    // in: the pending row is removed when the transcript's user item MATCHES it, so
    // a head that queued an abbreviation and received the real thing would leave the
    // `queued` line on the screen for the rest of the session. Measured — the fold
    // belongs to the rendering, not to what was sent.
    let folded = fold_cells(text);
    let text: &str = folded.as_deref().unwrap_or(text);
    let p = cfg.palette();
    let w = cfg.width.max(20);
    let bar = p.paint(Role::UserAccent, "▌");
    let tag = "queued";
    // The first row shares its width with the tag; the rest hang under the text.
    let head_w = w.saturating_sub(2 + visible_width(tag) + 3);
    let mut lines = wrap(text, head_w.max(8));
    if lines.is_empty() {
        lines.push(String::new());
    }
    let indent = " ".repeat(visible_width(tag) + 3);
    let mut out = Vec::with_capacity(lines.len());
    for (i, l) in lines.iter().enumerate() {
        let label = if i == 0 {
            p.paint(Role::Pending, &format!("{tag} · "))
        } else {
            indent.clone()
        };
        out.push(format!("{bar} {}{}", label, p.paint(Role::Faint, l)));
    }
    out
}

/// `14:32:07` in the local zone, or empty when the row carries no timestamp.
///
/// Zero is *unknown*, not the epoch: a log recorded before `SnapshotItem::ts`
/// existed replays with zeros, and rendering those as `01:00:00` would be a
/// measurement that was never taken rendered as one that was.
fn clock_time(ms: u64) -> String {
    if ms == 0 {
        return String::new();
    }
    let secs = (ms / 1000) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `localtime_r` writes into `tm` and reads `secs`; both are owned here.
    // The `_r` form is the one that does not hand back a shared static, which
    // matters because the driver is not the only thread in this process.
    let ok = unsafe { !libc::localtime_r(&secs, &mut tm).is_null() };
    if !ok {
        return String::new();
    }
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}

/// Shorten a path to `max` columns by eating its **left**.
///
/// `…/worktrees/agent-a19da2/crates/tui`, not `~/Projects/letibot/.claud…`. A path
/// is recognised by where it ends; truncating from the right of a deep tree leaves
/// every session on this box looking identical.
fn ellipsise_left(s: &str, max: usize) -> String {
    if visible_width(s) <= max || max < 2 {
        return s.to_string();
    }
    // **At a separator, not at a character.** `…/1f0655c6-…/scratchpad` was the
    // operator's example and it is two lies in twenty-two columns: the first
    // ellipsis says a prefix was dropped, which is true, and the second says a
    // directory has a shorter name than it does, which is not — and neither
    // segment can be pasted back into a shell. Dropping *whole* segments leaves a
    // suffix that is a real path, which is what a person compares against.
    //
    // `match_indices` runs left to right, so the first candidate that fits is the
    // longest suffix that fits.
    if s.contains('/') {
        for (i, _) in s.match_indices('/') {
            let cand = format!("…{}", &s[i..]);
            if visible_width(&cand) <= max {
                return cand;
            }
        }
    }
    // A single segment longer than the whole allowance, or no separator at all.
    // Then there is nothing to cut on and the characters are all there is.
    let keep = max - 1;
    let mut out = String::new();
    let mut cols = 0usize;
    for c in s.chars().rev() {
        let cw = visible_width(&c.to_string());
        if cols + cw > keep {
            break;
        }
        out.push(c);
        cols += cw;
    }
    format!("…{}", out.chars().rev().collect::<String>())
}

/// Shorten a tool call's subject to `max` columns, cutting at the end a reader
/// does not need.
///
/// A **path** loses its left, at a separator: `…/crates/tui/src/app.rs` is still
/// a file you can recognise and `crates/tui/src/ap…` is not. Anything else — a
/// regex, a command line, a glob — loses its **right**, because those are read
/// from the start and the first token is the one that says what it is.
///
/// The test for "path" is a separator **and no glob metacharacter**. Measured at
/// 60 columns: `**/*.{md,json,toml,yaml,yml} 40` has a slash in it and cutting
/// its left gave `…json,toml,yaml,yml} 40`, which has lost the fact that it is a
/// glob at all. Cutting its right gives `**/*.{md,json,tom…`, which has not.
fn shorten_subject(s: &str, max: usize) -> String {
    if visible_width(s) <= max {
        return s.to_string();
    }
    // A glob metacharacter, or a quote — `display_target` quotes any argument
    // containing whitespace, so a leading `"` is how prose announces itself.
    // Measured: an `ask_code` call whose subject was a sentence with `src/` in the
    // middle of it left-cut to `…/ is responsible for, how main.rs, editor.rs,
    // and…`, which has thrown away the question and kept its tail.
    let not_a_path = s.contains(['*', '?', '{', '[', '"']);
    if s.contains('/') && !not_a_path {
        ellipsise_left(s, max)
    } else {
        trim_to(s, max)
    }
}

/// A path with `$HOME` written as `~`. Twelve columns of an eighty-column header
/// spent on `/home/dead` is twelve columns not spent on the session's name.
fn tilde(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() && path.starts_with(&h) => format!("~{}", &path[h.len()..]),
        _ => path.to_string(),
    }
}

/// The call ids answered by a result row **in this round**: the rows between the
/// assistant row at `at` and the next assistant or user row.
///
/// Bounded by the round for the same reason everything else here is: `call_0` is
/// reused every round, so "does a result for `call_0` exist anywhere in this
/// transcript" is a question with the wrong answer in it.
///
/// A row whose body has not arrived yet counts as unanswered — the head cannot
/// read a call id out of an announcement. The proposal line stays until the body
/// lands, and `record_item` rebuilds the history when it does.
fn round_results(items: &[SnapshotItem], at: usize) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for it in items.iter().skip(at + 1) {
        match it.item.as_ref() {
            Some(TranscriptItem::ToolResult { call_id, .. }) => {
                out.insert(call_id.clone());
            }
            Some(TranscriptItem::Assistant { .. }) | Some(TranscriptItem::User { .. }) => break,
            // An announcement with no body yet, or a reasoning row between the
            // calls and their results. Neither ends the round.
            _ if it.kind == "assistant" || it.kind == "user" => break,
            _ => {}
        }
    }
    out
}

/// The columns everything the model *does* is set in, under everything anybody
/// *says*.
///
/// # A turn had no shape
///
/// The operator's words: *"user message, then a flat wall of cards. Nothing says
/// this is one assistant turn, nothing separates thinking from acting from
/// answering, and assistant prose has no home of its own."* Every row started in
/// the same column, so a question, a file listing and the answer were three
/// things of equal weight in a stack.
///
/// What separates them here is a **step**, not a new glyph. The operator's
/// question and the model's answer sit at the body's own column — they are the
/// conversation. Thinking and acting are indented one step under them: they are
/// how the answer was arrived at, and they are subordinate to it. The turn's
/// footer rule closes the block at the outer column again.
///
/// That gives a turn four readable levels out of the vocabulary already on the
/// screen — `▌` for the question, a step in for the working, the answer flush
/// left, `──` to close — and costs no colour, so it survives [`Palette::None`]
/// and a copy-paste, which is the same argument the reasoning rail makes.
///
/// **Two columns, matching the reasoning rail's width** (`card::REASONING_RAIL_WIDTH`)
/// and the frame's own gutter, so the page reads as one repeated step rather than
/// as three unrelated indents. Given up below sixty columns, where two columns
/// out of every line is a bigger fraction than the hierarchy is worth — the same
/// trade `App::gutter` makes at forty.
fn activity_indent(w: usize) -> usize {
    if w >= 60 { card::REASONING_RAIL_WIDTH } else { 0 }
}

/// Drop a leading line-number gutter — `     1| ` — from one line of tool output.
///
/// Only ever applied to a **one-line preview inlaid on a header**, never to a
/// body: a body's gutter is how a reader refers to a line, and taking it away
/// there would lose a fact. On a header it is `1|` before the only line there is,
/// which is three columns saying "this is line one of one".
///
/// A prefix match rather than a parse of any tool's format. It matches what
/// `read` emits and nothing that is not shaped exactly like it; a tool whose
/// output happens to begin `12| ` gets three columns back and loses nothing.
fn strip_gutter(l: &str) -> String {
    let t = l.trim_start();
    let digits = t.len() - t.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    match t[digits..].strip_prefix("| ") {
        Some(rest) if digits > 0 => rest.trim_end().to_string(),
        _ => l.trim().to_string(),
    }
}

/// Set `lines` one step in. Empty rows stay empty: trailing spaces on a blank
/// line are invisible until something copies them.
fn step_in(lines: Vec<String>, n: usize) -> Vec<String> {
    if n == 0 {
        return lines;
    }
    let pad = " ".repeat(n);
    lines
        .into_iter()
        .map(|l| if l.is_empty() { l } else { format!("{pad}{l}") })
        .collect()
}

/// What kind of row this is, for the one question the layout asks about its
/// neighbours: does a blank line belong between them.
///
/// Activity rows **pack**. A run of tool cards is one block and reads as one; a
/// blank line between each of them was costing a third of the vertical budget to
/// separate things that are already separated by a glyph in the first column. Air
/// goes where the *kind* changes — around the question, around the answer, around
/// a warning — because that is where the reader's attention has to move.
/// Where the history walk stood before one row. See [`App::hist_marks`].
///
/// Three fields because the walk carries three cursors, and the fourth —
/// `call_targets` — is *derivable* from the rows above rather than stored:
/// it is the calls of the nearest assistant row with a body, which
/// [`App::retarget_before`] finds by scanning back. Storing a map per row would
/// be the cache growing with the session, which is the thing being fixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HistMark {
    /// `hist_lines.len()` before the row was drawn.
    lines: usize,
    /// `note_upto` before the row was drawn.
    note_upto: usize,
    /// `hist_class` before the row was drawn — the separator's whole input.
    class: Option<RowClass>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowClass {
    /// Somebody said something: the operator's question, the model's answer.
    Speech,
    /// The model working: reasoning, and tool calls.
    Activity,
    /// Anything else — a system row, a segment mark, an announcement with no
    /// body yet.
    Other,
}

/// Everything one transcript row needs to know about where it sits.
///
/// A struct rather than seven positional parameters because two of the seven are
/// round-scoped and one is row-scoped, and a caller passing them in the wrong
/// order is exactly the defect this file has just finished fixing.
struct ItemCtx<'a> {
    cfg: &'a RenderConfig,
    think: Fold,
    tools: Fold,
    raw: bool,
    /// Display targets for **this row's round**, keyed by call id.
    targets: &'a std::collections::HashMap<String, String>,
    /// Call ids in this round that already have a settled result row below.
    /// Their card is that row; the assistant row does not draw them again.
    answered: &'a std::collections::HashSet<String>,
    /// This row belongs to the turn the live pane is still drawing, so the pane
    /// below owns whatever has not settled and this row draws none of it.
    drawn_live: bool,
    /// How long this row's call took, when this head watched it run.
    elapsed_ms: Option<u64>,
    /// Both sides of the file this row's call changed, when this head watched
    /// it run. See `App::call_edits`.
    edit: Option<&'a letibot_sessionlog::event::ToolEdit>,
    /// The operator's `/diff` choice; the width decides the rest.
    diff_split: bool,
}

fn item_lines(it: &SnapshotItem, ctx: &ItemCtx<'_>) -> (RowClass, Vec<String>) {
    let ItemCtx {
        cfg,
        think,
        tools,
        raw,
        targets,
        edit,
        diff_split,
        answered,
        drawn_live,
        elapsed_ms,
    } = *ctx;
    let ind = activity_indent(cfg.width);
    let Some(item) = &it.item else {
        // The event arrived and the body has not — which, since the body now
        // travels on the log too, is a real in-flight state and no longer a
        // permanent one. It says so.
        return (
            RowClass::Other,
            vec![dim(
                cfg,
                &format!("[{} — waiting for the body of {}]", it.kind, it.item_id),
            )],
        );
    };
    match item {
        TranscriptItem::System { text, origin } => {
            let mut out = vec![dim(cfg, &format!("system ({origin:?})"))];
            out.extend(wrap(text, cfg.width).into_iter().map(|l| dim(cfg, &l)));
            (RowClass::Other, out)
        }
        TranscriptItem::User { parts } => {
            let text = parts
                .iter()
                .map(|p| match p {
                    UserPart::Text { text } => text.clone(),
                    UserPart::Image { media_type, .. } => format!("[image {media_type}]"),
                    UserPart::FileRef { path, .. } => format!("[file {path}]"),
                })
                .collect::<Vec<_>>()
                .join(" ");
            (RowClass::Speech, user_block(&text, it.ts, cfg))
        }
        TranscriptItem::Reasoning { text, .. } => {
            // A settled row: `Thought`, with no duration. The head can compute one
            // for a *live* turn from the delta timestamps, and a transcript row
            // carries no timestamps at all — see `crates/ui/DESIGN.md` §4.4.
            let mut out = step_in(
                vec![thinking_header(cfg, text, think.is_open(), false, None)],
                ind,
            );
            if think.is_open() {
                let rcfg = reasoning_cfg(cfg);
                let mut md = IncrementalMarkdown::new();
                md.push(text);
                let mut cache = BlockCache::decorated(reasoning_decor(cfg));
                out.extend(cache.lines(&md, &rcfg, cfg.budget.reasoning_lines));
            }
            (RowClass::Activity, out)
        }
        TranscriptItem::Assistant { text, tool_calls, .. } => {
            let mut md = IncrementalMarkdown::new();
            md.push(text);
            let mut cache = BlockCache::new();
            // The answer sits at the body's own column, with the question. It is
            // the one thing on the screen that is not subordinate to something
            // else, and that is what says so.
            let prose = cache.lines(&md, cfg, cfg.budget.body_lines);
            let spoke = !prose.iter().all(|l| l.trim().is_empty());
            let mut out = prose;
            let mut acted = false;
            let p = cfg.palette();
            for c in tool_calls {
                // ONE ROW PER CALL. A call whose result is on the screen is drawn
                // by that result and not here.
                //
                // This row used to draw `→ Read foo.rs` for every call it made and
                // the result row then drew `▸ Read foo.rs · ok · 21 lines` for the
                // same call three lines below, which is two rows and one fact: the
                // proposal says a call is coming, and once the result has settled
                // nothing is coming. That doubling is most of why a turn read as a
                // wall — four calls cost eight rows of a thirty-four-row screen
                // before any output was shown.
                //
                // What survives is the case the proposal line is actually FOR: a
                // call with no result. The turn was interrupted, the round is still
                // running, or the body has not arrived. `→` now means exactly
                // "asked for, nothing came back", which is a fact worth a row.
                if answered.contains(&c.id) || drawn_live {
                    if raw && !c.arguments.is_empty() {
                        out.extend(raw_call_lines(cfg, &format!("{} {}", c.name, c.arguments)));
                    }
                    continue;
                }
                let verb = card::Verb::of(&c.name);
                let mut line = format!("→ {}", verb.label(false));
                // Derived from the arguments **on this row**, never looked up by
                // call id. The row is holding the very bytes the rule reads, and it
                // is the only copy of them that is guaranteed to belong to this
                // round — an id-keyed lookup was how `→ Read TODO.md` came to sit
                // above a card whose payload was `README.md`.
                //
                // It is the same function the engine puts on the wire,
                // `letibot_sessionlog::display_target`, so a call watched live and
                // one reconstructed from the transcript still render identically;
                // a second copy of the rule here is what would make a switched head
                // disagree with the head it switched away from.
                let target = letibot_sessionlog::display_target(&c.arguments);
                if target.is_empty() {
                    // The call id earns its columns only when there is nothing
                    // better: it is a correlation key, and it is the only thing
                    // that distinguishes two calls to the same tool.
                    line.push_str(&format!(" ({})", c.id));
                } else {
                    line.push(' ');
                    line.push_str(&target);
                }
                // Said out loud, because a row that looks like every other tool row
                // and quietly has no output is the shape a person reads straight
                // past. It is the only thing this row now means.
                line.push_str(" · no result");
                acted = true;
                out.push(trim_to(
                    &format!("{}{}", " ".repeat(ind), p.paint(Role::Attention, &line)),
                    cfg.width,
                ));
                // The settled row's half of `ctrl-x`. A live turn shows the raw
                // markup from the `ToolCall` deltas; once the row is committed the
                // markup is gone and the arguments the parser read out of it are
                // what remain, so that is what the chord shows here. Different
                // bytes, same question — and saying which one you are looking at is
                // the difference between evidence and a guess.
                if raw && !c.arguments.is_empty() {
                    out.extend(raw_call_lines(
                        cfg,
                        &format!("{} {}", c.name, c.arguments),
                    ));
                }
            }
            // A row that says something is speech; a row that only names calls is
            // working. A row that does both is speech, because the sentence is
            // what the reader's eye is going to land on.
            let class = if spoke {
                RowClass::Speech
            } else if acted {
                RowClass::Activity
            } else {
                RowClass::Other
            };
            (class, out)
        }
        TranscriptItem::ToolResult {
            name,
            outcome,
            payload,
            call_id,
        } => {
            // **The envelope is addressed to the model, not to the operator.**
            //
            // `<<<TOOL_ERROR 5ebfdef6>>>` and its `<<<END_…>>>` are how a result
            // tells the model where the harness's text stops and the payload starts
            // — a marker, with a per-call nonce so a payload cannot forge one. On a
            // screen it is a line of noise in the middle of the two lines a folded
            // row has, and the operator reads a random hex string where the result
            // should be.
            let lines: Vec<&str> = payload.lines().filter(|l| !is_envelope(l)).collect();
            let bad = !matches!(outcome, letibot_transcript::ToolOutcome::Ok);
            let mark = if tools.is_open() { "▾" } else { "▸" };
            // `▾ Read crates/ui/src/style.rs · ok · 183 lines · ctrl-t`, not
            // `▾ read(call_0) …`. The verb and the target are the two words a
            // person scans a settled call for, and the id — a correlation key —
            // takes their place only when the target is not known.
            let verb = card::Verb::of(name).label(false).to_string();
            let subject = match targets.get(call_id) {
                Some(t) if !t.is_empty() => t.clone(),
                _ => format!("({call_id})"),
            };
            // How long it took, when this head watched it run. Carried from the
            // live card at the moment the transcript took the call over — a
            // `TranscriptItem::ToolResult` has no timestamps of its own — and
            // simply absent for a row read out of a snapshot, which is the same
            // rule `card::Phase::Replayed` follows and for the same reason.
            let took = match elapsed_ms {
                Some(ms) => format!(" · {}", letibot_ui::progress::duration(ms)),
                None => String::new(),
            };
            // # Everything used to be the same weight
            //
            // A one-line `ls` and a two-hundred-line search rendered identically:
            // one grey header, one dim body. The operator's words — *"size,
            // indentation and rule-weight should tell you what matters before you
            // read a word"*.
            //
            // The header is now built out of roles rather than painted one colour,
            // and the roles are chosen so the **scan** works with no reading at
            // all:
            //
            // - The subject — the path, the pattern — is [`Role::Plain`], i.e. no
            //   sequence at all, so it is the brightest thing on the row. It is
            //   what a person is looking for.
            // - Everything structural around it is [`Role::Faint`]: the glyph, the
            //   verb, the separators, the chord. Present, skippable.
            // - `ok` is faint too. It is the boring case and it is most of them;
            //   anything else keeps its own loud role, which is §8.2's rule
            //   (abstention must not read like success) and is now the *only*
            //   coloured thing on an ordinary row.
            // - The line count is [`Role::Strong`] once the output is big enough
            //   to be worth a fold — that is the size signal, and it is an
            //   attribute rather than a second colour, so it survives a
            //   terminal-native theme.
            //
            // Under [`Palette::None`] the words are unchanged and the count is
            // still a number, which is the whole reason the weighting is carried
            // by *which* field rather than by a decoration.
            const BIG: usize = 40;
            let p = cfg.palette();
            let w = cfg.width.saturating_sub(ind).max(20);
            let outcome_role = if bad { Role::Failure } else { Role::Faint };
            let size_role = if lines.len() >= BIG {
                Role::Strong
            } else {
                Role::Faint
            };
            // # It degrades by shortening the subject, never by losing the tail
            //
            // The same rule `header_line` had to learn, and for the same reason:
            // this row was built left to right and trimmed at the right, so a long
            // target ate the outcome. Measured on the operator's session — an
            // `ask_code` call that did not run rendered
            // `▸ ask_code "Give an overview of the crate architecture: what each…`
            // with the word `not run` cut off the end, which is a failed call
            // wearing the shape of a successful one.
            //
            // So the tail is measured first and the subject is given what is left.
            // A path is shortened from its LEFT at a separator — the end of a path
            // is what identifies it, and `crates/tui/src/…` names nothing.
            let word = outcome_word(outcome);
            let tail_cols = 3 + visible_width(word)
                + visible_width(&took)
                + 3 + 6 + lines.len().to_string().len();
            let lead = format!("{mark} {verb} ");
            let subject = shorten_subject(
                &subject,
                w.saturating_sub(visible_width(&lead) + tail_cols).max(8),
            );
            let mut head = p.paint(if bad { Role::Failure } else { Role::Faint }, mark);
            head.push_str(&p.paint(Role::Faint, &format!(" {verb} ")));
            head.push_str(&p.paint(Role::Plain, &subject));
            head.push_str(&p.paint(outcome_role, &format!(" · {word}")));
            head.push_str(&p.paint(Role::Faint, &took));

            // A result of one line goes ON the header. `▸ Read .gitignore · ok ·
            // 1.1s · /target` is one row where `▸ Read .gitignore · ok · 1 line ·
            // ctrl-t` over `  /target` was two, and the second of them carried the
            // count and the chord for a fold that has nothing to fold. At 34 rows
            // that halving is the difference between four calls fitting and eight.
            let inline = (!bad && lines.len() == 1)
                .then(|| strip_gutter(lines[0]))
                .filter(|l| !l.is_empty())
                .filter(|l| visible_width(&head) + 3 + visible_width(l) <= w);
            if let Some(l) = inline {
                head.push_str(&p.paint(Role::Faint, " · "));
                head.push_str(&p.paint(Role::Plain, &l));
                return (RowClass::Activity, step_in(vec![trim_to(&head, w)], ind));
            }

            head.push_str(&p.paint(
                size_role,
                &format!(
                    " · {} line{}",
                    lines.len(),
                    if lines.len() == 1 { "" } else { "s" }
                ),
            ));
            // No `· ctrl-t` here. The chord belongs on the elision row below, which
            // exists exactly when something is hidden — an affordance on a card
            // with nothing folded is eight columns of every row spent advertising
            // a key that would do nothing, and the hint bar already teaches it.
            let mut out = vec![trim_to(&head, w)];
            // The reason, on its own wrapping line rather than in the header's
            // tail. Never folded, never truncated, and in the outcome's own role:
            // a call that abstained or was refused said *why*, and that sentence
            // is the whole content of the row.
            let why = outcome_why(outcome);
            // **A reason that is a DOCUMENT is not a sentence.**
            //
            // This printed the reason in full, unfoldable, on the argument that a
            // refusal nobody can read is a refusal nobody acts on. That holds while
            // the reason is a sentence. Layer A's is not: it names every construct
            // it could not resolve, one indented paragraph each, and ends with the
            // instruction to re-issue — twenty lines of prose addressed to the
            // MODEL, which the head then painted into the operator's chat. Measured
            // on a six-line shell loop; the operator's answer was *"i get what it
            // tries to do, but it just throws up on my chat"*.
            //
            // So the first sentence stands unfolded — what happened, always visible,
            // which is what the original rule was protecting — and the rest arrives
            // with ctrl-t like every other long thing on this screen.
            let mut why_folded = false;
            if let Some(why) = &why {
                let shown = if tools.is_open() {
                    why.clone()
                } else {
                    let gist = first_sentence(why);
                    why_folded = gist.len() < why.len();
                    gist
                };
                out.extend(
                    wrap(&shown, w.saturating_sub(2))
                        .into_iter()
                        .map(|l| p.paint(outcome_role, &format!("  {l}"))),
                );
            }
            // Folded shows the first line, which is where a tool puts what it did.
            //
            // A failure used to be exempt — *an error nobody can read is an error
            // nobody acts on* — and that rule is satisfied by the line above,
            // which prints the reason in full, wrapped, unfoldable. What the
            // exemption was actually doing on the screen was printing a tool's
            // whole `<<<TOOL_ERROR>>>` envelope, in which the reason appears twice
            // more. So the exemption now applies only when there is **no** reason
            // to have printed: a timeout, where the payload is all there is.
            // **A file edit draws its diff, not the tool's prose.** The tool's
            // payload is addressed to the model — "path: 1 replacement(s)" and a
            // window of the new file — and a folded row showed two lines of it.
            // When this head watched the call run it holds both sides, and the
            // operator's question about an edit is "what changed", which is a
            // diff in whichever of the two shapes fits (`sidediff::edit_view`).
            // Folded keeps the first hunk's opening rows so the change is on the
            // screen without the fold; open shows it whole, up to the diff's own
            // cap. A row this head did not watch run has no pair and keeps the
            // prose, which is the `Replayed` rule.
            if let Some(e) = edit
                && matches!(card::Verb::of(name), card::Verb::Edit | card::Verb::Write)
                && !bad
            {
                let dcfg = DiffConfig {
                    width: w.saturating_sub(2),
                    palette: p,
                    context: 3,
                    line_numbers: true,
                    intra_line: false,
                    max_rows: 60,
                };
                let view = sidediff::edit_view(diff_split, w.saturating_sub(2));
                let mut rows = sidediff::render_edit_view(
                    &e.path, &e.before, &e.after, e.before_start, e.after_start, &dcfg, view,
                );
                if e.truncated {
                    rows.push(p.paint(
                        Role::Faint,
                        &format!("… the excerpt was capped; the file is {} lines now", e.after_lines),
                    ));
                }
                let keep = if tools.is_open() { rows.len() } else { 8.min(rows.len()) };
                let hidden = rows.len() - keep;
                out.extend(rows.into_iter().take(keep).map(|l| format!("  {l}")));
                if hidden > 0 {
                    out.push(p.paint(Role::Faint, &format!("  … +{hidden} diff rows · ctrl-t")));
                }
                return (RowClass::Activity, step_in(out, ind));
            }
            let limit = if tools.is_open() || (bad && why.is_none()) {
                cfg.budget.body_lines
            } else {
                2
            };
            if lines.len() > limit && limit >= 2 {
                out.extend(
                    lines[..limit - 1]
                        .iter()
                        .map(|l| dim(cfg, &format!("  {l}"))),
                );
                // grok-build's `execute.rs:549` form: `… +{n} lines`, and it is a
                // separator row rather than a sentence — it is not content, it is
                // the seam where content was taken out. The chord goes here, where
                // there is something for it to do.
                out.push(p.paint(
                    Role::Faint,
                    &format!("  … +{} lines · ctrl-t", lines.len() - (limit - 1)),
                ));
            } else {
                out.extend(lines.iter().map(|l| dim(cfg, &format!("  {l}"))));
                // The payload was short enough to show whole, but the REASON was
                // cut — so the affordance has to be here, or the rest of it would
                // be hidden behind a chord nothing on the row mentions.
                if why_folded {
                    out.push(p.paint(Role::Faint, "  … the rest of the reason · ctrl-t"));
                }
            }
            (
                RowClass::Activity,
                step_in(out.into_iter().map(|l| trim_to(&l, w)).collect(), ind),
            )
        }
        TranscriptItem::SegmentMark { label, .. } => (
            RowClass::Other,
            vec![dim(cfg, &format!("─── {label} ───"))],
        ),
    }
}

/// Visible width of a rendered screen line. Re-exported so a test can assert the
/// screen fits.
pub fn line_width(s: &str) -> usize {
    visible_width(s)
}

/// An open password request, as the head shows it.
#[derive(Debug, Clone)]
struct SecretAsk {
    req_id: String,
    prompt: String,
    command: String,
    deadline: u64,
}

/// Something that happened between two transcript rows.
#[derive(Debug, Clone)]
enum Note {
    /// §18's post-flight assertions and §8.5's guards land here, and a guard
    /// nobody notices is a guard nobody wrote.
    Warned(Warned),
    /// **A refusal nobody made.** The harness itself could not read the call — the
    /// normaliser could not resolve the command, the host boundary refused it — so
    /// there is nothing for the operator to answer, grant or lift, and the thing
    /// that has to change is the model's next attempt.
    ///
    /// Its own variant rather than a `Warned` because the register is the message:
    /// red with a `!` says *look at this now*, and an operator who is shown that
    /// for something they cannot act on learns to stop reading the red lines. The
    /// operator's report was exactly that — *"i get what it tries to do, but it
    /// just throws up on my chat"* — about a 20-line refusal that was also already
    /// on the screen as the tool's own result, one row up.
    NotRun(Warned),
    /// §13.2b: a settled decision *"renders as its outcome, not as an open
    /// prompt"* — and not as nothing either, which is what it rendered as before.
    /// A tool that was refused has to look refused.
    Decided(SettledDecision),
}

fn note_lines(cfg: &RenderConfig, n: &Note) -> Vec<String> {
    match n {
        Note::Warned(w) => wrap(&format!("! {} — {}", w.code, w.detail), cfg.width)
            .into_iter()
            .map(|l| warn_line(cfg, &l))
            .collect(),
        // No `!`, no red, no request id: nothing here is answerable, and the id is
        // only useful to somebody typing a grant. The detail that was cut is not
        // lost — the model's own tool result carries it, folded, one row above.
        Note::NotRun(w) => wrap(&format!("· {}", w.detail), cfg.width)
            .into_iter()
            .map(|l| dim(cfg, &l))
            .collect(),
        Note::Decided(d) => {
            use letibot_sessionlog::event::DecisionOutcome as O;
            let (word, code) = match &d.outcome {
                O::Selected { option_id } if option_id.starts_with("allow") => {
                    (format!("allowed ({option_id})"), sgr::GREEN)
                }
                O::Selected { option_id } => (format!("REFUSED ({option_id})"), sgr::RED),
                O::Cancelled => ("cancelled".to_string(), sgr::YELLOW),
                // A deadline is not an answer, and must not read like one.
                O::TimedOut => ("NOT ANSWERED — the deadline decided it".to_string(), sgr::RED),
            };
            let who = if d.by.identity.is_empty() {
                d.by.kind.clone()
            } else {
                format!("{} {}", d.by.kind, d.by.identity)
            };
            let late = if d.late { " · an answer arrived after it had settled" } else { "" };
            wrap(
                &format!("? {} — {word}, by {who}{}{late}", d.summary,
                    if d.basis.is_empty() { String::new() } else { format!(" ({})", d.basis) }),
                cfg.width,
            )
            .into_iter()
            .map(|l| colour(cfg, code, &l))
            .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_sessionlog::hub::Hub;
    use letibot_sessionlog::protocol::Caps;
    use letibot_sessionlog::testing;

    fn app() -> App {
        App::new(RenderConfig {
            width: 80,
            color: false,
            ..RenderConfig::default()
        })
    }

    fn decision_with(kinds: &[letibot_sessionlog::event::OptionKind]) -> OpenDecision {
        use letibot_sessionlog::event::DecisionOption;
        OpenDecision {
            req_id: "d1".into(),
            kind: "permission".into(),
            call_id: None,
            summary: "edit a file".into(),
            target: String::new(),
            detail: String::new(),
            options: kinds
                .iter()
                .map(|k| DecisionOption {
                    option_id: match k {
                        letibot_sessionlog::event::OptionKind::AllowOnce => "allow_once",
                        letibot_sessionlog::event::OptionKind::AllowSession => "allow_session",
                        letibot_sessionlog::event::OptionKind::AllowAlways => "allow_always",
                        letibot_sessionlog::event::OptionKind::RejectOnce => "deny",
                        letibot_sessionlog::event::OptionKind::RejectAlways => "deny_always",
                    }
                    .to_string(),
                    label: "x".into(),
                    kind: *k,
                })
                .collect(),
            choices: vec![],
            because: String::new(),
            advice: None,
            deadline: None,
            on_timeout: letibot_sessionlog::event::OnTimeout::Deny,
            asked_ts: 0,
        }
    }

    /// **A glob typed after the option id is the rule's coverage.**
    ///
    /// > *"please add globbing to my answers somehow too"*
    #[test]
    fn an_answer_can_carry_the_operators_own_glob() {
        use letibot_sessionlog::event::OptionKind;
        let d = decision_with(&[OptionKind::AllowOnce, OptionKind::AllowAlways, OptionKind::RejectOnce]);

        // The bare id still answers, and asks for no pattern.
        assert_eq!(
            match_option(&d, "allow_once"),
            Some(("allow_once".into(), None))
        );
        // A prefix still answers, which is how people actually type.
        assert_eq!(match_option(&d, "d"), Some(("deny".into(), None)));

        // And the glob rides after it, verbatim: a pattern is a path, so it is not
        // lowercased the way the option id is.
        assert_eq!(
            match_option(&d, "allow_always crates/**/Cargo.toml"),
            Some(("allow_always".into(), Some("crates/**/Cargo.toml".into())))
        );

        // **A glob on anything but `allow_always` is refused, not dropped.**
        // Somebody who typed `allow_once src/**` meant the rule to cover `src/**`;
        // granting one call instead is an answer they did not give. `None` leaves
        // the line in the composer where they can see it.
        assert_eq!(match_option(&d, "allow_once src/**"), None);
        assert_eq!(match_option(&d, "deny src/**"), None);
    }

    /// The hint only appears when the option it describes is on offer.
    #[test]
    fn the_glob_hint_is_absent_when_no_rule_can_be_written() {
        use letibot_sessionlog::event::OptionKind;
        let a = app();
        let with = decision_with(&[OptionKind::AllowOnce, OptionKind::AllowAlways]);
        let without = decision_with(&[OptionKind::AllowOnce, OptionKind::RejectOnce]);
        assert!(
            a.decision_lines(&with, 100).iter().any(|l| l.contains("allow_always <glob>")),
            "an always-allow on offer says how to scope it"
        );
        assert!(
            !a.decision_lines(&without, 100).iter().any(|l| l.contains("<glob>")),
            "a request with no rule to write must not advertise one"
        );
    }

    /// **The command is on a line of its own, and the taxonomy is not in front of
    /// it.** The operator's report: *"no possible to see wtf was the command i
    /// supposed to approve"* — because the summary carried the target inside a
    /// sentence, layer A's reading was appended to that sentence, and a long
    /// command was then wrapped into the middle of the wall.
    #[test]
    fn the_command_being_approved_gets_its_own_line() {
        use letibot_sessionlog::event::OptionKind;
        let a = app();
        let mut d = decision_with(&[OptionKind::AllowOnce, OptionKind::RejectOnce]);
        let cmd = "cargo test -p letibot-tools --test clauses -- --nocapture";
        d.summary = format!("`bash` wants exec access to `{cmd}`");
        d.target = cmd.to_string();
        d.detail = "ask — intents [execute_code] over [host_other]".into();
        let lines = a.decision_lines(&d, 100);

        // The command, alone on its line, indented — not embedded in the question.
        let own = lines
            .iter()
            .find(|l| l.contains(cmd))
            .unwrap_or_else(|| panic!("the command is not shown at all: {lines:#?}"));
        assert_eq!(own.trim(), cmd, "the command shares its line with prose");

        // The question above it names the tool and the access, and does NOT repeat
        // the command or carry layer A's vocabulary.
        assert!(lines[0].contains("`bash` wants exec access"), "{}", lines[0]);
        assert!(!lines[0].contains(cmd), "the command is back in the headline: {}", lines[0]);
        assert!(!lines[0].contains("intents"), "the taxonomy is back on top: {}", lines[0]);

        // And layer A is still there, under it, for whoever wants it.
        assert!(
            lines.iter().any(|l| l.contains("intents [execute_code]")),
            "the deterministic reading was dropped rather than demoted: {lines:#?}"
        );
    }

    /// A summary some other builder wrote is shown whole rather than mangled: the
    /// head takes the target off the end only when the end IS the target.
    #[test]
    fn a_summary_that_does_not_end_in_its_target_is_left_alone() {
        assert_eq!(
            ask_without_target("`bash` wants exec access to `ls -la`", "ls -la").as_deref(),
            Some("`bash` wants exec access")
        );
        assert_eq!(ask_without_target("something else entirely", "ls -la"), None);
        assert_eq!(ask_without_target("`web_search` wants network access", ""), None);
    }

    /// **`/cells` sends the rows this head drew, with the operator's message.**
    ///
    /// The model can ask for a screen (`harness what=screen`); this is the other
    /// direction, and it is captured at Enter rather than when the turn gets round
    /// to it — by then the screen has moved.
    #[test]
    fn cells_sends_the_screen_with_the_message() {
        let mut a = app();
        // Draw once, so the head has a size and a frame. Nothing is sent before
        // that: an unrendered head has no cells, and an empty block would read as a
        // blank terminal.
        assert!(
            matches!(a.submit("/cells look at this".into()), None),
            "a head that has drawn nothing must refuse rather than send emptiness"
        );
        let _ = a.screen(100, 12);

        let Some(Action::Prompt(text)) = a.submit("/cells look at this".into()) else {
            panic!("/cells did not send");
        };
        assert!(text.starts_with("look at this\n\n"), "{text}");
        assert!(text.contains("100x12"), "the head's real size: {text}");
        // Delimited at both ends, so the model can see where the picture stops.
        assert!(text.contains(CELLS_OPEN) && text.contains(CELLS_CLOSE), "{text}");

        // And the transcript shows the words plus one line, not the screen again.
        let folded = fold_cells(&text).expect("a cells message folds");
        assert!(folded.starts_with("look at this"), "{folded}");
        assert!(folded.contains("rows of this screen (100x12)"), "{folded}");
        assert!(!folded.contains(CELLS_OPEN), "the marker leaked into the fold: {folded}");
        assert_eq!(folded.lines().count(), 2, "one message, one note: {folded}");
        // An ordinary message is left exactly alone.
        assert_eq!(fold_cells("just a message"), None);
        // The rows themselves, not a summary of them.
        let drawn = a.screen(100, 12);
        let last = drawn.last().expect("a frame has rows");
        assert!(text.contains(last.as_str()), "the rows are not in the message");

        // The pending row holds what was SENT, byte for byte — that is what the
        // transcript's user item will match when it lands.
        let echo = a.pending_prompts.last().expect("the message is echoed");
        assert_eq!(echo, &text);
        // And it is DRAWN folded: the operator's words and one line, never a copy
        // of the screen inside the screen.
        let drawn = queued_lines(echo, &a.cfg);
        assert!(drawn.iter().any(|l| l.contains("look at this")), "{drawn:#?}");
        assert!(
            drawn.iter().any(|l| l.contains("rows of this screen")),
            "{drawn:#?}"
        );
        assert!(
            drawn.len() < 6,
            "the queued row is painting the whole screen: {} lines",
            drawn.len()
        );
    }

    /// **A refusal the harness made is one dim line, not a wall in red.**
    ///
    /// The operator's report, about a `bash` one-liner the normaliser could not
    /// read: *"i get what it tries to do, but it just throws up on my chat"*. Every
    /// word of that explanation is addressed to the model, it was already on the
    /// screen as the tool's own result one row above, and nothing in it is
    /// answerable — no adjudicator was consulted and no grant lifts it.
    #[test]
    fn a_refusal_nobody_made_is_quiet_and_one_line() {
        let mut a = app();
        let long = "this command's meaning does not exist yet, so nothing can decide \
                    about it. The grammar read 315 bytes and 7 stage(s) and could not \
                    resolve:\n  parameter_expansion at 1:2 (bytes 2..4) decides the \
                    assignment: \"$$\"\n      `$$` decides what the variable will hold, \
                    and its value is not in this text.";
        let hub = Hub::new("s");
        let att = hub.attach("tui", "test", Caps::default(), 0);
        hub.publish(letibot_sessionlog::event::SessionEvent::DenialRaised {
            request_id: "adj-s-1789462738453908838-0001".into(),
            turn_id: "t1".into(),
            call_id: "c1".into(),
            tool: "bash".into(),
            summary: "`bash` wants exec access to `p=$$; for i in 1 2 3; do read -r ppid; done`"
                .into(),
            baseline: "ask — intents [execute_code]".into(),
            by: "boundary:normaliser".into(),
            basis: long.into(),
            tier: "adjudicable".into(),
            outcome: "not_run".into(),
            repeat_count: 1,
            breaker_open: false,
            grant: "Nothing was executed and nothing changed. No grant applies.".into(),
        });
        feed(&mut a, &hub, &att.head_id);

        // A first refusal says nothing here at all: the refused call is a row in
        // the transcript one line below, with the same sentence on it.
        assert!(
            a.notes.is_empty(),
            "a single refusal is stated twice: {:?}",
            a.notes
        );

        // A SECOND attempt at the same direction is a fact about the session that
        // the row cannot carry, so that one does speak.
        let hub = Hub::new("s2");
        let att = hub.attach("tui", "test", Caps::default(), 0);
        hub.publish(letibot_sessionlog::event::SessionEvent::DenialRaised {
            request_id: "adj-2".into(),
            turn_id: "t1".into(),
            call_id: "c2".into(),
            tool: "bash".into(),
            summary: "`bash` wants exec access to `p=$$`".into(),
            baseline: "ask — intents [execute_code]".into(),
            by: "boundary:normaliser".into(),
            basis: long.into(),
            tier: "adjudicable".into(),
            outcome: "not_run".into(),
            repeat_count: 2,
            breaker_open: false,
            grant: "No grant applies.".into(),
        });
        let mut a = app();
        feed(&mut a, &hub, &att.head_id);
        let note = a.notes.last().expect("a repeat is worth saying");
        let lines = note_lines(&a.cfg, &note.1);
        // One sentence, so at most a wrap of one. The thing being measured is that
        // it is not a paragraph per unresolved construct.
        assert!(lines.len() <= 2, "a wall again, {} lines: {lines:#?}", lines.len());
        let l = lines.join(" ");
        let l = &l;
        assert!(l.contains("bash not_run"), "{l}");
        assert!(l.contains("meaning does not exist yet"), "the gist survived: {l}");
        // Not the paragraph, not the id, not the loud register.
        assert!(!l.contains("parameter_expansion"), "the model's detail leaked: {l}");
        assert!(!l.contains("adj-s-"), "an id nobody can use: {l}");
        assert!(!l.contains('!'), "still shouting: {l}");
    }

    /// **A refusal's reasoning folds, and the envelope never shows.** Both halves
    /// of *"it just throws up on my chat"*: layer A's reason is a document, and the
    /// row under it was carrying the marker the model reads.
    #[test]
    fn a_reason_that_is_a_document_folds_to_its_first_sentence() {
        assert!(is_envelope("<<<TOOL_ERROR 5ebfdef6>>>"));
        assert!(is_envelope("  <<<END_TOOL_ERROR 5ebfdef6>>>  "));
        assert!(!is_envelope("< < <TOOL_ERROR 5ebfdef6>>>"), "a neutralised body line");
        assert!(!is_envelope("error: could not find `Cargo.toml`"));

        let doc = "this command's meaning does not exist yet, so nothing can decide \
                   about it. The grammar read 315 bytes and could not resolve:\n  \
                   parameter_expansion at 1:2 decides the assignment";
        let gist = first_sentence(doc);
        assert_eq!(gist, "this command's meaning does not exist yet, so nothing can decide about it.");
        assert!(!gist.contains("parameter_expansion"));
        // A reason that IS a sentence is left exactly alone.
        let one = "the workspace has no writable backend";
        assert_eq!(first_sentence(one), one);
    }

    /// Type into the composer the way a person does, one key at a time. There is
    /// no `input` field to assign any more, and that is the point: the composer
    /// is a state machine with undo batching and a paste ledger, and a test that
    /// reaches past it is not testing what runs.
    fn typed(a: &mut App, text: &str) {
        for c in text.chars() {
            a.key(Key::Char(c));
        }
    }

    /// Drive an app from a hub the way the real driver does.
    fn feed(app: &mut App, hub: &Hub, head_id: &str) -> (u64, u64) {
        let (mut r, mut f) = (0, 0);
        while let letibot_sessionlog::hub::Delivery::Events(b) = hub.next_batch(head_id, 512) {
            for env in b.events() {
                match app.apply(ServerFrame::Event(env.clone())) {
                    Disposition::Rendered => r += 1,
                    Disposition::Filtered => f += 1,
                    Disposition::Control => {}
                }
            }
            if b.len() < 512 {
                break;
            }
        }
        (r, f)
    }

    #[test]
    fn a_recorded_session_renders_without_a_daemon() {
        // W8 is a leaf: give it a recorded log and it is built and demoed before a
        // turn engine exists.
        let mut a = app();
        for e in testing::recorded_session() {
            a.apply(ServerFrame::Event(letibot_sessionlog::event::Envelope {
                session_id: "s".into(),
                seq: a.seq + 1,
                ts: 0,
                event: e,
            }));
        }
        for (id, item) in testing::recorded_items() {
            a.record_item(&id, item);
        }
        let screen = a.screen(80, 30);
        assert_eq!(screen.len(), 30);
        assert!(screen.iter().all(|l| line_width(l) <= 80));
        let joined = screen.join("\n");
        assert!(joined.contains("cached"), "the status of the turn is shown");
    }

    #[test]
    fn a_head_that_filters_everything_still_says_so() {
        let mut a = app();
        a.verbosity = Verbosity::Terse;
        a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
        let before = a.filtered;
        for i in 0..10 {
            a.apply(ServerFrame::Event(env(
                2 + i,
                testing::reasoning("t1", "thinking "),
            )));
        }
        assert_eq!(a.filtered - before, 10);
        // Where it says so is `/status`, not the bottom border. §13.2b's rule is
        // about the moment the disclosure is READ — the count has to exist, be
        // exact, and be reachable without restarting anything. It was never an
        // argument for a resident row of zeros next to the prompt.
        a.command("status");
        let screen = a.screen(120, 40).join("\n");
        assert!(screen.contains("filtered"), "{screen}");
        assert!(screen.contains("10 (terse)"), "{screen}");
        // And a head that has lost nothing says nothing on the border.
        assert_eq!(a.status_line(200), "", "a clean head has a clean border");
    }

    /// The border is not silent about a counter that has **moved**.
    ///
    /// The half of §13.2b that does belong on a resident row: a head that dropped
    /// events is a head whose transcript has a hole in it, and that must not wait
    /// for somebody to type a command.
    #[test]
    fn a_counter_that_has_moved_reaches_the_border_and_a_zero_one_does_not() {
        let hub = Hub::new("s");
        hub.publish(testing::turn_started("t1"));
        let mut a = app();
        assert_eq!(a.status_line(200), "");
        a.apply(ServerFrame::Resync {
            reason: "queue overflow".into(),
            dropped: 12,
            snapshot: Box::new(hub.snapshot()),
            scrubbed: Default::default(),
        });
        // The border says it with a triangle, pinned right — a fact that exists
        // only while it does, and never a resident sentence of bright yellow.
        let screen = a.screen(120, 24).join("\n");
        assert!(screen.contains('⚠'), "{screen}");
        assert!(
            !screen.contains("dropped 12"),
            "the numbers are /status's, not the border's: {screen}"
        );
        // …where they keep their names and their counts.
        a.command("status");
        let stats = a.screen(120, 40).join("\n");
        let dropped_row = stats
            .lines()
            .find(|l| l.contains("dropped"))
            .expect("the dropped row is on the /status screen");
        assert!(dropped_row.contains("12"), "{dropped_row}");
        let resync_row = stats
            .lines()
            .find(|l| l.contains("resync"))
            .expect("the resync row is on the /status screen");
        assert!(resync_row.contains('1'), "{resync_row}");
        // The unboxed composer — no border to pin a triangle to — still names
        // them on a line of its own.
        let border = a.status_line(200);
        assert!(border.contains("dropped 12"), "{border}");
        assert!(border.contains("resync 1"), "{border}");
        assert!(border.contains("/status"), "and says where the rest is: {border}");
    }

    #[test]
    fn a_late_head_shows_the_accumulated_text_and_then_increments() {
        let hub = Hub::new("s");
        hub.publish(testing::turn_started("t1"));
        for w in ["Hello", " ", "world"] {
            hub.publish(testing::delta("t1", w));
        }
        let att = hub.attach("tui", "test", Caps::default(), 0);
        let mut a = app();
        a.apply(ServerFrame::Hello {
            protocol_version: letibot_sessionlog::protocol::PROTOCOL_VERSION,
            session_id: "s".into(),
            head_id: att.head_id.clone(),
            dropped: att.dropped,
            snapshot: att.snapshot.map(Box::new),
            resumed_from: att.resumed_from,
            scrubbed: att.scrubbed,
            wiring: Default::default(),
            sessions: Vec::new(),
        });
        hub.publish(testing::delta("t1", "!"));
        feed(&mut a, &hub, &att.head_id);
        assert_eq!(a.turn.as_ref().unwrap().text.raw(), "Hello world!");
    }

    #[test]
    fn a_refused_call_is_rendered_as_refused_rather_than_as_silence() {
        // The old head recorded settled decisions in a field it never drew, so a
        // denied tool call left a prompt on the screen and then nothing where the
        // answer should have been.
        let mut a = app();
        a.apply(ServerFrame::Event(env(1, testing::requested("r1", "rm -rf /"))));
        a.apply(ServerFrame::Event(env(2, testing::answered("r1", "deny"))));
        let screen = a.screen(120, 16).join("\n");
        assert!(screen.contains("rm -rf /"), "{screen}");
        assert!(screen.contains("REFUSED"), "{screen}");
    }

    #[test]
    fn a_settled_decision_is_not_offered_for_answering() {
        let mut a = app();
        a.apply(ServerFrame::Event(env(1, testing::requested("r1", "rm"))));
        assert_eq!(a.open_decisions().len(), 1);
        a.apply(ServerFrame::Event(env(2, testing::answered("r1", "deny"))));
        assert!(a.open_decisions().is_empty());
        // And typing an option id no longer answers it: it becomes a prompt.
        typed(&mut a, "allow");
        assert!(matches!(a.key(Key::Enter), Some(Action::Prompt(_))));
    }

    #[test]
    fn a_compact_is_asked_of_the_session_this_head_is_in() {
        // Not attached: refused with a line, not an action the daemon would have
        // to guess about.
        let mut a = app();
        typed(&mut a, "/compact");
        assert!(a.key(Key::Enter).is_none());
        // Attached: the action, naming nobody — the daemon compacts the session
        // the head is sitting in, which is the one /compact can reach.
        let hub = letibot_sessionlog::hub::Hub::new("s");
        let att = hub.attach("tui", "test", letibot_sessionlog::protocol::Caps::default(), 0);
        let mut a = app();
        a.apply(ServerFrame::Hello {
            protocol_version: letibot_sessionlog::protocol::PROTOCOL_VERSION,
            session_id: "s".into(),
            head_id: att.head_id.clone(),
            dropped: att.dropped,
            snapshot: att.snapshot.map(Box::new),
            resumed_from: att.resumed_from,
            scrubbed: att.scrubbed,
            wiring: Default::default(),
            sessions: Vec::new(),
        });
        typed(&mut a, "/compact");
        assert_eq!(a.key(Key::Enter), Some(Action::Compact));
    }

    #[test]
    fn ctrl_p_opens_the_todos_pane_and_esc_closes_it() {
        let mut a = app();
        // Opening asks for the list — the bootstrap read — and the pane draws
        // both of its sections, labelled as the two different things they are.
        assert_eq!(a.key(Key::CtrlP), Some(Action::ListTodos));
        let screen = a.screen(100, 30).join("\n");
        assert!(screen.contains("todos"), "{screen}");
        assert!(screen.contains("the model's plan"), "{screen}");
        assert!(screen.contains("read-only"), "{screen}");
        // And Esc is "go back", before the composer sees it.
        a.key(Key::Esc);
        assert!(!a.todos_pane);
        // Toggling twice does not ask twice without opening in between.
        assert_eq!(a.key(Key::CtrlP), Some(Action::ListTodos));
        assert_eq!(a.key(Key::CtrlP), None);
    }

    #[test]
    fn ctrl_g_opens_the_subagent_tree_and_esc_closes_it() {
        let mut a = app();
        a.apply(ServerFrame::Event(env(
            1,
            SessionEvent::Subagent {
                subagent_id: "s-sub-1".into(),
                state: "running".into(),
                prompt: "summarize ~/bin/letibot".into(),
                role: "coder".into(),
            },
        )));
        // A spawn that the head saw. The running count lives in the pane —
        // the composer's border used to repeat it, and that border is plain now.
        assert_eq!(a.subagents.len(), 1);

        a.key(Key::CtrlG);
        let screen = a.screen(100, 24).join("\n");
        assert!(screen.contains("subagents"), "{screen}");
        assert!(screen.contains("summarize ~/bin/letibot"), "{screen}");
        assert!(screen.contains("running"), "{screen}");

        a.key(Key::Esc);
        assert!(!a.subagents_pane);
    }

    /// The subagent tree is the parent's fact. Measured 2026-09-16: switching
    /// into a subagent carried "1 subagent running" onto ITS composer, and the
    /// pane there offered the row of the very session being looked at. On the way
    /// back the daemon replays the parent's retained events after `Hello`, so the
    /// tree is rebuilt from the same events that built it the first time.
    #[test]
    fn switching_into_a_subagent_drops_the_parents_tree_and_coming_back_rebuilds_it() {
        let mut a = app();
        a.apply(hello("s", vec![brief("s", "parent", true)], Hub::new("s").snapshot()));
        let spawn = SessionEvent::Subagent {
            subagent_id: "s-sub-1".into(),
            state: "running".into(),
            prompt: "find the bug".into(),
            role: "coder".into(),
        };
        a.apply(ServerFrame::Event(env(1, spawn.clone())));
        a.key(Key::CtrlG);
        // Enter reads; `o` is the key that moves the head.
        assert_eq!(a.key(Key::Char('o')), Some(Action::Switch("s-sub-1".into())));
        assert!(!a.subagents_pane, "switching closes the pane");

        a.apply(hello("s-sub-1", vec![brief("s", "parent", true)], Hub::new("s-sub-1").snapshot()));
        assert_eq!(a.session_id, "s-sub-1");
        assert!(a.subagents.is_empty(), "the parent's tree came along");
        let screen = a.screen(100, 24).join("\n");
        assert!(!screen.contains("subagent running"), "{screen}");

        // Back to the parent: Hello, then the replayed backlog.
        a.apply(hello("s", vec![brief("s", "parent", true)], Hub::new("s").snapshot()));
        a.apply(ServerFrame::Event(env(1, spawn)));
        assert_eq!(a.subagents.len(), 1);
        a.key(Key::CtrlG);
        let screen = a.screen(100, 24).join("\n");
        assert!(screen.contains("find the bug"), "{screen}");
    }

    /// A child that is still copying its workspace or booting its VM is listed as
    /// `opening`, and Enter on it goes nowhere — there is no session to go to.
    #[test]
    fn an_opening_subagent_is_listed_but_cannot_be_entered() {
        let mut a = app();
        a.apply(hello("s", vec![brief("s", "parent", true)], Hub::new("s").snapshot()));
        a.apply(ServerFrame::Event(env(
            1,
            SessionEvent::Subagent {
                subagent_id: "s-sub-1".into(),
                state: "opening".into(),
                prompt: "find the bug".into(),
                role: "coder".into(),
            },
        )));
        a.key(Key::CtrlG);
        let screen = a.screen(100, 24).join("\n");
        assert!(screen.contains("not attachable yet"), "{screen}");
        assert_eq!(a.key(Key::Enter), None, "Enter peeked at a session that is not open");
        assert_eq!(a.key(Key::Char('o')), None, "`o` switched into a session that is not open");
        assert!(a.subagents_pane, "the pane stays where the operator was");

        // Open now: the same row, and Enter goes there.
        a.apply(ServerFrame::Event(env(
            2,
            SessionEvent::Subagent {
                subagent_id: "s-sub-1".into(),
                state: "running".into(),
                prompt: "find the bug".into(),
                role: "coder".into(),
            },
        )));
        assert_eq!(a.subagents.len(), 1);
        assert_eq!(a.key(Key::Enter), Some(Action::Peek("s-sub-1".into())));
        a.sub_out_pending = None;
        assert_eq!(a.key(Key::Char('o')), Some(Action::Switch("s-sub-1".into())));
    }

    #[test]
    fn enter_on_a_subagent_row_asks_for_its_output_instead_of_switching() {
        let mut a = app();
        a.apply(ServerFrame::Event(env(
            1,
            SessionEvent::Subagent {
                subagent_id: "s-sub-1".into(),
                state: "running".into(),
                prompt: "summarize ~/bin/letibot".into(),
                role: "coder".into(),
            },
        )));
        a.key(Key::CtrlG);
        // Enter reads; it does not move the head. The ask is remembered, so a
        // rejection has something to end.
        assert_eq!(a.key(Key::Enter), Some(Action::Peek("s-sub-1".into())));
        assert_eq!(a.sub_out_pending.as_deref(), Some("s-sub-1"));
        assert!(a.subagents_pane, "the tree stays open under the read");
    }

    #[test]
    fn a_peeked_subagent_shows_its_tool_output_and_spills_the_whole_view() {
        let mut a = app();
        a.sub_out_pending = Some("s-sub-1".into());
        a.apply(ServerFrame::Peeked {
            session_id: "s-sub-1".into(),
            dropped: 3,
            events: vec![
                env(
                    1,
                    SessionEvent::TranscriptContent {
                        item_id: "i1".into(),
                        item: Box::new(TranscriptItem::ToolResult {
                            call_id: "c1".into(),
                            name: "bash".into(),
                            outcome: letibot_transcript::ToolOutcome::Ok,
                            payload: "line one\nline two".into(),
                        }),
                    },
                ),
                env(
                    2,
                    SessionEvent::ToolFinished {
                        turn_id: "t1".into(),
                        call_id: "c1".into(),
                        outcome: letibot_transcript::ToolOutcome::Ok,
                        payload_digest: "d".into(),
                        inline_bytes: 18,
                        full_bytes: 400,
                        spill: Some("/spill/c1".into()),
                        repairs: 0,
                        edit: None,
                    },
                ),
            ],
        });
        // The ask is answered; the view is the tool results, verbatim, with the
        // spill locator named and the drop disclosed.
        assert!(a.sub_out_pending.is_none());
        let v = a.sub_out.as_ref().expect("the view opened");
        assert_eq!(v.session_id, "s-sub-1");
        assert_eq!(v.dropped, 3);
        let text = v.lines.join("\n");
        assert!(text.contains("· bash — ok"), "{text}");
        assert!(text.contains("line one"), "{text}");
        assert!(text.contains("line two"), "{text}");
        assert!(text.contains("/spill/c1"), "{text}");
        // The whole view is on disk, at a name a re-read overwrites — under the
        // head's own runtime dir, never a world-readable /tmp. The live write
        // went where the head puts things; the writer itself is exercised under
        // a directory this test owns.
        let spill = v.spill.as_ref().expect("spilled");
        assert!(spill.ends_with("/letibot/subagent-s-sub-1.log") || spill.contains("/letibot-"), "{spill}");
        assert!(!spill.starts_with("/tmp/letibot-subagent"), "spilled to a predictable /tmp name: {spill}");
        let _ = std::fs::remove_file(spill);
        let dir = std::env::temp_dir().join(format!("letibot-peek-test-{}", std::process::id()));
        let under = spill_sub_out_under(&dir, "s-sub-1", &v.lines).expect("spilled");
        assert!(under.starts_with(dir.to_str().unwrap()), "{under}");
        let on_disk = std::fs::read_to_string(&under).expect("read");
        assert!(on_disk.contains("line two"), "{on_disk}");
        let _ = std::fs::remove_dir_all(&dir);
        // And the pane draws, header and disclosure included.
        let screen = a.screen(100, 24).join("\n");
        assert!(screen.contains("subagent output"), "{screen}");
        assert!(screen.contains("3 earlier events"), "{screen}");
    }

    #[test]
    fn arrows_walk_the_subagent_output_back_to_its_beginning() {
        let mut a = app();
        let payload: String = (0..50).map(|i| format!("line {i}\n")).collect();
        a.apply(ServerFrame::Peeked {
            session_id: "s-sub-1".into(),
            dropped: 0,
            events: vec![env(
                1,
                SessionEvent::TranscriptContent {
                    item_id: "i1".into(),
                    item: Box::new(TranscriptItem::ToolResult {
                        call_id: "c1".into(),
                        name: "bash".into(),
                        outcome: letibot_transcript::ToolOutcome::Ok,
                        payload,
                    }),
                },
            )],
        });
        // A terminal: the tail shows by default, the beginning does not.
        let screen = a.screen(100, 24).join("\n");
        assert!(screen.contains("line 49"), "{screen}");
        assert!(!screen.contains("line 0"), "{screen}");
        // Up walks back, and the draw clamps at the beginning — sixty ups on a
        // fifty-line view stop at the top rather than scrolling into nothing.
        for _ in 0..60 {
            a.key(Key::Up);
        }
        let screen = a.screen(100, 24).join("\n");
        assert!(screen.contains("line 0"), "{screen}");
    }

    #[test]
    fn esc_leaves_the_output_and_o_still_switches_into_the_subagent() {
        let mut a = app();
        a.apply(ServerFrame::Event(env(
            1,
            SessionEvent::Subagent {
                subagent_id: "s-sub-1".into(),
                state: "done".into(),
                prompt: "summarize ~/bin/letibot".into(),
                role: "coder".into(),
            },
        )));
        a.key(Key::CtrlG);
        a.key(Key::Enter);
        a.apply(ServerFrame::Peeked {
            session_id: "s-sub-1".into(),
            dropped: 0,
            events: vec![],
        });
        // An empty scrollback says so; it does not look like a missing session.
        let v = a.sub_out.as_ref().expect("the view opened");
        assert!(v.lines[0].contains("no tool output"), "{}", v.lines[0]);
        // Esc goes back to the tree — the tree, not everything closed.
        a.key(Key::Esc);
        assert!(a.sub_out.is_none());
        assert!(a.subagents_pane, "back to the tree");
        // `o` is still the way in: it switches, as Enter used to.
        assert!(matches!(
            a.key(Key::Char('o')),
            Some(Action::Switch(id)) if id == "s-sub-1"
        ));
    }

    #[test]
    fn a_running_subagent_row_becomes_done_rather_than_a_second_line() {
        let mut a = app();
        a.apply(ServerFrame::Event(env(
            1,
            SessionEvent::Subagent {
                subagent_id: "s-sub-1".into(),
                state: "running".into(),
                prompt: "summarize ~/bin/letibot".into(),
                role: "coder".into(),
            },
        )));
        a.apply(ServerFrame::Event(env(
            2,
            SessionEvent::Subagent {
                subagent_id: "s-sub-1".into(),
                state: "done".into(),
                prompt: "Here is the summary.".into(),
                role: "coder".into(),
            },
        )));
        assert_eq!(a.subagents.len(), 1, "done replaces running, not appends");
        assert_eq!(a.subagents[0].state, "done");
        a.key(Key::CtrlG);
        let screen = a.screen(100, 24).join("\n");
        assert!(screen.contains("done"), "{screen}");
        assert!(screen.contains("Here is the summary."), "{screen}");
    }

    #[test]
    fn the_todos_pane_shows_both_sources_and_says_which_is_which() {
        let mut a = app();
        // The session's list, as the event carried it.
        a.apply(ServerFrame::Event(env(
            1,
            SessionEvent::TodosUpdated {
                todos: vec![
                    letibot_sessionlog::event::TodoEntry {
                        content: "seat the tool".into(),
                        status: letibot_sessionlog::event::TodoStatus::Completed,
                    },
                    letibot_sessionlog::event::TodoEntry {
                        content: "render the pane".into(),
                        status: letibot_sessionlog::event::TodoStatus::InProgress,
                    },
                ],
            },
        )));
        // The repo's queue, as the pane-open read found it. Pointed at this
        // workspace, which has a real TODO.md with sections and checkboxes.
        a.wiring.workspace = std::env::var("CARGO_MANIFEST_DIR")
            .map(|d| {
                std::path::Path::new(&d)
                    .parent()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .display()
                    .to_string()
            })
            .unwrap_or_default();
        a.key(Key::CtrlP);
        let screen = a.screen(110, 40).join("\n");
        assert!(screen.contains("[x] seat the tool"), "{screen}");
        assert!(screen.contains("[~] render the pane"), "{screen}");
        assert!(
            screen.contains("TODO.md"),
            "the second source is named: {screen}"
        );
        assert!(
            screen.contains("open,") && screen.contains("done"),
            "sections carry their checkbox counts: {screen}"
        );
    }

    /// **The prompt is a control, not a spelling test.**
    ///
    /// Up and Down walk the ladder and Enter takes the highlighted one — the operator
    /// never types a word they can get wrong. Reported twice as *"it wasn't a choice
    /// but something I have to type (and mistype) myself"* before it was built.
    #[test]
    fn a_decision_is_answered_with_the_arrows_and_enter() {
        let mut a = app();
        a.apply(ServerFrame::Event(env(1, testing::requested("r1", "rm"))));
        let opts: Vec<String> = a.open_decisions()[0]
            .options
            .iter()
            .map(|o| o.option_id.clone())
            .collect();
        assert!(opts.len() >= 2, "need a ladder to walk: {opts:?}");

        // Enter with nothing typed takes the FIRST option, because a fresh question
        // starts at the top of its own ladder.
        assert_eq!(
            a.key(Key::Enter),
            Some(Action::Answer {
                req_id: "r1".into(),
                option_id: opts[0].clone(),
                pattern: None
            })
        );

        // Down moves one, and the answer follows the marker rather than the order the
        // options happen to arrive in.
        let mut b = app();
        b.apply(ServerFrame::Event(env(1, testing::requested("r2", "rm"))));
        assert_eq!(b.key(Key::Down), None, "moving is not answering");
        assert_eq!(
            b.key(Key::Enter),
            Some(Action::Answer {
                req_id: "r2".into(),
                option_id: opts[1].clone(),
                pattern: None
            })
        );

        // Up from the top wraps to the bottom rather than sticking, so the last option
        // — which is usually the one that denies — is one keypress away.
        let mut c = app();
        c.apply(ServerFrame::Event(env(1, testing::requested("r3", "rm"))));
        assert_eq!(c.key(Key::Up), None);
        assert_eq!(
            c.key(Key::Enter),
            Some(Action::Answer {
                req_id: "r3".into(),
                option_id: opts[opts.len() - 1].clone(),
                pattern: None
            })
        );
    }

    /// The arrows belong to the decision only while the composer is empty.
    ///
    /// A half-typed line still edits and still sends, so nothing was taken away from
    /// the person who prefers typing — including `/command`, which shares Enter.
    #[test]
    fn a_half_typed_line_keeps_the_arrows_and_enter() {
        let mut a = app();
        a.apply(ServerFrame::Event(env(1, testing::requested("r1", "rm"))));
        typed(&mut a, "some prose");
        // Enter sends the line as a prompt, not as an answer to the decision.
        assert!(
            matches!(a.key(Key::Enter), Some(Action::Prompt(t)) if t == "some prose"),
            "a typed line must still submit while a decision is open"
        );
    }

    #[test]
    fn an_open_decision_is_answered_by_typing_the_option() {
        let mut a = app();
        a.apply(ServerFrame::Event(env(1, testing::requested("r1", "rm"))));
        typed(&mut a, "deny");
        assert_eq!(
            a.key(Key::Enter),
            Some(Action::Answer {
                req_id: "r1".into(),
                option_id: "deny".into(),
                pattern: None
            })
        );
    }

    #[test]
    fn ctrl_c_clears_the_composer_and_only_a_double_tap_quits() {
        // The old rule quit an idle head on one press and interrupted a running
        // one, so there was no way to abandon a half-typed prompt and a stray
        // Ctrl+C killed the head. opencode's rule, via `letibot_ui::editor`.
        let mut a = app();
        a.clock(1_000);
        typed(&mut a, "half a question I am still");
        assert_eq!(a.key(Key::CtrlC), None, "it clears, it does not quit");
        assert_eq!(a.input(), "");
        assert_eq!(a.key(Key::CtrlC), None, "one press on an empty composer");
        assert_eq!(a.key(Key::CtrlC), Some(Action::Quit));
    }

    #[test]
    fn esc_twice_interrupts_a_running_turn_and_says_so_when_nothing_is_running() {
        let mut a = app();
        a.clock(1_000);
        a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
        assert_eq!(a.key(Key::Esc), None, "one press arms, it does not fire");
        // …and the hint changes, which is the entire mechanism by which anybody
        // learns the double-tap exists.
        assert!(a.hint_bar(120).contains("again"), "{}", a.hint_bar(120));
        assert!(matches!(a.key(Key::Esc), Some(Action::Interrupt(_))));

        let mut a = app();
        a.clock(1_000);
        a.key(Key::Esc);
        assert_eq!(a.key(Key::Esc), None);
        assert!(a.screen(100, 20).join("\n").contains("nothing is running"));
    }

    #[test]
    fn the_composer_is_a_field_with_a_caret_in_it_and_no_prose() {
        // The complaint, as an assertion: *"the chat prompt is basically not
        // empty — 'ask something...' and it just stays without any visual cues it
        // is actually a text input"*. Three things have to be true of an empty
        // composer: it is inside a container, the caret is in it, and there is no
        // prose in the field telling you to type.
        let mut a = app();
        let screen = a.screen(80, 20);
        let (row, col) = a.cursor().expect("a composer always has a caret");
        assert!(screen[row].contains('│'), "walls: {:?}", screen[row]);
        assert!(screen[row - 1].contains('╭'), "top: {:?}", screen[row - 1]);
        assert!(screen[row + 1].contains('╰'), "bottom: {:?}", screen[row + 1]);
        assert!(
            !screen[row].contains("ask something"),
            "no prose inside the field: {:?}",
            screen[row]
        );
        // The caret sits just past the prompt glyph, in an otherwise empty field —
        // and the field itself sits one gutter in from the terminal's edge.
        assert_eq!(col, 4 + App::GUTTER, "{:?}", screen[row]);

        // And it moves with the text.
        typed(&mut a, "why did the cache miss");
        let screen = a.screen(80, 20);
        let (row2, col2) = a.cursor().unwrap();
        assert_eq!(col2, 4 + App::GUTTER + "why did the cache miss".len());
        assert!(screen[row2].contains("why did the cache miss"));
    }

    #[test]
    fn the_facts_and_the_keys_are_on_their_own_rows_not_in_the_field() {
        let mut a = app();
        a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
        let screen = a.screen(100, 20);
        let (row, _) = a.cursor().unwrap();
        let joined = screen.join("\n");
        // The top edge closes the box and says nothing: the legend that lived
        // there — model, dialect, endpoint, verbosity — was a row of attention
        // the eye paid on every return to the field for facts read once.
        assert!(screen[row - 1].contains('╭'), "{:?}", screen[row - 1]);
        assert!(!screen[row - 1].contains("normal"), "{:?}", screen[row - 1]);
        // …the bottom edge closes the box and says nothing, because nothing has
        // gone wrong…
        assert!(screen[row + 1].contains('╰'), "{:?}", screen[row + 1]);
        assert!(
            !screen[row + 1].contains("seq"),
            "the telemetry is not resident in the operator's frame: {:?}",
            screen[row + 1]
        );
        // …and the keys on a bar below the box, never in the field.
        assert!(screen[row + 2].contains("ctrl-r"), "{:?}", screen[row + 2]);
        assert!(!screen[row].contains("ctrl-r"), "{:?}", screen[row]);
        assert!(!joined.contains("dropped 0"), "{joined}");
        // Reachable in one command, with the sequence numbers, the full id, and
        // the verbosity the border used to carry.
        a.command("status");
        let stats = a.screen(100, 40).join("\n");
        assert!(stats.contains("dropped"), "{stats}");
        assert!(stats.contains("seq"), "{stats}");
        assert!(stats.contains("verbosity"), "{stats}");
        assert!(stats.contains("normal"), "{stats}");
    }

    #[test]
    fn the_composer_survives_a_narrow_terminal_and_a_short_one() {
        let mut a = app();
        a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
        typed(&mut a, "a question long enough to wrap in a narrow terminal");
        for w in [20usize, 30, 40, 60, 80, 200] {
            for h in [3usize, 5, 8, 12, 24, 60] {
                let screen = a.screen(w, h);
                assert_eq!(screen.len(), h, "w={w} h={h}");
                for l in &screen {
                    assert!(line_width(l) <= w, "w={w} h={h}: {} cols: {l}", line_width(l));
                }
                let (row, col) = a.cursor().unwrap();
                assert!(row < h, "the caret is off the screen at w={w} h={h}");
                assert!(col < w, "the caret is off the right edge at w={w} h={h}");
                // The box is either whole or gone; never one wall of it.
                let top = screen.iter().filter(|l| l.contains('╭')).count();
                let bot = screen.iter().filter(|l| l.contains('╰')).count();
                assert_eq!(top, bot, "half a box at w={w} h={h}:\n{}", screen.join("\n"));
            }
        }
    }

    #[test]
    fn a_multi_line_prompt_grows_the_field_and_the_caret_follows() {
        let mut a = app();
        typed(&mut a, "first line");
        a.key(Key::SoftEnter);
        typed(&mut a, "second line");
        let screen = a.screen(80, 24);
        let (row, _) = a.cursor().unwrap();
        assert!(screen[row].contains("second line"), "{:?}", screen[row]);
        assert!(screen[row - 1].contains("first line"), "{:?}", screen[row - 1]);
        // Enter still sends the whole thing, both lines.
        match a.key(Key::Enter) {
            Some(Action::Prompt(t)) => assert_eq!(t, "first line\nsecond line"),
            other => panic!("{other:?}"),
        }
        // …and Up recalls it.
        a.key(Key::Up);
        assert_eq!(a.input(), "first line\nsecond line");
    }

    #[test]
    fn a_pasted_stack_trace_collapses_and_is_sent_in_full() {
        let mut a = app();
        typed(&mut a, "why does this happen: ");
        let trace: String = (0..40).map(|i| format!("  at frame {i}\n")).collect();
        a.key(Key::Paste(trace));
        assert!(a.input().contains("[Pasted #1"), "{}", a.input());
        // The composer stayed small; the conversation did not scroll away.
        assert!(a.editor.height(a.composer_cols(), 24) <= 2);
        match a.key(Key::Enter) {
            Some(Action::Prompt(t)) => {
                assert!(t.contains("at frame 39"));
                assert!(!t.contains("[Pasted"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_prefill_line_reads_as_nearly_done_when_the_prompt_was_mostly_cached() {
        // The number this harness exists to move, on the screen while it is being
        // moved. Neither surveyed project can draw this line at all.
        let mut a = app();
        a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
        a.apply(ServerFrame::Event(env(
            2,
            SessionEvent::PromptProgress {
                turn_id: "t1".into(),
                progress: letibot_sessionlog::event::PromptProgress {
                    total: 41_233,
                    cache: 38_100,
                    processed: 39_900,
                    time_ms: 900,
                },
            },
        )));
        let line = a.turn_status(120);
        assert!(line.contains("prefill 97%"), "{line}");
        // The counts live in the header's ctx/cached readout; the line keeps what
        // the header cannot show — the expansion rate. 1,800 computed tokens in
        // 900 ms.
        assert!(line.contains("2000 tok/s"), "{line}");
        assert!(!line.contains("cached"), "{line}");
        // And it never wraps, at any width.
        for w in [24usize, 40, 60, 80, 120, 200] {
            assert!(line_width(&a.turn_status(w)) <= w, "w={w}");
        }
    }

    #[test]
    fn a_running_tool_call_shows_how_long_it_has_been_running_and_what_it_last_said() {
        // All three were derivable from events the head was already reading;
        // `Envelope::ts` is on every one of them and `ToolProgress { note }` was
        // being dropped on the floor.
        let mut a = app();
        a.apply(ServerFrame::Event(env_at(1, 1_000, testing::turn_started("t1"))));
        a.apply(ServerFrame::Event(env_at(
            2,
            1_000,
            testing::proposed("t1", "c1", "bash"),
        )));
        a.apply(ServerFrame::Event(env_at(
            3,
            2_000,
            SessionEvent::ToolStarted {
                turn_id: "t1".into(),
                call_id: "c1".into(),
                name: "bash".into(),
                access: Default::default(),
            },
        )));
        a.apply(ServerFrame::Event(env_at(
            4,
            6_200,
            SessionEvent::ToolProgress {
                turn_id: "t1".into(),
                call_id: "c1".into(),
                note: "compiling letibot-tui".into(),
            },
        )));
        let screen = a.screen(120, 24).join("\n");
        // The tense says "wait" without a colour or a glyph.
        assert!(screen.contains("Running"), "{screen}");
        assert!(screen.contains("4.2s"), "{screen}");
        assert!(screen.contains("compiling letibot-tui"), "{screen}");

        a.apply(ServerFrame::Event(env_at(
            5,
            9_500,
            SessionEvent::ToolFinished {
                turn_id: "t1".into(),
                call_id: "c1".into(),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload_digest: "fnv1a:1".into(),
                inline_bytes: 214,
                full_bytes: 214,
                spill: None,
                repairs: 0,
                edit: None,
            },
        )));
        let screen = a.screen(120, 24).join("\n");
        assert!(screen.contains("Ran"), "past tense once it is done: {screen}");
        assert!(screen.contains("7.5s"), "{screen}");
    }

    #[test]
    fn a_call_from_a_snapshot_has_no_duration_rather_than_a_zero_one() {
        // A snapshot carries no timestamps. `0.0s` is a measurement that was never
        // taken rendered as one that was.
        let hub = Hub::new("s");
        hub.publish(testing::turn_started("t1"));
        hub.publish(testing::proposed("t1", "c1", "read"));
        hub.publish(SessionEvent::ToolFinished {
            turn_id: "t1".into(),
            call_id: "c1".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload_digest: "fnv1a:1".into(),
            inline_bytes: 40,
            full_bytes: 40,
            spill: None,
            repairs: 0,
            edit: None,
        });
        let mut a = app();
        a.apply(ServerFrame::Resync {
            reason: "test".into(),
            dropped: 0,
            snapshot: Box::new(hub.snapshot()),
            scrubbed: Default::default(),
        });
        let card: Vec<String> = a
            .screen(120, 24)
            .into_iter()
            .filter(|l| l.trim_start().starts_with('\u{25cf}'))
            .collect();
        assert_eq!(card.len(), 1, "{card:?}");
        assert!(card[0].contains("Read"), "{card:?}");
        assert!(
            !card[0].contains("0ms") && !card[0].contains("0.0s"),
            "a snapshot has no clock, so it must show no duration: {card:?}"
        );
    }

    #[test]
    fn a_rejection_shows_both_sequence_numbers() {
        let mut a = app();
        a.apply(ServerFrame::Rejected {
            client_request_id: "c1".into(),
            reason: "stale expected_seq".into(),
            expected_seq: 12,
            actual_seq: 40,
        });
        // On its own line, above the composer — never *instead* of the composer,
        // which is what it used to be.
        let screen = a.screen(200, 12);
        let joined = screen.join("\n");
        assert!(joined.contains("12") && joined.contains("40"), "{joined}");
        let (row, _) = a.cursor().unwrap();
        assert!(
            screen[row].contains('›'),
            "the composer survived the notice: {:?}",
            screen[row]
        );
    }

    #[test]
    fn a_notice_does_not_outlive_its_welcome_or_hide_the_input() {
        let mut a = app();
        a.apply(ServerFrame::Accepted {
            client_request_id: "c1".into(),
            seq: 3,
            note: "stale expected_seq: queued anyway as a follow-up user item".into(),
        });
        assert!(a.screen(80, 12).join("\n").contains("queued anyway"));
        for _ in 0..200 {
            a.screen(80, 12);
        }
        assert!(
            !a.screen(80, 12).join("\n").contains("queued anyway"),
            "a notice that never expires becomes furniture"
        );
    }

    #[test]
    fn the_body_a_frame_builds_does_not_grow_with_the_session() {
        // §13.3, at the renderer rather than at the lexer. The old shape cloned the
        // whole history into every frame; this asserts the frame is the window.
        let mut a = app();
        for i in 0..400u64 {
            a.apply(ServerFrame::Event(env(
                i * 2 + 1,
                testing::appended(&format!("s.{i}"), "user"),
            )));
            a.apply(ServerFrame::Event(env(
                i * 2 + 2,
                testing::content(&format!("s.{i}"), "a line of conversation"),
            )));
        }
        let screen = a.screen(80, 24);
        assert_eq!(screen.len(), 24);
        assert!(a.body_len > 400, "the history is there: {}", a.body_len);
    }

    /// The same rule one layer up: **rendering** the history does not grow with
    /// the session either.
    ///
    /// `the_body_a_frame_builds_…` asserts the frame is the window. It says
    /// nothing about how many rows were rendered to build it, and that was the
    /// hole: every row whose body arrived threw the whole rendered transcript
    /// away, so a session of `n` rows cost `n²/2` renders. At 400 rows that is
    /// 80,000 against 400 — and the count, not a stopwatch, is the thing a test
    /// can hold.
    ///
    /// The bound is deliberately loose (`4n`): a row is legitimately re-rendered
    /// when its own round changes under it, and pinning this to the exact number
    /// would make it a test of the current round shape rather than of the rule.
    #[test]
    fn rendering_the_history_does_not_grow_with_the_session_either() {
        let mut a = app();
        let n = 400u64;
        for i in 0..n {
            a.apply(ServerFrame::Event(env(
                i * 2 + 1,
                testing::appended(&format!("s.{i}"), "user"),
            )));
            a.apply(ServerFrame::Event(env(
                i * 2 + 2,
                testing::content(&format!("s.{i}"), "a line of conversation"),
            )));
            // A frame per event, which is what the driver does: the walk has to
            // have caught up before the next row lands or nothing is re-rendered.
            let _ = a.screen(80, 24);
        }
        assert!(
            a.hist_renders < 4 * n,
            "{n} rows cost {} row renders; a full rebuild per row would be about {}",
            a.hist_renders,
            n * n / 2
        );
    }

    #[test]
    fn the_body_of_a_row_arrives_and_replaces_the_placeholder() {
        // Fault one, end to end through the head: announce, then fill.
        let mut a = app();
        a.apply(ServerFrame::Event(env(1, testing::appended("s.0", "user"))));
        assert!(
            a.screen(80, 12).join("\n").contains("waiting for the body"),
            "an announced row with no body says so"
        );
        a.apply(ServerFrame::Event(env(
            2,
            testing::content("s.0", "the operator's own prompt"),
        )));
        let screen = a.screen(80, 12).join("\n");
        assert!(screen.contains("the operator's own prompt"), "{screen}");
        assert!(!screen.contains("waiting for the body"), "{screen}");
    }

    #[test]
    fn a_turn_that_was_cut_short_does_not_read_like_one_that_finished() {
        let mut a = app();
        a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
        a.apply(ServerFrame::Event(env(2, testing::delta("t1", "half an ans"))));
        a.apply(ServerFrame::Event(env(
            3,
            SessionEvent::TurnFinished {
                turn_id: "t1".into(),
                finish_reason: letibot_sessionlog::event::FinishReason::Length,
                usage: Default::default(),
                timings: Default::default(),
            },
        )));
        let screen = a.screen(120, 16).join("\n");
        assert!(screen.contains("CUT SHORT"), "{screen}");
    }

    /// Found by watching a decode run: the last line of the streaming answer sat
    /// directly against whatever was drawn under it, and the blank appeared only
    /// when the turn ended and the pane stood down. The reasoning block and the
    /// call cards already carry their own trailing air, so the text block does
    /// too now, and this pins it.
    #[test]
    fn a_running_decode_keeps_a_blank_line_above_what_follows_it() {
        let mut a = app();
        // The window has to be full for this to mean anything: a short transcript
        // is padded to the room the body has, and that padding would pass the
        // assertion with or without the fix. Forty settled rows push the running
        // answer to the bottom of the window, which is where the defect lived.
        let mut seq = 1;
        for i in 0..40 {
            a.apply(ServerFrame::Event(env(
                seq,
                testing::appended(&format!("s.{i}"), "user"),
            )));
            seq += 1;
            a.apply(ServerFrame::Event(env(
                seq,
                testing::content(&format!("s.{i}"), "a line of earlier transcript"),
            )));
            seq += 1;
        }
        a.apply(ServerFrame::Event(env(seq, testing::turn_started("t1"))));
        seq += 1;
        a.apply(ServerFrame::Event(env(
            seq,
            testing::delta("t1", "half an answer"),
        )));
        let rows = a.screen(80, 24);
        let last_text = rows
            .iter()
            .rposition(|l| l.contains("half an answer"))
            .expect("the streaming answer is on the screen");
        let under = &rows[last_text + 1];
        assert!(
            under.trim().is_empty(),
            "a running decode is padded by a blank line, but this row follows it \
             directly: {under:?}"
        );
    }

    /// A fixture with the two things the operator's screen had in it: a heading
    /// and an inline code span, inside the model's reasoning.
    const REASONING_WITH_MARKDOWN: &str = "## The plan\n\nFirst read `crates/tui/src/render.rs`, then look at the `Decor` type, because **that** is where the style has to be restored, and the rest of this sentence has to stay the reasoning colour even though it wraps onto another row.\n\n### Then\n\nOrdinary prose, still grey.\n\n";

    /// The defect the operator found by looking: *"thinking color rendering has
    /// something unclosed in escapes — it tries to be gray, then say goes green
    /// and becomes white for several rows and then gray again."*
    ///
    /// It was not an unclosed escape and it was not a delta split mid-sequence. It
    /// was a **closed** one: a styled span inside the reasoning block closed with
    /// `\x1b[0m`, which restores the terminal default rather than the block. So
    /// the check is not "is everything closed" — it is "does every close hand the
    /// block's own style back".
    ///
    /// A screenshot would not have stopped this coming back; this does.
    #[test]
    fn every_reset_in_a_reasoning_row_restores_the_reasoning_style() {
        let mut a = App::new(RenderConfig {
            width: 100,
            color: true,
            ..RenderConfig::default()
        });
        a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
        a.apply(ServerFrame::Event(env(
            2,
            testing::reasoning("t1", REASONING_WITH_MARKDOWN),
        )));
        a.key(Key::CtrlR);
        let rows = a.screen(100, 40);

        let reopen = letibot_ui::style::Palette::Colour.open(Role::Reasoning);
        let reset = letibot_ui::width::RESET;
        let rail: Vec<&String> = rows.iter().filter(|l| l.contains('┃')).collect();
        assert!(
            rail.len() >= 4,
            "the fixture must reach the screen as several rail rows:\n{}",
            rows.join("\n")
        );
        // The heading has to actually be styled, or this test would pass on a
        // renderer that had simply stopped colouring anything.
        assert!(
            rail.iter().any(|l| l.contains(
                letibot_ui::style::Palette::Colour.open(Role::Subheading)
            )),
            "no heading was styled inside the reasoning; the fixture is not exercising the bug"
        );
        for l in rail {
            assert!(
                l.ends_with(reset),
                "a reasoning row ended with the block still open: {l:?}"
            );
            // The row's own close is a reset (and `wrap` may have added one of its
            // own), so the trailing run of them is the end of the row, not a leak.
            let body = l.trim_end_matches(reset);
            let mut at = 0;
            while let Some(hit) = body[at..].find(reset) {
                let after = at + hit + reset.len();
                assert!(
                    body[after..].starts_with(reopen),
                    "a reset inside the reasoning left the block: the text after \
                     it is {:?}\nwhole row: {:?}",
                    &body[after..body.len().min(after + 24)],
                    l
                );
                at = after;
            }
        }
    }

    /// The third defect, from using it: *"tool calls — i see `<function…` like
    /// strings first, then closing tag arrives and it becomes a toolcall."*
    ///
    /// The head's half of the fix. The engine's half — that the markup arrives on
    /// a channel of its own at all — is
    /// `letibot-turn`'s `the_body_of_a_tool_call_is_never_announced_as_assistant_text`.
    #[test]
    fn the_raw_markup_of_a_tool_call_is_never_on_the_screen_by_default() {
        const MARKUP: &str = "\n<function=read>\n<parameter=path>\nsrc/main.rs\n</parameter>\n";
        let mut a = app();
        a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
        a.apply(ServerFrame::Event(env(
            2,
            testing::delta("t1", "Reading it now.\n\n"),
        )));
        a.apply(ServerFrame::Event(env(
            3,
            SessionEvent::Delta {
                turn_id: "t1".into(),
                target: DeltaTarget::ToolCall,
                text: MARKUP.into(),
            },
        )));
        let default = a.screen(100, 30).join("\n");
        assert!(
            !default.contains("<function="),
            "the raw markup reached the default view:\n{default}"
        );
        assert!(
            default.contains("Reading it now."),
            "the answer went missing with it:\n{default}"
        );
        // …and the reader is told something is happening, which is the only thing
        // the markup was accidentally conveying.
        assert!(
            default.contains("writing a tool call"),
            "nothing stood in for the call being written:\n{default}"
        );

        // The operator kept the raw form deliberately: "I want to save the ability
        // to see raw tool calls but it should be behind some chord, different to
        // C-r."
        a.key(Key::CtrlX);
        let raw = a.screen(100, 30).join("\n");
        assert!(
            raw.contains("<function=read>") && raw.contains("<parameter=path>"),
            "ctrl-x revealed nothing:\n{raw}"
        );
        a.key(Key::CtrlX);
        assert!(
            !a.screen(100, 30).join("\n").contains("<function="),
            "ctrl-x does not toggle back off"
        );
    }

    /// The chord is not Ctrl+R — the operator ruled that out by name — and it is
    /// not one the composer or the terminal already owns.
    #[test]
    fn the_raw_chord_is_its_own_key_and_reaches_nothing_else() {
        let mut a = app();
        typed(&mut a, "hello");
        a.key(Key::CtrlX);
        assert_eq!(a.input(), "hello", "ctrl-x typed into the composer");
        assert!(a.raw_calls, "ctrl-x did not toggle the raw view");
        assert_eq!(a.reasoning, Fold::Folded, "ctrl-x moved the thinking fold");
        assert_eq!(a.tools, Fold::Folded, "ctrl-x moved the tool-output fold");
    }

    /// The second defect: *"no margins for the main output — things are hard left
    /// with literally zero space."*
    ///
    /// Asserted on **every** row rather than on the transcript, because the failure
    /// mode of fixing only the body is a header and a composer inset differently
    /// from the answer, which reads worse than no margin at all.
    #[test]
    fn every_row_of_the_frame_starts_one_gutter_in() {
        let mut a = app();
        a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
        a.apply(ServerFrame::Event(env(
            2,
            testing::delta("t1", "a paragraph of answer that is long enough to wrap\n"),
        )));
        a.apply(ServerFrame::Event(env(3, testing::appended("s.0", "user"))));
        a.apply(ServerFrame::Event(env(4, testing::content("s.0", "why"))));
        for w in [60usize, 80, 100, 110, 120] {
            let rows = a.screen(w, 24);
            for l in rows.iter().filter(|l| !l.trim().is_empty()) {
                assert!(
                    l.starts_with(&" ".repeat(App::GUTTER)),
                    "at {w} columns a row is hard left: {l:?}"
                );
                assert!(
                    line_width(l) <= w - App::GUTTER,
                    "at {w} columns a row overran the right gutter ({}): {l:?}",
                    line_width(l)
                );
            }
        }
    }

    /// A terminal too narrow to spare four columns gives the gutter up before it
    /// gives up any content.
    #[test]
    fn a_very_narrow_terminal_keeps_its_content_and_drops_the_gutter() {
        let mut a = app();
        a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
        a.apply(ServerFrame::Event(env(2, testing::delta("t1", "answer\n"))));
        let rows = a.screen(30, 20);
        assert!(
            rows.iter().any(|l| l.starts_with("answer")),
            "the gutter survived a 30-column terminal: {rows:?}"
        );
    }

    #[test]
    fn thinking_is_folded_by_default_and_the_fold_says_which_key_opens_it() {
        let mut a = app();
        a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
        for _ in 0..40 {
            a.apply(ServerFrame::Event(env(
                2,
                testing::reasoning("t1", "a line of working out\n"),
            )));
        }
        let folded = a.screen(80, 24).join("\n");
        assert!(folded.contains("ctrl-r"), "the affordance is on the screen");
        assert!(
            folded.matches("a line of working out").count() <= 1,
            "folded thinking shows the live line and no more:\n{folded}"
        );
        a.key(Key::CtrlR);
        let open = a.screen(80, 24).join("\n");
        assert!(
            open.matches("a line of working out").count() > 1,
            "ctrl-r opened nothing:\n{open}"
        );
    }

    #[test]
    fn the_log_alone_is_enough_to_render_the_conversation() {
        // T13.1's other half: *"the log should be a sufficient record of a
        // session"*. It was not. `letibot-tui --replay` reads exactly these
        // envelopes and nothing else, and before the body travelled on the log it
        // showed a placeholder for every row — the transcript existed only inside a
        // snapshot nobody had written down.
        let hub = Hub::new("s");
        hub.publish(testing::turn_started("t1"));
        hub.publish(testing::appended("s.0", "user"));
        hub.record_item(
            "s.0",
            TranscriptItem::User {
                parts: vec![UserPart::Text {
                    text: "why did the cache miss".into(),
                }],
            },
        );
        hub.publish(testing::appended("t1.0", "assistant"));
        hub.record_item(
            "t1.0",
            TranscriptItem::Assistant {
                text: "because reasoning_content was replayed into the wrong field".into(),
                tool_calls: vec![],
                truncated: false,
            },
        );
        hub.publish(testing::turn_finished("t1"));

        // Round-trip through the wire form, which is what `--replay` reads.
        let jsonl: Vec<String> = hub
            .retained()
            .iter()
            .map(|e| serde_json::to_string(e).unwrap())
            .collect();
        let mut a = app();
        for line in &jsonl {
            let env: letibot_sessionlog::event::Envelope = serde_json::from_str(line).unwrap();
            a.apply(ServerFrame::Event(env));
        }
        let screen = a.screen(100, 30).join("\n");
        assert!(screen.contains("why did the cache miss"), "{screen}");
        assert!(screen.contains("wrong field"), "{screen}");
        assert!(!screen.contains("waiting for the body"), "{screen}");
    }

    #[test]
    fn a_turn_that_has_gone_quiet_says_so_rather_than_spinning() {
        // Measured live: a turn failed, the engine published a `Warning` and no
        // `TurnFinished`, and the head span a spinner at a dead session for as long
        // as anyone left it open.
        let mut a = app();
        a.clock(1_000);
        a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
        assert!(!a.screen(120, 12).join("\n").contains("nothing received"));
        a.clock(1_000 + 40_000);
        let screen = a.screen(120, 12).join("\n");
        assert!(screen.contains("nothing received for 40.0s"), "{screen}");
    }

    #[test]
    fn a_spilled_result_reads_as_the_harness_working_not_as_damage() {
        let mut a = app();
        a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
        a.apply(ServerFrame::Event(env(2, testing::proposed("t1", "c1", "grep"))));
        a.apply(ServerFrame::Event(env(
            3,
            SessionEvent::ToolFinished {
                turn_id: "t1".into(),
                call_id: "c1".into(),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload_digest: "fnv1a:1".into(),
                inline_bytes: 8192,
                full_bytes: 480_000,
                spill: Some("9fa3c1".into()),
                repairs: 0,
                edit: None,
            },
        )));
        let screen = a.screen(160, 12).join("\n");
        assert!(screen.contains("8.0 KB of 468.8 KB"), "{screen}");
        assert!(screen.contains("read_spill hash=9fa3c1"), "{screen}");
        assert!(screen.contains("the rest is kept"), "{screen}");
    }

    #[test]
    fn a_long_line_is_broken_by_the_head_and_not_by_the_terminal() {
        // A 200-character path in an 80-column terminal. If the head emits it long,
        // the terminal wraps it, the head's line count is wrong, and the frame
        // fights the scroll region for the rest of the session.
        let mut a = app();
        a.apply(ServerFrame::Event(env(1, testing::appended("s.0", "user"))));
        a.apply(ServerFrame::Event(env(
            2,
            testing::content("s.0", &"/very/long/path".repeat(20)),
        )));
        for l in a.screen(80, 24) {
            assert!(line_width(&l) <= 80, "{} cols: {l}", line_width(&l));
        }
    }

    #[test]
    fn a_resync_replaces_state_rather_than_appending_to_it() {
        let hub = Hub::new("s");
        hub.publish(testing::turn_started("t1"));
        hub.publish(testing::delta("t1", "abc"));
        let mut a = app();
        a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
        a.apply(ServerFrame::Event(env(2, testing::delta("t1", "abc"))));
        a.apply(ServerFrame::Resync {
            reason: "queue overflow".into(),
            dropped: 0,
            snapshot: Box::new(hub.snapshot()),
            scrubbed: Default::default(),
        });
        assert_eq!(a.turn.as_ref().unwrap().text.raw(), "abc", "not abcabc");
        assert_eq!(a.resyncs, 1);
    }

    fn brief(id: &str, title: &str, running: bool) -> SessionBrief {
        SessionBrief {
            session_id: id.into(),
            title: title.into(),
            created_ms: 0,
            live: true,
            stored_items: 0,
            status: letibot_sessionlog::SessionStatus {
                session_id: id.into(),
                seq: 4,
                items: 6,
                heads: 1,
                running,
                model: "qwen-3.8-flash-next".into(),
                last_ms: 0,
            },
            wiring: wiring(),
            parent_session_id: None,
        }
    }

    fn wiring() -> SessionWiring {
        SessionWiring {
            model: "qwen-3.8-flash-next".into(),
            dialect: "qwen3.8".into(),
            endpoint: "127.0.0.1:8080".into(),
            workspace: "/home/dead/Projects/letibot".into(),
        }
    }

    fn hello(session: &str, sessions: Vec<SessionBrief>, snapshot: Snapshot) -> ServerFrame {
        ServerFrame::Hello {
            protocol_version: letibot_sessionlog::protocol::PROTOCOL_VERSION,
            session_id: session.into(),
            head_id: "h1".into(),
            dropped: 0,
            snapshot: Some(Box::new(snapshot)),
            resumed_from: None,
            scrubbed: Default::default(),
            wiring: wiring(),
            sessions,
        }
    }

    #[test]
    fn the_header_can_name_what_the_session_is_talking_to_before_any_turn() {
        // §4.4. It read `no turn yet` for a freshly attached head, because
        // `TurnStarted { model }` was the only one of the three facts that reached a
        // head and it only arrives when a turn starts — so nothing could say what
        // the session was about to talk to at the one moment somebody was about
        // to. The daemon has known the model since it parsed its own command
        // line; it says so on `Hello`, and the header — not the composer's
        // border, which the eye crosses on every return to the field — is where
        // it renders. The dialect and the endpoint do not ride along.
        let mut a = app();
        let empty = Hub::new("s").snapshot();
        a.apply(hello("s", vec![brief("s", "", false)], empty));
        let header = a.header_line(200);
        assert!(header.contains("qwen-3.8-flash-next"), "{header}");
        assert!(
            !header.contains("qwen3.8"),
            "the dialect is not the model's double: {header}"
        );
        assert!(
            !header.contains("127.0.0.1:8080"),
            "the endpoint is the daemon's business: {header}"
        );
    }

    #[test]
    fn a_running_tool_call_says_what_it_is_running_on() {
        // §4.1, fixed. The whole difference between a tool list that is useful and
        // one that is decorative.
        let mut a = app();
        a.apply(ServerFrame::Event(env_at(1, 1_000, testing::turn_started("t1"))));
        a.apply(ServerFrame::Event(env_at(
            2,
            1_000,
            testing::proposed_on("t1", "c1", "bash", "\"cargo test --workspace\""),
        )));
        a.apply(ServerFrame::Event(env_at(
            3,
            2_000,
            SessionEvent::ToolStarted {
                turn_id: "t1".into(),
                call_id: "c1".into(),
                name: "bash".into(),
                access: Default::default(),
            },
        )));
        let screen = a.screen(120, 24).join("\n");
        assert!(
            screen.contains("Running \"cargo test --workspace\""),
            "a running call renders its argument, not just its verb:\n{screen}"
        );
    }

    #[test]
    fn a_tool_call_the_head_never_saw_proposed_still_names_its_target() {
        // The reattach case, which is the common one: a head that joins after a
        // turn has no proposal event to have learned the target from, and the
        // arguments on the settled row are the only place it survives. It uses the
        // *same* function the wire does, so both renderings agree.
        let mut a = app();
        a.apply(ServerFrame::Event(env(1, testing::appended("t1.0", "assistant"))));
        a.apply(ServerFrame::Event(env(
            2,
            SessionEvent::TranscriptContent {
                item_id: "t1.0".into(),
                item: Box::new(TranscriptItem::Assistant {
                    text: String::new(),
                    tool_calls: vec![letibot_transcript::ToolCall {
                        id: "c1".into(),
                        name: "read".into(),
                        arguments: r#"{"path":"crates/ui/src/style.rs"}"#.into(),
                    }],
                    truncated: false,
                }),
            },
        )));
        let screen = a.screen(120, 24).join("\n");
        assert!(
            screen.contains("→ Read crates/ui/src/style.rs"),
            "{screen}"
        );
        assert!(!screen.contains("(c1)"), "the id is not shown when a name is: {screen}");
    }

    /// Two rounds of one turn, both numbering their calls from `call_0`, which is
    /// what `letibot_turn::items` does whenever the wire format carries no id.
    ///
    /// Taken from the operator's own session (`s-1788987496351498881`, fourteen
    /// rounds of `call_0`/`call_1`/`call_2`) and reduced to the two rows that make
    /// the defect: the head kept one session-wide table keyed on the call id, so
    /// the later round's paths overwrote the earlier round's and every settled card
    /// read back the survivor. On screen that was `▸ Read TODO.md · ok · 143 lines`
    /// above a body beginning `# rano` — the payload of `README.md`.
    ///
    /// It is a correctness test, not a layout one. A card that names a file the
    /// tool did not open is the head telling the operator something false about
    /// what a tool returned.
    #[test]
    fn a_second_round_of_calls_does_not_relabel_the_first_rounds_results() {
        fn call(id: &str, name: &str, arguments: &str) -> letibot_transcript::ToolCall {
            letibot_transcript::ToolCall {
                id: id.into(),
                name: name.into(),
                arguments: arguments.into(),
            }
        }
        fn assistant(a: &mut App, seq: u64, id: &str, calls: Vec<letibot_transcript::ToolCall>) {
            a.apply(ServerFrame::Event(env(seq, testing::appended(id, "assistant"))));
            a.apply(ServerFrame::Event(env(
                seq + 1,
                SessionEvent::TranscriptContent {
                    item_id: id.into(),
                item: Box::new(TranscriptItem::Assistant {
                    text: String::new(),
                    tool_calls: calls,
                    truncated: false,
                }),
                },
            )));
        }
        fn result(a: &mut App, seq: u64, id: &str, call_id: &str, name: &str, payload: &str) {
            a.apply(ServerFrame::Event(env(seq, testing::appended(id, "tool_result"))));
            a.apply(ServerFrame::Event(env(
                seq + 1,
                SessionEvent::TranscriptContent {
                    item_id: id.into(),
                    item: Box::new(TranscriptItem::ToolResult {
                        call_id: call_id.into(),
                        name: name.into(),
                        outcome: letibot_transcript::ToolOutcome::Ok,
                        payload: payload.into(),
                    }),
                },
            )));
        }

        let mut a = app();
        assistant(&mut a, 1, "r1.a", vec![call("call_0", "read", r#"{"path":"README.md"}"#)]);
        // Two lines each, so the payload is a body under a header rather than
        // inlined onto it — the pairing is what is under test, and it is only
        // visible when the two are separate rows.
        result(&mut a, 3, "r1.t", "call_0", "read", "FIRST-ROUND-PAYLOAD\nmore\n");
        assistant(&mut a, 5, "r2.a", vec![call("call_0", "read", r#"{"path":"TODO.md"}"#)]);
        result(&mut a, 7, "r2.t", "call_0", "read", "SECOND-ROUND-PAYLOAD\nmore\n");

        // A tall enough screen that both rounds are on it at once, which is the
        // only way the pairing is visible at all.
        let lines = a.screen(120, 60);
        // The card a payload is sitting under: the nearest header above it.
        let label = |needle: &str| -> String {
            let i = lines
                .iter()
                .position(|l| l.contains(needle))
                .unwrap_or_else(|| panic!("{needle} is not on the screen:\n{}", lines.join("\n")));
            lines[..i]
                .iter()
                .rev()
                .find(|l| l.contains('▸') || l.contains('▾'))
                .cloned()
                .unwrap_or_default()
        };
        let screen = lines.join("\n");
        assert!(
            label("FIRST-ROUND-PAYLOAD").contains("README.md"),
            "round one's payload is under `{}`:\n{screen}",
            label("FIRST-ROUND-PAYLOAD")
        );
        assert!(
            label("SECOND-ROUND-PAYLOAD").contains("TODO.md"),
            "round two's payload is under `{}`:\n{screen}",
            label("SECOND-ROUND-PAYLOAD")
        );
    }

    /// A settled call is **one** row, not two.
    ///
    /// It used to be two: `→ Read TODO.md` from the assistant row, then
    /// `▸ Read TODO.md · ok · 129 lines · ctrl-t` from the result row three lines
    /// below, saying the same thing with an outcome attached. Four calls cost
    /// eight rows of a thirty-four-row terminal before any output was on it.
    #[test]
    fn a_settled_call_is_one_row_and_the_row_is_the_one_with_the_result_on_it() {
        let mut a = app();
        a.apply(ServerFrame::Event(env(1, testing::appended("r.a", "assistant"))));
        a.apply(ServerFrame::Event(env(
            2,
            SessionEvent::TranscriptContent {
                item_id: "r.a".into(),
                item: Box::new(TranscriptItem::Assistant {
                    text: String::new(),
                    tool_calls: vec![letibot_transcript::ToolCall {
                        id: "call_0".into(),
                        name: "read".into(),
                        arguments: r#"{"path":"TODO.md"}"#.into(),
                    }],
                    truncated: false,
                }),
            },
        )));
        let before = a.screen(120, 40).join("\n");
        assert_eq!(
            before.matches("TODO.md").count(),
            1,
            "a call with no result yet is announced exactly once:\n{before}"
        );
        assert!(before.contains("no result"), "and says it has none:\n{before}");

        a.apply(ServerFrame::Event(env(3, testing::appended("r.t", "tool_result"))));
        a.apply(ServerFrame::Event(env(
            4,
            SessionEvent::TranscriptContent {
                item_id: "r.t".into(),
                item: Box::new(TranscriptItem::ToolResult {
                    call_id: "call_0".into(),
                    name: "read".into(),
                    outcome: letibot_transcript::ToolOutcome::Ok,
                    payload: "# rano TODO\n".into(),
                }),
            },
        )));
        let after = a.screen(120, 40).join("\n");
        assert_eq!(
            after.matches("TODO.md").count(),
            1,
            "and once the result lands the proposal does not stay beside it:\n{after}"
        );
        assert!(after.contains("▸ Read TODO.md · ok"), "{after}");
        assert!(
            !after.contains("no result"),
            "a call that returned does not still read as one that did not:\n{after}"
        );
    }

    /// The same claim, with a row above the round so the narrowed invalidation
    /// cannot fall back on "rewind to zero".
    ///
    /// `a_settled_call_is_one_row_…` starts at the assistant row, so its round
    /// head is index 0 and every invalidation in it is a full rebuild — which is
    /// exactly the path that was never narrowed. Put a user row in front and the
    /// rewind is a real one. Found by diffing the byte stream of a replay against
    /// the same replay with the narrowing switched off: 59 frames of 1028 showed
    /// a different screen, and the first of them had `→ Listed * · no result`
    /// sitting above `▸ Listed * · ok`.
    #[test]
    fn a_settled_call_is_one_row_when_the_round_does_not_start_at_row_zero() {
        let mut a = app();
        a.apply(ServerFrame::Event(env(1, testing::appended("u", "user"))));
        a.apply(ServerFrame::Event(env(
            2,
            SessionEvent::TranscriptContent {
                item_id: "u".into(),
                item: Box::new(TranscriptItem::User {
                    parts: vec![UserPart::Text {
                        text: "what is in the tree".into(),
                    }],
                }),
            },
        )));
        // A pane, because `drawn_live` and `superseded` are inputs to the walk and
        // a test without a turn exercises neither. The daemon's order, from
        // `docs/tui-testing.md`: the round's own rows, then the terminal event,
        // then the result rows.
        a.apply(ServerFrame::Event(env(3, testing::turn_started("t1"))));
        a.apply(ServerFrame::Event(env(4, testing::appended("r.a", "assistant"))));
        a.apply(ServerFrame::Event(env(
            5,
            SessionEvent::TranscriptContent {
                item_id: "r.a".into(),
                item: Box::new(TranscriptItem::Assistant {
                    text: String::new(),
                    tool_calls: vec![letibot_transcript::ToolCall {
                        id: "call_0".into(),
                        name: "read".into(),
                        arguments: r#"{"path":"TODO.md"}"#.into(),
                    }],
                    truncated: false,
                }),
            },
        )));
        a.apply(ServerFrame::Event(env(6, testing::turn_finished("t1"))));
        // The walk has to have RENDERED the round before the result lands, or the
        // rewind has nothing to rewind and the bug hides.
        let before = a.screen(120, 40).join("\n");
        assert!(before.contains("no result"), "{before}");

        a.apply(ServerFrame::Event(env(7, testing::appended("r.t", "tool_result"))));
        let _ = a.screen(120, 40);
        a.apply(ServerFrame::Event(env(
            8,
            SessionEvent::TranscriptContent {
                item_id: "r.t".into(),
                item: Box::new(TranscriptItem::ToolResult {
                    call_id: "call_0".into(),
                    name: "read".into(),
                    outcome: letibot_transcript::ToolOutcome::Ok,
                    payload: "# rano TODO\n".into(),
                }),
            },
        )));
        let after = a.screen(120, 40).join("\n");
        assert_eq!(
            after.matches("TODO.md").count(),
            1,
            "the proposal does not stay beside its own answer:\n{after}"
        );
        assert!(
            !after.contains("no result"),
            "a call that returned does not still read as one that did not:\n{after}"
        );
    }

    /// One turn, with all four of its levels on the screen at once.
    ///
    /// The operator's report was that a turn has no shape: *"user message, then a
    /// flat wall of cards"*. What answers it is a step, not a glyph — the
    /// question and the answer at the body's own column, the working one step in
    /// under them.
    #[test]
    fn a_turn_is_a_question_a_step_of_working_and_an_answer_back_at_the_margin() {
        let mut a = app();
        a.apply(ServerFrame::Event(env(1, testing::appended("u", "user"))));
        a.apply(ServerFrame::Event(env(
            2,
            SessionEvent::TranscriptContent {
                item_id: "u".into(),
                item: Box::new(TranscriptItem::User {
                    parts: vec![UserPart::Text {
                        text: "what crates are in this workspace".into(),
                    }],
                }),
            },
        )));
        a.apply(ServerFrame::Event(env(3, testing::appended("t", "tool_result"))));
        a.apply(ServerFrame::Event(env(
            4,
            SessionEvent::TranscriptContent {
                item_id: "t".into(),
                item: Box::new(TranscriptItem::ToolResult {
                    call_id: "call_0".into(),
                    name: "read".into(),
                    outcome: letibot_transcript::ToolOutcome::Ok,
                    payload: "one\ntwo\nthree\n".into(),
                }),
            },
        )));
        a.apply(ServerFrame::Event(env(5, testing::appended("s", "assistant"))));
        a.apply(ServerFrame::Event(env(
            6,
            SessionEvent::TranscriptContent {
                item_id: "s".into(),
                item: Box::new(TranscriptItem::Assistant {
                    text: "There are twelve.".into(),
                    tool_calls: vec![],
                    truncated: false,
                }),
            },
        )));
        let screen = a.screen(120, 40);
        let at = |needle: &str| -> usize {
            let l = screen
                .iter()
                .find(|l| l.contains(needle))
                .unwrap_or_else(|| panic!("{needle} missing:\n{}", screen.join("\n")));
            l.len() - l.trim_start().len()
        };
        let question = at("what crates are in this workspace");
        let working = at("Read (call_0)");
        let answer = at("There are twelve.");
        assert_eq!(question, answer, "the question and the answer share a column");
        assert_eq!(
            working,
            question + card::REASONING_RAIL_WIDTH,
            "and the working is one step in under them:\n{}",
            screen.join("\n")
        );
    }

    /// [`Palette::None`] is not a monochrome theme, it is the `--replay`, pipe and
    /// CI case: **no sequences at all**. The accent glyphs are what survive it, and
    /// they are why the hierarchy above is carried by a step and a word rather than
    /// by a colour.
    #[test]
    fn the_plain_palette_emits_no_escapes_and_keeps_the_glyphs() {
        let mut a = app();
        a.apply(ServerFrame::Event(env(1, testing::appended("u", "user"))));
        a.apply(ServerFrame::Event(env(
            2,
            SessionEvent::TranscriptContent {
                item_id: "u".into(),
                item: Box::new(TranscriptItem::User {
                    parts: vec![UserPart::Text { text: "hello".into() }],
                }),
            },
        )));
        a.apply(ServerFrame::Event(env(3, testing::appended("r", "reasoning"))));
        a.apply(ServerFrame::Event(env(
            4,
            SessionEvent::TranscriptContent {
                item_id: "r".into(),
                item: Box::new(TranscriptItem::Reasoning {
                    text: "working it out".into(),
                    field: letibot_transcript::ReasoningField::ReasoningContent,
                }),
            },
        )));
        a.apply(ServerFrame::Event(env(5, testing::appended("t", "tool_result"))));
        a.apply(ServerFrame::Event(env(
            6,
            SessionEvent::TranscriptContent {
                item_id: "t".into(),
                item: Box::new(TranscriptItem::ToolResult {
                    call_id: "call_0".into(),
                    name: "grep".into(),
                    outcome: letibot_transcript::ToolOutcome::Failed {
                        reason: "no such path".into(),
                    },
                    payload: "a\nb\n".into(),
                }),
            },
        )));
        a.reasoning = Fold::Open;
        let screen = a.screen(120, 40).join("\n");
        assert!(!screen.contains('\x1b'), "{screen:?}");
        for glyph in ["▌", "▸", "┃", "╭", "│", "╰"] {
            assert!(screen.contains(glyph), "{glyph} is missing:\n{screen}");
        }
        assert!(screen.contains("failed"), "and the word survives too:\n{screen}");
        assert!(screen.contains("no such path"), "{screen}");
    }

    /// Size tells you what matters before you read a word — by **attribute**, so
    /// it survives a terminal-native theme, and never by a cube colour.
    #[test]
    fn a_big_result_weighs_more_than_a_small_one_and_costs_no_cube_colour() {
        let mut a = App::new(RenderConfig {
            width: 120,
            color: true,
            ..RenderConfig::default()
        });
        let mut add = |seq: u64, id: &str, n: usize| {
            a.apply(ServerFrame::Event(env(seq, testing::appended(id, "tool_result"))));
            a.apply(ServerFrame::Event(env(
                seq + 1,
                SessionEvent::TranscriptContent {
                    item_id: id.into(),
                    item: Box::new(TranscriptItem::ToolResult {
                        call_id: "call_0".into(),
                        name: "grep".into(),
                        outcome: letibot_transcript::ToolOutcome::Ok,
                        payload: "x\n".repeat(n),
                    }),
                },
            )));
        };
        add(1, "small", 3);
        add(3, "big", 236);
        let screen = a.screen(120, 40);
        let row = |needle: &str| {
            screen
                .iter()
                .find(|l| l.contains(needle))
                .cloned()
                .unwrap_or_else(|| panic!("{needle}: {}", screen.join("\n")))
        };
        assert!(
            row("236 lines").contains("\x1b[1m"),
            "a big result is bold: {:?}",
            row("236 lines")
        );
        assert!(
            !row("3 lines").contains("\x1b[1m"),
            "a small one is not: {:?}",
            row("3 lines")
        );
        let joined = screen.join("\n");
        for cube in ["38;5;", "48;5;", "38;2;"] {
            assert!(!joined.contains(cube), "{cube} is not a theme slot: {joined:?}");
        }
    }

    /// A subject that does not fit takes the shortening, and the outcome does not.
    #[test]
    fn a_subject_too_long_for_the_row_never_pushes_the_outcome_off_it() {
        let mut a = app();
        a.apply(ServerFrame::Event(env(1, testing::appended("t", "tool_result"))));
        a.apply(ServerFrame::Event(env(
            2,
            SessionEvent::TranscriptContent {
                item_id: "t".into(),
                item: Box::new(TranscriptItem::ToolResult {
                    call_id: "call_0".into(),
                    name: "ask_code".into(),
                    outcome: letibot_transcript::ToolOutcome::NotRun {
                        why: "no retrieval backend is attached to this session".into(),
                    },
                    payload: "a\nb\nc\n".into(),
                }),
            },
        )));
        // The subject comes from the round's assistant row; here there is none, so
        // it is the correlation id — long enough to matter once the row narrows.
        let screen = a.screen(60, 24).join("\n");
        assert!(screen.contains("not run"), "{screen}");
        assert!(
            screen.contains("no retrieval backend"),
            "the reason wraps in the body rather than being cut off a header:\n{screen}"
        );
    }

    #[test]
    fn a_path_is_shortened_at_a_separator_and_a_pattern_is_not() {
        assert_eq!(
            ellipsise_left("/home/dead/Projects/letibot/crates/tui", 22),
            "…/letibot/crates/tui",
            "whole segments, and the longest suffix that fits"
        );
        assert_eq!(
            shorten_subject("crates/tui/src/app.rs", 14),
            "…/src/app.rs",
            "a path loses its left"
        );
        assert_eq!(
            shorten_subject("^pub (fn|struct|enum)", 12),
            "^pub (fn|st…",
            "a pattern loses its right — it is read from the start"
        );
        assert_eq!(
            shorten_subject("**/*.{md,json,toml,yaml}", 12),
            "**/*.{md,js…",
            "and so does a glob, slash or no slash"
        );
        assert_eq!(
            shorten_subject("\"what is src/main.rs for\"", 12),
            "\"what is sr…",
            "a quoted sentence is prose with a slash in it, not a path"
        );
        // A single segment with no separator to cut on falls back to characters
        // rather than returning something wider than it was asked for.
        assert!(letibot_ui::width::width(&ellipsise_left("averylongsinglesegment", 10)) <= 10);
    }

    /// The sentence the model writes before its first tool call is on the screen
    /// once, before and after its row lands.
    ///
    /// `TurnPane::text` accumulates every text delta of the whole turn, and prose
    /// is committed to the transcript one round at a time — so once round one's
    /// row had a body, its sentence was in history and still in the pane below the
    /// cards. Measured at 60x34 on the operator's session.
    /// `sudo` wants a password: the card names the command, the keys are the
    /// field's alone, the screen shows dots and never the text, Enter sends it
    /// once as an `Action::Secret`, and the composer's history never had it.
    #[test]
    fn a_password_field_owns_the_keys_shows_dots_and_never_reaches_the_composer() {
        let mut a = app();
        a.apply(ServerFrame::Event(env(
            1,
            SessionEvent::SecretRequested {
                req_id: "secret-s-1".into(),
                prompt: "[sudo] password for dead: ".into(),
                command: "sudo apt install x".into(),
                deadline: 1_000_000,
            },
        )));
        let card = a.screen(100, 20).join("\n");
        assert!(card.contains("sudo wants a password"), "{card}");
        assert!(card.contains("sudo apt install x"), "{card}");
        for c in "hunter2".chars() {
            assert!(a.key(Key::Char(c)).is_none());
        }
        let typing = a.screen(100, 20).join("\n");
        assert!(!typing.contains("hunter2"), "the password is on the screen:\n{typing}");
        assert!(typing.contains("\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}"), "{typing}");
        assert!(a.input().is_empty(), "the composer must never hold it");
        assert!(a.key(Key::Backspace).is_none());
        assert!(a.key(Key::Char('2')).is_none());
        let sent = a.key(Key::Enter);
        assert_eq!(
            sent,
            Some(Action::Secret {
                req_id: "secret-s-1".into(),
                secret: Some("hunter2".into()),
            })
        );
        assert!(a.input().is_empty());
        assert!(a.editor.history().iter().all(|h| !h.contains("hunter2")));
        let after = a.screen(100, 20).join("\n");
        assert!(!after.contains("sudo wants a password"), "{after}");

        a.apply(ServerFrame::Event(env(
            2,
            SessionEvent::SecretRequested {
                req_id: "secret-s-2".into(),
                prompt: "[sudo] password: ".into(),
                command: "sudo true".into(),
                deadline: 1_000_000,
            },
        )));
        assert_eq!(
            a.key(Key::Esc),
            Some(Action::Secret {
                req_id: "secret-s-2".into(),
                secret: None,
            })
        );
    }

    #[test]
    fn a_rounds_prose_moves_into_the_transcript_rather_than_being_copied_into_it() {
        let mut a = app();
        a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
        for w in ["I'll take ", "a look ", "at the tree first."] {
            a.apply(ServerFrame::Event(env(2, testing::delta("t1", w))));
        }
        // Streaming: the pane is the only place it exists, and it is showing.
        let live = a.screen(120, 30).join("\n");
        assert_eq!(
            live.matches("at the tree first.").count(),
            1,
            "{live}"
        );
        a.apply(ServerFrame::Event(env(3, testing::appended("t1.0", "assistant"))));
        a.apply(ServerFrame::Event(env(
            4,
            SessionEvent::TranscriptContent {
                item_id: "t1.0".into(),
                item: Box::new(TranscriptItem::Assistant {
                    text: "I'll take a look at the tree first.".into(),
                    tool_calls: vec![],
                    truncated: false,
                }),
            },
        )));
        // Settled: still once, and now in the transcript where it belongs.
        let settled = a.screen(120, 30).join("\n");
        assert_eq!(
            settled.matches("at the tree first.").count(),
            1,
            "the pane kept a copy of what the transcript took over:\n{settled}"
        );
        // The next round's prose still streams into the pane.
        a.apply(ServerFrame::Event(env(5, testing::delta("t1", "And now the answer."))));
        let next = a.screen(120, 30).join("\n");
        assert!(next.contains("And now the answer."), "{next}");
        assert_eq!(next.matches("at the tree first.").count(), 1, "{next}");
    }

    /// A call the turn was cut short in the middle of does not leave the screen.
    ///
    /// The hazard in "one row per call": while the live pane is drawing a turn,
    /// that turn's assistant rows deliberately draw none of their own unsettled
    /// calls. If the pane then stands down with a call still unanswered, nobody is
    /// drawing it — and the operator is looking at a turn that asked for three
    /// files with no sign it ever did.
    #[test]
    fn a_call_the_turn_was_interrupted_in_the_middle_of_still_says_it_asked() {
        let mut a = app();
        a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
        a.apply(ServerFrame::Event(env(2, testing::appended("t1.0", "assistant"))));
        a.apply(ServerFrame::Event(env(
            3,
            SessionEvent::TranscriptContent {
                item_id: "t1.0".into(),
                item: Box::new(TranscriptItem::Assistant {
                    text: String::new(),
                    tool_calls: vec![letibot_transcript::ToolCall {
                        id: "call_0".into(),
                        name: "read".into(),
                        arguments: r#"{"path":"TODO.md"}"#.into(),
                    }],
                    truncated: false,
                }),
            },
        )));
        // While it is running the pane owns it and the row says nothing.
        assert!(
            !a.screen(120, 24).join("\n").contains("no result"),
            "not while it is still running"
        );
        a.apply(ServerFrame::Event(env(
            4,
            SessionEvent::TurnInterrupted {
                turn_id: "t1".into(),
                reason: "operator pressed esc twice".into(),
                partial_kept: true,
            },
        )));
        let screen = a.screen(120, 24).join("\n");
        assert!(
            screen.contains("→ Read TODO.md · no result"),
            "and once nothing is drawing it, the row does:\n{screen}"
        );
    }

    /// The other half of the same table: a result whose round is not on the screen
    /// borrows nothing. `(call_0)` is a correlation key and reads as one; a
    /// neighbour's path reads as a fact.
    #[test]
    fn a_result_whose_round_the_head_cannot_see_says_so_rather_than_borrowing() {
        let mut a = app();
        a.apply(ServerFrame::Event(env(1, testing::appended("t.0", "tool_result"))));
        a.apply(ServerFrame::Event(env(
            2,
            SessionEvent::TranscriptContent {
                item_id: "t.0".into(),
                item: Box::new(TranscriptItem::ToolResult {
                    call_id: "call_0".into(),
                    name: "read".into(),
                    outcome: letibot_transcript::ToolOutcome::Ok,
                    payload: "hello\n".into(),
                }),
            },
        )));
        let screen = a.screen(120, 24).join("\n");
        assert!(screen.contains("(call_0)"), "{screen}");
    }

    #[test]
    fn a_failed_turn_stops_the_spinner_instead_of_being_a_warning_and_a_hang() {
        // §4.5. Observed live before this event existed: the engine published a
        // `Warning` and no terminal event, `TurnState` stayed `Running`, and the
        // head span at a dead session for as long as anyone left it open.
        let mut a = app();
        a.clock(1_000);
        a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
        assert!(a.turn_running());
        a.apply(ServerFrame::Event(env(
            2,
            SessionEvent::TurnFailed {
                turn_id: "t1".into(),
                error: "http io: Connection refused (os error 111)".into(),
                partial_kept: false,
            },
        )));
        assert!(!a.turn_running(), "the turn is still marked running");
        assert!(a.turn_status(120).is_empty(), "the spinner is still there");
        let screen = a.screen(120, 16).join("\n");
        assert!(screen.contains("FAILED"), "{screen}");
        assert!(screen.contains("Connection refused"), "{screen}");
        // …and the daemon's grep-able warning is not the same sentence a second
        // time three lines away. Filtered, and counted as filtered.
        let before = a.filtered;
        a.apply(ServerFrame::Event(env(
            3,
            SessionEvent::Warning {
                code: "turn_failed".into(),
                detail: "http io: Connection refused (os error 111)".into(),
            },
        )));
        assert_eq!(a.filtered - before, 1);
        assert_eq!(
            a.screen(120, 16).join("\n").matches("Connection refused").count(),
            1,
            "the same failure is on the screen twice"
        );
    }

    #[test]
    fn a_prompt_typed_mid_turn_stays_on_screen_marked_queued() {
        // The complaint this whole queue answers: enter pressed while a turn runs,
        // the hub accepts the prompt as a follow-up user item, and until the step
        // boundary appends it the words were on no part of the screen. Now they
        // wait at the tail of the body, marked, where their row will land.
        let mut a = app();
        a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
        for c in "also bump the retry budget".chars() {
            a.key(Key::Char(c));
        }
        assert!(matches!(a.key(Key::Enter), Some(Action::Prompt(_))));
        assert_eq!(a.input(), "", "the composer handed the words off");
        let screen = a.screen(80, 24).join("\n");
        assert!(
            screen.contains("queued · also bump the retry budget"),
            "{screen}"
        );
    }

    #[test]
    fn the_queued_echo_stands_down_when_the_row_lands() {
        let mut a = app();
        a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
        for c in "also bump the retry budget".chars() {
            a.key(Key::Char(c));
        }
        a.key(Key::Enter);
        assert!(a.screen(80, 24).join("\n").contains("queued ·"));
        // The step boundary appends the follow-up user item: the transcript has
        // taken the words over, so the dim echo must go — one row retires one
        // entry, and the settled row is what remains.
        a.apply(ServerFrame::Event(env(2, testing::appended("u2", "user"))));
        a.apply(ServerFrame::Event(env(
            3,
            SessionEvent::TranscriptContent {
                item_id: "u2".into(),
                item: Box::new(TranscriptItem::User {
                    parts: vec![UserPart::Text {
                        text: "also bump the retry budget".into(),
                    }],
                }),
            },
        )));
        let screen = a.screen(80, 24).join("\n");
        assert!(!screen.contains("queued ·"), "{screen}");
        assert!(
            screen.contains("also bump the retry budget"),
            "the words left with the echo: {screen}"
        );
    }

    #[test]
    fn switching_sessions_leaves_the_queue_behind_and_a_resync_keeps_what_is_still_queued() {
        let mut a = app();
        a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
        for c in "also bump the retry budget".chars() {
            a.key(Key::Char(c));
        }
        a.key(Key::Enter);
        // A switch is a replacement: the queue belongs to the session it was typed
        // at, and the hub drains it into *that* transcript.
        a.apply(hello(
            "s2",
            vec![brief("s2", "other", false)],
            Hub::new("s2").snapshot(),
        ));
        assert!(
            !a.screen(80, 24).join("\n").contains("queued ·"),
            "an echo of another session's queue"
        );
        // Back to the first session. A resync of the *same* session keeps what is
        // still queued — the hub's queue survived — but a transcript that already
        // holds the words stands the echo down, the way the live row would have.
        let hub = Hub::new("s");
        hub.publish(testing::appended("u1", "user"));
        hub.record_item(
            "u1",
            TranscriptItem::User {
                parts: vec![UserPart::Text {
                    text: "also bump the retry budget".into(),
                }],
            },
        );
        a.apply(hello("s", vec![brief("s", "", true)], hub.snapshot()));
        assert!(
            !a.screen(80, 24).join("\n").contains("queued ·"),
            "the snapshot already holds the words"
        );
    }

    #[test]
    fn a_turn_joined_from_a_snapshot_shows_no_elapsed_rather_than_an_epoch() {
        // Found by switching into a session that was mid-turn — the case the whole
        // switch feature exists for. A snapshot has no timestamps, `started_ms` is
        // 0, and `last_ms - 0` is a Unix epoch in milliseconds: the line read
        // `Responding · 496940h16m`.
        let hub = Hub::new("s");
        hub.publish(testing::turn_started("t1"));
        hub.publish(testing::delta("t1", "half an answer"));
        let mut a = app();
        a.apply(ServerFrame::Resync {
            reason: "switch".into(),
            dropped: 0,
            snapshot: Box::new(hub.snapshot()),
            scrubbed: Default::default(),
        });
        a.apply(ServerFrame::Event(env_at(
            99,
            1_788_984_000_000,
            testing::delta("t1", " more"),
        )));
        let line = a.turn_status(120);
        assert!(!line.is_empty(), "the turn is running");
        assert!(line.contains("started before this head attached"), "{line}");
        // No `NNNh` anywhere: that shape is what an epoch renders as.
        let chars: Vec<char> = line.chars().collect();
        assert!(
            !chars.windows(2).any(|w| w[0].is_ascii_digit() && w[1] == 'h'),
            "an epoch rendered as a duration: {line}"
        );
    }

    #[test]
    fn the_session_header_degrades_by_deletion_and_never_wraps() {
        // It handed both halves to `split_row`, which drops the **whole** right one
        // when they do not both fit — correct for the in-flight line and wrong here,
        // where the right half is the part you cannot get anywhere else. Measured
        // under tmux at 110 columns: an 82-column path plus a 27-column tail is 111,
        // and the entire tail vanished with nothing to say it had.
        let mut a = app();
        let hub = Hub::new("s");
        hub.publish(testing::turn_started("t1"));
        hub.publish(SessionEvent::TurnFinished {
            turn_id: "t1".into(),
            finish_reason: letibot_sessionlog::event::FinishReason::Eos,
            usage: Usage {
                prompt_tokens: 41_233,
                cached_tokens: 38_100,
                predicted_tokens: 200,
            },
            timings: Default::default(),
        });
        a.apply(hello(
            "s",
            vec![brief("s", "the cache question", false), brief("s2", "", false)],
            hub.snapshot(),
        ));
        for w in [40usize, 60, 80, 110, 200] {
            let l = a.header_line(w);
            assert!(line_width(&l) <= w, "w={w}: {} cols: {l}", line_width(&l));
            // The session index is the field that survives longest: with several
            // sessions, "which one is this" is the question the header exists for.
            assert!(l.contains("1/2"), "w={w}: {l}");
            if w >= 80 {
                assert!(l.contains("41.2k ctx"), "w={w}: {l}");
            }
        }
    }

    #[test]
    fn the_turns_numbers_live_in_the_header_and_an_ordinary_ending_has_no_footer() {
        // One fact, one place. The footer used to close with `45 tok/s · 12.3s ·
        // 1.2k out` on a line that also repeated the header's context and cache
        // numbers; the duplicates went, the measurements moved up, and an
        // ordinary ending now leaves the body with no footer line at all — a
        // settled fact was occupying the row a live fact used to have to earn.
        let mut a = app();
        a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
        a.apply(ServerFrame::Event(env(
            2,
            SessionEvent::TurnFinished {
                turn_id: "t1".into(),
                finish_reason: letibot_sessionlog::event::FinishReason::Eos,
                usage: Usage {
                    prompt_tokens: 41_233,
                    cached_tokens: 38_100,
                    predicted_tokens: 1_200,
                },
                timings: letibot_sessionlog::event::Timings {
                    prompt_ms: 900.0,
                    predicted_ms: 2_000.0,
                    wall_ms: 12_300,
                },
            },
        )));
        let header = a.header_line(200);
        assert!(header.contains("600 tok/s"), "{header}");
        assert!(header.contains("12.3s"), "{header}");
        assert!(header.contains("1200 out"), "{header}");
        assert!(header.contains("41.2k ctx"), "{header}");
        // An ordinary ending says nothing at all on the body.
        let screen = a.screen(120, 30);
        assert!(
            !screen.iter().any(|l| l.contains("── ")),
            "an ordinary ending has no footer: {screen:?}"
        );
    }

    #[test]
    fn the_decode_line_does_not_repeat_the_prompts_cache_numbers() {
        // While the answer decodes, the header already carries the prompt's size
        // and cache fraction — live prefill numbers win there for the whole
        // turn — so the in-flight line says only what it alone knows.
        let mut a = app();
        a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
        a.apply(ServerFrame::Event(env(
            2,
            SessionEvent::PromptProgress {
                turn_id: "t1".into(),
                progress: letibot_sessionlog::event::PromptProgress {
                    total: 41_233,
                    cache: 38_100,
                    processed: 41_233,
                    time_ms: 900,
                },
            },
        )));
        let line = a.turn_status(120);
        // Nothing has arrived yet, and `0 chars` is a zero field wearing a
        // measurement's clothes: absent, not zero.
        assert!(!line.contains("chars"), "{line}");
        assert!(!line.contains("prompt"), "{line}");
        assert!(!line.contains("cached"), "{line}");
        // The header is where those numbers live instead.
        let header = a.header_line(200);
        assert!(header.contains("41.2k ctx"), "{header}");
        assert!(header.contains("92% cached"), "{header}");
        // Once something has arrived, the count is the one fact this line alone
        // knows — and it is the delta's own count, not a running estimate.
        a.apply(ServerFrame::Event(env(
            3,
            testing::delta("t1", "hello"),
        )));
        let line = a.turn_status(120);
        assert!(line.contains("5 chars"), "{line}");
        // One compact string for the border to pin right — never a justified
        // full-width line, which as a legend put `Responding` at the left edge
        // and clipped the count it was carrying.
        assert!(!line.contains("  "), "no justification padding: {line}");
    }

    #[test]
    fn the_spinner_spins_on_the_heads_clock_not_on_the_daemons_events() {
        // A spinner keyed off the last event's timestamp is not a spinner, it is
        // a snapshot of one: a tool running thirty silent seconds froze it on one
        // glyph, and the frozen duration beside it read as a dead turn. The phase
        // and the duration both run on `now_ms`, which the driver advances every
        // tick whether or not anything arrived.
        let mut a = app();
        a.apply(ServerFrame::Event(env_at(
            1,
            1_000,
            testing::turn_started("t1"),
        )));
        a.clock(1_000);
        let first = a.turn_status(120);
        assert!(first.contains("Responding"), "{first}");
        assert!(first.contains("0ms"), "{first}");
        a.clock(1_160);
        let second = a.turn_status(120);
        assert!(second.contains("160ms"), "the duration ticks: {second}");
        let glyph = |s: &str| s.chars().find(|c| "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏".contains(*c));
        assert_ne!(
            glyph(&first),
            glyph(&second),
            "the glyph moved with no event in between: {first} → {second}"
        );
        // And it is inlaid in the bottom border, pinned right — not a row of its
        // own above the box.
        let screen = a.screen(100, 24);
        let row = screen
            .iter()
            .find(|l| l.contains("Responding"))
            .expect("the turn's status is on the screen");
        assert!(row.contains('╰'), "inlaid in the bottom edge: {row}");
    }

    #[test]
    fn a_running_subagent_is_counted_on_the_top_border_and_a_done_one_is_not() {
        let mut a = app();
        a.apply(ServerFrame::Event(env(
            1,
            SessionEvent::Subagent {
                subagent_id: "s-sub-1".into(),
                state: "running".into(),
                prompt: "summarize ~/bin/letibot".into(),
                role: "coder".into(),
            },
        )));
        let screen = a.screen(100, 24);
        let row = screen
            .iter()
            .find(|l| l.contains("subagent"))
            .expect("the count is on the screen");
        assert!(row.contains("1 subagent running"), "{row}");
        assert!(row.contains('╭'), "pinned to the top edge: {row}");
        a.apply(ServerFrame::Event(env(
            2,
            SessionEvent::Subagent {
                subagent_id: "s-sub-1".into(),
                state: "done".into(),
                prompt: "summarize ~/bin/letibot".into(),
                role: "coder".into(),
            },
        )));
        assert!(
            !a.screen(100, 24).join("\n").contains("subagent running"),
            "a fact that exists only while it does"
        );
    }

    #[test]
    fn a_users_own_message_is_a_block_with_a_bar_and_the_time_it_was_sent() {
        let mut a = app();
        a.apply(ServerFrame::Event(env_at(
            1,
            1_788_984_000_000,
            testing::appended("s.0", "user"),
        )));
        a.apply(ServerFrame::Event(env(
            2,
            testing::content("s.0", "why did the cache miss"),
        )));
        let screen = a.screen(80, 16);
        let row = screen
            .iter()
            .find(|l| l.contains("why did the cache miss"))
            .expect("the prompt is on the screen");
        // The bar is the signal that survives with no colour and survives a
        // copy-paste, which is why it is a glyph and not only a colour.
        assert!(
            row.starts_with(&format!("{}▌", " ".repeat(App::GUTTER))),
            "{row:?}"
        );
        // A wall-clock time, from the log's own clock. Which one depends on the
        // box's zone, so the assertion is on the shape.
        assert!(
            row.split_whitespace().last().is_some_and(|t| t.len() == 8 && t.contains(':')),
            "no timestamp on the row: {row:?}"
        );
        // The block is padded to the full **content** width, or the background
        // stops mid-row and reads as damage. Content width is the terminal less
        // both gutters; the right one is empty by design, and `term::paint` erases
        // it with the row.
        assert_eq!(line_width(row), 80 - App::GUTTER, "{row:?}");
    }

    #[test]
    fn a_row_with_no_timestamp_shows_none_rather_than_the_epoch() {
        // A log recorded before `SnapshotItem::ts` existed replays with zeros, and
        // `01:00:00` would be a measurement nobody took rendered as one they did.
        let mut a = app();
        a.apply(ServerFrame::Event(env(1, testing::appended("s.0", "user"))));
        a.apply(ServerFrame::Event(env(2, testing::content("s.0", "no clock here"))));
        let screen = a.screen(80, 16);
        let row = screen.iter().find(|l| l.contains("no clock here")).unwrap();
        assert!(!row.contains(':'), "a fabricated timestamp: {row:?}");
    }

    #[test]
    fn the_picker_lists_the_sessions_and_a_number_switches_to_one() {
        let mut a = app();
        a.apply(hello(
            "s",
            vec![
                brief("s", "the cache question", true),
                brief("s2", "scratch", false),
            ],
            Hub::new("s").snapshot(),
        ));
        assert_eq!(a.key(Key::CtrlS), Some(Action::ListSessions));
        let screen = a.screen(110, 24).join("\n");
        assert!(screen.contains("the cache question"), "{screen}");
        assert!(screen.contains("scratch"), "{screen}");
        assert!(screen.contains("generating"), "the busy one says so:\n{screen}");
        typed(&mut a, "2");
        assert_eq!(a.key(Key::Enter), Some(Action::Switch("s2".into())));
    }

    #[test]
    fn the_picker_moves_with_arrows_and_enter_takes_the_marked_row() {
        let mut a = app();
        a.apply(hello(
            "s",
            vec![
                brief("s", "one", true),
                brief("s2", "two", false),
                brief("s3", "three", false),
            ],
            Hub::new("s").snapshot(),
        ));
        assert_eq!(a.key(Key::CtrlS), Some(Action::ListSessions));
        // The cursor starts on the session this head is in, so the mark and the
        // bold name are on the same row until an arrow moves it.
        assert_eq!(a.picker_sel, 0);
        let row = a
            .screen(110, 24)
            .into_iter()
            .find(|l| l.contains("one") && l.contains('▸'))
            .unwrap();
        assert!(row.contains('▸'), "the mark is what enter takes: {row}");
        a.key(Key::Down);
        a.key(Key::Down);
        assert_eq!(a.picker_sel, 2);
        let row = a
            .screen(110, 24)
            .into_iter()
            .find(|l| l.contains("three") && l.contains('▸'))
            .unwrap();
        assert!(row.contains('▸'), "the mark moved with the arrows: {row}");
        // Up wraps past the top; Up again wraps in from the bottom.
        a.key(Key::Up);
        a.key(Key::Up);
        assert_eq!(a.picker_sel, 0);
        a.key(Key::Up);
        assert_eq!(a.picker_sel, 2);
        assert_eq!(a.key(Key::Enter), Some(Action::Switch("s3".into())));
    }

    #[test]
    fn a_click_picks_the_row_under_the_pointer_and_enter_still_switches() {
        let mut a = app();
        a.apply(hello(
            "s",
            vec![
                brief("s", "one", true),
                brief("s2", "two", false),
                brief("s3", "three", false),
            ],
            Hub::new("s").snapshot(),
        ));
        assert_eq!(a.key(Key::CtrlS), Some(Action::ListSessions));
        a.screen(110, 24);
        // The session header takes row 0, the picker's title and blank take
        // two more, so the first session row is y=3 — 0-based, the decoder
        // having taken the wire's one off.
        a.key(Key::Click { x: 10, y: 5 });
        assert_eq!(a.picker_sel, 2);
        let row = a
            .screen(110, 24)
            .into_iter()
            .find(|l| l.contains("three") && l.contains('▸'))
            .unwrap();
        assert!(row.contains('▸'), "the mark moved to the clicked row: {row}");
        a.key(Key::Click { x: 0, y: 3 });
        assert_eq!(a.picker_sel, 0);
        // A click into the blank space under the list moves nothing: the row
        // was truncated away, so selecting it would switch to a session
        // nobody saw.
        a.key(Key::Click { x: 4, y: 12 });
        assert_eq!(a.picker_sel, 0);
        // Select and confirm stay two acts: the click only moves the mark.
        a.key(Key::Click { x: 4, y: 5 });
        assert_eq!(a.picker_sel, 2);
        assert_eq!(a.key(Key::Enter), Some(Action::Switch("s3".into())));
    }

    #[test]
    fn tab_completes_a_slash_command_and_more_tabs_cycle_the_matches() {
        let mut a = app();
        a.editor.insert("/se");
        assert_eq!(a.key(Key::Tab), None);
        assert_eq!(a.input(), "/sessions");
        // "/s" matches three commands; the second Tab walks the cycle in table
        // order, and the cycle wraps.
        a.set_composer("/s");
        a.completion = None;
        a.key(Key::Tab);
        assert_eq!(a.input(), "/sessions");
        a.key(Key::Tab);
        assert_eq!(a.input(), "/switch");
        a.key(Key::Tab);
        assert_eq!(a.input(), "/status");
        a.key(Key::Tab);
        assert_eq!(a.input(), "/sessions", "the cycle wraps");
        // A character typed on after a completion kills the cycle: the next
        // Tab matches fresh, and must not clobber what was typed.
        a.set_composer("/switch");
        a.completion = Some(("/sw".into(), vec!["switch"], 0));
        a.editor.insert("i");
        assert_eq!(a.input(), "/switchi");
        a.key(Key::Tab);
        assert_eq!(a.input(), "/switchi", "no match, so nothing changed");
        // A prefix nothing matches is refused where it stands.
        a.set_composer("/zz");
        a.completion = None;
        a.key(Key::Tab);
        assert_eq!(a.input(), "/zz");
        assert!(a.notice.is_some(), "the refusal is said, not silent");
        // The live row above the composer lists the matches while typing.
        a.set_composer("/s");
        a.completion = None;
        let screen = a.screen(110, 24);
        assert!(
            screen.iter().any(|l| l.contains("/sessions") && l.contains("/switch")),
            "the live completions row shows the matches:\n{}",
            screen.join("\n")
        );
    }

    #[test]
    fn an_ambiguous_pick_is_refused_with_the_count_rather_than_resolved() {
        // Switching to the wrong session is not a keystroke you can take back: the
        // prompt you type next lands there.
        let mut a = app();
        a.apply(hello(
            "s",
            vec![
                brief("s", "cache one", false),
                brief("s2", "cache two", false),
                brief("s3", "other", false),
            ],
            Hub::new("s").snapshot(),
        ));
        a.key(Key::CtrlS);
        typed(&mut a, "cache");
        assert_eq!(a.key(Key::Enter), None, "an ambiguous prefix must not switch");
        let screen = a.screen(110, 24).join("\n");
        assert!(screen.contains("2 sessions match"), "{screen}");
    }

    #[test]
    fn switching_replaces_the_previous_sessions_facts_and_does_not_carry_them_over() {
        // Seen under tmux: a brand-new empty session claiming `4470 ctx · 34%
        // cached`, because the head assigned `session_id` before `load` compared
        // the two and so `load` never saw that it had moved.
        let mut a = app();
        let one = Hub::new("s1");
        one.publish(testing::turn_started("t1"));
        one.publish(SessionEvent::TurnFinished {
            turn_id: "t1".into(),
            finish_reason: letibot_sessionlog::event::FinishReason::Eos,
            usage: Usage {
                prompt_tokens: 4_470,
                cached_tokens: 1_500,
                predicted_tokens: 10,
            },
            timings: Default::default(),
        });
        a.apply(hello("s1", vec![brief("s1", "one", false)], one.snapshot()));
        assert!(a.header_line(200).contains("4470 ctx"));

        a.apply(hello(
            "s2",
            vec![brief("s1", "one", false), brief("s2", "two", false)],
            Hub::new("s2").snapshot(),
        ));
        let h = a.header_line(200);
        assert!(!h.contains("4470"), "the old session's token count came along: {h}");
        assert!(h.contains("2/2"), "{h}");
        assert!(a.turn.is_none(), "the old session's turn came along");
    }

    #[test]
    fn the_wheel_scrolls_and_wheeling_back_follows_the_stream() {
        let mut a = app();
        for i in 0..40u64 {
            a.apply(ServerFrame::Event(env(
                i * 2 + 1,
                testing::appended(&format!("s.{i}"), "user"),
            )));
            a.apply(ServerFrame::Event(env(
                i * 2 + 2,
                testing::content(&format!("s.{i}"), &format!("line {i}")),
            )));
        }
        a.screen(80, 24);
        assert_eq!(a.key(Key::WheelUp), None);
        assert_eq!(a.key(Key::WheelUp), None);
        // A notch is three body lines, because one line of body can be two screen
        // rows after wrapping and a notch that moves one wrapped row reads as
        // nothing happened.
        assert_eq!(a.scroll, 6);
        assert_eq!(a.key(Key::WheelDown), None);
        assert_eq!(a.key(Key::WheelDown), None);
        assert_eq!(a.scroll, 0, "wheeling back to the bottom follows the stream");
    }

    #[test]
    fn scrolling_to_the_top_leaves_a_full_screen_rather_than_a_blank_one() {
        // Found by pressing PageUp six times under tmux. The scroll was clamped to
        // `total - 1`, so the top of the history was a one-line window — and since
        // the scrollback banner overwrites the last line of the window, the whole
        // screen went blank with the banner alone on it.
        let mut a = app();
        for i in 0..40u64 {
            a.apply(ServerFrame::Event(env(i * 2 + 1, testing::appended(&format!("s.{i}"), "user"))));
            a.apply(ServerFrame::Event(env(
                i * 2 + 2,
                testing::content(&format!("s.{i}"), &format!("line {i}")),
            )));
        }
        // A frame first: `body_len` is what `PageUp` clamps against and it is only
        // known once something has been laid out.
        a.screen(80, 24);
        for _ in 0..30 {
            a.key(Key::PageUp);
            a.screen(80, 24);
        }
        let screen = a.screen(80, 24);
        let filled = screen.iter().filter(|l| !l.trim().is_empty()).count();
        assert!(
            filled > 6,
            "scrolled to the top and the screen is empty:\n{}",
            screen.join("\n")
        );
        assert!(screen.iter().any(|l| l.contains("line 0")), "{}", screen.join("\n"));
    }

    fn env(seq: u64, event: SessionEvent) -> letibot_sessionlog::event::Envelope {
        env_at(seq, 0, event)
    }

    /// An envelope with a `ts`. The elapsed times a card shows come from the
    /// log's own clock, so a test that wants one has to supply it.
    fn env_at(seq: u64, ts: u64, event: SessionEvent) -> letibot_sessionlog::event::Envelope {
        letibot_sessionlog::event::Envelope {
            session_id: "s".into(),
            seq,
            ts,
            event,
        }
    }

    /// A finished edit call carrying both sides of a small change.
    fn edit_row(edit: Option<letibot_sessionlog::event::ToolEdit>) -> CallRow {
        CallRow {
            call_id: "c1".into(),
            name: "edit".into(),
            target: "a.rs".into(),
            state: CallState::Finished {
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload_digest: "fnv1a:1".into(),
                inline_bytes: 64,
                full_bytes: 64,
                spill: None,
                edit,
            },
            started_ms: 1_000,
            ended_ms: 2_000,
            note: None,
        }
    }

    fn edit_excerpt() -> letibot_sessionlog::event::ToolEdit {
        letibot_sessionlog::event::ToolEdit {
            path: "a.rs".into(),
            created: false,
            before_start: 1,
            after_start: 1,
            before_lines: 2,
            after_lines: 3,
            truncated: false,
            before: "fn a() {}\n".into(),
            after: "fn a() {\n    x();\n}\n".into(),
        }
    }

    fn plain_cfg(width: usize) -> RenderConfig {
        RenderConfig { width, color: false, ..Default::default() }
    }

    #[test]
    fn an_edit_call_renders_the_two_panel_diff_when_it_is_on() {
        let rows = call_card(&edit_row(Some(edit_excerpt())), &plain_cfg(120), 0, Fold::Open, true);
        let joined = rows.join("\n");
        assert!(joined.contains('│'), "two panels with a separator: {joined}");
        assert!(joined.contains('-') && joined.contains("fn a() {}"), "{joined}");
        assert!(joined.contains('+') && joined.contains("x();"), "{joined}");
        // The removed and added first lines share one row — the change reads
        // across — and the two added lines that have no old counterpart get
        // their own rows with an empty left panel.
        assert!(
            rows.iter().any(|r| r.contains('-') && r.contains('+') && r.contains('│')),
            "{joined}"
        );
        assert!(
            rows.iter()
                .filter(|r| r.contains('│'))
                .any(|r| r.split_once('│').unwrap().0.trim().is_empty() && r.contains("x();")),
            "{joined}"
        );
    }

    /// The fallback is a UNIFIED diff, not the byte count. The toggle's own
    /// message promised "unified below 100 columns" while the card drew the
    /// old body there — measured by the operator as *"even unified claude-code
    /// style edit panes are not here"*.
    #[test]
    fn the_diff_toggle_and_a_narrow_pane_both_fall_back_to_unified() {
        let off = call_card(&edit_row(Some(edit_excerpt())), &plain_cfg(120), 0, Fold::Open, false);
        let text = off.join("\n");
        assert!(!text.contains('│'), "switched off: {off:?}");
        assert!(text.contains("+    x();") || text.contains("+x();"), "no unified diff: {off:?}");
        assert!(!text.contains("64 B"), "the byte count came back instead of a diff: {off:?}");

        // opencode's gate: under 100 columns the panels cannot hold code and
        // gutters, so the switch being on is not enough — and the answer is
        // still a diff.
        let narrow = call_card(&edit_row(Some(edit_excerpt())), &plain_cfg(80), 0, Fold::Open, true);
        let text = narrow.join("\n");
        assert!(!text.contains('│'), "narrow pane: {narrow:?}");
        assert!(text.contains("x();"), "narrow pane lost the change: {narrow:?}");
    }

    #[test]
    fn a_created_file_renders_as_all_right_panel_and_a_cap_says_so() {
        let created = letibot_sessionlog::event::ToolEdit {
            created: true,
            before: String::new(),
            before_lines: 0,
            ..edit_excerpt()
        };
        let rows = call_card(&edit_row(Some(created)), &plain_cfg(120), 0, Fold::Open, true);
        let body: Vec<&str> = rows.iter().filter(|r| r.contains('│')).map(String::as_str).collect();
        assert!(!body.is_empty(), "{rows:?}");
        for r in &body {
            let (left, _right) = r.split_once('│').unwrap();
            assert!(left.trim().is_empty(), "created: no left panel: {r:?}");
        }

        let capped = letibot_sessionlog::event::ToolEdit { truncated: true, ..edit_excerpt() };
        let rows = call_card(&edit_row(Some(capped)), &plain_cfg(120), 0, Fold::Open, true);
        assert!(
            rows.iter().any(|r| r.contains("the excerpt was capped")),
            "{rows:?}"
        );
    }

    #[test]
    fn a_non_edit_call_never_grows_a_second_panel() {
        let mut row = edit_row(Some(edit_excerpt()));
        row.name = "grep".into();
        let rows = call_card(&row, &plain_cfg(120), 0, Fold::Open, true);
        assert!(!rows.join("\n").contains('│'), "{rows:?}");
    }

    #[test]
    fn the_live_event_path_feeds_the_two_panel_view() {
        let mut a = app();
        a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
        a.apply(ServerFrame::Event(env(2, testing::proposed("t1", "c1", "edit"))));
        a.apply(ServerFrame::Event(env(
            3,
            SessionEvent::ToolFinished {
                turn_id: "t1".into(),
                call_id: "c1".into(),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload_digest: "fnv1a:1".into(),
                inline_bytes: 64,
                full_bytes: 64,
                spill: None,
                repairs: 0,
                edit: Some(edit_excerpt()),
            },
        )));
        let screen = a.screen(120, 24).join("\n");
        // The diff body, not the byte-count fallback: the added line is on
        // the screen. (The screen always contains `│` — the composer's box —
        // so the separator proves nothing; the code does.)
        assert!(screen.contains("x();"), "{screen}");
        assert!(screen.contains("fn a() {}"), "{screen}");
        // And the toggle the command flips is the one the card reads: off, the
        // same change is drawn as a unified diff — signed, still there.
        a.command("diff");
        let screen = a.screen(120, 24).join("\n");
        assert!(screen.contains("x();"), "{screen}");
        assert!(screen.contains("+"), "{screen}");
        assert!(!screen.contains("64 B"), "the byte count came back: {screen}");
    }

    /// **The bug the operator reported.** The diff was drawn only by the live
    /// card, and the transcript takes a call over the moment its result row
    /// lands — so the diff existed for the gap between `ToolFinished` and
    /// `TranscriptAppended`, which is to say never. The settled row must draw
    /// it, folded and open, and must keep drawing it after the next turn starts.
    #[test]
    fn a_settled_edit_row_draws_the_diff_and_keeps_it_across_turns() {
        let mut a = app();
        a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
        a.apply(ServerFrame::Event(env(2, testing::proposed("t1", "c1", "edit"))));
        a.apply(ServerFrame::Event(env(
            3,
            SessionEvent::ToolFinished {
                turn_id: "t1".into(),
                call_id: "c1".into(),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload_digest: "fnv1a:1".into(),
                inline_bytes: 64,
                full_bytes: 64,
                spill: None,
                repairs: 0,
                edit: Some(edit_excerpt()),
            },
        )));
        // The transcript takes the call over: the row lands, then its body.
        a.apply(ServerFrame::Event(env(4, testing::appended("t1.r1", "tool_result"))));
        a.apply(ServerFrame::Event(env(
            5,
            SessionEvent::TranscriptContent {
                item_id: "t1.r1".into(),
                item: Box::new(TranscriptItem::ToolResult {
                    call_id: "c1".into(),
                    name: "edit".into(),
                    outcome: letibot_transcript::ToolOutcome::Ok,
                    payload: "a.rs: 1 replacement(s). lines 1-3\n\n     1| fn a() {\n     2|     x();\n     3| }\n".into(),
                }),
            },
        )));
        let screen = a.screen(120, 30).join("\n");
        assert!(screen.contains("x();"), "settled row lost the diff:\n{screen}");
        assert!(!screen.contains("1 replacement(s)"), "the tool's prose was drawn instead of the diff:\n{screen}");

        // The next turn takes the pane away; the settled row still has its pair.
        a.apply(ServerFrame::Event(env(6, testing::turn_started("t2"))));
        let screen = a.screen(120, 30).join("\n");
        assert!(screen.contains("x();"), "the diff vanished when the next turn started:\n{screen}");

        // Narrow: unified, and still the change.
        let screen = a.screen(80, 30).join("\n");
        assert!(screen.contains("x();"), "narrow settled row lost the change:\n{screen}");
        // The toggle reaches the settled row too, not only the live pane.
        let split = a.screen(120, 30).join("\n");
        assert!(split.contains('│') && split.contains("1 - fn a() {}"), "{split}");
        a.command("diff");
        let unified = a.screen(120, 30).join("\n");
        assert!(unified.contains("-fn a() {}") || unified.contains("- fn a() {}"), "{unified}");
        assert!(!unified.contains("1 - fn a() {}                                          │"), "still split after /diff:\n{unified}");
        if std::env::var("LETIBOT_SHOW").is_ok() {
            eprintln!("=== 120 unified ===\n{unified}");
        }
    }

    // -- background jobs -----------------------------------------------------

    /// A finished `bash` call that left a job behind: the outcome names the
    /// handle, the way the runtime builds it.
    fn backgrounded_finished(handle: &str, call_id: &str) -> SessionEvent {
        SessionEvent::ToolFinished {
            turn_id: "t1".into(),
            call_id: call_id.into(),
            outcome: letibot_transcript::ToolOutcome::Backgrounded {
                handle: handle.into(),
                ran_for_ms: 0,
                how: letibot_transcript::Backgrounding::Asked,
                next: "job_wait".into(),
            },
            payload_digest: "fnv1a:1".into(),
            inline_bytes: 0,
            full_bytes: 0,
            spill: None,
            repairs: 0,
            edit: None,
        }
    }

    fn proposed_bash(call_id: &str, target: &str) -> SessionEvent {
        SessionEvent::ToolCallProposed {
            turn_id: "t1".into(),
            call_id: call_id.into(),
            name: "bash".into(),
            args_digest: "fnv1a:2".into(),
            target: target.into(),
        }
    }

    #[test]
    fn a_backgrounded_finish_pushes_a_job_row_and_a_settlement_settles_it() {
        let mut a = App::new(plain_cfg(80));
        a.apply(ServerFrame::Event(env(
            1,
            proposed_bash("c1", "\"cargo test --workspace\""),
        )));
        a.apply(ServerFrame::Event(env(2, backgrounded_finished("j1", "c1"))));
        assert_eq!(a.jobs.len(), 1);
        assert_eq!(a.jobs[0].job, "j1");
        assert_eq!(a.jobs[0].call_id, "c1");
        assert_eq!(a.jobs[0].how, "asked");
        assert!(a.jobs[0].state.is_empty(), "no settlement yet: it is running");

        a.apply(ServerFrame::Event(env(
            3,
            SessionEvent::JobSettled {
                job: "j1".into(),
                state: "exited 0".into(),
                produced: 512,
                elapsed_ms: 1_400,
            },
        )));
        assert_eq!(a.jobs.len(), 1, "the settlement folds into the row");
        assert_eq!(a.jobs[0].state, "exited 0");
        assert_eq!(a.jobs[0].produced, 512);
        assert_eq!(a.jobs[0].elapsed_ms, 1_400);
    }

    #[test]
    fn the_jobs_pane_joins_the_command_and_marks_a_running_job() {
        let mut a = App::new(plain_cfg(80));
        // The command joins through the call row, which lives on the turn: a
        // backgrounded call is always mid-turn, and the test is honest about it.
        a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
        a.apply(ServerFrame::Event(env(
            2,
            proposed_bash("c1", "\"cargo test --workspace\""),
        )));
        a.apply(ServerFrame::Event(env(3, backgrounded_finished("j1", "c1"))));
        let lines = a.jobs_lines(100).join("\n");
        assert!(lines.contains("j1"), "{lines}");
        assert!(lines.contains("cargo test --workspace"), "{lines}");
        assert!(lines.contains("running"), "{lines}");
        assert!(lines.contains("asked"), "{lines}");
    }

    #[test]
    fn a_settlement_without_its_start_still_gets_a_row_that_says_what_it_knows() {
        // A head that attaches late replays the durable settlement but not a
        // start that scrolled off. The job ran; the row says so, and says what
        // it does not know rather than inventing it.
        let mut a = App::new(plain_cfg(80));
        a.apply(ServerFrame::Event(env(
            1,
            SessionEvent::JobSettled {
                job: "j9".into(),
                state: "killed by job_kill".into(),
                produced: 0,
                elapsed_ms: 40_000,
            },
        )));
        assert_eq!(a.jobs.len(), 1);
        assert!(a.jobs[0].call_id.is_empty());
        let lines = a.jobs_lines(100).join("\n");
        assert!(lines.contains("j9"), "{lines}");
        assert!(lines.contains("killed by job_kill"), "{lines}");
        assert!(
            lines.contains("not in this head's window"),
            "an unknown command says so: {lines}"
        );
    }

    #[test]
    fn ctrl_q_and_slash_jobs_toggle_the_pane_and_esc_closes_it() {
        let mut a = App::new(plain_cfg(80));
        assert_eq!(a.key(Key::CtrlQ), None);
        assert!(a.jobs_pane);
        assert_eq!(a.key(Key::CtrlQ), None);
        assert!(!a.jobs_pane);

        a.command("jobs");
        assert!(a.jobs_pane);
        a.key(Key::Esc);
        assert!(!a.jobs_pane, "esc closes the pane like the other screens");
    }
}
