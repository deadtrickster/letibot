//! Opening a session that is already in the store: the daemon's half of resume.
//! `letibot-turn`'s `restore.rs` asserts the chain reproduces. This asserts the four things
//! the *daemon* has to get right around it, none of which the ledger knows about 1.

use std::path::{Path, PathBuf};

use letibot_harnessd::config::{Config, ProviderConfig};
use letibot_harnessd::{Dialect, Harness, Parts};
use letibot_sessionlog::hub::Hub;
use letibot_tokencore::ledger::{LedgerRow, chain, hash_tokens};
use letibot_tokencore::store::{SessionRecord, StablePrefixRecord, Store};
use letibot_transcript::{TranscriptItem, UserPart};

/// The dialect this file configures, named once so the fixture and the config cannot drift apart.
const WANTED: Dialect = Dialect::Qwen;

/// The pre-compaction conversation's rows: what a compaction put behind the current transcript, and
/// therefore what a resume republishes as the ancestor tail.
const ANCESTOR_ROWS: u32 = 5;

/// The current transcript's rows: the summary a compaction wrote and what came after it. This is
/// what the session's own ledger holds after a resume.
const CURRENT_ROWS: u32 = 3;

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

/// The hash this build renders `WANTED` with, spelled the way the store holds it.
fn renders_template(parts: &Parts) -> String {
    parts
        .wiring
        .spec()
        .template_sha
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The operator's project modes are not this test's business.
fn empty_modes(parts: Parts) -> Parts {
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    parts
}

fn config(store: &Path, session_id: &str, workspace: &str) -> Config {
    let mut cfg = Config::for_this_box(workspace);
    cfg.dialect = WANTED;
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = session_id.to_string();
    // A provider that is never called, and no GGUF. This is what the byte vocabulary is for
    // the tokens this file resumes are the store's own, and the re-render the second test stages
    // needs a renderer rather than a model.
    cfg.vocab_gguf = None;
    cfg.provider = Some(ProviderConfig {
        name: "deepseek".into(),
        model: None,
        api_key: Some("sk-not-used".into()),
        thinking: false,
    });
    cfg
}

/// Write `items` onto `transcript` as the ledger's own rows.
fn write_rows(store: &Store, transcript: &str, prefix_tokens: &[u32], items: &[TranscriptItem]) {
    let mut h = hash_tokens(prefix_tokens);
    let mut offset = prefix_tokens.len() as u32;
    for (i, item) in items.iter().enumerate() {
        let tokens: Vec<u32> = vec![100 + i as u32, 200 + i as u32];
        h = chain(&h, &tokens);
        let row = LedgerRow {
            item_id: format!("{transcript}.{i}"),
            tok_offset: offset,
            tok_len: tokens.len() as u32,
            h_k: h,
        };
        offset += tokens.len() as u32;
        store
            .append_item(transcript, i as u32, item, &row, &tokens)
            .expect("appending a row");
    }
}

/// The store this file resumes from, built here with a known shape.
struct Fixture {
    /// Held so the temp tree outlives the test.
    _dir: TempDir,
    store: PathBuf,
    session_id: String,
    /// The session's own tree, from its row - not where the daemon is started.
    workspace: String,
    /// The transcript the session is currently on: the fork a compaction made.
    current: String,
}

/// A session whose conversation a compaction split across two transcripts, written into a
/// store this test owns. `recorded_under` is the dialect sha written on the prefix row.
fn fixture(tag: &str, parts: &Parts, recorded_under: &str) -> Fixture {
    let dir = TempDir::new(tag);
    let store_path = dir.path().join("sessions.db");
    let store = Store::open(&store_path).expect("opening the store");
    let session_id = "s-fixture".to_string();
    let workspace_dir = dir.path().join("ws");
    std::fs::create_dir_all(&workspace_dir).expect("the session's own tree");
    let workspace = workspace_dir.display().to_string();

    store
        .put_session(&SessionRecord {
            id: session_id.clone(),
            title: Some("a conversation a compaction split".into()),
            model_id: WANTED.name().into(),
            dialect_sha: recorded_under.into(),
            workspace_root: workspace.clone(),
            owner: "dead".into(),
            approvers: vec![],
            role: None,
            parent_session_id: None,
        })
        .expect("putting the session");

    // The prefix row is the session's own, and the rows below chain from it: the same tokens
    // so `restore` recomputes the same `h_init` a store written by a daemon holds.
    let prefix_tokens: Vec<u32> = vec![7, 8, 9];
    let prefix_id = store
        .put_stable_prefix(&StablePrefixRecord {
            dialect_sha: recorded_under.into(),
            system: "you are letibot".into(),
            tools_json: vec![],
            tokens: prefix_tokens.clone(),
            h_init: hash_tokens(&prefix_tokens),
            vocab_source: parts.vocab.source().to_string(),
        })
        .expect("putting the prefix");

    // The conversation before the compaction.
    let ancestor = format!("{session_id}#t0");
    store
        .put_transcript(&ancestor, &session_id, &prefix_id)
        .expect("putting t0");
    let items: Vec<TranscriptItem> = (0..ANCESTOR_ROWS)
        .map(|i| user(&format!("prompt {i}")))
        .collect();
    write_rows(&store, &ancestor, &prefix_tokens, &items);

    // The compaction: a fork carrying the summary and what came after it. The session's current
    // transcript is the newest one, which is the fork.
    let current = format!("{session_id}#t1");
    store
        .put_fork(&current, &session_id, &prefix_id, &ancestor, ANCESTOR_ROWS)
        .expect("forking");
    let items: Vec<TranscriptItem> = vec![
        user("[summary of the conversation so far]"),
        assistant("understood"),
        user("now do the jobs pane"),
    ];
    write_rows(&store, &current, &prefix_tokens, &items);

    Fixture {
        _dir: dir,
        store: store_path,
        session_id,
        workspace,
        current,
    }
}

#[test]
fn a_resumed_session_is_put_back_on_the_log_where_a_head_can_see_it() {
    let probe =
        Parts::load(&config(Path::new("unused"), "", "/tmp")).expect("the wiring must load");
    let f = fixture("harnessd-resume", &probe, &renders_template(&probe));

    // Deliberately NOT the session's own workspace: the daemon is started somewhere else, which is
    // the normal case for `--continue` and the one that could seat a conversation's tools in the
    // wrong tree.
    let elsewhere = "/tmp";
    assert_ne!(f.workspace, elsewhere);
    let cfg = config(&f.store, &f.session_id, elsewhere);
    let parts = empty_modes(Parts::load(&cfg).expect("the wiring must load"));
    let hub = Hub::new(&f.session_id);
    let h = Harness::open(&parts, cfg, hub.clone()).expect("the session must resume");

    let report = h
        .resumed()
        .unwrap_or_else(|| panic!("{} was not resumed", f.session_id));
    assert_eq!(
        report.rows as u32, CURRENT_ROWS,
        "every stored row came back"
    );
    assert_eq!(report.head, h.ledger_head());
    assert_eq!(h.items().len(), CURRENT_ROWS as usize);

    // (2) The session's own tree, and it said so rather than moving quietly.
    assert_eq!(report.workspace, f.workspace);
    assert!(
        report.notes.iter().any(|n| n.contains(&f.workspace)),
        "a resume that relocated the workspace must say which one it used: {:?}",
        report.notes
    );

    // (1) The head's view. A snapshot is what a head attaching now would be handed, and `item`
    // being `None` on a row is not a loading state - it is the final state, which is how a
    // prompt can render as `[user … - content not loaded]` for ever.
    let snap = hub.snapshot();
    assert_eq!(
        snap.items.len() as u32,
        ANCESTOR_ROWS + CURRENT_ROWS,
        "the snapshot is not the conversation: it must hold the current transcript's rows \
         AND the tail of the one a compaction put behind it, so a head can scroll above the \
         summary"
    );
    assert!(
        snap.items.iter().all(|r| r.item.is_some()),
        "{} row(s) announced with no body",
        snap.items.iter().filter(|r| r.item.is_none()).count()
    );
    // Ids come off the ledger rows, which is what pairs an announcement with its body. A
    // second minting rule here would go stale against `append_items`. And the session's own
    // rows are the TAIL of the snapshot
    let ids = h.row_ids();
    for (row, ledger) in snap.items[snap.items.len() - ids.len()..].iter().zip(&ids) {
        assert_eq!(
            row.item_id, *ledger,
            "the announced id is the ledger row's, in order, and the session's own rows are \
             the tail of the snapshot"
        );
    }

    // (3) Nothing to write: every row is already stored. Asserted through the store rather than
    // through a private counter - a second insert at seq 0 is what the append-only trigger would
    // refuse, and this is the state that avoids it.
    let store = Store::open(&f.store).expect("reopening");
    assert_eq!(
        store.item_count(h.transcript_id()).expect("counting"),
        CURRENT_ROWS,
        "the store grew or shrank during a resume, which reads nothing and writes \
         nothing"
    );
}

#[test]
fn a_session_recorded_under_another_dialect_is_re_rendered_not_refused() {
    // The failure the chain cannot catch.
    let probe =
        Parts::load(&config(Path::new("unused"), "", "/tmp")).expect("the wiring must load");
    let this_build = renders_template(&probe);
    // One byte of the recorded template flipped: a template this build does not render
    // with everything else about the fixture identical.
    let recorded_under = format!("ff{}", &this_build[2..]);
    assert_ne!(
        recorded_under, this_build,
        "the fixture recorded the template this build renders, so nothing was staged"
    );
    let f = fixture("harnessd-dialect", &probe, &recorded_under);

    let cfg = config(&f.store, &f.session_id, "/tmp");
    let parts = empty_modes(Parts::load(&cfg).expect("the wiring must load"));
    let hub = Hub::new(&f.session_id);

    // It is no longer refused, and that is the change this asserts.
    let h = Harness::open(&parts, cfg, hub.clone())
        .expect("a dialect mismatch re-renders rather than refusing");

    let report = h.resumed().expect("a resume reports");
    let notes = report.notes.join(" ");
    assert!(
        notes.contains("RE-RENDERED"),
        "the operator is told the conversation was rebuilt, not quietly given a new one: {notes}"
    );
    assert!(
        notes.contains("fork"),
        "and told the old transcript is kept: {notes}"
    );
    // The fork is a NEW transcript; the old one is untouched, which is what makes this safe to
    // do without asking. Asserted in the store rather than by comparing two ids that could
    // never be equal
    assert_ne!(
        report.transcript_id, f.current,
        "the re-render lands in a fork, not on top of the recorded tokens"
    );
    let store = Store::open(&f.store).expect("reopening");
    assert_eq!(
        store
            .parent_of(&report.transcript_id)
            .expect("the fork's parent"),
        Some(f.current.clone()),
        "the re-render did not fork the transcript it was recorded on"
    );
    assert_eq!(
        store.item_count(&f.current).expect("counting"),
        CURRENT_ROWS,
        "the recorded transcript was rewritten, which the append-only store refuses"
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
