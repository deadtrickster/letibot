//! The recall offer against a real store with a real fork chain: the Done-when's own session, where
//! the turn that discussed "etcd reference-format archives" is in a transcript the compaction left
//! behind, and the person then asks "remember how we built the etcd archives?".
//!
//! No model and no network: the point is the index and the line, and neither of them needs one.

use std::path::{Path, PathBuf};

use letibot_harnessd::recall_index;
use letibot_harnessd::recall_offer::{LOG, Offers};
use letibot_tokencore::ledger::LedgerRow;
use letibot_tokencore::store::{SessionRecord, StablePrefixRecord, Store};
use letibot_transcript::{SystemOrigin, ToolCall, ToolOutcome, TranscriptItem, UserPart};

/// A directory that removes itself.
struct Dir(PathBuf);

impl Dir {
    fn new(tag: &str) -> Dir {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let p = letibot_sessionlog::testing::scratch_root().join(format!(
            "letibot-recall-{tag}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("making the scratch dir");
        Dir(p)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

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

fn result(name: &str, payload: &str) -> TranscriptItem {
    TranscriptItem::ToolResult {
        call_id: "c1".into(),
        name: name.into(),
        outcome: ToolOutcome::Ok,
        payload: payload.into(),
        edits: Vec::new(),
        origin: None,
        media: None,
    }
}

fn row(seq: u32) -> LedgerRow {
    LedgerRow {
        item_id: format!("i{seq}"),
        // The fixture seats a three-token prefix, so row 0 starts after it.
        tok_offset: 3 + seq,
        tok_len: 1,
        h_k: [0u8; 32],
    }
}

/// A session whose earlier turn built the etcd archives, compacted into `#t1`: the work is in `#t0`,
/// which the prompt no longer carries, and the live transcript holds only the summary and the turn
/// in hand. Returns the store, the session id and the live transcript's id.
fn store_with_a_compaction(dir: &Path, workspace: &Path) -> (Store, String, String, String) {
    let store = Store::open(&dir.join("sessions.db")).expect("opening the store");
    let session_id = "s-etcd".to_string();
    store
        .put_session(&SessionRecord {
            id: session_id.clone(),
            title: Some("the store split".into()),
            model_id: "m".into(),
            dialect_sha: "d".into(),
            workspace_root: workspace.display().to_string(),
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
        effort: None,
    };
    let prefix_id = store
        .put_stable_prefix(&prefix)
        .expect("putting the prefix");

    let t0 = format!("{session_id}#t0");
    store
        .put_transcript(&t0, &session_id, &prefix_id)
        .expect("putting t0");
    // The turn the compaction is about to take away: the person asked for the export, the model
    // said how it was built.
    for (seq, item) in [
        TranscriptItem::System {
            text: "you are letibot".into(),
            origin: SystemOrigin::Bootstrap,
        },
        user("build the etcd export as reference-format archives, one file per key prefix"),
        assistant(
            "done: the exporter walks the prefixes and writes one reference-format archive each",
        ),
        result("bash", "ls: etcd-0001.archive etcd-0002.archive"),
        // Two more turns, because a corpus of ONE document weighs every term at zero and nothing
        // can score against it - a real conversation is never one turn, and this fixture is.
        user("now the postgres backfill, in batches"),
        assistant("batched in a thousand rows, the lock released between them"),
        user("and the model card's filter"),
        assistant("a digit narrows the card instead of marking a row"),
    ]
    .into_iter()
    .enumerate()
    {
        store
            .append_item(&t0, seq as u32, &item, &row(seq as u32), &[seq as u32])
            .expect("appending to t0");
    }

    // The compaction: a fork at seq 2 that carries a summary, and then the turn in hand.
    let t1 = format!("{session_id}#t1");
    store
        .put_fork(&t1, &session_id, &prefix_id, &t0, 2)
        .expect("forking");
    for (seq, item) in [
        user("[summary: the export work is done]"),
        user("now the restore"),
        assistant("the restore replays key by key"),
    ]
    .into_iter()
    .enumerate()
    {
        store
            .append_item(&t1, seq as u32, &item, &row(seq as u32), &[seq as u32])
            .expect("appending to t1");
    }
    (store, session_id, t1, prefix_id)
}

/// A subagent of that session, in the same workspace, with a turn of its own: its parent's index
/// must not carry it, because a child's turns re-tell the work its parent did.
fn add_child(store: &Store, prefix_id: &str, workspace: &Path, parent: &str) -> String {
    let child = "s-etcd-child".to_string();
    store
        .put_session(&SessionRecord {
            id: child.clone(),
            title: Some("a child".into()),
            model_id: "m".into(),
            dialect_sha: "d".into(),
            workspace_root: workspace.display().to_string(),
            owner: "dead".into(),
            approvers: vec![],
            role: None,
            parent_session_id: Some(parent.to_string()),
        })
        .expect("putting the child");
    let t = format!("{child}#t0");
    store
        .put_transcript(&t, &child, prefix_id)
        .expect("putting the child's transcript");
    for (seq, item) in [
        user("a subagent: ship the kafka credentials rotation"),
        assistant("rotated and restarted"),
    ]
    .into_iter()
    .enumerate()
    {
        store
            .append_item(&t, seq as u32, &item, &row(seq as u32), &[seq as u32])
            .expect("appending to the child");
    }
    child
}

/// The two tools a seat has to have for the hint to be composed at all.
fn tools() -> Vec<String> {
    vec!["transcript".to_string(), "read".to_string()]
}

/// The line the model is handed, as text.
fn text_of(item: &TranscriptItem) -> String {
    match item {
        TranscriptItem::User { parts, .. } => parts
            .iter()
            .filter_map(|p| match p {
                UserPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" "),
        other => panic!("a user-side row, not {other:?}"),
    }
}

/// The Done-when: an evicted turn about "etcd reference-format archives", and the operator's
/// "remember how we built the etcd archives?" gets a tail hint naming `transcript`, a match on
/// those words, and the session the turn is in. Without a back-reference and with nothing over the
/// floor it says nothing.
#[test]
fn an_evicted_turn_is_offered_when_the_operator_refers_back() {
    let d = Dir::new("done-when");
    let workspace = d.path().join("ws");
    std::fs::create_dir_all(&workspace).expect("the workspace");
    let (store, session_id, live, _prefix) = store_with_a_compaction(d.path(), &workspace);

    // No embedder on this box's fixture, so this is the lexical path - which is the third clause.
    let built = recall_index::build(Some(&store), &workspace, Some(live.as_str()), "", None);
    assert!(
        built
            .units
            .iter()
            .any(|u| u.text.contains("reference-format archives")),
        "the evicted turn is in the index: {:?}",
        built.units.iter().map(|u| &u.text).collect::<Vec<_>>()
    );
    assert!(
        !built
            .units
            .iter()
            .any(|u| u.text.contains("now the restore")),
        "and the live transcript, which the prompt carries, is not"
    );

    let mut offers = Offers::new().with_tools(tools().iter());
    offers.units(built.units, built.vectors);
    let words = "remember how we built the etcd archives?";
    assert!(offers.compose(words, words), "the hint is earned");
    let text = text_of(&offers.tail()[0]);
    assert!(
        text.contains("`transcript` with match: \"archives\""),
        "{text}"
    );
    assert!(
        text.contains("etcd"),
        "and the other term that scored it: {text}"
    );
    assert!(text.contains("history: true"), "{text}");
    assert!(text.contains(&session_id), "the source session: {text}");
    assert!(text.contains("the store split"), "and its title: {text}");

    // A prompt with no back-reference and nothing over the floor says nothing at all.
    let mut quiet = Offers::new().with_tools(tools().iter());
    let again = recall_index::build(Some(&store), &workspace, Some(live.as_str()), "", None);
    quiet.units(again.units, again.vectors);
    let sourdough = "the sourdough starter wants equal weights of flour and water";
    assert!(!quiet.compose(sourdough, sourdough));
    assert!(quiet.tail().is_empty());
}

/// The round that makes the call is the take-up, and the log says so - which is what the floor is
/// tuned from rather than guessed.
#[test]
fn the_rounds_own_transcript_call_is_logged_as_a_take_up() {
    let d = Dir::new("take-up");
    let workspace = d.path().join("ws");
    std::fs::create_dir_all(&workspace).expect("the workspace");
    let (store, _session, live, _prefix) = store_with_a_compaction(d.path(), &workspace);
    let built = recall_index::build(Some(&store), &workspace, Some(live.as_str()), "", None);

    let logs = d.path().join("logs");
    std::fs::create_dir_all(&logs).expect("the logs dir");
    let mut offers = Offers::new().with_tools(tools().iter());
    offers.units(built.units, built.vectors);
    let words = "remember how we built the etcd archives?";
    assert!(offers.compose(words, words));
    let call = ToolCall {
        id: "c9".into(),
        name: "transcript".into(),
        arguments: r#"{"match": "etcd", "history": true}"#.into(),
    };
    let settled = offers
        .settle(&logs, "s-me", &[call])
        .cloned()
        .expect("the round's offer");
    assert!(settled.taken);
    let text = std::fs::read_to_string(logs.join(LOG)).expect("the log exists");
    let row: serde_json::Value =
        serde_json::from_str(text.lines().next().expect("a line")).expect("a JSON line");
    assert_eq!(row["taken"], true);
    assert_eq!(row["match"], "archives");
}

/// The index is a cache by unit id: the first read embeds what it found and the second embeds
/// nothing, because the vectors are in the project's own file.
#[test]
fn the_index_costs_its_embeddings_once() {
    let d = Dir::new("warm");
    let workspace = d.path().join("ws");
    std::fs::create_dir_all(&workspace).expect("the workspace");
    let (store, _session, live, _prefix) = store_with_a_compaction(d.path(), &workspace);
    let tier = letibot_embeddings::testing::planted(
        &d.path().join("data"),
        &[&["etcd", "prefix"], &["restore", "replay"]],
    );
    let first = recall_index::build(
        Some(&store),
        &workspace,
        Some(live.as_str()),
        "planted",
        Some(&tier),
    );
    assert!(!first.units.is_empty(), "there is something to index");
    assert_eq!(
        first.embedded,
        first.units.len(),
        "the first read embeds every unit"
    );
    assert!(
        first.scanned > 0,
        "the first read has to read a transcript it does not hold"
    );
    let second = recall_index::build(
        Some(&store),
        &workspace,
        Some(live.as_str()),
        "planted",
        Some(&tier),
    );
    assert_eq!(second.embedded, 0, "and the second read embeds none");
    assert_eq!(second.scanned, 0, "and reads no transcript at all");
    assert_eq!(second.vectors.len(), first.vectors.len());
}

/// A subagent's turns are not the project's past: a child re-tells the work its parent did, and a
/// hint pointing at the child's copy of it is a hint pointing at nothing new. Off by default, and
/// this is the default.
#[test]
fn a_childs_turns_are_not_in_the_projects_index() {
    let d = Dir::new("child");
    let workspace = d.path().join("ws");
    std::fs::create_dir_all(&workspace).expect("the workspace");
    let (store, session, live, prefix) = store_with_a_compaction(d.path(), &workspace);
    add_child(&store, &prefix, &workspace, &session);
    let built = recall_index::build(Some(&store), &workspace, Some(live.as_str()), "", None);
    assert!(
        !built
            .units
            .iter()
            .any(|u| u.text.contains("kafka credentials")),
        "the child's turn was indexed: {:?}",
        built.units.iter().map(|u| &u.text).collect::<Vec<_>>()
    );
    assert!(
        built.units.iter().all(|u| u.session == session),
        "and every unit is the parent's"
    );
}
