//! The TUI head's state machine: frames in, a screen out.
//! [`App`] touches no file descriptor, so feeding it frames and reading `screen()` is a test;
//! [`Verbosity`] counts what it suppresses, so a hidden screen is not an idle session.

use letibot_sessionlog::event::{Timings, Usage};
use letibot_sessionlog::registry::{SessionBrief, SessionWiring};
use letibot_sessionlog::view::{OpenDecision, SnapshotItem};

use letibot_ui::editor::Editor;
use rano::agent::card;

use crate::ui::panes::queue::{QueueScope, QueueView};
use crate::ui::render::{RenderConfig, visible_width};
use crate::ui::*;

/// Which count label on the composer's top edge a click landed on, and the act it runs:
/// the subagents label does `ctrl-g`'s, the jobs label `ctrl-q`'s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CountLabel {
    Subagents,
    Jobs,
    Monitors,
}

/// The count labels' columns in the composer's top edge, as rano's own line drew them. Read
/// off the drawn line and not the counts, because rano pins the labels right and truncates the
/// edge, which would misplace a target derived from arithmetic.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct BoxTopLabels {
    pub(crate) subagents: Option<(usize, usize)>,
    pub(crate) jobs: Option<(usize, usize)>,
    pub(crate) monitors: Option<(usize, usize)>,
}

/// The composer's top edge as a click target: the row the last frame drew it on, and each
/// label's column and width in terminal cells. Recorded by the frame that drew it, so a click
/// against a frame that drew no edge has nothing to hit ([`App::box_top_label_at`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BoxTopHits {
    /// The screen row the edge sat on, counting the header when the frame drew one.
    pub(crate) row: usize,
    /// The `N subagent(s) running` label, when the frame drew one.
    pub(crate) subagents: Option<(usize, usize)>,
    /// The `N job(s) running · M to a file` label, the tail included: it names no pane.
    pub(crate) jobs: Option<(usize, usize)>,
    /// The `N monitor(s) watching` label, which opens the monitors pane.
    pub(crate) monitors: Option<(usize, usize)>,
}

/// What reads the workspace's git state: a function of the path, run on a thread of its
/// own because it touches the disk (see [`App::refresh_git`]).
pub(crate) type GitReader =
    std::sync::Arc<dyn Fn(&str) -> Option<crate::gitfield::GitState> + Send + Sync>;

/// The head.
pub struct App {
    pub cfg: RenderConfig,
    /// The set of switches in force: one field, because two fields that both say how much
    /// is on the screen can disagree. [`Visibility::rung`] still draws the tools, thinking
    /// and system rows at one of `Verbosity`'s four rungs.
    pub visibility: Visibility,
    /// The two-panel before/after view for file-edit cards. `/config` or `head.toml` sets it, and
    /// the unified renderer is the fallback at every width, which is what makes it safe to flip
    /// on a narrow terminal.
    pub diff_split: bool,
    pub(crate) config_sel: usize,
    /// Where the head's own choices are written. `None` is a head with no
    /// config directory, and the pane says so instead of pretending to save.
    pub(crate) prefs_path: Option<std::path::PathBuf>,
    /// The daemon's settings, as last listed. Empty until asked.
    pub(crate) settings: Vec<letibot_sessionlog::protocol::SettingRow>,
    /// The `!term` pane, when a screen program runs in it. `None` is the ordinary state,
    /// where the transcript has the conversation's rectangle. One at a time, and only the
    /// daemon refuses a second, because it owns the pty. See [`TermPane`].
    pub(crate) term: Option<TermPane>,
    /// The editor pane, on a file this conversation changed, when it is open. `None` costs
    /// nothing: no rano is made until a click or `ctrl-]` asks for one. Under a `!term` pane
    /// in precedence, which owns the keyboard when both are up. See [`EditorPane`].
    pub(crate) edit_pane: Option<EditorPane>,
    /// The conversation's rectangle in the last frame, in terminal cells: what a pane opened
    /// between frames is given before it draws, so it opens centred on its change.
    pub(crate) edit_area: rano::editor::Area,
    /// Which screen rows of the last frame are a call the turn is still drawing, as `(row, call
    /// id)`. Keyed by the call and not the card's words, which change: the ids come out of the
    /// function that drew the lines, so the record and the drawing cannot disagree.
    pub(crate) call_rows: Vec<(usize, String)>,
    /// Which screen rows of the last frame are about a file: `(row, place)` for every row of
    /// a finished edit or write the transcript drew. Rebuilt every frame the conversation is
    /// drawn and emptied when anything else is, so a click meets only rows on the glass.
    pub(crate) file_rows: Vec<(usize, FileRef)>,
    /// Which screen rows of the last frame are a job's completion notice, as `(row, index of
    /// the row in `items`)`, rebuilt with [`App::file_rows`]. A click opens its card
    /// ([`PopupBody::Notice`]).
    pub(crate) notice_rows: Vec<(usize, usize)>,
    /// Which screen rows of the last frame are a finished subagent's line, as `(row, index into
    /// [`App::task_switches`])`. Recorded after the notice rows, so a click goes to the child's
    /// session; only a child this head can reach is listed ([`App::task_rows_in`]).
    pub(crate) task_rows: Vec<(usize, usize)>,
    /// The child each of those rows names: the session id, carried out of the fold that drew
    /// the line rather than recovered by reading it back.
    pub(crate) task_switches: Vec<String>,
    /// The notes the walk drew as a card's one line, with where their line is in `hist_lines`
    /// and the detail the popup shows whole. Kept with the history it indexes: truncated where
    /// the history is, shifted where a fill prepends to it.
    pub(crate) note_cards: Vec<crate::app::diff_popup::NoteCard>,
    /// Which screen rows of the last frame are one of those lines, as `(row, index into
    /// [`App::note_cards`])`, rebuilt with [`App::notice_rows`]. A click opens the detail.
    pub(crate) card_rows: Vec<(usize, usize)>,
    /// Which screen rows of the last frame are a line of the open payload window, read off the
    /// drawn rows because only the drawing knows where the seams fell. A click puts that line at
    /// the top; the header and the seam are not lines and act on nothing.
    pub(crate) payload_rows: Vec<(usize, usize)>,
    /// The clauses of the run markers on the last frame, each a click that opens its run's
    /// rows of that kind. See `app::runclick`.
    pub(crate) marker_rows: Vec<runclick::MarkerHit>,
    /// The one switch a click throws: the kind a run is drawn lifted to, on [`App::payload_sel`],
    /// or [`runclick::LIVE_RUN`]; `None` is the whole run. One field, so no door can lift a run
    /// with no panel able to close it. [`App::lifted`] reads it and `runclick` writes it.
    pub(crate) lift: Option<visibility::Show>,
    /// The open run's own tool-row fold, thrown by `/t` and a click on the run while it is up. The
    /// transcript-wide [`App::tools`] stays untouched; closing the run puts this back.
    pub(crate) run_tools: Fold,
    /// What this head holds of each ended job's output, by handle: the second line of a
    /// completion notice and the tail its popup shows. Filled by a `ReadJobOutput` the window
    /// sends for a notice on the screen, once per job (see `App::ask_notice_tails`).
    pub(crate) job_tails:
        std::collections::HashMap<String, crate::ui::transcript::job_card::JobTail>,
    /// What this head believes about the session's pane: the daemon's answer to
    /// `ClientFrame::TermStatus`, kept because a head not drawing the pane still has to say
    /// something runs in it. [`App::pane_behind`] joins the two sources. See [`PaneFact`].
    pub(crate) term_fact: PaneFact,
    /// The confirmation that ends a pane, when one is up. See [`TermAsk`].
    pub(crate) term_ask: Option<TermAsk>,
    /// A `!term close` waiting for the status read. The head cannot always answer *is there a
    /// pane to end* from what it holds, having just attached or never opened one, so the line
    /// is held and the answer runs the same decision. See [`App::begin_close`].
    pub(crate) close_pending: bool,
    /// Which pane the held `!term close` named, if it named one (`!term close 2`).
    pub(crate) close_which: Option<u32>,
    pub(crate) session_id: String,
    pub(crate) head_id: String,
    /// The head id the daemon just handed out, for the driver to give the client. A client
    /// keeping the old id would ack into the session it left, which the hub ignores silently,
    /// so the mark would stop advancing and nothing would say why.
    pub(crate) seated: Option<String>,
    /// What this session is talking to: `model · dialect · endpoint · workspace`, on `Hello`.
    pub(crate) wiring: SessionWiring,
    /// Every session the daemon holds, as of the last `Hello` or `Sessions` frame.
    pub(crate) sessions: Vec<SessionBrief>,
    /// Subagents this session spawned, joined from the live `Subagent` events, the snapshot's own
    /// children and the daemon's session list: a head that missed the spawn has only the last two.
    /// Keyed by session id, so a `running` row becomes its `done` row.
    pub(crate) subagents: Vec<SubagentState>,
    /// The daemon's job table, as it last answered `ListJobs`. Not folded here: a head that
    /// joined the command out of the turn it was drawing lost the name of a job that outlived
    /// its turn. A `JobSettled` still folds onto a row for liveness.
    pub(crate) jobs: Vec<letibot_sessionlog::protocol::JobEntry>,
    /// Seq of the newest job event seen: a job list stamped older than it is discarded and asked
    /// for again. One `ListJobs` is in flight at most, and one more is owed when a second is wanted.
    pub(crate) jobs_event_seq: u64,
    pub(crate) jobs_in_flight: bool,
    pub(crate) jobs_again: bool,
    /// The daemon's monitor table as it last answered `ListJobs`: the monitors pane's rows and
    /// the composer edge's `N monitors watching`. Live ones first, then the ended ones.
    pub(crate) monitors: Vec<letibot_sessionlog::protocol::MonitorEntry>,
    /// An index into [`App::monitor_stops`], the live monitors, so the drawn cursor and the `x`
    /// that retires cannot disagree about which watch is selected.
    pub(crate) monitors_sel: usize,
    /// The pane row each monitor stop was drawn on, which the arrows scroll by and a click is
    /// tested against. See [`App::jobs_stop_rows`].
    pub(crate) monitors_stop_rows: Vec<usize>,
    /// The plugin panes this head holds, by the name the daemon publishes them under
    /// (`<layer>/<hook>`): the view each one's last look answered, or why it could not be read.
    /// A map because the wire says `this pane's view` and this is what that names.
    pub(crate) plugin_panes: std::collections::BTreeMap<String, PluginState>,
    /// Which of them the `PluginPane` surface is showing: the surface says where on the screen,
    /// this says what.
    pub(crate) plugin_open: Option<String>,
    /// Which of that pane's screens is drawn: the overview, one panel's view behind its key,
    /// the findings, or the legend.
    pub(crate) plugin_screen: crate::ui::panes::plugin::Screen,
    /// An index into [`App::plugin_drawn`]: the row the cursor is on.
    pub(crate) plugin_sel: usize,
    /// The stops the last frame drew, in the order the cursor walks them - `plugin_stop_rows`' sibling,
    /// written by the same draw. A panel cut to its budget drops the stops it could not draw, so the
    /// walk over the view would name rows nobody is looking at.
    pub(crate) plugin_drawn: Vec<crate::ui::panes::plugin::Stop>,
    /// Which screen that record is of. A frame drawn for the findings is not a record of the
    /// overview, and reading it as one would name a different row.
    pub(crate) plugin_drawn_for: Option<crate::ui::panes::plugin::Screen>,
    /// The pane row each plugin stop was drawn on, the record the arrows scroll by and a click
    /// is tested against. See [`App::jobs_stop_rows`].
    pub(crate) plugin_stop_rows: Vec<usize>,
    /// The pane rows the panel titles were drawn on, as `(row, panel)`: a click opens that
    /// panel's view. Not stops, because a title is a door and Enter is for the rows behind it.
    pub(crate) plugin_panel_rows: Vec<(usize, usize)>,
    /// The pane rows whose label the view's legend documents, as `(row, legend key)`: what a
    /// hover reads to answer which label means what. Written by the frame that drew the rows.
    pub(crate) plugin_legend_rows: Vec<(usize, String)>,
    /// The same record in screen rows, translated where the pane's top and scroll are known
    /// ([`App::record_main_hits`]): a pointer arrives with a screen row and nothing else.
    pub(crate) plugin_legend_at: Vec<(usize, String)>,
    /// The pane rows a panel's counted seam was drawn on: the rows that say how much is out of view.
    /// Not stops, and a click on one is nobody's - the block above them may not reach across.
    pub(crate) plugin_seam_rows: Vec<usize>,
    /// The same record in screen rows, translated like the legend's.
    pub(crate) plugin_seam_at: Vec<usize>,
    /// The stop a detail overlay was opened on: the screen it was read out of, and the stop itself -
    /// never an index into the walk, which a panel cut to its budget makes a different row. A value,
    /// because the overlay covers the pane and the pane's own screen never becomes the detail's.
    pub(crate) plugin_detail: Option<(
        crate::ui::panes::plugin::Screen,
        crate::ui::panes::plugin::Stop,
    )>,
    /// What this head last told the daemon about a pane's rectangle. The rectangle is only known
    /// while drawing, so the open sends its best value and the frame that draws corrects it: a
    /// frame per draw would be a frame per keystroke.
    pub(crate) plugin_sent: Option<(String, u32, u32)>,
    /// Which picker row the cursor is on. It starts on the session this head is already in, so
    /// an untouched list answers Enter with a no-op rather than a surprise.
    pub(crate) picker_sel: usize,
    /// The picker opened before the cursor could be seated on this session (its list had not
    /// landed, or lacked it). The next list seats it; any cursor move cancels.
    pub(crate) picker_seat_pending: bool,
    /// The home this head shortens paths against, `$HOME`, read once at construction: three
    /// places draw a path and a per-frame read would make the picture depend on the process's
    /// environment, which no test can pin. A test sets this field.
    pub(crate) home: String,
    /// The head's stack: the built-in layers and whatever a project put above them. Built on
    /// the first draw of the session list or the first `/todo`, whichever asks first, and kept
    /// because discovering layers reads the disk and the modules compile.
    pub(crate) sessions_host: std::cell::RefCell<Option<crate::app::sessions_pane::Host>>,
    /// A stack being built on a thread of its own. Discovery reads the disk and the binary's own
    /// thirteen built-ins compile, which on the drawing thread freezes the first picture; the
    /// frame that asks draws the head's own list and a later one draws the layer's.
    pub(crate) host_build: std::cell::RefCell<Option<crate::app::sessions_pane::HostBuild>>,
    /// Build the stack where the frame stands instead of on a thread. A head never does - that wait
    /// is the freeze this field exists to keep out - and a test sets it, because a test has no
    /// second frame: the two in `tests::host_build` leave it alone and drive the thread instead.
    #[cfg(test)]
    pub(crate) host_inline: std::cell::Cell<bool>,
    /// Which rows the person has folded or opened, by row id: the session list's fold state,
    /// which belongs to the host and not to the layer. A fold survives a re-answer because it
    /// travels back in the pane's input.
    pub(crate) picker_folds: std::collections::BTreeMap<String, bool>,
    /// The sessions the person has taken off this list, oldest first: `x` on the row pushes one,
    /// `u` pops it. Survives the next answer from the daemon, because the list is a view. See
    /// `app::removal` for the three senses of remove and which one this is.
    pub(crate) removed: Vec<String>,
    /// The row `x` has armed, and when: `(id, at_ms)`. A second `x` on the same row inside
    /// `removal::ARM_MS` is the removal.
    pub(crate) removal_armed: Option<(String, u64)>,
    /// The key a pane's `event` action asked the head to deliver, until the next ask.
    pub(crate) sessions_event: std::cell::RefCell<Option<letibot_plugin::pane::Event>>,
    /// Which session each screen row of the last picker frame belongs to, as `(y, k)` for every
    /// row drawn for session row `k`, its id line included. A click is trusted only for a row
    /// this record proves was on the screen. See [`App::picker_layout`].
    pub(crate) picker_hits: Vec<(usize, usize)>,
    /// The terminal height the last screen was composed for, so a click can
    /// redo the header-row arithmetic the screen did without a repaint.
    pub(crate) screen_rows: usize,
    /// The live completion cycle: the prefix it was started for, the names it is cycling, and
    /// which one is showing. Owned `String`s, because half the list comes from the daemon and a
    /// borrowed list could only hold this head's own table.
    pub(crate) completion: Option<(String, Vec<String>, usize)>,
    /// The model's half of the `!` completion: the asks in flight by request id, and the answered
    /// ones by prefix and transcript position. Both clear when the transcript advances, because a
    /// suggestion about a conversation that moved is stale.
    pub(crate) shell_ask: std::collections::HashMap<String, (String, u64)>,
    pub(crate) shell_suggestions: std::collections::HashMap<(String, u64), Vec<String>>,
    /// The number the next `SuggestShell`'s `client_request_id` takes. Unique per head,
    /// because the head is the one that has to recognise the answer on the way back.
    pub(crate) shell_ask_seq: u64,
    /// The model's cycle, when it is live: the prefix it was started for, the lines it is
    /// cycling, and which one is showing. Separate from `completion`, because a model line
    /// drawn as a history line looks like something the person typed and did not.
    pub(crate) shell_model: Option<(String, Vec<String>, usize)>,
    /// The file names a Tab found for the word being typed, and the line they were found for.
    /// Shown in the completion row while that line is unchanged, dropped the moment it is
    /// edited.
    pub(crate) path_matches: Option<(String, Vec<String>)>,
    /// The `!` candidates, held until the rows move: building them walks the view and parses
    /// every `bash` call, and `completions_line` runs once per frame ([`App::the_rows_moved`]
    /// drops it). `None` is not built yet and an empty `Some` is there are none.
    pub(crate) shell_candidates_memo: Option<Vec<String>>,
    /// How many times that walk has run, ever: the encoder for the memo above. A count and not
    /// a wall time, because a test can assert on a count. See [`App::hist_renders`].
    pub shell_walks: u64,
    /// Actions produced by a *frame* rather than by a key: the switch that follows
    /// a session being created. Drained by the driver, which is the only thing that
    /// can send.
    pub(crate) queued: Vec<Action>,
    /// Prompts this head has sent that the transcript does not hold yet: a prompt sent mid-turn is
    /// appended at the next step boundary, so until then the words were nowhere on the screen.
    /// Each renders at the tail as `queued` until its row lands ([`App::record_item`]).
    pub(crate) pending_prompts: Vec<String>,
    /// The chips in the composer: the pictures and files the person pasted, each with the text
    /// it was inserted as. The list is not the truth about what is sent, the chip text in the
    /// editor is; this is only what turns it back into an `Attachment` at send time.
    pub(crate) chips: Vec<crate::app::attachments::Chip>,
    /// Where a pasted picture is written, when it is not the default. `None` is production:
    /// `app::attachments::attachments_dir()`, resolved when the paste happens. A test sets it,
    /// because a test's directory must be its own and never the person's paste directory.
    pub(crate) attachments_dir: Option<std::path::PathBuf>,
    /// Where a pasted picture comes from. A trait object, because a test must never read the
    /// person's real clipboard.
    pub(crate) clipboard: Box<dyn crate::app::attachments::Clipboard>,
    /// How many pictures this head has taken from the clipboard, so two pastes in one session get
    /// two files rather than one overwritten.
    pub(crate) pasted_images: u64,
    /// The echo in the transcript's own place, before its body lands: `item_id` to the text bound
    /// to an announced body-less user row, so the prompt sits above the reply it caused. A guess
    /// until the body confirms it, so retiring still waits for content matched by text.
    pub(crate) bound_prompts: std::collections::HashMap<String, String>,
    /// The echoes in the air when the conversation was about to be replaced. A fork replaces the
    /// transcript, so a queued prompt's row is summarised away and the content `retire_pending`
    /// waits for never arrives: the echo would say `queued` for the rest of the session.
    pub(crate) fork_pending: Vec<String>,
    /// Set when this head asked for a session and is waiting to be told its id.
    pub(crate) want_new_session: bool,
    /// The last turn's `usage`, kept past the end of the turn so the header can
    /// say how much context this session is carrying while nothing is running.
    pub(crate) usage: Option<Usage>,
    /// Whether `usage.cached_tokens` is a measurement or a placeholder. A usage seeded from the
    /// session's row after a restart knows the prompt size but not the cache fraction, and the
    /// header shows a percentage only when one was measured.
    pub(crate) usage_cache_measured: bool,
    /// The last turn's `timings`, kept past the end of the turn so the header can show the
    /// decode rate while nothing is running.
    pub(crate) last_timings: Option<Timings>,
    /// The verdict on the last turn's ending, as the daemon sent it. Without it zero reuse
    /// is one number for a slot that did not hold the history and for a prompt that diverged;
    /// `None` is a daemon older than the field, which measured nothing a head can read.
    pub(crate) prefix_check: Option<letibot_sessionlog::event::PrefixCheck>,
    /// The dialect's `template_sha` on the last turn's ending: a model update reads as a different
    /// dialect rather than as an unexplained cache miss. Empty when the daemon did not say.
    pub(crate) dialect_template_sha: String,
    pub(crate) items: Vec<SnapshotItem>,
    /// The rendered transcript, every row as the bytes that reach the terminal. Once a row is
    /// rendered nothing about it changes on its own except the live marker's counts and a
    /// verbosity toggle: a row that quietly re-renders is a defect.
    pub(crate) hist_lines: Vec<String>,
    pub(crate) hist_upto: usize,
    /// The first item index represented in `hist_lines`: a head attaching to a session too big to
    /// lex renders its tail and this is how many rows it did not render. Scrolling up decreases
    /// it, and the ring raises it again.
    pub(crate) hist_floor: usize,
    /// The row `hist_lines` stops before, when it stops short of the session: the rows below are
    /// held and not drawn, which is what a reader who is not looking at them costs nothing. `None`
    /// is a buffer that reaches the last row this head holds.
    pub(crate) hist_ceiling: Option<usize>,
    /// How many bytes of rendered lines `hist_lines` keeps before the rows farthest from the reader
    /// are let go. A window and not a history: the head that scrolls through a long session would
    /// otherwise render every row it passed and keep all of it. A field, so a test can shrink it.
    pub(crate) hist_limit: usize,
    /// How many lines the forward walk may add on this frame. `usize::MAX` for an ordinary frame; a
    /// screen when the reader has come down to the end of the window, so walking toward the tail is
    /// a screen's work rather than a session's.
    pub(crate) walk_ahead: usize,
    /// The row `items[0]` sits at in the SESSION, oldest first: the rows above the window this head
    /// holds, counted from what the snapshot said it left out. It is what makes a scrolling reader's
    /// position nameable after the window has been trimmed or paged.
    pub(crate) items_base: usize,
    /// How many of the rows this head holds are ABOVE that base - the tail of the transcripts a
    /// resume republished, which a fork's base does not number. Ordinal `items_base` is the row at
    /// this index, and a page of the base belongs here rather than at the front of the list.
    pub(crate) items_lead: usize,
    /// How many rows sit BELOW the window this head holds: the tail it let go because the reader was
    /// parked above it, counted so coming back down knows how much is missing and where it starts.
    pub(crate) items_below: usize,
    /// The page of rows above the window this head has asked for and not been answered: the `from`
    /// it named. One ask at a time, so a reader holding a key does not queue a page per frame.
    pub(crate) fetch_asked: Option<usize>,
    /// The same for the page below the window. Its own field, because the two are independent
    /// questions and a page above that has not come back must not hold up the tail.
    pub(crate) fetch_below: Option<usize>,
    /// The daemon answered a page and had nothing above it: the head stops asking and says the
    /// transcript begins there.
    pub(crate) no_more_above: bool,
    /// How many bytes of row text this head holds before it lets the oldest rows go. A window and
    /// not the buffer: `items` is every body the session has shown, and nothing else bounds it.
    /// A field, so a test can shrink it.
    pub(crate) held_limit: usize,
    /// The `seq` at which the head last read the `model` settings row, and the `seq` of the
    /// last live word about it - a turn starting, or the daemon saying the row moved. The
    /// header takes whichever is later: a row is only ever an answer to a request.
    pub(crate) model_from_settings_at: u64,
    pub(crate) model_from_live_at: u64,
    /// How much transcript this head will walk from the beginning before it renders the tail
    /// instead. A field and not a constant, so a test can force the full walk with `usize::MAX`
    /// and render both ways.
    pub(crate) walk_limit: usize,
    /// The kind of the first row in `hist_lines`. `hist_class` is the last, which is all a
    /// forward walk needs; a backward walk prepends, and the separator it owes the seam is
    /// decided by the two kinds that meet there. `None` for an empty history.
    pub(crate) hist_first_class: Option<RowClass>,
    /// Where the walk stood just before it rendered each item: `hist_marks[k]` is the state at the
    /// top of the iteration that drew `items[k]`. This is what makes "the history from row k on is
    /// stale" expressible, so an arriving body re-lexes one tail instead of the whole session.
    pub(crate) hist_marks: Vec<HistMark>,
    pub(crate) hist_width: usize,
    /// The class of the last row the walk actually drew, so the next one knows whether a blank
    /// line belongs between them. The walk is incremental across frames, so this survives the
    /// frame that set it.
    pub(crate) hist_class: Option<RowClass>,
    pub(crate) turn: Option<TurnPane>,
    pub(crate) open: Vec<OpenDecision>,
    /// `sudo` in the session wants a password: the request, and what has been typed for it.
    /// Kept out of the composer, so it is never in the composer's history, completed or shown;
    /// the composer draws a dot per character while this is `Some`.
    pub(crate) secret: Option<SecretAsk>,
    pub(crate) secret_buf: String,
    /// Which open secret requests are key asks, kept until they settle. The card closes on the
    /// answer, so when `SecretSettled` arrives the card can no longer say what it was, and a
    /// refused key would be noted as sudo's refused password.
    pub(crate) key_secrets: Vec<String>,
    /// What the terminal speaks beyond cells (`rano::term::features`), told by the head after
    /// it entered the terminal. Default (nothing) for a test, a replay and a pipe.
    pub(crate) features: rano::term::Features,
    /// Whether the terminal window has focus, from `?1004` reports. `None` until the first
    /// report, and read as focused: a notification goes only to somebody known to be away.
    pub(crate) focused: Option<bool>,
    /// The terminal's answer to OSC 11: is its background light. `None` until it answers.
    pub(crate) light_background: Option<bool>,
    /// What needed the person last tick - see [`App::take_notification`]. `None` until the
    /// first look, which is a baseline and never a notification: attaching to a session with
    /// a card already open is not news.
    pub(crate) attention: Option<Attention>,
    /// Text the operator asked to put on the clipboard, for the head to write (OSC 52).
    pub(crate) clipboard_out: Option<String>,
    /// Images already in the terminal's memory, by id (see `render::image_id`), and how many
    /// rows of `items` have been looked at for new ones.
    pub(crate) images_sent: std::collections::HashMap<u32, (Option<u32>, Option<u32>)>,
    /// The image box the placements were last sent for. A frame at another width re-sends every
    /// placement at the new size.
    pub(crate) images_box: u32,
    pub(crate) images_scanned: usize,
    /// Upload bytes for the head to write before the next frame (kitty graphics).
    pub(crate) image_uploads: Vec<Vec<u8>>,
    /// Which picture protocol the terminal speaks, decided from the same environment
    /// [`App::features`] came from. [`App::set_features`] sets it, so one decision is made once
    /// and the frame does not ask `TERM` again.
    pub(crate) proto: rano::term::Proto,
    /// The pictures a view draws through rano's widget: the enlarged picture in the popup, whose
    /// box moves with the frame. The conversation's thumbnails are one upload per row for ever, so
    /// a row that scrolls back still shows its picture.
    pub(crate) pictures: rano::term::Pictures,
    /// The thumbnails the last frame drew, as `(screen row, index into picture_refs)`: what a
    /// click acts on and what `Enter` opens. Recorded by the draw, read by the key.
    pub(crate) picture_rows: Vec<(usize, usize)>,
    /// The pictures those rows show, this frame: what a click names.
    pub(crate) picture_refs: Vec<PictureRef>,
    /// A command of the person's own is waiting for an answer: the request and what has been typed
    /// for it. Kept out of the composer, or the line would sit there to be submitted again as a
    /// shell command, and drawn unmasked because what it carries is a program's stdin.
    pub(crate) prompt: Option<PromptAsk>,
    pub(crate) prompt_buf: String,
    /// The password card put away by Esc: the ask stays open at the daemon and the run waits,
    /// the card is not drawn and owns no key, and alt-a brings it back.
    pub(crate) secret_away: bool,
    /// The card is put away, and the request is not: the daemon keeps it open and the run waits,
    /// so a line typed at the composer meanwhile must not become a prompt for the model. The
    /// request's presence and the card's visibility are two facts ([`App::submit`] reads the first).
    pub(crate) prompt_away: bool,
    /// A key the model picker asked for. Its own state and never the sudo path's: a provider key
    /// is stored, not spent, and borrowing `SecretAsk` would tie a head-side ask to a daemon
    /// `req_id` that does not exist.
    pub(crate) key_ask: Option<KeyAsk>,
    pub(crate) key_buf: String,
    /// The model check a sent message waits on, and the message it holds. See [`reach`]: the
    /// launcher no longer gates on a model, so the head asks.
    pub(crate) reach: reach::Reach,
    /// Screen requests this head has not answered yet. Answered by the DRIVER,
    /// after the frame is built, with the rows it actually drew.
    pub(crate) screen_requests: Vec<String>,
    /// The terminal's full width at the last render, gutter included. See
    /// [`App::screen`].
    pub(crate) term_cols: usize,
    /// Which option of `open[0]` is highlighted. Up/Down move it and Enter on an empty composer
    /// answers it; typing still works, because a script and a test drive the head that way. Held
    /// apart from the composer so a mistyped word cannot answer a prompt.
    pub(crate) sel: usize,
    /// The `req_id` [`App::sel`] was last reset for: the highlight is reset exactly when the
    /// front card changes, so a card arriving behind the one on screen does not move it and the
    /// next card up does not inherit the last one's row. See [`App::sync_front`].
    pub(crate) sel_for: String,
    /// The person's own line, held while a question card owns the composer: a question arriving
    /// over typed words would otherwise take them as its answer. They come back when no question
    /// is in front. See [`App::sync_front`].
    pub(crate) held_line: Option<String>,
    /// Things that happened between transcript rows and belong in the conversation. Each is
    /// anchored to the number of rows that existed when it arrived, so the rebuild puts it back
    /// where it happened. A note from a snapshot is [`Placed::Before`] and is listed, not drawn.
    pub(crate) notes: Vec<(Placed, Note)>,
    /// How far the open card's content is scrolled, in rows from the top: the opposite of
    /// [`App::scroll`], because a card is read from its head. Reset on a change of
    /// `open[0].req_id`, because a card that inherited the offset would open mid-screenful.
    pub(crate) dec_scroll: usize,
    /// The `req_id` [`App::dec_scroll`] belongs to, so the reset above can tell a new card
    /// from the same card drawn again.
    pub(crate) dec_scroll_for: String,
    /// What the card's content window was on the last frame: how many lines the content has and
    /// how many rows the viewport got. Only the draw knows whether anything is out of view, since
    /// the content's length is a function of the width. Named after the panes' `pane_len`.
    pub(crate) dec_content_len: usize,
    pub(crate) dec_content_room: usize,
    /// The notes this reader has retired, by [`note_key`], the identity a note keeps across a
    /// resync. A note is a disclosure and not a record, so retired is not deleted: it is hidden,
    /// counted and findable. The keys live in `head.toml` because a restart is how the wall came back.
    pub(crate) dismissed: Vec<String>,
    /// The warning KINDS `leticode.toml`'s `[warnings] mute` names, and whether a burst of one
    /// kind is one counted row: filed and counted, and never drawn. One line for a kind rather
    /// than a `retired` key per copy - the 127 dismissals of `model_slow_first_byte` a head can
    /// carry are this one entry. See [`crate::prefs::leticode`].
    pub(crate) quiet: Quiet,
    /// The rooms sidebar: on or off, focused, and the rows it drew.
    pub(crate) sidebar: sidebar::Sidebar,
    /// The surface keys `leticode.toml`'s `[keys]` moved, resolved against the tables in
    /// `app::keys::bindings`.
    pub(crate) key_overrides: KeyOverrides,
    /// The profiles `leticode.toml`'s `[verbosity.<name>]` tables define, each the set of things
    /// it shows.
    pub(crate) profiles: Profiles,
    /// What this reader has read, by kind: the watermark `ctrl-n` leaves. A copy whose words
    /// differ is a different key, so a hide cannot say what the reader means; the newest `ts` per
    /// code can. Per kind, so a warning kind never seen before still has no mark to be older than.
    pub(crate) read_marks: Vec<(String, u64)>,
    /// How many of those are already in `hist_lines`.
    pub(crate) note_upto: usize,
    pub(crate) heads: usize,
    /// Counters. Every one is on the status line, because a number a head keeps and does not show
    /// is a number nobody can act on. Transcript rows the walk has drawn, ever: a row re-rendered
    /// ten times counts ten, which is the encoder for [`App::invalidate_history_from`].
    pub hist_renders: u64,
    pub seq: u64,
    pub dropped: u64,
    pub scrubbed: u64,
    pub resyncs: u64,
    pub rendered: u64,
    pub filtered: u64,
    /// Frames this build could not read and did not die of. A head that exits takes the session
    /// down with it, and one that steps over a frame in silence makes a newer daemon
    /// indistinguishable from a quiet one. The read mark is untouched: there is no seq to ack.
    pub unreadable: u64,
    /// Events the daemon sent and this head never got. `seq` is dense, so a frame more than one
    /// past the last is proof that something is missing. Counted and said, because a gap repaired
    /// silently looks identical to a session that never had one. See [`App::behind`].
    pub gaps: u64,
    /// How far the daemon says it is ahead of this head, in events. A head that is behind has an
    /// empty queue and a correct screen, which looks the same as current from inside; an
    /// `Accepted` or a `Rejected` carries the seq it is at, and a lower bound is enough.
    pub behind: u64,
    /// Bodies that arrived for rows this head does not hold: [`App::record_item`] drops a body
    /// whose id it cannot find, which makes that row undrawable for ever. One cause and it is not
    /// this head's fault: a snapshot replaces `items` and the daemon's view is bounded.
    pub orphan_bodies: u64,
    /// Times the provider was slow to send its first byte, and said so. A fact about the weather
    /// and not an event in the conversation, so it is a counter here and a warning triangle
    /// rather than a row. See `letibot_sessionlog::warning::ALARM_ONLY`.
    pub slow_first_byte: u64,
    /// The counter values this reader has already been shown: the `⚠` on the composer's edge is
    /// drawn while a counter exceeds its value here, so reading the screen clears it. Not persisted,
    /// because the counters die with this process and an ack on disk would outlive them.
    pub(crate) acked: Counters,
    /// Scroll offset from the bottom, in lines. 0 is "following the stream".
    pub scroll: usize,
    /// The composer: multi-line, with history, a kill ring, undo batching, a paste ledger and the
    /// two interrupt double-taps.
    pub(crate) editor: Editor,
    /// The room composer's own state - the history, drafts, reply target and completion cycle of
    /// the input in front - keyed by the input's key (`app::chat_input`).
    pub(crate) compose: chat_input::Compose,
    /// Folds. Reasoning starts folded; tool output starts folded.
    pub reasoning: Fold,
    pub tools: Fold,
    /// Whether tool calls are drawn in the raw form the model wrote them in.
    /// Off, and not a fold: a fold hides something the reader knows is there, while this reveals
    /// markup the default view must never show. Reachable, and never what anybody sees by accident.
    pub raw_calls: bool,
    pub(crate) notice: Option<String>,
    /// When the notice stops being news, on this head's own clock. `None` is a notice nobody
    /// started a clock on, which a direct write to `notice` can still make: a note with no clock
    /// is the note that can never be cleared, so every writer goes through [`App::say`].
    pub(crate) notice_until: Option<u64>,
    /// What the theme `head.toml` names could not supply, held from `apply_theme` until
    /// `load_prefs` says every problem of the file in one notice.
    pub(crate) theme_problems: Vec<String>,
    /// The setting being chosen, or nothing. One field for every setting card, because four
    /// openers each clearing the other three is a hand-kept invariant, and `Option` makes it
    /// structural. One renderer for all of them, because a second copy of a list drifts.
    pub(crate) pick: Option<Pick>,
    /// `allow-all`, held one keystroke short of sent: it admits the always-ask list, so it asks
    /// rather than refuses. Asked on every `allow-all`, a VM included where the daemon ignores the
    /// answer, because deciding otherwise would mean knowing whether the session is confined.
    pub(crate) mode_confirm: Option<String>,
    /// A confirmation the daemon put up because another head asked for it, where the bridge wants
    /// `allow-all` and cannot be the person whose box is the boundary. It rides the same card as
    /// the person's own; what it adds is the `req_id` the answer must name and who asked.
    pub(crate) mode_asked: Option<AskedMode>,
    /// When the card appeared: a card a frame opens sits under keys already on their way, so its
    /// first moments take no answer. See `MODE_ARM_MS`.
    pub(crate) mode_confirm_at: u64,
    /// Confirmations that arrived while the card was already up. The front card owns the keys,
    /// so a later one waits rather than silently replacing the request the screen is showing.
    pub(crate) mode_waiting: Vec<AskedMode>,
    /// A confirmation Esc put away: the daemon's request stays open, and alt-a brings it back.
    pub(crate) mode_away: Option<AskedMode>,
    /// Whether the mode card was up at the last `sync_front` - its change detector, since the
    /// card is not in `open` and the front-card detector cannot see it arrive.
    pub(crate) mode_up_for: bool,
    /// The new-todo card: title and detail, the composer being the field. `(title, detail,
    /// typing_the_detail)`, so the field under the cursor is never a keystroke behind. A keyboard
    /// owner like the password field, so `key` returns before the composer sees anything.
    pub(crate) todo_draft: Option<TodoDraft>,
    /// `todo_template` as this head loaded it: the starter-todo switch, carried on `App` because
    /// the seed runs at the attach, long after `load_prefs`. See `prefs::TodoTemplate`.
    pub(crate) todo_template: crate::prefs::TodoTemplate,
    /// The projects that have had their starter todos, as hashed workspace paths in `head.toml`.
    /// A record and not an *is the list empty* test: a starter row the person deletes must not
    /// come back.
    pub(crate) todo_seed: Vec<String>,
    /// A seed is waiting for the board: the next `TodosUpdated` copies the template's items onto
    /// the person's half. The board is read first because seeding against a stale one would omit
    /// rows the daemon holds, and `SetOperatorTodos` replaces the half.
    pub(crate) todo_seed_pending: bool,
    /// The quit card, opened by the second Ctrl+C instead of leaving at once: leave the head and
    /// keep the session warm, or stop both. A card, because the second answer is irreversible and
    /// a cold prefill of a long session takes minutes.
    pub(crate) quit_card: bool,
    /// Which row of the quit card the cursor is on. Seeded to *leave the head*, so Enter on an
    /// untouched card does the smaller thing.
    pub(crate) quit_sel: usize,
    /// The operator has been told which editor buffers leaving discards and answered anyway. Reset
    /// whenever the card opens: the first Enter on a card whose counts are not empty only asks.
    pub(crate) quit_armed: bool,
    /// The reseat card, `Some` exactly while the daemon says this session's prompt is stale: open
    /// in the ask slot, or pinned above the composer after *not now*. See `app::reseat`.
    pub(crate) reseat: Option<reseat::ReseatCard>,
    /// Which mode row the cursor is on. Seeded to the mode the session is
    /// already under, so Enter on an untouched list is a no-op rather than a
    /// surprise - the same rule the session picker's cursor follows.
    pub(crate) mode_sel: usize,
    /// Whether the open picker has been positioned by what it actually lists. `/mode` and `/models`
    /// open the card and ask for fresh rows in the same breath, so the seed reads an empty list and
    /// the cursor sits on row 0. Cleared by anything the reader does, so an answer cannot snap it back.
    pub(crate) pick_unseeded: bool,
    /// Which row the setting card's cursor was on at the last frame's scroll decision, so a cursor
    /// that MOVED takes the window with it and a window scrolled by hand is not pulled back every
    /// frame. `None` while no setting card is drawn.
    pub(crate) pick_sel_at: Option<usize>,
    /// Which value of the setting card each screen row of the last frame belongs to, as `(y, k)`
    /// for every row value `k` was drawn on, its wrapped `why` included. Empty unless the whole
    /// card fit, because a click is trusted only for a list the frame proved was all on screen.
    pub(crate) mode_hits: Vec<(usize, usize)>,
    /// The card the last frame drew in the slot and, for a decision, which request's: a click is
    /// answered only while that same card is still in front, since a key earlier in the read can
    /// have answered it - and a row index from the old card marks a row on the new one.
    pub(crate) drawn_card: Option<(Target, String)>,
    /// The rows the last frame's card in the slot took, `[from, to)`: its body is another party's
    /// text (a command, a question), so no hint is read off it.
    pub(crate) card_span: Option<(usize, usize)>,
    /// The count labels on the composer's top edge as the last frame drew them, `None` when the
    /// frame drew no edge or cut it. A click on a label opens the pane its count names.
    pub(crate) box_top_hits: Option<BoxTopHits>,
    /// The standing notes, as the daemon last answered `ListNotes`: the path, the abstract and the
    /// form. The size, the mtime and whether the file is still on disk are read at draw time,
    /// because they change while the pane is open. See `ui/panes/standing.rs`.
    pub(crate) standing: Vec<letibot_sessionlog::protocol::NoteEntry>,
    /// Which note row the cursor is on, an index into [`App::standing`] and the same list the
    /// drawn mark, the arrows and Enter read. Kept across a close.
    pub(crate) standing_sel: usize,
    /// The pane row each note was drawn on: the record the arrows scroll by, never arithmetic
    /// over the list. See `jobs_stop_rows` for the defect that avoids.
    pub(crate) standing_stop_rows: Vec<usize>,
    /// What the corpus looked like on the last draw, so a note edited while the pane is open is a
    /// change the pane can notice. `None` until the first draw after the rows land, which is the
    /// baseline rather than a change. See [`standing::Corpus`].
    pub(crate) standing_corpus: Option<standing::Corpus>,
    /// The project notes directory the daemon named, watched while it holds no note.
    pub(crate) standing_dir: Option<String>,
    /// The skills, as the daemon last answered `ListSkills`: a name, its one line and where it was
    /// found. Never a body, which the daemon injects on Enter (`Action::LoadSkill`).
    pub(crate) skills: Vec<letibot_sessionlog::protocol::SkillRow>,
    /// Which skill row the cursor is on, an index into [`App::skills`], kept across a close.
    pub(crate) skills_sel: usize,
    /// The pane row each skill was drawn on, the record the arrows move by.
    pub(crate) skills_stop_rows: Vec<usize>,
    /// Both queues as the daemon last answered `ListMergeQueue`: who serves each repository, the
    /// targets' health and the last moves. A `QueueMoved` asks again. Cut to the pane's scope from
    /// [`App::queues_all`], so every reader reads one copy.
    pub(crate) queues: letibot_sessionlog::event::QueueSnapshot,
    /// The daemon's snapshot whole, every repository's rows, with its word about which repository
    /// this session is in (`here`). The edge's standings read this.
    pub(crate) queues_all: letibot_sessionlog::event::QueueSnapshot,
    /// Whose queues the pane shows. See [`QueueScope`].
    pub(crate) queue_scope: QueueScope,
    /// When this head received `queues`, by its own clock: an age the pane draws is the
    /// snapshot's own plus how long ago it arrived. `0` until one has arrived.
    pub(crate) queues_got_ms: u64,
    /// When the pane last asked for a snapshot, so an open pane re-reads the heartbeats every
    /// few seconds and not on every frame.
    pub(crate) queues_asked_ms: u64,
    /// What each running gate's tail was when this head last looked, and when it last changed,
    /// keyed by `entry#step`. Only two sightings say it has not moved, and that is the difference
    /// between a slow step and a stuck one. See `ui::panes::queue::GateTail`.
    pub(crate) gate_tails: std::collections::HashMap<String, crate::ui::panes::queue::GateTail>,
    /// Which row the cursor is on, an index into the current view's rows (`App::queue_rows`), the
    /// same enumeration the drawn mark, the arrows and Enter read.
    pub(crate) queue_sel: usize,
    /// The pane row each entry was drawn on: the record the arrows scroll by, never arithmetic
    /// over the list. See `jobs_stop_rows` for the defect that avoids.
    pub(crate) queue_stop_rows: Vec<usize>,
    /// Which half of the queue the pane shows. The list is the same either way and what changes
    /// is the row under each entry. See [`QueueView`].
    pub(crate) queue_view: QueueView,
    /// Why the queue layer did not answer, when it did not: with one renderer the refusal is the
    /// whole pane, so the sentence is part of the drawing.
    pub(crate) queue_why: Option<String>,
    /// The entry whose detail overlay is open, by id, until Esc. By id and not by index, because a
    /// re-read after a `QueueMoved` can move the queue under it; the overlay reads the entry at draw
    /// time, so a move shows the new state rather than the state at the keypress.
    pub(crate) queue_open: Option<String>,
    /// The row folds the plugin's queue pane keeps, by row id: the ledger C3 gives the host so a
    /// fold is instant and survives the next answer. Sent as `Input::folded` and written by
    /// [`App::queue_fold`] for a row whose act is `{"fold": true}`.
    pub(crate) queue_folds: std::collections::BTreeMap<String, bool>,
    /// The view the plugin last answered, kept so a keypress reads the row that was drawn, which
    /// is what `a`/`v`/`r`/`d` act on.
    pub(crate) queue_held: Option<letibot_sessionlog::pane::View>,
    /// How many stops that view has, so the arrows walk what was drawn rather than the host's own
    /// rows.
    pub(crate) queue_plugin_stops: usize,
    /// Which job row the cursor is on: an index into [`App::job_stops`] and not [`App::jobs`], so
    /// the drawn cursor and Enter cannot disagree. Enter asks the daemon for that job's output.
    pub(crate) jobs_sel: usize,
    /// Whether the jobs pane's `finished` group is unfolded. Collapsed by default, so the finished
    /// jobs do not push the running ones off the screen. Enter on the group row toggles it.
    pub(crate) jobs_finished_open: bool,
    /// The pane row each job stop was drawn on, which is what the arrows scroll by. See
    /// [`App::jobs_row_of`].
    pub(crate) jobs_stop_rows: Vec<usize>,
    /// Which row of the pane the cursor is on, an index into [`App::subagent_stops`] and not into
    /// [`App::subagents`]: arrows move it, Enter switches into the child it names or folds the
    /// `finished` group.
    pub(crate) subagents_sel: usize,
    /// Whether the `finished` group is unfolded; collapsed by default so finished children cannot
    /// push the running ones off the screen. Climbing out of a child must not unfold it to land the
    /// cursor: it is the head's own state, handed to the pane each frame.
    pub(crate) subagents_finished_open: bool,
    /// The pane row each stop was drawn on, the same record [`App::todos_stop_rows`] keeps for its
    /// own pane and for the same reason: the arrows scroll by what the pane wrote, never by
    /// arithmetic over the lists it drew from. See [`App::subagents_row_of`].
    pub(crate) subagents_stop_rows: Vec<usize>,
    /// The output view `p` opens on a subagent row, until Esc closes it.
    pub(crate) sub_out: Option<SubOut>,
    /// The subagent whose output was asked for and not yet answered. Esc cancels.
    pub(crate) sub_out_pending: Option<String>,
    /// The surface that asked for the output, as the top of the stack when the read was sent,
    /// `None` for a `/peek` typed over the conversation. Closing that surface cancels the read,
    /// so a late `Peeked` does not open over whatever the person went on to.
    pub(crate) sub_out_from: Option<Surface>,
    /// The child this head climbed up out of, read once by [`App::fold_subagents`] so the cursor
    /// lands on that child's row. A row id and not an index, because the rows are rebuilt the
    /// moment the parent's `Hello` lands.
    pub(crate) up_from: Option<String>,
    /// The change a click opened, as a popup over the conversation. `None` when nothing is open.
    /// See `ui::diff_popup`.
    pub(crate) diff_popup: Option<crate::app::diff_popup::DiffPopup>,
    /// The job-output view the jobs pane's Enter opens, until Esc returns to the
    /// jobs list. The bytes the pane was counting, finally shown in the pane.
    pub(crate) job_out: Option<JobOut>,
    /// The session's todo list, as the last `TodosUpdated` said it was. Seeded by
    /// the `Todos` reply when the pane first opens; carried forward by the events.
    pub(crate) todos: Vec<letibot_sessionlog::event::TodoEntry>,
    /// The Claude Code seats' boards the daemon's store holds, drawn under the session's: a seat is
    /// not a session, so it arrives on the `Todos` read and this head only ever draws it. See
    /// [`App::seat_board_lines`].
    pub(crate) seat_boards: Vec<letibot_sessionlog::event::SeatBoard>,
    /// The repo's `TODO.md` as a section map, read once per pane-open. The file
    /// can be longer than the pane and is the operator's to edit; the map is what
    /// a pane can honestly show.
    pub(crate) repo_todos: Option<Vec<TodoRow>>,
    /// What the file looked like when it was last read: `(mtime, len)`. One `stat` per draw and not
    /// a watcher, because `TODO.md` is edited exactly while somebody is looking at it. The length as
    /// well as the mtime, because a one-second mtime can miss two writes.
    pub(crate) repo_todos_at: Option<(std::time::SystemTime, u64)>,
    /// The cursor, as an index into [`App::todos_stops`], so the arrows, the click, the drawn mark
    /// and Enter cannot disagree about which row the cursor is on.
    pub(crate) todos_sel: usize,
    /// Where each stop was drawn, parallel to [`App::todos_stops`], recorded as the rows go out and
    /// never recomputed. The arithmetic would be the defect: a click has a screen row and nothing
    /// else, and only the pane knows which stop it drew there.
    pub(crate) todos_stop_rows: Vec<usize>,
    /// Where each row's `waits on …` link landed on the last frame, in the pane's own rows and
    /// columns, written by `todos_lines` while it drew: only the draw knows where the words went.
    /// See [`WaitLink`].
    pub(crate) todos_wait_links: Vec<WaitLink>,
    /// The todo row whose whole text is open, by its identity, until Esc: the only place a row cut
    /// off at the pane's width exists in full. A [`TodoStop`] and not an index, for the reason
    /// `queue_open` is an id. The board has no ids, so a row whose words changed is a different row.
    pub(crate) todos_open: Option<TodoStop>,
    /// The todos pane has the done group folded away. `d` flips it and the answer is kept in
    /// `head.toml`, because a switch that forgets is a switch the reader keeps re-making.
    /// It folds `completed` rows and only those: the rows set aside and struck off are not finished.
    pub(crate) todos_fold_done: bool,
    /// The next draw of the todos pane scrolls the window to the cursor. A change that moves the
    /// cursor with no row number to move it by, such as folding the done group, has no record to
    /// scroll by until the next frame writes one, so it asks the draw to follow instead.
    pub(crate) todos_follow: bool,
    /// A layer's failure sentence, waiting for the act it explains. `say` keeps only the last
    /// line, so a `/todo` line whose layer could not answer is answered by the head first and this
    /// is said after it: the failure must never be what the row's own sentence replaces.
    pub(crate) layer_failure: Option<String>,
    pub(crate) repo_sel: usize,
    pub(crate) repo_open: bool,
    /// A slash reply that is a listing, not a sentence: `(title, lines)` for a screen you open and
    /// close, `None` when it is shut. A one-line confirmation is still a note, but a `/job`
    /// answer's sixteen kilobytes of output must not land in the scrollback.
    pub(crate) slash_out: Option<(String, Vec<String>)>,
    /// The surface stack: what holds the conversation's rectangle, top last. See `app::surface`
    /// for the rule and for the nine `bool`s it replaced.
    pub(crate) surfaces: Vec<Open>,
    /// The last frame's click targets. Rebuilt by every frame and read by every click and wheel
    /// step that knows where the pointer is. See `app::hits`.
    pub(crate) hits: Vec<Hit>,
    /// The URL behind each of the last frame's link targets: a [`Target::Link`]'s stop indexes
    /// this, so a click answers with the URL the frame drew and not with a second read of the row.
    /// Rebuilt with the frame by `App::draw_links`.
    pub(crate) link_targets: Vec<String>,
    /// The first row of the frame's chrome, as the last frame put it: the status row, a card in
    /// the slot, the composer and the hint bar. A link is a target only above this row, since a
    /// URL the person is typing is text and a click in it belongs to the caret.
    pub(crate) body_rows: usize,
    /// The last frame's key hints: every `key label` on the glass, a button that types its key.
    pub(crate) hint_rows: Vec<keyhints::HintRow>,
    /// Keys a click typed, for the head's next read to take before the terminal's.
    pub(crate) injected: Vec<u8>,
    /// The card in front when [`Self::injected`] was queued; see `take_injected`.
    pub(crate) injected_for: String,
    /// Where the pointer is, the one fact the hover keeps. What it lights is worked out against
    /// the frame being drawn ([`App::relight`]), never kept from an earlier one.
    pub(crate) pointer_at: Option<(u16, u16)>,
    /// What the pointer is over on this frame, if a click there would act: derived from
    /// `pointer_at` by [`App::relight`] on every draw.
    pub(crate) hovered: Option<pointer::Hovered>,
    /// The left button's run since its press.
    pub(crate) drag: pointer::Drag,
    /// The cells a drag selected, press to pointer: drawn reversed, copied on release.
    pub(crate) selection: Option<((u16, u16), (u16, u16))>,
    /// The pointer shape the terminal should switch to, when it changed (the hand or not).
    pub(crate) pointer_hand: Option<bool>,
    /// The view is held because a selection began, not because the person pressed ctrl-p, so
    /// dropping the selection lets it follow again. See `App::drag_to`.
    pub(crate) selection_held: bool,
    /// The scroll of the surface on top of the stack, the one every pane draws through
    /// (`pane_window`) and every key moves; a covered surface keeps its own in its stack entry.
    /// One field for all of them, because only one pane is open at a time.
    pub(crate) pane_scroll: usize,
    /// What the last draw of a pane measured: how many rows it had, and how many fitted. Kept so a
    /// cursor moved by a keypress can scroll itself into view, since the key handler has no width
    /// or height of its own.
    pub(crate) pane_len: usize,
    pub(crate) pane_room: usize,
    /// The channel column's width at the last `/rooms` draw: sized from the labels and the
    /// pane width, so a test reading a row's left half reads what the frame actually used
    /// instead of re-deriving it and drifting.
    pub(crate) rooms_left_w: usize,
    /// How far into an unfolded row's payload the reader has paged, in wrapped display lines from
    /// the head and clamped at draw time. A result can wrap to thousands of lines and the fold's
    /// seam must reveal more of one row, not just allow long rows.
    pub(crate) payload_page: usize,
    /// The furthest `payload_page` that still shows a full window, written by the draw, the only
    /// place that knows the payload's wrapped length, and read by the keys to clamp. Without it
    /// Down added steps past the end while the screen stood still. `usize::MAX` until drawn.
    pub(crate) payload_max: std::cell::Cell<usize>,
    /// The key that asked, so the arrows page only the row whose view is open: several rows can be
    /// open on one screen, and the panel says `▸ paging <subject>` so the reader knows which.
    pub(crate) payload_sel: Option<String>,
    /// What this conversation has cost, in micro-USD, summed over the turns this head saw finish.
    /// It covers only watched turns and says so rather than inventing a total that is wrong in the
    /// direction that costs money.
    pub(crate) spent_micros: u64,
    /// Whether any turn this head saw carried a cost at all. Distinguishes "free,
    /// so nothing to show" from "metered and nothing has finished yet".
    pub(crate) spent_seen: bool,
    pub(crate) quit: bool,
    /// The glass is not what the painter last sent, so the next frame erases and repaints every
    /// row. Only Ctrl-L sets it: every frame is built and diffed anyway, so a state change needs
    /// no flag, and setting this for one erases the screen and reads as a flicker.
    pub(crate) repaint: bool,
    /// The view is held: the head writes nothing at all, so a mouse selection survives a
    /// streaming turn. Events keep arriving and folding; only the drawing stops.
    /// See [`App::toggle_hold`].
    pub(crate) hold: bool,
    /// The frozen frame, composed once when the hold began and returned byte for byte
    /// thereafter, so the terminal's own diff writes nothing at all.
    pub(crate) hold_frame: Option<Vec<String>>,
    /// The size `hold_frame` was composed for. A resize moves every row, so the freeze owes
    /// exactly one more frame there.
    pub(crate) hold_size: (usize, usize),
    /// `items.len()` when the hold began, so the release can say how much arrived while it was
    /// held. Counted once, at that moment: a live count is an animation and an animation writes.
    pub(crate) hold_rows: usize,
    /// Which rows of the sessions tree the picker has opened. Empty is closed for every node, and a
    /// list with nothing open still draws where you are: the project holding this session and its
    /// chain of parents, which keeps twenty subagents from burying the conversations.
    pub(crate) expanded: Vec<String>,
    /// Wall clock fed in by the driver, and when this head last had anything from the daemon.
    /// Received-at rather than the event's `ts`, because the daemon's opinion of how long it has
    /// been quiet is exactly the number missing when it stopped talking. Zero says nothing.
    pub(crate) now_ms: u64,
    pub(crate) last_event_at: u64,
    /// The workspace's git field, as the pieces the format in force asks for: `(text, role)` pairs
    /// the header renders and paints per role. Fitting happens at the draw, where the width is,
    /// and the branch is the floor. See `crate::gitfield`.
    pub git: Option<Vec<(String, crate::gitfield::GitRole)>>,
    /// The reading the pieces were rendered from: the facts and not the text, so a format changed
    /// on `/config` re-renders from the cache rather than waiting out the reader's interval.
    pub(crate) git_state: Option<crate::gitfield::GitState>,
    /// The git field's template, as loaded: `None` is the shipped default
    /// (`gitfield::GIT_FORMAT_DEFAULT`). Held on the `App` because the field renders on the reader
    /// thread, long after `load_prefs`.
    pub(crate) git_format: Option<String>,
    /// Which workspace that reading was of, and when. A switch to another session carries another
    /// path, and a field left over from the previous tree would be drawn as this one's branch.
    pub(crate) git_read: (String, u64),
    /// The git reading in flight: the workspace it is of and the channel its answer arrives on.
    /// `git status` is a process and can take seconds, so the loop starts it and collects it and
    /// never waits. One at a time: a reading that outlives the interval is not joined by a second.
    pub(crate) git_job: Option<(
        String,
        std::sync::mpsc::Receiver<Option<crate::gitfield::GitState>>,
    )>,
    /// What takes a reading: [`crate::gitfield::read_state`] in production, and a reader a test
    /// can hold down to prove the loop does not wait for it.
    pub(crate) git_reader: GitReader,
    /// Package sessions of other folders' daemons, as last read, and what reads them. `None`
    /// reader means this head cannot see another daemon (a replay, a test).
    pub(crate) fleet: Vec<letibot_sessionlog::FleetRow>,
    /// The operator's own top-level sessions of other daemons, for the rooms sidebar.
    pub(crate) fleet_mine: Vec<letibot_sessionlog::FleetRow>,
    pub(crate) fleet_reader: Option<letibot_sessionlog::FleetReader>,
    pub(crate) fleet_unlisted: usize,
    pub(crate) fleet_job: Option<std::sync::mpsc::Receiver<letibot_sessionlog::Fleet>>,
    pub(crate) fleet_read: u64,
    /// The workspace's repositories as last read: one for an ordinary workspace, several for a
    /// folder of them (`gitfield::repos_in`).
    pub(crate) git_repos: Vec<std::path::PathBuf>,
    /// Which of them the git field reads: the one pinned from the repo card, else the one the
    /// session's newest edit or command was in (`App::followed_repo`). `None` with one repository,
    /// and with several and nothing to follow.
    pub(crate) git_dir: Option<std::path::PathBuf>,
    /// The session repository's GitHub references, its `owner/repo`, branches and commits, so the
    /// words on the screen that name them become links (`app::ghlinks`).
    pub(crate) gh_refs: Option<std::sync::Arc<ghlinks::GitRefs>>,
    /// Whether a `link_` layer is on disk at all, and when it was last asked: read from manifests
    /// alone, so a session with none never compiles a plugin stack for links
    /// (`Host::any_link_layer`), and read again every minute so a layer written in this session
    /// starts answering without a restart.
    pub(crate) link_layers: Option<bool>,
    pub(crate) link_layers_at: u64,
    /// Whether a `card_` layer is on disk at all, and when it was last asked: the same rule as the
    /// links' - manifests alone, so a session whose cards are the head's own never compiles a
    /// plugin stack for them (`Host::any_card_layer`), asked again every minute so a layer written
    /// in this session starts drawing without a restart.
    pub(crate) card_layers: Option<bool>,
    pub(crate) card_layers_at: u64,
    /// The background read of them, and which repository it is for.
    pub(crate) gh_job: Option<(String, std::sync::mpsc::Receiver<Option<ghlinks::GitRefs>>)>,
    /// Which repository was last read, and when.
    pub(crate) gh_read: (String, u64),
    /// The repository the person pinned on the repo card: `None` follows the work.
    pub(crate) git_pinned: Option<std::path::PathBuf>,
    /// The repo card's rows, read once together when it opens, so a card left open is not a
    /// reading on every frame (the loop never waits for git; see `refresh_git`).
    pub(crate) repo_rows: Vec<(String, String)>,
    /// The live marker's join, as the pair that lets it be undone: the line's text before the
    /// marker was glued, and the marker. The marker is appended to a line in `hist_lines`, and a
    /// cached row a derived overlay mutates cannot be recomputed. `None` while nothing is joined.
    pub(crate) live_join: Option<(String, String)>,
    /// Everything the marker draws about now, as one value, and the cache key for the row it is
    /// painted into. [`hidden_run_marker`] reads this value and has no other door to now, so a fact
    /// the marker draws is a fact the key holds and the row cannot go stale behind it.
    pub(crate) marker_facts: MarkerFacts,
    /// The model this session is talking to, kept past the end of a turn: the composer's line has
    /// to say what it is talking to while nothing is running, which is most of the time. It
    /// arrives on `Hello` too, so a head with no turn yet has an answer.
    pub(crate) model: String,
    /// The display target, by call id, for the round the walk is inside and no other: a call id is
    /// positional within one round, so a session-scoped table made every settled card read
    /// whichever round wrote last. Replaced wholesale at each `Assistant` row.
    pub(crate) call_targets: std::collections::HashMap<String, String>,
    /// How long the call behind a settled `tool_result` row took, by item id, which is unique per
    /// row. Absent for a row this head did not watch run, and the card then shows no duration
    /// rather than a fabricated one.
    pub(crate) call_ms: std::collections::HashMap<String, u64>,
    /// Both sides of every file a settled `edit`/`write`/`bash` row changed, by item id: what puts
    /// a diff on the screen at all, since the transcript takes a call over the moment its result row
    /// lands. A list because one call can change several files (R161). Absent for rows older than
    /// the turn it carries, which then show the tool's own text.
    pub(crate) call_edits:
        std::collections::HashMap<String, Vec<letibot_sessionlog::event::ToolEdit>>,
    /// The files a settled JOB changed while it ran, by handle: the daemon snapshots the tree at the
    /// spawn and reads it back here, and the completion's row is where they are drawn (R161).
    /// A `BTreeMap` so the oldest can be dropped when a session settles more jobs than a reader will
    /// scroll back through.
    pub(crate) job_edits:
        std::collections::BTreeMap<String, Vec<letibot_sessionlog::event::ToolEdit>>,
    /// The settled decision a `tool_result` row's call was gated by, by item id: the call id is
    /// round-positional, so it cannot be the key. It keeps the oracle's brief and reply on a card
    /// that has settled, which would otherwise leave with the live card.
    pub(crate) call_decisions:
        std::collections::HashMap<String, letibot_sessionlog::view::SettledDecision>,
    /// The total body length of the last frame, so `Up` can be clamped to it.
    pub(crate) body_len: usize,
    /// The protocol version the daemon last said it speaks. `None` until a daemon has answered,
    /// which is not the same as speaking zero. Kept because `/status` must answer which build is on
    /// the other end long after the handshake. The comparison is [`letibot_sessionlog::protocol_skew`].
    pub(crate) daemon_protocol: Option<u32>,
    /// Which daemon this head is drawing: the socket's peer pid (`SO_PEERCRED`, so an identity and
    /// not a guess) and the `PROTOCOL_VERSION` from the `Hello`. A head outlives its daemon, and a
    /// daemon that comes back is not the one whose answers are on the screen.
    pub(crate) daemon_seat: Option<DaemonSeat>,
    /// The seat a person has already said *attach anyway* for: refusing is half the ruling and this
    /// is the other half's memory, set by [`App::attach_anyway`] and read by [`App::skew_locked`].
    /// A seat and not a boolean, because the override lasts for the daemon it was given on.
    pub(crate) skew_override_for: Option<DaemonSeat>,
    /// The daemon connection, as far as this head can tell. On the head and not in the driver
    /// because the screen shows it: a head with no daemon draws the conversation it has plus a
    /// line saying the connection is down and for how long. See [`Link`].
    pub(crate) link: Link,
    /// True between taking the screen and the daemon's `Hello` arriving. The `Hello` carries the
    /// whole snapshot, so attaching is a round trip; a head that draws before it has that answer
    /// must claim nothing, so the empty-transcript banner is suppressed for the walking cat.
    pub(crate) attaching: bool,
    /// The session this head asked to resume and has not been seated in yet. `--continue` and the
    /// picker's resume are two steps, and the `Hello` for the first is not the answer: while this
    /// is set, a `Hello` for another session leaves [`Self::attaching`] up.
    pub(crate) fetching: Option<String>,
    /// How many rows the store holds while this head holds none of them: the daemon is restoring.
    /// The count is the store's and the rows are the snapshot's, so a session registered but not
    /// yet opened has an empty one and must never be drawn as one that has said nothing.
    pub(crate) restoring: Option<u32>,
    /// When the attach began, on the clock `App::clock` is given. The cat's frame comes from the
    /// elapsed time rather than a counter, so `screen()` stays a pure function of the clock and
    /// the wait can be driven from a loop, a test or a handshake thread.
    pub(crate) attach_started_ms: u64,
    /// Where the terminal's caret belongs, from the last frame.
    pub(crate) cursor: Option<(usize, usize)>,
    /// The daemon's reason for ending this head, kept past the screen. See
    /// [`App::farewell`].
    pub(crate) bye: Option<String>,
    /// The daemon's pid, from `SO_PEERCRED` on this head's connection. Set by the caller that owns
    /// the socket, so `/status` can answer what the person would otherwise take to `ps`.
    pub(crate) daemon_pid: Option<i32>,
    /// What the reader's viewport is holding: `None` is following the tail, `Some` is a reader who
    /// scrolled back. A row and an offset into it, never a line count, because a snapshot replaces
    /// the transcript whole and both counts are invalidated by that. See [`App::hold`].
    pub(crate) anchor: Option<Held>,
    /// The last alt-g / alt-b and when: a second press within five seconds opens the note.
    /// See `app/feedback.rs`.
    pub(crate) fb_last: Option<(bool, u64)>,
    /// Where the reader was when a snapshot replaced the rows under them, taken before the old rows
    /// went. A place does not cross a transcript boundary by counting, since ids are per transcript:
    /// what survives is the row's own words and the line. Spent by [`App::repair_anchor`].
    pub(crate) carry: Option<Carry>,
    /// How many wheel notches down this flick has counted, when the last one fell, and how many rows
    /// the transcript held when it did. A notch walks three lines, which cannot walk a reader 190
    /// lines back to a receding tail; a flick gathers speed and the row count says if it is moving.
    pub(crate) wheel_run: usize,
    pub(crate) wheel_last_ms: u64,
    pub(crate) wheel_items: usize,
    /// Where each rendered row's lines are, ascending by `at`. Rebuilt as the history is walked,
    /// prepended to, and cleared whenever that buffer is thrown away.
    pub(crate) spans: Vec<Span>,
    /// The body line at the top of the last frame's window, and how many lines it had. The key
    /// handler runs between frames, so it answers where the reader is looking from what the last
    /// frame drew rather than forming a second opinion about the person's screen.
    pub(crate) view_top: usize,
    pub(crate) view_room: usize,
    /// Names this head's door calls so the daemon can tell them apart: the `call_id` is
    /// `{head}-{n}` with the count per head, which makes it unique within the session, the only
    /// property the daemon's pending set needs.
    pub(crate) head_run_seq: u64,
    /// The echoes a snapshot could not resolve: `pending_prompts` claims the daemon owes a row, and
    /// after a snapshot the head cannot tell *still coming* from *replaced by a fork*. So the echo
    /// says `unconfirmed` and retires when a row lands. See [`App::retire_pending`].
    pub(crate) unconfirmed: Vec<String>,
    /// Whether an echo is drawn in full or as its elided headline. Folded by default and flipped
    /// by `/t`: one key for one idea, so a reader who wants the long rows shown whole asks once.
    pub(crate) echo_open: bool,
    /// A stop this head asked for and has not finished: `Some` from the frame going out until the
    /// daemon is gone or the deadline passes. While it is set and unresolved [`App::should_quit`]
    /// is false - the head does not exit until it can say the daemon has gone.
    pub(crate) stopping: Option<Stopping>,
    /// The `/rooms` view: channels, messages read, what was shown.
    pub(crate) rooms: rooms::RoomsView,
    /// P4's navigation: the message cursor, the open search, the switcher, and how far the reader
    /// had read at the tail.
    pub(crate) nav: chat_nav::Nav,
    /// P4's pictures: the blobs this head holds, the ones it asked for, and the uploads on their
    /// way out.
    pub(crate) media: chat_media::Media,
    /// P5's chat layer: the feed generation it last answered and the view it answered with, held
    /// here so a frame costs no call and a push re-asks once.
    pub(crate) chat_pane: chat_plugin::ChatPane,
    /// A bulk announcement the daemon has not filled yet, recorded only when a snapshot is
    /// ingested and never by a live `TranscriptAppended`: a live row is body-less for the window of
    /// every ordinary message, so a trigger built on that would fire on a healthy session.
    pub(crate) bulk: Option<Bulk>,
    /// A fill the daemon named: `what`, `unit`, `done`, `total`, `None` when nothing runs. Cleared
    /// once `done >= total`, because the finish is a note and not a line that stays. The daemon's
    /// count and not a count of body-less rows: four things produce those and only some are a carry.
    pub(crate) filling: Option<(String, String, u64, u64)>,
    /// When the fill in [`App::filling`] started, on this head's clock: re-armed when the daemon
    /// names a different operation. The status row's clock reads from it, and with two progresses
    /// live the one that started last is the one shown ([`App::progresses`]).
    pub(crate) filling_since: u64,
    /// The daemon is still starting: the last `daemon_starting` step, until `daemon_started`. The
    /// socket listens before the worker runs, so this says a session cannot run anything yet, and
    /// labels a prompt typed meanwhile as running once the daemon has started.
    pub(crate) booting: Option<String>,
    /// This daemon's binary is older than the store it serves, as the daemon's own sentence;
    /// `None` is every ordinary attach. State and not a row: an older binary keeps working when
    /// the newer schema only added things, and says so in red on the status line every frame.
    pub(crate) store_newer: Option<String>,
    /// What the daemon is holding back from the model right now, `(jobs, subagents, failed)` from
    /// the last `HeldResults`, `None` once all are zero. Drawn on the turn's status row
    /// ([`App::held_label`]).
    pub(crate) held: Option<(u64, u64, u64)>,
    /// What the daemon says this session is waiting on: quiet, or busy with the list. Folded from
    /// `ActivityChanged` and seeded from a snapshot, drawn as one muted line on the status row
    /// ([`App::activity_line`]). `Quiet` is all a daemon without the frame can say.
    pub(crate) activity: letibot_sessionlog::activity::Activity,
    /// A fold's long wait, in the compaction's own units. Its own field and not a second use of
    /// `turn.progress`: an overrun compaction summarises a scratch transcript, and forwarding that
    /// count as the session's drew the scratch context as the session's.
    pub(crate) compacting: Option<CompactionLine>,
    /// When the fold in [`App::compacting`] started, on this head's clock: its first
    /// `CompactionProgress`. See [`App::filling_since`].
    pub(crate) compacting_since: u64,
    /// Why this fold is running, in the daemon's own words: the wall, a refusal, which fold was
    /// chosen. State and not a note, so it can be the progress line's click while the one row a
    /// compaction leaves is its report.
    pub(crate) compacting_why: Option<String>,
    /// What a click on the status row opens, as the last frame drew it: `(title, text)`, or `None`
    /// when the row said everything it had. A cut flash notice, or a progress whose row form left
    /// something out. See [`App::status_row`].
    pub(crate) status_click: Option<(String, String)>,
    /// Pre-emptive compaction is off and why, as the daemon rendered it; `None` draws nothing.
    /// A state, fed by the live warning and by the settings row for a later head. The `off -`
    /// spelling only the guard writes is the key, so a typed `--no-auto-compact` is not re-announced.
    pub(crate) auto_compact_off: Option<String>,
    /// The person has paused this session's plan-check, carrying the daemon's `until ...` words
    /// (empty until the settings that name the end). Drawn every frame, because a board that
    /// stopped asking looks exactly like a clock that broke.
    pub(crate) nags_paused: Option<String>,
    /// The daemon's `asks` settings row while the board waits on the person, `None` when it says
    /// `none`: the durable half of the waiting line for a head that has not read the board. Never
    /// refreshed, since settings rows are stored for the next request and nothing is pushed.
    pub(crate) asks_setting: Option<String>,
    /// Which session's board this head has read: the mark that ends the settings row's authority,
    /// since a question lives on a row and the board is the whole truth about who waits. A session
    /// id, so a switch invalidates it; cleared on every `Hello`.
    pub(crate) asks_board: Option<String>,
    /// Question cards the person put away with Esc, oldest first: the model's `ask_user_question`
    /// cards and the todo rows' cards alike, each with the words and the row that were on it.
    /// See [`AwayCard`] and [`App::put_question_away`].
    pub(crate) away: Vec<AwayCard>,
    /// What alt-h folded away, by [`App::waiting_keys`]: `None` while nothing is folded. The fold
    /// holds while everything waiting is something it hid. See `app/waiting.rs`.
    pub(crate) asks_hidden: Option<Vec<String>>,
    /// Question and permission cards answered from this head and not yet settled: closed the
    /// moment the answer left, so a second Enter cannot send it again, and kept until the daemon
    /// settles them or refuses the answer, which puts the card back in front.
    pub(crate) answering: Vec<crate::app::todos::Answering>,
    /// Rows a warning's card stands for, one event one row: the row's `item_id` to the key of the
    /// note whose card draws the event, so the row itself draws nothing. Keyed by id, because a
    /// snapshot renumbers nothing: an index-keyed map had to be cleared on every resync.
    pub(crate) absorbed: std::collections::HashMap<String, String>,
    /// Todo questions this head has answered whose board has not come back, as `(row, question)`.
    /// [`App::operator_asks`] leaves them out so the card can move on and the resident line counts
    /// what is left; a fresh board clears the list, so a refused answer is asked again.
    pub(crate) todo_answered: Vec<(String, String)>,
    /// A model switch the daemon has taken and not yet applied: its own sentence, or `None`.
    /// Drawn as state ([`App::switch_line`]), because a note would scroll away inside the very
    /// turn it is about while the header keeps naming the old model - true, and half of it.
    pub(crate) model_pending: Option<String>,
    /// The number the next retry row is keyed by. A series is ONE row whose value moves, so the
    /// row needs an identity that survives those moves and that a later outage cannot reuse.
    pub(crate) retry_series: u64,
}

/// A confirmation another head asked for, as this screen holds it. Beside [`App::mode_confirm`]
/// and not instead of it: the two are one card with two answers, and this one must be answered by
/// its id or the daemon cannot tell which request was settled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskedMode {
    pub req_id: String,
    pub by: String,
    /// The point being confirmed: `allow-all`, the only one ever asked about.
    pub name: String,
}

/// The two words an echo can carry. Constants because the renderer and the tests both name them,
/// and a mark spelled twice can be spelled differently. Both reach the screen as `queued`: the
/// states stay separate in the code ([`App::unconfirmed`]) but the reader needs one word.
pub const QUEUED: &str = "queued";
pub const UNCONFIRMED: &str = "queued";

/// The opening delimiter of a screen sent by `/cells`, and its closing one. A marker, because two
/// readers need the edges: the model, to know where the words stop, and this head, to fold a copy
/// of its own screen out of its transcript ([`fold_cells`]).
pub(crate) const CELLS_OPEN: &str = "\u{27e6}screen ";
pub(crate) const CELLS_MARK_END: &str = "\u{27e7}";
pub(crate) const CELLS_CLOSE: &str = "\u{27e6}end screen\u{27e7}";

// Every verb the dispatcher acts on, checked against [`SLASH_COMMANDS`] by a test rather than
// derived: a `match` is not reflectable, so a test reads [`App::command`] and fails when an arm is
// not offered. Aliases and the daemon's verbs are not enumerated here.

impl App {
    pub fn new(cfg: RenderConfig) -> Self {
        App {
            cfg,
            visibility: Visibility::starting(),
            diff_split: true,
            config_sel: 0,
            prefs_path: None,
            settings: Vec::new(),
            term: None,
            edit_pane: None,
            edit_area: rano::editor::Area::default(),
            call_rows: Vec::new(),
            file_rows: Vec::new(),
            notice_rows: Vec::new(),
            task_rows: Vec::new(),
            task_switches: Vec::new(),
            note_cards: Vec::new(),
            card_rows: Vec::new(),
            payload_rows: Vec::new(),
            marker_rows: Vec::new(),
            lift: None,
            run_tools: Fold::Folded,
            job_tails: Default::default(),
            term_fact: PaneFact::Unasked,
            term_ask: None,
            close_pending: false,
            close_which: None,
            session_id: String::new(),
            head_id: String::new(),
            seated: None,
            wiring: SessionWiring::default(),
            sessions: Vec::new(),
            subagents: Vec::new(),
            jobs: Vec::new(),
            jobs_event_seq: 0,
            jobs_in_flight: false,
            jobs_again: false,
            monitors: Vec::new(),
            monitors_sel: 0,
            monitors_stop_rows: Vec::new(),
            plugin_panes: std::collections::BTreeMap::new(),
            plugin_open: None,
            plugin_screen: Default::default(),
            plugin_sel: 0,
            plugin_drawn: Vec::new(),
            plugin_drawn_for: None,
            plugin_stop_rows: Vec::new(),
            plugin_panel_rows: Vec::new(),
            plugin_legend_rows: Vec::new(),
            plugin_legend_at: Vec::new(),
            plugin_seam_rows: Vec::new(),
            plugin_seam_at: Vec::new(),
            plugin_detail: None,
            plugin_sent: None,
            picker_sel: 0,
            picker_seat_pending: false,
            home: std::env::var("HOME").unwrap_or_default(),
            sessions_host: std::cell::RefCell::new(None),
            host_build: std::cell::RefCell::new(None),
            #[cfg(test)]
            host_inline: std::cell::Cell::new(false),
            picker_folds: std::collections::BTreeMap::new(),
            removed: Vec::new(),
            removal_armed: None,
            sessions_event: std::cell::RefCell::new(None),
            picker_hits: Vec::new(),
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
            chips: Vec::new(),
            attachments_dir: None,
            clipboard: crate::app::attachments::default_clipboard(),
            pasted_images: 0,
            bound_prompts: std::collections::HashMap::new(),
            fork_pending: Vec::new(),
            want_new_session: false,
            usage: None,
            usage_cache_measured: true,
            last_timings: None,
            prefix_check: None,
            dialect_template_sha: String::new(),
            items: Vec::new(),
            hist_lines: Vec::new(),
            hist_upto: 0,
            hist_floor: 0,
            hist_ceiling: None,
            hist_limit: HIST_LIMIT,
            walk_ahead: usize::MAX,
            items_base: 0,
            items_lead: 0,
            items_below: 0,
            fetch_asked: None,
            fetch_below: None,
            no_more_above: false,
            held_limit: HELD_LIMIT,
            model_from_settings_at: 0,
            model_from_live_at: 0,
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
            proto: rano::term::Proto::None,
            pictures: rano::term::Pictures::default(),
            picture_rows: Vec::new(),
            picture_refs: Vec::new(),
            prompt: None,
            prompt_buf: String::new(),
            secret_away: false,
            prompt_away: false,
            key_ask: None,
            key_buf: String::new(),
            reach: Default::default(),
            screen_requests: Vec::new(),
            term_cols: 0,
            sel: 0,
            sel_for: String::new(),
            held_line: None,
            notes: Vec::new(),
            dec_scroll: 0,
            dec_scroll_for: String::new(),
            dec_content_len: 0,
            dec_content_room: 0,
            dismissed: Vec::new(),
            quiet: Quiet::shipped(),
            sidebar: sidebar::Sidebar::default(),
            key_overrides: KeyOverrides::default(),
            profiles: Profiles::default(),
            read_marks: Vec::new(),
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
            compose: Default::default(),
            model: String::new(),
            call_targets: std::collections::HashMap::new(),
            call_ms: std::collections::HashMap::new(),
            call_edits: std::collections::HashMap::new(),
            job_edits: std::collections::BTreeMap::new(),
            call_decisions: std::collections::HashMap::new(),
            reasoning: Fold::Folded,
            tools: Fold::Folded,
            raw_calls: false,
            notice: None,
            notice_until: None,
            theme_problems: Vec::new(),
            pick: None,
            mode_confirm: None,
            mode_asked: None,
            mode_confirm_at: 0,
            mode_waiting: Vec::new(),
            mode_away: None,
            mode_up_for: false,
            todo_draft: None,
            todo_template: crate::prefs::TodoTemplate::Off,
            todo_seed: Vec::new(),
            todo_seed_pending: false,
            quit_card: false,
            quit_sel: 0,
            quit_armed: false,
            reseat: None,
            mode_sel: 0,
            pick_unseeded: false,
            pick_sel_at: None,
            mode_hits: Vec::new(),
            drawn_card: None,
            card_span: None,
            box_top_hits: None,
            standing: Vec::new(),
            standing_sel: 0,
            skills: Vec::new(),
            skills_sel: 0,
            skills_stop_rows: Vec::new(),
            standing_stop_rows: Vec::new(),
            standing_corpus: None,
            standing_dir: None,
            queues: Default::default(),
            queues_all: Default::default(),
            queue_scope: QueueScope::default(),
            queues_got_ms: 0,
            queues_asked_ms: 0,
            gate_tails: Default::default(),
            queue_sel: 0,
            queue_stop_rows: Vec::new(),
            queue_view: QueueView::default(),
            queue_why: None,
            queue_open: None,
            queue_folds: std::collections::BTreeMap::new(),
            queue_held: None,
            queue_plugin_stops: 0,
            jobs_sel: 0,
            jobs_finished_open: false,
            jobs_stop_rows: Vec::new(),
            subagents_sel: 0,
            subagents_finished_open: false,
            subagents_stop_rows: Vec::new(),
            sub_out: None,
            sub_out_pending: None,
            sub_out_from: None,
            up_from: None,
            diff_popup: None,
            job_out: None,
            todos: Vec::new(),
            seat_boards: Vec::new(),
            repo_todos: None,
            repo_todos_at: None,
            todos_sel: 0,
            todos_stop_rows: Vec::new(),
            todos_wait_links: Vec::new(),
            todos_open: None,
            repo_sel: 0,
            repo_open: false,
            slash_out: None,
            surfaces: Vec::new(),
            hits: Vec::new(),
            link_targets: Vec::new(),
            body_rows: 0,
            hint_rows: Vec::new(),
            injected: Vec::new(),
            injected_for: String::new(),
            pointer_at: None,
            hovered: None,
            drag: Default::default(),
            selection: None,
            pointer_hand: None,
            selection_held: false,
            pane_scroll: 0,
            pane_len: 0,
            pane_room: 0,
            rooms_left_w: 0,
            payload_page: 0,
            payload_max: std::cell::Cell::new(usize::MAX),
            payload_sel: None,
            spent_micros: 0,
            spent_seen: false,
            quit: false,
            repaint: false,
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
            git_job: None,
            git_reader: std::sync::Arc::new(crate::gitfield::read_state),
            fleet: Vec::new(),
            fleet_mine: Vec::new(),
            fleet_reader: None,
            fleet_unlisted: 0,
            fleet_job: None,
            fleet_read: 0,
            git_repos: Vec::new(),
            git_dir: None,
            gh_refs: None,
            link_layers: None,
            link_layers_at: 0,
            card_layers: None,
            card_layers_at: 0,
            gh_job: None,
            gh_read: (String::new(), 0),
            git_pinned: None,
            repo_rows: Vec::new(),
            live_join: None,
            marker_facts: MarkerFacts::default(),
            body_len: 0,
            attaching: false,
            fetching: None,
            restoring: None,
            link: Link::Attached,
            daemon_protocol: None,
            daemon_seat: None,
            skew_override_for: None,
            attach_started_ms: 0,
            cursor: None,
            bulk: None,
            filling: None,
            filling_since: 0,
            booting: None,
            store_newer: None,
            compacting: None,
            compacting_since: 0,
            compacting_why: None,
            status_click: None,
            held: None,
            activity: letibot_sessionlog::activity::Activity::Quiet,
            auto_compact_off: None,
            nags_paused: None,
            asks_setting: None,
            asks_board: None,
            away: Vec::new(),
            asks_hidden: None,
            answering: Vec::new(),
            absorbed: std::collections::HashMap::new(),
            todo_answered: Vec::new(),
            model_pending: None,
            retry_series: 0,
            // The done group starts folded, matching `HeadPrefs::default` - this is the
            // builder's value, and `apply_prefs` puts the file's answer over it at startup. The
            // operator's ruling: *"i want done collapsed by default"*.
            todos_fold_done: true,
            todos_follow: false,
            layer_failure: None,
            bye: None,
            daemon_pid: None,
            unconfirmed: Vec::new(),
            anchor: None,
            fb_last: None,
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
            rooms: rooms::RoomsView::default(),
            nav: chat_nav::Nav::default(),
            media: chat_media::Media::default(),
            chat_pane: chat_plugin::ChatPane::default(),
        }
    }

    /// Tell the head what time it is. The driver calls this once a tick; nothing
    /// else in `App` reads a clock, so a test drives time by hand.
    pub fn clock(&mut self, now_ms: u64) {
        // A notice posted before the head had a clock starts timing at its first tick: `head.toml`'s
        // problems are said while the prefs load, when `now_ms` is still 0, so their deadline was
        // `NOTICE_MS` after the epoch and the sentence was never seen.
        if self.now_ms == 0
            && now_ms > 0
            && let Some(until) = self.notice_until
            && until <= NOTICE_MS
        {
            self.notice_until = Some(now_ms.saturating_add(NOTICE_MS));
        }
        self.now_ms = now_ms;
        // The composer's edge draws the queues whether or not the pane was ever opened, and a
        // snapshot only ages: see `refresh_queues_on_the_clock`.
        self.refresh_queues_on_the_clock();
        // An open window over a running job follows what it writes. See `poll_job_out`.
        self.poll_job_out();
        // The chat view polls for news only while it is on the stack.
        self.rooms_poll();
    }

    /// Whether the head wants the terminal repainted from scratch, which only Ctrl-L asks for.
    /// Reading it clears it.
    pub fn take_repaint(&mut self) -> bool {
        // A held view does not let the glass be thrown away: a `true` here makes the driver call
        // `Terminal::invalidate`, a full repaint, which is the one thing the hold exists to
        // prevent. The flag is LEFT SET so the release spends it exactly once.
        if self.hold {
            return false;
        }
        std::mem::take(&mut self.repaint)
    }

    pub fn should_quit(&self) -> bool {
        // A head that asked the daemon to stop does not leave until it knows, so a stop this head
        // asked for outranks the `Bye` the stop itself publishes before the worker has ended;
        // `watch_stop`'s `gone` is the fact honoured. A `Bye` still ends an unasked exit.
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

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Actions a *frame* produced, for the driver to send. Empty almost always.
    pub fn take_actions(&mut self) -> Vec<Action> {
        let mut out = std::mem::take(&mut self.queued);
        // However many asks piled up, one goes out, and none while one is unanswered.
        if out.contains(&Action::ListJobs) {
            out.retain(|a| *a != Action::ListJobs);
            if self.jobs_in_flight {
                self.jobs_again = true;
            } else {
                self.jobs_in_flight = true;
                out.push(Action::ListJobs);
            }
        }
        out
    }

    /// Screen requests to answer with the frame just drawn. Drains.
    pub fn take_screen_requests(&mut self) -> Vec<String> {
        std::mem::take(&mut self.screen_requests)
    }

    /// Columns of empty space down each side of the frame: two, so the header, the answers and the
    /// composer start in one column. It agrees with [`card::REASONING_RAIL_WIDTH`], so reasoning
    /// text lands one step further in and the page reads as a single indent.
    pub const GUTTER: usize = 2;
}

// The marker: the two counts and nothing else, since its shape is narration -> work -> report and
// the counts are the only fact its neighbours do not give. Punctuation inside a sentence rather
// than an entry: glued to the [`RowClass::Speech`] line above it, and alone when there is none.

// The person's own message: an accent bar, a raised block, and the time it was sent. The bar
// survives no colour and a copy-paste; the block sets background and foreground together, since
// either alone is invisible or unreadable; the timestamp is omitted when the row carries no `ts`.

// What kind of row this is, for the one question the layout asks about its neighbours: does a
// blank line belong between them. Activity rows pack, because a run of tool cards reads as one
// block and a glyph in the first column already separates them; air goes where the kind changes.

/// Visible width of a rendered screen line. Re-exported so a test can assert the
/// screen fits.
pub fn line_width(s: &str) -> usize {
    visible_width(s)
}

mod action;
mod asks;
mod attachments;
mod attention;
pub(crate) mod cards_plugin;
pub(crate) mod chat_adapter;
pub(crate) mod chat_input;
pub(crate) mod chat_media;
pub(crate) mod chat_nav;
pub(crate) mod chat_plugin;
mod commands;
mod composer;
pub(crate) mod diff_popup;
mod editor;
mod events;
mod feedback;
mod ghlinks;
mod hits;
pub(crate) mod keyhints;
mod keys;
pub(crate) mod links;
mod notes;
pub(crate) mod one_event;
mod panes;
mod pick;
mod pictures;
mod plugin;
mod pointer;
mod prefs;
mod queue_pane;
pub(crate) mod queue_rows;
mod reach;
mod removal;
pub(crate) mod reseat;
pub(crate) mod rooms;
pub(crate) mod rooms_push;
pub(crate) mod runclick;
mod scroll;
mod session;
mod sessions_pane;
pub(crate) mod sidebar;
pub(crate) mod standing;
mod surface;
mod term_pane;
mod todos;
mod turn;
mod visibility;
mod waiting;
pub use action::*;
pub(crate) use asks::*;
pub(crate) use attention::*;
pub(crate) use cards_plugin::*;
pub(crate) use commands::*;
pub use editor::*;
pub(crate) use events::*;
pub(crate) use hits::*;
pub use keys::*;
// The two `[keys]` values the bar and the help screen read, named here because `keys` is a private
// module of `app` and `ui` is its sibling: a help screen that named the key a rebinding replaced
// would be the advertisement W8 forbids.
pub(crate) use keys::bindings::{KeyOverrides, surface_help_keys};
pub(crate) use links::{link_path_lines, osc8_spans};
pub(crate) use notes::*;
pub(crate) use panes::*;
pub(crate) use pick::*;
pub(crate) use pictures::*;
pub(crate) use plugin::*;
pub(crate) use prefs::*;
pub(crate) use scroll::*;
pub use session::*;
pub(crate) use surface::*;
pub use term_pane::*;
pub(crate) use todos::*;
pub(crate) use turn::*;
pub use visibility::*;

#[cfg(test)]
mod tests;
