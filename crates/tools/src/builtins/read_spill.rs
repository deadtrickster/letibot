//! `read_spill`: the tool that makes a spilled result's notice actionable.
//! The locator is a content hash into the spill table, so a model told its output spilled can read
//! any range of it back, and an unknown hash comes back with the hashes this session has.

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
        let Some(hash) = args.get("hash").and_then(|v| v.as_str()) else {
            return Invocation::failed(
                "read_spill needs a hash",
                available(&ctx.spiller.store.list()),
            );
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
            Ok(mut bytes) => {
                bytes.truncate(window(ctx, args, &bytes));
                let (text, lossy) = text_of(&bytes);
                let mut inv = Invocation::ok(text);
                let full = ctx.spiller.store.size(hash);
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
            // Never a silent miss: a hash the store once held says when it went and how big it was.
            Err(crate::spill::SpillError::Evicted(t)) => {
                Invocation::failed(format!("`{hash}` was evicted"), t.said())
            }
            Err(_) => Invocation::failed(
                format!("no spilled output is held under `{hash}`"),
                available(&ctx.spiller.store.list()),
            )
            .with_note(
                "the hash is the one printed in the omission notice; a store kept in memory \
                 loses every spill when its daemon restarts."
                    .to_string(),
            ),
        }
    }
}

/// How much of `bytes` fits under the session's own inline cap. A window that fit would otherwise
/// spill again under the same hash and hand back the same head and tail.
fn window(ctx: &InvokeCtx<'_>, args: &Value, bytes: &[u8]) -> usize {
    let at = crate::spill::SpillContext {
        tool: "read_spill",
        args,
        bytes: bytes.len(),
    };
    let mut end = bytes.len();
    if let Some(cap) = ctx.spiller.budget.max_inline_bytes(&at) {
        end = end.min(cap);
    }
    if let Some(lines) = ctx.spiller.budget.max_inline_lines(&at)
        && let Some((i, _)) = bytes[..end]
            .iter()
            .enumerate()
            .filter(|(_, b)| **b == b'\n')
            .nth(lines.saturating_sub(1))
    {
        end = i + 1;
    }
    if end == bytes.len() {
        return end;
    }
    // A cut on a line end where one is in the second half, else on a char boundary.
    match bytes[..end].iter().rposition(|b| *b == b'\n') {
        Some(i) if (i + 1) * 2 >= end => i + 1,
        _ => match std::str::from_utf8(&bytes[..end]) {
            Err(e) if e.error_len().is_none() => e.valid_up_to(),
            _ => end,
        },
    }
}

fn available(held: &[crate::spill::SpillEntry]) -> String {
    if held.is_empty() {
        return "this session has spilled nothing yet, so there is nothing to fetch.".to_string();
    }
    let mut out = format!("this store holds {} spilled output(s):\n", held.len());
    // A project store can hold thousands; the most recently read come first.
    for e in held.iter().take(LISTED) {
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
    if held.len() > LISTED {
        out.push_str(&format!("  and {} more\n", held.len() - LISTED));
    }
    out
}

const LISTED: usize = 20;

#[cfg(test)]
mod tests {
    use crate::spill::{DirStore, FixedBudget, MemoryStore, Spiller};
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

        // Asking for all of it comes back a window at a time under the same cap rather than
        // spilling again behind the same locator.
        let whole = h.call("read_spill", &format!(r#"{{"hash":"{}"}}"#, spill.hash));
        assert!(whole.spill.is_none(), "{}", whole.payload);
        assert!(whole.payload.len() <= 600, "{}", whole.payload.len());
        assert!(whole.payload.ends_with('\n'), "cut on a line end");
        assert!(
            whole.notes.iter().any(|n| n.contains("offset=")),
            "{:?}",
            whole.notes
        );
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

    #[test]
    fn an_evicted_hash_says_when_and_how_big() {
        let root = crate::backend::tempdir::TempDir::new();
        let mut h = harness_with_spiller(Spiller::new(
            Box::new(FixedBudget(600)),
            Box::new(DirStore::for_project(root.path(), "p", Some(1))),
        ));
        let first = h
            .call("read", r#"{"path":"big.txt"}"#)
            .spill
            .expect("spills");
        let second = h
            .call("read", r#"{"path":"big.txt","offset":2}"#)
            .spill
            .expect("spills");
        assert_ne!(first.hash, second.hash);
        let r = h.call("read_spill", &format!(r#"{{"hash":"{}"}}"#, first.hash));
        assert!(!r.is_grounded());
        assert!(r.payload.contains("was evicted"), "{}", r.payload);
        assert!(r.payload.contains("evicted on "), "{}", r.payload);
        assert!(
            r.payload.contains(&format!("{} bytes", first.full_bytes)),
            "{}",
            r.payload
        );
    }
}
