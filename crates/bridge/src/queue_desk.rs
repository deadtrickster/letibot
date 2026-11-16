//! `queue`: a Claude Code agent puts its pushed branch up for the merge queue and reads its row.
//! The queue owns the review, the gate and the landing, so an agent never runs the workspace gate.
//! `leticode_queue_verb` is the operator's `/queue` verbs, and `wait` is `letibot-mcp wait-queue`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use letibot_harnessd::mergequeue::{desk, enqueue, verbs};
use letibot_tokencore::store::{MergeRow, MergeState, ReviewState, Store};
use serde_json::{Value, json};

use crate::tools::Bridge;

/// The tool's name.
pub const TOOL: &str = "queue";

/// The tool's schema, for `tools/list`.
pub fn definition() -> Value {
    json!({
        "name": TOOL,
        "description": "The merge queue of this folder's repository. `enqueue` puts a pushed \
            branch up for review: the gatekeeper reviews it, then the queue rebases it onto the \
            target with the other approved branches, gates them once and lands them. Push, \
            enqueue, stop: never run the workspace gate yourself. `status` with `id` reads that \
            row (review and landing state, and why); without `id` it reads the repository's queue.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["enqueue", "status"] },
                "branch": { "type": "string", "description": "enqueue: the pushed local branch." },
                "why": { "type": "string", "description": "enqueue: what the work is for - the reviewer judges the branch against it." },
                "target": { "type": "string", "description": "enqueue: the branch to land on. Omitted: the project's `[queue] target`." },
                "id": { "type": "string", "description": "status: the row id `enqueue` answered." },
                "folder": { "type": "string", "description": "A folder of the repository. Omitted: this bridge's folder." }
            },
            "required": ["action"]
        }
    })
}

fn arg<'a>(args: &'a Value, name: &str) -> Option<&'a str> {
    args.get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// The daemon's store, refused when it is not there: a store made here would be one no queue serves.
fn store(bridge: &Bridge) -> Result<Store, String> {
    let at = crate::holds::store_of(bridge)?;
    if !crate::store::exists(&at) {
        return Err(format!(
            "there is no store at {}: no daemon has run for this folder, so no queue serves it",
            at.display()
        ));
    }
    crate::store::open(&at, "merge_queue", None)
}

/// `tools/call` for `queue`.
pub fn call(bridge: &Bridge, args: &Value) -> Result<Value, String> {
    let folder = arg(args, "folder")
        .map(PathBuf::from)
        .unwrap_or_else(|| bridge.folder());
    let (repo, _) = desk::resolve(&folder, None, Some("-"))?;
    let store = store(bridge)?;
    match arg(args, "action") {
        Some("enqueue") => {
            let branch = arg(args, "branch").ok_or("enqueue needs `branch`")?;
            let why = arg(args, "why").unwrap_or_default();
            let by = format!("bridge:{}", crate::rooms::handle());
            let row = enqueue::enqueue_branch(
                &store,
                Path::new(&repo),
                branch,
                arg(args, "target"),
                why,
                &by,
                letibot_tools::lifecycle::now_ms().max(0) as u64,
            )?;
            Ok(json!({
                "id": row.id,
                "branch": row.branch,
                "target": row.target,
                "state": row.state.as_str(),
                "said": format!(
                    "`{}` is in the review queue as `{}` onto `{}`. The queue reviews, gates and \
                     lands it; `queue` with `action: \"status\"` and this `id` reads it.",
                    row.branch, row.id, row.target
                ),
            }))
        }
        Some("status") => match arg(args, "id") {
            Some(id) => row_status(&store, &repo, id),
            None => Ok(json!({ "status": desk::status(&store, Some(&repo))? })),
        },
        Some(other) => Err(format!(
            "`{other}` is not a `queue` action: \"enqueue\" or \"status\""
        )),
        None => Err("`queue` needs `action`: \"enqueue\" or \"status\"".into()),
    }
}

/// The landing row of review `id`: the newest, since a branch that moved after approval ends
/// its landing (`moved`) and gets a new one.
fn landing_of(store: &Store, repo: &str, id: &str) -> Result<Option<MergeRow>, String> {
    Ok(store
        .merge_rows(Some(repo))
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|m| m.review_id == id || m.id == id)
        .max_by_key(|m| m.created_ms))
}

/// One entry: its review row and, once approved, its landing row.
fn row_status(store: &Store, repo: &str, id: &str) -> Result<Value, String> {
    let review = store.review_row(id).map_err(|e| e.to_string())?;
    let merge = landing_of(store, repo, id)?;
    if review.is_none() && merge.is_none() {
        return Err(format!("no row of {repo}'s queues is called `{id}`"));
    }
    Ok(json!({
        "id": id,
        "review": review.map(|r| json!({
            "state": r.state.as_str(),
            "branch": r.branch,
            "target": r.target,
            "head": r.head_sha,
            "reasons": r.reasons,
            "why": r.evidence,
        })),
        "landing": merge.map(|m| json!({
            "id": m.id,
            "state": m.state.as_str(),
            "landed_sha": m.landed_sha,
            "why": m.evidence,
        })),
    }))
}

/// The verb tool's name.
pub const VERB_TOOL: &str = "leticode_queue_verb";

const VERBS: [&str; 7] = [
    "approve", "restart", "veto", "rm", "reset", "clean", "regate",
];

/// The verb tool's schema, for `tools/list`.
pub fn verb_definition() -> Value {
    json!({
        "name": VERB_TOOL,
        "description": "The operator's `/queue` verbs on this folder's repository, recorded as \
            `bridge:<handle>`. `approve ID` over the reviewer (the merge gate still runs; not a row a \
            bridge enqueued, which the operator approves), \
            `restart ID` a parked row, `veto ID why` rejects in your words, `rm ID` withdraws, \
            `reset`/`clean` restart or withdraw every parked row (or the one `id` names), \
            `regate` gates the target's tip again.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "verb": { "type": "string", "enum": VERBS },
                "id": { "type": "string", "description": "The row: a review or landing id, or a unique prefix. Required except for reset, clean and regate." },
                "why": { "type": "string", "description": "veto: the words the branch's author gets back." },
                "target": { "type": "string", "description": "regate: the target branch. Omitted: the project's `[queue] target`." },
                "folder": { "type": "string", "description": "A folder of the repository. Omitted: this bridge's folder." }
            },
            "required": ["verb"]
        }
    })
}

/// `tools/call` for `leticode_queue_verb`.
pub fn verb_call(bridge: &Bridge, args: &Value) -> Result<Value, String> {
    let folder = arg(args, "folder")
        .map(PathBuf::from)
        .unwrap_or_else(|| bridge.folder());
    let (repo, _) = desk::resolve(&folder, None, Some("-"))?;
    let store = store(bridge)?;
    let who = format!("bridge:{}", crate::rooms::handle());
    let id = arg(args, "id");
    let quiet = |_| {};
    let need = |verb: &str| -> Result<&str, String> {
        let id = id.ok_or_else(|| format!("`{verb}` needs `id`"))?;
        verbs::in_scope(&store, id, Some(&repo))?;
        Ok(id)
    };
    // The bridge holds no line to the child that wrote the branch; the reasons stay on the row.
    let no_line = |to: &str, _: &str| -> Result<String, String> {
        Err(format!("the bridge has no line to `{to}`"))
    };
    let said = match arg(args, "verb") {
        Some(v @ "approve") => verbs::approve_by(&store, need(v)?, &who, &quiet)?,
        Some(v @ "restart") => verbs::restart_by(&store, need(v)?, &who, &quiet)?,
        Some(v @ "veto") => {
            let why = arg(args, "why").unwrap_or_default();
            verbs::veto_by(&store, need(v)?, why, &who, &no_line, &quiet)?
        }
        Some(v @ "rm") => verbs::remove_by(&store, need(v)?, &who, &quiet)?,
        Some("reset") => {
            if let Some(id) = id {
                verbs::in_scope(&store, id, Some(&repo))?;
            }
            verbs::reset_by(&store, id, Some(&repo), &who, &quiet)?
        }
        Some("clean") => {
            if let Some(id) = id {
                verbs::in_scope(&store, id, Some(&repo))?;
            }
            verbs::clean_by(&store, id, Some(&repo), &who, &quiet)?
        }
        Some("regate") => {
            let (repo, target) = desk::resolve(&folder, None, arg(args, "target"))?;
            verbs::regate(&store, &repo, &target, &who, &quiet)?
        }
        Some(other) => {
            return Err(format!(
                "`{other}` is not a queue verb: {}",
                VERBS.join(", ")
            ));
        }
        None => return Err(format!("`{VERB_TOOL}` needs `verb`: {}", VERBS.join(", "))),
    };
    Ok(json!({ "said": said }))
}

/// How a row a waiter follows has ended, as `wait-queue` exits.
pub mod exit {
    /// Landed.
    pub const LANDED: i32 = 0;
    /// Rejected, undecided, vetoed or withdrawn: the reviewers' or a person's words.
    pub const REJECTED: i32 = 10;
    /// The gate was red, timed out or conflicted, or the queue could not run it.
    pub const FAILED: i32 = 20;
    /// The bound passed with the row still in line.
    pub const TIMEOUT: i32 = 30;
    /// No store, or no such row.
    pub const UNKNOWN: i32 = 2;
}

/// Where review `id` stands: `Some(code)` once it landed or parked, `None` while it is in line.
fn ended(store: &Store, repo: &str, id: &str) -> Result<Option<i32>, String> {
    let review = store.review_row(id).map_err(|e| e.to_string())?;
    let Some(review) = review else {
        return Err(format!("no review of {repo}'s queue is called `{id}`"));
    };
    Ok(match review.state {
        ReviewState::Waiting | ReviewState::Reviewing => None,
        ReviewState::Rejected | ReviewState::CannotDecide | ReviewState::Withdrawn => {
            Some(exit::REJECTED)
        }
        ReviewState::Blocked | ReviewState::Archived => Some(exit::FAILED),
        ReviewState::Approved => match landing_of(store, repo, id)?.map(|m| m.state) {
            None | Some(MergeState::Queued | MergeState::Gating | MergeState::Moved) => None,
            Some(MergeState::Landed | MergeState::LandedLocal) => Some(exit::LANDED),
            Some(MergeState::Withdrawn) => Some(exit::REJECTED),
            Some(
                MergeState::Failed
                | MergeState::TimedOut
                | MergeState::Conflict
                | MergeState::Blocked,
            ) => Some(exit::FAILED),
        },
    })
}

/// `letibot-mcp wait-queue`: poll the store until the row lands or parks, or `deadline` passes.
/// The answer is the exit code and the row's state as JSON. A landing id is followed through its
/// review, so a branch that moved after approval is waited on through its next review.
pub fn wait(
    bridge: &Bridge,
    folder: &Path,
    id: &str,
    deadline: Instant,
    every: Duration,
) -> (i32, String) {
    let opened = (|| {
        let (repo, _) = desk::resolve(folder, None, Some("-"))?;
        let store = store(bridge)?;
        let review = match store.review_row(id).map_err(|e| e.to_string())? {
            Some(r) => r.id,
            None => landing_of(&store, &repo, id)?
                .map(|m| m.review_id)
                .ok_or_else(|| format!("no row of {repo}'s queues is called `{id}`"))?,
        };
        Ok::<_, String>((repo, store, review))
    })();
    let (repo, store, review) = match opened {
        Ok(it) => it,
        Err(e) => return (exit::UNKNOWN, e),
    };
    loop {
        let code = match ended(&store, &repo, &review) {
            Ok(c) => c,
            Err(e) => return (exit::UNKNOWN, e),
        };
        let now = Instant::now();
        if code.is_some() || now >= deadline {
            let state = row_status(&store, &repo, &review)
                .map(|v| v.to_string())
                .unwrap_or_else(|e| e);
            return (code.unwrap_or(exit::TIMEOUT), state);
        }
        std::thread::sleep(every.min(deadline - now));
    }
}
