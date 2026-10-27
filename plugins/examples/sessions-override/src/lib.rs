//! **Overriding the session list, the way the plan says it should be done** — one small layer that
//! asks for the layer below and edits what it finds, instead of re-implementing the list.
//!
//! Copy this directory into `<project>/.letibot/plugins/sessions/` (the `plugin.wasm` beside this
//! file is what the host loads; `cargo build --offline --release --target wasm32-unknown-unknown`
//! rebuilds it) and open the session list: the `one-shots` group is renamed to `leftovers`, says
//! what this project thinks of them, and stops listing them. Everything else — the order, the
//! other groups, the rows, the numbers, the keys — is exactly what the built-in answered.
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

/// The group this project renames and empties, by the id the built-in gives it.
const GROUP: &str = "one-shots";

/// The name it is drawn under here, and the line that says why.
const RENAMED: &str = "leftovers (0)";
const LEGEND: &str = "what a `--prompt` run left behind — this project keeps no such rows on its list";

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
    // The group's position, read before anything is written: a list this layer has no business
    // with is answered back exactly as it arrived.
    let Some(at) = rows(&view)?
        .iter()
        .position(|r| r.get("id").and_then(Value::as_str) == Some(GROUP))
    else {
        return Some(view);
    };
    let rows = rows_mut(&mut view)?;
    // **The group's line**: a new name, a new id (the host keys its fold state by id, so a
    // rename that kept the old one would fold under a name nobody can see), and a legend that
    // says what this project decided.
    rows[at]["id"] = json!("leftovers");
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

    /// A view shaped like the built-in's: two groups, one row under the second.
    fn builtin_view() -> Value {
        json!({
            "title": "sessions in this daemon",
            "panels": [{"id": "sessions", "summary": [{"tree": {"rows": [
                {"id": "mine", "cells": ["mine (1)"], "open": "fixed", "enter": {"fold": true}},
                {"id": "s-a", "cells": ["the habr article"], "number": 1, "depth": 1,
                 "enter": {"switch": "s-a"}},
                {"id": "one-shots", "cells": ["one-shots (1)"], "open": false,
                 "enter": {"fold": true}},
                {"id": "s-shot", "cells": ["one prompt, one answer"], "number": 2, "depth": 1,
                 "enter": {"switch": "s-shot"}},
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

    /// **The second call keeps everything and changes the one group**: the name, the legend, and
    /// the rows not drawn — and not one other row is touched.
    #[test]
    fn it_renames_the_group_and_hides_its_rows() {
        let edited = edit(builtin_view()).expect("an edit");
        let rows = edited["panels"][0]["summary"][0]["tree"]["rows"]
            .as_array()
            .expect("rows")
            .clone();
        let ids: Vec<&str> = rows.iter().filter_map(|r| r["id"].as_str()).collect();
        assert_eq!(ids, vec!["mine", "s-a", "leftovers"], "{rows:#?}");
        let group = &rows[2];
        assert_eq!(group["cells"][0]["text"], "leftovers (0)");
        assert_eq!(group["under"][0], LEGEND);
        // The other rows are the built-in's own, byte for byte.
        assert_eq!(rows[1], builtin_view()["panels"][0]["summary"][0]["tree"]["rows"][1]);
    }

    /// **A list with no such group is left alone** — the edit is an edit, not a requirement.
    #[test]
    fn a_list_without_the_group_is_untouched() {
        let view = json!({"panels": [{"summary": [{"tree": {"rows": [{"id": "mine"}]}}]}]});
        assert_eq!(edit(view.clone()), Some(view));
    }
}
