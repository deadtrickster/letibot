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

use crate::markdown::IncrementalMarkdown;
use crate::render::{BlockCache, RenderConfig, sgr, trim_to, visible_width, wrap};

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Backspace,
    CtrlC,
    Up,
    Down,
    PageUp,
    PageDown,
    Esc,
}

#[derive(Debug, Default)]
struct TurnPane {
    turn_id: String,
    model: String,
    text: IncrementalMarkdown,
    reasoning: IncrementalMarkdown,
    text_cache: BlockCache,
    reasoning_cache: BlockCache,
    calls: Vec<(String, String, CallState)>,
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
    settled: Vec<SettledDecision>,
    warnings: Vec<Warned>,
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
    pub input: String,
    notice: Option<String>,
    quit: bool,
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
            hist_width: 0,
            turn: None,
            open: Vec::new(),
            settled: Vec::new(),
            warnings: Vec::new(),
            heads: 0,
            seq: 0,
            dropped: 0,
            scrubbed: 0,
            resyncs: 0,
            rendered: 0,
            filtered: 0,
            scroll: 0,
            input: String::new(),
            notice: None,
            quit: false,
        }
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
                let d = self.event(env.event);
                match d {
                    Disposition::Rendered => self.rendered += 1,
                    Disposition::Filtered => self.filtered += 1,
                    Disposition::Control => {}
                }
                d
            }
            ServerFrame::Accepted { note, .. } => {
                self.notice = Some(note);
                Disposition::Control
            }
            ServerFrame::Rejected {
                reason,
                expected_seq,
                actual_seq,
                ..
            } => {
                // Both numbers, so the operator can see what they were looking at.
                self.notice = Some(format!(
                    "rejected: {reason} (you saw {expected_seq}, the session is at {actual_seq})"
                ));
                Disposition::Control
            }
            ServerFrame::Bye { reason } => {
                self.notice = Some(format!("daemon: {reason}"));
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
        self.hist_lines.clear();
        self.hist_upto = 0;
        self.open = s.open_decisions;
        self.settled = s.settled_decisions;
        self.warnings = s.warnings;
        self.heads = s.heads.len();
        self.turn = s.turn.map(|t| {
            let mut pane = TurnPane {
                turn_id: t.turn_id,
                model: t.model,
                calls: t
                    .calls
                    .into_iter()
                    .map(|c| (c.call_id, c.name, c.state))
                    .collect(),
                progress: t.progress,
                state: Some(t.state),
                ..TurnPane::default()
            };
            // The snapshot carries the accumulated text **once**. Everything after
            // this is an increment. That is §13.3's wire half, arriving.
            pane.text.push(&t.text);
            pane.reasoning.push(&t.reasoning);
            pane
        });
    }

    fn event(&mut self, e: SessionEvent) -> Disposition {
        match e {
            SessionEvent::TurnStarted {
                turn_id,
                model,
                ledger_head: _,
            } => {
                self.turn = Some(TurnPane {
                    turn_id,
                    model,
                    state: Some(TurnState::Running),
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
                        t.text.push(&text);
                        Disposition::Rendered
                    }
                    DeltaTarget::Reasoning => {
                        if self.verbosity >= Verbosity::Normal {
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
                    t.calls.push((call_id, name, CallState::Proposed));
                }
                Disposition::Rendered
            }
            SessionEvent::ToolStarted { call_id, name } => {
                if let Some(t) = self.turn.as_mut() {
                    match t.calls.iter_mut().find(|c| c.0 == call_id) {
                        Some(c) => c.2 = CallState::Running,
                        None => t.calls.push((call_id, name, CallState::Running)),
                    }
                }
                Disposition::Rendered
            }
            // Interactive, and deliberately not accumulated: partial tool output has
            // no durable form. The head shows "running" and nothing else, which is
            // the same rule the daemon's view applies.
            SessionEvent::ToolProgress { .. } => Disposition::Filtered,
            SessionEvent::ToolFinished {
                call_id,
                outcome,
                payload_digest,
                bytes,
            } => {
                if let Some(t) = self.turn.as_mut()
                    && let Some(c) = t.calls.iter_mut().find(|c| c.0 == call_id)
                {
                    c.2 = CallState::Finished {
                        outcome,
                        payload_digest,
                        bytes,
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
                self.settled.push(SettledDecision {
                    req_id,
                    summary,
                    outcome,
                    by,
                    basis,
                    late,
                });
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
            SessionEvent::Warning { code, detail } => {
                self.warnings.push(Warned {
                    code,
                    detail,
                    ts: 0,
                });
                if self.warnings.len() > 64 {
                    self.warnings.remove(0);
                }
                Disposition::Rendered
            }
            SessionEvent::CommandIssued {
                identity,
                command,
                note,
                ..
            } => {
                // Two humans in one session: seeing who did what is the point.
                self.notice = Some(format!("{identity} · {command}: {note}"));
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
    pub fn key(&mut self, k: Key) -> Option<Action> {
        match k {
            Key::CtrlC => {
                if self.turn_running() {
                    // Interrupt is not quit. A shared session's interrupt is
                    // announced with the issuer, so it must be a deliberate act.
                    return Some(Action::Interrupt("operator pressed ctrl-c".into()));
                }
                self.quit = true;
                Some(Action::Quit)
            }
            Key::Enter => {
                let text = std::mem::take(&mut self.input);
                if text.trim().is_empty() {
                    return None;
                }
                if let Some(rest) = text.strip_prefix('/') {
                    return self.command(rest.trim());
                }
                // An open decision takes the line as an option id or its first
                // letter, so answering does not require a second keymap.
                if let Some(d) = self.open.first().cloned()
                    && let Some(opt) = match_option(&d, text.trim())
                {
                    return Some(Action::Answer {
                        req_id: d.req_id,
                        option_id: opt,
                    });
                }
                Some(Action::Prompt(text))
            }
            Key::Backspace => {
                self.input.pop();
                None
            }
            Key::Char(c) => {
                self.input.push(c);
                None
            }
            Key::Up => {
                self.scroll += 1;
                None
            }
            Key::Down => {
                self.scroll = self.scroll.saturating_sub(1);
                None
            }
            Key::PageUp => {
                self.scroll += 10;
                None
            }
            Key::PageDown => {
                self.scroll = self.scroll.saturating_sub(10);
                None
            }
            Key::Esc => {
                self.input.clear();
                self.scroll = 0;
                None
            }
        }
    }

    fn command(&mut self, cmd: &str) -> Option<Action> {
        match cmd {
            "quit" | "q" => {
                self.quit = true;
                Some(Action::Quit)
            }
            "resync" => Some(Action::Resync),
            "verbosity" | "v" => {
                self.verbosity = self.verbosity.next();
                self.notice = Some(format!(
                    "verbosity {} — {} events filtered so far",
                    self.verbosity.as_str(),
                    self.filtered
                ));
                None
            }
            "interrupt" | "i" => Some(Action::Interrupt("operator typed /interrupt".into())),
            other => {
                self.notice = Some(format!("unknown command /{other}"));
                None
            }
        }
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
            self.hist_lines.clear();
            self.hist_upto = 0;
        }
    }

    /// The whole screen, `h` lines of at most `w` columns.
    pub fn screen(&mut self, w: usize, h: usize) -> Vec<String> {
        self.cfg.width = w;
        let mut body = self.body_lines(w);

        let mut chrome = Vec::new();
        if let Some(d) = self.open.first() {
            chrome.extend(self.decision_lines(d, w));
        }
        chrome.push(self.status_line(w));
        chrome.push(self.input_line(w));

        let room = h.saturating_sub(chrome.len()).max(1);
        // Follow the tail unless the operator has scrolled.
        let end = body.len().saturating_sub(self.scroll);
        let start = end.saturating_sub(room);
        let mut out: Vec<String> = body.drain(start..end).collect();
        while out.len() < room {
            out.push(String::new());
        }
        out.extend(chrome);
        out.into_iter().map(|l| trim_to(&l, w)).collect()
    }

    fn body_lines(&mut self, w: usize) -> Vec<String> {
        if self.hist_width != w {
            self.hist_width = w;
            self.hist_lines.clear();
            self.hist_upto = 0;
        }
        let cfg = self.cfg.clone();
        let limit = cfg.budget.body_lines;
        for it in &self.items[self.hist_upto..] {
            self.hist_lines.extend(item_lines(it, &cfg));
            self.hist_lines.push(String::new());
        }
        self.hist_upto = self.items.len();

        let mut out = self.hist_lines.clone();
        // Does the transcript already own this turn's content?
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
        if let Some(t) = self.turn.as_mut() {
            if !superseded && !t.reasoning.is_empty() {
                out.push(dim(&cfg, "reasoning"));
                out.extend(
                    t.reasoning_cache
                        .lines(&t.reasoning, &cfg, cfg.budget.reasoning_lines),
                );
                out.push(String::new());
            }
            if !superseded {
                for (call_id, name, state) in &t.calls {
                    out.push(call_line(call_id, name, state, &cfg));
                }
                if !t.calls.is_empty() {
                    out.push(String::new());
                }
                if !t.text.is_empty() {
                    out.extend(t.text_cache.lines(&t.text, &cfg, limit));
                }
            }
            if let Some(TurnState::Finished {
                finish_reason,
                usage,
                timings,
            }) = &t.state
            {
                let keep = usage
                    .f_keep()
                    .map(|f| format!("{:.0}% cached", f * 100.0))
                    .unwrap_or_else(|| "no prompt".into());
                out.push(dim(
                    &cfg,
                    &format!(
                        "── {} · {} prompt / {} out · {keep} · {:.0} tok/s",
                        finish_reason.as_str(),
                        usage.prompt_tokens,
                        usage.predicted_tokens,
                        if timings.predicted_ms > 0.0 {
                            usage.predicted_tokens as f64 * 1000.0 / timings.predicted_ms
                        } else {
                            0.0
                        }
                    ),
                ));
            }
            if let Some(TurnState::Interrupted {
                reason,
                partial_kept,
            }) = &t.state
            {
                out.push(warn_line(
                    &cfg,
                    &format!(
                        "── interrupted: {reason} ({})",
                        if *partial_kept {
                            "partial output kept"
                        } else {
                            "nothing kept"
                        }
                    ),
                ));
            }
        }
        for w_ in self
            .warnings
            .iter()
            .rev()
            .take(3)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            out.push(warn_line(&cfg, &format!("! {} — {}", w_.code, w_.detail)));
        }
        let _ = w;
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

    fn status_line(&self, w: usize) -> String {
        let turn = match self.turn.as_ref() {
            Some(t) => match (&t.state, &t.progress) {
                (_, Some(p)) if p.total > 0 => {
                    format!("prefill {}/{} ({} cached)", p.processed, p.total, p.cache)
                }
                (Some(TurnState::Running), _) => format!("running {}", t.model),
                (Some(TurnState::Finished { .. }), _) => "idle".into(),
                (Some(TurnState::Interrupted { .. }), _) => "interrupted".into(),
                _ => "idle".into(),
            },
            None => "idle".into(),
        };
        // Every disclosure on one line: the read mark, what was suppressed here,
        // what the daemon will never send, and what it stripped on the way.
        let s = format!(
            "{} {} · seq {} · {} · rendered {} · filtered {} ({}) · dropped {} · scrubbed {} · resync {} · heads {}",
            self.session_id,
            self.head_id,
            self.seq,
            turn,
            self.rendered,
            self.filtered,
            self.verbosity.as_str(),
            self.dropped,
            self.scrubbed,
            self.resyncs,
            self.heads,
        );
        colour(&self.cfg, sgr::GREY, &trim_to(&s, w))
    }

    fn input_line(&self, w: usize) -> String {
        match &self.notice {
            Some(n) => colour(&self.cfg, sgr::MAGENTA, &trim_to(&format!("· {n}"), w)),
            None => {
                let prompt = if self.open.is_empty() {
                    "› "
                } else {
                    "answer › "
                };
                trim_to(&format!("{prompt}{}", self.input), w)
            }
        }
    }

    /// Drop the transient notice, once the operator has had a frame to see it.
    pub fn clear_notice(&mut self) {
        self.notice = None;
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

fn call_line(call_id: &str, name: &str, state: &CallState, cfg: &RenderConfig) -> String {
    let (mark, detail, code) = match state {
        CallState::Proposed => ("○", "proposed".to_string(), sgr::GREY),
        // No partial output. There is nowhere to put it, by design.
        CallState::Running => ("◐", "running".to_string(), sgr::YELLOW),
        CallState::Finished { outcome, bytes, .. } => (
            "●",
            format!("{} · {bytes} B", outcome_str(outcome)),
            match outcome {
                letibot_transcript::ToolOutcome::Ok => sgr::GREEN,
                letibot_transcript::ToolOutcome::Abstained { .. } => sgr::YELLOW,
                _ => sgr::RED,
            },
        ),
    };
    colour(cfg, code, &format!("{mark} {name}({call_id}) — {detail}"))
}

fn outcome_str(o: &letibot_transcript::ToolOutcome) -> String {
    use letibot_transcript::ToolOutcome as O;
    match o {
        O::Ok => "ok".into(),
        // §8.2: abstention is not a flavour of success and must not read like one.
        O::Abstained { reason } => format!("ABSTAINED — {reason}"),
        O::Failed { reason } => format!("failed — {reason}"),
        O::Denied { req_id } => format!("denied ({req_id})"),
        O::Timeout => "timeout".into(),
        O::NotRun { why } => format!("not run — {why}"),
    }
}

fn item_lines(it: &SnapshotItem, cfg: &RenderConfig) -> Vec<String> {
    let Some(item) = &it.item else {
        // Honest: the event arrived, the content did not.
        return vec![dim(
            cfg,
            &format!("[{} {} — content not loaded]", it.kind, it.item_id),
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
            let mut md = IncrementalMarkdown::new();
            md.push(text);
            let mut out = vec![dim(cfg, "reasoning")];
            let mut cache = BlockCache::new();
            out.extend(
                cache
                    .lines(&md, cfg, cfg.budget.reasoning_lines)
                    .into_iter()
                    .map(|l| dim(cfg, &l)),
            );
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
            let head = colour(
                cfg,
                sgr::GREY,
                &format!("← {name}({call_id}) {}", outcome_str(outcome)),
            );
            let mut out = vec![head];
            let lines: Vec<String> = payload.lines().map(str::to_string).collect();
            let limit = cfg.budget.reasoning_lines;
            if lines.len() > limit && limit >= 2 {
                out.push(colour(
                    cfg,
                    sgr::GREY,
                    &format!("  … {} lines elided …", lines.len() - (limit - 1)),
                ));
                out.extend(
                    lines[lines.len() - (limit - 1)..]
                        .iter()
                        .map(|l| dim(cfg, &format!("  {l}"))),
                );
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
    fn a_settled_decision_is_not_offered_for_answering() {
        let mut a = app();
        a.apply(ServerFrame::Event(env(1, testing::requested("r1", "rm"))));
        assert_eq!(a.open_decisions().len(), 1);
        a.apply(ServerFrame::Event(env(2, testing::answered("r1", "deny"))));
        assert!(a.open_decisions().is_empty());
        // And typing an option id no longer answers it: it becomes a prompt.
        a.input = "allow".into();
        assert!(matches!(a.key(Key::Enter), Some(Action::Prompt(_))));
    }

    #[test]
    fn an_open_decision_is_answered_by_typing_the_option() {
        let mut a = app();
        a.apply(ServerFrame::Event(env(1, testing::requested("r1", "rm"))));
        a.input = "deny".into();
        assert_eq!(
            a.key(Key::Enter),
            Some(Action::Answer {
                req_id: "r1".into(),
                option_id: "deny".into()
            })
        );
    }

    #[test]
    fn ctrl_c_interrupts_a_running_turn_and_quits_an_idle_one() {
        let mut a = app();
        assert_eq!(a.key(Key::CtrlC), Some(Action::Quit));
        let mut a = app();
        a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
        assert!(matches!(a.key(Key::CtrlC), Some(Action::Interrupt(_))));
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
        let line = a.input_line(200);
        assert!(line.contains("12") && line.contains("40"), "{line}");
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
        letibot_sessionlog::event::Envelope {
            session_id: "s".into(),
            seq,
            ts: 0,
            event,
        }
    }
}
