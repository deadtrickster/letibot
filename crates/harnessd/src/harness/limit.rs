//! A usage limit is not a failure: what a session does when its provider puts a time on one.
//! The operator's ruling - *never fail on a limit with a known reset* - and the two answers to it:
//! park the session until the reset, or move it to a model that is not limited and move back after.

use std::collections::BTreeMap;

use super::*;

/// What a park adds to the provider's own instant: two sessions refused together must not call
/// together, and a couple of seconds is enough to spread a fleet of them.
pub(crate) const PARK_JITTER_MS: u64 = 2_000;

/// One park: when the turn is owed again, which model refused, and what it said.
#[derive(Debug, Clone)]
pub struct LimitPark {
    /// The instant the turn is owed again, on the clock a park is measured by.
    pub until_ms: u64,
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
#[derive(Debug, Clone)]
pub(crate) struct LimitReturn {
    /// The model that was limited, in `/models` spelling.
    pub model: String,
    pub at_ms: u64,
}

/// What the limit rule says to do with a failure whose ladder is spent.
pub(crate) enum LimitAnswer {
    /// The provider put a time on it: park the session and owe the turn at that instant.
    Wait { until_ms: u64 },
    /// A rate limit that named no instant at all: wait the long beat and take the round again.
    Retry,
    /// Not a limit, or one nothing here can lift: the caller fails as it always did.
    GiveUp,
}

/// What one exhausted ladder became - the answer [`Harness::after_the_ladder`] gives the round loop.
pub(crate) enum AfterTheLadder {
    /// The session moved to another model; the caller says it with the `[fallback]` notice.
    Moved { next: String },
    /// The session is parked until the reset; `said` is the turn's whole ending.
    Parked { said: String },
    /// A rate limit with no instant: wait the long beat and take the round again.
    Retry,
    /// Nothing left to try: the turn fails as the tree always failed it.
    Failed,
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

    /// When the turn this park owes comes due, for the worker's own deadline.
    pub fn limit_park_until(&self) -> Option<u64> {
        self.limit_park.as_ref().map(|p| p.until_ms)
    }

    /// Is the turn this park owes due, as of `now_ms`?
    pub fn limit_park_due(&self, now_ms: u64) -> bool {
        self.limit_park
            .as_ref()
            .is_some_and(|p| p.until_ms <= now_ms)
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
    }

    /// What the policy says about one failure, asked once the ladder is spent. The reset is the
    /// provider's own instant when it named one, plus the jitter; a rate limit that named none is
    /// the long retry, not a park, because there is no instant to park until.
    pub(crate) fn limit_answer(&mut self, e: &letibot_turn::HttpError) -> LimitAnswer {
        use letibot_http::retry::{Policy, Trouble};
        if !matches!(Policy::trouble(e), Some(Trouble::RateLimit)) {
            return LimitAnswer::GiveUp;
        }
        let Some(at) = Policy::reset_ms(e, self.limit_now_ms()) else {
            return LimitAnswer::Retry;
        };
        let until_ms = at.saturating_add(PARK_JITTER_MS);
        let model = self.model_name();
        self.note_limited(&model, until_ms);
        LimitAnswer::Wait { until_ms }
    }

    /// Park this session until `until_ms` and say it: the status row, the session's own log, and
    /// the words the turn's ending carries. `None` is the words the resumed turn opens with.
    pub(crate) fn park_until(&mut self, until_ms: u64, e: &letibot_turn::HttpError) -> String {
        let model = self.model_name();
        let said = said_one_line(&e.to_string(), 200);
        let words = format!("paused until {}", clock_words(until_ms));
        self.limit_park = Some(LimitPark {
            until_ms,
            model: model.clone(),
            said: said.clone(),
        });
        self.write_limit_row(Some(words.clone()));
        self.seat.limited(Some((until_ms, words.clone())));
        self.hub.warn(
            "limit_park",
            format!(
                "the provider put a time on its limit, so this session is {words} and the turn it \
                 stopped is owed again at the reset: {model} said `{said}`. Nothing is lost and \
                 nothing needs sending - the daemon opens the turn by itself. A prompt sent \
                 meanwhile waits for the same instant, and `/models` moves the session somewhere \
                 else now if waiting is not what you want."
            ),
        );
        words
    }

    /// The reset has come: clear the park, say it, and hand back the notice the resumed turn opens
    /// with. `None` when this session is not parked or the instant has not arrived.
    pub fn take_a_due_park(&mut self) -> Option<String> {
        let park = self.limit_park.clone()?;
        if park.until_ms > self.limit_now_ms() {
            return None;
        }
        self.limit_park = None;
        self.seat.limited(None);
        self.set_limit_row(None);
        let now = self.limit_now_ms();
        self.limited.retain(|_, at| *at > now);
        self.hub.warn(
            "limit_resumed",
            format!(
                "the limit on {} lifted, so the turn it stopped goes on by itself - nothing was \
                 sent to make that happen",
                park.model
            ),
        );
        Some(format!(
            "[limit] the limit on {model} lifted at {at}; this turn was parked on it and nothing \
             it did was lost. Pick the work up where it stopped and carry on; do not repeat what \
             is already done.",
            model = park.model,
            at = clock_words(park.until_ms),
        ))
    }

    /// The turn a park owes, opened at the reset by a notice in the harness's voice: the same door
    /// a fallback uses, because the prompt that was refused is still the last thing said and the
    /// model has to be told why it is being asked again.
    pub fn resume_after_limit(&mut self, notice: String) -> Result<Reply, HarnessError> {
        let mut text = notice;
        let (riding, told) = self.batch_riding_in();
        if let Some(meanwhile) = riding {
            text.push_str("\n\n");
            text.push_str(&meanwhile);
        }
        // The wall is checked BEFORE the send, like every other door: a resumed turn is not more
        // likely to fit, and a first round refused for LENGTH would read as a limit again.
        self.compact_if_at_the_wall();
        self.trail.begin_turn();
        self.trail.say(Speaker::Agent, &text, Some(Instant::now()));
        let mut opened = false;
        let out = self.submit_items_then(
            vec![TranscriptItem::User {
                speaker: letibot_transcript::Speaker::Agent,
                parts: vec![UserPart::Text { text }],
            }],
            |_| opened = true,
        );
        self.the_batch_arrived(told, opened);
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
    }

    /// The switch back, owed once and made here: `/models`' own door, so a session that moved for a
    /// limit comes home the same way one the operator typed a switch does.
    pub fn return_from_a_limit(&mut self) -> Option<String> {
        let back = self.limit_return.clone()?;
        if !self.limit_return_due(self.limit_now_ms()) {
            return None;
        }
        self.limit_return = None;
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
        let here = self.model_name();
        // Asked first, and for every failure: it is what records `here` as limited when the provider
        // named an instant, which is the fact a fallback list is filtered by.
        match self.limit_answer(e) {
            LimitAnswer::Wait { until_ms } => {
                if self.cfg.on_limit == OnLimit::Switch
                    && let Some(next) = self.fall_back_from(e, tried)
                {
                    // The same row `limit_answer` wrote is rewritten from the move, so the status
                    // line says where the turns go and not only which model is out.
                    self.note_the_way_back_from_a_limit(&here, until_ms);
                    self.write_limit_row(Some(format!(
                        "{here} limited until {}, on {next}",
                        clock_words(until_ms)
                    )));
                    return AfterTheLadder::Moved { next };
                }
                // No usable fallback - none named, or every one of them limited too - so waiting
                // is the only answer left, and it is the one the operator ruled for anyway.
                let said = self.park_until(until_ms, e);
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
        Some(format!(
            "{model} limited until {}, on {here}",
            clock_words(*at)
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

/// `s` in one line, cut to `n` bytes on a char boundary: a provider's body is prose with newlines
/// in it, and a warning carries one line.
fn said_one_line(s: &str, n: usize) -> String {
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
    fn a_providers_body_becomes_one_short_line() {
        assert_eq!(
            said_one_line("http 429: Usage limit\nreached for 5 hour.", 80),
            "http 429: Usage limit reached for 5 hour."
        );
        let cut = said_one_line(&"x".repeat(300), 40);
        assert!(cut.ends_with('…') && cut.len() <= 44, "{cut}");
    }
}
