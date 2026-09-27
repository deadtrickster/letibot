//! **`transcript` against a real store with a real fork chain.**
//!
//! The unit tests in `letibot-tools` use a fake source, which proves the tool's
//! reporting; this proves the thing the tool exists for, which is that a row a
//! compaction moved into a parent transcript is still found — and is labelled as
//! history rather than presented as current context.
//!
//! The store is built the way a compaction builds one: a first transcript, then a
//! fork of it at a seq, with the fork carrying only what compaction kept. Nothing
//! here reaches through to SQL; the assertions are on what the tool says.

use std::sync::Arc;

use letibot_harnessd::transcript_source::StoreTranscripts;
use letibot_tokencore::ledger::LedgerRow;
use letibot_tokencore::store::{SessionRecord, StablePrefixRecord, Store};
use letibot_tools::builtins::transcript::{TranscriptQuery, TranscriptSource};
use letibot_transcript::{SystemOrigin, ToolOutcome, TranscriptItem, UserPart};

fn user(text: &str) -> TranscriptItem {
    TranscriptItem::User {
        speaker: Default::default(),
        parts: vec![UserPart::Text { text: text.into() }],
    }
}

fn assistant(text: &str) -> TranscriptItem {
    TranscriptItem::Assistant {
        text: text.into(),
        tool_calls: Vec::new(),
        truncated: false,
    }
}

fn tool_result(name: &str, payload: &str) -> TranscriptItem {
    TranscriptItem::ToolResult {
        call_id: "c1".into(),
        name: name.into(),
        outcome: ToolOutcome::Ok,
        payload: payload.into(),
        edit: None,
        origin: None,
        media: None,
    }
}

fn row(seq: u32) -> LedgerRow {
    LedgerRow {
        item_id: format!("i{seq}"),
        tok_offset: seq,
        tok_len: 1,
        h_k: [0u8; 32],
    }
}

/// A session with two transcripts: `#t0` (the pre-compaction conversation) and
/// `#t1`, forked from it at seq 2, holding the summary and what came after.
fn store_with_a_fork(dir: &std::path::Path) -> (Store, String) {
    let store = Store::open(&dir.join("sessions.db")).expect("opening the store");
    let session_id = "s-test".to_string();
    store
        .put_session(&SessionRecord {
            id: session_id.clone(),
            title: Some("the letibot session".into()),
            model_id: "m".into(),
            dialect_sha: "d".into(),
            workspace_root: "/w".into(),
            owner: "dead".into(),
            approvers: vec![],
            role: None,
            parent_session_id: None,
        })
        .expect("putting the session");
    let prefix = StablePrefixRecord {
        dialect_sha: "d".into(),
        system: "you are letibot".into(),
        tools_json: vec![],
        tokens: vec![1, 2, 3],
        h_init: [0u8; 32],
        vocab_source: "test".into(),
    };
    let prefix_id = store
        .put_stable_prefix(&prefix)
        .expect("putting the prefix");

    let t0 = format!("{session_id}#t0");
    store
        .put_transcript(&t0, &session_id, &prefix_id)
        .expect("putting t0");
    // The pre-compaction conversation. The thing to find later is in here.
    for (seq, item) in [
        TranscriptItem::System {
            text: "you are letibot".into(),
            origin: SystemOrigin::Bootstrap,
        },
        user("the oracle should reach unknown intents"),
        assistant("covers() now filters Intent::Unknown"),
        tool_result("bash", "cargo test: 563 passed"),
    ]
    .into_iter()
    .enumerate()
    {
        store
            .append_item(&t0, seq as u32, &item, &row(seq as u32), &[seq as u32])
            .expect("appending to t0");
    }

    // The compaction: a fork at seq 2, carrying a summary and then new turns.
    let t1 = format!("{session_id}#t1");
    store
        .put_fork(&t1, &session_id, &prefix_id, &t0, 2)
        .expect("forking");
    for (seq, item) in [
        user("[summary of the conversation so far]"),
        user("now do the jobs pane"),
        assistant("enter on a row sends /job ID"),
    ]
    .into_iter()
    .enumerate()
    {
        store
            .append_item(&t1, seq as u32, &item, &row(seq as u32), &[seq as u32])
            .expect("appending to t1");
    }
    (store, session_id)
}

fn source(dir: &std::path::Path, session_id: &str) -> Arc<dyn TranscriptSource> {
    Arc::new(
        StoreTranscripts::open(&dir.join("sessions.db"), session_id.to_string())
            .expect("opening the reader"),
    )
}

fn q() -> TranscriptQuery {
    TranscriptQuery {
        history: true,
        limit: 50,
        ..Default::default()
    }
}

/// **The whole point.** A row that a compaction left behind in `#t0` is found from
/// a session now running on `#t1`, and it comes back marked as an earlier
/// generation — because "this is history you were never given" and "this is
/// something you forgot" are different facts about a row.
#[test]
fn a_row_the_compaction_left_behind_is_found_and_marked_as_history() {
    let d = tempdir();
    let (_store, session_id) = store_with_a_fork(d.path());
    let src = source(d.path(), &session_id);

    let hits = src
        .find(&TranscriptQuery {
            matching: Some("unknown intents".into()),
            ..q()
        })
        .expect("searching");
    assert_eq!(hits.matched, 1, "one row says it");
    let r = &hits.rows[0];
    assert_eq!(
        r.generation, 1,
        "it is in the transcript the fork left behind"
    );
    assert!(r.text.contains("unknown intents"), "{}", r.text);
    assert_eq!(r.kind, "user");
    // And the chain is reported, so the caller can see there was a compaction.
    assert_eq!(hits.chain.len(), 2);
    assert_eq!(hits.chain[0].generation, 0, "the live one is generation 0");
    assert_eq!(hits.chain[1].forked_at_seq, None, "t0 forked from nothing");
    assert_eq!(hits.chain[0].forked_at_seq, Some(2), "t1 was cut at seq 2");
}

/// `history=false` is the caller asking only about the conversation it is in, and
/// then the same search misses. This is the assertion that proves the walk is
/// doing the work rather than the row being in both places.
#[test]
fn without_history_the_same_search_misses() {
    let d = tempdir();
    let (_store, session_id) = store_with_a_fork(d.path());
    let hits = source(d.path(), &session_id)
        .find(&TranscriptQuery {
            matching: Some("unknown intents".into()),
            history: false,
            ..q()
        })
        .expect("searching");
    assert_eq!(hits.matched, 0);
    assert_eq!(hits.chain.len(), 2, "the chain is still reported");
}

/// The text predicate matches what a person would read, not the JSON. `kind` is a
/// column name and appears in every row's `item_json`; a search for it must find
/// nothing rather than everything.
#[test]
fn the_match_is_against_the_text_and_not_the_stored_json() {
    let d = tempdir();
    let (_store, session_id) = store_with_a_fork(d.path());
    let src = source(d.path(), &session_id);
    for json_word in ["item_json", "tool_calls", "parts"] {
        let hits = src
            .find(&TranscriptQuery {
                matching: Some(json_word.into()),
                ..q()
            })
            .expect("searching");
        assert_eq!(hits.matched, 0, "`{json_word}` is JSON, not conversation");
    }
    // While a tool result's payload and its tool name both are text.
    let hits = src
        .find(&TranscriptQuery {
            matching: Some("563 passed".into()),
            ..q()
        })
        .expect("searching");
    assert_eq!(hits.matched, 1);
    assert_eq!(hits.rows[0].tool, "bash");
}

/// Predicates AND, and `kind` reaches the variant without the caller knowing its
/// serde spelling.
#[test]
fn kind_and_tool_narrow_the_search() {
    let d = tempdir();
    let (_store, session_id) = store_with_a_fork(d.path());
    let src = source(d.path(), &session_id);

    let users = src
        .find(&TranscriptQuery {
            kinds: vec!["user".into()],
            ..q()
        })
        .expect("searching");
    assert_eq!(users.matched, 3, "one in t0, two in the fork");

    let by_tool = src
        .find(&TranscriptQuery {
            tool: Some("bash".into()),
            ..q()
        })
        .expect("searching");
    assert_eq!(by_tool.matched, 1);
    assert_eq!(by_tool.rows[0].generation, 1);
}

/// Newest first, across generations: generation 0 before generation 1, and within
/// a transcript the higher seq first. `last` then takes the newest N of that.
#[test]
fn rows_come_back_newest_first_and_last_takes_the_newest_n() {
    let d = tempdir();
    let (_store, session_id) = store_with_a_fork(d.path());
    let hits = source(d.path(), &session_id)
        .find(&TranscriptQuery {
            last: Some(2),
            ..q()
        })
        .expect("searching");
    assert_eq!(hits.rows.len(), 2);
    assert_eq!(hits.rows[0].generation, 0);
    assert_eq!(hits.rows[0].seq, 2, "the newest row in the live transcript");
    assert_eq!(hits.rows[1].seq, 1);
    assert_eq!(
        hits.matched, 2,
        "`last` is applied before the count is taken"
    );
}

/// A session named by a fragment of its title resolves, because that is how the
/// operator names one: *"check letibot letibot session"*.
#[test]
fn a_session_resolves_by_a_fragment_of_its_title() {
    let d = tempdir();
    let (_store, session_id) = store_with_a_fork(d.path());
    // Opened as if from somewhere else entirely, so the default session is not it.
    let src: Arc<dyn TranscriptSource> = Arc::new(
        StoreTranscripts::open(&d.path().join("sessions.db"), "some-other".into())
            .expect("opening"),
    );
    let hits = src
        .find(&TranscriptQuery {
            session: Some("letibot session".into()),
            matching: Some("unknown intents".into()),
            ..q()
        })
        .expect("searching");
    assert_eq!(hits.session_id, session_id);
    assert_eq!(hits.matched, 1);

    // A name that matches nothing is a refusal that says so, not an empty result
    // that reads like a session with no history.
    let miss = src.find(&TranscriptQuery {
        session: Some("no such thing".into()),
        ..q()
    });
    let e = miss.expect_err("a session that does not exist is an error");
    assert!(e.contains("no session matches"), "{e}");
    assert!(
        e.contains("what=sessions"),
        "and says how to list them: {e}"
    );
}

/// A store with no such session id still lists — the reader is not the session,
/// and a session that has written nothing yet is not a broken store.
#[test]
fn a_session_with_no_transcript_reports_an_empty_chain_not_an_error() {
    let d = tempdir();
    let store = Store::open(&d.path().join("sessions.db")).expect("opening");
    store
        .put_session(&SessionRecord {
            id: "s-empty".into(),
            title: None,
            model_id: "m".into(),
            dialect_sha: "d".into(),
            workspace_root: "/w".into(),
            owner: "dead".into(),
            approvers: vec![],
            role: None,
            parent_session_id: None,
        })
        .expect("putting the session");
    let hits = source(d.path(), "s-empty").find(&q()).expect("searching");
    assert!(hits.chain.is_empty());
    assert_eq!(hits.scanned, 0);
    assert_eq!(hits.matched, 0);
}

/// A scratch directory that cleans itself up.
struct Dir(std::path::PathBuf);
impl Dir {
    fn path(&self) -> &std::path::Path {
        &self.0
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tempdir() -> Dir {
    let p = std::env::temp_dir().join(format!(
        "letibot-transcript-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&p).expect("making the scratch dir");
    Dir(p)
}
