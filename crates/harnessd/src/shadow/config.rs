//! `[shadow]` in providers.toml, resolved: which jobs run, and on which model for a watched model.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::Job;

/// The shadow's configuration. The default is every job off.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShadowConfig {
    enabled: BTreeSet<Job>,
    /// Per job, watched-model key to the model the job runs on.
    models: BTreeMap<Job, BTreeMap<String, String>>,
    /// `[default]` as one model spelling, for a job that is on with no model line. `local` when
    /// there is no `[default]`, which is what a daemon started bare would answer on.
    default: String,
    /// One fresh call per trigger instead of a running conversation: the fallback and the
    /// comparison.
    stateless: bool,
    /// Every Nth persistent question is also asked statelessly; 0 is never.
    fresh_every: u32,
    /// `answer_reads = true` turns it on; unset is off. Turning it on promotes the job from
    /// observing to acting, so it is the operator's act, never a side effect of `enabled`.
    answer_reads_on: bool,
}

/// How often a persistent question is checked against a fresh call when the file does not say.
pub const FRESH_EVERY: u32 = 10;

impl ShadowConfig {
    /// The operator's file, or `file` for a test. A fault in it is every job off, said by name.
    pub fn load(file: Option<&Path>) -> Result<ShadowConfig, String> {
        let lines = letibot_provider::keys::shadow(file).map_err(|e| e.to_string())?;
        let default = letibot_provider::keys::default_choice(file)
            .map_err(|e| e.to_string())?
            .map(|d| match d.model {
                Some(m) => format!("{}/{m}", d.provider),
                None => d.provider,
            })
            .unwrap_or_else(|| "local".to_string());
        ShadowConfig::from_lines(lines, default)
    }

    /// The reader's lines, typed. The reader already refused unknown jobs; this is the second
    /// check for a caller that built lines by hand.
    pub fn from_lines(
        lines: letibot_provider::keys::ShadowLines,
        default: String,
    ) -> Result<ShadowConfig, String> {
        let job = |s: &str| Job::parse(s).ok_or_else(|| format!("[shadow] `{s}` is not a job"));
        let mut out = ShadowConfig {
            default,
            stateless: lines.mode.as_deref() == Some("stateless"),
            fresh_every: lines.fresh_every.unwrap_or(FRESH_EVERY),
            answer_reads_on: lines.answer_reads == Some(true),
            ..ShadowConfig::default()
        };
        if let Some(m) = lines.mode.as_deref()
            && !letibot_provider::keys::SHADOW_MODES.contains(&m)
        {
            return Err(format!(
                "[shadow] mode `{m}` is not persistent or stateless"
            ));
        }
        for j in &lines.enabled {
            out.enabled.insert(job(j)?);
        }
        for (j, table) in lines.models {
            out.models.insert(job(&j)?, table);
        }
        Ok(out)
    }

    pub fn is_on(&self, job: Job) -> bool {
        self.enabled.contains(&job)
    }

    /// Each job a running conversation, unless `mode = "stateless"`.
    pub fn persistent(&self) -> bool {
        !self.stateless
    }

    /// Whether the permission job answers a relayed read-only view ask for the operator.
    /// Only with the permission job on and `answer_reads = true`.
    pub fn answers_reads(&self) -> bool {
        self.is_on(Job::Permission) && self.answer_reads_on
    }

    /// For a test or a caller that sets the switch in code.
    pub fn with_answer_reads(mut self, on: bool) -> ShadowConfig {
        self.answer_reads_on = on;
        self
    }

    pub fn fresh_every(&self) -> u32 {
        self.fresh_every
    }

    /// For a test or a caller that picks the mode in code.
    pub fn with_mode(mut self, persistent: bool, fresh_every: u32) -> ShadowConfig {
        self.stateless = !persistent;
        self.fresh_every = fresh_every;
        self
    }

    pub fn any_on(&self) -> bool {
        !self.enabled.is_empty()
    }

    pub fn enabled(&self) -> impl Iterator<Item = Job> + '_ {
        self.enabled.iter().copied()
    }

    /// The model `job` runs on for a session on `watched`: its own line, then `provider/*`, then
    /// `*`, then `[default]`. `None` when the job is off.
    pub fn model_for(&self, job: Job, watched: &str) -> Option<String> {
        self.is_on(job).then(|| self.model_line(job, watched))
    }

    /// [`ShadowConfig::model_for`] whether or not the job is on: what a replay judges with.
    pub fn model_line(&self, job: Job, watched: &str) -> String {
        self.models
            .get(&job)
            .and_then(|t| crate::leticode_config::by_model(t, watched))
            .cloned()
            .unwrap_or_else(|| self.default.clone())
    }

    /// One line per job that is on, for the shadow session's first row and `--status`.
    pub fn disclose(&self, watched: &str) -> Vec<String> {
        let mut out: Vec<String> = Job::ALL
            .into_iter()
            .map(|j| match self.model_for(j, watched) {
                Some(m) => format!("{}: on, {m}", j.as_str()),
                None => format!("{}: off", j.as_str()),
            })
            .collect();
        if self.is_on(Job::Permission) {
            out.push(match self.answers_reads() {
                true => "answer_reads: on, relayed read-only view asks".to_string(),
                false => "answer_reads: off".to_string(),
            });
        }
        out.push(match self.persistent() {
            true => format!(
                "mode: persistent, a fresh call every {} questions",
                self.fresh_every
            ),
            false => "mode: stateless".to_string(),
        });
        out
    }
}
