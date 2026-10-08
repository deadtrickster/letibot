//! **The conversation's window**: which rows of the transcript are on the screen, from the
//! anchor the reader holds or the bottom they follow.

use crate::app::*;
use crate::ui::render::{RenderConfig, row, row_strings, visible_width};
use crate::ui::*;
use letibot_sessionlog::view::{CallState, TurnState};
use letibot_transcript::TranscriptItem;

impl App {
    /// The visible `room` lines of the body, and nothing else built.
    pub(crate) fn body_window(&mut self, room: usize) -> Vec<String> {
        let cfg = self.cfg.clone();
        let (think, tool) = (self.reasoning, self.tools);
        let raw = self.raw_calls;
        if self.hist_width != cfg.width {
            self.hist_width = cfg.width;
            self.invalidate_history();
        }
        // Rows and notes, interleaved in the order they happened. A note anchored
        // at row N renders between row N-1 and row N, which is where it was when it
        // arrived.
        //
        // Destructured rather than indexed through `self`, so a row can be rendered
        // *while* the rendered lines are being appended and the tool-target table
        // is being read — three disjoint fields, one borrow each, no clone of a row
        // per frame.
        // Does the transcript already own this turn's content? If so the live pane
        // is a duplicate of history and only its summary line survives — otherwise
        // the answer is on the screen twice, once in the wrong order.
        //
        // Computed before the walk, because the walk needs it: it is the
        // difference between "the pane below is drawing this call" and "nothing
        // is".
        //
        // **And not while a call is still running.** The turn's text and its
        // proposals are recorded before the calls run, so "the transcript owns the
        // content" is true the moment the model stops speaking — minutes before
        // a `task` call returns. Retiring on that fact handed the screen to the
        // transcript row, which draws a call from its RESULT row, and the daemon
        // appends the round's results as one batch after the last call. Measured
        // 2026-09-17: a `todo_write` that finished in a millisecond stayed
        // `→ no result` for the fifteen minutes the subagent behind it ran. The
        // pane stands down when nothing is in flight, which is the fact.
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
        // **A conversation too big to walk is rendered from its end.**
        //
        // Placed here, before `in_flight` borrows `turn`, because the fill needs the
        // whole `App` while that borrow is alive.
        //
        // The frame shows the bottom of the session, so the bottom is what gets
        // rendered — see `fill_backward`. The test is the size of the transcript
        // rather than a time budget: below `SELF_WALK_LIMIT` the whole walk is a couple
        // of milliseconds and doing it keeps `hist_marks` dense, which is what makes
        // `invalidate_history_from` incremental. Above it, only the tail is rendered
        // and marks go unbuilt — correct, and a full rebuild when a row changes.
        if self.hist_floor == 0
            && self.hist_lines.is_empty()
            && transcript_bytes(&self.items) > self.walk_limit
        {
            self.hist_floor = self.items.len();
            self.fill_backward(room + TAIL_SLACK);
        }
        // **And the held row, if this head has not drawn it** (R36).
        //
        // A tail walk renders the last screenful and skips everything above it, so a
        // viewport holding a row from further up has no span and nothing to place itself
        // against. Rendering down to that row is the one thing that fixes it, and it is
        // asked for by ROW rather than by lines — a fill measured in lines can stop short
        // of the very row being held, which is a viewport chasing its own tail.
        if let Some(h) = self.anchor.clone() {
            let drawn = self.span_for(&h.item_id).is_some();
            if !drawn && self.items.iter().any(|it| it.item_id == h.item_id) {
                self.fill_to_row(h.ordinal);
            }
        }
        // **The one row `ctrl-t` can act on**, read once here rather than per row inside
        // the walk below — and before the walk's own borrow of `self`, which is why it is
        // not beside the rest of the tail's inputs. See [`ItemCtx::payload_newest`].
        let newest_payload = self.newest_payload_row();
        // The rows the live pane is still drawing. An assistant row in this set
        // does **not** draw its own unsettled calls: the pane below is drawing
        // them, with a spinner and a running clock, and `→ Read foo.rs · no result`
        // above a `◐ Reading foo.rs` is both a duplicate and, while the call is
        // still running, false.
        //
        // Empty once the pane has stood down, which is what stops a call the turn
        // was interrupted in the middle of from vanishing off the screen entirely:
        // nothing is drawing it, so the assistant row draws it, and says it never
        // came back.
        let live = live_work(self.turn.as_ref(), &self.items, &self.cfg, superseded);
        // **A CHANGE IN THE LIVE COUNTS PUTS THEM IN A CACHED ROW, SO IT INVALIDATES THAT ROW.**
        //
        // The walk bakes its marker — the counts and all — into `hist_lines`, which is the cache of
        // RENDERED rows, and that cache is only rebuilt from the row something changed at. Nothing
        // in it moves when the live counts move, because the counts are not rows: a reasoning delta
        // arrives, `live.think_lines` goes up, and every line of the walk is served unchanged from
        // the cache with the number it was rendered with.
        //
        // The operator, watching it: *"you started replying with `[1 tool call]`. then response
        // line appeared and then that `[1 tool call]` became `[1 tool call, 52 thinking lines]` —
        // so unlike in leticl thinking count is not live and backfilled after the first
        // non-thinking line."* Every word of that is this: the number is **backfilled**, and what
        // fills it is the arrival of a ROW — the first non-thinking line — which invalidates the
        // cache and re-renders the marker with whatever the counts had become.
        //
        // The pane's own marker is not affected, and that is why the two halves looked different:
        // it is pushed fresh every frame from the same `live`. This is the fix for the other half.
        //
        // Cheap and targeted: the counts change at most once per delta, and what is invalidated is
        // one run — the one the live work belongs to — not the transcript.
        // **`running` is in the key and must be: it is the YELLOW.** `marker_carries_live` is
        // `live.running > 0`, so a call that starts or stops executing changes what the marker
        // paints without changing either count. Left out, the invalidation above missed exactly the
        // case the operator reported next: *"i didnt see yellow toolcalls for a while. maybe the
        // same problem"* — and it was. The plain marker was in the cache, the call started running,
        // the counts did not move, and the yellow had nothing to rebuild it.
        // **One value, compared once.** [`MarkerFacts`] is everything the marker draws about NOW
        // plus the row the work is in, and it is the SAME value [`hidden_run_marker`] is handed —
        // so a fact the marker draws is a fact this comparison holds. See the field for the two
        // omissions this replaces (`running`, then the run).
        let facts = MarkerFacts {
            live,
            run: self.newest_run_row(&live),
        };
        if facts != self.marker_facts {
            let left = self.marker_facts.run;
            self.marker_facts = facts;
            // **From the EARLIER of the two rows.** A rebuild is *from row k onward*, so rewinding to
            // the row the colour was on re-renders both it and the run that now owns the work — and
            // the rows between them, which is the price of one invalidation rather than a second
            // mechanism for taking one colour off one row.
            if let Some(row) = [left, facts.run].into_iter().flatten().min()
                && !self.items.is_empty()
            {
                // **Clamped, because the run the live work belongs to may have no row yet.**
                // `newest_unseen_run` answers `items.len()` for the work in flight with nothing
                // committed under it — the one index past the end, which is its own spelling of
                // *there is no row* — and that is not a row to hand to `round_head`.
                let row = row.min(self.items.len() - 1);
                let from = self.round_head(row);
                self.invalidate_history_from(from);
            }
        }
        let in_flight: std::collections::HashSet<String> = if superseded {
            std::collections::HashSet::new()
        } else {
            self.turn
                .as_ref()
                .map(|t| t.appended.iter().cloned().collect())
                .unwrap_or_default()
        };
        // And the turn's rows — see the sibling set in `fill_backward_until`, and
        // [`TurnPane::turn_rows`] for the defect this distinction is.
        let turn_rows: std::collections::HashSet<String> = self
            .turn
            .as_ref()
            .map(|t| t.turn_rows.iter().cloned().collect())
            .unwrap_or_default();
        self.walk_history(
            &cfg,
            think,
            tool,
            raw,
            &newest_payload,
            live,
            &in_flight,
            &turn_rows,
        );

        // **A marker with no row joins the sentence above it, and the joining happens HERE.**
        //
        // The pane pushes the marker while `hist_lines` is lent to the frame, and the join is
        // a mutation of that buffer's last line — so it is decided now, one statement before
        // the borrow. The rule is the walk's own: the row above must be the MODEL's prose.
        // Without this the counts stood a blank line under the sentence that introduced them,
        // which is a row rather than a continuation — the same defect the operator reported
        // for their own message, in the one place the walk could not reach it.
        // **Last frame's join is UNDONE first**, so this is a set and not an append — see
        // [`App::live_join`] for what appending cost. The search is by the marker's own text
        // rather than by an index, because the cache may have been truncated and rebuilt since;
        // a line that no longer carries the marker needs no undo, and one that does is the line
        // this put it on.
        if let Some((original, marker)) = self.live_join.take()
            && let Some(at) = self
                .hist_lines
                .iter()
                .rposition(|l| l.trim_end().ends_with(&format!(" {marker}")))
        {
            self.hist_lines[at] = original;
        }
        // **Has the walk already carried the in-flight counts?** The walk folds them into the run
        // the current turn is working in — the `live_here` question — and if it has, a marker here
        // would be a SECOND one for one turn's work: `[1 tool call]` from the walk and
        // `[1 thinking line]` from the pane, which is the duplicate caught in a tmux sample of the
        // live head. Asked here, before the destructure below lends `items` out.
        //
        // **`LETIBOT_MARKER_DEBUG=1` prints the decision this guard makes.** The operator has
        // twice caught a turn's work drawn by two markers — *"just saw = [1 tool call] [1 tool
        // call] that later merge to [2 tool calls]"* — and neither state could be rebuilt from the
        // code: a test written for the closest one passes, which means the state is one of the
        // combinations nobody has described yet. So the two facts are printed at the moment of the
        // decision, and the next occurrence says which one it was instead of inviting a third
        // guess. The same technique the marker-doubling hunt used (`scripts/headwatch.sh` and the
        // captured frames in `docs/evidence/`), applied to the predicate rather than to the screen.
        let walk_carried_live = live.work() > 0
            && (0..self.items.len())
                .rev()
                .find_map(|k| {
                    let (s0, e0) =
                        unseen_run_at(&self.items, self.visibility, &self.bound_prompts, live, k)?;
                    (s0 == k).then_some((s0, e0))
                })
                .is_some_and(|(s0, e0)| {
                    (s0..e0).any(|r| {
                        self.turn.as_ref().is_some_and(|t| {
                            t.turn_rows.iter().any(|id| *id == self.items[r].item_id)
                        })
                    })
                });
        if std::env::var("LETIBOT_MARKER_DEBUG").is_ok() {
            let runs: Vec<(usize, usize)> = (0..self.items.len())
                .filter_map(|k| {
                    unseen_run_at(&self.items, self.visibility, &self.bound_prompts, live, k)
                        .filter(|(s0, _)| *s0 == k)
                })
                .collect();
            eprintln!(
                "MARKER walk_carried_live={walk_carried_live} pane_turn={:?} \
                 live(calls={}, running={}, think={}) runs={} items={} appended={:?}",
                self.turn.as_ref().map(|t| t.turn_id.clone()),
                live.calls,
                live.running,
                live.think_lines,
                runs.len(),
                self.items.len(),
                self.turn
                    .as_ref()
                    .map(|t| t.appended.clone())
                    .unwrap_or_default(),
            );
        }
        // **And not when the walk has already carried the counts** — the same guard the standalone
        // push below carries, and its absence here is what the pair was. The two paths are two
        // ways of drawing ONE marker, so a guard on one of them is a guard on neither: with the
        // walk's marker already on the screen, this joined a second copy to the end of the line it
        // was on, and the operator's screen read
        //
        //   `let me check that for you: [2 tool calls, 2 thinking lines] [2 thinking lines]`
        //
        // — one line, two markers, and their report of the same shape (*"[1 tool call] [1 tool
        // call]"*).
        let live_joins = live.work() > 0
            && !superseded
            && !walk_carried_live
            && self.visibility.hides_the_working()
            && self
                .spans
                .last()
                .and_then(|sp| self.items.get(sp.row))
                .is_some_and(|it| {
                    matches!(
                        it.item.as_ref(),
                        Some(letibot_transcript::TranscriptItem::Assistant { text, .. })
                            if !text.trim().is_empty()
                    )
                });
        if live_joins {
            let painted = Marker::new(
                live.calls,
                live.think_lines,
                // **No events: this marker stands for work with no row yet**, and an event is
                // a row the rung DID hide. See [`COUNT_RUNGS`].
                0,
                true,
                marker_carries_live(live),
                marker_room(self.cfg.width),
            )
            .painted(&self.cfg);
            if let Some(at) = self.hist_lines.iter().rposition(|l| !l.trim().is_empty()) {
                let original = self.hist_lines[at].trim_end().to_string();
                let joined = format!("{original} {painted}");
                if visible_width(&joined) <= self.cfg.width {
                    self.hist_lines[at] = joined;
                    // Remembered, so the next frame restores the line before deciding again.
                    self.live_join = Some((original, painted));
                }
            }
        }

        // **Scrolling up back-fills.** A belt to `scroll_up`'s braces: that key renders what
        // its own press needs, and this catches anything that moved the scroll without going
        // through a key — a click, a restore, a test that sets `scroll` directly. It asks for
        // a screen beyond `scroll` for the same reason `scroll_up` does.
        //
        // Before the borrows below, because it needs the whole `App`.
        if self.scroll > 0 && self.hist_floor > 0 {
            self.fill_backward(self.scroll + room + TAIL_SLACK);
        }

        // **One walk, two numbers, and the numerator cannot exceed its denominator.**
        // How many rows have ARRIVED, out of how many there are — both counted off the
        // same pass over `items`, so the difference is a count of rows that are genuinely
        // missing and never a number larger than the whole. The version this replaces kept
        // a high-water mark and reported `peak - pending`: on a replaced item vector the
        // peak was stale and the line said **`97 of 4 rows`**.
        let arrived = self.items.iter().filter(|i| i.item.is_some()).count();
        let outstanding = self.items.len() - arrived;
        // **Nothing missing means the bulk announcement is complete**, so the trigger
        // clears itself rather than leaving a line to be aged out by a clock.
        if outstanding == 0 {
            self.bulk = None;
        }
        let now_ms = self.now_ms;

        // **The anchor, before the frame borrows anything** (R36).
        //
        // Two things, and both need `&mut self`: a held row that the replacement took away
        // has to be **said**, and the reader has to be moved to the nearest row that
        // survived. Here rather than beside the placement below, because a note is written
        // on the head and the head is lent to the frame from the next line on.
        self.repair_anchor();

        // Read before the disjoint borrow below, for the same reason: it asks the rows
        // which announcements are already drawing an echo. See `App::bound_prompts`.
        let echoes_on_screen = self.echoes_on_screen();
        // Disjoint field borrows, so the history can be lent to the frame while the
        // block caches are still being written to.
        let App {
            hist_lines,
            turn,
            // **The anchor's own inputs** (R36), and the rung (R37). Taken apart here rather
            // than read through `self` further down, because `hist_lines` is lent to the
            // frame below and a `&self` method would need the whole of it back.
            items,
            spans,
            scroll,
            anchor,
            visibility,
            ..
        } = self;
        let vis = *visibility;
        // **Room for the segments this frame will actually push.** A live frame adds the gap, the
        // echo block, the pane and the chrome — a handful — and `vec![…]` starts at capacity 1, so
        // the pushes past the first few are reallocations on a per-frame path. The number is a
        // guess and a cheap one: too small only means one reallocation.
        let mut segs: Vec<Seg<'_>> = Vec::with_capacity(8);
        segs.push(Seg::Borrowed(hist_lines));
        // The history no longer ends with a blank — separators go *before* a row
        // now, so the last row of the transcript is the last line of it. The live
        // pane therefore brings its own.
        // `vec![String::new()]` allocated a heap `Vec` to hold ONE empty line. An array on the
        // stack is the same slice without that allocation: `String::new()` itself allocates
        // nothing, so the only thing that was being bought was the Vec's buffer.
        //
        // **One line, not zero.** The gap is the blank that separates the transcript from the
        // echo and the live pane — emptying it would move every row below by one, which is the
        // class of defect the comment above this block is about.
        let gap: [String; 1] = [String::new()];
        if !hist_lines.is_empty() {
            segs.push(Seg::Borrowed(&gap));
        }

        // **THE QUEUED ECHO IS SPLIT BY ONE FACT, AND THE FACT DECIDES WHERE IT IS DRAWN.**
        //
        // **Words nothing has taken yet go to the TAIL — below the live pane.** The operator's own
        // screen, 2026-10-05: *"queued above thinking"* — their words, and directly under them the
        // turn's reasoning, which reads as though that working were the answer to words the daemon
        // has not been given yet. There is no answer to those words, so nothing below them may
        // look like one.
        //
        // **Words a row is already drawing stay HERE, glued above the pane.** That rule is the one
        // this block used to apply to *everything*, and it is right for exactly this half: the row
        // is committed above the pane and the reply to it streams below, so a prompt whose reply
        // can already be streaming has to sit above that reply. The report it answers: *"a message
        // was queued to harnessd, delivered to model, reply started streaming above the queued
        // message and then some tick goes off and queued message dequeued and rendered rightfully
        // above the reply. pure ui desync."*
        //
        // **The fact is `claimed_by`, and it is the same fact the MARK reads** — a row is drawing
        // these words, or nothing is. So a remainder stays with its row while an untaken entry
        // waits at the tail, and the words move exactly once: at the moment the daemon takes them,
        // which is a change of state and not a rendering artefact. Nothing else about the two
        // blocks differs — both are `queued_lines` through the same marks.
        let mut tail_echo: Vec<String> = Vec::new();
        // The prompts this head has sent that the transcript does not hold yet. See
        // `pending_prompts` for why this is the head's own queue and not the hub's.
        if !self.pending_prompts.is_empty() {
            // **THE AIR GOES AFTER THE ECHO, NOT BEFORE IT** — because that is where a landed row's
            // air is. `hist_lines` is followed by `gap`, and a row that has LANDED is part of
            // `hist_lines`, so a committed row has its blank BELOW it. The echo's blank used to be
            // in front, which put the echo one row higher than the row that replaces it and moved
            // every row between them when the announcement arrived — the same desync, one row out.
            // Measured: with the blank in front, the announcement lifts the echo by one; with it
            // behind, the frame is identical.
            let mut taken: Vec<String> = Vec::new();
            let open = self.echo_open;
            let unconfirmed = self.unconfirmed.clone();
            // **The pieces the rows above are already drawing, taken out of the queue** —
            // R51 item 15. Computed once for the whole queue rather than asked per entry,
            // because a claim is spent: two entries that read the same must not both claim
            // the one line a row is drawing.
            for (i, drawn, claimed_by) in
                unclaimed_prompts(&self.pending_prompts, &echoes_on_screen)
            {
                // **The mark follows the words this row is drawing.** A remainder is drawing what
                // the row above does not — the same prompt — so it asks `unconfirmed` under the
                // text that claimed it, which is the text the row itself asked under. Asking
                // under the remainder's own words would answer differently from the row beside it
                // the moment an entry grew, and asking under the ENTRY's would too. See
                // [`unclaimed_prompts`] for why that would be two statements about one fact.
                let key = claimed_by.as_deref().unwrap_or(&self.pending_prompts[i]);
                // **`queued` is not true once the row is on screen, and the mark is about the row.**
                //
                // R2: *"nothing the model says in reply may reach the screen before the prompt that
                // caused it"* — and the two reach the head on channels with different latencies.
                // The reply streams (`Delta` carries its text); the prompt's row is announced in a
                // frame that carries NO text and its body follows later. So for as long as the body
                // is in flight, the answer to a prompt was on the screen above an echo still
                // labelled `queued` — the state lagging the fact, in the operator's words:
                // *"my message | your line | and only then unqueued."*
                //
                // `claimed_by` is the answer: a row is drawing these words, so the words are not in
                // a queue any more, they are on the screen this reader is looking at. What the head
                // still owes is the BODY — whether the row's own text will match what was bound —
                // and that is exactly what `unconfirmed` says. So the remainder of a claimed entry
                // takes that mark, and the two are one statement rather than two: *queued* (nothing
                // has it), *unconfirmed* (a row has it and the body is still coming), unmarked (it
                // landed).
                // **A remainder of a claimed entry is not the same question as the walk's row.**
                // The walk draws a row that HAS landed, so it carries no claim. This draws the part
                // of the entry no row is drawing yet, and the daemon still owes it — but it is owed
                // *as part of an entry a row already covers*, so whether it lands with that row's
                // body or needs one of its own is exactly what the head cannot say. That is
                // `unconfirmed`'s own definition, so that is the mark.
                let mark = match claimed_by {
                    Some(_) => UNCONFIRMED,
                    None => echo_mark(&unconfirmed, key, false),
                };
                // **The split, at the one place the fact is known.** A remainder has a row
                // drawing its head, so the rest of it belongs under that row; an entry nothing has
                // taken waits at the tail until the daemon does.
                let lines = queued_lines(&drawn, &cfg, mark, open);
                if claimed_by.is_some() {
                    taken.extend(lines);
                } else {
                    tail_echo.extend(lines);
                }
            }
            // The trailing blank is the air the landed row will have, so it goes if the block is
            // empty: a lone blank row above the pane is a row of nothing.
            if !taken.is_empty() {
                taken.push(String::new());
                segs.push(Seg::Owned(taken));
            }
        }

        if let Some(t) = turn {
            push_turn_pane(
                &mut segs,
                t,
                &cfg,
                think,
                tool,
                raw,
                now_ms,
                self.diff_split,
                live,
                vis,
                superseded,
                live_joins,
                walk_carried_live,
            );
        }

        // **THE UN-TAKEN WORDS WAIT BELOW THE PANE**, and the leading blank is what keeps them
        // from reading as the last line of the working. See the split's own comment above: the
        // pane's rows are the turn in flight and nothing in them is an answer to words the daemon
        // has not taken, so those words belong under them — where the reader put them, and next to
        // the composer they are waiting at. The moment the daemon takes them they move up into
        // their row instead, and both reports this ordering answers are quoted at that comment.
        if !tail_echo.is_empty() {
            segs.push(Seg::Owned(vec![String::new()]));
            segs.push(Seg::Owned(tail_echo));
        }

        // **A fill the daemon NAMED, with a bar when it is big enough to want one.**
        // The numbers are the daemon's (`Filling`); the threshold gates only whether it is
        // worth a bar with a cat on it (`MIN_FILLING`), not whether the line appears at
        // all. The bar carries settled-vs-to-come in the **glyph** (`█`/`░`, never the
        // prefill's `▓` *being computed now*) because nothing here is being computed — so
        // the distinction survives a terminal with no colour.
        if let Some((what, unit, done, total)) = self.filling.clone()
            && total >= MIN_FILLING
        {
            segs.push(Seg::Owned(filling_line(
                &what, &unit, done, total, now_ms, &cfg,
            )));
        }

        // **A compaction's own progress, drawn without a threshold.** A fill is gated by
        // `MIN_FILLING` because a three-row carry does not deserve a cat; a fold is never
        // small, never ordinary and never optional — it runs only when the session is at
        // the wall, and it is the longest wait in the program, so there is nothing to
        // weigh. This is the line the operator asked for twice: *"leticl compacts but why
        // no progress bar?"*
        if let Some(c) = self.compacting.clone() {
            segs.push(Seg::Owned(compacting_line(&c, now_ms, &cfg)));
        }

        // **An unnamed bulk announcement: the head says only what it observed.**
        //
        // Nobody told this head what the operation is, so it **names no cause** — the
        // defect this replaces announced *"carrying the conversation onto the new prompt"*
        // over every ordinary message, because the trigger was a body-less row and every
        // message has one for the R2 window. The trigger is now the snapshot's bulk
        // announcement ([`Bulk`]), and the sentence is only ever *how many rows are
        // missing*; past [`BODY_PATIENCE`] it stops claiming to be progress.
        //
        // **Not gated by `MIN_FILLING`.** A three-row batch does not deserve a bar, but a
        // three-row batch that never lands is exactly what this sentence is for — the
        // operator learned about a real daemon-side hole from `2 row(s) announced and
        // never filled in` fired outside any carry.
        if self.filling.is_none()
            && let Some(b) = &self.bulk
        {
            // **The count is the ANNOUNCEMENT's, and it used to be every body-less row.**
            //
            // `outstanding` is `items.len() - arrived`: every body-less row this head holds,
            // live ones included. The trigger is `bulk`, which is the ids a *snapshot*
            // announced. Those are different sets, and the mismatch was measured on the
            // operator's own screen 2026-09-23 — it read `2 row(s) announced and never filled
            // in` where the snapshot had announced **one** and a live row with no body made up
            // the difference. The sentence then attributed the live row to the daemon.
            //
            // Reproduced in `a_named_fill_draws_the_bar…`'s own test: a good snapshot plus one
            // live body-less row, and the mixed case prints the snapshot's count.
            let announced = b.ids.len();
            let said = if now_ms.saturating_sub(b.at_ms) >= BODY_PATIENCE {
                // **What this head OBSERVED, and no cause it cannot see.** The head was told
                // these rows exist — a snapshot carried them — and the bodies never arrived.
                // Whether the daemon withheld them or sent them and they were lost is not
                // knowable from here: a head sees what arrives. So the sentence says the fact
                // and then names the ROWS, because a count is the least useful form of it and
                // the ids are what make it checkable.
                // **The ids first, because the line is trimmed to the frame.** The sentence
                // used to spend its whole width on a cause and leave the count — the one fact
                // it had — as the last thing on a line that gets cut. So the naming comes
                // immediately after the count, and the cause and the remedy follow for a
                // frame wide enough to hold them.
                let mut ids: Vec<&str> = b.ids.iter().map(String::as_str).collect();
                ids.sort_unstable();
                let named = if announced <= 4 {
                    format!(": {}", ids.join(", "))
                } else {
                    format!(": {}, … ({} more)", ids[..3].join(", "), announced - 3)
                };
                format!(
                    "  {announced} row(s) announced to this head and never filled{named} \
                     — `/resync` clears this"
                )
            } else {
                format!("  {announced} row(s) announced, waiting for the daemon to send them")
            };
            segs.push(Seg::Owned(vec![
                String::new(),
                row(&rano::agent::pane::faint(said), cfg.palette()),
            ]));
        }

        // **The wait, as a walking cat at the centre of the conversation.**
        //
        // This is the frame the head draws while it is still asking the daemon —
        // `HeadClient::attach` blocks on a `Hello` that carries the whole snapshot, so
        // on a big session there is a real wait with nothing to show. A spinner is
        // the usual answer and it is the right one *here*: the work is client-side and
        // the head genuinely cannot say more, because it has been told nothing.
        //
        // It is drawn rather than the empty-transcript banner because that banner is
        // a **claim about the session** ("this session has said nothing yet"), and a
        // head that has not been answered is in no position to make one. See
        // `attaching`.
        //
        // Dead centre: horizontally by padding to the width, vertically by the same
        // arithmetic `centred` uses for the panes, so it sits in the middle of
        // whatever room the walk left rather than two rows under the header.
        let waiting;
        if self.attaching {
            let elapsed = self.now_ms.saturating_sub(self.attach_started_ms);
            // The cat, its trail, the sentence, the clock — and the way out once it is late
            // (`ATTACH_IMPATIENT`): `rano::agent::screens::attaching`, which keeps the reasons
            // for each (the cat padded to its widest frame, alone on its row).
            let rows = row_strings(
                &rano::agent::screens::attaching(elapsed, room, cfg.width),
                cfg.palette(),
            );
            waiting = rows;
            segs.push(Seg::Borrowed(&waiting));
        }

        // Nothing has happened yet. An empty screen with a status line under it is
        // indistinguishable from a head that attached to the wrong socket.
        //
        // **Unless the head has not been told yet** — that case has the cat above,
        // and this banner would assert that the session is empty when the truth is
        // that nobody has reported yet.
        let opening;
        if segs.iter().all(|s| s.len() == 0) && !self.attaching {
            opening = row_strings(&rano::agent::screens::opening(), cfg.palette());
            segs.push(Seg::Borrowed(&opening));
        }

        let total: usize = segs.iter().map(Seg::len).sum();
        self.body_len = total;
        // Clamped so the window stays **full**, not so the last line stays on
        // screen. It was `total - 1`, which meant scrolling to the top left a
        // one-line window — and since the banner below overwrites the last line of
        // the window, the whole screen went blank with `── scrolled back · 56 lines
        // below` at the top of it. Found by pressing PageUp six times under tmux,
        // which is a thing a person does and no test did.
        // **R36: the viewport is placed from the ROW it is holding, not from a count.**
        //
        // This is the one place the anchor becomes lines, and it happens after everything
        // that can move a line: the history walk, the fill above it, the live pane and the
        // tail block are all in `total` by now. A count from the bottom would have been
        // invalidated by every one of them — content arriving below moves `total`, and the
        // window moves with it, which is the defect exactly.
        if let Some(held) = anchor.as_ref() {
            let span = spans.iter().find(|sp| {
                items.get(sp.row).map(|i| i.item_id.as_str()) == Some(held.item_id.as_str())
            });
            match span {
                Some(span) => {
                    let top = (span.at + held.into).min(total.saturating_sub(1));
                    let end = (top + room.max(1)).min(total);
                    *scroll = total.saturating_sub(end);
                    // **And the state is NOT changed here, even when `scroll` lands at 0.**
                    //
                    // That was tempting and it is wrong: a replacement that shortens the
                    // transcript can put a held row near the end, the arithmetic lands the
                    // window at the bottom, and clearing the hold there would mean **the
                    // head returned the reader to following by itself** — which is exactly
                    // what the requirement forbids. Only an act does that: a scroll down
                    // past the bottom ([`App::hold`]) or `esc`.
                }
                // **The row is gone.** Its handling is above, before the frame borrowed
                // the history — see `App::repair_anchor` — because saying so is a note and
                // a note is `&mut self`. Reaching here means it went away between that
                // check and this line, which cannot happen: nothing between them replaces
                // the rows. Falling back to the count is the honest answer if it ever does.
                None => {}
            }
        }
        *scroll = (*scroll).min(total.saturating_sub(room.max(1)));
        // The borrow ends HERE and not later: everything below reads `self` again, and the
        // window arithmetic is the last thing that wants the field itself.
        let scrolled = *scroll;
        let end = total.saturating_sub(scrolled);
        let start = end.saturating_sub(room);
        // **What the last frame drew**, for the key handler between frames — the same rule
        // `dec_content_room` follows for the decision card. R36's key handler has to answer
        // *where is the reader looking* and the only honest source is the glass.
        self.view_top = start;
        self.view_room = room;
        // The borrow is over — nothing below reads `segs` — so what follows is the head's
        // own state rather than the frame's.
        let mut out = take_window(&segs, start, end);
        // **Which of these rows are about a file**, for a click to open (see `file_rows`). Only
        // the history's own lines are rows of the conversation with a span behind them — the
        // live pane and the tail below them are not — so the map stops where `hist_lines` does.
        self.file_rows = self.file_rows_in(start, end);
        // **The disclosure is about the HOLD, not about a number** (R36). It used to be
        // `scroll > 0`, which ties the reader's sentence to a derived count: a hold whose
        // anchored row happens to sit near the END of a shortened transcript has
        // `scroll == 0`, and the reader was then left holding a viewport with nothing on
        // screen saying so. The state is the anchor, so the sentence follows the anchor.
        if !self.following() {
            let behind = total - end;
            let last = out.len().saturating_sub(1);
            // **The state, and the act that undoes it** — R29's rule for a disclosure, and
            // R36's for the reader who cannot tell pinned from following.
            out[last] = row(&rano::agent::screens::holding(behind), self.cfg.palette());
            // The banner took that row, so it is no longer a row of any file.
            self.file_rows.retain(|(r, _)| *r != last);
        }
        out
    }
}

/// A run of body lines: history is **borrowed** from the head's own buffer, the
/// live tail is owned and rebuilt. See [`App::screen`] for why this is not one
/// `Vec<String>`.
pub(crate) enum Seg<'a> {
    Borrowed(&'a [String]),
    Owned(Vec<String>),
}

impl Seg<'_> {
    pub(crate) fn len(&self) -> usize {
        match self {
            Seg::Borrowed(s) => s.len(),
            Seg::Owned(v) => v.len(),
        }
    }

    pub(crate) fn get(&self, i: usize) -> &str {
        match self {
            Seg::Borrowed(s) => &s[i],
            Seg::Owned(v) => &v[i],
        }
    }
}

/// Lines `[start, end)` of the concatenation, and only those.
pub(crate) fn take_window(segs: &[Seg<'_>], start: usize, end: usize) -> Vec<String> {
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
    /// **The turn's in-flight work, read before the walk's disjoint borrow.** The
    /// destructure below deliberately leaves `turn` out (the pane later needs it whole),
    /// so this is the one place the walk can see it — and it must, because a call in
    /// flight is not a row and the counts have to carry it.
    pub(crate) fn walk_history(
        &mut self,
        cfg: &RenderConfig,
        think: Fold,
        tool: Fold,
        raw: bool,
        newest_payload: &Option<String>,
        live: LiveWork,
        in_flight: &std::collections::HashSet<String>,
        turn_rows: &std::collections::HashSet<String>,
    ) {
        let App {
            hist_lines,
            hist_upto,
            hist_marks,
            note_upto,
            items,
            notes,
            call_targets,
            call_ms,
            call_edits,
            call_decisions,
            hist_class,
            hist_first_class,
            hist_floor,
            hist_renders,
            diff_split,
            payload_sel,
            payload_page,
            payload_max,
            screen_rows,
            bound_prompts,
            unconfirmed,
            echo_open,
            spans,
            visibility,
            dismissed,
            ..
        } = self;
        let diff_split = *diff_split;
        let echo_open = *echo_open;
        // **The run `ctrl-t` opens, computed once per frame** (R37 AMENDED): the same
        // shape `newest_payload` has above, and for the same reason — the seam names the
        // chord only on the run the chord acts on, and asking per row would be a scan of
        // the transcript for every row drawn.
        let newest_run = newest_unseen_run(items, *visibility, bound_prompts, live);
        loop {
            // **A note from before this window is stepped over, not drawn** (R19).
            // It is a disclosure this head holds — `/notes` lists it and `/status`
            // counts it — and it has no seam in this conversation to be drawn at,
            // because this head was not there when it happened. Stepped over here,
            // once, and never again: `note_upto` only goes forward, so the cost is
            // paid at the head of the list and not per row.
            while matches!(
                notes.get(*note_upto).map(|(place, _)| place),
                Some(Placed::Before)
            ) {
                *note_upto += 1;
            }
            let note_next = notes
                .get(*note_upto)
                .is_some_and(|(place, _)| matches!(place, Placed::Seam(at) if *at <= *hist_upto));
            if note_next {
                // **A retired note contributes nothing — not its text, and not
                // the blank line above it.** R10: the note is a disclosure, and
                // the reader has read it. Skipping the blank as well is what
                // makes the wall go rather than becoming a column of gaps; the
                // note itself is still in `notes`, still counted on `/status`,
                // and still listed by `/notes`.
                if !dismissed.contains(&note_key(&notes[*note_upto].1)) {
                    if !hist_lines.is_empty() {
                        hist_lines.push(String::new());
                    }
                    hist_lines.extend(note_lines(&cfg, &notes[*note_upto].1));
                    *hist_class = Some(RowClass::Other);
                }
                *note_upto += 1;
            // `hist_upto` is "every row at or below this index is accounted for".
            // A tail walk sets it to the row count, so this does nothing until a
            // *new* row arrives at the end — which is the whole point: the rows
            // above the floor are deliberately not walked.
            //
            // **In tail mode there are no marks.** They are indexed by absolute
            // row, the walk pushes one per row it passes, and a tail walk does not
            // pass the rows it skipped — so `invalidate_history_from` finds none
            // and falls back to a full rebuild. Correct, just not incremental, and
            // it is the honest trade for not lexing a 160 MB session to draw its
            // last 40 rows.
            // **This turn's assistant row, announced and not yet filled, holds back every row
            // after it.** Its text is on the screen already — it streamed into the live pane —
            // and it moves into the transcript only when the row's BODY lands. A row announced
            // after it (the operator's message the daemon took at the step boundary) drawn now
            // would sit ABOVE the reply it follows, and then be pushed under it when the body
            // landed: *"it can go above the most recent piece of reply and then get reordered to
            // the bottom. very strange feeling"* (2026-10-08). So the walk waits here; the message
            // keeps its `queued` echo under the live pane until then, and everything lands in
            // order, once. Only rows of the turn in flight: a body that never comes for an old
            // row must not freeze the transcript, and once the turn ends this does not apply.
            } else if *hist_upto < items.len()
                && *hist_upto >= *hist_floor
                && items[*hist_upto].kind == "assistant"
                && items[*hist_upto].item.is_none()
                && in_flight.contains(&items[*hist_upto].item_id)
            {
                break;
            } else if *hist_upto < items.len() && *hist_upto >= *hist_floor {
                // Where the walk stands before this row, so a later "from row
                // k on" can come back to exactly here. Recorded for every row,
                // including one that renders to nothing, because the mark is
                // indexed by row and a gap would misalign every mark after it.
                if hist_marks.len() == *hist_upto {
                    hist_marks.push(HistMark {
                        lines: hist_lines.len(),
                        note_upto: *note_upto,
                        class: *hist_class,
                    });
                }
                // An assistant row carries the arguments for the calls it
                // proposed, and the tool-result rows that follow it want the
                // same label. Learning them here, in transcript order, is what
                // lets a head that attached *after* a turn still say which file
                // was read — the proposal event is long gone and the row is the
                // only place the arguments survive.
                //
                // **Replaced, not merged.** `call_0` is round-positional, so a
                // merge is how round 4's `call_0` came to be labelled with
                // round 1's path. An assistant row opens a new round and its
                // calls are the only ones the rows after it can be about; one
                // with no calls at all opens a round with no calls, and a
                // stray result then has to say so rather than borrow.
                let mut answered: std::collections::HashSet<String> =
                    std::collections::HashSet::new();
                if let Some(TranscriptItem::Assistant { tool_calls, .. }) =
                    items[*hist_upto].item.as_ref()
                {
                    call_targets.clear();
                    for c in tool_calls {
                        call_targets.insert(
                            c.id.clone(),
                            letibot_sessionlog::display_target(&c.arguments),
                        );
                    }
                    answered = round_results(items, *hist_upto);
                }
                *hist_renders += 1;
                // **The run this row belongs to, and whether it is the one that is open**
                // (R37 AMENDED). Three states, and only one of them is a row:
                //
                // * the row is inside the OPEN run — the rung is lifted for it, so it
                //   draws itself, and the marker for that run must not also be drawn;
                // * the row is the FIRST row of a closed run — the marker stands for the
                //   whole run and is drawn here, at the run's first row, in both walk
                //   directions (which is what makes the two walks agree without either
                //   of them having to remember anything);
                // * any other row of a closed run — nothing at all, which is R37 as
                //   filed, and the walk treats a row that renders to nothing as no row.
                // **The room the counts will need, reserved before the sentence wraps.**
                // See `reserved_for_run`: without this the join depends on where the
                // prose's last line happened to end, which is the operator's *"sometimes
                // you do it same line - sometimes dont."*
                let reserve = reserved_for_run(
                    items,
                    *visibility,
                    bound_prompts,
                    live,
                    &cfg,
                    *hist_upto,
                    newest_run == Some(*hist_upto + 1),
                );
                let row_cfg = match reserve {
                    Some(room) => RenderConfig {
                        width: cfg.width.saturating_sub(room).max(20),
                        ..cfg.clone()
                    },
                    None => cfg.clone(),
                };
                let open_run = run_open_at(
                    items,
                    *visibility,
                    bound_prompts,
                    live,
                    payload_sel.as_deref(),
                    *hist_upto,
                );
                let unseen = if open_run {
                    None
                } else {
                    unseen_run_at(items, *visibility, bound_prompts, live, *hist_upto)
                };
                // **No blank line in front of a marker.** It continues the sentence above
                // it rather than standing as a row of its own, so the separator's blank —
                // which exists to say *a new kind of thing starts here* — is the opposite
                // of what it means. See [`hidden_run_marker`].
                let joinable = unseen.is_some_and(|(start, _)| run_continues_prose(items, start));
                let tight = unseen.is_some() && joinable;
                let (class, rows) = match unseen {
                    Some((start, end)) if start == *hist_upto => {
                        // See the backward walk's note: the run holding the turn's
                        // NEWEST row, which is the one the in-flight work continues.
                        let live_here = newest_run == Some(start)
                            && (start..end).any(|r| turn_rows.contains(items[r].item_id.as_str()));
                        let marker = hidden_run_marker(
                            items,
                            start,
                            end,
                            *visibility,
                            &cfg,
                            newest_run == Some(start),
                            MarkerFacts::of(live, newest_run),
                            live_here,
                        );
                        // **Glued to the sentence it continues**, when there is one: the
                        // last drawn row is [`RowClass::Speech`] — the class this file
                        // already has for prose the reader can see — and the joined line
                        // still fits the frame. Otherwise it stands alone, which is the
                        // honest degradation: counts with no sentence are still the fact,
                        // and a marker clipped to fit would lose them.
                        // **Painted before it is measured or placed.** `visible_width`
                        // skips escapes, so the width check below sees the counts and not
                        // the register they are drawn in.
                        let painted = marker.painted(&cfg);
                        let joined = if joinable && *hist_class == Some(RowClass::Speech) {
                            hist_lines
                                .iter()
                                .rposition(|l| !l.trim().is_empty())
                                .map(|at| format!("{} {painted}", hist_lines[at].trim_end()))
                                .filter(|l| visible_width(l) <= cfg.width)
                        } else {
                            None
                        };
                        match joined {
                            Some(line) => {
                                let at = hist_lines
                                    .iter()
                                    .rposition(|l| !l.trim().is_empty())
                                    .expect("the joined line came from one");
                                hist_lines[at] = line;
                                (RowClass::Other, Vec::new())
                            }
                            None => (RowClass::Activity, vec![painted]),
                        }
                    }
                    Some(_) => (RowClass::Other, Vec::new()),
                    None => item_lines(
                        &items[*hist_upto],
                        &ItemCtx {
                            cfg: &row_cfg,
                            think,
                            tools: tool,
                            raw,
                            targets: call_targets,
                            answered: &answered,
                            subagents: &self.subagents,
                            drawn_live: in_flight.contains(items[*hist_upto].item_id.as_str()),
                            elapsed_ms: call_ms.get(&items[*hist_upto].item_id).copied(),
                            edit: call_edits.get(&items[*hist_upto].item_id),
                            decision: call_decisions.get(&items[*hist_upto].item_id),
                            bound: bound_prompts
                                .get(&items[*hist_upto].item_id)
                                .map(String::as_str),
                            // **A bound row is being drawn, so its mark is never `queued`** —
                            // see [`echo_mark`]. This was the same `_ => QUEUED` as its sibling
                            // walk above, and it is the one that drew the operator's own line:
                            // *"my message | your line | and only then unqueued."*
                            echo_mark: bound_prompts
                                .get(&items[*hist_upto].item_id)
                                .map(String::as_str)
                                .map(|t| echo_mark(unconfirmed, t, true))
                                .unwrap_or(QUEUED),
                            echo_open,
                            // **An open run lifts the rung for its own rows**, which is
                            // what "it opens" means: the reader sees the very rows the
                            // rung was hiding, with their own headlines, payloads and
                            // diffs, rather than a second rendering of them.
                            vis: if open_run {
                                Visibility::lifted()
                            } else {
                                *visibility
                            },
                            diff_split,
                            // Rebuilt per row inside the walk, so it cannot be hoisted
                            // out of this borrow — it reads two fields the walk is
                            // holding. **Closed for an open run**: that run is being
                            // read whole, and opening a payload window inside a row that
                            // is only on screen because the run is open would be two
                            // unfoldings of one thing.
                            payload_view: if open_run {
                                None
                            } else {
                                payload_sel
                                    .as_deref()
                                    .filter(|id| !id.is_empty())
                                    .map(|id| (id, *payload_page))
                            },
                            payload_newest: newest_payload.as_deref(),
                            payload_max: Some(payload_max),
                            window_rows: screen_rows.saturating_sub(WINDOW_CHROME),
                        },
                    ),
                };
                // A row that rendered nothing gets no separator either. An
                // assistant row whose text is `"\n\n\n"` and whose every call
                // is drawn by its own result row is a real and common shape —
                // it is what a tool-calling round looks like — and paying two
                // blank lines for it puts a hole in the transcript.
                if !rows.iter().all(|l| l.trim().is_empty()) {
                    // Air where the KIND changes, not between every pair of
                    // rows. Two tool cards in a row are one block and read as
                    // one; a blank between each of them was a third of the
                    // vertical budget spent separating things a glyph in the
                    // first column already separates.
                    let pack =
                        *hist_class == Some(RowClass::Activity) && class == RowClass::Activity;
                    if !hist_lines.is_empty() && !pack && !tight {
                        hist_lines.push(String::new());
                    }
                    if hist_first_class.is_none() {
                        *hist_first_class = Some(class);
                    }
                    // **And where this row's lines begin** (R36). Recorded here rather
                    // than derived later, because a separator's blank line belongs to no
                    // row and only this walk knows it pushed one.
                    spans.push(Span {
                        row: *hist_upto,
                        at: hist_lines.len(),
                        lines: rows.len(),
                    });
                    hist_lines.extend(rows);
                    *hist_class = Some(class);
                }
                *hist_upto += 1;
            } else {
                break;
            }
        }
    }
}
