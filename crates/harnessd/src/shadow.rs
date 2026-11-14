//! Shadow sessions (R243): a second model, on any provider, judging a session off the critical
//! path. One seat, `shadow`, with three jobs grouped by trigger. Every exchange is recorded in the
//! shadow's own stored session, and the guardrails here are code a prompt cannot override.

pub mod config;
pub mod guard;
pub mod judge;
pub mod prompts;
pub mod record;
pub mod runner;
pub mod verdict;

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

/// A harness's shadow, opened on first need: the watched session has to be in the store before
/// a shadow session can hang from it.
#[derive(Default)]
pub enum Slot {
    #[default]
    NotYet,
    Off,
    On(runner::Shadow),
}

impl Slot {
    pub fn get(&self) -> Option<&runner::Shadow> {
        match self {
            Slot::On(s) => Some(s),
            _ => None,
        }
    }
}

/// The shadow for a session with `cfg`, on `watched_model`. `NotYet` while its row or transcript is
/// still to be written; `Off` for a session no job watches, which is every session by default.
pub fn open_for(cfg: &crate::config::Config, watched_model: &str) -> Slot {
    let shadow = &cfg.shadow;
    if !shadow.any_on()
        || cfg.parent_session_id.is_some()
        || cfg.parent_seat.is_some()
        || letibot_sessionlog::is_gatekeeper_session(&cfg.session_id)
        || cfg.seat == crate::config::Seat::Shadow
    {
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
    let recorder = match record::StoreRecorder::open(store, &cfg.session_id, &label) {
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
    match runner::Shadow::start(
        shadow,
        watched_model,
        Some(&cfg.workspace),
        recorder,
        &judge_for,
        &runner::Timing::of,
        &|_| Box::new(|_, _| {}),
    ) {
        Some(s) => Slot::On(s),
        None => Slot::Off,
    }
}
