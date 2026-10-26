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
//! every item through this one, and report what it costs.
//!
//! # It reads the operator's store, so it is OPT IN — the merge gate does not opt in
//!
//! This is a MEASUREMENT against the box's data, not proof about the code under review, so
//! `LETIBOT_CHECK_REAL_STORE=1` is what asks for it:
//!
//!     LETIBOT_CHECK_REAL_STORE=1 cargo test -p letibot-harnessd --test reprefill_feasibility -- --nocapture
//!
//! Without it the test prints one line saying it was skipped and why. `LETIBOT_REQUIRE_APPARATUS`
//! deliberately does not reach it: the store is somebody's history, not the apparatus the code
//! needs. See `letibot_tokencore::apparatus::check_real_store`.
//!
//! # The store is read through a read-only view, never opened and never copied
//!
//! `Store::open` is not a read: it **migrates**. A build whose `SCHEMA_VERSION` is
//! newer than the file's rewrites it in place, so a test that opens the operator's
//! live `~/.local/share/letibot/sessions.db` is a test that upgrades the operator's
//! store underneath the daemon serving it. This one did exactly that — MEASURED
//! with `strace` on 2026-10-06, before the first fix:
//!
//!     openat(AT_FDCWD, "/home/dead/.local/share/letibot/sessions.db",
//!            O_RDWR|O_CREAT|O_NOFOLLOW|O_CLOEXEC, 0644) = 3
//!
//! — and the cost was two subagent spawns: the store moved to 16 while the running
//! daemon was built at 15, and every spawn after that was refused with
//! `SchemaTooNew { found: 16, known: 15 }`.
//!
//! The first fix copied the whole file into the temp dir, which by 2026-10-14 was
//! 5.2 GB of a RAM-backed `/tmp` per run, left behind whenever the run was killed,
//! and not a consistent snapshot of a file a daemon is checkpointing. So the live
//! file is now attached `mode=ro` by `letibot_tokencore::store::live_view::LiveView`,
//! which SQLite itself refuses to write through, and the one transcript measured is
//! copied into memory in one read transaction. `crates/turn/tests/restore.rs` reads
//! the store the same way.

use letibot_tokencore::control::{resolve, tokenize_spans};
use letibot_tokencore::store::live_view::LiveView;
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
    if !letibot_tokencore::apparatus::check_real_store() {
        return;
    }
    let store_path = env_path(
        "LETIBOT_STORE",
        format!("{}/.local/share/letibot/sessions.db", home()),
    );
    let vocab_path = env_path(
        "LETIBOT_VOCAB_GGUF",
        format!("{}/models/Qwen3.8-27B-UD-Q6_K_XL.gguf", home()),
    );
    if !store_path.is_file() || !vocab_path.is_file() {
        eprintln!(
            "SKIPPED: LETIBOT_CHECK_REAL_STORE is set, but this box has no {} or no {} — \
             THIS IS NOT A PASS.",
            store_path.display(),
            vocab_path.display()
        );
        return;
    }

    let wiring = letibot_harnessd::dialect::Dialect::Qwen.wiring(None);
    let ours: String = wiring
        .spec()
        .template_sha
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();

    // **Read-only, never the writer's `Store::open`** — see the module doc.
    let view = LiveView::open(&store_path)
        .unwrap_or_else(|e| panic!("opening {}: {e}", store_path.display()));
    let Some(transcript_id) = biggest_foreign(&view, &ours) else {
        eprintln!("skipped: no transcript recorded under a different dialect");
        return;
    };
    let loaded = view.load(&transcript_id).expect("it loads");

    let vocab = letibot_llama::load(&vocab_path).expect("the vocabulary loads");
    let control =
        resolve(&vocab, &wiring.spec().control_tokens).expect("the control tokens resolve");

    // Each item is rendered against the history as it stood BEFORE it — the same
    // contract `Session::append_items` renders under — so this is the real cost
    // and not an approximation of it.
    let mut history: Vec<TranscriptItem> = Vec::new();
    let mut tokens = 0usize;
    let mut by_kind: std::collections::BTreeMap<&str, (usize, usize)> = Default::default();
    for (item, _, _) in &loaded.items {
        let spans = wiring
            .renderer
            .render_incremental(&history, std::slice::from_ref(item));
        let n = tokenize_spans(&vocab, &control, &spans)
            .unwrap_or_else(|e| {
                panic!(
                    "re-rendering {}: {e}",
                    letibot_tokencore::store::item_kind(item)
                )
            })
            .len();
        let e = by_kind
            .entry(letibot_tokencore::store::item_kind(item))
            .or_default();
        e.0 += 1;
        e.1 += n;
        tokens += n;
        history.push(item.clone());
    }

    let stored: usize = loaded.items.iter().map(|(_, _, t)| t.len()).sum();
    println!(
        "\n  transcript {transcript_id}   {} items",
        loaded.items.len()
    );
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
///
/// A session's transcript is its CURRENT one — newest by `created_at`, then `rowid`, the
/// order `Store::list_sessions` uses — and its size is that transcript's row count.
fn biggest_foreign(view: &LiveView, ours: &str) -> Option<String> {
    let mut stmt = view
        .connection()
        .prepare(
            "SELECT t.id, (SELECT COUNT(*) FROM live.transcript_item i
                            WHERE i.transcript_id = t.id) AS n
               FROM live.session s
               JOIN live.transcript t ON t.id = (
                    SELECT t2.id FROM live.transcript t2 WHERE t2.session_id = s.id
                     ORDER BY t2.created_at DESC, t2.rowid DESC LIMIT 1)
              WHERE s.dialect_sha <> ?1 AND n > 0
              ORDER BY n DESC LIMIT 1",
        )
        .expect("querying the live store");
    let mut rows = stmt
        .query_map([ours], |r| r.get::<_, String>(0))
        .expect("querying the live store");
    rows.next().map(|r| r.expect("reading the live store"))
}
