//! The map a compaction leaves, end to end: evicted bodies go to the store under ids, the new base carries one line per unit, and `recall` opens the body a line names.
//! The join is the point. Each half is tested where it lives (`letibot-turn` `evict`, `letibot-tools` `recall`) and neither would catch a harness evicting a body nothing can open.

use super::*;
use crate::harness::wall::Spilled;
use letibot_tools::NullToolSink;
use letibot_transcript::ToolOutcome;
use letibot_turn::evict;

/// A harness over a store of its own, or `None` when this box has no vocabulary GGUF.
fn a_harness(id: &str) -> Option<(Harness, std::path::PathBuf)> {
    let gguf = crate::vocab_gguf()?;
    let dir =
        letibot_sessionlog::testing::scratch_root().join(format!("{id}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("the scratch directory");
    let path = dir.join("sessions.db");
    let mut cfg = Config::for_this_box(&dir);
    cfg.dialect = crate::Dialect::Qwen;
    cfg.session_id = id.into();
    cfg.vocab_gguf = Some(gguf);
    cfg.store = Some(path.clone());
    // The window a recall is bounded by is the session's own, and this fixture's is the box's.
    cfg.context_window = Some(262_144);
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    let h = Harness::open(&parts, cfg, Hub::new(id)).expect("the session opens");
    Some((h, path))
}

fn user(text: &str) -> TranscriptItem {
    TranscriptItem::User {
        speaker: Default::default(),
        parts: vec![UserPart::Text { text: text.into() }],
    }
}

fn call(id: &str, name: &str, arguments: &str) -> TranscriptItem {
    TranscriptItem::Assistant {
        text: String::new(),
        tool_calls: vec![letibot_transcript::ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: arguments.into(),
        }],
        truncated: false,
    }
}

fn result(id: &str, name: &str, payload: String) -> TranscriptItem {
    TranscriptItem::ToolResult {
        call_id: id.into(),
        name: name.into(),
        outcome: ToolOutcome::Ok,
        payload,
        edit: None,
        origin: None,
        media: None,
    }
}

/// The body the eviction takes: long enough to be worth a stub.
fn body() -> String {
    let mut s = String::from("THE-SUPERSEDED-READ\n");
    while s.len() < 4 * 1024 {
        s.push_str("the quick brown fox jumps over the lazy dog, and again\n");
    }
    s
}

/// One read of a file, then the same file read again in the exchange the session is inside: the
/// first body cannot be the current state of anything, so it is the one a compaction evicts.
fn conversation() -> Vec<TranscriptItem> {
    vec![
        user("read src/a.rs and tell me what is wrong"),
        call("c1", "read", r#"{"path":"src/a.rs"}"#),
        result("c1", "read", body()),
        user("now read it again"),
        call("c2", "read", r#"{"path":"src/a.rs"}"#),
        result("c2", "read", "the second read, small".to_string()),
        user("carry on"),
    ]
}

fn outcome(summary: &str) -> CompactionOutcome {
    CompactionOutcome {
        turn_id: "test-turn".into(),
        summary: summary.into(),
        tool_calls: 0,
        truncated: false,
        cached_tokens: 0,
        reusable: 0,
        generated_tokens: 0,
        budget: None,
        fidelity: None,
    }
}

/// The text of the base's first item, which is the note a compaction writes about itself.
fn base_note(path: &std::path::Path, transcript_id: &str) -> String {
    let store = Store::open(path).expect("reopening the store");
    let items = store
        .load_transcript(transcript_id)
        .expect("reading the fork")
        .items;
    match &items[0].0 {
        TranscriptItem::System { text, .. } => text.clone(),
        other => panic!("the base's first item is a system update, got {other:?}"),
    }
}

/// The whole chain in one test, because its subject is that the three halves agree: an eviction
/// whose id the base prints and `recall` cannot open is a page of dead ends, and so is one whose
/// body never reached the store.
#[test]
fn the_new_base_names_what_was_evicted_and_recall_opens_it() {
    let Some((mut h, path)) = a_harness("s-recall-map") else {
        return;
    };
    let items = conversation();
    // The store the harness's tools read, which is where a compaction's bodies must land.
    let runtime = h.runtime.clone();
    let reduced = evict::reduce(&items, &[], &Spilled(&*runtime.spiller.store));
    let unit = reduced
        .units
        .first()
        .unwrap_or_else(|| panic!("the superseded read must go: {:?}", reduced.units))
        .clone();
    assert_eq!(unit.tool, "read");
    assert_eq!(
        unit.named, "src/a.rs",
        "the map line says what it was about"
    );

    // The body is in the store the tool reads, under the id the map prints, byte for byte.
    let held = runtime
        .spiller
        .store
        .get(&unit.id, None)
        .expect("the evicted body must be in the store");
    assert_eq!(String::from_utf8_lossy(&held), body());

    h.note_evicted(&reduced.units);
    let summary = "the file's first read said the same as its second; nothing was written";
    let report = h
        .fork_to_summary(&outcome(summary), None, None, ForkTail::NONE)
        .expect("the fork must land");

    // The map is IN the base, under the record: a model that reads the record and not the map has
    // been told what was lost and not where it is.
    let note = base_note(&path, &report.transcript_id);
    assert!(note.contains(summary), "the record rides verbatim: {note}");
    assert!(
        note.contains(evict::MAP_HEADER),
        "the map is in the compacted prompt: {note}"
    );
    assert!(
        note.contains(&unit.id) && note.contains("src/a.rs"),
        "and its line carries the id and the subject: {note}"
    );

    // And the id opens: the exact body, not a description of it.
    let mut sink = NullToolSink;
    let asked = ToolCall {
        id: "call_recall".into(),
        name: "recall".into(),
        arguments: format!(r#"{{"id":"{}"}}"#, unit.id),
    };
    let opened = h.runtime.invoke("turn_1", &asked, &mut sink);
    assert!(opened.is_grounded(), "{}", opened.payload);
    assert_eq!(opened.payload, body(), "the body, exactly as it was");
}

/// Nothing evicted means no map: a record with an empty index under it reads as a compaction that
/// lost something and is not saying what.
#[test]
fn a_fork_with_nothing_evicted_carries_no_map() {
    let Some((mut h, path)) = a_harness("s-recall-empty") else {
        return;
    };
    let report = h
        .fork_to_summary(&outcome("nothing was evicted"), None, None, ForkTail::NONE)
        .expect("the fork must land");
    let note = base_note(&path, &report.transcript_id);
    assert!(
        !note.contains(evict::MAP_HEADER),
        "a fork that evicted nothing must not carry a map: {note}"
    );
}
