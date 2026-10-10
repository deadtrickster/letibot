//! **Where the reader is**: the history buffer of drawn rows, the anchor a held view keeps,
//! following the bottom, and filling backwards when the reader scrolls past what was drawn.

use super::*;
use crate::ui::render::{RenderConfig, visible_width};
use letibot_sessionlog::view::{CallState, SnapshotItem, TurnState, Warned};
use letibot_transcript::{TranscriptItem, UserPart};

impl App {
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
        // **A change of direction is a change of intent** — the run a flick was counting
        // is over the moment the reader turns around, so the next flick starts at a walk.
        // Here rather than in the key handler, because every way of going up — the wheel,
        // the page keys, a pane handing its edge on — means the same thing: not this way.
        self.wheel_run = 0;
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

    /// **Render what a held viewport needs, before anything asks where it is** — the
    /// frame's own bootstrap in one place, so that a key can run it too (2026-10-09's
    /// *"stuck at 190 lines"*).
    ///
    /// Two steps, and the frame has always done both:
    ///
    /// * the **tail bootstrap** — a conversation too big to walk from the beginning is
    ///   rendered from its end, which is the frame's ordinary way into a big session;
    /// * **the held row, if this head has not drawn it** (R36): a tail walk renders the
    ///   last screenful and skips everything above it, so a viewport holding a row from
    ///   further up has no span and nothing to place itself against. Rendering down to
    ///   that row is the one thing that fixes it, and it is asked for by ROW rather than
    ///   by lines — a fill measured in lines can stop short of the very row being held,
    ///   which is a viewport chasing its own tail.
    ///
    /// **Why a key needs this and not only the frame.** In tail mode every arriving row
    /// invalidates the rendered history *wholesale* — a tail walk leaves no marks, so
    /// [`App::invalidate_history_from`] has nothing to rewind to and throws the lot away,
    /// correctly, because the rows above the floor were never rendered and there is
    /// nothing to be stale — and on a session whose turns keep arriving, that wipe sits
    /// *between two frames* more often than not. A wheel notch that lands there finds no
    /// spans at all: [`App::held_line`] falls back to a stale `view_top`,
    /// [`App::span_at_line`] answers `None`, and the notch's own `None` arm has nothing
    /// to anchor on either — so it moves **nothing**. Measured on the fixture below: the
    /// operator's park — 190 lines up, on the tail path, rows arriving — took thirty-two
    /// notches down and the window never moved a line while the count grew to 319. `esc`
    /// worked because it clears the anchor outright and never asks the spans anything.
    ///
    /// **A key that scrolls is a key that renders** — the rule [`App::scroll_up`] already
    /// keeps for going up, and the mirror of `fill_backward` for coming down. The frame's
    /// `room` is the frame's own; a key between frames uses the last frame's
    /// ([`App::view_room`]), which is R36's rule about the key handler answering from the
    /// glass.
    pub(crate) fn render_held(&mut self, room: usize) {
        if self.hist_floor == 0
            && self.hist_lines.is_empty()
            && transcript_bytes(&self.items) > self.walk_limit
        {
            self.hist_floor = self.items.len();
            self.fill_backward(room + TAIL_SLACK);
        }
        if let Some(h) = self.anchor.clone() {
            let drawn = self.span_for(&h.item_id).is_some();
            if !drawn && self.items.iter().any(|it| it.item_id == h.item_id) {
                self.fill_to_row(h.ordinal);
            }
        }
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
        // **The row being held must be drawable before the hold can move off it** — or the
        // step is spent on a span table an arriving row just wiped, which is the pin this
        // answers. See [`App::render_held`].
        self.render_held(self.view_room.max(1));
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

    /// **One notch down walks three lines; a RUN of notches gathers speed — but only
    /// against a stream that is still arriving.**
    ///
    /// This is the reconciliation of the wheel's three reports. 2026-10-05: *"I cant
    /// scroll back to bottom with a mouse wheel - have to press escape"* — answered by
    /// making one notch clear the anchor, which 2026-10-09 recoiled from: *"one simple
    /// stroke gets me to the bottom immediately — effectively like Esc"*, because a
    /// reader could no longer walk DOWN through a conversation. The answer then was the
    /// three-line walk, and it is right — on a transcript that is holding still.
    ///
    /// What it cannot do is cover a deep park against a stream that keeps arriving,
    /// which is where the operator sat: *"scrolled up and couldnt scroll back with mouse
    /// - stuck at 190 lines lol. Esc worked"*, and the narrowing — *"to trigger
    /// conversation must be scrolled up far enough"*. Two mechanisms measured on the
    /// failing fixture: a notch landing between an arrival and the next frame found the
    /// span table wiped and moved **nothing** (fixed in [`App::render_held`]); and with
    /// the spans live, three lines a notch is outrun by two arriving rows — the notch
    /// moved every time and the count still grew, 191 to 319 over thirty-two notches.
    ///
    /// So the walk and the run are separated by the one fact that makes the difference:
    /// **whether the transcript moved under the reader**. On a still transcript the
    /// notch is a walk — three lines, every notch, whatever came before — which is
    /// 2026-10-09's reconciliation entire, and its tests pin it. Against a stream the
    /// notch joins a run, and each further notch of the same run doubles the step:
    /// 3, 6, 12, 24 … — a stream adds rows at some finite pace and a doubling outgrows
    /// any fixed pace, so the tail is reachable in a bounded number of notches however
    /// fast it recedes, near the end included. The first notch of a run is three lines
    /// either way, so one stroke is never the tail — October's line, held exactly.
    ///
    /// The run ends three ways, all of them the reader's: they pause
    /// ([`WHEEL_RUN_MS`] — a notch spent reading is its own), they turn around
    /// ([`App::scroll_up`] resets the count — a change of direction is a change of
    /// intent), or they are back on the stream and the next park is a fresh question.
    pub(crate) fn wheel_down_notch(&mut self) -> usize {
        // **The distance this notch steps is measured on a live span table** — the wipe an
        // arriving row leaves behind ([`App::render_held`]) would otherwise hand the step a
        // stale `view_top` and a stale `body_len`, and the step it sizes from them is the
        // step that stalls four lines from the end for ever. Rendering here is idempotent:
        // `hold` runs the same call a moment later and finds everything drawn.
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
        // **The one fact that separates the walk from the run**: did rows arrive since
        // the last notch down? A still transcript is walked whatever the distance; a
        // living one is the case the walk was outrun by.
        let stream = self.items.len() != self.wheel_items;
        self.wheel_items = self.items.len();
        self.wheel_run = self.wheel_run.saturating_add(1);
        if !stream {
            return 3;
        }
        // **3, 6, 12, 24 … capped at 192** — bigger than any glass, so the cap is about
        // sanity rather than reach — **and never past the bottom**: the step that lands
        // on the tail is the step that follows it, and `hold` treats it as arrival.
        (3usize << (self.wheel_run - 1).min(6)).min(dist.max(3))
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

    /// **The words of the row the reader is holding** — what [`Carry`] is taken from.
    ///
    /// `None` when nothing is held, when the held row is not in `items`, and when the row
    /// carries no words at all: a row whose body has not arrived has nothing to be found
    /// by, and that is a fact the carry has to be told rather than guessed around.
    pub(crate) fn held_row_words(&self) -> Option<String> {
        let held = self.anchor.as_ref()?;
        let row = self.items.iter().find(|r| r.item_id == held.item_id)?;
        row_words(row)
    }

    /// **The row a held viewport was on is not in this transcript — find it again, or say
    /// that it is gone.**
    ///
    /// A `resync`, a snapshot on `hello`, a fork and a compaction all replace the rows
    /// wholesale, and a fork replaces them **under new ids** (`{transcript_id}.{n}`,
    /// `engine.rs`). So the id the viewport holds names nothing in the transcript that
    /// arrives, and the replacement is the only thing that moves a reader who has not
    /// touched a key.
    ///
    /// # The row is found by its own words, and never by its index
    ///
    /// [`Carry`] is the row's text, taken before the old rows went, and this is where it is
    /// spent. The [`App::retire_pending`] precedent: a fork's carried rows arrive under new
    /// ids, so **match on the content**. An ordinal is a position in the list that has just
    /// been replaced — carrying one across is carrying an offset measured against something
    /// that no longer exists, and the same integer then names a different place. That is the
    /// defect this exists for: the operator, scrolled up and reading, *"it also broke my
    /// scroll - i was scrolled up and it showed me thousands of lines 'below'"*.
    ///
    /// **And a row that is not there is said, not jumped over.** The view goes to the TAIL
    /// — the one place in the new transcript that is true of the whole of it — and the
    /// reader is told in a sentence they can see. A reader who was reading must never be
    /// moved in silence, and the tail is a move.
    ///
    /// # Not matchable YET is not gone
    ///
    /// A carry announces every row and publishes the bodies in the same pass, so a snapshot
    /// taken mid-carry holds rows with no words to match ([`Bulk`]). Until that carry
    /// completes the anchor is **pending**: it stays where it is, the window holds the line
    /// it was on (see the anchor's arm in `ui/transcript/window.rs`), and the row is placed
    /// the moment its body lands. Only when every announced row has its body and the words
    /// are still nowhere is the row honestly gone.
    ///
    /// Emitted as a note with its own code rather than a notice: the reader has to be able to
    /// find it again, `/notes` lists it, and `/status` counts it. `Failure`, by R29 part
    /// two's own test — it is not the reader's act, and what is at risk is their orientation:
    /// the thing they were reading is not there.
    pub(crate) fn repair_anchor(&mut self) {
        let Some(held) = self.anchor.clone() else {
            self.carry = None;
            return;
        };
        // **The row is on the screen, or in the transcript, under its own id** — so whatever
        // the replacement did, it did not take this row. The carry is spent either way: an
        // id that survived is an id this viewport never had to cross a boundary for, and
        // leaving the words pending would match them against a later transcript.
        if self.span_for(&held.item_id).is_some() {
            self.carry = None;
            return;
        }
        // **A row that is simply not rendered yet is NOT a row that is gone.** In tail mode
        // the rows above `hist_floor` were deliberately not walked, so a held row can be
        // present in `items` and absent from `spans` — and saying *it is gone* about a row
        // sitting in the transcript would be a false alarm on every scroll in a long
        // session. The distinction is `items`, which knows every row, versus `spans`, which
        // knows the drawn ones.
        if self.items.iter().any(|it| it.item_id == held.item_id) {
            self.carry = None;
            return;
        }
        // **The id is gone, so this is a transcript boundary.** The row's own words are the
        // one thing both transcripts can be asked for — and the first row that says them is
        // the row the reader was on, because a carry preserves the conversation's order and
        // that is the only ordering the two sides agree on.
        //
        // Borrowed rather than cloned: this runs every frame until the row is placed, and a
        // tool result's words are its whole payload.
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
            // **No `redraw`.** A moved viewport is a diff rather than an erase — the same
            // rule the scroll keys keep — and this is the head moving a reader who did not
            // move: the frame that follows writes only the rows that changed.
            return;
        }
        // **Not matchable yet is not gone.** A snapshot taken mid-carry holds rows whose
        // bodies are still coming, and there is nothing in a body-less row to match — so the
        // anchor waits, and the frame holds the line rather than counting from a bottom that
        // has moved. See the docstring.
        if self.bulk.is_some() {
            return;
        }
        // **Not carried.** The tail is the only place left that is true, and the reader is
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
                "the row you were reading is not in the transcript that replaced it — the \
                 view follows the tail again (row `{}`)",
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

/// **How long a wheel notch remembers the one before it** — 2026-10-09's third report.
///
/// Notches closer together than this are one flick, and a flick is an intent repeated;
/// notches further apart are a reader stepping through the conversation, and the step
/// between them is a walk. The driver waits ~100 ms for a read and hands one tick
/// whatever the terminal buffered, so an inertial flick lands whole bursts inside this
/// window — often several ticks' worth, because the flick outlasts one read — while a
/// notch spent reading arrives alone. 200 ms sits above the one and below the other.
pub(crate) const WHEEL_RUN_MS: u64 = 200;

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
/// The row's **id and not a line count**, because a count is what every arrival and every
/// re-wrap invalidates: content below moves the bottom a count is measured from, and a row
/// above growing moves every line under it.
///
/// **An id is a name inside ONE transcript.** The ids are `{transcript_id}.{n}`, so the
/// transcript a fork carries across names the same conversation with different ids — and
/// across that boundary the id is no better than an index. What survives a fork is the
/// row's own words: see [`Carry`] and [`App::repair_anchor`].
///
/// `ordinal` is the index at the moment of capture, and it is used **within one
/// transcript** and never across one: it is what [`App::reanchor_off_hidden`] searches
/// outward from when a rung hides the row. A snapshot that replaces the rows is a
/// boundary, and an index measured on the other side of it names a different place — see
/// [`App::repair_anchor`], which finds the row by its own words instead and goes to the
/// tail when it cannot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Held {
    pub(crate) item_id: String,
    pub(crate) ordinal: usize,
    /// Lines into the row's own rendering. Bounded to the row's height when it is used, so
    /// a row that shrank under the anchor does not push the view past its own end.
    pub(crate) into: usize,
}

/// **Where the reader was when a snapshot replaced the rows under them** — R36, taken at
/// the boundary and spent on the other side of it.
///
/// A transcript boundary (a fork, a re-seat, a compaction, a resync) replaces `items`
/// wholesale, and the ids are per-transcript (`{transcript_id}.{n}`, `engine.rs`) — so the
/// row the viewport was holding is named by nothing in the transcript that arrives. Two
/// facts still hold, and they are the two a reader can be found by:
///
/// * **`words`** — the row's own content ([`row_words`]), which is what a fork carries
///   across unchanged. [`App::retire_pending`] met the same wall when a fork replaced the
///   row it was waiting for and answered it the same way: **match on the text**. `None`
///   when the held row had no body at the boundary — there is then nothing to match, and
///   the honest outcome is the tail plus a sentence saying so.
/// * **`line`** — the body line the reader was at, in the buffer that just went. The
///   window holds it while the carry lands, because the alternative is the count in
///   [`App::scroll`], and that count is measured from a bottom that has moved.
///
/// **This is the view's state and not the daemon's `carry`.** A *carry* on the wire is a
/// snapshot whose rows have no bodies yet ([`Bulk`]); this is the reader's place taken at
/// the moment such a snapshot landed, and it is `Some` only until [`App::repair_anchor`]
/// has placed the row or given up on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Carry {
    pub(crate) words: Option<String>,
    pub(crate) line: usize,
}

/// **A row's own words** — what [`Carry::words`] is taken from.
///
/// The engine's JOIN for a `User` row (the text parts newline-joined, the same reading
/// [`App::retire_pending`] makes of one), the text for the prose kinds, the payload for a
/// tool result, the label for a segment mark. `None` for a row with no body yet and for
/// one that says nothing at all: a row with no words is a row this cannot find, and an
/// empty string would match the first empty row in the next transcript.
///
/// **One rule, two readers.** This builds the string the boundary takes; [`row_says`]
/// compares a row against it without building anything, because that one runs per row
/// while a carry lands and one row's text can be hundreds of kilobytes.
pub(crate) fn row_words(it: &SnapshotItem) -> Option<String> {
    match it.item.as_ref()? {
        TranscriptItem::System { text, .. }
        | TranscriptItem::Assistant { text, .. }
        | TranscriptItem::Reasoning { text, .. } => (!text.is_empty()).then(|| text.clone()),
        TranscriptItem::ToolResult { payload, .. } => {
            (!payload.is_empty()).then(|| payload.clone())
        }
        TranscriptItem::SegmentMark { label, .. } => (!label.is_empty()).then(|| label.clone()),
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

/// Whether this row says exactly `words` — [`Carry::words`]' question, asked of every row
/// in the transcript that replaced the one it was taken from. See [`row_words`].
///
/// **An empty `words` is never a match**, and that is [`row_words`]' own rule kept from this
/// side: a row that says nothing is a row the carry has nothing to find by, so an empty
/// string must not become the one thing it matches — which is what it would be, since the
/// first row of a conversation that says nothing says `""`.
pub(crate) fn row_says(it: &SnapshotItem, words: &str) -> bool {
    if words.is_empty() {
        return false;
    }
    match it.item.as_ref() {
        None => false,
        Some(TranscriptItem::System { text, .. })
        | Some(TranscriptItem::Assistant { text, .. })
        | Some(TranscriptItem::Reasoning { text, .. }) => text == words,
        Some(TranscriptItem::ToolResult { payload, .. }) => payload == words,
        Some(TranscriptItem::SegmentMark { label, .. }) => label == words,
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

/// One `User` row's text piece, if it has one — the same reading [`App::load`] and
/// [`App::record_item`] make when they retire a queued echo.
fn text_part(p: &UserPart) -> Option<&str> {
    match p {
        UserPart::Text { text } => Some(text),
        _ => None,
    }
}

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
