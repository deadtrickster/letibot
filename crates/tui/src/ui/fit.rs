//! **What fits on a short screen.** The rows under the conversation — the cards, the
//! disclosures, the composer and its box, the hint bar — compete for a screen that may not hold
//! them all; this decides which give way, and in what order, before anything is drawn.

/// What the ladder is told: the screen's height and what each row would cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FitInput {
    /// The terminal's height.
    pub(crate) h: usize,
    /// The rows the composer wants.
    pub(crate) composer: usize,
    /// The open card's rows, all of them.
    pub(crate) card: usize,
    /// Rows pinned under the card.
    pub(crate) pinned: usize,
    /// A notice is waiting to be shown.
    pub(crate) notice: bool,
    /// The stuck-turn disclosure has something to say.
    pub(crate) stuck: bool,
    /// A program is running in the pane off-screen.
    pub(crate) pane: bool,
    /// The completion row is reserved.
    pub(crate) completion_slot: bool,
    /// The link's lines, never given up.
    pub(crate) link: usize,
    /// A stop's lines, never given up.
    pub(crate) stopping: usize,
    /// Counters are alarmed, so an unboxed composer still needs their row.
    pub(crate) alarmed: bool,
}

/// What the ladder decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Fit {
    /// The composer's rows.
    pub(crate) rows: usize,
    pub(crate) hint: bool,
    pub(crate) show_notice: bool,
    pub(crate) show_stuck: bool,
    pub(crate) show_pane: bool,
    pub(crate) show_status: bool,
    /// The composer is drawn in its box.
    pub(crate) boxed: bool,
    /// How many of the card's rows are shown.
    pub(crate) content_rows: usize,
}

/// **The ladder**: start with everything, and give up the most expendable row until the whole
/// thing fits with a line of transcript left over.
pub(crate) fn fit_ladder(i: &FitInput) -> Fit {
    // How many rows the composer wants, and then what actually fits. The
    // ladder deletes the most expendable row first and stops as soon as the
    // whole thing fits with a line of transcript left over. The old code
    // drained the chrome from the *front*, which for a box would have eaten
    // the top border and left the bottom one — a container with one side is
    // worse than none. The turn's own status is now one of those rows — it used to be inlaid
    // in the bottom border and cost nothing, and R51 item 1 moved it out because an edge
    // truncates a sentence (see `let status` above).
    let mut rows = i.composer;
    let mut hint = true;
    let mut show_notice = i.notice;
    let mut show_stuck = i.stuck;
    // **The pane's line is news too, and it is given up LAST of the news.** See `let pane`
    // for what it is: a program is running off-screen, which is a fact a reader can act on
    // and cannot get from anything else on the screen. The composer's own rows are given up
    // before it (`rows -= 1` below), because a line a person is not typing into is worth
    // less than the knowledge that a program they started is still alive.
    let mut show_pane = i.pane;
    // **The row is RESERVED, not conditional.** The operator: *"keep the line reserved for
    // `Responding...` always free, or we have these ugly jumps"* — and the jump is the whole
    // reason. The row exists only while a turn does, so the moment one starts or ends, every
    // row of the transcript above it moves by one: the thing a reader is reading while a turn
    // begins is exactly the thing that gets shoved. A reserved line costs one row of screen on
    // an idle session and buys a frame that does not move.
    //
    // So it is always counted in the height and always drawn, empty when there is nothing to
    // say. The two rows below stay conditional, because each of them is *news* — a notice
    // being read, a disclosure — and a reserved line for news is the furniture this head keeps
    // deleting. This one is not news: the turn's own row is where the reader's eye is, every
    // turn. (The third of them, the completion row, is a slot as well now — not because it is
    // always there but because its height must not follow its content; see
    // `let completion_slot` above.)
    let mut show_status = true;
    let mut boxed = true;
    // **The content viewport, and R20's one rule about it.** The loop used to shrink the
    // whole card (`dec_rows -= 1`), which trims from the END — and the END of a card is
    // the ladder, the deadline and the hint. So the last rows the operator needed were
    // the first rows given up, and what stayed was the wall. Only this number moves now;
    // `dec_pinned` is added in full and never enters the ladder of sacrifices.
    let mut content_rows = i.card;
    loop {
        // A windowed content costs one row for the seam that says so, and it is not
        // drawn at all when there is no room for any of it.
        let seam = usize::from(content_rows > 0 && i.card > content_rows);
        let n = content_rows
            + seam
            + i.pinned
            + usize::from(show_stuck)
            + usize::from(show_pane)
            + usize::from(show_status)
            + usize::from(show_notice)
            // **Counted by the SLOT, not by the text in it.** This is the whole fix: the
            // row's height is a fact about the composer's line, so the transcript's
            // budget does not change when the candidate list does.
            + usize::from(i.completion_slot)
            + i.link
            // **Counted in full and never sacrificed.** R30's sentence is the one
            // thing on this screen the operator must not have to go looking for: it
            // is drawn only while a stop is in flight, and a fit loop that dropped it
            // on a short terminal would make the head wait in silence — which is the
            // freeze the requirement names. `link.len()` is counted the same way and
            // for the same reason.
            + i.stopping
            // Unboxed costs one row **only when there is an alarm to show**:
            // the counters move off the border and back onto a line of their
            // own, and a counter that has moved is not what a narrow screen
            // gives up. A clean head owes that row to the transcript.
            + if boxed { 2 } else { usize::from(i.alarmed) }
            + rows
            + usize::from(hint);
        if n < i.h {
            break;
        }
        if hint {
            hint = false;
        } else if show_notice {
            show_notice = false;
        } else if rows > 1 {
            rows -= 1;
        } else if show_stuck {
            show_stuck = false;
        } else if show_pane {
            // **The pane's line goes before the turn's row and after the stuck
            // disclosure.** On a terminal this short something has to go, and the pane's
            // line is the one fact here that a reader can still get another way — the
            // program is still there, and `!term` finds it whether or not this head said
            // so. The stuck disclosure is about silence and the status row is about the
            // work; both are about the thing the reader is looking at.
            show_pane = false;
        } else if show_status {
            // **The turn's row goes before the box does**, and after the stuck disclosure:
            // on a terminal this short something has to go, and what a reader loses least by
            // losing is the aside about silence rather than the line saying work is happening.
            show_status = false;
        } else if boxed {
            boxed = false;
        } else if content_rows > 0 {
            content_rows -= 1;
        } else {
            break;
        }
    }
    Fit {
        rows,
        hint,
        show_notice,
        show_stuck,
        show_pane,
        show_status,
        boxed,
        content_rows,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(h: usize) -> FitInput {
        FitInput {
            h,
            composer: 3,
            card: 4,
            pinned: 0,
            notice: true,
            stuck: true,
            pane: true,
            completion_slot: false,
            link: 0,
            stopping: 0,
            alarmed: false,
        }
    }

    /// Room for everything: nothing gives way.
    #[test]
    fn with_room_everything_is_shown() {
        let f = fit_ladder(&input(40));
        assert_eq!(
            f,
            Fit {
                rows: 3,
                hint: true,
                show_notice: true,
                show_stuck: true,
                show_pane: true,
                show_status: true,
                boxed: true,
                content_rows: 4,
            }
        );
    }

    /// **The order things give way in**, one row at a time as the screen shrinks: the hint,
    /// the notice, the composer's extra rows, the stuck disclosure, the pane's line, the turn's
    /// row, the box — and the card's own rows last of all.
    #[test]
    fn rows_give_way_in_the_ladders_order() {
        let mut seen = Vec::new();
        let mut last = fit_ladder(&input(40));
        for h in (1..40).rev() {
            let f = fit_ladder(&input(h));
            let gave = |a: bool, b: bool| a && !b;
            if gave(last.hint, f.hint) {
                seen.push("hint");
            }
            if gave(last.show_notice, f.show_notice) {
                seen.push("notice");
            }
            if f.rows < last.rows {
                seen.push("composer");
            }
            if gave(last.show_stuck, f.show_stuck) {
                seen.push("stuck");
            }
            if gave(last.show_pane, f.show_pane) {
                seen.push("pane");
            }
            if gave(last.show_status, f.show_status) {
                seen.push("status");
            }
            if gave(last.boxed, f.boxed) {
                seen.push("box");
            }
            if f.content_rows < last.content_rows {
                seen.push("card");
            }
            last = f;
        }
        seen.dedup();
        assert_eq!(
            seen,
            [
                "hint", "notice", "composer", "stuck", "pane", "status", "box", "card"
            ]
        );
    }

    /// The link's and a stop's lines are counted and never given up: they take room from
    /// everything else instead.
    #[test]
    fn the_link_and_a_stop_are_never_given_up() {
        let mut i = input(12);
        let roomy = fit_ladder(&i);
        i.link = 2;
        i.stopping = 2;
        let tight = fit_ladder(&i);
        let shown = |f: &Fit| {
            usize::from(f.hint)
                + usize::from(f.show_notice)
                + usize::from(f.show_stuck)
                + usize::from(f.show_pane)
                + usize::from(f.show_status)
                + f.rows
                + f.content_rows
        };
        assert!(shown(&tight) < shown(&roomy), "{roomy:?} vs {tight:?}");
    }
}
