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
use letibot_sessionlog::view::{OpenDecision, SnapshotItem};

use letibot_ui::editor::Editor;
use rano::agent::card;

use crate::ui::render::{RenderConfig, visible_width};
use crate::ui::*;

/// **Which count label on the composer's top edge a click landed on** — the click's
/// answer, and the act it runs: the subagents label does `ctrl-g`'s, the jobs label
/// `ctrl-q`'s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CountLabel {
    Subagents,
    Jobs,
}

/// **The count labels' columns in the composer's top edge, as rano's own line drew them**
/// — each label's first column and width **in the row's own columns**, the gutter not yet
/// added. `None` for a label this frame did not draw: a zero count, or an edge too narrow
/// for rano, whose truncation eats the jobs label first (it sits to the right). Taken off
/// the line rano returned rather than rebuilt from the counts, because the line is the
/// thing on the screen — rano pins the labels right and cuts them when the edge is
/// narrow, and both of those move a target that arithmetic over the counts would
/// misplace.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct BoxTopLabels {
    pub(crate) subagents: Option<(usize, usize)>,
    pub(crate) jobs: Option<(usize, usize)>,
}

/// **The composer's top edge as a click target**: the screen row the last frame drew it
/// on, and each label's first column and width in TERMINAL coordinates — the gutter
/// added, because a click's `x` is in terminal cells.
///
/// Recorded by the frame that drew the edge ([`App::compose_screen`]) and left `None` by
/// every frame that did not, for the reason every other click record in this head is
/// recorded rather than derived (see [`App::todos_stop_rows`]): the labels are pinned
/// right, so their columns move with the terminal's width and with rano's truncation of
/// the edge, and the edge itself is drawn only while a count is non-zero — a label that
/// is not on the screen is not a target, and a click that arrives against a frame that
/// never drew the edge has nothing to hit. See [`App::box_top_label_at`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BoxTopHits {
    /// The screen row the edge sat on, counting the header when the frame drew one.
    pub(crate) row: usize,
    /// The `N subagent(s) running` label, when the frame drew one.
    pub(crate) subagents: Option<(usize, usize)>,
    /// The `N job(s) running · M to a file` label — the tail included, because it is that
    /// fact's own second half and names no pane of its own.
    pub(crate) jobs: Option<(usize, usize)>,
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
    /// **The editor pane — rano, on a file this conversation changed**, when it is open.
    ///
    /// `None` is the ordinary state and costs nothing: no rano is made until a click or
    /// `ctrl-]` asks for one. `Some` takes the conversation's rectangle the way a `!term` pane
    /// does, under it in precedence (a `!term` pane drawn over it owns the keyboard). See
    /// [`EditorPane`] and `app/editor.rs` for the movements in and out.
    pub(crate) edit_pane: Option<EditorPane>,
    /// **The conversation's rectangle in the last frame**, in terminal cells — what a pane
    /// opened between frames is given before it draws, so it opens centred on its change.
    pub(crate) edit_area: rano::editor::Area,
    /// **Which screen rows of the last frame are about a file**: `(row, place)` for every row
    /// of a finished edit or write the transcript drew. Rebuilt every frame the conversation is
    /// drawn and emptied when anything else is, so a click is only ever measured against rows
    /// that are on the glass. See [`FileRef`].
    pub(crate) file_rows: Vec<(usize, FileRef)>,
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
    /// **What the terminal speaks beyond cells** (`rano::term::features`), told by the head after
    /// it entered the terminal. Default — nothing — for a test, a replay and a pipe.
    pub(crate) features: rano::term::Features,
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
    /// **The card is put away, and the request is not.** `esc` hides the prompt card because a
    /// person would rather type the answer as a `!send` line or watch the stream a moment
    /// longer — but the daemon keeps the request OPEN and the run keeps WAITING, which is
    /// exactly the window the operator's measured `y` fell through: card put away, `y` typed at
    /// the composer, and the line became a prompt that reached the MODEL while their own
    /// command waited and died at its deadline (2026-10-09, `! sudo apt install mc`). So the
    /// request's presence and the card's visibility are two facts and not one: this flag is the
    /// second, and [`App::submit`] reads the first.
    pub(crate) prompt_away: bool,
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
    /// What the theme `head.toml` names could not supply, held from `apply_theme` until
    /// `load_prefs` says every problem of the file in one notice.
    pub(crate) theme_problems: Vec<String>,
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
    /// **The count labels on the composer's top edge, as the last frame drew them** —
    /// [`BoxTopHits`] for the shape, `None` whenever the frame drew no edge, cut it, or
    /// the head was never drawn at all. The labels are the composer's own affordance:
    /// they count what this session has in flight, and a click on one opens the pane its
    /// count names — exactly the act the label's chord has always done.
    pub(crate) box_top_hits: Option<BoxTopHits>,
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
    /// **The standing-notes pane** — the notes the harness reads into the system prompt, one row
    /// each, in the order the section carries them. `/standing`.
    ///
    /// The operator, handed a note's path in a conversation: *"yeah you gave md name but it is
    /// not clickable"*, then *"i mean do the usual - notes pane"*. This is that pane, and the
    /// field it exists for is the FORM: which notes the prompt was given whole and which arrived
    /// as an index because they did not fit the budget — a fact that was, until this, visible
    /// nowhere outside the model's own prompt.
    ///
    /// **No chord, for the queue pane's reason one field up**, and one more of its own: the word
    /// this pane needs is *standing*, and every chord that could spell it is spent — `ctrl-n` and
    /// `/notes` are this head's own disclosures (`Note::Warned`, numbered for `/notes dismiss N`)
    /// and must keep meaning that, the readline keys own `a`–`z`, and the two control bytes the
    /// C0 tail had spare went to the editor crossing and `attach anyway`. A seventh pane takes
    /// the verb.
    pub(crate) standing_pane: bool,
    /// **The standing notes, as the daemon last answered `ListNotes`** — the index's own rows.
    ///
    /// The path, the abstract and the form are the daemon's (decided with the session's token
    /// counter, which this head does not have and must not grow). What is NOT here is the file's
    /// size, its mtime and whether it is still on disk: those are the disk's facts, they change
    /// while the pane is open, and they are read at draw time — which is also the only way *the
    /// index names a note the disk no longer has* can be seen at all. See `ui/panes/standing.rs`.
    pub(crate) standing: Vec<letibot_sessionlog::protocol::NoteEntry>,
    /// Which note row the cursor is on — an index into [`App::standing`], the same list the
    /// drawn `▸`, the arrows and Enter read.
    pub(crate) standing_sel: usize,
    /// **The pane row each note was DRAWN on** — the record the arrows scroll by, never
    /// arithmetic over the list. See `jobs_stop_rows` for the defect this avoids.
    pub(crate) standing_stop_rows: Vec<usize>,
    /// **The note the pane's Enter opened, until Esc.** Read from disk at the keypress — the
    /// same read the row's size and age come from — and held as the text, so Esc back to the
    /// list does not read it again.
    pub(crate) note_open: Option<standing::NoteOpen>,
    /// **What the corpus looked like on the last draw**, so a note edited while the pane is
    /// open is a change the pane can notice and answer for. `None` until the first draw after
    /// the rows land, which is the baseline rather than a change. See [`standing::Corpus`].
    pub(crate) standing_corpus: Option<standing::Corpus>,
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
    /// **The change a click opened, as a popup over the conversation** — see
    /// `ui::diff_popup`. `None` when nothing is open.
    pub(crate) diff_popup: Option<crate::app::diff_popup::DiffPopup>,
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
    /// **The seat an operator has already said "attach anyway" for.**
    ///
    /// The informed decision in *"connect, look around and make informed decision"* is the
    /// person's, not the head's: a head that only knew how to refuse would have implemented
    /// half the ruling. This is the other half's memory — set by [`App::attach_anyway`]
    /// (the `ctrl-^` chord), compared against [`App::daemon_seat`] by [`App::skew_locked`].
    ///
    /// **A seat, not a boolean, and that is the whole rule.** The override lasts for the
    /// daemon it was given on: a switch produces a second `Hello` from the same process and
    /// must not re-lock a head the operator has already unlocked, while a *replaced* daemon
    /// — different pid, or the same pid speaking a different protocol — is a new party the
    /// head has not been told about, and the lock returns until it is lifted again. An
    /// override that outlived the daemon it judged would be a standing decision made by
    /// nobody.
    pub(crate) skew_override_for: Option<DaemonSeat>,
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
    /// The session this head asked the daemon to resume and has not been seated in yet.
    ///
    /// `--continue` and the picker's resume are two steps — seated somewhere, then moved —
    /// and the `Hello` for the first step is not the answer. While this is set a `Hello`
    /// for any other session leaves [`Self::attaching`] up, so the placeholder's empty
    /// banner is never drawn as if it were the conversation asked for.
    pub(crate) fetching: Option<String>,
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
    /// **Where the reader was when a snapshot replaced the rows under them** — the two
    /// things that can still be asked of a transcript that has not arrived yet, taken at
    /// the moment the old one went.
    ///
    /// **A snapshot is a transcript boundary, and a place does not cross one by
    /// counting.** The ids are per-transcript (`{transcript_id}.{n}`, `engine.rs`), so the
    /// rows a fork carries across arrive under new ones and the id the viewport is
    /// holding names a different place — or nothing — in what replaces it. The two things
    /// that survive are the row's **own words** ([`App::retire_pending`]'s precedent: match
    /// on the content, never on the id a fork has replaced) and the **line** the reader
    /// was at, which is what the window holds while the carry lands.
    ///
    /// `Some` is a carry in flight. It is taken in [`App::load`], **before the rows go**,
    /// because afterwards there is nothing left to ask; and it is spent by
    /// [`App::repair_anchor`], which places the row again under its new id or says that
    /// the new transcript does not carry it and goes to the tail. See [`Carry`].
    pub(crate) carry: Option<Carry>,
    /// **How many wheel notches down this flick has counted, when the last one fell, and
    /// how many rows the transcript held when it did** (2026-10-09's third report).
    ///
    /// A notch on its own walks three lines — that is October's reconciliation and it is
    /// not negotiable. What it cannot do is walk a reader 190 lines up back to a tail that
    /// keeps receding, which is what the operator sat inside: the notch moved, the count
    /// grew. A *flick* — several notches inside one read, an intent repeated — is the unit
    /// that gathers speed, and the row count is how the head tells a transcript that is
    /// holding still (walked) from one that is streaming (run). See
    /// [`App::wheel_down_notch`].
    pub(crate) wheel_run: usize,
    pub(crate) wheel_last_ms: u64,
    pub(crate) wheel_items: usize,
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
    /// **Pre-emptive compaction is OFF for this session, and why** — the
    /// no-progress guard's finding, as the daemon rendered it on the
    /// `auto-compact` settings row or in its `auto_compact_no_progress`
    /// warning. `None` is the ordinary state and draws nothing.
    ///
    /// A resident line's state, for `link_line`'s reason: it is true until it is
    /// not, and a note anchored in the scrollback would be saying nothing about
    /// NOW. Two channels feed it because two moments matter — the WARNING while
    /// this head is attached to see it happen, and the SETTINGS row when a head
    /// attaches after the fact (a head that never saw the warning must not draw
    /// a blank screen about a fact the daemon has held for a week). The row is
    /// keyed on the `off —` spelling the daemon writes ONLY for the guard's
    /// finding, so a `--no-auto-compact` the operator typed themselves is not
    /// re-announced to them on every frame.
    pub(crate) auto_compact_off: Option<String>,
}

/// **The two words an echo can carry** (R16). Constants because both the renderer and
/// the tests name them, and a mark that is spelled twice is a mark that can be spelled
/// differently.
///
/// `queued` is a claim about the DAEMON's queue — *you owe me a row for this*. It is a
/// claim the head can make for an echo it has just sent and has not seen land.
///
/// `UNCONFIRMED` is the state after a snapshot has replaced the transcript: the head can no
/// longer tell *still coming* from *replaced by a fork*. It is still a separate state in the
/// code (see [`App::unconfirmed`]), and it is SPELLED `queued` on the screen: the operator,
/// 2026-10-08, *"rename unconfirmed back to queued"* — to the reader both mean the same
/// thing, a message of theirs that has not become a row of the conversation yet, and a second
/// word for it was a distinction the screen did not need to draw.
pub const QUEUED: &str = "queued";
pub const UNCONFIRMED: &str = "queued";

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
            edit_pane: None,
            edit_area: rano::editor::Area::default(),
            file_rows: Vec::new(),
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
            features: rano::term::Features::default(),
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
            prompt_away: false,
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
            theme_problems: Vec::new(),
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
            box_top_hits: None,
            todos_pane: false,
            subagents_pane: false,
            jobs_pane: false,
            queue_pane: false,
            standing_pane: false,
            standing: Vec::new(),
            standing_sel: 0,
            standing_stop_rows: Vec::new(),
            note_open: None,
            standing_corpus: None,
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
            diff_popup: None,
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
            fetching: None,
            link: Link::Attached,
            daemon_protocol: None,
            daemon_seat: None,
            skew_override_for: None,
            attach_started_ms: 0,
            cursor: None,
            bulk: None,
            filling: None,
            compacting: None,
            auto_compact_off: None,
            bye: None,
            daemon_pid: None,
            unconfirmed: Vec::new(),
            anchor: None,
            carry: None,
            wheel_run: 0,
            wheel_last_ms: 0,
            wheel_items: 0,
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
        // **A notice posted before the head had a clock starts timing at its first tick.**
        // `head.toml`'s problems are said while the prefs load, when `now_ms` is still 0, so
        // their deadline was `NOTICE_MS` after the epoch — long gone by the first frame, and
        // the sentence was never seen.
        if self.now_ms == 0
            && now_ms > 0
            && let Some(until) = self.notice_until
            && until <= NOTICE_MS
        {
            self.notice_until = Some(now_ms.saturating_add(NOTICE_MS));
        }
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

mod action;
mod asks;
mod attention;
mod commands;
mod composer;
pub(crate) mod diff_popup;
mod editor;
mod events;
mod keys;
mod notes;
mod panes;
mod pick;
mod prefs;
mod scroll;
mod session;
pub(crate) mod standing;
mod term_pane;
mod todos;
mod turn;
mod visibility;
pub use action::*;
pub(crate) use asks::*;
pub(crate) use attention::*;
pub(crate) use commands::*;
pub(crate) use composer::*;
pub use editor::*;
pub(crate) use events::*;
pub use keys::*;
pub(crate) use notes::*;
pub(crate) use panes::*;
pub(crate) use pick::*;
pub(crate) use prefs::*;
pub(crate) use scroll::*;
pub use session::*;
pub use term_pane::*;
pub(crate) use todos::*;
pub(crate) use turn::*;
pub use visibility::*;

#[cfg(test)]
mod tests;
