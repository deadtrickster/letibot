//! Where the reader is: the history buffer of drawn rows, the anchor a held view keeps,
//! following the bottom, and filling backwards when the reader scrolls past what was drawn.

use super::*;
use crate::ui::render::{RenderConfig, visible_width};
use letibot_sessionlog::view::{CallState, SnapshotItem, TurnState, Warned};
use letibot_transcript::{TranscriptItem, UserPart};

/// One row the backward walk built: its index, its class, its lines, the marker it introduced,
/// and whether that marker joins the prose above rather than standing alone.
type Built = (usize, RowClass, Vec<String>, Option<Marker>, bool);

impl App {
    /// Throw the rendered history away; it is rebuilt from `items` and `notes` on the next frame.
    /// One place, because forgetting one of the two cursors duplicates or loses everything after it.
    /// For the callers that mean all of it; everything else means [`App::invalidate_history_from`].
    pub(crate) fn invalidate_history(&mut self) {
        self.invalidate_history_from(0);
    }

    /// The rendered history is stale from row `k` on, and rows above it are settled.
    /// Not a repaint: the painter writes only rows whose text changed, so a rebuild that produces
    /// the same lines writes no bytes.
    pub(crate) fn invalidate_history_from(&mut self, k: usize) {
        // A row at or below the buffer's end changes nothing this head drew: the window stops above
        // it, and the body or the marker it carries is drawn when the reader comes down to it.
        if self.hist_ceiling.is_some_and(|end| k >= end) {
            return;
        }
        if k == 0 {
            self.hist_lines.clear();
            self.hist_marks.clear();
            self.hist_ceiling = None;
            self.walk_ahead = usize::MAX;
            // The anchor's map goes with the lines it describes. A stale span would
            // place the viewport inside a frame that no longer exists.
            self.spans.clear();
            self.note_cards.clear();
            self.hist_upto = 0;
            self.hist_floor = 0;
            self.hist_first_class = None;
            self.note_upto = 0;
            self.hist_class = None;
            // The target table is the walk's own state - the round it is currently
            // inside - so it is thrown away with the lines it labelled. Leaving it
            // behind is what let a rebuild start at row 0 holding round 14's paths.
            self.call_targets.clear();
            return;
        }
        // A row the walk has not reached has nothing to throw away, and rewinding to it would rewind
        // past rows that are fine. In tail mode there are no marks at all, so the only correct
        // answer is to throw the history away and render the tail again.
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
        self.note_cards.retain(|c| c.at < mark.lines);
        self.hist_upto = k;
        self.note_upto = mark.note_upto;
        self.hist_class = mark.class;
        self.retarget_before(k);
    }

    /// Put `call_targets` back to what it held before row `k`: the calls of the nearest assistant
    /// row above it with a body. Derived, and it must match the walk, which replaces the table at
    /// every such row because `call_0` is positional within a round.
    pub(crate) fn retarget_before(&mut self, k: usize) {
        self.call_targets = targets_before(&self.items, k);
    }

    /// The transcript's hidden runs and ids as they stand, built fresh by every door that needs
    /// one. Derived state that cannot go stale is cheaper than an invalidation hook every
    /// mutation site would have to remember (R123.3).
    pub(crate) fn runs_index(&self) -> RunIndex<'_> {
        RunIndex::of(&self.items, self.visibility, &self.bound_prompts)
    }

    /// Where the marker counting row `idx` is drawn: the start of the stretch of undrawn rows it
    /// belongs to, and the drawn row before it, whose line the marker is glued to. A run spans
    /// rounds, so its marker can sit on an earlier round's prose line.
    pub(crate) fn run_head(&self, idx: usize) -> usize {
        let vis = self.visibility;
        if !vis.hides_the_working() || idx >= self.items.len() {
            return idx;
        }
        let drawn = |r: usize| crate::ui::row_drawn(&self.items, vis, &self.bound_prompts, r);
        let mut start = idx;
        while start > 0 && !drawn(start - 1) {
            start -= 1;
        }
        start.saturating_sub(1)
    }

    /// The first row whose rendering a change to row `idx` can reach: the row's round.
    /// An assistant row asks which of its calls came back and that answer lives after it, so
    /// rewinding only to a result leaves a proposal beside its own answer.
    pub(crate) fn round_head(&self, idx: usize) -> usize {
        // A user row is its own head, or a conversation of user rows would rewind to zero on every
        // one. Anything else belongs to the nearest assistant row above, past any user rows between:
        // a result landing after a mid-round message must reach the row that proposed its call.
        if self.items[idx].kind == "user" {
            return idx;
        }
        self.items[..=idx]
            .iter()
            .rposition(|r| r.kind == "assistant")
            .unwrap_or(0)
    }

    /// The first history row this turn's pane is drawing, or `None`. A turn-state transition changes
    /// `drawn_live`, which is true only for a row in `TurnPane::appended`, so it can change those
    /// rows and nothing above them.
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

    /// Hold the view, or let it follow again: `ctrl-p` and nothing else. While held the head writes
    /// nothing at all, because one written cell is one lost selection; [`App::screen`] composes one
    /// frame at the release and this counts what arrived.
    ///
    /// ```text
    ///   ╭────────────────────────────────────────────── 1 subagent running ─╮
    ///   │ › why did the cache miss                                               │
    ///   ╰────────────────────────────── ⚠ · ⠹ Responding · 4.2s · 1.2k chars ─╯
    ///   ctrl-s sessions · ctrl-p todos · ctrl-g subagents · ctrl-r thinking · …
    /// ```
    pub(crate) fn toggle_hold(&mut self) -> Option<Action> {
        if self.hold {
            self.hold = false;
            let arrived = self.items.len().saturating_sub(self.hold_rows);
            self.hold_rows = 0;
            self.hold_frame = None;
            self.hold_size = (0, 0);
            self.say(&format!(
                "the view follows again — {arrived} rows arrived while it was held"
            ));
        } else {
            self.hold = true;
            self.hold_rows = self.items.len();
            self.hold_frame = None;
            self.hold_size = (0, 0);
        }
        None
    }

    /// Render the tail of a conversation rather than all of it: walking from row 0 to draw the bottom forty lexes
    /// every row above. It walks backward from the floor with the same renderer. `want` is how many lines the reader
    /// needs ABOVE them; returns the count of rows drawn.
    pub(crate) fn fill_backward(&mut self, want: usize) -> usize {
        self.fill_backward_until(want, None)
    }

    /// The same walk with a row it must reach: a reader moving by lines wants a screen's worth, and a
    /// viewport holding a row wants that row. `stop_row` of `None` is the lines-only walk.
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
        // The turn's rows, for the one question that is about the TURN: `in_flight` is this round's,
        // while `live_here` asks whether a run belongs to the turn at all. Asking the second with the
        // first is the defect [`TurnPane::turn_rows`] records.
        let turn_rows: std::collections::HashSet<String> = self
            .turn
            .as_ref()
            .map(|t| t.turn_rows.iter().cloned().collect())
            .unwrap_or_default();
        // `(row, class, lines, marker, joinable)`: the block is prepended to the history, so the
        // records must be made here. `joinable` rides along because the assembly loop has only the
        // row index and the class, and a marker that joins must not have the separator's blank.
        let mut built: Vec<Built> = Vec::new();
        // Read once, before the loop: the walk needs it per row and recomputing it there
        // would be a scan of `items` for every row drawn.
        let newest_payload = self.newest_payload_row();
        // The runs and the ids, once for the whole walk: the questions below are asked per
        // row, and re-deriving them from `items` per row is what made one long hidden run
        // quadratic (R123.3).
        let runs = RunIndex::of(&self.items, self.visibility, &self.bound_prompts);
        // The calls in force and the ids answered, carried down the walk rather than
        // rescanned per row: both hold until the walk crosses the row that changes them,
        // which is the row the scans would have stopped at anyway.
        let mut targets = targets_before(&self.items, self.hist_floor);
        let mut answered = round_results(&self.items, self.hist_floor);
        let mut k = self.hist_floor;
        // `want` is counted from the reader and not from the buffer's first line: the ring cuts rows
        // off the top, so an absolute line index would say *enough* about a window that has none
        // above it. With nothing drawn yet the reader is the buffer's start, which is the first frame.
        let held = self.held_line().min(self.hist_lines.len());
        let covered = |built: &[Built]| {
            self.hist_lines.len().saturating_sub(held)
                + built
                    .iter()
                    .map(|(_, _, l, _, _)| l.len() + 1)
                    .sum::<usize>()
        };
        // The row condition is `Option`al on purpose: `k > stop_row` with 0 for "no row" is true for
        // every `k > 0`, so `None` is the lines-only walk. The work in flight is counted here too, since
        // the walk is entered when the transcript is too big to render forward.
        let live = live_work(
            self.turn.as_ref(),
            &self.items,
            &self.cfg,
            !matches!(
                self.turn.as_ref().and_then(|t| t.state.as_ref()),
                Some(TurnState::Running) | None
            ),
        );
        // The run `ctrl-t` opens, once, for the same reason the forward walk computes it
        // once: the seam names the chord only where it acts.
        let newest_run = newest_unseen_run(&runs, live);
        // `carry`: the walk does not stop in the middle of a run, because a run's marker is drawn at its first
        // row and this walk renders the newest first. Continuing costs one predicate per row, and the rows
        // draw no lines.
        let mut carry = false;
        // `need_speaker`: a marker with nothing drawn beside it yet. The join depends on where the line budget
        // stopped, so the walk renders one more row rather than let the same transcript join or not depending
        // on how far somebody had scrolled.
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
            // The row just passed over joins the round below it: a result answers a call, the
            // model's own row ends the round, anything else leaves the set alone - `round_results`'
            // own ladder. A walk from the very end passes over no row: `k + 1` is past the list.
            if k + 1 < self.items.len() {
                match self.items[k + 1].item.as_ref() {
                    Some(TranscriptItem::ToolResult { call_id, .. }) => {
                        answered.insert(call_id.clone());
                    }
                    Some(TranscriptItem::Assistant { .. }) => {
                        answered.clear();
                    }
                    _ if self.items[k + 1].kind == "assistant" => {
                        answered.clear();
                    }
                    _ => {}
                }
            }
            // And the row reached: when it is an assistant row it held the calls in force for
            // the row above, so the walk is leaving its round and the table is rebuilt from the
            // next one down - a rescan, but only ever at an assistant row.
            if matches!(
                self.items[k].item.as_ref(),
                Some(TranscriptItem::Assistant { .. })
            ) {
                targets = targets_before(&self.items, k);
            }
            // The marker, or the row: both walks decide which row of a run owns the line the same way, the
            // run's first, so they agree about a block without either remembering what the other drew.
            let open_run = run_open_at(&runs, live, self.payload_sel.as_deref(), k);
            // The same reservation the forward walk makes, from the same rule and the same
            // data - so the two walks wrap the introducing sentence identically and the
            // marker lands in the room either of them left.
            let reserve =
                reserved_for_run(&self.items, &runs, live, &cfg, k, newest_run == Some(k + 1));
            let row_cfg = match reserve {
                Some(room) => RenderConfig {
                    width: cfg.width.saturating_sub(room).max(20),
                    ..cfg.clone()
                },
                None => cfg.clone(),
            };
            // The marker is drawn whether the run is open or not - see the forward walk's
            // note: an open run's marker is the header of the rows it lifted, and its clauses are
            // the toggle that closes it again. The two walks keep the same three states.
            let unseen = unseen_run_at(&runs, live, k);
            carry = unseen.is_some_and(|(start, _)| start < k);
            let joinable = unseen.is_some_and(|(start, _)| run_continues_prose(&self.items, start));
            // A row of an OPEN run draws itself under that header, and a row the click's kind
            // skips draws nothing. A CLOSED run's rows are not lexed at all.
            let absorbed_here = crate::app::one_event::is_absorbed(
                &self.absorbed,
                &self.notes,
                &self.dismissed,
                &self.quiet,
                &self.read_marks,
                self.items[k].item_id.as_str(),
            );
            let skipped = open_run && crate::app::runclick::run_skips(self.lift, &self.items[k]);
            // The marker's text, when this row is the first of the run. The joining is the assembly loop's,
            // since forward order exists only there. See [`hidden_run_marker`].
            let marker = unseen.filter(|(start, _)| *start == k).map(|(start, end)| {
                // Does this run hold the turn's rows, or reach the live edge: the two clauses live in
                // [`run_carries_live`] so the two walks and the pane's guard cannot disagree about them.
                let live_here = newest_run == Some(start)
                    && run_carries_live(&self.items, &turn_rows, start, end);
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
            let (body_class, body) = if (open_run || unseen.is_none()) && !absorbed_here && !skipped
            {
                item_lines(
                    &self.items[k],
                    &ItemCtx {
                        cfg: &row_cfg,
                        think: if open_run {
                            crate::app::runclick::run_think(self.lift, think)
                        } else {
                            think
                        },
                        tools: tool,
                        raw,
                        targets: &targets,
                        answered: &answered,
                        subagents: &self.subagents,
                        job_tails: &self.job_tails,
                        drawn_live: in_flight.contains(self.items[k].item_id.as_str()),
                        elapsed_ms: self.call_ms.get(&self.items[k].item_id).copied(),
                        edit: self.call_edits.get(&self.items[k].item_id),
                        decision: self.call_decisions.get(&self.items[k].item_id),
                        workspace: &self.wiring.workspace,
                        bound: self
                            .bound_prompts
                            .get(&self.items[k].item_id)
                            .map(String::as_str),
                        // What this head knows about the echo and nothing about the text: the same string is
                        // `queued` in one session and `unconfirmed` in another, and a bound row being drawn is
                        // never `queued`. See [`App::echo_mark`].
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
                        replied_to: question_before(&self.items, k).as_deref(),
                    },
                )
            } else {
                (RowClass::Other, Vec::new())
            };
            let (class, rows) = match unseen {
                Some((start, _)) if start == k => {
                    let mut rows = vec![
                        marker
                            .as_ref()
                            .expect("a marker was built for this row")
                            .painted(&cfg),
                    ];
                    rows.extend(body);
                    (
                        if open_run {
                            body_class
                        } else {
                            RowClass::Activity
                        },
                        rows,
                    )
                }
                // A row of an open run - its header was drawn at the run's first row - and a row
                // of a closed one, which draws nothing.
                Some(_) => (body_class, body),
                None => (body_class, body),
            };
            if !rows.iter().all(|l| l.trim().is_empty()) {
                // A marker about to be the top of the block needs the row above it, or it cannot join. Only
                // when the marker will be glued, since fetching a sentence for a lone marker puts a row on the
                // screen the budget did not ask for.
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
                // The marker, glued into the sentence it continues: forward order exists only in this loop and
                // `prev` is the row just above. A line that cannot hold the counts leaves the marker standing
                // alone, which still hugs rather than starting a row.
                if let Some(m) = marker
                    && *joinable
                    && prev == Some(RowClass::Speech)
                    && let Some(at) = block.iter().rposition(|l| !l.trim().is_empty())
                {
                    let joined = format!("{} {}", block[at].trim_end(), m.painted(&cfg));
                    if visible_width(&joined) <= cfg.width {
                        block[at] = joined;
                        // The rows the marker stands over still go in: the header went onto the sentence above,
                        // so the row's own lines are the whole of what an open run shows under its marker.
                        if rows.len() > 1 {
                            fresh.push(Span {
                                row: *row,
                                at: block.len(),
                                lines: rows.len() - 1,
                                class: *class,
                            });
                            block.extend(rows[1..].iter().cloned());
                            prev = Some(*class);
                        }
                        continue;
                    }
                }
                let pack = prev == Some(RowClass::Activity) && *class == RowClass::Activity;
                // A marker that stands alone keeps the air prose gets - which is what the
                // operator asked for after their own message. Only a JOINED marker loses it.
                if !block.is_empty() && !pack && (marker.is_none() || !*joinable) {
                    block.push(String::new());
                }
                // The span, before the lines go in. `block` is in forward row order
                // here, so `built.iter().rev()` is the order the reader reads them in.
                fresh.push(Span {
                    row: *row,
                    at: block.len(),
                    lines: rows.len(),
                    class: *class,
                });
                block.extend(rows.iter().cloned());
                prev = Some(*class);
            }
            // And one at the seam: the row this block now precedes is the old head.
            if !self.hist_lines.is_empty() {
                let pack = self.hist_first_class == Some(RowClass::Activity)
                    && built.last().map(|(_, c, _, _, _)| *c) == Some(RowClass::Activity);
                // The marker keeps its sentence across the seam too: a fill prepends older rows, and a marker
                // at the top of the old buffer continues prose that is in the new block, so the blank goes.
                let tight = built
                    .last()
                    .is_some_and(|(_, _, _, m, joinable)| m.is_some() && *joinable);
                if !pack && !tight {
                    block.push(String::new());
                }
            }
            let first = built.last().map(|(_, c, _, _, _)| *c);
            // Every recorded line offset moves down by what was prepended, which is why the anchor is a row
            // and not a line number: a line index would creep on every fill.
            let shifted = block.len();
            block.append(&mut self.hist_lines);
            self.hist_lines = block;
            for sp in &mut self.spans {
                sp.at += shifted;
            }
            for c in &mut self.note_cards {
                c.at += shifted;
            }
            fresh.append(&mut self.spans);
            self.spans = fresh;
            if let Some(f) = first {
                self.hist_first_class = Some(f);
            }
        }
        self.hist_floor = k;
        // Accounted for, so the forward walk has no work until a new row arrives - and no further
        // than the window reaches: the rows below a buffer that stopped short are still its work.
        self.hist_upto = self
            .hist_upto
            .max(self.hist_ceiling.unwrap_or(self.items.len()));
        rendered
    }

    /// How many transcript rows this head has rendered. `hist_upto` counts every row
    /// accounted for, and `hist_floor` says how many were deliberately skipped, so the
    /// difference is what was drawn. Read by the tests that pin the budget.
    #[cfg(test)]
    pub(crate) fn rendered_rows(&self) -> usize {
        self.hist_upto.saturating_sub(self.hist_floor)
    }

    /// The row `ctrl-t` opens, asked of the whole head, and the answer depends on the rung: under
    /// `conversation` the long rows are inside a run that draws as one marker line, so what there is to open
    /// is the run, and the newest one. One function, so the chord and the seam cannot disagree.
    pub(crate) fn newest_openable(&self) -> Option<String> {
        let live = self.live_work_now();
        if let Some(start) = newest_unseen_run(&self.runs_index(), live) {
            // A run with no row yet is addressed by a sentinel, because the work in flight has no item. The
            // chord still opens something, and a marker that cannot be opened is the elision this file refuses.
            return Some(match self.items.get(start) {
                Some(it) => it.item_id.clone(),
                None => LIVE_RUN.to_string(),
            });
        }
        self.newest_payload_row()
    }

    /// Is what `payload_sel` names an open run rather than a window on one result? One field names both, and
    /// they want opposite things from the scroll keys: a window pages its own text, a run is the conversation
    /// drawn lifted. Asked of the same [`run_open_at`] the walks draw by, so the keys and the screen agree.
    pub(crate) fn run_is_open(&self) -> bool {
        let Some(id) = self.payload_sel.as_deref().filter(|id| !id.is_empty()) else {
            return false;
        };
        // The live tail's sentinel names no row, so it is answered here and not by [`run_open_at`], which
        // asks about a row. The panel is open while there is work in flight and no run of rows has taken it.
        if id == LIVE_RUN {
            let live = self.live_work_now();
            return live.work() > 0
                && !crate::ui::transcript::markers::live_tail_covered(
                    &self.items,
                    self.visibility,
                    &self.bound_prompts,
                );
        }
        let runs = self.runs_index();
        let Some(row) = runs.row_of(id) else {
            return false;
        };
        run_open_at(&runs, self.live_work_now(), Some(id), row)
    }

    /// A ctrl-v window is open - `payload_sel` names a result, not a run (see
    /// [`App::run_is_open`]). What the scroll keys ask before they page a window.
    pub(crate) fn payload_window_open(&self) -> bool {
        // The live tail's sentinel with no work behind it is a window on nothing.
        self.payload_sel.is_some()
            && self.payload_sel.as_deref() != Some(super::runclick::LIVE_RUN)
            && !self.run_is_open()
    }

    /// Open the payload window on `id`, at its top: the one writer of the three fields a window is made of.
    /// The page is not a property of the head but where the reader is in one payload, so a second payload
    /// opening at the first one's offset is a window that skips a head nobody has read.
    pub(crate) fn open_payload_window(&mut self, id: String) {
        self.payload_sel = Some(id);
        self.payload_page = 0;
        self.payload_max.set(usize::MAX);
    }

    /// Put the clicked line at the top of the window: it writes [`App::payload_page`], the field the arrows
    /// and the wheel write, so a click and a key agree about where the reader is. Clamped again here, so a
    /// click near the end cannot push the window past the last full page.
    pub(crate) fn payload_click(&mut self, line: usize) {
        if !self.payload_window_open() {
            return;
        }
        let max = self.payload_max.get();
        let to = line.min(max);
        if to == self.payload_page.min(max) {
            return;
        }
        self.payload_page = to;
        // The history buffer is a cache of the rendered rows, and the offset changes what one of
        // them renders to - see [`App::page_payload`].
        self.invalidate_history();
    }

    /// Which rows of the conversation are lines of the open payload window, read off the rows the walk drew,
    /// because only the drawing knows where the seams fell. The body is between the header and the first `…`
    /// row, and a payload line is its index plus the page the draw used.
    pub(crate) fn payload_rows_in(&self, start: usize, end: usize) -> Vec<(usize, usize)> {
        let Some(id) = self.payload_sel.as_deref().filter(|id| !id.is_empty()) else {
            return Vec::new();
        };
        if !self.payload_window_open() {
            return Vec::new();
        }
        // The page the DRAW used: `payload_max` is this frame's measurement (see
        // [`ItemCtx::payload_max`]), and a page past it was drawn clamped.
        let page = self.payload_page.min(self.payload_max.get());
        let end = end.min(self.hist_lines.len());
        let mut out = Vec::new();
        let mut last: Option<(usize, bool)> = None;
        for line in start..end {
            let i = self.spans.partition_point(|sp| sp.at <= line);
            let Some(sp) = i.checked_sub(1).map(|i| self.spans[i]) else {
                continue;
            };
            if line >= sp.at + sp.lines {
                continue;
            }
            // One question per row rather than per line: the window is one row's.
            let mine = match last {
                Some((row, mine)) if row == sp.row => mine,
                _ => {
                    let mine = self.items.get(sp.row).is_some_and(|it| it.item_id == id);
                    last = Some((sp.row, mine));
                    mine
                }
            };
            if !mine {
                continue;
            }
            // Where this row's body starts in its own lines: the header, the `↑` seam when the reader is past
            // the head, then the payload. Counted from the row's first line, so a scrolled header still names
            // its lines correctly.
            let body_at = sp.at + 1 + usize::from(page > 0);
            if line < body_at {
                // The header, or the `↑ N more lines above` seam: neither is payload text.
                continue;
            }
            let text: String = crate::app::keyhints::cells(&self.hist_lines[line])
                .into_iter()
                .collect();
            let t = text.trim_start();
            if t.starts_with('…') && t.ends_with("esc closes") {
                // The seam that ends the window - and with it the body, so a row this head
                // appended under it is not a line of the payload either.
                break;
            }
            out.push((line - start, page + (line - body_at)));
        }
        out
    }

    /// The turn's in-flight work, as this head currently knows it - one function, so the
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

    /// The newest transcript row with a payload to page: a tool result with more than a line or two. Newest
    /// because that is the row the reader is looking at, and the only one a head with no cursor can address;
    /// every other long row's seam names `/t`. `None` means the chord opens nothing.
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

    /// Scroll back by `by` lines, rendering whatever that needs: the fill happens before the scroll is
    /// clamped and asks for a screen beyond where the reader is going, since a key that scrolls is a key
    /// that renders. `body_len` is not the clamp, being the last frame's total.
    pub(crate) fn scroll_up(&mut self, by: usize) {
        // A change of direction is a change of intent: the run a flick was counting is over the moment the
        // reader turns around. Every way of going up means the same thing.
        self.wheel_run = 0;
        if self.hist_floor > 0 {
            // A screen past where the reader is going, so the next press has rows to move into. In
            // lines above the reader, which is what the fill counts in.
            self.fill_backward(by + self.screen_rows + TAIL_SLACK);
        }
        self.hold(-(by as isize));
    }

    /// What the reader is looking at, as the last frame drew it: the anchored row's line, or the top of the
    /// last frame's window when nothing is anchored yet. One function, so the two answers cannot disagree.
    pub(crate) fn held_line(&self) -> usize {
        self.anchor
            .as_ref()
            .and_then(|h| self.span_for(&h.item_id).map(|s| s.at + h.into))
            .unwrap_or(self.view_top)
    }

    /// Render what a held viewport needs before anything asks where it is: the tail bootstrap, then the held
    /// row. A key needs it because in tail mode every arriving row throws the history away, so a notch
    /// between two frames finds no spans and moves nothing.
    pub(crate) fn render_held(&mut self, room: usize) {
        if self.hist_floor == 0
            && self.hist_lines.is_empty()
            && transcript_bytes(&self.items) > self.walk_limit
        {
            self.hist_floor = self.items.len();
            self.fill_backward(room + TAIL_SLACK);
        }
        // Before the anchor is asked for, since a reader at the end of the buffer is the one whose
        // next step down has nothing drawn under it yet.
        self.release_below(room);
        if let Some(h) = self.anchor.clone() {
            let drawn = self.span_for(&h.item_id).is_some();
            if !drawn && self.items.iter().any(|it| it.item_id == h.item_id) {
                self.fill_to_row(h.ordinal);
            }
        }
    }

    /// How many rows this session has, as far as this head can account for them: everything it let
    /// go above the window, the window itself, and everything it let go below.
    pub(crate) fn rows_in_session(&self) -> usize {
        self.end_ordinal() + self.items_below
    }

    /// The ordinal of the first row this head does not hold. The rows above the base are the tail of
    /// a transcript a resume republished, and the base does not number them, so they do not count.
    pub(crate) fn end_ordinal(&self) -> usize {
        self.items_base + self.items.len().saturating_sub(self.items_lead)
    }

    /// The head holds a window, not the buffer: once it is over `held_limit` the rows the reader is not looking at
    /// go, and the window is rendered again from what is left. The lines go with them, so a re-render costs one window
    /// and saves the history.
    pub(crate) fn trim_held(&mut self, room: usize) {
        if transcript_bytes(&self.items) <= self.held_limit {
            return;
        }
        // The rows below a parked reader go first: they are not what the reader is looking at,
        // so letting them go is what keeps the held row on screen and the window under its bound.
        self.trim_held_below(room);
        let over = transcript_bytes(&self.items).saturating_sub(self.held_limit);
        if over == 0 {
            return;
        }
        // Then the rows above, never the one the reader is looking at or anything below it: a window
        // that dropped the held row would move the page under them.
        let wanted = self.rows_freeing(over);
        let capped = match &self.anchor {
            Some(h) => wanted.min(h.ordinal),
            None => wanted,
        };
        if capped > 0 {
            self.drop_rows_above(capped, room);
        }
        // And the bound still wins. A reader parked on the oldest row this head keeps left
        // nothing above them to free, so the trim takes their row too and `drop_rows_above`
        // re-points at the oldest survivor rather than holding the whole session for the head's life.
        if wanted > capped && transcript_bytes(&self.items) > self.held_limit {
            self.drop_rows_above(wanted - capped, room);
        }
    }

    /// How many of the oldest rows it takes to free `over` bytes of row text.
    fn rows_freeing(&self, over: usize) -> usize {
        let (mut freed, mut n) = (0, 0);
        while freed < over && n < self.items.len() {
            freed += self.items[n].item.as_ref().map_or(0, |i| i.bytes());
            n += 1;
        }
        n
    }

    /// And the other end of the same window. A reader parked above the tail is not reading what arrives below, so
    /// over the limit with nothing left above the held row, those rows go. They go with a margin, since each
    /// re-render of the window costs a screen.
    fn trim_held_below(&mut self, room: usize) {
        let left = transcript_bytes(&self.items);
        let Some(held) = self.anchor.as_ref().map(|h| h.ordinal) else {
            return;
        };
        if left <= self.held_limit {
            return;
        }
        let over = left.saturating_sub(self.held_limit / 2);
        // A screen of rows stays below the reader: a window that freed everything under them would
        // be one row tall, and everything else is a page away in the direction the session runs.
        let keep = (held + 1 + room).min(self.items.len());
        let (mut freed, mut end) = (0, self.items.len());
        while end > keep && freed < over {
            end -= 1;
            freed += self.items[end].item.as_ref().map_or(0, |i| i.bytes());
        }
        if end < self.items.len() {
            self.drop_rows_below(self.items.len() - end, room);
        }
    }

    /// Let go of the `n` oldest rows, and of everything drawn for them. Every row this head keeps is
    /// still one contiguous run, so the page a reader asks for next is `items_base`.
    fn drop_rows_above(&mut self, n: usize, room: usize) {
        // The reader's own row, when the trim takes it: the anchor names a row that is about to be
        // gone, and the nearest surviving one is the oldest this head keeps.
        let held_gone = self.anchor.as_ref().is_some_and(|h| h.ordinal < n);
        self.items.drain(..n);
        // A row above the base is history no ordinal names, so letting it go does not move the
        // base: only the base's own rows that went count.
        self.items_base += n.saturating_sub(self.items_lead);
        self.items_lead = self.items_lead.saturating_sub(n);
        if let Some(h) = &mut self.anchor {
            h.ordinal = h.ordinal.saturating_sub(n);
        }
        if held_gone {
            self.anchor = self.items.first().map(|first| Held {
                item_id: first.item_id.clone(),
                ordinal: 0,
                into: 0,
            });
            self.say(
                "the row you were reading is above the window this head keeps — the view is \
                 holding the oldest row it still has",
            );
        }
        // The seam a note was filed at is a row index, and the rows it named are gone: a note above
        // the window becomes one the head no longer draws, which is the rule a fork's seams follow.
        for (place, _) in &mut self.notes {
            if let Placed::Seam(at) = place {
                *place = if *at >= n {
                    Placed::Seam(*at - n)
                } else {
                    Placed::Before
                };
            }
        }
        // The rendered buffer describes rows this head no longer has, so it goes whole and the
        // window is drawn again from the end. One window's work, once per trim.
        self.prune_to_the_rows();
        self.invalidate_history_from(0);
        if self.items.is_empty() {
            return;
        }
        let stop = self.anchor.as_ref().map(|h| h.ordinal);
        self.hist_floor = self.items.len();
        self.fill_backward_until(room + TAIL_SLACK, stop);
    }

    /// The rows below the window, let go: nothing this head draws is of rows it no longer holds,
    /// so the history is rendered again from the end back to the reader's row.
    fn drop_rows_below(&mut self, m: usize, room: usize) {
        let keep = self.items.len().saturating_sub(m);
        if keep == 0 {
            return;
        }
        self.items.truncate(keep);
        self.items_below += m;
        // A note filed past the window is one the head no longer draws, the rule a fork's seams
        // follow: reachable through `/notes`, which is where a note off the screen is read. `keep`
        // is the first index that went, so a seam AT it names a row this head no longer holds.
        for (place, _) in &mut self.notes {
            if matches!(place, Placed::Seam(at) if *at >= keep) {
                *place = Placed::Before;
            }
        }
        // A row that left takes its call facts with it, the rule the front of the window
        // follows: the maps the renderer reads are keyed by row, and nothing else bounds them.
        self.prune_to_the_rows();
        self.invalidate_history_from(0);
        let stop = self.anchor.as_ref().map(|h| h.ordinal);
        self.hist_floor = self.items.len();
        self.fill_backward_until(room + TAIL_SLACK, stop);
    }

    /// The rendered history is a window too: over `hist_limit` the rows at whichever end is farther from the reader
    /// go, and the walk draws them again on return. Nothing is asked of the daemon - the row is still in `items` -
    /// and the bound is bytes, because a rendered line is what the head's memory is.
    pub(crate) fn trim_history(&mut self, room: usize) {
        // An ordinary session walks from row 0 and is bounded by `walk_limit`, and a head that still
        // holds a rewind table has one indexed by absolute row that a cut would leave pointing at
        // the wrong place. Only a tail-mode head holds lines its reader can leave behind.
        if self.hist_floor == 0 || !self.hist_marks.is_empty() {
            return;
        }
        let over = self
            .hist_bytes()
            .saturating_sub(self.hist_limit.saturating_sub(self.hist_limit / 4));
        if over == 0 {
            return;
        }
        let line = self.held_line().min(self.hist_lines.len());
        // A screen on each side of the reader is never let go: the window has to be drawable, and a
        // bound smaller than that is a bound the head cannot keep.
        let above = line.saturating_sub(room + TAIL_SLACK);
        let below = line + room + 2 * TAIL_SLACK;
        // The end farther from the reader pays first, since it is the one nobody is reading.
        let ends = if self.hist_lines.len().saturating_sub(line + room) >= line {
            [(true, below), (false, above)]
        } else {
            [(false, above), (true, below)]
        };
        let mut left = over;
        for (from_below, keep) in ends {
            if left == 0 {
                break;
            }
            left = left.saturating_sub(self.let_lines_go(from_below, keep, left));
        }
    }

    /// How many bytes the rendered lines are: the size this head pays for the transcript on screen.
    pub(crate) fn hist_bytes(&self) -> usize {
        self.hist_lines.iter().map(String::len).sum::<usize>() + self.hist_lines.len()
    }

    /// Let rendered rows go from one end of the buffer until `need` bytes are freed or `keep` (a line
    /// index) would be crossed. Returns the bytes freed. Rows, not lines: a half row on the screen is
    /// a row cut in two.
    pub(crate) fn let_lines_go(&mut self, from_below: bool, keep: usize, need: usize) -> usize {
        if from_below {
            let (mut freed, mut n) = (0, self.spans.len());
            while n > 0 {
                let sp = self.spans[n - 1];
                if sp.at < keep || freed >= need {
                    break;
                }
                freed += self.hist_lines[sp.at..sp.at + sp.lines]
                    .iter()
                    .map(|l| l.len() + 1)
                    .sum::<usize>()
                    + 1;
                n -= 1;
            }
            if n == self.spans.len() {
                return 0;
            }
            if n == 0 {
                // Every row is below the lines the reader needs, which cannot happen while their own
                // row is in the buffer; a cut here would empty the window rather than trim it.
                return 0;
            }
            // The cut is the first line of the first row let go - its separator goes with it - and
            // the row below it is the one the reader's next step down draws again.
            let cut = self.spans[n].at;
            let first_gone = self.spans[n].row;
            self.hist_lines.truncate(cut);
            self.spans.truncate(n);
            self.note_cards.retain(|c| c.at < cut);
            self.hist_upto = first_gone;
            self.hist_ceiling = Some(first_gone);
            self.hist_class = self.spans.last().map(|s| s.class);
            // The notes those rows drew are drawn again with them.
            self.note_upto = self
                .notes
                .iter()
                .position(|(p, _)| !matches!(p, Placed::Seam(at) if *at < first_gone))
                .unwrap_or(self.notes.len());
            return freed;
        }
        let (mut freed, mut n) = (0, 0);
        while let Some(sp) = self.spans.get(n).copied() {
            if sp.at + sp.lines > keep || freed >= need {
                break;
            }
            freed += self.hist_lines[sp.at..sp.at + sp.lines]
                .iter()
                .map(|l| l.len() + 1)
                .sum::<usize>()
                + 1;
            n += 1;
        }
        if n == 0 {
            return 0;
        }
        // Every row fits above `keep`: nothing is left to cut at.
        let Some(kept) = self.spans.get(n).copied() else {
            return 0;
        };
        let cut = kept.at;
        self.hist_lines.drain(..cut);
        self.spans.drain(..n);
        for sp in &mut self.spans {
            sp.at -= cut;
        }
        // The cards are filed by line, so the ones above the cut move up with it and the ones the
        // dropped rows drew go with them.
        self.note_cards.retain(|c| c.at >= cut);
        for c in &mut self.note_cards {
            c.at -= cut;
        }
        self.hist_floor = kept.row;
        self.hist_first_class = Some(kept.class);
        freed
    }

    /// The window may need the rows below it: a reader who has come down to the end of what this head drew gets the
    /// next screen rendered on this frame, and the walk stops again at its end. The ceiling stays until that walk
    /// has run, so a key handler asking *are there rows below* is answered.
    pub(crate) fn release_below(&mut self, room: usize) {
        if self.hist_ceiling.is_none() || self.anchor.is_none() {
            return;
        }
        if self.held_line() + room + TAIL_SLACK < self.hist_lines.len() {
            return;
        }
        self.walk_ahead = room + TAIL_SLACK;
    }

    /// Back to the end of the session. A head that follows the tail must show its newest row, and
    /// a window that stopped short of it would draw a row from however long ago as the newest. One
    /// window's work, once, and only for a reader who came back down to the tail.
    pub(crate) fn reanchor_at_tail(&mut self, room: usize) {
        self.invalidate_history_from(0);
        self.hist_floor = self.items.len();
        self.fill_backward(room + TAIL_SLACK);
    }

    /// Ask the daemon for the rows above the window, when the reader has scrolled past the oldest
    /// one this head holds. One ask in flight and none once the daemon has answered that there is
    /// nothing above, so a held key is pages and not a flood.
    pub(crate) fn ask_for_rows_above(&mut self) {
        if self.hist_floor > 0
            || self.items_base == 0
            || self.fetch_asked.is_some()
            || self.no_more_above
        {
            return;
        }
        let count = self.items_base.min(PAGE_ROWS);
        let from = self.items_base - count;
        self.fetch_asked = Some(from);
        self.queued.push(Action::FetchRows {
            session: self.session_id.clone(),
            from,
            count,
        });
    }

    /// The same ask in the other direction: the reader is at the end of what this head holds and
    /// the session went on below it, so the tail is behind a page. One in flight, and none at all
    /// once the daemon answers that nothing is at that ordinal.
    pub(crate) fn ask_for_rows_below(&mut self) {
        if self.items_below == 0 || self.anchor.is_some() || self.fetch_below.is_some() {
            return;
        }
        let from = self.end_ordinal();
        let count = self.items_below.min(PAGE_ROWS);
        self.fetch_below = Some(from);
        self.queued.push(Action::FetchRows {
            session: self.session_id.clone(),
            from,
            count,
        });
    }

    /// A page of rows the daemon answered, placed on the end of the window it asked about. Every
    /// row this head holds is one contiguous run of the session, so a page lands where it joins
    /// that run or not at all, and a page that does not join is read in the direction it names.
    pub(crate) fn apply_rows(&mut self, from: usize, rows: Vec<SnapshotItem>) {
        let end = self.end_ordinal();
        // A page below the window is the tail the reader is coming back down to: it joins the
        // rows where they end, and what is above it is unchanged, so no index moves.
        if from == end {
            if rows.is_empty() {
                // Nothing at that ordinal, so the session ends where this window does and the
                // count the head was holding was stale. The next arriving row is pushed normally.
                self.items_below = 0;
                return;
            }
            let n = rows.len();
            self.items.extend(rows);
            // A row appended while the page was in flight is one this head still does not hold.
            self.items_below = self.items_below.saturating_sub(n);
            return;
        }
        // A page above: the ask's answer joins the window at its front, or nowhere.
        if !rows.is_empty() && from.saturating_add(rows.len()) == self.items_base {
            let n = rows.len();
            let lead = self.items_lead;
            // The page lands where the base begins, which is not the front of the list when a
            // resume republished the transcripts a compaction put behind it: those rows are above
            // the base, and a page of the base belongs under them.
            self.items.splice(lead..lead, rows);
            self.items_lead += n;
            self.items_base = from;
            // Everything indexed by row moves down by the page, and the anchor is a row: without
            // this the reader's held row would be a page's worth of rows away from their screen.
            // The rows ABOVE the base do not move.
            let shift = |at: &mut usize| {
                if *at >= lead {
                    *at += n;
                }
            };
            shift(&mut self.hist_floor);
            shift(&mut self.hist_upto);
            if let Some(end) = &mut self.hist_ceiling {
                shift(end);
            }
            for s in &mut self.spans {
                shift(&mut s.row);
            }
            if let Some(h) = &mut self.anchor {
                shift(&mut h.ordinal);
            }
            for (place, _) in &mut self.notes {
                if let Placed::Seam(at) = place {
                    shift(at);
                }
            }
            // The marks are a row-indexed rewind table and a page above them moves every index it
            // holds. A head that has them walked from row 0, so it had no rows to be given; the
            // rebuild is here so that a page landing on one is not left half-shifted.
            if !self.hist_marks.is_empty() {
                self.invalidate_history_from(0);
                self.hist_upto = 0;
            }
            return;
        }
        // The page was empty or short, so what is above cannot be reached: empty means nothing is above, short means a
        // row could not be read and every later page would be short too. Splicing leaves a hole, so it is dropped
        // and said. A page that starts past the window is the tail's and says nothing about above.
        if from >= end {
            // The count below was wrong or a row of the tail cannot be read: asking again would
            // fetch the same hole for ever, so the count goes and the live tail resumes from here.
            self.items_below = 0;
            self.say(
                "the page below this window did not join it: a row in it could not be read, so \
                 the rows between this window and the tail are lost",
            );
            return;
        }
        self.no_more_above = true;
        self.say(if rows.is_empty() {
            "nothing is above this window — the daemon holds no rows above the oldest one this \
             head has, so the transcript stops here"
        } else {
            "the page above this window did not join it: a row in it could not be read, so the \
             transcript above stops here"
        });
    }

    /// Move the held viewport by `delta` lines, staying on a row: the line-to-row conversion happens here and
    /// nowhere else. Reaching the bottom returns the reader to following, which is an act they took, while
    /// arriving content never does.
    pub(crate) fn hold(&mut self, delta: isize) {
        // The row being held must be drawable before the hold can move off it - or the
        // step is spent on a span table an arriving row just wiped, which is the pin this
        // answers. See [`App::render_held`].
        self.render_held(self.view_room.max(1));
        // The count is kept in step with the hold and it is derived: the frame recomputes it from the anchor,
        // since only the frame knows how many lines the body has. A reader of `scroll` between a key and a
        // paint sees the position the hold implies rather than a stale zero.
        let now = self.held_line();
        let want = if delta < 0 {
            now.saturating_sub(delta.unsigned_abs())
        } else {
            now.saturating_add(delta as usize)
        };
        let bottom = self.body_len.saturating_sub(self.view_room.max(1));
        let stall_from = (delta > 0).then_some(now);
        // The bottom of the window is not the bottom of the session. With rows below it this head
        // has not drawn, reaching the last drawn line is a page and not a return to following.
        if want >= bottom && delta > 0 && self.hist_ceiling.is_none() {
            self.anchor = None;
            self.scroll = 0;
            // No `redraw` here either - this is the flag's second home on the scroll path.
            // See the note at the foot of this function: a moved viewport is a diff, not an
            // erase.
            return;
        }
        // The top of the transcript is as far as this goes, and holding there is not
        // following: a reader at the very top of a long session is reading the beginning,
        // and content arriving below must not drag them down to it.
        let want = want.min(bottom.max(1));
        match self.span_at_line(want) {
            Some(span) => {
                // `into` may be `lines`, one past the row's last line, and it must be: the blank a separator
                // puts between two rows belongs to no row, and clamping sent a notch that landed on one back up
                // a line, so two three-line notches moved eight lines rather than six.
                let into = want.saturating_sub(span.at).min(span.lines);
                self.anchor = Some(Held {
                    item_id: self.items[span.row].item_id.clone(),
                    ordinal: span.row,
                    into,
                });
            }
            // Above the first rendered row: either the reader has gone past what this head drew, or nothing has
            // been drawn. Holding the topmost row at offset 0 is the closest true thing and what a second press
            // scrolls from.
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
        // `scroll` is left alone: it is derived by the frame from the anchor, and a second derivation would
        // be two implementations of one formula. Anything asking whether the reader is at the bottom asks
        // [`App::following`].
        if let Some(was) = stall_from
            && self.anchor.is_some()
            && self.held_line() <= was
        {
            self.note_scroll_stall(was, want, bottom);
        }
    }

    /// A step down that did not move a reader who is not following: the stop above the tail that
    /// only Esc frees, seen in long sessions and not reproduced. One line per stop, to a file the
    /// operator can send, naming the window's state at it.
    fn note_scroll_stall(&self, held: usize, want: usize, bottom: usize) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static LAST: AtomicUsize = AtomicUsize::new(usize::MAX);
        if cfg!(test) || LAST.swap(held, Ordering::Relaxed) == held {
            return;
        }
        let Some(home) = std::env::var_os("HOME") else {
            return;
        };
        let dir = std::path::Path::new(&home).join(".local/state/letibot");
        let _ = std::fs::create_dir_all(&dir);
        let line = format!(
            "{} session={} items={} floor={} upto={} ceiling={:?} below={} body_len={} room={} \
             held={held} want={want} bottom={bottom} walk_ahead={} spans={}\n",
            self.now_ms,
            self.session_id,
            self.items.len(),
            self.hist_floor,
            self.hist_upto,
            self.hist_ceiling,
            self.items_below,
            self.body_len,
            self.view_room,
            self.walk_ahead,
            self.spans.len(),
        );
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("scroll-stalls.log"))
        {
            let _ = f.write_all(line.as_bytes());
        }
    }

    /// One notch down walks three lines; a run of notches gathers speed, but only against a stream still
    /// arriving. A notch that cleared the anchor made the conversation between two positions unreachable, and
    /// three lines a notch is outrun by arriving rows, so the step doubles while the transcript moves.
    pub(crate) fn wheel_down_notch(&mut self) -> usize {
        // The step is measured on a live span table, since the wipe an arriving row leaves would hand it a
        // stale `view_top` and a stale `body_len`. Rendering here is idempotent.
        self.render_held(self.view_room.max(1));
        let now = self.held_line();
        let bottom = self.body_len.saturating_sub(self.view_room.max(1));
        let dist = bottom.saturating_sub(now);
        let fresh =
            self.anchor.is_none() || self.now_ms.saturating_sub(self.wheel_last_ms) > WHEEL_RUN_MS;
        if fresh {
            self.wheel_run = 0;
        }
        self.wheel_last_ms = self.now_ms;
        // The one fact that separates the walk from the run: did rows arrive since the last notch down? A still
        // transcript is walked whatever the distance. The session's count is used, not this head's window, which
        // stops growing once the window is what rows land in.
        let stream = self.rows_in_session() != self.wheel_items;
        self.wheel_items = self.rows_in_session();
        self.wheel_run = self.wheel_run.saturating_add(1);
        if !stream {
            return 3;
        }
        // 3, 6, 12, 24 … capped at 192 - bigger than any glass, so the cap is about
        // sanity rather than reach - and never past the bottom: the step that lands
        // on the tail is the step that follows it, and `hold` treats it as arrival.
        (3usize << (self.wheel_run - 1).min(6)).min(dist.max(3))
    }

    /// Draw rows until one of them is rendered: `fill_backward` covers enough lines, which is the wrong
    /// question when the head must find a specific row, since a fill measured in lines can stop short of the
    /// very row the viewport holds and leave the anchor unresolvable.
    pub(crate) fn fill_to_row(&mut self, row: usize) {
        if self.hist_floor > row {
            self.fill_backward_until(self.screen_rows + TAIL_SLACK, Some(row));
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

    /// The words of the row the reader is holding, which is what [`Carry`] is taken from. `None` when
    /// nothing is held, when the row is not in `items`, and when it carries no words at all.
    pub(crate) fn held_row_words(&self) -> Option<String> {
        let held = self.anchor.as_ref()?;
        let row = self.items.iter().find(|r| r.item_id == held.item_id)?;
        row_words(row)
    }

    /// The row a held viewport was on is not in this transcript: find it again by its own words,
    /// or say it is gone and go to the tail, the one place true of the whole new transcript. An
    /// ordinal names a place in the list just replaced, and not matchable yet is not gone.
    pub(crate) fn repair_anchor(&mut self) {
        let Some(held) = self.anchor.clone() else {
            self.carry = None;
            return;
        };
        // The row is on the screen or in the transcript under its own id, so the replacement did not take
        // it. The carry is spent either way, since a surviving id needed no boundary crossed.
        if self.span_for(&held.item_id).is_some() {
            self.carry = None;
            return;
        }
        // A row that is simply not rendered yet is not a row that is gone: in tail mode a held row can be in
        // `items` and absent from `spans`, and calling it gone would fire on every scroll in a long session.
        if self.items.iter().any(|it| it.item_id == held.item_id) {
            self.carry = None;
            return;
        }
        // The id is gone, so this is a transcript boundary: the row's words are the one thing both
        // transcripts can be asked for, and the first row that says them is the one the reader was on.
        // Borrowed rather than cloned, since this runs every frame until the row is placed.
        let found = self
            .carry
            .as_ref()
            .and_then(|c| c.words.as_deref())
            .and_then(|words| self.items.iter().position(|it| row_says(it, words)));
        if let Some(row) = found {
            self.anchor = Some(Held {
                item_id: self.items[row].item_id.clone(),
                ordinal: row,
                into: held.into,
            });
            self.carry = None;
            // No `redraw`. A moved viewport is a diff rather than an erase - the same
            // rule the scroll keys keep - and this is the head moving a reader who did not
            // move: the frame that follows writes only the rows that changed.
            return;
        }
        // Not matchable yet is not gone: a snapshot taken mid-carry holds rows whose bodies are still
        // coming, so the anchor waits and the frame holds the line.
        if self.bulk.is_some() {
            return;
        }
        // The live half of the same rule, where there is nothing to wait on: a fork stated before its rows
        // has no announcement to be complete against, so such a carry is placed and never concluded.
        if self.carry.as_ref().is_some_and(|c| c.arriving) {
            return;
        }
        // Not carried. The tail is the only place left that is true, and the reader is
        // told: a move made in silence is the defect this whole path exists to refuse.
        self.anchor = None;
        let said = match self.carry.take().and_then(|c| c.words) {
            Some(_) => format!(
                "the row you were reading is not in the transcript that replaced it: the rows \
                 were replaced (a fork — a compaction, a re-seat or a resync) and `{}` was not \
                 carried across. The view follows the tail again rather than holding a place \
                 the new transcript does not have.",
                held.item_id
            ),
            // The row had no body when the rows went, so there were no words to look for.
            // Said as that rather than as *the row is gone*, which is a different claim.
            None => "the row you were reading could not be found in the transcript that \
                     replaced it: its body had not arrived, so there were no words to match, \
                     and the rows were replaced (a fork — a compaction, a re-seat or a \
                     resync). The view follows the tail again."
                .to_string(),
        };
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
            // And said where the reader is certainly looking: the note sits at the seam at the end of the
            // replacement, which is where a reader holding a row near the top is not. The transient line above
            // the composer is on screen whatever the viewport shows.
            self.say(&format!(
                "the row you were reading is not in the transcript that replaced it — the \
                 view follows the tail again (row `{}`)",
                held.item_id
            ));
        }
    }

    /// How the viewport's state reads on screen: `None` when the head is following, since a marker always on
    /// is furniture, and `Some` when it is holding, naming the act that returns them.
    pub fn scroll_state(&self) -> Option<&'static str> {
        (!self.following()).then_some("holding")
    }

    /// Whether the row is drawn is the set's question, and the name on the status row is the set's name: the
    /// profile when the set is one, and `custom …` when a switch has moved.
    pub fn rung_state(&self) -> Option<String> {
        let v = self.visibility;
        (v.hides_the_working() || v.profile().is_none()).then(|| v.as_str())
    }

    /// Does this rung draw this row as a row - the one place the question is asked about a row
    /// rather than an item. A row with no body yet is drawn from this head's own echo. Hidden no
    /// longer means absent: a hidden row's run draws one marker line, and both walks ask this.
    pub fn hidden_by_rung(&self, row: usize) -> bool {
        row_hidden(&self.items, self.visibility, row)
    }

    /// Move a held viewport off a row this rung hides, to the nearest surviving row outward from
    /// where the reader was, and say so rather than jumping. The sentence is the shape
    /// `repair_anchor` writes for a row a replacement took away.
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
                self.say(
                    "the row you were reading is one this rung hides — the view is holding the \
                     nearest row that still shows. `/verbosity` brings the working back, and `esc` \
                     follows the stream again",
                );
            }
            None => {
                // Nothing survives to hold on to: a transcript with no conversation in it at
                // all. Following is the only true answer, and the rung is on screen saying
                // why the screen is empty.
                self.anchor = None;
                self.say("this rung hides every row here, so there is nothing to hold a place in");
            }
        }
    }

    /// Whether the viewport is following the stream, and the thing the screen has to say out loud:
    /// a reader who cannot tell pinned from following scrolls to find out, which is the affordance
    /// failing. See [`App::scroll_state`].
    pub fn following(&self) -> bool {
        self.anchor.is_none()
    }

    /// The row of the run the live work belongs to - what a change in the counts invalidates. It
    /// has to agree with `newest_unseen_run`, or the count is rebuilt on one row while the marker
    /// is drawn on another.
    pub(crate) fn newest_run_row(&self, live: &LiveWork) -> Option<usize> {
        newest_unseen_run(&self.runs_index(), *live)
    }
}

/// The call ids answered by a result row in this round: the rows between the assistant row at `at`
/// and the next assistant row. Bounded by the round because `call_0` is reused every round. A row
/// whose body has not arrived counts as unanswered; `record_item` rebuilds when it lands.
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
            // Only the next assistant row ends a round. A USER row does not: a message sent while
            // the calls run is appended between the calls and their results, and a user cannot
            // produce a tool result, so what follows still answers the calls above.
            Some(TranscriptItem::Assistant { .. }) => break,
            // An announcement with no body yet, or a reasoning row between the
            // calls and their results. Neither ends the round.
            _ if it.kind == "assistant" => break,
            _ => {}
        }
    }
    out
}

/// How much transcript a head will walk from the beginning before it renders the tail instead. The
/// walk is ~40 ms for 6000 rows, so 2 MB is well under a frame's budget and the incremental marks
/// it buys are worth having; above it the only affordable thing is the end.
pub(crate) const SELF_WALK_LIMIT: usize = 2 * 1024 * 1024;

/// How many bytes of row text a head holds before it lets the oldest rows above the window go. The
/// daemon bounds its own view at the same number, and the two are not the same budget: this one is
/// the head's copy, and without it a session that runs for hours grows the head with it.
pub(crate) const HELD_LIMIT: usize = 8 * 1024 * 1024;

/// How many bytes of rendered lines `hist_lines` keeps. Rendering costs about three times the source
/// it came from, so this is the head's counterpart to `HELD_LIMIT` and not a second history: over it,
/// the rows farthest from the reader go and are drawn again when the reader comes back to them.
pub(crate) const HIST_LIMIT: usize = 3 * HELD_LIMIT;

/// How many rows one page asks the daemon for. A page is what a screenful of scrolling consumes
/// several times over, so a reader holding a key is not one round trip per screen, and small enough
/// that a reader who paged too far did not pay for a transcript.
pub(crate) const PAGE_ROWS: usize = 100;

/// How many rows above the rendered window to keep, so a frame can be drawn while the reader is a
/// little way up - the window plus one screen. Not a scrollback budget: scrolling further
/// back-fills more (see `App::fill_backward`).
pub(crate) const TAIL_SLACK: usize = 40;

/// How long a wheel notch remembers the one before it: notches closer together are one flick, and
/// a flick is an intent repeated; further apart is a reader stepping, and the step between them is
/// a walk. The driver hands one tick whatever the terminal buffered, so a flick lands in bursts.
pub(crate) const WHEEL_RUN_MS: u64 = 200;

/// Roughly how many bytes of text the transcript carries: a sum of `TranscriptItem::bytes`, the one
/// definition, because the daemon bounds its view by the same function. Cheap on purpose, since it
/// runs every frame; a row with no body yet counts as zero, which errs toward walking.
pub(crate) fn transcript_bytes(items: &[SnapshotItem]) -> usize {
    items
        .iter()
        .map(|it| it.item.as_ref().map(|i| i.bytes()).unwrap_or(0))
        .sum()
}

/// The tool-call targets in force for row `k`: the calls of the nearest assistant row above it that
/// has a body. Deriving it is what lets the BACKWARD walk render a row without having rendered
/// everything above it first; the forward walk builds the same thing as it goes.
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

/// What the reader is holding their viewport on: the row's id and not a line count, because a count
/// is what every arrival and re-wrap invalidates. An id names a row inside ONE transcript, so what
/// survives a fork is its words ([`Carry`]); `ordinal` is used within one transcript only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Held {
    pub(crate) item_id: String,
    pub(crate) ordinal: usize,
    /// Lines into the row's own rendering. Bounded to the row's height when it is used, so
    /// a row that shrank under the anchor does not push the view past its own end.
    pub(crate) into: usize,
}

/// Where the reader was when a snapshot replaced the rows under them: `words`, the row's own
/// content, which a fork carries across unchanged, and `line`, the body line in the buffer that
/// just went. Both doors take one - `App::load` for a snapshot, `rows_replaced` for a fork.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Carry {
    pub(crate) words: Option<String>,
    pub(crate) line: usize,
    /// Whether the rows that replace these are still to come. Stated before its rows there is no
    /// signal that the carry is complete, so it is placed and never concluded: saying the row is
    /// gone about a row that has not arrived is the one thing this must not do.
    pub(crate) arriving: bool,
}

/// A row's own words, what [`Carry::words`] is taken from: the engine's JOIN for a `User` row, the
/// text for prose, the payload for a result, the label for a mark. `None` for a row that says
/// nothing, because an empty string would match the first empty row of the next transcript.
pub(crate) fn row_words(it: &SnapshotItem) -> Option<String> {
    match it.item.as_ref()? {
        TranscriptItem::System { text, .. }
        | TranscriptItem::Notice { text, .. }
        | TranscriptItem::Assistant { text, .. }
        | TranscriptItem::Reasoning { text, .. } => (!text.is_empty()).then(|| text.clone()),
        TranscriptItem::ToolResult { payload, .. } => {
            (!payload.is_empty()).then(|| payload.clone())
        }
        TranscriptItem::SegmentMark { label, .. } => (!label.is_empty()).then(|| label.clone()),
        TranscriptItem::Offer { title, .. } => Some(title.clone()),
        TranscriptItem::User { parts, .. } => {
            let joined = parts
                .iter()
                .filter_map(text_part)
                .collect::<Vec<_>>()
                .join("\n");
            (!joined.is_empty()).then_some(joined)
        }
    }
}

/// Whether this row says exactly `words`, asked of every row in the transcript that replaced the one
/// it was taken from. An empty `words` is never a match: a row that says nothing gives the carry
/// nothing to find by, and `""` would match the first such row.
pub(crate) fn row_says(it: &SnapshotItem, words: &str) -> bool {
    if words.is_empty() {
        return false;
    }
    match it.item.as_ref() {
        None => false,
        Some(TranscriptItem::System { text, .. })
        | Some(TranscriptItem::Notice { text, .. })
        | Some(TranscriptItem::Assistant { text, .. })
        | Some(TranscriptItem::Reasoning { text, .. }) => text == words,
        Some(TranscriptItem::ToolResult { payload, .. }) => payload == words,
        Some(TranscriptItem::SegmentMark { label, .. }) => label == words,
        Some(TranscriptItem::Offer { title, .. }) => title == words,
        Some(TranscriptItem::User { parts, .. }) => user_says(parts, words),
    }
}

/// The `User` half of [`row_says`]: the text parts newline-joined, compared in place
/// rather than built, because a row's text is not bounded by anything this loop needs.
fn user_says(parts: &[UserPart], words: &str) -> bool {
    let mut rest = words;
    let mut any = false;
    for text in parts.iter().filter_map(text_part) {
        if any {
            match rest.strip_prefix('\n') {
                Some(r) => rest = r,
                None => return false,
            }
        }
        match rest.strip_prefix(text) {
            Some(r) => rest = r,
            None => return false,
        }
        any = true;
    }
    any && rest.is_empty()
}

/// One `User` row's text piece, if it has one - the same reading [`App::load`] and
/// [`App::record_item`] make when they retire a queued echo.
fn text_part(p: &UserPart) -> Option<&str> {
    match p {
        UserPart::Text { text } => Some(text),
        _ => None,
    }
}

/// Where the history walk stood before one row. See [`App::hist_marks`]. Three fields because the
/// walk carries three cursors; `call_targets` is derivable from the rows above
/// ([`App::retarget_before`]), and a map per row would be the cache growing with the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HistMark {
    /// `hist_lines.len()` before the row was drawn.
    pub(crate) lines: usize,
    /// `note_upto` before the row was drawn.
    pub(crate) note_upto: usize,
    /// `hist_class` before the row was drawn - the separator's whole input.
    pub(crate) class: Option<RowClass>,
}

/// Where one rendered row's lines are: `at` is a line index into `hist_lines` and `row` is the
/// row's index in `items`, so one list answers both *which row is at line 400* and *where did row
/// 91 go*. A row that rendered to nothing has no span, which keeps that first question unambiguous.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Span {
    pub(crate) row: usize,
    pub(crate) at: usize,
    pub(crate) lines: usize,
    /// The class the walk drew it as. Kept per row because the ring cuts rows off both ends of the
    /// buffer, and the separator the walk owes the next row is the class of the last one kept.
    pub(crate) class: RowClass,
}
