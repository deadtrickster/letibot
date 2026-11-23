//! Shadow sessions: a second model, on any provider, judging a session off the critical
//! path. One seat, `shadow`, with three jobs grouped by trigger. Every exchange is recorded in the
//! shadow's own stored session, and the guardrails here are code a prompt cannot override.

pub mod config;
pub mod conversation;
pub mod during;
pub mod feed;
pub mod guard;
pub mod judge;
/// The permission ask as it shipped before the split, frozen for `--shadow-arm`.
pub mod legacy;
pub mod oracle;
pub mod overreach;
pub mod permission;
pub mod prompts;
pub mod record;
pub mod replay;
pub mod resume;
pub mod runner;
pub mod turn_end;
pub mod verdict;
pub mod words;

#[cfg(test)]
mod oracle_tests;
#[cfg(test)]
mod overreach_tests;
#[cfg(test)]
mod persistent_tests;
#[cfg(test)]
mod tests;

use std::time::Duration;

/// What a shadow job is for, by the trigger that starts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Job {
    /// A turn ended: the asleep reminder and note capture, in one call.
    TurnEnd,
    /// The turn goes on: drift, skill offers and note recall.
    During,
    /// A tool call waits on the gate: allow or ask, never deny.
    Permission,
}

impl Job {
    pub const ALL: [Job; 3] = [Job::TurnEnd, Job::During, Job::Permission];

    /// The spelling `[shadow]` uses, and the shadow session's rows.
    pub fn as_str(self) -> &'static str {
        match self {
            Job::TurnEnd => "turn_end",
            Job::During => "during",
            Job::Permission => "permission",
        }
    }

    pub fn parse(s: &str) -> Option<Job> {
        Job::ALL.into_iter().find(|j| j.as_str() == s)
    }

    /// How long a verdict may take. Past it the check is skipped; for permission that is `ask`.
    pub fn deadline(self) -> Duration {
        match self {
            Job::TurnEnd => Duration::from_secs(30),
            Job::During => Duration::from_secs(20),
            Job::Permission => Duration::from_millis(2_500),
        }
    }

    /// The median latency past which the breaker pauses the job: half the deadline, so a job
    /// that only just makes it every time is paused before it starts missing.
    pub fn budget(self) -> Duration {
        self.deadline() / 2
    }

    /// Whether the runner drops a late verdict as stale. Only turn_end's: a late during verdict is
    /// still a drift measurement with its own stale rule (`during::operator_since`), and
    /// permission answers inside the oracle's bounded wait, so it is never late.
    pub fn acts(self) -> bool {
        self == Job::TurnEnd
    }

    /// What runs while this job is paused or off - said in the row that pauses it.
    pub fn fallback(self) -> &'static str {
        match self {
            Job::TurnEnd => "the regex intent nudge",
            Job::During => "similarity offers",
            Job::Permission => "asking the operator",
        }
    }
}

/// The shadow session's id for a watched session: derived, so a restart finds the same record.
pub fn session_id(watched: &str) -> String {
    format!("{watched}-shadow")
}

/// One outcome a watched session acts on, at its next wake.
#[derive(Debug, Clone)]
pub struct Event {
    pub job: Job,
    pub position: u64,
    pub outcome: runner::Outcome,
}

pub type Events = std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<Event>>>;

/// Rings the watched session's wake, so an arrived verdict is acted on between turns.
pub type Wake = std::sync::Arc<dyn Fn() + Send + Sync>;

/// A harness's shadow, opened on first need: the watched session has to be in the store before
/// a shadow session can hang from it.
#[derive(Default)]
pub enum Slot {
    #[default]
    NotYet,
    Off,
    On {
        shadow: std::sync::Arc<runner::Shadow>,
        events: Events,
        recorder: std::sync::Arc<dyn record::Recorder>,
        session_id: String,
    },
}

impl Slot {
    pub fn get(&self) -> Option<&runner::Shadow> {
        match self {
            Slot::On { shadow, .. } => Some(shadow),
            _ => None,
        }
    }

    /// The queue verdicts arrive on, the recorder, and the shadow session's id.
    pub fn parts(&self) -> Option<(Events, std::sync::Arc<dyn record::Recorder>, String)> {
        match self {
            Slot::On {
                events,
                recorder,
                session_id,
                ..
            } => Some((events.clone(), recorder.clone(), session_id.clone())),
            _ => None,
        }
    }
}

/// Whether this session gets a shadow: a job on, a top-level session that is not a package, a
/// gatekeeper or a shadow itself, and a store to record in.
pub fn watches(cfg: &crate::config::Config) -> bool {
    cfg.shadow.any_on()
        && cfg.parent_session_id.is_none()
        && cfg.parent_seat.is_none()
        && cfg.session_origin != letibot_sessionlog::origin::SessionOrigin::Package
        && !letibot_sessionlog::is_gatekeeper_session(&cfg.session_id)
        && cfg.seat != crate::config::Seat::Shadow
        && cfg.store_at().is_some()
}

/// Whether the shadow is this session's oracle: [`watches`], with a stage promoted.
pub fn is_oracle(cfg: &crate::config::Config) -> bool {
    cfg.shadow.answers_anything() && watches(cfg)
}

/// The shadow for a session with `cfg`, on `watched_model`. `NotYet` while its row or transcript is
/// still to be written. `Off` for a session no job watches, which is every session by default.
pub fn open_for(cfg: &crate::config::Config, watched_model: &str, wake: Wake) -> Slot {
    let shadow = &cfg.shadow;
    if !watches(cfg) {
        return Slot::Off;
    }
    let Some(at) = cfg.store_at() else {
        return Slot::Off;
    };
    let Ok(store) = at.open_existing() else {
        return Slot::NotYet;
    };
    let label = shadow
        .enabled()
        .find_map(|j| shadow.model_for(j, watched_model))
        .unwrap_or_default();
    let recorder: std::sync::Arc<dyn record::Recorder> =
        match record::StoreRecorder::open(store, &cfg.session_id, &label) {
            Ok(r) => std::sync::Arc::new(r),
            Err(record::OpenError::NotYet(_)) => return Slot::NotYet,
            Err(record::OpenError::Fault(why)) => {
                eprintln!("  {} · no shadow: {why}", cfg.session_id);
                return Slot::Off;
            }
        };
    let providers = cfg.providers_file.clone();
    let local = cfg.endpoint.clone();
    let judge_for = |job: Job, model: &str| -> std::sync::Arc<dyn judge::Judge> {
        match judge::build(model, job, providers.as_deref(), &local) {
            Ok(j) => j,
            Err(why) => std::sync::Arc::new(judge::Unbuildable {
                label: model.to_string(),
                why,
            }),
        }
    };
    let events: Events = Default::default();
    let on_outcome = |job: Job| -> runner::OnOutcome {
        let (events, wake) = (events.clone(), wake.clone());
        Box::new(move |position, outcome| {
            // turn_end acts at the next wake. A during verdict waits for the next round, where
            // its skill pick may act. Permission acts through `oracle::ShadowOracle`, which
            // waits on its own ask; a queued outcome is only recorded.
            let during = job == Job::During && matches!(outcome, runner::Outcome::Verdict(_));
            if !(job == Job::TurnEnd || during) || *outcome == runner::Outcome::Superseded {
                return;
            }
            if during {
                // The newest during verdict replaces one not yet acted on: it judged later rows.
                let mut q = events.lock().unwrap_or_else(|e| e.into_inner());
                q.retain(|e| e.job != Job::During);
                q.push_back(Event {
                    job,
                    position,
                    outcome: outcome.clone(),
                });
                return;
            }
            events
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push_back(Event {
                    job,
                    position,
                    outcome: outcome.clone(),
                });
            wake();
        })
    };
    match runner::Shadow::start(
        shadow,
        watched_model,
        Some(&cfg.workspace),
        recorder.clone(),
        &judge_for,
        &runner::Timing::of,
        &on_outcome,
    ) {
        Some(s) => Slot::On {
            shadow: std::sync::Arc::new(s),
            events,
            recorder,
            session_id: session_id(&cfg.session_id),
        },
        None => Slot::Off,
    }
}
