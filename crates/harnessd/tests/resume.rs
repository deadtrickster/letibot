//! Opening a session that is already in the store: the daemon's half of resume.
//!
//! `letibot-turn`'s `restore.rs` asserts the chain reproduces. This asserts the four
//! things the *daemon* has to get right around it, none of which the ledger knows
//! about:
//!
//! 1. **A head sees the conversation.** The harness holds the restored transcript and
//!    the hub does not, so without a republish a head attaching to a resumed session
//!    is told it has joined an empty one with a 39,384-token prompt. That is the
//!    shape the report described — *"`letibot --continue` doesn't work"* — and it
//!    would survive a perfect ledger restore.
//! 2. **The tools are confined to the session's own tree.** A `~/Projects/rano`
//!    conversation resumed by a daemon started in `~` must not be seated at `~`:
//!    every path would resolve, nothing would error, and the only symptom would be
//!    answers about the wrong tree.
//! 3. **Nothing is written twice.** `persist` picks up at the row after the last
//!    stored one. A resume that reset the counter would replay 56 inserts into an
//!    append-only table and fail on the first.
//! 4. **A dialect that does not match is re-rendered, not refused.** This is the case
//!    the hash chain *cannot* catch — every stored row is correctly hashed by whoever
//!    wrote it — so it is the one that has to be read from the prefix record.
//!
//! # The store is built here, and never borrowed
//!
//! This used to resume a session out of a COPY of the operator's real
//! `~/.local/share/letibot/sessions.db`, and assert that the hub snapshot held exactly
//! as many rows as the store did. MEASURED 2026-10-09: on this box that read `2000`
//! against a session of `1033` rows — red — and on CI the store is absent, so the test
//! returned early and was green. A test that is green only where its fixture is missing
//! measures nothing, and one that reads the operator's history to find out is measuring
//! that history rather than resume.
//!
//! **And the 2000 was right.** A resume republishes the CONVERSATION, which a
//! compaction splits across transcripts: the current one, plus the tail of the ones
//! before it, up to the view's own bound ([`Harness::ancestor_tail`]). So the count the
//! old assertion demanded — *the stored row count* — was one only a store with no
//! compaction chain could satisfy, which is what a fresh store always is.
//!
//! So the store is built here, in a temp dir, with a shape this file states: a session
//! whose conversation a compaction split across two transcripts, [`ANCESTOR_ROWS`] and
//! [`CURRENT_ROWS`] of them. The ancestor tail is then staged on purpose rather than
//! borrowed, the numbers are the same on a laptop and on a runner, and the assertion
//! still fails if a resume stops putting rows on the log.
//!
//! # No vocabulary and no model server
//!
//! The tokens are written here, and a resume is a store read and a re-hash. The config
//! names a provider that is never called, which is what gives it the byte vocabulary
//! and lets this file run on a machine with no GGUF — see
//! `crates/harnessd/tests/no_vocab.rs`. The second test's re-render needs a RENDERER,
//! not a model, and the byte vocabulary is one.

use std::path::{Path, PathBuf};

use letibot_harnessd::config::{Config, ProviderConfig};
use letibot_harnessd::{Dialect, Harness, Parts};
use letibot_sessionlog::hub::Hub;
use letibot_tokencore::ledger::{LedgerRow, chain, hash_tokens};
use letibot_tokencore::store::{SessionRecord, StablePrefixRecord, Store};
use letibot_transcript::{TranscriptItem, UserPart};

/// The dialect this file configures, named once so the fixture and the config cannot
/// drift apart.
const WANTED: Dialect = Dialect::Qwen;

/// The pre-compaction conversation's rows: what a compaction put behind the current
/// transcript, and therefore what a resume republishes as the ancestor tail.
const ANCESTOR_ROWS: u32 = 5;

/// The current transcript's rows: the summary a compaction wrote and what came after
/// it. This is what the session's own ledger holds after a resume.
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

/// **The operator's project modes are not this test's business.**
///
/// A resumed session takes its workspace from the STORE, so on this box that would be
/// `~/Projects/letibot` — which has a row in the operator's real
/// `~/.config/letibot/modes.tsv`. `Harness::open` applies it, and the test is then
/// measuring resume against whatever this laptop is configured for.
///
/// (This used to say the refusal it produced — "missing: writable backend" — was
/// correct and nothing to do with resume. It was a REAL defect in resume: the
/// stored role reached the tool registry and not the backend, so a resumed
/// `leticode` session seated `write` against a read-only view. Fixed in
/// `Harness::open`, where the seat is now taken from the store beside the
/// workspace. The lesson kept: a red test explained in a comment is a hypothesis,
/// and this one was wrong for a week.)
///
/// These tests are about the resume path, so they supply an empty store and let the
/// mode be the daemon's. Same discipline as `wired.rs`: state the condition as a value
/// rather than inheriting whatever this laptop happens to be configured for. Not by
/// pinning $XDG_CONFIG_HOME — `set_var` beside a running thread is undefined behaviour
/// this edition aborts for.
fn empty_modes(parts: Parts) -> Parts {
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    parts
}

fn config(store: &Path, session_id: &str, workspace: &str) -> Config {
    let mut cfg = Config::for_this_box(workspace);
    cfg.dialect = WANTED;
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = session_id.to_string();
    // **A provider that is never called, and no GGUF.** This is what the byte vocabulary
    // is for: the tokens this file resumes are the store's own, and the re-render the
    // second test stages needs a renderer rather than a model.
    cfg.vocab_gguf = None;
    cfg.provider = Some(ProviderConfig {
        name: "deepseek".into(),
        model: None,
        api_key: Some("sk-not-used".into()),
        thinking: false,
    });
    cfg
}

/// **Write `items` onto `transcript` as the ledger's own rows.**
///
/// The offsets, lengths and chain this writes are the ones
/// `letibot_tokencore::ledger::TokenLedger::restore` recomputes, because they are built
/// from the same two functions the ledger uses (`hash_tokens` over the prefix, then
/// `chain` per row). There is no model in this test to render a turn, and a resume does
/// not need one: it replays the numbers it is given.
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

/// **The store this file resumes from**, built here with a known shape.
struct Fixture {
    /// Held so the temp tree outlives the test.
    _dir: TempDir,
    store: PathBuf,
    session_id: String,
    /// The session's own tree, from its row — not where the daemon is started.
    workspace: String,
    /// The transcript the session is currently on: the fork a compaction made.
    current: String,
}

/// **A session whose conversation a compaction split across two transcripts**, written
/// into a store this test owns.
///
/// `recorded_under` is the dialect sha written on the prefix row. The build's own
/// ([`renders_template`]) is the ordinary case — a resume that replays the stored tokens;
/// anything else is the mismatch the second test stages.
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

    // **The prefix row is the session's own**, and the rows below chain from it: the same
    // tokens, so `restore` recomputes the same `h_init` a store written by a daemon holds.
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

    // The compaction: a fork carrying the summary and what came after it. The session's
    // current transcript is the newest one, which is the fork.
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

    // Deliberately NOT the session's own workspace: the daemon is started somewhere
    // else, which is the normal case for `--continue` and the one that used to seat
    // a conversation's tools in the wrong tree.
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

    // (1) The head's view. A snapshot is what a head attaching now would be handed,
    // and `item` being `None` on a row is not a loading state — it is the final
    // state, which is how the operator's own prompt rendered as
    // `[user … — content not loaded]` forever.
    //
    // **The count is the conversation, not the transcript.** A compaction splits the
    // conversation across transcripts, so the snapshot carries the tail of the ones before
    // the current one as well ([`Harness::ancestor_tail`]) — which is why the assertion
    // below is the fixture's two constants rather than [`CURRENT_ROWS`] alone. That is the
    // number the old version of this test got from the operator's store as `2000 against
    // 1033`, and the 2000 was the right one.
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
    // Ids come off the ledger rows, which is what pairs an announcement with its
    // body. A second minting rule here would go stale against `append_items`.
    //
    // **And the session's own rows are the TAIL of the snapshot**: the ancestor tail is
    // published first, oldest first, and the current transcript last. This is what fails if
    // a resume stops putting rows on the log — a count alone would still be satisfied by the
    // ancestor tail.
    let ids = h.row_ids();
    for (row, ledger) in snap.items[snap.items.len() - ids.len()..].iter().zip(&ids) {
        assert_eq!(
            row.item_id, *ledger,
            "the announced id is the ledger row's, in order, and the session's own rows are \
             the tail of the snapshot"
        );
    }

    // (3) Nothing to write: every row is already stored. Asserted through the store
    // rather than through a private counter — a second insert at seq 0 is what the
    // append-only trigger would refuse, and this is the state that avoids it.
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
    // The failure the chain cannot catch. Every row in that transcript is correctly
    // hashed by whoever wrote it, so `verify_chain` passes and the tokens are still
    // another renderer's — appending this one's bytes to them builds a prompt no
    // model was ever trained on, and nothing downstream can see it.
    let probe =
        Parts::load(&config(Path::new("unused"), "", "/tmp")).expect("the wiring must load");
    let this_build = renders_template(&probe);
    // One byte of the recorded template flipped: a template this build does not render,
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

    // **It is no longer refused, and that is the change this asserts.**
    //
    // The refusal was right about the path it was on -- a resume replays the
    // stored TOKENS, and two renderers' bytes in one prompt is a prompt no model
    // was trained on -- and wrong about the data. `item_json` sits beside
    // `tokens` in every row and is dialect-neutral, so the conversation is
    // rebuilt for this renderer instead of being turned away. The old assertion
    // is kept in the negative below: what must NOT happen now is a refusal.
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
    // The fork is a NEW transcript; the old one is untouched, which is what makes
    // this safe to do without asking. Asserted in the store rather than by comparing two
    // ids that could never be equal: the fork's parent is the transcript that was there,
    // and that transcript still holds every row it had.
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
        let p = std::env::temp_dir().join(format!(
            "letibot-{tag}-{}-{}",
            std::process::id(),
            letibot_harnessd::config::now_ns()
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
