//! **The wall controller** — one state for what a session does at its context wall,
//! changed in one place, and only by a count somebody MEASURED.
//!
//! # The case it answers
//!
//! MEASURED 2026-10-11 on pg-noop, session `s-1789919514688401228` (deepseek-flash,
//! window 1,000,000): `compacting: 941101 of 1000000 deepseek tokens resident (936387 at
//! the last round + 4714 appended since)`, then on the head *"⚠ auto-compaction is off:
//! compacted from 941101 to 1039055 tokens and that is STILL within 62500 of the 1000000
//! window … and a refusal will not force another"*. The first round on the new
//! transcript (`#t18`) stored `context_tokens = 507355` — deepseek's own count; the head
//! showed 504k. The fold had HALVED the session. The no-progress guard judged it on an
//! estimate of a base the provider had not seen yet (in the wrong unit besides) and
//! latched two switches for the daemon's life: `auto_compact = false` (the pre-emptive
//! door) and `forced_gave_up` (the refused door — the overrun protection itself). The
//! operator: *"wtf is this"*, *"window shows 504k tokens"*, and *"why it didnt believe
//! what api told it"*.
//!
//! And the fold that did not help enough had nowhere to go. `plan_overrun` picks the
//! sliced fold by SIZE only (`resident + MIN_SUMMARY_ROOM > window`); at 941k of 1M the
//! history fit one summary call, so the ordinary fold ran — a summary of the first half
//! and the second half verbatim — and that verbatim half is the ~500k that remained.
//! Nothing escalated from "the ordinary fold did not make enough room" to a smaller
//! verbatim part or to the sliced fold, though the guard's own comment said the sliced
//! fold *"may still cure exactly the case the ordinary one failed on"*. It latched.
//!
//! # The states, and every transition
//!
//! ```text
//!   Armed ──fold F──▶ Awaiting{F}            (a metered base is an estimate until measured)
//!   Awaiting{F} ──measured below the wall──▶ Armed            "the F fold left N"
//!   Awaiting{F} ──measured at the wall────▶ Escalating{F'}    F' = the next rung
//!   Awaiting{Sliced} ──measured at the wall──▶ Stuck          nothing gentler is left
//!   Escalating{F'} ──the pre-emptive door──▶ fold F' ──▶ Awaiting{F'}
//!   Escalating ──measured below the wall──▶ Armed
//!   Stuck ──measured under window − 2×headroom──▶ Armed       "re-armed"
//!   any ──/compact, /auto-compact on──▶ (a fold → Awaiting) / Armed
//! ```
//!
//! The ladder is [`Fold::Ordinary`] → [`Fold::SmallerTail`] → [`Fold::Sliced`]. A local
//! session has no verbatim part to shrink (its compaction carries none — R27) and goes
//! straight from the ordinary fold to the sliced one; its base is measured the moment it
//! exists, because a local prompt IS the ledger.
//!
//! **What no state does: refuse a refused request.** A turn the provider refused for
//! length is folded from every state — the fold its size calls for when the controller
//! is `Armed`, the sliced fold from anywhere else, `Stuck` included. 2026-10-09 was this
//! session wedged for thirty refusals by a switch that stood the refused door down with
//! the pre-emptive one; `forced_gave_up` rebuilt that switch one door along, and this
//! module is what replaced both.
//!
//! `Stuck` stands down only the PRE-EMPTIVE door (and the round loop's wall stop with
//! it, the trade `auto_compact = false` always made), and it is loud — the settings row
//! the head draws its resident line from — and reversible, by a measured count or by
//! the operator.

/// **How hard a fold presses**, in the order the controller tries them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fold {
    /// What a compaction always was: the fold the size calls for (`plan_overrun`), with
    /// its verbatim part — the second half of a `plan_fold`, or a budgeted tail.
    Ordinary,
    /// The same fold with half the verbatim part: a fold that summarises up to the last
    /// quarter of the history by tokens instead of the last half, or a tail of half the
    /// budget.
    SmallerTail,
    /// Both halves summarised, nothing verbatim — `plan_cut`, asked for by name rather
    /// than reached by size.
    Sliced,
}

impl Fold {
    /// The rung above this one, or `None` at the top. A local session skips
    /// [`Fold::SmallerTail`]: it carries no verbatim part to shrink.
    pub fn next(self, metered: bool) -> Option<Fold> {
        match self {
            Fold::Ordinary if metered => Some(Fold::SmallerTail),
            Fold::Ordinary | Fold::SmallerTail => Some(Fold::Sliced),
            Fold::Sliced => None,
        }
    }

    /// The name a line prints.
    pub fn name(self) -> &'static str {
        match self {
            Fold::Ordinary => "ordinary",
            Fold::SmallerTail => "smaller-tail",
            Fold::Sliced => "sliced",
        }
    }
}

/// **The release margin, in headrooms.** A stuck session is re-armed when a measured
/// count is under `window − RELEASE_HEADROOMS × headroom` — at 1,000,000 with the
/// sixteenth reserve, under 875,000. One headroom BELOW the wall rather than at it, so a
/// count that hovers at the wall cannot flip the state once per round: re-armed, the
/// pre-emptive door fires a headroom later, after a turn's worth of growth (the reserve
/// is sized for one), not on the next round.
pub const RELEASE_HEADROOMS: u64 = 2;

/// **The wall, in the units a measurement is in** — the window as configured and its
/// reserve (`Config::shown_wall`): the provider's tokens on a metered session, the
/// server's on a local one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wall {
    pub window: u64,
    pub headroom: u64,
}

impl Wall {
    /// Within a headroom of the window — the question every compaction door asks.
    pub fn at(&self, count: u64) -> bool {
        count + self.headroom >= self.window
    }

    /// Comfortably below it: the release test. See [`RELEASE_HEADROOMS`].
    pub fn clear_of(&self, count: u64) -> bool {
        count + RELEASE_HEADROOMS * self.headroom < self.window
    }
}

/// Where the controller stands.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum WallState {
    /// The ordinary state: the pre-emptive door compacts at the wall with the
    /// ordinary fold.
    #[default]
    Armed,
    /// A fold ran onto `transcript`; what it left is unmeasured until a round on that
    /// transcript reports `prompt_tokens`. `before` is the count it folded (measured
    /// units), `estimate` the base as sized before the provider saw it — said with
    /// `≈`, and never judged.
    Awaiting {
        fold: Fold,
        transcript: String,
        before: u64,
        estimate: u64,
    },
    /// The last fold was MEASURED still at the wall; the pre-emptive door's next fold
    /// is `next`.
    Escalating { next: Fold },
    /// Every rung was measured and the sliced fold still left the session at the wall.
    /// The pre-emptive door stands down; a refused turn is still folded. `before` is
    /// what that last fold folded.
    Stuck { before: u64 },
}

/// What a measurement changed, for the one place that says it ([`crate::harness`]'s
/// `observe_wall`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// The fold made room, by the provider's own count.
    Helped {
        fold: Fold,
        before: u64,
        estimate: u64,
        measured: u64,
    },
    /// The fold did not make room; the next one presses harder.
    Escalated {
        fold: Fold,
        next: Fold,
        before: u64,
        measured: u64,
    },
    /// The top rung did not make room either.
    Stuck { before: u64, measured: u64 },
    /// A stuck session measured comfortably below the wall, and is armed again.
    Released { measured: u64 },
}

/// **The controller**: the state and the last count a provider (or, locally, the
/// prompt itself) measured. Carried on `Config`, the per-session object the settings
/// rows are built from, so the state a head draws and the state a door reads are one
/// value.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WallControl {
    pub state: WallState,
    pub last_measured: Option<u64>,
}

impl WallControl {
    /// **Which fold a door runs, or `None` for a door that stands down.** The threshold
    /// is the caller's (`Config::should_compact` for the pre-emptive door; a refusal is
    /// its own threshold). `transcript` is the one the session is on now.
    pub fn fold_for(&self, refused: bool, transcript: &str) -> Option<Fold> {
        if refused {
            // Never `None`: see the module docs. The size-chosen fold first, and the
            // hardest press once anything has been tried.
            return Some(match self.state {
                WallState::Armed => Fold::Ordinary,
                _ => Fold::Sliced,
            });
        }
        match &self.state {
            WallState::Armed => Some(Fold::Ordinary),
            // The pre-emptive door does not fold an unmeasured base again: the estimate
            // is what said 1,039,055 of a base deepseek counted at 507,355.
            WallState::Awaiting { transcript: t, .. } if t == transcript => None,
            // A later fork (a manual `/compact`, a `/clear`) replaced the base the wait
            // was for; nothing will ever measure it.
            WallState::Awaiting { .. } => Some(Fold::Ordinary),
            WallState::Escalating { next } => Some(*next),
            WallState::Stuck { .. } => None,
        }
    }

    /// **A fold landed on `transcript`.** Returns whether the session was `Stuck`
    /// before it — a fold the operator asked for (`/compact`) restarts the ladder, and
    /// the stuck pair on the store row has to go with it.
    pub fn folded(&mut self, fold: Fold, transcript: &str, before: u64, estimate: u64) -> bool {
        let was_stuck = self.is_stuck();
        self.state = WallState::Awaiting {
            fold,
            transcript: transcript.to_string(),
            before,
            estimate,
        };
        was_stuck
    }

    /// **A count was measured on `transcript`** — the only input that moves the
    /// controller past a fold. `metered` picks the ladder ([`Fold::next`]).
    pub fn measured(
        &mut self,
        transcript: &str,
        count: u64,
        wall: Wall,
        metered: bool,
    ) -> Option<Change> {
        self.last_measured = Some(count);
        match self.state.clone() {
            WallState::Awaiting {
                fold,
                transcript: t,
                before,
                estimate,
            } if t == transcript => {
                if !wall.at(count) {
                    self.state = WallState::Armed;
                    return Some(Change::Helped {
                        fold,
                        before,
                        estimate,
                        measured: count,
                    });
                }
                match fold.next(metered) {
                    Some(next) => {
                        self.state = WallState::Escalating { next };
                        Some(Change::Escalated {
                            fold,
                            next,
                            before,
                            measured: count,
                        })
                    }
                    None => {
                        self.state = WallState::Stuck { before };
                        Some(Change::Stuck {
                            before,
                            measured: count,
                        })
                    }
                }
            }
            WallState::Awaiting { .. } => {
                self.state = WallState::Armed;
                None
            }
            WallState::Escalating { .. } if !wall.at(count) => {
                self.state = WallState::Armed;
                None
            }
            WallState::Stuck { .. } if wall.clear_of(count) => {
                self.state = WallState::Armed;
                Some(Change::Released { measured: count })
            }
            _ => None,
        }
    }

    /// **The operator's re-arm** (`/auto-compact on`). Returns whether anything was
    /// stood down.
    pub fn rearm(&mut self) -> bool {
        let was = !matches!(self.state, WallState::Armed);
        self.state = WallState::Armed;
        was
    }

    pub fn is_stuck(&self) -> bool {
        matches!(self.state, WallState::Stuck { .. })
    }

    /// Whether the count on `transcript` is an unmeasured estimate of a fold's base —
    /// the one count no judgement may be made on.
    pub fn awaiting_on(&self, transcript: &str) -> bool {
        matches!(&self.state, WallState::Awaiting { transcript: t, .. } if t == transcript)
    }

    /// The pair the store row keeps for a stuck session: what the last fold folded and
    /// what was measured after it, both in measured units.
    pub fn stuck_pair(&self) -> Option<(u64, u64)> {
        match self.state {
            WallState::Stuck { before } => Some((before, self.last_measured.unwrap_or(0))),
            _ => None,
        }
    }

    /// A resume's half of [`Self::stuck_pair`].
    pub fn restore_stuck(&mut self, before: u64, measured: u64) {
        self.state = WallState::Stuck { before };
        self.last_measured = Some(measured);
    }

    /// **The settings row's value** — the state and the last measured count, in `unit`.
    pub fn row(&self, unit: &str) -> String {
        let last = match self.last_measured {
            Some(n) => format!("; last measured {n} {unit}"),
            None => "; nothing measured yet".into(),
        };
        let state = match &self.state {
            WallState::Armed => "armed".to_string(),
            WallState::Awaiting { fold, estimate, .. } => format!(
                "waiting for the next round to measure the {} fold (≈ {estimate} {unit}, \
                 estimated)",
                fold.name()
            ),
            WallState::Escalating { next } => format!(
                "the last fold left the session at the wall; the next one is {}",
                next.name()
            ),
            WallState::Stuck { before } => {
                format!("stuck: the sliced fold of {before} {unit} was measured still at the wall")
            }
        };
        format!("{state}{last}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// pg-noop's window and reserve.
    const WALL: Wall = Wall {
        window: 1_000_000,
        headroom: 62_500,
    };

    fn awaiting(c: &WallControl) -> Option<Fold> {
        match c.state {
            WallState::Awaiting { fold, .. } => Some(fold),
            _ => None,
        }
    }

    /// **The live case, replayed on the controller.** `s-1789919514688401228`: the fold
    /// of 941,101 was ESTIMATED at 1,039,055 — past the window — and the first round on
    /// `#t18` measured 507,355. On the estimate nothing is judged (the pre-emptive door
    /// does not even fire again); on the measurement the fold helped, and nothing is
    /// stood down.
    #[test]
    fn an_estimate_over_the_wall_whose_first_round_measures_well_under_it_stands_nothing_down() {
        let mut c = WallControl::default();
        assert_eq!(c.fold_for(false, "s#t17"), Some(Fold::Ordinary));
        c.folded(Fold::Ordinary, "s#t18", 941_101, 1_039_055);
        assert!(c.awaiting_on("s#t18"));
        assert_eq!(
            c.fold_for(false, "s#t18"),
            None,
            "an unmeasured base is not folded again on its estimate"
        );
        let change = c.measured("s#t18", 507_355, WALL, true);
        assert_eq!(
            change,
            Some(Change::Helped {
                fold: Fold::Ordinary,
                before: 941_101,
                estimate: 1_039_055,
                measured: 507_355
            })
        );
        assert_eq!(c.state, WallState::Armed);
        assert_eq!(c.last_measured, Some(507_355));
        assert!(!c.is_stuck());
    }

    /// **The ladder steps in order, and only on measured counts.** Ordinary, then the
    /// smaller tail, then the sliced fold, then `Stuck` — each step taken by a round that
    /// measured the base still within a headroom of the window.
    #[test]
    fn the_ladder_steps_in_order_on_measured_counts() {
        let mut c = WallControl::default();
        let mut seen = Vec::new();
        let mut t = 18;
        while let Some(fold) = c.fold_for(false, &format!("s#t{}", t - 1)) {
            seen.push(fold);
            c.folded(fold, &format!("s#t{t}"), 960_000, 400_000);
            // The estimate said room; the provider says otherwise — and it is the
            // provider's count that moves the state.
            c.measured(&format!("s#t{t}"), 950_000, WALL, true);
            t += 1;
            assert!(seen.len() <= 3, "the ladder has three rungs: {seen:?}");
        }
        assert_eq!(seen, [Fold::Ordinary, Fold::SmallerTail, Fold::Sliced]);
        assert_eq!(c.stuck_pair(), Some((960_000, 950_000)));

        // A local session has no verbatim part to shrink.
        assert_eq!(Fold::Ordinary.next(false), Some(Fold::Sliced));
    }

    /// **A refusal after `Stuck` still folds**, with the hardest press, and so does
    /// one in every other state: the 2026-10-09 wedge was a stood-down switch that
    /// the refused door also read.
    #[test]
    fn a_refusal_is_folded_from_every_state_stuck_included() {
        let mut c = WallControl::default();
        assert_eq!(c.fold_for(true, "t"), Some(Fold::Ordinary));
        c.folded(Fold::Sliced, "t1", 990_000, 900_000);
        assert_eq!(c.fold_for(true, "t1"), Some(Fold::Sliced));
        c.measured("t1", 990_000, WALL, true);
        assert!(c.is_stuck());
        assert_eq!(
            c.fold_for(false, "t1"),
            None,
            "the pre-emptive door stands down"
        );
        assert_eq!(
            c.fold_for(true, "t1"),
            Some(Fold::Sliced),
            "a refused request is never left unanswered"
        );
    }

    /// **A stuck session re-arms on a measured drop** — under the release margin, not
    /// merely under the wall, so a count hovering at the wall does not flip it.
    #[test]
    fn a_stuck_session_re_arms_when_a_measured_count_drops_comfortably_below() {
        let mut c = WallControl::default();
        c.restore_stuck(941_101, 950_000);
        // Under the wall (937,500) but within the margin (875,000): still stuck.
        assert_eq!(c.measured("t", 900_000, WALL, true), None);
        assert!(c.is_stuck());
        assert_eq!(
            c.measured("t", 507_355, WALL, true),
            Some(Change::Released { measured: 507_355 })
        );
        assert_eq!(c.state, WallState::Armed);
        assert_eq!(c.fold_for(false, "t"), Some(Fold::Ordinary));
    }

    /// **A wait a later fork replaced is dropped**, rather than standing the
    /// pre-emptive door down for a transcript nothing will ever measure.
    #[test]
    fn a_wait_on_a_replaced_transcript_does_not_hold_the_door() {
        let mut c = WallControl::default();
        c.folded(Fold::Ordinary, "t1", 941_101, 1_039_055);
        assert_eq!(c.fold_for(false, "t2"), Some(Fold::Ordinary));
        assert_eq!(c.measured("t2", 950_000, WALL, true), None);
        assert_eq!(c.state, WallState::Armed);
        assert_eq!(awaiting(&c), None);
    }

    /// `/compact` on a stuck session restarts the ladder, and says it was stuck.
    #[test]
    fn a_fold_the_operator_asks_for_unsticks_the_session() {
        let mut c = WallControl::default();
        c.restore_stuck(941_101, 950_000);
        assert!(c.folded(Fold::Ordinary, "t2", 950_000, 300_000));
        assert_eq!(awaiting(&c), Some(Fold::Ordinary));
        assert!(c.rearm());
        assert!(!c.rearm(), "armed twice is nothing to say");
    }
}
