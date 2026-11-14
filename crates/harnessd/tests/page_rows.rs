//! A page of rows above a head's window, answered by the store: the ordinal each row names and the
//! clock it was written at. `FetchRows` is ONE frame and a head places a page by the ordinal it says
//! it starts at, so a row the store cannot hand over must not slide the rows after it.

use std::path::{Path, PathBuf};

use letibot_harnessd::Dialect;
use letibot_harnessd::config::Config;
use letibot_harnessd::sessions::StoreSessions;
use letibot_sessionlog::registry::RowSource;
use letibot_tokencore::ledger::{LedgerRow, chain, hash_tokens};
use letibot_tokencore::store::{SessionRecord, StablePrefixRecord, Store};
use letibot_transcript::{TranscriptItem, UserPart};

const SESSION: &str = "s-page";
const TRANSCRIPT: &str = "s-page#t0";
/// A row written on this box is stamped with the clock, so its `ts` is after this. A zero is what a
/// row that was never asked for its clock carries.
const AFTER_2020: u64 = 1_600_000_000_000;

fn user(text: &str) -> TranscriptItem {
    TranscriptItem::User {
        speaker: Default::default(),
        parts: vec![UserPart::Text { text: text.into() }],
    }
}

fn config(store: &Path) -> Config {
    let mut cfg = Config::for_this_box("/tmp");
    cfg.dialect = Dialect::Qwen;
    cfg.store = Some(store.to_path_buf());
    cfg
}

/// A session in a store this test owns, with `n` rows on its only transcript - written the way the
/// ledger's own restore reads them back. `unreadable` is the seq of a row written with JSON this
/// build cannot parse - the row a store hands a page with a hole in it.
fn fixture(tag: &str, n: u32, unreadable: Option<u32>) -> (TempDir, PathBuf) {
    let dir = TempDir::new(tag);
    let path = dir.path().join("sessions.db");
    let store = Store::open(&path).expect("opening the store");
    store
        .put_session(&SessionRecord {
            id: SESSION.into(),
            title: Some("a conversation to page back through".into()),
            model_id: Dialect::Qwen.name().into(),
            dialect_sha: "d".repeat(64),
            workspace_root: "/tmp".into(),
            owner: "dead".into(),
            approvers: vec![],
            role: None,
            parent_session_id: None,
        })
        .expect("putting the session");
    let prefix_tokens: Vec<u32> = vec![7, 8, 9];
    let prefix_id = store
        .put_stable_prefix(&StablePrefixRecord {
            dialect_sha: "d".repeat(64),
            system: "you are letibot".into(),
            tools_json: vec![],
            tokens: prefix_tokens.clone(),
            h_init: hash_tokens(&prefix_tokens),
            vocab_source: "test".into(),
            effort: None,
        })
        .expect("putting the prefix");
    store
        .put_transcript(TRANSCRIPT, SESSION, &prefix_id)
        .expect("putting the transcript");
    let mut h = hash_tokens(&prefix_tokens);
    let mut offset = prefix_tokens.len() as u32;
    for i in 0..n {
        let tokens: Vec<u32> = vec![100 + i, 200 + i];
        h = chain(&h, &tokens);
        if unreadable == Some(i) {
            // The append-only trigger is on the API's path, and the JSON is the only thing wrong
            // with this row: written where the ledger would have put it.
            store
                .connection()
                .execute(
                    "INSERT INTO transcript_item
                       (transcript_id, seq, item_id, kind, item_json, tok_offset, tok_len, h_k, tokens, created_at)
                     VALUES (?1, ?2, ?3, 'user', '{ not json', ?4, ?5, ?6, ?7, ?8)",
                    (
                        TRANSCRIPT,
                        i as i64,
                        format!("{TRANSCRIPT}.{i}"),
                        offset as i64,
                        tokens.len() as i64,
                        h.to_vec(),
                        letibot_tokencore::store::tokens_to_blob(&tokens),
                        AFTER_2020 as i64,
                    ),
                )
                .expect("writing a row this build cannot read");
            offset += tokens.len() as u32;
            continue;
        }
        store
            .append_item(
                TRANSCRIPT,
                i,
                &user(&format!("row {i}")),
                &LedgerRow {
                    item_id: format!("{TRANSCRIPT}.{i}"),
                    tok_offset: offset,
                    tok_len: tokens.len() as u32,
                    h_k: h,
                },
                &tokens,
            )
            .expect("appending a row");
        offset += tokens.len() as u32;
    }
    (dir, path)
}

/// A paged row names its ordinal and the clock it was written at. The store answers a page by
/// ordinal, and a row drawn without its `ts` is the one row on the screen with no time beside it.
#[test]
fn a_paged_row_names_its_ordinal_and_its_clock() {
    let (_dir, path) = fixture("page-rows", 4, None);
    let source = StoreSessions::open(&config(&path)).expect("the store is the source");
    let rows = source.row_items(SESSION, 0, 10);
    assert_eq!(rows.len(), 4, "the fixture wrote four rows");
    for (i, (ordinal, row)) in rows.iter().enumerate() {
        assert_eq!(*ordinal, i, "a row is labelled with another row's ordinal");
        assert_eq!(row.item_id, format!("{TRANSCRIPT}.{i}"));
        assert!(
            row.ts >= AFTER_2020,
            "a paged row draws no clock: its ts is {}",
            row.ts
        );
    }
    // And a page asked for from the middle starts where it was asked to.
    let rows = source.row_items(SESSION, 2, 1);
    assert_eq!(
        rows.first().map(|(ordinal, _)| *ordinal),
        Some(2),
        "a page from the middle is labelled from the front"
    );
}

/// A row the store cannot read does not slide the page. `row_items` leaves an unparseable row
/// out, and the rows after it keep the ordinals they were written at - a page labelled from the
/// request instead puts every row of it one place to the left.
#[test]
fn a_row_the_store_cannot_read_does_not_slide_the_page() {
    let (_dir, path) = fixture("page-rows-skip", 5, Some(2));
    let source = StoreSessions::open(&config(&path)).expect("the store is the source");
    let rows = source.row_items(SESSION, 0, 10);
    let named: Vec<(usize, String)> = rows
        .iter()
        .map(|(ordinal, row)| (*ordinal, row.item_id.clone()))
        .collect();
    assert_eq!(
        named,
        vec![
            (0, format!("{TRANSCRIPT}.0")),
            (1, format!("{TRANSCRIPT}.1")),
            (3, format!("{TRANSCRIPT}.3")),
            (4, format!("{TRANSCRIPT}.4")),
        ],
        "the page was labelled from the request and not from the rows in it"
    );
}

/// Ten lines rather than a dev-dependency.
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let p = letibot_sessionlog::testing::scratch_root().join(format!(
            "letibot-{tag}-{}-{}-{}",
            std::process::id(),
            letibot_harnessd::config::now_ns(),
            {
                static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            }
        ));
        std::fs::create_dir_all(&p).expect("a temp dir");
        TempDir(p)
    }
    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
