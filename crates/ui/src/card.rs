//! Blocks with a header, a state and a bounded body: tool calls and reasoning.
//!
//! # What is wrong with the line we have now
//!
//! `letibot-tui` renders a settled tool call as one line:
//!
//! ```text
//! ● edit(call_7) — ok · 214 B
//! ```
//!
//! and a running one as `◐ edit(call_7) — running`, with a comment saying "No
//! partial output. There is nowhere to put it, by design." Both are true and
//! both are the wrong shape for a person:
//!
//! - **The argument is missing.** `edit` is not the interesting word; the path
//!   is. Every tool has exactly one argument that identifies *what it did to
//!   what*, and it is the only part a reader scans for.
//! - **The result is missing.** `214 B` is a fact about a number of bytes.
//! - **`running` does not say for how long.** The single most common question
//!   during a turn is "is this stuck", and an elapsed time answers it while a
//!   spinner does not.
//! - **`ToolProgress { note }` has nowhere to go.** The daemon emits it; the
//!   head drops it. "There is nowhere to put it" was true of a one-line
//!   renderer and is the reason to stop having one.
//!
//! # Three fold states, not two
//!
//! From grok-build. Collapsed / Truncated / Expanded, rather than the usual
//! collapsed-or-not. The middle state is the one that earns its place: a
//! finished `bash` call should show its last three lines without being asked,
//! because those are the lines that say whether it worked — and neither
//! "header only" nor "all 400 lines" is that.
//!
//! # Provenance
//!
//! **Adapted from grok-build** (xAI, Apache-2.0),
//! `crates/codegen/xai-grok-pager/src/scrollback/`:
//!
//! - `types.rs:52` — `DisplayMode { Collapsed, Truncated, Expanded }`.
//! - `blocks/tool/mod.rs:107` — `verb(running: bool)` flipping tense:
//!   `Read`/`Reading`, `Searched`/`Searching`, `Ran`/`Running`. [`Verb`] is that
//!   idea; the verb set is letibot's tool set.
//! - `blocks/tool/read.rs:16` (`FIRST_LINES`/`LAST_LINES` = 5/3),
//!   `use_tool.rs:15` and `web_fetch.rs:13` (10/3),
//!   `appearance/config.rs:613` (2/3 for shell, user-overridable). Those exact
//!   numbers are the defaults in [`Budget`], because they are somebody else's
//!   shipped calibration and inventing our own would be guessing.
//! - `blocks/tool/execute.rs:549` — the elided marker is `… +{n} lines`, and it
//!   is a *separator row*, styled apart from the content either side.
//! - `blocks/thinking.rs` — `"Thinking…"` while running becoming
//!   `"Thought for 4.2s"`; the `┃ ` rail down the body; the body de-emphasised
//!   **by attribute (dim + italic) rather than by colour**, because a
//!   terminal-native palette makes a colour change a no-op; and
//!   `streaming_replay()` setting `started_at: None` so a replayed session does
//!   not render a fabricated `"Thought for 0.0s"`. [`Phase::Replayed`] is that
//!   last one.
//!
//! Changed:
//!
//! - grok-build's blocks each carry a `BlockContext`, a `Theme`, a `Selectable`
//!   and an `AppearanceConfig`; these are plain data and one function. Their
//!   renderers are 750–3,000 lines apiece and are not liftable — the policies
//!   are what port, and the policies are what is above.
//! - Their `execute.rs` gives the head and tail chunks **different selection
//!   range ids** so a drag-copy across the ellipsis cannot silently splice
//!   non-adjacent text. letibot has no mouse selection, so there is nothing to
//!   port — but the hazard is real the moment one is added, and it is recorded
//!   here rather than rediscovered.
//! - They show `⇣12k` token counts on the turn-status row, not on the card.
//!   [`Card::bytes`] carries letibot's `inline`/`full` split instead, which is a
//!   §8.3 disclosure obligation and has no grok-build counterpart.

use crate::style::{Painter, Palette, Role};
use crate::width;

/// How much of a block is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DisplayMode {
    /// Header only.
    Collapsed,
    /// Header, plus a head and a tail of the body with the middle elided.
    #[default]
    Truncated,
    /// Everything.
    Expanded,
}

impl DisplayMode {
    /// The cycle a fold key walks. Running blocks skip `Expanded`: a body that
    /// is still growing pushes the prompt off the screen a line at a time,
    /// which is the thing that reads as flicker.
    pub fn next(self, running: bool) -> DisplayMode {
        match (self, running) {
            (DisplayMode::Collapsed, _) => DisplayMode::Truncated,
            (DisplayMode::Truncated, false) => DisplayMode::Expanded,
            (DisplayMode::Truncated, true) => DisplayMode::Collapsed,
            (DisplayMode::Expanded, _) => DisplayMode::Collapsed,
        }
    }
}

/// How the card is titled, and in which tense.
///
/// The tense is not decoration. `Reading src/main.rs` and `Read src/main.rs` are
/// the difference between "wait" and "done", read at a glance from the first
/// word, without a colour or a glyph — which matters because the glyph is the
/// part a screen reader or a `--replay` transcript loses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verb {
    Read,
    Edit,
    Write,
    Search,
    List,
    Run,
    Fetch,
    /// Anything the head has no opinion about; the tool's own name is used.
    Other(String),
}

impl Verb {
    /// Map a tool name to a verb. Unknown names keep their own name, which is
    /// right: inventing a verb for a tool we do not know is a guess presented as
    /// a fact.
    pub fn of(tool: &str) -> Verb {
        match tool {
            "read" | "read_file" | "cat" | "view" => Verb::Read,
            "edit" | "patch" | "apply_patch" | "str_replace" => Verb::Edit,
            "write" | "write_file" | "create" => Verb::Write,
            "grep" | "search" | "rg" | "find" => Verb::Search,
            "ls" | "list" | "list_dir" | "glob" => Verb::List,
            "bash" | "shell" | "run" | "exec" => Verb::Run,
            "fetch" | "web_fetch" | "http" => Verb::Fetch,
            other => Verb::Other(other.to_string()),
        }
    }

    pub fn label(&self, running: bool) -> &str {
        match (self, running) {
            (Verb::Read, false) => "Read",
            (Verb::Read, true) => "Reading",
            (Verb::Edit, false) => "Edited",
            (Verb::Edit, true) => "Editing",
            (Verb::Write, false) => "Wrote",
            (Verb::Write, true) => "Writing",
            (Verb::Search, false) => "Searched",
            (Verb::Search, true) => "Searching",
            (Verb::List, false) => "Listed",
            (Verb::List, true) => "Listing",
            (Verb::Run, false) => "Ran",
            (Verb::Run, true) => "Running",
            (Verb::Fetch, false) => "Fetched",
            (Verb::Fetch, true) => "Fetching",
            (Verb::Other(s), _) => s,
        }
    }
}

/// How a tool call ended.
///
/// Mirrors `letibot_transcript::ToolOutcome`'s distinctions rather than
/// collapsing them, because §8.2's rule — *abstention is not a flavour of
/// success and must not read like one* — is a rule about this display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    /// The tool declined to act and said why. Not a failure and not a success.
    Abstained(String),
    Failed(String),
    /// A person or a policy refused the call.
    Denied(String),
    /// The turn was interrupted while this call was in flight.
    Interrupted,
    /// **The call ran out of its own time and was killed.** Its own variant because the word is
    /// its own: `failed · timed out` is a sentence about a `failed`, and a reader cannot tell it
    /// from a command that ran and returned an error — the two want different next moves.
    Timeout,
    /// **The call never ran, and the daemon said why.** Not a `Failed`: nothing was attempted, and
    /// a retry is not obviously the answer until the `why` has been read. Carries the `why`, which
    /// is prose and belongs where prose wraps.
    NotRun(String),
    /// **Still running, in the background, and reachable.** Carries the handle.
    ///
    /// Its own variant for the same reason `Abstained` is one: a backgrounded
    /// call rendered as `Failed` reads as something to retry, and rendered as
    /// `Ok` reads as something that finished with nothing to say. Both are wrong
    /// about a process that is still working, and the operator acts on what the
    /// card says.
    Backgrounded(String),
}

impl Outcome {
    /// **The register this outcome is drawn in** — the one mapping, and the reason it is public.
    ///
    /// A head's settled row needs the same answer this card's own header gets, and the defect
    /// that made this `pub` is a head asking the question a second time with a coarser test:
    /// `let bad = !matches!(outcome, Ok)` puts `backgrounded`, `denied` and `abstained` all in
    /// `Failure`. The operator, looking at a command the harness had just backgrounded: *"why on
    /// earth backgrounding message is in red"*. Two spellings of one mapping — and `bceef58` in
    /// the other head is the same fix, with the same comment warning that *"it used to be spelled
    /// again here, and the two spellings disagreed about `not_run` and about backgrounded"*.
    ///
    /// A head may still overrule it for its own surface (leticl and letibot both draw `ok` faint
    /// rather than green, which is a decision about one row rather than about this mapping) — but
    /// it starts from here rather than from a test of its own.
    pub fn role(&self) -> Role {
        match self {
            Outcome::Ok => Role::Success,
            Outcome::Abstained(_) => Role::Attention,
            Outcome::Denied(_) => Role::Attention,
            Outcome::Failed(_) => Role::Failure,
            Outcome::Interrupted => Role::Failure,
            // **FAINT, not Attention, and the call is OVER the moment it is backgrounded.**
            //
            // It was `Attention` — this head's yellow — which meant the card of a call that had
            // already returned stayed lit for the whole life of the job behind it, while the
            // composer's edge and the jobs pane carried the same job's liveness. Measured on the
            // operator's screen 2026-10-05: `[3 tool calls, 24 thinking lines]` drew yellow with
            // its calls long done, beside `2 jobs running · 1 to a file` and a session row saying
            // `Job j12 exited 0` — three facts, one frame, and only the card was stale.
            //
            // The first fix was to make the card read the job's CURRENT state, which would have
            // lit and unlit it as the job ran and settled — and the operator ruled against that
            // for a reason worth keeping: *"wait, color change can mean some rerenders, so lets
            // make it white as soon as job starts"*. A colour that changes on a settlement is a
            // redraw the operator pays for a fact they already have twice. So the call draws
            // settled from the start, and **the job's liveness is the jobs pane's and the edge's**
            // — one fact, one place, and no row that has to be revisited.
            Outcome::Backgrounded(_) => Role::Faint,
            // Both are `Failure`, which is what they were drawn as before they had their own
            // variants: a call that timed out or never ran is not work that is happening.
            Outcome::Timeout | Outcome::NotRun(_) => Role::Failure,
        }
    }

    /// **The word this outcome prints — the ONE list, and it is `pub` for that reason.**
    ///
    /// A head's settled transcript row draws the same fact this card's header draws, and it was
    /// doing it with a second function of its own. The two disagreed, and the disagreement is the
    /// one leticl's `bceef58` records from the other side: *"it used to be spelled again here, and
    /// the two spellings disagreed about `not_run` and about backgrounded."* MEASURED here before
    /// this change, on the same call, live and settled:
    ///
    /// ```text
    ///   live       refused            failed · timed out        failed · not run — {why}
    ///   settled    REFUSED            timeout                   not run
    /// ```
    ///
    /// — the word changed as the row landed, twice into a different word and once only in case.
    ///
    /// **leticl's reading is the one kept**, and its docstring says why: *"the word the row prints
    /// for an outcome name — `outcome_word`"* — it took this head's transcript spelling as the
    /// reference, shouted `REFUSED` and all. So the card is the side that changes, and from here
    /// both renderers ask this one function.
    ///
    /// Shouted where §8.2 requires it: abstention is not a flavour of success, and a refusal is not
    /// a flavour of failure — the two ends of that rule.
    pub fn word(&self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::Abstained(_) => "ABSTAINED",
            Outcome::Failed(_) => "failed",
            Outcome::Denied(_) => "REFUSED",
            Outcome::Interrupted => "interrupted",
            Outcome::Timeout => "timeout",
            Outcome::NotRun(_) => "not run",
            // **`backgrounded`, not `STILL RUNNING`.** The call is over; what continues is a JOB
            // with a handle, and the handle is in the reason beside this word
            // (*"as `j12` after 0.0s — read it"*) so the row stays a way in. A word in the
            // present continuous drew a finished call as unfinished, which is the defect the
            // role above records — and lowercase, because it is a fact about how the call ended
            // rather than a decision of the operator's (unlike `ABSTAINED` and `REFUSED`).
            Outcome::Backgrounded(_) => "backgrounded",
        }
    }

    /// **The why, or nothing** — the other half of [`Outcome::word`], one list for the same
    /// reason. The caller supplies the sentences: this returns what the outcome was handed.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Outcome::Ok | Outcome::Interrupted | Outcome::Timeout => None,
            Outcome::Abstained(r)
            | Outcome::Failed(r)
            | Outcome::Denied(r)
            | Outcome::NotRun(r)
            | Outcome::Backgrounded(r) => Some(r),
        }
    }
}

/// **The fewest columns a running call's subject may keep**, while the tail keeps its own.
///
/// leticl's `(max 8 (- cols fixed …))` — *"a subject squeezed below a few columns says nothing"* —
/// and a floor rather than a fare share on purpose: the row is allowed to run long and be trimmed
/// by the frame, because the trim takes the tail's END (a note, a reason) and the clock sits at
/// the tail's HEAD. If the subject ate into the tail instead, the number that says the call is
/// alive would be what disappeared.
const MIN_SUBJECT: usize = 8;

/// Where a block is in its life.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// The model asked for it; nothing has run.
    ///
    /// `note` is what is happening WHILE nothing runs — today, the guard being
    /// asked whether the call follows from what the operator wanted. That wait is
    /// seconds long and used to render as the bare word "proposed", so a turn sat
    /// still with no reason given: *"the tool call latency grew, i almost thought
    /// something stalled and looked at htop"*.
    Proposed { note: Option<String> },
    /// In flight. `elapsed_ms` is supplied by the caller, never read from a
    /// clock here.
    Running {
        elapsed_ms: u64,
        /// The most recent `ToolProgress { note }`, which today has nowhere to
        /// go.
        note: Option<String>,
    },
    Finished {
        outcome: Outcome,
        elapsed_ms: Option<u64>,
    },
    /// Reconstructed from the session log rather than watched live.
    ///
    /// The distinction exists because a replay has no honest elapsed time, and
    /// printing `0.0s` is worse than printing nothing — it is a measurement that
    /// was never taken, rendered as one that was.
    Replayed { outcome: Outcome },
}

impl Phase {
    pub fn is_running(&self) -> bool {
        matches!(self, Phase::Running { .. } | Phase::Proposed { .. })
    }
}

/// Head and tail line counts per fold state.
///
/// The defaults are grok-build's shipped numbers (see the module header). They
/// are values on a struct rather than constants in the middle of a function for
/// the same reason `letibot_tui::render::Budget` is: a bound nobody can change
/// is a bound nobody can measure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    pub first_lines: usize,
    pub last_lines: usize,
    /// Cap even in [`DisplayMode::Expanded`]. A 40,000-line tool result
    /// expanded into a terminal is not "expanded", it is the conversation gone.
    pub expanded_max: usize,
}

impl Budget {
    /// Reading a file: enough head to see what it is, enough tail to see it
    /// ended. grok-build `read.rs:16`.
    pub const READ: Budget = Budget {
        first_lines: 5,
        last_lines: 3,
        expanded_max: 400,
    };
    /// A shell command: the tail is what matters, the head almost never is.
    /// grok-build `appearance/config.rs:613`.
    pub const SHELL: Budget = Budget {
        first_lines: 2,
        last_lines: 3,
        expanded_max: 400,
    };
    /// Anything else. grok-build `use_tool.rs:15`.
    pub const GENERIC: Budget = Budget {
        first_lines: 10,
        last_lines: 3,
        expanded_max: 400,
    };

    /// The budget a verb deserves.
    pub fn for_verb(v: &Verb) -> Budget {
        match v {
            Verb::Read | Verb::List => Budget::READ,
            Verb::Run => Budget::SHELL,
            _ => Budget::GENERIC,
        }
    }
}

impl Default for Budget {
    fn default() -> Self {
        Budget::GENERIC
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CardConfig {
    pub width: usize,
    pub palette: Palette,
    pub mode: DisplayMode,
    pub budget: Budget,
    /// Show the call id. Off by default: it is a correlation key for a log, not
    /// something a person reads, and it costs a dozen columns of a header that
    /// has a path to show.
    pub show_id: bool,
}

impl Default for CardConfig {
    fn default() -> Self {
        CardConfig {
            width: 100,
            palette: Palette::Colour,
            mode: DisplayMode::Truncated,
            budget: Budget::GENERIC,
            show_id: false,
        }
    }
}

/// One tool call, ready to draw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    pub verb: Verb,
    /// The tool's own name, for the cases where the verb hides it.
    pub tool: String,
    pub call_id: String,
    /// The one argument that says what was acted on: a path, a pattern, a
    /// command line. Empty when the tool has none.
    pub target: String,
    /// The body: already-rendered lines. A diff, a file excerpt, stdout. This
    /// type does not know which, on purpose — [`crate::diff`] and
    /// [`crate::highlight`] produce lines and this lays them out.
    pub body: Vec<String>,
    pub phase: Phase,
    /// §8.3's disclosure: bytes shown inline, and bytes that exist. Rendered
    /// only when they differ, because `8 KB` beside a 480 KB output is a number
    /// that misleads.
    pub bytes: Option<(u64, u64)>,
    /// Content hash of the spilled full output, when there is one.
    pub spill: Option<String>,
}

impl Card {
    pub fn new(tool: &str, call_id: &str) -> Card {
        Card {
            verb: Verb::of(tool),
            tool: tool.to_string(),
            call_id: call_id.to_string(),
            target: String::new(),
            body: Vec::new(),
            phase: Phase::Proposed { note: None },
            bytes: None,
            spill: None,
        }
    }

    pub fn target(mut self, t: &str) -> Card {
        self.target = t.to_string();
        self
    }

    pub fn phase(mut self, p: Phase) -> Card {
        self.phase = p;
        self
    }

    pub fn body(mut self, lines: Vec<String>) -> Card {
        self.body = lines;
        self
    }

    /// The header line, which is what survives every fold state.
    pub fn header(&self, cfg: &CardConfig) -> String {
        let p = cfg.palette;
        let running = self.phase.is_running();
        let (mark, mark_role) = match &self.phase {
            Phase::Proposed { .. } => ('○', Role::Faint),
            Phase::Running { .. } => ('◐', Role::Pending),
            Phase::Finished { outcome, .. } | Phase::Replayed { outcome } => ('●', outcome.role()),
        };
        // **The head is built first and the SUBJECT is filled in last** — R53 §1.5, and it is
        // leticl's own rule: *"the tail is measured first and the subject is given what is left…
        // the tail does not shrink — it is the fact, and half of `· 12.4s` is not a duration."*
        //
        // The order used to be the other way round: the subject was written into the row at full
        // length and the tail appended, so when the two did not fit the WHOLE row was truncated —
        // and the clock, which is the one number that says this call is alive, went with it. The
        // operator's own report, in R53's words: a running call rendered as
        // `◐ Running "cd /tmp && (sleep 6; …) & …"` with no `· 12.4s` anywhere on it.
        let mut s = String::new();
        s.push_str(&p.paint(mark_role, &mark.to_string()));
        s.push(' ');
        s.push_str(&p.paint(Role::Strong, self.verb.label(running)));

        // The right-hand side: state, timing, disclosure.
        let mut tail: Vec<String> = Vec::new();
        match &self.phase {
            Phase::Proposed { note } => match note {
                // The reason beats the state: "proposed" says what it is, and the
                // note says why nothing is happening yet, which is the question the
                // operator actually has while looking at it.
                Some(n) => tail.push(crate::text::without_control_lines(n).into_owned()),
                None => tail.push("proposed".into()),
            },
            Phase::Running { elapsed_ms, note } => {
                tail.push(crate::progress::duration(*elapsed_ms));
                if let Some(n) = note {
                    tail.push(crate::text::without_control_lines(n).into_owned());
                }
            }
            Phase::Finished {
                outcome,
                elapsed_ms,
            } => {
                if let Some(ms) = elapsed_ms {
                    tail.push(crate::progress::duration(*ms));
                }
                if !matches!(outcome, Outcome::Ok) {
                    tail.push(outcome.word().to_string());
                    if let Some(r) = outcome.reason() {
                        tail.push(crate::text::without_control_lines(r).into_owned());
                    }
                }
            }
            Phase::Replayed { outcome } => {
                if !matches!(outcome, Outcome::Ok) {
                    tail.push(outcome.word().to_string());
                    if let Some(r) = outcome.reason() {
                        tail.push(crate::text::without_control_lines(r).into_owned());
                    }
                }
            }
        }
        if let Some((inline, full)) = self.bytes
            && inline != full
        {
            tail.push(format!("{inline} B of {full} B"));
        }
        if let Some(h) = &self.spill {
            tail.push(format!("spill {h}"));
        }
        if tail.is_empty() {
            return width::truncate(&s, cfg.width);
        }
        let role = match &self.phase {
            Phase::Finished { outcome, .. } | Phase::Replayed { outcome }
                if !matches!(outcome, Outcome::Ok) =>
            {
                outcome.role()
            }
            _ => Role::Faint,
        };
        let joined = p.paint(role, &format!(" · {}", tail.join(" · ")));
        // The call id, kept beside the subject because it is the same kind of fact — and measured
        // before the subject, for the same reason the tail is.
        let id_str = if cfg.show_id {
            p.paint(
                Role::Faint,
                &format!(" ({})", crate::text::without_control_lines(&self.call_id)),
            )
        } else {
            String::new()
        };

        // **What the subject may have: everything the tail and the id have not claimed.** The floor
        // is leticl's `(max 8 …)`: a subject squeezed below a few columns says nothing, and letting
        // the row run long instead means the frame's own trim takes the tail's END — the note —
        // while the clock at its head survives. What must never give way is the tail's beginning.
        if !self.target.is_empty() {
            // **§3.1: a card is text this head did not author.** Target, call id, note
            // and outcome reason all come from a tool call or from the daemon, and this
            // row is written to the terminal verbatim. The sanitiser lives in
            // `crate::text` because **this crate had none** — the falsification test in
            // `letibot-tui` found a tool-progress note reaching a card's tail raw, which
            // is exactly the hole a per-head helper leaves.
            let target = crate::text::without_control_lines(&self.target);
            let spare = cfg.width.saturating_sub(
                width::width(&s) + width::width(&joined) + width::width(&id_str) + 1,
            );
            // A subject that fits whole keeps its own length; one that does not is cut with an
            // ellipsis, which is what `width::truncate` does.
            let room = spare.max(MIN_SUBJECT);
            // **A path is cut from the left and anything else from the right** — the rule
            // `letibot-tui`'s `shorten_subject` already applies to the transcript row, kept the
            // same here so one call does not read two ways on two rows. A glob or a quoted
            // sentence is not a path however many separators it contains: measured there, cutting
            // `**/*.{md,json,toml,yaml,yml} 40` from the left loses the fact that it is a glob.
            let not_a_path = target.contains(['*', '?', '{', '[', '"']);
            let shown = if target.contains('/') && !not_a_path {
                width::ellipsise_left(&target, room)
            } else {
                width::truncate(&target, room)
            };
            s.push(' ');
            s.push_str(&p.paint(Role::Plain, &shown));
        }
        s.push_str(&id_str);
        s.push_str(&joined);
        // One last guard for the case the floor above creates — a tail longer than any subject
        // could leave room for — and it takes the END, so the clock survives it.
        width::truncate(&s, cfg.width)
    }

    /// The whole card.
    pub fn render(&self, cfg: &CardConfig) -> Vec<String> {
        let mut out = vec![self.header(cfg)];
        if cfg.mode == DisplayMode::Collapsed || self.body.is_empty() {
            if cfg.mode == DisplayMode::Collapsed && !self.body.is_empty() {
                out.push(
                    cfg.palette
                        .paint(Role::Faint, &format!("  … {} lines", self.body.len())),
                );
            }
            return out;
        }
        let body = match cfg.mode {
            DisplayMode::Expanded => head_tail(&self.body, cfg.budget.expanded_max, 0, cfg.palette),
            _ => head_tail(
                &self.body,
                cfg.budget.first_lines,
                cfg.budget.last_lines,
                cfg.palette,
            ),
        };
        for l in body {
            // **Not sanitised, and the regression is why.** `body` is composed by the
            // *caller* — `call_card` builds it out of painted lines and rendered diff
            // panels — so this is the head's own text by the time `render` sees it, and
            // guarding it here stripped the head's own colour and left `[2m` behind. The
            // foreign text that reaches a card is guarded where it enters: the tool's
            // reason, the target, the progress note, and a diff's two sides before the
            // differ sees them. See `letibot_ui::text`.
            out.push(width::truncate(&format!("  {l}"), cfg.width));
        }
        out
    }
}

/// Keep `first` lines, then `last` lines, and say how many went.
///
/// The marker is grok-build's `… +{n} lines` (`execute.rs:549`) and it is a
/// **separator row**, not a line of the content — the distinction matters
/// because a reader must never mistake the elision for output. It is never a
/// silent cut: the count is the disclosure, which is the same rule
/// `letibot_sessionlog` applies to `dropped`.
pub fn head_tail(lines: &[String], first: usize, last: usize, p: Palette) -> Vec<String> {
    if lines.len() <= first + last + 1 {
        return lines.to_vec();
    }
    let hidden = lines.len() - first - last;
    let mut out: Vec<String> = lines[..first].to_vec();
    out.push(p.paint(Role::Faint, &format!("… +{hidden} lines")));
    if last > 0 {
        out.extend_from_slice(&lines[lines.len() - last..]);
    }
    out
}

/// The model's reasoning, rendered so it can never be mistaken for its answer.
///
/// Three separate signals, because any one of them is lost somewhere: the word
/// (`Thinking…` / `Thought for 4.2s`), the rail (`┃`), and the dim-italic
/// attribute. grok-build's note is worth repeating — de-emphasis by **colour**
/// is a no-op under a terminal-native palette, so the attribute is what actually
/// carries it.
///
/// `elapsed_ms` is `None` for a replayed session. It renders as `Thought` with
/// no duration rather than `Thought for 0.0s`.
pub fn reasoning(
    body: &[String],
    running: bool,
    elapsed_ms: Option<u64>,
    cfg: &CardConfig,
) -> Vec<String> {
    let p = cfg.palette;
    let head = if running {
        p.paint(Role::Reasoning, "Thinking…")
    } else {
        match elapsed_ms {
            Some(ms) => format!(
                "{}{}",
                p.paint(Role::Strong, "Thought"),
                p.paint(
                    Role::Faint,
                    &format!(" for {}", crate::progress::duration(ms))
                )
            ),
            None => p.paint(Role::Strong, "Thought"),
        }
    };
    let mut out = vec![width::truncate(&head, cfg.width)];
    if cfg.mode == DisplayMode::Collapsed {
        if !body.is_empty() {
            out.push(p.paint(Role::Faint, &format!("  … {} lines", body.len())));
        }
        return out;
    }
    // The rail is two columns, so the body was wrapped two columns narrower.
    // Getting this wrong is how a "reasoning" block ends up one row taller than
    // the space reserved for it, which pushes everything below it by a line
    // every frame.
    let shown = match cfg.mode {
        DisplayMode::Expanded => head_tail(body, cfg.budget.expanded_max, 0, p),
        _ => head_tail(body, cfg.budget.first_lines, cfg.budget.last_lines, p),
    };
    // The body is painted **inside** the reasoning style: any escape the caller's
    // line already carries closes back to the block, not to the terminal default.
    // Without this a `code span` or a heading in the model's working-out takes the
    // rest of its row white with it.
    let inner = Painter::inside(p, Role::Reasoning);
    for l in shown {
        out.push(width::truncate(
            &format!(
                "{} {}",
                p.paint(Role::Faint, "┃"),
                p.paint(Role::Reasoning, &inner.rebase_resets(&l))
            ),
            cfg.width,
        ));
    }
    out
}

/// Columns a caller must subtract from the width before wrapping a reasoning
/// body, so that the wrap and the rail agree.
pub const REASONING_RAIL_WIDTH: usize = 2;

#[cfg(test)]
mod tests {
    use super::*;

    fn body(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("line {i}")).collect()
    }

    fn cfg() -> CardConfig {
        CardConfig {
            width: 80,
            palette: Palette::None,
            ..Default::default()
        }
    }

    #[test]
    fn the_header_names_what_was_acted_on_not_just_the_tool() {
        let c = Card::new("read", "call_7")
            .target("crates/tui/src/app.rs")
            .phase(Phase::Finished {
                outcome: Outcome::Ok,
                elapsed_ms: Some(120),
            });
        let h = c.header(&cfg());
        assert!(h.contains("crates/tui/src/app.rs"), "{h}");
        assert!(h.starts_with("● Read "), "{h}");
    }

    #[test]
    fn the_tense_says_whether_to_wait_without_a_colour() {
        let running = Card::new("bash", "c1")
            .target("cargo test")
            .phase(Phase::Running {
                elapsed_ms: 4_300,
                note: None,
            });
        let done = Card::new("bash", "c1")
            .target("cargo test")
            .phase(Phase::Finished {
                outcome: Outcome::Ok,
                elapsed_ms: Some(4_300),
            });
        assert!(running.header(&cfg()).contains("Running cargo test"));
        assert!(done.header(&cfg()).contains("Ran cargo test"));
        // And the elapsed time is there while it runs, which is the answer to
        // "is it stuck".
        assert!(running.header(&cfg()).contains("4.3s"));
    }

    #[test]
    fn a_tool_progress_note_has_somewhere_to_go() {
        let c = Card::new("bash", "c1")
            .target("cargo build")
            .phase(Phase::Running {
                elapsed_ms: 9_000,
                note: Some("Compiling letibot-ui".into()),
            });
        assert!(c.header(&cfg()).contains("Compiling letibot-ui"));
    }

    #[test]
    fn abstention_does_not_read_like_success() {
        let c = Card::new("read", "c1")
            .target("/etc/shadow")
            .phase(Phase::Finished {
                outcome: Outcome::Abstained("outside the workspace".into()),
                elapsed_ms: Some(1),
            });
        let h = c.header(&cfg());
        assert!(h.contains("ABSTAINED"), "{h}");
        assert!(h.contains("outside the workspace"), "{h}");
        assert!(!h.contains("ok"), "{h}");
    }

    #[test]
    fn a_long_result_shows_a_head_a_count_and_a_tail() {
        let c = Card::new("bash", "c1")
            .target("ls -R")
            .body(body(400))
            .phase(Phase::Finished {
                outcome: Outcome::Ok,
                elapsed_ms: Some(300),
            });
        let cfg = CardConfig {
            budget: Budget::SHELL,
            ..cfg()
        };
        let out = c.render(&cfg);
        // header + 2 head + marker + 3 tail
        assert_eq!(out.len(), 7, "{out:#?}");
        assert!(out[1].contains("line 0"));
        assert!(out[3].contains("+395 lines"), "{:?}", out[3]);
        assert!(out.last().unwrap().contains("line 399"));
    }

    #[test]
    fn expanding_is_still_bounded_and_says_so() {
        let c = Card::new("read", "c1").body(body(5_000));
        let cfg = CardConfig {
            mode: DisplayMode::Expanded,
            ..cfg()
        };
        let out = c.render(&cfg);
        assert!(out.len() <= 402, "{} lines", out.len());
        assert!(
            out.iter().any(|l| l.contains("+4600 lines")),
            "{:?}",
            &out[..3]
        );
    }

    #[test]
    fn collapsing_keeps_the_header_and_admits_what_it_hid() {
        let c = Card::new("read", "c1").target("a.rs").body(body(40));
        let cfg = CardConfig {
            mode: DisplayMode::Collapsed,
            ..cfg()
        };
        let out = c.render(&cfg);
        assert_eq!(out.len(), 2);
        assert!(out[1].contains("40 lines"));
    }

    #[test]
    fn the_fold_cycle_never_expands_a_running_block() {
        // A body that is still growing pushes the prompt down a line at a time.
        let mut m = DisplayMode::Truncated;
        for _ in 0..6 {
            m = m.next(true);
            assert_ne!(m, DisplayMode::Expanded);
        }
        // Settled, it does.
        assert_eq!(DisplayMode::Truncated.next(false), DisplayMode::Expanded);
    }

    #[test]
    fn a_replayed_turn_does_not_invent_a_duration() {
        let live = Card::new("read", "c1").phase(Phase::Finished {
            outcome: Outcome::Ok,
            elapsed_ms: Some(0),
        });
        let replayed = Card::new("read", "c1").phase(Phase::Replayed {
            outcome: Outcome::Ok,
        });
        assert!(live.header(&cfg()).contains("0ms"));
        assert!(
            !replayed.header(&cfg()).contains("0"),
            "{}",
            replayed.header(&cfg())
        );
    }

    #[test]
    fn the_byte_split_is_shown_only_when_it_discloses_something() {
        let mut c = Card::new("bash", "c1").phase(Phase::Finished {
            outcome: Outcome::Ok,
            elapsed_ms: Some(1),
        });
        c.bytes = Some((214, 214));
        assert!(!c.header(&cfg()).contains("of"), "{}", c.header(&cfg()));
        c.bytes = Some((8_192, 491_000));
        assert!(c.header(&cfg()).contains("8192 B of 491000 B"));
    }

    #[test]
    fn a_narrow_terminal_keeps_the_clock_and_the_paths_own_name() {
        let c = Card::new("read", "call_00000007")
            .target("crates/sessionlog/src/protocol.rs")
            .phase(Phase::Finished {
                outcome: Outcome::Ok,
                elapsed_ms: Some(1_200),
            });
        for w in [20usize, 40, 60, 100] {
            let cfg = CardConfig { width: w, ..cfg() };
            let h = c.header(&cfg);
            assert!(width::width(&h) <= w, "{w}: {h:?}");
        }
        let narrow = c.header(&CardConfig { width: 44, ..cfg() });
        // **The path keeps the half that identifies it, and the tail keeps its place.**
        //
        // This test used to assert the opposite of the second half — *the tail is dropped whole*,
        // `!narrow.contains("1.2s")` — and that was the rule until R53 §1.5. A tail dropped whole
        // takes the CLOCK with it, and the clock is the one number that says a running call is
        // alive: the operator's own screen showed `◐ Running "cd /tmp && (sleep 6; …) & …"` with
        // no `· 12.4s` on it anywhere. leticl's rule is the one kept now — *"the tail does not
        // shrink — it is the fact, and half of `· 12.4s` is not a duration. What gives way is the
        // SUBJECT."* And the subject gives way from the LEFT, because a path is recognised by
        // where it ends: `…/src/protocol.rs` names the file, `crates/sessionlog/src/protoco…`
        // names only the tree.
        assert!(narrow.contains("protocol.rs"), "{narrow}");
        assert!(
            narrow.contains("1.2s"),
            "the clock is not the thing to drop: {narrow}"
        );
        assert!(
            narrow.contains('…'),
            "and the cut is disclosed where it happened: {narrow}"
        );
    }

    #[test]
    fn reasoning_is_marked_three_ways_so_no_single_loss_hides_it() {
        let cfg = CardConfig {
            palette: Palette::None,
            ..cfg()
        };
        let out = reasoning(&body(20), false, Some(4_200), &cfg);
        assert!(out[0].contains("Thought for 4.2s"), "{:?}", out[0]);
        assert!(out[1].starts_with('┃'), "{:?}", out[1]);
        // And under a colour palette the body carries the dim attribute.
        let coloured = reasoning(
            &body(4),
            true,
            None,
            &CardConfig {
                ..Default::default()
            },
        );
        assert!(coloured[0].contains("Thinking…"));
        assert!(coloured[1].contains("\x1b[2;"), "{:?}", coloured[1]);
    }

    #[test]
    fn a_replayed_reasoning_block_does_not_say_zero_seconds() {
        let out = reasoning(&body(3), false, None, &cfg());
        assert_eq!(out[0], "Thought");
    }

    /// **R25: the head cuts the target to ITS OWN viewport, and 227 shows more than 80.**
    ///
    /// The daemon's cut is a wire-safety limit now, not a display decision — so this is the
    /// layer that decides what a reader sees, and it is the *only* layer that can, because
    /// **more than one head may be attached to one session at different widths at the same
    /// time.** The operator's measurement was a 227-column pane showing a headline cut at 121
    /// characters, ~100 columns unused on every tool row.
    ///
    /// The elision is disclosed here rather than silently: `width::truncate` appends `…`, and
    /// §3.3's rule is that the mark belongs to the layer that made the cut. Two cuts, two
    /// marks — the daemon's is only reached by a command past 2048 bytes.
    #[test]
    fn the_head_cuts_a_long_target_to_its_own_viewport_and_says_so() {
        // Longer than every viewport in the test, so both widths have something to cut — the
        // operator's own shape, where the command was longer than his 227 columns. (A target
        // that FITS is not marked at all, which the second half of this test pins.)
        let target = format!(
            "cd /opt/secure_auth && gcc -o test_auth test_auth.c {} \
             -Llib -lsecure_auth -Wl,-rpath,/opt/secure_auth/lib && ./test_auth --selftest",
            "-Iinclude ".repeat(12)
        );
        assert!(
            target.len() > 227,
            "the premise is a target longer than the viewport"
        );
        let c = Card::new("bash", "call_00000007")
            .target(&target)
            .phase(Phase::Finished {
                outcome: Outcome::Ok,
                elapsed_ms: Some(900),
            });

        let wide = c.header(&CardConfig {
            width: 227,
            ..cfg()
        });
        let narrow = c.header(&CardConfig { width: 80, ..cfg() });
        for (w, h) in [(227usize, &wide), (80, &narrow)] {
            assert!(width::width(h) <= w, "{w} columns overflowed: {h:?}");
        }
        // **227 shows 227, 80 shows 80, and neither number is in the daemon.**
        assert!(
            width::width(&wide) > width::width(&narrow),
            "227 showed no more than 80:\n{wide}\n{narrow}"
        );
        assert!(
            width::width(&wide) > 120,
            "a 227-column card used {} columns — the daemon's old cut is still the limit",
            width::width(&wide)
        );
        // **The cut is disclosed where it happened, which is now the SUBJECT's left edge.** It
        // used to be the row's right edge, because the whole row was truncated from the right —
        // and that is the shape that ate the tail. See
        // `a_narrow_terminal_keeps_the_clock_and_the_paths_own_name` for the rule and leticl's
        // words for it.
        for (w, h) in [(227usize, &wide), (80, &narrow)] {
            assert!(
                h.contains('…'),
                "the {w}-column cut is not disclosed: {h:?}"
            );
            assert!(
                h.contains("900ms"),
                "the {w}-column row lost its clock: {h:?}"
            );
        }

        // **And a target that fits is shown whole and marked not at all** — a card that put a
        // `…` on a complete command would be telling the reader something was cut when nothing
        // was, which is the same class of lie as hiding a cut.
        let short = Card::new("bash", "c1")
            .target("cargo test --workspace")
            .phase(Phase::Finished {
                outcome: Outcome::Ok,
                elapsed_ms: Some(900),
            });
        let fits = short.header(&CardConfig {
            width: 227,
            ..cfg()
        });
        assert!(fits.contains("cargo test --workspace"), "{fits}");
        assert!(
            !fits.contains('…'),
            "a complete target was marked as cut: {fits}"
        );
    }

    #[test]
    fn nothing_a_card_renders_ever_exceeds_the_width() {
        let c = Card::new("edit", "c1")
            .target("a/very/long/path/that/keeps/going/and/going/src/lib.rs")
            .body((0..50).map(|i| format!("{}{i}", "x".repeat(120))).collect())
            .phase(Phase::Finished {
                outcome: Outcome::Failed("permission denied on a long path".into()),
                elapsed_ms: Some(4),
            });
        for w in [16usize, 30, 60, 120] {
            let cfg = CardConfig {
                width: w,
                palette: Palette::Colour,
                ..Default::default()
            };
            for l in c.render(&cfg) {
                assert!(
                    width::width(&l) <= w,
                    "{w}: {} cols {l:?}",
                    width::width(&l)
                );
            }
        }
    }
}
