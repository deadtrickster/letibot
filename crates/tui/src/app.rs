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

use letibot_sessionlog::event::{DeltaTarget, SessionEvent};
use letibot_sessionlog::protocol::ServerFrame;
use letibot_sessionlog::view::{
    CallState, OpenDecision, SettledDecision, Snapshot, SnapshotItem, TurnState, Warned,
};
use letibot_transcript::{TranscriptItem, UserPart};

use letibot_ui::editor::{Editor, Reaction};
use letibot_ui::style::Role;
use letibot_ui::{card, progress, width};

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
    Answer { req_id: String, option_id: String },
    Resync,
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
    /// Repaint from scratch.
    CtrlL,
    PageUp,
    PageDown,
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
            Key::CtrlR | Key::CtrlT | Key::CtrlL | Key::PageUp | Key::PageDown => return None,
        })
    }
}

/// How much of a foldable thing is on the screen.
///
/// Two states and a key that flips them, rather than a per-item toggle: there is
/// no pointer here and no selection, so a per-item affordance would need a cursor
/// mode, and a cursor mode is a second keymap for a head whose whole input surface
/// is one line. The **discoverability** is bought instead by the fold's own header
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
/// line. See `crates/ui/DESIGN.md` §2.3.
#[derive(Debug, Clone)]
struct CallRow {
    call_id: String,
    name: String,
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
    session_id: String,
    head_id: String,
    items: Vec<SnapshotItem>,
    hist_lines: Vec<String>,
    hist_upto: usize,
    hist_width: usize,
    turn: Option<TurnPane>,
    open: Vec<OpenDecision>,
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
    notice: Option<String>,
    /// Frames the notice has left. A notice that never expires becomes furniture,
    /// and the old one replaced the input line for the rest of the session.
    notice_ttl: u32,
    help: bool,
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
    /// running — which is most of the time a person is looking at it.
    model: String,
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

impl App {
    pub fn new(cfg: RenderConfig) -> Self {
        App {
            cfg,
            verbosity: Verbosity::Normal,
            session_id: String::new(),
            head_id: String::new(),
            items: Vec::new(),
            hist_lines: Vec::new(),
            hist_upto: 0,
            note_upto: 0,
            hist_width: 0,
            turn: None,
            open: Vec::new(),
            notes: Vec::new(),
            heads: 0,
            seq: 0,
            dropped: 0,
            scrubbed: 0,
            resyncs: 0,
            rendered: 0,
            filtered: 0,
            scroll: 0,
            editor: Editor::new(),
            model: String::new(),
            reasoning: Fold::Folded,
            tools: Fold::Folded,
            notice: None,
            notice_ttl: 0,
            help: false,
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

    pub fn open_decisions(&self) -> &[OpenDecision] {
        &self.open
    }

    /// Apply one frame. Never sends anything; see the module note on acking.
    pub fn apply(&mut self, frame: ServerFrame) -> Disposition {
        match frame {
            ServerFrame::Hello {
                session_id,
                head_id,
                dropped,
                snapshot,
                scrubbed,
                ..
            } => {
                self.session_id = session_id;
                self.head_id = head_id;
                self.dropped += dropped;
                self.scrubbed += scrubbed.total();
                if let Some(s) = snapshot {
                    self.load(*s);
                }
                Disposition::Control
            }
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
        self.session_id = s.session_id;
        self.seq = s.seq;
        self.dropped = self.dropped.max(s.dropped);
        self.items = s.items;
        self.invalidate_history();
        self.open = s.open_decisions;
        // Everything in a snapshot is history and none of it is anchored, so it
        // goes at the top rather than being invented a position among the rows.
        self.notes = s
            .warnings
            .into_iter()
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
            SessionEvent::TurnStarted {
                turn_id,
                model,
                ledger_head: _,
            } => {
                self.model = model.clone();
                self.turn = Some(TurnPane {
                    turn_id,
                    model,
                    state: Some(TurnState::Running),
                    started_ms: ts,
                    last_ms: ts,
                    ..TurnPane::default()
                });
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
                }
            }
            SessionEvent::ToolCallProposed { call_id, name, .. } => {
                if let Some(t) = self.turn.as_mut() {
                    t.calls.push(CallRow {
                        call_id,
                        name,
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
                    match t.calls.iter_mut().find(|c| c.call_id == call_id) {
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
                    .and_then(|t| t.calls.iter_mut().find(|c| c.call_id == call_id))
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
                ..
            } => {
                if let Some(t) = self.turn.as_mut()
                    && let Some(c) = t.calls.iter_mut().find(|c| c.call_id == call_id)
                {
                    c.ended_ms = ts;
                    c.note = None;
                    c.state = CallState::Finished {
                        outcome,
                        payload_digest,
                        inline_bytes,
                        full_bytes,
                        spill,
                    };
                }
                Disposition::Rendered
            }
            SessionEvent::DecisionRequested {
                req_id,
                kind,
                call_id,
                summary,
                options,
                deadline,
                on_timeout,
            } => {
                self.open.retain(|d| d.req_id != req_id);
                self.open.push(OpenDecision {
                    req_id,
                    kind,
                    call_id,
                    summary,
                    options,
                    deadline,
                    on_timeout,
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
                if let Some(t) = self.turn.as_mut() {
                    t.progress = None;
                    t.state = Some(TurnState::Finished {
                        finish_reason,
                        usage,
                        timings,
                    });
                }
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
                Disposition::Rendered
            }
            SessionEvent::TranscriptAppended {
                item_id,
                kind,
                ledger_head,
            } => {
                if let Some(t) = self.turn.as_mut() {
                    t.appended.push(item_id.clone());
                }
                self.items.push(SnapshotItem {
                    item_id,
                    kind,
                    ledger_head,
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
            SessionEvent::Warning { code, detail } => {
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
            Key::CtrlL => {
                self.redraw = true;
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
            _ => {}
        }

        // Help is a screen, and the two keys that mean "go back" close it before
        // the composer ever sees them.
        if self.help && matches!(k, Key::Esc | Key::CtrlC) {
            self.help = false;
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
        // An open decision takes the line as an option id or its first letter, so
        // answering does not require a second keymap.
        if let Some(d) = self.open.first().cloned()
            && let Some(opt) = match_option(&d, text.trim())
        {
            return Some(Action::Answer {
                req_id: d.req_id,
                option_id: opt,
            });
        }
        // Sending scrolls back to the tail: the answer is about to arrive at the
        // bottom, and staying parked in the scrollback while it does looks exactly
        // like nothing happening.
        self.scroll = 0;
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
            "thinking {} · tool output {}",
            fold_word(self.reasoning),
            fold_word(self.tools)
        ));
    }

    fn command(&mut self, cmd: &str) -> Option<Action> {
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
            "interrupt" | "i" => Some(Action::Interrupt("operator typed /interrupt".into())),
            other => {
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
        if let Some(r) = self.items.iter_mut().find(|r| r.item_id == item_id) {
            r.item = Some(item);
            // The row's rendered form changed, so the history cache from that row
            // on is stale.
            self.invalidate_history();
        }
    }

    /// Throw the rendered history away; it is rebuilt from `items` and `notes`
    /// on the next frame. One place, because forgetting one of the two cursors
    /// duplicates or loses everything after it.
    fn invalidate_history(&mut self) {
        self.hist_lines.clear();
        self.hist_upto = 0;
        self.note_upto = 0;
    }

    /// File something that happened between rows, at the row it happened at.
    fn note(&mut self, n: Note) {
        let at = self.items.len();
        self.notes.push((at, n));
        if self.notes.len() > 64 {
            self.notes.remove(0);
            self.note_upto = self.note_upto.saturating_sub(1);
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
    ///   ⠹ Responding · 4.2s                        1.2k chars · prompt 41.2k tok
    ///   ╭─ qwen-3.8-flash-next · normal ─────────────────────────────────────────╮
    ///   │ › why did the cache miss                                               │
    ///   ╰─ seq 41 · rendered 12 · filtered 0 · dropped 0 · scrubbed 0 ───────────╯
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
    pub fn screen(&mut self, w: usize, h: usize) -> Vec<String> {
        self.cfg.width = w;
        let h = h.max(1);
        if self.notice_ttl > 0 {
            self.notice_ttl -= 1;
            if self.notice_ttl == 0 {
                self.notice = None;
            }
        }

        let dec: Vec<String> = match self.open.first() {
            Some(d) => self.decision_lines(d, w),
            None => Vec::new(),
        };
        let inflight = self.inflight_line(w);
        let notice = self
            .notice
            .clone()
            .map(|n| colour(&self.cfg, sgr::MAGENTA, &trim_to(&format!("· {n}"), w)));

        // How many rows the composer wants, and then what actually fits. The
        // ladder deletes the most expendable row first and stops as soon as the
        // whole thing fits with a line of transcript left over. The old code
        // drained the chrome from the *front*, which for a box would have eaten
        // the top border and left the bottom one — a container with one side is
        // worse than none.
        let mut rows = self.editor.height(self.composer_cols(), h);
        let mut hint = true;
        let mut show_notice = notice.is_some();
        let mut show_inflight = inflight.is_some();
        let mut boxed = true;
        let mut dec_rows = dec.len();
        loop {
            let n = dec_rows
                + usize::from(show_inflight)
                + usize::from(show_notice)
                // Unboxed still costs one row: the disclosure counters move off
                // the border and back onto a line of their own. They are §13.2b
                // obligations and are not what a narrow screen gives up.
                + if boxed { 2 } else { 1 }
                + rows
                + usize::from(hint);
            if n < h {
                break;
            }
            if hint {
                hint = false;
            } else if show_notice {
                show_notice = false;
            } else if rows > 1 {
                rows -= 1;
            } else if show_inflight {
                show_inflight = false;
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
        if show_inflight && let Some(l) = inflight {
            chrome.push(l);
        }
        if show_notice && let Some(l) = notice {
            chrome.push(l);
        }
        if boxed {
            chrome.push(self.box_edge(w, '╭', '╮', &self.facts()));
        }
        let caret_at = chrome.len() + caret_row;
        chrome.extend(input_rows);
        if boxed {
            chrome.push(self.box_edge(w, '╰', '╯', &self.status_line(w.saturating_sub(6))));
        } else {
            chrome.push(colour(
                &self.cfg,
                if self.dropped + self.scrubbed + self.resyncs > 0 {
                    sgr::YELLOW
                } else {
                    sgr::GREY
                },
                &self.status_line(w),
            ));
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

        let room = h.saturating_sub(chrome.len()).max(1);
        let mut out = if self.help {
            let mut help = help_lines(&self.cfg, w);
            help.truncate(room);
            help
        } else {
            self.body_window(room)
        };
        while out.len() < room {
            out.push(String::new());
        }
        out.truncate(room);
        let body_rows = out.len();
        out.extend(chrome);
        out.truncate(h);
        // The caret is the affordance. It goes where the composer says, and the
        // terminal draws it as a steady block because `term::enter` asked for one.
        self.cursor = Some((
            (body_rows + caret_at).min(out.len().saturating_sub(1)),
            caret_col.min(w.saturating_sub(1)),
        ));
        out.into_iter().map(|l| trim_to(&l, w)).collect()
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
        let (lines, (crow, ccol)) = self.editor.render(inner, self.cfg.palette());
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
                let wall = colour(&self.cfg, sgr::GREY, "│");
                format!("{wall} {}{wall}", width::fit(&body, inner + 1))
            } else {
                trim_to(&body, w)
            });
        }
        let col = if boxed { ccol + 2 } else { ccol };
        (out, crow.saturating_sub(start), col)
    }

    /// One edge of the box, with `legend` inlaid at the left.
    ///
    /// `╭─ qwen-3.8-flash-next · normal ────────╮`. A legend rather than a
    /// decoration: the row is paid for either way, and a border that says what
    /// the session is talking to costs nothing a plain border does not.
    fn box_edge(&self, w: usize, open: char, close: char, legend: &str) -> String {
        let w = w.max(4);
        let inner = w - 2;
        let text = if legend.is_empty() || inner < 10 {
            String::new()
        } else {
            format!("─ {} ", trim_to(legend, inner - 4))
        };
        let fill = inner.saturating_sub(visible_width(&text));
        colour(
            &self.cfg,
            sgr::GREY,
            &format!("{open}{text}{}{close}", "─".repeat(fill)),
        )
    }

    /// What this composer is talking to: the top edge's legend.
    ///
    /// On its own row and not inside the field. The model name is kept past the
    /// end of a turn, because "what am I talking to" is a question asked while
    /// nothing is running, which is most of the time anybody is looking at it.
    fn facts(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.model.is_empty() {
            parts.push("no turn yet".into());
        } else {
            parts.push(self.model.clone());
        }
        parts.push(self.verbosity.as_str().to_string());
        if self.heads > 1 {
            parts.push(format!("{} heads", self.heads));
        }
        if !self.open.is_empty() {
            parts.push("waiting on your answer".into());
        }
        parts.join(" · ")
    }

    /// The bottom bar: what the keys do, right now.
    ///
    /// The composer owns the first half and changes it after the first Esc or
    /// Ctrl+C — that is how anyone finds out a double-tap exists. The head owns
    /// the second half, which is its own keys.
    fn hint_bar(&self, w: usize) -> String {
        let p = self.cfg.palette();
        let mut s = self.editor.hint(self.turn_running(), self.now_ms, p);
        let tail = if self.help {
            "esc closes this"
        } else if !self.open.is_empty() {
            "type an option above to answer · /help"
        } else {
            "ctrl-r thinking · ctrl-t tool output · pgup scrolls · /help"
        };
        s.push_str(&p.paint(Role::Faint, &format!(" · {tail}")));
        trim_to(&s, w)
    }

    /// The visible `room` lines of the body, and nothing else built.
    fn body_window(&mut self, room: usize) -> Vec<String> {
        let cfg = self.cfg.clone();
        let (think, tool) = (self.reasoning, self.tools);
        if self.hist_width != cfg.width {
            self.hist_width = cfg.width;
            self.invalidate_history();
        }
        // Rows and notes, interleaved in the order they happened. A note anchored
        // at row N renders between row N-1 and row N, which is where it was when it
        // arrived.
        loop {
            let note_next = self
                .notes
                .get(self.note_upto)
                .is_some_and(|(at, _)| *at <= self.hist_upto);
            if note_next {
                let (_, n) = self.notes[self.note_upto].clone();
                self.hist_lines.extend(note_lines(&cfg, &n));
                self.hist_lines.push(String::new());
                self.note_upto += 1;
            } else if self.hist_upto < self.items.len() {
                let it = self.items[self.hist_upto].clone();
                self.hist_lines.extend(item_lines(&it, &cfg, think, tool));
                self.hist_lines.push(String::new());
                self.hist_upto += 1;
            } else {
                break;
            }
        }

        // Does the transcript already own this turn's content? If so the live pane
        // is a duplicate of history and only its summary line survives — otherwise
        // the answer is on the screen twice, once in the wrong order.
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

        // Disjoint field borrows, so the history can be lent to the frame while the
        // block caches are still being written to.
        let App {
            hist_lines, turn, ..
        } = self;
        let mut segs: Vec<Seg<'_>> = vec![Seg::Borrowed(hist_lines)];

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
                state,
                ..
            } = t;
            if !superseded && !reasoning.is_empty() {
                // Two columns narrower, because the rail is two columns wide.
                // Getting this wrong makes the block one row taller than the space
                // reserved for it, which moves everything below it by a line every
                // frame — which is one of the things being called flicker.
                let mut rcfg = cfg.clone();
                rcfg.width = cfg.width.saturating_sub(card::REASONING_RAIL_WIDTH).max(20);
                reasoning_cache.set_decor(reasoning_decor(&cfg));
                segs.push(Seg::Owned(vec![thinking_header(
                    &cfg,
                    reasoning.raw(),
                    think.is_open(),
                    running,
                    think_elapsed,
                )]));
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
                if !calls.is_empty() {
                    let mut owned: Vec<String> = Vec::new();
                    for c in calls.iter() {
                        owned.extend(call_card(c, &cfg, now_ms, tool));
                    }
                    owned.push(String::new());
                    segs.push(Seg::Owned(owned));
                }
                if !text.is_empty() {
                    let (stable, tail) = text_cache.split(text, &cfg, cfg.budget.body_lines);
                    segs.push(Seg::Borrowed(stable));
                    segs.push(Seg::Owned(tail));
                }
            }
            if let Some(s) = state {
                segs.push(Seg::Owned(turn_footer(&cfg, s)));
            }
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
        self.scroll = self.scroll.min(total.saturating_sub(1));
        let end = total.saturating_sub(self.scroll);
        let start = end.saturating_sub(room);
        let mut out = take_window(&segs, start, end);
        if self.scroll > 0 {
            let behind = total - end;
            let last = out.len().saturating_sub(1);
            out[last] = colour(
                &self.cfg,
                sgr::YELLOW,
                &format!("── scrolled back · {behind} lines below · ↓ or esc to follow"),
            );
        }
        out
    }

    fn decision_lines(&self, d: &OpenDecision, w: usize) -> Vec<String> {
        let mut out = vec![colour(
            &self.cfg,
            sgr::YELLOW,
            &format!("? {} [{}]", d.summary, d.kind),
        )];
        let opts: Vec<String> = d
            .options
            .iter()
            .map(|o| format!("{} ({})", o.label, o.option_id))
            .collect();
        out.extend(
            wrap(&format!("  {}", opts.join("  ·  ")), w)
                .into_iter()
                .map(|l| colour(&self.cfg, sgr::YELLOW, &l)),
        );
        out
    }

    /// The in-flight line: the one display in this harness with a real
    /// denominator.
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
    /// Once prefill is done there is no denominator any more — the head does not
    /// know how many tokens are coming — so the line changes to what is actually
    /// true: elapsed on the left, what has arrived on the right.
    fn inflight_line(&self, w: usize) -> Option<String> {
        let t = self.turn.as_ref()?;
        if !matches!(t.state, Some(TurnState::Running)) {
            return None;
        }
        let elapsed = t.last_ms.saturating_sub(t.started_ms);
        let p = self.cfg.palette();

        // A turn that is running and silent. The daemon sends prefill progress
        // while it prefills and a delta per chunk while it generates, so a gap this
        // long is a real gap and not a slow model — and the case that produced this
        // line is one a head cannot otherwise show: when a turn *fails*, the engine
        // publishes a `Warning` and nothing else, so `TurnState` stays `Running`
        // and the old head span its spinner at a dead session indefinitely. See the
        // report: `TurnFinished`/`TurnInterrupted` on failure is the daemon's to
        // fix, and a head saying "nothing for 40s" is not a substitute for it.
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

        let spin = progress::spinner(elapsed).to_string();
        let s = match &t.progress {
            Some(pp) if pp.total > 0 && pp.processed < pp.total => {
                let pf = progress::Prefill {
                    total: pp.total,
                    cache: pp.cache,
                    processed: pp.processed,
                    time_ms: pp.time_ms,
                };
                format!(
                    "{} {}",
                    p.paint(Role::Pending, &spin),
                    progress::prefill_line(&pf, w.saturating_sub(2), p),
                )
            }
            // Prefill finished, generation running. `cache` and `total` are still
            // the truth about the prompt and they are the number this harness
            // exists to move, so they stay on the screen.
            Some(pp) if pp.total > 0 => split_row(
                &p.paint(
                    Role::Pending,
                    &format!("{spin} Responding · {}", progress::duration(elapsed)),
                ),
                &p.paint(
                    Role::Faint,
                    &format!(
                        "{} chars · prompt {} tok, {} cached ({:.0}%)",
                        progress::thousands(t.out_chars as u64),
                        progress::thousands(pp.total),
                        progress::thousands(pp.cache),
                        pp.cache as f64 * 100.0 / pp.total as f64,
                    ),
                ),
                w,
            ),
            _ => split_row(
                &p.paint(
                    Role::Pending,
                    &format!("{spin} Responding · {}", progress::duration(elapsed)),
                ),
                &p.paint(
                    Role::Faint,
                    &format!("{} chars", progress::thousands(t.out_chars as u64)),
                ),
                w,
            ),
        };
        Some(trim_to(&s, w))
    }

    /// The disclosure line: the read mark, what this head suppressed, what the
    /// daemon will never send, and what it stripped on the way.
    ///
    /// Ordered by how likely it is to matter, and truncated from the right, because
    /// on an 80-column terminal the old line lost `dropped`, `scrubbed` and
    /// `resync` to the ellipsis — the three numbers whose whole purpose is to be
    /// impossible to miss. Anything nonzero is promoted to the front.
    fn status_line(&self, w: usize) -> String {
        let mut parts: Vec<String> = Vec::new();
        let alarms = self.dropped + self.scrubbed + self.resyncs;
        if alarms > 0 {
            parts.push(format!(
                "dropped {} · scrubbed {} · resync {}",
                self.dropped, self.scrubbed, self.resyncs
            ));
        }
        parts.push(format!("seq {}", self.seq));
        parts.push(format!(
            "rendered {} · filtered {} ({})",
            self.rendered,
            self.filtered,
            self.verbosity.as_str()
        ));
        if self.heads > 1 {
            parts.push(format!("heads {}", self.heads));
        }
        if alarms == 0 {
            parts.push("dropped 0 · scrubbed 0 · resync 0".into());
        }
        // Not while they are empty: `--demo` and `--replay` have no session and no
        // head, and a trailing separator with nothing after it reads as a field
        // that failed to load.
        if !self.session_id.is_empty() || !self.head_id.is_empty() {
            parts.push(format!("{} {}", self.session_id, self.head_id).trim().into());
        }
        // Uncoloured: this is inlaid into the box's bottom edge, which is grey
        // all the way across, and a second colour inside a border reads as
        // damage. The alarm case is promoted to the front instead, which is what
        // survives a truncation.
        trim_to(&parts.join(" · "), w)
    }

    /// Drop the transient notice, once the operator has had a frame to see it.
    pub fn clear_notice(&mut self) {
        self.notice = None;
        self.notice_ttl = 0;
    }
}

fn match_option(d: &OpenDecision, typed: &str) -> Option<String> {
    let t = typed.trim().to_ascii_lowercase();
    d.options
        .iter()
        .find(|o| o.option_id.eq_ignore_ascii_case(&t) || o.label.to_ascii_lowercase() == t)
        .or_else(|| {
            d.options
                .iter()
                .find(|o| o.option_id.to_ascii_lowercase().starts_with(&t) && !t.is_empty())
        })
        .map(|o| o.option_id.clone())
}

fn colour(cfg: &RenderConfig, code: &str, s: &str) -> String {
    if cfg.color {
        format!("{code}{s}{}", sgr::RESET)
    } else {
        s.to_string()
    }
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

/// The rail every line of the model's reasoning carries.
///
/// Three independent signals, because each one is lost somewhere: the **word**
/// (`Thinking…` / `Thought for 4.2s`), the **rail** (`┃`), and the **dim-italic
/// attribute**. Colour is a no-op under a terminal-native palette; the rail is
/// what survives a copy-paste; the attribute is what survives a `--replay` diff.
/// Handing it to the `BlockCache` as a `Decor` rather than mapping over the lines
/// per frame is what keeps §13.3: the rail is applied once, when the line enters
/// the cache, not once per line per frame.
fn reasoning_decor(cfg: &RenderConfig) -> Decor {
    let p = cfg.palette();
    Decor {
        prefix: format!("{} ", p.paint(Role::Faint, "┃")),
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
/// `length` is the case that matters: it is not a normal ending, it means the
/// answer was cut off mid-sentence, and the old line rendered it as `── length ·
/// …` in the same dim grey as `── eos · …`. §5.7's rule is that truncation is
/// never folded into success, and a display that makes the two indistinguishable
/// folds it at the last possible moment.
fn turn_footer(cfg: &RenderConfig, state: &TurnState) -> Vec<String> {
    match state {
        TurnState::Running => Vec::new(),
        TurnState::Finished {
            finish_reason,
            usage,
            timings,
        } => {
            let keep = usage
                // `f_sim`, not `f_keep`: `Usage` carries one turn's three numbers and
                // cannot compute `f_keep`, which needs the previous turn's entry as its
                // denominator (D11/T22). The wording names the denominator so the two
                // can never be read for each other again.
                .f_sim()
                .map(|f| format!("{:.0}% of prompt cached", f * 100.0))
                .unwrap_or_else(|| "no prompt".into());
            let rate = if timings.predicted_ms > 0.0 {
                usage.predicted_tokens as f64 * 1000.0 / timings.predicted_ms
            } else {
                0.0
            };
            let stats = format!(
                "{} in ({keep}) · {} out · {rate:.0} tok/s · {}",
                usage.prompt_tokens,
                usage.predicted_tokens,
                dur_human(timings.wall_ms),
            );
            match finish_reason {
                letibot_sessionlog::event::FinishReason::Length => vec![colour(
                    cfg,
                    sgr::YELLOW,
                    &format!(
                        "── CUT SHORT — it hit the output limit mid-answer; \
                         ask it to continue · {stats}"
                    ),
                )],
                letibot_sessionlog::event::FinishReason::Aborted => vec![colour(
                    cfg,
                    sgr::YELLOW,
                    &format!("── stopped early (aborted) · {stats}"),
                )],
                // `eos` and `word` are ordinary endings and read as ordinary.
                letibot_sessionlog::event::FinishReason::Eos
                | letibot_sessionlog::event::FinishReason::Word => {
                    vec![dim(cfg, &format!("── {stats}"))]
                }
                // A reason nobody recognises is shown, never normalised.
                letibot_sessionlog::event::FinishReason::Other(s) => vec![colour(
                    cfg,
                    sgr::YELLOW,
                    &format!("── ended for an unrecognised reason: {s} · {stats}"),
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
    }
}

fn help_lines(cfg: &RenderConfig, w: usize) -> Vec<String> {
    let rows = [
        ("enter", "send what you typed; while a turn runs it is queued as a follow-up"),
        ("alt+enter", "a newline inside the prompt, without sending it"),
        ("esc esc", "interrupt the running turn — twice, within five seconds"),
        ("ctrl-c", "clear what you typed; twice on an empty prompt, within a second, quits"),
        ("↑ ↓", "move inside the prompt, then walk the prompts you have sent"),
        ("pgup pgdn", "scroll the transcript; esc returns to following the stream"),
        ("ctrl-a ctrl-e", "start and end of the line; ctrl-w and ctrl-u kill, ctrl-y yanks"),
        ("ctrl-z", "undo — a word at a time, and a kill is always its own step"),
        ("paste", "five lines or more collapses to a marker and is sent in full"),
        ("ctrl-r", "fold or unfold the model's thinking"),
        ("ctrl-t", "fold or unfold tool output"),
        ("ctrl-l", "repaint the screen"),
        ("/verbosity", "terse → normal → loud; the status line counts what is filtered"),
        ("/interrupt", "interrupt, when a key is awkward"),
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
fn call_card(c: &CallRow, cfg: &RenderConfig, now_ms: u64, fold: Fold) -> Vec<String> {
    let mut card = card::Card::new(&c.name, &c.call_id);
    // §4.1: `ToolCallProposed` carries `name` and `args_digest` and no arguments,
    // so there is nothing honest to put here. A digest is not a display string
    // and a guess is worse than a blank.
    let mut body: Vec<String> = Vec::new();
    card.phase = match &c.state {
        CallState::Proposed => card::Phase::Proposed,
        CallState::Running => card::Phase::Running {
            elapsed_ms: now_ms.saturating_sub(c.started_ms),
            note: c.note.clone(),
        },
        CallState::Finished {
            outcome,
            inline_bytes,
            full_bytes,
            spill,
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

fn outcome_str(o: &letibot_transcript::ToolOutcome) -> String {
    use letibot_transcript::ToolOutcome as O;
    match o {
        O::Ok => "ok".into(),
        // §8.2: abstention is not a flavour of success and must not read like one.
        O::Abstained { reason } => format!("ABSTAINED — {reason}"),
        O::Failed { reason } => format!("failed — {reason}"),
        O::Denied { req_id } => format!("REFUSED — the call was denied ({req_id})"),
        O::Timeout => "timeout".into(),
        O::NotRun { why } => format!("not run — {why}"),
    }
}

fn item_lines(it: &SnapshotItem, cfg: &RenderConfig, think: Fold, tools: Fold) -> Vec<String> {
    let Some(item) = &it.item else {
        // The event arrived and the body has not — which, since the body now
        // travels on the log too, is a real in-flight state and no longer a
        // permanent one. It says so.
        return vec![dim(
            cfg,
            &format!("[{} — waiting for the body of {}]", it.kind, it.item_id),
        )];
    };
    match item {
        TranscriptItem::System { text, origin } => {
            let mut out = vec![dim(cfg, &format!("system ({origin:?})"))];
            out.extend(wrap(text, cfg.width).into_iter().map(|l| dim(cfg, &l)));
            out
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
            wrap(&text, cfg.width.saturating_sub(2))
                .into_iter()
                .map(|l| colour(cfg, sgr::BOLD, &format!("› {l}")))
                .collect()
        }
        TranscriptItem::Reasoning { text, .. } => {
            // A settled row: `Thought`, with no duration. The head can compute one
            // for a *live* turn from the delta timestamps, and a transcript row
            // carries no timestamps at all — see `crates/ui/DESIGN.md` §4.4.
            let mut out = vec![thinking_header(cfg, text, think.is_open(), false, None)];
            if think.is_open() {
                let mut rcfg = cfg.clone();
                rcfg.width = cfg.width.saturating_sub(card::REASONING_RAIL_WIDTH).max(20);
                let mut md = IncrementalMarkdown::new();
                md.push(text);
                let mut cache = BlockCache::decorated(reasoning_decor(cfg));
                out.extend(cache.lines(&md, &rcfg, cfg.budget.reasoning_lines));
            }
            out
        }
        TranscriptItem::Assistant { text, tool_calls } => {
            let mut md = IncrementalMarkdown::new();
            md.push(text);
            let mut cache = BlockCache::new();
            let mut out = cache.lines(&md, cfg, cfg.budget.body_lines);
            for c in tool_calls {
                out.push(colour(cfg, sgr::CYAN, &format!("→ {}({})", c.name, c.id)));
            }
            out
        }
        TranscriptItem::ToolResult {
            name,
            outcome,
            payload,
            call_id,
        } => {
            let lines: Vec<&str> = payload.lines().collect();
            let bad = !matches!(outcome, letibot_transcript::ToolOutcome::Ok);
            let mark = if tools.is_open() { "▾" } else { "▸" };
            let head = colour(
                cfg,
                if bad { sgr::RED } else { sgr::GREY },
                &format!(
                    "{mark} {name}({call_id}) {} · {} line{} · ctrl-t",
                    outcome_str(outcome),
                    lines.len(),
                    if lines.len() == 1 { "" } else { "s" }
                ),
            );
            let mut out = vec![head];
            // Folded shows the first line, which is where a tool puts what it did.
            // A failure is never folded: an error nobody can read is an error
            // nobody acts on.
            let limit = if tools.is_open() || bad {
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
                out.push(colour(
                    cfg,
                    sgr::GREY,
                    &format!("  … {} more lines …", lines.len() - (limit - 1)),
                ));
            } else {
                out.extend(lines.iter().map(|l| dim(cfg, &format!("  {l}"))));
            }
            out
        }
        TranscriptItem::SegmentMark { label, .. } => {
            vec![dim(cfg, &format!("─── {label} ───"))]
        }
    }
}

/// Visible width of a rendered screen line. Re-exported so a test can assert the
/// screen fits.
pub fn line_width(s: &str) -> usize {
    visible_width(s)
}

/// Something that happened between two transcript rows.
#[derive(Debug, Clone)]
enum Note {
    /// §18's post-flight assertions and §8.5's guards land here, and a guard
    /// nobody notices is a guard nobody wrote.
    Warned(Warned),
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
        let status = a.status_line(200);
        assert!(status.contains("filtered 10"), "{status}");
        assert!(status.contains("terse"), "{status}");
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
    fn an_open_decision_is_answered_by_typing_the_option() {
        let mut a = app();
        a.apply(ServerFrame::Event(env(1, testing::requested("r1", "rm"))));
        typed(&mut a, "deny");
        assert_eq!(
            a.key(Key::Enter),
            Some(Action::Answer {
                req_id: "r1".into(),
                option_id: "deny".into()
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
        // The caret sits just past the prompt glyph, in an otherwise empty field.
        assert_eq!(col, 4, "{:?}", screen[row]);

        // And it moves with the text.
        typed(&mut a, "why did the cache miss");
        let screen = a.screen(80, 20);
        let (row2, col2) = a.cursor().unwrap();
        assert_eq!(col2, 4 + "why did the cache miss".len());
        assert!(screen[row2].contains("why did the cache miss"));
    }

    #[test]
    fn the_facts_and_the_keys_are_on_their_own_rows_not_in_the_field() {
        let mut a = app();
        a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
        let screen = a.screen(100, 20);
        let (row, _) = a.cursor().unwrap();
        let joined = screen.join("\n");
        // The model is on the top edge…
        assert!(screen[row - 1].contains("normal"), "{:?}", screen[row - 1]);
        // …the disclosure counters on the bottom edge…
        assert!(screen[row + 1].contains("seq 1"), "{:?}", screen[row + 1]);
        // …and the keys on a bar below the box, never in the field.
        assert!(screen[row + 2].contains("ctrl-r"), "{:?}", screen[row + 2]);
        assert!(!screen[row].contains("ctrl-r"), "{:?}", screen[row]);
        assert!(joined.contains("dropped 0"), "{joined}");
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
        let line = a.inflight_line(120).unwrap();
        assert!(line.contains("prefill 97%"), "{line}");
        assert!(line.contains("38.1k cached (92%)"), "{line}");
        // And it never wraps, at any width.
        for w in [24usize, 40, 60, 80, 120, 200] {
            assert!(line_width(&a.inflight_line(w).unwrap()) <= w, "w={w}");
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
            .filter(|l| l.starts_with('\u{25cf}'))
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
}
