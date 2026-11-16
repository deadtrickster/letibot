//! A usage limit is not a failure: what a session does when its provider puts a time on one.
//! The rule - never fail on a limit with a known reset - and the two answers to it:
//! park the session until the reset, or move it to a model that is not limited and move back after.

use std::collections::BTreeMap;

use super::*;

/// The spread a park adds to the provider's own instant: two sessions refused together must not
/// call together. This is the MEAN - each park draws its own, [`letibot_http::retry::spread`]'s
/// percent either way, because a fixed couple of seconds puts a fleet back in step.
pub(crate) const PARK_JITTER_MS: u64 = 2_000;

/// One park: when the turn is owed again, which model refused, and what it said.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LimitPark {
    /// The instant the turn is owed again, on the clock a park is measured by.
    pub until_ms: u64,
    /// The provider's own reset, unbounded. Later than `until_ms` when the park hit its bound
    /// ([`letibot_http::reset::LONGEST`]) and the turn is only asked again, not lifted.
    pub reset_ms: u64,
    /// The model that refused, in `/models` spelling.
    pub model: String,
    /// The provider's own words, one line - the operator's copy, never the model's.
    pub said: String,
}

impl LimitPark {
    /// The instant in a person's words - `21:02:27 UTC` - for the status row and every sentence
    /// that says when the park ends.
    pub fn until_words(&self) -> String {
        clock_words(self.until_ms)
    }
}

/// A switch back to the model a limit moved this session off, and when it may be made.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct LimitReturn {
    /// The model that was limited, in `/models` spelling.
    pub model: String,
    pub at_ms: u64,
}

/// Early re-tries of a park ([`crate::config::Config::limit_probe`]): a park comes due at the next
/// step rather than the reset, and the first answer lifts it.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct Probing {
    /// Re-tries refused since the last answer: the index of the next step.
    refused: usize,
    /// The park the re-try in flight was lifted from: its model and its reset.
    asked: Option<(String, u64)>,
    /// This session's jitter state, seeded afresh per session: two sessions parked together draw
    /// different instants, and the same session keeps drawing from one state.
    #[serde(skip, default = "letibot_http::retry::fresh_seed")]
    rng: u64,
    /// The offset this episode's parks add, once drawn. A re-try that drew again would move the
    /// reset it is counting to, and the reset is the provider's fact, not this build's.
    #[serde(default)]
    jitter: Option<u64>,
}

impl Default for Probing {
    fn default() -> Probing {
        Probing {
            refused: 0,
            asked: None,
            rng: letibot_http::retry::fresh_seed(),
            jitter: None,
        }
    }
}

impl Probing {
    /// What this park adds to the provider's own instant: [`PARK_JITTER_MS`] moved by up to
    /// [`letibot_http::retry::JITTER_PERCENT`] either way, so two parks do not end in step. Drawn
    /// ONCE per episode, so every re-try of one park counts to the same instant.
    pub(crate) fn jitter_ms(&mut self) -> u64 {
        if let Some(at) = self.jitter {
            return at;
        }
        let drawn = letibot_http::retry::spread(
            std::time::Duration::from_millis(PARK_JITTER_MS),
            &mut self.rng,
        );
        let ms = u64::try_from(drawn.as_millis()).unwrap_or(PARK_JITTER_MS);
        self.jitter = Some(ms);
        ms
    }
}

/// Everything a session knows about its limits, as the session row keeps it. One record, so a
/// restart cannot bring back half of it: a park without its re-try step, or a way back without
/// the limit that earned it.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct LimitState {
    #[serde(default)]
    park: Option<LimitPark>,
    #[serde(default)]
    probing: Probing,
    #[serde(default, rename = "return")]
    way_back: Option<LimitReturn>,
    #[serde(default)]
    limited: Limited,
}

/// What the limit rule says to do with a failure whose ladder is spent.
pub(crate) enum LimitAnswer {
    /// The provider put a time on it: the reset as it named it, unbounded and without jitter.
    Wait { reset_ms: u64 },
    /// A rate limit that named no instant at all: wait the long beat and take the round again.
    Retry,
    /// Not a limit, or one nothing here can lift: the caller fails as it always did.
    GiveUp,
}

/// What one exhausted ladder became - the answer [`Harness::after_the_ladder`] gives the round loop.
pub(crate) enum AfterTheLadder {
    /// The session moved to another model; the caller says it with the `[fallback]` notice,
    /// and `why` is the limit's own sentence for it.
    Moved { next: String, why: String },
    /// The session is parked until the reset; `said` is the turn's whole ending.
    Parked { said: String },
    /// A rate limit with no instant: wait the long beat and take the round again.
    Retry,
    /// Nothing left to try: the turn fails as the tree always failed it.
    Failed,
    /// The limit hook had the operator answer: take the round again now.
    Again,
    /// The limit hook ended the turn; `said` is its ending.
    Stopped { said: String },
}

impl Harness {
    /// Now, on the clock a park is measured by ([`crate::config::LimitClock`]).
    pub fn limit_now_ms(&self) -> u64 {
        self.cfg.limit_clock.now_ms()
    }

    /// The park, when this session is parked.
    pub fn limit_park(&self) -> Option<&LimitPark> {
        self.limit_park.as_ref()
    }

    /// When the turn this park owes comes due, for the worker's own deadline. Now, once the
    /// session has moved off the parked model: the park was that model's, not the session's.
    pub fn limit_park_until(&self) -> Option<u64> {
        let p = self.limit_park.as_ref()?;
        Some(if self.park_moved() {
            self.limit_now_ms()
        } else {
            p.until_ms
        })
    }

    /// Is the turn this park owes due, as of `now_ms`?
    pub fn limit_park_due(&self, now_ms: u64) -> bool {
        self.limit_park
            .as_ref()
            .is_some_and(|p| p.until_ms <= now_ms || self.park_moved())
    }

    /// Has a switch moved this session off the model it is parked on?
    pub(crate) fn park_moved(&self) -> bool {
        self.limit_park
            .as_ref()
            .is_some_and(|p| p.model != self.model_name())
    }

    /// When the next early re-try of a park made now is due, or `None` when probing is off.
    fn next_probe_ms(&self, now: u64) -> Option<u64> {
        let steps = &self.cfg.limit_probe;
        let step = steps.get(self.limit_probing.refused.min(steps.len().checked_sub(1)?))?;
        Some(now.saturating_add(u64::try_from(step.as_millis()).unwrap_or(u64::MAX)))
    }

    /// A round answered. If it was a park's early re-try, the limit cleared before its reset: the
    /// model is no longer limited, and the session's log says when.
    pub(crate) fn a_round_answered(&mut self) {
        let was_refused = std::mem::take(&mut self.limit_probing.refused) > 0;
        // The limit is over: the next one is a new episode and draws its own offset.
        self.limit_probing.jitter = None;
        let Some((model, reset_ms)) = self.limit_probing.asked.take() else {
            if was_refused {
                self.save_limit_state();
            }
            return;
        };
        self.limited.remove(&model);
        self.save_limit_state();
        let now = self.limit_now_ms();
        self.hub.warn(
            "limit_cleared_early",
            format!(
                "limit cleared early at {}: {model} answered the re-try, {} before the reset it \
                 named ({})",
                clock_words(now),
                crate::leticode_config::span_words(std::time::Duration::from_millis(
                    reset_ms.saturating_sub(now)
                )),
                clock_words_from(now, reset_ms),
            ),
        );
    }

    /// Clear the park and everything that says it: the status row, the seat, the expired limits.
    fn lift_the_park(&mut self) -> Option<LimitPark> {
        let park = self.limit_park.take()?;
        self.seat.limited(None);
        self.set_limit_row(None);
        let now = self.limit_now_ms();
        self.limited.retain(|_, at| *at > now);
        self.save_limit_state();
        Some(park)
    }

    /// Esc on a parked session: the parked turn ends here, its rows stay, and the record says so
    /// in the harness's voice, so neither the model nor a restart reads the turn as still owed.
    pub fn interrupt_a_park(&mut self, reason: &str) -> Option<String> {
        let park = self.lift_the_park()?;
        self.limit_probing = Probing::default();
        self.save_limit_state();
        let now = self.limit_now_ms();
        let said = format!(
            "{} while it was parked on {}'s usage limit (reset {}){}. The turn ends here and \
             nothing more was sent; what it did is kept.",
            crate::restart::PARK_INTERRUPTED,
            park.model,
            clock_words_from(now, park.reset_ms),
            if reason.is_empty() {
                String::new()
            } else {
                format!(": {reason}")
            },
        );
        let warning = match self.record_notice(&said) {
            Ok(()) => said.clone(),
            Err(e) => format!("{said} The row could not be written: {e}"),
        };
        self.hub.warn("limit_park_interrupted", warning);
        Some(said)
    }

    /// When the switch back may be made, if one is owed.
    pub fn limit_return_at(&self) -> Option<u64> {
        self.limit_return.as_ref().map(|r| r.at_ms)
    }

    pub fn limit_return_due(&self, now_ms: u64) -> bool {
        self.limit_return
            .as_ref()
            .is_some_and(|r| r.at_ms <= now_ms)
    }

    /// Is this model limited, as of the clock? Kept per session: a model one session was refused
    /// on says nothing about another session's key.
    pub fn limited_until(&self, model: &str) -> Option<u64> {
        self.limited
            .get(model)
            .copied()
            .filter(|at| *at > self.limit_now_ms())
    }

    /// Record that a model is limited until `until_ms` - the fact a fallback list is filtered by.
    pub(crate) fn note_limited(&mut self, model: &str, until_ms: u64) {
        let now = self.limit_now_ms();
        self.limited.retain(|_, at| *at > now);
        self.limited.insert(model.to_string(), until_ms);
        self.save_limit_state();
    }

    /// What the policy says about one failure, asked once the ladder is spent. The reset is the
    /// provider's own instant, unbounded, so a weekly limit is recorded as the day it lifts; a rate
    /// limit that named none is the long retry, not a park, because there is no instant to park until.
    pub(crate) fn limit_answer(&mut self, e: &letibot_turn::HttpError) -> LimitAnswer {
        use letibot_http::retry::{Policy, Trouble};
        if !matches!(Policy::trouble(e), Some(Trouble::RateLimit)) {
            return LimitAnswer::GiveUp;
        }
        let Some(reset_ms) = Policy::reset_unbounded_ms(e, self.limit_now_ms()) else {
            return LimitAnswer::Retry;
        };
        let model = self.model_name();
        let jitter = self.limit_probing.jitter_ms();
        self.note_limited(&model, reset_ms.saturating_add(jitter));
        LimitAnswer::Wait { reset_ms }
    }

    /// Park this session on the reset at `reset_ms` and say it with `words`: the status row, the
    /// session's own log, and the turn's ending. The park itself is bounded by
    /// [`letibot_http::reset::LONGEST`], so a weekly limit is asked again rather than slept through.
    pub(crate) fn park_until(
        &mut self,
        reset_ms: u64,
        words: Option<String>,
        e: &letibot_turn::HttpError,
    ) -> String {
        let model = self.model_name();
        let said = said_one_line(&e.to_string(), 200);
        let now = self.limit_now_ms();
        // A re-try that was refused again moves the next one a step out.
        if self.limit_probing.asked.take().is_some() {
            self.limit_probing.refused += 1;
        }
        let reset_ms = reset_ms.saturating_add(self.limit_probing.jitter_ms());
        let probe_ms = self.next_probe_ms(now).map_or(u64::MAX, |at| {
            at.saturating_add(self.limit_probing.jitter_ms())
        });
        let until_ms = reset_ms
            .min(
                now.saturating_add(letibot_http::reset::LONGEST)
                    .saturating_add(self.limit_probing.jitter_ms()),
            )
            .min(probe_ms);
        let words = words.unwrap_or_else(|| park_words(now, until_ms, reset_ms));
        self.limit_park = Some(LimitPark {
            until_ms,
            reset_ms,
            model: model.clone(),
            said: said.clone(),
        });
        self.save_limit_state();
        self.write_limit_row(Some(parked_row(&model, now, until_ms, reset_ms)));
        self.seat.limited(Some((until_ms, words.clone())));
        self.hub.warn(
            "limit_park",
            format!(
                "{model} said `{said}`, so this session is {words}. Nothing is lost - the daemon \
                 opens the turn by itself and a prompt sent meanwhile waits with it. Esc ends the \
                 turn now, and `/models` moves it to another model now."
            ),
        );
        words
    }

    /// The reset has come: clear the park, say it, and hand back the notice the resumed turn opens
    /// with. `None` when this session is not parked or the instant has not arrived.
    pub fn take_a_due_park(&mut self) -> Option<String> {
        if !self.limit_park_due(self.limit_now_ms()) {
            return None;
        }
        if self.park_moved() {
            return self.unpark_after_a_switch();
        }
        let park = self.lift_the_park()?;
        let early = park.until_ms < park.reset_ms;
        if early {
            self.limit_probing.asked = Some((park.model.clone(), park.reset_ms));
            self.save_limit_state();
        }
        self.hub.warn(
            "limit_resumed",
            if early {
                format!(
                    "{} is asked again before its reset at {}, so the turn it stopped goes on by \
                     itself; a refusal parks it again",
                    park.model,
                    clock_words_from(park.until_ms, park.reset_ms)
                )
            } else {
                format!(
                    "the limit on {} lifted, so the turn it stopped goes on by itself - nothing \
                     was sent to make that happen",
                    park.model
                )
            },
        );
        // A park cut short by its bound has not seen the limit lift, and must not say it did.
        let what = if park.until_ms < park.reset_ms {
            format!(
                "{} is limited until {} and this turn is asked again at {}",
                park.model,
                clock_words_from(park.until_ms, park.reset_ms),
                clock_words(park.until_ms)
            )
        } else {
            format!(
                "the limit on {} lifted at {}",
                park.model,
                clock_words(park.until_ms)
            )
        };
        Some(format!(
            "[limit] {what}; this turn was parked on it and nothing it did was lost. Pick the work \
             up where it stopped and carry on; do not repeat what is already done."
        ))
    }

    /// The park lifted by a switch: the new model is not the one that refused, so the parked
    /// request is asked of it now instead of at the reset. `None` when not parked or not moved.
    pub(crate) fn unpark_after_a_switch(&mut self) -> Option<String> {
        if !self.park_moved() {
            return None;
        }
        let park = self.lift_the_park()?;
        self.limit_probing = Probing::default();
        self.save_limit_state();
        // The switch was the operator's, not the limit rule's: no way back is owed, so no row
        // says the session is on the new model until a reset.
        self.write_limit_row(None);
        let now = self.limit_now_ms();
        let here = self.model_name();
        self.hub.warn(
            "limit_unparked",
            format!(
                "this session was parked on {}'s limit until {}, and it is on {here} now, so the \
                 parked turn goes on there without waiting for the reset",
                park.model,
                clock_words_from(now, park.reset_ms)
            ),
        );
        Some(format!(
            "[limit] this turn was parked on {}'s usage limit and the session has moved to {here}; \
             nothing it did was lost. Pick the work up where it stopped and carry on; do not repeat \
             what is already done.",
            park.model
        ))
    }

    /// The turn a park owes, opened at the reset by a notice in the harness's voice: the same door
    /// a fallback uses, because the prompt that was refused is still the last thing said and the
    /// model has to be told why it is being asked again.
    pub fn resume_after_limit(&mut self, notice: String) -> Result<Reply, HarnessError> {
        let mut text = notice;
        let riding = self.batch_riding_in();
        let mut batch = String::new();
        if let Some(now) = &riding {
            batch = now.text();
            if !batch.is_empty() {
                text.push_str("\n\n");
                text.push_str(&batch);
            }
        }
        // The wall is checked BEFORE the send, like every other door: a resumed turn is not more
        // likely to fit, and a first round refused for LENGTH would read as a limit again.
        self.compact_if_at_the_wall();
        // A re-try whose last one was refused finds that notice as the last row and sends it again:
        // rows are append-only, and one notice per refusal grows the conversation for ever.
        let notice_stands = batch.is_empty()
            && self.limit_probing.asked.is_some()
            && self
                .items()
                .iter()
                .rev()
                .find(|i| !crate::restart::display_only(i))
                .is_some_and(crate::restart::a_parks_resume);
        self.trail.begin_turn();
        let mut items = Vec::new();
        if !notice_stands {
            self.trail.say(Speaker::Agent, &text, Some(Instant::now()));
            items.push(TranscriptItem::User {
                speaker: letibot_transcript::Speaker::Agent,
                parts: vec![UserPart::Text { text }],
            });
        }
        let mut opened = false;
        let out = self.submit_items_then(items, |h| {
            opened = true;
            h.the_batch_stored(riding.as_ref());
        });
        self.the_batch_arrived(riding, opened);
        out
    }

    /// Record the way back a switch earns: the model the limit moved this session off, and when
    /// its limit lifts. Called where the switch is RECORDED, so the return can never point at a
    /// move that did not happen.
    pub(crate) fn note_the_way_back_from_a_limit(&mut self, model: &str, at_ms: u64) {
        self.limit_return = Some(LimitReturn {
            model: model.to_string(),
            at_ms,
        });
        self.save_limit_state();
    }

    /// The switch back, owed once and made here: `/models`' own door, so a session that moved for a
    /// limit comes home the same way one the operator typed a switch does.
    pub fn return_from_a_limit(&mut self) -> Option<String> {
        let back = self.limit_return.clone()?;
        if !self.limit_return_due(self.limit_now_ms()) {
            return None;
        }
        self.limit_return = None;
        self.save_limit_state();
        if self.model_name() == back.model {
            // Already home: an operator who moved the session back by hand owes nobody a notice.
            self.set_limit_row(None);
            return None;
        }
        let (provider, model) = crate::slash::split_model_name(&back.model);
        let Ok((choice, _notes)) =
            crate::slash::models_choice(&provider, model.as_deref(), None, None)
        else {
            return Some(format!(
                "the limit on {} lifted, and this build cannot resolve that name any more, so \
                 this session stays on {}",
                back.model,
                self.model_name()
            ));
        };
        let crate::slash::ModelChoice::Metered(pc) = choice else {
            return Some(format!(
                "the limit on {} lifted, and it is not a metered model any more, so this session \
                 stays on {}",
                back.model,
                self.model_name()
            ));
        };
        match self.set_provider(Some(pc)) {
            Ok(line) => {
                self.set_limit_row(None);
                self.limited.remove(&back.model);
                self.save_limit_state();
                if let Err(why) = self.persist_provider_choice() {
                    self.hub.warn("model_switch_applied", why);
                }
                let said = format!("the limit on {} lifted: {line}", back.model);
                self.hub.warn("limit_returned", said.clone());
                Some(said)
            }
            Err(e) => Some(format!(
                "the limit on {} lifted, and the switch back did not take: {e}. This session \
                 stays on {}",
                back.model,
                self.model_name()
            )),
        }
    }

    /// Write the limit state to the session row when it changed. A failed write is said and the
    /// state holds in memory: the park is still right for this daemon.
    pub(crate) fn save_limit_state(&mut self) {
        let Some(store) = &self.store else {
            return;
        };
        let state = LimitState {
            park: self.limit_park.clone(),
            probing: Probing {
                refused: self.limit_probing.refused,
                asked: self.limit_probing.asked.clone(),
            },
            way_back: self.limit_return.clone(),
            limited: self.limited.clone(),
        };
        let empty = state.park.is_none()
            && state.way_back.is_none()
            && state.limited.is_empty()
            && state.probing.refused == 0
            && state.probing.asked.is_none();
        let json = if empty {
            None
        } else {
            match serde_json::to_string(&state) {
                Ok(j) => Some(j),
                Err(e) => {
                    self.hub.warn(
                        "limit_state_unsaved",
                        format!("the limit state could not be encoded: {e}"),
                    );
                    return;
                }
            }
        };
        if json == self.limit_saved {
            return;
        }
        match store.set_limit_state(&self.cfg.session_id, json.as_deref()) {
            Ok(()) => self.limit_saved = json,
            Err(e) => {
                self.hub.warn(
                    "limit_state_unsaved",
                    format!(
                        "the limit state was not written to the session row: {e}. It holds until \
                         this daemon stops"
                    ),
                );
            }
        }
    }

    /// Bring back the limit state the session row holds: the park and its re-try step, the way
    /// back and the limited models. Called after the provider choice is restored, because a park
    /// on another model than the session's is a park a switch already lifted.
    pub fn restore_limit_state(&mut self) {
        let Some(json) = self
            .store
            .as_ref()
            .and_then(|s| s.limit_state(&self.cfg.session_id).ok())
            .flatten()
        else {
            return;
        };
        let state: LimitState = match serde_json::from_str(&json) {
            Ok(s) => s,
            Err(e) => {
                self.hub.warn(
                    "limit_state_unread",
                    format!(
                        "the session row's limit state could not be read ({e}), so it is dropped"
                    ),
                );
                return;
            }
        };
        self.limit_saved = Some(json);
        let now = self.limit_now_ms();
        self.limited = state.limited;
        self.limited.retain(|_, at| *at > now);
        self.limit_return = state.way_back;
        self.limit_probing = state.probing;
        self.limit_park = state.park;
        match self.limit_park.clone() {
            Some(park) => {
                let words = park_words(now, park.until_ms, park.reset_ms);
                self.write_limit_row(Some(parked_row(
                    &park.model,
                    now,
                    park.until_ms,
                    park.reset_ms,
                )));
                self.seat.limited(Some((park.until_ms, words)));
                self.hub.warn(
                    "limit_park_restored",
                    format!(
                        "this session was parked on {}'s limit when the daemon stopped, so the \
                         parked turn is owed again at {}. The daemon opens it by itself then.",
                        park.model,
                        clock_words_from(now, park.until_ms)
                    ),
                );
            }
            None => self.set_limit_row(None),
        }
        self.save_limit_state();
    }

    /// The status row's own words for a limit, or `None` when nothing is limited: `paused until
    /// 21:02:27`, or `glm limited until 21:02, on deepseek` while the session works elsewhere.
    pub(crate) fn set_limit_row(&self, park: Option<String>) {
        let words = match park {
            Some(park) => Some(park),
            None => self.limit_row_while_moved(),
        };
        self.write_limit_row(words);
    }

    /// Say one set of words on the status row, and let the hub re-derive: the writer every other
    /// row goes through, so no two callers can disagree about what the hub holds.
    pub(crate) fn write_limit_row(&self, words: Option<String>) {
        *self.limit_row.lock().unwrap_or_else(|e| e.into_inner()) = words;
        self.publish_activity();
    }

    /// What the ladder's exhaustion became, decided here so a park, a move and the long retry are
    /// one rule rather than three arms that can drift apart in the round loop.
    pub(crate) fn after_the_ladder(
        &mut self,
        e: &letibot_turn::HttpError,
        tried: &mut Vec<String>,
    ) -> AfterTheLadder {
        use crate::leticode_config::OnLimit;
        if let Some(hooked) = self.limit_hook(e) {
            return hooked;
        }
        let here = self.model_name();
        // Asked first, and for every failure: it is what records `here` as limited when the provider
        // named an instant, which is the fact a fallback list is filtered by.
        match self.limit_answer(e) {
            LimitAnswer::Wait { reset_ms } => {
                let now = self.limit_now_ms();
                let within = self.cfg.wait_if_within;
                let left = std::time::Duration::from_millis(reset_ms.saturating_sub(now));
                if self.cfg.on_limit == OnLimit::Switch {
                    // A reset this close costs less waited for than a switch there and back.
                    if left < within {
                        let until = reset_ms.saturating_add(self.limit_probing.jitter_ms());
                        let words = wait_words(now, until, within);
                        let said = self.park_until(reset_ms, Some(words), e);
                        return AfterTheLadder::Parked { said };
                    }
                    if let Some(next) = self.fall_back_from(e, tried) {
                        let back_ms = reset_ms.saturating_add(self.limit_probing.jitter_ms());
                        self.note_the_way_back_from_a_limit(&here, back_ms);
                        let why = switch_words(&here, now, back_ms, within, &next);
                        self.write_limit_row(Some(why.clone()));
                        return AfterTheLadder::Moved { next, why };
                    }
                }
                // No usable fallback - none named, or every one of them limited too - so waiting
                // is the only answer left, and it is the one the operator ruled for anyway.
                let said = self.park_until(reset_ms, None, e);
                AfterTheLadder::Parked { said }
            }
            LimitAnswer::Retry => AfterTheLadder::Retry,
            LimitAnswer::GiveUp => AfterTheLadder::Failed,
        }
    }

    /// The row a session that MOVED carries: which model is limited, until when, and where the
    /// turns go instead. `None` when nothing is limited or the session is on it.
    fn limit_row_while_moved(&self) -> Option<String> {
        let now = self.limit_now_ms();
        let here = self.model_name();
        let (model, at) = self
            .limited
            .iter()
            .filter(|(model, at)| **at > now && **model != here)
            .min_by_key(|(_, at)| **at)?;
        Some(switch_words(
            model,
            now,
            *at,
            self.cfg.wait_if_within,
            &here,
        ))
    }
}

/// The models this session has been told are limited and when they lift - the order is not a
/// fact, so a `BTreeMap` and one spelling of the reading.
pub(crate) type Limited = BTreeMap<String, u64>;

/// `21:02:27`, UTC: the instant a provider named, in the one zone this box can compute without a
/// crate. The provider's own text names none, so every word written from a parse says UTC with it.
fn clock_words(ms: u64) -> String {
    let secs = ms / 1000;
    let (h, m, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    format!("{h:02}:{m:02}:{s:02} UTC")
}

/// `at` as seen from `now`: the bare clock within a day, the weekday within a week, and the date
/// beyond, so a weekly or monthly reset is never read as today's.
fn clock_words_from(now_ms: u64, at_ms: u64) -> String {
    const DAY: u64 = 86_400_000;
    let clock = clock_words(at_ms);
    let ahead = at_ms.saturating_sub(now_ms);
    if ahead < DAY {
        return clock;
    }
    const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let days = at_ms / DAY;
    // 1970-01-01 was a Thursday.
    let weekday = WEEKDAYS[((days + 4) % 7) as usize];
    if ahead < 6 * DAY {
        return format!("{weekday} {clock}");
    }
    let (y, m, d) = civil_from_days(days as i64);
    format!("{weekday} {y:04}-{m:02}-{d:02} {clock}")
}

/// Howard Hinnant's `civil_from_days`: the date of a day count since 1970-01-01.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// The park that waits by choice: `waiting 12m for the reset at 21:02:27 UTC (under
/// wait_if_within 45m)`.
fn wait_words(now_ms: u64, reset_ms: u64, within: std::time::Duration) -> String {
    use crate::leticode_config::span_words;
    let left = std::time::Duration::from_millis(reset_ms.saturating_sub(now_ms));
    format!(
        "waiting {} for the reset at {} (under wait_if_within {})",
        span_words(left),
        clock_words_from(now_ms, reset_ms),
        span_words(within)
    )
}

/// The move a limit earned: `glm reset Thu 14:00:00 UTC is past wait_if_within 45m - on deepseek
/// until then`.
fn switch_words(
    model: &str,
    now_ms: u64,
    reset_ms: u64,
    within: std::time::Duration,
    next: &str,
) -> String {
    format!(
        "{model} reset {} is past wait_if_within {} - on {next} until then",
        clock_words_from(now_ms, reset_ms),
        crate::leticode_config::span_words(within)
    )
}

/// The status row while parked: the reset and the two ways out of the wait.
fn parked_row(model: &str, now_ms: u64, until_ms: u64, reset_ms: u64) -> String {
    let again = if until_ms < reset_ms {
        format!(", asking again at {}", clock_words_from(now_ms, until_ms))
    } else {
        String::new()
    };
    format!(
        "parked on {model}'s limit until {}{again} - esc ends the turn, /models switches",
        clock_words_from(now_ms, reset_ms)
    )
}

/// The park with no choice in it: `paused until 21:02:27 UTC`, or for a reset past the park's
/// bound, `limited until Thu 14:00:00 UTC, asking again at 03:22:29 UTC`.
fn park_words(now_ms: u64, until_ms: u64, reset_ms: u64) -> String {
    if until_ms >= reset_ms {
        return format!("paused until {}", clock_words_from(now_ms, until_ms));
    }
    format!(
        "limited until {}, asking again at {}",
        clock_words_from(now_ms, reset_ms),
        clock_words_from(now_ms, until_ms)
    )
}

/// `s` in one line, cut to `n` bytes on a char boundary: a provider's body is prose with newlines
/// in it, and a warning carries one line.
pub(super) fn said_one_line(s: &str, n: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.len() <= n {
        return flat;
    }
    let mut end = n;
    while !flat.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &flat[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clock_words_are_a_wall_clock_in_utc() {
        // 2026-11-07 21:02:27 UTC.
        assert_eq!(clock_words(1_794_085_347_000), "21:02:27 UTC");
        assert_eq!(clock_words(0), "00:00:00 UTC");
    }

    #[test]
    fn a_reset_beyond_a_day_names_its_day() {
        // 2026-11-07 21:02:27 UTC is a Saturday.
        let at = 1_794_085_347_000;
        assert_eq!(clock_words_from(at - 3_600_000, at), "21:02:27 UTC");
        assert_eq!(
            clock_words_from(at - 3 * 86_400_000, at),
            "Sat 21:02:27 UTC"
        );
        assert_eq!(
            clock_words_from(at - 20 * 86_400_000, at),
            "Sat 2026-11-07 21:02:27 UTC"
        );
    }

    #[test]
    fn the_limit_sentences_say_which_answer_was_taken() {
        let at = 1_794_085_347_000;
        let m45 = std::time::Duration::from_secs(45 * 60);
        assert_eq!(
            wait_words(at - 12 * 60_000, at, m45),
            "waiting 12m for the reset at 21:02:27 UTC (under wait_if_within 45m)"
        );
        assert_eq!(
            switch_words("glm", at - 3 * 86_400_000, at, m45, "deepseek"),
            "glm reset Sat 21:02:27 UTC is past wait_if_within 45m - on deepseek until then"
        );
        assert_eq!(
            park_words(at - 3_600_000, at, at),
            "paused until 21:02:27 UTC"
        );
        assert_eq!(
            park_words(at - 3 * 86_400_000, at - 3 * 86_400_000 + 6 * 3_600_000, at),
            "limited until Sat 21:02:27 UTC, asking again at 03:02:27 UTC"
        );
    }

    #[test]
    fn a_providers_body_becomes_one_short_line() {
        assert_eq!(
            said_one_line("http 429: Usage limit\nreached for 5 hour.", 80),
            "http 429: Usage limit reached for 5 hour."
        );
        let cut = said_one_line(&"x".repeat(300), 40);
        assert!(cut.ends_with('…') && cut.len() <= 44, "{cut}");
    }
}
