//! The exec substrate: a lifetime mechanism, not an isolation one.
//!
//! Every process a turn starts is a member of a cgroup owned by a scope, so a
//! scope that ends kills its cgroup and records what it killed. Confinement is
//! here too; the transcript edge and the adjudicator are not.

pub mod ask;
pub mod confine;
pub mod console;
#[cfg(target_os = "macos")]
pub mod darwin;
pub mod github;
pub mod host;
pub mod jobs;
pub mod memory;
pub mod monitor;
pub mod plugin_condition;
pub mod predicate;
pub mod procs;
pub mod pty;
pub mod scope;
pub mod seatbelt;
pub mod shell;
pub mod term;
pub mod terminal;

pub use ask::{Waiting, last_line, waiting_for_an_answer};
pub use confine::{
    Boundary, Bwrap, ConfinePlan, Confinement, Egress, EnvSource, Grant, HomeView, Mechanism,
    Namespace, NoConfinement, NsState, Piece, Presence, Prop, PropState, Seal, SealKind, Session,
    TmpView, Unconfined, ViewSpec,
};
pub use host::{
    Adopted, Aggregate, DeadlineFired, Deadlines, Handover, HostProcesses, JobView, ProcessHost,
    Promotion, Protected, SpawnRequest, Waited, hand_over, job_label,
};
pub use jobs::{
    DEADLINE_KILL, JobId, JobState, MAX_JOB_NAME, MemoryCap, OutputFile, OutputSlice, Stdin,
    job_name,
};
pub use monitor::{
    ChannelCondition, CommandCondition, CommandExpect, Condition, CustomWatch, Fired, Firing,
    LogTailCondition, Monitor, MonitorError, Monitors, PortState, TimerCondition, Wait, Watch,
};
pub use plugin_condition::{Look, LookChild, NotArmed, PluginCondition, PluginStack};
pub use predicate::{Hazard, Predicate, Verdict, Witness};
pub use pty::Pty;
#[cfg(target_os = "macos")]
pub use scope::ProcessGroups;
pub use scope::{
    Cgroup2, DAEMON_LEAF, HOST_KILL, MemoryUse, Migration, NoScopes, Reaped, Reaping, ScopeId,
    ScopeKind, ScopeTree, SiblingScope, delegate_memory, host_tree, human_bytes, join_script,
    live_members, memory_use_of,
};
pub use shell::{ShellConfig, ShellError, ShellSession, Turn};
pub use term::{TermConfig, TermError, TermSession, TermSink};
pub use terminal::{Class, Refusal, wants_the_terminal};

/// Why an exec request did not become a process. Every variant names what was missing: a refusal the
/// caller cannot act on is the one that costs an hour, and `Display` is what the model reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecError {
    /// No scope mechanism. Fail closed: a process with no owner is a leak, so
    /// it is not started.
    NoScopes(String),
    /// The cgroup tree refused an operation, with the operation named.
    Cgroup {
        op: &'static str,
        path: String,
        why: String,
    },
    /// The process could not be started at all.
    Spawn(String),
    /// No job by that id.
    NoSuchJob(String),
    /// No scope by that id.
    NoSuchScope(String),
    /// A boundary was asked for and is not there. Fail closed, as with
    /// [`ExecError::NoScopes`]: a command that would have been confined is not
    /// run unconfined instead.
    NoConfinement(String),
    /// The path is outside this session's filesystem view. Kept distinct from
    /// "does not exist" on purpose; see [`confine`].
    NotInView { path: String, view: String },
    /// A credential could not be made usable inside the boundary without also
    /// making it readable, and the second is not an available outcome.
    NoCredentialMechanism(String),
}

impl std::fmt::Display for ExecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExecError::NoScopes(why) => write!(
                f,
                "there is no process-lifetime mechanism on this backend, so a process \
                 started here would have no owner and nothing would reap it: {why}"
            ),
            ExecError::Cgroup { op, path, why } => {
                write!(f, "cgroup {op} failed on `{path}`: {why}")
            }
            ExecError::Spawn(e) => write!(f, "the process did not start: {e}"),
            ExecError::NoSuchJob(id) => write!(f, "no job called `{id}`"),
            ExecError::NoSuchScope(id) => write!(f, "no scope called `{id}`"),
            ExecError::NoConfinement(why) => write!(
                f,
                "this session asks for a project-scoped boundary and does not have \
                 one, so the command was NOT run. Running it unconfined is not an \
                 available outcome here — a boundary that silently is not there is \
                 worse than none, and a confined-looking result about an unconfined \
                 run is the defect this refuses to produce. What is missing: {why}"
            ),
            ExecError::NotInView { path, view } => write!(
                f,
                "`{path}` is not in this session's filesystem view, so the command \
                 was NOT run. This is the boundary and not a missing file: the view \
                 is {view}. No other spelling of the path reaches it — the mount \
                 namespace does not contain it."
            ),
            ExecError::NoCredentialMechanism(why) => write!(
                f,
                "no credential could be made usable inside the boundary without also \
                 making it readable, and making it readable is not an available \
                 outcome: {why}"
            ),
        }
    }
}

impl std::error::Error for ExecError {}

/// A spill budget for a session that can exec, with the recovery tool excluded
/// by name: a spill of the tool that reads spills is a read, spill, read loop
/// with no fixed point, and `job_output` is that tool here.
pub fn exec_budget(default_cap: usize) -> crate::spill::PerToolBudget {
    let mut per_tool = std::collections::BTreeMap::new();
    // Not an absent entry: `PerToolBudget` falls back to `default` for a tool it
    // has no entry for, so "no opinion" must be a cap it cannot reach.
    // `job_output` bounds itself with `offset`/`limit` and reports that bound.
    per_tool.insert("job_output".to_string(), usize::MAX);
    crate::spill::PerToolBudget {
        default: Some(default_cap),
        per_tool,
    }
}
