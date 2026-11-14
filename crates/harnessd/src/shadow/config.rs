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
}

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
            ..ShadowConfig::default()
        };
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

    pub fn any_on(&self) -> bool {
        !self.enabled.is_empty()
    }

    pub fn enabled(&self) -> impl Iterator<Item = Job> + '_ {
        self.enabled.iter().copied()
    }

    /// The model `job` runs on for a session on `watched`: its own line, then `provider/*`, then
    /// `*`, then `[default]`. `None` when the job is off.
    pub fn model_for(&self, job: Job, watched: &str) -> Option<String> {
        if !self.is_on(job) {
            return None;
        }
        Some(
            self.models
                .get(&job)
                .and_then(|t| crate::leticode_config::by_model(t, watched))
                .cloned()
                .unwrap_or_else(|| self.default.clone()),
        )
    }

    /// One line per job that is on, for the shadow session's first row and `--status`.
    pub fn disclose(&self, watched: &str) -> Vec<String> {
        Job::ALL
            .into_iter()
            .map(|j| match self.model_for(j, watched) {
                Some(m) => format!("{}: on, {m}", j.as_str()),
                None => format!("{}: off", j.as_str()),
            })
            .collect()
    }
}
