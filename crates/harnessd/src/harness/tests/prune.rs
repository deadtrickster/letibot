//! `/prune` end to end: the fork carries every row with the oldest bodies stubbed, the lead names
//! every id, `recall` opens each one, and the old transcript still holds the bodies - a prune whose
//! ids the base prints and nothing can open is a page of dead ends (`tests/recall.rs`).

use super::*;
use crate::harness::PruneReport;
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

/// A body long enough that a stub is worth its line, distinct per call so a wrong id shows.
fn body(marked: &str) -> String {
    let mut s = format!("THE-{marked}-BODY\n");
    while s.len() < 4 * 1024 {
        s.push_str("the quick brown fox jumps over the lazy dog, and again\n");
    }
    s
}

/// Two prunable units - a read the same file's later read supersedes, and a finished command -
/// beside a small body and the exchange in hand, which must both stay.
fn conversation() -> Vec<TranscriptItem> {
    vec![
        user("read src/a.rs and tell me what is wrong"),
        call("c1", "read", r#"{"path":"src/a.rs"}"#),
        result("c1", "read", body("SUPERSEDED")),
        call("c3", "bash", r#"{"command":"cargo test -p letibot-turn"}"#),
        result("c3", "bash", body("FINISHED")),
        user("now read it again"),
        call("c2", "read", r#"{"path":"src/a.rs"}"#),
        result("c2", "read", "the second read, small".to_string()),
        user("carry on"),
    ]
}

/// The first item of a transcript from the store, as text - the lead a prune opens with.
fn first_item_text(path: &std::path::Path, transcript_id: &str) -> String {
    let store = Store::open(path).expect("reopening the store");
    let items = store
        .load_transcript(transcript_id)
        .expect("reading the fork")
        .items;
    match &items[0].0 {
        TranscriptItem::System { text, .. } => text.clone(),
        other => panic!("the base's first item is the lead, got {other:?}"),
    }
}

/// One call's result payload in a stored transcript, by call id.
fn result_of(path: &std::path::Path, transcript_id: &str, call_id: &str) -> String {
    let store = Store::open(path).expect("reopening the store");
    let items = store
        .load_transcript(transcript_id)
        .expect("reading the transcript")
        .items;
    items
        .iter()
        .filter_map(|(i, _, _)| match i {
            TranscriptItem::ToolResult {
                call_id: id,
                payload,
                ..
            } if id == call_id => Some(payload.clone()),
            _ => None,
        })
        .next()
        .unwrap_or_else(|| panic!("call {call_id} has no result in {transcript_id}"))
}

/// The whole chain in one test, because its subject is that the halves agree: a fork whose lead
/// names ids nothing can open, or whose bodies never reached the store, is worse than no fork.
#[test]
fn a_prune_carries_every_row_stubbed_and_recall_opens_each_id() {
    let Some((mut h, path)) = a_harness("s-prune") else {
        return;
    };
    let was = h.transcript_id().to_string();
    h.session
        .append_items(
            &h.engine,
            &conversation(),
            &mut letibot_turn::events::NullSink,
        )
        .expect("seeding the conversation");
    h.persist().expect("the seed reaches the store");

    let r: PruneReport = h.prune().expect("the prune lands");
    assert_ne!(r.fork.transcript_id, was, "a prune is a fork");
    assert_eq!(r.fork.parent_id, was);
    assert!(
        r.fork.base_tokens < r.fork.was_tokens,
        "the stubs are smaller than the bodies were: {} -> {}",
        r.fork.was_tokens,
        r.fork.base_tokens
    );
    assert_eq!(
        r.units.len(),
        2,
        "the superseded read and the finished command go, and nothing else: {:?}",
        r.units
    );

    // The lead says what happened and carries the map: an id the model cannot see is one it
    // cannot ask for, and a body with no id is one it cannot get back.
    let lead = first_item_text(&path, &r.fork.transcript_id);
    assert!(lead.contains("[pruned]"), "the lead names the act: {lead}");
    assert!(
        lead.contains(evict::MAP_HEADER),
        "the map is in the base: {lead}"
    );
    for u in &r.units {
        assert!(
            lead.contains(&u.id),
            "unit {}'s id is in the map: {lead}",
            u.id
        );
    }

    // The carried rows: the call, its arguments and its outcome stay; the body is the stub.
    let stub = result_of(&path, &r.fork.transcript_id, "c1");
    assert!(
        stub.starts_with("[evicted before this record was written"),
        "the superseded body left as a stub: {stub}"
    );
    assert!(
        stub.contains(&r.units[0].id),
        "the stub names where the body went: {stub}"
    );
    assert_eq!(
        result_of(&path, &r.fork.transcript_id, "c2"),
        "the second read, small",
        "a body under the floor stays as it was"
    );

    // And every id opens, exactly: the store the spill wrote and the tool the harness seats are
    // the same store, or the map is a promise nothing keeps.
    for (u, marked) in r.units.iter().zip(["SUPERSEDED", "FINISHED"]) {
        let mut sink = NullToolSink;
        let asked = ToolCall {
            id: format!("call_{}", u.id),
            name: "recall".into(),
            arguments: format!(r#"{{"id":"{}"}}"#, u.id),
        };
        let opened = h.runtime.invoke("turn_1", &asked, &mut sink);
        assert!(opened.is_grounded(), "{}", opened.payload);
        assert_eq!(opened.payload, body(marked), "the body, exactly as it was");
    }

    // The old transcript is untouched: pruning stops carrying, it never deletes.
    assert_eq!(
        result_of(&path, &was, "c1"),
        body("SUPERSEDED"),
        "the old transcript still holds the whole body"
    );
}

/// A prune with nothing to take refuses rather than paying a fork for nothing, and a refusal
/// changes no transcript.
#[test]
fn a_prune_with_nothing_to_take_refuses_and_forks_nothing() {
    let Some((mut h, _path)) = a_harness("s-prune-empty") else {
        return;
    };
    let was = h.transcript_id().to_string();
    let small = vec![
        user("look at this"),
        call("c1", "read", r#"{"path":"src/a.rs"}"#),
        result("c1", "read", "ten bytes".to_string()),
        user("that is all"),
    ];
    h.session
        .append_items(&h.engine, &small, &mut letibot_turn::events::NullSink)
        .expect("seeding the conversation");
    h.persist().expect("the seed reaches the store");

    let refused = h.prune().expect_err("nothing is prunable");
    assert!(
        refused.to_string().contains("nothing to prune"),
        "the refusal says what it looked for: {refused}"
    );
    assert_eq!(h.transcript_id(), was, "a refusal forks nothing");
}
