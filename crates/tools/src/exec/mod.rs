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
//! | **not here** | §11.4's guest boundary — *the guest sees a copy of one project and nothing else of the host*. A cgroup bounds a process's **lifetime**, not what it can read. |
//!
//! That distinction is why [`crate::backend::HostBackend::executable`] is spelled
//! as its own constructor and says `unsandboxed` in what it reports. A session
//! running this substrate can still read the operator's home directory. It simply
//! cannot leak a process past the scope that started it.
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
//! | [`scope`] | the cgroup tree, the three scopes, and [`Reaping`] |
//! | [`jobs`] | one running command: its capture ring, its state, its denominator |
//! | [`host`] | [`host::ProcessHost`], the seam a firecode backend would also implement, and the host implementation |
//! | [`predicate`] | T21.1 and T21.2 — what the harness knows that the model cannot |

pub mod host;
pub mod jobs;
pub mod predicate;
pub mod scope;

pub use host::{HostProcesses, JobView, ProcessHost, Protected, SpawnRequest, Waited};
pub use jobs::{JobId, JobState, OutputSlice};
pub use predicate::{Hazard, Predicate, Verdict, Witness};
pub use scope::{Cgroup2, NoScopes, Reaped, Reaping, ScopeId, ScopeKind, ScopeTree};

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
