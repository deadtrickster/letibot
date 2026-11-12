//! What the conversation is not showing: the markers that stand for hidden runs of rows,
//! and the live work under them.

use crate::app::*;
use crate::ui::render::{RenderConfig, visible_width};
use crate::ui::*;
use letibot_sessionlog::view::{CallState, SnapshotItem};
use letibot_transcript::TranscriptItem;

/// Does this set hide this row: the item-level answer is [`Visibility::keeps`], and this is the same test asked
/// about a row, which is what the walks, the run finder and [`App::hidden_by_rung`] need. A row with no body yet is
/// not hidden, since it is drawn from this head's own echo.
pub(crate) fn row_hidden(items: &[SnapshotItem], vis: Visibility, row: usize) -> bool {
    row_hidden_at(vis, items.get(row))
}

/// Does a row announced without its body draw a line of its own? Only a tool result does: a payload
/// past `ANNOUNCE_BYTES` is sent on a second journey, on purpose, and the row has a header for
/// meanwhile. A `user` row draws from this head's echo; the rest are a fork's, by the hundred.
pub(crate) fn carries_a_header(it: &SnapshotItem) -> bool {
    it.kind == "tool_result"
}

/// The same question about a row rather than a list index, so it can be asked about the live tail. `None` is a row
/// with no body yet, and it is not hidden: it is drawn from this head's echo when it has one.
pub(crate) fn row_hidden_at(vis: Visibility, it: Option<&SnapshotItem>) -> bool {
    if !vis.hides_the_working() {
        return false;
    }
    match it.and_then(|it| it.item.as_ref()) {
        Some(item) => !vis.keeps(item),
        // A row with no body is drawn from this head's echo, or - a tool result - as its own header.
        // A header is the working, so the rung that hides the working hides it with the rest.
        None => it.is_some_and(carries_a_header),
    }
}

/// Work this turn is doing right now that no row holds yet. The daemon appends a round's results after every call
/// has run, so between the narration and the first result the transcript holds no work at all, and a marker built
/// from rows counted nothing while a `cargo test` ran.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct LiveWork {
    /// Calls proposed or running with no settled result row yet - the NUMBER the marker carries.
    pub(crate) calls: usize,
    /// Calls still executing, the number the yellow is on: `calls` drops the moment a result row is
    /// held - that row draws its own header, body or not - while `running` says whether a call is
    /// executing now, so a call that finished a frame ago keeps its number and loses the colour.
    pub(crate) running: usize,
    /// Display lines of reasoning streamed this turn and not yet a row.
    pub(crate) think_lines: usize,
}

impl LiveWork {
    pub(crate) fn work(self) -> usize {
        self.calls + self.think_lines
    }
}

/// Does this row put anything on the screen at this rung: a run ends only at a row the rung draws, so an item
/// boundary, a hidden row or a whitespace-only text part does not break it. Contiguity belongs to the rendered
/// screen and not to the item list.
pub(crate) fn row_drawn(
    items: &[SnapshotItem],
    vis: Visibility,
    bound: &std::collections::HashMap<String, String>,
    row: usize,
) -> bool {
    items
        .get(row)
        .is_some_and(|it| row_drawn_at(vis, bound, it))
}

/// The same question for one row, or for the live tail: `None` there is the work in
/// flight, which draws nothing as a row and is therefore invisible - the whole of why the
/// counts must carry it.
pub(crate) fn row_drawn_at(
    vis: Visibility,
    bound: &std::collections::HashMap<String, String>,
    it: &SnapshotItem,
) -> bool {
    if !vis.hides_the_working() {
        return true;
    }
    if row_hidden_at(vis, Some(it)) {
        return false;
    }
    match it.item.as_ref() {
        None => bound.contains_key(&it.item_id),
        Some(letibot_transcript::TranscriptItem::Assistant { text, .. }) => !text.trim().is_empty(),
        _ => true,
    }
}

/// The stretches of rows the rung hides and where every row is, precomputed and handed to the
/// walks, so a per-row question is one lookup instead of a rescan of the run around it (R123.3:
/// one long hidden run made every walk over it quadratic this way).
pub(crate) struct RunIndex<'a> {
    /// Per row, the stretch of undrawn rows it sits in, or [`NO_STRETCH`]. Rows are bounded by
    /// the bytes a head holds, far below 2^32, so `u32` is the whole index.
    of_row: Vec<u32>,
    /// The stretches as `[start, end)`, ascending, each with whether it hides a row: the three
    /// facts [`unseen_run_at`] used to derive by walking, and nothing else.
    stretches: Vec<(usize, usize, bool)>,
    /// Row by id, first occurrence: [`run_open_at`] scanned the transcript per row.
    by_id: std::collections::HashMap<&'a str, usize>,
    len: usize,
    hides: bool,
}

/// A row that is drawn, or past the end: it is in no stretch.
const NO_STRETCH: u32 = u32::MAX;

impl<'a> RunIndex<'a> {
    /// One pass over the rows, asking the same predicates the per-row walks asked: a stretch is
    /// a maximal run of rows [`row_drawn_at`] refuses, and `hidden` is folded while it grows.
    pub(crate) fn of(
        items: &'a [SnapshotItem],
        vis: Visibility,
        bound: &std::collections::HashMap<String, String>,
    ) -> RunIndex<'a> {
        let hides = vis.hides_the_working();
        let mut of_row = vec![NO_STRETCH; items.len()];
        let mut stretches: Vec<(usize, usize, bool)> = Vec::new();
        let mut by_id = std::collections::HashMap::with_capacity(items.len());
        let mut open: Option<(usize, bool)> = None;
        for (r, it) in items.iter().enumerate() {
            by_id.entry(it.item_id.as_str()).or_insert(r);
            if hides && !row_drawn_at(vis, bound, it) {
                let hidden = row_hidden_at(vis, Some(it));
                match open.as_mut() {
                    Some((_, h)) => *h |= hidden,
                    None => open = Some((r, hidden)),
                }
            } else if let Some((start, hidden)) = open.take() {
                let idx = stretches.len() as u32;
                stretches.push((start, r, hidden));
                of_row[start..r].fill(idx);
            }
        }
        if let Some((start, hidden)) = open.take() {
            let idx = stretches.len() as u32;
            stretches.push((start, items.len(), hidden));
            of_row[start..items.len()].fill(idx);
        }
        RunIndex {
            of_row,
            stretches,
            by_id,
            len: items.len(),
            hides,
        }
    }

    /// The row an id names, first occurrence - what `position` answered, without the scan.
    pub(crate) fn row_of(&self, id: &str) -> Option<usize> {
        self.by_id.get(id).copied()
    }

    /// The stretch around `row` with its `hidden` flag, when the row is in one.
    fn stretch(&self, row: usize) -> Option<(usize, usize, bool)> {
        let idx = self.of_row.get(row).copied().filter(|i| *i != NO_STRETCH)?;
        Some(self.stretches[idx as usize])
    }
}

/// The stretch of rows the reader does not see that `row` sits in, as `[start, end)`, or `None` when this row is
/// visible. The boundary is [`row_drawn`] and not [`row_hidden`]: an invisible row is not a hidden one, and an
/// assistant row with no prose is exactly what split one run into eight. The bounds are [`RunIndex`]'s.
pub(crate) fn unseen_run_at(runs: &RunIndex, live: LiveWork, row: usize) -> Option<(usize, usize)> {
    let (start, end, hidden) = runs.stretch(row)?;
    // The live tail is contiguous with a stretch that runs to the end of the transcript: the work in flight comes
    // after every row there is, so it belongs to the last stretch and to nothing else. A stretch that hides nothing
    // draws no marker at all.
    let reaches_the_turn = end == runs.len && live.work() > 0;
    (hidden || reaches_the_turn).then_some((start, end))
}

/// Is this row inside the run that is currently open? Opening a run is not a second rendering of it but the rung
/// lifted for those rows and no others, so the reader gets the very rows the rung was hiding from the one renderer
/// that always drew them.
pub(crate) fn run_open_at(runs: &RunIndex, live: LiveWork, open: Option<&str>, row: usize) -> bool {
    let Some(id) = open.filter(|id| !id.is_empty()) else {
        return false;
    };
    // The live tail is not a row, and this is the question about a row. Answering it for the sentinel made EVERY row
    // of the transcript read as inside the open run, so a thinking click hid every tool row in the session.
    if id == LIVE_RUN {
        return false;
    }
    let Some(start) = runs.row_of(id) else {
        return false;
    };
    unseen_run_at(runs, live, start).is_some_and(|(s, e)| row >= s && row < e)
}

/// The run with no row yet: the sentinel `App::payload_sel` carries for the work in flight, so the marker standing
/// for it can name a chord and the chord can reach it. One constant, and it is `app::runclick`'s, since two
/// spellings made a panel opened by a click not the panel `ctrl-t` opened.
pub(crate) use crate::app::runclick::LIVE_RUN;

/// The newest run of invisible rows, by its first row, which is what `ctrl-t` opens: there is no cursor in this
/// head, so exactly one run can be addressed by a chord and the one a reader means is the newest. A chord may only
/// be named where it acts.
pub(crate) fn newest_unseen_run(runs: &RunIndex, live: LiveWork) -> Option<usize> {
    if !runs.hides {
        return None;
    }
    // A run with rows comes first and the live tail only when there is none: the fold decides which run carries the
    // work, since `hidden_run_marker` folds the live counts into the run holding one of the current turn's rows.
    // Preferring a run with rows without that gate is the trap; the stretches are ascending, so the last is newest.
    let newest = runs
        .stretches
        .iter()
        .rev()
        .find(|&&(_, end, hidden)| hidden || (end == runs.len && live.work() > 0))
        .map(|&(start, _, _)| start);
    // And only when nothing has rows: the work in flight is its own run, addressed by the one
    // index past the end and drawn by the live pane.
    newest.or_else(|| (live.work() > 0).then_some(runs.len))
}

/// Is the work in flight already counted by a run that has rows? True when the last row is invisible, since the
/// stretch it sits in runs to the end and [`unseen_run_at`] has folded the tail in. False when the last row is
/// prose or there are none, and then the live pane draws its own marker.
pub(crate) fn live_tail_covered(
    items: &[SnapshotItem],
    vis: Visibility,
    bound: &std::collections::HashMap<String, String>,
) -> bool {
    // `vis.hides_the_working()` and NOT its negation: read as `!`, the term was false in exactly the case the
    // function exists for, so every caller took the live work for a run of its own and one turn was drawn by two
    // markers.
    vis.hides_the_working() && items.last().is_some_and(|it| !row_drawn_at(vis, bound, it))
}

/// Does the work in flight belong to this run: it does when the run reaches the end of the transcript, or when it
/// holds one of the turn's own rows. One place, because three callers ask it. Neither clause merges two runs, since
/// [`unseen_run_at`] still ends a run at every row the reader sees.
pub(crate) fn run_carries_live(
    items: &[SnapshotItem],
    turn_rows: &std::collections::HashSet<String>,
    start: usize,
    end: usize,
) -> bool {
    end == items.len()
        || ((start..end).any(|r| turn_rows.contains(items[r].item_id.as_str()))
            && !(end..items.len()).any(|r| model_prose(&items[r])))
}

/// A row that is the model's reply as the reader sees it: an assistant row with text.
fn model_prose(it: &SnapshotItem) -> bool {
    matches!(
        it.item.as_ref(),
        Some(letibot_transcript::TranscriptItem::Assistant { text, .. }) if !text.trim().is_empty()
    )
}

/// Does the model's own sentence introduce this run: the marker continues the sentence the colon points at, and
/// that is the model's narration, never the person's message. A run continued by prose only when the row above is
/// an assistant row with text the reader can see.
pub(crate) fn run_continues_prose(items: &[SnapshotItem], start: usize) -> bool {
    start > 0
        && matches!(
            items[start - 1].item.as_ref(),
            Some(letibot_transcript::TranscriptItem::Assistant { text, .. })
                if !text.trim().is_empty()
        )
}

/// Does a run begin at the very next row, and what would its marker say: the marker is appended to the last line of
/// the sentence it continues, which leaves whatever width the prose had, and a join that does not fit stands alone
/// instead.
pub(crate) fn reserved_for_run(
    items: &[SnapshotItem],
    runs: &RunIndex,
    live: LiveWork,
    _cfg: &RenderConfig,
    row: usize,
    newest: bool,
) -> Option<usize> {
    if !runs.hides || row + 1 >= runs.len {
        return None;
    }
    let (start, end) = unseen_run_at(runs, live, row + 1)?;
    if start != row + 1 {
        return None;
    }
    // Only when the counts will actually be glued to this row. A marker that stands on
    // its own line needs no room left for it, and narrowing the prose for one would be a
    // sentence wrapped short for nothing.
    if !run_continues_prose(items, start) {
        return None;
    }
    // The prose is NOT narrowed for the marker: reserving its room wrapped every sentence early for the sake of a
    // marker that is almost never that wide. What the marker may claim is [`marker_room`]'s ceiling.
    let _ = (items, start, end, newest, live);
    None
}

/// The seam, which is the one thing here that is not a count: a marker that opens must name the chord where it acts,
/// so the newest run's names the chord and every other one names the verb that reaches it. One string to delete if
/// the operator rules the sentence better without it.
pub(crate) fn marker_seam_rung(newest: bool, rung: usize) -> &'static str {
    if !MARKER_SEAM {
        return "";
    }
    let (chord, verb) = SEAM_RUNGS[rung.min(SEAM_RUNGS.len() - 1)];
    if newest { chord } else { verb }
}

/// The calls this round's result rows have answered: a result row among [`TurnPane::appended`] answers one of that
/// round's calls, which is what the marker's colour reads. Scoped to the round, since the ids are round-positional,
/// and keyed on the id, since the positional handover counts rows.
pub(crate) fn round_answered<'a>(t: &TurnPane, items: &'a [SnapshotItem]) -> Vec<&'a str> {
    t.appended
        .iter()
        .filter_map(|id| {
            items
                .iter()
                .find(|it| &it.item_id == id)
                .and_then(|it| match &it.item {
                    Some(TranscriptItem::ToolResult { call_id, .. }) => Some(call_id.as_str()),
                    _ => None,
                })
        })
        .collect()
}

/// How many of this round's calls the transcript has taken over: one per held tool-result row, in the
/// order the calls were made. A bodyless row counts - it draws its own header - while a row that HAS a
/// body must name its call, so a row out of another round cannot hand this one's call over.
pub(crate) fn settled_rows(t: &TurnPane, items: &[SnapshotItem]) -> usize {
    let held: Vec<&SnapshotItem> = t
        .appended
        .iter()
        .filter_map(|id| {
            items
                .iter()
                .find(|it| &it.item_id == id && it.kind == "tool_result")
        })
        .collect();
    let mut n = 0;
    while let Some(call) = t.calls.get(n) {
        let Some(row) = held.get(n) else { break };
        if let Some(TranscriptItem::ToolResult { call_id, .. }) = &row.item
            && call_id != &call.call_id
        {
            break;
        }
        n += 1;
    }
    n
}

/// How the live pane knows what is in flight: calls proposed or running with no result row yet, and reasoning
/// streamed and not yet committed. Recomputed every frame from the turn, which is the only place it exists.
pub(crate) fn live_work(
    turn: Option<&TurnPane>,
    items: &[SnapshotItem],
    cfg: &RenderConfig,
    superseded: bool,
) -> LiveWork {
    let Some(t) = turn else {
        return LiveWork::default();
    };
    if superseded {
        return LiveWork::default();
    }
    // The calls the transcript has not taken over yet: `settled_calls` advances when a result row's BODY lands and
    // not when the call finishes, which is why this is not a filter on the call's own state.
    let calls = t.calls.len().saturating_sub(t.settled_calls);
    // Executing, and `Proposed` is not executing yet: a call the model wrote but the daemon has not started is work
    // the marker counts and not work that is happening, so the colour is on the calls alone and never on the
    // brackets or the thinking count.
    let answered = round_answered(t, items);
    let running = t
        .calls
        .iter()
        .filter(|c| matches!(c.state, CallState::Running))
        .filter(|c| !answered.contains(&c.call_id.as_str()))
        .count();
    // The thinking no row holds, which is the whole of `t.reasoning`, since the pane's buffer is emptied as its
    // rows land. Slicing it at a byte mark trimmed the number while the drawing kept the whole buffer, so a landed
    // thought was drawn twice with the fold open.
    let think_lines = if t.reasoning.is_empty() {
        0
    } else {
        reasoning_display_lines(t.reasoning.raw(), cfg.width)
    };
    LiveWork {
        calls,
        running,
        think_lines,
    }
}

/// Everything the marker draws about now, plus the row the work is in, as one value: the facts are what
/// [`hidden_run_marker`] reads, and the same value is the cache key for the row the marker is painted into. They
/// must not come apart.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct MarkerFacts {
    /// The live turn's work, exactly as [`live_work`] reports it - the numbers the marker
    /// prints and the `running` that lights them.
    pub(crate) live: LiveWork,
    /// The row of the run this work belongs to, from `newest_unseen_run`, or `None` while
    /// the work has committed no row yet.
    pub(crate) run: Option<usize>,
}

impl MarkerFacts {
    /// The facts as the two inputs report them. One constructor, so every caller fills the value from the same pair
    /// and cannot hand the renderer a value the cache key does not hold.
    pub(crate) fn of(live: LiveWork, run: Option<usize>) -> MarkerFacts {
        MarkerFacts { live, run }
    }
}

/// The counts as the two halves that can be painted differently: split rather than composed, because the live-edge
/// colour goes on the calls count alone and never on the brackets or the thinking count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Counts {
    /// The number and its noun, kept apart: the yellow is on the digits alone, so the two halves cannot be one
    /// string. The other head splits the same clause into segments for the same reason.
    pub(crate) calls: Option<(String, String)>,
    /// `2 thinking lines`. Never coloured; a thought is not work that is still happening.
    pub(crate) think: Option<(String, String)>,
    /// `2 head events`: the fallback that keeps `[]` off the screen, and never coloured, since these are not work
    /// that is happening but rows the rung hid that neither of the other two numbers describe. See [`COUNT_RUNGS`].
    pub(crate) events: Option<(String, String)>,
}

/// The widest room a marker may claim from the sentence it continues, leading space included: measured against the
/// longest marker a realistic run writes, which is 54 columns plus the space in front.
pub(crate) const MARKER_ROOM_MAX: usize = 56;

/// The least room a marker may claim, however narrow the frame. Below about twenty columns the
/// two clauses stop being readable at all - `[100t, 246l]` and its separator are twelve - and a
/// marker that cannot be read is a marker that did nothing. leticl's `+hidden-run-marker-floor+`.
pub(crate) const MARKER_ROOM_FLOOR: usize = 22;

/// How many columns of `cols` the marker may occupy, its leading space included: fixed for a frame, which is the
/// point. At most half the frame, so a narrow terminal keeps half its line for the sentence, and never less than
/// the smallest rung.
pub(crate) fn marker_room(cols: usize) -> usize {
    cols.min(MARKER_ROOM_MAX)
        .min(MARKER_ROOM_FLOOR.max(cols / 2))
}

/// The count clause, most-spelled first: `2 tool calls`, `2 tools`, `2 calls`, `2t`. Growth is paid in words and
/// not in layout, so a marker that outgrows its room steps down instead of moving the wrap.
pub(crate) const COUNT_RUNGS: [(&str, &str, &str, &str, &str, &str); 4] = [
    // (calls one, calls many, thinking one, thinking many, events one, events many)
    (
        " tool call",
        " tool calls",
        " thinking line",
        " thinking lines",
        " head event",
        " head events",
    ),
    (
        " tool",
        " tools",
        " thinking",
        " thinking",
        " event",
        " events",
    ),
    (" call", " calls", " line", " lines", " event", " events"),
    ("t", "t", "l", "l", "e", "e"),
];

/// Whether the marker names its own key: off, and that is the operator's ruling, so the seam names the chord only
/// where it acts and the sentence carries the counts alone.
pub(crate) const MARKER_SEAM: bool = false;

/// The seam, most-spelled first: the whole chord, the chord alone, nothing. Dropped last of the three ladders,
/// since it is the only thing on the line saying the rows can be opened at all.
pub(crate) const SEAM_RUNGS: [(&str, &str); 3] = [
    (" · ctrl-v opens it", " · /verbosity"),
    (" · ctrl-v", " · /verbosity"),
    ("", ""),
];

/// The order the two ladders are spent in: the counts step down through every rung first, and only then does the
/// seam start to go. Counts before seam, because the counts are the fact the line exists to carry.
pub(crate) const MARKER_LADDER: [(usize, usize); 6] =
    [(0, 0), (1, 0), (2, 0), (3, 0), (3, 1), (3, 2)];

impl Counts {
    /// The counts spelled at one rung of the ladder - see [`COUNT_RUNGS`].
    pub(crate) fn at_rung(calls: usize, think_lines: usize, events: usize, rung: usize) -> Counts {
        let (c1, cn, t1, tn, e1, en) = COUNT_RUNGS[rung.min(COUNT_RUNGS.len() - 1)];
        // The number, then the suffix - which is where the space lives, and why the last rung has
        // none. See [`COUNT_RUNGS`].
        // The number and its noun, apart. See [`Counts`].
        let count = |n: usize, one: &str, many: &str| {
            (n.to_string(), if n == 1 { one } else { many }.to_string())
        };
        Counts {
            calls: (calls > 0).then(|| count(calls, c1, cn)),
            think: (think_lines > 0).then(|| count(think_lines, t1, tn)),
            events: (events > 0).then(|| count(events, e1, en)),
        }
    }

    /// The clauses this marker draws, in order - the two counts, or the fallback when neither
    /// of them can describe the run. See [`COUNT_RUNGS`]: leticl branches exactly here, drawing the
    /// events clause only when the other two are empty.
    pub(crate) fn clauses(&self) -> Vec<&(String, String)> {
        if self.calls.is_none() && self.think.is_none() {
            self.events.iter().collect()
        } else {
            [self.calls.as_ref(), self.think.as_ref()]
                .into_iter()
                .flatten()
                .collect()
        }
    }

    /// The counts as PLAIN text - for measuring, and for a test that wants the words rather
    /// than the registers. Never for drawing: a caller that painted this would be painting the
    /// brackets and the thinking count along with the number.
    pub(crate) fn plain(&self) -> String {
        let words: Vec<String> = self
            .clauses()
            .into_iter()
            .map(|(n, noun)| format!("{n}{noun}"))
            .collect();
        format!("[{}]", words.join(", "))
    }
}

/// Does this marker carry the work that is in flight? The colour has been wrong in three directions: keyed on the
/// work in flight at the card it flickered off in the gap between a call finishing and its row landing, and keyed
/// on the turn every round's marker lit at once.
pub(crate) fn marker_carries_live(live: LiveWork) -> bool {
    // `running` and not `calls`: the count of calls with no result row is the number the marker carries, and only a
    // call still executing lights it. A call that finished a frame ago keeps its number and loses the colour.
    live.running > 0
}

/// A struct rather than a `String` because the register is per half: passing the composed text
/// around and splitting it at paint time would be a second definition of where the counts stop
/// - and the counts are a string the head builds, so nothing may search them for a delimiter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Marker {
    /// `[1 tool call, 2 thinking lines]` - the content, drawn plain except for the calls count.
    pub(crate) counts: Counts,
    /// ` · ctrl-v opens it` - an affordance, drawn faint. Dot included.
    pub(crate) seam: &'static str,
    /// Does this marker carry the work in flight - [`marker_carries_live`], decided by the
    /// caller because only the caller knows which row it is drawing.
    pub(crate) live: bool,
}

impl Marker {
    /// The marker spelled to fit the room it was given: the counts and the seam step down until the marker fits
    /// `room - 1`, and the last rung's spelling is used whatever its width. See [`MARKER_LADDER`].
    pub(crate) fn new(
        calls: usize,
        think_lines: usize,
        events: usize,
        newest: bool,
        live: bool,
        room: usize,
    ) -> Marker {
        let limit = room.saturating_sub(1).max(1);
        let mut chosen = MARKER_LADDER[0];
        for (count_rung, seam_rung) in MARKER_LADDER {
            chosen = (count_rung, seam_rung);
            let counts = Counts::at_rung(calls, think_lines, events, count_rung);
            let seam = marker_seam_rung(newest, seam_rung);
            if visible_width(&format!("{}{seam}", counts.plain())) <= limit {
                break;
            }
        }
        Marker {
            counts: Counts::at_rung(calls, think_lines, events, chosen.0),
            seam: marker_seam_rung(newest, chosen.1),
            live,
        }
    }

    /// Painted - the counts plain (the calls count PENDING when this marker carries the live
    /// work), the seam faint. See [`marker_painted`].
    pub(crate) fn painted(&self, cfg: &RenderConfig) -> String {
        marker_painted(cfg, &self.counts, self.seam, self.live)
    }
}

/// The marker painted: the counts plain, the seam faint, and the calls count pending while the work is in flight.
/// The counts are punctuation inside a sentence, and the seam is the aside, so dim is for the part a reader could
/// delete.
pub(crate) fn marker_painted(
    cfg: &RenderConfig,
    counts: &Counts,
    seam: &str,
    live: bool,
) -> String {
    let marker = rano::agent::blocks::RunMarker {
        calls: counts.calls.clone(),
        think: counts.think.clone(),
        events: counts.events.clone(),
        seam: seam.to_string(),
        live,
    };
    crate::ui::render::row(&marker.line(), cfg.palette())
}

// A marker is drawn from the run's own facts, each already computed by the caller that owns it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn hidden_run_marker(
    items: &[SnapshotItem],
    start: usize,
    end: usize,
    vis: Visibility,
    cfg: &RenderConfig,
    newest: bool,
    // The facts and not `live`: `facts.live` is everything this marker reads about now, and the same value is the
    // cache key for the row, so what the marker draws is what the key holds.
    facts: MarkerFacts,
    // Is the work in flight this run's: the fact that decides whether the in-flight counts continue this marker.
    // Computed by the callers through [`run_carries_live`], which is where the two clauses live.
    live_here: bool,
) -> Marker {
    let live = facts.live;
    let mut calls = 0usize;
    let mut think_lines = 0usize;
    let mut events = 0usize;
    // Per row, and only the rows this rung actually hides: a run normally starts at an assistant row with no prose,
    // which is invisible and not hidden, so a guard keyed on the start would count nothing.
    for r in start..end {
        if !row_hidden(items, vis, r) {
            continue;
        }
        match items[r].item.as_ref() {
            Some(letibot_transcript::TranscriptItem::ToolResult { .. }) => calls += 1,
            Some(letibot_transcript::TranscriptItem::Reasoning { text, .. }) => {
                think_lines += reasoning_display_lines(text, cfg.width);
            }
            // A hidden row of any other kind, a system update or a segment mark, is an event: it is counted
            // by no other arm, so a run of nothing but these would draw `[]`.
            _ => events += 1,
        }
    }
    // And the work in flight when this run is the one it belongs to: a stretch reaching the end of the transcript
    // is where the turn is, so the counts move as the round runs.
    let carries_live = live_here && marker_carries_live(live);
    if live_here {
        calls += live.calls;
        think_lines += live.think_lines;
    }
    Marker::new(
        calls,
        think_lines,
        events,
        newest,
        carries_live,
        marker_room(cfg.width),
    )
}
