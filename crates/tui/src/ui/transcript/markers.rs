//! **What the conversation is not showing**: the markers that stand for hidden runs of rows,
//! and the live work under them.

use crate::app::*;
use crate::ui::render::{RenderConfig, visible_width};
use crate::ui::*;
use letibot_sessionlog::view::{CallState, SnapshotItem};
use letibot_transcript::TranscriptItem;
use letibot_ui::painter::Sgr;
use rano::style::Role;

/// **Does this set hide this row** — R37, the row-level question in one place.
///
/// The item-level answer is [`Visibility::keeps`]; this is the same test asked about a row,
/// which is what the walks, the run finder and [`App::hidden_by_rung`] all need.
///
/// A row with no body yet is **not** hidden: it is drawn from this head's own echo of what
/// the operator typed, and that is the conversation. A row the head never got an item for
/// cannot be judged, and the honest default for unjudgeable is *show it*.
pub(crate) fn row_hidden(items: &[SnapshotItem], vis: Visibility, row: usize) -> bool {
    row_hidden_at(vis, items.get(row))
}

/// **The same question about a row rather than about a list index** — so it can be asked
/// about the live tail, which is the work after every row there is.
///
/// `None` is a row with no body yet, and it is **not hidden**: it is drawn from this head's
/// own echo of what the operator typed when it has one, and a row nobody can read is not the
/// working this set is about.
pub(crate) fn row_hidden_at(vis: Visibility, it: Option<&SnapshotItem>) -> bool {
    if !vis.hides_the_working() {
        return false;
    }
    match it.and_then(|it| it.item.as_ref()) {
        Some(item) => !vis.keeps(item),
        None => false,
    }
}

/// **Work this turn is doing right now that no row holds yet** — R37 AMENDED.
///
/// The operator, watching a long `cargo test` behind a `blabal:` line: *"display looks
/// frozen, while in fact it is just say cargo testing with yellow dot. which means the
/// `[6 tool calls, 7 thinking lines]` must somehow show if there is a tool call or thinking
/// in flight. and obviously be updated earlier, even for the empty card."*
///
/// They are right, and the reason is structural. A round's result rows are appended **after
/// every call in the round has run** (`harnessd::harness`: `for call in &calls`, then one
/// `append_items`), so a call in flight is not a row, is counted by no marker, and at this
/// rung is not drawn either — the screen showed the narration and then nothing until the
/// result landed. The head cannot derive any of it from the transcript, because the
/// transcript does not have it yet; but it is watching the turn, so it knows.
///
/// The same is true of the reasoning a turn has streamed and not yet committed: the deltas
/// arrive, the row does not.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct LiveWork {
    /// Calls proposed or running with no settled result row yet — **the NUMBER the marker carries**.
    pub(crate) calls: usize,
    /// **Calls still EXECUTING — the number the YELLOW is on.** Two different facts, and conflating
    /// them is what stuck a counter yellow:
    ///
    /// * `calls` counts what the marker says it has to count: a call leaves this number at the
    ///   moment its result row starts being counted, which is when the row's BODY lands. That is
    ///   also what keeps the number from going DOWN — *"2 tool calls dropping to 1 and then
    ///   changing back to 2"*, the operator's own measurement of the first version.
    /// * `running` is *is a call executing right now*. A call that has FINISHED while its row is
    ///   still a frame away keeps the number and must lose the colour, because the yellow says
    ///   *happening* and nothing is.
    ///
    /// **And the same handover in the other direction, which is the half that stuck the yellow ON
    /// for the rest of the session:** a call whose row HAS landed is over, whatever its state says,
    /// so `running` is never counted over the whole pane — a call this round's transcript has
    /// answered is not asked ([`round_answered`]). The two numbers are then about one set of calls,
    /// which is what lets the marker's colour be a fact about the marker's own number.
    ///
    /// leticl keeps both and says exactly why: *"The colour is a different fact — is a call still
    /// executing — and that is `:running`, which `marker-rising-p` reads. A finished call whose row
    /// is still in flight keeps the number and drops the yellow, which is what the screen should
    /// say about it."* Its own version of the two cares only about the calls with no result row yet
    /// either: `:calls` is `%hidden-run-live-work`'s count over `call-answered-p`, whose answer is
    /// *this call has a row* — set when the row's BODY lands and from a snapshot's rows
    /// (`src/cards/hidden-run.lisp:173-177`, `src/cards/roles.lisp:374`, `src/session/seq-gap.lisp:319`,
    /// `src/cards/targets.lisp:433`).
    pub(crate) running: usize,
    /// Display lines of reasoning streamed this turn and not yet a row.
    pub(crate) think_lines: usize,
}

impl LiveWork {
    pub(crate) fn work(self) -> usize {
        self.calls + self.think_lines
    }
}

/// **Does this row put anything on the SCREEN at this rung** — R37 AMENDED's boundary.
///
/// The operator ran the rung and got **eight markers in a row with no prose between any
/// of them**: *"too many tool/thinking lines — they should all coalece to one."* Every
/// marker's content was right; what was wrong was how many there were, and the cause was
/// that the run was being broken by something the reader **cannot see**. An empty assistant
/// part, a tool_result between a call and the next call, a row whose body has not arrived —
/// each ended a run and started a new marker, and the result was the wall the rung exists
/// to abolish, now with brackets.
///
/// So: **a run ends only at a row this rung actually draws.** Prose the reader can see
/// ends a run. Nothing else does — not an item boundary, not a hidden row of another kind,
/// not an empty or whitespace-only text part. **Contiguity is a property of the rendered
/// screen, not of the item list**, and computing it over the list is the same class of
/// error as counting lines instead of anchoring to a row, which is R36.
///
/// # The two rows that are kept but draw nothing, which is the whole of the bug
///
/// `Verbosity::keeps` keeps `User` and `Assistant`. Of those:
///
/// * an **`Assistant` row draws only its prose** at this rung — the loop over `tool_calls`
///   is filtered out for it, so a row with no text and three calls renders to *nothing*.
///   That is the common shape of a tool-calling round, which is to say it is the run.
/// * a row whose **body has not arrived** draws nothing either, and draws it deliberately
///   (`item_lines`' own comment: *"a row with no body is not information"*). It draws an
///   echo only when this head has bound one to it.
///
/// The prose test is on the TEXT rather than on the rendered lines, and that is a choice
/// worth naming: the renderer's own test is `!prose.iter().all(|l| l.trim().is_empty())`,
/// and whitespace text lexes to whitespace lines. Lexing every row to ask the question would
/// put a second renderer in the walk's inner loop; `trim()` answers it for every text the
/// two can disagree about, and it errs toward *visible*, which errs toward *one more marker*
/// rather than toward an invisible run.
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

/// The same question for one row, or for **the live tail**: `None` there is the work in
/// flight, which draws nothing as a row and is therefore invisible — the whole of why the
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

/// **The stretch of rows the reader does not see, that `row` sits in** — as `[start, end)`,
/// and `None` when the reader sees this row — R37 AMENDED.
///
/// The boundary is [`row_drawn`] and not [`row_hidden`]: a stretch of *invisible* rows is
/// what the marker stands for, and an invisible row is not the same thing as a row the rung
/// hides. An assistant row with no prose is invisible and is not hidden, and it is exactly
/// what was splitting one run into eight.
///
/// A stretch with **nothing hidden in it draws no marker** — a line saying `[]` over a few
/// blank rows would be a sentence about nothing. That case arises from the fix itself: the
/// old boundary could not produce an all-visible stretch, and the new one can.
pub(crate) fn unseen_run_at(
    items: &[SnapshotItem],
    vis: Visibility,
    bound: &std::collections::HashMap<String, String>,
    live: LiveWork,
    row: usize,
) -> Option<(usize, usize)> {
    if !vis.hides_the_working() || row >= items.len() || row_drawn(items, vis, bound, row) {
        return None;
    }
    let mut start = row;
    while start > 0 && !row_drawn(items, vis, bound, start - 1) {
        start -= 1;
    }
    let mut end = row + 1;
    while end < items.len() && !row_drawn(items, vis, bound, end) {
        end += 1;
    }
    let hidden = (start..end).any(|r| row_hidden(items, vis, r));
    // **The live tail is contiguous with a stretch that runs to the end of the transcript.**
    // Work in flight comes after every row there is, so it belongs to the last stretch of
    // invisible rows and to nothing else — anything else would be two markers for one run.
    // A stretch that nothing hides and that no work is reaching into draws no marker at all.
    let reaches_the_turn = end == items.len() && live.work() > 0;
    (hidden || reaches_the_turn).then_some((start, end))
}

/// **Is this row inside the run that is currently OPEN** — R37 AMENDED's "it opens".
///
/// Opening a run is not a second rendering of it: it is **the rung lifted for those rows
/// and no others**, so the reader gets the very rows the rung was hiding — their own
/// headlines, payloads, diffs and decisions — and gets them from the one renderer that has
/// always drawn them. A bespoke "expanded run" view would be a second way to draw a tool
/// row, which is the copy this file keeps refusing to make.
///
/// `open` is `payload_sel`, which is the same field the payload window uses. One field for
/// both because they are the same act — *show me the whole of this* — and because a reader
/// can only be reading one thing at a time.
pub(crate) fn run_open_at(
    items: &[SnapshotItem],
    vis: Visibility,
    bound: &std::collections::HashMap<String, String>,
    live: LiveWork,
    open: Option<&str>,
    row: usize,
) -> bool {
    let Some(id) = open.filter(|id| !id.is_empty()) else {
        return false;
    };
    // **The live tail is addressed by a sentinel**, because there is no row to key it on:
    // `App::newest_openable` returns this for it. A run with no row yet still has to open —
    // *"a marker that cannot be opened is the elision this document refuses everywhere else"*
    // — and what it opens is the turn's own live view, which is where that work is.
    if id == LIVE_RUN {
        return live.work() > 0 && !live_tail_covered(items, vis, bound);
    }
    let Some(start) = items.iter().position(|it| it.item_id == id) else {
        return false;
    };
    unseen_run_at(items, vis, bound, live, start).is_some_and(|(s, e)| row >= s && row < e)
}

/// **The run with no row yet** — the sentinel [`App::newest_openable`] returns for the work in
/// flight, so the marker that stands for it can name a chord and that chord can reach it.
///
/// Not an item id: an item id is minted by the daemon and this run has no item. A constant
/// that cannot collide with one (`s-…`/`t…r…` ids are alphanumeric) and that reads as what it
/// is in a debugger.
pub const LIVE_RUN: &str = "<live>";

/// **The newest run of invisible rows, by its first row** — what `ctrl-t` opens.
///
/// The same shape `App::newest_payload_row` has for a long result, and for the same reason:
/// there is no cursor in this head, so exactly one run can be addressed by a chord, and the
/// one a reader reaching for the key means is the newest. A chord may only be named where
/// it acts, so only this run's marker names `ctrl-t`.
/// **Which run the chord opens**, as *the row it starts at* — and the live tail has no row,
/// so it is `items.len()`, which is the one index past the end and reads as what it is.
///
/// **The in-flight run is the newest run there is**: it is the work of this moment, after
/// every row, so when it stands alone its marker is the one that names `ctrl-t`.
pub(crate) fn newest_unseen_run(
    items: &[SnapshotItem],
    vis: Visibility,
    bound: &std::collections::HashMap<String, String>,
    live: LiveWork,
) -> Option<usize> {
    if !vis.hides_the_working() {
        return None;
    }
    // **A run with rows comes first, and the live tail only when there is none** — the order here
    // is the fix for the two-marker duplicate, and it is safe because the FOLD is what decides
    // which run carries the work: `hidden_run_marker` folds the live counts into the run that holds
    // one of the current turn's rows (`live_here`), and a run of an EARLIER turn's rows folds
    // nothing.
    //
    // **Preferring a run with rows *without* that gate is the trap**, and this file carries the
    // scar: a settled turn's marker folded the current turn's work and went yellow, which is
    // *"all tool call counters are yellow now"*.
    if let Some(r) = (0..items.len())
        .rev()
        .find(|r| unseen_run_at(items, vis, bound, live, *r).is_some_and(|(start, _)| start == *r))
    {
        return Some(r);
    }
    // **And only when nothing has rows**: the work in flight is its own run, addressed by the one
    // index past the end and drawn by the live pane.
    (live.work() > 0).then_some(items.len())
}

/// **Is the work in flight already counted by a run that has rows.**
///
/// True when the transcript's last row is invisible, because then the stretch it sits in runs
/// to the end and [`unseen_run_at`] has already folded the tail into its counts. False when
/// the last row is prose or there are none — and then the in-flight work is its own run, with
/// no row to be drawn at, and the live pane draws its marker.
pub(crate) fn live_tail_covered(
    items: &[SnapshotItem],
    vis: Visibility,
    bound: &std::collections::HashMap<String, String>,
) -> bool {
    // **`vis.hides_the_working()`, NOT `!`** — the term was inverted, and that is the whole of
    // the two-marker defect. Read against the docstring above: *true when the last row is
    // invisible, because then the stretch it sits in runs to the end and `unseen_run_at` has
    // already folded the tail into its counts.* A row can only be invisible at a rung that hides
    // the working, so the guard has to be the positive one — as written it was false in exactly
    // the case the function exists for, so it answered `false` always, every caller took the
    // live work for a run of its own, and one turn's work was drawn by two markers: the walk's and
    // the live pane's.
    vis.hides_the_working() && items.last().is_some_and(|it| !row_drawn_at(vis, bound, it))
}

/// **Does the MODEL's own sentence introduce this run.**
///
/// The marker continues the sentence the colon points at, and that sentence is the model's
/// narration — never the operator's message. Measured on their screen: they typed a prompt,
/// the model worked with no prose, and the counts landed on the end of *their* line. Their
/// words: *"the [] thing comes right after my message … add an empty line between them."*
///
/// So a run is continued by prose only when the row above it is an assistant row with text
/// the reader can see. After anything else — their own message, a system row, the top of the
/// transcript — the marker stands as a line of its own, and the separator gives it the air
/// prose gets.
pub(crate) fn run_continues_prose(items: &[SnapshotItem], start: usize) -> bool {
    start > 0
        && matches!(
            items[start - 1].item.as_ref(),
            Some(letibot_transcript::TranscriptItem::Assistant { text, .. })
                if !text.trim().is_empty()
        )
}

/// **Does a run begin at the very next row**, and if so what would its marker say — R37
/// AMENDED, and the answer to *"sometimes you do it same line - sometimes dont."*
///
/// The marker is appended to the last line of the sentence it continues, and that line has
/// whatever width the prose happened to leave. Measured on the operator's own screen: every
/// joined marker sat on a sentence whose last line ended short, and every lone one sat on a
/// sentence whose last line ran to the frame's edge, where the join is refused for want of
/// room. So the reservation is made **before** the sentence is wrapped — the row that
/// introduces a run is rendered a little narrower, the counts go in the room that leaves, and
/// the same transcript now reads the same way whatever the reader has scrolled to.
///
/// Returns the marker's own width, seam included, so the caller can take exactly that much.
pub(crate) fn reserved_for_run(
    items: &[SnapshotItem],
    vis: Visibility,
    bound: &std::collections::HashMap<String, String>,
    live: LiveWork,
    cfg: &RenderConfig,
    row: usize,
    newest: bool,
) -> Option<usize> {
    if !vis.hides_the_working() || row + 1 >= items.len() {
        return None;
    }
    let (start, end) = unseen_run_at(items, vis, bound, live, row + 1)?;
    if start != row + 1 {
        return None;
    }
    // **Only when the counts will actually be glued to this row.** A marker that stands on
    // its own line needs no room left for it, and narrowing the prose for one would be a
    // sentence wrapped short for nothing.
    if !run_continues_prose(items, start) {
        return None;
    }
    // **The prose is NOT narrowed for the marker, and that is the correction.**
    //
    // This used to return the marker's room so the sentence could be rendered narrower and the
    // counts could land in the gap. It worked — the marker always fitted — and it wrapped every
    // sentence early for the sake of a marker that is almost never that wide. The operator, at 210
    // columns: *"there is no need to have the line break here because the whole tail fits. you
    // didnt try the 'tool calls' -> 'tools' -> 't' progressing. so I complain about line wrapping
    // here."* They are right, and the arithmetic is on their side: the tail is ~55 columns of a
    // 210-column frame and the reservation was holding back 56 of them from a line that needed 55
    // once.
    //
    // **So the sentence wraps at the frame's own width and the marker fits what is LEFT of its last
    // line** — see the join sites, which measure that line and hand the marker the remainder. What
    // is still reserved, and still fixed, is [`marker_room`]'s ceiling: it is what the marker may
    // claim when the line is short enough, so growth cannot move the wrap.
    let _ = (items, start, end, newest, live);
    None
}

/// # The seam, which is the one thing here that is not a count
///
/// ` · ctrl-v opens it`, or ` · /verbosity` on a run that is not the newest. R37 AMENDED
/// requires a marker that **opens**, and R40's rule is that a chord may only be named where
/// it acts — so the newest run's marker names the chord and every other one names the verb
/// that does reach it. It is a seam and not content, exactly as `… +8 lines · /t unfolds it`
/// is on every other elided row in this file, and it is one string to delete if the operator
/// rules that the sentence is better without it.
///
/// The dot lives inside the seam string rather than being painted beside it, so the separator
/// cannot come out in one register and its own key in another.
pub(crate) fn marker_seam(newest: bool) -> &'static str {
    marker_seam_rung(newest, 0)
}

/// **The seam at one rung** — the whole chord, the chord alone, nothing; and the verb instead of
/// the chord on a run the chord does not act on. See [`SEAM_RUNGS`].
pub(crate) fn marker_seam_rung(newest: bool, rung: usize) -> &'static str {
    if !MARKER_SEAM {
        return "";
    }
    let (chord, verb) = SEAM_RUNGS[rung.min(SEAM_RUNGS.len() - 1)];
    if newest { chord } else { verb }
}

/// **The calls THIS ROUND's result rows have answered** — the ids of the `tool_result` rows the
/// round published whose bodies this head holds.
///
/// The daemon appends a round's results after every call in it has run, so a `tool_result` among
/// [`TurnPane::appended`] is the answer to one of that round's calls — and this is the fact the
/// marker's colour reads ([`LiveWork::running`]), because a call whose result row is on the screen
/// is not executing. leticl's `call-answered-p` is the same answer, set when the row's BODY lands
/// (`src/session/seq-gap.lisp:315-319`) and from a snapshot's own rows
/// (`src/cards/targets.lisp:433`).
///
/// **Scoped to the round, and keyed on the row's own id.** Scoped, because the ids are
/// round-positional — `crates/turn/src/items.rs` mints `call_0`, `call_1`, … for a wire format that
/// carries none, and [`TurnPane::settled_calls`] already records that *"the ids repeat"* — so
/// asking the whole transcript whether any row ever carried this id would answer YES for a call
/// that has just started in the round after the last one's row landed, and put the yellow out while
/// a command ran. Keyed on the id, because the positional handover counts ROWS and a row can land
/// for a call the pane does not hold (R31 deposits an operator's own result row, and its
/// `TranscriptAppended` reaches this head like any other) — a counter that ran ahead would claim
/// the next call of the round for it, which is the same false negative from the other side.
///
/// A row whose body has not arrived yet carries no `call_id` and claims nothing: the row is the
/// answer once it can be read, which is also the moment the walk starts counting it as a row.
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

/// **How the live pane knows what is in flight** — calls proposed or running with no result
/// row yet, and reasoning streamed and not yet committed.
///
/// Recomputed every frame from the turn, which is the only place it exists: the daemon
/// appends a round's results **after every call in it has run**, so between the narration and
/// the first result the transcript is empty of the work and this is the whole of the evidence.
///
/// **And it is the correct reading of *is the model working*** (R51's preamble): it asks the
/// CALLS rather than the turn's state name, so a call running under a `finished` round counts.
/// [`App::turn_busy`] asks the same question of the same facts; this is the rendering half.
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
    // **The calls the transcript has NOT taken over yet** — `settled_calls` is the count of them
    // that a result row has already claimed, and it advances when the row's BODY lands, not when
    // the call finishes. That distinction is the whole reason this is not a filter on the call's
    // own state, and leticl measured what the filter costs: *"counting the UNFINISHED calls here
    // handed over at `tool_finished` instead, which arrives BEFORE the row, so for that window the
    // call was in neither half. The operator watched it: '2 (in yellow) tool calls dropping to 1
    // (in yellow) tool calls and then changing back to 2 (in white) tool calls.'* **A number that
    // is a count of work done cannot go down**, and it did.
    let calls = t.calls.len().saturating_sub(t.settled_calls);
    // **Executing, and `Proposed` is not executing yet.** A call the model has written but the
    // daemon has not started is work the marker counts and is not work that is happening — the
    // screen has a `◐ Running` card for the second and would show `proposed` for the first.
    //
    // **AND ONLY THE CALLS THIS ROUND'S TRANSCRIPT HAS NOT ANSWERED** — the same boundary the live
    // pane refuses to draw its cards across (`calls.get(*settled_calls..)`), applied to the colour,
    // and the reason the two have to agree: **the yellow says one of THIS NUMBER's calls is
    // executing**, so asking the whole pane puts the colour on calls the number beside it has
    // already stopped counting. That is the stuck yellow, exactly — the operator: *"yellow tool
    // calls are not resolved unfortunately"*, a marker like `[4 tool calls, 72 thinking lines]`
    // whose digits stay pending on a turn whose calls have all finished and whose daemon has
    // published every row.
    //
    // A call whose result ROW the transcript holds is over, and this head holds that fact itself:
    // the daemon appends a round's results after every call in it has run, so a `tool_result` row
    // among [`TurnPane::appended`] IS the answer to one of this round's calls. **The row is the
    // head's own record that the call is over** — nothing else clears a `CallState::Running` it
    // never received a `ToolFinished` for, the pane is the daemon's to correct, and a daemon that
    // never published the finish reports the same `running` in its own view — so the colour has to
    // read it.
    //
    // **It cannot make the marker quieter on a healthy round**: a call leaves that set only at the
    // moment the row that answers it lands, which is after every call of its round has returned.
    let answered = round_answered(t, items);
    let running = t
        .calls
        .iter()
        .filter(|c| matches!(c.state, CallState::Running))
        .filter(|c| !answered.contains(&c.call_id.as_str()))
        .count();
    // **Only the reasoning no row has taken over** — `reasoned_upto` is the mark, and the slice is
    // what is left of the stream. `get` rather than an index so a mark from a longer text (a
    // rebuilt pane, a replay) cannot panic; an impossible mark reads as *nothing unlanded*.
    let unlanded = t.reasoning.raw().get(t.reasoned_upto..).unwrap_or("");
    let think_lines = if unlanded.is_empty() {
        0
    } else {
        reasoning_display_lines(unlanded, cfg.width)
    };
    LiveWork {
        calls,
        running,
        think_lines,
    }
}

/// **Everything the marker draws about NOW, plus the row the work is in — as ONE value.**
///
/// It is two things wearing one name because they must not come apart: the facts are what
/// [`hidden_run_marker`] reads, and the SAME value is the cache key for the row that marker is
/// painted into ([`App::marker_facts`]). A fact the marker draws is therefore a fact the key
/// holds, by construction rather than by remembering — which is the whole point, because the
/// remembering is what failed: `(calls, think_lines)` omitted `running` (the colour, see
/// [`marker_carries_live`]), and the counts alone omitted *which run*, which left **five**
/// markers lit at once (*"look how many tools are yellow"*) when two rounds carried identical
/// numbers.
///
/// `run` is the ONE field the renderer never reads. It is which row the work belongs to, and it
/// is here because the rebuild needs it: the run the work has LEFT has to be rebuilt without
/// the colour, as well as the run it moved to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct MarkerFacts {
    /// The live turn's work, exactly as [`live_work`] reports it — the numbers the marker
    /// prints and the `running` that lights them.
    pub(crate) live: LiveWork,
    /// **The row of the run this work belongs to**, from `newest_unseen_run`, or `None` while
    /// the work has committed no row yet.
    pub(crate) run: Option<usize>,
}

impl MarkerFacts {
    /// **The facts as the two inputs report them.** One constructor, so every caller — the
    /// invocation in `body_window` and the two walks that draw the marker — fills the value
    /// from the same pair (`live_work`'s answer and `newest_unseen_run`'s row) and cannot
    /// hand the renderer a value the key is not made of.
    pub(crate) fn of(live: LiveWork, run: Option<usize>) -> MarkerFacts {
        MarkerFacts { live, run }
    }
}

/// **The counts, as the two halves that can be painted differently** — R51 item 7.
///
/// Split apart rather than composed, because the live-edge colour goes on ONE of them: the CALLS
/// count alone, never the brackets and never the thinking count. The operator, twice, the second
/// time correcting the first fix: *"when you correctly do account running jobs in verbosity mode [],
/// mark counters yellow if the tail job is still running"*, then *"you should yellow only tool call
/// number, not the whole [] thing."*
///
/// The reason it is only the number: a count still going up and a count that has stopped are the
/// same characters, and the colour is the only thing that tells them apart — on a line whose whole
/// job is to be punctuation inside the model's sentence. Colouring the brackets too makes it a
/// highlight rather than a signal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Counts {
    /// **The number and its noun, kept APART** — the operator: *"yellow <count> not entire <Count>
    /// tool call."*
    ///
    /// The yellow is on the digits alone, so the two halves cannot be one string: `%counts-clause-segs`
    /// in leticl is the same split — *"ONE count clause as SEGMENTS — `2` in STYLE, ` tools`
    /// plain"* — and it is the second correction to this colour (the first was painting the
    /// brackets and the thinking count with it).
    ///
    /// `None` when the clause is absent, which is how a zero clause is dropped.
    pub(crate) calls: Option<(String, String)>,
    /// `2 thinking lines`. Never coloured; a thought is not work that is still happening.
    pub(crate) think: Option<(String, String)>,
    /// **`2 head events`** — the fallback that keeps `[]` off the screen. See [`COUNT_RUNGS`].
    ///
    /// leticl's own word for it (*"head event"*), and never coloured: these are not work that is
    /// happening, they are the rows the rung hid that neither of the other two numbers could
    /// describe.
    pub(crate) events: Option<(String, String)>,
}

/// **The widest room a marker may claim from the sentence it continues**, leading space included.
///
/// leticl's `+hidden-run-marker-cols+` is 56, measured there against the longest marker a realistic
/// run writes — `[100 tool calls, 999 thinking lines] · ctrl-v opens it` is 54 columns plus the
/// space in front. letibot reserves the marker's **measured** width instead, and that is the jump
/// the operator complained about: *"the text starts to jump — counts add digits when grow, and at
/// some point the line could be split so things jump even more. i dont like jumps."* A room that
/// depends on the counts re-wraps the sentence above every time a count gains a digit, so the same
/// transcript reads two ways depending on how many calls a turn happened to run.
pub(crate) const MARKER_ROOM_MAX: usize = 56;

/// **The least room a marker may claim**, however narrow the frame. Below about twenty columns the
/// two clauses stop being readable at all — `[100t, 246l]` and its separator are twelve — and a
/// marker that cannot be read is a marker that did nothing. leticl's `+hidden-run-marker-floor+`.
pub(crate) const MARKER_ROOM_FLOOR: usize = 22;

/// **How many columns of `cols` the marker may occupy**, its leading space included.
///
/// **Fixed for a given frame, and that is the whole point** — see [`MARKER_ROOM_MAX`]. At most half
/// the frame, so a narrow terminal keeps half its line for the sentence; at most
/// [`MARKER_ROOM_MAX`]; never less than [`MARKER_ROOM_FLOOR`]; and never more than the frame itself,
/// because a room wider than the line it is on is not a room.
///
/// leticl's `hidden-run-marker-room`, **arithmetic and all** — `(min cols 56 (max 22 (floor cols
/// 2)))`, kept in its own shape rather than collapsed. The first version here folded the `min`/`max`
/// into one expression and got 56 at 80 columns instead of 40, which a test caught: three-deep
/// min/max is not worth being clever about.
pub(crate) fn marker_room(cols: usize) -> usize {
    cols.min(MARKER_ROOM_MAX)
        .min(MARKER_ROOM_FLOOR.max(cols / 2))
}

/// **The count clause, most-spelled first** — `2 tool calls` → `2 tools` → `2 calls` → `2t`.
///
/// The operator's own ladder, verbatim: *"we just start to remove bloat - 'tool calls' -> 'tools'
/// -> 't' and so on"*. This is what pays for a marker that has outgrown its room: **growth is paid
/// in words, not in layout**, which is the other half of [`MARKER_ROOM_MAX`]'s argument.
///
/// **Three clauses, and the third is why `[]` can never be drawn.** A run can hide rows that are
/// neither a call nor a thought — a system update, a segment mark — and counting neither left the
/// marker with an empty body: `[]`, on the line whose whole job is to be the fact the rung was
/// hiding. leticl's fallback says it exactly: *"a run neither count can describe … falls back to
/// their count, because `[]` is not a marker."* Its `%hidden-run-counts` ends `(t (incf events))`
/// where this file had `_ => {}`.
///
/// The events clause is drawn **only when both of the other two are zero** — leticl branches
/// there and nowhere else — so a run of calls says `[2 tool calls]` and does not mention the
/// system row beside it.
///
/// The two clauses step down TOGETHER — leticl's rung is a triple for that reason, and its words are
/// *"a marker reading `[2 tools, 3 thinking lines]` is one rung's word beside another's, and the
/// operator's ladder is about the marker and not about the clauses in it"*.
/// **The suffix carries its own leading space, and the last rung has none.** leticl's rung is a
/// format string — `~d tool call~:p` and then `~dt` — so the compressed form is genuinely tighter
/// rather than one space shorter: `[11t, 246l]`, not `[11 t, 246 l]`. The space belongs to the
/// rung for the same reason the seam's dot belongs to the seam.
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

/// **Whether the marker names its own key. OFF, and that is the operator's ruling.**
///
/// leticl has this as a switch that DEFAULTS OFF, with the ruling quoted in its own docstring:
/// *"also make showing \" dot /verbosity\" a config and switch it off."* And again today, plainly:
/// *"dont print \" dot /verbosity\" or ctrl-t opens it - we dont need that."*
///
/// **Why it is the right call on a line whose whole job is to sit inside the model's sentence.** The
/// counts are a fact about the work; the seam is the head talking about its own key. On
/// `…has to give: [11 tool calls, 246 thinking lines]` the first is punctuation in the sentence and
/// the second is an advertisement stapled to it.
///
/// **What is deliberately NOT deleted with it.** The row it would have opened can still be opened,
/// and R29's rule — a disclosure carries the act that undoes it — is kept by the places that are not
/// this line: the hint bar names `ctrl-t` and `/t`, `/help` lists both, and `ctrl-t` opens the
/// newest run. What goes is the advertisement on every marker, which is the same trade
/// [`App::rung_state`] makes for the rung's own name.
///
/// **The ladder keeps its seam slots even so** — see [`MARKER_LADDER`]. Turning this back on must not
/// move a single line of prose, and the room is a function of the frame width rather than of what
/// the marker says, so the slots are free.
pub(crate) const MARKER_SEAM: bool = false;

/// **The seam, most-spelled first** — the whole chord, the chord alone, nothing.
///
/// Dropped LAST of the three ladders, because the seam is the only thing on the line that says the
/// rows can be opened at all — and it goes at all only because a marker that will not fit is a
/// marker that did nothing. `ctrl-t` survives a rung longer than `opens it` does, which is the rule
/// R29 already keeps on every other elided row: **the key is the part that cannot go.**
///
/// **Empty while [`MARKER_SEAM`] is off**, and the ladder is then walked on the counts alone: every
/// seam rung reads empty, so the first rung that fits is decided entirely by the count clauses. (That
/// is a correction to leticl, which with its seam off tries one rung — `((0 . 2))` — and so never
/// steps the counts down; the operator asked for the counts to compress either way.)
pub(crate) const SEAM_RUNGS: [(&str, &str); 3] = [
    (" · ctrl-v opens it", " · /verbosity"),
    (" · ctrl-v", " · /verbosity"),
    ("", ""),
];

/// **The order the two ladders are spent in** — the counts step down through every rung first, and
/// only then does the seam start to go.
///
/// **Counts before seam**, because the counts are the fact the line exists to carry and the seam is
/// the head talking about its own keys. The seam still goes before the counts reach their last rung,
/// which is why the pairs interleave rather than running as two sweeps. leticl's
/// `+hidden-run-marker-ladder+`, verbatim.
pub(crate) const MARKER_LADDER: [(usize, usize); 6] =
    [(0, 0), (1, 0), (2, 0), (3, 0), (3, 1), (3, 2)];

impl Counts {
    pub(crate) fn of(calls: usize, think_lines: usize) -> Counts {
        Counts::at_rung(calls, think_lines, 0, 0)
    }

    /// **The counts spelled at one rung of the ladder** — see [`COUNT_RUNGS`].
    pub(crate) fn at_rung(calls: usize, think_lines: usize, events: usize, rung: usize) -> Counts {
        let (c1, cn, t1, tn, e1, en) = COUNT_RUNGS[rung.min(COUNT_RUNGS.len() - 1)];
        // The number, then the suffix — which is where the space lives, and why the last rung has
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

    /// **The clauses this marker draws, in order** — the two counts, or the fallback when neither
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

    /// **The counts as PLAIN text** — for measuring, and for a test that wants the words rather
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

/// **Does this marker carry the work that is in flight** — R51 item 8, and the whole of the colour
/// question in one place.
///
/// # The three wrong answers, because this colour has been wrong in three directions
///
///   · keyed on the work IN FLIGHT at the card, it flickered off in the gap between a call
///     finishing and the next round's first delta — *"running tool is no longer yellow the
///     counter, wtf why it regressed."*
///   · keyed on *the turn is running* for every marker, it lit the turn's settled history too —
///     *"all tool call counters are yellow now."* A walk draws a marker for EVERY run in the
///     transcript, so that question is not a property of a marker at all.
///   · keyed on *the newest run*, it lit a row from the PREVIOUS turn while the current one had a
///     call proposed and no result row yet — *"yes one old tool call is still yellow"*.
///
/// # What it is, and why a named predicate rather than an `and`
///
/// **The yellow is on the marker for the row that CARRIES the live work.** That is not a condition
/// this function can test — only the caller knows which marker it is drawing — so it is the
/// caller's answer encoded in [`Marker::live`], and this is the half that decides whether there is
/// any work to speak of:
///
/// * **at least one call is proposed or running with no result row yet.** This is `live_work`'s
///   own reading, which is the correct one (see [`App::turn_busy`]: the state name describes the
///   GENERATION, and it reads `finished` for the whole of every command). Nothing else counts:
///   a thought still streaming colours no count, because the thinking count is not the number that
///   moves with the work.
///
/// A `bool` return, deliberately: the head and its tests must be able to ask the same question, and
/// a truthy plist is a value two callers can disagree about while both are "not false".
///
/// **What it cannot close.** `TurnFinished` is emitted per ROUND, so between a call finishing and
/// the next round's `TurnStarted` the answer is honestly *no* — the same millisecond window R51 §3
/// records for the tense, and closing it needs the daemon to publish *the prompt is over* as its own
/// fact. Neither head has that.
pub(crate) fn marker_carries_live(live: LiveWork) -> bool {
    // **`running`, not `calls`** — this is the stuck yellow. See [`LiveWork::running`]: the count
    // of calls with no result row is the NUMBER the marker carries, and only a call that is still
    // executing lights it. A call that finished a frame ago keeps its number and loses the colour,
    // which is what the screen should say about it.
    live.running > 0
}

/// A struct rather than a `String` because the register is per half: passing the composed text
/// around and splitting it at paint time would be a second definition of where the counts stop
/// — and the counts are a string the head builds, so nothing may search them for a delimiter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Marker {
    /// `[1 tool call, 2 thinking lines]` — the content, drawn plain except for the calls count.
    pub(crate) counts: Counts,
    /// ` · ctrl-v opens it` — an affordance, drawn faint. Dot included.
    pub(crate) seam: &'static str,
    /// **Does this marker carry the work in flight** — [`marker_carries_live`], decided by the
    /// caller because only the caller knows which row it is drawing.
    pub(crate) live: bool,
}

impl Marker {
    /// **The marker, spelled to fit the room it was given** — see [`MARKER_LADDER`].
    ///
    /// `room` is [`marker_room`]`(width)`, which is fixed for the frame; the counts and the seam
    /// step down until the marker fits `room - 1`, and the last rung's spelling is used whatever
    /// its width. **The `- 1` is the room's own definition**: the room includes the leading space
    /// the join puts in, so the marker itself may be one column narrower than the room.
    ///
    /// The counts come first and the seam last — [`MARKER_LADDER`] records why — so a marker that
    /// has outgrown its room loses `opens it` before it loses `tool calls`, and a count that gains
    /// a digit costs a word rather than a line.
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

    /// **Painted** — the counts plain (the calls count PENDING when this marker carries the live
    /// work), the seam faint. See [`marker_painted`].
    pub(crate) fn painted(&self, cfg: &RenderConfig) -> String {
        marker_painted(cfg, &self.counts, self.seam, self.live)
    }
}

/// **The marker, painted** — the counts PLAIN, the seam faint, and the calls count PENDING while
/// the work is in flight.
///
/// Ruled by the operator on the two heads' difference, and leticl's reading is the one that
/// stands: *"counts plain, seam faint."* The counts are **punctuation inside a sentence** —
/// the marker sits on the end of the prose the colon points at, and nothing in prose is dimmed
/// mid-sentence except an aside. They are not an aside; they are the only content the marker
/// carries.
///
/// The seam is the opposite case, and this head's own register rule says so: **dim is for a
/// sentence you could delete with the reader no worse off** (R29). `· ctrl-v opens it` is that
/// — an affordance, not the count — and it is the same register every other elided row says
/// `… +N lines · /t unfolds it` in.
///
/// **And the live edge, which is the ONE thing here that moves** (R51 item 7): with `live`, the
/// calls count alone is painted [`Role::Pending`]. Not the brackets and not the thinking count —
/// the operator's own correction, and the argument is in [`Counts`].
///
/// Takes the two halves already composed rather than re-splitting a string: a `]` searched for
/// at paint time is a second definition of where the counts stop, and one marker whose counts
/// carried a `]` would find the wrong one.
pub(crate) fn marker_painted(
    cfg: &RenderConfig,
    counts: &Counts,
    seam: &str,
    live: bool,
) -> String {
    let p = cfg.palette();
    // **The NUMBER goes pending and its noun does not** — *"yellow <count> not entire <Count> tool
    // call"*, and leticl's `%counts-clause-segs` word for word: *"`2` in STYLE, ` tools` plain."*
    // The digits are the thing that moves; `tool call` is the thing the digits are counting, and a
    // phrase in yellow on a line whose job is to be punctuation inside a sentence reads as a
    // highlight rather than as a signal.
    let calls = match &counts.calls {
        Some((n, noun)) if live => format!("{}{noun}", p.painted(Role::Pending, n)),
        Some((n, noun)) => format!("{n}{noun}"),
        None => String::new(),
    };
    let think = counts
        .think
        .as_ref()
        .map(|(n, noun)| format!("{n}{noun}"))
        .unwrap_or_default();
    // **The fallback is a branch and not a third element.** The events clause is drawn only when
    // the run is one neither count describes — see [`COUNT_RUNGS`] — so a run of calls must not
    // grow a `, 1 head event` beside it. Written as leticl writes it: `parts`, or the fallback.
    let events = counts
        .events
        .as_ref()
        .map(|(n, noun)| format!("{n}{noun}"))
        .unwrap_or_default();
    let body: Vec<&str> = if calls.is_empty() && think.is_empty() {
        vec![events.as_str()]
    } else {
        vec![calls.as_str(), think.as_str()]
    }
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect();
    format!("[{}]{}", body.join(", "), p.painted(Role::Faint, seam))
}

pub(crate) fn hidden_run_marker(
    items: &[SnapshotItem],
    start: usize,
    end: usize,
    vis: Visibility,
    cfg: &RenderConfig,
    newest: bool,
    // **The facts, not `live`.** `facts.live` is everything this marker reads about NOW, and the
    // SAME value is the cache key for the row it draws into ([`App::marker_facts`]) — so a fact
    // the marker draws is a fact the key holds. Taking [`LiveWork`] here instead would leave the
    // renderer free to read a fact the key does not carry, which is how the colour and then the
    // run went missing.
    facts: MarkerFacts,
    // **Does this run hold a row of the current turn** — leticl's `live-here`, and the fact that
    // decides whether the in-flight work is these counts' continuation. See the fold below.
    live_here: bool,
) -> Marker {
    let live = facts.live;
    let mut calls = 0usize;
    let mut think_lines = 0usize;
    let mut events = 0usize;
    // **Per row, and only the rows this rung actually hides.** The guard has to be on the row
    // being counted and not on the run's first one: a run normally *starts* at an assistant
    // row with no prose — which is invisible and is NOT hidden (`keeps` keeps `Assistant`) —
    // so a guard keyed on the start asked the wrong question and counted nothing. Found by
    // the interleaved test reporting `[1 thinking line]` for seven calls.
    for r in start..end {
        if !row_hidden(items, vis, r) {
            continue;
        }
        match items[r].item.as_ref() {
            Some(letibot_transcript::TranscriptItem::ToolResult { .. }) => calls += 1,
            Some(letibot_transcript::TranscriptItem::Reasoning { text, .. }) => {
                think_lines += reasoning_display_lines(text, cfg.width);
            }
            // **A hidden row of any other kind — a system update, a segment mark — is an EVENT.**
            // It used to be counted by nothing, on the argument that *a third would be a number
            // about a row nobody classified*. That argument is what drew `[]`: a run of nothing
            // but these has no calls and no thinking lines, so the marker had no body at all, on
            // the one line whose whole job is to be the fact the rung was hiding. See
            // [`COUNT_RUNGS`] for leticl's fallback and its word for the number.
            _ => events += 1,
        }
    }
    // **And the work in flight, when this run is the one it belongs to.** A stretch that
    // reaches the end of the transcript is where the turn is, so the counts move as the round
    // runs — which is the operator's *"obviously be updated earlier, even for the empty card."*
    //
    // **This is also R51 item 8's *which marker* answer, and it falls out of the fact above:**
    // the run that reaches the end of the transcript is the run carrying the live work, so it is
    // the one whose calls count goes pending. Every other marker in the walk is settled history —
    // a previous round, or a previous turn — and stays plain. The second of the three wrong cuts
    // was exactly this question asked of the turn instead of the marker, which lit every run
    // behind it.
    // **Fold when this run holds a row of the CURRENT TURN**, which is the fact the gate was
    // standing in for. `end == items.len()` said *this stretch reaches the live edge*, and that is
    // true only while nothing visible has arrived after it — so a row the reader can see landing
    // after the run (the operator's own message, say) turned the fold OFF, and the same work was
    // then drawn twice: the run's counts in one marker and the live work in another.
    //
    // **And `newest` alone is the wrong widening** — that is the trap this file already records.
    // The newest run with hidden rows can be the PREVIOUS turn's, and folding the live work into it
    // is *"all tool call counters are yellow now"*. The discriminating fact is the turn: the live
    // work belongs to the run the current turn is working in, so the run has to hold one of the
    // turn's own rows.
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
