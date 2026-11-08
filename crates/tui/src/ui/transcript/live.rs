//! The turn in flight, drawn: what the model is thinking and writing right now, the calls it
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
    lifted: Option<Show>,
) -> Vec<(usize, String)> {
    // Every row of every call card this pane draws, and the call it belongs to - the record a click
    // is answered from (`App::call_rows`). A card is drawn here and nowhere else, so this is the one
    // place that knows where it landed; `before` counts the `Seg`s already pushed.
    let mut cards: Vec<(usize, String)> = Vec::new();
    // What a click on the live marker lifted (`App::live_open`): that kind is drawn open while
    // the turn runs, whatever the rung hides.
    let show_thinking = lifted == Some(Show::Thinking);
    let show_calls = lifted == Some(Show::Tools);
    let think = if show_thinking { Fold::Open } else { think };
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
    // What the model THOUGHT is the working, not the conversation: hidden here as it is hidden in
    // the transcript, so the rung does not depend on whether a row has committed yet - and
    // `reasoning` is only the part NO ROW HOLDS, so a landed thought is never drawn twice.

    // The rung's own count of the work in flight (drawn further down, after the reply): without it
    // the pane showed the narration and then nothing until a result row landed, which is a screen
    // that says the head has stopped. Only when no run has already carried it.

    if !superseded && !reasoning.is_empty() && (!vis.hides_the_working() || show_thinking) {
        // Narrower by the rail and by the step it is set in: getting this wrong makes the block one
        // row taller than the space reserved for it, which moves everything below it by a line every
        // frame. The header's number and this body are one text at [`reasoning_body_width`].
        let rcfg = reasoning_cfg(cfg);
        reasoning_cache.set_decor(reasoning_decor(cfg));
        segs.push(Seg::Owned(step_in(
            vec![thinking_header(
                cfg,
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
            segs.push(Seg::Borrowed(tail));
        } else {
            // Folded, but a *running* turn still shows the last line, so
            // "it is thinking" and "it is stuck" do not look the same.
            let d = reasoning_decor(cfg);
            segs.push(Seg::Owned(vec![
                d.apply(&last_line(reasoning.raw(), &rcfg)),
            ]));
        }
        segs.push(Seg::Owned(vec![String::new()]));
    }
    if !superseded {
        // Only the calls the transcript has NOT taken over yet: the rest are already drawn above as
        // settled cards, and drawing them here too showed eight live rows under eight settled ones.
        // A call in flight is the working, and it has no marker because it is not a row yet.
        let live_calls = calls.get(*settled_calls..).unwrap_or(&[]);
        // Not a blanket rung question - the `edits` SWITCH is asked per call, because a call that
        // changed a file is still live while its body has not landed, so a rung-only test hid the
        // diff `read-edits` exists to keep. An edit card is the switches'; the rest is the rung's.
        let live_calls: Vec<_> = live_calls
            .iter()
            .filter(|c| match &c.state {
                CallState::Finished { edits, .. } if !edits.is_empty() => {
                    vis.shows(Show::Edits) || show_calls
                }
                _ => !vis.hides_the_working() || show_calls,
            })
            .collect();
        if !live_calls.is_empty() {
            // Where these cards land in the body, so each row of one can be a click target (see
            // `App::call_rows`). The pane pushes whole `Seg`s and the window is taken off their
            // concatenation, so the only honest line number is the one counted here.
            let before: usize = segs.iter().map(Seg::len).sum();
            let mut owned: Vec<String> = Vec::new();
            for c in live_calls.iter().copied() {
                // A running call is timed against the clock that was running when it started:
                // `now_ms` is the log's clock and it stops when the daemon stops saying things, which
                // a silent `cargo build` does. One clocked by this head is measured by this head.
                let card_now = if c.started_at > 0 {
                    head_now_ms
                } else {
                    now_ms
                };
                let lines = step_in(call_card(c, cfg, card_now, tool, diff_split), ind);
                for i in 0..lines.len() {
                    cards.push((before + owned.len() + i, c.call_id.clone()));
                }
                owned.extend(lines);
            }
            owned.push(String::new());
            segs.push(Seg::Owned(owned));
        }
        if !text.is_empty() {
            let (stable, tail) = text_cache.split(text, cfg, cfg.budget.body_lines);
            segs.push(Seg::Borrowed(stable));
            segs.push(Seg::Borrowed(tail));
            // The same air the reasoning block and the call cards already carry. Without it the last
            // line of a running decode touches the top border of the composer, and the blank appears
            // only when the turn ends, so the screen grows by a line at the moment the reader looks.
            segs.push(Seg::Owned(vec![String::new()]));
        }
        // The rung's own count of the work in flight, after the reply it follows.
        if !superseded
            && vis.hides_the_working()
            && live.work() > 0
            && !live_joins
            && !walk_carried_live
        {
            let painted = Marker::new(
                live.calls,
                live.think_lines,
                // No events - see the sibling call above.
                0,
                true,
                marker_carries_live(live),
                marker_room(cfg.width),
            )
            .painted(cfg);
            if std::env::var("LETIBOT_MARKER_DEBUG").is_ok() {
                eprintln!("MARKER pane draws: {painted}");
            }
            // Not indented, because this marker and the walk's are the same kind of row: the walk's
            // is flush with the prose it continues, so this one is flush too. The reasoning block is
            // indented because it is the model's text under its rail; a count of rows is punctuation.
            segs.push(Seg::Owned(vec![painted]));
            // And the air every other block in this pane already carries: the reasoning block ends
            // with a blank and so does the streaming answer. Without it the counts sat on whatever
            // came next - the `Responding` row, or the model's first line.
            segs.push(Seg::Owned(vec![String::new()]));
        }
        // The call the model is writing right now. The markup itself is never here: what is on the
        // screen is that a call is being written, which is the fact the raw text accidentally
        // conveyed and the only part of it a reader wanted.
        if *writing_call {
            segs.push(Seg::Owned(step_in(
                vec![writing_call_line(cfg, now_ms)],
                ind,
            )));
        }
        if raw && !raw_call.is_empty() {
            segs.push(Seg::Owned(raw_call_lines(cfg, raw_call)));
        }
    }
    if let Some(s) = state {
        segs.push(Seg::Owned(turn_footer(cfg, s)));
    }
    cards
}
