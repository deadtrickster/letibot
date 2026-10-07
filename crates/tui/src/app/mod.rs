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
use letibot_sessionlog::registry::{SessionBrief, SessionWiring, short_id};
use letibot_sessionlog::view::{
    CallState, OpenDecision, SettledDecision, SnapshotItem, TurnState, Warned,
};
use letibot_transcript::{TranscriptItem, UserPart};

use letibot_ui::card;
use letibot_ui::editor::Editor;

use crate::markdown::IncrementalMarkdown;
use crate::render::{BlockCache, RenderConfig, visible_width};
use crate::ui::*;

/// **Which setting a card is choosing** — R38.
///
/// Two of these are the daemon's (`Mode` from a `SettingRow`'s `choices`, `Model` from the
/// catalogue); two are this head's own (`Verbosity`, the diff style). They share one card
/// because they are one KIND of act — the reader is choosing between named values and can see
/// all of them — and one card is what keeps them from becoming three vocabularies.
///
/// **`Verbosity::Terse` and its neighbours are the reason this exists.** `/verbosity` used to
/// cycle, which requires the reader to hold four rungs in their head and to find the current
/// one by changing it: three presses and three repaints for the value they wanted, and no
/// screen anywhere saying what the four were. R38's rule: **a setting with more than two
/// values is chosen from a card; only a true toggle may cycle.**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    /// The mode this session runs under — the daemon's names.
    Mode,
    /// What answers this conversation — the daemon's models.
    Model,
    /// **How much of the stream reaches the transcript** — R37's ladder.
    Verbosity,
    /// **How a diff is laid out** — R38's new setting.
    Diff,
}

impl Pick {
    /// What the card is asking, as its title.
    pub(crate) fn title(self) -> &'static str {
        match self {
            Pick::Mode => "the mode this session runs under",
            Pick::Model => "what answers this conversation",
            Pick::Verbosity => "how much reaches the transcript",
            Pick::Diff => "how a diff is drawn",
        }
    }

    /// The settings row the daemon publishes the choices on, or `None` for a setting this
    /// head owns. **The daemon's lists are read, never kept** — the mistake the `mode` row's
    /// own comment records.
    pub(crate) fn row_key(self) -> Option<&'static str> {
        match self {
            Pick::Mode => Some("mode"),
            Pick::Model => Some("model"),
            Pick::Verbosity | Pick::Diff => None,
        }
    }

    /// What this setting can be, with what each value MEANS.
    ///
    /// **Every value carries its meaning on the card and not only its name** (R38). `Terse`,
    /// `Normal`, `Loud` and `Conversation` are not self-describing, and a reader choosing
    /// between them is choosing between *what will be on my screen*, which the card can state
    /// and the name cannot. The daemon's two settings have no sentences here because the head
    /// does not know what they mean — it renders the daemon's choices verbatim and says
    /// nothing more, which is the same rule as its hints.
    pub(crate) fn values(self) -> &'static [(&'static str, &'static str)] {
        match self {
            // The daemon's own names arrive at runtime; see `SettingPick::daemon_lists`.
            Pick::Mode | Pick::Model => &[],
            // The head's own two carry their sentences from [`Pick::values`] — and the verbosity
            // card is the PROFILE TABLE itself, built in `App::pick_values` rather than listed
            // here. It used to be a hand-written list beside this one, and the two drifted: the
            // copy was missing `read-edits` entirely, so a rung `/v` cycles onto had no row.
            Pick::Verbosity => &[],
            Pick::Diff => DIFF_VALUES,
        }
    }

    /// The lines under the list: what taking a row DOES, which differs per subject and is the
    /// difference a reader is most likely to get wrong.
    ///
    /// **A slice and not one line**, because the model card has two facts and the file's own
    /// rule is one fact per line: these are trimmed rather than wrapped, and measured at 110
    /// columns a two-fact version read *"It also become…"* with its useful half never reaching
    /// the screen. The second line is the verb for the OTHER thing, which the operator went
    /// looking for — one verb doing both is what sent them.
    pub(crate) fn consequence(self) -> &'static [&'static str] {
        match self {
            Pick::Mode => &[
                "a mode change moves THIS session from its next call, and every later session \
                 in this project.",
            ],
            Pick::Model => &[
                "this conversation only, from the next turn; the transcript and the tools are \
                 untouched",
                "`/default-model NAME` is what new sessions start on · this is not that",
            ],
            // **The one that surprises people**, and R38 asks for it in as many words: the
            // ladder is applied to the whole transcript at once, so a rung takes effect on
            // what is already drawn rather than on what comes next.
            Pick::Verbosity => &[
                "this applies to the WHOLE transcript, already drawn — switch back and the rows \
                 you had hidden are there again.",
            ],
            Pick::Diff => &[
                "every edit card, drawn and future — the excerpt is the same either way; only \
                 the layout changes.",
            ],
        }
    }
}

/// **What the diff style means** — named here so both heads spell one setting one way (R38,
/// §11.6).
///
/// # The two axes, and the one that is NOT a value
///
/// The operator named two: *unified against side-by-side*, and *whether colour or the `+`/`-`
/// marks carry the meaning*. **The first is the setting. The second is not a choice and this
/// is the ruling:** the marks are drawn in BOTH layouts, always, because they are the diff's
/// meaning and colour is reinforcement of it. A value that removed the marks would be a value
/// that makes the diff unreadable on exactly the terminals the operator is worried about — a
/// pipe, a `--replay`, a light theme, a reader who cannot tell red from green — and R20's own
/// argument is that an appearance which collapses in half the terminals it is read in is not
/// an appearance at all. So there is no `marks: off`, and no `colour` value either: colour is
/// a property of the terminal, which the head already knows about (`RenderConfig::color`),
/// not a preference to be stored.
pub(crate) const DIFF_VALUES: &[(&str, &str)] = &[
    (
        "unified",
        "one column: `-` lines removed, `+` lines added, in order — best on a narrow terminal",
    ),
    (
        "split",
        "two columns: the old text left, the new right, lined up — best when there is width",
    ),
];

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

/// One tool call as the head watches it happen.
///
/// A tuple until now, and the three things it did not keep are the three a person
/// looking at a running call wants: **how long it has been going**, **what it
/// last said**, and **how much came out**. All three were derivable —
/// `Envelope::ts` is on every event and `ToolProgress { note }` was being read and
/// dropped — and none of them had anywhere to go while a call rendered as one
/// A subagent this session spawned, as the latest `Subagent` event reported it.
///
/// **The event is durable, and a head that was attached when it happened can replay it —
/// but a SNAPSHOT does not carry it**, and a head that attached after the spawn has no
/// event to fold at all. So a row has two possible sources and they are joined in one
/// place: this shape as the live event reported it, and the same fields as far as the
/// daemon's own session list can supply them ([`App::fold_subagents`]), which is what a
/// late head and every switch rebuilds the tree from.
#[derive(Debug, Clone)]
pub(crate) struct SubagentState {
    pub(crate) session_id: String,
    /// `opening` | `running` | `done` | `failed`, **or empty**, which is a row rebuilt from the
    /// daemon's session list for a child this head never watched: that list says whether a turn
    /// is generating in the session and nothing about how a settled one ended, so an empty word
    /// draws as `[?] state unknown` rather than as a `done` nobody measured. See
    /// [`App::fold_subagents`].
    pub(crate) state: String,
    /// **A turn is generating in this child at this instant** — the daemon's session list's
    /// own word (`SessionStatus::running`: *"a turn is generating in this session at this
    /// instant"*), and a measurement of NOW rather than of a life.
    ///
    /// **Kept BESIDE [`SubagentState::state`] and not folded into it**, which is the whole of
    /// this field's reason. The two sources spell the same word — the event's `running` and the
    /// list's `running` — and they mean different things: the event publishes its `running`
    /// **once**, when the child's harness is open, and it is a lifecycle word (*this child is
    /// up*), while the list's `running` is *a turn is generating in it right now*.
    /// [`App::fold_subagents`] merged the two, so a child that had merely stopped between
    /// turns went back to looking un-started: it left the count above the composer and moved
    /// into the `finished` group beside children that had actually ended. The operator
    /// measured exactly that on 2026-10-06 — the count reading `4, 2, 3, 1` over children
    /// that were alive throughout, one of them parked on its own background job with two
    /// commits already behind it.
    pub(crate) generating: bool,
    /// **The legacy field, and the pre-`task` fallback**: the subtask's first line on the
    /// opening states, and the child's answer's first line once it has finished. A new
    /// row reads [`SubagentState::task`]; this is here so a daemon older than that field
    /// still draws what it always did.
    pub(crate) prompt: String,
    pub(crate) role: String,
    /// **The subtask in full, from the event's `task`.** Empty against a daemon that
    /// predates the field, and the pane then falls back to `prompt`.
    pub(crate) task: String,
    /// **The model this child runs on**, from the event's `model` — `local`, or
    /// `PROVIDER/MODEL`. Empty when the child inherited its parent's model, which is the
    /// default: the pane then draws no model clause rather than claiming one.
    pub(crate) model: String,
    /// **The child's answer's first line**, `Some` only once it has finished — the
    /// subtitle, kept apart from the row so a completion cannot be mistaken for the
    /// question.
    pub(crate) answer: Option<String>,
    /// **When this child was spawned, for the pane's order** — the *"most recent agents must be on
    /// top"* the operator asked for on 2026-10-05.
    ///
    /// Two sources, and the daemon's wins: `SessionBrief::created_ms`, which is the session's own
    /// creation time and therefore the spawn, and — for a row this head watched appear before the
    /// list carried it — the `Subagent` event's own `ts`, which is when this head *heard* about the
    /// child rather than when it was made. A later finish event does not overwrite either: recency
    /// in this pane is *when the agent started*, so a child that has run for an hour does not jump
    /// above one spawned a minute ago for having ended last.
    ///
    /// **`0` is *not known*** — a replay, or a brief from a daemon that did not stamp the row — and
    /// it sorts LAST, below every row somebody can date. That is the honest place for it: a row
    /// nobody can order belongs at the bottom, not at the top pretending to be new.
    pub(crate) spawned_ms: u64,
}

impl SubagentState {
    /// **Whether this child is done** — the one thing the pane's two groups are made of, and
    /// now the one thing the count above the composer is made of too.
    ///
    /// Anything that is not `running` or `opening` is finished, and that includes a row the
    /// daemon's list rebuilt with no state word at all: a child this head did not watch, whose
    /// brief said only *a turn is not generating here* — which a running child would have
    /// contradicted.
    ///
    /// **A life, and not a measurement of an instant.** The word here is the one the daemon
    /// published for the child — `opening` at the spawn, `running` when its harness came up,
    /// `done` or `failed` at the end — and the only thing that may end it is that end. A child
    /// with a tool call in flight, a child between two rounds, a child parked on its own
    /// background job: all three are `running` and all three are alive, which is the standard
    /// the operator set in their own words — *"claude code for example shows subagent as alive
    /// until it finished turn with reply. not 'pausing it' on tool calls"*.
    ///
    /// **A stale `answer` does not enter into it.** A row can hold the answer of a turn that
    /// has since ended while the list says a second turn is generating in it right now, and
    /// that child is alive — see [`SubagentState::generating`] and the merge in
    /// [`App::fold_subagents`], which is where the two facts are kept apart.
    pub(crate) fn is_finished(&self) -> bool {
        !matches!(self.state.as_str(), "running" | "opening")
    }
}

/// **One row of the subagent pane** — the ONE enumeration the arrows, Enter, `p`, the drawn
/// `▸` and the scroll all read. A pane whose cursor comes from one list and whose rows come
/// from another is the defect leticl's `todos-stops` docstring names; see [`App::subagent_stops`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubStop {
    /// A child in [`App::subagents`], by index.
    Agent(usize),
    /// **The `finished` group row.** The finished children live under it, collapsed by
    /// default; Enter unfolds them.
    Finished,
}

/// **One row of the jobs pane** — the ONE enumeration the arrows, Enter, the drawn `▸` and the
/// scroll all read, exactly as [`SubStop`] is for the subagents pane. See [`App::job_stops`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JobStop {
    /// A job in [`App::jobs`], by index.
    Job(usize),
    /// **The `finished` group row.** The settled jobs live under it, collapsed by default;
    /// Enter unfolds them.
    Finished,
}

/// One row of the config pane.
#[derive(Debug, Clone)]
pub(crate) struct ConfigRow {
    pub(crate) section: &'static str,
    pub(crate) key: String,
    pub(crate) value: String,
    /// Where the value came from — a path, a flag, "default" — shown under the
    /// selected row. Empty when nobody tracks it.
    pub(crate) source: String,
    /// The values this row may take, as the DAEMON sent them. Empty for a row
    /// with no closed set, and for a daemon older than protocol 18 — the pane
    /// then says it cannot cycle rather than cycling a list it invented.
    pub(crate) choices: Vec<String>,
    pub(crate) edit: ConfigEdit,
}

#[derive(Debug, Clone)]
pub(crate) enum ConfigEdit {
    /// This head's own: Enter flips it and writes `head.toml`.
    Head(HeadSetting),
    /// The session's, changeable now by an existing verb; `(key, how)`.
    Session(String, String),
    /// Not now, and why.
    No(&'static str),
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum HeadSetting {
    Diff,
    /// **The rung of the ladder** (R37/R38), persisted like the rest. The pane's row is a
    /// *reader* of the setting rather than a second way to set it: Enter says which verb
    /// opens the card, because four values are chosen from a card and not cycled (R38) — a
    /// pane row that cycled them would be the interface R38 removed, one screen over.
    Verbosity,
    Thinking,
    Tools,
    RawCalls,
    /// **The git field's template** — cycles three stops: the shipped default, a spaced
    /// one, and the branch alone. leticl's own cycle (`%flip-head-setting`: *"Three stops:
    /// the shipped default, a spaced one, and the branch alone. `nil` is the default rather
    /// than a fourth string, because the default has to stay one value in one place"*). A
    /// FREE template stays the file's business — `git_format` takes any of them, and the
    /// pane cycles presets rather than pretending to edit text.
    GitFormat,
}

#[derive(Debug, Clone)]
pub(crate) struct SubOut {
    pub(crate) session_id: String,
    /// One line per rendered row — **and which renderer produced them is [`SubOut::degraded`]'s
    /// business**: the session's own rows go through `item_lines`, the same one every row of the
    /// transcript uses, and the event-ring fallback goes through `subagent_out_lines`.
    pub(crate) lines: Vec<String>,
    /// **The daemon answered with the event ring rather than the session's rows.**
    ///
    /// `Peeked::snapshot` is `None` when the daemon predates the field, or when the peek did not ask
    /// for rows — so the fallback is necessary, and the operator's rule is that a fallback has to be
    /// *visible*: a degraded render and a plain one must not look alike, or a reader cannot tell
    /// whether they are looking at a session or at a list of its events.
    pub(crate) degraded: bool,
    /// Lines hidden off the bottom. Zero is "following the tail"; the pane draw
    /// clamps it, because only the draw knows the visible height.
    pub(crate) scroll: usize,
    /// Where the whole view was spilled, when it was written.
    pub(crate) spill: Option<String>,
    /// Events that fell off the daemon's scrollback before this read — the same
    /// disclosure a `Hello` makes, because a peek is a replay.
    ///
    /// **Kept across the rewrite and it had to be**: a snapshot is bounded by the daemon's view
    /// bounds exactly as the ring is by its cap, so a trimmed read must still say it was trimmed.
    /// A missing answer rendering as an empty one is `card::Outcome::Abstained`'s rule, one pane
    /// along.
    pub(crate) dropped: u64,
}

/// The output view the jobs pane's Enter opens: one job's retained output, as the
/// daemon measured it.
///
/// The bytes **and the offsets beside them**, because the pane draws its own header
/// and its own paging — see `SessionEvent::JobOutput` for why the read comes back as
/// an event with the numbers attached rather than as the `Warning` prose `/job`
/// replies with.
#[derive(Debug, Clone)]
pub(crate) struct JobOut {
    pub(crate) job: String,
    /// The daemon's word for where the job is — `running`, `exited 0`, … Empty
    /// until the first answer arrives.
    pub(crate) state: String,
    /// **Whether anything was ever executed for this job** (A.2, §11.6).
    ///
    /// The daemon's answer, not the head's inference: an empty window is the shape of
    /// *ran and wrote nothing* and of *never ran*, and until this field existed the head
    /// had one sentence for both — so a card whose header read `not run (could not join
    /// its scope)` went on to say `it wrote nothing at all` about a command that was
    /// never started.
    ///
    /// `false` until the answer arrives, which is the reading that renders what every
    /// daemon before this field produced.
    pub(crate) never_ran: bool,
    /// **Where this job's output actually went, when it did not come here** (R41) — the file the
    /// job's `redirect` named in the list, taken from the row Enter was pressed on.
    ///
    /// A redirected job's window is empty BY CONSTRUCTION: the daemon gave its bytes to the file,
    /// so an empty window here is the shape of *this pane cannot show it*, and `it wrote nothing at
    /// all` would be a lie about a job that wrote a build log. The operator: *"entering a job never
    /// shows me its output - whether it went to file or not"*.
    pub(crate) redirect: Option<String>,
    /// The offsets of the window actually loaded: `from..to` of `produced`.
    pub(crate) from: u64,
    pub(crate) to: u64,
    pub(crate) produced: u64,
    /// Bytes that fell off the front of the ring before this window. Disclosed
    /// because a window that starts mid-log is otherwise read as the job's start.
    pub(crate) dropped: u64,
    /// The window, already split into lines by the daemon so two heads cannot
    /// disagree about where a line ends.
    pub(crate) lines: Vec<String>,
    /// Where the daemon says the next page starts, or `None` when the end is here.
    pub(crate) next: Option<u64>,
    /// Offsets already loaded, newest last: `←` walks back the way `→` came. A
    /// stack rather than `from - page` arithmetic, because the page size is the
    /// daemon's choice and recomputing it here would be a second copy of it.
    pub(crate) back: Vec<u64>,
    /// Lines hidden off the bottom of the loaded window. Zero follows the tail;
    /// the draw clamps it, because only the draw knows the visible height.
    pub(crate) scroll: usize,
    /// True from the request until its answer: the overlay says so rather than
    /// showing an empty window it cannot yet fill.
    pub(crate) loading: bool,
    /// The daemon's refusal, when the read could not be answered — a job that fell
    /// out of the host's table between the listing and Enter. Shown in place of the
    /// window so the pane does not sit at `reading…` forever.
    pub(crate) error: Option<String>,
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

/// **One row of the todos pane the cursor may land on** — see [`App::todos_stops`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TodoStop {
    /// The `[+] add todo item` control, at the head of the list.
    Add,
    /// **One of the operator's own rows, BY ITS WORDS.** There is no id on the wire —
    /// `TodoEntry` is `content`, `status`, `by`, and the operator has ruled out a bump for one — so
    /// the words are the identity, which is the same key `/todo done N` uses. A row renamed is a
    /// different row, and that is the honest reading of a list with no ids.
    Mine(String),
    /// A row of the workspace's `TODO.md`, by index into `repo_todos` — the file's own order IS
    /// its identity, because the pane re-reads the file.
    Repo(usize),
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

pub(crate) const SLASH_COMMANDS: &[(&str, &str)] = &[
    ("new", "TITLE — start a fresh session"),
    ("sessions", "the session picker"),
    ("switch", "ID — go to another session"),
    ("rename", "NAME — name the session you are in"),
    ("help", "the key and command reference"),
    ("status", "the bottom border's telemetry, full screen"),
    ("think", "fold or unfold the model's reasoning"),
    (
        "tools",
        "what this conversation can call, and what it only looks like it can",
    ),
    (
        "default-model",
        "what a NEW session starts on; /models switches this one",
    ),
    (
        "verbosity",
        "how much reaches the transcript: the card, or a rung by name",
    ),
    ("diff", "how a diff is drawn: unified or side by side"),
    (
        "copy",
        "the open ctrl-v output, or the last reply, onto the clipboard",
    ),
    ("notes", "what this head has shown — and how to retire one"),
    (
        "config",
        "every setting, the runtime-editable ones editable in place",
    ),
    ("mode", "the mode picker — or /mode NAME to type it"),
    ("jobs", "open or close the background-jobs pane"),
    (
        "queue",
        "open or close the merge-queue pane: what is landing on main, and why it is not",
    ),
    // **§6's verbs, and §7's C14 ruling that the table is the UNION.** Five of these
    // had a chord and no word, so they were unreachable from a pipe and `/help` had no
    // name for them; `/models` and `/resync` were implemented and simply not listed.
    // C14 calls the table *vocabulary, not implementation* and wants one shared artefact
    // both heads read — that is a cross-tree change and is filed rather than half-done
    // here, but **listing what this head implements is this head's half of it.**
    ("todos", "open or close the todos pane (ctrl-t)"),
    (
        "todo",
        "TEXT adds one of YOUR rows · done N · rm N · postpone N · resume N — the pane numbers \
         your half",
    ),
    ("subagents", "open or close the subagent tree (ctrl-g)"),
    (
        "peek",
        "ID — read one subagent's output without leaving this session",
    ),
    ("resume", "ID — bring a session on disk back and go there"),
    (
        "promote",
        "move the running command to the background (ctrl-o)",
    ),
    (
        "models",
        "which model answers: /models is a menu, /models PROVIDER/MODEL switches \
         (/models glm-coding --key PASTE stores the key)",
    ),
    (
        "resync",
        "throw this head's state away and take a fresh snapshot",
    ),
    ("cells", "MESSAGE — send it with a copy of this screen"),
    ("compact", "summarise this session and fork it"),
    (
        "reseat",
        "rebuild the prompt from the tools seated now, keeping the conversation",
    ),
    (
        "reseat summarise",
        "the same, but summarise the conversation instead of carrying it",
    ),
    ("interrupt", "stop the running turn"),
    ("quit", "leave the head"),
    // **The three this table was missing, and the test below is why they cannot be
    // missed again** (R32). `/dismiss` and `/notes` are one action under two words —
    // the second is what somebody types at a wall of red — and `/settings`/`/stats` are
    // the dispatcher's own aliases for `/config`/`/status`. All three worked and none
    // was offered, which is the whole finding: the table is a registry that was read as
    // if it were the dispatcher.
    (
        "dismiss",
        "retire this head's notes — the same as /notes dismiss",
    ),
    ("settings", "every setting — the same as /config"),
    ("stats", "this head's counters — the same as /status"),
];

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

/// **How a save treats the retired set** — because one write cannot express both verbs.
///
/// A dismissal asserts that a key **is** retired. A restore asserts that the set is **not**,
/// which is a removal. A union adds and never removes, so it can express the first and cannot
/// express the second; a replacement can express both, but applying it to a dismissal would
/// discard every key another head had retired since this one loaded — which is the operator's
/// original report (*"i dismissed letibot notes but they stay"*).
///
/// So the verb decides the write, and the call site says which it is rather than a bool that
/// could be passed by accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RetiredWrite {
    /// Add to the file's set. A dismissal is an assertion no other head's save may contradict.
    Union,
    /// Take this head's set as the whole truth. A restore is an assertion of removal.
    Replace,
}

/// **Which of a daemon row's `choices` its `value` names** — the reading both the picker's cursor
/// and the card's `← now` are made of.
///
/// A row's `value` is not always a choice verbatim. The daemon spells it as a name **plus whatever
/// qualifies it**:
///
/// ```text
/// value   "writes allowed"                     choice  "writes allowed"
/// value   "allow-all (this box, consented)"     choice  "allow-all"
/// value   "local (qwen-3.8-27b)"               choice  "local"
/// value   "deepseek/deepseek-flash --key …"    choice  "deepseek/deepseek-flash"
/// ```
///
/// # The rule, and the two ways of getting it wrong
///
/// **The value itself; failing that, the longest choice the value begins with at a boundary.**
///
/// * **Not the value's first word — that was the defect, and the whole-value comparison is the
///   branch that fixed it.** A NAME CAN ITSELF CONTAIN A SPACE (`Mode::WRITES_ALLOWED` is
///   `writes allowed`), so taking the first word turns it into `writes`, which names no choice,
///   and both readers fall back together to row 0. Measured on the real wire row: `writes allowed`
///   gave no cursor and no `← now` while every other named mode gave both.
///
///   **The daemon's own parser is why nobody noticed.** `Mode::parse` folds `_` and spaces to `-`
///   on *both* sides, so `writes-allowed` and `writes allowed` both select that point — the head's
///   fixture said the hyphenated one and agreed with itself while the wire said the other.
/// * **A boundary, so one name is not read as a prefix of another.** `automode-edits` begins with
///   `automode`, so a bare prefix match would seed on the shorter row. This branch is for the
///   QUALIFIED values the daemon writes — `allow-all (this box, consented)`, `local (qwen-3.8-27b)`
///   — where the qualifier follows a space and the name itself has none, which is why the old
///   first-word rule happened to survive them.
/// A whitespace boundary and not a list of separators: the daemon writes `name (note)` and
/// `name --flag`, and inventing a grammar for the qualifier would be a rule about a spelling this
/// head does not own. If a future row qualifies a name with something that is not whitespace-
/// separated, it shows up here as *no choice named* — which renders as no `← now`, the honest
/// answer, rather than as a mark on the wrong row.
pub(crate) fn named_choice<'a>(value: &str, choices: &'a [String]) -> Option<&'a str> {
    choices
        .iter()
        .filter(|c| {
            value.len() > c.len()
                && value.starts_with(c.as_str())
                && value[c.len()..].starts_with(char::is_whitespace)
        })
        .chain(choices.iter().filter(|c| value == c.as_str()))
        .max_by_key(|c| c.len())
        .map(String::as_str)
}

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

    /// What a submitted line means: a command, an answer to an open decision, or
    /// a prompt.
    pub(crate) fn submit(&mut self, text: String) -> Option<Action> {
        // **`!term` is checked before the bare `!`**, because `!term mc` is also a perfectly
        // good `!` line — `operator_shell_command` reads it as the command `term mc`, which is
        // a program nobody has. The verb has to be taken first, and the parse is the daemon's
        // own function so the two halves cannot disagree about what a `!term` line is.
        //
        // **No gate, no card, no ladder** — the same sentence `!` gets, for the same reason:
        // it is the operator's own act. The difference from `!` is that this appends no row: a
        // pane is not a transcript item, it is the conversation's rectangle given to a
        // program, and when it closes the transcript is exactly what it was. So the line does
        // not join `pending_prompts` either — there is no `User` row coming to retire it, and
        // an echo that waited for one would wait for ever.
        //
        // **And the verb with nothing after it is not refused any more — it attaches.** It was
        // a sentence (*"`!term` needs a command to run"*), and the operator's own report is why
        // that was wrong: a pane is the *session's*, so a person whose head lost the rectangle
        // still has a program running and no way back to it. `!term` means *the pane this
        // session has*; a session with no pane says so through the same `TermEnded` every other
        // pane that could not start uses, and the note that closes the rectangle is where that
        // sentence is read.
        //
        // **And the third reading is the ending.** `!term close` is not a program to run: it is
        // how a pane is ENDED, and it is the only act that sends `Action::TermClose` — see
        // [`TermPane`] for why the destructive act has to be spelled out while `ctrl-\` only
        // detaches. The parse is `term_line`, the one function both halves share, so a head
        // cannot treat the line as the ending while the daemon runs a program called `close`.
        if let Some(what) = letibot_sessionlog::term_line(&text) {
            if self.detached() {
                self.set_composer(&text);
                self.say(
                    "no daemon connection — nothing was started and nothing was ended. Your line \
                     is held here. It sends when the daemon is back.",
                );
                self.redraw = true;
                return None;
            }
            if matches!(what, letibot_sessionlog::TermLine::Close) {
                return self.begin_close();
            }
            self.scroll = 0;
            // **The pane is created now, empty, and the daemon's first bytes fill it.** The
            // alternative — wait for `TermOutput` before opening the rectangle — would show
            // the transcript for as long as the pty takes to start a program, which is the
            // flicker this pane exists to remove. A pane that never starts is closed by the
            // `TermEnded` that carries the refusal, a moment later; an attach is closed by the
            // same frame when the session has no pane, and filled by the daemon's replay when
            // it has one.
            //
            // The rectangle here is the **last frame's**, which is the best this layer can
            // know; the first `compose_screen` corrects it to the pane's own and sends the
            // `TermResize` that tells the program — and on an attach the daemon has already
            // resized the pty to the rectangle this frame carries.
            self.term = Some(TermPane::new(
                &text,
                self.term_cols.max(1),
                self.screen_rows.max(1),
            ));
            self.redraw = true;
            return Some(Action::TermOpen { line: text });
        }

        if letibot_sessionlog::send_line(&text).is_some() {
            if self.detached() {
                self.set_composer(&text);
                self.say(
                    "no daemon connection — your line is held here. It sends when the daemon \
                     is back.",
                );
                self.redraw = true;
                return None;
            }
            self.scroll = 0;
            // **No echo and no `pending_prompts`.** A `!` line and a `!term` line both put
            // something in the conversation or on the screen; a `!send` line goes into a
            // running program's stdin and leaves no row behind. An echo would be this head
            // claiming a line that the transcript will never carry — the same rule
            // `!term`'s arm states for its own reason.
            return Some(Action::SendLine {
                line: letibot_sessionlog::send_line(&text)
                    .expect("just checked")
                    .to_string(),
            });
        }

        // **A line whose first character is `!` is the operator's own shell command.**
        //
        // The operator's ask: *"when prompt starts with ! it is going to be a shell command
        // from me"*. The bang has to be the FIRST character, exactly as `/` does for verbs —
        // one rule for the two sigils a composer line can start with, so leading whitespace
        // means prose, as it always did. The recognition is the same shape as the `/` arm
        // below and sits beside it for the same reason: while a card or a picker is open, a
        // line that begins with a sigil is that thing and cannot sensibly be anything else.
        //
        // **A line that is nothing but the bang is refused here, with the words kept.** `!`
        // and `!   ` carry no command, and sending one would make the daemon echo a refusal
        // for something the head could see was empty. The daemon re-checks anyway
        // (`operator_shell_command`), because a frame is a socket and not a keyboard.
        //
        // **No gate, no card, no ladder** — not because this head skips one but because
        // there is none on the path: the frame is `OperatorShell`, the daemon runs it through
        // `ToolRuntime::invoke_operator` (the door's own ungated entry), and no
        // `DecisionRequested` can appear for it. Pinned where the run lives:
        // `letibot-harnessd`'s `tests/operator_shell.rs` counts the adjudicator's calls
        // (and the log's decisions) and fails if either moves.
        //
        // The echo joins `pending_prompts` so the line is visibly held until the daemon's
        // `User` row lands and retires it — the same trust a prompt places — and the detached
        // guard is the prompt's own, because a `!` line that cannot reach a daemon must not
        // look sent either.
        if text.starts_with('!') {
            if letibot_sessionlog::operator_shell_command(&text).is_none() {
                self.set_composer(&text);
                self.say("! COMMAND — the bang has to be followed by the command to run");
                self.redraw = true;
                return None;
            }
            if self.detached() {
                self.set_composer(&text);
                self.say(
                    "no daemon connection — your line is held here. It sends when the daemon \
                     is back.",
                );
                self.redraw = true;
                return None;
            }
            self.scroll = 0;
            self.pending_prompts.push(text.clone());
            return Some(Action::OperatorShell { line: text });
        }
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
        // The mode picker takes the line the same way — a row number or a name
        // prefix — for the same reason: while the list is on the screen a bare
        // `2` means the second mode and cannot sensibly mean anything else.
        if self.pick == Some(Pick::Mode) {
            return self.pick_mode(text.trim());
        }
        // An open decision owns Enter, typed line or not. A line that names an
        // option is that answer. Any other line is not disposable: the ask
        // arrived while it was being typed, and Enter on a card means "answer
        // this" — the marked row — so the words go back to the composer and
        // the next Enter, with the ask settled, sends them. Sending them here
        // is how a permission arriving mid-typing turned Enter into "send the
        // half-thought" (the operator, 2026-09-17).
        if let Some(d) = self.open.first().cloned() {
            // **A question is answered with words, and that is not a courtesy**
            // (§1.7). D10's third field is `free` — *"a typed answer, and a
            // first-class one"* — and the requirement names the alternative it is
            // against: *"not claude code 'chat later'"*. So under a question every
            // line is either one of the model's own choices or an answer, and
            // neither is "hold the words and answer the marked row".
            //
            // **A line that IS a choice answers by index.** Matching the text is
            // how a person answers a menu they were shown, and it costs one
            // comparison per choice; `option` is preferred over `free` because the
            // model gets back *which of the three it offered* rather than a
            // sentence it has to re-read as one of them.
            if d.kind == "question" {
                let typed = text.trim();
                if typed.is_empty() {
                    return self.answer_marked();
                }
                let at = d
                    .choices
                    .iter()
                    .position(|c| c.trim().eq_ignore_ascii_case(typed));
                let answer = match at {
                    Some(i) => letibot_sessionlog::question::QuestionAnswer::choosing(i),
                    None => letibot_sessionlog::question::QuestionAnswer::free(typed),
                };
                return Some(Action::AnswerQuestion {
                    req_id: d.req_id.clone(),
                    answer,
                });
            }
            match match_option(&d, text.trim()) {
                OptionChoice::One {
                    option_id,
                    pattern,
                    note,
                } => {
                    return Some(Action::Answer {
                        req_id: d.req_id,
                        option_id,
                        pattern,
                        note,
                    });
                }
                // **A name that fits several options answers nothing.** The line goes
                // back to the composer so it can be finished, the card stays up so the
                // arrows still work, and the sentence names every candidate — because a
                // refusal with no next step is a head that has stopped listening, and
                // answering the marked row here would be the very defect this refusal
                // is against: a grant the operator did not choose, written to the audit
                // under a name they can see they did not type.
                OptionChoice::Ambiguous { word, candidates } => {
                    self.set_composer(&text);
                    self.say(&ambiguous_option_line(&word, &candidates));
                    self.redraw = true;
                    return None;
                }
                // Nothing on the card answers to the line: the mid-typing courtesy,
                // below.
                OptionChoice::Unnamed => {}
            }
            self.set_composer(&text);
            if let Some(a) = self.answer_marked() {
                self.say("answered the ask — your line is held, enter sends it");
                return Some(a);
            }
            // An ask with no options cannot be taken by Enter at all; the
            // line stays held rather than becoming a prompt sent under it.
            self.say("this ask offers no options — your line is held");
            return None;
        }
        // **A line typed while a fill is still running.**
        //
        // The daemon answers a prompt sent mid-import against the whole history — the
        // import is one worker job, so a prompt cannot interleave — but the head must not
        // show one as `queued` for a turn nobody has started. So it is refused and the
        // words go back to the field they were typed in: the §4.2 behaviour (`7b9ca62`),
        // reused, because answering against a half-adopted transcript and then appending
        // the rest would put the conversation in the wrong order (R2's rule with the whole
        // history missing).
        if self.filling.is_some() {
            self.set_composer(&text);
            self.say(
                "the conversation is still being imported — your line is held here. It sends \
                 when the import is done.",
            );
            self.redraw = true;
            return None;
        }
        // **A line typed into a head with no daemon must not look sent.**
        //
        // This is the one place where a detached head could lie quietly. Everything else
        // a key does is local — it moves a cursor, opens a pane, folds a block — but a
        // submit is the head taking the operator's words on the promise that something
        // will read them, and with no daemon nothing will. The failure mode without
        // this guard is the worst of the three: `submit` pushes the echo into
        // `pending_prompts`, the write fails, the conversation shows `queued · <their
        // words>` and the operator believes it was sent — and it is not queued anywhere,
        // so when the daemon comes back the sentence is simply gone, having been shown as
        // held.
        //
        // So: **refused, with the words put back where they were.** `set_composer`
        // restores the text the editor handed over on Enter — without it the refusal
        // would clear the field, which loses the sentence just as thoroughly as sending
        // it nowhere would — and pressing enter again once the daemon is back sends it
        // unchanged. Nothing is pushed to `pending_prompts`, so nothing can be shown as
        // queued and then evaporate.
        if self.detached() {
            self.set_composer(&text);
            self.say(
                "no daemon connection — your line is held here. It sends when the daemon \
                 is back.",
            );
            self.redraw = true;
            return None;
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
        //
        // **Behind a running turn the queue is one message.** The engine merges
        // the operator's consecutive steering into one held item (one user turn
        // for the model, not a stack of fragments), so the echo joins the same
        // way — the landing row retires the echo by being its text. Idle submits
        // land each as their own row within a tick, so they stay separate.
        // **Busy, not generating** — see `turn_busy`. This gate decides whether the line joins the
        // last echo or starts a new one, and the daemon merges everything typed during a ROUND
        // while the state name only covers generation: two prompts typed during a tool call got two
        // `queued` rows for one message.
        if self.turn_busy()
            && let Some(last) = self.pending_prompts.last_mut()
        {
            last.push('\n');
            last.push_str(&text);
        } else {
            self.pending_prompts.push(text.clone());
        }
        Some(Action::Prompt(text))
    }

    /// Columns the composer's text has, inside the box.
    ///
    /// One function, because the wrap width the editor is *drawn* at and the one
    /// vertical motion is *computed* at have to be the same number — a cursor
    /// that moves by a row the renderer did not draw lands somewhere the person
    /// was not looking.
    pub(crate) fn composer_cols(&self) -> usize {
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
    pub(crate) fn refold(&mut self) {
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
    pub(crate) fn pick(&mut self, typed: &str) -> Option<Action> {
        if typed.is_empty() {
            self.picker = false;
            self.redraw = true;
            return None;
        }
        // **The number is the row the picker DREW.** They are the same list only while nothing is
        // expanded, and a number that means one thing on the screen and another in this function is
        // the two-enumerations defect. The prefix search below stays over every session on purpose:
        // a collapsed child is still reachable by name, which is what collapsing is for.
        let rows = self.session_rows();
        if let Ok(n) = typed.parse::<usize>()
            && n >= 1
            && n <= rows.len()
        {
            let id = self.sessions[rows[n - 1].idx].session_id.clone();
            return self.switch_to(id);
        }
        let hits: Vec<&SessionBrief> = self
            .sessions
            .iter()
            .filter(|s| {
                s.session_id.starts_with(typed)
                    || (!s.title.is_empty()
                        && s.title
                            .to_ascii_lowercase()
                            .contains(&typed.to_ascii_lowercase()))
            })
            .collect();
        match hits.len() {
            1 => {
                let id = hits[0].session_id.clone();
                self.switch_to(id)
            }
            0 => {
                self.say(&format!(
                    "no session matches {typed:?} — esc closes the list"
                ));
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

    /// Take a submitted line while the mode picker is up: a row number, or a
    /// mode name — exact, or a prefix only one mode shares.
    ///
    /// The normalization is `Mode::parse`'s own, read-only side: case folds,
    /// and `_` and a space fold to `-`, so `automode_edits` and `Automode
    /// Edits` reach the mode the daemon spells `automode-edits`. An exact
    /// match wins before prefixes are counted, so `automode` reaches
    /// `automode` even though `automode-edits` also starts with it.
    pub(crate) fn pick_mode(&mut self, typed: &str) -> Option<Action> {
        if typed.is_empty() {
            self.pick = None;
            self.redraw = true;
            return None;
        }
        let choices = self.mode_choices();
        if choices.is_empty() {
            self.say("this daemon does not send the mode list; use `/mode NAME`");
            return None;
        }
        if let Ok(n) = typed.parse::<usize>()
            && n >= 1
            && n <= choices.len()
        {
            let name = choices[n - 1].clone();
            return self.take_mode(name);
        }
        let norm = |s: &str| s.to_ascii_lowercase().replace(['_', ' '], "-");
        let want = norm(typed);
        if let Some(exact) = choices.iter().find(|c| norm(c) == want) {
            let name = exact.clone();
            return self.take_mode(name);
        }
        let mut hits: Vec<String> = choices
            .iter()
            .filter(|c| norm(c).starts_with(&want))
            .cloned()
            .collect();
        match hits.len() {
            1 => self.take_mode(hits.remove(0)),
            0 => {
                self.say(&format!("no mode matches {typed:?} — esc closes the list"));
                None
            }
            n => {
                self.say(&format!(
                    "{n} modes match {typed:?}; type the number on the left instead"
                ));
                None
            }
        }
    }

    /// Leave the mode picker for the mode the operator chose. The mode the
    /// session already runs under closes the list and says so, the way the
    /// session picker answers Enter on its own row — a round trip to the
    /// daemon to be told what the screen already showed is not worth its
    /// flicker.
    pub(crate) fn take_mode(&mut self, name: String) -> Option<Action> {
        self.pick = None;
        self.redraw = true;
        if name == self.mode_current() {
            self.say("already that mode");
            return None;
        }
        self.mode_action(name)
    }

    /// **The one place a mode leaves the head**, so the `allow-all` confirmation
    /// cannot be reached by one route and skipped by another. The picker, `/mode
    /// NAME` and the config pane's cycle all end here.
    pub(crate) fn mode_action(&mut self, name: String) -> Option<Action> {
        // The literal, not `Mode::ALLOW_ALL.name`: the head does not link
        // `letibot-tools` and does not keep a mode list — every other name it
        // handles comes from the daemon's `SettingRow::choices`. This is the one
        // name it has to recognise, and it is the daemon's own spelling.
        if name == "allow-all" {
            self.mode_confirm = Some(name);
            self.redraw = true;
            return None;
        }
        Some(Action::Mode {
            name,
            consented: false,
        })
    }

    /// The mode row of the daemon's last settings answer, and the two facts
    /// the picker and the config pane both read from it. `None` is a daemon
    /// that has not answered yet, or one older than protocol 18.
    pub(crate) fn mode_row(&self) -> Option<&letibot_sessionlog::protocol::SettingRow> {
        self.settings.iter().find(|r| r.key == "mode")
    }

    /// The mode names, as the daemon spelled them. Empty when it sent none —
    /// the head keeps no list of its own to fall back on, because a second
    /// copy of a list is a copy that drifts.
    pub(crate) fn mode_choices(&self) -> Vec<String> {
        self.mode_row()
            .map(|r| r.choices.clone())
            .unwrap_or_default()
    }

    /// **The settings row the open card is choosing from**, or `None` for a setting this
    /// head owns: `Verbosity` and the diff style are the head's own and have no daemon row.
    ///
    /// One function for the four, because the choice of row is the only thing that differs
    /// between a daemon's setting and the head's — see [`Pick::row_key`].
    pub(crate) fn pick_row(&self) -> Option<&letibot_sessionlog::protocol::SettingRow> {
        let key = self.pick?.row_key()?;
        self.settings.iter().find(|r| r.key == key)
    }

    /// **What the open card offers, as values with meanings** (R38).
    ///
    /// Two sources, and the difference is where the knowledge lives. A daemon's setting is
    /// read from its own `SettingRow` — the head keeping its own copy of a list is the
    /// mistake the `mode` row's comment records — and the values carry **no sentence**,
    /// because the head does not know what `automode-edits` means and inventing a gloss would
    /// be writing the other half's documentation. The head's own two settings carry the
    /// sentences from [`Pick::values`].
    pub(crate) fn pick_values(&self) -> Vec<(String, String)> {
        let Some(subject) = self.pick else {
            return Vec::new();
        };
        // **The verbosity card is the PROFILE TABLE, and not a second list built beside it.**
        //
        // The rows used to be a hand-written const (`VERBOSITY_VALUES`) and the copy drifted the way
        // a copy does: it was missing `read-edits` — the rung that is *conversation plus the edit
        // cards* — so a rung `/v` cycles onto had no row on the card, and the profile the operator
        // asked for by name did not exist as far as the card was concerned. One table, so a profile
        // added to `Profile::ALL` appears here by construction.
        if subject == Pick::Verbosity {
            let mut rows: Vec<(String, String)> = Profile::ALL
                .iter()
                .map(|p| (p.name.to_string(), p.why.to_string()))
                .collect();
            // **And the set in force, when no profile is it.** The rows above are the table; the set
            // is runtime state, so a set off the ladder had no row at all — the operator, having
            // typed one: *"it is not saved - when i do /verbosity there is no custom"*. The row is
            // named by the set's own `custom …` string, which `as_str` writes so that it can be
            // typed back; and since the row IS that string, it is also the row the marker lands
            // on — `pick_current` reads the same one.
            if self.visibility.profile().is_none()
                && !rows.iter().any(|(v, _)| v == &self.visibility.as_str())
            {
                rows.push((
                    self.visibility.as_str(),
                    "the set in force — no profile names it, and Enter on this row keeps it"
                        .to_string(),
                ));
            }
            return rows;
        }
        if !subject.values().is_empty() {
            return subject
                .values()
                .iter()
                .map(|(v, why)| ((*v).to_string(), (*why).to_string()))
                .collect();
        }
        self.pick_row()
            .map(|r| {
                r.choices
                    .iter()
                    .map(|c| (c.clone(), String::new()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// What the open card is already on.
    ///
    /// **The profile when the set is one, and the set's own `custom …` name when it is not** — and
    /// since `pick_values` now draws a row under exactly that name, a custom set is marked on the
    /// card like any other state rather than matching nothing. (It used to match no row at all,
    /// which read as the card having lost the setting; see that function.)
    pub(crate) fn pick_current(&self) -> String {
        match self.pick {
            Some(Pick::Verbosity) => self.visibility.as_str(),
            Some(Pick::Diff) => {
                if self.diff_split {
                    "split".into()
                } else {
                    "unified".into()
                }
            }
            _ => {
                let Some(r) = self.pick_row() else {
                    return String::new();
                };
                match named_choice(&r.value, &r.choices) {
                    Some(c) => c.to_string(),
                    None => r.value.clone(),
                }
            }
        }
    }

    /// **Commit the highlighted row.** The four subjects differ only here.
    pub(crate) fn take_pick(&mut self, name: String) -> Option<Action> {
        match self.pick {
            // The daemon's two: a mode is a protocol command this head already has, a model
            // is a daemon verb.
            Some(Pick::Mode) => return self.take_mode(name),
            Some(Pick::Model) => {
                self.pick = None;
                self.redraw = true;
                if self.session_id.is_empty() {
                    self.say("not attached to a session yet");
                    return None;
                }
                // **A row this box holds no key behind ASKS rather than switching.**
                //
                // The operator's row: *"if i choose a model without key picker should ask for the key."*
                // Left as it was, the switch went to the daemon and came back as its refusal
                // (`NO KEY — /models … --key PASTE`) — the picker telling the operator to type a
                // command it could have collected on the spot.
                //
                // **Only when the daemon named the keyless row** (`models.keys` present): an
                // absent row is *no greening*, never *no keys*, and asking on its absence would
                // block every switch behind a prompt for a key this box may well hold. `local`
                // never asks — it needs no credential, and `choice_ready` says so.
                if self.keys_row_present() && !self.choice_ready(&name) {
                    let provider = name.split('/').next().unwrap_or(&name).trim().to_string();
                    self.say(&format!(
                        "no key held for {provider} — paste it below; enter stores it (mode 600) \
                         and takes the row, esc cancels"
                    ));
                    self.key_ask = Some(KeyAsk {
                        choice: name,
                        provider,
                    });
                    self.key_buf.clear();
                    return None;
                }
                self.say(&format!("switching to {name}…"));
                // The switch, then a re-read of the rows it changed — in that order, which
                // the daemon honours, so the header names what answers now rather than what
                // answered a moment ago.
                self.queued.push(Action::Settings);
                return Some(Action::Slash {
                    line: format!("models {name}"),
                });
            }
            // **The head's own two are local settings**, so taking one is a write to this
            // head's config and not a frame — the same act `/verbosity NAME` and `/diff NAME`
            // perform, through the same function, so a card and a typed word cannot disagree.
            Some(Pick::Verbosity) => {
                self.pick = None;
                return self.set_verbosity(&name);
            }
            Some(Pick::Diff) => {
                self.pick = None;
                return self.set_diff(&name);
            }
            None => None,
        }
    }

    /// **Choose the profile `typed` names — one of R38's settings, and a local one.** Returns
    /// `None` because nothing is sent anywhere.
    ///
    /// **The one place a set is set**, called by the card's `Enter`, by `/verbosity NAME` and
    /// by `/v`, so the three cannot disagree about what a word means or about what happens to
    /// the transcript when it changes. The word itself is read by [`Visibility::parse`] — the
    /// same function `head.toml` is read with — so a name this reads here is a name the file
    /// reads there.
    pub(crate) fn set_verbosity(&mut self, typed: &str) -> Option<Action> {
        let t = typed.trim().to_ascii_lowercase();
        let next = if t == "v" || t == "next" {
            // **`/v` is the next profile**, in `Profile::ALL`'s order — which is the ladder's
            // order with `read-edits` in it — and one key's worth of cycling is a promise this
            // head made (R38 does not revoke it: what R38 rules out is having to CYCLE to
            // learn what the values are, and the card is where they are read).
            let at = self
                .visibility
                .profile()
                .and_then(|p| Profile::ALL.iter().position(|q| *q == p))
                .unwrap_or(0);
            Visibility::of(Profile::ALL[(at + 1) % Profile::ALL.len()])
        } else {
            match Visibility::parse(typed) {
                Ok(Change::Set(v)) => v,
                Ok(Change::Switch(s, l)) => self.visibility.with(s, l),
                Err(said) => {
                    self.say(&said);
                    return None;
                }
            }
        };
        // **What cannot be drawn is not stored** — see [`Visibility::undrawable`]. The ladder
        // turns its three switches on in one order, so a set that hides one below a switch it
        // shows would put a word on the status row that the screen does not carry, and the
        // operator's own rule for the whole rewrite is that *"a profile a person can select
        // that changes nothing is worse than an unfinished rewrite, because it lies about the
        // screen."* The refusal names the switch that cannot be honoured and the way round it.
        if let Some(s) = next.undrawable() {
            // **And the sentence names the way round that actually works — with the REMEDY
            // FIRST, because a notice is trimmed to the frame.**
            //
            // Two faults in one line, and the second was found by the test below rather than by
            // reading it. It read *"`{s}` cannot be off while something above it is on"*, which
            // is backwards in BOTH cases this can fire: the ladder turns `tools`, `thinking` and
            // `system` on in that order, so what cannot be drawn is a switch **on** with one below
            // it **off** — `thinking=open` while `tools=hidden` (which is `read-edits` and then the
            // thinking's chord), or `system=open` while the thinking is hidden. The old wording
            // named the switch the reader had just turned ON as the one to turn off, so the only
            // way to follow it was to make the set worse.
            //
            // And when it was fixed the other way round, the test failed: `say` draws the sentence
            // through `trim_to(…, w)`, so at 100 columns a refusal that opened by explaining the
            // ladder was cut off **before the verb that undoes it** — an R29 remedy the reader
            // cannot see is the same as no remedy. So the one word that always works comes first,
            // then which switch is drawn while which is off, then the profiles.
            //
            // **And the switches it names are only the ones BELOW the offending one**, which is a
            // third fault the same test found: the first cut collected every switch that was off,
            // so `read-edits`' thinking refusal read *"`tools` and `system` is off"* — the verb
            // agreeing with nothing, and `system` named as a requirement when it sits ABOVE
            // `thinking` and is nothing of the kind. The ladder is a prefix, so what a switch that
            // is ON needs is the ones below it: the statement is now true, and with one name it
            // reads as a sentence.
            const LADDER: [Show; 3] = [Show::Tools, Show::Thinking, Show::System];
            let below = LADDER.iter().position(|l| *l == s).unwrap_or(0);
            let missing: Vec<String> = LADDER[..below]
                .iter()
                .filter(|l| !next.shows(**l))
                .map(|l| format!("`{}`", l.name()))
                .collect();
            // `undrawable` only fires when one of those IS off, so the list is never empty — but
            // the sentence is built to read as a sentence anyway rather than to rely on it.
            let verb = if missing.len() == 1 { "is" } else { "are" };
            self.say(&format!(
                "`/verbosity {}=hidden` undoes it: `{}` {} drawn while {} {} off, and no rung \
                 draws that set — the ladder turns `tools`, `thinking` and `system` on in one \
                 order. Or choose a profile — {}",
                s.name(),
                s.name(),
                "is",
                names(&missing),
                verb,
                Profile::ALL
                    .iter()
                    .map(|p| p.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            return None;
        }
        let was = self.visibility;
        if next == was {
            // Nothing to do, and the notice would be a sentence about a change that did not
            // happen. (The card's Enter can land on the row already in force.)
            self.pick = None;
            return None;
        }
        self.visibility = next;
        // **And the body the set names moves with it**, through the one writer: `folded` and
        // `open` are the fold this head has always had (`ctrl-r`, `/t`), so a profile that
        // says `tools: folded` sets the fold and not only the word. `hidden` is left alone —
        // a fold on a row nobody draws is not a fact about the screen, and the rows are the
        // rung's business.
        for (show, level) in [
            (Show::Tools, next.level(Show::Tools)),
            (Show::Thinking, next.level(Show::Thinking)),
        ] {
            match level {
                Level::Open => self.set_fold(show, Fold::Open),
                Level::Folded => self.set_fold(show, Fold::Folded),
                Level::Hidden => {}
            }
        }
        self.raw_calls = next.shows(Show::RawCalls);
        // **A set that hides rows can hide the one the reader is holding** (R37's consequence
        // for R36), so the view moves onto its nearest surviving neighbour at the moment of the
        // change, where the fact is known for certain.
        self.reanchor_off_hidden();
        // **And whatever was OPEN is closed, because it was open in the other rendering**
        // (R37 AMENDED). `payload_sel` names one thing — a payload window under every other
        // rendering, the run `ctrl-t` opened under this one — and the same id means a different
        // thing on either side of the change. Carrying it across opened a payload window on
        // a row the reader had not asked about, which is the surprise this field exists to
        // avoid: *I opened a run, changed my mind about the rung, and a window appeared.*
        self.payload_sel = None;
        self.payload_page = 0;
        self.invalidate_history();
        // **And it is written down**, which is what *"persists headrestarts"* asks for: the
        // change and the file are one act from here, so a set cannot be chosen and then
        // forgotten. `RetiredWrite::Union` because this is not a statement about the retired
        // set — the fold and diff toggles pass it for the same reason.
        let saved = self.save_prefs(RetiredWrite::Union);
        self.say(&if next.hides_the_working() {
            format!(
                "verbosity {} (was {}) — the conversation and the edit cards, and nothing else \
                 the head made. Tool calls, reasoning and head arrivals are HIDDEN, not \
                 dropped: `/verbosity` brings them back and the span you had it on is drawn \
                 again. This applies to the whole transcript, already drawn.{saved}",
                next.as_str(),
                was.as_str()
            )
        } else {
            format!(
                "verbosity {} (was {}) — this applies to the whole transcript, already drawn, \
                 not only to what comes next.{saved}",
                next.as_str(),
                was.as_str()
            )
        });
        None
    }

    /// **The one writer of the two folds** — so a switch moved by a chord, by `/t`, by the
    /// config pane or by a profile all land in the same field, and none of them can move the
    /// other's.
    pub(crate) fn set_fold(&mut self, show: Show, fold: Fold) {
        match show {
            Show::Tools => {
                self.tools = fold;
                // **`/t` moves one key for the long rows**, and an echo's headline is one of
                // them (R33), so the two travel together here as they did at every other
                // writing of this field.
                self.echo_open = fold.is_open();
            }
            Show::Thinking => self.reasoning = fold,
            Show::Edits | Show::System | Show::RawCalls => {}
        }
    }

    /// **Set the diff style by name, or refuse by name** — R38's second setting.
    ///
    /// A local setting like the rung above, and one place for the same reason: the card's
    /// `Enter` and the typed word must not be able to disagree.
    pub(crate) fn set_diff(&mut self, typed: &str) -> Option<Action> {
        // **`DiffPref::parse` and not a second list.** The preference file is SHARED with
        // the other head, which accepts `split` / `side-by-side` / `auto` and `unified` /
        // `single` on input and writes back exactly two words — so the spellings a value can
        // arrive in are already a fact of this tree, and a verb with its own list would be a
        // verb that refused a value the file accepts. One parser.
        let split = match crate::prefs::DiffPref::parse(typed) {
            Some(crate::prefs::DiffPref::Split) => true,
            Some(crate::prefs::DiffPref::Unified) => false,
            None => {
                self.say(&format!(
                    "`{typed}` is not a diff style — the two are {}; `/diff` with nothing after \
                     it shows what each one means",
                    DIFF_VALUES
                        .iter()
                        .map(|(v, _)| *v)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
                return None;
            }
        };
        if split == self.diff_split {
            self.say(&format!(
                "diff is already {} — nothing changed",
                if split { "split" } else { "unified" }
            ));
            return None;
        }
        self.diff_split = split;
        // **The history holds RENDERED rows**, and a diff style decides what one of them
        // renders to — so the whole buffer is stale, not just the row that changed. The same
        // invalidation `/think` and `/t` make, for the same reason.
        self.invalidate_history();
        let saved = self.save_prefs(RetiredWrite::Union);
        self.say(&format!(
            "diff {} — every edit card, drawn and future{saved}",
            if split {
                "split (side by side)"
            } else {
                "unified"
            }
        ));
        None
    }

    /// The mode this session runs under, **as one of the daemon's own names** — see
    /// [`named_choice`], which is the whole of the reading.
    ///
    /// It used to be `value.split_whitespace().next()`, and that is wrong for a name that
    /// contains a space: `Mode::WRITES_ALLOWED` is spelled `writes allowed`, so the first word is
    /// `writes`, which names no choice. Both things that read this — the picker's cursor and the
    /// card's `← now` — then failed together, which is the operator's report exactly: *"permission
    /// mode menu no longer highlights the current mode when opened"*.
    pub(crate) fn mode_current(&self) -> String {
        let Some(r) = self.mode_row() else {
            return String::new();
        };
        match named_choice(&r.value, &r.choices) {
            Some(c) => c.to_string(),
            // **No row to mark, and the value is returned as it stands.** A current value that
            // names none of the choices is a real state — a daemon that lists fewer modes than it
            // accepts — and the honest render is no `← now` anywhere rather than one on the wrong
            // row. Returning the first word here is what made that case indistinguishable from a
            // name the head had failed to read.
            None => r.value.clone(),
        }
    }

    /// **Put the open picker's cursor on the row that answers now**, and remember that nothing
    /// has touched it yet — see [`App::pick_unseeded`].
    ///
    /// One function for the four cards, because they seed identically now that
    /// [`App::pick_current`] reads a row's value through [`named_choice`]: the mode, the model,
    /// the rung and the diff style all pick whichever of their values the current one names.
    /// They used to seed at four call sites with two spellings of the same rule, which is the
    /// shape this file keeps deleting.
    pub(crate) fn seed_pick(&mut self) {
        let values = self.pick_values();
        let now = self.pick_current();
        self.mode_sel = values.iter().position(|(n, _)| *n == now).unwrap_or(0);
        self.pick_unseeded = true;
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
    /// **Every verb this head offers, in one list** — its own and the daemon's (R32).
    ///
    /// The defect this replaces is measured, not argued: the completion table offered 27
    /// verbs while **five working daemon verbs were absent** (`/flowy`, `/gate`, `/job`,
    /// `/login`, `/supervise`) and three of the head's own (`/dismiss`, `/settings`,
    /// `/stats`). `docs/evidence/slash-completion-2026-09-23.py`.
    ///
    /// The two halves have two owners and neither enumerates the other:
    ///
    /// * **the head's**, from [`SLASH_COMMANDS`], which a test in this file holds against
    ///   the dispatcher's own source — so adding an arm without listing it fails the suite;
    /// * **the daemon's**, from the `daemon.verbs` `SettingRow`, because a head that
    ///   guessed at them is exactly how `/gate` and `/flowy` came to be missing while
    ///   working perfectly. An absent row means a daemon older than this one, and then the
    ///   head offers its own verbs and says nothing about the rest — which is the honest
    ///   answer, not a guess.
    ///
    /// The daemon's names carry no hint here: the daemon publishes names, not descriptions,
    /// and a head that invented a sentence about somebody else's verb would be writing the
    /// other half's documentation. They are drawn bare, which is also what tells a reader
    /// the two halves of the list apart without a label.
    pub(crate) fn command_names(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = SLASH_COMMANDS
            .iter()
            .map(|(n, h)| ((*n).to_string(), (*h).to_string()))
            .collect();
        for v in self.daemon_verbs() {
            // **A verb both halves reach is offered once, with the head's hint.** `/jobs`
            // is the live case: the head opens the pane and the daemon reads a job's
            // output, and the head's arm wins. Drawn twice it would read as two verbs.
            if !out.iter().any(|(n, _)| *n == v) {
                out.push((v, String::new()));
            }
        }
        // **And the door's, hyphenated** — R34. Offered from the daemon's own row, so this
        // head still holds no schema: it knows a name, a field and a kind, and the verb it
        // offers is a textual transform of the name rather than a second list.
        for t in self.door_tools() {
            let verb = letibot_sessionlog::head_run_verb(&t.name);
            if out.iter().any(|(n, _)| *n == verb) {
                continue;
            }
            let hint = if t.field.is_empty() {
                // **Said on the row rather than left to a refusal**, which is R31's own
                // requirement: *the head says which kind it is refusing* rather than
                // leaving the operator to guess which tools are which.
                "JSON arguments".to_string()
            } else {
                format!("{} — {}", t.kind, t.field)
            };
            out.push((verb, hint));
        }
        out
    }

    /// **The door's tools, as the daemon described them** — R31.
    pub(crate) fn door_tools(&self) -> Vec<letibot_sessionlog::HeadRunTool> {
        self.settings
            .iter()
            .find(|r| r.key == letibot_sessionlog::HEAD_RUN_TOOLS_KEY)
            .map(|r| r.tools.clone())
            .unwrap_or_default()
    }

    /// **A door verb the operator typed, turned into the call the wire wants** — R31, R34.
    ///
    /// Returns the tool's OWN name and the arguments object, or a sentence saying why not.
    /// The head's whole knowledge is the three facts on the row: **which field a bare line
    /// goes into, what kind it is, and which fields have defaults.** Nothing here knows what
    /// `web_search` does.
    ///
    /// * **No bare form** (`field` empty) — the row's own `why_json` sentence is returned, so
    ///   the refusal names the reason the daemon gave rather than the head's guess at it.
    /// * **A line with nothing after the verb** — refused by name, because a bare call to a
    ///   tool that needs a query is a call nobody meant.
    /// * **Anything that looks like a JSON object** goes through untouched, which is the form
    ///   R31 keeps for a tool with several arguments. A line that *starts* with `{` is the
    ///   operator asking for the JSON form; there is no tool whose bare text begins that way
    ///   by accident and the ambiguity is resolved in favour of the form they can see.
    pub(crate) fn head_run_call(
        &self,
        tool: &letibot_sessionlog::HeadRunTool,
        line: &str,
    ) -> Result<String, String> {
        let line = line.trim();
        if line.starts_with('{') {
            // **Checked for BEING json and for nothing else** — the original rule, kept.
            let v: serde_json::Value = serde_json::from_str(line)
                .map_err(|e| format!("`{}` with `{{…}}` arguments: {e}", tool.name))?;
            if !v.is_object() {
                return Err(format!(
                    "`{}` takes an object of arguments; `{line}` is a {}",
                    tool.name,
                    match v {
                        serde_json::Value::Array(_) => "list",
                        serde_json::Value::String(_) => "string",
                        serde_json::Value::Number(_) => "number",
                        serde_json::Value::Bool(_) => "boolean",
                        serde_json::Value::Null => "null",
                        serde_json::Value::Object(_) => "object",
                    }
                ));
            }
            return Ok(v.to_string());
        }
        if tool.field.is_empty() {
            return Err(tool.why_json.clone());
        }
        if line.is_empty() {
            return Err(format!(
                "/{} WHAT — this one puts a bare line into `{}` ({})",
                letibot_sessionlog::head_run_verb(&tool.name),
                tool.field,
                tool.kind
            ));
        }
        // **The defaults travel with the line**, so the object the daemon runs is the one a
        // model's minimal call would have produced. Without them the same tool answers two
        // different questions depending on who asked — the daemon published them for exactly
        // this and a head that dropped them would be editing the call.
        let mut obj = serde_json::Map::new();
        obj.insert(
            tool.field.clone(),
            serde_json::Value::String(line.to_string()),
        );
        for (k, v) in &tool.defaults {
            obj.insert(k.clone(), serde_json::Value::String(v.clone()));
        }
        Ok(serde_json::Value::Object(obj).to_string())
    }

    /// The verbs the daemon published, from its settings row. Empty when it sent none.
    /// **The providers this box holds a key for**, from the daemon's own row —
    /// [`letibot_sessionlog::protocol::MODEL_KEYS_KEY`].
    ///
    /// Empty when the row is absent, which is a daemon older than this one: **no greening**
    /// rather than every row greened, the same rule `daemon_verbs` follows. The head keeps no list
    /// of its own because it *cannot* have one — whether a preset resolves a key is a fact about
    /// this box's files and environment (`keys::resolve`), and the daemon is the half that reads
    /// them.
    pub(crate) fn keyed_providers(&self) -> Vec<String> {
        self.settings
            .iter()
            .find(|r| r.key == letibot_sessionlog::protocol::MODEL_KEYS_KEY)
            .map(|r| {
                r.value
                    .split(',')
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// **The rows that need no credential**, from the daemon's own
    /// [`letibot_sessionlog::protocol::MODEL_KEYLESS_KEY`] row: `local` and every local
    /// model declared in `providers.toml`.
    ///
    /// Falls back to `local` alone when the row is absent, which is a daemon older than
    /// the row. That is exactly what this head did before the row existed, so an old
    /// daemon greens what it always greened and nothing reads as newly broken.
    pub(crate) fn keyless_choices(&self) -> Vec<String> {
        self.settings
            .iter()
            .find(|r| r.key == letibot_sessionlog::protocol::MODEL_KEYLESS_KEY)
            .map(|r| {
                r.value
                    .split(',')
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_else(|| vec!["local".to_string()])
    }

    /// **Whether a picker row is one this box can actually take.**
    ///
    /// The operator, 2026-10-04: *"model peeker should green models we have keys for. — if i
    /// choose a model without key picker should ask for the key."* So the colour answers *will this
    /// work if I press enter*.
    ///
    /// Two ways to be ready, and they are different facts: a credential this box holds
    /// ([`Self::keyed_providers`]), or **no credential wanted at all**
    /// ([`Self::keyless_choices`]). `local` was once hardcoded here for the second
    /// reason — *"leaving it uncoloured would read as this row has no key about the one
    /// row that never wanted one"* — and the operator found what a hardcoded literal
    /// costs the moment there is a second such row: *"dense78 needs a key this box does
    /// not hold"*, about a LAN box with no key and no meter. The daemon publishes the
    /// list now, for the same reason it publishes the keyed one: it is the half that
    /// knows.
    pub(crate) fn choice_ready(&self, name: &str) -> bool {
        let name = name.trim();
        // **Three ways a row can name its model, so three candidates.**
        //
        // `deepseek/deepseek-flash` -- the preset is the part before the slash.
        // `dense78` -- a declared name, whole, and it may contain anything the operator
        // typed. `dense78 (qwen-3.8-27b at http://192.168.1.78:8082)` -- the row for a
        // session already ON one, where the first WORD is the name; the `model` row's
        // own comment is the rule (*"the first word is what a picker matches on"*), and
        // splitting that on `/` lands inside the url instead.
        let first_word = name.split_whitespace().next().unwrap_or(name);
        let provider = first_word.split('/').next().unwrap_or(first_word);
        let keyless = self.keyless_choices();
        keyless
            .iter()
            .any(|k| k == name || k == first_word || k == provider)
            || self.keyed_providers().iter().any(|k| k == provider)
    }

    /// **Did the daemon publish its key row at all?** An absent `models.keys` is a daemon
    /// older than the field and reads as *no greening* — never as *no keys* — so the ask is
    /// gated on the row being present rather than on the key list being empty, and an older
    /// daemon never finds its switches blocked behind a prompt.
    pub(crate) fn keys_row_present(&self) -> bool {
        self.settings
            .iter()
            .any(|r| r.key == letibot_sessionlog::protocol::MODEL_KEYS_KEY)
    }

    pub(crate) fn daemon_verbs(&self) -> Vec<String> {
        self.settings
            .iter()
            .find(|r| r.key == letibot_sessionlog::protocol::DAEMON_VERBS_KEY)
            .map(|r| {
                r.value
                    .split(',')
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn complete_slash(&mut self) {
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
                let word = names[*idx].clone();
                self.set_composer(&format!("/{word}"));
                return;
            }
        }
        // **The needle is normalised the same way the lookup is** (R34), so a person who
        // typed `/web_` because that is what the daemon calls the tool gets `/web-search`
        // offered rather than *no /command starts with*. The transform runs on both sides
        // of the comparison, which is what makes it a transform rather than a second list.
        let needle = text[1..].replace('_', "-");
        let names: Vec<String> = self
            .command_names()
            .into_iter()
            .map(|(n, _)| n)
            .filter(|n| n.replace('_', "-").starts_with(&needle))
            .collect();
        match names.first() {
            Some(first) => {
                let first = first.clone();
                self.completion = Some((text.clone(), names, 0));
                self.set_composer(&format!("/{first}"));
            }
            None => {
                self.completion = None;
                self.say(&format!("no /command starts with {text:?}"));
            }
        }
    }

    pub(crate) fn complete_shell(&mut self) {
        let text = self.editor.text().to_string();
        // The one recogniser: a `!` line is what the sessionlog crate says it is,
        // and a bang with no command after it is not one — so `!` alone does
        // nothing, the same refusal the send makes.
        if letibot_sessionlog::operator_shell_command(&text).is_none() {
            return;
        }
        // **A file name first, the way a shell completes one** — the operator, after
        // `! ./stroppy/build/stroppy` had to be typed out whole: *"when i do ! <command> i
        // dont get path name or context suggestion"*. The history and the model complete
        // whole LINES; a path is a word, and the filesystem is the one completer that is
        // free, local and never wrong about what exists. Only with the cursor at the end,
        // and only when it finds something — otherwise the line cycles below as before.
        if self.editor.cursor() == text.len()
            && let Some(done) = complete_path_word(&text, &self.wiring.workspace)
        {
            match done {
                PathCompletion::Line(line) => {
                    self.path_matches = None;
                    self.set_composer(&line);
                }
                PathCompletion::Choices { line, names } => {
                    if line != text {
                        self.set_composer(&line);
                    }
                    self.path_matches = Some((line, names));
                }
            }
            return;
        }
        // **The model's cycle, if it is live.** It is checked first because it is the
        // more recent answer: the operator exhausted the history to get here. An empty
        // lines list is the *waiting* state — the history is exhausted and the model
        // has not answered yet — and a Tab in that state re-checks the cache rather
        // than asking again.
        if let Some((prefix, lines, idx)) = &mut self.shell_model {
            let live = text.starts_with(prefix.as_str())
                && (lines.is_empty() || lines.get(*idx).is_some_and(|current| text == *current));
            if live {
                if lines.is_empty() {
                    self.shell_model_fallback(&text);
                    return;
                }
                *idx = (*idx + 1) % lines.len();
                let line = lines[*idx].clone();
                self.set_composer(&line);
                return;
            }
        }
        // **The history's cycle, if it is live.** When it is exhausted — the next Tab
        // would wrap to the first history candidate — the model is the fallback, asked
        // once, and its suggestions are cycled instead. **The wrap is what changes**:
        // the composer stays on the last candidate rather than snapping back to the
        // first, and that candidate is the prefix the model is asked about, because a
        // proposal for the line the operator is looking at is a completion and a
        // proposal for a line they have already scrolled past is the same line offered
        // twice. A model with nothing to offer leaves the composer where it is and says
        // so; the history's candidates are all still one character away, since a prefix
        // typed on is matched fresh.
        if let Some((_, lines, idx)) = &mut self.completion {
            let live = lines.get(*idx).is_some_and(|current| text == *current);
            if live && !lines.is_empty() {
                let next = (*idx + 1) % lines.len();
                if next == 0 {
                    self.completion = None;
                    self.shell_model_fallback(&text);
                    return;
                }
                *idx = next;
                let line = lines[*idx].clone();
                self.set_composer(&line);
                return;
            }
        }
        // No live cycle: start the history's, or fall to the model when the history
        // has nothing for this prefix.
        let lines: Vec<String> = self
            .shell_candidates()
            .iter()
            .filter(|line| line.starts_with(&text))
            .cloned()
            .collect();
        match lines.first() {
            Some(first) => {
                let first = first.clone();
                self.completion = Some((text.clone(), lines, 0));
                self.set_composer(&first);
            }
            None => {
                self.completion = None;
                self.shell_model_fallback(&text);
            }
        }
    }

    /// **The model's half of the `!` completion, asked once per (prefix, position).**
    ///
    /// Called when the history has no match for the prefix, or its cycle is exhausted.
    /// It is the whole of the "ask the model" decision, and the rule it keeps is that
    /// **the same prefix asked twice is not two model calls**: an answered ask is
    /// cycled from the cache, an in-flight ask is waited on, and only a prefix never
    /// asked is sent to the daemon.
    ///
    /// **The prefix is the line in the composer, not the line the operator started
    /// with.** The two are the same thing whenever the history had no match, which is
    /// the case the feature is for. At the end of a history cycle they are not: the
    /// composer holds the last candidate the history offered, and that is the line the
    /// model is asked to complete. Asking about the typed prefix instead would let a
    /// proposal come back that the operator has just cycled past — the same line
    /// offered twice in a row, once as a fact and once as a guess — and the rule the
    /// daemon's prompt carries is that a wrong suggestion is worse than none.
    ///
    /// **Presence in the cache is the answer, not the length of the list.** A model
    /// that said nothing usable *said something*, and an empty answer read as *not
    /// asked* would send a fresh call on every Tab for the same prefix — the one thing
    /// the (prefix, position) key exists to prevent.
    ///
    /// The position is the number of transcript rows, so a row landing is a new
    /// position and a fresh ask — the cache is cleared when the transcript advances,
    /// and a suggestion built on the conversation as it was is a suggestion about
    /// that conversation.
    ///
    /// **Nothing here submits.** The answer is a list of candidate lines for the
    /// composer, drawn as candidates with their provenance, and Enter is still the
    /// operator's.
    pub(crate) fn shell_model_fallback(&mut self, text: &str) {
        let position = self.items.len() as u64;
        let key = (text.to_string(), position);
        // Already answered for this prefix at this position: cycle the cached lines,
        // or say that the model had nothing and do not ask again.
        if let Some(lines) = self.shell_suggestions.get(&key) {
            let lines = lines.clone();
            if lines.is_empty() {
                // The model said nothing usable. That is a good answer rather than a
                // failure — the rule it was given is that a wrong suggestion is worse
                // than none — so it is said plainly, and the cycle stays live with
                // nothing in it: the next Tab says the same thing without a call.
                self.shell_model = Some((text.to_string(), Vec::new(), 0));
                self.say(&format!("the model has no ! line starting with {text:?}"));
                return;
            }
            let first = lines[0].clone();
            self.shell_model = Some((text.to_string(), lines, 0));
            self.set_composer(&first);
            return;
        }
        // Already asked for this prefix at this position: the response is in flight.
        // Enter the waiting state and say so, rather than asking again.
        if self
            .shell_ask
            .values()
            .any(|(p, n)| p == text && *n == position)
        {
            self.shell_model = Some((text.to_string(), Vec::new(), 0));
            self.say(&format!(
                "asking the model for a ! line starting with {text:?}…"
            ));
            return;
        }
        // Ask the model, once. The id is minted here so the head can recognise the
        // answer on the way back; the daemon echoes it in `ShellSuggestions`.
        let id = self.next_shell_ask_id();
        self.shell_ask
            .insert(id.clone(), (text.to_string(), position));
        self.shell_model = Some((text.to_string(), Vec::new(), 0));
        self.queued.push(Action::SuggestShell {
            prefix: text.to_string(),
            client_request_id: id,
        });
        self.say(&format!(
            "asking the model for a ! line starting with {text:?}…"
        ));
    }

    /// The next `SuggestShell`'s `client_request_id`, beside `next_head_run` and for
    /// the same reason: the id has to be unique per head, and the head is the one
    /// that has to recognise it when the answer comes back on the pump.
    pub(crate) fn next_shell_ask_id(&mut self) -> String {
        self.shell_ask_seq += 1;
        format!("{}-s{}", self.head_id, self.shell_ask_seq)
    }

    /// **The transcript moved, so everything derived from it is stale.**
    ///
    /// Two things are derived from the rows and both are held between frames: the `!`
    /// candidate list ([`App::shell_candidates_memo`]), and the model's suggestions —
    /// which are answers about the conversation *as it was*, and a conversation that
    /// moved is a different question. One method, because the two call sites are the
    /// two ways the transcript changes and a third caller is a third chance to
    /// remember only one of them.
    ///
    /// **A body landing counts as a move**, which is why [`App::record_item`] calls it
    /// too: a row announced with no body carries no tool calls yet, and a prompt built
    /// on it would be a prompt about a row that had not arrived.
    pub(crate) fn the_rows_moved(&mut self) {
        self.shell_candidates_memo = None;
        self.clear_shell_suggestions();
    }

    /// **The model's suggestions are stale: the conversation moved.**
    ///
    /// A suggestion is built on the conversation as it was when it was asked, and a
    /// conversation that moved is a different question. So when a row lands — or a
    /// snapshot replaces the rows — the asks in flight and the answered asks are
    /// both dropped, and the next Tab for the same prefix is a fresh ask rather than
    /// a stale answer. The model's cycle is dropped too: it is cycling lines about a
    /// conversation that no longer is, and a character typed on would match fresh
    /// anyway.
    ///
    /// **Called from [`App::the_rows_moved`], which is the only caller besides the link
    /// going down** — that is deliberate, because the two are always stale together and
    /// a call site that remembered one of them would be a call site that forgot the
    /// other.
    pub(crate) fn clear_shell_suggestions(&mut self) {
        if self.shell_ask.is_empty()
            && self.shell_suggestions.is_empty()
            && self.shell_model.is_none()
        {
            return;
        }
        self.shell_ask.clear();
        self.shell_suggestions.clear();
        self.shell_model = None;
    }

    /// **The whole `!` lines this session has run**, newest first, deduped: the
    /// operator's own `!` rows verbatim, and the model's `bash` calls as `! ` plus
    /// the command they ran.
    ///
    /// **Newest first, because that is what a person re-running a command wants**:
    /// the last thing they did is the most likely thing they are about to do again.
    /// Deduped keeping the newest, so a command run twice is offered once, as the
    /// line it most recently was.
    ///
    /// **Held between frames**, because this is the render path's as well as Tab's:
    /// see [`App::shell_candidates_memo`] for the measurement that made it one walk
    /// per row change rather than one per frame.
    /// **The composer's Up history is the session's, not this head's.**
    ///
    /// The operator: *"i worked - sent 30 prompts. then restart, send 2. and arrow up sees
    /// only these two"*. The editor's history lived in the head process, so a restarted
    /// head — or a second head on the same session — recalled only what it had typed
    /// itself. The session's own record has every prompt: the operator's `User` rows, in
    /// order, which is what the `!` candidates already walk. Refreshed at the start of a
    /// recall, so it is the session on screen now; lines this head typed that the session
    /// does not hold (yet) stay at the end, newest last.
    pub(crate) fn refresh_prompt_history(&mut self) {
        let mut merged: Vec<String> = Vec::new();
        for r in &self.items {
            let Some(TranscriptItem::User {
                speaker: letibot_transcript::Speaker::Operator,
                parts,
                ..
            }) = r.item.as_ref()
            else {
                continue;
            };
            let text = parts
                .iter()
                .filter_map(|p| match p {
                    UserPart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ");
            let text = text.trim();
            if !text.is_empty() && merged.last().map(String::as_str) != Some(text) {
                merged.push(text.to_string());
            }
        }
        for own in self.editor.history().to_vec() {
            if !merged.contains(&own) {
                merged.push(own);
            }
        }
        self.editor.set_history(merged);
    }

    pub(crate) fn shell_candidates(&mut self) -> &[String] {
        if self.shell_candidates_memo.is_none() {
            self.shell_walks += 1;
            self.shell_candidates_memo = Some(self.walk_shell_candidates());
        }
        self.shell_candidates_memo.as_deref().unwrap_or(&[])
    }

    /// The walk itself — what [`App::shell_candidates`] caches, and the only place that
    /// reads the rows for it.
    ///
    /// **The walk is the head's own rows** — the snapshot items, `item` an
    /// `Option` because a row can be announced before its body lands — walked the
    /// way `targets_before` walks them. A `bash` call whose arguments do not parse,
    /// or that carries no `command`, is skipped: a candidate that cannot be re-run
    /// is not a candidate.
    pub(crate) fn walk_shell_candidates(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for r in self.items.iter().rev() {
            let Some(item) = r.item.as_ref() else {
                continue;
            };
            match item {
                TranscriptItem::User {
                    speaker: letibot_transcript::Speaker::Operator,
                    parts,
                    ..
                } => {
                    // The row's text, the way the renderer reads it: the text parts
                    // joined. A `!` line is one part, so this is the line verbatim.
                    let text = parts
                        .iter()
                        .filter_map(|p| match p {
                            UserPart::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    if text.starts_with('!') {
                        out.push(text);
                    }
                }
                TranscriptItem::Assistant { tool_calls, .. } => {
                    for c in tool_calls {
                        if c.name != "bash" {
                            continue;
                        }
                        let Ok(v) = serde_json::from_str::<serde_json::Value>(&c.arguments) else {
                            continue;
                        };
                        let Some(cmd) = v.get("command").and_then(|c| c.as_str()) else {
                            continue;
                        };
                        out.push(format!("! {cmd}"));
                    }
                }
                _ => {}
            }
        }
        // Dedupe keeping the newest (first) occurrence.
        let mut seen = std::collections::HashSet::new();
        out.retain(|line| seen.insert(line.clone()));
        out
    }

    /// Replace the whole composer line. Completion words are single tokens, so
    /// Home + kill-to-end + insert is the honest way there: the editor has no
    /// text setter, and the three public ops keep its undo and history exactly
    /// as true as any typed edit.
    pub(crate) fn set_composer(&mut self, text: &str) {
        self.editor.key(letibot_ui::editor::Key::Home, self.now_ms);
        self.editor
            .key(letibot_ui::editor::Key::KillToEnd, self.now_ms);
        self.editor.insert(text);
    }

    /// **Whether the composer holds a line the completion row belongs to** — the SHAPE
    /// that *could* be completed, whether or not anything matches it right now.
    ///
    /// This is the row's gate and the frame's reservation in one place, because the height
    /// is computed from it and the content is computed from it: two spellings of the shape
    /// would be a frame that reserves a row and draws nothing in it, or draws a row it did
    /// not count — and either one is the transcript moving on its own.
    ///
    /// **The shape, and not the candidate list.** A predicate that answered *there is a
    /// match* would flicker as the operator types — `! cargo` matches and `! cargo x` does
    /// not — and every flicker takes a line from the conversation above it, which is the
    /// defect the reservation exists to stop. The shape changes only when the operator
    /// starts or abandons such a line; an ordinary line, and an empty composer, are not
    /// ones: `hello` completes nothing, so it pays nothing.
    pub(crate) fn completion_slot(&self) -> bool {
        let text = self.editor.text();
        text.starts_with('!') || (text.starts_with('/') && !text.contains(char::is_whitespace))
    }

    pub(crate) fn command(&mut self, cmd: &str) -> Option<Action> {
        // **The operator's own tool call** — R24 part two's door, R31's bare form, R34's
        // hyphen.
        //
        // First, because a door verb is neither this head's nor the daemon's other half's:
        // it is a TOOL, and it has to be matched against the list the daemon published
        // before anything else gets to refuse it as an unknown word.
        //
        // **Both spellings are accepted and only one is offered** (R34): the operator who
        // types `/web_search` because that is what the daemon calls it *"should not be told
        // they are wrong"*, so the transform runs at the lookup and the tool's own name goes
        // on the wire. The head holds no schema — it knows which field takes the line and
        // what kind it is, both published on the row.
        let (typed_verb, rest) = match cmd.trim().split_once(char::is_whitespace) {
            Some((v, r)) => (v, r),
            None => (cmd.trim(), ""),
        };
        if !typed_verb.is_empty() {
            let tools = self.door_tools();
            let allow = letibot_sessionlog::HEAD_RUN_TOOLS;
            if let Some(tool) = letibot_sessionlog::head_run_tool(typed_verb, &allow)
                .and_then(|name| tools.iter().find(|t| t.name == name))
            {
                return match self.head_run_call(tool, rest) {
                    Ok(arguments) => Some(Action::HeadRun {
                        name: tool.name.clone(),
                        arguments,
                    }),
                    Err(why) => {
                        self.say(&why);
                        None
                    }
                };
            }
        }

        // **`/cells MESSAGE` — the message, and what is on this screen with it.**
        //
        // `harness what=screen` lets the model ASK; this is the operator pointing.
        // Same rows, same bytes, and captured here rather than a moment later on
        // purpose: the screen being talked about is the one that was there when
        // Enter was pressed, and a turn takes seconds during which it moves.
        //
        // Rendered from this head, at this head's size, escape codes intact — the
        // whole point is what is actually painted, not a description of it.
        if let Some(rest) = verb_arg(cmd, "cells") {
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
        if let Some(title) = verb_arg(cmd, "new") {
            self.want_new_session = true;
            self.say("making a session…");
            return Some(Action::NewSession(title.trim().to_string()));
        }
        if cmd == "copy" {
            self.copy_command();
            return None;
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
        if let Some(title) = verb_arg(cmd, "rename") {
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
        if let Some(name) = verb_arg(cmd, "mode") {
            let name = name.trim().to_string();
            if self.session_id.is_empty() {
                self.say("not attached to a session yet");
                return None;
            }
            if name.is_empty() {
                // Bare `/mode` opens the picker rather than printing a list to
                // copy a name out of. The rows come from the daemon's last
                // answer, and asking again — the way `/config` does on open —
                // is what keeps the `← now` marker honest when the mode moved
                // since this head last asked. The cursor is seeded to the mode
                // the session is already under, so Enter on an untouched list
                // is a no-op; the answer arriving does not re-seed it, so an
                // arrow pressed while the ask was in flight is not undone.
                self.pick = Some(Pick::Mode);
                self.picker = false;
                self.config_pane = false;
                self.seed_pick();
                self.redraw = true;
                return Some(Action::Settings);
            }
            return self.mode_action(name);
        }
        match cmd {
            "quit" | "q" => {
                self.quit = true;
                Some(Action::Quit)
            }
            "resync" => Some(Action::Resync),
            // **`/notes` — the disclosures this head has shown** (R10), and
            // `/dismiss` — the same action under the word a person types at a wall
            // of red. Both land in `notes_command`, so the two spellings cannot
            // disagree about what retired means.
            //
            // Placed before the one-word arms because it takes arguments: `rest`
            // is everything after the verb, and `/notes` with nothing after it
            // lists rather than acting.
            _ if verb_arg(cmd, "notes")
                .or_else(|| verb_arg(cmd, "dismiss"))
                .is_some() =>
            {
                let (verb, rest) = if let Some(rest) = verb_arg(cmd, "notes") {
                    ("notes", rest)
                } else {
                    ("dismiss", verb_arg(cmd, "dismiss").unwrap_or(""))
                };
                return self.notes_command(verb, rest);
            }
            "help" | "h" | "?" => {
                self.help = !self.help;
                self.pane_scroll = 0;
                self.redraw = true;
                None
            }
            // Where the bottom border's telemetry went. See `App::stats`.
            "status" | "stats" => {
                self.stats = !self.stats;
                self.pane_scroll = 0;
                self.redraw = true;
                // **Opening the screen IS the acknowledgement** (R51 item 17). The `⚠` on the
                // composer's edge is a pointer at these numbers, so the act of going to read them
                // is what clears it — there is no separate key, and there should not be: a second
                // verb for *I have read it* is a second thing to learn about a mark whose whole
                // job is to send you here.
                //
                // **Only on the way IN.** Closing it acknowledges nothing, and a reader who
                // opened it by accident and did not look has still not read the numbers — but they
                // also cannot have missed them, because the screen is the thing they were looking
                // at. The asymmetry that matters is the one `Counters::exceeds` holds: a counter
                // that moves AFTER this brings the mark straight back.
                if self.stats {
                    self.acknowledge_counters();
                }
                None
            }
            "think" | "r" => {
                self.reasoning = self.reasoning.flip();
                self.refold();
                None
            }
            // **`/tools` asks what this conversation can call.** It used to be a
            // second spelling of ctrl-t, which already folds tool output and is
            // the key anybody actually uses for it. The operator: *"it toggles
            // tools view but i think i want it to show me currently seated
            // tools"*. The listing is the question worth a word; the fold keeps
            // its key, and `/t` keeps the old behaviour for the fingers that
            // learnt it.
            // **`/t` unfolds every tool row at once and is the fold's only spelling**
            // (R10 moved it off `ctrl-t`, which now opens one result's window — see
            // `Key::CtrlT`). It used to be the legacy spelling of a chord that already
            // did this, which made it a synonym nothing pointed at; now it is the name.
            "t" => {
                self.tools = self.tools.flip();
                // **One key for the long rows, and that includes an echo** (R33). A
                // queued prompt is drawn as one elided headline and this is what opens
                // it; a second fold chord for a second kind of row is a second thing to
                // learn, and the operator asked for *"expandable the usual way"*.
                self.echo_open = !self.echo_open;
                self.refold();
                None
            }
            // **Bare, it opens the card; named, it sets the rung** — R38's rule, and the
            // shape `/mode` already had: a setting with more than two values is CHOSEN from a
            // card showing all of them, and cycling makes the reader hold the list in their
            // head and discover the current value by changing it. With R37's fourth rung that
            // was up to three presses and three repaints.
            _ if verb_arg(cmd, "verbosity").is_some() => {
                let rest = verb_arg(cmd, "verbosity").unwrap_or("").trim().to_string();
                if rest.is_empty() {
                    self.pick = Some(Pick::Verbosity);
                    self.picker = false;
                    self.config_pane = false;
                    self.seed_pick();
                    self.redraw = true;
                    return None;
                }
                return self.set_verbosity(&rest);
            }
            // **`/diff` — R38's new setting.** It was `slash_refused` until today, and that
            // refusal is the card R29 was filed from.
            _ if verb_arg(cmd, "diff").is_some() => {
                let rest = verb_arg(cmd, "diff").unwrap_or("").trim().to_string();
                if rest.is_empty() {
                    self.pick = Some(Pick::Diff);
                    self.picker = false;
                    self.config_pane = false;
                    self.seed_pick();
                    self.redraw = true;
                    return None;
                }
                return self.set_diff(&rest);
            }
            // **`/v` keeps meaning *the next rung*.** It is an alias in `HEAD_COMMAND_ALIASES`
            // — deliberately not offered by tab, deliberately still taken — and one key's worth
            // of cycling is a promise this head made. R38 does not revoke it: what R38 rules out
            // is having to CYCLE to find out what the values are, and the card is where they are
            // read. It goes through the same function as the card and the long spelling, so the
            // three cannot disagree.
            "v" => return self.set_verbosity("v"),
            "config" | "settings" => {
                self.config_pane = !self.config_pane;
                self.config_sel = 0;
                self.pane_scroll = 0;
                // One list on the screen at a time, the same rule the pickers
                // keep between themselves.
                if self.config_pane {
                    self.pick = None;
                }
                self.redraw = true;
                // Opening asks the daemon for its settings; the head's own are
                // already here. A pane drawn from the last answer would show the
                // mode the session had when this head attached.
                if self.config_pane && !self.session_id.is_empty() {
                    return Some(Action::Settings);
                }
                None
            }
            "jobs" => {
                self.jobs_pane = !self.jobs_pane;
                self.pane_scroll = 0;
                self.redraw = true;
                self.jobs_pane.then_some(Action::ListJobs)
            }
            // **The merge queue**, and it is not this session's: there is one `main` and one
            // queue, so the pane asks for the whole thing and the daemon answers with it.
            "queue" => {
                self.queue_pane = !self.queue_pane;
                self.queue_open = None;
                self.pane_scroll = 0;
                self.redraw = true;
                self.queue_pane.then_some(Action::ListMergeQueue)
            }
            // **§6: the panes and the promote, reachable as verbs.**
            //
            // Each of these had a chord and no word, which is two problems: a head
            // driven over a pipe — and a person who has not learnt the chord — cannot
            // reach them at all, and `/help` cannot teach a chord it has no name for.
            // The chords stay, because they are faster; both spellings end in the same
            // function, so they cannot drift.
            "todos" => self.toggle_todos(),
            _ if verb_arg(cmd, "todo").is_some() => {
                return self.todo_command(verb_arg(cmd, "todo").unwrap_or(""));
            }
            "subagents" => {
                self.toggle_subagents();
                None
            }
            "promote" => self.promote(),
            // **`/peek ID` reads one subagent's scrollback into the pane the tree's
            // Enter opens.** The id is the daemon's and an id it does not hold is
            // refused by name — the head keeps no list of subagents to validate
            // against, which would be a second copy of the tree it already folds.
            _ if verb_arg(cmd, "peek").is_some() => {
                let id = verb_arg(cmd, "peek").unwrap_or("").trim().to_string();
                if id.is_empty() {
                    self.say("/peek ID — the subagent to read; ctrl-g lists them");
                    return None;
                }
                if self.session_id.is_empty() {
                    self.say("not attached to a session yet");
                    return None;
                }
                self.sub_out_pending = Some(id.clone());
                Some(Action::Peek(id))
            }
            // **`/resume ID` brings a session that is on disk but not in this daemon
            // back to life, and goes there.** The same two steps `switch_to` takes for a
            // row the picker labels *on disk*: the head attaches to whatever the daemon
            // put it on and then asks, because "not held yet" is exactly the state a
            // resume is for, and `Attach` refuses a session the daemon does not hold.
            _ if verb_arg(cmd, "resume").is_some() => {
                let id = verb_arg(cmd, "resume").unwrap_or("").trim().to_string();
                if id.is_empty() {
                    self.say(
                        "/resume ID — a session on disk that this daemon is not holding; \
                         /sessions lists them",
                    );
                    return None;
                }
                self.want_new_session = true;
                self.say(&format!(
                    "resuming {} from the store…",
                    self.session_label(&id)
                ));
                Some(Action::ResumeSession(id))
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
                // **R16: the fork this head is asking for.** The daemon announces a
                // manual compaction only when it has finished (`compacted`), so the
                // mark has to be taken here, on the way out.
                self.mark_fork();
                Some(Action::Compact)
            }
            // **The tool list is in the prompt, and a prompt is fixed for a
            // conversation.** So this is the only way a session that opened without
            // a shell ever gets one: summarise, and continue under a prompt built
            // from what is seated now. Same refusal as `/compact` for the same
            // reason — it acts on the session this head is in, never one you are
            // only looking at.
            // Both kinds change message zero; the difference is what happens to
            // everything under it. **The lossless one is the default** — the
            // operator: *"id say flip it - reset is loseless and reset summarize
            // will be not"*. Re-seating is about the prompt, and paying for it
            // with the conversation should be the thing you ask for by name.
            "reseat" | "reseat keep" | "reseat verbatim" | "reseat summarise"
            | "reseat summarize" => {
                if self.session_id.is_empty() {
                    self.say("not attached to a session yet");
                    return None;
                }
                let summarise = cmd.ends_with("summarise") || cmd.ends_with("summarize");
                // **R16: the same mark, for the other fork.**
                self.mark_fork();
                if summarise {
                    self.say("re-seating: summarising, so the summary replaces the conversation…");
                } else {
                    self.say(
                        "re-seating: carrying the conversation across as it is. The next \
                         turn re-sends all of it once.",
                    );
                }
                Some(Action::Reseat { summarise })
            }
            other => {
                // The daemon's verbs. The head does not know them and does not
                // need to: the line goes over as typed and the answer comes back
                // on the session log.
                let verb = other.split_whitespace().next().unwrap_or("");
                // **Bare `/models` is the menu.** With a name after it the line
                // goes to the daemon as typed, which is what `/models X --once`
                // and `/models X --key K` need. The operator: *"for starters i
                // want it to be usual menu, like /mode"*.
                if matches!(verb, "models" | "model") && other.trim() == verb {
                    if self.session_id.is_empty() {
                        self.say("not attached to a session yet");
                        return None;
                    }
                    self.pick = Some(Pick::Model);
                    self.picker = false;
                    self.config_pane = false;
                    // Seeded to what answers now, so Enter on an untouched list
                    // is a no-op — the same courtesy the mode picker pays.
                    self.seed_pick();
                    self.redraw = true;
                    // **`Settings`, not the `models` verb.** Asking the daemon to
                    // refresh is right — a session whose model moved in another
                    // head would draw a stale `← now` — but `/models` with no
                    // argument answers with the whole provider listing, which
                    // landed on the session log underneath the card. The operator,
                    // looking at the wall of text this picker exists to replace:
                    // *"models is still not a selector"*.
                    //
                    // `Settings` is what `/mode` asks for and it prints nothing:
                    // it refreshes the rows the picker reads.
                    return Some(Action::Settings);
                }
                // **Every other verb goes to the daemon, and the head keeps no list.**
                //
                // There used to be an allowlist right here — twelve names — and it was
                // a second copy of the daemon's verb table, which is the mistake this
                // file has been burned by twice already (see the `mode` settings row,
                // which the head kept its own copy of and got wrong). Its failure mode
                // is the worst kind: the daemon gains a verb, this head is not rebuilt
                // with it, and the operator is told *"unknown command /import — try
                // /help"* about a command the other half implements. The head is then
                // lying about its own daemon, which is the defect class this repo
                // exists against.
                //
                // The comment four lines up already says the right thing — *"the head
                // does not know them and does not need to: the line goes over as typed
                // and the answer comes back on the session log"* — and then the code
                // refused the ones it had not heard of.
                //
                // Nothing is lost by forwarding, because the daemon answers an
                // unrecognised verb **by name**: `harnessd/src/slash.rs` builds
                // `/{verb} is not a daemon verb; /help lists the head's`, so the
                // question is settled by the half that owns the table and a typo gets
                // a better sentence than this head could write.
                if self.session_id.is_empty() {
                    self.say("not attached to a session yet");
                    return None;
                }
                Some(Action::Slash {
                    line: other.trim().to_string(),
                })
            }
        }
    }

    /// Post a transient line. It lives for [`NOTICE_MS`] of **wall time** and then gets
    /// out of the way; it does **not** take the input line's place, which is what the old
    /// one did — after the first prompt of a session there was nowhere to see what you
    /// were typing, for the rest of the session.
    ///
    /// **The one writer of the notice, and it is the one that starts the clock.** Both
    /// halves in one `setf` is the point: a sentence nobody timed is a sentence nobody can
    /// get rid of, and there is now no way to set one without starting its clock.
    pub(crate) fn say(&mut self, text: &str) {
        self.notice = Some(text.to_string());
        self.notice_until = Some(self.now_ms.saturating_add(NOTICE_MS));
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

    /// **The operator's rows, out of the union the daemon serves.** The head keeps no second
    /// list: the daemon persists them, every head sees them, and this is the one place that says
    /// which half of that list the operator wrote. Sending is the whole list, so a divergence
    /// between a head's copy and the store is impossible to accumulate.
    pub(crate) fn operator_todos(&self) -> Vec<letibot_sessionlog::event::TodoEntry> {
        self.todos
            .iter()
            .filter(|t| t.by == letibot_sessionlog::event::TodoBy::Operator)
            .cloned()
            .collect()
    }

    /// **Open the new-todo card**, and put the composer where the card expects it.
    ///
    /// **The composer is the field**, so it is emptied and handed to the card: a half-typed prompt
    /// left under a card whose Enter adds an item is the shape that costs somebody a message, which
    /// is the same reason `mode_confirm` takes the keyboard. leticl's `%todo-draft-open`.
    pub(crate) fn open_todo_card(&mut self) {
        if self.session_id.is_empty() {
            self.say("not attached to a session yet");
            return;
        }
        self.set_composer("");
        self.todo_draft = Some(TodoDraft::new());
        self.say(
            "adding a todo item — title, then tab for the description; enter adds it to the \
             session's plan as yours, esc cancels",
        );
        self.redraw = true;
    }

    /// **`/todo …` — the operator's own rows.** The verb's three forms, and each one sends the
    /// whole list:
    ///
    /// ```text
    /// /todo finish the parity row        add it, at the end
    /// /todo done 2                       mark the second of MY rows complete
    /// /todo rm 2                         take it off the board
    /// /todo postpone 2                   set it aside: it stays, and nothing nags about it
    /// /todo resume 2                     put it back in the list
    /// ```
    ///
    /// **Numbered over the operator's rows and not the union**, because the model's rows are not
    /// the operator's to edit — that is the same rule `/rename` and `/compact` keep about acting on
    /// the session you are in. The count is the one the pane prints for that half.
    ///
    /// **Marked complete rather than deleted** by `done`, which is the difference the daemon's own
    /// `set_operator_states` draws: a finished row is a record of work, and only `rm` takes one off
    /// the board. The model may move a row's STATUS (by quoting its words) and may not remove it,
    /// which is the operator's ruling recorded on `TodoBoard`.
    /// **THE OPERATOR'S OWN ROWS, DRAWN FROM THE MOMENT THEY ARE SENT** — their half of what
    /// `pending_prompts` does for their words.
    ///
    /// The operator: *"when i send todos they appear in the todo pane some time later — it feels
    /// like their appearance depend on the turn state. but to me - im not sure if i lost them or
    /// not."* They did not lose them: the write publishes at once (`set_operator_todos` has a
    /// retry for exactly that), but the COMMAND reaches the worker behind whatever is running
    /// and is injected at the next step boundary (`harness.rs`: *"the step boundary is the only
    /// place any of the three can be injected"*), and until `TodosUpdated` arrives the pane drew
    /// only what the daemon had published — nothing, for the whole of a running turn.
    ///
    /// So the head applies the list it is about to send, the same trust `pending_prompts` places:
    /// show what I sent until the daemon answers. `mine` is the operator's whole half (the
    /// command replaces it), so the optimistic state is the model's rows as they stand with this
    /// half in their place — and `TodosUpdated` replaces the union wholesale when it lands, in
    /// the daemon's own order, which is why nothing here needs to guess at that order for longer
    /// than the boundary.
    pub(crate) fn echo_operator_todos(&mut self, mine: Vec<letibot_sessionlog::event::TodoEntry>) {
        self.todos
            .retain(|t| t.by != letibot_sessionlog::event::TodoBy::Operator);
        self.todos.extend(mine);
        self.redraw = true;
    }

    /// **Is a starter-todo seed due for THIS project?** The three-part gate, in one place
    /// because the attach and the seed itself both ask it — a second spelling is how a project
    /// gets seeded by one reading and skipped by the other.
    ///
    /// The gate is leticl's `%seed-operator-todos` verbatim: the switch is on, the workspace is
    /// known (a project it cannot name is not a project it may put rows into — a head loads
    /// before the socket exists), and the record does not hold this workspace. **The record, not
    /// the list**: a starter row the operator deleted must not come back, which is what an *is
    /// the list empty* test would do on every restart.
    pub(crate) fn todo_seed_due(&self) -> bool {
        self.todo_template != crate::prefs::TodoTemplate::Off
            && !self.wiring.workspace.is_empty()
            && !self
                .todo_seed
                .contains(&todo_seed_key(&self.wiring.workspace))
    }

    /// Where the starter todos come from: `todo_template`'s three shapes as a path — leticl's
    /// `todo-template-path`, which its own docstring rules: *“one function because the setting
    /// has three shapes and two callers must not spell them differently.”* `None` when the
    /// switch is off or this head has no config directory to read a default from.
    pub(crate) fn todo_template_file(&self) -> Option<std::path::PathBuf> {
        match &self.todo_template {
            crate::prefs::TodoTemplate::Off => None,
            crate::prefs::TodoTemplate::Path(p) => Some(std::path::PathBuf::from(p)),
            crate::prefs::TodoTemplate::Default => self
                .prefs_path
                .as_ref()
                .and_then(|p| p.parent())
                .map(|d| d.join("todo-template.md")),
        }
    }

    /// **Copy the template TODO.md's items onto the operator's half of the board** — leticl's
    /// `%seed-operator-todos`, run where leticl runs it: the moment the head has learned its
    /// list. The behaviour copied, piece by piece:
    ///
    /// * **The template is a `TODO.md`, parsed by the same reader the repo section uses**
    ///   (`render_todo_md`) — a starter list is written in the format the operator already
    ///   writes by hand, boxes and indented bodies included, and there is no second syntax
    ///   to learn.
    /// * **ITEMS ONLY are copied**: the checkbox rows, not the headings and their roll-ups.
    ///   What is copied is what leticl copies — the text, the body, and the mark: `[x]`
    ///   seeds a completed row (how a template carries something already settled), anything
    ///   else seeds an open one.
    ///
    ///   The one place this cannot be leticl: **the body rides in `content`, joined `“ · ”`**.
    ///   leticl keeps a `:detail` beside its rows in its own sqlite; this head keeps no second
    ///   list, and the wire's `TodoEntry` is `content`/`status`/`by` — leticl's own push drops
    ///   the detail at the same door. The card already made this head's choice for a typed
    ///   detail (`title “ — ” detail`); body LINES join with `·` so each continuation stays a
    ///   segment rather than merging into one sentence.
    ///
    /// * **ONCE PER PROJECT, marked even when nothing was added** — an empty template records
    ///   the seeding and says so, because the alternative re-reads it on every start. A
    ///   MISSING file is not marked: leticl says why and leaves the project unseeded, so the
    ///   file the operator was going to write still gets its chance.
    /// * **Refusals are notes, not silences** — from the operator's side *the feature did not
    ///   work* and *I never turned it on* look identical otherwise.
    ///
    /// The rows go out through the same door `/todo TEXT` uses — `echo_operator_todos` for the
    /// optimistic view, `SetOperatorTodos` for the whole half — so a seed and a typed row cannot
    /// become different acts.
    pub(crate) fn seed_todos(&mut self) {
        if !self.todo_seed_due() {
            return;
        }
        let Some(path) = self.todo_template_file() else {
            self.say("todo_template is on but this head has no config directory to read it from");
            return;
        };
        let body = match std::fs::read_to_string(&path) {
            Ok(b) => b,
            Err(e) => {
                // Not marked — see the docstring: the file may still be written.
                self.say(&format!(
                    "todo_template is on but {} is not there: {e}",
                    path.display()
                ));
                return;
            }
        };
        let rows = render_todo_md(&body);
        let items: Vec<&TodoRow> = rows
            .iter()
            .filter(|r| r.item && !r.text.trim().is_empty())
            .collect();
        if items.is_empty() {
            self.mark_seeded();
            self.say(&format!(
                "{} has no items in it, so nothing was added",
                path.display()
            ));
            return;
        }
        let mut mine = self.operator_todos();
        for r in &items {
            let mut content = r.text.trim().to_string();
            if !r.body.is_empty() {
                content.push_str(" — ");
                content.push_str(&r.body.join(" · "));
            }
            mine.push(letibot_sessionlog::event::TodoEntry {
                content,
                status: if r.mark == Some(TodoMark::Done) {
                    letibot_sessionlog::event::TodoStatus::Completed
                } else {
                    letibot_sessionlog::event::TodoStatus::Pending
                },
                by: letibot_sessionlog::event::TodoBy::Operator,
                when: None,
            });
        }
        let n = items.len();
        self.mark_seeded();
        self.echo_operator_todos(mine.clone());
        self.queued.push(Action::SetOperatorTodos(mine));
        self.say(&format!(
            "{n} starter todo{} from {}",
            if n == 1 { "" } else { "s" },
            path.display()
        ));
    }

    /// **Record that this project has had its starter todos** — and do it BEFORE the send, for
    /// leticl's own reason: a record that waits for an acknowledgement re-fires on the next
    /// start if the write failed quietly, and the operator gets the duplicates this feature
    /// exists to avoid. The write is a UNION with whatever the file holds
    /// (`merge_todo_seed`) because two heads share one `head.toml` and a dropped record is a
    /// project that re-seeds.
    pub(crate) fn mark_seeded(&mut self) {
        let key = todo_seed_key(&self.wiring.workspace);
        if !self.todo_seed.contains(&key) {
            self.todo_seed.push(key);
        }
        if let Some(path) = self.prefs_path.clone() {
            let mut p = self.prefs();
            p.todo_seed = crate::prefs::merge_todo_seed(&path, &self.todo_seed);
            self.todo_seed = p.todo_seed.clone();
            if let Err(e) = crate::prefs::save(&path, &p) {
                self.say(&format!("seed not recorded: {e}"));
            }
        }
    }

    pub(crate) fn todo_command(&mut self, rest: &str) -> Option<Action> {
        let rest = rest.trim();
        if self.session_id.is_empty() {
            self.say("not attached to a session yet");
            return None;
        }
        let mut mine = self.operator_todos();
        // `done N` and `rm N` — a number is what the pane prints beside each of these rows.
        let (verb, arg) = match rest.split_once(char::is_whitespace) {
            Some((v, a)) => (v, a.trim()),
            None => (rest, ""),
        };
        match (verb, arg) {
            ("done" | "rm", n) if !n.is_empty() => {
                let Ok(at) = n.parse::<usize>() else {
                    self.say(&format!("`{n}` is not a row number — `/todo` lists yours"));
                    return None;
                };
                if at < 1 || at > mine.len() {
                    self.say(&format!(
                        "there is no row {at} of yours — you have {}",
                        mine.len()
                    ));
                    return None;
                }
                if verb == "rm" {
                    mine.remove(at - 1);
                    self.say(&format!("row {at} is off the board"));
                } else {
                    mine[at - 1].status = letibot_sessionlog::event::TodoStatus::Completed;
                    self.say(&format!("row {at} is done"));
                }
            }
            // **`postpone N` and `resume N` — the state the operator owns, over the same numbers
            // every other verb uses.**
            //
            // The operator's ask: *"can we handle postponed todo item properly? i.e. they persist
            // but without nag and with some counter visible to me"*. The state is THEIRS and this
            // is the door: a model that could set its own row aside would have a way to silence the
            // check that exists to stop it abandoning a plan, so `todo_write` still takes three
            // words and the two that are missing are here.
            //
            // **The verb pair and not a key on the row.** Enter on one of these rows already means
            // *toggle done* — the pane's own act since R44 — and a second row key would be a second
            // thing to learn for an act that has a typed door; these two are listed in
            // `SLASH_COMMANDS` (which is what `/help` and tab read) and named in the pane's own
            // hint line, which is how every other verb here is found.
            //
            // **`resume` and not a second spelling of `done`,** because the two answers are
            // different questions: `done` is *this is finished*, `resume` is *ask me about this
            // again*. Lifting a row puts it back as `pending` — open work, which is what the queue
            // and the idle check read — and **it keeps whatever condition it was carrying**: the
            // handle is not touched by either verb, so a row set aside while waiting on a job goes
            // back to waiting on the same one.
            //
            // A bare `postpone` or `resume` with no number is the text of a new row, exactly as a
            // bare `done` is — see the arm below, which is the one convention for all of them.
            ("postpone" | "resume", n) if !n.is_empty() => {
                let Ok(at) = n.parse::<usize>() else {
                    self.say(&format!("`{n}` is not a row number — `/todo` lists yours"));
                    return None;
                };
                if at < 1 || at > mine.len() {
                    self.say(&format!(
                        "there is no row {at} of yours — you have {}",
                        mine.len()
                    ));
                    return None;
                }
                if verb == "postpone" {
                    mine[at - 1].status = letibot_sessionlog::event::TodoStatus::Postponed;
                    self.say(&format!(
                        "row {at} is set aside — it stays on your list and the model still sees \
                         it, and nothing is reminded of it until you lift it with `/todo resume \
                         {at}`"
                    ));
                } else {
                    mine[at - 1].status = letibot_sessionlog::event::TodoStatus::Pending;
                    self.say(&format!(
                        "row {at} is back in the list — the check may ask about it again"
                    ));
                }
            }
            // **`when N JOB` — the condition, attached by number.** The operator's own shape: *"if
            // you are telling me 'job ends and i do this and that' then 'this and that' is a todo
            // item, which is conditioned by job status (end)"*, and *"when I file a todo"* is where
            // it belongs — the row is filed first and the condition is put on it here.
            //
            // **`when N -` clears it, and that is not a courtesy.** A condition nobody can take
            // off is a row waiting for ever on a job that already ended, and the store would go on
            // reporting it as due.
            ("when", both) => {
                let Some((n, handle)) = both.split_once(char::is_whitespace) else {
                    self.say(
                        "`/todo when N JOB` — a row number and the handle it waits on. \
                         `/todo when N -` takes the condition off.",
                    );
                    return None;
                };
                let (n, handle) = (n.trim(), handle.trim());
                let Ok(at) = n.parse::<usize>() else {
                    self.say(&format!("`{n}` is not a row number — `/todo` lists yours"));
                    return None;
                };
                if at < 1 || at > mine.len() {
                    self.say(&format!(
                        "there is no row {at} of yours — you have {}",
                        mine.len()
                    ));
                    return None;
                }
                if handle == "-" {
                    mine[at - 1].when = None;
                    self.say(&format!("row {at} no longer waits on anything"));
                } else {
                    mine[at - 1].when = Some(letibot_sessionlog::event::TodoCondition::Job {
                        handle: handle.to_string(),
                    });
                    self.say(&format!(
                        "row {at} is due once `{handle}` is not running — a job this daemon has \
                         never heard of counts as ended, which is what a restart looks like."
                    ));
                }
            }
            // Anything else is the text of a new row — including a line that begins with a number,
            // or with `done` and no argument, because those are sentences somebody could type.
            _ if !rest.is_empty() => {
                mine.push(letibot_sessionlog::event::TodoEntry {
                    content: rest.to_string(),
                    status: letibot_sessionlog::event::TodoStatus::Pending,
                    by: letibot_sessionlog::event::TodoBy::Operator,
                    when: None,
                });
                self.say(&format!("added to your list — {} row(s)", mine.len()));
            }
            // **Bare `/todo` opens the card**, which is the shape `/mode` and `/models` keep: a
            // setting or an act with more than one part is CHOSEN from a card rather than typed
            // blind. The three forms are still there and the card's hint line says where.
            _ => {
                self.open_todo_card();
                return None;
            }
        }
        self.echo_operator_todos(mine.clone());
        Some(Action::SetOperatorTodos(mine))
    }

    /// **`/todos` and `ctrl-p`, as one action.**
    ///
    /// A pane with a chord and no word is unreachable from a pipe and unteachable by
    /// `/help`, and two spellings of one action must not be two implementations.
    ///
    /// Returns the bootstrap read when the pane is opening: the daemon's todo list
    /// rides no snapshot, so a head that attached after the model last wrote has to ask.
    /// Later changes arrive as `TodosUpdated` and need no asking.
    pub(crate) fn toggle_todos(&mut self) -> Option<Action> {
        self.todos_pane = !self.todos_pane;
        // A pane opens at its top. Kept per-pane would be four fields that each go
        // stale; one field reset on every open is the same behaviour with nothing to
        // forget.
        self.pane_scroll = 0;
        self.redraw = true;
        if self.todos_pane {
            // Read at open, and re-read on every draw the file has moved under — see
            // `refresh_repo_todos`. The file is the operator's to edit, and a pane
            // showing an old read of it is a pane that lies quietly.
            self.refresh_repo_todos();
        }
        self.todos_pane.then_some(Action::ListTodos)
    }

    /// **EVERY ROW OF THE TODOS PANE THE CURSOR MAY LAND ON, in the order the pane draws them** —
    /// leticl's `todos-stops`, and the ONE enumeration everything about the cursor reads.
    ///
    /// Its docstring is the operator's two reports, and both were the same defect: R44's first cut
    /// spread this over a `-1` sentinel and the repo's own stop indices, and then *"arrows dont go
    /// here"* — the cursor moving to a row whose line the pane computed from another list's
    /// arithmetic — and *"mouse doesnt click"* — a click on the add row computing a negative index,
    /// thrown away. **Two enumerations was the defect.**
    ///
    /// Three kinds, and the tag carries IDENTITY rather than position: a position is a fact about
    /// the list when it was DRAWN, and a list changes between a draw and a keypress (a `TodosUpdated`
    /// arriving, a row removed), so every action would land on whatever took its neighbour's place.
    ///
    /// **The model's rows are NOT stops**, which is the R44 boundary and not an omission: no key
    /// acts on one — the model may move its own row's status and the operator may not — and a cursor
    /// that stops where no key acts is a cursor the operator presses keys into and nothing happens.
    /// They are skipped the way the repo's headings are.
    pub(crate) fn todos_stops(&self) -> Vec<TodoStop> {
        let mut out = vec![TodoStop::Add];
        for t in self
            .todos
            .iter()
            .filter(|t| t.by == letibot_sessionlog::event::TodoBy::Operator)
        {
            out.push(TodoStop::Mine(t.content.clone()));
        }
        if let Some(rows) = &self.repo_todos {
            for (i, r) in rows.iter().enumerate() {
                if r.item {
                    out.push(TodoStop::Repo(i));
                }
            }
        }
        out
    }

    /// **The pane row the stop at the cursor was DRAWN on**, read out of
    /// [`App::todos_stop_rows`] — the record the pane wrote while drawing, and not arithmetic over
    /// the lists it drew from. leticl's `todos-lines` second value, an `aref` of the third.
    ///
    /// The clamp is the one thing here that is not a read: a list can change under the cursor — a
    /// `TodosUpdated` arriving, a row removed, another workspace — and a key pressed against a
    /// shorter list must land on a row rather than on an index that no longer exists.
    pub(crate) fn todos_row_of(&self) -> usize {
        let at = self
            .todos_sel
            .min(self.todos_stop_rows.len().saturating_sub(1));
        self.todos_stop_rows.get(at).copied().unwrap_or(0)
    }

    /// **The stop drawn on SCREEN ROW `y`, or nothing** — leticl's `todo-stop-at-line`, and the
    /// answer to the operator's other report on this pane, *"mouse doesnt click"*.
    ///
    /// Read from the rows the last draw recorded, so a click and the drawing cannot disagree about
    /// where a row is. That disagreement is the whole of the old defect: letibot's pane took no
    /// click at all, and leticl's took one that computed the add row as a negative index and threw
    /// it away — the add row being the first selectable row and `line - header` making it negative.
    ///
    /// Guarded on the WINDOW: a row above the pane's top or below its last drawn row is not a row
    /// anybody is looking at, and a click into the blank space under a short list moves nothing.
    pub(crate) fn todo_stop_at_row(&self, y: u16) -> Option<usize> {
        let y = usize::from(y).checked_sub(self.todos_pane_top)?;
        if y >= self.pane_room {
            return None;
        }
        let pane_row = y + self.pane_scroll;
        self.todos_stop_rows.iter().position(|r| *r == pane_row)
    }

    /// **The repo cursor follows the stop cursor**, for the repo's own keys — the unfold and the
    /// body it shows read `repo_sel`, and a second cursor that did not follow would be the two
    /// enumerations this whole change removed.
    pub(crate) fn sync_repo_from_stop(&mut self, stops: &[TodoStop]) {
        let at = self.todos_sel.min(stops.len().saturating_sub(1));
        if let Some(TodoStop::Repo(i)) = stops.get(at) {
            self.repo_sel = *i;
        }
    }

    /// The same, for the subagent tree — `/subagents` and `ctrl-g`.
    ///
    /// **And the fold runs on the way in.** The pane used to be built only by the live
    /// `Subagent` events, and the comment here used to claim *"the tree is folded from
    /// durable `Subagent` events, which a snapshot carries"* — **which was not true of this
    /// daemon** (`SessionEvent::Subagent` was folded into nothing at all by the view: see
    /// the arm in `letibot_sessionlog::view`, which now folds it). So a head that attached
    /// after the spawns drew an empty pane and no count, and the only thing that could ever
    /// fill it was a later spawn — the operator: *"i just restarted the head and the
    /// subagents list is gone … when you started new subagents the subagents pane
    /// refreshed"*. Two durable halves now feed it: the snapshot's own children, and the
    /// daemon's session list — so [`App::fold_subagents`] reads it here, where the rows are
    /// about to be looked at.
    pub(crate) fn toggle_subagents(&mut self) {
        self.subagents_pane = !self.subagents_pane;
        self.pane_scroll = 0;
        self.fold_subagents();
        self.redraw = true;
    }

    /// **The pane's rows, as ONE enumeration** — the arrows, Enter, `p`, the drawn `▸` and the
    /// scroll all read this and nothing else.
    ///
    /// # The two groups, and why `finished` is folded
    ///
    /// The operator, 2026-10-06: *"i went to subagents panel and dont see it here"* — a subagent
    /// just started, and the pane drew the finished ones and pushed the running one off the
    /// bottom, because a child this head WATCHED spawn is appended after the durable rows
    /// ([`App::fold_subagents`]) and so lands LAST. (It is not a delay: the daemon publishes the
    /// child within a fraction of a second of the spawn — `subagent … open after 0.3s — running`
    /// is its own progress line — so the row is there and simply below the fold.) And then:
    /// *"please group finished separately in the finished group which will be collapsed"*.
    ///
    /// So the children still going come first, then one `finished (N)` row, then — only when it
    /// is unfolded — the finished children themselves. The active half is never empty for a live
    /// spawn, which is the whole point: the row the operator opened the pane to see is at the top.
    pub(crate) fn subagent_stops(&self) -> Vec<SubStop> {
        let mut out = Vec::with_capacity(self.subagents.len() + 1);
        for (i, s) in self.subagents.iter().enumerate() {
            if !s.is_finished() {
                out.push(SubStop::Agent(i));
            }
        }
        if self.subagents.iter().any(SubagentState::is_finished) {
            out.push(SubStop::Finished);
            if self.subagents_finished_open {
                for (i, s) in self.subagents.iter().enumerate() {
                    if s.is_finished() {
                        out.push(SubStop::Agent(i));
                    }
                }
            }
        }
        out
    }

    /// **The pane row the stop at the cursor was DRAWN on**, read out of
    /// [`App::subagents_stop_rows`] — the record the pane wrote while drawing, and not arithmetic
    /// over the lists it drew from. The sibling of [`App::todos_row_of`], and the clamp is the
    /// same: the list can change under the cursor, and an arrow pressed against a shorter list
    /// must land on a row rather than on an index that no longer exists.
    pub(crate) fn subagents_row_of(&self) -> usize {
        let at = self
            .subagents_sel
            .min(self.subagents_stop_rows.len().saturating_sub(1));
        self.subagents_stop_rows.get(at).copied().unwrap_or(0)
    }

    /// **The subagent rows: this session's children, as the DAEMON's list has them.**
    ///
    /// # Why this exists, and the two halves it joins
    ///
    /// The pane and the composer's count are the same list, and the list had exactly one
    /// source: the live `SessionEvent::Subagent` arm. That event carries **one child**, so
    /// it can only ever describe a spawn or a finish this head was attached for — a fresh
    /// head, a head that switched away and came back, and the parent of children spawned
    /// before it attached all drew an empty pane with no count, forever, because nothing
    /// replays a spawn. `App::load` used to clear the rows on every switch, on the belief
    /// that the live events were the only other source — and there was then nothing to put
    /// back, which is what made an empty pane (and an empty count) the permanent state of
    /// every head that attached late or switched back. Measured 2026-10-05: *"i just
    /// restarted the head and the subagents list is gone"*.
    ///
    /// **The rows are rebuilt from the snapshot's own children, then overlaid with the
    /// daemon's list** — and that order is the whole of the fix for a count that flapped.
    /// The snapshot half ([`letibot_sessionlog::view::Snapshot::subagents`], folded by the
    /// parent's view) is the events' own conclusion: it knows `opening`, `running`, `done`,
    /// `failed`, the role, the task and the answer. The list half knows one bit — whether a
    /// turn is generating in the child *at this instant* — and that bit is `false` for a
    /// child parked on its own background job or between two rounds. Rebuilding from the
    /// list alone therefore read that `false` as *finished*, and the composer's count
    /// dropped a live child on every switch back and picked it up again when some later
    /// list reply happened to catch the child generating: the operator's *"so the counter
    /// is gone"* … *"yep and now it is back. wtf"*, over a subagent that ran throughout.
    ///
    /// The durable half is on the wire already and needs no new frame: **`SessionBrief`
    /// carries `parent_session_id`** — the registry's own words for it are *"A head draws
    /// a subagent tree from this without reaching the store"* — and every `Hello` (which
    /// a `Switch` is answered with) and every `Sessions` frame carries the whole list. So
    /// a row is rebuilt from the same fact the picker's tree is drawn from, and the two
    /// cannot disagree about who is whose child.
    ///
    /// **What the list is still for, now that the snapshot carries the children.** Two
    /// things, and neither of them is redundant: the list's `running` is the only
    /// measurement of NOW on the wire, so it is what moves a row between *generating* and
    /// *between turns* without waiting for the child's next event; and a child of a daemon
    /// generation this view did not see — one only in the store, after a daemon was
    /// replaced — is on the list and nowhere else.
    ///
    /// # What a rebuilt row can and cannot say
    ///
    /// The list carries the child's **title** (the daemon's own one-line form of the
    /// subtask), its **model**, and whether a turn is **generating in it right now** —
    /// which is exactly what the count asks. It does not carry the state word, the role,
    /// or the answer, because those are what the event is for. So a rebuilt row says what
    /// the list says and **claims nothing about a state it was not told**: `state` stays
    /// empty, the pane draws `[?]` and `state unknown`, and the running count does not
    /// count it. A row the head *did* watch keeps every richer field, and a live event
    /// landing later fills the rebuilt row in place — same id, one row.
    ///
    /// # The one word the LIST is authoritative for, in both directions
    ///
    /// `state` is the one field both halves can speak to, and only through one word:
    /// the list's `running` is a measurement of *this instant* (a turn is generating in
    /// that session now), while a `running` a row holds is what an event said when it
    /// was published. So the word `running` comes from the list both ways round — **the
    /// list saying `true` makes the row `running` even if the row last said `done`** (a
    /// child asked for more work has started a second turn), and **the list saying
    /// `false` drops a `running` the row still claims** (a finish this head was not
    /// attached for leaves a count above the composer reading `1 subagent running` for a
    /// child that is not). Every other word is the event's and is kept as it stands:
    /// `done`, `failed` and `opening` are all *not generating*, which is a fact the list
    /// cannot tell apart from each other, and none of them is a claim about now.
    ///
    /// # Order, and one enumeration
    ///
    /// Children come in the daemon's order (the list is the enumeration the picker
    /// already numbers), and a child this head watched spawn whose brief the list does not
    /// carry yet — the list is a snapshot of its own moment, the event is not — is
    /// appended after them rather than dropped.
    pub(crate) fn fold_subagents(&mut self) {
        let known: Vec<SubagentState> = std::mem::take(&mut self.subagents);
        let mut rows: Vec<SubagentState> = Vec::with_capacity(known.len().max(4));
        for b in &self.sessions {
            if b.parent_session_id.as_deref() != Some(self.session_id.as_str()) {
                continue;
            }
            match known.iter().find(|k| k.session_id == b.session_id) {
                // Watched: the event's own row, which knows more than the list does about
                // everything except whether a turn is generating in it right now.
                Some(k) => {
                    let mut k = k.clone();
                    // **The list's measurement of NOW**, kept beside the lifecycle word rather
                    // than written over it. See [`SubagentState::generating`].
                    k.generating = b.status.running;
                    k.state = if b.status.running {
                        // **A positive measurement of life, and the one direction the list may
                        // move a row**: a turn is generating in this child at this instant, so
                        // it is working. This overrides a `done` on purpose — a child asked for
                        // more work is generating whatever it last finished.
                        "running".into()
                    } else if k.state == "running" && k.answer.is_some() {
                        // **`false` may retire a row that has ALREADY completed.** The finish
                        // this row is still running on happened before the list was cut, and
                        // the child holds the answer the daemon published with its `done` — so
                        // the list's `false` is the later word about a child that has ended,
                        // and the pane may stop claiming it is running.
                        String::new()
                    } else {
                        // **And it may not touch any other row.** `false` here is *no turn is
                        // generating in this child this instant* — which is exactly what a
                        // child parked on its own background job, or sitting between two
                        // rounds, looks like — and it is NOT a completion. Reading it as one is
                        // the defect this change exists for: the count above the composer
                        // dropped live children (`4, 2, 3, 1`) while they were working, and the
                        // pane moved them into the `finished` group beside children that had
                        // actually ended.
                        k.state
                    };
                    // **The daemon's own stamp wins over the event's**, when the list carries one:
                    // that is the session's creation time, and an event's `ts` is only when this
                    // head heard about the child.
                    if b.created_ms > 0 {
                        k.spawned_ms = b.created_ms;
                    }
                    rows.push(k);
                }
                None => rows.push(SubagentState {
                    session_id: b.session_id.clone(),
                    // **Only what the daemon actually said.** `running` is the list's own
                    // "a turn is generating in this session at this instant"; anything
                    // else is a state nobody has told this head, and an empty word draws
                    // as unknown rather than as `done`.
                    state: if b.status.running {
                        "running".into()
                    } else {
                        String::new()
                    },
                    generating: b.status.running,
                    prompt: String::new(),
                    // **The child's name, or the id the daemon shows for one it has not
                    // named** — the fallback the picker's own rows make, and the reason
                    // is the pane: a row whose words are all empty is a row the operator
                    // cannot tell from an empty pane, which is the report this change
                    // exists for.
                    task: if b.title.is_empty() {
                        short_id(&b.session_id)
                    } else {
                        b.title.clone()
                    },
                    role: String::new(),
                    model: b.status.model.clone(),
                    answer: None,
                    spawned_ms: b.created_ms,
                }),
            }
        }
        for k in known {
            if !rows.iter().any(|r| r.session_id == k.session_id) {
                rows.push(k);
            }
        }
        // **NEWEST FIRST** — the operator's ask, 2026-10-05: *"fix agents pane - the ordering is
        // off - most recent agents must be on top"*.
        //
        // The rows above are built in the daemon's list order, which is creation order — oldest
        // first — with the children this head watched spawn appended after them, so without this
        // the pane drew a child that had just been started at the BOTTOM of its group: the worst
        // place for the one row the operator opened the pane to see.
        //
        // **`sort_by` and not `sort_unstable_by`**: the sort is STABLE, so rows nobody can date
        // (all the zeroes — a replay, a brief with no stamp) keep the order they arrived in rather
        // than being shuffled into an order that means nothing. A dated row still comes before an
        // undated one whatever the stability, because zero sorts last descending.
        rows.sort_by(|a, b| b.spawned_ms.cmp(&a.spawned_ms));
        self.subagents = rows;
        // **The child this head climbed up out of**, by id, once the rebuild has happened
        // — a stop index taken before it would point at whatever the new list has there.
        //
        // **A finished child is unfolded to land on.** The cursor is an index into the stops
        // ([`App::subagent_stops`]), and a finished child is not one of them while the group is
        // collapsed — so coming back up out of a child that has since ended opens the group it
        // went into, rather than dropping the cursor on the fold and hiding the row the operator
        // just left.
        let up_from = self.up_from.clone();
        if let Some(from) = up_from {
            if let Some(i) = self.subagents.iter().position(|r| r.session_id == from) {
                if self.subagents[i].is_finished() && !self.subagents_finished_open {
                    self.subagents_finished_open = true;
                }
                if let Some(k) = self
                    .subagent_stops()
                    .iter()
                    .position(|s| matches!(s, SubStop::Agent(j) if *j == i))
                {
                    self.subagents_sel = k;
                }
                self.up_from = None;
            }
        }
        self.subagents_sel = self
            .subagents_sel
            .min(self.subagent_stops().len().saturating_sub(1));
    }

    /// **The jobs pane's rows, as ONE enumeration** — running first, then one folded
    /// `finished (N)` row.
    ///
    /// The operator's own ask: *"jobs panel - same as subagents - show list of running, group
    /// finished"*. It is [`App::subagent_stops`]' shape because that pane already learned the two
    /// lessons this one needs: the rows the cursor walks and the rows the keys act on must be the
    /// same list, and a settled row the reader has stopped caring about must not push a running
    /// one off the bottom of the pane.
    pub(crate) fn job_stops(&self) -> Vec<JobStop> {
        let mut out = Vec::with_capacity(self.jobs.len() + 1);
        for (i, j) in self.jobs.iter().enumerate() {
            if j.running {
                out.push(JobStop::Job(i));
            }
        }
        if self.jobs.iter().any(|j| !j.running) {
            out.push(JobStop::Finished);
            if self.jobs_finished_open {
                for (i, j) in self.jobs.iter().enumerate() {
                    if !j.running {
                        out.push(JobStop::Job(i));
                    }
                }
            }
        }
        out
    }

    /// **The pane row the job stop at the cursor was DRAWN on**, read out of
    /// [`App::jobs_stop_rows`] — the record the pane wrote while drawing, never arithmetic over
    /// the table it drew from. The sibling of [`App::subagents_row_of`].
    pub(crate) fn jobs_row_of(&self) -> usize {
        let at = self
            .jobs_sel
            .min(self.jobs_stop_rows.len().saturating_sub(1));
        self.jobs_stop_rows.get(at).copied().unwrap_or(0)
    }

    /// **The pane row the entry at the cursor was DRAWN on**, read out of
    /// [`App::queue_stop_rows`] — the record the pane wrote while drawing, never arithmetic over
    /// the queue. The sibling of [`App::jobs_row_of`], and the clamp is the same: the queue can
    /// move under the cursor (a `MergeEntryAdded` arriving), and an arrow pressed against a
    /// shorter queue must land on a row rather than on an index that no longer exists.
    pub(crate) fn queue_row_of(&self) -> usize {
        let at = self
            .queue_sel
            .min(self.queue_stop_rows.len().saturating_sub(1));
        self.queue_stop_rows.get(at).copied().unwrap_or(0)
    }

    /// **The entry drawn on SCREEN ROW `y`, or nothing** — the queue pane's `todo_stop_at_row`.
    ///
    /// Read from the rows the last draw recorded, so a click and the drawing cannot disagree
    /// about where a row is; guarded on the window, so a click into the blank space under a
    /// short queue moves nothing.
    pub(crate) fn queue_stop_at_row(&self, y: u16) -> Option<usize> {
        let y = usize::from(y).checked_sub(self.queue_pane_top)?;
        if y >= self.pane_room {
            return None;
        }
        let pane_row = y + self.pane_scroll;
        self.queue_stop_rows.iter().position(|r| *r == pane_row)
    }

    /// **The verdict on one entry, as the wire spells it**, or `None` when nobody has asked.
    ///
    /// `None` is *no review row at all* and `Some` with `decision: None` is *asked and not
    /// answered*: the pane draws them differently, because the first is a queue nobody has
    /// looked at and the second is a queue that is being looked at now.
    pub(crate) fn review_of(
        &self,
        entry_id: &str,
    ) -> Option<&letibot_sessionlog::event::MergeReview> {
        self.merge_reviews.iter().find(|r| r.entry_id == entry_id)
    }

    /// Throw the rendered history away; it is rebuilt from `items` and `notes`
    /// on the next frame. One place, because forgetting one of the two cursors
    /// duplicates or loses everything after it.
    ///
    /// For the three callers that really do mean *all of it*: a snapshot replaced
    /// `items` wholesale, a fold changed how many lines every cached block renders
    /// to, and a width change moved every wrap. Everything else means
    /// [`App::invalidate_history_from`].
    pub(crate) fn invalidate_history(&mut self) {
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
    pub(crate) fn invalidate_history_from(&mut self, k: usize) {
        if k == 0 {
            self.hist_lines.clear();
            self.hist_marks.clear();
            // **The anchor's map goes with the lines it describes.** A stale span would
            // place the viewport inside a frame that no longer exists.
            self.spans.clear();
            self.hist_upto = 0;
            self.hist_floor = 0;
            self.hist_first_class = None;
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
        //
        // **Unless the head is in tail mode.** A tail walk does not pass the rows it
        // skipped, so it leaves no marks (`fill_backward`) — and "no mark" here would
        // otherwise mean "nothing to do", which is how a row that *changed* above the
        // window would keep being drawn as it was. With no marks to rewind by, the only
        // correct answer is to throw the history away and render the tail again: the
        // rows above the floor were never rendered, so there is nothing to be stale.
        let Some(mark) = self.hist_marks.get(k).copied() else {
            if self.hist_floor > 0 {
                self.invalidate_history_from(0);
            }
            return;
        };
        self.hist_lines.truncate(mark.lines);
        self.hist_marks.truncate(k);
        // By ROW, not by position: a row that rendered to nothing has no span, so the two
        // lists are not parallel and `truncate` here would drop the wrong ones.
        self.spans.retain(|s| s.row < k);
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
    pub(crate) fn retarget_before(&mut self, k: usize) {
        self.call_targets = targets_before(&self.items, k);
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
    pub(crate) fn round_head(&self, idx: usize) -> usize {
        // A user row is its own head — a conversation of user rows must not
        // rewind to zero on every one. Anything else belongs to the nearest
        // assistant row above it, PAST any user rows in between, for the same
        // reason `round_results` stops only at an assistant row: a result
        // landing after a mid-round message must reach the row that proposed
        // its call.
        if self.items[idx].kind == "user" {
            return idx;
        }
        self.items[..=idx]
            .iter()
            .rposition(|r| r.kind == "assistant")
            .unwrap_or(0)
    }

    /// The first history row this turn's pane is drawing, or `None` if it is
    /// drawing none.
    ///
    /// A turn-state transition changes one input to the walk — `drawn_live`, which
    /// is true only for a row in `TurnPane::appended` — so it can change what those
    /// rows render to and nothing above the first of them.
    pub(crate) fn turn_first_row(&self) -> Option<usize> {
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
    pub(crate) fn invalidate_turn_rows(&mut self) {
        if let Some(k) = self.turn_first_row() {
            let k = self.round_head(k);
            self.invalidate_history_from(k);
        }
    }

    /// File something that happened between rows, at the row it happened at.
    pub(crate) fn note(&mut self, n: Note) {
        // **A warning is announced once, so it is noted once.**
        //
        // The same `Warning` can reach a head twice: `adopt` plants everything the
        // snapshot carries at anchor 0, and the live arm anchors at the CURRENT
        // end of the transcript. Two copies of one announcement, in two places —
        // and the second one sits under the conversation, so new rows arrive
        // beneath it and it reads as pinned to the bottom. The operator, on three
        // of them: *"sometimes new messages come under those three but then those
        // three again pinned to the bottom, sometimes they just stay pinned"*.
        //
        // Identity is `(code, detail, ts)`: `ts` is the log's own clock for the
        // envelope that carried it, so the same announcement has the same one by
        // whichever route it arrives, and two genuinely separate warnings that
        // agree on all three are the same sentence at the same instant — which a
        // reader cannot tell apart either, and should not be shown twice.
        //
        // The FIRST anchor wins. A warning belongs where it happened, and the
        // later arrival is a redelivery rather than a new event.
        //
        // The identity is [`note_key`]'s, and it is one rule for the whole head: the
        // same three facts (code, the log's `ts`, and the detail) that `/notes dismiss`
        // retires a note by and that [`holds`] asks a snapshot against. A note that a
        // *snapshot* already carries is not filed a second time at a live seam either —
        // the snapshot's copy is the older statement of the same fact (R19).
        if holds(&self.notes, &n) {
            return;
        }
        self.file_note(n);
    }

    /// **Put a note in the conversation**, with the bookkeeping and without the identity test.
    ///
    /// The split exists for [`Note::Pane`], which is the one disclosure that cannot be
    /// *redelivered* and so must not be deduplicated: a pane's ending is filed by the head that
    /// took the pane, the pane is taken on the first `TermEnded`, and the second `!term mc`
    /// that exits 7 with the same sentence on its screen is a **second ending** and not the same
    /// one announced twice. [`note_key`] cannot tell those two apart — they are the same line,
    /// the same reason and the same rows — so the rule that protects a warning from a snapshot
    /// would silently swallow every repeat of a pane that dies at once, which is the defect
    /// this variant exists for.
    pub(crate) fn file_note(&mut self, n: Note) {
        let at = self.items.len();
        self.notes.push((Placed::Seam(at), n));
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

    /// **Is this note one the reader has retired?**
    ///
    /// A retired note is not rendered, and nothing else about it changes: it stays
    /// in `notes`, [`App::notes_lines`] lists it with its text, and
    /// [`App::retired_notes`] counts it. See [`App::dismissed`].
    pub(crate) fn is_retired(&self, n: &Note) -> bool {
        self.dismissed.contains(&note_key(n))
    }

    /// How many of the notes this head holds are hidden right now.
    ///
    /// The number `/status` shows. Deliberately *computed from the notes* rather
    /// than kept as a counter: a counter can disagree with the screen, and the one
    /// thing a count of what is hidden may not do is be wrong.
    pub(crate) fn retired_notes(&self) -> usize {
        self.notes
            .iter()
            .filter(|(_, n)| self.is_retired(n))
            .count()
    }

    /// **How many notes this head holds and is not drawing, because they are older than
    /// the conversation it is showing** (R19).
    ///
    /// The number `/status` shows beside the count, computed from the notes for the same
    /// reason [`App::retired_notes`] is: the two are different reasons for a line not
    /// being on the screen — one the reader chose, one the window decided — and a reader
    /// who cannot tell *"I dismissed it"* from *"it happened before I attached"* will
    /// believe the wrong one.
    pub(crate) fn notes_before(&self) -> usize {
        self.notes
            .iter()
            .filter(|(place, _)| matches!(place, Placed::Before))
            .count()
    }

    /// **Retire a note, or all of them.** Returns how many were newly hidden.
    ///
    /// Written through one function so the two callers (`/notes dismiss` and
    /// `/dismiss`) cannot disagree about the three things that have to happen
    /// together: the key list, the rendered history, and the file.
    pub(crate) fn retire(&mut self, keys: Vec<String>) -> usize {
        let mut added = 0;
        for k in keys {
            if self.dismissed.contains(&k) {
                continue;
            }
            self.dismissed.push(k);
            added += 1;
        }
        if added == 0 {
            return 0;
        }
        // Oldest out, `prefs::RETIRED_CAP` deep: the list is this reader's memory,
        // and an unbounded one is a file that grows for the life of the box.
        let over = self
            .dismissed
            .len()
            .saturating_sub(crate::prefs::RETIRED_CAP);
        if over > 0 {
            self.dismissed.drain(..over);
        }
        // A note that was in `hist_lines` has to come out of it, and the walk is
        // incremental: the only honest way to un-draw a line is to rebuild.
        self.invalidate_history();
        self.redraw = true;
        added
    }

    /// **`/notes` — what this head has shown, and how to retire it.**
    ///
    /// R10. Three verbs in one, because they are one subject: nothing listed the
    /// notes, nothing retired one, and a reader who has just retired the wall needs
    /// a way back if they were wrong. `rest` is the text after the verb.
    pub(crate) fn notes_command(&mut self, verb: &str, rest: &str) -> Option<Action> {
        // **The listing is the moment to find out what the file says.** `load_prefs` ran
        // once, at startup, and a head up for hours has a `dismissed` that only ever grew
        // from its own presses — so without this, `/notes` shows a note retired on disk as
        // live on the screen, which is the operator's 2026-09-22 report read one way round.
        self.refresh_retired();
        // `/dismiss` is the same action under the word the operator would type at a
        // red wall; `/notes dismiss` is where it is documented.
        let rest = if verb == "dismiss" && rest.is_empty() {
            "all"
        } else {
            rest
        };
        match rest {
            "" => {
                let lines = self.notes_lines();
                self.slash_out = Some(("/notes".to_string(), lines));
                self.pane_scroll = 0;
                self.redraw = true;
                None
            }
            "restore" | "back" | "undismiss" => {
                let back = self.dismissed.len();
                self.dismissed.clear();
                self.invalidate_history();
                self.redraw = true;
                // **`Replace`, and this is the whole of the third item.** `restore` clears
                // this head's list and saves; under a union that save wrote the FILE's keys
                // straight back, so the dismissal survived on disk while the head believed it
                // had undone it — two facts about one key, disagreeing, which is the defect
                // `merge_retired` was written to end. A union can only ever ADD a key; an
                // assertion that a key is *not* retired is a removal, and only a replacement
                // can say it.
                //
                // **What this does not fix, said rather than implied:** another head that
                // still holds that key in its own `dismissed` will union it back on its next
                // save, because from *that* reader's seat nothing has changed. A restore is
                // this head's statement about the whole set; it is not a push to anybody
                // else. Making it one would need a second channel, and the operator asked for
                // the shape that keeps the two verbs distinct.
                let saved = self.save_prefs(RetiredWrite::Replace);
                self.say(&if back == 0 {
                    "nothing was retired, so nothing came back".to_string()
                } else {
                    format!(
                        "{back} retired note(s) back on the screen — the log was never \
                         the thing they were hidden from{saved}"
                    )
                });
                None
            }
            other => {
                // `dismiss` is the word itself: `/notes dismiss` and `/dismiss all`
                // both land here.
                let arg = other.strip_prefix("dismiss").map(str::trim).unwrap_or("");
                let keys: Vec<String> = match arg {
                    "" | "all" => self.notes.iter().map(|(_, n)| note_key(n)).collect(),
                    n => {
                        let Some(n) = n.parse::<usize>().ok().filter(|k| *k >= 1) else {
                            self.say(&format!(
                                "`{n}` is not a number — `/notes` lists them, and \
                                 `/notes dismiss N` retires the Nth"
                            ));
                            return None;
                        };
                        match self.notes.get(n - 1) {
                            Some((_, note)) => vec![note_key(note)],
                            None => {
                                self.say(&format!(
                                    "there is no note {n} — `/notes` lists the {} this \
                                     head holds",
                                    self.notes.len()
                                ));
                                return None;
                            }
                        }
                    }
                };
                let hidden = self.retire(keys);
                // **Union.** A dismissal asserts a key IS retired, and no other head's save
                // is evidence to the contrary — this is the write `merge_retired` exists for.
                let saved = self.save_prefs(RetiredWrite::Union);
                self.say(&format!(
                    "retired {hidden} note(s) — hidden, still counted on /status, and \
                     `/notes` shows them{saved}"
                ));
                None
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
    ///   ctrl-s sessions · ctrl-p todos · ctrl-g subagents · ctrl-r thinking · …
    /// ```
    ///
    /// **The bar is one constant string.** It used to open with the keys that change —
    /// `enter send` idle, `esc interrupt` while a turn runs — and those are three
    /// different lengths in front of the same tail, so the line moved sideways whenever
    /// a turn started or the first character was typed. See `Editor::hint`.
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
    /// **Hold the view, or let it follow again** (R56) — `ctrl-p` and nothing else.
    ///
    /// # The contract, and it is all of it: while held, the head writes NOTHING
    ///
    /// Not a spinner, not a clock, not a counter that ticks. **One written cell is one lost
    /// selection** — the terminal clears a selection as soon as anything is painted over it, so
    /// there is no gentler way to keep painting and keep the selection. The events keep arriving
    /// and this head keeps folding them; it simply stops drawing, which is why nothing in the
    /// protocol or in the daemon had to change.
    ///
    /// **The reader is the only party who can know a selection exists.** With mouse reporting on,
    /// a Shift-drag is handed to the TERMINAL and never reaches this process — which is exactly
    /// why Shift is the gesture — so *do not repaint while something is selected* is not
    /// implementable as written. The reader knows, so the reader holds the view.
    ///
    /// # How the hold is kept, in three pieces
    ///
    /// * [`App::screen`] composes ONE frame when the hold begins — with the marker on it, which
    ///   is the single write the freeze owes — and returns that same frame byte for byte
    ///   thereafter, so the terminal's own diff produces no bytes at all;
    /// * [`App::take_redraw`] refuses while held, so nothing can force `invalidate` and a full
    ///   repaint behind the hold's back;
    /// * this function, which counts what arrived ONCE, at the release, because a live count while
    ///   held would be an animation and an animation is writes.
    pub(crate) fn toggle_hold(&mut self) -> Option<Action> {
        if self.hold {
            self.hold = false;
            let arrived = self.items.len().saturating_sub(self.hold_rows);
            self.hold_rows = 0;
            self.hold_frame = None;
            self.hold_size = (0, 0);
            self.redraw = true;
            self.say(&format!(
                "the view follows again — {arrived} rows arrived while it was held"
            ));
        } else {
            self.hold = true;
            self.hold_rows = self.items.len();
            self.hold_frame = None;
            self.hold_size = (0, 0);
            self.redraw = true;
        }
        None
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

    /// **Render the tail of a conversation instead of all of it.**
    ///
    /// The frame shows the *end* of a session, and the end is what this renders first.
    /// On a session of thousands of turns and 160 MB, walking from row 0 to draw the
    /// bottom 40 rows lexes every row above them, which is the operator's *"does
    /// nothing, then ... after a while it renders history"*.
    ///
    /// Walks **backward** from the current floor, rendering whole rows, until
    /// `want` lines are covered or the beginning is reached. Each row is rendered by
    /// the same [`item_lines`] the forward walk uses, with the same context — by
    /// *field*, from [`targets_before`] rather than from the forward walk's running
    /// table, which is the only thing that made direction matter.
    ///
    /// Returns how many rows it rendered, for a test to count.
    pub(crate) fn fill_backward(&mut self, want: usize) -> usize {
        self.fill_backward_until(want, None)
    }

    /// The same walk, with a **row** it must reach — R36.
    ///
    /// Two stopping conditions rather than one, because the two callers ask different
    /// questions: a reader moving by lines wants *a screen's worth*, and a viewport holding
    /// a row wants *that row*, however few lines it takes. `stop_row` of 0 is the lines-only
    /// walk.
    pub(crate) fn fill_backward_until(&mut self, want: usize, stop_row: Option<usize>) -> usize {
        if self.hist_floor == 0 {
            return 0;
        }
        let cfg = self.cfg.clone();
        let (think, tool, raw, diff_split) =
            (self.reasoning, self.tools, self.raw_calls, self.diff_split);
        let in_flight: std::collections::HashSet<String> = self
            .turn
            .as_ref()
            .map(|t| t.appended.iter().cloned().collect())
            .unwrap_or_default();
        // **And the turn's rows, for the one question that is about the TURN.** `in_flight` is
        // this round's — what the live pane is still drawing — while `live_here` asks whether a run
        // belongs to the turn at all. Two sets because they are two questions, and asking the second
        // with the first is the defect [`TurnPane::turn_rows`] records.
        let turn_rows: std::collections::HashSet<String> = self
            .turn
            .as_ref()
            .map(|t| t.turn_rows.iter().cloned().collect())
            .unwrap_or_default();
        // (row, class, lines, tight) for each row, newest first as they are built. The row
        // index rides along for R36: the block these are assembled into is PREPENDED to the
        // history, so every span already in `spans` shifts by its length and the new ones
        // have to be recorded here rather than recovered later.
        //
        // `tight` is R37 AMENDED's: a run of hidden rows draws one marker line that must
        // read as the continuation of the prose above it, so the separator's blank line is
        // suppressed for that row. It rides in this tuple rather than being recomputed in
        // the assembly loop because the assembly has only the row index and the class.
        // `(row, class, lines, marker, joinable)`: the marker's TEXT and whether the model's
        // own sentence introduces it. Two facts rather than one, because a marker that stands
        // alone still needs the separator's blank — which is the operator's *"add an empty
        // line between them"* — and a marker that joins must not have it.
        let mut built: Vec<(usize, RowClass, Vec<String>, Option<Marker>, bool)> = Vec::new();
        // Read once, before the loop: the walk needs it per row and recomputing it there
        // would be a scan of `items` for every row drawn.
        let newest_payload = self.newest_payload_row();
        let mut k = self.hist_floor;
        let mut covered = |built: &[(usize, RowClass, Vec<String>, Option<Marker>, bool)]| {
            self.hist_lines.len()
                + built
                    .iter()
                    .map(|(_, _, l, _, _)| l.len() + 1)
                    .sum::<usize>()
        };
        // **The row condition is `Option`al on purpose.** Written as `k > stop_row` with a
        // `0` meaning "no row", the disjunct is true for every `k > 0` and the walk renders
        // the whole session — 400 rows where a screen was asked for, found by the debug
        // print and not by reading it. `None` is the lines-only walk.
        // **What the turn is doing that no row holds yet**, computed once for this walk.
        // The backward walk is entered when the transcript is too big to render forward, and
        // it still has to count the work in flight against the run it belongs to.
        let live = live_work(
            self.turn.as_ref(),
            &self.items,
            &self.cfg,
            !matches!(
                self.turn.as_ref().and_then(|t| t.state.as_ref()),
                Some(TurnState::Running) | None
            ),
        );
        // **The run `ctrl-t` opens**, once, for the same reason the forward walk computes it
        // once: the seam names the chord only where it acts.
        let newest_run = newest_unseen_run(&self.items, self.visibility, &self.bound_prompts, live);
        // **`carry`: the walk does not stop in the middle of a run.**
        //
        // A run's marker is drawn at its FIRST row, and this walk renders the newest rows
        // first — so the start is the last thing it reaches. Stopping before it (on the line
        // budget) left the rows it had already passed with nothing on the screen at all: the
        // reader scrolling a tool-heavy turn would see prose and then a hole, and the counts
        // for the rows they were looking at would be nowhere. The cost of continuing is a
        // pass of a cheap predicate per row — the rows themselves draw no lines and are
        // dropped from `built` — which is nothing next to a marker that is not there.
        let mut carry = false;
        // **`need_speaker`: a marker with nothing drawn beside it yet.**
        //
        // The operator, on a real screen: *"sometimes you do it same line - sometimes dont."*
        // They were right, and this is why. The marker joins the sentence it continues **when
        // the prose row is already in the block**, and that depends on where the line budget
        // happened to stop: a reader one line short of the window fills exactly one line — the
        // marker's — and the narration above it is never built, so the marker stands alone.
        // The same transcript joined or did not depending on how far somebody had scrolled.
        //
        // So the walk does not stop while the newest rendered row is a marker that has no row
        // above it: it renders one more row, which is the prose, and the join in the assembly
        // loop becomes unconditional. One extra row, only when a marker is the top of what has
        // been drawn.
        let mut need_speaker = false;
        loop {
            let enough = covered(&built) >= want
                && stop_row.is_none_or(|r| k <= r)
                && !carry
                && !need_speaker;
            if k == 0 || enough {
                break;
            }
            k -= 1;
            let targets = targets_before(&self.items, k);
            let answered = round_results(&self.items, k);
            // **The marker, or the row** — R37 AMENDED, and the same three states the
            // forward walk keeps. Both walks decide *which row of a run owns the line* the
            // same way — the run's first — so the two agree about a block without either of
            // them having to remember what the other drew.
            let open_run = run_open_at(
                &self.items,
                self.visibility,
                &self.bound_prompts,
                live,
                self.payload_sel.as_deref(),
                k,
            );
            // The same reservation the forward walk makes, from the same rule and the same
            // data — so the two walks wrap the introducing sentence identically and the
            // marker lands in the room either of them left.
            let reserve = reserved_for_run(
                &self.items,
                self.visibility,
                &self.bound_prompts,
                live,
                &cfg,
                k,
                newest_run == Some(k + 1),
            );
            let row_cfg = match reserve {
                Some(room) => RenderConfig {
                    width: cfg.width.saturating_sub(room).max(20),
                    ..cfg.clone()
                },
                None => cfg.clone(),
            };
            let unseen = if open_run {
                None
            } else {
                unseen_run_at(&self.items, self.visibility, &self.bound_prompts, live, k)
            };
            carry = unseen.is_some_and(|(start, _)| start < k);
            let joinable = unseen.is_some_and(|(start, _)| run_continues_prose(&self.items, start));
            // **The marker's text, when this row is the first of the run.** The joining is
            // the assembly loop's, because that is where forward order exists — this walk
            // renders newest first, so the prose this marker continues has not been reached
            // yet when the row is built. See [`hidden_run_marker`].
            let marker = unseen.filter(|(start, _)| *start == k).map(|(start, end)| {
                // **Does this run hold one of the TURN's rows, AND reach the live edge** —
                // the `live_here` question, and it is two clauses because one is not enough.
                //
                // A run made only of an earlier turn's rows is history, and folding the
                // in-flight work into it is the defect `marker_carries_live` already records.
                // But *this turn's rows* is not the discriminator either, and that is what the
                // operator saw: one long turn of forty rounds is forty runs, every one of them
                // holding this turn's rows — so every marker folded the live counts (inflating
                // each) and every marker went yellow. *"old tool calls stayed yellow for some
                // reason."* The work in flight happens AFTER every committed row, so it belongs
                // to the run that REACHES THE TAIL (`end == items.len()`) and to no other; every
                // earlier run of the same turn is settled history and draws plain.
                let live_here = newest_run == Some(start)
                    && (start..end).any(|r| turn_rows.contains(&self.items[r].item_id));
                hidden_run_marker(
                    &self.items,
                    start,
                    end,
                    self.visibility,
                    &cfg,
                    newest_run == Some(start),
                    MarkerFacts::of(live, newest_run),
                    live_here,
                )
            });
            let (class, rows) = match unseen {
                Some((start, _)) if start == k => (
                    RowClass::Activity,
                    vec![
                        marker
                            .as_ref()
                            .expect("a marker was built for this row")
                            .painted(&cfg),
                    ],
                ),
                Some(_) => (RowClass::Other, Vec::new()),
                None => item_lines(
                    &self.items[k],
                    &ItemCtx {
                        cfg: &row_cfg,
                        think,
                        tools: tool,
                        raw,
                        targets: &targets,
                        answered: &answered,
                        subagents: &self.subagents,
                        drawn_live: in_flight.contains(self.items[k].item_id.as_str()),
                        elapsed_ms: self.call_ms.get(&self.items[k].item_id).copied(),
                        edit: self.call_edits.get(&self.items[k].item_id),
                        decision: self.call_decisions.get(&self.items[k].item_id),
                        bound: self
                            .bound_prompts
                            .get(&self.items[k].item_id)
                            .map(String::as_str),
                        // **What this head knows about the echo and nothing about the text.**
                        // A bound row keeps the echo's mark: whether the snapshot that put
                        // this row here carried the words is the head's history, and the same
                        // string is `queued` in one session and `unconfirmed` in another.
                        // **A bound row is being drawn, so its mark is never `queued`** — see
                        // [`App::echo_mark`]. This was `_ => QUEUED`, which is the mark the operator
                        // kept seeing under a reply that was already streaming.
                        echo_mark: self
                            .bound_prompts
                            .get(&self.items[k].item_id)
                            .map(String::as_str)
                            .map(|t| echo_mark(&self.unconfirmed, t, true))
                            .unwrap_or(QUEUED),
                        echo_open: self.echo_open,
                        // See the forward walk: an open run IS the rung lifted for its rows.
                        vis: if open_run {
                            Visibility::lifted()
                        } else {
                            self.visibility
                        },
                        diff_split,
                        payload_view: if open_run {
                            None
                        } else {
                            self.payload_sel
                                .as_deref()
                                .filter(|id| !id.is_empty())
                                .map(|id| (id, self.payload_page))
                        },
                        payload_newest: newest_payload.as_deref(),
                        payload_max: Some(&self.payload_max),
                        window_rows: self.screen_rows.saturating_sub(WINDOW_CHROME),
                    },
                ),
            };
            if !rows.iter().all(|l| l.trim().is_empty()) {
                // A marker that is about to be the top of the block needs the row above it,
                // or it cannot join and will be drawn as a row of its own.
                // Only when the marker will be glued: a marker standing on its own line has
                // no sentence to fetch, and fetching one would put a row on the screen that
                // the budget did not ask for.
                need_speaker = marker.is_some() && joinable;
                built.push((k, class, rows, marker, joinable));
            }
        }
        let rendered = self.hist_floor - k;
        if !built.is_empty() {
            // Assemble in forward order, with the separator the forward walk puts
            // *before* a row whose kind changed.
            let mut block: Vec<String> = Vec::new();
            let mut fresh: Vec<Span> = Vec::new();
            let mut prev: Option<RowClass> = None;
            for (row, class, rows, marker, joinable) in built.iter().rev() {
                // **The marker, glued into the sentence it continues** — R37 AMENDED's final
                // shape, and this is the walk where the joining has to happen HERE rather
                // than at the row: forward order exists only in this loop, and the prose the
                // marker continues is the row just above it. `prev` is that row's class, and
                // `Speech` is this file's own name for prose the reader can see.
                //
                // The width check is the fallback's: a line that cannot hold the counts
                // would put them past the frame's edge, and counts that are off the screen
                // are not a marker. Then it stands alone instead — `marker.is_none()` below
                // leaves it without a blank, so it still hugs rather than starts a row.
                if let Some(m) = marker
                    && *joinable
                    && prev == Some(RowClass::Speech)
                    && let Some(at) = block.iter().rposition(|l| !l.trim().is_empty())
                {
                    let joined = format!("{} {}", block[at].trim_end(), m.painted(&cfg));
                    if visible_width(&joined) <= cfg.width {
                        block[at] = joined;
                        continue;
                    }
                }
                let pack = prev == Some(RowClass::Activity) && *class == RowClass::Activity;
                // **A marker that stands alone keeps the air prose gets** — which is what the
                // operator asked for after their own message. Only a JOINED marker loses it.
                if !block.is_empty() && !pack && (marker.is_none() || !*joinable) {
                    block.push(String::new());
                }
                // **The span, before the lines go in.** `block` is in forward row order
                // here, so `built.iter().rev()` is the order the reader reads them in.
                fresh.push(Span {
                    row: *row,
                    at: block.len(),
                    lines: rows.len(),
                });
                block.extend(rows.iter().cloned());
                prev = Some(*class);
            }
            // And one at the seam: the row this block now precedes is the old head.
            if !self.hist_lines.is_empty() {
                let pack = self.hist_first_class == Some(RowClass::Activity)
                    && built.last().map(|(_, c, _, _, _)| *c) == Some(RowClass::Activity);
                // **And the marker keeps its sentence across the seam as well.** A fill
                // renders older rows and prepends them, so the row at the top of the old
                // buffer sits directly under the oldest row of the new block — and a marker
                // there is the continuation of prose that is also in that block, so the
                // blank goes. `built.first()` is the OLDEST row of the block (the vector is
                // newest-first and walked in reverse above).
                let tight = built
                    .last()
                    .is_some_and(|(_, _, _, m, joinable)| m.is_some() && *joinable);
                if !pack && !tight {
                    block.push(String::new());
                }
            }
            let first = built.last().map(|(_, c, _, _, _)| *c);
            // **Every line offset already recorded moves down by what was prepended** — and
            // that is the whole reason the anchor is a row rather than a line number. A head
            // holding a line index would creep by this amount on every fill; a head holding
            // a row asks this list where the row went.
            let shifted = block.len();
            block.append(&mut self.hist_lines);
            self.hist_lines = block;
            for sp in &mut self.spans {
                sp.at += shifted;
            }
            fresh.extend(self.spans.drain(..));
            self.spans = fresh;
            if let Some(f) = first {
                self.hist_first_class = Some(f);
            }
        }
        self.hist_floor = k;
        // Accounted for, so the forward walk has no work until a new row arrives.
        self.hist_upto = self.hist_upto.max(self.items.len());
        rendered
    }

    /// How many transcript rows this head has rendered. `hist_upto` counts every row
    /// accounted for, and `hist_floor` says how many were deliberately skipped, so the
    /// difference is what was drawn.
    pub(crate) fn rendered_rows(&self) -> usize {
        self.hist_upto.saturating_sub(self.hist_floor)
    }

    /// **The row `ctrl-t` opens, asked of the whole head** — and the answer depends on the
    /// rung, which is the one thing R37 AMENDED changed here.
    ///
    /// Under `conversation` the long rows are not rows any more: they are inside a run that
    /// draws as one marker line, so a chord that opened a result's payload window would be
    /// naming something that is not on the screen. What there is to open is **the run**, and
    /// the one a reader reaching for the key means is the newest — exactly the rule
    /// [`App::newest_payload_row`] already follows one level down.
    ///
    /// One function, so the chord and the marker's seam cannot come to disagree about which
    /// of the two things the key is about to open.
    pub(crate) fn newest_openable(&self) -> Option<String> {
        let live = self.live_work_now();
        if let Some(start) =
            newest_unseen_run(&self.items, self.visibility, &self.bound_prompts, live)
        {
            // **A run with no row yet is addressed by a sentinel**, because there is no id to
            // key it on: the work in flight has no item. The chord still opens something —
            // this is the turn's own live view, which is where that work is drawn — and
            // *"a marker that cannot be opened is the elision this document refuses
            // everywhere else."*
            return Some(match self.items.get(start) {
                Some(it) => it.item_id.clone(),
                None => LIVE_RUN.to_string(),
            });
        }
        self.newest_payload_row()
    }

    /// The turn's in-flight work, as this head currently knows it — one function, so the
    /// walk, the chord and the pane cannot disagree about what is running.
    pub(crate) fn live_work_now(&self) -> LiveWork {
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
                && t.calls
                    .iter()
                    .all(|c| matches!(c.state, CallState::Finished { .. }))
        });
        live_work(self.turn.as_ref(), &self.items, &self.cfg, superseded)
    }

    /// The newest transcript row that has a payload to page: a tool result with more
    /// than a line or two of text.
    ///
    /// "Newest" because that is the one the reader is looking at — rows are appended at
    /// the bottom — and because it is the **only** row a head with no cursor can
    /// address: `ctrl-t` opens a window on this row and the arrows page it. Every other
    /// long row's seam therefore names `/t` rather than the chord; see
    /// [`ItemCtx::payload_newest`]. Returning `None` means no result is long enough to
    /// have a rest, and then the chord opens nothing rather than claiming a window.
    pub(crate) fn newest_payload_row(&self) -> Option<String> {
        self.items
            .iter()
            .rev()
            .find_map(|it| match it.item.as_ref() {
                Some(TranscriptItem::ToolResult { payload, .. }) if payload.lines().count() > 2 => {
                    Some(it.item_id.clone())
                }
                _ => None,
            })
    }

    /// **Scroll back by `by` lines, rendering whatever that needs.**
    ///
    /// The operator: *"i want scroll back work"*. The first version set `scroll` and let the
    /// next frame's `fill_backward` catch up — but the frame clamps `scroll` against the
    /// rows rendered **so far**, so the scroll could never express "further up than I have
    /// drawn", and a press bought only the handful of lines the last frame happened to add.
    /// Measured: twelve rounds of eight PageUps reached message 143 of 400.
    ///
    /// So the fill happens **here**, before the scroll is clamped, and it asks for a screen
    /// beyond where the reader is going rather than for exactly where they are. A key that
    /// scrolls is a key that renders; leaving the rendering to the next frame is what made
    /// it crawl.
    ///
    /// `body_len` is deliberately **not** the clamp here. It is the *last frame's* total, and
    /// on the tail path it is smaller than where the reader is going — so clamping against it
    /// is what made a press buy one frame's worth instead of a screen. The fill raises the
    /// real total, and `body_window` clamps against that after it has.
    pub(crate) fn scroll_up(&mut self, by: usize) {
        if self.hist_floor > 0 {
            // A screen past where the reader is *going*, so the next press has rows to move
            // into and does not have to wait for a frame to catch up. `view_top` is the last
            // frame's own top line, which is the best estimate the key handler has — R36
            // made this a real measurement of the glass rather than a count of lines from a
            // bottom that moves.
            self.fill_backward(self.view_top.saturating_sub(by) + self.screen_rows + TAIL_SLACK);
        }
        self.hold(-(by as isize));
    }

    /// **What the reader is looking at, right now, as the last frame drew it** — R36.
    ///
    /// Either the anchored row's line, or the top of the last frame's window when nothing is
    /// anchored yet (the frame that *begins* a scroll). One function, so the two answers
    /// cannot disagree about where the reader is.
    pub(crate) fn held_line(&self) -> usize {
        self.anchor
            .as_ref()
            .and_then(|h| self.span_for(&h.item_id).map(|s| s.at + h.into))
            .unwrap_or(self.view_top)
    }

    /// **Move the held viewport by `delta` lines**, staying on a ROW.
    ///
    /// The sign is the reader's: negative is up, positive is down. The conversion
    /// line-to-row happens here and nowhere else, and it is the whole of R36: a line number
    /// means something different every time a row above grows, and a row means the same
    /// thing until it is gone.
    ///
    /// **Reaching the bottom returns the reader to following**, because that is what
    /// following means and it is an act they took — the same act as `esc`. Arriving content
    /// never does it, which is the difference this requirement is about.
    pub(crate) fn hold(&mut self, delta: isize) {
        // **The count is kept in step with the hold**, and it is a *derived* value: the
        // frame recomputes it from the anchor every time it draws, because only the frame
        // knows how many lines the body has. What this buys is that the two never disagree
        // between frames — a reader of `scroll` between a key press and a paint (a test, a
        // `/status`, any of the four places that clear it) sees the position the hold
        // implies rather than a stale zero.
        let now = self.held_line();
        let want = if delta < 0 {
            now.saturating_sub(delta.unsigned_abs())
        } else {
            now.saturating_add(delta as usize)
        };
        let bottom = self.body_len.saturating_sub(self.view_room.max(1));
        if want >= bottom && delta > 0 {
            self.anchor = None;
            self.scroll = 0;
            // **No `redraw` here either** — this is the flag's second home on the scroll path.
            // See the note at the foot of this function: a moved viewport is a diff, not an
            // erase.
            return;
        }
        // **The top of the transcript is as far as this goes**, and holding there is not
        // following: a reader at the very top of a long session is reading the beginning,
        // and content arriving below must not drag them down to it.
        let want = want.min(bottom.max(1));
        match self.span_at_line(want) {
            Some(span) => {
                // **`into` may be `lines`` — one past the row's last line — and it must be.**
                // The blank line a separator puts between two rows belongs to no row, and
                // clamping `into` to the row's own height sent a notch that landed on one
                // back up a line: two three-line notches moved the window eight lines
                // rather than six, found by asserting the distance rather than the count.
                // Allowing `lines` makes the mapping exact in both directions — the
                // separator is addressable as *the line just below this row*.
                let into = want.saturating_sub(span.at).min(span.lines);
                self.anchor = Some(Held {
                    item_id: self.items[span.row].item_id.clone(),
                    ordinal: span.row,
                    into,
                });
            }
            // **Above the first rendered row.** Either the reader has gone past what this
            // head drew, or nothing has been drawn yet. Holding the topmost row at offset 0
            // is the closest true thing, and it is what a second press then scrolls from —
            // `fill_backward` has already been asked for more, and the next frame has them.
            None => {
                if let Some(first) = self.spans.first().copied() {
                    let item_id = self.items[first.row].item_id.clone();
                    self.anchor = Some(Held {
                        item_id,
                        ordinal: first.row,
                        into: 0,
                    });
                }
            }
        }
        // **`scroll` is left alone, and it is deliberately not kept in step.**
        //
        // It is derived — the frame recomputes it from the anchor on every paint, because
        // only the frame knows how many lines the body has — and a second derivation here
        // would be two implementations of one formula, which is the shape this file has
        // been bitten by more than once. Anything that wants to know whether the reader is
        // at the bottom asks [`App::following`], which is a question about the anchor
        // rather than about a number.
        //
        // **And this is where `redraw` used to be, for every key that scrolls the transcript** —
        // `WheelUp` and `PageUp` through [`App::scroll_up`], `PageDown` and the parked arrows
        // through this function directly. It should not have been. The flag *throws the glass
        // away*: the head reads it before the next frame and calls `Terminal::invalidate`, which
        // sets `full`, and a full frame is `ESC[2J` followed by every row rewritten with the row
        // diff disabled ([`crate::term`]). A slid window wants the diff: `paint_full` rewrites the
        // rows whose text differs and erases the rows the frame no longer has, which is the whole
        // of what a scroll changed — the rest of the screen is already right, and rewriting it
        // identically is the one thing this head's encoder exists not to do.
        //
        // What the erase cost was a flash per key, and on a touchpad it is a flash per *notch*:
        // the inertial scroll arrives over many `read()`s, every read is its own tick and its own
        // frame, and every frame erased the screen. Measured — see the commit that removed this.
        // The state above is the part a scroll owes, and it is untouched; what to write to the
        // glass is the frame's business and the diff already answers it.
    }

    /// **Draw rows until one of them is rendered** — R36.
    ///
    /// [`App::fill_backward`] walks back until it has covered *enough lines*, which is the
    /// right question when the reader is moving and the wrong one when the head has to find
    /// a specific row: after an invalidation the rows are gone, the line count is stale, and
    /// a fill measured in lines can stop short of the very row the viewport is holding —
    /// leaving the anchor unresolvable and the view adrift. Found by the debug rather than
    /// by reasoning: the frame log showed `total` and `view_top` wandering on every payload
    /// page, which is a fill chasing its own tail.
    pub(crate) fn fill_to_row(&mut self, row: usize) {
        if self.hist_floor > row {
            self.fill_backward_until(
                self.view_top.saturating_sub(1) + self.screen_rows + TAIL_SLACK,
                Some(row),
            );
        }
    }

    /// The span holding body line `line`, or the nearest one at or above it.
    pub(crate) fn span_at_line(&self, line: usize) -> Option<Span> {
        self.spans
            .iter()
            .rev()
            .find(|s| s.at <= line)
            .copied()
            .filter(|s| s.lines > 0)
    }

    /// Where one row's lines are, by id.
    pub(crate) fn span_for(&self, item_id: &str) -> Option<Span> {
        self.spans
            .iter()
            .find(|s| self.items.get(s.row).map(|i| i.item_id.as_str()) == Some(item_id))
            .copied()
    }

    /// **The row a held viewport was on is gone — say so, and land on its neighbour.**
    ///
    /// A `resync`, a snapshot on `hello` and a compaction all replace the rows wholesale, so
    /// the thing the reader was reading may not be carried any more: it was summarised into
    /// the base, or the transcript it lived in was replaced. That is **a fact about their
    /// session** rather than a rendering detail, so it is said rather than silently
    /// absorbed — R29's rule, and the disclosure carries the act that undoes it.
    ///
    /// **The view lands on the nearest surviving row**, not somewhere arbitrary: the ordinal
    /// it held is the only ordering both sides of a replacement agree on, so the row that
    /// took its place is where the reader goes. Jumping to the bottom would lose their place
    /// twice — once to the replacement and once to the head.
    ///
    /// Emitted as a note with its own code rather than a notice: the reader has to be able to
    /// find it again, `/notes` lists it, and `/status` counts it. `Failure`, by R29 part
    /// two's own test — it is not the reader's act, and what is at risk is their orientation:
    /// the thing they were reading is not there.
    pub(crate) fn repair_anchor(&mut self) {
        let Some(held) = self.anchor.clone() else {
            return;
        };
        if self.span_for(&held.item_id).is_some() {
            return;
        }
        // **A row that is simply not rendered yet is NOT a row that is gone.** In tail mode
        // the rows above `hist_floor` were deliberately not walked, so a held row can be
        // present in `items` and absent from `spans` — and saying *it is gone* about a row
        // sitting in the transcript would be a false alarm on every scroll in a long
        // session. The distinction is `items`, which knows every row, versus `spans`, which
        // knows the drawn ones.
        if self.items.iter().any(|it| it.item_id == held.item_id) {
            return;
        }
        let ordinal = held.ordinal.min(self.items.len().saturating_sub(1));
        self.anchor = self.items.get(ordinal).map(|it| Held {
            item_id: it.item_id.clone(),
            ordinal,
            into: 0,
        });
        let said = format!(
            "the row you were reading is no longer carried: this transcript was replaced (a \
             compaction, a resync or a snapshot), and the row was `{}`. The view is holding \
             the nearest row that survived — `esc` follows the stream again.",
            held.item_id
        );
        let already = self.notes.iter().any(|(_, n)| match n {
            Note::Warned(w) => w.code == "anchor_lost" && w.detail == said,
            _ => false,
        });
        if !already {
            self.note(Note::Warned(Warned {
                code: "anchor_lost".into(),
                detail: said,
                ts: 0,
            }));
            // **And said where the reader is certainly looking.**
            //
            // A note is planted at a *seam* in the conversation, and the seam for this one
            // is the end of the replacement — which is precisely where a reader who is
            // holding a row near the top **is not looking**. The durable sentence is the
            // note (`/notes` lists it, `/status` counts it, and it stays); this is the
            // transient line above the composer, which is on screen whatever the viewport is
            // showing. A disclosure the reader cannot see is not a disclosure, and this is
            // one about the viewport itself.
            self.say(&format!(
                "the row you were reading is gone — the transcript was replaced. Holding the                  nearest surviving row; `esc` follows again (row `{}`)",
                held.item_id
            ));
            self.redraw = true;
        }
    }

    /// **How the viewport's state reads on screen** — R36, and R29's rule applied to it.
    ///
    /// `None` when the head is following, which is the ordinary case and owes the reader
    /// nothing: a marker that is always on is furniture. `Some` when it is **holding**, and
    /// it names the act that returns them, because a reader who cannot tell pinned from
    /// following will scroll to find out — which is the affordance failing.
    pub fn scroll_state(&self) -> Option<&'static str> {
        (!self.following()).then_some("holding")
    }

    /// **Whether the row is drawn is the SET's question now**, and the name on the status row
    /// is the SET's name: the profile when the set is one, and `custom …` when a switch has
    /// been moved off one. **Both halves of R29's rule**: the mode is named, and the name is
    /// also the verb (`/verbosity`) that changes it.
    ///
    /// Drawn when the set hides the working, as it was drawn at `Conversation` — and **also
    /// whenever the set is no profile**, because a state with no name is exactly the thing a
    /// reader cannot ask about: *"any set that is no profile reads as `custom …`"*. A profile
    /// that does not hide anything names nothing, because a marker that is always on is the
    /// furniture this head keeps deleting.
    ///
    /// **The other half is the marker** — R37 AMENDED, and the correction is worth keeping in
    /// view here because this comment used to argue the opposite. R37 as filed said R29 was
    /// satisfied "by the mode being NAMED on the screen rather than by a placeholder per
    /// hidden row" — and the second half of that was wrong. The operator never asked for no
    /// marker; they asked not to read the rows. **One marker per RUN is not a placeholder per
    /// row**, and without it the rung does not hide the work — it makes the model's own prose
    /// lie, because the sentence introducing the work ends in a colon pointing at nothing.
    /// See [`hidden_run_lines`], and [`App::newest_openable`] for what opens one.
    pub fn rung_state(&self) -> Option<String> {
        let v = self.visibility;
        (v.hides_the_working() || v.profile().is_none()).then(|| v.as_str())
    }

    /// **Does this rung draw this row as a row** — R37, and the one place the question is
    /// asked about a row rather than about an item.
    ///
    /// A row with no body yet is not hidden by the rung: it is drawn from this head's own
    /// echo of what the operator typed, and that is the conversation. A row whose *item* the
    /// rung does not keep is hidden, which is the same test `item_lines` makes.
    ///
    /// **Hidden no longer means absent** (R37 AMENDED): a hidden row's *run* draws one marker
    /// line, and this predicate is what says which rows are inside one — the anchor repair and
    /// the run finder both ask it, and neither should be asking the question a second way.
    pub fn hidden_by_rung(&self, row: usize) -> bool {
        row_hidden(&self.items, self.visibility, row)
    }

    /// **Move a held viewport off a row this rung hides** — R37's consequence for R36.
    ///
    /// *"Hiding changes row heights and removes rows. If the anchored row is one that this
    /// rung hides, the view anchors to the nearest surviving row and says so rather than
    /// jumping."* Nearest in row order, outward from where the reader was, and the sentence
    /// is the same shape `repair_anchor` writes for a row a replacement took away.
    pub(crate) fn reanchor_off_hidden(&mut self) {
        let Some(held) = self.anchor.clone() else {
            return;
        };
        if !self.hidden_by_rung(held.ordinal) {
            return;
        }
        let n = self.items.len();
        let found = (1..n.max(1))
            .flat_map(|d| {
                let up = held.ordinal.checked_sub(d);
                let down = held.ordinal + d;
                [up, (down < n).then_some(down)].into_iter().flatten()
            })
            .find(|r| !self.hidden_by_rung(*r));
        match found {
            Some(row) => {
                self.anchor = Some(Held {
                    item_id: self.items[row].item_id.clone(),
                    ordinal: row,
                    into: 0,
                });
                self.say(&format!(
                    "the row you were reading is one this rung hides — the view is holding the                      nearest row that still shows. `/verbosity` brings the working back, and                      `esc` follows the stream again"
                ));
            }
            None => {
                // Nothing survives to hold on to: a transcript with no conversation in it at
                // all. Following is the only true answer, and the rung is on screen saying
                // why the screen is empty.
                self.anchor = None;
                self.say("this rung hides every row here, so there is nothing to hold a place in");
            }
        }
        self.redraw = true;
    }

    /// **Whether the viewport is following the stream** — R36's state, and the thing the
    /// screen has to say out loud.
    ///
    /// A reader who cannot tell whether they are pinned or following will scroll to find
    /// out, and that is the affordance failing. See [`App::scroll_state`].
    pub fn following(&self) -> bool {
        self.anchor.is_none()
    }

    /// **The row of the run the live work belongs to** — what a change in the counts invalidates.
    ///
    /// `newest_unseen_run` is the walk's own answer to *which run is this work part of*, and the
    /// invalidation has to agree with it or the count would be rebuilt on one row while the marker
    /// was drawn on another.
    pub(crate) fn newest_run_row(&self, live: &LiveWork) -> Option<usize> {
        newest_unseen_run(&self.items, self.visibility, &self.bound_prompts, *live)
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
    /// Load the head's preferences from disk and apply them. Called once, before
    /// the first frame; the notes are what could not be read, said on the screen.
    ///
    /// **A path already named is kept.** `main` calls this before anything else and
    /// names nothing, so in production this is still `prefs::path()` — the one file the
    /// head writes. The reason it does not *replace* a path is that the retired notes
    /// are read here and nowhere else, so a test that wants to prove a dismissal
    /// survives a restart has to be able to point a head at the file it wrote; forcing
    /// the reader to reach into `$HOME` would have made the round trip untestable and
    /// left the property asserted nowhere.
    /// **Take a git reading, from the LOOP.** See `crate::gitfield` for why this is not done where
    /// the frame is drawn: it spawns a process.
    ///
    /// A workspace that CHANGED is read at once rather than waiting out the interval, because a
    /// session switch carries another path and the cached field would be the old tree's branch.
    ///
    /// **THE FORMAT IS APPLIED HERE, ON THE READER, AND NOWHERE ELSE** — leticl's `%git-refresh`
    /// rule: a paint draws cached pieces, it never parses and never formats, so a mistyped
    /// template stays a bad line rather than becoming a header that fails to draw. The STATE is
    /// kept beside the pieces for the one thing that changes the format without a new reading:
    /// `/config`'s cycle re-renders from the cache (`apply_git_format`).
    pub fn refresh_git(&mut self) {
        let ws = self.wiring.workspace.clone();
        if ws.is_empty() {
            self.git = None;
            self.git_state = None;
            return;
        }
        let (last_of, at) = &self.git_read;
        let stale = self.now_ms.saturating_sub(*at) >= crate::gitfield::GIT_REFRESH_MS;
        if last_of == &ws && !stale {
            return;
        }
        self.git_state = crate::gitfield::read_state(&ws);
        self.apply_git_format();
        self.git_read = (ws, self.now_ms);
    }

    /// **Render the cached state through the format in force** — the whole of what a format
    /// change needs to do, and the reason the state is cached: no process, no interval, the
    /// same facts re-said in the new template's words.
    pub(crate) fn apply_git_format(&mut self) {
        let format = self
            .git_format
            .clone()
            .unwrap_or_else(|| crate::gitfield::GIT_FORMAT_DEFAULT.to_string());
        self.git = self
            .git_state
            .as_ref()
            .map(|s| crate::gitfield::git_pieces(s, &format));
    }

    pub fn load_prefs(&mut self) {
        self.prefs_path = self.prefs_path.clone().or_else(crate::prefs::path);
        let Some(path) = self.prefs_path.clone() else {
            return;
        };
        let (p, notes) = crate::prefs::load(&path);
        self.diff_split = p.diff == crate::prefs::DiffPref::Split;
        // **The set comes back too.** It is the one setting the card could change and the
        // file did not keep, so a reader who chose `conversation` got `normal` on every
        // restart. [`Visibility::parse`] is the same reader the card and the verb use — and
        // the head's own start is the BASE a `custom …` name is read against, which is what
        // makes the name a state rather than a caption.
        self.visibility = match Visibility::parse(&p.verbosity) {
            Ok(Change::Set(v)) => v,
            // A bare switch name in the file is a change against the head's start, for the
            // same reason it is one on the verb: *this switch, from where I am*.
            Ok(Change::Switch(s, l)) => Visibility::starting().with(s, l),
            // **An unreadable word is REPORTED and not obeyed.** §13.2b's rule for a setting:
            // silently starting at the default would make a typo and a deliberate `normal`
            // the same screen. The sentence is the parse's own, so a name this refuses is a
            // name the verb refuses with the same words.
            Err(said) => {
                self.say(&format!("head.toml: {said}"));
                Visibility::starting()
            }
        };
        self.reasoning = if p.thinking == "open" {
            Fold::Open
        } else {
            Fold::Folded
        };
        self.tools = if p.tools == "open" {
            Fold::Open
        } else {
            Fold::Folded
        };
        // **The fold keys move the switches they are, but only where the switch is SHOWING.**
        // The three keys are older than this vocabulary and a file written before it says
        // `verbosity = "conversation"` beside `thinking = "open"`, which is a fold on a row
        // nobody draws — not a fact about the screen, and not a reason to raise the profile
        // that hid it. `raw-calls` is the exception and is read straight: its level IS this
        // boolean, and the key that already meant it keeps meaning it.
        for (show, fold) in [(Show::Thinking, self.reasoning), (Show::Tools, self.tools)] {
            if self.visibility.shows(show) {
                self.visibility = self.visibility.with(
                    show,
                    if fold.is_open() {
                        Level::Open
                    } else {
                        Level::Folded
                    },
                );
            }
        }
        self.raw_calls = p.raw_calls;
        self.visibility = self.visibility.with(
            Show::RawCalls,
            if p.raw_calls {
                Level::Open
            } else {
                Level::Hidden
            },
        );
        // **The starter-todo switch and its record come with the rest** — the seed runs at the
        // attach, which is long after this, and a switch or record that lived only in this run
        // would re-seed every project on every restart, which is the duplicate defect the record
        // exists to prevent.
        self.todo_template = p.todo_template;
        self.todo_seed = p.todo_seed;
        self.git_format = p.git_format;
        // A format that was loaded before the first reading still has nothing to render
        // over — but a head that RESUMES into a session renders the header at once, so the
        // format is applied here too and not only on the reader's first tick.
        self.apply_git_format();
        // **R10's retired notes come from the file, not from the process.** A head
        // restart is one of the two things that used to replant the wall, so a
        // dismissal that lived only in this run would be a dismissal that lasts
        // until the next restart — which is the defect, not the fix.
        self.dismissed = p.retired.clone();
        for n in notes {
            self.say(&n);
        }
    }

    /// The head's current choices, as the file holds them.
    pub(crate) fn prefs(&self) -> crate::prefs::HeadPrefs {
        crate::prefs::HeadPrefs {
            diff: if self.diff_split {
                crate::prefs::DiffPref::Split
            } else {
                crate::prefs::DiffPref::Unified
            },
            thinking: fold_word(self.reasoning).into(),
            tools: fold_word(self.tools).into(),
            raw_calls: self.raw_calls,
            verbosity: self.visibility.as_str(),
            retired: self.dismissed.clone(),
            todo_template: self.todo_template.clone(),
            todo_seed: self.todo_seed.clone(),
            git_format: self.git_format.clone(),
        }
    }

    /// Write the head's choices. Returns the suffix for the confirmation line:
    /// where it went, or why it did not — a change that silently failed to
    /// persist would be found at the next start, as a surprise.
    ///
    /// **`retired` is written according to `retired`, and it cannot be one rule.** A
    /// dismissal and a restore are *opposite* assertions about one key, so the write that
    /// expresses one cannot express the other — see [`RetiredWrite`].
    pub(crate) fn save_prefs(&self, retired: RetiredWrite) -> String {
        match &self.prefs_path {
            None => " (not saved: no $HOME or $XDG_CONFIG_HOME)".into(),
            Some(path) => {
                let mut p = self.prefs();
                p.retired = match retired {
                    RetiredWrite::Union => crate::prefs::merge_retired(path, &self.dismissed),
                    RetiredWrite::Replace => self.dismissed.clone(),
                };
                match crate::prefs::save(path, &p) {
                    Ok(()) => String::new(),
                    Err(e) => format!(" (not saved: {e})"),
                }
            }
        }
    }

    /// **Take back what the file says is retired, not only what this head wrote.**
    ///
    /// `load_prefs` runs once, at startup. A head that has been up for hours has a
    /// `dismissed` that only ever grew from its own presses — so a dismissal another head
    /// recorded since is invisible, and `/notes` shows a note that is retired on disk as
    /// live on the screen. The file is the durable record (`load_prefs`' own comment says
    /// so), and reading it is what makes that true rather than a claim.
    ///
    /// Called where the operator is looking or acting — the listing and the chord — rather
    /// than on a timer: nothing here needs to notice a change nobody has asked about, and a
    /// stat-and-read on a keypress is free while a poll loop is a poll loop.
    pub(crate) fn refresh_retired(&mut self) {
        let Some(path) = self.prefs_path.clone() else {
            return;
        };
        let (p, _) = crate::prefs::load(&path);
        if p.retired != self.dismissed {
            self.dismissed = p.retired;
            self.invalidate_history();
            self.redraw = true;
        }
    }

    /// The pane's rows, in order. Rebuilt on every draw and every key, so the
    /// cursor and the screen can never disagree about what row N is.
    pub(crate) fn config_rows(&self) -> Vec<ConfigRow> {
        let mut rows = Vec::new();
        let head = |key: &str, value: String, edit: ConfigEdit| ConfigRow {
            choices: Vec::new(),
            section: "head — this window",
            key: key.into(),
            value,
            source: self
                .prefs_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "not persisted".into()),
            edit,
        };
        rows.push(head(
            "diff view",
            if self.diff_split {
                "split".into()
            } else {
                "unified".into()
            },
            ConfigEdit::Head(HeadSetting::Diff),
        ));
        rows.push(head(
            "verbosity",
            self.visibility.as_str(),
            ConfigEdit::Head(HeadSetting::Verbosity),
        ));
        rows.push(head(
            "thinking",
            fold_word(self.reasoning).into(),
            ConfigEdit::Head(HeadSetting::Thinking),
        ));
        rows.push(head(
            "tool output",
            fold_word(self.tools).into(),
            ConfigEdit::Head(HeadSetting::Tools),
        ));
        rows.push(head(
            "raw tool calls",
            if self.raw_calls {
                "shown".into()
            } else {
                "hidden".into()
            },
            ConfigEdit::Head(HeadSetting::RawCalls),
        ));
        // **THE GIT FIELD'S FORMAT IS HERE BECAUSE THE OPERATOR LOOKED FOR IT HERE** —
        // leticl's `72a4314` finding, and the row names the template IN FORCE (so the pane
        // and the row can never disagree), with the first stop of the cycle the built-in
        // default.
        rows.push(head(
            "git format",
            self.git_format
                .clone()
                .unwrap_or_else(|| format!("default ({})", crate::gitfield::GIT_FORMAT_DEFAULT)),
            ConfigEdit::Head(HeadSetting::GitFormat),
        ));
        for r in &self.settings {
            rows.push(ConfigRow {
                section: "session — the daemon",
                key: r.key.clone(),
                value: r.value.clone(),
                source: r.source.clone(),
                choices: r.choices.clone(),
                edit: if r.editable.is_empty() {
                    ConfigEdit::No("takes a restart of the daemon")
                } else {
                    ConfigEdit::Session(r.key.clone(), r.editable.clone())
                },
            });
        }
        for f in [
            "modes.tsv",
            "permission.json",
            "providers.toml",
            // **The system prompt's per-model overrides** — a file the operator edits by hand
            // like the three above it, and the one they would never find: nothing else in the
            // tree names it, and an override that is silently not read looks exactly like an
            // override that is. See `harnessd::config::Prompts`.
            "prompts.toml",
            "sensitive.json",
        ] {
            let path = self
                .prefs_path
                .as_ref()
                .and_then(|p| p.parent())
                .map(|d| d.join(f));
            let (value, source) = match &path {
                Some(p) if p.is_file() => {
                    let n = std::fs::read_to_string(p)
                        .map(|t| t.lines().count())
                        .unwrap_or(0);
                    (format!("{n} lines"), p.display().to_string())
                }
                Some(p) => ("not present".into(), p.display().to_string()),
                None => ("no config directory".into(), String::new()),
            };
            rows.push(ConfigRow {
                section: "files — edit with an editor",
                key: f.into(),
                value,
                source,
                choices: Vec::new(),
                edit: ConfigEdit::No("a file the guard protects: a person edits it, not a pane"),
            });
        }
        rows
    }

    /// Enter on the selected row.
    pub(crate) fn config_change(&mut self) -> Option<Action> {
        let rows = self.config_rows();
        let Some(row) = rows.get(self.config_sel.min(rows.len().saturating_sub(1))) else {
            return None;
        };
        self.redraw = true;
        match &row.edit {
            ConfigEdit::Head(which) => {
                match which {
                    HeadSetting::Diff => {
                        self.diff_split = !self.diff_split;
                        self.invalidate_history();
                    }
                    HeadSetting::Verbosity => {
                        // **It opens the card rather than cycling.** Four values are chosen
                        // from a card and not walked through (R38), so the pane points at the
                        // verb instead of doing the thing the card exists to stop.
                        self.say("`/verbosity` with nothing after it opens the card");
                        return None;
                    }
                    HeadSetting::Thinking => {
                        self.reasoning = self.reasoning.flip();
                        self.invalidate_history();
                    }
                    HeadSetting::Tools => {
                        self.tools = self.tools.flip();
                        self.invalidate_history();
                    }
                    HeadSetting::RawCalls => {
                        self.raw_calls = !self.raw_calls;
                        self.invalidate_history();
                    }
                    HeadSetting::GitFormat => {
                        // **Three stops, and the default is `None` rather than a fourth
                        // string** — the default has to stay one value in one place
                        // (`GIT_FORMAT_DEFAULT`), so the cycle passes through `None` and
                        // not through a copy of it.
                        self.git_format = match self.git_format.as_deref() {
                            None => Some("%b %!%+".into()),
                            Some("%b %!%+") => Some("%b".into()),
                            _ => None,
                        };
                        // **Re-rendered from the cache at once** — the reading is the
                        // reader's business and the template is this head's, so a change
                        // must not wait out the interval to be seen (`apply_git_format`).
                        self.apply_git_format();
                    }
                }
                // **Union.** A fold or a raw-call toggle is not a statement about the retired
                // set at all, so it must not discard another head's dismissals on its way past.
                let saved = self.save_prefs(RetiredWrite::Union);
                let rows = self.config_rows();
                if let Some(r) = rows.get(self.config_sel) {
                    let line = format!("{} → {}{saved}", r.key, r.value);
                    self.say(&line);
                }
                None
            }
            ConfigEdit::Session(key, how) => {
                // The verbs that already exist, so the pane is a way to see and
                // not a second way to set.
                match key.as_str() {
                    // **The names come from the daemon, on the row.** This was a
                    // `const NAMES` here, and a second copy of a list is a copy
                    // that drifts: it offered `supervised`, which is not a mode,
                    // and not `automode-edits`, which is — so the pane could not
                    // reach the point the daemon was already standing at. The
                    // operator, 2026-09-17: *"I started leticode and there is no
                    // automode-edits"*. A row with no choices is a daemon older
                    // than protocol 18, and then the pane says so rather than
                    // cycling a list it made up.
                    "mode" => {
                        if row.choices.is_empty() {
                            self.say("this daemon does not send the mode list; use `/mode NAME`");
                            return None;
                        }
                        // **The same reading the card makes**, or the cycle starts from the wrong
                        // place: this took the value's first word, so at `writes allowed` it found
                        // no row (`writes` is not a mode) and wrapped to the FIRST one — the pane's
                        // mode row cycled to `always-ask` from a point in the middle of the list.
                        // Found by fixing the card and watching this test fail beside it; the two
                        // are one defect and they were three readers apart.
                        let cur = match named_choice(&row.value, &row.choices) {
                            Some(c) => c.to_string(),
                            None => row.value.clone(),
                        };
                        let at = row.choices.iter().position(|n| *n == cur).unwrap_or(0);
                        let next = row.choices[(at + 1) % row.choices.len()].clone();
                        self.say(&format!("mode → {next} (asking the daemon)"));
                        self.mode_action(next)
                    }
                    "supervise" => {
                        let on = row.value.starts_with("on");
                        let line = format!("supervise {}", if on { "off" } else { "on" });
                        self.say(&format!("{line} (asking the daemon)"));
                        Some(Action::Slash { line })
                    }
                    _ => {
                        let line = format!("change it with {how}");
                        self.say(&line);
                        None
                    }
                }
            }
            ConfigEdit::No(why) => {
                let line = format!("{}: {why}", row.key);
                self.say(&line);
                None
            }
        }
    }

    /// **The visible slice of a pane**, and the two numbers the scroll keys need.
    ///
    /// Clamped here rather than at the keypress: the key handler does not know
    /// how tall the terminal is or how many rows the pane has, and a scroll
    /// clamped against a stale height scrolls past the end and shows a blank
    /// screen the operator has to page back from.
    pub(crate) fn pane_window(&mut self, rows: Vec<String>, room: usize) -> Vec<String> {
        self.pane_len = rows.len();
        self.pane_room = room;
        // The last screenful is the furthest anything scrolls: past that is
        // blank rows, which is not a place to be.
        let max = rows.len().saturating_sub(room);
        self.pane_scroll = self.pane_scroll.min(max);
        rows.into_iter().skip(self.pane_scroll).take(room).collect()
    }

    /// Keep the cursor on screen after an arrow moved it.
    ///
    /// `row` is the cursor's index among the pane's rows. Called by the panes
    /// that have a cursor, after they move it: an arrow that walks the selection
    /// out of the window otherwise looks like a key that does nothing.
    pub(crate) fn scroll_into_view(&mut self, row: usize) {
        if self.pane_room == 0 {
            return;
        }
        if row < self.pane_scroll {
            self.pane_scroll = row;
        } else if row >= self.pane_scroll + self.pane_room {
            self.pane_scroll = row + 1 - self.pane_room;
        }
    }

    /// Re-read `TODO.md` when it has changed since the last read, and not
    /// otherwise. Called on open and before every draw of the pane.
    ///
    /// A failed `stat` — the file was deleted, or was never there — reads again,
    /// so the pane's own "no TODO.md" line is the answer and stays current if one
    /// appears. That costs a failed `open` per draw in the case where there is
    /// nothing to show, which is the case nobody is watching.
    pub(crate) fn refresh_repo_todos(&mut self) {
        // **Resolved, like the read it guards** — the mtime watch and the parse must look at the
        // same file, or a nested layout re-reads on every draw (the watch misses, so `now` is
        // None, so the cache never holds).
        let path = crate::gitfield::project_dir(&self.wiring.workspace).join("TODO.md");
        let now = std::fs::metadata(&path)
            .ok()
            .and_then(|m| Some((m.modified().ok()?, m.len())));
        if self.repo_todos.is_some() && now.is_some() && now == self.repo_todos_at {
            return;
        }
        self.repo_todos_at = now;
        self.repo_todos = Some(repo_todos_map(&self.wiring.workspace));
    }

    /// The job-output view's paging. `forward` asks for the page after the one on
    /// screen — or re-reads the last page when the end is already here, because a
    /// running job appends and that is how you see what it has written since.
    /// `!forward` walks back the way forward came, and does nothing at the front of
    /// the log, where there is no page before the first byte.
    ///
    /// The `back` stack lives on the head because the **page size is the daemon's**:
    /// the head remembers the offsets it was given rather than recomputing a window
    /// it does not size — the same reason `next` arrives on the event.
    pub(crate) fn job_out_page(&mut self, forward: bool) -> Option<Action> {
        let v = self.job_out.as_mut()?;
        let offset = if forward {
            let at = v.from;
            if v.next.is_some() {
                v.back.push(at);
            }
            v.next.unwrap_or(at)
        } else {
            v.back.pop()?
        };
        v.loading = true;
        Some(Action::ReadJobOutput {
            job: v.job.clone(),
            offset,
        })
    }
}

/// **The six disclosure counters, as one comparable value** — R51 item 17.
///
/// A struct rather than six arguments, because the alarm and its acknowledgement have to agree
/// about WHICH numbers count, and a list spelled out at two call sites is a list that grows at one.
/// The doc that used to sit on `alarmed`'s six-term sum already says what each one is; this is the
/// same six, named once so `exceeds` can be the single definition of *has anything moved*.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Counters {
    pub(crate) dropped: u64,
    pub(crate) scrubbed: u64,
    pub(crate) resyncs: u64,
    pub(crate) unreadable: u64,
    pub(crate) gaps: u64,
    pub(crate) orphan_bodies: u64,
    pub(crate) slow_first_byte: u64,
}

impl Counters {
    /// **Has any counter moved past what was acknowledged** — the alarm's whole question.
    ///
    /// Per counter, so acknowledging is *I have seen `2 resyncs`* and not *stop telling me about
    /// resyncs*: the third one exceeds the second and the mark returns. A counter that somehow went
    /// BACKWARDS (a resync that cleared the state) is not news, and an alarm that fired on a
    /// decrease would be a mark nobody could ever clear.
    pub(crate) fn exceeds(self, seen: Counters) -> bool {
        self.dropped > seen.dropped
            || self.scrubbed > seen.scrubbed
            || self.resyncs > seen.resyncs
            || self.unreadable > seen.unreadable
            || self.gaps > seen.gaps
            || self.orphan_bodies > seen.orphan_bodies
            || self.slow_first_byte > seen.slow_first_byte
    }
}

impl App {
    /// True when a §13.2b disclosure counter has moved **past what this reader has been shown**.
    ///
    /// R51 item 17: the triangle is a POINTER at `/status`, and reading that screen acknowledges
    /// it. Without the second half the mark is permanent — the counters are cumulative and start
    /// at zero with the process, so a head that took two resyncs carried `⚠` for the rest of its
    /// life while saying nothing new, and the operator asked the only question available:
    /// *"how to hide that resync counter arrow?"*
    ///
    /// **Up to the value that was READ, and not a switch.** A resync *after* the one that was
    /// acknowledged is a new fact about this head, so the mark comes back — which is what makes
    /// acknowledging safe rather than a way to turn the alarm off and forget it.
    pub(crate) fn alarmed(&self) -> bool {
        self.counters().exceeds(self.acked)
    }

    /// **This head's six disclosure counters, as one value** — the shape the alarm and its
    /// acknowledgement both compare, so *"has anything moved"* has one definition.
    pub(crate) fn counters(&self) -> Counters {
        Counters {
            dropped: self.dropped,
            scrubbed: self.scrubbed,
            resyncs: self.resyncs,
            unreadable: self.unreadable,
            gaps: self.gaps,
            orphan_bodies: self.orphan_bodies,
            slow_first_byte: self.slow_first_byte,
        }
    }

    /// **The reader has read the numbers; stop pointing at them.**
    ///
    /// Called when `/status` opens, and only then: the screen is where the counters are read, so
    /// the act of reading it is the acknowledgement. Nothing is reset — the screen keeps showing
    /// the raw values, `/status` still lists them, and a counter that moves again starts the
    /// conversation over.
    pub(crate) fn acknowledge_counters(&mut self) {
        self.acked = self.counters();
    }

    /// **Move the counter that belongs to an edge-bound code**, or say this head has none.
    ///
    /// One place, so the arm that handles a `Warning` and the test that checks every
    /// `ALARM_ONLY` row is registered both read the same table. `false` is the case worth
    /// having: the tree says a code belongs on the triangle and this head has nowhere to put
    /// it, which the arm above then *says* rather than swallowing — a note that reaches
    /// neither the record nor a counter is a note nobody has.
    pub(crate) fn count_edge_note(&mut self, code: &str) -> bool {
        match code {
            "model_slow_first_byte" => {
                self.slow_first_byte += 1;
                true
            }
            _ => false,
        }
    }

    /// Drop the transient notice, once the operator has had a frame to see it, and stop
    /// whatever clock it started.
    pub fn clear_notice(&mut self) {
        self.notice = None;
        self.notice_until = None;
    }
}

pub(crate) fn open_call<'a>(calls: &'a mut [CallRow], call_id: &str) -> Option<&'a mut CallRow> {
    calls
        .iter_mut()
        .rev()
        .find(|c| c.call_id == call_id && !matches!(c.state, CallState::Finished { .. }))
}

/// The whole view, spilled: the pane caps like a terminal, the file does not
/// cap. One name per subagent, overwritten on each read, so the path is stable
/// enough to open twice.
/// Where a head keeps files of its own: `$XDG_RUNTIME_DIR/letibot`, where the
/// socket already lives — per-user, mode 0700, tmpfs. Not `/tmp`: a subagent's
/// tool output is whatever the model read, and a world-readable file at a name
/// anyone can predict is both a disclosure and the classic symlink target. The
/// fallback is the shape the sudo shims use when there is no runtime dir.
pub(crate) fn head_runtime_dir() -> std::path::PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(d) => std::path::PathBuf::from(d).join("letibot"),
        None => std::env::temp_dir().join(format!("letibot-{}", unsafe { libc::getuid() })),
    }
}

pub(crate) fn spill_sub_out(session_id: &str, lines: &[String]) -> Option<String> {
    spill_sub_out_under(&head_runtime_dir(), session_id, lines)
}

pub(crate) fn spill_sub_out_under(
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
pub(crate) fn round_results(
    items: &[SnapshotItem],
    at: usize,
) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for it in items.iter().skip(at + 1) {
        match it.item.as_ref() {
            Some(TranscriptItem::ToolResult { call_id, .. }) => {
                out.insert(call_id.clone());
            }
            // Only the next assistant row ends a round. A USER row does not: a
            // message sent while the calls run is appended between the calls
            // and their results — measured in the store 2026-09-17 as
            // `assistant, user, user, tool_result ×10` — and a user cannot
            // produce a tool result, so whatever results follow still answer
            // the calls above. Breaking here left every call of such a round
            // `→ no result` after the results had landed and the model had
            // moved on.
            Some(TranscriptItem::Assistant { .. }) => break,
            // An announcement with no body yet, or a reasoning row between the
            // calls and their results. Neither ends the round.
            _ if it.kind == "assistant" => break,
            _ => {}
        }
    }
    out
}

/// How much transcript a head will walk from the beginning before it renders the tail
/// instead.
///
/// A judgement, not a measurement: the walk is ~40 ms for 6000 rows, so 2 MB of
/// transcript is well under a frame's budget and the incremental marks it buys are
/// worth having. Above it the operator's own sessions live — 160 MB, thousands of turns
/// — and there the only affordable thing is the end. See `App::fill_backward`.
pub(crate) const SELF_WALK_LIMIT: usize = 2 * 1024 * 1024;

/// How many rows above the rendered window to keep, so a frame can be drawn while the
/// reader is a little way up — the window, plus one screen.
///
/// Not a scrollback budget: scrolling further back-fills more (see
/// `App::fill_backward`). This is only what is kept ready for the frames that need no
/// new work.
pub(crate) const TAIL_SLACK: usize = 40;

/// Roughly how many bytes of text the transcript carries.
///
/// **A sum of `TranscriptItem::bytes`, which is the one definition** — the daemon bounds
/// its view by the same function, and two copies of "what counts as size" would drift
/// into two answers to one question.
///
/// Cheap on purpose: it runs every frame, so it is a length test over strings already in
/// memory. A row with no body yet counts as zero, which errs toward walking the
/// conversation — the safe direction, since the other one only changes how the frame is
/// produced and this one still produces it correctly.
pub(crate) fn transcript_bytes(items: &[SnapshotItem]) -> usize {
    items
        .iter()
        .map(|it| it.item.as_ref().map(|i| i.bytes()).unwrap_or(0))
        .sum()
}

/// The tool-call targets in force for row `k`: the calls of the nearest assistant row
/// above it that has a body.
///
/// The forward walk builds this as it goes (replacing, not merging, at every assistant
/// row); this derives it, which is what lets the **backward** walk render a row without
/// having rendered everything above it first. Same rule, one implementation.
pub(crate) fn targets_before(
    items: &[SnapshotItem],
    k: usize,
) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    for r in items[..k.min(items.len())].iter().rev() {
        if let Some(TranscriptItem::Assistant { tool_calls, .. }) = r.item.as_ref() {
            for c in tool_calls {
                out.insert(
                    c.id.clone(),
                    letibot_sessionlog::display_target(&c.arguments),
                );
            }
            return out;
        }
    }
    out
}

/// **What the reader is holding their viewport on** — R36.
///
/// The row's **id and not its index**, because the index is exactly what a snapshot
/// replacement moves: a `resync`, a `hello` and a compaction all replace `items` wholesale,
/// and the row the reader was on can survive that with a different index or not survive it
/// at all. The id is the only name for it that both sides of a replacement agree on.
///
/// `ordinal` is the index at the moment of capture, and it is the fallback: when the id is
/// gone, the nearest surviving row in row order is the one that took its place, and the
/// head anchors there and **says so** rather than jumping somewhere arbitrary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Held {
    pub(crate) item_id: String,
    pub(crate) ordinal: usize,
    /// Lines into the row's own rendering. Bounded to the row's height when it is used, so
    /// a row that shrank under the anchor does not push the view past its own end.
    pub(crate) into: usize,
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

/// Where the history walk stood before one row. See [`App::hist_marks`].
///
/// Three fields because the walk carries three cursors, and the fourth —
/// `call_targets` — is *derivable* from the rows above rather than stored:
/// it is the calls of the nearest assistant row with a body, which
/// [`App::retarget_before`] finds by scanning back. Storing a map per row would
/// be the cache growing with the session, which is the thing being fixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HistMark {
    /// `hist_lines.len()` before the row was drawn.
    pub(crate) lines: usize,
    /// `note_upto` before the row was drawn.
    pub(crate) note_upto: usize,
    /// `hist_class` before the row was drawn — the separator's whole input.
    pub(crate) class: Option<RowClass>,
}

/// **Where one rendered row's lines are** — R36's anchor map.
///
/// `at` is a line index into `hist_lines`, and `row` is the row's index in `items`, so a
/// span is addressable from either end: *which row is at line 400* and *where did row 91
/// go* are the two questions the anchor asks, and one list answers both.
///
/// A row that rendered to **nothing** has no span. It has no lines to be looking at, so
/// there is nothing to hold, and inventing a zero-height span would make *the row at this
/// line* ambiguous between it and its neighbour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Span {
    pub(crate) row: usize,
    pub(crate) at: usize,
    pub(crate) lines: usize,
}

/// **A verb and its argument, where the verb has to be the whole word.**
///
/// `cmd.strip_prefix("mode")` matches every command that BEGINS with those four
/// letters, so `/models` arrived as `/mode` with the argument `ls` and the head
/// answered `mode ls requested` — the operator: *"/models doesnt work — printed
/// mode ls requested lol"*. It never reached the daemon at all, because the
/// `mode` arm returns before the fallthrough that forwards unknown verbs.
///
/// `Some("")` for the bare verb, `Some(arg)` when a space follows, and `None`
/// when the word merely starts the same way. Four call sites had the bug and one
/// of them was reported; the other three are `cells`, `new` and `rename`, which
/// would have taken `/newton` as "make a session called ton".
pub(crate) fn verb_arg<'a>(cmd: &'a str, verb: &str) -> Option<&'a str> {
    let rest = cmd.strip_prefix(verb)?;
    if rest.is_empty() {
        return Some("");
    }
    // A digit or letter here means a longer word, not an argument.
    rest.starts_with(char::is_whitespace).then(|| rest.trim())
}

/// Visible width of a rendered screen line. Re-exported so a test can assert the
/// screen fits.
pub fn line_width(s: &str) -> usize {
    visible_width(s)
}

/// **A todo being filed from the card** — its fields, and which one owns the composer.
///
/// The FOCUS was a `bool` while there were two fields, and a third is exactly what a bool cannot
/// hold: *typing the detail?* answers nothing about a `when` field, so every reader of that flag
/// would have grown a second one and the two could disagree. One enum, and Tab cycles it.
///
/// **The composer holds the focused field and the draft holds the rest**, which is what keeps the
/// row being typed from being a keystroke behind — leticl's `%todo-draft-focus`, and the reason
/// [`TodoDraft::take`] exists: every key that LEAVES a field commits the composer into it first, so
/// nothing typed is ever lost to a Tab.
pub(crate) struct TodoDraft {
    pub(crate) title: String,
    pub(crate) detail: String,
    /// **The handle this row waits on, or empty for a row that waits on nothing.** A bare handle:
    /// the *condition* is what the row holds, and the card does not collect a kind because there is
    /// one kind — see `TodoCondition`.
    pub(crate) when: String,
    pub(crate) focus: TodoField,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TodoField {
    Title,
    Detail,
    When,
}

impl TodoDraft {
    pub(crate) fn new() -> TodoDraft {
        TodoDraft {
            title: String::new(),
            detail: String::new(),
            when: String::new(),
            focus: TodoField::Title,
        }
    }

    /// **What one field holds**, with the composer's live text standing in for the focused one.
    pub(crate) fn shown(&self, live: &str, which: TodoField) -> String {
        if self.focus == which {
            return live.to_string();
        }
        match which {
            TodoField::Title => self.title.clone(),
            TodoField::Detail => self.detail.clone(),
            TodoField::When => self.when.clone(),
        }
    }

    /// **Commit the composer into the field it belongs to** — every key that leaves a field does
    /// this first, so a Tab cannot lose what was just typed.
    pub(crate) fn take(&mut self, live: &str) {
        let into = match self.focus {
            TodoField::Title => &mut self.title,
            TodoField::Detail => &mut self.detail,
            TodoField::When => &mut self.when,
        };
        *into = live.to_string();
    }

    /// The field Tab goes to next, wrapping — a cycle, so there is no field a reader cannot reach.
    pub(crate) fn next(&self) -> TodoField {
        match self.focus {
            TodoField::Title => TodoField::Detail,
            TodoField::Detail => TodoField::When,
            TodoField::When => TodoField::Title,
        }
    }
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

/// **Where a note sits, or whether it sits in the conversation at all.**
///
/// `head-parity-2026-09-21.md` **R19**, the operator's ruling of 2026-09-22. A warning is
/// *how a head shows a fact once*; a head that has just attached has shown nothing, so a
/// snapshot's warnings were being replayed as though they had just happened — at position
/// 0, above a conversation they did not precede. The operator restarted a head and was met
/// by twelve red lines: *"i dont want to see that on restart."*
///
/// The distinction is not age — a snapshot's warnings are not old, they are **prior**: the
/// head was not there. So a note filed live is anchored at a seam of this conversation and
/// a note that arrived with a snapshot has no seam to be drawn at, and the two are told
/// apart by this type rather than by a sentinel position that would be a lie the walk
/// would have to undo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Placed {
    /// At this many rows: the seam the walk puts it back into.
    Seam(usize),
    /// **Before this window.** Reachable — `/notes` lists it, `/status` counts it, and a
    /// reader who wants it is one verb away — and not drawn, because it is not news.
    Before,
}

/// Something that happened between two transcript rows.
#[derive(Debug, Clone)]
pub(crate) enum Note {
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
    /// **A pane's ending, as a row that is still there a minute later.**
    ///
    /// The defect this exists for, in the operator's words: *"i typed `!term mc`, it
    /// flashed and was gone"*. An ending used to be said as a **notice** — [`App::say`],
    /// which lives for `NOTICE_MS` of wall time — and a program that dies at once is over
    /// before the eye reaches the rectangle: the conversation came back, one sentence
    /// appeared and faded, and what the program had printed went with the pane. A person
    /// who looked a minute later saw nothing at all, and a person who came back to the
    /// session saw less than that.
    ///
    /// So an ending is a disclosure like every other one this head has, and this head's
    /// disclosures are notes: a row in the conversation, at the seam where it happened,
    /// retirable with `ctrl-n`, listed by `/notes`.
    Pane {
        /// The line that was run, verb included — *what ended* is half the fact.
        line: String,
        /// **The last rows the program left on the screen**, in order and with the blank
        /// ones dropped. See [`TermPane::last_rows`] for why the screen and not the bytes,
        /// and why the tail.
        said: Vec<String>,
        /// The daemon's own sentence: the exit status, or the operator's act. Never
        /// guessed here — see [`ServerFrame::TermEnded`].
        reason: String,
        /// **This head asked for the end and the operator confirmed it** — this head's own
        /// record that it sent [`Action::TermClose`], and not a reading of the reason's wording.
        ///
        /// **`closed` and not `left`.** Leaving is now a detach — `ctrl-\` sends nothing at all
        /// — so a row that said *left* about an ending would name the act that does not end
        /// anything. The three endings this can be true of are the deliberate one (this flag),
        /// the program's own exit (false), and a refusal to start (false, and it is a `×` for
        /// the same reason: it is the answer to what the operator just typed).
        closed: bool,
    },
}

/// **What a note is called when a reader wants to retire it.**
///
/// One identity per disclosure, and it has to survive the two things that used to
/// replant the wall — a resync and a restart. So it is built from the note's own
/// facts and from nothing about where it is on the screen:
///
/// * a warning is `(code, ts)` with the detail hashed. `ts` is the log's clock for
///   the envelope that carried it, which is what [`App::note`] already uses to tell
///   one announcement from a redelivery of the same one — the same identity, for
///   the same reason.
/// * a settled decision is its `req_id`, which is the id the daemon recorded the
///   decision under and the one `/gate` takes.
///
/// **The detail is hashed**, and that is not decoration: a warning's detail can be
/// paragraphs long, and the key is written into `head.toml` as one comma-separated
/// value. `("code|ts|hash", …)` is a line a person can still read and edit. The
/// hash is FNV-1a, which is not a security boundary here — it distinguishes an
/// incident from its neighbours, and two notes that collide on code, second and
/// hash are the same sentence at the same instant.
pub(crate) fn note_key(n: &Note) -> String {
    match n {
        Note::Warned(w) => format!("w|{}|{}|{:016x}", w.code, w.ts, fnv1a(&w.detail)),
        Note::NotRun(w) => format!("n|{}|{}|{:016x}", w.code, w.ts, fnv1a(&w.detail)),
        Note::Decided(d) => format!("d|{}", d.req_id),
        Note::Pane {
            line, said, reason, ..
        } => format!("t|{line}|{reason}|{:016x}", fnv1a(&said.join("\n"))),
    }
}

/// **A workspace's key in the seeded-projects record** — its FNV-1a hash, in `note_key`'s
/// shape: the record lives in `head.toml` as a comma list, so a key that contains a comma or
/// whitespace (as a path can) would corrupt the list, and hashing is the same answer
/// `note_key` gives for the same reason.
pub(crate) fn todo_seed_key(ws: &str) -> String {
    format!("{:016x}", fnv1a(ws))
}

/// **Is this announcement already one this head holds?**
///
/// One identity for a disclosure, and it is [`note_key`]'s rather than a second rule: the
/// same announcement has to mean the same thing to the walk that draws it, the listing
/// that numbers it and the file that remembers it was retired. Asked in two places —
/// [`App::note`], so a redelivery is not filed twice, and [`App::load`], so a note the
/// snapshot carries is not planted a second time beside the head's own copy of it (R19).
pub(crate) fn holds(notes: &[(Placed, Note)], n: &Note) -> bool {
    let k = note_key(n);
    notes.iter().any(|(_, o)| note_key(o) == k)
}

/// FNV-1a, 64-bit: the offset basis and prime, and nothing else.
///
/// Hand-rolled rather than taken from `std`'s hasher, which is **not** stable
/// across releases — and a key that changes when the head is rebuilt would resurrect
/// every note the operator had retired, which is the exact defect this exists for.
pub(crate) fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// What a Tab on a `!` line's last word found in the filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PathCompletion {
    /// One match: the whole line with the word completed — `/` after a directory, a space
    /// after a file, so the next word can be typed at once.
    Line(String),
    /// Several: the line with the word extended to what they share (maybe unchanged), and
    /// the names to show.
    Choices { line: String, names: Vec<String> },
}

/// **Complete the last word of a `!` line as a path**, relative to `workspace` — where an
/// operator's `!` command runs — with `~/` as the home directory.
///
/// The word in command position (the first after `!`) is a program, and is completed only
/// when it is spelled as a path (`./build/x`, `/usr/bin/x`, `~/bin/x`); every later word is
/// an argument and is completed as a file. A word with quotes or `$` in it is left alone —
/// what it names is the shell's to work out. `None` when nothing matches, so a Tab falls
/// through to the line completions.
pub(crate) fn complete_path_word(text: &str, workspace: &str) -> Option<PathCompletion> {
    let cmd = text.strip_prefix('!')?;
    let start = text.rfind(char::is_whitespace).map(|i| i + 1).unwrap_or(1);
    let word = &text[start..];
    if word.contains(['\'', '"', '$', '`']) {
        return None;
    }
    let first = cmd.trim_start().find(char::is_whitespace).is_none();
    let pathish = word.starts_with(['.', '/', '~']) || word.contains('/');
    if first && !pathish {
        return None;
    }
    // Split at the last `/`: what is listed, and the prefix the names must start with.
    let (dir_part, base) = match word.rfind('/') {
        Some(i) => (&word[..=i], &word[i + 1..]),
        None => ("", word),
    };
    let home = std::env::var("HOME").unwrap_or_default();
    let dir = if let Some(rest) = dir_part.strip_prefix("~/") {
        std::path::Path::new(&home).join(rest)
    } else if dir_part == "~" {
        std::path::PathBuf::from(&home)
    } else if dir_part.starts_with('/') {
        std::path::PathBuf::from(dir_part)
    } else {
        std::path::Path::new(workspace).join(dir_part)
    };
    let mut names: Vec<(String, bool)> = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_string();
            // Hidden names only when asked for, as a shell does.
            if !name.starts_with(base) || (name.starts_with('.') && !base.starts_with('.')) {
                return None;
            }
            let is_dir = e.path().is_dir();
            Some((name, is_dir))
        })
        .collect();
    if names.is_empty() {
        return None;
    }
    names.sort();
    let escape = |n: &str| n.replace(' ', "\\ ");
    if let [(name, is_dir)] = names.as_slice() {
        let tail = if *is_dir { "/" } else { " " };
        return Some(PathCompletion::Line(format!(
            "{}{dir_part}{}{tail}",
            &text[..start],
            escape(name)
        )));
    }
    // The longest prefix every match shares, in whole characters.
    let mut common: String = names[0].0.clone();
    for (n, _) in &names[1..] {
        let keep = common
            .char_indices()
            .zip(n.chars())
            .take_while(|((_, a), b)| a == b)
            .last()
            .map(|((i, c), _)| i + c.len_utf8())
            .unwrap_or(0);
        common.truncate(keep);
    }
    let line = format!("{}{dir_part}{}", &text[..start], escape(&common));
    let shown = names
        .into_iter()
        .map(|(n, d)| if d { format!("{n}/") } else { n })
        .collect();
    Some(PathCompletion::Choices { line, names: shown })
}

mod attention;
mod events;
mod keys;
mod session;
mod visibility;
pub use attention::*;
pub use keys::*;
pub use session::*;
pub use visibility::*;

#[cfg(test)]
mod tests;
