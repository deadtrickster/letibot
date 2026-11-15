//! `queue`: a Claude Code agent puts its pushed branch up for the merge queue and reads its row.
//! The queue owns the review, the gate and the landing, so an agent never runs the workspace gate.

use std::path::Path;

use letibot_harnessd::mergequeue::{desk, enqueue};
use letibot_tokencore::store::Store;
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
        .map(std::path::PathBuf::from)
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

/// One entry: its review row and, once approved, its landing row.
fn row_status(store: &Store, repo: &str, id: &str) -> Result<Value, String> {
    let review = store.review_row(id).map_err(|e| e.to_string())?;
    let merge = store
        .merge_rows(Some(repo))
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|m| m.review_id == id || m.id == id);
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
