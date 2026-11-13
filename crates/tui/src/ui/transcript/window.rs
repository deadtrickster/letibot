//! The conversation's window: which rows of the transcript are on the screen, from the
//! anchor the reader holds or the bottom they follow.

use crate::app::*;
use crate::ui::render::{RenderConfig, row, row_strings, trim_to, visible_width};
use crate::ui::*;
use letibot_sessionlog::view::{CallState, TurnState};
use letibot_transcript::TranscriptItem;

impl App {
    /// The conversation while a switch is in flight: the wait, and nothing of the session being left. The cat used
    /// to be pushed after every row of the session being left, which reads as the parent's conversation with a
    /// wait squeezed under it.
    fn switch_wait(&mut self, room: usize, cfg: &RenderConfig) -> Vec<String> {
        self.file_rows.clear();
        self.notice_rows.clear();
        self.card_rows.clear();
        self.call_rows.clear();
        self.picture_rows.clear();
        self.payload_rows.clear();
        self.marker_rows.clear();
        self.picture_refs.clear();
        self.view_top = 0;
        self.view_room = room;
        let elapsed = self.now_ms.saturating_sub(self.attach_started_ms);
        let mut rows = row_strings(
            &rano::agent::screens::attaching(elapsed, room, cfg.width),
            cfg.palette(),
        );
        // Named, because the whole defect was two sessions on one screen with nothing saying
        // which was which. The reader can check the name against the row he pressed Enter on.
        let to = self.switching_to().unwrap_or_default();
        let from = self.session_label(&self.session_id);
        rows.extend(row_strings(
            &rano::agent::screens::system_row(
                "switch",
                &format!("leaving {from} for {to} — waiting for the daemon to answer"),
                // The body is drawn two columns in (the gutter), and `wrap` cuts at the width it
                // is given: `cfg.width` would clip the last words rather than fold them.
                cfg.width.saturating_sub(2),
            ),
            cfg.palette(),
        ));
        // The frame keeps the tail, because the sentence is the point: `attaching` pads itself to `room / 2`
        // blanks above the cat, which only holds while nothing follows it, and `main_area` truncates to the body's
        // height. The padding goes instead.
        let over = rows.len().saturating_sub(room);
        rows.drain(..over);
        self.body_len = rows.len();
        rows
    }

    /// The visible `room` lines of the body, and nothing else built.
    pub(crate) fn body_window(&mut self, room: usize) -> Vec<String> {
        let cfg = self.cfg.clone();
        let (think, tool) = (self.reasoning, self.tools);
        let raw = self.raw_calls;
        if self.hist_width != cfg.width {
            self.hist_width = cfg.width;
            self.invalidate_history();
        }
        // A switch is a wait, and the rows this head holds belong to the session it is leaving: the walk below
        // renders those rows, so while a switch is in flight none of it is the frame. See [`App::switch_wait`].
        if self.leaving() {
            return self.switch_wait(room, &cfg);
        }
        // Rows and notes interleaved in the order they happened: a note anchored at row N renders between rows
        // N-1 and N. Destructured rather than indexed through `self`, so a row renders while the lines are being
        // appended and the tool-target table read.
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
        // Following the tail means showing its end. A buffer that stopped short of the session is
        // a window the reader left behind, and drawn as if it followed it would put a row from
        // however long ago at the foot of the screen as the newest one.
        if self.anchor.is_none() && self.hist_ceiling.is_some() {
            self.reanchor_at_tail(room);
        }
        // A conversation too big to walk is rendered from its end and the held row rendered down to it: the
        // frame's half of [`App::render_held`], which is where the bootstrap lives now so a scroll key between
        // frames can run the same two steps.
        self.render_held(room);
        // The one row `ctrl-v` can act on, read once here rather than per row inside
        // the walk below - and before the walk's own borrow of `self`, which is why it is
        // not beside the rest of the tail's inputs. See [`ItemCtx::payload_newest`].
        let newest_payload = self.newest_payload_row();
        // The rows the live pane is still drawing. An assistant row in this set does not draw its own unsettled
        // calls: the pane below draws them with a spinner and a clock, and both above and below would be a
        // duplicate and, while the call runs, false.
        let live = live_work(self.turn.as_ref(), &self.items, &self.cfg, superseded);
        // A change in the live counts puts them in a cached row, so it invalidates that row: the walk bakes the
        // marker into `hist_lines` and nothing in that cache moves when the counts move, since the counts are not
        // rows.
        let facts = MarkerFacts {
            live,
            run: self.newest_run_row(&live),
        };
        if facts != self.marker_facts {
            let left = self.marker_facts.run;
            self.marker_facts = facts;
            // From the EARLIER of the two rows: a rebuild is from row k onward, so rewinding to the row the
            // colour was on re-renders it and the run that now owns the work, which is cheaper than a second
            // mechanism for taking one colour off one row.
            if let Some(row) = [left, facts.run].into_iter().flatten().min()
                && !self.items.is_empty()
            {
                // Clamped, because the run the live work belongs to may have no row yet:
                // `newest_unseen_run` answers `items.len()`, its own spelling of *there is no row*.
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
        // And the turn's rows - see the sibling set in `fill_backward_until`, and
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
        // The budget `release_below` set is spent by the walk that used it, and no other frame's.
        self.walk_ahead = usize::MAX;

        // A marker with no row joins the sentence above it, and the joining happens here: the pane pushes the
        // marker while `hist_lines` is lent to the frame, so the mutation of that buffer's last line is decided
        // one statement before the borrow. The row above must be the model's prose.
        // The join is taken back so the next walk can make it again - and ONLY while there is live
        // work to re-join: after hand-over the counts are 0, the walk need not run again, and taking
        // the line back would leave a folded run with no marker at all (its marker IS this line).
        if let Some((original, marker)) = self.live_join.take()
            && live.work() > 0
            && let Some(at) = self
                .hist_lines
                .iter()
                .rposition(|l| l.trim_end().ends_with(&format!(" {marker}")))
        {
            self.hist_lines[at] = original;
        }
        // Has the walk already carried the in-flight counts? It folds them into the run the current turn is
        // working in, and if it has, a marker here would be a second one for one turn's work.
        // The runs, once: the walk built its own while it held the fields (R123.3).
        let runs = RunIndex::of(
            &self.items,
            self.visibility,
            &self.bound_prompts,
            self.reasoning,
        );
        let walk_carried_live = live.work() > 0
            && facts.run.is_some_and(|start| {
                unseen_run_at(&runs, live, start).is_some_and(|(s0, e0)| {
                    s0 == start && run_carries_live(&self.items, &turn_rows, s0, e0)
                })
            });
        if std::env::var("LETIBOT_MARKER_DEBUG").is_ok() {
            let shown: Vec<(usize, usize)> = (0..self.items.len())
                .filter_map(|k| unseen_run_at(&runs, live, k).filter(|(s0, _)| *s0 == k))
                .collect();
            eprintln!(
                "MARKER walk_carried_live={walk_carried_live} pane_turn={:?} \
                 live(calls={}, running={}, think={}) runs={} items={} appended={:?}",
                self.turn.as_ref().map(|t| t.turn_id.clone()),
                live.calls,
                live.running,
                live.think_lines,
                shown.len(),
                self.items.len(),
                self.turn
                    .as_ref()
                    .map(|t| t.appended.clone())
                    .unwrap_or_default(),
            );
        }
        // A window that stops short of the session draws nothing of its tail: the live pane and queued echo belong to the
        // newest rows and would claim the working is where it is not. See `below`, counted after the trims. Nor when the
        // walk already carried the counts: two paths drawing one marker need one guard between them.
        let live_joins = live.work() > 0
            && !superseded
            && !walk_carried_live
            // The marker belongs to the newest row, which a window that stops short of the session
            // is not drawing: joining it here would put the counts on a row from long ago.
            && self.hist_ceiling.is_none()
            && self.items_below == 0
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
                // No events: this marker stands for work with no row yet, and an event is
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

        // Scrolling up back-fills: a belt to `scroll_up`'s braces, catching anything that moved the scroll
        // without a key - a click, a restore, a test setting `scroll` directly. It asks for a screen beyond
        // `scroll`, as that key does.
        if self.scroll > 0 && self.hist_floor > 0 {
            self.fill_backward(self.scroll + room + TAIL_SLACK);
        }
        // The window has a budget, and everything this frame drew is in it. Run after the walks
        // and the fill above, so what is measured is what the head would go on holding.
        self.trim_held(room);
        // And the lines those rows were drawn as are a window too: the second bound, run last so it
        // measures everything the frame put in the buffer.
        self.trim_history(room);
        // And when the reader has scrolled up past the oldest row this head holds, the daemon answers the rows above in
        // pages. `hist_floor == 0` alone means *everything I hold is rendered*, also true at the tail of a short session,
        // so the ask needs the reader to have left the tail as well.
        if self.hist_floor == 0 && (self.scroll > 0 || !self.following()) {
            self.ask_for_rows_above();
        }
        // And the same at the other end: a reader at the end of what is held, with the session gone
        // on below it, has a tail one page away in the direction the session runs.
        if self.anchor.is_none() {
            self.ask_for_rows_below();
        }

        // One walk, two numbers, and the numerator cannot exceed its denominator: how many rows have arrived out
        // of how many there are, both counted off one pass over `items`. The version this replaces kept a
        // high-water mark and reported `97 of 4 rows`.
        let arrived = self.items.iter().filter(|i| i.item.is_some()).count();
        let outstanding = self.items.len() - arrived;
        // Nothing missing means the bulk announcement is complete, so the trigger
        // clears itself rather than leaving a line to be aged out by a clock.
        if outstanding == 0 {
            self.bulk = None;
        }
        let now_ms = self.now_ms;

        // The anchor, before the frame borrows anything: a held row the replacement took away is found again by
        // its own words or said to be gone, and the reader is moved to the tail when the new transcript does not
        // carry it. Both need `&mut self`.
        self.repair_anchor();

        // A carry in flight, and the line the window holds while it lands: read before the disjoint borrow below
        // because the anchor's own arm needs it and it is a field that arm cannot reach. See [`App::carry`].
        let carrying = self.carry.as_ref().map(|c| c.line);

        // Read before the disjoint borrow below, for the same reason: it asks the rows
        // which announcements are already drawing an echo. See `App::bound_prompts`.
        let echoes_on_screen = self.echoes_on_screen();
        // What a click lifted, read before the frame borrows the history. It is one field
        // (`App::lift`) read against the run `payload_sel` names - see `App::lifted`, which is
        // the one reader - and it is taken here because `hist_lines` is lent to the frame below.
        let lifted = self.lifted();
        // Disjoint field borrows, so the history can be lent to the frame while the
        // block caches are still being written to.
        let App {
            hist_lines,
            turn,
            // The anchor's own inputs, and the rung. Taken apart here rather
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
        // Room for the segments this frame will actually push: a live frame adds the gap, the echo block, the
        // pane and the chrome, and `vec![…]` starts at capacity 1, so the pushes past the first few are
        // reallocations on a per-frame path. A guess, and a cheap one.
        let mut segs: Vec<Seg<'_>> = Vec::with_capacity(8);
        segs.push(Seg::Borrowed(hist_lines));
        // The history does not end with a blank (separators go before a row), so the live pane brings
        // its own. An array on the stack, since `String::new()` allocates nothing.
        let gap: [String; 1] = [String::new()];
        if !hist_lines.is_empty() {
            segs.push(Seg::Borrowed(&gap));
        }

        // A window that stops short of the session draws nothing of its tail, and says where the tail is. Rows let go of
        // as lines (the ring) and whole (the byte window a parked reader kept a screen of) put the tail below the window.
        // Counted here, after the walks and trims, so the seam and the pane are this frame's facts.
        let unheld = items.len().saturating_sub(self.hist_upto);
        let below = self.hist_ceiling.map_or(0, |_| unheld) + self.items_below;
        let parked = below > 0;
        let below_seam: Vec<String> = if parked {
            row_strings(
                &rano::agent::screens::system_row(
                    "window",
                    &format!(
                        "{below} row(s) below this window — \u{2193} comes down to them, and the \
                         working is under the newest of them",
                    ),
                    cfg.width,
                ),
                cfg.palette(),
            )
        } else {
            Vec::new()
        };
        if parked {
            segs.push(Seg::Borrowed(&below_seam));
        }

        // The queued echo is split by one fact and the fact decides where it is drawn: words nothing has taken
        // go to the tail, below the live pane, because nothing under them is an answer to them; once the daemon
        // takes them they move into their row's own place.
        let mut tail_echo: Vec<String> = Vec::new();
        // The prompts this head has sent that the transcript does not hold yet. See
        // `pending_prompts` for why this is the head's own queue and not the hub's.
        if !parked && !self.pending_prompts.is_empty() {
            // The air goes after the echo and not before it, because that is where a landed row's air is: a row in
            // `hist_lines` has its blank below it, and an echo's blank in front put it one row higher than the row
            // that replaces it.
            let mut taken: Vec<String> = Vec::new();
            let open = tool.is_open();
            let unconfirmed = self.unconfirmed.clone();
            // What the daemon is busy with instead, when it has said: a named fill, or a fold
            // (`CompactionProgress`), which holds the worker the same way.
            let booting = self.booting.is_some();
            let filling_now = self
                .filling
                .as_ref()
                .map(|(what, ..)| what.clone())
                .or_else(|| self.compacting.as_ref().map(|_| "compacting".to_string()));
            // The pieces the rows above are already drawing, taken out of the queue: computed once for the whole
            // queue, since a claim is spent and two entries reading the same must not both claim one line.
            for (i, drawn, claimed_by) in
                unclaimed_prompts(&self.pending_prompts, &echoes_on_screen)
            {
                // The mark follows the words this row is drawing: a remainder draws what the row above does not,
                // so it asks under the text that claimed it, as the row itself did. Its own words would answer
                // differently the moment the entry grew.
                let key = claimed_by.as_deref().unwrap_or(&self.pending_prompts[i]);
                // `queued` is not true once the row is on screen, and the mark is about the row: the reply streams
                // as `Delta` and the prompt's row is announced as an id, so the reply is always the faster one.
                let mark = match claimed_by {
                    Some(_) => UNCONFIRMED,
                    None => echo_mark(&unconfirmed, key, false),
                };
                // And what it is queued behind, when the daemon has said: a `/reseat` can hold the worker for
                // minutes, and a bare `queued` under it reads like a daemon that stopped listening. While a fill is
                // named, the line says so.
                let mark = match claimed_by {
                    Some(_) => mark.to_string(),
                    // A prompt typed while the daemon boots runs once it has started
                    //: nothing takes it before the worker runs.
                    None if booting && mark == QUEUED => QUEUED_UNTIL_STARTED.to_string(),
                    None => queued_behind(mark, filling_now.as_deref()),
                };
                // The split, at the one place the fact is known. A remainder has a row
                // drawing its head, so the rest of it belongs under that row; an entry nothing has
                // taken waits at the tail until the daemon does.
                let lines = queued_lines(&drawn, &cfg, &mark, open);
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

        // The call cards this frame draws, and the call each row belongs to - recorded here
        // and turned into window rows below, with every other `*_rows` record.
        let mut cards: Vec<(usize, String)> = Vec::new();
        if let Some(t) = turn
            && !parked
        {
            cards = push_turn_pane(
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
                lifted,
            );
        }

        // The un-taken words wait below the pane, and the leading blank is what keeps them from reading as the
        // last line of the working: nothing in the pane is an answer to words the daemon has not taken.
        if !parked && !tail_echo.is_empty() {
            segs.push(Seg::Owned(vec![String::new()]));
            segs.push(Seg::Owned(tail_echo));
        }

        // The daemon still starting, a named fill, a fold and a snapshot's announced rows are all drawn in the
        // status row now, not as rows here: each was a block at the foot of the conversation that changed height
        // as the operation went.

        // The wait, as a walking cat at the centre of the conversation: `attach` blocks on a `Hello` that carries
        // the whole snapshot, so on a big session there is a real wait with nothing to show, and the work is the
        // head's own rather than a program's.
        let waiting;
        if self.attaching {
            let elapsed = self.now_ms.saturating_sub(self.attach_started_ms);
            // The cat, its trail, the sentence, the clock - and the way out once it is late
            // (`ATTACH_IMPATIENT`): `rano::agent::screens::attaching`, which keeps the reasons
            // for each (the cat padded to its widest frame, alone on its row).
            let rows = row_strings(
                &rano::agent::screens::attaching(elapsed, room, cfg.width),
                cfg.palette(),
            );
            waiting = rows;
            segs.push(Seg::Borrowed(&waiting));
        }

        // Nothing has happened yet, and an empty screen with a status line is indistinguishable from a head that
        // attached to the wrong socket. Two cases are not this one: a head that has not been told yet has the cat
        // above, and a session whose store rows are still being restored says so.
        let opening;
        let restoring;
        if segs.iter().all(|s| s.len() == 0) && !self.attaching {
            match self.restoring {
                Some(n) => {
                    restoring = row_strings(
                        &rano::agent::screens::system_row(
                            "restore",
                            &format!(
                                "this session's {n} row(s) are in the store and have not been \
                                 loaded into this head yet — the daemon restores a session it does \
                                 not hold on its worker, so they arrive as it walks them (the \
                                 status row says how far it has got). Nothing is lost, and a \
                                 prompt typed now waits for the restore and runs on it."
                            ),
                            cfg.width,
                        ),
                        cfg.palette(),
                    );
                    segs.push(Seg::Borrowed(&restoring));
                }
                None => {
                    opening = row_strings(&rano::agent::screens::opening(), cfg.palette());
                    segs.push(Seg::Borrowed(&opening));
                }
            }
        }

        let total: usize = segs.iter().map(Seg::len).sum();
        self.body_len = total;
        // Clamped so the window stays full, not so the last line stays on screen: `total - 1` left a one-line
        // window at the top, and the banner below overwrites the last line, so the screen went blank.
        if let Some(held) = anchor.as_ref() {
            let span = spans.iter().find(|sp| {
                items.get(sp.row).map(|i| i.item_id.as_str()) == Some(held.item_id.as_str())
            });
            match span {
                Some(span) => {
                    let top = (span.at + held.into).min(total.saturating_sub(1));
                    let end = (top + room.max(1)).min(total);
                    *scroll = total.saturating_sub(end);
                    // The state is not changed here, even when `scroll` lands at 0: a shorter transcript can put a
                    // held row near the end, and clearing the hold there would return the reader to following on a
                    // frame nobody asked for.
                }
                // The row is not on the screen. Its handling is above, before the frame
                // borrowed the history - see `App::repair_anchor` - because saying so is a
                // note and a note is `&mut self`.
                None => {
                    // A carry in flight, and the count must not be asked: the reader's row is named but has no body
                    // yet, and `scroll` was measured against the transcript this one replaced, so every body that
                    // lands moves `total` under it.
                    if let Some(line) = carrying {
                        *scroll = total.saturating_sub((line + room.max(1)).min(total));
                    }
                    // And with no carry pending the count is what is left. Reaching here means the row went away
                    // between the check above and this line, which cannot happen; the count is the honest fallback.
                }
            }
        }
        *scroll = (*scroll).min(total.saturating_sub(room.max(1)));
        // The borrow ends HERE and not later: everything below reads `self` again, and the
        // window arithmetic is the last thing that wants the field itself.
        let scrolled = *scroll;
        let end = total.saturating_sub(scrolled);
        let start = end.saturating_sub(room);
        // What the last frame drew, for the key handler between frames - the same rule
        // `dec_content_room` follows for the decision card. The key handler has to answer
        // *where is the reader looking* and the only honest source is the glass.
        self.view_top = start;
        self.view_room = room;
        // The borrow is over - nothing below reads `segs` - so what follows is the head's
        // own state rather than the frame's.
        let mut out = take_window(&segs, start, end);
        // Which of these rows are about a file, for a click to open (see `file_rows`). Only
        // the history's own lines are rows of the conversation with a span behind them - the
        // live pane and the tail below them are not - so the map stops where `hist_lines` does.
        self.file_rows = self.file_rows_in(start, end);
        // And which of them are lines of the open payload window, for a click to take the
        // reader there (see `payload_rows`) - the same walk over the same spans, so the line a
        // click names is the line the frame drew.
        self.payload_rows = self.payload_rows_in(start, end);
        // And which are a job's completion notice, for a click to open whole (see
        // `notice_rows`) - the same walk, the same window.
        self.notice_rows = self.notice_rows_in(start, end);
        // And which of them are a finished subagent's line, for a click to go to that child
        // (`task_rows`) - the same walk, and the ids it carries are the drawing's own.
        let tasks = self.task_rows_in(start, end);
        self.task_switches = tasks.iter().map(|(_, s)| s.clone()).collect();
        self.task_rows = tasks
            .iter()
            .enumerate()
            .map(|(k, (y, _))| (*y, k))
            .collect();
        // And which are a red target's warning, drawn as one line (`red_card`).
        self.card_rows = self.card_rows_in(start, end);
        // And which of them are rows of a call the turn is still drawing: the live pane's cards, keyed by the
        // CALL and not by their text, which changes as the call goes from `Running` to `Finished`. See
        // [`App::call_rows`].
        self.call_rows = cards
            .into_iter()
            .filter(|(line, _)| *line >= start && *line < end)
            .map(|(line, id)| (line - start, id))
            .collect();
        // And the clauses of the run markers, for a click to open that kind (`runclick`).
        self.marker_rows = self.marker_rows_in(start, end);
        // And which of them are a picture's thumbnail rows, for a click - or Enter - to open
        // the picture larger (`app::pictures`). Read off the same window and the same spans, so the
        // row a click lands on is the row the frame drew the picture in.
        let pictures = self.picture_rows_in(start, end);
        self.picture_refs = pictures.iter().map(|(_, p)| p.clone()).collect();
        self.picture_rows = pictures
            .iter()
            .enumerate()
            .map(|(k, (y, _))| (*y, k))
            .collect();
        // And the live pane's own marker - the turn in flight is drawn below the history, so
        // its clauses are read off this frame's rows (`live_marker_rows`).
        let live = self.live_marker_rows(start, &out);
        self.marker_rows.extend(live);
        // The disclosure is about the hold and not about a number: `scroll > 0` ties the sentence to a derived
        // count, so a hold whose row sits near the end of a shortened transcript has none and the reader is left
        // holding a viewport with nothing saying so.
        if !self.following()
            && out.len() > 2
            && let Some(pin) = self.pinned_prompt(start)
        {
            out[0] = pin;
            self.file_rows.retain(|(r, _)| *r != 0);
            self.notice_rows.retain(|(r, _)| *r != 0);
            self.task_rows.retain(|(r, _)| *r != 0);
            self.card_rows.retain(|(r, _)| *r != 0);
            self.call_rows.retain(|(r, _)| *r != 0);
            self.marker_rows.retain(|m| m.y != 0);
            self.picture_rows.retain(|(r, _)| *r != 0);
            self.payload_rows.retain(|(r, _)| *r != 0);
        }
        if !self.following() && !out.is_empty() {
            let behind = total - end;
            let last = out.len().saturating_sub(1);
            // The state, and the act that undoes it - the rule for a disclosure, and
            // for the reader who cannot tell pinned from following.
            out[last] = row(&rano::agent::screens::holding(behind), self.cfg.palette());
            // The banner took that row, so it is no longer a row of any file.
            self.file_rows.retain(|(r, _)| *r != last);
            self.notice_rows.retain(|(r, _)| *r != last);
            self.task_rows.retain(|(r, _)| *r != last);
            self.card_rows.retain(|(r, _)| *r != last);
            self.call_rows.retain(|(r, _)| *r != last);
            self.marker_rows.retain(|m| m.y != last);
            self.picture_rows.retain(|(r, _)| *r != last);
            self.payload_rows.retain(|(r, _)| *r != last);
        }
        out
    }
}

impl App {
    /// The prompt of the turn at line `top` of the history, as one pinned row, or `None` when the prompt's own
    /// first line is still on screen. The turn is found from the row at the window's top by the span map,
    /// walking back to the nearest row the person wrote.
    pub(crate) fn pinned_prompt(&self, top: usize) -> Option<String> {
        let at_top = self.spans.iter().rev().find(|sp| sp.at <= top)?;
        let (row, it, text) = (0..=at_top.row).rev().find_map(|k| {
            let it = self.items.get(k)?;
            match it.item.as_ref()? {
                letibot_transcript::TranscriptItem::User {
                    speaker: letibot_transcript::Speaker::Operator,
                    parts,
                } => {
                    let text = parts
                        .iter()
                        .filter_map(|p| match p {
                            letibot_transcript::UserPart::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    Some((k, it, text))
                }
                _ => None,
            }
        })?;
        // Still on screen: its first line is at or below the window's top. A prompt with no
        // span at all is above the rows a tail frame rendered - off screen by definition.
        if self
            .spans
            .iter()
            .find(|sp| sp.row == row)
            .is_some_and(|sp| sp.at >= top)
        {
            return None;
        }
        let stamp = clock_time(it.ts);
        let stamp_w = visible_width(&stamp);
        let cols = self
            .cfg
            .width
            .max(20)
            .saturating_sub(2 + stamp_w + usize::from(!stamp.is_empty()))
            .max(8);
        let one = trim_to(&text.split_whitespace().collect::<Vec<_>>().join(" "), cols);
        user_block(&one, it.ts, &self.cfg).into_iter().next()
    }
}

/// A run of body lines: history is borrowed from the head's own buffer, the
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
    /// The turn's in-flight work, read before the walk's disjoint borrow: the destructure below leaves `turn`
    /// out, so this is the one place the walk can see it, and it must, since a call in flight is not a row and
    /// the counts have to carry it.
    #[allow(clippy::too_many_arguments)]
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
        // The stack the transcript's cards are asked of: built only when a layer ON DISK answers
        // `card_`, because the head's own facts and readers draw what the built-in layer draws and a
        // stack is a wasm compile per built-in. Asked before the destructure takes the head apart.
        self.note_card_layers();
        if self.card_layers_on() {
            let _ = self.plugin_host();
        }
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
            job_edits,
            call_decisions,
            hist_class,
            hist_first_class,
            hist_floor,
            hist_ceiling,
            walk_ahead,
            hist_renders,
            diff_split,
            payload_sel,
            payload_page,
            payload_max,
            screen_rows,
            bound_prompts,
            unconfirmed,
            spans,
            note_cards,
            visibility,
            lift,
            run_tools,
            dismissed,
            quiet,
            read_marks,
            sessions_host,
            ..
        } = self;
        let diff_split = *diff_split;
        // The cards layer, built here because the destructure below leaves `self` apart: a row's card
        // is asked of the SAME stack the picker asks, so a layer installed is a layer answering every
        // row of the conversation too. Borrowed per row - the ask is cached inside it by row id.
        let cards_host = sessions_host.borrow();
        let cards_host = cards_host.as_ref();
        // The runs and the ids, once per frame: the questions below are per row, and
        // re-deriving them from `items` per row is what made a long hidden run
        // quadratic (R123.3).
        let runs = RunIndex::of(items, *visibility, bound_prompts, think);
        // The run `ctrl-v` opens, computed once per frame: the seam names the chord only on the run it acts on,
        // and asking per row would be a scan of the transcript for every row drawn.
        let newest_run = newest_unseen_run(&runs, live);
        // Where the window ends. The ring's own end while it has rows let go, and the session's
        // end once the budget `release_below` set is buying the reader the next screen of them: a
        // walk to `items.len()` with nothing stopped it would be the whole session.
        let stop = if *walk_ahead == usize::MAX {
            (*hist_ceiling).unwrap_or(items.len())
        } else {
            items.len()
        }
        .min(items.len());
        // The lines this frame's walk has added, against the budget `release_below` set.
        let mut added = 0usize;
        loop {
            // A note with no seam in this window is stepped over, not drawn: either it happened before the
            // conversation this head holds, or its seam is below `hist_floor` and its row is above the window.
            while let Some((place, _)) = notes.get(*note_upto) {
                let gone = match place {
                    Placed::Before => true,
                    Placed::Seam(at) => *at < *hist_floor,
                };
                if !gone {
                    break;
                }
                *note_upto += 1;
            }
            let note_next = notes
                .get(*note_upto)
                .is_some_and(|(place, _)| matches!(place, Placed::Seam(at) if *at <= *hist_upto));
            if note_next {
                // A retired note contributes nothing, not its text and not the blank above it: the reading has
                // happened, and skipping the blank is what makes the wall go rather than become a column of gaps.
                if !crate::app::note_retired(dismissed, quiet, read_marks, &notes[*note_upto].1) {
                    // Every note but a pane's ending is a card's one line, and a click opens it
                    // whole: where it is, kept for the click map, with the card's own title.
                    let n = &notes[*note_upto].1;
                    let lines = note_lines(cfg, n);
                    let popup = note_card(n).map(|(card, text, _)| (text, (card.title, card.role)));
                    if !hist_lines.is_empty() {
                        hist_lines.push(String::new());
                    }
                    if let Some((text, title)) = popup {
                        note_cards.push(crate::app::diff_popup::NoteCard {
                            at: hist_lines.len(),
                            lines: lines.len(),
                            text,
                            title,
                        });
                    }
                    hist_lines.extend(lines);
                    *hist_class = Some(RowClass::Other);
                }
                *note_upto += 1;
            // `hist_upto` is "every row at or below this index is accounted for": a tail walk sets it to the row
            // count, so this does nothing until a new row arrives at the end, which is the point. In tail mode there
            // are no marks, since they are indexed by absolute row and the walk never passes the skipped ones.
            } else if *hist_upto < items.len()
                && *hist_upto >= *hist_floor
                && items[*hist_upto].kind == "assistant"
                && items[*hist_upto].item.is_none()
                && in_flight.contains(&items[*hist_upto].item_id)
            {
                break;
            } else if *hist_upto < stop && *hist_upto >= *hist_floor {
                // Where the walk stands before this row, so a later "from row k on" comes back to exactly here.
                // Recorded for every row including one that renders to nothing, since the marks are indexed by row
                // and a gap would misalign every mark after it.
                if hist_marks.len() == *hist_upto {
                    hist_marks.push(HistMark {
                        lines: hist_lines.len(),
                        note_upto: *note_upto,
                        class: *hist_class,
                    });
                }
                // An assistant row carries the arguments for the calls it proposed, and the result rows after it
                // want the same label. Learning them here, in transcript order, is what lets a head that attached
                // after a turn say which file was read.
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
                // The run this row belongs to, and whether it is the open one: a row inside the open run draws
                // itself and the run's marker must not also be drawn, while a closed run's first row draws the
                // marker and the rows under it draw nothing.
                let reserve = reserved_for_run(
                    items,
                    &runs,
                    live,
                    cfg,
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
                let open_run = run_open_at(&runs, live, payload_sel.as_deref(), *hist_upto);
                // The marker is drawn whether the run is open or not: an open run's marker is the header of the
                // rows it lifted, its clauses are the toggle that puts it back, and the hint bar names the key.
                let unseen = unseen_run_at(&runs, live, *hist_upto);
                // No blank line in front of a marker: it continues the sentence above it, so the separator's
                // blank, which says *a new kind of thing starts here*, is the opposite of what it means. See
                // [`hidden_run_marker`].
                let joinable = unseen.is_some_and(|(start, _)| run_continues_prose(items, start));
                let tight = unseen.is_some() && joinable;
                // A row of an OPEN run draws itself under that header, and a row the click's
                // kind skips draws nothing (that is what `run_skips` is for). A CLOSED run's rows
                // are not lexed at all, which is the whole point of the marker.
                let skipped =
                    open_run && crate::app::runclick::run_skips(*lift, &items[*hist_upto]);
                let (body_class, body) = if (open_run || unseen.is_none()) && !skipped {
                    // This row's cards, asked of the layer once and kept by the row's id.
                    let cards = Cards::row(cards_host, &items[*hist_upto]);
                    item_lines(
                        &items[*hist_upto],
                        &ItemCtx {
                            cards: &cards,
                            cfg: &row_cfg,
                            pending: &self.pending_rows,
                            think: if open_run {
                                crate::app::runclick::run_think(*lift, think)
                            } else {
                                think
                            },
                            tools: if open_run { *run_tools } else { tool },
                            raw,
                            targets: call_targets,
                            answered: &answered,
                            subagents: &self.subagents,
                            job_tails: &self.job_tails,
                            drawn_live: in_flight.contains(items[*hist_upto].item_id.as_str()),
                            elapsed_ms: call_ms.get(&items[*hist_upto].item_id).copied(),
                            edits: call_edits
                                .get(&items[*hist_upto].item_id)
                                .map(Vec::as_slice)
                                .unwrap_or(&[]),
                            job_edits,
                            decision: call_decisions.get(&items[*hist_upto].item_id),
                            workspace: &self.wiring.workspace,
                            bound: bound_prompts
                                .get(&items[*hist_upto].item_id)
                                .map(String::as_str),
                            // A bound row is being drawn, so its mark is never `queued`: see [`echo_mark`]. This
                            // was the same `_ => QUEUED` as its sibling walk, and it is the one that drew the
                            // person's own line as still waiting.
                            echo_mark: bound_prompts
                                .get(&items[*hist_upto].item_id)
                                .map(String::as_str)
                                .map(|t| echo_mark(unconfirmed, t, true))
                                .unwrap_or(QUEUED),
                            // An open run lifts the rung for its own rows, which is what "it opens" means: the
                            // reader sees the rows the rung was hiding with their own headlines and diffs,
                            // rather than a second rendering of them.
                            vis: if open_run {
                                Visibility::lifted()
                            } else {
                                *visibility
                            },
                            diff_split,
                            // Rebuilt per row inside the walk, so it cannot be hoisted out of this borrow: it
                            // reads two fields the walk is holding. Closed for an open run, since that run is
                            // being read whole.
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
                            replied_to: question_before(items, *hist_upto).as_deref(),
                        },
                    )
                } else {
                    (RowClass::Other, Vec::new())
                };
                let (class, rows) = match unseen {
                    Some((start, end)) if start == *hist_upto => {
                        // See the backward walk's note: the run holding the turn's
                        // NEWEST row, which is the one the in-flight work continues.
                        let live_here = newest_run == Some(start)
                            && run_carries_live(items, turn_rows, start, end);
                        let marker = hidden_run_marker(
                            items,
                            start,
                            end,
                            *visibility,
                            cfg,
                            newest_run == Some(start),
                            MarkerFacts::of(live, newest_run),
                            live_here,
                            think,
                        );
                        // Glued to the sentence it continues when there is one: the last drawn row is
                        // [`RowClass::Speech`] and the joined line still fits the frame. Otherwise it stands alone,
                        // which is the honest degradation.
                        let painted = marker.painted(cfg);
                        let joined = if joinable && *hist_class == Some(RowClass::Speech) {
                            hist_lines
                                .iter()
                                .rposition(|l| !l.trim().is_empty())
                                // Idempotent: a line that already carries this marker is the one
                                // this head joined on an earlier frame, and a second copy of it
                                // would count the same run twice.
                                .map(|at| {
                                    let base = hist_lines[at].trim_end();
                                    if base.ends_with(painted.as_str()) {
                                        base.to_string()
                                    } else {
                                        format!("{base} {painted}")
                                    }
                                })
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
                                // The header went onto the sentence above; the rows it
                                // stands over still go in below it.
                                (
                                    if open_run {
                                        body_class
                                    } else {
                                        RowClass::Other
                                    },
                                    body,
                                )
                            }
                            None => {
                                let mut rows = vec![painted];
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
                        }
                    }
                    // A row of an open run - its header was drawn at the run's first row - and
                    // a row of a closed one, which draws nothing.
                    Some(_) => (body_class, body),
                    None => (body_class, body),
                };
                let before = hist_lines.len();
                // A row that rendered nothing gets no separator either: a tool-calling round's assistant row has
                // no visible text and its calls are drawn by their own result rows, and paying two blanks for it
                // puts a hole in the transcript.
                if !rows.iter().all(|l| l.trim().is_empty()) {
                    // Air where the kind changes and not between every pair of rows: two tool cards in a row read
                    // as one block, and a blank between them spends a third of the vertical budget separating what
                    // a glyph already separates.
                    let pack =
                        *hist_class == Some(RowClass::Activity) && class == RowClass::Activity;
                    if !hist_lines.is_empty() && !pack && !tight {
                        hist_lines.push(String::new());
                    }
                    if hist_first_class.is_none() {
                        *hist_first_class = Some(class);
                    }
                    // And where this row's lines begin. Recorded here rather
                    // than derived later, because a separator's blank line belongs to no
                    // row and only this walk knows it pushed one.
                    spans.push(Span {
                        row: *hist_upto,
                        at: hist_lines.len(),
                        lines: rows.len(),
                        class,
                    });
                    hist_lines.extend(rows);
                    *hist_class = Some(class);
                }
                *hist_upto += 1;
                added += hist_lines.len() - before;
                if added >= *walk_ahead && *hist_upto < items.len() {
                    // The window ends here. The rows below are held and not drawn until the reader
                    // comes down to them: this frame's budget is a screen, not the session.
                    *hist_ceiling = Some(*hist_upto);
                    break;
                }
            } else {
                break;
            }
        }
        // The buffer reaches the session's end, so nothing is being held back below it.
        if *hist_upto == items.len() {
            *hist_ceiling = None;
        }
    }
}
