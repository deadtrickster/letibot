//! **A run's marker opens by its parts** — the operator: *"when I see this "[7 tool calls, 11
//! thinking lines]" and then click on tool calls section or thinking section i wanted them to
//! expand. Esc should fold them back"*.
//!
//! Opening a run is `ctrl-t`'s act already (`payload_sel` naming the run's first row, the rung
//! lifted for that run's rows — see `ui::transcript::markers::run_open_at`). A click does the same
//! for the run under the pointer, and lifts only the kind it was on: the calls clause opens the
//! tool calls, the thinking clause the reasoning, anything else the whole run. Esc closes it as it
//! closes any open run.
//!
//! # One switch, one writer
//!
//! The lift is **one field** ([`App::lift`]) and it applies to **the thing `payload_sel` names** —
//! a run of rows, or [`LIVE_RUN`] for the turn still running ([`App::lifted`]). `set_lift` and
//! `drop_lift` are the only writers, and the panel goes with the switch: a lift with no panel is a
//! state whose only exit is a row that may have scrolled away, which is the operator's *"it gets
//! stuck"*.
//!
//! **The thinking has two spellings of *open* and one verb.** Where the rung draws the thinking,
//! its openness is the fold ([`App::reasoning`], moved by `ctrl-r`, `/think` and `head.toml`);
//! where the rung hides it, the only spelling that draws it is the lift on a run. [`App::thinking_open`]
//! is the one reader and [`App::set_thinking`] the one writer, so a click, `ctrl-r`, `/think` and
//! `esc` flip the same state — and the state is the one that is DRAWN, so a stale fold at a hiding
//! rung cannot answer for a screen that does not show it.

use super::*;
use crate::app::visibility::Show;

/// **One clause of a marker on the last frame** — where it was drawn and what a click opens.
/// The run a live pane's marker stands for — the turn in flight, whose rows have not landed.
pub(crate) const LIVE_RUN: &str = "\u{0}live";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MarkerHit {
    /// The screen row of the window it is on.
    pub(crate) y: usize,
    /// Its columns, `from..to`.
    pub(crate) from: usize,
    pub(crate) to: usize,
    /// The run's first row, as `payload_sel` names an open run.
    pub(crate) run: String,
    /// The kind a click lifts — `None` is the whole run.
    pub(crate) show: Option<Show>,
}

/// The kind a clause names, by its noun at any rung of [`COUNT_RUNGS`].
fn clause_kind(clause: &str) -> Option<Option<Show>> {
    let c = clause.trim();
    let digits = c.chars().take_while(|ch| ch.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    let noun = &c[digits..];
    for rung in crate::ui::COUNT_RUNGS {
        let (calls1, calls, think1, think, ev1, ev) = rung;
        if noun == calls1 || noun == calls {
            return Some(Some(Show::Tools));
        }
        if noun == think1 || noun == think {
            return Some(Some(Show::Thinking));
        }
        if noun == ev1 || noun == ev {
            return Some(None);
        }
    }
    None
}

/// **The clauses of the marker on one line of text**, as `(from, to, kind)` in columns — the
/// last `[…]` on the line whose every clause is a count.
fn clauses(cells: &[char]) -> Vec<(usize, usize, Option<Show>)> {
    let Some(close) = cells.iter().rposition(|c| *c == ']') else {
        return Vec::new();
    };
    let Some(open) = cells[..close].iter().rposition(|c| *c == '[') else {
        return Vec::new();
    };
    let inner: String = cells[open + 1..close].iter().collect();
    let mut out = Vec::new();
    let mut at = open + 1;
    for part in inner.split(", ") {
        let Some(kind) = clause_kind(part) else {
            return Vec::new();
        };
        let n = part.chars().count();
        out.push((at, at + n, kind));
        at += n + 2;
    }
    out
}

/// **A row an open run leaves out**, because the click asked for the other kind: the reasoning
/// when it asked for the tool calls, and the tool rows (and a round's call-only assistant row)
/// when it asked for the thinking. The rung's own ladder cannot say *thinking without tools* —
/// thinking is a higher rung than tools — so an open run is drawn lifted and this takes the
/// other kind out.
pub(crate) fn run_skips(only: Option<Show>, it: &letibot_sessionlog::view::SnapshotItem) -> bool {
    use letibot_transcript::TranscriptItem as T;
    match (only, it.item.as_ref()) {
        (Some(Show::Tools | Show::Edits), Some(T::Reasoning { .. })) => true,
        (Some(Show::Thinking), Some(T::ToolResult { .. })) => true,
        (Some(Show::Thinking), Some(T::Assistant { text, .. })) => text.trim().is_empty(),
        _ => false,
    }
}

/// The thinking fold for a row of an open run: open when the click asked for the thinking.
pub(crate) fn run_think(only: Option<Show>, think: Fold) -> Fold {
    if only == Some(Show::Thinking) {
        Fold::Open
    } else {
        think
    }
}

impl App {
    /// **The kind a click lifted, for the turn still running** — the pane's half of the one
    /// switch.
    ///
    /// Two addresses draw the same lift, and neither is derived from a second field: the pane
    /// itself ([`LIVE_RUN`]), and **a run whose counted work is still running** — the marker on
    /// such a run counts calls that are not rows yet (`MarkerFacts::of`), so a click on its
    /// clause is a question about the work in flight as well, and the panel alone would show
    /// nothing of the kind that was asked for (the operator's *"clicked on the tools counter —
    /// empty panel"*). The expensive question is only asked while a lift is set.
    pub(crate) fn lifted(&self) -> Option<Show> {
        let show = self.lift?;
        let id = self.payload_sel.as_deref()?;
        (id == LIVE_RUN || self.run_carries_live_now(id)).then_some(show)
    }

    /// **Throw the one switch** — the kind a run is drawn lifted to, on the run named.
    ///
    /// The address and the kind are written together, which is what makes an orphan impossible:
    /// the lift is only ever read against `payload_sel`, so a door that cleared one and not the
    /// other (the old `set_verbosity`, and `ctrl-r` through it) can no longer leave a lift with
    /// no panel to close it.
    pub(crate) fn set_lift(&mut self, show: Option<Show>, run: &str) {
        self.lift = show;
        self.payload_sel = Some(run.to_string());
        self.payload_page = 0;
        self.payload_max.set(usize::MAX);
        self.invalidate_history();
        self.redraw = true;
    }

    /// **Put the switch back, and take its panel with it.**
    ///
    /// A panel is the lift's own address and carries its exit (`esc folds the run back`), so the
    /// two go together — the defect this closes is a lift whose panel was cleared by somebody else
    /// and which then had no key that could reach it. A ctrl-v window on one result is **not** the
    /// lift's and is left alone.
    pub(crate) fn drop_lift(&mut self) {
        let panel = self.payload_sel.as_deref() == Some(LIVE_RUN) || self.run_is_open();
        if self.lift.is_none() && !panel {
            return;
        }
        self.lift = None;
        if panel {
            self.payload_sel = None;
            self.payload_page = 0;
        }
        self.invalidate_history();
        self.redraw = true;
    }

    /// **Is the model's thinking open on the screen** — one question, whichever of the two
    /// spellings holds it.
    ///
    /// It is asked of what is **drawn**, not of a field: above the bottom rung the reasoning rows
    /// are drawn and their openness is the fold; at the bottom rung (`conversation`, `read-edits`)
    /// they are hidden and the fold draws nothing at all, so the answer there is the lift on a run.
    /// A reader who had `thinking = "open"` in `head.toml` and then chose `read-edits` has a fold
    /// nobody can see; counting it as open would make `ctrl-r` a key that closes something
    /// invisible and opens nothing.
    pub(crate) fn thinking_open(&self) -> bool {
        if self.visibility.hides_the_working() {
            self.lift == Some(Show::Thinking)
        } else {
            self.reasoning.is_open()
        }
    }

    /// **Open or close the thinking** — the one writer of the answer above, and the reason a click
    /// and `ctrl-r` cannot disagree about it.
    ///
    /// **Which spelling it moves is decided by the rung and by nothing else.** Where the reasoning
    /// rows are drawn, the switch is the fold and `/verbosity thinking=…` is the one writer of it
    /// (so the status row, `head.toml` and the drawing move together). At the bottom rung they are
    /// hidden and writing the fold would move the RUNG rather than the thinking —
    /// `thinking=folded` over `conversation` is `normal`, and `esc` would then *raise* a rung
    /// instead of returning to a default — so there the switch is the lift, on the newest run
    /// there is.
    pub(crate) fn set_thinking(&mut self, open: bool) -> Option<Action> {
        if self.visibility.hides_the_working() {
            if !open {
                self.drop_lift();
                return None;
            }
            return match self.newest_run_id() {
                Some(run) => {
                    self.set_lift(Some(Show::Thinking), &run);
                    None
                }
                // Nothing is folded, so there is nothing to open — and a key that silently did
                // nothing is the state this whole change is about.
                None => {
                    self.say("nothing is folded to open");
                    None
                }
            };
        }
        // The rung draws the reasoning, so the switch is the fold.
        if !open && self.lift == Some(Show::Thinking) {
            self.drop_lift();
        }
        if self.reasoning.is_open() == open {
            // Already there. An `esc` on a panel must not re-say a verbosity change that is not
            // one, and must not rewrite the preference file for it.
            return None;
        }
        self.set_verbosity(if open {
            "thinking=open"
        } else {
            "thinking=folded"
        })
    }

    /// **Flip it** — what `ctrl-r`, `/think`, the config pane's row and a second click all ask.
    pub(crate) fn flip_thinking(&mut self) -> Option<Action> {
        self.set_thinking(!self.thinking_open())
    }

    /// **The newest run a key can address**, by the id `payload_sel` names — the live tail's
    /// sentinel for a turn whose work has no row yet.
    fn newest_run_id(&self) -> Option<String> {
        let at = crate::ui::transcript::markers::newest_unseen_run(
            &self.items,
            self.visibility,
            &self.bound_prompts,
            self.live_work_now(),
        )?;
        Some(match self.items.get(at) {
            Some(it) => it.item_id.clone(),
            None => LIVE_RUN.to_string(),
        })
    }

    /// **The marker clauses among lines `start..end` of the conversation**, as screen rows of the
    /// window — the same walk over `spans` that [`App::file_rows_in`] makes. A marker is drawn on
    /// the run's own first row, or glued to the prose row just before it; either way the run is
    /// the hidden stretch starting at the first hidden row from there.
    pub(crate) fn marker_rows_in(&self, start: usize, end: usize) -> Vec<MarkerHit> {
        let end = end.min(self.hist_lines.len());
        let mut out = Vec::new();
        for line in start..end {
            let cells = keyhints::cells(&self.hist_lines[line]);
            let found = clauses(&cells);
            if found.is_empty() {
                continue;
            }
            let i = self.spans.partition_point(|sp| sp.at <= line);
            let Some(sp) = i.checked_sub(1).map(|i| self.spans[i]) else {
                continue;
            };
            let Some(first) = (sp.row..self.items.len())
                .find(|k| crate::ui::row_hidden(&self.items, self.visibility, *k))
            else {
                continue;
            };
            let run = self.items[first].item_id.clone();
            for (from, to, show) in found {
                out.push(MarkerHit {
                    y: line - start,
                    from,
                    to,
                    run: run.clone(),
                    show,
                });
            }
        }
        out
    }

    /// The clause under the pointer, on the last frame.
    pub(crate) fn marker_at(&self, x: u16, y: u16) -> Option<&MarkerHit> {
        let top = self
            .hits
            .iter()
            .find(|h| h.target == Target::Conversation)?
            .rect
            .y;
        let r = usize::from(y).checked_sub(top)?;
        let x = usize::from(x);
        self.marker_rows
            .iter()
            .find(|m| m.y == r && x >= m.from && x < m.to)
    }

    /// The clause's place on the screen, for the hover to light.
    pub(crate) fn marker_screen(&self, m: &MarkerHit) -> Option<(usize, usize, usize)> {
        let top = self
            .hits
            .iter()
            .find(|h| h.target == Target::Conversation)?
            .rect
            .y;
        Some((top + m.y, m.from, m.to))
    }

    /// **The live pane's marker clauses, as targets** — the rows of this frame below the history
    /// (`hist_lines`), which is where the turn in flight is drawn. Its run is [`LIVE_RUN`]: it has
    /// no rows of its own until they land.
    pub(crate) fn live_marker_rows(&self, start: usize, out: &[String]) -> Vec<MarkerHit> {
        let mut hits = Vec::new();
        // Nothing to read while no turn is up, and a row with no `]` holds no clause — read
        // cheaply first, so a frame copies nothing for the rows that cannot be markers.
        if self.turn.is_none() {
            return hits;
        }
        for (y, row) in out.iter().enumerate() {
            if start + y < self.hist_lines.len() || !row.contains(']') {
                continue;
            }
            for (from, to, show) in clauses(&keyhints::cells(row)) {
                hits.push(MarkerHit {
                    y,
                    from,
                    to,
                    run: LIVE_RUN.into(),
                    show,
                });
            }
        }
        hits
    }

    /// **The run the newest hidden rows make**, by its first row's id — what a lift in the live
    /// pane is carried to when a round's rows land.
    pub(crate) fn newest_run_head(&self) -> Option<String> {
        let hidden = |k: usize| crate::ui::row_hidden(&self.items, self.visibility, k);
        let last = (0..self.items.len()).rev().find(|&k| hidden(k))?;
        let mut first = last;
        while first > 0 && hidden(first - 1) {
            first -= 1;
        }
        Some(self.items[first].item_id.clone())
    }

    /// **A round's rows landed: what the live pane had lifted stays lifted on them** — the run
    /// they make opens with the same kind, and the pane keeps it for the next round.
    pub(crate) fn carry_live_open(&mut self) {
        // **The VALUE is not this function's business.** A reader who opened the thinking keeps
        // the thinking open; what a round's end moves is the ADDRESS, from the pane (which is
        // over) to the rows the work became — the handover that stops the panel vanishing at the
        // moment it is most worth reading. `on_turn_event` drops the lift at a new TURN, and that
        // is the only boundary allowed to change it.
        if self.lift.is_none() || self.payload_sel.as_deref() != Some(LIVE_RUN) {
            return;
        }
        match self.newest_run_head() {
            Some(run) => {
                let show = self.lift;
                self.set_lift(show, &run);
            }
            // Nothing landed to carry it to — the panel was the pane's, and the pane is over.
            None => self.drop_lift(),
        }
    }

    /// **Open the run a clause stands for**, lifting only its kind — and put it back when the same
    /// clause of the same run is clicked again.
    pub(crate) fn open_marker(&mut self, m: MarkerHit) {
        // **The whole-run clause of the pane's own marker** names no kind (`ctrl-t`'s act is the
        // run lifted whole), and the pane has no rows to lift whole — so it opens the calls, which
        // is what the marker it is drawn on counts.
        let show = m.show.or((m.run == LIVE_RUN).then_some(Show::Tools));
        // **The same clause, twice, is the toggle** — the operator's *"click once opens, click
        // again closes"* — and it is a toggle on the STATE and not on the frame: `payload_sel`
        // still naming this run is what says the panel the first click opened is still up.
        // Nothing here depends on the marker's row being on the screen, which is the half of
        // the old defect a second click could not reach.
        if self.payload_sel.as_deref() == Some(m.run.as_str()) && self.lift == show {
            if show == Some(Show::Thinking) {
                // Through the one verb, so a click closes exactly what `ctrl-r` would.
                self.set_thinking(false);
            } else {
                self.drop_lift();
            }
            return;
        }
        // **`payload_sel` names the live run too**, and it did not use to: the lift lived in
        // `live_open` alone, which is not what [`App::run_is_open`] reads — so the panel a click
        // opened could not be closed by Esc, named no keys in the hint bar and scrolled as the
        // conversation rather than as a view. One field for one act.
        self.set_lift(show, &m.run);
    }

    /// **Is `head` the run the work in flight belongs to** — [`run_carries_live`] asked of the head
    /// rather than of the frame being drawn, so a click and the draw cannot disagree about which
    /// run's counts the marker carried.
    fn run_carries_live_now(&self, head: &str) -> bool {
        let live = self.live_work_now();
        if live.work() == 0 {
            return false;
        }
        let Some(start) = self.items.iter().position(|it| it.item_id == head) else {
            return false;
        };
        let turn_rows: std::collections::HashSet<String> = self
            .turn
            .as_ref()
            .map(|t| t.turn_rows.iter().cloned().collect())
            .unwrap_or_default();
        crate::ui::transcript::markers::unseen_run_at(
            &self.items,
            self.visibility,
            &self.bound_prompts,
            live,
            start,
        )
        .is_some_and(|(s0, e0)| {
            s0 == start && crate::ui::run_carries_live(&self.items, &turn_rows, s0, e0)
        })
    }
}
