//! **Overriding the session list, the way the plan says it should be done** — one small layer that
//! asks for the layer below and edits what it finds, instead of re-implementing the list.
//!
//! Copy this directory into `<project>/.letibot/plugins/sessions/` (the `plugin.wasm` beside this
//! file is what the host loads; `cargo build --offline --release --target wasm32-unknown-unknown`
//! rebuilds it) and open the session list: the `system` branch is renamed to `the harness's own`,
//! says what this project thinks of it, and stops listing it. Everything else — the order, the
//! projects, the rows, the numbers, the keys — is exactly what the built-in answered.
//!
//! # Why `below` is the whole point
//!
//! `docs/plugins-plan.md` §2.1: a layer that asks below is called again with the lower layer's
//! answer under `below`, and is the dotenv `${VAR}` of this design. The built-in `sessions` layer
//! (in the binary) decides the list; this one changes two lines of it. A policy change is then a
//! diff of what changed, not a second copy of a list that will drift from the first.
//!
//! # What it does NOT do
//!
//! It reads the feed the host hands it only to pass it down: the request is echoed back as
//! `{"below": <the same input>}`, because a layer that rebuilt the request would be a layer that
//! had to know the request's shape. And it names no verb it did not declare: the rows it keeps
//! carry the acts they arrived with (`switch`, `fold`), which `plugin.toml` declares.

use letibot_guest_abi as abi;
use serde_json::{Value, json};

/// **The branch this project hides** — `system (N)`, the harness's own agents under a project.
///
/// Matched by the WORDS of its line rather than by its id, because the id is a path
/// (`<project>/system`) and this layer is one project's: a constant id would be a constant path.
const SYSTEM: &str = "system (";

/// The name it is drawn under here, and the line that says why.
const RENAMED: &str = "the harness's own (0)";
const LEGEND: &str = "this project keeps no system agents on its list";

/// **The id the renamed line takes.** The host keys its fold state by id, so a rename that kept
/// the old one would fold under a name nobody can see.
const RENAMED_ID: &str = "no-system-agents";

/// **One call**: the first time, ask below with the same request; the second time, edit.
fn answer(bytes: &[u8]) -> Option<String> {
    let input: Value = serde_json::from_slice(bytes).ok()?;
    match input.get("below") {
        // The first call. `{"below": <input>}` is the ask — the whole request, so the layer below
        // is asked exactly the question this one was.
        None => Some(json!({"below": input}).to_string()),
        Some(below) => edit(below.clone()).map(|view| view.to_string()),
    }
}

/// **The edit**: the group's name and legend, and its rows taken out of the list.
///
/// A row this layer does not understand is left exactly as it was — a same-shaped row from a
/// different build of the built-in still moves through here unchanged, which is what keeps this
/// example from being a fork.
fn edit(mut view: Value) -> Option<Value> {
    // The branch's position, read before anything is written: a list this layer has no business
    // with is answered back exactly as it arrived.
    let Some(at) = rows(&view)?.iter().position(|r| {
        r.get("cells")
            .and_then(Value::as_array)
            .and_then(|c| c.first())
            .and_then(|c| c.get("text"))
            .and_then(Value::as_str)
            .is_some_and(|t| t.starts_with(SYSTEM))
    }) else {
        return Some(view);
    };
    let rows = rows_mut(&mut view)?;
    // **The branch's line**: a new name, a new id (the host keys its fold state by id, so a
    // rename that kept the old one would fold under a name nobody can see), and a legend that
    // says what this project decided.
    rows[at]["id"] = json!(RENAMED_ID);
    rows[at]["cells"] = json!([{"text": RENAMED, "role": "strong"}]);
    rows[at]["under"] = json!([LEGEND]);
    // **And its rows go** — every row after it that is deeper than the group's own line, up to the
    // next line at its depth. The list is flat, and `depth` is what says whose child a row is.
    let depth = rows[at]
        .get("depth")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let mut end = at + 1;
    while end < rows.len()
        && rows[end]
            .get("depth")
            .and_then(Value::as_u64)
            .unwrap_or_default()
            > depth
    {
        end += 1;
    }
    rows.drain(at + 1..end);
    Some(view)
}

/// The list's rows, wherever the host's own answer put them — the path is the document's, not this
/// example's.
fn rows(view: &Value) -> Option<&Vec<Value>> {
    view.get("panels")?
        .get(0)?
        .get("summary")?
        .get(0)?
        .get("tree")?
        .get("rows")?
        .as_array()
}

fn rows_mut(view: &mut Value) -> Option<&mut Vec<Value>> {
    view.get_mut("panels")?
        .get_mut(0)?
        .get_mut("summary")?
        .get_mut(0)?
        .get_mut("tree")?
        .get_mut("rows")?
        .as_array_mut()
}

/// The pane hook — `pane_<name>`, the name a head opens by.
#[unsafe(no_mangle)]
pub extern "C" fn pane_sessions(ptr: i32, len: i32) -> i32 {
    abi::hook(ptr, len, answer)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A view shaped like the built-in's: a project's line, a session under it, and the `system`
    /// branch with one group and one row.
    fn builtin_view() -> Value {
        json!({
            "title": "sessions in this daemon",
            "panels": [{"id": "sessions", "summary": [{"tree": {"rows": [
                {"id": "/p/letibot", "cells": [{"text": "letibot (3)", "role": "strong"}],
                 "open": "open", "enter": {"fold": true}},
                {"id": "s-a", "cells": ["the habr article"], "number": 1, "depth": 1,
                 "enter": {"switch": "s-a"}},
                {"id": "/p/letibot/system", "cells": [{"text": "system (2)", "role": "strong"}],
                 "depth": 1, "open": "open", "enter": {"fold": true}},
                {"id": "/p/letibot/system/reviewers",
                 "cells": [{"text": "reviewers (2)", "role": "strong"}],
                 "depth": 2, "open": false, "enter": {"fold": true}},
                {"id": "s-review", "cells": ["You are the gatekeeper. review it"],
                 "number": 2, "depth": 3, "enter": {"switch": "s-review"}},
            ]}}]}],
        })
    }

    /// **The first call asks below with the request it was given**, not with a rebuilt one.
    #[test]
    fn the_first_call_asks_below_with_the_same_request() {
        let request = json!({"v": 2, "feeds": {"sessions": {"sessions": []}}});
        let said = answer(request.to_string().as_bytes()).expect("an answer");
        let asked: Value = serde_json::from_str(&said).expect("json");
        assert_eq!(asked, json!({"below": request}));
    }

    /// **The second call keeps everything and changes the one branch**: the name, the legend, and
    /// the rows not drawn — and not one other row is touched.
    #[test]
    fn it_renames_the_branch_and_hides_its_rows() {
        let edited = edit(builtin_view()).expect("an edit");
        let rows = edited["panels"][0]["summary"][0]["tree"]["rows"]
            .as_array()
            .expect("rows")
            .clone();
        let ids: Vec<&str> = rows.iter().filter_map(|r| r["id"].as_str()).collect();
        assert_eq!(
            ids,
            vec!["/p/letibot", "s-a", "no-system-agents"],
            "{rows:#?}"
        );
        let branch = &rows[2];
        assert_eq!(branch["cells"][0]["text"], "the harness's own (0)");
        assert_eq!(branch["under"][0], LEGEND);
        // The other rows are the built-in's own, byte for byte.
        assert_eq!(
            rows[1],
            builtin_view()["panels"][0]["summary"][0]["tree"]["rows"][1]
        );
    }

    /// **A list with no such branch is left alone** — the edit is an edit, not a requirement.
    #[test]
    fn a_list_without_the_branch_is_untouched() {
        let view = json!({"panels": [{"summary": [{"tree": {"rows": [
            {"id": "/p/letibot", "cells": [{"text": "letibot (1)", "role": "strong"}]},
        ]}}]}]});
        assert_eq!(edit(view.clone()), Some(view));
    }
}
