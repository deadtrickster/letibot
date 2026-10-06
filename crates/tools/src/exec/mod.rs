//! The exec substrate: **a lifetime mechanism, not an isolation one.**
//!
//! `bash` was the largest gap in `docs/tool-survey.md` §5 — all five surveyed
//! harnesses have a shell and we had none — and it was a deliberate gap:
//! [`crate::backend::HostBackend::run`] refused by construction because *"exec
//! needs the adjudication boundary (§11.4)"*. This module is the part of that
//! sentence which is a **substrate**. It is not the other part.
//!
//! # What is here and what is emphatically not
//!
//! | | |
//! |---|---|
//! | **here** | every process a turn starts is a member of a cgroup owned by a scope, and a scope that ends kills its cgroup and **records what it killed** |
//! | **here** | a shell command whose process predicate matches the process evaluating it, or a process the harness manages, is refused with the diagnosis (T21.1, T21.2) |
//! | **here, since layer 1** | [`confine`] — project-scoped mount, PID, network and user namespaces, so a secret outside the project is **absent** rather than denied |
//! | **not here** | the transcript edge. §3's invariant has two halves and this module delivers the first: secret bytes stay consumable inside the boundary and out of the view. *Never entering the transcript* is a choke point on tool results that `docs/boundary-and-adjudication.md` §5 still lists as open. |
//! | **not here** | layers 2 and 3 — normalisation through a grammar, and the adjudicator. A boundary is what makes those a defence in depth rather than the only guard. |
//!
//! That distinction is why the constructors are spelled separately.
//! [`crate::backend::HostBackend::executable`] is the **unconfined** exec path and
//! says so in everything it reports; [`crate::backend::HostBackend::confined`] is
//! the one that adds the namespaces, and it **fails rather than degrading** when
//! they are not available.
//!
//! # Why cgroups, and what they retire
//!
//! `TODO.md` T24, from the operator (D4): *"just do byobu, which connects to
//! cgroups — use dependent. if a vm is temporary then it is a session cgroup
//! otherwise not. still must be clearly reapable."* Three scopes and no fourth:
//! [`ScopeKind::Turn`], [`ScopeKind::Session`], [`ScopeKind::Explicit`]. A child
//! scope's cgroup is a **directory inside** its parent's, so a parent that ends
//! reaps its children by construction rather than by remembering to.
//!
//! The part that pays for itself immediately is that this makes a whole class of
//! command **unnecessary**:
//!
//! | the pattern form | the scope form |
//! |---|---|
//! | `pkill -f <name>` | `job_kill` — a job id, or a scope |
//! | `until pgrep -f <name>; do sleep 1; done` | `job_list` — is this cgroup populated |
//!
//! On 2026-09-09 a process check self-matched **five times in one session** on this
//! box, with the lesson in memory and T21 open in `TODO.md`. The fifth used the
//! bracket trick, which defeats `pgrep` — but the shell wrapper echoed the expanded
//! pattern back into its own command line, so the literal string was there to find.
//! A hazard with that many spellings is not one you check for. [`predicate`] still
//! checks, because clause 1 says a refusal must carry its diagnosis — but the check
//! is **not the mechanism**. The mechanism is that the model never needed to write
//! the pattern, and the refusal's remedy is the tool that has none.
//!
//! # A reaper whose zero is unfalsifiable is the empty-haystack bug one layer up
//!
//! From the fleet, on a box reporting zero orphans: *"I killed a stray llama-server
//! earlier tonight after a benchmark, and I only noticed because I went looking. My
//! zero is partly attention, not only design."* A zero produced by vigilance and a
//! zero produced by a mechanism are the same number and different facts.
//!
//! So [`Reaping`] is not a boolean. It carries the members observed **before** the
//! kill, the mechanism that did it, and the members still there **after** — and
//! `docs/closed-loop.md`'s ordering is kept: presence first, then absence. A record
//! saying `observed 0, killed 0` and one saying `observed 3, killed 3` are
//! different facts and are stored as such.
//!
//! # Layout
//!
//! | module | what |
//! |---|---|
//! | [`confine`] | the namespaces: the mount view, the measured [`Boundary`], and the refusal when there is none |
//! | [`scope`] | the cgroup tree, the three scopes, and [`Reaping`] |
//! | [`jobs`] | one running command: its capture ring, its state, its denominator |
//! | [`monitor`] | a condition watched ACROSS turns, keyed on a handle and never on a pattern (T24) |
//! | [`host`] | [`host::ProcessHost`], the seam a firecode backend would also implement, and the host implementation |
//! | [`predicate`] | T21.1 and T21.2 — what the harness knows that the model cannot |
//! | [`pty`] | a pseudo-terminal, for the operator's own run: a pipe is not a terminal, so `ls` prints plain |
//! | [`terminal`] | the programs that pty must NOT be handed to — `nano`, `less`, a bare `python` — refused by name, before the run, with the reason and the remedy |

pub mod confine;
pub mod host;
pub mod jobs;
pub mod monitor;
pub mod predicate;
pub mod procs;
pub mod pty;
pub mod scope;
pub mod terminal;

pub use confine::{
    Boundary, Bwrap, ConfinePlan, Confinement, Egress, Grant, HomeView, Namespace, NoConfinement,
    NsState, Presence, Seal, SealKind, Unconfined, ViewSpec,
};
pub use host::{HostProcesses, JobView, ProcessHost, Promotion, Protected, SpawnRequest, Waited};
pub use jobs::{JobId, JobState, OutputSlice};
pub use monitor::{
    ChannelCondition, CommandCondition, CommandExpect, Condition, CustomWatch, Fired, Firing,
    LogTailCondition, Monitor, MonitorError, Monitors, PortState, TimerCondition, Wait, Watch,
};
pub use predicate::{Hazard, Predicate, Verdict, Witness};
pub use pty::Pty;
pub use scope::{Cgroup2, Migration, NoScopes, Reaped, Reaping, ScopeId, ScopeKind, ScopeTree};
pub use terminal::{Class, Refusal, wants_the_terminal};

/// Why an exec request did not become a process.
///
/// Every variant names the thing that was missing, because a refusal a caller
/// cannot act on is the refusal that costs an hour. `Display` is what the model
/// reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecError {
    /// No scope mechanism. **Fail closed**: a process with no owner is the leak
    /// T24 exists to stop, so it is not started.
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
    /// **A boundary was asked for and is not there.** Fail closed, the same shape
    /// as [`ExecError::NoScopes`] one layer over: a command that would have been
    /// confined is not run unconfined instead, because a boundary that silently is
    /// not there is worse than none.
    NoConfinement(String),
    /// The path is outside this session's filesystem view. Distinguished from
    /// "does not exist" **on purpose** — see [`confine`]'s rule 4.
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

/// A spill budget shaped for a session that can exec, with `dsh`'s three good
/// properties taken (`docs/tool-survey.md` §3.5).
///
/// Two of the three are already in [`crate::spill::Spiller::apply`] — the notice's
/// byte cost is reserved *inside* the cap, and a store failure falls back to the
/// untouched inline content rather than erroring the call. The third is this
/// function's whole reason to exist: **the recovery tool is excluded by name.**
///
/// dsh excludes `read` because a spilled `read` result is fetched with `read`, and
/// a spill of that is a read → spill → read loop with no fixed point. Ours is
/// `job_output`: it is the way back to a `bash` result that was capped, so a
/// `job_output` that can itself spill is the same loop wearing a different name.
pub fn exec_budget(default_cap: usize) -> crate::spill::PerToolBudget {
    let mut per_tool = std::collections::BTreeMap::new();
    // Not an absent entry: `PerToolBudget` falls back to `default` for a tool it
    // has no entry for, so "no opinion for this one tool" has to be spelled as a
    // cap it cannot reach. `job_output` bounds itself with `offset`/`limit` and
    // reports its own denominator, which is the bound that actually applies.
    per_tool.insert("job_output".to_string(), usize::MAX);
    crate::spill::PerToolBudget {
        default: Some(default_cap),
        per_tool,
    }
}
