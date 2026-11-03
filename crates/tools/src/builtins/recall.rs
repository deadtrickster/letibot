//! `recall`: the body of one unit a compaction evicted, by the id its map printed.
//!
//! The map is what a reader can see; a body can be megabytes, so it is read back one window at a
//! time. A read is bounded by the window the session has, because what comes back is part of the
//! conversation from then on, and one line of a map must not fill the room the next turn needs.

use serde_json::Value;

use crate::recall::Recall;
use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

use super::text_of;

pub struct RecallTool;

impl Tool for RecallTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "recall",
            "Return the body of a unit a compaction evicted, by the id the map printed. \
             The body is returned exactly as it was, bounded: a body larger than one read \
             comes back in parts, and the note says which offset continues it.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "The id on one line of the map."},
                    "offset": {"type": "integer", "description": "First byte to return."}
                },
                "required": ["id"]
            }),
            Access::Read,
        )
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let Some(held) = ctx.recall else {
            return Invocation::failed(
                "no compaction's map is reachable from here",
                "This runtime holds no evicted units, so there is no id to recall: nothing \
                 was compacted in it."
                    .to_string(),
            );
        };
        let Some(id) = args.get("id").and_then(|v| v.as_str()) else {
            return Invocation::failed("recall needs an `id`", available(held));
        };
        let offset = args
            .get("offset")
            .and_then(|v| v.as_i64())
            .filter(|n| *n >= 0)
            .unwrap_or(0) as usize;
        let range = offset..offset.saturating_add(held.bound());
        match ctx.spiller.store.get(id, Some(range.clone())) {
            Ok(bytes) => {
                let (text, lossy) = text_of(&bytes);
                let mut inv = Invocation::ok(text);
                if lossy {
                    inv = inv.with_note("the stored body is not valid UTF-8".to_string());
                }
                let full = held.named(id).map(|e| e.bytes).unwrap_or(bytes.len());
                let done = (offset + bytes.len()).min(full);
                if done < full {
                    inv = inv.with_note(format!(
                        "bytes {offset}–{done} of {full}; call recall again with offset={done} \
                         for the next part"
                    ));
                }
                inv
            }
            // Two absences, two sentences: an id the map never named is a typo, and an id it
            // named whose body is gone is a store that did not outlive the daemon.
            Err(_) if held.named(id).is_some() => Invocation::failed(
                format!("`{id}` is on this session's map and the store no longer holds its body"),
                format!(
                    "`{id}` was evicted, so the map names it, and the store it went to does \
                     not have it any more. A spill store with no directory keeps bodies only for \
                     the life of the daemon that evicted them, so a restart leaves a map of ids \
                     nothing can open."
                ),
            ),
            Err(_) => Invocation::failed(
                format!("no unit is held under `{id}`, which this session's map does not name"),
                format!(
                    "`{id}` is not an id this session's map names.\n\n{}",
                    available(held)
                ),
            ),
        }
    }
}

fn available(held: &Recall) -> String {
    let units = held.held();
    if units.is_empty() {
        return "this session has evicted nothing yet, so there is no id to recall.".to_string();
    }
    let mut out = format!("this session's map names {} unit(s):\n", units.len());
    for (id, unit) in units.iter().take(20) {
        out.push_str(&format!("  {id} `{}` {}\n", unit.kind, unit.named));
    }
    if units.len() > 20 {
        out.push_str(&format!("  and {} more\n", units.len() - 20));
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::recall::{Evicted, Recall};
    use crate::spill::{MemoryStore, NoBudget, SpillStore, Spiller};

    use crate::testing::harness_with_spiller;

    const BODY: usize = 40 * 1024;

    /// The body a test hands the store: long enough that one read cannot return it whole.
    fn body() -> String {
        let mut s = String::from("THE-EVICTED-BODY\n");
        while s.len() < BODY {
            s.push_str("the quick brown fox jumps over the lazy dog, and again, and again\n");
        }
        s
    }

    /// A harness whose spill store holds one body, and whose map names it. The id is the store's
    /// own, which is what a compaction hands the map.
    fn harnessed(window: Option<u64>) -> (crate::testing::Harness, String, String) {
        let store = MemoryStore::new();
        let text = body();
        let id = store
            .put("read", text.as_bytes())
            .expect("the fixture store takes it");
        let recall = Recall::new(window);
        recall.remember([(
            id.clone(),
            Evicted {
                kind: "read".into(),
                named: "src/a.rs".into(),
                bytes: text.len(),
            },
        )]);
        let mut h = harness_with_spiller(Spiller::new(Box::new(NoBudget), Box::new(store)));
        h.rt.recall = Some(Arc::new(recall));
        (h, id, text)
    }

    /// The whole point: the body comes back as it was, not as a description of it.
    #[test]
    fn a_recall_returns_the_exact_evicted_body() {
        let (mut h, id, text) = harnessed(Some(262_144));
        let r = h.call("recall", &format!(r#"{{"id":"{id}"}}"#));
        assert!(r.is_grounded(), "{}", r.payload);
        assert_eq!(r.payload, text, "byte for byte, and not a summary of it");
    }

    /// An id the map never named is refused by name, and the refusal says what there is: a model
    /// that guessed gets the list it can guess from.
    #[test]
    fn an_unknown_id_is_refused_by_name() {
        let (mut h, id, _) = harnessed(Some(262_144));
        let r = h.call("recall", r#"{"id":"deadbeefdeadbeef"}"#);
        assert!(!r.is_grounded());
        assert!(
            r.payload
                .contains("`deadbeefdeadbeef` is not an id this session's map names"),
            "the refusal names the id it was given: {}",
            r.payload
        );
        assert!(
            r.payload.contains(&id),
            "and lists the one it does hold: {}",
            r.payload
        );
    }

    /// A body outlives the memory the map lives in: with `--spill-dir` the store keeps it across a
    /// restart, and a new process has no map to consult. The store is the authority on an id, so
    /// the body opens rather than being refused as an id this session's map never named.
    #[test]
    fn a_body_the_store_kept_is_opened_without_a_map() {
        let dir = crate::backend::tempdir::TempDir::new();
        let session = "s-recall-restart";
        let store = crate::spill::FileStore::new(dir.path(), session).expect("the directory store");
        let text = body();
        let id = store
            .put("read", text.as_bytes())
            .expect("the fixture store takes it");

        // The daemon that evicted it: a map naming the id.
        let recall = Recall::new(Some(262_144));
        recall.remember([(
            id.clone(),
            Evicted {
                kind: "read".into(),
                named: "src/a.rs".into(),
                bytes: text.len(),
            },
        )]);
        let mut first = harness_with_spiller(Spiller::new(
            Box::new(NoBudget),
            Box::new(crate::spill::FileStore::new(dir.path(), session).unwrap()),
        ));
        first.rt.recall = Some(Arc::new(recall));
        assert_eq!(
            first.call("recall", &format!(r#"{{"id":"{id}"}}"#)).payload,
            text
        );

        // The daemon that restarted: the same directory, and a map no process refilled.
        let mut after = harness_with_spiller(Spiller::new(
            Box::new(NoBudget),
            Box::new(crate::spill::FileStore::new(dir.path(), session).unwrap()),
        ));
        after.rt.recall = Some(Arc::new(Recall::new(Some(262_144))));
        let r = after.call("recall", &format!(r#"{{"id":"{id}"}}"#));
        assert!(r.is_grounded(), "{}", r.payload);
        assert_eq!(
            r.payload, text,
            "the body outlived the map, so it still opens"
        );
    }

    /// A unit on the map whose body the store has lost says exactly that, rather than reading as a
    /// typo: the spill store keeps nothing across a restart unless it was given a directory.
    #[test]
    fn a_body_the_store_has_lost_says_so() {
        let (h, id, _) = harnessed(Some(262_144));
        // A second runtime over the same map and an empty store: what a restart leaves.
        let mut after = harness_with_spiller(Spiller::new(
            Box::new(NoBudget),
            Box::new(MemoryStore::new()),
        ));
        after.rt.recall = h.rt.recall.clone();
        let r = after.call("recall", &format!(r#"{{"id":"{id}"}}"#));
        assert!(!r.is_grounded());
        assert!(
            r.payload.contains(&format!("`{id}` was evicted")),
            "{}",
            r.payload
        );
    }

    /// A read is bounded by the window the session has: an eighth of it, so the body a map line
    /// points at can never arrive in one piece and eat the turn after it.
    #[test]
    fn a_recall_is_bounded_by_the_window_it_has() {
        let (mut small, id, text) = harnessed(Some(8_192));
        let r = small.call("recall", &format!(r#"{{"id":"{id}"}}"#));
        assert!(r.is_grounded(), "{}", r.payload);
        assert!(
            r.payload.len() <= 8_192 / 8 * 4,
            "a 8192-token window bounds one read to {} byte(s), and this is {}",
            8_192 / 8 * 4,
            r.payload.len()
        );
        assert!(
            text.starts_with(&r.payload),
            "what does come back is the body's own first bytes"
        );
        assert!(
            r.notes.iter().any(|n| n.contains("call recall again")),
            "and the note says where the rest is: {:?}",
            r.notes
        );

        // A wider window reads more of the same body in one call, which is the bound following
        // the window rather than a constant that happens to fit.
        let (mut wide, id, _) = harnessed(Some(262_144));
        let wide_read = wide.call("recall", &format!(r#"{{"id":"{id}"}}"#));
        assert!(
            wide_read.payload.len() > r.payload.len(),
            "{} vs {}",
            wide_read.payload.len(),
            r.payload.len()
        );
    }
}
