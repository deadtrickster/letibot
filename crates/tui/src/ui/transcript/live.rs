//! **The turn in flight, drawn**: what the model is thinking and writing right now, the calls it
//! has proposed, and the turn's footer once it ends.

use crate::app::*;
use crate::ui::render::RenderConfig;
use crate::ui::*;
use letibot_sessionlog::view::{CallState, TurnState};

#[allow(clippy::too_many_arguments)]
pub(crate) fn push_turn_pane<'a>(
    segs: &mut Vec<Seg<'a>>,
    t: &'a mut TurnPane,
    cfg: &RenderConfig,
    think: Fold,
    tool: Fold,
    raw: bool,
    head_now_ms: u64,
    diff_split: bool,
    live: LiveWork,
    vis: Visibility,
    superseded: bool,
    live_joins: bool,
    walk_carried_live: bool,
) {
    let running = matches!(t.state, Some(TurnState::Running));
    let think_elapsed = if t.think_started_ms == 0 {
        None
    } else {
        Some(t.think_last_ms.saturating_sub(t.think_started_ms))
    };
    let now_ms = t.last_ms;
    let TurnPane {
        text,
        reasoning,
        text_cache,
        reasoning_cache,
        calls,
        settled_calls,
        raw_call,
        writing_call,
        state,
        ..
    } = t;
    let ind = activity_indent(cfg.width);
    // **What the model THOUGHT is the working, not the conversation** (R37). Hidden
    // here as it is hidden in the transcript, so the rung does not depend on whether
    // a row has been committed yet — a reader who switched mid-turn would otherwise
    // watch the thinking appear when it settles.
    // **The rung's own count of the work in flight** — R37 AMENDED, the operator's
    // *"display looks frozen, while in fact it is just say cargo testing with yellow
    // dot"*. Everything below this line is the working and this rung does not draw it,
    // so without this the pane showed the narration and then nothing until a result
    // row landed — which is a screen that says the head has stopped.
    //
    // Drawn where the work is: right after the prose that introduced it, which is where
    // the counts belong and where the reader is looking.
    // **And only when no RUN has already carried it** — see `walk_carried_live`, computed
    // above from the same fact the fold uses, so the two cannot disagree.
    if !superseded
        && vis.hides_the_working()
        && live.work() > 0
        && !live_joins
        && !walk_carried_live
    {
        let painted = Marker::new(
            live.calls,
            live.think_lines,
            // No events — see the sibling call above.
            0,
            true,
            marker_carries_live(live),
            marker_room(cfg.width),
        )
        .painted(&cfg);
        if std::env::var("LETIBOT_MARKER_DEBUG").is_ok() {
            eprintln!("MARKER pane draws: {painted}");
        }
        // **Not indented, and that is the operator's own report** — *"plus current thinking
        // lines while counting are indented by 1 or 2 cells"*. This marker and the walk's are
        // the same kind of row and now the same row; the walk's is flush with the prose it
        // continues (`hist_lines`), so this one is flush too. The reasoning block below is
        // indented because it is the model's *text*, set in under its rail; a count of rows
        // is punctuation on the sentence, not a quotation under it.
        segs.push(Seg::Owned(vec![painted]));
        // **And the air every other block in this pane already carries.** The reasoning
        // block ends with a blank, and so does the streaming answer — whose comment gives
        // the reason: *"without it the last line of a running decode touches the top
        // border of the composer."* The counts had none, so they sat on whatever came
        // next: the `Responding` row when no text had arrived yet, and the model's own
        // first line when it had.
        //
        // The operator, twice: *"«Responding…» status line appears and [XX Thinking lines]
        // appeared then right above «Responding» without an empty line"*, and *"this also
        // sometimes happened when you replied while the turn goes."* One blank fixes both,
        // and it is the same blank the walk's marker already has — inside `hist_lines`,
        // where the frame's own `gap` separates it from what follows. The pane's marker
        // is the same fact with no gap under it, so it brings its own.
        segs.push(Seg::Owned(vec![String::new()]));
    }
    if !superseded && !reasoning.is_empty() && !vis.hides_the_working() {
        // Narrower by the rail and by the step it is set in. Getting this
        // wrong makes the block one row taller than the space reserved for
        // it, which moves everything below it by a line every frame — which
        // is one of the things being called flicker.
        let rcfg = reasoning_cfg(&cfg);
        reasoning_cache.set_decor(reasoning_decor(&cfg));
        segs.push(Seg::Owned(step_in(
            vec![thinking_header(
                &cfg,
                reasoning.raw(),
                think.is_open(),
                running,
                think_elapsed,
            )],
            ind,
        )));
        if think.is_open() {
            let (stable, tail) =
                reasoning_cache.split(reasoning, &rcfg, cfg.budget.reasoning_lines);
            segs.push(Seg::Borrowed(stable));
            segs.push(Seg::Owned(tail));
        } else {
            // Folded, but a *running* turn still shows the last line, so
            // "it is thinking" and "it is stuck" do not look the same.
            let d = reasoning_decor(&cfg);
            segs.push(Seg::Owned(vec![
                d.apply(&last_line(reasoning.raw(), &rcfg)),
            ]));
        }
        segs.push(Seg::Owned(vec![String::new()]));
    }
    if !superseded {
        // Only the calls the transcript has NOT taken over yet. The rest
        // are already on the screen above as settled cards with their
        // output under them, and drawing them here as well was the second
        // half of the doubling: a turn eight calls deep showed eight live
        // rows under eight settled ones, in the same order, saying less.
        // **And a card for a call in flight is the working too.** Kept out for the
        // same reason: this rung shows the conversation, and a spinner over a tool
        // call is the head reporting its own machinery.
        //
        // **A live call has no marker, and that is the one window R37 AMENDED does
        // not close.** A run of hidden *rows* collapses to one line; a call that has
        // not returned is not a row yet, so while the first call of a round is still
        // running there is nothing for a marker to count. It closes itself: the
        // moment that call's result row lands, the run exists and the line appears
        // above it. The rung's liveness obligation is elsewhere and unbroken — the
        // footer says a turn is running and for how long (R13/§5.6).
        let live_calls = calls.get(*settled_calls..).unwrap_or(&[]);
        // **Not a blanket rung question — the `edits` SWITCH is asked per call.**
        //
        // This read `if vis.hides_the_working() { &[] }`, which is right for the working
        // and wrong for the one card the operator's own profile exists to keep: a call
        // that CHANGED a file is still "live" while its body has not landed, so
        // `read-edits` — *"all is hidden except edits"* — hid the diff it was chosen for.
        // MEASURED while landing this: the acceptance test failed with exactly that
        // screen, the narration and no diff.
        //
        // The rule is [`Visibility::keeps`]'s, one row up: an edit card is the switches',
        // everything else is the rung's. A call whose state says it changed something and
        // whose `edits` switch is showing is drawn whatever the rung hides; every other
        // live call is the working and follows the ladder as before.
        let live_calls: Vec<_> = live_calls
            .iter()
            .filter(|c| match &c.state {
                CallState::Finished { edit: Some(_), .. } => vis.shows(Show::Edits),
                _ => !vis.hides_the_working(),
            })
            .collect();
        if !live_calls.is_empty() {
            let mut owned: Vec<String> = Vec::new();
            for c in live_calls.iter().copied() {
                // **A running call is timed against the clock that was running
                // when it started** (R13). `now_ms` here is `t.last_ms` — the
                // log's clock — and that number **stops** when the daemon stops
                // saying things, which is exactly what a silent `cargo build`
                // does: the row read `0ms` for the whole build while the spinner
                // in the border below it turned, because the two were reading
                // different clocks two hundred lines apart. A call whose start
                // was recorded with this head's clock is measured against this
                // head's clock instead; one that was not — a `--replay`, whose
                // frames are applied before any clock is set — keeps the log's
                // own span, which is the only honest measurement there.
                let card_now = if c.started_at > 0 {
                    head_now_ms
                } else {
                    now_ms
                };
                owned.extend(step_in(call_card(c, &cfg, card_now, tool, diff_split), ind));
            }
            owned.push(String::new());
            segs.push(Seg::Owned(owned));
        }
        if !text.is_empty() {
            let (stable, tail) = text_cache.split(text, &cfg, cfg.budget.body_lines);
            segs.push(Seg::Borrowed(stable));
            segs.push(Seg::Owned(tail));
            // The same air the reasoning block and the call cards already
            // carry. Without it the last line of a running decode touches
            // the top border of the composer, and the blank appears only
            // when the turn ends and the pane stands down, so the screen
            // grows by a line at the moment the reader finally has time to
            // look. Padding while running is also the shape the transcript
            // row takes over, so nothing reflows at the handoff.
            segs.push(Seg::Owned(vec![String::new()]));
        }
        // The call the model is writing right now. The markup itself is
        // never here: what is on the screen is that a call is being
        // written, which is the fact the raw text was accidentally
        // conveying and the only part of it a reader wanted.
        if *writing_call {
            segs.push(Seg::Owned(step_in(
                vec![writing_call_line(&cfg, now_ms)],
                ind,
            )));
        }
        if raw && !raw_call.is_empty() {
            segs.push(Seg::Owned(raw_call_lines(&cfg, raw_call)));
        }
    }
    if let Some(s) = state {
        segs.push(Seg::Owned(turn_footer(&cfg, s)));
    }
}
