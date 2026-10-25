//! **A run's marker opens by its parts** — the operator: *"when I see this "[7 tool calls, 11
//! thinking lines]" and then click on tool calls section or thinking section i wanted them to
//! expand. Esc should fold them back"*.
//!
//! Opening a run is `ctrl-t`'s act already (`payload_sel` naming the run's first row, the rung
//! lifted for that run's rows — see `ui::transcript::markers::run_open_at`). A click does the same
//! for the run under the pointer, and lifts only the kind it was on: the calls clause opens the
//! tool calls, the thinking clause the reasoning, anything else the whole run. Esc closes it as it
//! closes any open run.

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
        let Some(show) = self.live_open else {
            return;
        };
        if let Some(run) = self.newest_run_head() {
            self.payload_sel = Some(run);
            self.payload_page = 0;
            self.payload_max.set(usize::MAX);
            self.run_show = Some(show);
            self.invalidate_history();
        }
    }

    /// **Open the run a clause stands for**, lifting only its kind.
    pub(crate) fn open_marker(&mut self, m: MarkerHit) {
        // **The turn still running** — its rows are in the pane, not the history: lift that kind
        // there, or put it back when the same clause is clicked again.
        if m.run == LIVE_RUN {
            self.live_open = if self.live_open == m.show && m.show.is_some() {
                None
            } else {
                m.show.or(Some(Show::Tools))
            };
            if self.live_open.is_none() {
                self.payload_sel = None;
                self.run_show = None;
                self.invalidate_history();
            }
            self.redraw = true;
            return;
        }
        self.payload_sel = Some(m.run);
        self.payload_page = 0;
        self.payload_max.set(usize::MAX);
        self.run_show = m.show;
        self.invalidate_history();
        self.redraw = true;
    }
}
