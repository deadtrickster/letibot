//! `read_spill` — the tool that makes clause 5's notice actionable.
//!
//! §8.3: *"the locator is a content hash into the `tool_spill` table, and there is
//! a `read_spill(hash, range)` tool. So the 'hint' is actionable rather than
//! advisory, which is clause 1 applied to spilling itself."*
//!
//! Its own miss follows the same rule as every other: an unknown hash comes back
//! with the hashes this session actually has, and their sizes.

use serde_json::Value;

use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

use super::text_of;

pub struct ReadSpill;

impl Tool for ReadSpill {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "read_spill",
            "Fetch a tool output that was too large to return inline. Give the `hash` \
             printed in the omission notice; optionally `offset` and `length` in bytes \
             to take part of it. A hash that is not held comes back with the ones that \
             are.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "hash": {"type": "string", "description": "The locator from the omission notice."},
                    "offset": {"type": "integer", "description": "First byte to return."},
                    "length": {"type": "integer", "description": "How many bytes to return."}
                },
                "required": ["hash"]
            }),
            Access::Read,
        )
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let held = ctx.spiller.store.list();
        let Some(hash) = args.get("hash").and_then(|v| v.as_str()) else {
            return Invocation::failed("read_spill needs a hash", available(&held));
        };
        let offset = args
            .get("offset")
            .and_then(|v| v.as_i64())
            .filter(|n| *n >= 0)
            .unwrap_or(0) as usize;
        let length = args
            .get("length")
            .and_then(|v| v.as_i64())
            .filter(|n| *n > 0)
            .map(|n| n as usize);
        let range = length.map(|l| offset..offset + l).or(if offset > 0 {
            Some(offset..usize::MAX)
        } else {
            None
        });

        match ctx.spiller.store.get(hash, range) {
            Ok(bytes) => {
                let (text, lossy) = text_of(&bytes);
                let mut inv = Invocation::ok(text);
                let full = held.iter().find(|e| e.hash == hash).map(|e| e.bytes);
                if let Some(full) = full
                    && offset + bytes.len() < full
                {
                    inv = inv.with_note(format!(
                        "bytes {offset}–{} of {full}; call read_spill again with \
                         offset={} for the next part",
                        offset + bytes.len(),
                        offset + bytes.len()
                    ));
                }
                if lossy {
                    inv = inv.with_note("the stored output is not valid UTF-8".to_string());
                }
                inv
            }
            Err(_) => Invocation::failed(
                format!("no spilled output is held under `{hash}`"),
                available(&held),
            )
            .with_note(
                "a spill locator is only valid within the session that produced it, and \
                 the hash is the one printed in the omission notice."
                    .to_string(),
            ),
        }
    }
}

fn available(held: &[crate::spill::SpillEntry]) -> String {
    if held.is_empty() {
        return "this session has spilled nothing yet, so there is nothing to fetch.".to_string();
    }
    let mut out = format!("this session holds {} spilled output(s):\n", held.len());
    for e in held {
        out.push_str(&format!(
            "  {} — {}{}\n",
            e.hash,
            super::human(e.bytes as u64),
            if e.tool.is_empty() {
                String::new()
            } else {
                format!(" from `{}`", e.tool)
            }
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use crate::spill::{FixedBudget, MemoryStore, Spiller};
    use crate::testing::harness_with_spiller;

    #[test]
    fn the_notice_hash_fetches_the_rest() {
        let mut h = harness_with_spiller(Spiller::new(
            Box::new(FixedBudget(600)),
            Box::new(MemoryStore::new()),
        ));
        let r = h.call("read", r#"{"path":"big.txt"}"#);
        let spill = r.spill.clone().expect("a large file must spill");
        assert!(r.payload.contains(&spill.hash), "{}", r.payload);

        // A range comes back whole, which is what makes the notice actionable.
        let part = h.call(
            "read_spill",
            &format!(r#"{{"hash":"{}","offset":0,"length":200}}"#, spill.hash),
        );
        assert!(part.is_grounded());
        assert_eq!(part.payload.len(), 200);
        assert!(part.payload.contains("filler line 0"), "{}", part.payload);

        // Asking for all of it under the same budget spills again — and to the
        // *same* locator, because the locator is a content hash. That is not a
        // loop: the notice tells the model to take a range, and the hash it
        // already has stays valid.
        let whole = h.call("read_spill", &format!(r#"{{"hash":"{}"}}"#, spill.hash));
        let again = whole.spill.expect("the full output exceeds the cap again");
        assert_eq!(again.hash, spill.hash);
        assert_eq!(again.full_bytes, spill.full_bytes);
    }

    #[test]
    fn an_unknown_hash_lists_what_is_held() {
        let mut h = harness_with_spiller(Spiller::new(
            Box::new(FixedBudget(600)),
            Box::new(MemoryStore::new()),
        ));
        h.call("read", r#"{"path":"big.txt"}"#);
        let r = h.call("read_spill", r#"{"hash":"deadbeef"}"#);
        assert!(!r.is_grounded());
        assert!(r.payload.contains("holds 1 spilled"), "{}", r.payload);
    }
}
