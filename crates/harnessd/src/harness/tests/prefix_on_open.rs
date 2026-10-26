//! **What `Harness::open` does about a transcript whose prefix the rows do not agree with.**
//!
//! Two behaviours, one defect: MEASURED 2026-10-26 in the operator's store, `s-gatekeeper#t0` was
//! seated 2026-10-11 under a 24,407-token prefix with no rows, and a daemon running newer
//! binaries appended its first fourteen rows at **32162** — the prompt *that* build composes.
//!
//! 1. **An empty transcript is re-seated onto the prompt this daemon composes** (option (a), not
//!    (b): with no rows there is no chain to rebuild and no cache to keep, so keeping the old
//!    prompt would only give a new conversation tools it cannot call). This is the prevention —
//!    the store refuses the append now too (`Store::assert_anchored`), so without it such a
//!    session could never take its first turn.
//! 2. **A transcript whose rows chain from a different prefix is forked onto it**, not refused
//!    for ever, and the old transcript is left exactly as it was. This is the repair for stores
//!    already in the state.
//!
//! Both run on stores built here, with the byte vocabulary and a provider that is never called —
//! no GGUF, no model server (`no_vocab.rs`'s reasoning).

use super::*;
use letibot_tokencore::ledger::{chain, hash_tokens};
use letibot_tokencore::store::StablePrefixRecord;

/// A scratch directory that removes itself.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let p = letibot_sessionlog::testing::scratch_root().join(format!(
            "{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        Scratch(p)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// **A config that opens on the BYTE vocabulary**, so nothing here needs a GGUF or a server.
fn byte_vocab_config(dir: &Scratch, id: &str) -> Config {
    let mut cfg = Config::for_this_box(&dir.0);
    cfg.dialect = crate::Dialect::Qwen;
    cfg.store = Some(dir.0.join("sessions.db"));
    cfg.session_id = id.into();
    cfg.vocab_gguf = None;
    cfg.provider = Some(crate::config::ProviderConfig {
        name: "deepseek".into(),
        model: Some("not-called".into()),
        api_key: Some("sk-not-used".into()),
        thinking: false,
    });
    cfg
}

/// The parts, with this box's project modes replaced by none — a resumed session takes its mode
/// from the store, and the test is about the prefix rather than about this laptop.
fn parts(cfg: &Config) -> Parts {
    let parts = Parts::load(cfg).expect("the byte vocabulary loads");
    *parts.mode_store.write().unwrap() = crate::modes::ModeStore::default();
    parts
}

/// A session row and a transcript under a prefix **this daemon does not compose**, with `rows`
/// rows in it (none for the re-seat case).
///
/// The dialect and the vocabulary are this build's own, because they are in the store's record
/// of the prefix and a mismatch there is a DIFFERENT repair (the re-render fork, `open`'s arm
/// above) — the defect this file is about changed the prompt's text and nothing else.
fn seed(
    dir: &Scratch,
    id: &str,
    transcript: &str,
    parts: &Parts,
    rows: &[(u32, Vec<u32>, [u8; 32])],
) -> String {
    let store = Store::open(&dir.0.join("sessions.db")).expect("the store");
    let dialect_sha = letibot_tokencore::ledger::hex(&parts.wiring.spec().template_sha);
    let old_tokens: Vec<u32> = vec![1, 2, 3, 4];
    let old = store
        .put_stable_prefix(&StablePrefixRecord {
            dialect_sha: dialect_sha.clone(),
            system: "a prompt an older build composed".into(),
            tools_json: vec![],
            tokens: old_tokens.clone(),
            h_init: hash_tokens(&old_tokens),
            vocab_source: parts.vocab.source().to_string(),
        })
        .expect("the old prefix row");
    store
        .put_session(&SessionRecord {
            id: id.into(),
            title: Some("a session a newer build opened".into()),
            model_id: "not-called".into(),
            dialect_sha,
            workspace_root: dir.0.display().to_string(),
            owner: "dead".into(),
            approvers: vec![],
            role: None,
            parent_session_id: None,
        })
        .expect("the session row");
    store
        .put_transcript(transcript, id, &old)
        .expect("the transcript row");
    // **Written through the escape hatch on purpose**: `append_item` refuses a row that does not
    // start where the transcript's prefix ends — that is the fix — so a fixture for the state a
    // buggy build left behind has to be written the way that build wrote it.
    for (seq, (offset, tokens, h_k)) in rows.iter().enumerate() {
        store
            .connection()
            .execute(
                "INSERT INTO transcript_item
                   (transcript_id, seq, item_id, kind, item_json,
                    tok_offset, tok_len, h_k, tokens, created_at)
                 VALUES (?1, ?2, ?3, 'user', ?4, ?5, ?6, ?7, ?8, ?9)",
                letibot_tokencore::rusqlite::params![
                    transcript,
                    seq as i64,
                    format!("{transcript}.{seq}"),
                    serde_json::to_string(&TranscriptItem::User {
                        speaker: Default::default(),
                        parts: vec![UserPart::Text {
                            text: format!("row {seq}"),
                        }],
                    })
                    .unwrap(),
                    *offset as i64,
                    tokens.len() as i64,
                    h_k.as_slice(),
                    blob(tokens),
                    1_760_000_000_000i64 + seq as i64,
                ],
            )
            .expect("the row a buggy build wrote");
    }
    old
}

/// Token ids as the store holds them: little-endian `u32`.
fn blob(tokens: &[u32]) -> Vec<u8> {
    tokens.iter().flat_map(|t| t.to_le_bytes()).collect()
}

/// **An empty transcript the older build seated is re-seated onto this daemon's prompt**, and a
/// row appended afterwards rebuilds — which is the whole point: before this, the append landed at
/// the daemon's prefix length inside a transcript whose prefix was the older one, and no resume
/// could rebuild it again.
#[test]
fn an_empty_transcript_is_reseated_onto_the_prompt_this_daemon_composes() {
    let dir = Scratch::new("prefix-reseat");
    let cfg = byte_vocab_config(&dir, "s-reseat");
    let parts = parts(&cfg);
    let old = seed(&dir, "s-reseat", "s-reseat#t0", &parts, &[]);
    let mut h = Harness::open(&parts, cfg, Hub::new("s-reseat")).expect("the session opens");

    let store = Store::open(&dir.0.join("sessions.db")).expect("a second connection");
    let now = store
        .transcript_prefix_id("s-reseat#t0")
        .expect("reading the transcript")
        .expect("the transcript row is there");
    assert_ne!(
        now, old,
        "the transcript was re-seated, not left on the old prompt"
    );
    assert_eq!(
        now, h.prefix_id,
        "it was re-seated onto the prompt THIS daemon composes"
    );
    assert_eq!(
        h.transcript_id(),
        "s-reseat#t0",
        "re-seating an empty transcript is not a fork: there is nothing to keep apart"
    );

    // The fingerprint follows the transcript, or the next resume calls this session stale against
    // a prompt nobody is speaking.
    let seated = store
        .session("s-reseat")
        .expect("reading the session")
        .expect("the session row")
        .prefix_fingerprint;
    assert_eq!(
        seated.as_deref(),
        Some(prefix_fingerprint(h.cfg.seat, &h.prefix).as_str()),
        "the session records the prompt it is now seated with"
    );

    // **And the first append lands at the prompt's own length and rebuilds.** This is what the
    // store would have refused before the re-seat: the row starts where the transcript's prefix
    // ends, so `Session::restore` recomputes the chain the ledger wrote.
    h.session
        .append_items(
            &h.engine,
            &[TranscriptItem::User {
                speaker: Default::default(),
                parts: vec![UserPart::Text {
                    text: "the first thing said in this session".into(),
                }],
            }],
            &mut letibot_turn::events::NullSink,
        )
        .expect("appending the first item");
    h.persist().expect("the row reaches the store");
    let loaded = store
        .load_transcript("s-reseat#t0")
        .expect("loading the transcript");
    assert_eq!(loaded.items.len(), 1);
    let restored = Session::restore(&loaded).unwrap_or_else(|e| {
        panic!("the transcript does not rebuild under the prompt it was re-seated with: {e}")
    });
    assert_eq!(restored.ledger.rows().len(), 1);
}

/// **An empty transcript already on this daemon's prompt is left exactly as it is** — the guard
/// that keeps the re-seat from being a rewrite that happens on every open of a session nobody has
/// spoken to yet, and its note from being pushed for a re-seat that did not happen.
#[test]
fn an_empty_transcript_on_this_prompt_is_not_reseated_or_reported() {
    let dir = Scratch::new("prefix-untouched");
    let cfg = byte_vocab_config(&dir, "s-untouched");
    let parts = parts(&cfg);
    // The first open of a session this store has never seen: it writes `#t0` under the prompt it
    // composes, with no rows, and nothing is appended.
    let first = Harness::open(&parts, cfg.clone(), Hub::new("s-untouched")).expect("the open");
    let prefix_id = first.prefix_id.clone();
    drop(first);

    let second = Harness::open(&parts, cfg, Hub::new("s-untouched")).expect("the second open");
    let store = Store::open(&dir.0.join("sessions.db")).expect("a second connection");
    assert_eq!(
        store
            .transcript_prefix_id("s-untouched#t0")
            .expect("reading")
            .as_deref(),
        Some(prefix_id.as_str()),
        "the transcript was already on this daemon's prompt"
    );
    assert!(
        !second.open_notes().iter().any(|n| n.contains("re-seated")),
        "a re-seat that did not happen must not be reported: {:#?}",
        second.open_notes()
    );
}

/// **A transcript whose rows chain from a prefix it no longer names is forked onto that prefix**,
/// the old transcript untouched — the repair for a store already in the state the defect left.
#[test]
fn a_transcript_that_does_not_rebuild_is_forked_onto_the_prefix_its_rows_chain_from() {
    let dir = Scratch::new("prefix-repair");
    let cfg = byte_vocab_config(&dir, "s-repair");
    let parts = parts(&cfg);
    // The prefix the rows were really written under: seven tokens, and it must be in the store
    // for the repair to find it — which is what the buggy daemon left behind, because it wrote
    // its own prefix row at open (that is where the length came from).
    let theirs: Vec<u32> = vec![9; 7];
    let h_init = hash_tokens(&theirs);
    let store = Store::open(&dir.0.join("sessions.db")).expect("the store");
    let theirs_id = store
        .put_stable_prefix(&StablePrefixRecord {
            dialect_sha: letibot_tokencore::ledger::hex(&parts.wiring.spec().template_sha),
            system: "the prompt that daemon composed".into(),
            tools_json: vec![],
            tokens: theirs.clone(),
            h_init,
            vocab_source: parts.vocab.source().to_string(),
        })
        .expect("the prefix the rows were written under");
    drop(store);

    // Two rows, at offsets 7 and 9 — past the end of the four-token prefix the transcript names.
    let first: Vec<u32> = vec![100, 101];
    let h0 = chain(&h_init, &first);
    let second: Vec<u32> = vec![102, 103, 104];
    let h1 = chain(&h0, &second);
    let old = seed(
        &dir,
        "s-repair",
        "s-repair#t0",
        &parts,
        &[(7, first, h0), (9, second.clone(), h1)],
    );

    let h = Harness::open(&parts, cfg, Hub::new("s-repair"))
        .expect("a transcript that can be repaired must not be a refusal at open");

    assert_eq!(
        h.transcript_id(),
        "s-repair#t1",
        "the conversation continues in a fork, not in the transcript that cannot rebuild"
    );
    assert_eq!(
        h.items().len(),
        2,
        "every row of the conversation came across"
    );
    let store = Store::open(&dir.0.join("sessions.db")).expect("a second connection");
    let moved = store
        .load_transcript("s-repair#t1")
        .expect("the fork loads");
    assert_eq!(
        moved.stable_prefix_id, theirs_id,
        "the fork is seated on the prefix its rows were written under"
    );
    let restored = Session::restore(&moved).unwrap_or_else(|e| {
        panic!("the repaired transcript must rebuild, or the repair is a second broken chain: {e}")
    });
    assert_eq!(restored.ledger.rows().len(), 2);
    assert_eq!(
        moved.items[1].2, second,
        "the tokens are the stored ones — carried, never re-rendered"
    );

    // **The old transcript is untouched**: still the prefix it named, still its rows, still the
    // chain nothing can rebuild. A repair that rewrote it would be unauditable, and a wrong
    // diagnosis would have destroyed the only copy.
    let kept = store
        .load_transcript("s-repair#t0")
        .expect("the old transcript is still there");
    assert_eq!(kept.stable_prefix_id, old);
    assert_eq!(kept.items.len(), 2);
    assert!(
        Session::restore(&kept).is_err(),
        "the old transcript was left exactly as it was, refusal and all"
    );

    // And the operator is told, on the resume report, with the numbers.
    let report = h.resumed().expect("a repaired session is a resumed one");
    let note = report
        .notes
        .iter()
        .find(|n| n.contains("FORKED"))
        .unwrap_or_else(|| panic!("the repair was not reported: {:#?}", report.notes));
    assert!(note.contains("token 7"), "{note}");
    assert!(note.contains("4 tokens long"), "{note}");
}
