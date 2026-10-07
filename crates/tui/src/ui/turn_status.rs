//! **The turn's status row**: what the model is doing, and a stuck turn said out loud.

use crate::app::*;
use crate::render::{dur_human, sgr, trim_to};
use crate::ui::*;
use letibot_ui::progress;
use letibot_ui::style::Role;

impl App {
    /// The turn's status, inlaid in the composer's bottom border and pinned
    /// right: the spinner, what phase the turn is in, and — once anything has
    /// arrived — how much. Empty when nothing is running.
    ///
    /// §5.6's prefill progress is the thing nothing surveyed reports, and it is
    /// not a small difference: both projects read for `letibot-ui` talk to a
    /// metered API and have **no prefill number at all**, so their in-flight line
    /// can only say `Responding… 15s`. `PromptProgress { total, cache, processed,
    /// time_ms }` supports a three-segment bar that answers *how far along* and
    /// *how much of this did the prefix cache save me* at once, which is the
    /// number §10 says the whole prompt pipeline exists to move.
    ///
    /// The arithmetic is `progress::Prefill`'s and the module header is worth
    /// reading before touching it: `processed` **includes** `cache`, so the
    /// fraction is `processed / total` and the work done is `processed - cache`.
    /// Read the other way a 90 %-cached prompt shows as 10 % done and then jumps
    /// to 100 %, which is the classic progress-bar lie; and the throughput has to
    /// divide by the computed tokens or it reports a cache hit as a speed in the
    /// hundreds of thousands.
    ///
    /// # The spinner runs on this head's clock
    ///
    /// It used to be keyed off `t.last_ms`, the last event's timestamp, and the
    /// defect was visible: a spinner that only moves when a token or a prefill
    /// batch arrives is not a spinner, it is a snapshot of one — a tool running
    /// thirty silent seconds froze it on one glyph. The phase is `now_ms` now,
    /// which the driver advances every tick whether or not anything arrived.
    /// The duration is measured against the same clock: head and daemon share
    /// the machine, which is the assumption the stuck line below already makes
    /// when it diffs `now_ms` against an event timestamp.
    pub(crate) fn turn_status(&self, w: usize) -> String {
        // **Busy, not the state name** — `turn_busy`'s docstring has the measurement. This gate
        // read `TurnState::Running`, so during every tool call the row was not drawn AT ALL,
        // which is the worst of the three possible answers: not the wrong tense, but no line —
        // indistinguishable from a head that has stopped, on the one screen whose whole job is to
        // say the work is still going (R51 item 3).
        //
        // **AND THE WORD IS `Responding` FOR THE WHOLE TURN — including the wait on a call, and
        // including the silence before the first token.** Ruled by the operator, 2026-09-27:
        // *"responding spans entire turn"*. So the tense is deliberately not the state name: a
        // head that said *waiting on a call* here would be naming the daemon's machinery instead
        // of the answer's arrival, and the turn IS the wait. What says *which* call is running is
        // the transcript's own row (`[3 tool calls, 51 thinking lines]`) and the call's card, not
        // this line. **This was proposed as a defect twice and refused twice**; the row's job is
        // to say the turn is alive, and `TurnPane::arrived_chars` is what makes its number move
        // while the model is emitting.
        let t = match self.turn.as_ref() {
            Some(t) if self.turn_busy() => t,
            _ => return String::new(),
        };
        // **`started_ms == 0` means the turn came out of a snapshot**, which has no
        // timestamps — the same case `Phase::Replayed` exists for on a tool card.
        // `last_ms` is then an epoch millisecond and the difference is one, so the
        // line read `Responding · 496940h16m`. Found by switching into a session
        // that was mid-turn, which is the case the whole switch feature is for.
        // `Responding · 4.2s` when the duration was measured, and `Responding
        // since you attached` when it was not — never a number nobody took.
        let since = match t.started_ms {
            0 => " · started before this head attached".to_string(),
            started => format!(
                " · {}",
                progress::duration(self.now_ms.saturating_sub(started))
            ),
        };
        let p = self.cfg.palette();
        let spin = p.paint(Role::Pending, &progress::spinner(self.now_ms).to_string());
        // **NO COUNT ON THIS ROW.** Ruled by the operator, 2026-09-27: *"i dont care about those
        // chars"* / *"just dont show me them"*. It carried `· 18.0k chars` (the whole stream since
        // the last round began) or `· 2,826 tok` when the server's own counter had spoken, and
        // three days of argument went into which number was honest — the character count missing
        // the reasoning and tool-call channels, and then, once fixed, still being a number nobody
        // reads. **A row whose job is to say the turn is alive does not need a volume**: what
        // says it is the spinner and the clock, and a figure that the reader has to interpret is
        // the row asking to be studied rather than glanced at.
        //
        // The plumbing is gone with it — `TurnPane::tokens` and `TurnPane::arrived_chars` were
        // written by three arms of the delta fold and read by nothing else, so the increments go
        // too. `TokensGenerated` is still *counted* as a rendered event (it moves the spinner's
        // clock, which is the row's whole business now).
        match &t.progress {
            Some(pp) if pp.total > 0 && pp.processed < pp.total => {
                let pf = progress::Prefill {
                    total: pp.total,
                    cache: pp.cache,
                    processed: pp.processed,
                    time_ms: pp.time_ms,
                };
                format!(
                    "{spin} {}",
                    progress::prefill_line(&pf, w.saturating_sub(6), p),
                )
            }
            // Generation running — prefill finished, or never reported at all, which is the
            // `messages` backend's turns. **The spinner, the word and the clock; nothing else.**
            // It is one row of its own (it used to be a legend on the composer's border, where it
            // put `Responding` at the left edge and clipped whatever it was carrying), and what
            // it carries now is deliberately the least a reader has to interpret: the glyph that
            // moves, the word that spans the turn, and how long the turn has been going.
            //
            // The prompt's size and cache are on the header, and the run's counts are on the
            // transcript's own marker (`[2 tool calls, 265 thinking lines]`). This row does not
            // repeat either — the operator's ruling is that it says the work is alive, and a
            // number it repeats from somewhere else is the row asking to be studied.
            _ => p.paint(Role::Pending, &format!("{spin} Responding{since}")),
        }
    }

    /// A turn that is generating and silent. The daemon sends prefill progress
    /// while it prefills and a delta per chunk while it generates, so a gap this
    /// long is a real gap and not a slow model — and the case that produced this
    /// line is one a head cannot otherwise show: when a turn *fails*, the engine
    /// publishes a `Warning` and nothing else, so `TurnState` stays `Running`
    /// and the old head span its spinner at a dead session indefinitely. See the
    /// report: `TurnFinished`/`TurnInterrupted` on failure is the daemon's to
    /// fix, and a head saying "nothing for 40s" is not a substitute for it.
    ///
    /// A row of its own, above the border, and not inlaid: it is a disclosure
    /// with a sentence in it, and a sentence truncated to fit a border is a
    /// disclosure that lost the words that mattered.
    ///
    /// **`turn_generating` and deliberately NOT `turn_busy`, and this is the one site where
    /// R51's instruction has to be read carefully.** R51 lists this line among the sites keyed on
    /// the state name; measured, its gate is right and widening it would be a regression. The
    /// question here is not *is the model working* but *should it be emitting and is it not* —
    /// and a `cargo test` that runs silently for two minutes is a call, not a stall. Gated on
    /// `turn_busy` this line would fire through every long command, which is exactly the false
    /// alarm that trains a reader to ignore it.
    pub(crate) fn stuck_line(&self, w: usize) -> Option<String> {
        let t = self.turn.as_ref()?;
        if !self.turn_generating() {
            return None;
        }
        let quiet = if self.last_event_at == 0 {
            0
        } else {
            self.now_ms.saturating_sub(self.last_event_at)
        };
        if quiet > 15_000 {
            return Some(colour(
                &self.cfg,
                sgr::YELLOW,
                &trim_to(
                    &format!(
                        "{} — nothing received for {}. The turn is still marked running; \
                         esc esc interrupts it.",
                        t.model,
                        dur_human(quiet)
                    ),
                    w,
                ),
            ));
        }
        None
    }
}
