//! The harness's own compaction record, as the built-in `compact` layer (X15).
//!
//! ```jsonc
//! // in
//! {"v":1, "sections":["## Objective", …],
//!  "candidates":[{"id":"agents_md","kind":"agents_md","tokens":900,"score":1},
//!                {"id":"row:1","kind":"row","tokens":12,"score":0.9}],
//!  "budget":{"summary_tokens":8000,"pinned_tokens":2000}}
//! // out
//! {"sections":[…], "pin":["agents_md","row:1"], "after":"continue"}
//! ```
//!
//! It asks for the record the harness has always asked for - the same eight sections, in the same
//! order, and no sentence of its own - and adds the two things G2 wanted carried verbatim: the
//! project's `AGENTS.md`, then the rows still open. Both are candidates the host scored; a pin past
//! the pin budget is cut and said by the host, not dropped here.

use letibot_guest_abi as abi;

/// The record's sections, as the harness's own instruction asks for them: `SUMMARY_SECTIONS` in
/// `crates/turn/src/compaction.rs`, and a test in the host holds the two lists to each other.
pub const SECTIONS: [&str; 8] = [
    "## Objective",
    "## Important Details",
    "## Work State",
    "### Completed",
    "### Active",
    "### Blocked",
    "## Next Move",
    "## Relevant Files",
];

#[unsafe(no_mangle)]
pub extern "C" fn compact_summary(ptr: i32, len: i32) -> i32 {
    abi::hook(ptr, len, answer)
}

fn answer(bytes: &[u8]) -> Option<String> {
    let input: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    Some(decide(&input).to_string())
}

/// The answer for one compaction: the harness's sections, the pins this conversation offers, and the
/// session going on after it - which is what the wall has always done.
pub fn decide(input: &serde_json::Value) -> serde_json::Value {
    let mut pin: Vec<&str> = Vec::new();
    let candidates = input
        .get("candidates")
        .and_then(|c| c.as_array())
        .map(Vec::as_slice)
        .unwrap_or_default();
    // `AGENTS.md` first and whatever rows follow: the order is the pin budget's order, and the
    // operator's own file is the last thing a record may lose.
    for kind in ["agents_md", "row"] {
        for candidate in candidates {
            let is = candidate.get("kind").and_then(|k| k.as_str()) == Some(kind);
            if let (true, Some(id)) = (is, candidate.get("id").and_then(|i| i.as_str())) {
                pin.push(id);
            }
        }
    }
    serde_json::json!({
        "sections": SECTIONS,
        "pin": pin,
        "after": "continue",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(candidates: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "v": 1,
            "sections": SECTIONS,
            "candidates": candidates,
            "budget": {"summary_tokens": 8000, "pinned_tokens": 2000},
        })
    }

    /// K4's rule for this layer: `AGENTS.md` is pinned, and the rows after it, in that order.
    #[test]
    fn agents_md_is_pinned_before_the_rows() {
        let said = decide(&input(serde_json::json!([
            {"id": "row:1", "kind": "row", "tokens": 12, "score": 0.9},
            {"id": "agents_md", "kind": "agents_md", "tokens": 900, "score": 1.0},
            {"id": "row:2", "kind": "row", "tokens": 30, "score": 0.9},
        ])));
        assert_eq!(
            said["pin"],
            serde_json::json!(["agents_md", "row:1", "row:2"]),
            "{said}"
        );
        // A conversation with no `AGENTS.md` pins nothing rather than naming a candidate it was
        // not offered: the host refuses a pin it did not score.
        let bare = decide(&input(serde_json::json!([])));
        assert_eq!(bare["pin"], serde_json::json!([]), "{bare}");
    }

    /// It asks for the record the harness asks for: every section, in the harness's order, and no
    /// instruction of its own - a layer that added one would be a second template.
    #[test]
    fn it_asks_for_the_records_own_sections() {
        let said = decide(&input(serde_json::json!([])));
        assert_eq!(said["sections"], serde_json::json!(SECTIONS));
        assert!(said.get("extra_instruction").is_none(), "{said}");
        assert_eq!(said["after"], serde_json::json!("continue"));
        assert_eq!(SECTIONS.len(), 8);
    }
}
