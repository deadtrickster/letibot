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

use letibot_sessionlog::client::Unreadable;
use letibot_sessionlog::event::{DeltaTarget, SessionEvent, Timings, Usage};
use letibot_sessionlog::protocol::ServerFrame;
use letibot_sessionlog::registry::{SessionBrief, SessionWiring, short_id};
use letibot_sessionlog::view::{
    CallState, OpenDecision, SettledDecision, Snapshot, SnapshotItem, TurnState, Warned,
};
use letibot_transcript::{TranscriptItem, UserPart};

use letibot_ui::card;
use letibot_ui::editor::{Editor, Reaction};
use letibot_ui::style::Role;

use crate::markdown::IncrementalMarkdown;
use crate::render::{BlockCache, RenderConfig, dur_human, sgr, trim_to, visible_width, wrap};
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

/// See [`App::take_notification`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Attention {
    pub(crate) busy: bool,
    pub(crate) asks: Vec<String>,
    pub(crate) secret: Option<String>,
    pub(crate) answered: Vec<String>,
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

/// What one subagent's read key opens — `p` on a row, and `/peek ID` typed: its tool
/// output, read out of the subagent's own scrollback by a `Peek`, shown without moving
/// the head out of the session it is in. The pane behaves like a terminal — the tail shows
/// by default, arrows walk back toward the beginning — and the whole view is spilled to a
/// file, because a cap on the pane must not be a cap on the record.
/// **A subprocess's bytes must not drive the operator's terminal.**
///
/// The two functions now live in `letibot_ui::text`, because the *UI crate* — which
/// draws every card — had no sanitiser at all, and a per-head helper is exactly how
/// that happened: the falsification test below found a tool-progress note reaching a
/// card's tail raw. Re-exported here so the ~twenty call sites in this file keep the
/// short names they use, with one definition behind them.
use letibot_ui::text::{without_control, without_control_lines};

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

/// **The daemon connection, as far as this head can tell.**
///
/// A head's socket to its daemon goes away for ordinary reasons — the daemon is
/// restarted, the box is shut down for a moment, a `--stop` lands — and the head's job
/// is to keep drawing the session it already has, say that the link is down, and keep
/// trying to get it back. It is **not** to exit: exiting takes the operator's view of a
/// conversation that is still on disk and could still be served.
///
/// Two states, and the second is the whole point: `Reconnecting` is a *state the screen
/// shows* rather than a moment between frames. It carries when the link went down, so
/// the head can say how long it has been trying rather than "reconnecting…" for ever,
/// and it carries the last thing known about why.
///
/// It is deliberately **not** the same thing as a `Bye`. A `Bye` is the daemon saying it
/// is finished with this connection — a refusal, a version skew, a shutdown — and the
/// head leaves with the reason on the screen. That is leticl's rule, and it was learned
/// there the expensive way: it dropped only its `connected` flag on a `Bye` and
/// re-attached two seconds later, for ever, so a refusal the daemon meant as the end of
/// the conversation became a two-second loop under a head that never attached and never
/// exited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    Attached,
    /// Down, and the caller is retrying. `attempts` counts the tries made since it went
    /// down; `since_ms` is this head's own clock, so `now - since_ms` is how long the
    /// operator has been without a daemon.
    Reconnecting {
        since_ms: u64,
        attempts: u32,
        /// The last thing known about why: the io error, the pump going away, or the
        /// refusal from the last attempt to connect.
        why: String,
        /// No attempt before this, on the head's own clock. The backoff lives here
        /// rather than in the caller so a test can drive it with `clock`.
        next_try_ms: u64,
    },
}

impl Link {
    pub fn is_down(&self) -> bool {
        matches!(self, Link::Reconnecting { .. })
    }
}

/// **A head that has asked the daemon to stop, and is waiting to find out.** (R30)
///
/// The operator chose *exit, and stop the daemon too*, and on 2026-09-23 they got the exit
/// without the stop: the head was gone and `harnessd` was still there at `PPID 1`, idle,
/// its socket bound. Nothing ever asked it, or nothing checked. The old code was two
/// discarded results and a return:
///
/// ```text
/// let _ = self.client.stop(app.seq, &who);
/// let _ = self.client.detach();
/// ```
///
/// — whether the frame reached the socket was a race against the head's own shutdown, and
/// **a request is not an outcome**. This is the state that makes the difference: while it
/// is unresolved the head does not leave, and the three facts it observes are kept apart,
/// because each one is a different answer for the operator:
///
/// * `sent` — the write returned `Ok`. The frame is in the kernel's buffer for this socket,
///   which is the most a writer can ever know.
/// * `acked` — the daemon answered `Accepted { note: "stopping" }`. **This is the one that
///   says the request was read**, and the daemon sends it before it closes anything.
/// * `closed` — the daemon's socket file is gone. Its `shutdown` unlinks that file after
///   joining the accept loop, which is what the wrapper's *"the record is removed only
///   after the process is gone"* is the same shape of.
/// * `gone` — **the daemon's process has exited and been collected.** Observed by reaping it
///   when it is this head's own child (`waitpid(WNOHANG)`) and by its `/proc` entry otherwise,
///   **a zombie counting as gone** — because a child that has exited stays in `/proc` until its
///   parent waits for it, and this head is that parent. The strongest observation available,
///   and the only one that is *the daemon has actually gone*.
///
///   **It was `fs::metadata("/proc/{pid}")`, and that is the defect the operator reported
///   twice**: *"they always tell me daemon not stopped after waiting for 5 sec, then `letibot
///   --stop` tells nothing runs."* Both true — the daemon had stopped, and the test said
///   otherwise, because a zombie keeps its directory. The wrapper's test is right where the
///   wrapper runs and wrong here, and the difference is the relationship. See
///   [`crate::driver::Parentage`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stopping {
    /// The identity that asked, as the daemon will have announced it.
    pub who: String,
    /// The head's own clock when the frame went out.
    pub since_ms: u64,
    /// When the head stops waiting and says what it saw.
    pub deadline_ms: u64,
    pub sent: bool,
    pub acked: bool,
    pub closed: bool,
    pub gone: bool,
    /// The daemon's pid, from `SO_PEERCRED` on this very connection — the process at the
    /// other end of the socket, not a number read out of a file that may be stale. `None`
    /// when the kernel would not say, which is a fact the farewell states rather than
    /// fills in.
    pub pid: Option<i32>,
    /// **Whether the model was still working when the head gave up waiting.** A daemon mid-turn
    /// legitimately finishes its round first, so this is the difference between *a slow stop* and
    /// *a daemon that did not go* — and it is the head's to know because it is the head that was
    /// watching the turn.
    ///
    /// **`turn_busy` and not the state name**: the round a daemon finishes before it stops can be
    /// one whose tool call is executing, and a head that asked only whether a round was generating
    /// would call that stop *a refused one* when it was merely slow.
    pub turn_busy: bool,
}

impl Stopping {
    /// **Has the question been answered?** Either the daemon's process is gone — the
    /// operator's choice, carried out — or the deadline has passed and the head can say
    /// what it observed.
    pub fn resolved(&self, now_ms: u64) -> bool {
        self.gone || now_ms >= self.deadline_ms
    }

    /// The line the head draws while it waits, so the screen is never a freeze.
    pub fn waiting_line(&self, now_ms: u64) -> String {
        let out = now_ms.saturating_sub(self.since_ms);
        let left = self.deadline_ms.saturating_sub(now_ms);
        let seen = if self.acked {
            "the daemon answered and is shutting down"
        } else if self.sent {
            "the request went out and the daemon has not answered yet"
        } else {
            "the request could not be sent — the socket is already gone"
        };
        format!(
            "stopping the daemon: {seen} — {} waiting, {} before this head gives up \
             and tells you what it saw. A turn already generating finishes its round.",
            dur_human(out),
            dur_human(left),
        )
    }
}

/// **How long a head waits for a daemon it has asked to stop.**
///
/// **Five seconds, and it is not this head's number** — it is the figure
/// `~/bin/letibot` settled on for the same question, at the site of the same incident:
///
/// ```bash
/// for _ in 1 2 3 4 5 6 7 8 9 10; do [ -d "/proc/$p" ] || break; sleep 0.5; done
/// if [ -d "/proc/$p" ]; then
///   echo "NOT stopped: $line (pid $p) is ignoring SIGTERM after 5s." >&2
/// ```
///
/// Its comment carries the measurement: *"'stopped' is said AFTER the process is gone, not
/// after the signal is sent … Measured 2026-09-16: a daemon wedged on a llama-server that
/// had gone away swallowed SIGTERM, this printed 'stopped', deleted the record, and left an
/// orphan holding the store and the GPU that `--daemons` could no longer see."* Two halves
/// of one program, one figure, and the head never read it.
pub const STOP_DEADLINE_MS: u64 = 5_000;

/// How long between attempts to get back, in milliseconds.
///
/// **Flat, and the same number leticl uses.** A daemon that is coming back is back in
/// well under a second, and one that is gone costs one connect to a socket path — one
/// syscall — every two seconds. Growing the interval would be an optimisation of
/// nothing, and it would make the thing the operator watches move less often than the
/// thing they are waiting for.
pub const RECONNECT_BACKOFF_MS: u64 = 2_000;

/// When a head with no daemon stops saying `reconnecting` and starts saying how long,
/// and what the operator can do about it.
///
/// Under it a drop is usually over before the sentence is read; over it the wait is not
/// a moment, and a head that has been saying the same word at the operator for a minute
/// has told them nothing they could not see for themselves.
pub const LINK_IMPATIENT_MS: u64 = 15_000;

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

/// **A pid as a sentence fragment, or the honest absence** — R30's rule, in one place because
/// two writers say it: the stop's farewell and the replaced-daemon note.
///
/// `None` is *the kernel would not name the peer*, which is a different statement from a pid of
/// zero; a head that printed a number it did not have would send the operator to `ps` for a
/// process that is not there.
pub(crate) fn pid_word(pid: Option<i32>) -> String {
    match pid {
        Some(p) => format!("pid {p}"),
        None => "a pid the kernel did not name".to_string(),
    }
}

/// **Which daemon a head is drawing the picture of** — the two facts that answer *is this the
/// one I attached to*.
///
/// A pair rather than the pid alone because a pid is reused by the kernel: a daemon restarted
/// and handed the same number is a different daemon with the same identity, and the build's
/// protocol version is the other half of the answer — a replaced daemon is usually a rebuilt
/// one, and a rebuild that moved the wire is the case a head most needs to be told about.
///
/// **What it deliberately is not: a daemon instance id.** The strongest available fact would be
/// a boot-time stamp the daemon mints once and sends on every `Hello`, which is a protocol
/// field and a `PROTOCOL_VERSION` bump; `SO_PEERCRED` is already on the wire (R30) and already
/// re-read on every connection, so this is the honest reading of what the head has. The residue
/// — a replaced daemon that happens to get the same pid and speaks the same protocol — is
/// named here rather than papered over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DaemonSeat {
    /// The process at the other end of this socket, from `SO_PEERCRED`. `None` when the kernel
    /// would not say, which compares equal to another `None` — the one case this cannot tell
    /// apart, and the reason the protocol version is beside it.
    pub(crate) pid: Option<i32>,
    /// The version the daemon claims, from the `Hello` that seated this connection.
    pub(crate) protocol: u32,
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

/// **One row of the session picker** — see [`App::session_rows`], which is the only thing that
/// builds one.
///
/// A pair of facts rather than a bare `usize`, because the list is NESTED: which session it is, and
/// how deep it sits. Deriving the depth at the drawing site instead is exactly how the drawing and
/// the keys become two enumerations again — the defect `todos_stops` exists to record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SessionRow {
    /// Index into `App::sessions`.
    pub(crate) idx: usize,
    /// 0 for a conversation; 1 for a sub-session under it; deeper for a tree.
    pub(crate) depth: usize,
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

    /// **Which process this head is attached to.** Called by the caller that opened the
    /// socket, once per connection — including a reconnect, where the answer can be a
    /// different process than the one before.
    ///
    /// **The head is the one holder of this fact.** `/status` reads it, the stop's farewell
    /// names it, and the driver asks the head for it rather than keeping its own copy — so
    /// there is one answer to *which process am I sending the operator to `ps` for*.
    pub fn set_daemon_pid(&mut self, pid: Option<i32>) {
        self.daemon_pid = pid;
    }

    /// The daemon's pid, or `None` when the kernel would not name the peer.
    pub fn daemon_pid(&self) -> Option<i32> {
        self.daemon_pid
    }

    /// **The stop this head is waiting on**, or `None`.
    ///
    /// `Some` from the moment the frame goes out until the head exits. Read by the
    /// renderer for its line, by the driver for the four facts it observes, and at the
    /// end by `main` for the farewell.
    pub fn stopping(&self) -> Option<&Stopping> {
        self.stopping.as_ref()
    }

    /// The same, to write the observations into. The driver is the only writer: it holds
    /// the socket, the clock and the pid, and none of those are the app's.
    pub fn stopping_mut(&mut self) -> Option<&mut Stopping> {
        self.stopping.as_mut()
    }

    /// **The head has asked, and is now waiting.** Called by the driver once, when the
    /// frame has been written (or failed to be), and never before — a head that showed
    /// this line while it had not sent anything would be lying about what it did.
    pub fn stop_began(&mut self, who: &str, sent: bool, pid: Option<i32>, now_ms: u64) {
        self.stopping = Some(Stopping {
            who: who.to_string(),
            since_ms: now_ms,
            deadline_ms: now_ms.saturating_add(STOP_DEADLINE_MS),
            sent,
            acked: false,
            closed: false,
            gone: false,
            pid,
            turn_busy: self.turn_busy(),
        });
        self.redraw = true;
    }

    /// **Whether a plain detach should be sent on the way out.** Not while a stop is in
    /// flight: the daemon's own `Detach` handling would take this head off the session it
    /// is shutting down, and the notice it publishes (`daemon_stopping`) names every head
    /// it reached. The connection is going anyway.
    pub fn wants_detach(&self) -> bool {
        self.stopping.is_none()
    }

    /// **The farewell this head owes, after the screen is gone.** R30's third part: an
    /// operator who chose *stop* and got a running daemon learns it here, once, on stderr,
    /// rather than from `ps` a day later — which is exactly how this one was found.
    ///
    /// `None` when there is nothing to say: no stop was asked for, or the daemon's process
    /// is gone and the question is answered. **A `Bye` is not one of those cases** — see
    /// below. The sentences below are four because the operator's next move differs: it did
    /// not go and a turn was running (legitimate, and it will finish); it did not go and
    /// nothing was running or it could not even be asked (the wedge, and here is the verb);
    /// or the daemon ended the connection itself, which is a different fact with its own
    /// sentence in [`App::farewell`].
    pub fn stop_farewell(&self) -> Option<String> {
        let s = self.stopping.as_ref()?;
        let secs = s.since_ms.max(self.now_ms).saturating_sub(s.since_ms) / 1000;
        // **A `Bye` is not the daemon going, on this path.** The operator was reading this
        // pair on nearly every orderly stop, four lines apart and in this order:
        //
        // ```text
        // letibot: the daemon was asked to stop and had not gone 0s later.
        //   the request was acknowledged and did not stop; the daemon is still there (pid
        //   2291248).
        //   `letibot --stop --force` finishes it — …
        // letibot: the daemon ended this head — daemon shutting down
        // ```
        //
        // The second line is the daemon saying goodbye. The first said it did not go — and
        // recommended `--force`, which *aborts in-flight turns over the protocol*, against a
        // daemon that had just left politely. The fix for that was to treat the `Bye` as the
        // answer — and **that over-corrected into the operator's second report**: *"it
        // reports the server exited within a second — while `harnessd` is in fact hung and
        // has to be killed with `--force`."*
        //
        // **The frame is published before the process ends, and by a thread that is not the
        // one being waited for.** `registry.close()` runs on the connection thread the
        // instant the `Stop` is taken; the **worker** that is running the operator's command
        // is a different thread, is inside that command, and has not ended. Measured on a
        // live daemon, 2026-10-06: `Bye` at 519 µs, the process still in `/proc`, and it
        // stayed there for the rest of the run. So the `Bye` is evidence that the request
        // was **read**, which is a real and useful fact — and it is not evidence about the
        // process, which is the only thing the operator's question is about.
        //
        // **`gone` is the observation that is about the process**, and it is not overloaded
        // to mean anything else: it is read from `waitpid` for this head's own child and
        // from `/proc` — zombie-aware — for anybody else's. It goes on meaning exactly what
        // its docstring says, and it is now the only thing that silences this sentence.
        //
        // The line below used to read `if s.gone || self.bye.is_some() { return None; }`,
        // so the arrival of the goodbye **silenced the farewell** — the head left saying
        // nothing, which a person reads as *it stopped*.
        //
        // The wait is no longer cut short (see [`App::should_quit`]), so this composes its
        // sentence when the question is actually settled: the process is gone, or the
        // deadline passed with it still there. A `Bye` then makes the sentence **stronger**
        // rather than quieter — the daemon did hear the request and did begin shutting
        // down, and its process is still in `/proc` — which is the fact a person needs in
        // order to know that `--force` is the right next move and not a workaround for a
        // lie.
        if s.gone {
            return None;
        }
        let pid = pid_word(s.pid);
        let ask = if self.bye.is_some() {
            "was acknowledged, and the daemon began shutting down"
        } else if s.acked {
            "was acknowledged and did not stop"
        } else if s.sent {
            "was sent and never acknowledged"
        } else {
            "could NOT be sent"
        };
        let because = if s.turn_busy {
            // **And a round can be inside a command.** The daemon has one worker and a
            // round calls its tools on that worker, so a stop that arrives mid-`! sudo apt
            // install mc` waits for that command — whose own deadline is what ends it, and
            // which is two minutes at the default. That is the difference between a stop
            // that is merely slow and one that has to be forced, and the operator is the
            // only one who can tell which they are looking at.
            "A turn was running, and the daemon finishes its round before it stops — and a \
             round can be inside a command, whose own deadline is what ends it. This is a \
             slow stop rather than a refused one."
        } else {
            "No turn was running, so there was nothing for it to finish."
        };
        Some(format!(
            "the daemon was asked to stop and had not gone {secs}s later.\n  \
             the request {ask}; the daemon is still there ({pid}).\n  {because}\n  \
             `letibot --stop --force` finishes it — it aborts in-flight turns over the \
             protocol, then signals, and says `NOT stopped` if the process survives."
        ))
    }

    /// **Why the daemon ended this, for after the screen is given back.**
    ///
    /// `Some` only when a [`ServerFrame::Bye`] arrived — an ordinary quit has
    /// nothing to say. The caller prints it once the terminal is restored, because
    /// anything said into the transcript goes down with the alternate screen and
    /// the operator is left with a head that exited for no stated reason. That is
    /// what a protocol skew looked like on 2026-09-20: *"when i went to job with
    /// enter in pfn project leticode just exited"*.
    pub fn farewell(&self) -> Option<&str> {
        self.bye.as_deref()
    }

    pub fn head_id(&self) -> &str {
        &self.head_id
    }

    /// The next door call's number, for the `call_id` the daemon keys its pending set on.
    pub fn next_head_run(&mut self) -> u64 {
        self.head_run_seq += 1;
        self.head_run_seq
    }

    /// Tell the head it is about to ask the daemon, so the frames it draws in the
    /// meantime tell the truth.
    ///
    /// Called once, by a head that has taken the screen and is about to attach. See
    /// [`Self::attaching`] for what it suppresses and why.
    pub fn begin_attach(&mut self) {
        self.begin_attach_at(self.now_ms);
    }

    /// The same, with the clock stated rather than read, for a caller that knows it —
    /// and for a test, which has no clock.
    pub fn begin_attach_at(&mut self, now_ms: u64) {
        self.attaching = true;
        self.attach_started_ms = now_ms;
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
    /// **The picker's rows, as ONE enumeration — every key and the drawing read this.**
    ///
    /// # Why one function and not a filter at each reader
    ///
    /// Five things index a session list: the picker's arrows and Enter, its seeding, the row a
    /// typed number names, and the header's position. Working out *which row is that* separately
    /// for a nested list is the **two-enumerations defect** and this file already carries the scar:
    /// [`App::todos_stops`], whose docstring is the operator's own two reports — *"arrows dont go
    /// here"* and *"mouse doesnt click"* — two symptoms of one cursor whose position came from one
    /// list and whose row came from another.
    ///
    /// # The shape
    ///
    /// A conversation is depth 0 and keeps the daemon's own order. A sub-session sits directly
    /// under the session that spawned it, **when that session is expanded** — `parent_session_id`
    /// is already on every row, so nothing had to be added to the wire for this.
    ///
    /// **The chain down to the session you are IN is always shown**, whatever the collapse state:
    /// a picker that hides where you are is a picker that cannot answer *where am I*, which is the
    /// one question the header above it exists to answer.
    ///
    /// # What the filter this replaces was protecting against
    ///
    /// Not curiosity — noise. Collapsed-by-default is that concern answered instead of obeyed, and
    /// the operator's own words are the reason the OBEDIENCE was wrong: *"yes subagents are not
    /// even scratch session they are session, just sub sessions"*, and *"why readonly? subagent
    /// session is more like you driving others via tmux"*. A session a head can post to, and get an
    /// answer from, is not a row to be filtered — it is a session to be driven.
    ///
    /// # Inside a subagent the list is its FAMILY
    ///
    /// A head switched into a child used to draw the whole daemon — every conversation, and
    /// every conversation's children — around a row that was one level down. The operator's
    /// ask is the narrow one a tree walk implies: *"make sure sessions list (ctrl-s) is
    /// filtered to the parent and siblings"*, so that is what this is. The parent is the top
    /// row and its children are under it, **shown whatever the collapse state says**, because
    /// the filter and the expansion would otherwise be the same gesture twice — a family list
    /// whose siblings were collapsed into the parent would be a list of one row.
    ///
    /// The chain up to where you are is still always shown, and it is here by construction:
    /// the child you are in *is* one of the listed siblings. `esc` (up) and a row's `enter` walk
    /// that same edge from the outside, so ctrl-s inside a child answers *where am I* and *who is
    /// next to me* with one list — while ctrl-s in a conversation still answers *what does this
    /// daemon hold* under the collapse rule above. Two questions, one enumeration per question.
    pub(crate) fn session_rows(&self) -> Vec<SessionRow> {
        // The chain from the current session up to its root, by id — so the way back to where you
        // are is always on the screen.
        let mut path: Vec<String> = vec![self.session_id.clone()];
        let mut cur = self.session_id.as_str();
        while let Some(s) = self.sessions.iter().find(|s| s.session_id == cur)
            && let Some(p) = s.parent_session_id.as_deref()
        {
            path.push(p.to_string());
            cur = p;
        }
        let mut out: Vec<SessionRow> = Vec::new();
        // **This head is in a subagent**: the parent and its children, and nothing else.
        if let Some(parent) = self.parent_session() {
            if let Some(i) = self.sessions.iter().position(|s| s.session_id == parent) {
                out.push(SessionRow { idx: i, depth: 0 });
                self.push_children(&mut out, &parent, 1, &path, self.family_open(&parent, 0));
            }
            return out;
        }
        for (i, s) in self.sessions.iter().enumerate() {
            if s.parent_session_id.is_some() {
                continue;
            }
            out.push(SessionRow { idx: i, depth: 0 });
            self.push_children(&mut out, &s.session_id, 1, &path, false);
        }
        out
    }

    /// **Whether a row's children are on the list because this head is standing among them.**
    ///
    /// The family view ([`App::session_rows`]) shows the parent's children whatever the collapse
    /// state says — a family whose members were folded away would be a list of one row — so the
    /// first level of that view is open by rule rather than by the operator's `→`. One function,
    /// read by the enumeration **and by the fold glyph the picker draws beside the row**: a `▸`
    /// next to the rows it is hiding is the one thing this list must not say, and the glyph came
    /// from the collapse list alone, so a family view drew `▸` over three visible rows. Same rule,
    /// same reader, which is the `todos_stops` lesson this file already carries.
    pub(crate) fn family_open(&self, id: &str, depth: usize) -> bool {
        depth == 0 && self.parent_session().as_deref() == Some(id)
    }

    /// A session's children, in the daemon's order, one step deeper — the recursive half of
    /// [`App::session_rows`].
    ///
    /// A child is shown when its parent is expanded **or** when it is on the chain to the current
    /// session; anything else is collapsed into its parent. `force` is [`App::family_open`]'s own
    /// answer for the family view's first level — see `session_rows` — and it applies to **one
    /// level only**: a sibling's own children are still folded away until that sibling is
    /// expanded, which is what keeps the family view a family rather than the whole subtree behind
    /// it.
    pub(crate) fn push_children(
        &self,
        out: &mut Vec<SessionRow>,
        parent: &str,
        depth: usize,
        path: &[String],
        force: bool,
    ) {
        let open = force || self.expanded.iter().any(|e| e == parent);
        for (i, s) in self.sessions.iter().enumerate() {
            if s.parent_session_id.as_deref() != Some(parent) {
                continue;
            }
            if !open && !path.iter().any(|p| *p == s.session_id) {
                continue;
            }
            out.push(SessionRow { idx: i, depth });
            self.push_children(out, &s.session_id, depth + 1, path, false);
        }
    }

    /// **The conversation this head's session belongs to** — itself, unless it is a sub-session.
    ///
    /// The header counts *conversations*, and this is the half that keeps a head driving a child
    /// from reading `0/4`: it still says which conversation it is in.
    pub(crate) fn session_root(&self) -> String {
        let mut cur = self.session_id.as_str();
        while let Some(s) = self.sessions.iter().find(|s| s.session_id == cur)
            && let Some(p) = s.parent_session_id.as_deref()
        {
            cur = p;
        }
        cur.to_string()
    }

    /// **What the terminal's window title says**: the session's name and the folder, so a
    /// tab is told apart by the conversation in it rather than reading `leticode` like
    /// every other tab. The name is the header's (`session_label`: its title, or a short id
    /// before it has one); before a session is attached there is nothing to name but the
    /// program. The terminal strips control characters before writing it.
    pub fn window_title(&self) -> String {
        if self.session_id.is_empty() {
            return "letibot".to_string();
        }
        let label = self.session_label(&self.session_id);
        let titled = self
            .sessions
            .iter()
            .any(|s| s.session_id == self.session_id && !s.title.is_empty());
        let folder = std::path::Path::new(&self.wiring.workspace)
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        // A name leads; a short id does not — before the first message (which titles the
        // session) the folder is the more useful word to find a tab by.
        match (titled, folder.is_empty()) {
            (_, true) => label,
            (true, false) => format!("{label} · {folder}"),
            (false, false) => format!("{folder} · {label}"),
        }
    }

    /// **Tell the head's state what the terminal speaks** (see `crate::features`).
    pub fn set_features(&mut self, f: crate::backend::features::Features) {
        if self.features != f {
            self.features = f;
            self.invalidate_history();
            self.redraw = true;
        }
    }

    /// **The tab's progress bar** (OSC 9;4): what it should show this tick. Somebody waiting on
    /// the person outranks the model working — a tab that wants you must not look like one
    /// that is merely busy.
    pub fn progress(&self) -> crate::backend::terminal::Progress {
        use crate::backend::terminal::Progress;
        if !self.open.is_empty() || self.secret.is_some() {
            return Progress::Waiting;
        }
        if self.turn_busy() {
            return Progress::Busy;
        }
        match self.turn.as_ref().and_then(|t| t.state.as_ref()) {
            Some(TurnState::Failed { .. }) => Progress::Failed,
            _ => Progress::Idle,
        }
    }

    /// What needs the person right now, as counts and ids — compared tick to tick by
    /// [`App::take_notification`], so a notification is an EDGE and never a level.
    pub(crate) fn attention_now(&self) -> Attention {
        Attention {
            busy: self.turn_busy(),
            asks: self.open.iter().map(|d| d.req_id.clone()).collect(),
            secret: self.secret.as_ref().map(|s| s.req_id.clone()),
            answered: self
                .subagents
                .iter()
                .filter(|s| matches!(s.state.as_str(), "done" | "failed"))
                .map(|s| s.session_id.clone())
                .collect(),
        }
    }

    /// **A desktop notification, when something just started needing the person and they are
    /// not looking.** The operator's ask that started this: a session sat for minutes on a
    /// wait nobody was watching. Four edges, in the order they matter: a permission card
    /// arrived, a key or password card arrived, a subagent answered, the turn ended.
    ///
    /// The snapshot moves every call, focused or not — so coming back to the window and
    /// leaving again does not replay what was already on the screen.
    pub fn take_notification(&mut self) -> Option<String> {
        let now = self.attention_now();
        let before = self.attention.replace(now.clone())?;
        if !self.features.notify || self.focused != Some(false) {
            return None;
        }
        let who = self.window_title();
        if let Some(d) = self
            .open
            .iter()
            .rev()
            .find(|d| !before.asks.contains(&d.req_id))
        {
            return Some(format!("{who}: permission needed — {}", d.summary));
        }
        if let Some(s) = &self.secret
            && before.secret.as_ref() != Some(&s.req_id)
        {
            let first = s.prompt.lines().next().unwrap_or("").trim();
            return Some(format!("{who}: {first}"));
        }
        if let Some(id) = now
            .answered
            .iter()
            .find(|id| !before.answered.contains(*id))
        {
            return Some(format!("{who}: subagent {} finished", short_id(id)));
        }
        if before.busy && !now.busy && now.asks.is_empty() && now.secret.is_none() {
            let how = match self.turn.as_ref().and_then(|t| t.state.as_ref()) {
                Some(TurnState::Failed { .. }) => "the turn failed",
                Some(TurnState::Interrupted { .. }) => "the turn was interrupted",
                _ => "done",
            };
            return Some(format!("{who}: {how}"));
        }
        None
    }

    /// **`/copy`: the open ctrl-v window's output, or else the model's last reply, onto the
    /// system clipboard** (OSC 52 — which works over ssh, where `pbcopy` on the far side would
    /// fill the wrong machine's clipboard). A slash verb rather than a key, because the
    /// composer is live under the window and every bare letter is typing.
    pub(crate) fn copy_command(&mut self) {
        if !self.features.clipboard {
            self.say(
                "this terminal is not known to take OSC 52, so nothing was copied — \
                 LETIBOT_TERM_FEATURES=clipboard turns it on",
            );
            return;
        }
        let window = self.payload_sel.as_ref().and_then(|id| {
            self.items
                .iter()
                .find(|r| &r.item_id == id)
                .and_then(|r| match r.item.as_ref() {
                    Some(TranscriptItem::ToolResult { payload, .. }) => {
                        Some(("the open output", payload.clone()))
                    }
                    _ => None,
                })
        });
        let found = window.or_else(|| {
            self.items.iter().rev().find_map(|r| match r.item.as_ref() {
                Some(TranscriptItem::Assistant { text, .. }) if !text.trim().is_empty() => {
                    Some(("the last reply", text.clone()))
                }
                _ => None,
            })
        });
        match found {
            Some((what, text)) => {
                let lines = text.lines().count();
                self.clipboard_out = Some(text);
                self.say(&format!("copied {what} — {lines} line(s)"));
            }
            None => self.say("nothing to copy: no output is open and the model has not replied"),
        }
    }

    /// **Every PNG a row carries that the terminal does not have yet, queued for upload** — each
    /// once, under the id its row draws with. Only the rows added since the last look are
    /// walked; a list that shrank (a switch, a resync) is walked again from the top.
    pub(crate) fn queue_image_uploads(&mut self) {
        // **The size follows the window.** Every image already in the terminal is placed again
        // at the box this frame's width gives — a placement command each, no image bytes — so
        // the rows the renderers draw at this width match what the terminal will fill.
        let box_cols = crate::backend::graphics::image_box(self.cfg.width);
        if box_cols != self.images_box {
            self.images_box = box_cols;
            for (id, (w, h)) in &self.images_sent {
                let (cols, rows) = crate::backend::graphics::image_cells(*w, *h, box_cols);
                self.image_uploads
                    .push(crate::backend::graphics::image_place(*id, cols, rows));
            }
        }
        if self.images_scanned > self.items.len() {
            self.images_scanned = 0;
        }
        let mut scanned = self.images_scanned;
        let mut found: Vec<(u32, Option<u32>, Option<u32>, String)> = Vec::new();
        for it in &self.items[self.images_scanned..] {
            // **A row whose content has not arrived stops the walk**, and is looked at again
            // next frame: `TranscriptAppended` and its content are separate events, and a mark
            // moved past an empty row would never come back for the picture in it.
            let Some(item) = it.item.as_ref() else { break };
            scanned += 1;
            match item {
                TranscriptItem::ToolResult { media: Some(m), .. } if m.mime == "image/png" => {
                    found.push((
                        crate::backend::graphics::image_id(&it.item_id),
                        m.width,
                        m.height,
                        m.wire_base64().to_string(),
                    ));
                }
                TranscriptItem::Assistant { text, .. } if text.contains("![") => {
                    for (_, target) in crate::render::markdown_images(text) {
                        let Some(m) = self.read_local_png(&target) else {
                            continue;
                        };
                        let id =
                            crate::backend::graphics::image_id(&format!("{}#{target}", it.item_id));
                        crate::render::remember_reply_image(
                            &it.item_id,
                            &target,
                            (id, m.width, m.height),
                        );
                        found.push((id, m.width, m.height, m.wire_base64().to_string()));
                    }
                }
                _ => {}
            }
        }
        self.images_scanned = scanned;
        for (id, w, h, b64) in found {
            if self.images_sent.insert(id, (w, h)).is_none() {
                let (cols, rows) = crate::backend::graphics::image_cells(w, h, box_cols);
                self.image_uploads
                    .push(crate::backend::graphics::image_upload(id, &b64));
                self.image_uploads
                    .push(crate::backend::graphics::image_place(id, cols, rows));
            }
        }
    }

    /// **A PNG a reply named, read by the head** — absolute, `~/`, or relative to the
    /// session's workspace; at most 16 MiB; and only if its bytes say PNG, whatever the name
    /// says. `None` for anything else, which leaves the alt text as the row's only word.
    pub(crate) fn read_local_png(&self, target: &str) -> Option<letibot_transcript::media::Media> {
        let path = if let Some(rest) = target.strip_prefix("~/") {
            std::path::PathBuf::from(std::env::var_os("HOME")?).join(rest)
        } else if target.starts_with('/') {
            std::path::PathBuf::from(target)
        } else {
            std::path::Path::new(&self.wiring.workspace).join(target)
        };
        let meta = std::fs::metadata(&path).ok()?;
        if !meta.is_file() || meta.len() > 16 * 1024 * 1024 {
            return None;
        }
        let bytes = std::fs::read(&path).ok()?;
        letibot_transcript::media::Media::of(&path.to_string_lossy(), &bytes)
            .filter(|m| m.mime == "image/png")
    }

    /// Image uploads for the head to write to the terminal (kitty graphics).
    pub fn take_image_uploads(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.image_uploads)
    }

    /// Text the operator asked to copy, for the head to write to the clipboard.
    pub fn take_clipboard(&mut self) -> Option<String> {
        self.clipboard_out.take()
    }

    /// Whether the terminal reported a light background (OSC 11).
    pub fn light_background(&self) -> Option<bool> {
        self.light_background
    }

    pub(crate) fn session_label(&self, id: &str) -> String {
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

    /// **A frame this head could not read: said, and counted.**
    ///
    /// The requirement is *survive AND count*. Dying on an unparseable frame takes the
    /// session down and says nothing; stepping over one in silence is the same failure
    /// more quietly, because *"this daemon is sending me something I do not
    /// understand"* then looks exactly like quiet. So this is the one entry point the
    /// driver has for `Inbound::Unreadable`: it says what it was, in the transcript,
    /// and it moves a counter that `/status` carries and the border names once it has
    /// moved.
    ///
    /// # What it deliberately does not do
    ///
    /// It does not touch `seq`, and nothing about it is acked. No frame was parsed, so
    /// there is no seq to report — and inventing one would rewind this head's read mark
    /// over frames it has already read, which is the one thing a mark must never do.
    /// `Ack`'s `filtered` is "events I chose not to show" and this is not that either,
    /// so it moves neither counter the daemon reads back.
    ///
    /// It is `Control` rather than `Rendered` for the same reason a `Hello` is: the
    /// counter it moves is on a screen this head does not reach for, and the sentence
    /// is filed as a note like every other thing that happened between rows — anchored
    /// where it arrived, so it scrolls away like the rest of the conversation instead
    /// of sitting above the composer for ever.
    pub fn unreadable(&mut self, u: Unreadable) -> Disposition {
        self.unreadable += 1;
        // `ts` is 0: this happened on the socket rather than on the session's log, and
        // the log's clock is not this. A note with no timestamp renders without one,
        // which is the honest shape — see `clock_time`.
        self.note(Note::Warned(Warned {
            code: "unreadable_frame".into(),
            detail: u.said(),
            ts: 0,
        }));
        self.redraw = true;
        Disposition::Control
    }

    /// **The daemon connection has gone.** Called by the driver when a write fails or
    /// when the pump's channel closes, and by the caller when a reconnect attempt
    /// fails.
    ///
    /// Idempotent, and that matters: the two ways it is noticed arrive together — the
    /// socket EOFs and the pump dies, so the reader sees `Disconnected` while a write
    /// in the same pass fails too — and the elapsed time is measured from the *first*
    /// report rather than from the last, so the sentence does not keep starting over.
    ///
    /// A head that is going away does not reconnect, and neither does one that is
    /// already on its way out: a `Bye` and a `/quit` both arrive mid-pass, and turning
    /// either into a two-second retry loop is how leticl made a refusal unescapable.
    pub fn link_down(&mut self, why: &str) {
        if self.quit || self.bye.is_some() {
            return;
        }
        // **No answer is coming, so nothing is asked for.** A `SuggestShell` this head
        // queued is dropped by the driver when the link is down ("nothing leaves a head
        // whose link is down"), and a head that kept the ask would go on saying *asking
        // the model* — for ever, because the id it is waiting on was never sent. Cleared
        // here rather than in the driver's refusal arm because this is the fact: the
        // socket the answer would arrive on is gone. The transcript's own snapshot after
        // a reconnect clears them too, and both are the same rule.
        self.clear_shell_suggestions();
        // **A link that went down because this head asked is not news.** R30: the daemon
        // closing our socket is the answer arriving, and drawing *"the daemon connection
        // is down — reconnecting"* over a shutdown the operator ordered would be this
        // head reporting its own request as a fault. Reconnecting is wrong for the same
        // reason: the loop would open a second socket to a process that is on its way
        // out, and `should_reconnect` would be true the whole time it waited.
        if self.stopping.is_some() {
            return;
        }
        // **And the pane goes with the connection.** See [`App::drop_pane`]: the pty is the
        // daemon's, and a head that cannot reach it can neither feed the screen nor forward
        // the one key that leaves.
        self.drop_pane();
        if self.link.is_down() {
            // Already known: refresh the reason if this report has one and keep the
            // clock. Both reports are true; the first is the more useful clock.
            if let Link::Reconnecting { why: known, .. } = &mut self.link
                && !why.is_empty()
            {
                *known = why.to_string();
            }
            return;
        }
        self.link = Link::Reconnecting {
            since_ms: self.now_ms,
            attempts: 0,
            why: why.to_string(),
            next_try_ms: self.now_ms.saturating_add(RECONNECT_BACKOFF_MS),
        };
        self.redraw = true;
    }

    /// **Whether the caller should try to get back now**: the link is down and the
    /// backoff has passed.
    ///
    /// The timing lives on the head because the head has the clock, and because a
    /// caller that kept its own would be a second copy of a rule about how often to
    /// retry.
    pub fn should_reconnect(&self) -> bool {
        matches!(&self.link, Link::Reconnecting { next_try_ms, .. } if self.now_ms >= *next_try_ms)
    }

    /// **An attempt to get back is out.** Called by the caller the moment it has opened
    /// a socket and sent its `ATTACH`, so the head does not try again while an answer is
    /// in flight.
    ///
    /// This is not an optimisation, it is what stops the retry from eating its own
    /// answer: an `ATTACH` goes out on a *live* socket and the `Hello` comes back a
    /// moment later, and a loop that asked `should_reconnect` between those two would
    /// open a second socket and — to open it — close the first, which is the one about
    /// to be answered. Found by the end-to-end test, which deadlocked on it.
    ///
    /// It pushes the window out without counting an attempt: a socket that opened is not
    /// a failure, and the `Hello` either arrives (the link goes up) or the backoff passes
    /// and this tries again.
    pub fn reconnect_sent(&mut self) {
        if let Link::Reconnecting { next_try_ms, .. } = &mut self.link {
            *next_try_ms = self.now_ms.saturating_add(RECONNECT_BACKOFF_MS);
        }
    }

    /// An attempt to get back has failed, and this is the last thing it said. Counted,
    /// so the sentence can say how many times the head has tried — a head that has tried
    /// forty times and one that has tried once are in different situations, and "still
    /// reconnecting…" says neither.
    pub fn reconnect_failed(&mut self, why: &str) {
        if let Link::Reconnecting {
            attempts,
            why: known,
            next_try_ms,
            ..
        } = &mut self.link
        {
            *attempts += 1;
            *known = why.to_string();
            *next_try_ms = self.now_ms.saturating_add(RECONNECT_BACKOFF_MS);
            self.redraw = true;
        }
    }

    /// **The daemon is back** — the `Hello` is what says so, because it is the frame
    /// that seats this connection.
    ///
    /// Returns the sentence rather than filing it, and the caller is why: this runs
    /// inside the `Hello` arm, and the very next thing that arm does is fold in the
    /// snapshot — which **replaces `self.notes` wholesale**, a snapshot's warnings being
    /// the head's whole warning history. A note filed here would be thrown away, which is
    /// how the first version of this said nothing at all. Same shape, same reason, as
    /// `letibot_sessionlog::protocol_skew`'s sentence in the same arm.
    ///
    /// The sentence carries the seq the head asked from, so "where it picks up" is a
    /// number and not a promise. `None` when nothing was down, so the `Hello` a `Switch`
    /// produces says nothing.
    pub(crate) fn link_up(&mut self) -> Option<String> {
        let Link::Reconnecting {
            since_ms, attempts, ..
        } = self.link.clone()
        else {
            return None;
        };
        let out = self.now_ms.saturating_sub(since_ms);
        // Only when there *were* failed attempts: the ordinary reconnect succeeds on
        // its first try, and `(...4.4s ().)` with nothing in the brackets is a head
        // saying a number where there is none.
        let tries = if attempts == 0 {
            String::new()
        } else {
            format!(
                " ({} attempt{})",
                attempts,
                if attempts == 1 { "" } else { "s" }
            )
        };
        self.link = Link::Attached;
        self.redraw = true;
        Some(format!(
            "the daemon is back after {}{tries}. Resuming from seq {} — anything the \
             daemon has for me in the gap arrives as events, or as a resync if it is \
             larger than the daemon still holds.",
            dur_human(out),
            self.seq,
        ))
    }

    /// A command the operator (or a frame) produced did not leave, because there is no
    /// daemon to send it to. Said once per batch: the alternative is one sentence per
    /// action, and the batch is usually one action.
    pub fn refused_while_detached(&mut self) {
        self.say("no daemon connection — that did not go out");
        self.redraw = true;
    }

    /// Whether the link to the daemon is down. What the screen asks before it draws the
    /// detached line, and what `submit` asks before it turns a line into a prompt.
    pub fn detached(&self) -> bool {
        self.link.is_down()
    }

    /// **The line a head with no daemon draws**, above the composer and under nothing.
    ///
    /// A resident line rather than a note, and the distinction is the requirement: a
    /// note is anchored to a place in the conversation and scrolls away with it, while
    /// this is a *state* — it is true until it is not, and a head that had said it once,
    /// at the top of the scrollback, would be saying nothing about right now. That is
    /// the same argument the stuck line makes, one level up: this is why the turn went
    /// quiet.
    ///
    /// Past [`LINK_IMPATIENT_MS`] it stops saying `reconnecting` and starts saying how
    /// long, how many tries, and what the operator can do — because at that point the
    /// wait is not going to end on its own, and a person staring at a head that says
    /// `reconnecting` has no way to tell a quarter of a second from an afternoon.
    pub(crate) fn link_line(&self, w: usize) -> Vec<String> {
        let Link::Reconnecting {
            since_ms,
            attempts,
            why,
            ..
        } = &self.link
        else {
            return Vec::new();
        };
        let out = self.now_ms.saturating_sub(*since_ms);
        let impatient = out >= LINK_IMPATIENT_MS;
        let said = if impatient {
            format!(
                "the daemon connection is down — trying for {} ({} attempt{}). If the \
                 daemon is gone: `letibot` starts one and `letibot --status` says what is \
                 on the socket. This head keeps trying either way.",
                dur_human(out),
                attempts,
                if *attempts == 1 { "" } else { "s" },
            )
        } else {
            format!("the daemon connection is down — reconnecting. {why}")
        };
        wrap(&format!("⚠ {said}"), w)
            .into_iter()
            .map(|l| colour(&self.cfg, sgr::YELLOW, &l))
            .collect()
    }

    /// **The line a head draws while it waits for a daemon it asked to stop** (R30).
    ///
    /// A resident line and not a note, for `link_line`'s reason: it is a state, true until
    /// it is not, and the operator is by definition still looking at the screen. The
    /// requirement says *the head says what it is waiting for rather than freezing on a
    /// dead screen*, and this is that sentence — with the elapsed time, the time left, and
    /// what the daemon has done so far, because "waiting" and "waiting and it has answered"
    /// are different facts and the second one means it worked.
    pub(crate) fn stopping_line(&self, w: usize) -> Vec<String> {
        let Some(s) = self.stopping.as_ref() else {
            return Vec::new();
        };
        if s.resolved(self.now_ms) {
            // The wait is over and the head is leaving on the next pass. A line that said
            // "waiting" under a verdict the farewell is about to give would be this head
            // arguing with itself.
            return Vec::new();
        }
        wrap(&format!("⚠ {}", s.waiting_line(self.now_ms)), w)
            .into_iter()
            .map(|l| colour(&self.cfg, sgr::YELLOW, &l))
            .collect()
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
                protocol_version,
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
                // The daemon has answered, so whatever the head drew while it was
                // asking is about to be replaced by the truth. See `attaching`.
                self.attaching = false;
                // **The version the daemon is TOLD to be, checked at the handshake.**
                //
                // This is the one moment a skew is cheap to say: nothing has been read
                // yet, the direction is known, and the sentence can name both numbers
                // while the operator is still looking at the head rather than at a frame
                // that failed to parse. `protocol_skew` is `None` when they match — the
                // normal case says nothing at all, so a line here is a fact and not
                // furniture.
                //
                // **It does not exit.** A skew is usually survivable — that is R3's whole
                // argument — so the head says which way round it is and carries on. What
                // differs is what the operator should expect, and that is why the two
                // directions get different sentences: a newer daemon means frames this
                // head will report and skip, an older one means the next command the two
                // do not share ends the session.
                self.daemon_protocol = Some(protocol_version);
                // **And which daemon that is.** A head outlives the daemon that gave it its
                // facts — the operator's box has had three `letibot-tui` processes up for days
                // while the daemon was replaced underneath them — so the party at the other end
                // of this socket has to be checked rather than assumed. The pid is re-read per
                // connection by the caller that opened it, so by the time this arm runs it is
                // *this* daemon's, and the protocol rides this frame.
                //
                // **The check is on the SEATING, and the seating is where it belongs**: a
                // `Hello` is an attach, a re-attach and the return from a switch, and a switch
                // is answered on the same socket by the same process — so this fires once per
                // connection that lands somewhere new, and not once per keystroke.
                let seat = DaemonSeat {
                    pid: self.daemon_pid,
                    protocol: protocol_version,
                };
                let was = self.daemon_seat;
                self.daemon_seat = Some(seat);
                // **Held, not filed.** `load` replaces `self.notes` wholesale a few lines down,
                // so a note written here is thrown away — the same reason the skew's sentence
                // and the reattach's are held rather than said. See `App::link_up`.
                //
                // **Both numbers on both sides, and `None` is said as `not told`.** A pid the
                // kernel declined to name is not a pid of zero, and the same rule R30 keeps on
                // the farewell applies here: a head that printed a number it did not have would
                // send the operator to `ps` for a process that is not there.
                let replaced_said = was.filter(|w| *w != seat).map(|w| {
                    format!(
                        "this is not the daemon this head was attached to. That one was {} and \
                         spoke protocol {}; this one is {} and speaks {}. Everything the old one \
                         told me that was its own — its session list, its job table, its children \
                         — has been asked for again, because a daemon's registry is in memory \
                         and a replacement holds none of it. What you are reading below is what \
                         THIS daemon holds.",
                        pid_word(w.pid),
                        w.protocol,
                        pid_word(seat.pid),
                        seat.protocol,
                    )
                });
                // **The `Hello` is what says the link is back.** It is the frame that
                // seats this connection, so it is the only honest answer to "are we
                // attached" — a socket that accepts and then says nothing is not.
                // `link_up` is a no-op when nothing was down, which is every ordinary
                // attach and the second `Hello` a `Switch` produces.
                let reattached = self.link_up();
                let mut skew_said = None;
                if let Some(said) = letibot_sessionlog::protocol_skew(
                    protocol_version,
                    letibot_sessionlog::protocol::PROTOCOL_VERSION,
                ) {
                    // **Held until after the snapshot is folded in.** `load` replaces
                    // `self.notes` wholesale — a snapshot's warnings are the head's whole
                    // warning history — so a note filed before it is not "anchored at the
                    // frame that revealed this", it is thrown away. Found by the test
                    // that asserts the sentence exists, which is the only reason this is
                    // not a silence nobody would have noticed.
                    skew_said = Some(said);
                }
                // **And every session-scoped read this head owes itself, in one place.**
                //
                // This is the attach, the re-attach (a reconnect is answered with a
                // `Hello` too) and the return from a switch — the daemon answers a
                // `Switch` with a second `Hello`, which is why there is one call here
                // and not three. See [`App::refetch_session_facts`] for what a switch
                // drops and why each of the three is asked for rather than waited on.
                //
                // **And what this session's pane is running** — the read behind the head's own
                // line about a program it is not drawing. Asked here, with the others, for
                // the same shape of reason: the daemon answers a read and never volunteers one,
                // and a head that has just been SEATED somewhere knows nothing about the pane
                // there — its own pane went with the session it left (see [`App::load`]), and
                // the pane in the new one is the session's, not this head's.
                //
                // **`Unasked` rather than `None`**, because those are different facts and one
                // of them decides whether `!term close` asks or refuses — see [`PaneFact`].
                self.term_fact = PaneFact::Unasked;
                self.close_pending = false;
                self.term_ask = None;
                self.queued.push(Action::TermStatus);
                self.refetch_session_facts();
                self.head_id = head_id.clone();
                self.seated = Some(head_id);
                self.wiring = wiring;
                // **Sub-sessions are KEPT, and the belief that used to filter them here is the
                // defect this line closes.** The operator, 2026-10-03: *"yes subagents are not even
                // scratch session they are session, just sub sessions"*, and *"why readonly?
                // subagent session is more like you driving others via tmux"*. A child is a
                // session the daemon holds, a head can post to it and it answers — so a head that
                // drops it is throwing away a session it can DRIVE.
                //
                // What the filter was protecting against was noise, and that is
                // [`App::session_rows`]'s business now: nested, collapsed by default, with the
                // parent's id already on the row.
                self.sessions = sessions;
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
                // **Now it can be said.** A note rather than a `say`: this is a fact about
                // the connection that outlives the next keystroke, and it belongs in the
                // conversation with everything else that happened — anchored at the end of
                // whatever the snapshot carried, which is where the reader is. `note`
                // dedupes on `(code, detail, ts)`, so the second `Hello` a `Switch`
                // produces does not say it twice.
                if let Some(said) = skew_said {
                    self.note(Note::Warned(Warned {
                        code: "protocol_skew".into(),
                        detail: said,
                        ts: 0,
                    }));
                }
                if let Some(said) = replaced_said {
                    self.note(Note::Warned(Warned {
                        code: "daemon_replaced".into(),
                        detail: said,
                        ts: 0,
                    }));
                }
                if let Some(said) = reattached {
                    self.note(Note::Warned(Warned {
                        code: "reattached".into(),
                        detail: said,
                        ts: 0,
                    }));
                }
                // A daemon that restarted has no turn state in the snapshot —
                // `TurnFinished` is ephemeral, and a view rebuilt from the
                // transcript has no turn — so the context the last turn left
                // behind comes from the session's own row: the daemon writes it
                // on every round finish, and the brief carries it here. A live
                // session's snapshot already set the usage, and that wins.
                if self.usage.is_none()
                    && let Some(b) = self
                        .sessions
                        .iter()
                        .find(|s| s.session_id == self.session_id)
                    && let Some(tokens) = b.context_tokens
                {
                    // The row knows the prompt size; it knows the cache fraction
                    // only if a turn finished after the column existed. A
                    // backfilled row has the size and not the fraction, and the
                    // header shows the percentage only when it was measured.
                    self.usage_cache_measured = b.context_cached.is_some();
                    self.usage = Some(Usage {
                        prompt_tokens: tokens,
                        cached_tokens: b.context_cached.unwrap_or(0),
                        predicted_tokens: 0,
                        // A backfilled row carries the prompt size, never the
                        // cost: nothing recorded it per turn before now, and a
                        // zero here would report a metered session as free.
                        cost_micros_usd: None,
                    });
                }
                if moved {
                    // The picker is closed by arriving, not by the key that opened
                    // it: the switch is the answer to the question the picker
                    // asked, and leaving it up over the session you just joined is
                    // a screen the operator has to dismiss for no reason.
                    self.picker = false;
                    self.say(&format!(
                        "switched to {}",
                        self.session_label(&self.session_id)
                    ));
                }
                // **AND THE STARTER TODOS** — leticl's `%seed-operator-todos`, on the one moment
                // leticl runs it: the HELLO, where a head learns its project (its own docstring:
                // *"this is the one function where a head learns its list, so it is the one place
                // a list can be STARTED"*). A switch lands here too, so each project is checked
                // in its own right. The seed itself waits for the board — see `todo_seed_pending`
                // — because this head keeps no second list and must not send a half it has not
                // read.
                if self.todo_seed_due() {
                    self.queued.push(Action::ListTodos);
                    self.todo_seed_pending = true;
                }
                Disposition::Control
            }
            ServerFrame::Sessions {
                sessions,
                current,
                created,
            } => {
                // Kept whole, like the `Hello` arm above — see it for why the filter is gone.
                self.sessions = sessions;
                self.session_id = current;
                // **And the rows the list is the source of are re-derived from it, here.**
                //
                // A `Sessions` frame REPLACES `self.sessions`, and `self.sessions` is what
                // [`App::fold_subagents`] folds the subagent rows and the composer's count
                // from — so a list that lands without this line moves the picker and leaves
                // the pane and the count standing on the list that was just thrown away.
                // That is the defect this arm had, and it is what made a re-ask useless:
                // the answer to *what are my children now* arrived and was not applied.
                self.fold_subagents();
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
            // **The daemon's job table, whole.** Which jobs are listed, what the
            // command reads as and what the state word is are all its answers —
            // the head used to fold them out of the event stream and join the
            // command against whichever turn it happened to be showing, which is
            // how a job that outlived its turn lost its name.
            ServerFrame::Jobs { session_id, jobs } => {
                if session_id == self.session_id {
                    self.jobs = jobs;
                    self.jobs_sel = self.jobs_sel.min(self.job_stops().len().saturating_sub(1));
                    self.redraw = true;
                }
                Disposition::Control
            }
            // **The merge queue, whole, and the reviewer's verdicts beside it.** The queue is
            // daemon-level, so there is no `session_id` to check against this head's — the
            // `session_id` on an entry is its origin and not a filter, which is why the same
            // frame reaches a head attached to any session.
            ServerFrame::MergeQueue { entries, reviews } => {
                self.merge = entries;
                self.merge_reviews = reviews;
                self.queue_sel = self.queue_sel.min(self.merge.len().saturating_sub(1));
                self.redraw = true;
                Disposition::Control
            }
            // **The model's proposed `!` completions, answered.** The answer to a
            // `SuggestShell` this head sent, correlated by the id it minted: a
            // suggestion cached under the wrong prefix is a wrong suggestion, and the
            // id is what tells one answer from another. An id this head is not
            // holding — the transcript advanced and the ask was cleared — is stale
            // and is dropped, because a suggestion about a conversation that moved is
            // a suggestion about the wrong conversation.
            //
            // **Nothing here fills the composer.** The lines are cached and drawn as
            // candidates, with their provenance; only a Tab fills the composer with
            // one, and Enter is still the operator's.
            ServerFrame::ShellSuggestions {
                client_request_id,
                prefix: _,
                lines,
            } => {
                if let Some((prefix, position)) = self.shell_ask.remove(&client_request_id) {
                    self.shell_suggestions.insert((prefix, position), lines);
                    self.redraw = true;
                }
                Disposition::Control
            }
            ServerFrame::Todos { session_id, todos } => {
                if session_id == self.session_id {
                    self.todos = todos;
                    // **A waiting seed runs here too** — this is the REPLY to the `ListTodos` the
                    // attach queues when a seed is due, and it reads the board as whole as the
                    // event does. One seed, two arrivals, because the wire has two: the reply and
                    // the announcement.
                    if self.todo_seed_pending {
                        self.todo_seed_pending = false;
                        self.seed_todos();
                    }
                    self.redraw = true;
                }
                Disposition::Control
            }
            // The answer to the tree's read key (`p`, or `/peek ID`): the named subagent's
            // scrollback, scrubbed as a replay. Read, never folded — these events are not
            // this session's history, and folding them would lie about whose
            // turn is whose. The pane shows the tool results; the whole view is
            // spilled to a file so no cap on the pane is a cap on the record.
            ServerFrame::Settings { rows } => {
                self.settings = rows;
                // Stamped, so the header can tell whether a turn has named a model
                // since. See `model_from_settings_at`.
                self.model_from_settings_at = self.seq;
                // **A PICKER THAT OPENED BEFORE ITS OWN LIST ARRIVED SEEDS NOW** — see
                // [`App::pick_unseeded`]. `/mode` and `/models` send the ask and draw the card in
                // the same breath, so on a head whose rows have not landed the cursor goes to row
                // 0 and the current row is marked further down with the cursor somewhere else.
                //
                // **Only while the cursor is still untouched.** A frame landing mid-arrow must not
                // snap the reader back to where they started, which is the reason this was not
                // re-seeded at all before; the flag is what tells the two cases apart.
                if self.pick.is_some() && self.pick_unseeded {
                    self.seed_pick();
                }
                self.redraw = true;
                Disposition::Control
            }
            // **A fetched row window the head did not ask for.**
            //
            // `FetchRow` exists and is answered (see `ClientFrame::FetchRow` and the
            // server's arm), and **this head does not send it yet**. Its paging works over
            // the rows the snapshot gave it: `ViewBounds` is 2000 rows and 8 MB, and the
            // head's own paging draws windows of a payload it already holds. What is
            // missing is the case where the row is *not* held — trimmed by those bounds —
            // and that needs the head to ask, track the answer, and render a partial body.
            // Filed in `TODO.md` R19.2 rather than half-built here.
            //
            // The arm is explicit rather than a wildcard so that the day the head starts
            // asking, a frame that arrives unhandled is visible here rather than swallowed
            // by a `_ =>`.
            // **R11's locator, answered.** The bytes go to the pane that asked, on the same
            // channel `Peeked` and `JobOutput` use — a pane, not the conversation, because a
            // head asked for them to READ and a brief scrolled past in the chat is a brief
            // nobody finds again.
            //
            // **`None` reads as *not recorded*, never as empty.** The store holds `NULL` on
            // every row written before R11 kept the exchange, and an oracle that never answered
            // has no reply either — the two are one sentence to a reader and neither is *"here
            // it is, and it is nothing"*.
            ServerFrame::Diagnostic {
                request_id,
                kind,
                body,
                total,
            } => {
                let what = match kind {
                    letibot_sessionlog::protocol::DiagnosticKind::Brief => "the brief it was shown",
                    letibot_sessionlog::protocol::DiagnosticKind::Reply => "the reply it gave",
                };
                let mut lines: Vec<String> = vec![format!("{request_id} — {what}")];
                match &body {
                    Some(b) if b.is_empty() => {
                        lines.push(String::new());
                        lines.push("(recorded, and zero bytes)".into());
                    }
                    Some(b) => {
                        lines.push(String::new());
                        lines.extend(b.lines().map(str::to_string));
                        lines.push(String::new());
                        lines.push(format!("  {total} bytes"));
                    }
                    None => {
                        lines.push(String::new());
                        lines.push(
                            "not recorded. A row written before the exchange was kept has no \
                             brief and no reply, and an oracle that never answered has no \
                             reply either — this says which case it is not."
                                .into(),
                        );
                    }
                }
                self.slash_out = Some(("diagnostic".to_string(), lines));
                self.pane_scroll = 0;
                self.redraw = true;
                Disposition::Control
            }
            ServerFrame::RowFetched { .. } => Disposition::Control,
            ServerFrame::Peeked {
                session_id,
                dropped,
                events,
                snapshot,
            } => {
                self.sub_out_pending = None;
                // **The session's rows, when the daemon sent them** — and the whole point of the
                // field is that this is the ORDINARY path now. A child is a session (the operator,
                // 2026-10-03: *"yes subagents are not even scratch session they are session, just
                // sub sessions"*), so its rows are drawn by the one renderer that draws rows, and
                // the hand-rolled plain-string path below is what an older daemon falls back to.
                let (lines, degraded) = match &snapshot {
                    Some(s) => (self.sub_out_from_rows(&s.items), false),
                    None => (subagent_out_lines(&events), true),
                };
                let mut lines = lines;
                if lines.is_empty() {
                    lines.push(match snapshot {
                        // A session with nothing in it is a different statement from a session
                        // whose rows could not be read, and the two must not look alike.
                        Some(_) => "    this session has no rows yet.".to_string(),
                        None => "    this subagent's scrollback has neither an answer nor tool \
                                  output. It may still be running, or its rows may have fallen \
                                  off the daemon's ring."
                            .to_string(),
                    });
                }
                let spill = spill_sub_out(&session_id, &lines);
                self.sub_out = Some(SubOut {
                    session_id,
                    lines,
                    degraded,
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
                // **Through `say`, so this line has a clock like every other.** It used to
                // be a direct write to the slot, which armed no countdown: the sentence
                // then stood until the next notice replaced it, for the rest of the
                // session. The reason is worth reading and is not worth keeping — the
                // `resyncs` counter holds it, the alarm triangle repeats it, and
                // `/status` spells it out — so it is news like the rest, and `say` is the
                // one writer that starts a clock.
                self.say(&format!("resync: {reason}"));
                self.load(*snapshot);
                Disposition::Control
            }
            ServerFrame::Event(env) => {
                // **R17: `seq` is dense, so a jump is proof of a miss.**
                //
                // The daemon assigns it and nobody else does; a head that assigns
                // unconditionally cannot tell a delivered row from a dropped one.
                // Guarded on the session so a `Switch` — whose events may arrive on
                // the same socket before the new `Hello` has been folded — cannot
                // read the other session's numbering as a gap in this one's.
                //
                // Nothing is done about a `self.seq` of 0: that is a head with no
                // mark yet, and the first thing it hears is not a gap.
                if self.seq > 0 && env.session_id == self.session_id && env.seq > self.seq + 1 {
                    let lost = env.seq - self.seq - 1;
                    self.gaps += 1;
                    // **Said in the conversation, once per gap, and the range is
                    // named.** Two gaps have to be two lines; deduping on the code
                    // would collapse a session's whole history of them into one.
                    self.note(Note::Warned(Warned {
                        code: "log_gap".into(),
                        detail: format!(
                            "{lost} event(s) never reached this head: seq {}..{} are \
                             missing, and the conversation you are reading has a hole in \
                             it. The daemon has them and this head does not — asking for a \
                             resync rebuilds from the snapshot, and `/status` counts how \
                             often this has happened.",
                            self.seq + 1,
                            env.seq - 1
                        ),
                        ts: 0,
                    }));
                    // **Repaired, and the repair is the daemon's to make.** A resync
                    // is the one thing that can put the two back in step, and the head
                    // cannot take a snapshot of a transcript it does not hold.
                    if !self.queued.iter().any(|a| matches!(a, Action::Resync)) {
                        self.queued.push(Action::Resync);
                    }
                }
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
            ServerFrame::Accepted { note, seq, .. } => {
                // **The daemon answering a STOP is the one acceptance that is not
                // routine.** R30: it is the only evidence that the request was *read* —
                // the frame being written says the bytes went to the kernel, and this says
                // a process on the other end understood them. Recorded rather than said:
                // the head is already drawing `stopping the daemon: the daemon answered`
                // from the state, and a notice on top of it would be the same fact twice.
                if note == letibot_sessionlog::NOTE_STOPPING
                    && let Some(s) = &mut self.stopping
                {
                    s.acked = true;
                    self.redraw = true;
                } else if note != letibot_sessionlog::protocol::NOTE_PROMPT_QUEUED {
                    // Telling the person who just pressed enter that their prompt was
                    // accepted is not information — and the old head left exactly that
                    // sitting on the input line for the rest of the session. Anything
                    // *other* than the routine acceptance still gets said.
                    self.say(&note);
                }
                // **The daemon saying where it is** (R17). This is the only route by
                // which a head learns it is *behind* rather than *at the end*: both
                // look the same from inside — an empty queue and a screen that has
                // drawn everything it was given — and on 2026-09-22 the difference
                // was 36 rows that were in the ledger and not on the screen.
                //
                // A `seq` below this head's own is a redelivery, not a gap; only a
                // greater one is a distance, and it is a lower bound, because the
                // daemon has moved on since it answered.
                self.behind = seq.saturating_sub(self.seq);
                Disposition::Control
            }
            ServerFrame::TermAttached { command } => {
                // **The daemon naming the pane this head attached to**, which is the half of the
                // attach the head cannot know: it sent `!term` with no command, and the head that
                // typed the original line may be another one or may have switched away. The
                // command is what the daemon was handed at `TermOpen` — with the verb stripped,
                // because that is how it received it — so the verb goes back on here, where the
                // line is a thing a person reads.
                //
                // **The bytes follow this frame**, so the pane is up and empty when this lands.
                // See `ClientFrame::TermOpen` for why the daemon sends the name first: a head
                // that drew the screen and learned what it was afterwards would flash a
                // rectangle it could not name.
                if let Some(p) = self.term.as_mut() {
                    p.line = format!("!term {command}");
                }
                // **And the fact the head draws when it is not drawing the pane.** An attach is
                // the daemon answering *what is running* in the same breath as handing over the
                // screen, so a head that detaches a moment later already knows what to say.
                self.term_fact = PaneFact::Running(command.clone());
                self.say(&format!(
                    "attached to `!term {command}` — the pane this session already has. \
                     ctrl-\\ leaves it running, `!term close` ends it."
                ));
                self.redraw = true;
                Disposition::Control
            }
            // **What this session's pane is running, or nothing** — the answer to a read, and
            // the fact a head draws **instead of a row** when it is not drawing the pane (see
            // [`App::pane_behind`] and [`PaneFact`]).
            ServerFrame::TermStatus { command } => {
                self.term_fact = match &command {
                    Some(command) => PaneFact::Running(command.clone()),
                    None => PaneFact::None,
                };
                self.redraw = true;
                // **A `!term close` that was waiting for this answer runs now**, through the same
                // decision it would have taken had the head known — see [`App::begin_close`]. The
                // line is held rather than guessed, which is the whole reason `Unasked` is a
                // state and not an `Option`.
                if self.close_pending {
                    self.close_pending = false;
                    if let Some(action) = self.begin_close() {
                        self.queued.push(action);
                    }
                }
                Disposition::Control
            }
            ServerFrame::TermOutput { bytes } => {
                // **A pane this head opened, fed its bytes.** Not an event and not counted as
                // one: `TermOutput` carries no seq, so it is `Control` for the same reason a
                // `Jobs` reply is — the ack's `rendered`/`filtered` are this head's disclosure
                // about *the batch*, and this frame is not in any batch.
                //
                // **A pane this head did NOT open is dropped, quietly.** The frames are fanned
                // out to every head of the session like events (one pane per session, see
                // `TerminalDriver`), and a second head attached to the same session has no
                // rectangle to draw them in. Dropping them is the honest reading of *a pane
                // this head did not open*, and the alternative — opening a pane from a frame
                // nobody asked for — would be a screen program appearing on a head that never
                // ran `!term`.
                //
                // **And no `redraw` flag**, which is the difference between a pane and a
                // transcript. `redraw` makes the driver call `Terminal::invalidate`, which
                // forgets the glass so the next frame is written whole — right for Ctrl-L, a
                // resize and a fold, and *wrong here*: a screen program repaints ten times a
                // second and the terminal's own diff writes exactly the rows that changed. A
                // flag per frame would pin the terminal rewriting all 24 rows ten times a
                // second, which is the flicker `term.rs`'s whole diff encoder exists to
                // remove. The frame is composed and drawn every tick either way — this flag
                // is about the *glass*, not about whether to draw.
                if let Some(p) = self.term.as_mut() {
                    p.screen.feed(&bytes);
                }
                Disposition::Control
            }
            ServerFrame::TermEnded { reason } => {
                // **The one place a pane closes, and the reason always comes from the daemon**
                // — *"the program exited with 3"*, *"you closed the terminal"*, *"a pane is
                // already open in this session"*. So a head never guesses why its rectangle
                // came back, and a refusal to start is the same frame as an ending.
                //
                // **And it becomes a row, not a notice.** This was `self.say(…)` — a sentence
                // on the chrome for `NOTICE_MS` — which is exactly why a program that dies at
                // once was *invisible*: the rectangle came back, one line appeared, and a few
                // seconds later the session said nothing about what had happened or what the
                // program had printed. The screen the program left is read here, while the
                // pane still holds it, and goes into the note with the daemon's sentence. See
                // [`Note::Pane`].
                //
                // **A pane this head had DETACHED from is the same arm, and that is the
                // point.** The pane is kept while the operator is away (see
                // [`TermPane::detached`]), the bytes keep arriving into its screen, and this
                // is where the row they would have seen had they been looking is filed — so
                // detaching does not hide a death. The only difference is that there is no
                // rectangle to give back, which `pane_open()` already accounts for.
                self.term_fact = PaneFact::None;
                if let Some(p) = self.term.take() {
                    self.file_note(Note::Pane {
                        line: p.line.clone(),
                        said: p.last_rows(),
                        closed: p.closing,
                        reason,
                    });
                    self.redraw = true;
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
                // **And this is the daemon naming a seq this head has not reached**
                // (R17) — the same fact `Accepted` carries, stated the other way round.
                // A refusal "for a stale `expected_seq`" is exactly what being behind
                // produces, so the head had better know the distance rather than
                // apologising for the operator's screen.
                self.behind = actual_seq.saturating_sub(self.seq);
                self.say(&format!(
                    "rejected: {reason} (you saw {expected_seq}, the session is at {actual_seq})"
                ));
                Disposition::Control
            }
            ServerFrame::Bye { reason } => {
                // Into the transcript, AND kept for after the terminal is restored.
                // A `Bye` is the last thing this head will draw, and the frame it is
                // drawn into is about to be torn down with the alternate screen — so
                // on the path that matters most, a version skew, the operator saw a
                // head vanish and nothing else. See `App::farewell`.
                self.say(&format!("daemon: {reason}"));
                self.bye = Some(reason.clone());
                self.quit = true;
                Disposition::Control
            }
        }
    }

    /// Replace all state from a snapshot. This is the late-join path and the
    /// resync path; they are the same path, which is why resync is not special.
    pub(crate) fn load(&mut self, s: Snapshot) {
        // **The same conversation, or a different one**, read once and at the top: the
        // session id is assigned a few lines down, and three things here ask the
        // question — what is carried over, and (R19) which of this head's own notes keep
        // the seam they were filed at.
        let same_session = self.session_id == s.session_id;
        // Everything session-scoped goes, not just the transcript. A snapshot is a
        // *replacement*, and this is also the switch path: carrying the previous
        // session's model name or a tool target keyed by a call id that only
        // existed over there is how a switched head shows the right conversation
        // with the wrong facts attached to it.
        if !same_session {
            self.call_targets.clear();
            self.call_ms.clear();
            self.call_edits.clear();
            self.usage = None;
            self.usage_cache_measured = true;
            self.last_timings = None;
            // The total belongs to the conversation, not to the head: switching
            // sessions must not carry one session's bill onto another's header.
            self.spent_micros = 0;
            self.spent_seen = false;
            self.model.clear();
            self.turn = None;
            self.heads = 0;
            // **The pane goes with the session it was opened in.**
            //
            // A screen belongs to the conversation it was drawn over: carried across a switch it
            // would be another session's program drawn in this one's rectangle, which is the
            // same lie a carried-over model name is. **The daemon's pane is not closed here**,
            // and it cannot be — a `TermClose` sent now would arrive *after* the `Switch`, on
            // the new session's hub, and kill the wrong thing. So the program is left running
            // for the session it belongs to, and it ends when the daemon stops or when a head in
            // that session leaves it. **And a head that switches back finds it again**: a bare
            // `!term` attaches to the pane this session has, and the daemon — which held the
            // screen all along — replays it. That is the half this rectangle's own TODO used to
            // say was missing (*"a head that switches back does not find its pane again, it
            // finds the transcript"*).
            self.term = None;
            // **And what this head believed about the pane is the session's, not this head's.**
            // A `Hello` re-asks (see that arm), and until the answer lands the fact is
            // `Unasked` — which is the state that makes `!term close` hold its line rather than
            // guess. Carrying the previous session's answer across a switch would be a
            // confirmation naming another session's program.
            self.term_fact = PaneFact::Unasked;
            self.term_ask = None;
            self.close_pending = false;
            // The subagent tree is the PARENT's fact. Carried across a switch it
            // put "1 subagent running" on the composer of the very subagent being
            // looked at (measured 2026-09-16), and Enter in the pane there would
            // have switched to itself.
            //
            // **So the rows are REPLACED, by this session's own children out of the
            // snapshot — not cleared, and not carried.** The snapshot's rows are the
            // parent's fact and nobody else's: the view they are cut from is per-session,
            // so a head that switches into `s-sub-1` is handed `s-sub-1`'s children and
            // not `s`'s, and the rule above is kept by construction rather than by a
            // clear. See `Snapshot::subagents` for the measurement that put them there.
            //
            // **This is the fix for a count that flapped.** The clear this replaces was
            // what turned a live child into a finished one on the way back: the rows were
            // gone, and the only thing left to rebuild them from was the daemon's session
            // list — whose `running` is *a turn is generating in that session at this
            // instant*, and `false` for a child parked on its own background job. So the
            // operator's `N subagents running` segment went away on a switch and came back
            // when some later list reply happened to catch the child generating, over a
            // subagent that ran throughout (*"so the counter is gone"* … *"yep and now it
            // is back. wtf"*). A switch sends `since_seq = 0`, so nothing is replayed and
            // the snapshot is the only route by which the head can be told what it watched
            // — see the `Subagent` arm in `letibot_sessionlog::view`.
            //
            // **What the clear was protecting is still protected.** A row can no longer
            // arrive from a session the head is not in: this is the snapshot's list, keyed
            // by the session the snapshot is of. And the fold below still refuses to emit a
            // row for a child whose brief belongs to somebody else.
            self.subagents = s
                .subagents
                .iter()
                .map(|v| SubagentState {
                    session_id: v.session_id.clone(),
                    state: v.state.clone(),
                    // **The event's own word for the instant**, read exactly as the live arm
                    // reads it: `running` is the one state it names in which a turn is
                    // generating. See [`SubagentState::generating`].
                    generating: v.state == "running",
                    prompt: v.prompt.clone(),
                    role: v.role.clone(),
                    task: v.task.clone(),
                    model: v.model.clone(),
                    answer: v.answer.clone(),
                    // The daemon's own stamp for when the event was published. The list's
                    // `created_ms` overrides it in the fold, the way it always did.
                    spawned_ms: v.ts,
                })
                .collect();
            self.subagents_sel = 0;
            // Jobs are the session's, the same way. The rows survived a switch
            // and kept drawing the old session's ids with the old session's byte
            // counts — and now that Enter on a row asks THIS session for that id,
            // a carried row is a question about a job that was never here.
            self.jobs.clear();
            self.jobs_sel = 0;
            // The queue is the old session's. Whatever was queued there stays
            // queued *there* — the hub drains it into that session's transcript —
            // but this head is no longer looking at that session, and an echo of
            // words belonging to a conversation that is no longer on the screen is
            // the same lie a carried-over model name is.
            self.pending_prompts.clear();
            self.unconfirmed.clear();
            // **And the viewport's place, which is a place in the rows that just went**
            // (R36). Carrying it across a switch would hold the reader on a row of another
            // conversation's transcript — the same lie a carried-over model name is.
            self.anchor = None;
        }
        self.session_id = s.session_id;
        self.seq = s.seq;
        self.dropped = self.dropped.max(s.dropped);
        // **And the subagent rows are rebuilt from the snapshot's own children, then
        // overlaid with the daemon's list.**
        //
        // Here, and not in the `Subagent` event arm, because this IS the late-join path
        // and the resync path at once (see the docstring above) — and a seed that ran
        // somewhere else would be a second way for the pane to be filled, which is how
        // the two come to disagree. Called for a same-session resync as well as a switch:
        // the fold is a rebuild from the current facts and is the same answer either way,
        // and a resync is exactly when a head's own list may be the stale one.
        //
        // The rows themselves were seeded by the `!same_session` block above, from
        // `Snapshot::subagents` — the parent's own view of its children, which is what a
        // switch can carry and a session list cannot. This call then folds the list over
        // them: the list's `running` is the one measurement of NOW either half has.
        //
        // `self.sessions` is already the fresh list by now — the `Hello` arm assigns it
        // before calling this — so the fold is reading the daemon's word and not the
        // previous session's.
        self.fold_subagents();
        // A resync of the *same* session keeps the queue — the hub's command
        // queue survives a resync, and a prompt queued behind a running turn is
        // still behind that turn — but anything the snapshot's transcript already
        // holds has landed, and its echo stands down the way `record_item` would
        // have stood it down had the row arrived live.
        for it in &s.items {
            if let Some(TranscriptItem::User { parts, .. }) = &it.item {
                for text in parts.iter().filter_map(|p| match p {
                    UserPart::Text { text } => Some(text.as_str()),
                    _ => None,
                }) {
                    self.retire_pending(text);
                }
            }
        }
        // **And what the snapshot could not resolve stops claiming `queued`** — R16's
        // third mark, and the claim the head can actually support.
        //
        // `pending_prompts` says *the daemon owes me a row for this*. A snapshot
        // **replaces** the transcript, so after one, an echo the snapshot does not carry
        // is either a row still coming or a row that a fork replaced — and from the head
        // those two are the same picture. Leaving it at `queued` asserts the first when
        // it might be the second; dropping it silently loses the operator's words. So it
        // is `unconfirmed`, and it retires normally when a row does land.
        //
        // **Where this is marked, and why here rather than at the fork.** A snapshot is
        // the only route by which the transcript is replaced — `reconnect`, `/resync`,
        // `Switch`, an import — so marking at the snapshot catches every one of them
        // instead of the two that happened to be thought of. `App::resolve_fork` handles
        // the fork the head *asked for*, where it knows the row will never arrive.
        //
        // An echo queued AFTER this point is untouched: it is added to
        // `pending_prompts` by a later `submit`, so it is not in the set being marked.
        for q in &self.pending_prompts {
            if !self.unconfirmed.iter().any(|u| u == q) {
                self.unconfirmed.push(q.clone());
            }
        }
        // The snapshot's in-flight calls are **not** seeded into `call_targets`.
        // They reach the screen as `TurnPane::calls`, which carries each call's own
        // target on the row that is about to draw it; putting them in an id-keyed
        // table as well is how a live `call_0` came to relabel a settled one.
        if let Some(TurnState::Finished { usage, timings, .. }) = s.turn.as_ref().map(|t| &t.state)
        {
            // A turn that finished measured its own cache, so the percentage is
            // real even though the row's copy may not have been.
            self.usage = Some(*usage);
            self.usage_cache_measured = true;
            self.last_timings = Some(*timings);
        }
        self.items = s.items;
        // A snapshot replaces the rows, so everything derived from them — the `!`
        // candidates and the model's suggestions — is stale and goes with them.
        self.the_rows_moved();
        // **And the fill's bar goes with the stream that carried it.**
        //
        // A `Filling` tick rides the event stream and its ONLY exit is a tick whose `done` has
        // reached `total` — so a stream that stops carrying ticks leaves the bar standing for
        // the rest of the session. That is not hypothetical: a republish of 2000 rows overruns
        // a head's 1024-event queue, the hub **demotes** the head rather than blocking
        // (`Inner::append_and_fan`), and from that moment every tick is skipped — the final one
        // included, because a demoted head is not written to at all. The head is handed a
        // snapshot instead, and this line is the head taking it: **the view it was watching is
        // gone, so the progress it was reporting belongs to a stream that no longer exists.**
        // Measured on the operator's own head, 2026-10-04: it stood at `897 of 2000 rows —
        // restoring the stored conversation` and did not move.
        //
        // **A fill that is genuinely still running is not lost by this.** Its next tick re-arms
        // the line, and ticks come one per 64 rows — so clearing here can cost one tick of a
        // bar that is still going, and buys the end of one that never will. The bar must end by
        // FACT rather than by a clock (that is why `republish_after` publishes its completion
        // unconditionally), and the fact here is that the head was just told its queue was
        // thrown away.
        self.filling = None;
        // **A snapshot records the bulk announcement; a live append never does.**
        //
        // The rows a snapshot carries without bodies are a *carry* — a fork, a reseat, a
        // resume, an import, or an attach to a daemon mid-carry. The rows a live
        // `TranscriptAppended` adds are the R2 window of an ordinary message, and putting
        // them here is exactly the defect this replaces: a trigger built on "some row
        // lacks a body" fires on every healthy turn.
        self.bulk = {
            let ids: std::collections::HashSet<String> = self
                .items
                .iter()
                .filter(|i| i.item.is_none())
                .map(|i| i.item_id.clone())
                .collect();
            (!ids.is_empty()).then(|| Bulk {
                ids,
                at_ms: self.now_ms,
            })
        };
        // **A snapshot replaced every row, so the bindings are pruned to what is
        // still there and still body-less.** Pruned rather than cleared: a resync
        // mid-prompt is exactly when the reply is racing the prompt, and dropping
        // the binding for one frame would put the echo back at the tail and take it
        // away again on the next `TranscriptContent`. An id that is gone, or whose
        // row now has its body, has nothing left for a binding to stand for — the
        // row renders from its content, and the echo at the tail is the echo's own
        // business again.
        {
            let bodyless: std::collections::HashSet<&str> = self
                .items
                .iter()
                .filter(|it| it.item.is_none())
                .map(|it| it.item_id.as_str())
                .collect();
            self.bound_prompts
                .retain(|id, _| bodyless.contains(id.as_str()));
        }
        // The snapshot's turn carries its calls **with their edit excerpts**, and
        // the rows it appended in order — the same two facts the live hand-off
        // used when it moved a card's excerpt into `call_edits` as the row landed.
        // Only the live path filled that map, so a restarted head drew every
        // landed edit panel-less even though the wire had just handed it the
        // excerpt (operator, 2026-09-17: past edits lose their diff panels on
        // restart). Seed it the same way the live arm does: positionally, the
        // Nth tool_result row is the Nth call. Rows from turns before this one
        // are not on the wire — the view keeps one turn's calls — and render as
        // they always did.
        if let Some(t) = &s.turn {
            let kinds: std::collections::HashMap<&str, &str> = self
                .items
                .iter()
                .map(|it| (it.item_id.as_str(), it.kind.as_str()))
                .collect();
            let mut call_idx = 0usize;
            for item_id in &t.appended {
                if kinds.get(item_id.as_str()).copied() == Some("tool_result") {
                    if let Some(CallState::Finished { edit: Some(e), .. }) =
                        t.calls.get(call_idx).map(|c| &c.state)
                    {
                        self.call_edits.insert(item_id.clone(), e.clone());
                    }
                    call_idx += 1;
                }
            }
        }
        self.invalidate_history();
        self.open = s.open_decisions;
        // A snapshot can replace the open set wholesale; keep the highlight in range.
        self.sel = 0;
        // A permission settles on the call it gated, so it rides the call's card
        // rather than the note list; a question — or a log recorded before the field
        // existed — has no call to ride and stays a note. The live arm makes the same
        // split, and the two have to agree.
        let (call_bound, notes_bound): (Vec<SettledDecision>, Vec<SettledDecision>) = s
            .settled_decisions
            .into_iter()
            .partition(|d| d.call_id.is_some());
        // **A snapshot's notes are HISTORY, and this head's own are not** (R19).
        //
        // Until this, everything the snapshot carried was planted at anchor 0 —
        // *everything in a snapshot is history and none of it is anchored* — which is
        // right about where it goes and wrong about what it is: a fresh head showed
        // nothing, so replaying hours of announcements as though they had just happened
        // put them above a conversation they did not precede. The operator restarted a
        // head and was met by twelve red lines: *"i dont want to see that on restart."*
        //
        // So the two kinds are sorted rather than merged. This head's own notes keep
        // their seam — it filed them while watching, at rows of this very conversation —
        // and what the snapshot adds is [`Placed::Before`]: listed by `/notes`, counted
        // by `/status`, and not drawn, because a head that has just attached has shown
        // nothing and the log is where these facts live.
        //
        // **A seam the new transcript no longer has is not a seam.** A compaction or a
        // reseat forks the conversation, so a note filed at row 200 of a 250-row
        // transcript is no longer between any two rows of this one; it joins the history
        // rather than being drawn at a place that has stopped existing.
        let mut mine: Vec<(Placed, Note)> = if same_session {
            std::mem::take(&mut self.notes)
                .into_iter()
                .map(|(place, note)| match place {
                    Placed::Seam(at) if at <= self.items.len() => (Placed::Seam(at), note),
                    _ => (Placed::Before, note),
                })
                .collect()
        } else {
            // A different conversation: these are that session's notes about rows this
            // head no longer holds, and the same rule that clears `call_targets` clears
            // them.
            Vec::new()
        };
        let mut before: Vec<(Placed, Note)> = Vec::new();
        for w in s.warnings {
            // Same rule as the live arm: `turn_failed` is the log's record of what
            // the turn's own terminal state already says on the screen. Filtering
            // it here as well is what stops a *snapshot* from putting it back —
            // which is exactly what happened the first time, and is the reason the
            // live path and the snapshot path have to agree about every filter.
            if w.code == "turn_failed" {
                continue;
            }
            let n = Note::Warned(w);
            if holds(&mine, &n) {
                continue;
            }
            before.push((Placed::Before, n));
        }
        for d in notes_bound {
            let n = Note::Decided(d);
            if holds(&mine, &n) {
                continue;
            }
            before.push((Placed::Before, n));
        }
        // Oldest first: the facts from before this window are older than anything this
        // head filed, and `/notes` numbers them in the order a reader reads.
        before.append(&mut mine);
        self.notes = before;
        self.note_upto = 0;
        // The settled rows this snapshot carries are history, and a decision that
        // gated one of them has to ride that row rather than vanish with the live
        // card. The snapshot's decisions are keyed by call id, which is
        // round-positional, so the match is best-effort: walking the rows newest
        // first, each decision goes to the most recent tool_result row that carries
        // its id, and a decision is spent on the first row it matches. Rows older
        // than the snapshot's decision window render without the approval — the same
        // `Replayed` rule the duration and the edit pair follow.
        {
            let mut by_call: std::collections::HashMap<String, SettledDecision> =
                std::collections::HashMap::new();
            for d in &call_bound {
                if let Some(cid) = &d.call_id {
                    by_call.insert(cid.clone(), d.clone());
                }
            }
            for it in self.items.iter().rev() {
                if let Some(TranscriptItem::ToolResult { call_id, .. }) = &it.item {
                    if let Some(d) = by_call.remove(call_id) {
                        self.call_decisions.insert(it.item_id.clone(), d);
                    }
                }
            }
        }
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
                        // The same rule one field along: a snapshot carries no
                        // timestamps, so there is no anchor to measure against
                        // either.
                        started_at: 0,
                        ended_ms: 0,
                        note: None,
                        decision: None,
                    })
                    .collect(),
                progress: t.progress,
                state: Some(t.state),
                // Which rows this turn produced. Without it a head that joined late
                // cannot tell that the transcript already holds the answer, and
                // renders it twice — measured on a second head attached to a
                // finished turn, where the whole reply appeared above itself.
                appended: t.appended,
                // **`turn_rows` is deliberately NOT here, because it is not on the wire.** A head
                // that attaches mid-turn gets the current round's `appended` from the snapshot and
                // starts with no history of the turn's earlier rounds, so for the window between
                // attaching and the next `TurnStarted` it can draw the duplicate this field exists
                // to prevent. Said rather than hidden: closing it needs a daemon field, and the
                // window is one attach rather than every round.
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
            // A permission settles on the call it gated, so the snapshot attaches
            // it to the call's card the way the live arm does. A decision whose call
            // is not in this turn — a log recorded before the field existed, or a
            // call the snapshot did not carry — has nowhere to ride and is dropped
            // here rather than rendered twice.
            for d in call_bound {
                let cid = d.call_id.clone().unwrap_or_default();
                if let Some(c) = pane.calls.iter_mut().find(|c| c.call_id == cid) {
                    c.decision = Some(d);
                }
            }
            pane
        });
        self.scroll = 0;
        self.redraw = true;
    }

    pub(crate) fn event(&mut self, e: SessionEvent, ts: u64) -> Disposition {
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
                // **AND A WAITING SEED RUNS HERE** — the board has just been read whole, which is
                // the only moment this head may add rows to its own half without risking a wipe:
                // `SetOperatorTodos` REPLACES the operator half, so seeding against a stale list
                // would take rows off the board rather than add to it.
                if self.todo_seed_pending {
                    self.todo_seed_pending = false;
                    self.seed_todos();
                }
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
                task,
                model,
                answer,
            } => {
                // **The event's own word for the instant**: it publishes `running` when the
                // child's harness comes up and `opening` before that, so `running` is the one
                // state it names in which a turn is generating. See
                // [`SubagentState::generating`].
                let generating = state == "running";
                if let Some(row) = self
                    .subagents
                    .iter_mut()
                    .find(|s| s.session_id == subagent_id)
                {
                    row.state = state;
                    row.generating = generating;
                    row.prompt = prompt;
                    row.role = role;
                    row.task = task;
                    row.model = model;
                    row.answer = answer;
                } else {
                    self.subagents.push(SubagentState {
                        session_id: subagent_id,
                        state,
                        generating,
                        prompt,
                        role,
                        task,
                        model,
                        answer,
                        // When this head heard of it, which is the best a spawn event can say.
                        // `fold_subagents` replaces it with the daemon's own `created_ms` the
                        // moment a list carrying the child arrives.
                        spawned_ms: ts,
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
            // **The daemon has admitted a call the operator is running themselves** (R24 part
            // two, decision 4). Nothing is filed as a note: the admission is the daemon's
            // record, and the call's visible half is the result row, which arrives with its
            // `origin` set. A head that narrated the admission would be reading a keystroke the
            // operator just made back to them.
            //
            // **Counted, not ignored.** A head that runs operator calls wants to know one was
            // admitted; a head that does not — this one, until the chord lands — must not
            // swallow it silently, or *"the event never came"* and *"this head drops it"* look
            // the same, and the second is the one a reader would never find.
            //
            // **`Filtered` and NOT `Control`, and the difference is the whole of R53 §1.2.** The
            // paragraph above asks for "counted, not ignored" and the code answered `Control`,
            // whose own definition is *"Not an event: a Hello, a Resync, a command reply"* — so the
            // arm contradicted the variant's documentation and its own comment at the same time.
            // `Filtered` is the counted one and says so. MEASURED consequence before the fix: the
            // `filtered` figure on `Ack` (and on `/status`) differed by one per occurrence between
            // the two heads, and letibot's stated protection against *"the event never came"*
            // reading exactly like *"this head drops it"* was not in force on the one event whose
            // comment argues for it. **No test caught it because nothing asserts the count for
            // this event, and the comment reads as the specification** — a reader checking the
            // file found an argument for the correct behaviour sitting on top of the incorrect
            // one, which is the same failure mode as a docstring describing colours over a
            // function that returns a plain string.
            SessionEvent::OperatorCallAllowed { .. } => Disposition::Filtered,
            SessionEvent::JobSettled {
                job,
                state,
                produced,
                elapsed_ms,
            } => {
                // **Folded, never invented.** The daemon owns the table; a
                // settlement for a job this head has not been told about is not a
                // row to make up, it is a row that arrives with the next
                // `ListJobs`. Inventing one is how the pane used to show a job
                // with no command and no idea how it got there.
                if let Some(row) = self.jobs.iter_mut().find(|j| j.id == job) {
                    row.state = state;
                    row.running = false;
                    row.produced = produced;
                    row.elapsed_ms = elapsed_ms;
                    // `never_ran` is deliberately **not** taken from this event: the
                    // settlement carries no such fact, and it cannot be stale here — a
                    // job that never ran never started, so it was never listed as a
                    // running one, and the `never_ran` the row already holds came from
                    // the daemon's own listing (`JobEntry`). A job that ran is never
                    // settled as one that did not.
                }
                // **And the count drops by one**, so the row above the composer re-asks (R51 item
                // 5). The fold just above is the pane's copy: a job settled in a turn this head
                // never watched has no row to fold into, which is exactly the case a count taken
                // from the pane's rows would get wrong.
                self.queued.push(Action::ListJobs);
                self.redraw = true;
                Disposition::Filtered
            }
            // **A job's output, the answer to the jobs pane's Enter.** Not folded
            // into any view: the pane that asked draws the window, and only that
            // pane has anywhere to put it. It is ephemeral besides
            // (`scrub::is_interactive`), so no late head replays one.
            SessionEvent::JobOutput {
                job,
                from,
                to,
                produced,
                dropped,
                state,
                never_ran,
                lines,
                next,
            } => {
                // Taken only when a window is open for *this* job: a head here may
                // have closed the pane with Esc before the reply landed, and a
                // window for a job nobody is looking at is nothing to keep.
                if let Some(v) = self.job_out.as_mut()
                    && v.job == job
                {
                    v.state = state;
                    v.never_ran = never_ran;
                    v.from = from;
                    v.to = to;
                    v.produced = produced;
                    v.dropped = dropped;
                    v.lines = lines;
                    v.next = next;
                    v.loading = false;
                    v.error = None;
                    // A window lands at its **tail**: a fresh page, or a re-read of
                    // a running job, should show what it just wrote. `back` is left
                    // alone, so ← still walks the pages the reader came through.
                    v.scroll = 0;
                    self.redraw = true;
                    return Disposition::Rendered;
                }
                Disposition::Filtered
            }
            // **A fill the daemon NAMED (R6).** `what` is the operation in the daemon's
            // own words and `done`/`total` are its own count in the `unit` it named — so
            // the head draws the fact instead of inferring a cause from a symptom.
            // Counting the rows still lacking a body would draw *"N rows announced and
            // never filled in"* three seconds into a healthy generation (see
            // `BODY_PATIENCE`), and — worse — would name the wrong operation: an ordinary
            // reply is not a carry.
            //
            // Ephemeral (`scrub::is_interactive`), so a late head never replays a tick.
            // On the last one the line goes: the daemon's durable finish note is what
            // says the operation ended, and a bar left at `total of total` would sit on
            // the screen for ever.
            SessionEvent::Filling {
                what,
                unit,
                done,
                total,
            } => {
                self.filling = (done < total).then_some((what, unit, done, total));
                self.redraw = true;
                Disposition::Rendered
            }
            SessionEvent::CompactionProgress {
                half,
                halves,
                prompt_tokens,
                processed,
                written,
                unit,
            } => {
                // **Stored in its own field, which is the whole point of the event.** This
                // must not touch `self.turn.progress`: that slot is the SESSION's turn, and
                // a scratch summary's numbers landing there is exactly the mislabel the
                // old suppression traded away the progress line to avoid. Nothing else
                // writes `self.compacting`, so the two cannot be confused.
                self.compacting = Some(CompactionLine {
                    half,
                    halves,
                    prompt_tokens,
                    processed,
                    written,
                    unit,
                });
                self.redraw = true;
                Disposition::Rendered
            }
            SessionEvent::TurnStarted {
                turn_id,
                model,
                ledger_head: _,
                // **The WHOLE turn's start, when the emitter knows it.** This event fires once
                // per ROUND, so `started_ms: ts` below restarted the header's clock at every
                // round — `2.1s` a minute into a turn. `began_ms` is the prompt's own stamp and
                // is used when present; `ts` stays as the fallback for a turn nobody measured
                // (a snapshot, or a test that emits the event by hand).
                began_ms,
            } => {
                // Asked *before* the pane is replaced. The rows that stop being
                // drawn live are the previous turn's, and once its pane is gone
                // there is nothing left to ask which they were.
                let stale_from = self.turn_first_row();
                let previous = self.turn.as_ref();
                // **A boundary the head has ALREADY HAD, taken again.** The frame is an event like
                // any other and may be delivered twice — and rebuilding the pane here is not
                // idempotent: the round's own stream (its text, its reasoning, its proposed calls)
                // is state the event does not describe and cannot restore, so a second copy of one
                // boundary would throw the round's work away and re-open a run that is already
                // open.
                //
                // `turn_id` is the key that can say so, and it is a sound one: `run_turn_steered`
                // mints it as `{transcript_id}#{turn_seq}` with `turn_seq += 1` per round, and a
                // restored session starts that counter at the number of restored items precisely
                // so that *"within one transcript the ids never repeat"* (`crates/turn/src/
                // resume.rs`, "`turn_seq` is a watermark, not a turn count"). So an equal
                // non-empty `turn_id` is one round, and this event has already been folded in.
                if !turn_id.is_empty() && previous.is_some_and(|t| t.turn_id == turn_id) {
                    return Disposition::Filtered;
                }
                self.model = model.clone();
                // Stamped, so the header can prefer this over a settings row it read
                // earlier at attach. See `model_from_turn_at`.
                self.model_from_turn_at = self.seq;
                // **Which boundary is this?** — and only one of the answers is *a new run*.
                //
                // [`TurnPane::turn_rows`] hangs the run on `began_ms`, because it is stamped once
                // per prompt and carried by every round of it: the one thing on the wire that
                // distinguishes *the next round of this prompt* from *a new prompt*. What the head
                // then does with it is the defect this closes, and the mistake is asking the wrong
                // question — *does this number match* rather than *may I start a new run here*.
                //
                // A boundary the head cannot MATCH is not a boundary it can tell APART. `began_ms:
                // None` means *nobody measured this one*, and two very different events carry it: a
                // turn that began before this head attached, and **a turn the daemon ran without
                // stamping its clock at all.** The second is real and reachable —
                // `harnessd::sessions::run_prompt` calls `begin_turn_clock` and
                // `Sessions::wake` does not, so the turn a job's settlement or a monitor's firing
                // opens arrives with no start in it (`harnessd/src/sessions.rs`: `run_prompt` vs
                // `wake`).
                //
                // Read as a new run, such a boundary emptied `turn_rows` and moved `started_ms` to
                // the event's own `ts`: the run the work is still in stopped reading as this turn's,
                // so `live_here` and `walk_carried_live` both answered no, the pane drew its OWN
                // marker beside the walk's, and the `Responding` clock restarted. The operator's
                // report, in their words: *"it looks like turn end or some other border is
                // misinterpreted and Responding timer resets and I get new line with `[N thinking
                // lines]` which then gets merged to the previous `[N tools, M thinking]`"* — and the
                // merge is the next row landing, which puts a row of the turn back into
                // `turn_rows`.
                //
                // **So an unmeasured boundary over a turn that is still RUNNING is the same run.**
                // The head is drawing that turn right now; nothing the reader can see has separated
                // the work before the event from the work after it; and the row filter, not this
                // event, is what actually ends a run on the screen ([`row_drawn`]) — a new prompt
                // is preceded by the operator's own message, which is a drawn row and ends the run
                // by itself. A head that says *new run* here is inventing a border out of an event
                // that carries no evidence for one, which is exactly the second `Responding` the
                // operator watched appear.
                let same_turn = match began_ms {
                    Some(b) => previous.is_some_and(|t| t.started_ms == b),
                    None => previous.is_some_and(|t| matches!(t.state, Some(TurnState::Running))),
                };
                // **The clock is never restarted by an event that carries no clock.** Same rule, on
                // the one field the operator watches move: a boundary with no `began_ms` leaves the
                // base where it was, so the row keeps counting from the prompt that is actually
                // running rather than from the frame that arrived. `ts` stays the base for a turn
                // nobody measured at all — the first `TurnStarted` of a head that joined late.
                let started = match (began_ms, same_turn) {
                    (Some(b), _) => b,
                    (None, true) => previous.map(|t| t.started_ms).unwrap_or(ts),
                    (None, false) => ts,
                };
                let turn_rows = if same_turn {
                    previous.map(|t| t.turn_rows.clone()).unwrap_or_default()
                } else {
                    Vec::new()
                };
                self.turn = Some(TurnPane {
                    turn_id,
                    model,
                    state: Some(TurnState::Running),
                    started_ms: started,
                    last_ms: started,
                    turn_rows,
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
            SessionEvent::TokensGenerated { turn_id, tokens } => {
                let Some(t) = self.turn.as_mut() else {
                    return Disposition::Filtered;
                };
                if t.turn_id != turn_id {
                    return Disposition::Filtered;
                }
                // **The counter it used to move is no longer drawn** — see `turn_status`: the
                // operator does not want the number to read. The event still counts as
                // `Rendered`, because it is a frame in which the turn is plainly alive: the
                // spinner beside the clock moves on it, and that is the row's whole business
                // now. It also counts as liveness for the stuck line, the same way a delta does.
                let _ = tokens;
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
                    if std::env::var("LETIBOT_MARKER_DEBUG").is_ok() {
                        eprintln!(
                            "MARKER delta REFUSED: pane={:?} delta={:?} target={:?} bytes={}",
                            t.turn_id,
                            turn_id,
                            target,
                            text.len()
                        );
                    }
                    return Disposition::Filtered;
                }
                match target {
                    DeltaTarget::Text => {
                        // **§3.1: model prose is content this head did not author**, and
                        // this is where it enters — every streamed chunk that becomes the
                        // answer goes through here and ends up as a row `paint_full`
                        // writes verbatim. A control byte the model emits (or copies out
                        // of a file it read) is an instruction to the operator's
                        // terminal: `ESC ] 0 ; … BEL` sets the window title, `ESC [ 2 J`
                        // clears the screen.
                        //
                        // Sanitised here rather than at the renderer because
                        // `IncrementalMarkdown` is a frozen-prefix lexer (§13.3): a
                        // control byte that reaches it is frozen into the stable half
                        // and cannot be removed later without re-lexing. The trade is
                        // `without_control`'s own — one space per control byte, so the
                        // character count `arrived_chars` keeps is unchanged.
                        let text = without_control_lines(&text);
                        t.text.push(&text);
                        Disposition::Rendered
                    }
                    DeltaTarget::Reasoning => {
                        // **Accumulated at every rung, drawn only where the rung shows it.**
                        //
                        // The guard used to be on the accumulation, which is the same mistake
                        // R37's marker made one layer up: `conversation` *hides* the working and
                        // does not *discard* it — the rung is a view — so the text has to be
                        // here to be counted, to be drawn when the reader opens the run, and to
                        // be there the moment they change rung. Found by the marker's own test:
                        // at `conversation` nothing accumulated, so the streamed thinking the
                        // operator was watching could not be counted.
                        if t.think_started_ms == 0 {
                            t.think_started_ms = ts;
                        }
                        t.think_last_ms = ts;
                        // The same rule as the answer above, for the same reason: reasoning is
                        // the model's own text and it reaches the glass through the markdown
                        // renderer. **And it counts**, for the reason the field gives: on a
                        // `messages` backend this is the channel that streams from the first
                        // second, and it was the one the row's number could not see.
                        let text = without_control_lines(&text);
                        t.reasoning.push(&text);
                        // **The `thinking` switch, not a rung comparison** — it is the same
                        // question (`>= Normal` meant *the reasoning rows are drawn*, and at the
                        // bottom rung they are not) asked of the set, so a set that hides the
                        // reasoning cannot count it as rendered.
                        if self.visibility.shows(Show::Thinking) {
                            Disposition::Rendered
                        } else {
                            Disposition::Filtered
                        }
                    }
                    // Never `t.text`. This is the markup, and the whole point of
                    // the channel is that the default view does not show it — see
                    // `letibot_sessionlog::event::DeltaTarget`. It *is* reachable
                    // through `ctrl-x`, so it is sanitised where it is drawn
                    // (`raw_call_lines`) rather than here.
                    DeltaTarget::ToolCall => {
                        // **The other channel that arrives and that the row could not see**, and
                        // the one an agentic round is *made of*: a call's arguments come as deltas,
                        // and on the backend that produced the measurement above the row sat at a
                        // frozen number while thousands of tokens of exactly this went by.
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
                        started_at: self.now_ms,
                        ended_ms: 0,
                        note: None,
                        decision: None,
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
                            // **And the head's anchor moves with it** (R13): the two
                            // clocks are the same clock, so a row anchored at the
                            // *proposal* would count the decision wait as running
                            // time — the exact sentence the line above exists to
                            // avoid, arrived at from the other side.
                            c.started_at = self.now_ms;
                            // **And the note from before it started is over.**
                            //
                            // `ToolProgress` notes are facts about right now, and
                            // the ones a call collects while it is `Proposed` are
                            // about the DECISION — "asking the guard". Only
                            // `ToolFinished` cleared the field, so that sentence
                            // rode the card through the whole run.
                            //
                            // Measured 2026-09-20 in the rano session: `cargo test
                            // stream_tests` ran for its full 300 s deadline with
                            // "asking the guard" underneath it the entire time. The
                            // operator read the screen exactly as it was written and
                            // reported the session hung requesting the oracle — the
                            // guard had answered in milliseconds and the diagnosis
                            // cost an hour. A stale note is worse than no note: it
                            // is a measurement of a moment that has passed, with
                            // nothing on it to say so.
                            //
                            // Cleared rather than replaced: the tool is running and
                            // the phase already says so. A note appears again when
                            // the tool sends one of its own.
                            c.note = None;
                        }
                        None => t.calls.push(CallRow {
                            call_id,
                            name,
                            // `ToolStarted` carries no target and none is invented.
                            target: String::new(),
                            state: CallState::Running,
                            started_ms: ts,
                            // **The clock starts with the same event the log's
                            // does** (R13): a call that waited on a decision did
                            // not spend that time running, and the head's clock is
                            // read here rather than at render so that a replay —
                            // which has no clock yet — falls back to the log's.
                            started_at: self.now_ms,
                            ended_ms: 0,
                            note: None,
                            decision: None,
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
                // A backgrounded finish used to push a row here, built from the
                // call this head happened to be holding. The daemon publishes its
                // table every round now, so the row arrives with the next
                // `ListJobs` — with a command that is right whichever turn it is
                // read in.
                // **A backgrounded call is the job STARTING, and the only event that says so**
                // (R51 item 5). There is no `JobStarted` on the wire: `JobSettled` is the whole
                // job vocabulary, so the count cannot be folded out of events — it has to be
                // ASKED for. The proof is the outcome rather than the tool's name: only a call
                // that was actually backgrounded carries a handle.
                //
                // Without this the row a reader sees is a **souvenir**: a count drawn from a list
                // only `/jobs` ever fetched sits at zero for the life of the job.
                //
                // Asked BEFORE the outcome is moved into the call's own state, because this is the
                // last read of it in this arm.
                if matches!(
                    outcome,
                    letibot_transcript::ToolOutcome::Backgrounded { .. }
                ) {
                    self.queued.push(Action::ListJobs);
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
                access,
                summary,
                target,
                write_targets,
                detail,
                options,
                choices,
                because,
                advice,
                deadline,
                on_timeout,
                subagent,
                ..
            } => {
                self.open.retain(|d| d.req_id != req_id);
                // A fresh question starts at the top of its ladder rather than
                // wherever the last one was left: the highlight must never be
                // somewhere the operator did not put it when Enter is one key away.
                self.sel = 0;
                self.open.push(OpenDecision {
                    req_id,
                    write_targets,
                    kind,
                    call_id,
                    access,
                    summary,
                    target,
                    detail,
                    options,
                    choices,
                    because,
                    advice,
                    deadline,
                    on_timeout,
                    subagent,
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
                // Three things are read off the open decision **before** it is
                // removed, because the answer event carries only the `req_id`: the
                // summary, the call to put the outcome on, and the oracle's advice.
                // The last is the one the answer event can never carry — its `basis`
                // is the DECIDER's, and under `/supervise` the decider is usually the
                // operator.
                let (summary, call_id, advice) = self
                    .open
                    .iter()
                    .find(|d| d.req_id == req_id)
                    .map(|d| (d.summary.clone(), d.call_id.clone(), d.advice.clone()))
                    .unwrap_or_default();
                self.open.retain(|d| d.req_id != req_id);
                let d = SettledDecision {
                    req_id,
                    call_id: call_id.clone(),
                    summary,
                    outcome,
                    by,
                    basis,
                    advice,
                    late,
                };
                // A permission settles on the call it gated: the approval is a fact
                // about the call, so it rides the call's card in the dim register
                // rather than as a standalone note. A question — or a log recorded
                // before the field existed — has no call to ride, and stays a note.
                if let Some(call_id) = &call_id
                    && let Some(t) = self.turn.as_mut()
                    && let Some(c) = t.calls.iter_mut().find(|c| &c.call_id == call_id)
                {
                    c.decision = Some(d);
                } else {
                    self.note(Note::Decided(d));
                }
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
                // no longer does. The turn measured its own cache, so the
                // percentage is real.
                self.usage = Some(usage);
                // Summed as the turns land. A turn with no cost — the local
                // server, or a metered model nothing prices — adds nothing and
                // does not light the meter: free and unpriced are both "no
                // number", and `$0.0000` on every local header would be noise.
                if let Some(c) = usage.cost_micros_usd {
                    self.spent_micros += c;
                    self.spent_seen = true;
                }
                self.usage_cache_measured = true;
                self.last_timings = Some(timings);
                // **And the count is re-read at the turn's end** (R51 item 5's third moment). A job
                // that was already running when this head attached is announced by no event at all:
                // the `ToolFinished` that started it happened before the attach, and a `JobSettled`
                // may be an hour away. The turn boundary is the moment the head knows it has been
                // through a round without hearing about it, so it is where the count gets its chance
                // to be right.
                self.queued.push(Action::ListJobs);
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
                // repeat every round so there is nothing to match on. The duration,
                // the edit pair and the decision are carried across here because
                // they are facts the live card had that the row does not.
                let mut carried: Option<u64> = None;
                let mut carried_edit: Option<letibot_sessionlog::event::ToolEdit> = None;
                let mut carried_decision: Option<letibot_sessionlog::view::SettledDecision> = None;
                if let Some(t) = self.turn.as_mut() {
                    t.appended.push(item_id.clone());
                    t.turn_rows.push(item_id.clone());
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
                        // The approval rides across too: the decision is a fact
                        // about this call, and the row that outlives the card is
                        // where it has to keep being shown.
                        carried_decision = c.and_then(|c| c.decision.clone());
                        t.settled_calls += 1;
                    }
                }
                if let Some(ms) = carried {
                    self.call_ms.insert(item_id.clone(), ms);
                }
                if let Some(e) = carried_edit {
                    self.call_edits.insert(item_id.clone(), e);
                }
                if let Some(d) = carried_decision {
                    self.call_decisions.insert(item_id.clone(), d);
                }
                // **A user row is drawn from the moment it is announced.** The body
                // follows on its own channel and, behind a running turn, the reply
                // streams in the meantime — so without this the prompt is invisible
                // while the answer to it is already on the screen, and the echo
                // underneath goes on saying `queued` about words that have landed.
                // See `App::bound_prompts` for why this is a guess and what keeps it
                // honest.
                if kind == "user" {
                    self.bind_echo(&item_id);
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
                // A row landed, so the transcript moved and everything derived from it
                // — the `!` candidates and the model's suggestions — is stale. The next
                // Tab for the same prefix is a fresh ask.
                self.the_rows_moved();
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
                // **The `system` switch**: *"who attached, and who issued which command"* —
                // the same three events `>= Verbosity::Loud` gated, and now the switch the
                // three profiles name, so `loud` is the set that turns them on.
                if self.visibility.shows(Show::System) {
                    Disposition::Rendered
                } else {
                    Disposition::Filtered
                }
            }
            SessionEvent::HeadDetached { .. } => {
                self.heads = self.heads.saturating_sub(1);
                if self.visibility.shows(Show::System) {
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
                // **And `Filtered` here too, for the same reason R53 gives one paragraph over.**
                // This is a `SessionEvent` — session content, read, and deliberately not drawn as
                // a row of its own, because the answer IS the rows this head draws. `Control`'s
                // definition is *"Not an event"*, and its own docstring above says the same thing
                // about this arm that `OperatorCallAllowed`'s says about its: a reader would never
                // find the difference. The two are changed together because they are one defect
                // spelled at two arms.
                Disposition::Filtered
            }
            SessionEvent::SecretRequested {
                req_id,
                prompt,
                command,
                deadline,
            } => {
                if command.is_empty() {
                    self.key_secrets.push(req_id.clone());
                }
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
                // A key card is a secret request with no command (`Harness::obtain_key`),
                // remembered by id because the card itself may already be gone.
                let was_key = match self.key_secrets.iter().position(|r| *r == req_id) {
                    Some(i) => {
                        self.key_secrets.remove(i);
                        true
                    }
                    None => false,
                };
                if self.secret.as_ref().is_some_and(|s| s.req_id == req_id) {
                    self.secret = None;
                    self.secret_buf.clear();
                }
                if was_key {
                    // Given: the daemon says where it was saved (`provider_key_saved`), so
                    // one line rather than two. Not given: the refusal is the operator's.
                    if !given {
                        self.note(Note::Warned(Warned {
                            code: "provider_key_refused".into(),
                            detail: format!("no key given ({by})"),
                            ts,
                        }));
                    }
                    return Disposition::Rendered;
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
            // **A command of the operator's own is waiting for an answer.** The card is up
            // for as long as the run is blocked, and the keyboard belongs to it while it
            // is — see the `Key` arm and [`App::prompt_lines`].
            //
            // **The text is NOT masked**, and that is the difference from the arm above
            // rather than an oversight: what this card carries is a line for a program's
            // stdin, drawn in the open. A password has its own path and its own card, and
            // the two must not be one.
            SessionEvent::PromptRequested {
                req_id,
                job,
                command,
                question,
            } => {
                // **A question from a RUNNING PROGRAM outranks the question about killing one.**
                // The two cards are mutually exclusive by construction everywhere else (the
                // confirmation is raised from the composer, and the prompt card owns the
                // composer while it is up) — but a prompt can arrive *while* the confirmation
                // is standing, and then one keystroke would have two meanings: `y` is this
                // card's yes and that card's text. The operator's rule is that neither may be
                // answerable by the other's keystroke, so the confirmation yields, with a
                // sentence — the program has asked something and must not be killable by the
                // answer to it.
                if let Some(ask) = self.term_ask.take() {
                    self.say(&format!(
                        "{} is still running — your command's question came first, so nothing \
                         was ended. `!term close` asks again.",
                        ask.line
                    ));
                }
                self.prompt = Some(PromptAsk {
                    req_id,
                    job,
                    command,
                    question,
                });
                self.prompt_buf.clear();
                self.redraw = true;
                Disposition::Rendered
            }
            // The card comes down, whether it was answered or the command ended — and the
            // sentence says which, because those are different things to have happened to a
            // person who was about to type.
            SessionEvent::PromptSettled { req_id, sent, by } => {
                if self.prompt.as_ref().is_some_and(|p| p.req_id == req_id) {
                    self.prompt = None;
                    self.prompt_buf.clear();
                }
                self.note(Note::Warned(Warned {
                    code: "prompt".into(),
                    detail: if sent {
                        format!("answer sent by {by}")
                    } else {
                        format!("nothing sent ({by})")
                    },
                    ts,
                }));
                Disposition::Rendered
            }
            SessionEvent::Warning { code, detail, .. } => {
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
                // **R16, the two halves of a fork.** `auto_compact` is published
                // *before* the fork and says the conversation is about to be
                // replaced; `compacted`/`reseated` are published after it and say it
                // has been. An echo in the air across those two lines was waiting for
                // a row the fork summarised away, so it is resolved here rather than
                // left saying `queued` for the rest of the session.
                if code == "auto_compact" {
                    self.mark_fork();
                }
                if code == "compacted" || code == "reseated" {
                    self.resolve_fork();
                    // **And the fold is over, so the line that walks stops walking.** The
                    // progress event is ephemeral by construction — it has no "done" — so
                    // what ends it is this: the durable warning that says the fork landed.
                    // `auto_compact_failed` clears it too, below: a fold that failed is not
                    // a fold that is still running, and a line that outlives its operation is
                    // the stale measurement this file's `ToolStarted` arm already refuses.
                    self.compacting = None;
                }
                if code == "auto_compact_failed" {
                    self.compacting = None;
                }
                // A slash LISTING opens the pane; a slash sentence stays a note.
                // The daemon sends both under one code — `detail` is the command
                // it echoes back, then the reply — so the head splits them by the
                // only thing that distinguishes them, which is length.
                if code == "slash" || code == "slash_refused" {
                    let (echo, body) = detail.split_once('\n').unwrap_or((&detail, ""));
                    let lines: Vec<String> = body.lines().map(|l| without_control(l)).collect();
                    if lines.len() > 3 {
                        self.slash_out = Some((echo.to_string(), lines));
                        self.pane_scroll = 0;
                        self.redraw = true;
                        return Disposition::Rendered;
                    }
                }
                // A refused job-output read is answered **in the pane that asked**,
                // which is still open — otherwise it would sit at `reading…` for
                // ever, waiting for a window that is not coming. The conversation
                // gets the note as well.
                if code == "job_output_refused"
                    && let Some(v) = self.job_out.as_mut()
                {
                    v.loading = false;
                    v.error = Some(detail.clone());
                    self.redraw = true;
                }
                // **A note about the weather goes on the edge, not in the record.**
                //
                // The operator, on `model_slow_first_byte`: *"it is important diagnostics -
                // we have a yellow triangle for that. both heads should not emit it inside
                // conversation."* So the diagnostic is kept and its PLACEMENT is moved: the
                // count moves a counter, the triangle comes up, and `/status` is where the
                // number lives. `warning::ALARM_ONLY` is the rule and the docstring there
                // says why a compaction stays a row and this does not.
                //
                // **Counted, and `Filtered` rather than dropped.** `Filtered` is what makes
                // "I chose not to show this" different from "nothing happened" — the same
                // distinction the `turn_failed` arm above is refused for. And if this head
                // has no register for a code the tree says is edge-bound, it says so rather
                // than swallowing it: a note that reaches neither the record nor a counter
                // is a note nobody has.
                if letibot_sessionlog::warning::to_the_alarm(&code) {
                    if !self.count_edge_note(&code) {
                        self.note(Note::Warned(Warned {
                            code: "alarm_only_unregistered".into(),
                            detail: format!(
                                "`{code}` is classified as edge-bound and this head has no \
                                 counter for it, so the diagnostic above is the only copy. \
                                 See `warning::ALARM_ONLY`."
                            ),
                            ts,
                        }));
                    }
                    self.redraw = true;
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
                if self.visibility.shows(Show::System) {
                    Disposition::Rendered
                } else {
                    Disposition::Filtered
                }
            }
            SessionEvent::MergeEntryAdded { entry } => {
                // **Folded, never invented** — the jobs pane's rule: the daemon owns the queue,
                // so an entry this head has not been told about is not one to make up. An entry
                // already here is REPLACED, because the same id arriving twice is the same
                // entry (the enqueue is idempotent by id) and the newer word is the true one.
                match self.merge.iter_mut().find(|e| e.id == entry.id) {
                    Some(row) => *row = entry,
                    None => self.merge.push(entry),
                }
                self.redraw = true;
                // **Counted as filtered and not as control**, for the reason
                // `OperatorCallAllowed`'s arm records: this is a session event, read, and
                // deliberately not drawn as a row of the conversation — the pane is where it
                // goes. `Control`'s definition is *not an event*, and a reader would never find
                // the difference.
                Disposition::Filtered
            }
            SessionEvent::MergeEntryMoved {
                id,
                state,
                evidence,
            } => {
                // **The move, folded onto the row the queue already holds.** An id this head
                // does not have is a move for an entry whose `MergeEntryAdded` it missed —
                // possible, since the events and the snapshot are two arrivals — so the row is
                // NOT invented here: the next `ListMergeQueue` carries it. The state is applied
                // either way, because a row that is here must not go on claiming the state it
                // had.
                if let Some(row) = self.merge.iter_mut().find(|e| e.id == id) {
                    row.state = state;
                    row.evidence = evidence;
                }
                self.redraw = true;
                Disposition::Filtered
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

    /// **A page or a wheel over a TAIL-ORIGIN overlay**, which both of them are.
    ///
    /// `sub_out` and `job_out` window their content from the end — a subagent's answer
    /// and a running job's newest bytes are what those panes are opened for — so their
    /// `scroll` counts rows hidden **below** the bottom and moving toward the beginning
    /// ADDS to it. That is the opposite of `pane_scroll`, which counts rows hidden above
    /// the top because a `help`/`todos`/`slash` pane is read from its head.
    ///
    /// One function, because the two must agree about the sign — and because the second
    /// one was **forgotten**: the page keys and the wheel reached this overlay's
    /// transcript instead of the overlay, which is the defect the subagent view had
    /// already been fixed for one arm above (`"a wheel in the subagent output view
    /// scrolled the conversation underneath it"`). One arm is a place to forget.
    ///
    /// Returns whether an overlay took the key, so the caller can fall through to the
    /// transcript when none did.
    pub(crate) fn scroll_tail_overlay(&mut self, up: bool, by: usize) -> bool {
        // **A closure over the value, not a binding to the struct.** The two overlays
        // are different types (`SubOut`, `JobOut`) that happen to share a field name, so
        // an `if let … else if let …` binding one `&mut` for both arms does not compile —
        // which is the compiler saying the obvious thing: there is no shared type here,
        // only a shared rule.
        let moved = |scroll: usize| {
            if up {
                scroll.saturating_add(by)
            } else {
                scroll.saturating_sub(by)
            }
        };
        if let Some(v) = self.sub_out.as_mut() {
            v.scroll = moved(v.scroll);
        } else if let Some(v) = self.job_out.as_mut() {
            v.scroll = moved(v.scroll);
        } else {
            return false;
        }
        self.redraw = true;
        true
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
        // **The terminal talking about itself**, not the person: focus and the background
        // colour. Recorded and nothing else — they must not dismiss a notice, end a recall or
        // reach a pane the way a keystroke does.
        match k {
            Key::FocusIn | Key::FocusOut => {
                self.focused = Some(matches!(k, Key::FocusIn));
                return None;
            }
            Key::Background { light } => {
                if self.light_background != Some(light) {
                    self.light_background = Some(light);
                    self.invalidate_history();
                    self.redraw = true;
                }
                return None;
            }
            _ => {}
        }
        // **The session's prompts, before a recall starts** — see `refresh_prompt_history`.
        // Up may yet be taken by a card or a pane further down; refreshing the list then
        // changes nothing a reader sees, and the editor ignores it mid-recall.
        if matches!(k, Key::Up) {
            self.refresh_prompt_history();
        }
        // **Any key is an acknowledgement of whatever the notice said**: the deadline
        // moves to now, so this tick's `screen()` — which runs below the key handling —
        // takes the sentence down before anything is drawn. The arrows and the paging
        // keys are exempt because they are the keys a reader scrolls *with*.
        //
        // A notice nobody timed is not the reader's to dismiss, exactly as only `say`
        // starts a clock: a key moves a deadline, it does not invent one.
        if !matches!(k, Key::Up | Key::Down | Key::PageUp | Key::PageDown)
            && self.notice_until.is_some()
        {
            self.notice_until = Some(self.now_ms);
        }
        // **The `allow-all` confirmation owns the keyboard too**, and for the same
        // reason the password field does: a question this consequential must not be
        // answered by a keystroke the operator aimed at the composer.
        //
        // **`y` and Enter both confirm.** It was `y` alone, on the fail-closed
        // argument that a mistyped answer should be a no — which is right about
        // stray keys and wrong about Enter, the key every other card in this file
        // confirms with (the quit card takes it, the ladder takes it, the pickers
        // take it). The operator, 2026-09-20: *"i did allow-all and even got to
        // that giant red warning"* — and the session was still at
        // `automode-edits` afterwards, because the natural keystroke on a
        // confirmation silently cancelled it. A card that names two keys and
        // means one of them is a card that lies.
        //
        // Everything else still cancels, Esc included, so a key aimed at the
        // composer is still a no.
        if self.mode_confirm.is_some() {
            let name = self.mode_confirm.take().unwrap();
            self.redraw = true;
            return match k {
                Key::Char('y') | Key::Char('Y') | Key::Enter => {
                    self.say("allow-all confirmed for this session");
                    Some(Action::Mode {
                        name,
                        consented: true,
                    })
                }
                _ => {
                    self.say("allow-all cancelled — the mode did not change");
                    None
                }
            };
        }
        // **A password field owns the keyboard.** While `sudo` is waiting, every
        // key is the password's: characters and pastes go into the buffer, Enter
        // sends it, Esc or Ctrl+C refuses. Nothing reaches the composer, the
        // ladder or the scrollback, so a password cannot land in a prompt.
        // **The new-todo card owns the keyboard**, ahead of the composer and behind nothing else
        // that is modal. Three keys are its own; everything else is the composer's, so the title and
        // the description are typed, edited and pasted with the keys the operator already has.
        if self.todo_draft.is_some() {
            match k {
                Key::Tab => {
                    let live = self.input().to_string();
                    let mut shown = String::new();
                    if let Some(draft) = self.todo_draft.as_mut() {
                        // The composer's text goes into the field being LEFT, and the field being
                        // entered comes out — leticl's `%todo-draft-focus`, one order. **The text is
                        // read BEFORE the focus moves**: `shown` answers with the composer's live
                        // text for the field that is focused, so asking after the move answers with
                        // the field we just left and the composer would come up empty.
                        draft.take(&live);
                        let next = draft.next();
                        shown = draft.shown("", next);
                        draft.focus = next;
                    }
                    self.set_composer(&shown);
                    self.redraw = true;
                    return None;
                }
                Key::Enter => {
                    let live = self.input().to_string();
                    let Some(mut draft) = self.todo_draft.take() else {
                        return None;
                    };
                    draft.take(&live);
                    // **A title is required and the card stays up without one** — the only field
                    // rule, and saying so beats storing a row of nothing.
                    if draft.title.trim().is_empty() {
                        self.todo_draft = Some(draft);
                        self.say("a todo item needs a title — type one, or esc to cancel");
                        self.redraw = true;
                        return None;
                    }
                    let mut text = draft.title.trim().to_string();
                    if !draft.detail.trim().is_empty() {
                        text.push_str(" — ");
                        text.push_str(draft.detail.trim());
                    }
                    let when = if draft.when.trim().is_empty() {
                        None
                    } else {
                        Some(letibot_sessionlog::event::TodoCondition::Job {
                            handle: draft.when.trim().to_string(),
                        })
                    };
                    self.set_composer("");
                    // **The row is filed HERE, not by handing the card's words to the verb parser.**
                    //
                    // The add used to go through `todo_command`, on the argument that a card and a
                    // typed line must not become different acts. They still are one act — this
                    // writes the same row, tagged the same way, as `SetOperatorTodos`, and echoes
                    // it on the same path — but the WORDS are not re-read as a command line, and
                    // that matters more with three fields than it did with two: a title reading
                    // `done 2` or `when 1 j7` was taken for the verb by that door and would move or
                    // condition a row the reader never named. A form's fields are fields; the three
                    // verbs stay the typed door, which is the other one the operator asked for
                    // (*"or via a form, when I file a todo"*).
                    let mut mine = self.operator_todos();
                    mine.push(letibot_sessionlog::event::TodoEntry {
                        content: text,
                        status: letibot_sessionlog::event::TodoStatus::Pending,
                        by: letibot_sessionlog::event::TodoBy::Operator,
                        when,
                    });
                    self.echo_operator_todos(mine.clone());
                    self.redraw = true;
                    return Some(Action::SetOperatorTodos(mine));
                }
                Key::Esc | Key::CtrlC => {
                    self.todo_draft = None;
                    self.set_composer("");
                    self.say("nothing added");
                    self.redraw = true;
                    return None;
                }
                _ => {}
            }
        }
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
        // **The confirmation that ends a pane owns the keys while it is up**, and it is checked
        // ahead of the prompt card so the two can never both be answered by one keystroke — see
        // [`TermAsk`] for why the yes is `y` and not Enter, and why every other key cancels.
        //
        // **A detach never asks**, because it ends nothing: this card exists only for a
        // `!term close` the operator typed, and only while a program is running (see
        // [`App::begin_close`]).
        if let Some(ask) = self.term_ask.take() {
            self.redraw = true;
            return match k {
                Key::Char('y') | Key::Char('Y') => {
                    // **An ending is on its way.** The keys stop here from now on — a byte written
                    // into a pty whose program is being signalled is a byte nobody will read —
                    // and the row the ending becomes is filed when the daemon's `TermEnded`
                    // lands, with `closed: true` so the register is the operator's own act.
                    //
                    // **Nothing is said here.** The card coming down is the act, and the row the
                    // ending files a moment later is the disclosure: a notice on top of the two
                    // would be the third copy of one fact, and it is the one that fades.
                    if let Some(p) = self.term.as_mut() {
                        p.closing = true;
                    }
                    Some(Action::TermClose)
                }
                // **Anything that is not a deliberate yes is the cancel**, Esc included — the
                // only shape a destructive confirmation can have. It says what it did NOT do,
                // because a card that vanishes in silence reads as an act.
                _ => {
                    self.say(&format!(
                        "{} is still running — nothing was ended",
                        ask.line
                    ));
                    None
                }
            };
        }
        // **A command of the operator's own asked them something, and this owns the
        // keyboard** — the password field's rule one card over, and for the same reason:
        // while a card is up, a character typed is an answer to it and not the first letter
        // of the next thing the operator meant to say.
        //
        // **Enter sends and Esc puts the card away, and the two are not the same act.**
        // Enter answers the command: the line goes down the frame the daemon writes into the
        // run's stdin. **Esc does NOT refuse anything** — the command is still running and
        // still waiting, and there is nothing to refuse — it only takes this head's card off
        // the screen, which is what a person wants when they would rather type the answer as
        // a `!send` line or watch the stream for a moment longer. The daemon keeps the
        // request open and the run keeps waiting; the card does not come back, because the
        // run has not asked a new question.
        //
        // **An empty line is a real answer** and Enter on an empty field sends it: `Continue?
        // [Y/n]` takes Enter as its default, and a person accepting a default must not have
        // to type a letter to say so.
        if let Some(ask) = &self.prompt {
            let req_id = ask.req_id.clone();
            match k {
                Key::Char(c) => self.prompt_buf.push(c),
                Key::Paste(s) => self.prompt_buf.push_str(s.trim_end_matches(['\n', '\r'])),
                Key::Backspace => {
                    self.prompt_buf.pop();
                }
                Key::KillToStart | Key::KillToEnd => self.prompt_buf.clear(),
                Key::Enter => {
                    let line = std::mem::take(&mut self.prompt_buf);
                    self.prompt = None;
                    self.redraw = true;
                    return Some(Action::PromptAnswer { req_id, line });
                }
                Key::Esc | Key::CtrlC => {
                    self.prompt_buf.clear();
                    self.prompt = None;
                    self.redraw = true;
                    self.say(
                        "card put away — the command is still waiting, and \
                              `!send LINE` answers it",
                    );
                    return None;
                }
                _ => {}
            }
            self.redraw = true;
            return None;
        }
        // **A key the picker asked for owns the keyboard**, exactly as the password field
        // does and for the same reason: characters and pastes go into the buffer, Enter does
        // both things in one verb — `/models CHOICE --key K` stores the key (mode 600, the
        // file the daemon reads) AND takes the row, which is the round trip the typed
        // spelling already is — and Esc cancels with nothing stored. Nothing reaches the
        // composer, so a key cannot land in a prompt, and the composer's own box draws a dot
        // per character while this is up (`composer_rows`).
        if let Some(ask) = self.key_ask.clone() {
            match k {
                Key::Char(c) => self.key_buf.push(c),
                Key::Paste(s) => self.key_buf.push_str(s.trim_end_matches(['\n', '\r'])),
                Key::Backspace => {
                    self.key_buf.pop();
                }
                Key::KillToStart | Key::KillToEnd => self.key_buf.clear(),
                Key::Enter => {
                    let key = std::mem::take(&mut self.key_buf);
                    self.key_ask = None;
                    self.redraw = true;
                    // **An empty enter is a cancelled ask, not a stored empty key** — the
                    // daemon would refuse it and the row was not taken.
                    if key.is_empty() {
                        self.say("no key given — the row was not taken");
                        return None;
                    }
                    self.say(&format!(
                        "storing the {} key and switching to {}…",
                        ask.provider, ask.choice
                    ));
                    // The switch, then a re-read of the rows it changed — the same order the
                    // plain switch keeps, so the header names what answers now.
                    self.queued.push(Action::Settings);
                    return Some(Action::Slash {
                        line: format!("models {} --key {}", ask.choice, key),
                    });
                }
                Key::Esc | Key::CtrlC => {
                    self.key_buf.clear();
                    self.key_ask = None;
                    self.say("cancelled — nothing was stored and the row was not taken");
                    self.redraw = true;
                    return None;
                }
                _ => {}
            }
            self.redraw = true;
            return None;
        }
        match k {
            // **A switch's chord, from the table that advertises it — and through `/verbosity`
            // its own self.**
            //
            // The two arms this replaces named their keys by hand, twice each: `ctrl-r` flipped
            // `self.reasoning` and `ctrl-x` flipped `self.raw_calls` — both of them the MIRROR the
            // drawing reads rather than the SET — so a press moved the field under the picture
            // while `Visibility` (the status row, `head.toml`, `keeps`, `rung()`) went on saying
            // the old set. `read-edits` plus `ctrl-r` was the sharp end: the thinking appeared
            // while the ladder still hid everything the thinking belongs with.
            //
            // The word is spelled and handed to [`App::set_verbosity`] rather than the fields
            // being written here, because that function is the ONE writer: it refuses a set no
            // rung can draw, moves the folds with the set, reanchors off a row the change hid,
            // closes what the change invalidates, writes the preference file, and says what
            // changed. So a chord is `/verbosity thinking=open` under a shorter spelling — which
            // is the operator's own ask, *"some toggled by shortcuts some by /commands"* — and it
            // cannot drift from the verb, because it IS the verb.
            k if k.show().is_some() => {
                let s = k.show().expect("the guard just asked the same question");
                let next = s.by_chord(self.visibility.level(s))?;
                return self.set_verbosity(&format!("{}={}", s.name(), next.as_str()));
            }
            Key::CtrlV => {
                // **One row, not a switch — R10's ruling on the overload.**
                //
                // This used to flip the conversation-wide tool fold *and* seed a window
                // on the newest long result, so one chord did two things: the wall, and
                // one row's rest. The seam under the reader's eyes says `… +N lines ·
                // ctrl-v opens it`, which reads per-row, and the operator's report is
                // exactly that **what surprised them was that ctrl-t triggered the wall
                // AT ALL**. A chord cannot be named by a per-row seam and mean the whole
                // conversation, so it keeps the meaning a seam can honestly name and the
                // conversation-wide unfold keeps `/t`, which is where it already lived
                // (and where `/help` now points).
                //
                // The window follows the newest long result for the reason
                // [`App::newest_payload_row`] gives, and the seam names this chord only
                // on that row — every other row names `/t`, because a chord may only be
                // named where it acts.
                //
                // **Under `conversation` the same chord opens the run** (R37 AMENDED), and
                // that is not a second meaning: the marker's own seam says `ctrl-t opens
                // it`, and what it opens is the newest thing on the screen that has a rest
                // to read — one result's window under every other rung, one run of hidden
                // work under this one. [`App::newest_openable`] is the one place that
                // choice is made, so the chord and the seam cannot come to disagree about
                // which of the two it is.
                if self.payload_sel.is_some() {
                    self.payload_sel = None;
                    self.payload_page = 0;
                    // **And an open run closes.** It was opened by this key, so this key is
                    // what closes it — the same bargain every other window in this file
                    // makes, and without it the second press would open a payload window
                    // inside a row that is only on screen because the run is open.
                    self.invalidate_history();
                } else if let Some(id) = self.newest_openable() {
                    self.payload_sel = Some(id);
                    self.payload_page = 0;
                    self.payload_max.set(usize::MAX);
                }
                // **Not `refold`.** That is the fold's own: it resets the scroll and
                // announces the fold state, and neither happened here.
                self.invalidate_history();
                self.redraw = true;
                return None;
            }
            Key::CtrlL => {
                // **This one keeps `redraw`.** The flag means *throw the glass away* — the next
                // frame erases and rewrites in full — and that is right exactly when this head's
                // memory of the screen is known to be wrong. Ctrl-L is the operator saying
                // something else wrote to their terminal, which is that case, and it is the only
                // key that is. The scroll keys are the opposite case and take the diff; see
                // [`App::hold`] for why the wheel does not.
                self.redraw = true;
                return None;
            }
            Key::CtrlS => {
                self.picker = !self.picker;
                self.redraw = true;
                // The two pickers are never open together: each opener closes
                // the other, so the screen holds one list and the arrows mean
                // one thing.
                if self.picker {
                    self.pick = None;
                }
                // Opening it asks for a fresh list rather than drawing the one from
                // the attach: sessions are a shared thing, and a picker showing what
                // was true when this head connected is a picker that hides the
                // session somebody else just started.
                if self.picker {
                    // The cursor starts where you are, so Enter on an untouched list
                    // is a no-op and the arrows move from a row that means something.
                    // **Through the same enumeration the arrows and Enter read.** The session this
                    // head is in is always a row — `session_rows` shows the chain down to it
                    // whatever is collapsed — so this position always exists.
                    self.picker_sel = self
                        .session_rows()
                        .iter()
                        .position(|r| self.sessions[r.idx].session_id == self.session_id)
                        .unwrap_or(0);
                }
                return self.picker.then_some(Action::ListSessions);
            }
            Key::CtrlT => return self.toggle_todos(),
            // **Hold the view (R56).** While held the head writes nothing at all, so a
            // mouse selection survives a streaming turn; the second press releases it and
            // says how much arrived while it was held. The whole contract is in
            // [`App::toggle_hold`].
            Key::CtrlP => return self.toggle_hold(),
            // Ctrl+G for the subagent tree: R/T/X/L/S/P are taken, A/E/W/U/Y/K/B/F
            // are the composer's readline keys, and the subagent tree is a *view*,
            // not a thing the composer needs a letter for.
            Key::CtrlG => {
                self.toggle_subagents();
                return None;
            }
            // Ctrl+Q for the background jobs. J would have been the mnemonic and
            // is line-feed; Q is XON, dead the same way Ctrl+S's XOFF would be —
            // and fixed the same way: cfmakeraw clears IXON, so nothing is
            // listening for flow control and the byte arrives like any other.
            Key::CtrlQ => {
                self.jobs_pane = !self.jobs_pane;
                self.pane_scroll = 0;
                self.redraw = true;
                // Opening it asks the daemon, the way the todos pane does: the
                // process table is the daemon's and a head that drew its own
                // version drew a stale one. Later changes arrive as `JobSettled`.
                return self.jobs_pane.then_some(Action::ListJobs);
            }
            // **R22: clearing your own screen costs one key.**
            //
            // Retiring a note used to be `/notes dismiss all` — the right power in the wrong
            // hand, because the thing you do to clear your own screen is a reflex and every
            // other reflex here is already a chord. The operator, on being told how to hide a
            // note: *"typing `/notes dismiss all` is not humane."*
            //
            // **It calls the VERB rather than reimplementing it.** `notes_command` is the one
            // writer for this act — it computes the keys, retires them through `retire()`,
            // saves the file and composes the sentence — so the chord and `/notes dismiss all`
            // cannot come to disagree about any of those four things. A second copy of "retire
            // every note" is a second place for the persisted set to be written differently,
            // and the whole point of agreeing the key with the other head is that one act has
            // one behaviour.
            //
            // **And the empty press says so.** That is the one place this head's answer to
            // R22 differs from the proposal it agreed with, and the reason is this tree's own
            // rule that *a chord may only be named where it acts*: `ctrl-t` is silent with
            // nothing to open because its seam names it only on the row it can open, and the
            // hint bar cannot be conditional — it names `ctrl-n` always, so the chord answers
            // when pressed. `ctrl-o` sets the same precedent one arm away ("nothing is running
            // to move to the background"), and the operator's own worry is that a reflex which
            // appears to do nothing invites a second press. One line, routine register, taken
            // down by any key including this one.
            Key::CtrlN => {
                // **Read the durable list before counting what is left.** Another head
                // may have retired one of these since this one loaded the file, and the
                // count is what decides between the verb and the honest empty answer.
                self.refresh_retired();
                // **"Nothing to retire" means nothing LEFT to retire**, not "no notes held".
                //
                // A retired note stays in `notes` on purpose (R10: retired is not deleted), so
                // after a first press the set is *all retired* rather than *empty* — and a guard
                // on `notes.is_empty()` would send the second press down the verb's path, where
                // it would say `retired 0 note(s)`, which is a true sentence an operator should
                // not be shown. Counting what is left makes the chord idempotent and honest:
                // first press retires N and says so, second press says this.
                let left = self
                    .notes
                    .iter()
                    .filter(|(_, n)| !self.is_retired(n))
                    .count();
                if left == 0 {
                    self.say("nothing to retire");
                } else {
                    let _ = self.notes_command("notes", "dismiss all");
                }
                return None;
            }
            // **Ctrl+O: move the running command to the background**, the same action
            // `/promote` names. See [`App::promote`] for why the fact it guards is a
            // running CALL and not a running turn.
            Key::CtrlO => return self.promote(),
            // **The wheel and the page keys move what is on the screen.** They
            // moved the transcript unconditionally, so a wheel in the subagent
            // output view scrolled the conversation underneath it, and Esc
            // came back to a transcript parked wherever the wheel had left it
            // — the operator's report (2026-09-17): "if i scroll subagent
            // output and return to the main conversation the scroll position
            // saved for some reason". The output view takes them; any other
            // screen on top swallows them, because a view that is not on the
            // screen does not move.
            // **An open payload window takes the page keys and the wheel**, as it takes the
            // arrows below: the reader opened one result to read it, and these moved the
            // transcript underneath it instead — which, at the bottom already, looked like
            // nothing happening at all.
            Key::PageUp | Key::PageDown | Key::WheelUp | Key::WheelDown
                if self.payload_sel.is_some() =>
            {
                let by = match k {
                    Key::WheelUp | Key::WheelDown => 3,
                    _ => self.screen_rows.max(1) / 2 + 1,
                };
                self.page_payload(matches!(k, Key::PageUp | Key::WheelUp), by);
                return None;
            }
            Key::PageUp | Key::PageDown | Key::WheelUp | Key::WheelDown => {
                // **A page is a screen, not ten lines.** `PageUp` moved by a constant ten,
                // which on a 40-row terminal is a quarter of the page the key is named for
                // — and on the tail path it compounded with the crawl `scroll_up` fixes.
                let (up, by) = match k {
                    Key::PageUp => (true, self.screen_rows.max(1)),
                    Key::PageDown => (false, self.screen_rows.max(1)),
                    // Three lines a notch: a wheel notch is a row at a time in
                    // a pager, but a transcript row can be two screen rows after
                    // wrapping, and a notch that moves one wrapped row reads as
                    // nothing happened.
                    Key::WheelUp => (true, 3),
                    _ => (false, 3),
                };
                // **An open card takes them while its own content has somewhere to go**
                // (R20). The card is the thing that needs an answer, it already owns
                // Up/Down and Enter, and the wall above its ladder is the one screenful
                // the operator may have to read past — so the page keys move *that* window
                // for as long as there is one. When the content fits, nothing here fires
                // and the keys do exactly what they always did: a card being up must not
                // cost the transcript its scroll.
                //
                // The two numbers are the last frame's, because whether anything is out of
                // view is a fact about the width — see [`App::card_window`].
                if !self.open.is_empty() && self.dec_content_len > self.dec_content_room {
                    let max = self.dec_content_len - self.dec_content_room;
                    self.dec_scroll = if up {
                        self.dec_scroll.saturating_sub(by)
                    } else {
                        (self.dec_scroll + by).min(max)
                    };
                    self.redraw = true;
                    return None;
                }
                if self.scroll_tail_overlay(up, by) {
                    return None;
                }
                // **An open pane takes them.** They used to be swallowed here,
                // on the reasoning that a view underneath a pane should not
                // move — which is right, and left the pane itself unable to
                // scroll at all. A pane longer than the terminal was a pane
                // whose tail could not be read: `leticl`'s TODO.md is 98 rows.
                //
                // The two pickers are excluded: they are short, and their click
                // arithmetic is keyed on rows counted from the top of the card.
                if self.picker || self.pick.is_some() {
                    return None;
                }
                if self.help
                    || self.stats
                    || self.todos_pane
                    || self.subagents_pane
                    || self.jobs_pane
                    || self.config_pane
                    // **And the slash listing, which is a document read from its
                    // head.** It has its own arm for the arrows, and it was missing
                    // from *this* list — so PageDown while a `/notes` listing was up
                    // scrolled the transcript underneath it, one pane over from the
                    // same defect the subagent view had.
                    || self.slash_out.is_some()
                {
                    // **The polarity is the opposite of the transcript's**, and
                    // getting it wrong here made PageDown a no-op that looked
                    // exactly like the swallowing this replaced. `self.scroll`
                    // counts rows back from the BOTTOM — scrolling up increases
                    // it — because the transcript is read from its tail.
                    // `pane_scroll` counts rows hidden above the TOP, because a
                    // pane is read from its head. So down is the one that grows.
                    let max = self.pane_len.saturating_sub(self.pane_room);
                    self.pane_scroll = if up {
                        self.pane_scroll.saturating_sub(by)
                    } else {
                        (self.pane_scroll + by).min(max)
                    };
                    self.redraw = true;
                    return None;
                }
                if up {
                    // **Through `scroll_up`, which renders as it goes.** Assigning `scroll`
                    // here and letting the frame's fill catch up is what made a page crawl:
                    // the frame clamps against the rows rendered so far, so a press could
                    // never express "further up than I have drawn".
                    self.scroll_up(by);
                } else {
                    // **Down is a WALK, the wheel included — and arriving at the tail is what
                    // resumes following.**
                    //
                    // The mirror of `scroll_up`, and `hold` for the same reason (R36): moving
                    // down is moving over the same rows in the other direction, and it is the
                    // same conversion from lines to a row. It also lands the reader back in
                    // *following* when it reaches the bottom, which is the one act that does —
                    // arriving content never will.
                    //
                    // **A `WheelDown` used to skip all of that and be the tail in ONE notch.**
                    // That was 2026-10-05's answer to *"I cant scroll back to bottom with a
                    // mouse wheel - have to press escape"*, and the cure cost more than the
                    // complaint: clearing the anchor made a single notch a jump rather than a
                    // step, so the reader could not walk *down* through a conversation at all.
                    // The operator again, with a mouse: *"one simple stroke gets me to the
                    // bottom immediately — effectively like Esc"*. Both reports are one coin,
                    // and this is the reconciliation — the notch walks three lines like its up
                    // twin, and a RUN of notches still returns the reader to the bottom, which
                    // answers October's need without the one-notch jump.
                    //
                    // The deliberate act keeps its own meaning and is not folded in here: Esc
                    // while parked — *"Esc while parked in the scrollback means \"follow the
                    // stream again\""* — and the parked `↓` below still clear the anchor in one
                    // press, and they are the keys the banner names for it.
                    self.hold(by as isize);
                }
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
                // **Through the one function that knows the sign**, so the arrows, the
                // page keys and the wheel cannot disagree about which way is back.
                Key::Up => {
                    self.scroll_tail_overlay(true, 1);
                    return None;
                }
                Key::Down => {
                    self.scroll_tail_overlay(false, 1);
                    return None;
                }
                Key::Enter => {
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

        // **The job-output view owns the keys while it is open.** Up and down walk
        // the loaded window; right asks for the next page and left walks back the
        // way right came; Enter refreshes a running job, or takes the next page when
        // there is one; Esc goes back to the jobs list, not out of everything — the
        // same shape, key for key, as the subagent-output view above.
        if self.job_out.is_some() {
            match k {
                Key::Up => {
                    self.scroll_tail_overlay(true, 1);
                    return None;
                }
                Key::Down => {
                    self.scroll_tail_overlay(false, 1);
                    return None;
                }
                Key::Enter => {
                    return self.job_out_page(true);
                }
                // **`Right` keeps the guard that `Enter` just lost**, and the difference
                // is what the key is *for*. Enter here is the pane's — it is the key the
                // pane advertises and the operator's words are not what they meant by it.
                // Right is a cursor key first: a half-typed line keeps its motion, which
                // is the same reason the composer's own arrows are not up for grabs.
                Key::Right if self.editor.text().is_empty() => {
                    return self.job_out_page(true);
                }
                Key::Left if self.editor.text().is_empty() => {
                    return self.job_out_page(false);
                }
                Key::Esc | Key::CtrlC => {
                    self.job_out = None;
                    self.redraw = true;
                    return None;
                }
                _ => {}
            }
        }

        // **The slash listing owns the keyboard while it is up**, the way the
        // subagent-output pane above does: it is a screen covering the
        // conversation, so the keys that scroll and dismiss it must not also
        // reach the composer behind it.
        if self.slash_out.is_some() {
            match k {
                Key::Esc | Key::CtrlC => {
                    self.slash_out = None;
                    self.pane_scroll = 0;
                    self.redraw = true;
                    return None;
                }
                // **Up moves toward the beginning, which means DECREASING this
                // offset.** `pane_scroll` counts rows hidden **above the top** —
                // `pane_window` is literally `skip(self.pane_scroll)` — so adding to it
                // walks further *down* the document. These two arms had it inverted, so
                // `↑` scrolled a `/notes` listing toward its end while the footer under
                // it said `up/down scrolls`. It is leticl's own finding in the same
                // place: *"called with the top-origin sign, ↑ walked toward the END
                // while the hint bar said otherwise."*
                Key::Up => {
                    self.pane_scroll = self.pane_scroll.saturating_sub(1);
                    self.redraw = true;
                    return None;
                }
                // Down is the bounded one: `pane_window` clamps it against the rows
                // it actually has, which is the only place that knows how many there
                // are (the slash listing is built on every draw).
                Key::Down => {
                    self.pane_scroll = self.pane_scroll.saturating_add(1);
                    self.redraw = true;
                    return None;
                }
                _ => {}
            }
        }

        // Help and the picker are screens, and the two keys that mean "go back"
        // close them before the composer ever sees them.
        // The config pane owns Up/Down/Enter while it is open: arrows move,
        // Enter changes the row under the cursor when it is one that can change
        // now, and says why when it is not.
        if self.config_pane {
            match k {
                Key::Up => {
                    let n = self.config_rows().len().max(1);
                    self.config_sel = if self.config_sel == 0 {
                        n - 1
                    } else {
                        self.config_sel - 1
                    };
                    self.redraw = true;
                    return None;
                }
                Key::Down => {
                    let n = self.config_rows().len().max(1);
                    self.config_sel = (self.config_sel + 1) % n;
                    self.redraw = true;
                    return None;
                }
                Key::Enter => {
                    return self.config_change();
                }
                _ => {}
            }
        }
        // **An entry's detail overlay owns Esc, the arrows and nothing else.** Esc goes back to
        // the LIST, which is still behind it — the jobs pane's rule, one pane along.
        //
        // **It sits ABOVE the block that closes every pane on Esc, and that is load-bearing.**
        // Below it, the first Esc closed the PANE and left the overlay standing — and because
        // the overlay is drawn before the pane, the screen did not change: one press did
        // nothing, two presses left the queue. Every other overlay in this file (`sub_out`,
        // `job_out`) is above that block for the same reason, and
        // `esc_leaves_the_entry_overlay_with_the_queue_still_behind_it` is the test that holds
        // this one there.
        if self.queue_open.is_some() {
            if matches!(k, Key::Esc | Key::CtrlC) {
                self.queue_open = None;
                self.pane_scroll = 0;
                self.redraw = true;
                return None;
            }
            if matches!(k, Key::Up | Key::Down | Key::PageUp | Key::PageDown) {
                let page = self.pane_room.max(1);
                match k {
                    Key::Up => self.pane_scroll = self.pane_scroll.saturating_sub(1),
                    Key::Down => self.pane_scroll += 1,
                    Key::PageUp => self.pane_scroll = self.pane_scroll.saturating_sub(page),
                    _ => self.pane_scroll += page,
                }
                // Clamped against the last draw's own numbers: the key handler has no width and
                // no height, and a scroll clamped against a guess walks past the end.
                let max = self.pane_len.saturating_sub(self.pane_room);
                self.pane_scroll = self.pane_scroll.min(max);
                self.redraw = true;
                return None;
            }
        }

        if (self.help
            || self.picker
            || self.pick.is_some()
            || self.stats
            || self.todos_pane
            || self.subagents_pane
            || self.jobs_pane
            || self.queue_pane
            || self.config_pane)
            && matches!(k, Key::Esc | Key::CtrlC)
        {
            self.help = false;
            self.picker = false;
            self.pick = None;
            self.quit_card = false;
            self.stats = false;
            self.todos_pane = false;
            self.subagents_pane = false;
            self.jobs_pane = false;
            self.queue_pane = false;
            self.config_pane = false;
            self.sub_out_pending = None;
            self.redraw = true;
            return None;
        }
        // **Esc while parked in the scrollback means "follow the stream again"**,
        // which is what the scrollback banner says it means. Only then does Esc
        // start arming an interrupt — **and not while a payload window is open**: that
        // window's own seam prints `esc closes`, and the surface carrying the promise is
        // the one Esc has to keep it to. See the arm below.
        if matches!(k, Key::Esc) && !self.following() && self.payload_sel.is_none() {
            // **Back to following**, which is what the banner says this key does — and it
            // clears the anchor as well as the count, because the two are one state (R36).
            self.scroll = 0;
            self.anchor = None;
            return None;
        }
        // **An open payload view owns the arrows and Esc**, and it sits here — ahead of
        // the transcript's own scrolling — for two reasons. The reader has said which row
        // they are reading, so Up/Down must move *inside* it rather than moving the
        // conversation underneath; and the seam it draws says `esc closes`, so Esc must
        // mean that while it is up. It used to lose Esc to the scrollback arm above,
        // which meant a reader who was parked in the history *and* had a window open got
        // the transcript un-parked instead — a panel on the screen advertising a key that
        // had just done something else. Ahead of the decision ladder too, because a
        // payload view is opened deliberately and a permission that arrives while it is
        // open should not steal the arrows from under it.
        //
        // The same bargain the subagent-output pane makes, and the rule behind both:
        // whichever surface prints `esc closes` owns Esc, and only one can be up.
        if self.payload_sel.is_some() {
            /// How far one press pages. The same unit the transcript scrolls by.
            const BY: usize = 10;
            match k {
                Key::Esc => {
                    self.payload_sel = None;
                    self.payload_page = 0;
                    self.redraw = true;
                    return None;
                }
                Key::Up | Key::PageUp => {
                    self.page_payload(true, BY);
                    return None;
                }
                Key::Down | Key::PageDown => {
                    self.page_payload(false, BY);
                    return None;
                }
                // The ends, which a long build log is read from as often as its head.
                Key::Home => {
                    self.page_payload(true, usize::MAX);
                    return None;
                }
                Key::End => {
                    self.page_payload(false, usize::MAX);
                    return None;
                }
                _ => {}
            }
        }
        // **An open decision owns Up/Down and a bare Enter.**
        //
        // Before the composer, because while a prompt is on the screen those keys mean
        // the ladder and cannot sensibly mean anything else -- the same argument the
        // picker arm above already makes for a bare row number.
        //
        // **Up/Down move the ladder whether or not a line is being typed**, and
        // this used to require an empty composer so that "a half-typed line still
        // scrolls and still edits". The cost of that was not visible until the
        // operator hit it: a permission arrives while you are typing, and the only
        // way to reach the menu is to empty the composer first — so the words you
        // were writing are the price of choosing an option. Their words,
        // 2026-09-20: *"suppose i type a prompt and permission ask arrives — until
        // i press down arrow I wont get into the permissions menu, by which time
        // my prompt is erased and gone"*.
        //
        // The quit card and the jobs pane in this same file take Up/Down
        // unconditionally — they no longer gate Enter, which is now every
        // pane's (see the arm that closes the composer to it below); the ladder
        // was the odd one out for the arrows.
        // Nothing is taken from the composer, because a one-line composer does not
        // edit with Up/Down — what moves aside is scrollback scrolling, for as long
        // as an ask is open, and PageUp/PageDown still do that.
        //
        // Enter and the digits keep the empty-composer guard, and for a reason that
        // is the opposite of this one: with a typed line, Enter is `submit`'s, which
        // answers the marked row and HOLDS the words — a permission arriving
        // mid-typing must not turn Enter into "send the half-thought" — and a line
        // being typed keeps its digits.
        if !self.open.is_empty() {
            let n = decision_rows(&self.open[0]);
            let typing = !self.editor.text().is_empty();
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
                Key::Enter if n > 0 && !typing => {
                    return self.answer_marked();
                }
                // A row number is the row, and answering it — see `digit_row`.
                _ if !typing && digit_row(&k, n).is_some() => {
                    self.sel = digit_row(&k, n).unwrap();
                    return self.answer_marked();
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
            let rows = self.session_rows();
            let n = rows.len();
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
                // **A conversation with sub-sessions opens and closes**, the two gestures a tree
                // uses everywhere — and the reason a collapsed default can afford to be collapsed.
                // Guarded on an empty composer, like every other pane's arrows.
                Key::Right if self.editor.text().is_empty() => {
                    let id = self.sessions[rows[self.picker_sel.min(n - 1)].idx]
                        .session_id
                        .clone();
                    if !self.expanded.iter().any(|e| *e == id) {
                        self.expanded.push(id);
                        self.redraw = true;
                    }
                    return None;
                }
                Key::Left if self.editor.text().is_empty() => {
                    let row = rows[self.picker_sel.min(n - 1)];
                    // **A child closes its PARENT**, so `←` means *back up the tree* rather than
                    // nothing at all on the row you just arrived at.
                    let want = match row.depth {
                        0 => Some(self.sessions[row.idx].session_id.clone()),
                        _ => self.sessions[row.idx].parent_session_id.clone(),
                    };
                    if let Some(want) = want
                        && let Some(pos) = self.expanded.iter().position(|e| *e == want)
                    {
                        self.expanded.remove(pos);
                        self.redraw = true;
                    }
                    return None;
                }
                Key::Enter if self.editor.text().is_empty() => {
                    let id = self.sessions[rows[self.picker_sel.min(n - 1)].idx]
                        .session_id
                        .clone();
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

        // **An open mode picker owns Up and Down, and Enter on an empty line.**
        //
        // The session picker's twin, one question narrow: the mode this session
        // runs under. It sits after the session picker, which keeps precedence
        // while both are up — though neither ever is, each opener closing the
        // other — and after the decision ladder for the same reason. The
        // empty-composer rule is the ladder's own: a half-typed line's Enter
        // still means the line, and the typed path lands in `pick_mode` through
        // `submit`.
        // **The quit card owns the keys while it is open**, ahead of every other
        // list: it was opened by a key that means "I am leaving", and a stray
        // arrow landing in the transcript under it would be a keystroke the
        // operator aimed at the card.
        if self.quit_card {
            match k {
                Key::Up | Key::Down => {
                    self.quit_sel = 1 - self.quit_sel.min(1);
                    self.redraw = true;
                    return None;
                }
                _ if self.editor.text().is_empty() && digit_row(&k, 2).is_some() => {
                    self.quit_sel = digit_row(&k, 2).unwrap();
                    self.quit_card = false;
                    self.quit = true;
                    return Some(if self.quit_sel == 0 {
                        Action::Quit
                    } else {
                        Action::StopDaemon
                    });
                }
                Key::Enter => {
                    self.quit_card = false;
                    self.quit = true;
                    return Some(if self.quit_sel == 0 {
                        Action::Quit
                    } else {
                        Action::StopDaemon
                    });
                }
                // Esc is "I did not mean to leave", which is the answer a card
                // like this has to have — the alternative is an operator who
                // hit Ctrl+C twice by habit and cannot take it back.
                Key::Esc | Key::CtrlC => {
                    self.quit_card = false;
                    self.say("staying");
                    self.redraw = true;
                    return None;
                }
                _ => {}
            }
        }
        if let Some(subject) = self.pick {
            let choices = self
                .pick_values()
                .into_iter()
                .map(|(v, _)| v)
                .collect::<Vec<_>>();
            let n = choices.len();
            match k {
                Key::Up if n > 0 => {
                    self.mode_sel = if self.mode_sel == 0 {
                        n - 1
                    } else {
                        self.mode_sel - 1
                    };
                    // **The reader has taken the cursor**, so a settings answer landing from
                    // here on must not move it — see [`App::pick_unseeded`]. A worse defect
                    // than the one the re-seed fixes: a cursor that jumps while they arrow.
                    self.pick_unseeded = false;
                    self.redraw = true;
                    return None;
                }
                Key::Down if n > 0 => {
                    self.mode_sel = (self.mode_sel + 1) % n;
                    self.pick_unseeded = false;
                    self.redraw = true;
                    return None;
                }
                Key::Enter if self.editor.text().is_empty() => {
                    if n == 0 {
                        // A daemon older than protocol 18 sends no choices; the
                        // pane says so rather than cycling a list it made up —
                        // the same words the config pane's mode row says.
                        self.say(if subject == Pick::Model {
                            "this daemon does not send the model list; use `/models PROVIDER/MODEL`"
                        } else {
                            "this daemon does not send the mode list; use `/mode NAME`"
                        });
                        return None;
                    }
                    let name = choices[self.mode_sel.min(n - 1)].clone();
                    return self.take_pick(name);
                }
                _ if self.editor.text().is_empty() && digit_row(&k, n).is_some() => {
                    let at = digit_row(&k, n).unwrap();
                    self.mode_sel = at;
                    return self.take_pick(choices[at].clone());
                }
                Key::Click { y, .. } => {
                    // The arithmetic the last frame did: the card's first
                    // choice sat at `mode_first_row`, and only a row the card
                    // provably drew in full is trusted — `mode_rows_drawn` is
                    // zero when the fit loop or the backstop cut the card, so
                    // a click into a list nobody saw whole moves nothing.
                    let row = usize::from(y).saturating_sub(self.mode_first_row);
                    if n > 0 && row < self.mode_rows_drawn {
                        self.mode_sel = row.min(n - 1);
                        // A click is the reader's too, for the reason the arrows are.
                        self.pick_unseeded = false;
                        self.redraw = true;
                    }
                    return None;
                }
                _ => {}
            }
        }

        // **An open subagent pane owns Up and Down, and ENTER IS THE SWITCH INTO THAT
        // SUBAGENT'S SESSION.** One keystroke, because that is what entering a row means
        // everywhere else in this head and going into a subagent *is* going to that
        // session — the operator, having driven into one and then been unable to get out
        // again: *"when I \"Enter\" Subagent it is like completely switching session"*,
        // and *"so after o I couldnt just Esc from the subagent — had to switch back here
        // via session. Which narrows the subagent prompt - make \"o\" to \"Enter\""*.
        //
        // `o` stays as an alias for the same act: it is the key this pane has always used
        // to move the head, and a hand that learned it must not have to learn something
        // new. What moved is the READ — `p` now, and `p` is `/peek ID`'s own key. Reading
        // a child's output without leaving the session is a real thing to want (R20's whole
        // argument for the `Peek` frame), and it must not be the thing Enter does when the
        // operator means to go there. It is neither Enter nor Esc, which is what the two
        // gestures had to be kept apart from.
        //
        // **And the group row is the third thing Enter means here.** The finished children live
        // under a fold (see [`App::subagent_stops`]); Enter on that row unfolds them, which is
        // the same *Enter acts on what the cursor is on* rule the todos pane keeps. The arrows
        // also scroll the cursor into view now — the fix for the operator's other report,
        // *"subagents panel doesnt scroll"*, which was a child appended below the fold and no
        // key that would bring it up.
        if self.subagents_pane {
            let stops = self.subagent_stops();
            if !stops.is_empty() {
                let n = stops.len();
                let at = self.subagents_sel.min(n - 1);
                match k {
                    Key::Up => {
                        self.subagents_sel = if at == 0 { n - 1 } else { at - 1 };
                        self.scroll_into_view(self.subagents_row_of());
                        self.redraw = true;
                        return None;
                    }
                    Key::Down => {
                        self.subagents_sel = (at + 1) % n;
                        self.scroll_into_view(self.subagents_row_of());
                        self.redraw = true;
                        return None;
                    }
                    // **Enter takes the row, and `o` is the same act as the alias this pane
                    // has always used.** One arm for one behaviour: Enter is unconditional (a
                    // pane owns Enter), and `o` keeps the composer's claim on a letter that is
                    // being typed — half a word on the line falls through to the composer, which
                    // is why the guard is here rather than in a second copy of these six lines.
                    Key::Enter | Key::Char('o')
                        if matches!(k, Key::Enter) || self.editor.text().is_empty() =>
                    {
                        match stops[at] {
                            // **The group row is a fold, not a session**: nobody to switch into,
                            // so Enter is the unfold.
                            SubStop::Finished => {
                                self.subagents_finished_open = !self.subagents_finished_open;
                                self.redraw = true;
                                return None;
                            }
                            SubStop::Agent(i) => {
                                // **Moving, not reading.** The row is a session and Enter goes to
                                // it; one that is not open yet is refused here, by name, rather
                                // than bounced off the daemon.
                                let row = &self.subagents[i];
                                if row.state == "opening" {
                                    // Nothing to attach to yet, and the daemon would refuse the
                                    // switch by name anyway; saying it here keeps the operator in
                                    // the pane they were using rather than bouncing them through
                                    // a rejection.
                                    self.say(
                                        "that subagent is still opening — nothing to attach to yet",
                                    );
                                    self.redraw = true;
                                    return None;
                                }
                                let id = row.session_id.clone();
                                self.subagents_pane = false;
                                return self.switch_to(id);
                            }
                        }
                    }
                    // **Reading, not moving: the output pane opens on the `Peeked` reply and
                    // this head never leaves the session it is in.** Nothing to read under the
                    // fold header, so `p` on it falls through.
                    Key::Char('p') if self.editor.text().is_empty() => match stops[at] {
                        SubStop::Finished => {}
                        SubStop::Agent(i) => {
                            let row = &self.subagents[i];
                            if row.state == "opening" {
                                self.say("that subagent is still opening — nothing to read yet");
                                self.redraw = true;
                                return None;
                            }
                            let id = row.session_id.clone();
                            self.sub_out_pending = Some(id.clone());
                            return Some(Action::Peek(id));
                        }
                    },
                    _ => {}
                }
            }
        }

        // **An open todos pane owns Up and Down, and Enter unfolds the item.**
        //
        // The items carry the detail a TODO.md puts under them — the commit a
        // vendoring pins, the `Deps:` that says what blocks it — and the pane
        // showed the first line only, so an item trailed off mid-sentence. Arrows
        // move, Enter acts: the same two the jobs and subagent panes use.
        // **ONE ENUMERATION, AND EVERY KEY READS IT** — leticl's `todos-stops`, whose docstring is
        // the operator's two reports: *"arrows dont go here"* and *"mouse doesnt click"*. Both were
        // the same defect, a cursor whose position came from one list and whose row came from
        // another. The stops are the add control, the operator's own items, and the repo's items —
        // and NOT the model's rows, which no key acts on.
        //
        // **A key this block does not name FALLS THROUGH**, which is what keeps the pane from
        // eating the composer: `Tab` is the completion key while a `/command` is half-typed (the
        // guard below is the same empty-composer one the card uses), and every ordinary character
        // is the operator's to type. The first cut of this block ended in an unconditional
        // `return None` and the pane swallowed the whole keyboard — a `▸` that looked right with
        // nothing behind it, which is a worse defect than the one it replaced.
        if self.todos_pane {
            let stops = self.todos_stops();
            let n = stops.len();
            let at = self.todos_sel.min(n.saturating_sub(1));
            match k {
                Key::Up => {
                    self.todos_sel = if at == 0 { n - 1 } else { at - 1 };
                    self.todos_sel = self.todos_sel.min(n - 1);
                    self.sync_repo_from_stop(&stops);
                    self.scroll_into_view(self.todos_row_of());
                    self.redraw = true;
                    return None;
                }
                Key::Down => {
                    self.todos_sel = (at + 1) % n;
                    self.sync_repo_from_stop(&stops);
                    self.scroll_into_view(self.todos_row_of());
                    self.redraw = true;
                    return None;
                }
                // **Enter acts on what the cursor is ON**, which is the whole point of one
                // enumeration: the add control opens the card, one of your rows is marked done,
                // and a repo item unfolds. Tab stays the repo's unfold, because the hand is
                // already there for it and the composer is empty here.
                // **A click moves the cursor, and Enter still does the act** — the same two
                // acts, kept two, that the pickers here already keep (*"select and confirm stay
                // two acts"*). Straight off the recorded rows, so the row the pointer is on is
                // the row the pane drew there.
                //
                // **A click into the blank space moves nothing**, and so does one on a row that
                // is not a stop at all — one of the model's rows, or a repo heading. There is no
                // key that would act on it, which is the same reason it is not a stop.
                Key::Click { y, .. } => {
                    if let Some(sel) = self.todo_stop_at_row(y) {
                        self.todos_sel = sel;
                        self.sync_repo_from_stop(&stops);
                        self.redraw = true;
                    }
                    return None;
                }
                Key::Enter | Key::Tab if self.editor.text().is_empty() => match &stops[at] {
                    TodoStop::Add => {
                        self.open_todo_card();
                        return None;
                    }
                    // **Marking done is the model's act too and it is the operator's own row**:
                    // the daemon's `set_operator_states` is its own door and `/todo done N` is the
                    // typed one. Here it is the same act under the cursor. A completed row can be
                    // reopened, because a cursor that can only go one way is a cursor you cannot
                    // correct.
                    TodoStop::Mine(content) => {
                        let content = content.clone();
                        let mut mine = self.operator_todos();
                        if let Some(t) = mine.iter_mut().find(|t| t.content == content) {
                            t.status =
                                if t.status == letibot_sessionlog::event::TodoStatus::Completed {
                                    letibot_sessionlog::event::TodoStatus::Pending
                                } else {
                                    letibot_sessionlog::event::TodoStatus::Completed
                                };
                        }
                        self.say("toggled");
                        self.echo_operator_todos(mine.clone());
                        return Some(Action::SetOperatorTodos(mine));
                    }
                    TodoStop::Repo(i) => {
                        self.repo_sel = *i;
                        self.repo_open = !self.repo_open;
                        self.scroll_into_view(self.todos_row_of());
                        self.redraw = true;
                        return None;
                    }
                },
                _ => {}
            }
        }

        // **An open queue pane owns Up and Down, and Enter opens the row it is on.** The rows
        // are ONE enumeration (`merge`, in the daemon's own order), so the drawn cursor, the
        // arrows and Enter cannot disagree.
        if self.queue_pane && self.queue_open.is_none() {
            if !self.merge.is_empty() {
                let n = self.merge.len();
                let at = self.queue_sel.min(n - 1);
                match k {
                    Key::Up => {
                        self.queue_sel = if at == 0 { n - 1 } else { at - 1 };
                        self.scroll_into_view(self.queue_row_of());
                        self.redraw = true;
                        return None;
                    }
                    Key::Down => {
                        self.queue_sel = (at + 1) % n;
                        self.scroll_into_view(self.queue_row_of());
                        self.redraw = true;
                        return None;
                    }
                    Key::Enter => {
                        // **What Enter opens is the ENTRY, and everything the queue knows
                        // about it**: the ask it was produced under, the state and its reason
                        // — which is the gate's own words when the gate refused it, and the
                        // reviewer's verdict when the reviewer did — and the verdict's
                        // evidence. Nothing here is a second read: the row carries the
                        // evidence and the snapshot carries the verdict, so the overlay is
                        // the two rows the pane already holds, in full.
                        self.queue_open = Some(self.merge[at].id.clone());
                        self.pane_scroll = 0;
                        self.redraw = true;
                        return None;
                    }
                    // **A click moves the cursor, and Enter still opens the entry** — the same
                    // two acts the pickers here keep (*select and confirm stay two acts*),
                    // straight off the rows the pane recorded while drawing.
                    Key::Click { y, .. } => {
                        if let Some(sel) = self.queue_stop_at_row(y) {
                            self.queue_sel = sel;
                            self.redraw = true;
                        }
                        return None;
                    }
                    _ => {}
                }
            }
        }

        // **An open jobs pane owns Up and Down, and Enter reads the row it is on — or folds the
        // group.** The same shape the subagents pane keeps, and for the same reason the
        // operator gave: *"jobs panel - same as subagents - show list of running, group
        // finished"*. The rows are ONE enumeration ([`App::job_stops`]), so the drawn cursor,
        // the arrows and Enter cannot disagree — and the arrows scroll the cursor into view, so
        // a job below the fold is reachable.
        if self.jobs_pane {
            let stops = self.job_stops();
            if !stops.is_empty() {
                let n = stops.len();
                let at = self.jobs_sel.min(n - 1);
                match k {
                    Key::Up => {
                        self.jobs_sel = if at == 0 { n - 1 } else { at - 1 };
                        self.scroll_into_view(self.jobs_row_of());
                        self.redraw = true;
                        return None;
                    }
                    Key::Down => {
                        self.jobs_sel = (at + 1) % n;
                        self.scroll_into_view(self.jobs_row_of());
                        self.redraw = true;
                        return None;
                    }
                    Key::Enter => match stops[at] {
                        // **The group row is a fold, not a job**: nobody to read.
                        JobStop::Finished => {
                            self.jobs_finished_open = !self.jobs_finished_open;
                            self.redraw = true;
                            return None;
                        }
                        JobStop::Job(i) => {
                            if self.session_id.is_empty() {
                                self.say("not attached to a session yet");
                                self.redraw = true;
                                return None;
                            }
                            let row = &self.jobs[i];
                            let job = row.id.clone();
                            // **Where its output went, when it did not come here** (R41). The
                            // window for a redirected job is empty by construction, so the
                            // pane needs the file's name to say anything true at all — see
                            // [`JobOut::redirect`].
                            let redirect = row.redirect.clone();
                            // **The output opens in a pane, not in the conversation.**
                            // This used to return `Action::Slash { "job {job}" }`, whose
                            // reply is a `Warning` on the session log — so the pane closed
                            // and the operator read a build log scrolling past in the chat.
                            // The operator, 2026-09-20: *"when i press enter on jobs pane im
                            // not shown the job output im brought back to the main
                            // conversation with /job <id> posted - this is not what i
                            // want"*. Now the read is a `ReadJobOutput`: it comes back as a
                            // `JobOutput` event with the offsets attached, and the overlay
                            // draws it. The jobs list stays behind it, so Esc returns here.
                            self.job_out = Some(JobOut {
                                job: job.clone(),
                                state: String::new(),
                                never_ran: false,
                                redirect,
                                from: 0,
                                to: 0,
                                produced: 0,
                                dropped: 0,
                                lines: Vec::new(),
                                next: None,
                                back: Vec::new(),
                                scroll: 0,
                                loading: true,
                                error: None,
                            });
                            self.redraw = true;
                            return Some(Action::ReadJobOutput { job, offset: 0 });
                        }
                    },
                    _ => {}
                }
            }
        }
        // **Up with an empty composer recalls the queued line.**
        //
        // The echo above the composer is the operator's own words, held only
        // until the next step boundary — and the one thing this head can do
        // about a message it already sent is take it back before it lands. Up
        // is the key readline taught for "the previous entry", and behind a
        // running turn the queue is one entry now. The take-back rides with the
        // recall: the daemon drops the queued prompts and the held operator
        // text, so the edited resend replaces the original instead of stacking
        // onto it. Parked in the scrollback, Up still scrolls — reading history
        // is what the operator is there for — and a half-typed line keeps the
        // editor's own Up: readline history, not the queue's recall. No take-back
        // rides on browsing history, or one press of Up behind a running turn
        // would silently drop the queue under the operator.
        if matches!(k, Key::Up)
            && self.editor.text().is_empty()
            && self.scroll == 0
            && !self.pending_prompts.is_empty()
        {
            let text = self.pending_prompts.join("\n");
            self.pending_prompts.clear();
            self.set_composer(&text);
            self.redraw = true;
            return Some(Action::WithdrawPrompts);
        }

        // Tab: completion, dispatched by the line's first character. A `/` line
        // completes a command; a `!` line completes from what this session has
        // actually run. The composer's own keys run after it because Tab means
        // nothing to the editor — its byte used to be eaten by the decoder — and
        // every other key leaves a running completion cycle alone: it re-validates
        // its prefix the next time Tab is pressed, so there is nothing to reset in
        // each arm here.
        if let Key::Tab = k {
            if self.editor.text().starts_with('!') {
                self.complete_shell();
            } else {
                self.complete_slash();
            }
            self.redraw = true;
            return None;
        }

        // **Parked in the scrollback, the arrows belong to the scrollback — and the composer
        // does not get them.**
        //
        // The intent was already written down two arms above (*"parked in the scrollback, Up
        // still scrolls"*) and it was never true. The composer is asked FIRST, and an empty
        // composer hands Up straight to the editor's `recall(true)`: that walks readline history
        // and answers `Changed`, so the transcript's own fallback below was reached only by the
        // keys the editor had no use for.
        //
        // MEASURED in the operator's own window, 2026-09-27, on a head that had been up for
        // hours — the three lines are the banner, then three presses of the key it names:
        //
        // ```text
        //   ── holding your place · 138 line(s) below … ↓ to the bottom or esc follows again
        //   Down ×3   ·  139 line(s) below                      ← the number did not move
        //   Up        ·  composer: "on this letibot head scrolll is …"
        //   Down ×3   ·  composer: "yeah scrol…"
        // ```
        //
        // Four presses of the keys the banner advertises, and the reader was no closer to the
        // bottom while an old prompt had appeared in the box. Their report: *"no way to scroll
        // back to the bottom, stuck at holding"*. It was not stuck — the arrows were being spent
        // on history, and a head with a long session had hundreds of entries to spend them on.
        //
        // **And ↓ is the whole of the way back, in one press.** A key that moves one line cannot
        // out-run a stream that adds lines faster, so *"↓ to the bottom"* was unreachable by
        // design as well as by the ordering: three ↓'s against a generating turn moved the count
        // by nothing measurable. The reference does not move one line either — `%normal-key`'s
        // `:down` sets the scroll to ZERO when the reader is parked: *"parked in the scrollback,
        // ↓ follows the stream again — it is what the banner says it does; only then does it move
        // inside the prompt"*. So this arm does what this head's banner already promised, and
        // what `esc` does beside it.
        //
        // **Esc is the way back to the composer**, which is the other key the banner names, and
        // the reason taking ↑ is safe: a reader who wants their draft's arrows back has an
        // advertised key for it rather than a hunt.
        if !self.following() && self.todo_draft.is_none() {
            match k {
                Key::Up => {
                    self.scroll_up(1);
                    self.redraw = true;
                    return None;
                }
                Key::Down => {
                    self.anchor = None;
                    self.scroll = 0;
                    self.redraw = true;
                    return None;
                }
                _ => {}
            }
        }

        // **An open pane owns Enter, even when it has nothing to act on.**
        //
        // The operator, 2026-10-03, having gone to the jobs pane and pressed Enter with a
        // stray character in the composer: *"the pane own keyboard in a way, so enter is a
        // pane thing."* That is the rule this arm is, and it is the rule the arms above now
        // keep — each of them used to gate its own Enter on `editor.text().is_empty()`, so
        // **a pane's Enter silently became "send what I was typing"** the moment there was
        // anything in the composer. What they read as a keystroke aimed at the pane was
        // sent to the model.
        //
        // This is the second half of that: the panes whose blocks above are conditional on
        // having rows (`jobs_pane && !self.jobs.is_empty()`, and the subagent tree's twin)
        // do not run at all over an empty list, and `help`, `stats` and the two overlay
        // screens have no Enter arm. Without this, Enter in an empty jobs pane is still the
        // composer's, which is the same defect with one row fewer on the screen.
        //
        // **What is deliberately NOT here.** The pickers, the mode picker, the decision
        // ladder and the todos stops: for those, a typed line IS the answer — a row number,
        // an id prefix, a name — and `submit` routes it to the right one and holds the
        // words. Swallowing Enter there would break the typed path those panes advertise.
        // The rule is *the pane owns the key*, not *the composer is dead*: a pane that
        // names no meaning for Enter takes it anyway, and one that names a meaning for the
        // typed line keeps it.
        //
        // **Nor is the ctrl-v window**, which was here and should not have been. It is not
        // an overlay over the composer; it is a row of the conversation opened wide, with
        // the composer live underneath and no Enter of its own to lose. Holding Enter for
        // it meant the operator could not send until they closed it: *"untill i hit ctrl-v
        // again - i couldnt sent my new prompt"*. A typed line now goes, and the window
        // closes with it — the reader has moved on to the next turn, and the arrows go
        // back to the conversation and the composer's history. An empty Enter still does
        // nothing, as it does with no window open.
        if matches!(k, Key::Enter) && self.payload_sel.is_some() {
            if self.editor.text().trim().is_empty() {
                return None;
            }
            self.payload_sel = None;
            self.payload_page = 0;
            self.invalidate_history();
            self.redraw = true;
        }
        if matches!(k, Key::Enter)
            && (self.help
                || self.stats
                || self.jobs_pane
                || self.subagents_pane
                || self.slash_out.is_some())
        {
            return None;
        }

        // **A single Esc inside a subagent's session is the way back UP the tree.**
        //
        // `ctrl-s` and a row's Enter was the only way back before this, and it is the
        // gesture the operator had to invent: *"so after o I couldnt just Esc from the
        // subagent — had to switch back here via session … make sure a single Esc goes up
        // to subagents list"*. Enter goes down a level (the pane's arm), Esc comes back
        // up, and the list you came out of is on the screen when you land — which is the
        // tree walk, with no state kept beyond the parent link the daemon already sends.
        //
        // # It sits HERE, after everything else that claims Esc
        //
        // Because Esc already means four things in this head and every one of them keeps
        // its contract:
        //
        // * **Esc closes a pane** — the arm near the top of this function closes help,
        //   the picker, the todos/jobs/subagents/config panes, a `pick` and the quit card,
        //   and *whichever surface prints `esc closes` owns Esc*. None of those says
        //   `esc goes up`, so none of them is shaved: this arm is below all of them.
        // * **Esc un-parks the scrollback**, **Esc closes a payload window** and **Esc
        //   closes a slash listing**, whose seams print exactly that — the arms above,
        //   likewise untouched.
        // * **Esc-Esc is the interrupt frame**, and it is armed by the composer's own
        //   editor below. That one *is* changed while this head is inside a subagent, and
        //   it is the conflict rather than an oversight — see the note under this arm.
        //
        // The empty-composer and no-decision guards are the ones every pane key in this
        // file uses: while a permission is on the screen, or words are half-typed, Esc is
        // not available to mean *up*.
        //
        // # The conflict, reported rather than taken
        //
        // Esc-Esc is the ONLY interrupt key, and it is the editor's (five seconds, two
        // presses). A single Esc that leaves the session and a first Esc that arms an
        // interrupt are the same keystroke, so inside a subagent the arm is what it is:
        // **one Esc goes up, and the pair no longer interrupts the child from inside the
        // child.** Two things bound the cost, and both are existing, tested behaviour
        // rather than something added here: the child's turn can still be interrupted
        // *from the child* with `/interrupt`, and from the parent with `job_kill HANDLE`,
        // which is how this tree already documents stopping a subagent (`harness.rs`: *"a
        // subagent is stopped by interrupting the turn it runs"*). Nothing else about the
        // frame moves: in a session that is not a subagent there is no parent to go to and
        // this arm does not fire, so Esc-Esc is byte for byte what it was.
        //
        // **The one thing this arm does to the frame here, with Esc gone up:** the editor
        // never sees this press, so it is not counted as the first of a pair. That is the
        // true statement — the keystroke went to the tree and not to the composer — but it
        // has a second edge: an Esc the operator pressed in the PARENT, inside the five
        // seconds, can pair with the NEXT press after an up-and-down round trip, and that
        // second press would interrupt the parent. Three presses across a switch inside
        // one window, and it needs the descent arm's press to have been taken by this arm
        // too; the parent presses are otherwise untouched. It is written down rather than
        // fixed because the fix is a way for a head to disarm the editor's pair, and
        // `Editor` publishes no such call — inventing one is a change to the UI crate for
        // a window narrower than the keystroke that opens it.
        if matches!(k, Key::Esc)
            && self.editor.text().is_empty()
            && self.open.is_empty()
            && let Some(parent) = self.parent_session()
        {
            // The rows the operator climbed out of are the ones the pane shows, so the
            // pane opens and the cursor lands on the child they came from — the row is
            // found by id once the parent's `Hello` has rebuilt the list
            // ([`App::fold_subagents`]), because an index taken now would be an index
            // into the rows of the session being left.
            self.subagents_pane = true;
            self.pane_scroll = 0;
            self.up_from = Some(self.session_id.clone());
            return self.switch_to(parent);
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
                // **Busy, not generating.** This gate read the state name, so it was dead for the
                // whole of every command — which is the only time anybody reaches for it. Measured
                // on leticl's head: esc esc during a `sleep 60`, and forty seconds later still
                // `Responding · 42.0s` with the call running. A turn whose only work is a running
                // tool call is exactly the turn a person wants to interrupt.
                if self.turn_busy() {
                    // Interrupt is not quit. A shared session's interrupt is
                    // announced with the issuer, so it must be a deliberate act —
                    // and two presses of Esc inside five seconds is one.
                    Some(Action::Interrupt("operator pressed esc twice".into()))
                } else {
                    self.say("nothing is running");
                    None
                }
            }
            // **The second Ctrl+C asks instead of leaving.** It used to detach
            // and that was the only thing it could do; an operator who wanted
            // the daemon stopped as well needed a second terminal and
            // `letibot --stop`. The card is one keystroke either way and it
            // makes the irreversible half a choice rather than a default.
            Reaction::Quit => {
                self.quit_card = true;
                self.quit_sel = 0;
                self.redraw = true;
                None
            }
            Reaction::Changed => None,
            // The composer had no use for it. Up and Down then belong to the
            // transcript; see the note above.
            Reaction::Idle => {
                match k {
                    // `redraw` here is not "rebuild the frame" — the loop rebuilds
                    // one every pass. It is "throw the glass away", and without it
                    // a scroll repaints only the rows whose TEXT differs, which for
                    // a window that slid by one line over similar rows can be
                    // almost none of them. Every other state change in this file
                    // sets it; these two did not.
                    Key::Up => {
                        self.scroll_up(1);
                        self.redraw = true;
                    }
                    Key::Down => {
                        self.hold(1);
                    }
                    _ => {}
                }
                None
            }
        }
    }

    /// The marked row is the answer: the marker IS the thing Enter takes, the
    /// same contract the pickers keep.
    ///
    /// **Two kinds on one card, and they answer through different frames** (§1.7). A
    /// permission's marked row is an option id and goes as `Action::Answer`; a
    /// question's is an index into the model's offered choices and goes as
    /// `Action::AnswerQuestion`. `None` when the row is not an answer: a permission
    /// that offers nothing, or a question whose model offered no choices and expects
    /// words instead — which the typed path carries, and which a bare Enter must not
    /// turn into an empty answer (the daemon refuses that, and a refusal the head
    /// could have predicted is a keystroke thrown away).
    pub(crate) fn answer_marked(&mut self) -> Option<Action> {
        let d = self.open.first()?;
        let n = decision_rows(d);
        if n == 0 {
            return None;
        }
        let at = self.sel.min(n - 1);
        if d.kind == "question" {
            return Some(Action::AnswerQuestion {
                req_id: d.req_id.clone(),
                answer: letibot_sessionlog::question::QuestionAnswer::choosing(at),
            });
        }
        let option_id = d.options[at].option_id.clone();
        let req_id = d.req_id.clone();
        Some(Action::Answer {
            req_id,
            option_id,
            // The ladder is the no-glob path by construction: there is nothing
            // typed to read one from. A glob is given by typing
            // `allow_always <pattern>` on the line, and a reason the same way
            // with `deny_and_tell <why>` — a `deny_and_tell` taken from the
            // ladder alone denies without a reason and says so, which is honest
            // about what was actually given.
            pattern: None,
            note: None,
        })
    }

    /// **A pane with no daemon is a pane with no program.**
    ///
    /// The pty is the *daemon's*, so a head that cannot reach the daemon cannot feed the
    /// screen and cannot forward a key: the rectangle would sit frozen on whatever the
    /// program drew last, with `ctrl-\` — the one way out, and a frame — going nowhere. The
    /// transcript is the honest thing to show, and this is the two moments it is known:
    /// the link going down, and an action the driver could not send.
    ///
    /// **The daemon's pane is not closed here**, and cannot be: the close is a frame, and the
    /// frame is exactly what cannot be sent. The program is left to the session it belongs to
    /// — the same bargain [`App::load`] makes on a switch, and the same TODO.
    ///
    /// Returns whether there was a pane, so a caller can say so once rather than per report.
    pub fn drop_pane(&mut self) -> bool {
        let had = self.term.take().is_some();
        if had {
            self.redraw = true;
        }
        had
    }

    /// Whether a pane is open **and drawn**, and therefore owns the keyboard. See [`TermPane`].
    ///
    /// The one question `Link::tick` asks before it decides whether a byte this head read is a
    /// key of its own or the program's, and it is asked of `App` because the pane is the
    /// head's state and not the terminal's.
    ///
    /// **A detached pane owns nothing**: the composer has its rows and its keys back, which is
    /// the whole point of leaving. The two questions are deliberately different — *is there a
    /// program here* ([`App::term`]) and *is it on the screen* (this) — and a head that answered
    /// both with one predicate would keep the keyboard for a pane nobody can see.
    pub fn pane_open(&self) -> bool {
        self.term.as_ref().is_some_and(|p| !p.detached)
    }

    /// **Does this head hold a pane at all** — drawn, or detached and still running.
    ///
    /// The third of the three questions about a pane, and the one about the PROGRAM rather
    /// than about the screen: [`App::pane_open`] is *is it being drawn*, [`App::pane_keys`] is
    /// *who gets the keys*, and this is *is there a process behind it*. They come apart in
    /// exactly one state — a detach — and that state is the whole of protocol 34's split: the
    /// pane is held (so `!term` attaches back and `!term close` has something to end) and it is
    /// not drawn (so the conversation has the rectangle and the composer has its keys).
    ///
    /// Public because an integration test that drives the head through the driver's own loop
    /// has to be able to ask it: `pane_open()` answers `false` for a detached pane, and a test
    /// that read *no pane* off it would pass whether the detach kept the program or killed it.
    pub fn holds_pane(&self) -> bool {
        self.term.is_some()
    }

    /// **Leave the pane without ending it** — the `ctrl-\` act. See [`TermPane`].
    ///
    /// The rectangle goes and the conversation comes back; **nothing is sent**, so the program
    /// keeps running on the daemon's pty, the daemon keeps its screen, and the pane's slot stays
    /// occupied — which is what makes a later `!term` an attach to the *same* run rather than a
    /// new one.
    ///
    /// **The head keeps its own copy of the screen and keeps feeding it**, deliberately: the
    /// `TermOutput` frames are still arriving (the pane is the session's and this head is still
    /// attached to the session), so a program that exits while the operator is away still leaves
    /// the row they would have seen had they been looking — with its last rows and its status.
    /// Dropping the screen here would make a death while detached a death with nothing to show,
    /// which is the second half of *a detach must not hide anything*.
    ///
    /// **The sentence names both ways on**, because a person who has just made a program
    /// disappear needs to know it is still there and how to end it if they meant to.
    ///
    /// # It is NOT a notice, and that is the correction
    ///
    /// This said *"{line} is still running — `!term` comes back to it, `!term close` ends it"*
    /// through [`App::say`], and it was the same fact twice: [`App::pane_behind`] draws that
    /// sentence **persistently**, one line above the composer, for as long as it is true. Worse,
    /// the notice is the copy that cannot be taken back — a notice lives for `NOTICE_MS` of wall
    /// time and nothing retires it early, so the `TermEnded` a second later left a sentence on
    /// the screen saying a program was still running, directly above the row saying it had
    /// ended. A fact about NOW belongs in the one place that stops drawing it when it stops
    /// being true.
    ///
    /// Returns whether there was a pane to leave, so a caller can say so once rather than per
    /// report — the shape [`App::drop_pane`] uses for the same reason.
    pub fn detach(&mut self) -> bool {
        let Some(p) = self.term.as_mut() else {
            return false;
        };
        p.detached = true;
        self.redraw = true;
        true
    }

    /// **`!term close` — the ending, and the head asks first.**
    ///
    /// # The four answers, and why the head cannot give three of them
    ///
    /// * **a pane of this head's own** — drawn or detached. The line the operator typed at
    ///   `TermOpen` is in [`TermPane::line`], so the card can name what is about to end;
    /// * **no pane here, but a live one the daemon named** — [`App::term_fact`] is
    ///   [`PaneFact::Running`], and the card names `!term <command>`. This is the case the
    ///   read exists for: the program's screen may be in another head entirely, and the
    ///   operator still means it;
    /// * **no pane at all** — a sentence, and **nothing sent**. `TermClose` is quiet about
    ///   there being nothing to end, so a head that sent it would look like it had done
    ///   something. The sentence names the way to start one;
    /// * **and *not asked yet*** — [`PaneFact::Unasked`] is the state between an attach and the
    ///   daemon's answer to the read, and **the line is held**: the read goes out, and its
    ///   answer runs this same decision (see the `TermStatus` arm). A head that guessed *no
    ///   pane* here would be wrong on the one case that matters — a person who attached a
    ///   moment ago and means the program the daemon is holding.
    ///
    /// **Nothing here ends anything.** Every arm either raises [`App::term_ask`] or says why it
    /// cannot; the frame leaves when the card is answered with a yes, and never before.
    pub(crate) fn begin_close(&mut self) -> Option<Action> {
        match (&self.term, &self.term_fact) {
            (Some(p), _) => {
                let line = p.line.clone();
                self.ask_close(&line);
                None
            }
            (None, PaneFact::Running(command)) => {
                let line = format!("!term {command}");
                self.ask_close(&line);
                None
            }
            (None, PaneFact::None) => {
                self.say(
                    "this session has no pane to end — `!term COMMAND` starts one, and \
                     `!term` attaches to one that is already running",
                );
                self.redraw = true;
                None
            }
            (None, PaneFact::Unasked) => {
                self.close_pending = true;
                self.say("asking the daemon what this session is running…");
                self.redraw = true;
                Some(Action::TermStatus)
            }
        }
    }

    /// **Raise the confirmation card**, and it is the only place [`App::term_ask`] is set — so
    /// there is one place a pane can be ended from, and it is the one that asks.
    pub(crate) fn ask_close(&mut self, line: &str) {
        self.term_ask = Some(TermAsk {
            line: line.to_string(),
        });
        self.redraw = true;
    }

    /// **The pane this session has that this head is not drawing**, as a line a person reads —
    /// or `None` when there is nothing to report.
    ///
    /// Two sources, and the order is the decision: **this head's own pane first** (a detach
    /// keeps the pane, so the head holds the program *and* the line the operator typed), and
    /// then **the daemon's answer** to `ClientFrame::TermStatus`, which is the only thing that
    /// can answer for a session this head has no pane in — a head that switched away and came
    /// back, or a second head attached to the same session.
    ///
    /// **Not a row, and that is the operator's rule.** A detach is not an event: there is no
    /// `SessionEvent` for it, nothing durable happened, and a transcript row saying *you left a
    /// pane* would be a disclosure about a moment that did not change anything. What the head
    /// draws instead is this — a fact about **now**, drawn while it is true and gone the moment
    /// it stops being true.
    pub(crate) fn pane_behind(&self) -> Option<String> {
        // On the screen: the pane itself is the fact, and a sentence about it would be the same
        // fact twice.
        if self.pane_open() {
            return None;
        }
        match (&self.term, &self.term_fact) {
            (Some(p), _) => Some(p.line.clone()),
            (None, PaneFact::Running(command)) => Some(format!("!term {command}")),
            (None, PaneFact::None | PaneFact::Unasked) => None,
        }
    }

    /// **The pane's keyboard: the raw bytes the reader consumed, turned into actions.**
    ///
    /// # The way out is found HERE, and that is what makes it untrappable
    ///
    /// `0x1c` — `Ctrl-\` — is looked for in the byte stream **before anything is forwarded**,
    /// and the bytes before it are the last thing the program gets. A key the program never
    /// receives is a key no program can trap, whatever it does to `SIGQUIT` or to its own input
    /// handling. **And the act it performs is a detach**: the head hides the rectangle and sends
    /// nothing at all, so the program is not signalled, not killed, and not even told — see
    /// [`TermPane`] for why the default is the non-destructive one.
    ///
    /// **The byte cannot be part of anything else.** `0x1c` is below `0x20`, so it is not a
    /// UTF-8 continuation and cannot appear inside a character; and it is not a CSI final byte
    /// (those are `0x40`-`0x7e`), so it cannot appear inside an escape sequence the reader is
    /// holding. It *can* appear inside a bracketed paste — somebody pasting a file that
    /// contains a literal `0x1c` — and the pane detaches: the honest reading of *the operator's
    /// terminal sent the way-out byte*, and a hole named in [`TermPane`] rather than a
    /// silent one.
    pub fn pane_keys(&mut self, raw: &[u8]) -> Vec<Action> {
        let Some(p) = self.term.as_ref() else {
            return Vec::new();
        };
        // An ending is in flight: between the confirmed `!term close` and the daemon's
        // `TermEnded` there is a kill on the way, and a byte written into a pty whose program is
        // being signalled is a byte nobody will read. The window is milliseconds, and this is
        // what makes it closed rather than merely short.
        //
        // **And a DETACHED pane takes no keys either**, which is the same rule from the other
        // end: the composer has its rows and its keys back, so a keystroke the operator aimed at
        // the composer must not reach a program nobody is drawing. `Link::tick` already routes
        // on [`App::pane_open`]; this is the second door, closed rather than left to the caller.
        if p.closing || p.detached {
            return Vec::new();
        }
        match raw.iter().position(|b| *b == WAY_OUT) {
            Some(at) => {
                let mut out = Vec::new();
                if at > 0 {
                    out.push(Action::TermInput {
                        bytes: raw[..at].to_vec(),
                    });
                }
                // **Detach: nothing leaves the head.** Not a frame and not a keystroke — the
                // whole point is that the program keeps running and this head stops drawing it.
                self.detach();
                out
            }
            None if raw.is_empty() => Vec::new(),
            None => vec![Action::TermInput {
                bytes: raw.to_vec(),
            }],
        }
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

    /// **Tab on a `!` line completes from what this session has actually run — and,
    /// when the history has nothing, from what the model proposes.**
    ///
    /// The operator's own words for the feature: *"smart autocomplete here for ! -
    /// you trying to suggest me commands based on conversation context"*, and then
    /// *"i want smart ! when a model suggest completions."* The candidates are whole
    /// lines, newest first, deduped — the operator's own `!` rows verbatim, and the
    /// model's `bash` calls as `! ` plus the command they ran — and the match is a
    /// whole-line prefix, so `! ls` reaches `! ls .`.
    ///
    /// **History first, model second.** The history is the first answer, because a
    /// command this session actually ran is a fact and a model's proposal is a guess,
    /// and a real command beats an invented one. The model is the fallback, asked
    /// only when the history has no match for the prefix — or its cycle is exhausted
    /// — and asked once per (prefix, transcript position), so the same prefix asked
    /// twice is not two model calls.
    ///
    /// **The one recogniser for "is this a `!` line" is `operator_shell_command`**,
    /// the same rule the daemon re-checks at the send: a bang with nothing after it
    /// is not a `!` line, so `!` alone does nothing here, the way it is refused
    /// there. A second list of what counts would be a second answer to the same
    /// question.
    ///
    /// **The cycle is the field `complete_slash` uses, and the same rule holds**: it
    /// only trusts a prefix that is still being typed, so a character typed on after
    /// a completion matches fresh rather than clobbering what was typed, and a
    /// prefix nothing matches leaves the composer exactly as it was and says so.
    /// Nothing is ever submitted — a candidate only fills the composer.
    /// **Move the open payload window** by `by` wrapped lines, clamped to the last full
    /// page the draw recorded (`payload_max`) — so Down stops where the output ends and the
    /// first Up after it moves at once, instead of unwinding steps past the end.
    ///
    /// **And what the window cannot take, the conversation does** — the operator: *"i want
    /// them to connect. so say i scrolled to the bottom of the ctrl-v view port it should
    /// keep scrolling the main convo"*. A window at its last line passes the rest of a
    /// Down to the transcript, and one at its first line passes the rest of an Up, the way
    /// a scroll box nested in a page hands over at its edge. Home and End are jumps inside
    /// the window and pass nothing on.
    pub(crate) fn page_payload(&mut self, up: bool, by: usize) {
        let max = self.payload_max.get();
        let from = self.payload_page.min(max);
        let to = if up {
            from.saturating_sub(by)
        } else {
            from.saturating_add(by).min(max)
        };
        // `max` is only known once the window has been drawn; before that nothing chains,
        // because "at the end" is not yet a fact.
        let rest = by.saturating_sub(from.abs_diff(to));
        let chain = !(by == usize::MAX || rest == 0 || (!up && max == usize::MAX));
        // **The handover first, while the rows are still measured.** Invalidating the history
        // drops the line spans the transcript scroll finds its row by, so a scroll made
        // after it landed nowhere — found by the test: the window was at its head, Up was
        // passed on, and the conversation did not move.
        if chain {
            if up {
                self.scroll_up(rest);
            } else {
                self.hold(rest as isize);
            }
        }
        if to != from {
            self.payload_page = to;
            // **The history buffer is a cache of the rendered rows**, and a page offset
            // changes what one of those rows renders to — so `redraw` alone re-draws the
            // *old* lines.
            self.invalidate_history();
        }
        self.redraw = true;
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

    pub(crate) fn switch_to(&mut self, id: String) -> Option<Action> {
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

    /// **The conversation is about to be replaced** (R16): remember what is in the
    /// air, so the fork can resolve those echoes rather than orphan them.
    ///
    /// Clone rather than a flag, because the list has to survive the echoes being
    /// retired normally in between — a prompt whose row lands before the fork needs
    /// no help from this, and one still waiting does.
    pub(crate) fn mark_fork(&mut self) {
        self.fork_pending = self.pending_prompts.clone();
    }

    /// **The fork happened** (R16): retire the echoes that were waiting on a
    /// transcript that no longer exists.
    ///
    /// The echo's prompt is in the ledger — a fork summarises everything said
    /// before it, which is why the summary exists — so what is retired is the
    /// *mark*, not the words: the conversation above already holds them, as prose
    /// in the summary, and the row that would have carried them was replaced.
    ///
    /// **Only the marked ones.** An echo queued after the fork began belongs to the
    /// new transcript and its row is still coming; retiring it would take a sentence
    /// off the screen that has not landed, which is the defect `pending_prompts`
    /// exists for.
    pub(crate) fn resolve_fork(&mut self) {
        for text in std::mem::take(&mut self.fork_pending) {
            self.retire_pending(&text);
        }
        // **A fork answers the question a snapshot could only raise.** The marks here were
        // `unconfirmed` because the head could not tell *still coming* from *replaced*; a
        // fork the head itself asked for is the second, and the echo is gone with it. Same
        // intersection, so a mark whose echo survived a piece-of-the-row retirement (the
        // engine split the run across a notice) stays until its own row lands.
        let queue: std::collections::HashSet<String> =
            self.pending_prompts.iter().cloned().collect();
        self.unconfirmed.retain(|u| queue.contains(u));
    }

    /// Stand down the echo of a queued prompt whose row has landed.
    ///
    /// **The unit is a LINE, not a message, and that is the whole of the fix.** The
    /// old rule asked whether the landing row *was* the echo, or began with it, and
    /// both questions are about whole strings while the thing being compared is a
    /// **run of prompts**: the engine merges the operator's consecutive prompts into
    /// ONE user item joined by newlines (`SteeringMessage::to_item`), and a head only
    /// sometimes does the same joining itself ([`App::submit`]'s coalescing is
    /// conditional on a turn it thinks is running). So the shapes that meet are
    /// "six prompts in the queue, one six-line row" and "two prompts joined in the
    /// queue, one five-line row" — and against a whole-string rule every one of them
    /// compares NO, which leaves the echo on the screen for the rest of the session.
    ///
    /// Measured on this head, 2026-09-23: six `queued ·` echoes of the R27
    /// instruction's six paragraphs, every one of them answered, none retired —
    /// because the row that answered them was their **join** (2752 characters, six
    /// lines) while the queue held two hundred-to-five-hundred characters per entry.
    ///
    /// So: a row's lines are consumed, once each, by the pending pieces that equal
    /// them as **whole lines**, from the front of the queue backwards. A piece that
    /// consumes nothing keeps its place; an entry left with nothing retires. The
    /// word *whole* is the correctness: a prompt `second thing` is NOT retired by a
    /// row reading `first thing\nsecond thing-guess`, which is the failure that
    /// matters — a head that swallows a prompt the daemon has not answered has put a
    /// sentence the operator typed where nobody will ever see it.
    ///
    /// **What is deliberately not done: matching a substring, or matching pieces out
    /// of order.** A piece is claimed only by a whole line at or after the last line
    /// claimed, so a queue whose pieces appear reversed in a row keeps them (an echo
    /// left standing costs a stale line; an echo wrongly retired costs the
    /// sentence), and a piece that is a *fragment* of a line claims nothing at all.
    ///
    /// A prompt coincidentally EQUAL to one whole line of a longer prompt the
    /// operator typed separately would still retire. That residue is accepted and
    /// older than this fix — it is the same risk [`App::retire_pending`]'s old
    /// front-piece branch carried, and the alternative (never retiring a piece) is
    /// the defect measured above.
    ///
    /// **This is the only thing that retires an echo, and it takes content.** The
    /// announcement of a user row says nothing about whose words it carries — a
    /// harness notice and another head's prompt are the same item — so a head that
    /// retired on [`SessionEvent::TranscriptAppended`] would lose the echo of a
    /// prompt still sitting in the hub's queue, and the operator would watch their
    /// own sentence vanish. See [`App::bound_prompts`] for the half that *is* drawn
    /// from an announcement.
    pub(crate) fn retire_pending(&mut self, row: &str) {
        // **An echo that is not in the queue is not in the unconfirmed set either.**
        // An intersection, not a removal of the row's text.
        //
        // Measured on this surface, 2026-09-23, in leticl's words and true here for the
        // same structural reason: *"an unconfirmed echo's text is never a queued text"*,
        // so `unconfirmed.retain(|u| u != row)` matches nothing — the row's text is the
        // engine's JOIN, not the echo — and an echo marked unconfirmed by an earlier
        // snapshot **whose row later landed inside a merged item** would be retired from
        // the queue and stay in this set for ever. The set is not the echo's text; it is a
        // mark ON an echo, so the only honest update is to keep the marks whose echo is
        // still there.
        let lines: Vec<&str> = row.split('\n').collect();
        // **A line is spent once.** Two prompts that say the same thing stay queued
        // separately until each of their rows lands — the property the equality rule
        // gave, which a rule retiring every matching entry would lose.
        let mut claimed = vec![false; lines.len()];
        let mut cursor = 0usize;
        let mut i = 0usize;
        while i < self.pending_prompts.len() {
            match strip_landed(&self.pending_prompts[i], &lines, &mut claimed, &mut cursor) {
                // Nothing of this entry is in the row. It keeps its place.
                None => i += 1,
                // Every piece of it has landed. The echo stands down.
                Some(rest) if rest.is_empty() => {
                    self.pending_prompts.remove(i);
                }
                // Some of it has. The echo shrinks to what is still owed.
                Some(rest) => {
                    self.pending_prompts[i] = rest;
                    i += 1;
                }
            }
        }
        // **The intersection, taken AFTER the loop.** Building it before is the same
        // defect inverted, and it is how the first version of this fix leaked: the set
        // held the queue as it was when the row arrived, so an echo that had just stood
        // down was still in it — a mark outliving the thing it was a mark on, which is
        // precisely the leak the intersection exists to close. Found by the assertion
        // below this call, on the first run.
        let queue: std::collections::HashSet<String> =
            self.pending_prompts.iter().cloned().collect();
        self.unconfirmed.retain(|u| queue.contains(u));
    }

    /// **Bind the oldest unbound echo to a row that has just been announced.**
    ///
    /// Called for a body-less `user` row, which is the shape of this head's own
    /// prompt *and* of a steering notice, a §5.7 salvage notice and another head's
    /// prompt. The announcement cannot tell them apart, so this is a guess: what it
    /// buys is that the row is drawn from the words the head already holds, at the
    /// position the transcript gave it — above the reply it caused — instead of
    /// being invisible until its body catches up while the reply streams above it.
    ///
    /// Oldest first, and never an echo already bound: several prompts in the air at
    /// once is the normal case behind a running turn, and their rows are announced
    /// in the order they were sent. The echo **stays in `pending_prompts`** — this
    /// binds a drawing, it does not retire anything (see [`App::retire_pending`]).
    ///
    /// The order is: a `BTreeMap`-free [`HashMap`] lookup, one scan of the pending
    /// list, and one clone of the text being bound. `pending_prompts` is one to a few
    /// entries — behind a running turn the engine merges consecutive operator
    /// messages, so it is ordinarily *one* — and this runs once per announced row,
    /// so it is nothing next to the render it is feeding.
    pub(crate) fn bind_echo(&mut self, item_id: &str) {
        if self.pending_prompts.is_empty() {
            return;
        }
        let taken: std::collections::HashSet<&str> =
            self.bound_prompts.values().map(String::as_str).collect();
        let Some(text) = self
            .pending_prompts
            .iter()
            .find(|p| !taken.contains(p.as_str()))
            .cloned()
        else {
            return;
        };
        self.bound_prompts.insert(item_id.to_string(), text);
    }

    /// The echo texts a **body-less row on screen is already drawing**, so the tail
    /// must not draw them a second time.
    ///
    /// Owned rather than borrowed, because the caller holds it across the frame's
    /// disjoint borrow of `self`. It is one entry per prompt in the air — ordinarily
    /// one — and it is built once per frame.
    ///
    /// Derived from `items` on every frame rather than counted, for the reason
    /// [`App::bulk`] gives: `items` is replaced wholesale by a snapshot, so anything
    /// remembered about the rows it replaced describes rows that no longer exist. A
    /// binding whose row has been trimmed out of the view, or replaced by a snapshot,
    /// draws nothing — and this then draws the echo at the tail again, which is the
    /// honest answer: the words are still this head's to show.
    ///
    /// **As LINES, not as whole texts** (R51 item 15). The tail does not ask *is this
    /// entry already on screen* — that question is answered NO for any entry that grew
    /// after it was bound, and the whole entry is then drawn twice. What it needs is the
    /// pieces being drawn, so it can take exactly those out. See [`unclaimed_prompts`],
    /// which is the walk that spends them.
    pub(crate) fn echoes_on_screen(&self) -> Vec<(String, Vec<String>)> {
        self.items
            .iter()
            .filter(|it| it.item.is_none())
            .filter_map(|it| self.bound_prompts.get(&it.item_id))
            .map(|text| (text.clone(), text.split('\n').map(str::to_string).collect()))
            .collect()
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

    /// **Everything a seated head must ASK FOR, in one place — attach, re-attach, and the
    /// return from a switch.**
    ///
    /// # Why an ask at all, when the `Hello` already carries a list
    ///
    /// The operator's report is the whole argument: the composer's `N subagents running`
    /// segment **disappeared and came back on its own** while a subagent ran throughout —
    /// *"so the counter is gone"*, then, minutes later, *"yep and now it is back. wtf"*.
    /// Nothing was restarted between the two. A drawn state that comes back by itself is a
    /// state whose restore is **opportunistic**, and the thing that was restoring it was
    /// some later list reply happening to arrive — the head was not asking for one.
    ///
    /// So every read here is a question the head puts to the daemon at the one moment it
    /// knows it needs the answer, rather than a frame it hopes will land:
    ///
    /// * **`ListSessions`** — the subagent rows and the composer's count are folded from
    ///   the daemon's own list ([`App::fold_subagents`]), and a `Hello` carries a copy of it
    ///   cut at the instant the daemon answered. A copy is not an ask: the fold's source was
    ///   whatever list the last frame happened to hold, and nothing re-asked. This does.
    /// * **`ListJobs`** — the same shape, and the reason is the brief's own: *"for a job
    ///   already running when this head attached, which no event announces."*
    /// * **`Settings`** — the header reads the live `model` row, and the daemon never sends
    ///   the rows unprompted. The operator, on a session answered by deepseek: *"restarted
    ///   the letibot - still qwen"*.
    ///
    /// # Which three moments this is
    ///
    /// There is one call site, in the `Hello` arm, because the daemon answers all three with
    /// the same frame: an **attach**, a **re-attach** (the driver's reconnect is an `ATTACH`
    /// and the daemon answers it with a `Hello` like any other), and the **return from a
    /// switch** — a `Switch` is answered with a second `Hello` on purpose, so that the
    /// late-join path is the only seating path. The return from a *pane* is the same moment:
    /// whether this session has a pane is asked on the same seating, by the same arm (see the
    /// `TermStatus` push above, which is `Action::TermStatus`).
    ///
    /// **The asks are reads and they are cheap.** `ListSessions` and `ListJobs` are answered
    /// off the registry on the connection's own thread, not through the command queue (see
    /// the server's arms), so none of them waits behind a running turn — which is the whole
    /// reason the daemon answers them here rather than as commands.
    ///
    /// **And every answer re-folds.** A re-ask whose reply is not applied is worse than no
    /// re-ask, because it looks like one: the `Sessions` arm calls [`App::fold_subagents`]
    /// and the `Jobs` arm replaces the table, for exactly this reason.
    pub(crate) fn refetch_session_facts(&mut self) {
        self.queued.push(Action::Settings);
        self.queued.push(Action::ListJobs);
        self.queued.push(Action::ListSessions);
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

    /// **The session that spawned this one, or `None` when this head is not in a subagent.**
    ///
    /// Read off the daemon's list and **not** inferred from the id: ids are minted by the
    /// daemon (`s-…-sub-…`) and a head that string-matched them would be inventing a fact
    /// the registry already states — `SessionBrief::parent_session_id`, which is `Some`
    /// exactly for a child. See [`App::fold_subagents`] for the same field used the other
    /// way round.
    pub(crate) fn parent_session(&self) -> Option<String> {
        self.sessions
            .iter()
            .find(|s| s.session_id == self.session_id)
            .and_then(|s| s.parent_session_id.clone())
    }

    /// **Move the running command to the background** — `ctrl-o` and `/promote`.
    ///
    /// The fact to guard is a command running, and the check used to ask whether the
    /// TURN was running instead. They come apart: a terminal turn state can leave a call
    /// unsettled — the comment on the `TurnFinished` arm says so in as many words, and
    /// the engine emits `TurnFinished` on the interrupt paths while a tool is still
    /// executing. The operator, looking at a `◐ Running "cargo test …"` card while the
    /// head said otherwise: *"nothing is running to move to the background"* /
    /// *"how come"*.
    ///
    /// So it asks the calls. The daemon honours a promote inside `bash`'s own wait loop,
    /// which exists only while a command is executing, so a running call is not a proxy
    /// for the thing being promoted — it IS it.
    pub(crate) fn promote(&mut self) -> Option<Action> {
        if self.running_call().is_some() {
            self.say("moving the running command to the background");
            return Some(Action::Promote);
        }
        // Two different silences, and a head that said the same thing for both sent the
        // operator looking for a command that had not been started yet. **Busy, not generating**: a
        // turn waiting on a call is still working, and *the model is still working* is the true
        // sentence for it.
        if self.turn_busy() {
            self.say("the model is still working — there is no command running to move yet");
        } else {
            self.say("nothing is running to move to the background");
        }
        None
    }

    /// **The command running right now**, whatever the turn's own state says.
    ///
    /// Ctrl+O's precondition, and deliberately not [`App::turn_busy`] either: this asks for a
    /// command the daemon is *executing*, which is a fact about ONE call and not about the turn.
    pub(crate) fn running_call(&self) -> Option<&CallRow> {
        self.turn
            .as_ref()?
            .calls
            .iter()
            .find(|c| matches!(c.state, CallState::Running))
    }

    /// Attach content to a transcript row, from whatever route the daemon offers.
    pub fn record_item(&mut self, item_id: &str, item: TranscriptItem) {
        let prose = matches!(item, TranscriptItem::Assistant { .. });
        // **A reasoning row takes over the reasoning it carries** — the mark advances to the end
        // of what has arrived, so the live count is only ever the part no row holds. See
        // [`TurnPane::reasoned_upto`]: without this the count includes landed reasoning twice, and
        // the round boundary then makes it fall.
        if matches!(item, TranscriptItem::Reasoning { .. })
            && let Some(t) = self.turn.as_mut()
        {
            t.reasoned_upto = t.reasoning.raw().len();
        }
        // **Content ends the binding, either way.** Confirmed: the row renders from
        // its real body and the echo retires by text below. Contradicted: the row was
        // never this head's prompt — a steering notice, a §5.7 salvage notice,
        // another head's prompt — and the echo is still in `pending_prompts`, so it
        // goes back to the tail where it belongs. Either way the guess has served its
        // purpose, and neither branch may retire on the announcement instead — see
        // `App::retire_pending`.
        self.bound_prompts.remove(item_id);
        // **A body landing takes its id off the bulk announcement**, so the count follows
        // the evidence and not a clock — and an empty set means the carry is complete and
        // the trigger clears itself.
        if let Some(b) = self.bulk.as_mut() {
            b.ids.remove(item_id);
            if b.ids.is_empty() {
                self.bulk = None;
            }
        }
        // A user row with body is the transcript taking a queued prompt over. The
        // steering path appends the operator's words verbatim
        // (`SteeringMessage::to_item`: "a plain `User` item with exactly its own
        // text"), so the text is the match — and one row retires one entry, so two
        // prompts that say the same thing stay queued separately until each of
        // their rows lands.
        // **Every text part, like [`App::load`].** This read only the FIRST part, and
        // the snapshot path read every one — so the two paths could retire different
        // things from the same row, which is the drift leticl measured on its own head
        // (*"the live arm read only the FIRST text part where the snapshot path reads
        // every part"*). One call site each; the common case is one part holding the
        // engine's join, and a two-part item is two things said.
        if let TranscriptItem::User { parts, .. } = &item {
            for text in parts.iter().filter_map(|p| match p {
                UserPart::Text { text } => Some(text.as_str()),
                _ => None,
            }) {
                self.retire_pending(text);
            }
        }
        let Some(idx) = self.items.iter().position(|r| r.item_id == item_id) else {
            // **A body with no row to land on — counted, never silent** (R17).
            //
            // This used to be a bare `return`, and it is the third way a row the
            // ledger has can be missing from the screen: the announcement was
            // replaced by a snapshot that no longer carries this id, so the words
            // arrive with nowhere to go and are thrown away. Nothing said so, and
            // nothing could — the row is not in `items`, so there is not even a
            // placeholder to notice.
            //
            // It is counted rather than made to work because there is nothing to
            // recover: an out-of-order body for a row nobody has is exactly the
            // case a snapshot exists to resolve. What can be wrong here is the
            // *frequency*, and a number is how that becomes visible.
            self.orphan_bodies += 1;
            self.note(Note::Warned(Warned {
                code: "orphan_body".into(),
                detail: format!(
                    "a row's content arrived for `{item_id}`, which this head is not \
                     holding — a snapshot replaced the rows and this one was not in it, \
                     so its words have nowhere to land and are recorded only here. \
                     `/status` counts how often this has happened; a body that arrives \
                     for a row that is gone is not a rendering choice."
                ),
                ts: 0,
            }));
            self.redraw = true;
            return;
        };
        self.items[idx].item = Some(item);
        // **A body landing is the transcript moving too.** The row was announced with no
        // content, so it carried no tool calls a moment ago: a `!` candidate list built
        // then is missing every command this row ran, and a model asked then was asked
        // about a row that had not arrived. See [`App::the_rows_moved`].
        self.the_rows_moved();
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
    /// The disclosure line: the read mark, what this head suppressed, what the
    /// daemon will never send, and what it stripped on the way.
    ///
    /// Ordered by how likely it is to matter, and truncated from the right, because
    /// on an 80-column terminal the old line lost `dropped`, `scrubbed` and
    /// `resync` to the ellipsis — the three numbers whose whole purpose is to be
    /// impossible to miss. Anything nonzero is promoted to the front.
    pub(crate) fn status_line(&self, w: usize) -> String {
        if !self.alarmed() {
            return String::new();
        }
        let p = self.cfg.palette();
        let mut said = format!(
            "⚠ dropped {} · scrubbed {} · resync {}",
            self.dropped, self.scrubbed, self.resyncs
        );
        // **Named, not only counted.** The other three are facts about what this head
        // did with what it was given; this one is a fact about the wire, and it is
        // the only one that means "you are running two different builds".
        if self.unreadable > 0 {
            said.push_str(&format!(
                " · unreadable {} (frames this head could not read)",
                self.unreadable
            ));
        }
        // **R17, and the reason it is on the border and not only on `/status`.** A gap
        // is a hole in the conversation in front of the reader: rows are missing from
        // the middle of what they are reading, and nothing else on the screen says so.
        // The two whose absence is *not* an alarm are deliberately absent here —
        // `behind` is an ordinary backlog, and `orphan` is a body for a row that is
        // already gone — and both are on `/status` where the whole set is read.
        if self.gaps > 0 {
            said.push_str(&format!(
                " · gaps {} (events that never arrived; a resync was asked for)",
                self.gaps
            ));
        }
        said.push_str(" · /status");
        trim_to(&p.paint(Role::Attention, &said), w)
    }

    /// `/status`: this head's own instrumentation, with what each number means.
    ///
    /// The gloss is the part the border could never carry, and it is the reason
    /// the counters are worth keeping at all — `scrubbed 4` is not actionable
    /// unless you know that scrubbing is what a *late* head does to an
    /// interactive-only frame, at which point it is the answer to "why is this
    /// head quieter than the one next to it".
    pub(crate) fn status_lines(&self, w: usize) -> Vec<String> {
        let p = self.cfg.palette();
        let mut out = vec![p.paint(Role::Strong, "this head"), String::new()];
        // **The screen says what it just did** (R51 item 17). Opening it acknowledged the alarm,
        // and a mark that vanishes with nothing said is a mark the reader cannot tell from a bug —
        // the numbers below are unchanged, which is precisely why the sentence is owed.
        //
        // Only when there was something to acknowledge, and only while it is true: on a first read
        // of a clean head there is nothing to say, and a permanent sentence about a mark that is not
        // there is the furniture this file keeps deleting.
        if self.acked != Counters::default() {
            out.push(dim(
                &self.cfg,
                "  the alarm is acknowledged up to the values below — the ⚠ is gone, and any \
                 counter that moves again brings it back",
            ));
            out.push(String::new());
        }
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
            format!("{} ({})", self.filtered, self.visibility.as_str()),
            "Events this head chose not to show at the current filter. \
             /verbosity with nothing after it shows every profile and what each gives you.",
        );
        // **R10: the notes this head holds, and how many the reader has retired.**
        //
        // Present and at zero, like every other counter here (§13.2b): "the reader
        // has retired nothing" and "this head does not count what it retired" must
        // not look the same, and the second is what every head did before this.
        // It is beside `filtered` because it is the same kind of number — a fact
        // about what was chosen NOT to be shown — and a different one from
        // `dropped`, which is a fact about what is gone.
        row(
            "notes",
            format!(
                "{} · {} retired{}",
                self.notes.len(),
                self.retired_notes(),
                // **A third number, because there is a third reason a note is not on the
                // screen** (R19): the reader retired it, or it is from before this window
                // and was never planted. Both are facts about what was chosen not to be
                // shown; they are not the same choice, and the zero case says nothing
                // rather than "0 from before this window" on every attach.
                match self.notes_before() {
                    0 => String::new(),
                    n => format!(" · {n} from before this window"),
                }
            ),
            "What this head is holding: a guard that fired, a decision that settled, a \
             sentence the daemon interrupted with. A retired note is HIDDEN, and still \
             here — `/notes` lists every one with its text and `/notes restore` puts the \
             retired ones back, which is the difference between a disclosure and a \
             deletion. The session log holds them either way; a note is how a head shows \
             a durable fact once. **A note from before this window** is one that arrived \
             with a snapshot: it happened before this head attached, so it is listed and \
             counted rather than planted in a conversation it did not precede (R19).",
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
        // **R17: the three numbers that tell a lost row from a late one** (R17).
        //
        // They are three rows rather than one because they are three different
        // facts about three different places, and the whole reason this defect
        // survived a night of measurement is that they looked the same:
        //
        // * `gaps` — the wire lost them.
        // * `behind` — the daemon still has them; they are in a queue, on a socket,
        //   or in this head's channel. Not lost, just not here. A head with a
        //   non-zero `behind` and an empty frame queue is *correct* and *not
        //   current*, which is the state nobody could name from outside.
        // * `orphan` — the wire delivered them and there is no row to put them on.
        row(
            "gaps",
            self.gaps.to_string(),
            "Times the log's seq jumped, which means events the daemon sent never \
             reached this head. Counted and said because a gap repaired in silence \
             looks exactly like a session that never had one — and then nobody learns \
             that a socket, a queue or a compaction is losing rows.",
        );
        row(
            "behind",
            self.behind.to_string(),
            "How far the daemon last said it was ahead of this head, in events. A head \
             that is behind has drawn everything it was given and has nothing to draw \
             — the same screen as a head that is current. This is the number that tells \
             the two apart, from the seq the daemon states on an `Accepted` or a \
             `Rejected`.",
        );
        // **Present and zero, like every counter here.**
        row(
            "orphan",
            self.orphan_bodies.to_string(),
            "Bodies that arrived for rows this head is not holding. The words cannot be \
             drawn — a snapshot replaced the rows and this one was not in it — so the \
             count is the only trace they leave.",
        );
        // **A diagnostic that is not an event in the conversation.** `model_slow_first_byte`
        // is the provider being slow to start answering; nothing about the turn is wrong
        // and nothing about the conversation changed, so it is a number to look at when the
        // triangle is up rather than a sentence between two messages — the operator's
        // ruling, and the rule is written down in `warning::ALARM_ONLY`.
        //
        // **The code is in the gloss, and that is not decoration.** Moving the note off the
        // screen took the one place the daemon's own name for this fact appeared — and the code
        // is the word a reader greps the session log for, so the row that replaced it owes
        // them the spelling. Every other row here is named by its code already
        // (`unreadable`, `orphan`, `gaps`); this is the one that had to move, so this is the
        // one that has to say where it went.
        row(
            "first byte",
            self.slow_first_byte.to_string(),
            "Times the provider took longer than this head's patience to send the first \
             byte of an answer — the daemon's code for it is `model_slow_first_byte`. \
             Nothing is wrong with the turn, which is why this is a count here and not a row \
             in the conversation: the latency is a fact about the provider now, and the \
             conversation is not different because of it.",
        );

        // **And the NAMES, which is the pair `orphan` was missing.**
        //
        // The operator, 2026-09-22, looking at `2 row(s) announced and never filled in` for a
        // day: *"tell me if i need to restart anything"*, and *"WHICH two rows, named — an
        // ordinal or an id, not a count, because a count is the least useful form of this
        // fact."* `orphan` counts bodies with no row; this is the other direction — rows with
        // no body — and it printed only a count for the same reason the line does.
        //
        // On `/status` rather than on the line because the line is trimmed to the frame and a
        // name is long: 80 columns hold a count and about three ids, and this pane holds all of
        // them, which is the form a reader can act on.
        let unfilled: Vec<&str> = {
            let mut ids: Vec<&str> = self
                .bulk
                .as_ref()
                .map(|b| b.ids.iter().map(String::as_str).collect())
                .unwrap_or_default();
            ids.sort_unstable();
            ids
        };
        row(
            "unfilled",
            unfilled.len().to_string(),
            &format!(
                "Rows a SNAPSHOT announced whose content never arrived. Named, because a \
                 count is not checkable: {}",
                if unfilled.is_empty() {
                    "none".to_string()
                } else {
                    unfilled.join(", ")
                }
            ),
        );
        // **Present and zero, like every other counter here.** §13.2b: an absent
        // field and a zero field must not look the same. A head that has never met a
        // frame it could not read says `0`, which is a different statement from a
        // head that does not count them at all — and the second is what every head
        // did before this bucket existed.
        row(
            "unreadable",
            self.unreadable.to_string(),
            "Frames that arrived and could not be parsed. Almost always a daemon \
             newer than this head: the frames the two share read fine, and the first \
             one they do not is this. The head stays attached and says so on the \
             border once it has happened; nothing is acked for one, because nothing \
             was read.",
        );
        // **The version, always present.** §13.2b in the other direction from the
        // counters: the question "which build is on the other end of this socket" has
        // no answer anywhere else on the screen, and its answer is the first thing to
        // check when a head behaves strangely. `None` reads as "not told yet", which is
        // a different statement from a version number — the same distinction the empty
        // transcript banner draws.
        row(
            "protocol",
            match self.daemon_protocol {
                None => "not told yet".to_string(),
                Some(d) if d == letibot_sessionlog::protocol::PROTOCOL_VERSION => format!(
                    "{d} · the same build as this head (protocol {})",
                    letibot_sessionlog::protocol::PROTOCOL_VERSION
                ),
                Some(d) => format!(
                    "{d} · this head speaks {} — {} build",
                    letibot_sessionlog::protocol::PROTOCOL_VERSION,
                    if d > letibot_sessionlog::protocol::PROTOCOL_VERSION {
                        "NEWER"
                    } else {
                        "OLDER"
                    }
                ),
            },
            "The protocol both halves were built against, compared at the handshake. \
             A NEWER daemon sends frames this build may not know: they are reported as \
             they arrive and skipped. An OLDER one cannot read a command it has never \
             heard of, and answers that by closing the connection — so a session with \
             an older daemon can end on the next thing you type, and a restart of the \
             daemon is the fix either way.",
        );
        // **Which PROCESS is on the other end of this socket** (R30). The protocol row
        // answers *which build*; this answers *which daemon*, and it is the fact the
        // operator reached for with `ps` a day after a stop that did not happen — which is
        // exactly how that orphan was found. `SO_PEERCRED` on this head's own connection,
        // so it is this daemon and not a pid out of a file that may be a predecessor's.
        //
        // A head does not signal it. Knowing which process is at the other end and
        // reaching around the protocol to signal it are different acts, and R30 keeps the
        // second out while making the first available.
        row(
            "daemon",
            match self.daemon_pid {
                Some(p) => p.to_string(),
                None => "not told (the kernel did not name the peer)".to_string(),
            },
            "The process serving this connection, from SO_PEERCRED — the same number `ps` \
             shows. `letibot --stop` asks it to stop over the protocol, and this head asks \
             with the quit card's second row: that one WAITS until the daemon has gone and \
             says so on stderr if it has not, so a stop that did not happen is not \
             something you find out a day later.",
        );
        row(
            "verbosity",
            self.visibility.as_str(),
            "What reaches the transcript at the current filter — a SET of switches, and the \
             profile is the name of one. `/verbosity` with nothing after it shows every \
             profile and what each one gives you — conversation, read-edits, terse, normal, \
             loud — and `/verbosity NAME` sets one; a switch moved off a profile reads as \
             `custom …`. It used to sit on the composer's border, which was a row of \
             attention paid for ever for a fact read once.",
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

    /// Drop the transient notice, once the operator has had a frame to see it, and stop
    /// whatever clock it started.
    pub fn clear_notice(&mut self) {
        self.notice = None;
        self.notice_until = None;
    }
}

/// **The option a typed line names** — the answer, or why there is not one.
///
/// # Three outcomes, not an `Option`
///
/// *"nothing answers to that name"* and *"several things do"* are different answers and
/// the caller has to treat them differently: the first is the mid-typing courtesy (hold
/// the words, answer the marked row, because a permission that arrives under somebody's
/// half-thought must not turn their Enter into a wasted keystroke), and the second is a
/// refusal that must answer **nothing at all**.
///
/// # The glob, and where it may ride
///
/// > *"please add globbing to my answers somehow too"*
///
/// `allow_always crates/**/tests/*.rs` answers the permission AND says what the rule
/// should cover, instead of accepting the pattern the gate derives from the one call in
/// front of you. The two halves split on the first space; everything after it is the
/// pattern, verbatim and un-lowercased — a glob is a path and `Cargo.toml` is not
/// `cargo.toml`.
///
/// A pattern is only meaningful with `allow_always`, which is the only option that writes
/// a rule. Typed after anything else it is **refused** rather than dropped: somebody who
/// wrote `allow_once src/**` meant the rule to cover `src/**`, and silently granting one
/// call instead is the answer they did not give.
///
/// # A name that fits several options is refused, not resolved
///
/// The prefix path in [`match_option`] is a courtesy for how people actually type
/// (`deny` for `deny_and_tell`), and it stops at one: **a prefix that begins more than
/// one option id is refused, and the candidates are named.** This was
/// `.find(starts_with)`, which took the first hit in list order, so `allow` silently
/// answered `allow_once` out of `allow_once`, `allow_session`, `allow_always` — and
/// which of three grants the operator gave is the entire content of the answer. Where an
/// option sits in a list is not something they said. This is a gate: a grant invented by
/// list position is an answer nobody gave, and the audit row it writes names an option
/// the operator cannot see on the card.
///
/// The reference implementation can afford first-match only because its fallback for a
/// line that names nothing is to answer the marked row anyway — once that fallback
/// declines, as this one does (it holds the words and says so), first-match stops being
/// a convenience and becomes a different answer from the one that was typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OptionChoice {
    /// Exactly one option answers to the line. This is the operator's answer, and the
    /// caller sends it.
    One {
        option_id: String,
        pattern: Option<String>,
        note: Option<String>,
    },
    /// The first word of the line begins more than one option id, so the operator has
    /// not said which one they mean. Refused, with the candidates named.
    Ambiguous {
        /// What they typed, echoed back so the sentence reads as a reply.
        word: String,
        /// The option ids it could have meant, in card order.
        candidates: Vec<String>,
    },
    /// Nothing on the card answers to this line.
    Unnamed,
}

/// The options whose id **begins with** `t`, as indices into the card — all of them.
///
/// All of them, and not the first: naming the candidates is the whole of the refusal in
/// [`OptionChoice::Ambiguous`], and a helper that returned the first hit is the exact
/// shape of the bug this exists against. Case-folded, because an id is typed by a person
/// and `allow_once` is spelled the same way in every case.
///
/// An empty `t` has no candidates: an empty line is the composer's, and every id starts
/// with the empty string, which would make every card ambiguous.
pub(crate) fn option_candidates(d: &OpenDecision, t: &str) -> Vec<usize> {
    if t.is_empty() {
        return Vec::new();
    }
    d.options
        .iter()
        .enumerate()
        .filter(|(_, o)| o.option_id.to_ascii_lowercase().starts_with(t))
        .map(|(i, _)| i)
        .collect()
}

/// **The sentence an ambiguous prefix gets.** The candidates by name, and the two ways
/// out — finish typing, or use the arrows — because a refusal that does not say what to
/// do next is a head that has stopped listening.
///
/// The same shape `App::pick` and `App::pick_mode` give the same problem for their own
/// lists ("{n} sessions match …; type the number on the left instead"): this file's answer
/// to an ambiguous name, in the place the operator is already reading.
pub(crate) fn ambiguous_option_line(word: &str, candidates: &[String]) -> String {
    /// Enough to name the difference, few enough to stay on one line. A card past this
    /// says how many there are, which is the fact that matters when the list is long.
    const SHOWN: usize = 6;
    let shown = candidates
        .iter()
        .take(SHOWN)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    let more = if candidates.len() > SHOWN {
        ", …"
    } else {
        ""
    };
    format!(
        "`{word}` starts {} options here: {shown}{more} — finish typing, or ↑↓ then enter",
        candidates.len(),
    )
}

pub(crate) fn match_option(d: &OpenDecision, typed: &str) -> OptionChoice {
    let line = typed.trim();
    let (word, rest) = match line.split_once(char::is_whitespace) {
        Some((w, r)) => (w, r.trim()),
        None => (line, ""),
    };
    let t = word.to_ascii_lowercase();
    // **An exact name is an answer, and it is checked first.** An equality cannot be
    // ambiguous, so it is never the case this requirement is about: `allow_once` is the
    // spelling the card prints beside every option, and a label that is one word
    // (`Deny`) resolves from the same rule. Only the *prefix* path can be ambiguous.
    let exact = d.options.iter().position(|o| {
        o.option_id.eq_ignore_ascii_case(&t) || (!t.is_empty() && o.label.to_ascii_lowercase() == t)
    });
    let at = match exact {
        Some(i) => i,
        None => {
            // **A prefix that fits more than one option is refused, not resolved.**
            //
            // This was `.or_else(|| …find(|o| o.option_id.starts_with(&t)))`, which took
            // the first hit in list order — so `allow` silently answered `allow_once`
            // out of `allow_once`, `allow_session`, `allow_always`. **Which of three
            // grants the operator gave is the whole content of the answer**, and where
            // an option sits in a list is not something they said. A head may take a
            // prefix that names exactly one option (that is how people actually type);
            // it may not choose among several.
            let candidates = option_candidates(d, &t);
            match candidates.as_slice() {
                [] => return OptionChoice::Unnamed,
                [only] => *only,
                _ => {
                    return OptionChoice::Ambiguous {
                        word: word.to_string(),
                        candidates: candidates
                            .into_iter()
                            .map(|i| d.options[i].option_id.clone())
                            .collect(),
                    };
                }
            }
        }
    };
    let id = &d.options[at];
    if rest.is_empty() {
        return OptionChoice::One {
            option_id: id.option_id.clone(),
            pattern: None,
            note: None,
        };
    }
    match id.kind {
        // A glob, for the option that writes a rule.
        letibot_sessionlog::event::OptionKind::AllowAlways => OptionChoice::One {
            option_id: id.option_id.clone(),
            pattern: Some(rest.to_string()),
            note: None,
        },
        // **The reason, for the option that promised one.** `deny_and_tell` is
        // labelled *"Deny, and tell the model why"* and typing the why used to
        // land here and be refused — the line stayed in the composer and nothing
        // was answered at all. The operator: *"deny and tell doesnt work - there
        // is no input for the 'tell' part"*.
        letibot_sessionlog::event::OptionKind::RejectAlways => OptionChoice::One {
            option_id: id.option_id.clone(),
            pattern: None,
            note: Some(rest.to_string()),
        },
        // Everything else refuses trailing words rather than dropping them:
        // somebody who typed them meant them, and answering as though they had
        // not is the answer they did not give.
        _ => OptionChoice::Unnamed,
    }
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
/// A row number typed on a card: `'1'`..`'9'` to a 0-based index, within `n`.
///
/// **Nine, not more.** A card with ten rows would make `1` ambiguous between
/// row one and the start of row twelve, and the fix for that is a composer that
/// collects digits — which is what the session picker already does and is why
/// this is not used there. Every card that uses it has a handful of rows; when
/// one grows past nine, its tenth row is reachable by the arrows and by typing,
/// and nothing here silently picks the wrong one.
///
/// The operator asked for it on all three cards at once (2026-09-17): *"it
/// shows numbered lists anyway so me pressing row number should constitute
/// focus and enter"*.
pub(crate) fn digit_row(k: &Key, n: usize) -> Option<usize> {
    let Key::Char(c) = k else { return None };
    let d = c.to_digit(10)? as usize;
    (1..=n.min(9)).contains(&d).then(|| d - 1)
}

/// **How many rows a card's ladder has** — and one question for both kinds, because
/// the two kinds do not carry their rows in the same field (§1.7).
///
/// A `permission` puts its allowed answers in `options`. A **`question` carries
/// `options: []`** and puts the model's offered choices in `choices`
/// (`Vec<String>`), so a head that asks `options.len()` gets `0` for every question —
/// which is exactly how this head came to be unable to answer one at all: the ladder
/// bound was zero, `answer_marked` returned `None`, and a typed line was held with
/// *"this ask offers no options"*.
///
/// One function rather than a condition at each of the four call sites, because those
/// four have to agree about it: the bound the arrows wrap on, the bound the digits
/// use, the row `answer_marked` takes, and the rows the card draws.
pub(crate) fn decision_rows(d: &OpenDecision) -> usize {
    if d.kind == "question" {
        d.choices.len()
    } else {
        d.options.len()
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

mod visibility;
pub use visibility::*;

#[cfg(test)]
mod tests;
