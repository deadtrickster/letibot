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

use letibot_sessionlog::event::{Timings, Usage};
use letibot_sessionlog::registry::{SessionBrief, SessionWiring};
use letibot_sessionlog::view::{CallState, OpenDecision, SettledDecision, SnapshotItem, TurnState};

use letibot_ui::card;
use letibot_ui::editor::Editor;

use crate::markdown::IncrementalMarkdown;
use crate::render::{BlockCache, RenderConfig, visible_width};
use crate::ui::*;

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
        /// **What to tell the model**, typed after `deny_and_tell`. The option's
        /// label promised this and nothing carried it.
        note: Option<String>,
    },
    /// **Answer a `question`** (§1.7), which is a different frame from `Answer` for a
    /// reason this tree already records: `Answer` grants or denies a **permission**,
    /// whose failure mode is *something runs*, while a question answers *which way
    /// should I go*, whose failure mode is *a person is quoted as saying something
    /// they did not*. So the two vocabularies stay two frames and two types, and this
    /// is the head's half of the one that had no sender.
    ///
    /// The head could not answer a question **at all** before this: `answer_marked`
    /// returned `None` for an empty `options`, a question always carries
    /// `options: []` with the model's offered choices in `choices`, and nothing in
    /// this crate constructed `ClientFrame::AnswerQuestion`. A question rendered as a
    /// headline over an empty ladder and *"this ask offers no options — your line is
    /// held"*.
    AnswerQuestion {
        req_id: String,
        answer: letibot_sessionlog::question::QuestionAnswer,
    },
    Resync,
    /// Ask the daemon what sessions it holds.
    ListSessions,
    /// Ask for this session's todo list — the pane's bootstrap read.
    ListTodos,
    /// **The operator's half of the board, replaced wholesale** — R51 item 18's write half, and
    /// the frame that had no sender until now.
    ///
    /// The daemon stores and serves these rows and hands them to the model as part of one union;
    /// what it cannot do is invent one, because the words are the operator's. So the head owns them
    /// and sends the whole list on every change, which is the shape `TodoBoard::set_operator`
    /// documents: *"a delta protocol for a list of tens of items would be a second source of truth
    /// about them."*
    SetOperatorTodos(Vec<letibot_sessionlog::event::TodoEntry>),
    /// Ask the daemon for its job table. The head renders the answer; it does
    /// not decide what is in it.
    ListJobs,
    /// **Ask for the merge queue, whole.** The queue pane's bootstrap read, and
    /// daemon-level: there is one `main` and one queue, so the answer is not scoped to this
    /// session — see `ClientFrame::ListMergeQueue`.
    ListMergeQueue,
    /// Make one. The head switches to it when the daemon says which id it minted;
    /// see [`App::apply`]'s `Sessions` arm.
    NewSession(String),
    /// Move this connection to another session.
    Switch(String),
    /// Read a subagent's output without leaving this session: the daemon answers
    /// with `Peeked`, and the pane the tree's read key opens is built from it —
    /// `p` on a subagent row, and `/peek ID` for the typed spelling.
    /// Lazy — nothing is read until this is sent.
    Peek(String),
    /// Read one background job's output into a pane, without leaving this session
    /// and without posting a slash to the conversation. Answered with a
    /// `SessionEvent::JobOutput` on the log, and the pane is built from it. The
    /// jobs pane's Enter sends this instead of `/job ID` — the operator asked for
    /// exactly that: *"when i press enter on jobs pane im not shown the job output
    /// im brought back to the main conversation with /job <id> posted - this is not
    /// what i want"*.
    ReadJobOutput {
        job: String,
        /// Where the window starts. 0 is the first byte; the pane pages by asking
        /// at the offset the previous answer named.
        offset: u64,
    },
    /// Ask the daemon for the settings this session runs under (`/config`).
    Settings,
    /// Bring a session that is in the store but not in this daemon back to life.
    /// The head switches to it on the same `Sessions` reply a `NewSession` produces.
    ResumeSession(String),
    /// Name a session, or clear its name with an empty title.
    Rename {
        session_id: String,
        title: String,
    },
    /// Compact the session this head is in: one summary turn, then the history
    /// is replaced by that summary through a transcript fork.
    Compact,
    /// Rebuild this conversation's prompt from the tools seated now, forking onto
    /// it. The only thing that changes a live session's tool list.
    Reseat {
        /// Summarise as well, replacing the conversation. The default carries it.
        summarise: bool,
    },
    /// Move this session's project to a named point, persisted by the daemon.
    /// See `D13`.
    /// `consented` is the operator's answer to the unconfined-`allow-all`
    /// confirmation. False for every other point, and false for `allow-all`
    /// until they say yes — the daemon reads it only where it matters and treats
    /// a missing answer as no.
    Mode {
        name: String,
        consented: bool,
    },
    /// Take back every prompt this head queued that the running turn has not
    /// consumed yet — the companion of a recall: Up pulled the queued line into
    /// the composer to edit it, and the original must not land behind the edit.
    WithdrawPrompts,
    /// A command the daemon handles: `flowy …`, `models …`. The line minus `/`.
    Slash {
        line: String,
    },
    /// **The operator's own tool call through the door** — R24 part two, R31, R34.
    ///
    /// `name` is the TOOL's spelling (`web_search`), never the typed verb; the hyphen is a
    /// keyboard transform and this is the wire. `arguments` is the JSON object the head
    /// built from the field the daemon published, and the head knows nothing else about the
    /// tool — see [`App::head_run_call`].
    ///
    /// **`execute` is true and there is no result to send back.** This head has no tool
    /// runtime and no HTTP client, so the daemon runs it, in this session, with the byte
    /// caps and spill policy a model's call gets. That is not a convenience: a head that
    /// fetched a page itself would write a corpus row saying *the operator ran `web_fetch`*
    /// about another program's answer. See `ClientFrame::OperatorCall::execute`.
    HeadRun {
        name: String,
        arguments: String,
    },
    /// **The operator's own shell line** — a submitted line that began with `!`.
    ///
    /// The line travels verbatim, bang included: the daemon strips it (one rule, at the
    /// execution site) and the row the operator gets back is the words they typed. The
    /// daemon runs it in this session's workspace through the `bash` path — confine, sudo
    /// askpass and byte caps identical to a model's call — and appends two rows: this
    /// line as the operator's own, and the output as a `bash` result the head folds and
    /// pages like every other tool row. See [`App::submit`]'s `!` arm for the typing
    /// surface and `ClientFrame::OperatorShell` for the wire.
    OperatorShell {
        line: String,
    },
    /// **`!term <command>` — open the pane and run a screen program in it.**
    ///
    /// `line` travels verbatim, `!term` first, exactly as [`Action::OperatorShell`]'s does: the
    /// daemon strips the verb (one rule, at the execution site) and the head has already
    /// checked at the composer that there *is* a command after it — see [`App::submit`].
    ///
    /// **The rectangle is not on this action and cannot be**: it is the terminal's, and the
    /// driver is the thing that knows it. `Link::tick` sends the frame with the size it was
    /// handed, which is also what makes a pane opened after a resize open at the right size
    /// rather than at the default of 80×24.
    TermOpen {
        line: String,
    },
    /// **The operator's keys, verbatim, to the pane's program.** Bytes and not a `Key`: the
    /// pane is a terminal and the head is not the thing that reads it — see
    /// [`ClientFrame::TermInput`](letibot_sessionlog::protocol::ClientFrame::TermInput) for why
    /// a decoded-and-re-encoded arrow would be a different byte string to a program that asked
    /// for the application-cursor spelling.
    TermInput {
        bytes: Vec<u8>,
    },
    /// **The pane's rectangle moved.** The head's fact — the daemon has no screen — and the
    /// only way the program is told the size it is being drawn at.
    TermResize {
        cols: usize,
        rows: usize,
    },
    /// **End the pane.** The head's own act, and it is sent **only after the operator has
    /// confirmed it** — the confirmation card is [`App::term_ask`], and the verb that raises it
    /// is `!term close`.
    ///
    /// **`ctrl-\` no longer sends this.** It detaches — the rectangle goes, the conversation
    /// comes back, and **nothing is sent at all** — which is the operator's own correction:
    /// *"but i dont want it to exit"*. Two acts, one frame, and the destructive one is the one
    /// that has to be spelled out. See [`TermPane`].
    TermClose,
    /// **Ask what this session's pane is running.** The answer is
    /// `ServerFrame::TermStatus`, and this is the read that lets a head which is **not drawing**
    /// the pane say that something is running in it — **without a transcript row**, because a
    /// detach is not an event. See [`App::term_fact`].
    ///
    /// Asked on attach (a head that has just switched sessions has no pane of its own and the
    /// pane is the session's), and asked before a `!term close` the head cannot answer from what
    /// it holds.
    TermStatus,
    /// **Ask the model to propose `!` completions for a prefix** — the smart half of
    /// the `!` completion. The history is this head's own and is the first answer;
    /// this is asked for only when the history has no match for the prefix (or its
    /// cycle is exhausted).
    ///
    /// `client_request_id` is minted here, by the head, because the answer comes back
    /// on the pump and the head has to recognise it: the id is the correlation, and a
    /// head that could not tell one answer from another would cache a suggestion under
    /// the wrong prefix. The daemon echoes it back in `ShellSuggestions`.
    ///
    /// **Nothing here submits.** The answer is a list of candidate lines for the
    /// composer, drawn as candidates with their provenance, and Enter is still the
    /// operator's.
    SuggestShell {
        prefix: String,
        client_request_id: String,
    },
    /// A password for `sudo`, or a refusal. Never logged by anything on the way.
    Secret {
        req_id: String,
        secret: Option<String>,
    },
    /// **The operator's answer to a command of their own that asked them something.**
    ///
    /// The card is raised by the daemon when the run is **blocked reading the device the
    /// daemon holds for it** — a reading of the process and not of its words, see
    /// `letibot_tools::exec::ask` — and this is the line the person typed into it.
    ///
    /// **Not `Action::Secret` and not a path to one.** A password has its own card, its own
    /// masked field and its own frame, and the two are separate variants on purpose: this one
    /// is drawn in the open and what it carries is a line for a program's stdin. See
    /// [`App::prompt_lines`].
    PromptAnswer {
        req_id: String,
        line: String,
    },
    /// **One line to the running command, on demand** — the `!send` verb.
    ///
    /// The manual floor under the card: a person watching the stream can answer whether or
    /// not anything looked like a question, so this needs no card, no request id and no
    /// signal at all. It addresses *whatever operator command this session is running right
    /// now*, which the daemon knows and this head does not.
    SendLine {
        line: String,
    },
    Quit,
    /// Leave AND stop the daemon. The head detaches after the daemon has been
    /// asked, so the notice reaches every other head first.
    StopDaemon,
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
    /// The terminal window gained focus (`?1004`, see `crate::features`).
    FocusIn,
    /// The terminal window lost focus.
    FocusOut,
    /// The terminal's answer about its background colour (OSC 11).
    Background {
        light: bool,
    },
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
    /// **Open the rest of the newest long tool result**, or close that window.
    ///
    /// One row, and not the conversation: the whole-conversation unfold is the verb
    /// `/t`. See the `CtrlV` arm in `App::key` for the ruling. **R56 moved this off
    /// `ctrl-t`**, which is now the todos pane; `v` for *view* is the mnemonic it never
    /// had, and `0x16` had no arm at all before this — see `term.rs`.
    CtrlV,
    /// Show or hide the raw, unparsed text of tool calls.
    CtrlX,
    /// Repaint from scratch.
    CtrlL,
    /// Open or close the session picker.
    CtrlS,
    /// Open or close the todos pane: the session's plan and the repo's queue.
    ///
    /// **R56 moved this off `ctrl-p`** (which is now the hold) and onto leticl's own key,
    /// so one operator learns one chord for one pane — see [`Self::CtrlP`].
    CtrlT,
    /// **Hold the view** (R56): while held, the head writes nothing at all, so a mouse
    /// selection survives a streaming turn. The reader is the only party who can know a
    /// selection exists — the terminal does not forward a Shift-drag — so the reader, not
    /// the head, decides when to stop painting. See [`App::toggle_hold`] for the contract.
    CtrlP,
    /// Open or close the subagent tree: the subagents this session spawned.
    CtrlG,
    /// Move the running command to the background (Ctrl+O, like Claude Code's
    /// Ctrl+B — B is the readline left-arrow here).
    CtrlO,
    /// Open or close the background-jobs pane: the jobs this session started.
    CtrlQ,
    /// **Retire every note this head is holding** (R22).
    ///
    /// R10 gave the reader the power to retire a note and spelled it `/notes dismiss all` —
    /// *the right power in the wrong hand*: the thing you do to clear your own screen is a
    /// **reflex, not a sentence**, and every other reflex on this screen is already a chord.
    /// The operator, being told how to hide a note: *"typing `/notes dismiss all` is not
    /// humane."*
    ///
    /// **ALL of them, and it is one decision made in `head-parity-2026-09-21.md` §R22 with the
    /// other head rather than here** — a reflex that does different things on two screens is
    /// worse than the verb it replaces. The argument for all over newest is in that section
    /// and in the answer beside the proposal; the short form is that `/notes dismiss N` is
    /// where a *deliberate* single retire belongs (it numbers the notes, so the operator can
    /// see which is which), and that over-clearing is one verb to undo while under-clearing
    /// cannot be undone by a chord at all.
    ///
    /// **What it keeps from R10, because the reason has not changed:** retired is not deleted.
    /// The note stays in `notes`, `/notes` prints it in full, `/status` counts it, and
    /// `/notes restore` brings it back. A head that can silently drop a warning is a head whose
    /// warnings cannot be trusted to be complete.
    CtrlN,
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
    Click {
        x: u16,
        y: u16,
    },
}

impl Key {
    /// The composer's key, when this is one of its.
    pub(crate) fn composer(&self) -> Option<letibot_ui::editor::Key> {
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
            | Key::CtrlV
            | Key::CtrlX
            | Key::CtrlL
            | Key::CtrlS
            | Key::CtrlP
            | Key::CtrlG
            | Key::CtrlO
            | Key::CtrlQ
            | Key::CtrlN
            | Key::PageUp
            | Key::PageDown
            | Key::WheelUp
            | Key::WheelDown
            | Key::Tab
            | Key::Click { .. }
            | Key::FocusIn
            | Key::FocusOut
            | Key::Background { .. } => {
                return None;
            }
        })
    }

    /// **The switch this key is the chord of, read from [`Show::chord`]** — the reverse lookup the
    /// key dispatch uses, so a chord is advertised and acts from ONE entry rather than from a pair
    /// of hand-written arms that each spelled their key twice. A key no switch has a chord for, and
    /// a switch whose chord no longer matches, both come out of here as `None`, which is what makes
    /// the drift impossible instead of merely unlikely.
    pub(crate) fn show(&self) -> Option<Show> {
        Show::ALL
            .into_iter()
            .find(|s| s.chord().is_some_and(|(_, k)| &k == self))
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
    pub(crate) fn flip(self) -> Fold {
        match self {
            Fold::Folded => Fold::Open,
            Fold::Open => Fold::Folded,
        }
    }

    pub(crate) fn is_open(self) -> bool {
        self == Fold::Open
    }
}

/// **How long a status notice stays on the screen** — in wall-clock milliseconds.
///
/// **1600 is the number that was already in effect, measured rather than chosen.** The
/// old countdown was 60 *frames*, and on the live head that was 1.6 s of wall time (about
/// 38 loop passes a second on an idle screen). So this changes the **unit** and not the
/// behaviour: the same sentence stays for the same second and a half, on a busy screen as
/// on a quiet one.
///
/// **Why the unit matters.** A TTL counted in frames is a timer that stops when the
/// frames stop — which is exactly when a notice is left standing longest. It was six
/// seconds on a head woken ten times a second, instant under `--replay`, and — because
/// the old body was guarded on a positive count — *permanent* for a notice whose count
/// had already reached zero. That is the shape R13 already fixed once for the elapsed
/// time of a running call: the clock belongs to the wall, not to the render loop.
///
/// leticl's `+notice-ttl-ms+` is the same 1600, reached the same way from the same 60
/// frames; see its `chrome.lisp` for the measurement. The two heads must not drift here,
/// because the operator reads the same sentence for the same length of time on both.
pub const NOTICE_MS: u64 = 1_600;

/// line. See `crates/ui/DESIGN.md` §2.3.
#[derive(Debug, Clone)]
pub(crate) struct CallRow {
    pub(crate) call_id: String,
    pub(crate) name: String,
    /// The §4.1 display target: the path, pattern or command line the call is
    /// about. Empty when the event carried none — a log recorded before the field
    /// existed, or a call first seen as `ToolStarted` — and the card then renders
    /// the verb alone rather than a guess.
    pub(crate) target: String,
    pub(crate) state: CallState,
    /// `Envelope::ts` of the proposal or the start, and of the finish. Zero means
    /// this call came out of a snapshot, which has no timestamps — and a duration
    /// invented from a clock the events were not measured against is worse than
    /// no duration, so that case renders as `card::Phase::Replayed`.
    pub(crate) started_ms: u64,
    /// **The head's own clock when this call started, or 0 when the head had no
    /// clock then** (R13).
    ///
    /// `started_ms` above is the *log's* clock — the `ts` the daemon stamped — and
    /// for as long as a call runs, that number **stops**: no event means no new
    /// timestamp, so the elapsed read off it sat at `0ms` for the whole of a
    /// `cargo build` while the spinner two rows below it turned. The two clocks are
    /// the same clock on this machine, so which one is used only matters for a head
    /// that was never told the time — and the anchor is taken **at the moment the
    /// call starts** rather than at render time for exactly that reason: a `--replay`
    /// applies every frame before it ever sets a clock (`bin/letibot-tui.rs`: the
    /// envelopes are applied in a loop, and `app.clock` is called only in the
    /// interactive loop after them), so a replayed call has no anchor and renders
    /// from the log's own span, which is the only honest measurement there.
    pub(crate) started_at: u64,
    pub(crate) ended_ms: u64,
    /// The most recent `ToolProgress { note }`.
    pub(crate) note: Option<String>,
    /// The settled decision this call was gated by, when there was one. Rendered on
    /// the card in the dim register — the approval is a fact about the call, not a
    /// stray note — and expanded to the brief the oracle saw and its reply.
    pub(crate) decision: Option<SettledDecision>,
}

/// **The way out of a pane: `Ctrl-\`.** See [`TermPane`] for why this byte and not `Esc`.
///
/// It is looked for on the raw byte stream, before anything is forwarded, so the program never
/// receives it — see [`App::pane_keys`], and `Link::tick`, which looks for it on every read and
/// not only on the reads where the pane was already open. `0x1c` is `FS` in ASCII and `QUIT`
/// only under `ISIG`, which raw mode clears; it is one of the three bytes `term.rs`'s decoder
/// has no arm for, and its own comment says so.
///
/// `pub` because the interception is the driver's as much as the app's: the byte is a fact
/// about the stream, and `Link::tick` is where the stream is routed.
pub const WAY_OUT: u8 = 0x1c;

/// **The pane: a program that owns the screen, drawn in the conversation's rectangle.**
///
/// # What it is, and the two things it is not
///
/// It is [`letibot_vt::Screen`] — a rectangle of cells, a cursor, a pen and an alternate
/// buffer, driven by the bytes a pty's far end wrote — plus the three facts a head needs about
/// the program that is drawing in it. **It is not an emulator of this head's own**: there is one
/// in `letibot-vt`, it is a crate *below* this one, and this head's half of it is
/// `letibot_ui::ansi::pane_rows`. And it
/// is not the conversation: nothing here is a transcript row, and when the pane closes the
/// transcript is exactly what it was.
///
/// # The rectangle is the contract
///
/// [`TermPane::rows`] is `letibot_ui::ansi::pane_rows(&mut screen, cols, room, palette)` and it
/// returns **exactly `room` rows** — the same property `ansi.rs` keeps for the pane it was written for. That is the
/// whole of *"the composer, header and status keep their rows"*: the pane takes the
/// conversation's rectangle and gives it back, so nothing above it moves by a line when it
/// opens and nothing below it loses a row it had.
///
/// # The way out, and why it is `ctrl-\`
///
/// **`Ctrl-\` (0x1c), and it is intercepted on the raw byte stream before a single byte is
/// forwarded**, so the program never receives it and cannot trap it — which is the whole
/// requirement. The tree chose it long before this branch: `term.rs`'s own decoder lists the
/// bytes with no arm and says *"`0x1c`-`0x1e` are the only bytes left in this table with no
/// arm, and none of the three has a mnemonic worth having"*. It is not a tty control character
/// (`cfmakeraw` clears `IXON`/`IEXTEN`, and `0x1c` is `QUIT` only under `ISIG`, which raw mode
/// clears), it is not a chord this head binds, and it is not one a program expects to be
/// typed at it.
///
/// **Esc was the other candidate and it is wrong**: Esc is a key `vi`, `mc`, `nano` and every
/// `less` read on purpose — it is *cancel*, it is the first byte of every meta sequence, and a
/// pane that ate it would be a pane the program could not be driven from. `Ctrl-\` is the one
/// key whose whole meaning is *stop this*, and the operator asked for exactly that: *"one
/// unambiguous way out."*
///
/// # What `ctrl-\` does, and what it deliberately does not
///
/// **It detaches. It does not end anything.** The rectangle goes, the conversation comes back,
/// the composer has its rows and its keys — and **nothing is sent at all**: the program keeps
/// running on the daemon's pty, the daemon keeps the screen it has been keeping since protocol
/// 32, the slot stays occupied, and a later `!term` attaches back to the same run.
///
/// **That is a correction, and the operator's words are the reason:** *"but i dont want it to
/// exit"*. This key used to send [`Action::TermClose`], which ends the pane's cgroup — so
/// leaving `nano` killed it, and the attach work bought nothing for anything a person cares
/// about. **The default is the non-destructive act on purpose**: a person reaching for *get me
/// out of here* must not lose an hour's editing, and a program that must be ended can be asked
/// for by name.
///
/// **Ending is `!term close`**, typed at the composer (so it works whether the pane is on the
/// screen or not), and it is confirmed first — see [`App::term_ask`]. The two acts are as
/// different as they can be: one sends nothing and keeps everything, the other sends one frame
/// and kills a process tree, and neither is reachable by the other's key.
pub(crate) struct TermPane {
    /// The program's screen, fed the daemon's bytes and asked for rows.
    pub(crate) screen: letibot_vt::Screen,
    /// The line the operator submitted, verb included — kept for the one sentence this head
    /// says when the pane ends, so *what ended* is not a mystery.
    pub(crate) line: String,
    /// **The rectangle last sent to the daemon.** A resize is a frame, and a frame per tick
    /// would be a frame per keystroke — this is what makes `TermResize` fire on a change and
    /// not on a redraw.
    pub(crate) sent: (usize, usize),
    /// **The operator has left and an ending is on its way.** Set when the head sends
    /// [`Action::TermClose`] — the confirmed `!term close` — and it stops the keys: between the
    /// frame and the daemon's `TermEnded` there is a kill in flight, and a byte written into a
    /// pty whose program is being signalled is a byte nobody will read. The window is
    /// milliseconds, and this is what makes it closed rather than merely short.
    ///
    /// **Not the detach flag.** Leaving (`ctrl-\`) sends nothing and waits for nothing — see
    /// [`TermPane::detached`].
    pub(crate) closing: bool,
    /// **The operator left with `ctrl-\`, and the program is still running.**
    ///
    /// The rectangle is not drawn, the composer has its rows and its keys back — and the pane
    /// is **kept**: the bytes keep arriving into this screen, and the ending, whenever it comes,
    /// is filed as the row it would have been had the operator been looking. That is the whole
    /// of *detaching is not ending*, and it is why [`App::detach`] drops nothing.
    pub(crate) detached: bool,
}

impl TermPane {
    pub(crate) fn new(line: &str, cols: usize, rows: usize) -> TermPane {
        TermPane {
            screen: letibot_vt::Screen::new(rows, cols),
            line: line.to_string(),
            sent: (cols, rows),
            closing: false,
            detached: false,
        }
    }

    /// **The pane's rows for this frame** — exactly `room` of them, which is the property the
    /// composer's own row budget depends on. See the type's note.
    pub(crate) fn rows(
        &mut self,
        cols: usize,
        room: usize,
        palette: letibot_ui::style::Palette,
    ) -> Vec<String> {
        letibot_ui::ansi::pane_rows(&mut self.screen, cols, room, palette)
    }

    /// **The last rows the program left on the screen**, for the row its ending becomes.
    ///
    /// **The screen and not the byte stream**: what a program *printed* is what a person can
    /// read, and the bytes behind it are cursor addressing, `\r` and half a UTF-8 character —
    /// the raw stream is for [`letibot_vt::Screen`] and this is for a transcript row.
    ///
    /// **Blank rows are dropped and the tail is kept**, because the two shapes are different
    /// and the same rule answers both: a program that dies with a sentence about why put that
    /// sentence last (and its rectangle is otherwise empty), and a full-screen program leaves
    /// a grid of mostly blanks whose last rows are where its status line is. Capped at
    /// [`PANE_LAST_LINES`], which is what makes an unfolded note safe to draw.
    pub(crate) fn last_rows(&self) -> Vec<String> {
        let (rows, _) = self.screen.size();
        let mut said: Vec<String> = (0..rows)
            .map(|r| self.screen.line(r))
            .map(|l| l.trim_end().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        if said.len() > PANE_LAST_LINES {
            said.drain(..said.len() - PANE_LAST_LINES);
        }
        said
    }
}

#[derive(Debug, Default)]
pub(crate) struct TurnPane {
    pub(crate) turn_id: String,
    pub(crate) model: String,
    pub(crate) text: IncrementalMarkdown,
    pub(crate) reasoning: IncrementalMarkdown,
    pub(crate) text_cache: BlockCache,
    pub(crate) reasoning_cache: BlockCache,
    pub(crate) calls: Vec<CallRow>,
    /// The raw `<function=…>` markup of this turn's tool calls, as it arrives on
    /// `DeltaTarget::ToolCall`.
    ///
    /// Kept, never shown by default. The default view shows a pending affordance
    /// while it is being written and the settled card afterwards; this is what the
    /// raw chord reveals, and it is the reason the chord can tell the truth
    /// instead of re-deriving markup the head never saw.
    pub(crate) raw_call: String,
    /// True between `<tool_call>` and the `ToolCallProposed` that settles it.
    ///
    /// A separate fact from `raw_call.is_empty()`: a turn that has written one call
    /// and is now writing prose has a non-empty `raw_call` and is not in a call.
    pub(crate) writing_call: bool,
    pub(crate) progress: Option<letibot_sessionlog::event::PromptProgress>,
    pub(crate) state: Option<TurnState>,
    /// Transcript rows appended while this turn ran.
    ///
    /// The pane is the *live* view of a turn. Once the turn has ended and those
    /// rows carry their content, the transcript is authoritative and the pane is a
    /// duplicate of it — so the pane stands down and only its summary line
    /// survives. Without this the answer is on the screen twice, once in the wrong
    /// order, which is what the first run of `--demo` showed.
    pub(crate) appended: Vec<String>,
    /// **Every row this TURN has produced, across all of its rounds** — and the difference from
    /// [`TurnPane::appended`] is the whole of a defect the operator caught twice.
    ///
    /// `appended` is per ROUND, because the daemon re-emits `TurnStarted` for every round of one
    /// prompt (`run_turn_steered` does `turn_seq += 1` inside the round loop) and each one builds a
    /// fresh `TurnPane`. That is right for what `appended` is used for — which rows the live pane is
    /// still drawing, and whether the turn has been superseded — and it was silently wrong for the
    /// one question asked with it: *does this run hold a row of the current turn.*
    ///
    /// Their screen, and their question:
    ///
    /// ```text
    ///   ▌ their message
    ///                    ← blank
    ///   [2 tool calls, 28 thinking lines]      ← the walk's marker, on the run of committed rows
    ///   [8 thinking lines]                     ← the pane's marker, for the same turn
    ///   ⠇ Responding · 1m42s
    /// ```
    ///
    /// *"ok, still not here - [2 tool calls] empty line [N thinking lines]. why not [2 tool calls, N
    /// thinking lines] on a single row?"* **Because round 3 forgot what rounds 1 and 2 did.** The run
    /// of committed rows stopped reading as this turn's, so `live_here` and `walk_carried_live` both
    /// answered *no*, the in-flight reasoning was not folded into the run, and the pane drew it
    /// beside the run instead — two markers, and two different thinking counts.
    ///
    /// One turn is one `began_ms` (stamped once per prompt and carried on every round's
    /// `TurnStarted`), so that is what this is carried on. **Cleared only when the turn is PROVEN
    /// to have changed** — a `began_ms` that differs from the one this pane is counting from. A
    /// boundary that carries no `began_ms` at all is *nobody measured this one* and not *a new
    /// prompt*: read as the latter it emptied this field, the run stopped reading as the turn's,
    /// and one run's work was drawn by two markers again (see the `TurnStarted` arm for the frame
    /// that made it reachable, and for the second `Responding` it manufactured).
    pub(crate) turn_rows: Vec<String>,
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
    ///
    /// **And the mark is not the pane's alone.** Three readers ask which calls are still the live
    /// one's, and they have to agree: the live cards drawn below (`calls.get(*settled_calls..)`),
    /// the marker's NUMBER (`live_work`), and — since the stuck yellow — the marker's COLOUR, which
    /// asks which of the calls still executing have no result row yet ([`round_answered`]). A call
    /// the transcript has taken over is not executing, and a colour that kept asking the whole pane
    /// stayed yellow for the rest of the session on a call whose `ToolFinished` this head never
    /// received.
    pub(crate) settled_calls: usize,
    /// **How much of `reasoning` has already landed as a row** — bytes of `reasoning.raw()`.
    ///
    /// The mark `settled_calls` is for calls, and it is here for the reason that one exists: work
    /// the transcript has taken over must not be counted a second time by the live pane.
    ///
    /// **Its absence is a count that went DOWN**, which is impossible from the arithmetic —
    /// `reasoning_display_lines` is `ceil(width / cols)` summed, so adding text can only add lines.
    /// The operator saw it happen: *"lol, just saw how thinking lines count went from 22 to 15."*
    /// What the number was counting was the reasoning *plus* the rows that reasoning had already
    /// become; the round boundary rebuilds the pane — empty `reasoning`, mark at zero — and the
    /// inflation vanished with it. A count of work done fell, which is the same defect leticl
    /// measured on the calls side: *"2 (in yellow) tool calls dropping to 1 (in yellow) tool calls
    /// and then changing back to 2 (in white) tool calls."*
    pub(crate) reasoned_upto: usize,
    /// The `ts` of `TurnStarted`, and of the last event seen for this turn. The
    /// difference is how long the turn has been going, taken from the log's own
    /// clock rather than from a wall clock in the head — a head that reads a
    /// recorded session must show the same elapsed time as the one that watched it.
    pub(crate) started_ms: u64,
    pub(crate) last_ms: u64,
    /// The `ts` of the first and last `Delta { target: Reasoning }`.
    ///
    /// `card::reasoning` renders `Thought for 4.2s`, and this is where the 4.2
    /// comes from — no engine change needed, only a head that keeps the two
    /// timestamps it was already being handed. Zero means the reasoning arrived
    /// in a snapshot and has no honest duration.
    pub(crate) think_started_ms: u64,
    pub(crate) think_last_ms: u64,
}

/// The head.
pub struct App {
    pub cfg: RenderConfig,
    /// **The set of switches in force** — the state the old rung used to be.
    ///
    /// It replaced a `Verbosity` field rather than joining it, because two fields that both
    /// say *how much is on the screen* are two answers that can disagree, and the screen has
    /// exactly one. The LADDER did not go anywhere: [`Visibility::rung`] asks this set which
    /// rung the rows it owns are drawn at, so `tools`, `thinking` and `system` are still drawn
    /// by `Verbosity`'s four rungs and by nothing new.
    pub visibility: Visibility,
    /// The two-panel before/after view for file-edit cards, on when the pane
    /// is wide enough to hold both. Set in `/config` (or `head.toml`); the
    /// unified renderer is the fallback at every width, which is what makes it
    /// safe to flip on a narrow terminal.
    pub diff_split: bool,
    /// The config pane (`/config`): every setting this head and its session run
    /// under, the runtime-editable ones editable in place.
    pub(crate) config_pane: bool,
    pub(crate) config_sel: usize,
    /// Where the head's own choices are written. `None` is a head with no
    /// config directory, and the pane says so instead of pretending to save.
    pub(crate) prefs_path: Option<std::path::PathBuf>,
    /// The daemon's settings, as last listed. Empty until asked.
    pub(crate) settings: Vec<letibot_sessionlog::protocol::SettingRow>,
    /// **`!term` — the pane, when a screen program is running in it.**
    ///
    /// `None` is *no pane*, which is the head's ordinary state: the transcript is drawn in
    /// the conversation's rectangle and every key is the composer's. `Some` is a program the
    /// operator started with `!term`, drawn in that same rectangle with the header, the
    /// status row and the composer keeping the rows they had — see [`TermPane`].
    ///
    /// **One at a time, and the daemon is what enforces that.** A second `!term` while one is
    /// live comes back as a `TermEnded` carrying the refusal's sentence, which is drawn where
    /// the pane's own ending is drawn; this head does not refuse it locally, because the
    /// authority on *is a pane open* is the daemon that owns the pty.
    pub(crate) term: Option<TermPane>,
    /// **What this head believes about the session's pane** — the daemon's answer to
    /// `ClientFrame::TermStatus`, kept because a head that is **not drawing** the pane still has
    /// to say that something is running in it (and a `!term close` has to name what it is about
    /// to end). See [`PaneFact`], and [`App::pane_behind`] for how the two sources of that fact
    /// — this head's own pane and the daemon's answer — are joined.
    pub(crate) term_fact: PaneFact,
    /// **The confirmation that ends a pane**, when one is up. See [`TermAsk`].
    pub(crate) term_ask: Option<TermAsk>,
    /// **A `!term close` that is waiting for the status read.** The head cannot always answer
    /// *is there a pane to end* from what it holds — it has just attached, or it never opened
    /// one — so the line is **held** and the answer runs the same decision. See
    /// [`App::begin_close`].
    pub(crate) close_pending: bool,
    pub(crate) session_id: String,
    pub(crate) head_id: String,
    /// The head id the daemon just handed out, for the driver to give the client.
    ///
    /// A `Switch` seats this connection as a *different head* in the new session,
    /// and a client that kept the old id would ack into a session it had left —
    /// which the hub would silently ignore, so the mark would stop advancing and
    /// nothing would say why.
    pub(crate) seated: Option<String>,
    /// What this session is talking to: `model · dialect · endpoint · workspace`.
    /// §4.4, arriving on `Hello`.
    pub(crate) wiring: SessionWiring,
    /// Every session the daemon holds, as of the last `Hello` or `Sessions` frame.
    pub(crate) sessions: Vec<SessionBrief>,
    /// Subagents this session has spawned, folded from the durable `Subagent`
    /// events. Keyed by session id: a `running` row becomes its `done` row.
    ///
    /// **Three sources, one list, and the two durable ones are joined in one place.** The live
    /// `Subagent` events are the richest — state, role, task, answer; the **snapshot's own
    /// children** ([`letibot_sessionlog::view::Snapshot::subagents`], folded by the parent's
    /// view) are the same facts' conclusion, and they are what a switch back is handed; and the
    /// daemon's session list is the one measurement of NOW either half has. A list built from
    /// the events alone was empty for every head that did not watch the spawn — and, once the
    /// events were gone, empty for a head that had.
    pub(crate) subagents: Vec<SubagentState>,
    /// Background jobs this session started, folded from the `Backgrounded`
    /// outcome on a tool finish and the durable `JobSettled` event. In the order
    /// they were backgrounded; a settlement folds into its row.
    /// **The daemon's job table**, as it last answered `ListJobs`.
    ///
    /// Not built here any more. The head folded `ToolFinished`/`JobSettled` into
    /// rows of its own and joined the command out of whichever turn it was
    /// showing, so a job that outlived its turn lost its name — and a second head
    /// in another language had to reimplement all of it. The operator,
    /// 2026-09-20: *"regarding jobs, subagents, etc, i expect them to be handled
    /// by harnessd not the heads"*. A `JobSettled` still folds onto a row for
    /// liveness; anything it does not recognise waits for the next answer.
    pub(crate) jobs: Vec<letibot_sessionlog::protocol::JobEntry>,
    /// Which picker row the cursor is on. Arrows move it, Enter takes it; it starts
    /// on the session this head is already in, so an untouched list answers Enter
    /// with a no-op rather than a surprise.
    pub(crate) picker_sel: usize,
    /// How many rows the picker block actually drew on the last screen: the
    /// two title lines plus the sessions that survived `truncate(room)`. A
    /// click is only trusted for a row this count proves was on screen — a
    /// click into the blank space below a truncated list must not select a
    /// session nobody can see.
    pub(crate) picker_rows_drawn: usize,
    /// The terminal height the last screen was composed for, so a click can
    /// redo the header-row arithmetic the screen did without a repaint.
    pub(crate) screen_rows: usize,
    /// A Tab-driven completion in progress: the prefix as typed, the candidate
    /// names it matched, and which one is current. Re-derived whenever the
    /// text no longer starts with the cached prefix; any other key leaves it
    /// alone, and the render only trusts a prefix that is still being typed.
    /// The live completion cycle: the prefix it was started for, the names it is
    /// cycling, and which one is showing. **Owned `String`s** rather than `&'static
    /// str`, because half the list now comes from the daemon (R32) and a borrowed list
    /// could only ever hold this head's own table.
    pub(crate) completion: Option<(String, Vec<String>, usize)>,
    /// **The model's half of the `!` completion, in flight and answered.**
    ///
    /// The history is the first answer and this is the fallback: when the history has
    /// no match for the prefix (or its cycle is exhausted), the head asks the daemon,
    /// and the daemon asks the local model. The operator's ask, in their words: *"i
    /// want smart ! when a model suggest completions."*
    ///
    /// `shell_ask` is the asks in flight, keyed by the `client_request_id` the head
    /// minted, mapping to the (prefix, transcript position) the ask was for. The id is
    /// the correlation: the answer comes back on the pump and the head has to tell one
    /// answer from another, because a suggestion cached under the wrong prefix is a
    /// wrong suggestion. `shell_suggestions` is the answered asks, keyed by
    /// (prefix, transcript position) — the same prefix asked twice at the same
    /// position is not two model calls.
    ///
    /// **Both are cleared when the transcript advances**, because a suggestion built
    /// on the conversation as it was is a suggestion about that conversation, and a
    /// conversation that moved is a different question. The position in the key is the
    /// number of transcript rows at the ask, so a row landing is a new position and a
    /// stale answer.
    pub(crate) shell_ask: std::collections::HashMap<String, (String, u64)>,
    pub(crate) shell_suggestions: std::collections::HashMap<(String, u64), Vec<String>>,
    /// The number the next `SuggestShell`'s `client_request_id` takes, beside
    /// `head_run_seq` and for the same reason: the id has to be unique per head, and
    /// the head is the one that has to recognise it on the way back.
    pub(crate) shell_ask_seq: u64,
    /// **The model's cycle, when it is live**: the prefix it was started for, the
    /// lines it is cycling, and which one is showing. Separate from `completion`
    /// (the history's cycle) because the two have different provenance and the render
    /// has to tell them apart — a model line drawn as a history line is a line that
    /// looks like the operator typed it and did not.
    pub(crate) shell_model: Option<(String, Vec<String>, usize)>,
    /// **The file names a Tab found for the word being typed**, and the line they were
    /// found for. Shown in the completion row while the line is unchanged — the shell's
    /// own "here are the choices" — and dropped the moment it is edited.
    pub(crate) path_matches: Option<(String, Vec<String>)>,
    /// **The `!` candidates, computed from the rows and held until they move.**
    ///
    /// The list is the same for every frame that draws the live `!` row, and building
    /// it walks the view and parses every `bash` call's arguments. **Measured: 14.2 ms
    /// a frame** on a 2,000-row session, which is a stall rather than a cost —
    /// `completions_line` runs once per frame, so the walk has to run once per row
    /// change instead. [`App::the_rows_moved`] is the one place that drops it.
    ///
    /// `None` is *not built yet* and an empty `Some` is *there are none*, which is the
    /// distinction a cache needs: a session with no `!` line in it must not walk the
    /// view again on every frame to find that out.
    pub(crate) shell_candidates_memo: Option<Vec<String>>,
    /// **How many times that walk has run**, ever — the encoder for the memo above, and
    /// for the same reason [`App::hist_renders`] exists: a wall time is not something a
    /// test can assert on and a count is.
    pub shell_walks: u64,
    /// Actions produced by a *frame* rather than by a key: the switch that follows
    /// a session being created. Drained by the driver, which is the only thing that
    /// can send.
    pub(crate) queued: Vec<Action>,
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
    ///
    /// **Unless the row has been announced.** The two are not one channel: the
    /// model's reply arrives as a `Delta` carrying its text and renders as it
    /// streams, while a user row arrives as `TranscriptAppended` — an id and a kind,
    /// no text — and only later as `TranscriptContent`. So the reply is always
    /// faster to display than the prompt that caused it, and the echo below kept
    /// saying `queued` while the words it stood for were already in the
    /// conversation above it. See [`App::bound_prompts`].
    pub(crate) pending_prompts: Vec<String>,
    /// **The echo in the transcript's own place, before its body lands.**
    ///
    /// `item_id` → the echo text this head optimistically bound to an announced
    /// body-less user row. A prompt typed while a turn runs is appended by the
    /// daemon at its next step boundary, and the *announcement* of that append is
    /// what says where the row goes; the body follows on the next frame or shortly
    /// after. Rendering the row from the echo the head already holds puts the prompt
    /// above the reply it caused, which is where the transcript has it, instead of
    /// leaving it below and tagged `queued` until the body catches up.
    ///
    /// **Optimistic, because not every `User` row is this head's prompt.** A
    /// harness steering notice and a §5.7 salvage notice are the same shape
    /// (`harnessd::harness` says so in as many words: *"a steering message and a
    /// §5.7 notice are the same shape as a prompt"*), and so is another attached
    /// head's prompt. The announcement carries nothing that tells them apart, so the
    /// binding is a guess and the row keeps the `queued` shape until the body
    /// confirms it: the word is exactly right for "bound but unconfirmed". The
    /// **retire** still waits for content matched by text
    /// ([`App::retire_pending`]) — never the announcement alone, or a notice would
    /// silently swallow the echo of the prompt still sitting in the hub's queue.
    ///
    /// A body that contradicts the binding ends it too, and then both correct: the
    /// row renders from its real content and the echo reappears at the tail.
    pub(crate) bound_prompts: std::collections::HashMap<String, String>,
    /// **The echoes that were in the air when the conversation was about to be
    /// replaced** (R16).
    ///
    /// A fork — `/compact`, `/reseat`, an automatic compaction at the wall —
    /// **replaces the transcript**, and a prompt that was queued under the old one
    /// had its row summarised away with it. `retire_pending` waits for
    /// `TranscriptContent` matched by text, and that event is never coming, so the
    /// echo said `queued` for the rest of the session: measured on this head
    /// 2026-09-22, three prompts still rendering `queued ·` while the tree was clean
    /// and the work they asked for was committed.
    ///
    /// So a fork **resolves** the binding instead of orphaning it, and the list here
    /// is what makes that exact rather than approximate: it is the echoes that were
    /// pending **when the fork began**, taken then and not inferred later. An echo
    /// queued *after* the fork began belongs to the new transcript and is left alone
    /// — which matters, because a prompt typed during the summary turn is queued
    /// behind that turn and lands in the base the fork produced. Retiring it would be
    /// §4.2's swallowed sentence with a new cause.
    ///
    /// **Taken at the two moments the head can know.** The automatic path publishes
    /// `auto_compact` *before* it forks (`sessions.rs`: the warning, then
    /// `self.compact`), so that is the mark; the manual path is one this head sent
    /// itself, so `command` marks it on the way out. The fork then says
    /// `compacted`/`reseated` **after** it has happened, and that is where the
    /// marked echoes go — see the `Warning` arm.
    pub(crate) fork_pending: Vec<String>,
    /// Set when this head asked for a session and is waiting to be told its id.
    pub(crate) want_new_session: bool,
    /// The last turn's `usage`, kept past the end of the turn so the header can
    /// say how much context this session is carrying while nothing is running.
    pub(crate) usage: Option<Usage>,
    /// Whether `usage.cached_tokens` is a measurement or a placeholder. A usage
    /// seeded from the session's row after a restart knows the prompt size (the
    /// row carries it) but not the cache fraction (a prompt that was never sent
    /// has none), and the header shows the percentage only when it was measured —
    /// a `0%` nobody took is the same defect as a rate nobody measured.
    pub(crate) usage_cache_measured: bool,
    /// The last turn's `timings`, kept for the same reason and shown beside it:
    /// the decode rate and the wall time the turn footer used to carry. They
    /// moved because the footer repeated the header's context and cache numbers
    /// next to them, and one fact on one screen twice is one fact rendered as a
    /// question — see `turn_footer` for what the footer kept.
    pub(crate) last_timings: Option<Timings>,
    pub(crate) items: Vec<SnapshotItem>,
    /// The rendered transcript — every row, as the bytes that reach the terminal.
    ///
    /// # ONCE A ROW IS RENDERED, NOTHING ABOUT IT CHANGES ON ITS OWN
    ///
    /// Ruled by the operator, 2026-09-27, in their own words: *"once something is rendered nothing
    /// left to it except tool calls / thinking lines count shouldn't ever change by itself, without
    /// say me toggling verbosity."*
    ///
    /// Two things may change a rendered row, and nothing else:
    ///
    ///  * **the counts clause of the live marker** — `[2 tool calls, 31 thinking lines]` while the
    ///    work it stands for is still happening. That is the one piece of a row that is a fact about
    ///    NOW rather than about what happened, and it is why [`App::marker_facts`] exists;
    ///  * **a verbosity toggle**, which is the reader asking for a different rendering of the same
    ///    conversation — every row may change then, and it is the only case where that is true.
    ///
    /// Everything else in here is a record. A row that quietly re-renders — a mark that changes as
    /// a fact settles, a line that grows, a count that appears late — is a defect, not a refresh:
    /// the reader's memory of what they just read is part of the interface. Every defect this head
    /// has had in this area has that shape, and three of them are recorded in this file: a marker
    /// whose counts were *backfilled* by the next row landing, a yellow that only arrived when a row
    /// did, and an echo that kept saying `queued` after its row had landed.
    ///
    /// So the rule for a change to any row: **can the operator see it happen, and did they ask for
    /// it.** The counts and the rung are the whole of the yes.
    pub(crate) hist_lines: Vec<String>,
    pub(crate) hist_upto: usize,
    /// The first item index represented in `hist_lines`.
    ///
    /// `0` for a head that has walked the conversation from its beginning, which is
    /// every head until it attaches to a session too big to lex. A head that
    /// attaches to such a session renders its **tail** — the current frame and
    /// enough above it to scroll — and this is how many rows it did *not* render.
    /// Scrolling up decreases it; nothing else does.
    ///
    /// **Why this exists at all**: the operator's sessions reach 160 MB and
    /// thousands of turns, and rendering the bottom 40 rows used to require lexing
    /// every row above them. The frame shows the end of the conversation, so the
    /// end is what gets rendered first. `tail_cut` is the markdown half of that
    /// (`crates/tui/src/markdown.rs`); this is the walk's.
    pub(crate) hist_floor: usize,
    /// The `seq` at which the head last read the `model` settings row, and the `seq` of
    /// the `TurnStarted` that last named a model.
    ///
    /// The header picks whichever is later: a settings row is only ever sent as an
    /// *answer*, so an attached head is not told about a mid-conversation provider
    /// switch, while a turn arrives unprompted and names what is answering. See the
    /// header's model selection for the whole argument.
    pub(crate) model_from_settings_at: u64,
    pub(crate) model_from_turn_at: u64,
    /// How much transcript this head will walk from the beginning before it renders the
    /// tail instead. [`SELF_WALK_LIMIT`] unless someone says otherwise.
    ///
    /// A field rather than a constant because the path it selects has to be testable:
    /// `usize::MAX` forces the full walk, so a test can render both ways and compare,
    /// and a machine with a different idea of "too big" can say so.
    pub(crate) walk_limit: usize,
    /// The kind of the **first** row in `hist_lines`.
    ///
    /// `hist_class` is the last one, which is all a forward walk needs; a backward
    /// walk prepends, and the separator it owes the seam is decided by the two kinds
    /// that meet there. `None` for an empty history.
    pub(crate) hist_first_class: Option<RowClass>,
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
    pub(crate) hist_marks: Vec<HistMark>,
    pub(crate) hist_width: usize,
    /// The class of the last row the walk actually drew, so the next one knows
    /// whether a blank line belongs between them. The walk is incremental across
    /// frames, so this has to survive the frame that set it.
    pub(crate) hist_class: Option<RowClass>,
    pub(crate) turn: Option<TurnPane>,
    pub(crate) open: Vec<OpenDecision>,
    /// `sudo` in the session wants a password: the request, and what has been
    /// typed for it so far. Kept OUT of the composer, so it is never in the
    /// composer's history, never completed, never shown: the composer draws a
    /// dot per character while this is `Some`.
    pub(crate) secret: Option<SecretAsk>,
    pub(crate) secret_buf: String,
    /// **Which open secret requests are key asks**, kept until they settle. The card is
    /// closed by the answer (Esc clears it at once), so by the time `SecretSettled`
    /// arrives the card can no longer say what it was — and a refused key would be
    /// noted as sudo's refused password.
    pub(crate) key_secrets: Vec<String>,
    /// **What the terminal speaks beyond cells** (`crate::features`), told by the head after
    /// it entered the terminal. Default — nothing — for a test, a replay and a pipe.
    pub(crate) features: crate::backend::features::Features,
    /// Whether the terminal window has focus, from `?1004` reports. `None` until the first
    /// report, and read as focused: a notification goes only to somebody known to be away.
    pub(crate) focused: Option<bool>,
    /// The terminal's answer to OSC 11: is its background light. `None` until it answers.
    pub(crate) light_background: Option<bool>,
    /// What needed the person last tick — see [`App::take_notification`]. `None` until the
    /// first look, which is a baseline and never a notification: attaching to a session with
    /// a card already open is not news.
    pub(crate) attention: Option<Attention>,
    /// Text the operator asked to put on the clipboard, for the head to write (OSC 52).
    pub(crate) clipboard_out: Option<String>,
    /// Images already in the terminal's memory, by id (see `render::image_id`), and how many
    /// rows of `items` have been looked at for new ones.
    pub(crate) images_sent: std::collections::HashMap<u32, (Option<u32>, Option<u32>)>,
    /// The image box the placements were last sent for (`render::image_box` of the frame's
    /// width). A frame at another width re-sends every placement at the new size.
    pub(crate) images_box: u32,
    pub(crate) images_scanned: usize,
    /// Upload bytes for the head to write before the next frame (kitty graphics).
    pub(crate) image_uploads: Vec<Vec<u8>>,
    /// **A command of the operator's own is waiting for an answer**: the request, and what has
    /// been typed for it so far.
    ///
    /// Kept OUT of the composer, exactly as [`App::secret`] is and for a sharper reason than
    /// the password's: a line typed into a card is an answer to a program that is blocked on
    /// it, and letting it fall into the composer would leave it sitting there to be submitted
    /// again as a shell command. It has its own field and its own buffer, and the composer
    /// draws that buffer while the card is up.
    ///
    /// **The text is NOT masked**, and that difference from [`App::secret_buf`] is the whole
    /// of what keeps the two channels apart: this card is drawn in the open because what it
    /// carries is a line for a program's stdin, and a secret must never travel here. See
    /// [`App::prompt_lines`].
    pub(crate) prompt: Option<PromptAsk>,
    pub(crate) prompt_buf: String,
    /// **A key the model picker asked for** — the operator's row: *"if i choose a model without
    /// key picker should ask for the key."* The greening told them WHICH rows need one; this is
    /// the row that collects it. Its own state and never the sudo path's: a provider key is
    /// stored, not spent, and borrowing `SecretAsk` would tie a head-side ask to a daemon
    /// `req_id` that does not exist.
    pub(crate) key_ask: Option<KeyAsk>,
    pub(crate) key_buf: String,
    /// Screen requests this head has not answered yet. Answered by the DRIVER,
    /// after the frame is built, with the rows it actually drew.
    pub(crate) screen_requests: Vec<String>,
    /// The terminal's full width at the last render, gutter included. See
    /// [`App::screen`].
    pub(crate) term_cols: usize,
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
    pub(crate) sel: usize,
    /// Things that happened *between* transcript rows and belong in the
    /// conversation: a guard that fired, a decision that settled.
    ///
    /// Each is anchored to the number of rows that existed when it arrived, so the
    /// history rebuild puts it back where it happened. They used to be pinned to
    /// the bottom of the body — the last three warnings sat above the status line
    /// forever, so a warning about turn three was still shoving turn nine up the
    /// screen — and a settled decision was recorded and then never rendered at all,
    /// which is the silence §13.2b says a refusal must not become.
    ///
    /// **And a note the head did not file itself is [`Placed::Before`]** (R19): it came
    /// with a snapshot, so it happened before this window and is listed rather than
    /// drawn. See the type, and [`App::load`] for where the two kinds are sorted.
    pub(crate) notes: Vec<(Placed, Note)>,
    /// **How far the open card's content is scrolled** (R20), counted in rows from the
    /// TOP of the content — the opposite of [`App::scroll`], which counts rows back from
    /// the bottom because a transcript is read from its tail. A card is read from its
    /// head: the question and what it is about are the first lines, and the wall is what
    /// you walk down into.
    ///
    /// Reset in one place ([`App::screen`], on a change of `open[0].req_id`) rather than
    /// at every site that replaces the open set, because there are several and one would
    /// have been forgotten — and a card that inherited the previous card's offset is a
    /// card whose first screenful was somewhere in the middle.
    pub(crate) dec_scroll: usize,
    /// The `req_id` [`App::dec_scroll`] belongs to, so the reset above can tell a new card
    /// from the same card drawn again.
    pub(crate) dec_scroll_for: String,
    /// **What the card's content window actually was on the last frame**: how many lines
    /// the content has, and how many rows the viewport got, seam excluded.
    ///
    /// The key handler asks these to decide whether the page keys belong to the card at
    /// all — the question is *is anything out of view*, and only the draw knows it, because
    /// the length of the content is a function of the width. Named for the panes'
    /// `pane_len`/`pane_room`, which are the same arrangement.
    pub(crate) dec_content_len: usize,
    pub(crate) dec_content_room: usize,
    /// **The notes this reader has retired**, by [`note_key`] — the identity a
    /// note keeps across a resync and a restart.
    ///
    /// R10: a note is a **disclosure, not a permanent record**. The session log
    /// holds the durable fact; the note is how a head shows it ONCE. Nothing
    /// removed one before — only the conversation growing past it — so on an idle
    /// session the red wall stayed for ever, and a resync or a restart made it
    /// *worse*: a snapshot's warnings are unanchored history, so the wall came
    /// back at position 0 above the whole conversation.
    ///
    /// The keys live in `head.toml` rather than in the process, because a
    /// restart is one of the two cases that used to replant the wall. They are
    /// keyed per incident (see [`note_key`]), so two sessions do not share a
    /// dismissal; the cap is a cap on *this reader's memory*, not on the log.
    ///
    /// **Retired is not deleted.** A note whose key is here is *hidden, counted
    /// and findable*: it stays in `notes`, `/notes` lists it with its text, and
    /// `/status` counts it. That is the rule `/status`'s own `filtered` counter
    /// keeps — "I chose not to show this" must not look like "nothing happened".
    pub(crate) dismissed: Vec<String>,
    /// How many of those are already in `hist_lines`.
    pub(crate) note_upto: usize,
    pub(crate) heads: usize,
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
    /// **Frames this build could not read, and did not die of.**
    ///
    /// The requirement is *survive AND count*: a head that exits on an unparseable
    /// frame says nothing and takes the session down with it, and a head that steps
    /// over one in silence is the same failure more quietly — *"this daemon is
    /// sending me something I do not understand"* becomes indistinguishable from
    /// quiet. `ServerFrame` and `SessionEvent` are internally tagged, so an unknown
    /// tag is what a daemon one version ahead looks like from here, and this is the
    /// number that says so. On `/status`, and on the border once it has moved.
    ///
    /// The read mark is deliberately not touched by one of these: nothing was
    /// parsed, so there is no seq to ack, and inventing one would rewind this head's
    /// mark over frames it has already read.
    pub unreadable: u64,
    /// **Events the daemon sent and this head never got** (R17).
    ///
    /// `seq` is dense and the daemon alone assigns it, so a received frame whose
    /// seq is more than one past the last is proof that something between the two
    /// is missing — not a guess, and not a rendering choice. Before this counter a
    /// head assigned `self.seq = env.seq` unconditionally, which is exactly what
    /// makes a delivered row and a dropped one indistinguishable.
    ///
    /// **Counted, said, and repaired**, and the count is the part that matters: a
    /// gap repaired silently looks identical to a session that never had one, so
    /// the operator learns nothing about a daemon, a socket or a compaction that is
    /// losing rows. It is the sixth bucket of that family, beside `dropped`,
    /// `scrubbed`, `filtered`, `resyncs` and `unreadable` — and the only one of the
    /// six that is about a row the ledger has and this head does not.
    ///
    /// **It does not fire on a backlog.** Events waiting in the daemon's per-head
    /// queue, on the socket, or in this head's own channel are simply not here yet —
    /// this head is at its own tail and correct about it. `App::behind` is the other
    /// number, and the two are different facts about different places.
    pub gaps: u64,
    /// **How far the daemon says it is ahead of this head**, in events, the last
    /// time it said anything at all.
    ///
    /// A head that is *behind* has an **empty** queue and a **correct** screen: it
    /// has drawn everything it was given and there is nothing more coming yet. That
    /// is indistinguishable from *current* from inside, and it was measured from
    /// outside on 2026-09-22 — a head whose last row was seq 339 while the ledger
    /// held 375, with no scrolled-back seam and nothing the head could have said.
    ///
    /// So the number comes from the one frame that states the daemon's position
    /// without being asked: an `Accepted` carries the seq at which the command's
    /// effect is visible, and a `Rejected` carries the seq the daemon is actually at.
    /// Either is the daemon saying *"I am here"*, and a head that compares that with
    /// its own `seq` learns the distance. It is a lower bound — the daemon has moved
    /// on since — and a lower bound is enough to say *"not current"*.
    pub behind: u64,
    /// **Bodies that arrived for rows this head does not hold.**
    ///
    /// A row is announced by id and its body follows on another event, and
    /// [`App::record_item`] drops a body whose id it cannot find. That drop was
    /// silent, and it is the third way a row the ledger has can be missing from the
    /// screen: not lost on the wire ([`App::gaps`]), not waiting for its body
    /// ([`App::outstanding`]), but **undrawable for ever** — the id is gone from
    /// `items` and the body that would have filled it has been thrown away.
    ///
    /// It has one known cause and it is not a bug in this head: a snapshot replaces
    /// `items` wholesale, and the daemon's view is bounded (2000 rows, 8 MB of
    /// bodies), so a body for a row the snapshot had already trimmed arrives with
    /// nowhere to go. Counted anyway, because "the daemon and I disagree about what
    /// exists" is a fact an operator should not have to infer from a gap in a
    /// conversation.
    pub orphan_bodies: u64,
    /// **Times the provider was slow to send its first byte**, and it said so.
    ///
    /// `model_slow_first_byte` — a fact about the weather rather than an event in the
    /// conversation, which is why it is a counter here and not a row in the transcript.
    /// See `letibot_sessionlog::warning::ALARM_ONLY` for the rule that puts it here. The
    /// operator's ruling: *"it is important diagnostics - we have a yellow triangle for
    /// that. both heads should not emit it inside conversation."*
    pub slow_first_byte: u64,
    /// **The counter values this reader has already been shown** — R51 item 17.
    ///
    /// The `⚠` on the composer's edge is a pointer at `/status`, and this is what makes it
    /// dismissible: the mark is drawn while a counter exceeds its value HERE, so reading the screen
    /// clears it and a counter that moves afterwards brings it back. Zero for a head that has read
    /// nothing, which is the same state as a fresh head because every counter starts at zero too.
    ///
    /// **Not persisted, and it must not be.** The counters are counts of what THIS process
    /// survived — they start at zero with it and die with it — so an acknowledgement written to
    /// disk would outlive the numbers it was an acknowledgement OF, and a restarted head would
    /// come up having already forgiven incidents it has not had.
    pub(crate) acked: Counters,
    /// Scroll offset from the bottom, in lines. 0 is "following the stream".
    pub scroll: usize,
    /// The composer. `letibot_ui::editor::Editor` — multi-line, with history, a
    /// kill ring, undo batching, a paste ledger and the two interrupt double-taps.
    /// It was a `String` and a character index, which is why there was no way to
    /// write a two-line prompt, recall the last one, or paste a stack trace
    /// without losing bytes.
    pub(crate) editor: Editor,
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
    pub(crate) notice: Option<String>,
    /// **When the notice stops being news, on this head's own clock** — the same
    /// milliseconds [`App::clock`] is fed and the same ones a running call's elapsed
    /// time is measured against.
    ///
    /// `None` is a notice **nobody started a clock on**, which a direct write to
    /// `notice` can still make. There is one such writer left — the resync line, which
    /// goes through [`App::say`] for exactly this reason — and it is written down here
    /// because a note with no clock is the note that can never be cleared: leticl's
    /// live defect was a magenta `permission answered` that stood for the rest of the
    /// session, and this field is what makes that state visible as a type rather than as
    /// a countdown somebody forgot to start.
    ///
    /// **A countdown of frames was the defect, not the number.** It made a notice's
    /// lifetime a fact about the render loop rather than about the reader: see
    /// [`NOTICE_MS`], which is also where the two symptoms and the fix are written down.
    pub(crate) notice_until: Option<u64>,
    pub(crate) help: bool,
    /// The session picker, which is a screen like `help` rather than a mode with a
    /// cursor. Same argument as the folds: there is one input surface here and it
    /// is a line, so the affordance is *typing the number you can see* — which also
    /// means the picker needs no keymap of its own and works over a pipe.
    pub(crate) picker: bool,
    /// **The setting being chosen, or nothing.** One field for every setting card, because
    /// "one list on the screen at a time" was a rule four openers kept by hand — each one
    /// clearing the other three — and R38 added two more subjects to it: hand-kept
    /// invariants are the shape this file has been bitten by, and `Option` makes it
    /// structural.
    ///
    /// The card itself is one renderer for all four (see `App::setting_picker_lines`): the
    /// choices, the cursor, the click arithmetic and the drawing are shared, because the one
    /// thing this file has already been burned by is a second copy of a list that then
    /// drifts.
    pub(crate) pick: Option<Pick>,
    /// **`allow-all`, held one keystroke short of sent.** The point admits the
    /// always-ask list — privilege escalation, a delete outside the project, a
    /// host never seen — and on this box those land on the operator's own
    /// machine. `allow-all` used to refuse outright here, naming a confinement
    /// no bare host can build; it now asks instead, and this holds the name
    /// while it is asking. `None` means nothing is pending.
    ///
    /// Asked for every `allow-all`, including inside a VM where the daemon
    /// ignores the answer and opens the structural point regardless: a head
    /// that decided when to ask would need to know whether the session is
    /// confined, and a head that guesses that wrong asks nothing at exactly the
    /// coordinate worth asking at.
    pub(crate) mode_confirm: Option<String>,
    /// **The new-todo card: title and detail, the composer being the field** — leticl's
    /// `*todo-draft*`, and the shape `mode_confirm` already keeps one screen over.
    ///
    /// `(title, detail, typing_the_detail)`. **The composer is the field being typed and this holds
    /// the OTHER one**, so the field under the cursor is never a keystroke behind — leticl's
    /// `%todo-draft-focus` records the one it is leaving for exactly that reason. `None` when no
    /// card is up.
    ///
    /// **A keyboard owner, like the password field and the `allow-all` card**, because a half-typed
    /// prompt left under a card whose Enter adds an item is the shape that costs somebody a message:
    /// `key` returns before the composer sees anything while this is `Some`, and every key that is
    /// not `Tab`/`Enter`/`Esc` is the editor's.
    pub(crate) todo_draft: Option<TodoDraft>,
    /// **`todo_template` as this head loaded it** — the starter-todo switch, leticl's own key,
    /// carried on `App` because the seed runs at the attach, long after `load_prefs`. Off by
    /// default; see `prefs::TodoTemplate` for the three shapes.
    pub(crate) todo_template: crate::prefs::TodoTemplate,
    /// **The projects that have had their starter todos** — hashed workspace paths, leticl's
    /// `todo_seed` table in the only store this head has (`head.toml`, beside `retired`). A record
    /// and not an *is the list empty* test: a starter row the operator deletes must not come back.
    pub(crate) todo_seed: Vec<String>,
    /// **A seed is waiting for the board.** Set by the attach when the switch is on and this
    /// project has not been seeded; the next `TodosUpdated` — the answer to the `ListTodos` the
    /// attach queued — copies the template's items onto the operator's half. The board must be
    /// read first because this head keeps no second list: seeding against a stale `todos` would
    /// send a half that omits rows the daemon holds, and `SetOperatorTodos` replaces the half.
    pub(crate) todo_seed_pending: bool,
    /// **The quit card**, opened by the second Ctrl+C instead of leaving at
    /// once. Two answers, because `Ctrl+C Ctrl+C` had one meaning and an
    /// operator often wants the other: leave the head and let the daemon keep
    /// the session warm, or stop both. The operator, 2026-09-17: *"when i do
    /// CcCc i should be asked if I want to exit letibot or letibot and
    /// harnessd"*.
    ///
    /// It is a card and not an immediate act because the second answer is the
    /// irreversible one — the daemon's KV goes with it, and on this box a cold
    /// prefill of a long session is minutes.
    pub(crate) quit_card: bool,
    /// Which row of the quit card the cursor is on. Seeded to 0 — leave the
    /// head — so Enter on an untouched card does the smaller thing.
    pub(crate) quit_sel: usize,
    /// Which mode row the cursor is on. Seeded to the mode the session is
    /// already under, so Enter on an untouched list is a no-op rather than a
    /// surprise — the same rule the session picker's cursor follows.
    pub(crate) mode_sel: usize,
    /// **Whether the open picker has been positioned by what it actually lists** — R51's
    /// neighbour, and leticl's `*pick-unseeded*` (its `head.lisp`, which records the operator's
    /// report of this exact symptom: *"mode selectors has selection on the first not on the
    /// current again"*).
    ///
    /// A picker's cursor is seeded from the rows the head holds. **`/mode` and `/models` open the
    /// card and ask for fresh rows in the same breath**, so on a head whose rows have not landed
    /// yet the seed reads an empty list, `position` answers nothing, and the cursor sits on row 0
    /// — while `← now` marks the real current row further down. It works on the second open,
    /// which is why the report is *again* rather than a permanent break, and why a test that sets
    /// the rows up first never sees it.
    ///
    /// Cleared by anything the READER does to the cursor, so an answer landing while they are
    /// arrowing cannot snap it back — a worse defect than the one it fixes.
    pub(crate) pick_unseeded: bool,
    /// How many choice rows the mode card actually drew on the last screen —
    /// zero unless the whole card fit, because a click is only trusted for a
    /// list the frame proved was all on screen. A partially drawn card is
    /// exactly the case where trusting clicks picks a mode nobody saw.
    pub(crate) mode_rows_drawn: usize,
    /// The screen row the card's first choice sat on, as the last frame
    /// composed it. A click redoes this frame's arithmetic without a repaint —
    /// the same trick the session picker's header arithmetic does, one card
    /// lower.
    pub(crate) mode_first_row: usize,
    /// The todos pane, a screen like the picker: the session's plan (what the
    /// model last wrote through `todo_write`) and the repo's own queue
    /// (`TODO.md`, read-only here — an agent's plan and the operator's queue are
    /// different lists, and the pane says which is which).
    pub(crate) todos_pane: bool,
    /// The subagent tree pane, a screen like `todos`: the subagents this session
    /// spawned, their state and their prompt. `ctrl-g`.
    pub(crate) subagents_pane: bool,
    /// The background-jobs pane, a screen like the other two: the jobs this
    /// session started, running and settled. `ctrl-q`.
    pub(crate) jobs_pane: bool,
    /// **The merge-queue pane** — the operator's *"we need a gated merge to main"* made
    /// visible: every entry the daemon is serving toward main, what state it is in, how old it
    /// is, and why it is where it is. `/queue`.
    ///
    /// **No chord, and that is a decision rather than an omission.** The head's own bar says
    /// the chords are over capacity at eighty columns and that *which* of them are visible is a
    /// decision — so a sixth pane takes the verb, which every pane already has and which a head
    /// driven over a pipe can reach.
    pub(crate) queue_pane: bool,
    /// **The merge queue, as the daemon last answered `ListMergeQueue`** — the daemon's own
    /// rows, never this head's reconstruction, for the reason `jobs` is: the queue is the
    /// daemon's and a head that folded its own version out of the events would draw a stale one
    /// after any event it missed. The `MergeEntryAdded`/`MergeEntryMoved` events carry the
    /// changes; this is the snapshot they start from.
    pub(crate) merge: Vec<letibot_sessionlog::event::MergeEntry>,
    /// **The reviewer's verdicts, beside the entries** — see `MergeReview` for why they travel
    /// apart rather than inside an entry: an entry can have no review at all, and *nobody asked*
    /// is a different fact from *asked and unanswered*.
    pub(crate) merge_reviews: Vec<letibot_sessionlog::event::MergeReview>,
    /// **Which entry row the cursor is on** — an index into `merge`, and the same enumeration
    /// the drawn `▸`, the arrows and Enter read, so they cannot disagree.
    pub(crate) queue_sel: usize,
    /// **The pane row each entry was DRAWN on** — the record the arrows scroll by, never
    /// arithmetic over the list. See `jobs_stop_rows` for the defect this avoids.
    pub(crate) queue_stop_rows: Vec<usize>,
    /// **The screen row the queue pane's own first body row goes to** — the session header,
    /// when the frame is tall enough to have one. Recorded rather than assumed because a
    /// click's `y` is in absolute screen coordinates; see [`App::queue_stop_at_row`].
    pub(crate) queue_pane_top: usize,
    /// **The entry whose detail overlay is open, by id**, until Esc. By ID and not by index:
    /// a `MergeEntryMoved` event can move the queue under the overlay while it is up, and an
    /// index would then point at whichever entry slid into that slot.
    ///
    /// The overlay reads the entry and its review out of `merge`/`merge_reviews` at DRAW time,
    /// so a move while it is open shows the new state rather than the state at the keypress.
    pub(crate) queue_open: Option<String>,
    /// Which job row the cursor is on. Arrows move it, Enter asks the daemon for
    /// that job's output — the pane counted the bytes and had no way to show them.
    ///
    /// **An index into [`App::job_stops`]**, not into [`App::jobs`], so the drawn cursor and
    /// Enter cannot disagree about which row is selected.
    pub(crate) jobs_sel: usize,
    /// **Whether the jobs pane's `finished` group is unfolded.** Collapsed by default — the
    /// operator's own ask: *"jobs panel - same as subagents - show list of running, group
    /// finished"*. Enter on the group row toggles it.
    pub(crate) jobs_finished_open: bool,
    /// **The pane row each job stop was DRAWN on**, which is what the arrows scroll by — the
    /// sibling of [`App::subagents_stop_rows`]. See [`App::jobs_row_of`].
    pub(crate) jobs_stop_rows: Vec<usize>,
    /// **Which row of the pane the cursor is on** — an index into [`App::subagent_stops`],
    /// not into [`App::subagents`]. Arrows move it, Enter switches into the child it names
    /// (or folds the `finished` group) — the same two acts the picker keeps separate.
    pub(crate) subagents_sel: usize,
    /// **Whether the `finished` group is unfolded.** Collapsed by default, because a session
    /// that has spawned twenty subagents has one or two still running and eighteen finished,
    /// and the eighteen pushed the one the operator opened the pane for off the bottom of the
    /// screen: *"i went to subagents panel and dont see it here"*. Enter on the group row
    /// toggles it.
    pub(crate) subagents_finished_open: bool,
    /// **The pane row each stop was DRAWN on** — the same record [`App::todos_stop_rows`]
    /// keeps for its own pane, and for the same reason: the arrows scroll the cursor into
    /// view by an `aref` of what the pane wrote, never by arithmetic over the lists it drew
    /// from. See [`App::subagents_row_of`].
    pub(crate) subagents_stop_rows: Vec<usize>,
    /// The output view `p` opens on a subagent row, until Esc closes it.
    pub(crate) sub_out: Option<SubOut>,
    /// The subagent whose output was asked for and not yet answered. Esc cancels.
    pub(crate) sub_out_pending: Option<String>,
    /// **The child this head climbed UP out of** — the session id it left when Esc sent it
    /// back to the parent, read once by [`App::fold_subagents`] so the cursor lands on the
    /// row that child owns instead of on row zero.
    ///
    /// A row id and not an index for the reason the fold exists at all: the rows are
    /// rebuilt from the daemon's list the moment the parent's `Hello` lands, and an index
    /// taken before that rebuild points at whatever the new list happens to have there.
    /// `None` the rest of the time, which is why it is taken rather than read.
    pub(crate) up_from: Option<String>,
    /// The job-output view the jobs pane's Enter opens, until Esc returns to the
    /// jobs list. The bytes the pane was counting, finally shown in the pane.
    pub(crate) job_out: Option<JobOut>,
    /// The session's todo list, as the last `TodosUpdated` said it was. Seeded by
    /// the `Todos` reply when the pane first opens; carried forward by the events.
    pub(crate) todos: Vec<letibot_sessionlog::event::TodoEntry>,
    /// The repo's `TODO.md` as a section map, read once per pane-open. The file
    /// can be longer than the pane and is the operator's to edit; the map is what
    /// a pane can honestly show.
    pub(crate) repo_todos: Option<Vec<TodoRow>>,
    /// **What the file looked like when it was last read**: `(mtime, len)`.
    ///
    /// The pane re-read `TODO.md` only when it was opened, so a file edited while
    /// the pane was up went on showing the old read — and this file is edited
    /// exactly while somebody is looking at it. The operator: *"since the file
    /// can be updated, dont cache it i guess or do a watcher with a nice
    /// syscall"*.
    ///
    /// One `stat` per draw rather than an inotify thread. A watcher would mean a
    /// descriptor, a thread and an event to route into a head whose whole design
    /// is one loop over one channel; `stat` is a syscall in the microseconds, and
    /// the pane is drawn only while it is open. `(mtime, len)` rather than mtime
    /// alone because a second-granularity mtime can miss two writes in one
    /// second, and a length change catches most of those.
    pub(crate) repo_todos_at: Option<(std::time::SystemTime, u64)>,
    /// Which row of the repo's queue the cursor is on, and whether its body is
    /// unfolded. The same two acts the jobs and subagent panes keep separate —
    /// arrows move, enter acts — because both of those got them today and a
    /// third spelling would be a third thing to learn.
    /// **The cursor, as an index into [`App::todos_stops`]** — one list, so the arrows, the click,
    /// the drawn mark and the Enter key cannot disagree about which row the cursor is on. leticl's
    /// `head-picker-sel`, and the reason the slot's type is untouched: a list changing under the
    /// cursor shifts the index, and what it lands on is still a row.
    pub(crate) todos_sel: usize,
    /// **Where each stop was DRAWN, parallel to [`App::todos_stops`]** — the pane's third value in
    /// leticl's `todos-lines`, recorded as the rows go out and never recomputed.
    ///
    /// This is not a cache of arithmetic that could be done at the call site; the arithmetic is the
    /// defect. leticl's docstring: *"the second is an `aref` of the third — never arithmetic over
    /// one of the three lists this draws from, which is what put the pane four lines above the row
    /// it was scrolling to."* Two of the operator's reports came from exactly that, and it is also
    /// what makes a click possible at all: a click has a screen row and nothing else, and the only
    /// honest answer to *which stop is on this row* is the one the pane wrote down while drawing.
    pub(crate) todos_stop_rows: Vec<usize>,
    /// **The screen row the pane's own first body row goes to** — the session header, when the
    /// frame is tall enough to have one. Recorded rather than assumed because a click's `y` is in
    /// absolute screen coordinates and the header above the pane is not part of it.
    pub(crate) todos_pane_top: usize,
    pub(crate) repo_sel: usize,
    pub(crate) repo_open: bool,
    /// **How far the open pane is scrolled**, in rows hidden above it.
    ///
    /// Every pane drew `rows.truncate(room)` and the scroll keys were swallowed
    /// while one was open — so anything past the terminal's height was
    /// unreachable, not merely off-screen. `leticl`'s TODO.md renders 98 rows;
    /// on a 40-row terminal more than half of it could not be looked at, and the
    /// cursor ↑↓ moves could walk into rows that are never drawn.
    ///
    /// One field for all of them: only one pane is open at a time, and the
    /// alternative is six of these that each go stale separately.
    /// **A slash reply that is a listing, not a sentence.**
    ///
    /// Feedback for `/…` arrives as `Warning { code: "slash" }`, and a head that
    /// renders every warning as a note put `/job j89`'s SIXTEEN KILOBYTES of
    /// command output straight into the conversation scrollback — between the
    /// model's turns, with the subprocess's own ANSI in it. The operator, looking
    /// at it: *"it returned the output in the main conversation window wtf"*.
    ///
    /// A one-line confirmation is still a note; those read well there and a pane
    /// for them would be a keystroke to dismiss nothing. Anything longer is a
    /// listing, and a listing belongs on a screen you open and close. `(title,
    /// lines)`, `None` when the pane is shut.
    pub(crate) slash_out: Option<(String, Vec<String>)>,
    pub(crate) pane_scroll: usize,
    /// What the last draw of a pane measured: how many rows it had, and how many
    /// fitted. Kept so a cursor moved by a keypress can scroll itself into view —
    /// the key handler has no width or height of its own.
    pub(crate) pane_len: usize,
    pub(crate) pane_room: usize,
    /// Which pane row the repo's first queue row is drawn at.
    ///
    /// `repo_sel` counts the repo's own rows; `pane_scroll` counts the pane's,
    /// which start with a title, the model's live list and two labels. Passing
    /// one where the other was meant scrolled to the wrong place and left the
    /// cursor off screen — recorded at draw time rather than derived, because
    /// the header's height depends on how many todos the model has written.
    /// **How far into an unfolded row's payload the reader has paged.**
    ///
    /// A tool result is a *logical* string — a `read` of a large file, a build log — that
    /// wraps to thousands of display lines of which the window shows a few dozen. The
    /// fold drew its first lines and reported the rest as `… +N lines · ctrl-t`, and
    /// **ctrl-t revealed nothing further**: it changed which rows were allowed to be long
    /// (`tools.is_open()`), not how much of one row was drawn. So the rest of a 418 KB
    /// payload was unreachable. The operator, 2026-09-20: *"a row is a typical editor
    /// problem of logical strings vs display"*.
    ///
    /// This is the window into that string. It counts **wrapped display lines** from the
    /// head of the payload, because that is what the reader is scrolling through, and it
    /// is clamped against the payload's own length at draw time.
    pub(crate) payload_page: usize,
    /// **The furthest `payload_page` that still shows a full window**, written by the draw
    /// — the only place that knows the payload's wrapped length — and read by the keys to
    /// clamp. Without it Down kept adding past the end while the screen stood still, and
    /// Up then had to unwind every invisible step before anything moved: the operator's
    /// *"couldnt scroll bottom anymore - only esc worked"*. `usize::MAX` until drawn.
    pub(crate) payload_max: std::cell::Cell<usize>,
    /// The key that asked, so the arrows page **only the row whose view is open**.
    ///
    /// Without it, Up/Down inside an open payload would move the transcript, or every
    /// open row at once — and there can be several open rows on one screen. The panel
    /// says `▸ paging <subject>` so the reader can see which one the arrows are on.
    pub(crate) payload_sel: Option<String>,
    /// **What this conversation has cost, in micro-USD**, summed over the turns
    /// this head has seen finish.
    ///
    /// A per-turn figure is gone by the next turn; what somebody running a
    /// metered model wants is the running total. Only turns this head watched
    /// are in it — a head that attached late says so rather than inventing the
    /// earlier ones, because the alternative is a total that is wrong in the
    /// direction that costs money.
    pub(crate) spent_micros: u64,
    /// Whether any turn this head saw carried a cost at all. Distinguishes "free,
    /// so nothing to show" from "metered and nothing has finished yet".
    pub(crate) spent_seen: bool,
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
    pub(crate) stats: bool,
    pub(crate) quit: bool,
    /// Set whenever a full repaint is wanted regardless of the diff.
    pub(crate) redraw: bool,
    /// **The view is held** (R56). While it is, the head writes nothing at all, so a mouse
    /// selection survives a streaming turn. See [`App::toggle_hold`] for the contract.
    ///
    /// The events keep arriving and the head keeps folding them — only the drawing stops.
    pub(crate) hold: bool,
    /// **The frozen frame**, composed once when the hold began (with the marker on it, which
    /// is the one write the freeze owes) and returned byte for byte thereafter, so the
    /// terminal's own diff produces no bytes at all.
    pub(crate) hold_frame: Option<Vec<String>>,
    /// The size `hold_frame` was composed for. A resize moves every row, so the freeze owes
    /// exactly one more frame there.
    pub(crate) hold_size: (usize, usize),
    /// `items.len()` when the hold began, so the release can say how much arrived while it
    /// was held — counted ONCE, at that moment, because a live count is an animation and an
    /// animation is writes.
    pub(crate) hold_rows: usize,
    /// **Which conversations the picker has EXPANDED** — a parent's session id, whose sub-sessions
    /// are being shown under it.
    ///
    /// **Empty is the default, and that default is the answer to the objection that kept
    /// sub-sessions out of this list altogether**: *twenty subagents bury the four conversations I
    /// care about*. Collapsed shows exactly what the picker showed before children were listed —
    /// so nothing is hidden that was not hidden, and nothing the daemon told us is thrown away.
    /// See [`App::session_rows`].
    pub(crate) expanded: Vec<String>,
    /// Wall clock, fed in by the driver, and when this head last had anything from
    /// the daemon.
    ///
    /// **Received-at, not the event's `ts`.** The difference is what a stall is,
    /// and taking it from the event's own clock would measure the daemon's opinion
    /// of how long it had been quiet — which is exactly the number that is missing
    /// when the daemon has stopped talking. Zero means nobody has told this head
    /// what time it is, and then it says nothing about stalls rather than guessing.
    pub(crate) now_ms: u64,
    pub(crate) last_event_at: u64,
    /// **The workspace's branch, or `None`** — see `crate::gitfield`. Read by the driver's tick
    /// (a process, never a paint), drawn beside the workspace path, and `None` when the directory
    /// is not a repository this head can read: an absence, not a clean tree.
    /// **The workspace's git field, as the pieces the format in force asks for** — leticl's
    /// `*git-cache*` half: `(text, role)` pairs, rendered by the header and painted per role.
    /// FITTING happens at the draw, where the width is; the branch is the floor and the marks
    /// fall off the right (`gitfield::git_fit`).
    pub git: Option<Vec<(String, crate::gitfield::GitRole)>>,
    /// **The reading the pieces were rendered from** — the FACTS, not the text, so a format
    /// changed on `/config` re-renders from the cache rather than waiting out the reader's
    /// interval. leticl caches state and pieces for the same reason.
    pub(crate) git_state: Option<crate::gitfield::GitState>,
    /// **The git field's template, as loaded** — `None` is the shipped default
    /// (`gitfield::GIT_FORMAT_DEFAULT`); `Some(t)` is the operator's `git_format`. Held on the
    /// App because the field renders on the reader thread (`refresh_git`), long after
    /// `load_prefs`.
    pub(crate) git_format: Option<String>,
    /// **Which workspace that reading was of, and when.** A switch to another session carries
    /// another path, and a field left over from the previous tree would be drawn as this one's
    /// branch — the same class of lie as inventing one.
    pub(crate) git_read: (String, u64),
    /// **The live marker's join, as the pair that lets it be UNDONE.**
    ///
    /// The marker is glued to the end of the sentence that introduces the work — *"…the last
    /// two: [1 tool call] · ctrl-t opens it"* — by appending to a line in `hist_lines`, which is
    /// the cache of RENDERED rows. **A cached row that a derived overlay mutates is a row that
    /// cannot be recomputed**, and appending on every frame is what it did: the operator's screen
    /// showed the same marker three times on one line, because three frames had each added one and
    /// nothing invalidated the cache in between — no event had arrived, which is exactly what
    /// makes a settled frame cheap to draw.
    ///
    /// So the join is a SET rather than an APPEND. This holds the line's text *before* the marker
    /// was glued and the marker that was glued to it, and each frame restores the original first.
    /// `None` while nothing is joined, which is every frame with no work in flight.
    ///
    /// **Found by asserting that two renders of one state are the same frame** — see
    /// `two_renders_of_one_state_are_the_same_frame`, the property this field exists to keep.
    pub(crate) live_join: Option<(String, String)>,
    /// **Everything the marker draws about NOW, as ONE value** — and the same value is the
    /// cache key for the row it is painted into.
    ///
    /// The marker is baked into `hist_lines`, the cache of RENDERED rows, and that cache only
    /// rebuilds from the row something changed at — so the row is stale the moment any fact the
    /// marker drew moves. This kept being missed because the key was written *beside* the
    /// renderer in prose, and the renderer was free to read anything: `(calls, think_lines)`
    /// omitted `running` (the colour), and the counts omitted *which run* (five markers lit at
    /// once — *"look how many tools are yellow"*, because two rounds can carry identical
    /// numbers).
    ///
    /// **So the key and the renderer take the SAME value.** [`hidden_run_marker`] reads `calls`,
    /// `think_lines` and `running` out of a [`MarkerFacts`] and has no other door to now, so a
    /// fact the marker draws is a fact this key holds. See [`MarkerFacts`] for the field that
    /// makes the run part of it.
    pub(crate) marker_facts: MarkerFacts,
    /// The model this session is talking to, kept past the end of a turn.
    ///
    /// It lives on `TurnPane` because that is where the event carries it, and the
    /// composer's own line has to say what it is talking to when nothing is
    /// running — which is most of the time a person is looking at it. Since §4.4 it
    /// is also on `Hello`, so a head with no turn yet has an answer too.
    pub(crate) model: String,
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
    pub(crate) call_targets: std::collections::HashMap<String, String>,
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
    pub(crate) call_ms: std::collections::HashMap<String, u64>,
    /// Both sides of the file a settled `edit`/`write` row changed, by **item
    /// id** — carried across the takeover exactly as `call_ms` is, and for the
    /// same reason: the transcript row has the tool's prose and not the pair.
    ///
    /// This is what puts a diff on the screen at all. The two-panel view used to
    /// be drawn only by the LIVE card, and the transcript takes a call over the
    /// moment its result row lands — so the diff existed for the milliseconds
    /// between `ToolFinished` and `TranscriptAppended`, and the operator, who
    /// asked for it twice, reported *"nothing really shown"*. Seeded from the
    /// snapshot too, for the turn it carries: a restart is not a reason to lose
    /// the change (operator, 2026-09-17: *"past edits lose their diff panels"*).
    /// Absent for rows older than that — the view keeps one turn's calls — and
    /// the row then shows the tool's own text, which is the `Replayed` rule.
    pub(crate) call_edits: std::collections::HashMap<String, letibot_sessionlog::event::ToolEdit>,
    /// The settled decision a `tool_result` row's call was gated by, by **item id**
    /// — carried across the takeover exactly as `call_ms` and `call_edits` are, and
    /// for the same reason: the transcript row has the tool's prose and not the
    /// approval, and the call id it carries is round-positional, so it cannot be the
    /// key.
    ///
    /// This is what puts the oracle's brief and reply on a card that has settled
    /// into the transcript. The live card shows it while the turn is the pane's;
    /// the moment the result row lands the transcript takes the call over, and
    /// without this the approval — and what the oracle was shown and said back —
    /// leaves the screen with the card. Seeded from the snapshot too, for the
    /// decisions it carries.
    pub(crate) call_decisions:
        std::collections::HashMap<String, letibot_sessionlog::view::SettledDecision>,
    /// The total body length of the last frame, so `Up` can be clamped to it.
    pub(crate) body_len: usize,
    /// **The protocol version the daemon last said it speaks**, from the `Hello`.
    ///
    /// `None` until a daemon has answered, which is a different statement from "it
    /// speaks 0". Worth keeping rather than comparing inline on `Hello` for two reasons:
    /// `/status` has to be able to say it *after* the fact — the handshake is one frame
    /// and the question "which build is on the other end of this socket" is asked hours
    /// later — and a head that switches sessions re-reads a `Hello` from the same
    /// daemon, so this is a fact about the connection and not about the attach.
    ///
    /// The comparison itself is [`letibot_sessionlog::protocol_skew`], which is in
    /// `sessionlog` rather than here because the sentence belongs to the protocol and
    /// every head has to say the same one.
    pub(crate) daemon_protocol: Option<u32>,
    /// **Which daemon this head is drawing the picture of.**
    ///
    /// Two facts, and both are about the connection rather than about the attach: the process at
    /// the other end of the socket, from `SO_PEERCRED` ([`App::set_daemon_pid`], re-read on every
    /// reconnect), and the build's `PROTOCOL_VERSION`, which rides the `Hello`. The pid is the
    /// kernel's own answer for *this socket*, which is what makes it an identity rather than a
    /// guess — a number read out of a file could be a predecessor's, and R30 already argued that
    /// through for `/status`.
    ///
    /// # Why a head needs one at all
    ///
    /// The operator's box, in their words: three `letibot-tui` processes alive (ages 15d, 1d18h,
    /// 21h) while the daemon was replaced this afternoon. **A head outlives the daemon that gave
    /// it its facts**, and the registry is in memory — so a daemon that comes back is not the one
    /// whose answers are still on the screen, and the head went on drawing them without ever
    /// saying that the party it was talking to had changed.
    ///
    /// **What the head does about it is the same refetch it owes after any seating**
    /// ([`App::refetch_session_facts`]) plus the sentence below — it cannot re-attach itself, and
    /// does not need to: the socket dying is what makes the driver reconnect, and the `Hello`
    /// that answers the reconnect is this arm. What was missing was *noticing*, and noticing is
    /// what turns a silent stale picture into a named one.
    ///
    /// `None` until a daemon has answered, which is a different statement from *pid unknown* —
    /// the kernel declines to name a peer on some platforms, and `Some(DaemonSeat { pid: None,
    /// .. })` is that case rather than this one.
    pub(crate) daemon_seat: Option<DaemonSeat>,
    /// **The daemon connection, as far as this head can tell.** See [`Link`].
    ///
    /// Kept on the head rather than in the driver because it is a fact the *screen*
    /// shows: a head with no daemon draws the conversation it has, plus a line saying
    /// the connection is down and for how long.
    pub(crate) link: Link,
    /// True between taking the screen and the daemon's `Hello` arriving.
    ///
    /// The `Hello` **carries the whole snapshot**, so `HeadClient::attach` is a round
    /// trip that can take a fifth of a second on a busy daemon and longer on a big
    /// session. A head that draws before it has that answer must not claim anything
    /// about the session — the empty-transcript banner says *"this session has said
    /// nothing yet"*, which is a different and false thing from *"I have not been
    /// told yet"*. So this suppresses the banner, and the body becomes the walking
    /// cat (see [`cat_frame`]), which is what says "working on it" without saying
    /// anything about the session.
    pub(crate) attaching: bool,
    /// When the attach began, on the clock `App::clock` is given.
    ///
    /// The cat's frame comes from the **elapsed** time rather than from a counter,
    /// so `screen()` is a pure function of the clock — which is what lets the
    /// pre-attach wait be driven from anywhere (a loop, a test, a future
    /// background-thread handshake) without the renderer knowing which.
    pub(crate) attach_started_ms: u64,
    /// Where the terminal's caret belongs, from the last frame.
    pub(crate) cursor: Option<(usize, usize)>,
    /// The daemon's reason for ending this head, kept past the screen. See
    /// [`App::farewell`].
    pub(crate) bye: Option<String>,
    /// **The daemon's pid**, from `SO_PEERCRED` on this head's connection (R30). Set by
    /// the caller that owns the socket, kept so `/status` can answer the question the
    /// operator would otherwise take to `ps` — which is how the orphan this rule exists for
    /// was found, a day late.
    pub(crate) daemon_pid: Option<i32>,
    /// **What the reader's viewport is holding** (R36).
    ///
    /// `None` is the *following* state — the head of a transcript being read from its tail
    /// — and it is the default. `Some` is a reader who scrolled back: they have said they
    /// are reading something, and nothing arriving below may move it.
    ///
    /// **A row and an offset into it, never a line count.** A count from the bottom is
    /// invalidated by every arrival; a count from the top by anything above being
    /// rewritten; and both happen here, because a snapshot replaces the transcript whole
    /// and an elision changes a row's height. See [`App::hold`].
    pub(crate) anchor: Option<Held>,
    /// **Where each rendered row's lines are**, ascending by `at`. Rebuilt as the history
    /// is walked and prepended to, cleared whenever that buffer is thrown away.
    pub(crate) spans: Vec<Span>,
    /// **The body line at the top of the last frame's window**, and how many lines it had.
    ///
    /// The key handler runs between frames and has to answer *where is the reader looking*
    /// with what the last frame actually drew — the same rule `dec_content_room` follows
    /// for the decision card. A scroll that computed its own position from the model
    /// rather than from the glass would be a second opinion about the reader's screen.
    pub(crate) view_top: usize,
    pub(crate) view_room: usize,
    /// **Names this head's door calls so the daemon can tell them apart.**
    ///
    /// The `call_id` is `{head}-{n}` and the count is per head, which is what makes it
    /// unique within the session — the only property the daemon's pending set needs. A
    /// second head's `h3-1` is a different call, and the daemon's set is keyed on the string.
    pub(crate) head_run_seq: u64,
    /// **The echoes a snapshot could not resolve** (R16's third mark).
    ///
    /// `pending_prompts` asserts something about the DAEMON — *you owe me a row for
    /// this* — and after a snapshot replaces the transcript the head cannot support
    /// that claim for an echo the snapshot does not carry. Either the row is still
    /// coming or it was replaced by a fork, and **from the head both look the same**.
    /// So the echo stops claiming `queued` and says `unconfirmed`, which is the claim
    /// it can actually support, and it retires the ordinary way when a row does land.
    ///
    /// The set is the *marked* ones and it is keyed by the echo's text, which is what
    /// `pending_prompts` is keyed by. Retirement is an **intersection**, not a removal
    /// of the landing row's text — see [`App::retire_pending`].
    pub(crate) unconfirmed: Vec<String>,
    /// **Whether an echo is drawn in full or as its elided headline** (R33).
    ///
    /// Folded by default, and flipped by `/t` — the head's *unfold the long rows*
    /// verb. One key for one idea: a reader who wants the long things shown whole asks
    /// once and gets them all, rather than learning a third chord for a third kind of
    /// row.
    pub(crate) echo_open: bool,
    /// **A stop this head asked for and has not finished.** R30. `Some` from the moment
    /// the frame goes out until the daemon is gone or the deadline has passed — and while
    /// it is `Some` and unresolved, [`App::should_quit`] is false, which is the whole of
    /// the requirement: *the head does not exit until the daemon has actually gone, or
    /// until it can say that it has not.*
    pub(crate) stopping: Option<Stopping>,
    /// **A bulk announcement the daemon has not filled yet.**
    ///
    /// Recorded **only when a snapshot is ingested** — never by a live
    /// `TranscriptAppended`. That is the whole point of it: a live row is body-less for
    /// the R2 window of *every ordinary message*, so a trigger built on "some row lacks a
    /// body" fires on a healthy session and announces a carry that is not happening. A
    /// snapshot's rows are a **bulk** announcement — a fork, a reseat, a resume, an import
    /// — and a live append is not, so the shape of the evidence separates the two with no
    /// threshold. See [`Bulk`].
    pub(crate) bulk: Option<Bulk>,
    /// **A fill the daemon named** ([`SessionEvent::Filling`](letibot_sessionlog::SessionEvent::Filling)):
    /// `what`, `unit`, `done`, `total`, or `None` when nothing is running. Cleared the
    /// moment `done >= total`, because the finish is a durable note, not a line that
    /// stays. See [`filling_line`] for why this is the daemon's count and not a count of
    /// the rows still lacking a body.
    pub(crate) filling: Option<(String, String, u64, u64)>,
    /// **A fold's long wait** ([`SessionEvent::CompactionProgress`](letibot_sessionlog::SessionEvent::CompactionProgress)),
    /// in the compaction's own units.
    ///
    /// **A field of its own rather than a second use of `turn.progress`, and that
    /// separation is the fix.** The overrun compaction summarises a SCRATCH transcript;
    /// when its `PromptProgress` was forwarded as itself it landed in `turn.progress`,
    /// which is the SESSION's turn, so the scratch prompt's token count was drawn as the
    /// session's context — the operator watched `69k` sit over a 240k conversation that
    /// had not changed (2026-09-20). Nothing here can be confused with the session's
    /// figures however alike they look, because nothing else writes this field.
    pub(crate) compacting: Option<CompactionLine>,
}

/// One compaction half, as the daemon reports it.
#[derive(Debug, Clone)]
pub(crate) struct CompactionLine {
    pub(crate) half: u64,
    pub(crate) halves: u64,
    pub(crate) prompt_tokens: u64,
    pub(crate) processed: u64,
    pub(crate) written: u64,
    /// What `written` counts — `tokens` or `chars`, the daemon's word, because the two
    /// transports do not report the same thing.
    pub(crate) unit: String,
}

/// **A bulk announcement: the ids a snapshot carried with no body, and when it landed.**
///
/// The evidence, not a symptom. A snapshot that arrives full of body-less rows is a
/// *carry* — a fork, a reseat, a resume, an import, or an attach to a daemon mid-carry —
/// and this is how the head knows that, because it recorded it at the moment of ingestion.
/// A live `TranscriptAppended` never creates one: it is the R2 window of an ordinary
/// message, which is why *"some row lacks a body"* was the wrong trigger.
#[derive(Debug, Clone)]
pub(crate) struct Bulk {
    /// The ids the snapshot announced with no body. A body landing removes its id; an
    /// empty set means the announcement is complete and the trigger clears itself.
    pub(crate) ids: std::collections::HashSet<String>,
    /// When the snapshot landed, on this head's clock — [`BODY_PATIENCE`]'s origin.
    pub(crate) at_ms: u64,
}

/// **The two words an echo can carry** (R16). Constants because both the renderer and
/// the tests name them, and a mark that is spelled twice is a mark that can be spelled
/// differently.
///
/// `queued` is a claim about the DAEMON's queue — *you owe me a row for this*. It is a
/// claim the head can make for an echo it has just sent and has not seen land.
///
/// `unconfirmed` is the honest one after a snapshot has replaced the transcript: the
/// head can no longer tell *still coming* from *replaced by a fork*, so it stops
/// asserting the first. See [`App::unconfirmed`].
pub const QUEUED: &str = "queued";
pub const UNCONFIRMED: &str = "unconfirmed";

/// **The one sentence a held view says** (R56), in the words the two heads agreed on, because an
/// operator who learns it on one head reaches for it on the other. It takes the hint bar's row —
/// the row that already talks about keys — and it names the key that undoes the hold, which is
/// R29's rule for a disclosure: it carries the act that ends it.
pub const HOLD_MARKER: &str = "⏸ the view is held — ctrl-p follows again";

/// The columns the live row puts between two candidates — `  ·  `, as [`App::completions_line`]
/// joins them. A constant because the fit arithmetic in `shell_completions_line` has to count
/// what the join will actually spend, and a number written twice is a number that drifts.
pub(crate) const SEPARATOR_COLS: usize = 5;

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
pub(crate) const CELLS_OPEN: &str = "\u{27e6}screen ";
pub(crate) const CELLS_MARK_END: &str = "\u{27e7}";
pub(crate) const CELLS_CLOSE: &str = "\u{27e6}end screen\u{27e7}";

/// **Every verb the dispatcher acts on, checked against [`SLASH_COMMANDS`].**
///
/// # Why a test and not a derivation
///
/// The requirement is that the completion table and the dispatcher must not be two lists
/// that agree by maintenance. In Rust a `match` is not reflectable, so the two possible
/// mechanisms are *one derived from the other* (unavailable) and *a test that fails when
/// they diverge* (this). It reads the source of [`App::command`] and names every verb it
/// finds, so adding an arm without listing it fails the suite rather than becoming a verb
/// nobody is offered.
///
/// **What it deliberately does not check.** The one-letter and short spellings (`?`, `h`,
/// `q`, `r`, `s`, `t`, `v`, `i`) are *aliases*: the table's own comment says offering both
/// spellings doubles the list to teach the same actions, and the dispatcher keeps taking
/// them. And the daemon's verbs are not this head's to enumerate — they arrive on a
/// `SettingRow` (`daemon.verbs`), because a head that guessed at them is precisely how
/// `/gate` and `/flowy` came to be missing while working perfectly.

impl App {
    pub fn new(cfg: RenderConfig) -> Self {
        App {
            cfg,
            visibility: Visibility::starting(),
            diff_split: true,
            config_pane: false,
            config_sel: 0,
            prefs_path: None,
            settings: Vec::new(),
            term: None,
            term_fact: PaneFact::Unasked,
            term_ask: None,
            close_pending: false,
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
            shell_ask: std::collections::HashMap::new(),
            shell_suggestions: std::collections::HashMap::new(),
            shell_ask_seq: 0,
            shell_model: None,
            path_matches: None,
            shell_candidates_memo: None,
            shell_walks: 0,
            queued: Vec::new(),
            pending_prompts: Vec::new(),
            bound_prompts: std::collections::HashMap::new(),
            fork_pending: Vec::new(),
            want_new_session: false,
            usage: None,
            usage_cache_measured: true,
            last_timings: None,
            items: Vec::new(),
            hist_lines: Vec::new(),
            hist_upto: 0,
            hist_floor: 0,
            model_from_settings_at: 0,
            model_from_turn_at: 0,
            walk_limit: SELF_WALK_LIMIT,
            hist_first_class: None,
            hist_marks: Vec::new(),
            note_upto: 0,
            hist_width: 0,
            hist_class: None,
            turn: None,
            open: Vec::new(),
            secret: None,
            secret_buf: String::new(),
            key_secrets: Vec::new(),
            features: crate::backend::features::Features::default(),
            focused: None,
            light_background: None,
            attention: None,
            clipboard_out: None,
            images_sent: std::collections::HashMap::new(),
            images_box: 0,
            images_scanned: 0,
            image_uploads: Vec::new(),
            prompt: None,
            prompt_buf: String::new(),
            key_ask: None,
            key_buf: String::new(),
            screen_requests: Vec::new(),
            term_cols: 0,
            sel: 0,
            notes: Vec::new(),
            dec_scroll: 0,
            dec_scroll_for: String::new(),
            dec_content_len: 0,
            dec_content_room: 0,
            dismissed: Vec::new(),
            heads: 0,
            hist_renders: 0,
            seq: 0,
            dropped: 0,
            scrubbed: 0,
            resyncs: 0,
            rendered: 0,
            filtered: 0,
            unreadable: 0,
            gaps: 0,
            behind: 0,
            orphan_bodies: 0,
            slow_first_byte: 0,
            acked: Counters::default(),
            scroll: 0,
            editor: Editor::new(),
            model: String::new(),
            call_targets: std::collections::HashMap::new(),
            call_ms: std::collections::HashMap::new(),
            call_edits: std::collections::HashMap::new(),
            call_decisions: std::collections::HashMap::new(),
            reasoning: Fold::Folded,
            tools: Fold::Folded,
            raw_calls: false,
            notice: None,
            notice_until: None,
            help: false,
            picker: false,
            pick: None,
            mode_confirm: None,
            todo_draft: None,
            todo_template: crate::prefs::TodoTemplate::Off,
            todo_seed: Vec::new(),
            todo_seed_pending: false,
            quit_card: false,
            quit_sel: 0,
            mode_sel: 0,
            pick_unseeded: false,
            mode_rows_drawn: 0,
            mode_first_row: 0,
            todos_pane: false,
            subagents_pane: false,
            jobs_pane: false,
            queue_pane: false,
            merge: Vec::new(),
            merge_reviews: Vec::new(),
            queue_sel: 0,
            queue_stop_rows: Vec::new(),
            queue_pane_top: 0,
            queue_open: None,
            jobs_sel: 0,
            jobs_finished_open: false,
            jobs_stop_rows: Vec::new(),
            subagents_sel: 0,
            subagents_finished_open: false,
            subagents_stop_rows: Vec::new(),
            sub_out: None,
            sub_out_pending: None,
            up_from: None,
            job_out: None,
            todos: Vec::new(),
            repo_todos: None,
            repo_todos_at: None,
            todos_sel: 0,
            todos_stop_rows: Vec::new(),
            todos_pane_top: 0,
            repo_sel: 0,
            repo_open: false,
            slash_out: None,
            pane_scroll: 0,
            pane_len: 0,
            pane_room: 0,
            payload_page: 0,
            payload_max: std::cell::Cell::new(usize::MAX),
            payload_sel: None,
            spent_micros: 0,
            spent_seen: false,
            stats: false,
            quit: false,
            redraw: false,
            hold: false,
            hold_frame: None,
            hold_size: (0, 0),
            hold_rows: 0,
            expanded: Vec::new(),
            now_ms: 0,
            last_event_at: 0,
            git: None,
            git_state: None,
            git_format: None,
            git_read: (String::new(), 0),
            live_join: None,
            marker_facts: MarkerFacts::default(),
            body_len: 0,
            attaching: false,
            link: Link::Attached,
            daemon_protocol: None,
            daemon_seat: None,
            attach_started_ms: 0,
            cursor: None,
            bulk: None,
            filling: None,
            compacting: None,
            bye: None,
            daemon_pid: None,
            unconfirmed: Vec::new(),
            anchor: None,
            spans: Vec::new(),
            view_top: 0,
            view_room: 0,
            head_run_seq: 0,
            echo_open: false,
            stopping: None,
        }
    }

    /// Tell the head what time it is. The driver calls this once a tick; nothing
    /// else in `App` reads a clock, so a test drives time by hand.
    pub fn clock(&mut self, now_ms: u64) {
        self.now_ms = now_ms;
    }

    /// Whether the head wants the terminal repainted from scratch (Ctrl-L, or a
    /// fold that changed every cached line). Reading it clears it.
    ///
    /// **Not the scroll keys.** A moved viewport is a diff rather than an erase —
    /// every row the window slid is a row whose text differs, and the encoder writes
    /// exactly those — so the flag on the scroll path bought a whole-screen erase per
    /// wheel notch and nothing else. See [`App::hold`].
    pub fn take_redraw(&mut self) -> bool {
        // **A held view does not let the glass be thrown away** (R56). A `true` here makes the
        // driver call `Terminal::invalidate`, which forces a full repaint — and a full repaint is
        // bytes, the one thing the hold exists to prevent. The flag is LEFT SET, so the release
        // spends it exactly once and the screen comes back whole.
        if self.hold {
            return false;
        }
        std::mem::take(&mut self.redraw)
    }

    pub fn should_quit(&self) -> bool {
        // **A head that asked the daemon to stop does not leave until it knows.** R30.
        //
        // `quit` is the operator's answer — leave — and it was the whole of the old
        // condition, which is why the head was gone while `harnessd` was still at
        // `PPID 1`. This is the head's obligation to go with it: the question is not
        // answered until the daemon has gone or the deadline has passed, and until then
        // there is nothing honest for this to return but `false`.
        //
        // **A stop this head asked for outranks a `Bye`, and that is a correction.**
        //
        // The line above used to read *"a `Bye` still ends everything: the daemon saying
        // goodbye is the daemon going"*, and that is true of every `Bye` **but the one a
        // stop produces**. That one is not the daemon going: it is published by
        // `registry.close()` on the **connection thread**, the moment the request is
        // taken, and the worker that is running the operator's command has not ended yet.
        // MEASURED on a live daemon, 2026-10-06: the `Bye` arrives **519 µs** after the
        // stop goes out, the daemon's process is still in `/proc` at that moment, and it
        // stays there for as long as the run holds the worker.
        //
        // So a head that left on it reported a stop that had not happened. The operator's
        // words: *"it reports the server exited within a second — while `harnessd` is in
        // fact hung and has to be killed with `--force`."* The head has the one
        // observation that is a fact about the **process** — `watch_stop`'s `gone`, read
        // from `/proc` and from `waitpid` — and this is the check that stops it being
        // overruled by a frame from a connection that is still open.
        //
        // `bye` is not discarded, and nothing else changes about it: it is still the end
        // of the conversation for a head that did **not** ask (a skew, a refusal, another
        // head's stop), and `should_quit` still returns `true` for those at once. What it
        // no longer is, is an answer to a question this head asked and has not had
        // answered.
        if self
            .stopping
            .as_ref()
            .is_some_and(|s| !s.resolved(self.now_ms))
        {
            return false;
        }
        if self.bye.is_some() {
            return true;
        }
        self.quit
    }

    /// **Ask for the next frame to be rebuilt.** The driver is a separate file and
    /// mutates the head's state directly (R30's four observations), so it needs one way to
    /// say *this changed, draw again* — the same flag every internal writer sets, exposed
    /// rather than kept private.
    pub fn mark_redraw(&mut self) {
        self.redraw = true;
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Actions a *frame* produced, for the driver to send. Empty almost always.
    pub fn take_actions(&mut self) -> Vec<Action> {
        std::mem::take(&mut self.queued)
    }

    /// Screen requests to answer with the frame just drawn. Drains.
    pub fn take_screen_requests(&mut self) -> Vec<String> {
        std::mem::take(&mut self.screen_requests)
    }

    /// **Is the model GENERATING right now?** The state name, and only that.
    ///
    /// This is the narrow question and it is almost never the one a caller means. Read
    /// [`App::turn_busy`] first: `TurnFinished` fires per ROUND, so this goes false the instant a
    /// round's generation ends — **which is precisely when a tool call starts.** A reader asking
    /// *is the model working* who reaches for this gets *no* for the whole of every command.
    ///
    /// It is kept because one caller genuinely asks the generating question: [`App::stuck_line`],
    /// which reports silence from a model that should be emitting. A call that runs for two
    /// minutes emits nothing and is not stuck, and gating that line on `turn_busy` would make it
    /// cry wolf through every long command.
    pub(crate) fn turn_generating(&self) -> bool {
        matches!(
            self.turn.as_ref().and_then(|t| t.state.as_ref()),
            Some(TurnState::Running)
        )
    }

    /// **Is the model WORKING — generating, or waiting on a call it made?**
    ///
    /// This is R51's `turn-busy-p`, and it is the question four of this head's call sites were
    /// silently asking with the state name instead. Measured on 2026-09-25 with a `sleep 60`
    /// executing, the daemon's own view reads:
    ///
    /// ```text
    /// (:TURN-STATE "finished"  :CALLS (("call_…" "running")))
    /// ```
    ///
    /// — *generating* is false and the turn is plainly working, so **every question of the form
    /// "is the model busy" must ask the CALLS.** The fact is: generating, OR any call of this
    /// turn has not finished.
    ///
    /// **What it is not.** It is not "a call exists" and not "a call is running": a call that has
    /// finished is history, and the calls of earlier rounds are in `calls` until the pane stands
    /// down. It also says nothing about whether the turn as a whole is over — `TurnFinished`
    /// carries a round's end, so for the few milliseconds between a last call finishing and the
    /// next round's `TurnStarted` this answers *not busy* over a turn that is not finished. That
    /// window is R51 §3's recorded limitation and it needs a daemon fact (*the prompt is over*)
    /// that neither head has; it is not something this predicate can close.
    ///
    /// **One definition, because this colour and this line have now been wrong in four
    /// directions** — see the call sites: the esc-esc gate dead exactly while a command ran, two
    /// `queued` rows for one message, the promote message saying the wrong one of two silences,
    /// and a status row that vanished during every call.
    pub(crate) fn turn_busy(&self) -> bool {
        let Some(t) = self.turn.as_ref() else {
            return false;
        };
        if matches!(t.state, Some(TurnState::Running)) {
            return true;
        }
        t.calls
            .iter()
            .any(|c| !matches!(c.state, CallState::Finished { .. }))
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
}

pub(crate) fn open_call<'a>(calls: &'a mut [CallRow], call_id: &str) -> Option<&'a mut CallRow> {
    calls
        .iter_mut()
        .rev()
        .find(|c| c.call_id == call_id && !matches!(c.state, CallState::Finished { .. }))
}

/// **The marker: the two counts, and nothing else** — R37 AMENDED, final shape.
///
/// The operator, having seen it built: *"I also now understand i wat to keep only `[<n> tool
/// calls, <m> thinking lines]`, right after `:`"*. So the verbs, the distinct targets and the
/// whole question of a summary line are **superseded** — they were a question asked and
/// answered, and the answer is that a marker with prose on both sides needs to carry
/// neither. The turn's shape is narration → work → report, and the counts are the only fact
/// the two neighbours do not already give.
///
/// # It is punctuation inside a sentence, not an entry in a list
///
/// The same message: *"so I do want to read it as a prose … in a way … but structured."*
/// Its position is therefore fixed — **glued to the end of the narration line that points at
/// the work**, with a space between the colon and the bracket:
///
/// ```text
/// …and the one where R22's arithmetic has to give: [11 tool calls, 246 thinking lines]
/// ```
///
/// The structure is what the brackets and the counts GIVE that sentence; it is not something
/// imposed on it by a row. **A marker drawn as its own row fails this test even when its
/// text is correct** — which is exactly what the eight-marker screen was. So the two walks
/// do not emit this as a line of its own; they append it to the last line of the row above,
/// and only when that row is [`RowClass::Speech`] — the class this file already has for
/// *prose the reader can see*. A run with no prose above it (the first row of a transcript, a
/// tail walk that starts inside one) has nothing to continue, and then it stands alone:
/// counts with no sentence are still the fact, and a marker that vanished would be the
/// elision this document refuses.
///

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

/// **One pending echo against one landing row**: what of the echo is still owed.
///
/// `None` when the row says nothing about this echo — no whole line of it is a whole
/// piece of the echo. `Some("")` when the echo is fully accounted for. `Some(rest)`
/// when a run of its lines landed and the rest has not.
///
/// `claimed` and `cursor` are the row's lines, spent across the whole queue in one
/// call: each line of the row answers at most one piece, and only in the order the
/// queue holds them, which is the order the daemon appended them in. See
/// [`App::retire_pending`] for why the unit is a line and why the two guards —
/// whole-line equality and no going backwards — are what make it safe.
pub(crate) fn strip_landed(
    entry: &str,
    lines: &[&str],
    claimed: &mut [bool],
    cursor: &mut usize,
) -> Option<String> {
    let pieces: Vec<&str> = entry.split('\n').collect();
    let mut kept: Vec<&str> = Vec::with_capacity(pieces.len());
    let mut hit = false;
    for piece in &pieces {
        // **A blank line is not a claim.** It carries no words, so it can say
        // nothing about whether a prompt landed — and two prompts that differ only
        // in blank lines would otherwise retire each other.
        if piece.is_empty() {
            kept.push(piece);
            continue;
        }
        match (*cursor..lines.len()).find(|k| !claimed[*k] && lines[*k] == *piece) {
            Some(k) => {
                claimed[k] = true;
                *cursor = k + 1;
                hit = true;
            }
            None => kept.push(piece),
        }
    }
    // **What is left is the WORDS still owed, not the blank scaffolding around them.**
    // The blank pieces are kept in the walk above (a blank line matches nothing, so it can
    // never be *claimed* and must not be dropped mid-compare), but they are not content:
    // an entry whose only remaining pieces are blank has had every word of it accounted
    // for, and joining them back would hand the caller a string that is truthy and empty
    // — so the echo would stay on the screen for the rest of the session showing nothing.
    // Found by replaying the operator's own rows through this rule
    // (`docs/evidence/queued-echoes-2026-09-23.py`), where the entry's tail was a blank.
    let rest = kept
        .iter()
        .filter(|p| !p.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    hit.then_some(rest)
}

/// **The queue as the tail must DRAW it** — R51 item 15, and it is `strip_landed`'s
/// read-only twin.
///
/// # The defect
///
/// A body-less `user` row is drawn from the echo this head bound to it ([`App::bound_prompts`]),
/// and the tail draws the rest of the queue. The two were kept apart by comparing WHOLE
/// STRINGS — the tail skipped a pending entry whose text was exactly one a row was drawing —
/// and **an entry that GREW after it was bound defeats that**: bound as `"A"`, it becomes
/// `"A\nB\nC"` as the operator keeps typing (the coalescing item 14 requires), so the text no
/// longer matches and the tail draws **the whole entry again** — `A` on the screen twice, once
/// in the row's own place and once in the queue below it.
///
/// # The rule
///
/// **The unit of drawing is the entry; the unit of claiming is the piece.** Each whole line a
/// bound row is drawing is spent once against the queue (the walk is [`strip_landed`]'s, so a
/// claim here and a claim by a landing row cannot disagree about what a claim IS), and what is
/// left of an entry is joined back and returned as ONE block.
///
/// Returns, per entry the tail still owes something for: **the index into `pending`, the
/// remainder to draw, and the ORIGINAL text whose drawing claimed it** (`None` when nothing did).
///
/// # Why the third field, which is not about drawing at all
///
/// The `unconfirmed` mark is a sentence about the prompt this head sent — *the snapshot replaced
/// the transcript, so I can no longer tell `still coming` from `replaced`* — and it is looked up
/// **by text**. The row above looks it up under the text IT is drawing (the bound one), so a tail
/// that looked it up under the ENTRY's text would answer differently for the same prompt the moment
/// the entry grew: `unconfirmed` on one row and `queued` on the next, which is two statements about
/// one fact. **The claiming text is returned so the remainder can carry the mark its own row
/// carries** — the row in the transcript's own place is the senior drawing, and the tail's
/// remainder is its tail.
pub(crate) fn unclaimed_prompts(
    pending: &[String],
    bound: &[(String, Vec<String>)],
) -> Vec<(usize, String, Option<String>)> {
    if pending.is_empty() {
        return Vec::new();
    }
    // One pass over the queue, in the order the daemon will append the rows — the same rule
    // `retire_pending` keeps, and for the same reason: an entry may not claim a line that an
    // earlier entry already claimed.
    //
    // Flattened into one line list with a flag per bound drawing, so the walk below can report
    // which drawing spent a line as well as that it did.
    let mut lines: Vec<&str> = Vec::new();
    for (_, text_lines) in bound {
        for l in text_lines {
            lines.push(l.as_str());
        }
    }
    // `owner[k]` is which bound drawing contributed line `k`.
    let mut owner: Vec<usize> = Vec::with_capacity(lines.len());
    for (b, (_, text_lines)) in bound.iter().enumerate() {
        for _ in text_lines {
            owner.push(b);
        }
    }
    let mut claimed = vec![false; lines.len()];
    let mut cursor = 0usize;
    let mut out: Vec<(usize, String, Option<String>)> = Vec::new();
    for (i, q) in pending.iter().enumerate() {
        // Recorded before the walk, because `strip_landed` moves the cursor past what it spent and
        // the first line this entry claimed is what says which row is drawing it.
        let before = cursor;
        let rest = strip_landed(q, &lines, &mut claimed, &mut cursor);
        // **The drawing that claimed the FIRST line of this entry.** Cursor order is queue order,
        // so the earliest claim is the one the entry's head belongs to; a later one is drawing a
        // line further down the same prompt.
        let by = (before..cursor)
            .find(|k| claimed[*k])
            .map(|k| owner[k])
            .and_then(|b| bound.get(b))
            .map(|(text, _)| text.clone());
        match rest {
            // Nothing of this entry is on screen in the row that bound it. Draw it whole.
            None => out.push((i, q.clone(), None)),
            // Every piece of it is. Nothing left to draw.
            Some(rest) if rest.is_empty() => {}
            // Some of it is drawn above; the rest is what the tail owes.
            Some(rest) => out.push((i, rest, by)),
        }
    }
    out
}

/// What kind of row this is, for the one question the layout asks about its
/// neighbours: does a blank line belong between them.
///
/// Activity rows **pack**. A run of tool cards is one block and reads as one; a
/// blank line between each of them was costing a third of the vertical budget to
/// separate things that are already separated by a glyph in the first column. Air
/// goes where the *kind* changes — around the question, around the answer, around
/// a warning — because that is where the reader's attention has to move.
///

/// Visible width of a rendered screen line. Re-exported so a test can assert the
/// screen fits.
pub fn line_width(s: &str) -> usize {
    visible_width(s)
}

/// **A provider key the picker is collecting, for a row this box holds no key behind.**
///
/// `choice` is the whole `PROVIDER/MODEL`, because Enter does both things in one verb —
/// `/models CHOICE --key K` stores the key (mode 600, the file the daemon reads) AND takes the
/// row, which is the round trip the typed spelling already is. `provider` is the name alone, for
/// the sentence the card asks with.
///
/// **Only asked when the daemon named the keyless row.** `models.keys` absent means *no
/// greening*, never *no keys* — an older daemon — and asking on its absence would block every
/// switch behind a prompt for a key the box may well hold.
#[derive(Debug, Clone)]
pub(crate) struct KeyAsk {
    pub(crate) choice: String,
    pub(crate) provider: String,
}

/// An open password request, as the head shows it.
#[derive(Debug, Clone)]
pub(crate) struct SecretAsk {
    pub(crate) req_id: String,
    pub(crate) prompt: String,
    pub(crate) command: String,
    pub(crate) deadline: u64,
}

/// **A command of the operator's own is waiting for an answer**, as the head shows it.
///
/// # It is not [`SecretAsk`] and it must not become it
///
/// The two are separate types, separate fields, separate buffers, separate actions and
/// separate frames — and the separation is the requirement rather than tidiness. A password
/// has its own path (`SUDO_ASKPASS`, a helper that attaches as an `askpass` head,
/// `ClientFrame::Secret`), and a prompt card is drawn **in the open**: what a person types
/// here is a line for a program's **stdin**, visible on the screen and unremarkable there.
/// A secret must never travel down it, and the cheapest way to keep that true is for the
/// two channels to have nothing in common to borrow — no masked buffer, no `secret` field,
/// no `Action::Secret`.
///
/// # What is on it
///
/// * `req_id` — what the answer is addressed to, so a stale card cannot answer a later
///   command. The daemon holds the open request and refuses one that is not.
/// * `command` — **the operator's own line, verbatim**. The daemon cannot know which
///   process in a pipeline asked (`sudo apt install mc` is three programs and the question
///   is the third one's), so the card names the one thing that is certain.
/// * `question` — **the last line the program wrote, to SHOW and never to decide on.**
///   `None` when it has written nothing at all, which is a real case (`! cat`, blocked
///   before its first byte): the card then says the command is waiting rather than showing
///   an empty line as if that were the question.
#[derive(Debug, Clone)]
pub(crate) struct PromptAsk {
    pub(crate) req_id: String,
    pub(crate) job: String,
    pub(crate) command: String,
    pub(crate) question: Option<String>,
}

/// **What this head believes about the session's pane** — the daemon's answer to
/// [`ClientFrame::TermStatus`](letibot_sessionlog::protocol::ClientFrame::TermStatus), as this
/// head holds it.
///
/// # Why it is a three-state and not an `Option`
///
/// `None` would mean two different things at once, and the difference decides whether a
/// `!term close` **asks or refuses**:
///
/// * **`Unasked`** — nobody has answered yet. The head has just attached, or switched, and the
///   question is in flight. A verb that read this as *no pane* would refuse to end a program
///   the operator can see, which is the one case the read exists for;
/// * **`None`** — the daemon said there is no live pane. That is a fact, and it is what makes
///   `!term close` a sentence rather than a card;
/// * **`Running(command)`** — the daemon said this is running in it, and it is what a
///   confirmation names.
///
/// **A fact about NOW, refreshed rather than accumulated.** It is set by the three frames that
/// can know ([`ServerFrame::TermStatus`], `TermAttached`, `TermEnded`), reset to `Unasked` on
/// every `Hello` — a head that has just been seated somewhere does not know what is in the
/// pane there — and never derived from anything durable, because there is nothing durable about
/// it: a detach is not an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PaneFact {
    /// Nobody has asked yet: the read is in flight, or this head has not attached.
    Unasked,
    /// The daemon said this session has no live pane.
    None,
    /// The daemon said this is running in it.
    Running(String),
}

/// **The confirmation that ends a pane** — the daemon's own question about killing something,
/// and never the program's question about itself.
///
/// # Why it is not the prompt card
///
/// `SessionEvent::PromptRequested`'s card answers **the program**: it is a line written into a
/// pipe the daemon holds, drawn in the open, answered by typing and Enter. This card answers
/// **the daemon**: whether a process tree should die. The operator's rule is that the two must
/// not be confusable — *"a person must never be unsure which one they are looking at, and
/// neither may be answerable by the other's keystroke"* — so they are kept apart in every way a
/// person can see or type:
///
/// * **different words** — this one names the program and says what ending it does, and it
///   never quotes the program's own output (see [`App::term_ask_lines`]);
/// * **different key** — the yes here is `y`, and it is deliberately **not Enter**, because
///   Enter is the prompt card's own (an empty line is a real answer there) and the composer's.
///   A stray Enter cannot kill anything;
/// * **and the safe default** — *anything that is not a deliberate yes* cancels, Esc included.
///   A confirmation whose default is the destructive answer is not a confirmation.
///
/// **A prompt that arrives while this is up takes the screen back**, and the arm that does it
/// says why: a program that has just asked a question must not be killable by the answer to it.
///
/// **And it outranks a card that was already waiting**, which is this head's rule for two
/// questions at once rather than a choice made here: the newest question owns the screen and the
/// older one waits its turn (`App::decision_card`'s slot says the same thing about a decision
/// card under a picker). So a decision card that was up when the operator typed `!term close`
/// comes back when this is answered, cancelled or confirmed — and `esc` is the way out of the
/// stack, one question at a time, because *anything that is not a yes* cancels.
#[derive(Debug, Clone)]
pub(crate) struct TermAsk {
    /// **What is about to end, as a line a person reads** — `!term nano notes.txt`, the spelling
    /// the operator typed at the composer when there is one, or `!term <command>` rebuilt from
    /// the daemon's own word when the pane is another head's.
    pub(crate) line: String,
}

mod attention;
mod commands;
mod composer;
mod events;
mod keys;
mod notes;
mod panes;
mod pick;
mod prefs;
mod scroll;
mod session;
mod todos;
mod visibility;
pub use attention::*;
pub use commands::*;
pub use composer::*;
pub use keys::*;
pub use notes::*;
pub use panes::*;
pub use pick::*;
pub use prefs::*;
pub use scroll::*;
pub use session::*;
pub use todos::*;
pub use visibility::*;

#[cfg(test)]
mod tests;
