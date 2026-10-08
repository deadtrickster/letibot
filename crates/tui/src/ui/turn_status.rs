//! **The turn's status row**: what the model is doing — green while it goes, yellow when slow.

use crate::app::*;
use crate::ui::render::row;
use rano::agent::turn_status::{Prefill, SLOW_AFTER_MS, TurnStatus};

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
        let t = match self.turn.as_ref() {
            Some(t) if self.turn_busy() => t,
            _ => return String::new(),
        };
        let status = TurnStatus {
            busy: true,
            // `started_ms == 0` means the turn came out of a snapshot, which has no timestamps:
            // the row says it started before this head attached rather than a number nobody took.
            elapsed_ms: (t.started_ms != 0).then(|| self.now_ms.saturating_sub(t.started_ms)),
            prefill: t.progress.as_ref().map(|pp| Prefill {
                total: pp.total,
                cache: pp.cache,
                processed: pp.processed,
                time_ms: pp.time_ms,
            }),
            now_ms: self.now_ms,
            slow: self.turn_slow(),
        };
        row(&status.line(w), self.cfg.palette())
    }

    /// **The turn is slow**: the model should be emitting and nothing has arrived for
    /// [`SLOW_AFTER_MS`]. It turns the Responding row yellow and says nothing else — the
    /// operator, 2026-10-08: *"I dont want notification that it is slow yet we continue"*. The
    /// sentence that used to sit above the composer (*"nothing received for 17s. The turn is
    /// still marked running"*) was there because a FAILED turn used to look like this; it ends
    /// with `TurnFailed` now, so silence here is only ever slowness.
    ///
    /// **`turn_generating` and deliberately NOT `turn_busy`**: a `cargo test` that runs silently
    /// for two minutes is a call, not a slow model, and gated on busy the row would go yellow
    /// through every long command — the false alarm that trains a reader to ignore it.
    pub(crate) fn turn_slow(&self) -> bool {
        self.turn.is_some()
            && self.turn_generating()
            && self.last_event_at != 0
            && self.now_ms.saturating_sub(self.last_event_at) > SLOW_AFTER_MS
    }
}
