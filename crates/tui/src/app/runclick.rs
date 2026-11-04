//! A run's marker opens by its parts: a click on the calls clause opens the tool calls, on the
//! thinking clause the reasoning, and anywhere else the whole run. The lift is one field applied to
//! whatever `payload_sel` names, and a lift with no panel has no key that could close it.

use super::*;
use crate::app::visibility::Show;

/// The run a live pane's marker stands for - the turn in flight, whose rows have not landed.
pub(crate) const LIVE_RUN: &str = "\u{0}live";

/// One clause of a marker on the last frame: where it was drawn and what a click opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MarkerHit {
    /// The screen row of the window it is on.
    pub(crate) y: usize,
    /// Its columns, `from..to`.
    pub(crate) from: usize,
    pub(crate) to: usize,
    /// The run's first row, as `payload_sel` names an open run.
    pub(crate) run: String,
    /// The kind a click lifts - `None` is the whole run.
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

/// The clauses of the marker on one line of text, as `(from, to, kind)` in columns - the
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

/// A row an open run leaves out, because the click asked for the other kind: the reasoning when it
/// asked for the calls, the tool rows and empty assistant rows when it asked for the thinking. The
/// rung ladder cannot express thinking without tools, so the lift plus this filter is what does.
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
    /// The kind a click lifted, for the turn still running: the pane's half of the one switch. A run
    /// whose counted work is still running draws a marker over calls that are not rows yet, so its
    /// clause must lift for the pane too or the panel shows none of the kind asked for.
    pub(crate) fn lifted(&self) -> Option<Show> {
        let show = self.lift?;
        let id = self.payload_sel.as_deref()?;
        (id == LIVE_RUN || self.run_carries_live_now(id)).then_some(show)
    }

    /// Throw the one switch: the kind a run is drawn lifted to, on the run named. The address and the
    /// kind are written together, which is what makes a lift with no panel to close it impossible -
    /// the lift is only ever read against `payload_sel`.
    pub(crate) fn set_lift(&mut self, show: Option<Show>, run: &str) {
        self.lift = show;
        // A new run starts folded. The pane's rows landing is the same run, so its address moving
        // off the sentinel keeps what was opened.
        if self.payload_sel.as_deref() != Some(LIVE_RUN) && self.payload_sel.as_deref() != Some(run)
        {
            self.run_tools = Fold::Folded;
        }
        // The panel's address and its offset are one act - see [`App::open_payload_window`],
        // which is also what `ctrl-v` opens with.
        self.open_payload_window(run.to_string());
        self.invalidate_history();
        self.redraw = true;
    }

    /// Put the switch back, and take its panel with it. A panel is the lift's own address and carries
    /// its exit, so the two go together; a `ctrl-v` window on one result is not the lift's and is left
    /// alone.
    pub(crate) fn drop_lift(&mut self) {
        let panel = self.payload_sel.as_deref() == Some(LIVE_RUN) || self.run_is_open();
        if self.lift.is_none() && !panel {
            return;
        }
        self.lift = None;
        self.run_tools = Fold::Folded;
        if panel {
            self.payload_sel = None;
            self.payload_page = 0;
        }
        self.invalidate_history();
        self.redraw = true;
    }

    /// Open or fold the rows of the open run, and only those: the one writer behind `/t` and a click
    /// on the run, so neither can reach the transcript-wide fold. A no-op when no run is up.
    pub(crate) fn flip_run_tools(&mut self) -> bool {
        if !self.run_is_open() {
            return false;
        }
        self.run_tools = self.run_tools.flip();
        self.invalidate_history();
        self.redraw = true;
        true
    }

    /// Is the screen row `y` inside the open run's rows, as the last frame drew them.
    pub(crate) fn run_row_at(&self, y: u16) -> bool {
        let Some(id) = self.payload_sel.as_deref().filter(|_| self.run_is_open()) else {
            return false;
        };
        let Some(top) = self
            .hits
            .iter()
            .find(|h| h.target == Target::Conversation)
            .map(|h| h.rect.y)
        else {
            return false;
        };
        let Some(line) = usize::from(y).checked_sub(top).map(|r| self.view_top + r) else {
            return false;
        };
        // Past the history is the live pane, which is the open run when the sentinel names it.
        let Some(i) = self
            .spans
            .partition_point(|sp| sp.at <= line)
            .checked_sub(1)
        else {
            return id == LIVE_RUN;
        };
        let sp = self.spans[i];
        if line >= self.hist_lines.len() || line >= sp.at + sp.lines {
            return id == LIVE_RUN;
        }
        crate::ui::transcript::markers::run_open_at(
            &self.items,
            self.visibility,
            &self.bound_prompts,
            self.live_work_now(),
            Some(id),
            sp.row,
        )
    }

    /// Is the model's thinking open on the screen - one question, whichever of the two spellings holds
    /// it. It is asked of what is drawn, not of a field: at the bottom rung the reasoning rows are
    /// hidden, so a fold there would answer for a screen that does not show it.
    pub(crate) fn thinking_open(&self) -> bool {
        if self.visibility.hides_the_working() {
            self.lift == Some(Show::Thinking)
        } else {
            self.reasoning.is_open()
        }
    }

    /// Open or close the thinking: the one writer of the answer above, which is why a click and
    /// `ctrl-r` cannot disagree. Which spelling it moves is decided by the rung - the fold where the
    /// reasoning is drawn, the lift at the bottom rung, where writing the fold would move the RUNG.
    pub(crate) fn set_thinking(&mut self, open: bool) -> Option<Action> {
        if self.visibility.hides_the_working() {
            if !open {
                self.drop_lift();
                return None;
            }
            return match self.newest_run_id() {
                // A run with no thinking in it is not opened on the thinking: `run_skips` would drop
                // its tool rows and draw nothing in their place. The honest answer is the sentence plus
                // the way out, not a press that eats the screen.
                Some(run) if self.run_holds_thinking(&run) => {
                    self.set_lift(Some(Show::Thinking), &run);
                    None
                }
                Some(_) => {
                    self.say(
                        "nothing to open: the newest run holds no thinking, and this rung hides \
                         it — `/verbosity normal` draws the reasoning, or click a clause of its \
                         marker for the kind it does hold",
                    );
                    None
                }
                // Nothing is folded, so there is nothing to open - and a key that silently did
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

    /// Flip it - what `ctrl-r`, `/think`, the config pane's row and a second click all ask.
    pub(crate) fn flip_thinking(&mut self) -> Option<Action> {
        self.set_thinking(!self.thinking_open())
    }

    /// Does the run named hold any thinking: the question a key that opens the thinking must ask,
    /// because `run_skips` hides everything else. The live tail answers from the pane's own count; a
    /// run of rows is asked row by row, and only the rows the rung hides count.
    fn run_holds_thinking(&self, run: &str) -> bool {
        if run == LIVE_RUN {
            return self.live_work_now().think_lines > 0;
        }
        let Some(start) = self.items.iter().position(|it| it.item_id == run) else {
            return false;
        };
        let Some((start, end)) = crate::ui::transcript::markers::unseen_run_at(
            &self.items,
            self.visibility,
            &self.bound_prompts,
            self.live_work_now(),
            start,
        ) else {
            return false;
        };
        (start..end).any(|r| {
            crate::ui::row_hidden(&self.items, self.visibility, r)
                && matches!(
                    self.items[r].item.as_ref(),
                    // A row with no words is not thinking: the marker counts display lines, so an
                    // empty reasoning row is counted by nothing and draws nothing, and lifting for it
                    // would hide the calls beside it for a screenful of nothing.
                    Some(letibot_transcript::TranscriptItem::Reasoning { text, .. })
                        if !text.trim().is_empty()
                )
        })
    }

    /// The newest run a key can address, by the id `payload_sel` names - the live tail's
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

    /// The marker clauses among lines `start..end`, as screen rows of the window. A marker with no
    /// hidden row under it stands for the work in flight - a round's first call is counted before any
    /// result row exists - and its address is `LIVE_RUN`, the same one the pane's counter carries.
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
            let run = match (sp.row..self.items.len())
                .find(|k| crate::ui::row_hidden(&self.items, self.visibility, *k))
            {
                Some(first) => self.items[first].item_id.clone(),
                // Nothing hidden here, so the only thing the counts can be about is the turn
                // that is still running. A marker with neither is a join the next frame undoes.
                None if self.live_work_now().work() > 0 => LIVE_RUN.to_string(),
                None => continue,
            };
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

    /// The clause under the pointer, on the last frame. The pointer's `x` is a terminal cell and a
    /// clause's columns are the row's own: the gutter is padded on after the frame is composed, so a
    /// record read off `hist_lines` is short by that much and the target sits left of what it drew.
    pub(crate) fn marker_at(&self, x: u16, y: u16) -> Option<&MarkerHit> {
        let top = self
            .hits
            .iter()
            .find(|h| h.target == Target::Conversation)?
            .rect
            .y;
        let r = usize::from(y).checked_sub(top)?;
        let x = usize::from(x).checked_sub(Self::gutter(self.term_cols))?;
        self.marker_rows
            .iter()
            .find(|m| m.y == r && x >= m.from && x < m.to)
    }

    /// The clause's place on the screen, for the hover to light - the columns it was drawn in,
    /// which is the record's own plus the gutter (see [`App::marker_at`]).
    pub(crate) fn marker_screen(&self, m: &MarkerHit) -> Option<(usize, usize, usize)> {
        let top = self
            .hits
            .iter()
            .find(|h| h.target == Target::Conversation)?
            .rect
            .y;
        let gutter = Self::gutter(self.term_cols);
        Some((top + m.y, m.from + gutter, m.to + gutter))
    }

    /// The live pane's marker clauses, as targets - the rows of this frame below the history
    /// (`hist_lines`), which is where the turn in flight is drawn. Its run is [`LIVE_RUN`]: it has
    /// no rows of its own until they land.
    pub(crate) fn live_marker_rows(&self, start: usize, out: &[String]) -> Vec<MarkerHit> {
        let mut hits = Vec::new();
        // Nothing to read while no turn is up, and a row with no `]` holds no clause - read
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

    /// The run the newest hidden rows make, by its first row's id - what a lift in the live
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

    /// A round's rows landed: what the live pane had lifted stays lifted on them - the run
    /// they make opens with the same kind. The pane does not keep it: the next round's pane
    /// starts as the rung draws it, and a new turn drops the lift entirely.
    pub(crate) fn carry_live_open(&mut self) {
        // The VALUE is not this function's business: a round's end moves the ADDRESS, from the pane
        // to the rows the work became, so the panel does not vanish when it is worth reading. Only a
        // new turn, in `on_turn_event`, drops the lift.
        if self.lift.is_none() || self.payload_sel.as_deref() != Some(LIVE_RUN) {
            return;
        }
        match self.newest_run_head() {
            Some(run) => {
                let show = self.lift;
                self.set_lift(show, &run);
            }
            // Nothing landed to carry it to - the panel was the pane's, and the pane is over.
            None => self.drop_lift(),
        }
    }

    /// Open the run a clause stands for, lifting only its kind - and put it back when the same
    /// clause of the same run is clicked again.
    pub(crate) fn open_marker(&mut self, m: MarkerHit) {
        // The whole-run clause of the pane's own marker names no kind (`ctrl-t`'s act is the
        // run lifted whole), and the pane has no rows to lift whole - so it opens the calls, which
        // is what the marker it is drawn on counts.
        let show = m.show.or((m.run == LIVE_RUN).then_some(Show::Tools));
        // The same clause, twice, is the toggle - and a toggle on the STATE, not on the frame:
        // `payload_sel` still naming this run is what says the panel the first click opened is up.
        if self.payload_sel.as_deref() == Some(m.run.as_str()) && self.lift == show {
            if show == Some(Show::Thinking) {
                // Through the one verb, so a click closes exactly what `ctrl-r` would.
                self.set_thinking(false);
            } else {
                self.drop_lift();
            }
            return;
        }
        // `payload_sel` names the live run too, so one field answers for one act: the panel a click
        // opened is the one Esc closes, the hint bar names it, and it scrolls as a view.
        self.set_lift(show, &m.run);
    }

    /// Is `head` the run the work in flight belongs to - [`run_carries_live`] asked of the head
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
