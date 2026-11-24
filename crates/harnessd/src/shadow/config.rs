//! `[shadow]` in providers.toml, resolved: which jobs run, and on which model for a watched model.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::Job;

/// A class of call the permission job may answer for the operator, in the order they are
/// promoted. `reads` is the one that reaches outside the workspace; the other two never do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    Reads,
    WorkspaceWrites,
    Exec,
}

impl Stage {
    pub const ALL: [Stage; 3] = [Stage::Reads, Stage::WorkspaceWrites, Stage::Exec];

    /// The spelling `[shadow] answers` uses, and the card's.
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::Reads => "reads",
            Stage::WorkspaceWrites => "workspace_writes",
            Stage::Exec => "exec",
        }
    }

    pub fn parse(s: &str) -> Option<Stage> {
        Stage::ALL.into_iter().find(|x| x.as_str() == s.trim())
    }
}

/// The shadow's configuration. The default is every job off, and no stage - except the turn
/// monitor, which is on for a turn_end job that is on until a file says otherwise.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// The stages the permission job answers for. Empty is observe-only, which is the default:
    /// promoting a stage is the operator's act, never a side effect of `enabled`.
    stages: BTreeSet<Stage>,
    /// Whether the turn_end job also reads the turn for what it did beyond the ask.
    turn_monitor: bool,
    /// How much of the turn's digest that check is shown, in bytes.
    turn_monitor_max_chars: usize,
    /// The most bytes of watched conversation a persistent job's mirror holds: the last rows of
    /// the session as the model saw them, elided from the oldest by a marker.
    mirror_bytes: usize,
}

impl Default for ShadowConfig {
    /// Written out rather than derived: the turn monitor is on, and a derived `false` would turn
    /// the one check that costs no card into one nobody has.
    fn default() -> ShadowConfig {
        ShadowConfig {
            enabled: BTreeSet::new(),
            models: BTreeMap::new(),
            default: String::new(),
            stateless: false,
            fresh_every: 0,
            stages: BTreeSet::new(),
            turn_monitor: true,
            turn_monitor_max_chars: super::overreach::DEFAULT_MAX_CHARS,
            mirror_bytes: super::conversation::BUDGET,
        }
    }
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

    /// The reader's lines, typed. The reader already refused unknown jobs and stages; this is the
    /// second check for a caller that built lines by hand.
    pub fn from_lines(
        lines: letibot_provider::keys::ShadowLines,
        default: String,
    ) -> Result<ShadowConfig, String> {
        let job = |s: &str| Job::parse(s).ok_or_else(|| format!("[shadow] `{s}` is not a job"));
        let mut out = ShadowConfig {
            default,
            stateless: lines.mode.as_deref() == Some("stateless"),
            fresh_every: lines.fresh_every.unwrap_or(FRESH_EVERY),
            stages: stages_of(&lines)?,
            turn_monitor: lines.turn_monitor.unwrap_or(true),
            turn_monitor_max_chars: lines
                .turn_monitor_max_chars
                .unwrap_or(super::overreach::DEFAULT_MAX_CHARS),
            mirror_bytes: lines.mirror_bytes.unwrap_or(super::conversation::BUDGET),
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

    /// Whether the permission job answers `stage`. Only with the permission job on, and only for
    /// a stage the operator promoted.
    pub fn answers(&self, stage: Stage) -> bool {
        self.is_on(Job::Permission) && self.stages.contains(&stage)
    }

    /// Whether the permission job answers anything at all: a stage is on.
    pub fn answers_anything(&self) -> bool {
        self.is_on(Job::Permission) && !self.stages.is_empty()
    }

    /// Whether the permission job answers a relayed read-only view ask for the operator. The
    /// stage-1 switch, kept as its own name because the relay asks exactly this question.
    pub fn answers_reads(&self) -> bool {
        self.answers(Stage::Reads)
    }

    /// The stages in force, for the card and the startup disclosure.
    pub fn stages(&self) -> impl Iterator<Item = Stage> + '_ {
        self.stages.iter().copied()
    }

    /// Whether any stage is on, whatever the job switch: what a disclosure reads.
    pub fn stages_on(&self) -> bool {
        !self.stages.is_empty()
    }

    /// For a test or a caller that sets the switch in code.
    pub fn with_answer_reads(mut self, on: bool) -> ShadowConfig {
        self.stages.clear();
        if on {
            self.stages.insert(Stage::Reads);
        }
        self
    }

    /// For a test or a caller that promotes stages in code.
    pub fn with_stages(mut self, stages: impl IntoIterator<Item = Stage>) -> ShadowConfig {
        self.stages = stages.into_iter().collect();
        self
    }

    pub fn fresh_every(&self) -> u32 {
        self.fresh_every
    }

    /// The mirror's byte budget for a persistent job: how much of the watched conversation it
    /// holds before the oldest rows are elided behind a marker.
    pub fn mirror_bytes(&self) -> usize {
        self.mirror_bytes
    }

    /// For a test or a caller that sizes the mirror in code.
    pub fn with_mirror_bytes(mut self, bytes: usize) -> ShadowConfig {
        self.mirror_bytes = bytes;
        self
    }

    /// What the turn-end job is shown of what the turn did, in bytes: `None` is the monitor off,
    /// which is a `turn_monitor = false` or a turn_end job that is not on.
    pub fn turn_monitor(&self) -> Option<usize> {
        (self.turn_monitor && self.is_on(Job::TurnEnd)).then_some(self.turn_monitor_max_chars)
    }

    /// For a test or a caller that switches the monitor in code.
    pub fn with_turn_monitor(mut self, on: bool, max_chars: usize) -> ShadowConfig {
        self.turn_monitor = on;
        self.turn_monitor_max_chars = max_chars;
        self
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
            // The model that answers, and every stage it answers: the operator reads this and
            // decides whether the stages in force are the ones they promoted.
            out.push(match self.stages.is_empty() {
                true => format!(
                    "answers: none - the permission job observes, and {} would decide",
                    self.model_line(Job::Permission, watched)
                ),
                false => format!(
                    "answers: {} - {} decides, allow or ask, never deny",
                    self.stages
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    self.model_line(Job::Permission, watched)
                ),
            });
        }
        out.push(match self.persistent() {
            true => format!(
                "mode: persistent, a fresh call every {} questions",
                self.fresh_every
            ),
            false => "mode: stateless".to_string(),
        });
        if self.persistent() {
            // What the mirror holds is a number a person can check against a model's window: a
            // mirror past it is a shadow that is voiding answers it never read.
            out.push(format!(
                "mirror: the last {} bytes of the watched rows, older rows elided",
                self.mirror_bytes
            ));
        }
        out
    }
}

/// `[shadow] answers`, with `answer_reads = true` as the old spelling of `["reads"]`. Both
/// written is one setting twice, refused by name rather than silently preferring one.
fn stages_of(lines: &letibot_provider::keys::ShadowLines) -> Result<BTreeSet<Stage>, String> {
    if lines.answers.is_some() && lines.answer_reads.is_some() {
        return Err(
            "[shadow] `answers` and `answer_reads` are one setting written twice: write \
             `answers = [\"reads\"]` and drop `answer_reads`"
                .into(),
        );
    }
    let mut out = BTreeSet::new();
    for name in lines.answers.iter().flatten() {
        let s = Stage::parse(name).ok_or_else(|| {
            format!(
                "[shadow] answers names `{name}`, and the stages are {}",
                Stage::ALL
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
        out.insert(s);
    }
    if lines.answer_reads == Some(true) {
        out.insert(Stage::Reads);
    }
    Ok(out)
}
