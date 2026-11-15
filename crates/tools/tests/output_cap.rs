//! The generic output cap: a tool with no cap of its own is bounded by the default, its middle is
//! behind one seam line, and a tool's own tighter cap still wins.

use letibot_tools::runtime::{Invocation, InvokeCtx, Tool};
use letibot_tools::schema::{Access, ToolSchema};
use letibot_tools::spill::{DEFAULT_INLINE_BYTES, MemoryStore, OutputCap, Spiller};
use letibot_tools::testing::{harness_with_spiller, runner_harness};
use letibot_transcript::seam::Seam;
use serde_json::Value;

/// A tool with no cap of its own, as a new or plugin tool is.
struct Flood(String);

impl Tool for Flood {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "flood",
            "Return a large fixed text.",
            serde_json::json!({"type": "object", "properties": {}}),
            Access::Read,
        )
    }
    fn invoke(&self, _ctx: &mut InvokeCtx<'_>, _args: &Value) -> Invocation {
        Invocation::ok(self.0.clone())
    }
}

fn default_spiller() -> Spiller {
    Spiller::new(Box::new(OutputCap::default()), Box::new(MemoryStore::new()))
}

#[test]
fn an_uncapped_tool_returning_200_kib_keeps_50_kib_and_one_seam_and_read_spill_returns_the_middle()
{
    let full: String = (0..2_600)
        .map(|i| format!("{i:06} {}\n", "x".repeat(72)))
        .collect();
    assert!(full.len() >= 200 * 1024, "{}", full.len());
    let mut h = harness_with_spiller(default_spiller());
    h.rt.registry
        .register(Box::new(Flood(full.clone())))
        .unwrap();

    let r = h.call("flood", "{}");
    let spill = r.spill.clone().expect("200 KiB over a 50 KiB cap spills");
    assert!(
        r.payload.len() <= DEFAULT_INLINE_BYTES,
        "{}",
        r.payload.len()
    );
    let seams: Vec<Seam> = r.payload.lines().filter_map(Seam::parse).collect();
    assert_eq!(seams.len(), 1, "{}", r.payload);
    let seam = &seams[0];
    assert_eq!(seam.hash, spill.hash);
    assert!(r.payload.starts_with("000000 "), "the head stays");
    assert!(
        r.payload.ends_with(&full[full.len() - 200..]),
        "the tail stays"
    );

    // The middle, a window at a time: each read is bounded by the same cap and none spills again.
    let mut middle = String::new();
    while middle.len() < seam.bytes {
        let left = seam.bytes - middle.len();
        let part = h.call(
            "read_spill",
            &format!(
                r#"{{"hash":"{}","offset":{},"length":{left}}}"#,
                seam.hash,
                seam.offset + middle.len()
            ),
        );
        assert!(part.spill.is_none(), "a read_spill window must not spill");
        assert!(!part.payload.is_empty());
        middle.push_str(&part.payload);
    }
    assert_eq!(middle, full[seam.offset..seam.offset + seam.bytes]);
    assert_eq!(middle.lines().count(), seam.lines);

    // Head, middle and tail put back together are the original.
    let (head, tail) = r
        .payload
        .split_once(&format!("{}\n", seam.render()))
        .unwrap();
    assert_eq!(format!("{head}{middle}{tail}"), full);
}

#[test]
fn the_line_cap_bites_on_short_lines_under_the_byte_cap() {
    let full: String = (0..5_000).map(|i| format!("{i}\n")).collect();
    assert!(full.len() < DEFAULT_INLINE_BYTES);
    let mut h = harness_with_spiller(default_spiller());
    h.rt.registry
        .register(Box::new(Flood(full.clone())))
        .unwrap();
    let r = h.call("flood", "{}");
    assert!(r.spill.is_some(), "5000 lines over a 2000 line cap spills");
    assert!(
        r.payload.lines().count() <= 2_000,
        "{}",
        r.payload.lines().count()
    );
    assert!(r.payload.starts_with("0\n1\n"));
    assert!(r.payload.ends_with("4998\n4999\n"));
}

#[test]
fn a_tools_own_tighter_cap_still_wins() {
    // `bash` keeps a 30 KB tail of its own: 200 KiB of output arrives under that, so the
    // generic cap has nothing left to spill.
    let mut h = match runner_harness() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("a_tools_own_tighter_cap_still_wins: no runner here ({e}), skipped");
            return;
        }
    };
    h.rt.spiller = default_spiller();
    let r = h.call(
        "bash",
        r#"{"command":"yes 0123456789abcdef0123456789abcdef | head -c 204800"}"#,
    );
    assert!(r.is_grounded(), "{}", r.payload);
    assert!(r.spill.is_none(), "bash's own cap must bite first");
    assert!(r.payload.len() <= 30_000 + 1_024, "{}", r.payload.len());
    assert!(!r.payload.lines().any(|l| Seam::parse(l).is_some()));
}
