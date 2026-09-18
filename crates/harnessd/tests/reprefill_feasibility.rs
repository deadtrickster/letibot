//! **Can a session recorded under one dialect be re-rendered under another?**
//!
//! The resume path refuses a dialect change, and correctly: it replays stored
//! TOKENS, and two renderers' bytes in one prompt is a prompt no model saw.
//!
//! But `transcript_item` stores `item_json` BESIDE `tokens`, and `item_json` is
//! dialect-neutral — a tool call is `{name, arguments}`, not `<function=…>`
//! markup. So the tokens are a cache of one rendering and the items are the
//! record, which means the conversation can be REBUILT for the other renderer
//! rather than refused. The refusal is a property of the fast path, not of the
//! data.
//!
//! This measures that on the operator's own store rather than asserting it from
//! a reading: load a real transcript recorded under the other dialect, render
//! every item through this one, and report what it costs. Skips when the store
//! or the vocabulary is not on this box.
//!
//!     cargo test -p letibot-harnessd --test reprefill_feasibility -- --nocapture

use letibot_tokencore::control::{resolve, tokenize_spans};
use letibot_tokencore::store::Store;
use letibot_tokencore::vocab::Vocab;
use letibot_transcript::TranscriptItem;

fn env_path(var: &str, fallback: String) -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var(var).unwrap_or(fallback))
}

fn home() -> String {
    std::env::var("HOME").unwrap_or_default()
}

#[test]
fn a_transcript_from_the_other_dialect_re_renders_under_this_one() {
    let store_path = env_path(
        "LETIBOT_STORE",
        format!("{}/.local/share/letibot/sessions.db", home()),
    );
    let vocab_path = env_path(
        "LETIBOT_VOCAB_GGUF",
        format!("{}/models/Qwen3.8-27B-UD-Q6_K_XL.gguf", home()),
    );
    if !store_path.is_file() || !vocab_path.is_file() {
        eprintln!("skipped: need {} and {}", store_path.display(), vocab_path.display());
        return;
    }

    let wiring = letibot_harnessd::dialect::Dialect::Qwen.wiring(None);
    let ours: String = wiring.spec().template_sha.iter().map(|b| format!("{b:02x}")).collect();

    let store = Store::open(&store_path).expect("the store opens");
    let Some(transcript_id) = biggest_foreign(&store, &ours) else {
        eprintln!("skipped: no transcript recorded under a different dialect");
        return;
    };
    let loaded = store.load_transcript(&transcript_id).expect("it loads");

    let vocab = Vocab::load(&vocab_path).expect("the vocabulary loads");
    let control = resolve(&vocab, &wiring.spec().control_tokens).expect("the control tokens resolve");

    // Each item is rendered against the history as it stood BEFORE it — the same
    // contract `Session::append_items` renders under — so this is the real cost
    // and not an approximation of it.
    let mut history: Vec<TranscriptItem> = Vec::new();
    let mut tokens = 0usize;
    let mut by_kind: std::collections::BTreeMap<&str, (usize, usize)> = Default::default();
    for (item, _, _) in &loaded.items {
        let spans = wiring.renderer.render_incremental(&history, std::slice::from_ref(item));
        let n = tokenize_spans(&vocab, &control, &spans)
            .unwrap_or_else(|e| panic!("re-rendering {}: {e}", letibot_tokencore::store::item_kind(item)))
            .len();
        let e = by_kind.entry(letibot_tokencore::store::item_kind(item)).or_default();
        e.0 += 1;
        e.1 += n;
        tokens += n;
        history.push(item.clone());
    }

    let stored: usize = loaded.items.iter().map(|(_, _, t)| t.len()).sum();
    println!("\n  transcript {transcript_id}   {} items", loaded.items.len());
    println!("  {:<14} {:>7} {:>12}", "kind", "rows", "re-rendered");
    for (k, (rows, toks)) in &by_kind {
        println!("  {k:<14} {rows:>7} {toks:>12}");
    }
    println!("\n  stored under the other dialect : {stored:>9} tokens");
    println!("  re-rendered under this one     : {tokens:>9} tokens\n");

    assert_eq!(history.len(), loaded.items.len(), "every item rendered");
    assert!(tokens > 0, "the re-rendered prompt is not empty");
}

/// The largest transcript whose recorded dialect is not `ours` — a two-row
/// session would prove nothing about a six-hundred-row one.
fn biggest_foreign(store: &Store, ours: &str) -> Option<String> {
    let mut best: Option<(u32, String)> = None;
    for s in store.list_sessions().ok()? {
        if s.dialect_sha == ours || s.items == 0 {
            continue;
        }
        let Some(t) = s.transcript_id else { continue };
        if best.as_ref().is_none_or(|(n, _)| s.items > *n) {
            best = Some((s.items, t));
        }
    }
    best.map(|(_, t)| t)
}
