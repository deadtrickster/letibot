//! The acceptance test for resume: **the hash chain reproduces, or the restore
//! refuses**.
//!
//! `harnessd`'s startup banner used to say a stored session is *"listed and refused
//! with that reason, not resumed approximately"*, and the reason was right: replaying
//! the items through `append_items` **re-renders** the assistant rows, which were cut
//! from the ids the server streamed. Re-rendering them during recovery reintroduces
//! exactly the renderer non-determinism the chain exists to catch, at the one moment
//! nobody is watching.
//!
//! [`Session::restore`] takes the tokens from the store instead. That makes the
//! refusal unnecessary and makes *this* file the thing that has to hold: a restore
//! that produced a different chain would be strictly worse than the honest refusal it
//! replaces, because the refusal at least told the truth.
//!
//! # What is asserted, and why each one is here
//!
//! 1. **Every restored row's `h_k` is the one the store recorded**, byte for byte,
//!    and the restored ledger's head is the last stored `h_k`. This is checked
//!    against the store's own bytes rather than against the value `restore` handed
//!    back, because `TokenLedger::restore` keeps the persisted row on success — so
//!    comparing its output to its input would be a tautology.
//! 2. **`verify_chain` passes on the rebuilt region.** A second, independent pass:
//!    (1) checks the rows the restore produced, this one recomputes the chain from
//!    the *tokens in the rebuilt memfd*. A restore that wrote the right rows over the
//!    wrong bytes fails here and nowhere else.
//! 3. **A tampered row is refused by name.** Not "restore returned Err" — the error
//!    has to say which row and both hashes, because the operator's next question is
//!    always "which turn".
//! 4. **The real store, not only fixtures.** The five sessions in
//!    `~/.local/share/letibot/sessions.db` are what the operator will actually type
//!    `--continue` against. A fixture proves the code path; those five prove it
//!    against tokens a real Qwen produced, including tool results and interleaved
//!    reasoning.
//!
//! The real-store half is skipped when there is no store — and says so loudly rather
//! than passing quietly, because a test that goes green when the thing it tests is
//! absent reports the health of a `Path::exists`.

use letibot_tokencore::ledger::{LedgerError, hex};
use letibot_tokencore::store::{LoadedTranscript, Store};
use letibot_turn::resume::RestoreError;
use letibot_turn::{Session, resume};

/// The operator's store, copied so nothing here can touch the original.
///
/// The `-wal` file is copied with it: this database is written by a daemon that may
/// be running right now, and a bare copy of the `.db` alone would silently be a
/// snapshot from the last checkpoint — old rows, no error, and a test that measured
/// the wrong week.
fn real_store_copy() -> Option<(TempDir, Store)> {
    let src = std::env::var("LETIBOT_STORE").unwrap_or_else(|_| {
        format!(
            "{}/.local/share/letibot/sessions.db",
            std::env::var("HOME").unwrap_or_default()
        )
    });
    let src = std::path::PathBuf::from(src);
    if !src.is_file() {
        return None;
    }
    let dir = TempDir::new("letibot-restore");
    let dst = dir.path().join("sessions.db");
    std::fs::copy(&src, &dst).expect("copying the store");
    for suffix in ["-wal", "-shm"] {
        let from = src.with_file_name(format!(
            "{}{suffix}",
            src.file_name().unwrap().to_string_lossy()
        ));
        if from.is_file() {
            let _ = std::fs::copy(&from, dir.path().join(format!("sessions.db{suffix}")));
        }
    }
    let store = Store::open(&dst).expect("opening the copy");
    Some((dir, store))
}

/// Ten lines rather than a dev-dependency, for one test file.
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let p = std::env::temp_dir().join(format!(
            "{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
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

fn transcripts(store: &Store) -> Vec<String> {
    let mut stmt = store
        .connection()
        .prepare("SELECT id FROM transcript ORDER BY id")
        .expect("listing transcripts");
    stmt.query_map([], |r| r.get::<_, String>(0))
        .expect("listing transcripts")
        .filter_map(Result::ok)
        .collect()
}

/// Assertions 1 and 2, over one loaded transcript.
fn restores_faithfully(loaded: &LoadedTranscript) -> Session {
    let session = Session::restore(loaded).unwrap_or_else(|e| {
        panic!("{} did not restore: {e}", loaded.transcript_id);
    });

    // (1) Against the store's own bytes.
    assert_eq!(
        session.ledger.rows().len(),
        loaded.items.len(),
        "{}: one ledger row per stored item",
        loaded.transcript_id
    );
    for (i, (_, stored, tokens)) in loaded.items.iter().enumerate() {
        let got = &session.ledger.rows()[i];
        assert_eq!(
            hex(&got.h_k),
            hex(&stored.h_k),
            "{} row {i} ({}): the restored chain head is not the stored one",
            loaded.transcript_id,
            stored.item_id
        );
        assert_eq!(
            got.item_id, stored.item_id,
            "{} row {i}",
            loaded.transcript_id
        );
        assert_eq!(
            got.tok_offset, stored.tok_offset,
            "{} row {i}",
            loaded.transcript_id
        );
        assert_eq!(
            got.tok_len, stored.tok_len,
            "{} row {i}",
            loaded.transcript_id
        );
        assert_eq!(
            session
                .ledger
                .item_tokens(i)
                .expect("a restored row has tokens"),
            tokens.as_slice(),
            "{} row {i}: the rebuilt region does not hold the stored tokens",
            loaded.transcript_id
        );
    }
    assert_eq!(
        session.items.len(),
        loaded.items.len(),
        "{}: one transcript item per row",
        loaded.transcript_id
    );

    // (2) Recomputed from the rebuilt region, not from the rows.
    session.ledger.verify_chain().unwrap_or_else(|e| {
        panic!(
            "{}: the rebuilt region does not hash to its rows: {e}",
            loaded.transcript_id
        )
    });

    if let Some(last) = loaded.items.last() {
        assert_eq!(
            hex(&session.ledger.head()),
            hex(&last.1.h_k),
            "{}: the head is not the last stored row",
            loaded.transcript_id
        );
    }
    session
}

#[test]
fn every_session_in_the_real_store_restores_with_its_chain_intact() {
    let Some((_dir, store)) = real_store_copy() else {
        // **Absent apparatus, not a failed assertion** — and the comment it replaces
        // is kept in substance, because it is right about FIXTURES: a synthetic store
        // holds tokens this harness produced, not tokens a model did, so it cannot
        // stand in. That argues against fabricating one, not against skipping: on the
        // box that has a real store this runs in full, and on the box that does not it
        // says so. See `letibot_tokencore::apparatus`.
        return letibot_tokencore::apparatus::absent(
            "a session store (LETIBOT_STORE, or ~/.local/share/letibot/sessions.db)",
        );
    };

    let ids = transcripts(&store);
    assert!(
        !ids.is_empty(),
        "the store has no transcripts; there is nothing to restore"
    );

    let mut with_rows = 0;
    for id in &ids {
        let loaded = store
            .load_transcript(id)
            .unwrap_or_else(|e| panic!("{id} did not load: {e}"));
        let session = restores_faithfully(&loaded);
        if !loaded.items.is_empty() {
            with_rows += 1;
        }
        eprintln!(
            "  restored {id}: {} rows, {} tokens, head {}",
            session.ledger.rows().len(),
            session.ledger.len(),
            &session.ledger_head()[..16]
        );
    }
    assert!(
        with_rows > 0,
        "every transcript in the store is empty, so nothing exercised the chain"
    );
}

#[test]
fn a_row_whose_tokens_were_changed_is_refused_by_name() {
    let Some((_dir, store)) = real_store_copy() else {
        return letibot_tokencore::apparatus::absent(
            "a session store (LETIBOT_STORE, or ~/.local/share/letibot/sessions.db)",
        );
    };
    let id = transcripts(&store)
        .into_iter()
        .find(|id| store.item_count(id).unwrap_or(0) >= 2)
        .expect("a transcript with rows to tamper with");
    let mut loaded = store.load_transcript(&id).expect("loading");

    // Somebody edited the database — or a renderer changed under us, which produces
    // the same bytes and is the case this is really guarding.
    let victim = loaded.items[1].1.item_id.clone();
    loaded.items[1].2[0] = loaded.items[1].2[0].wrapping_add(1);

    let err = Session::restore(&loaded)
        .err()
        .expect("a changed token must break the chain");
    let text = err.to_string();
    assert!(
        matches!(
            err,
            RestoreError::Chain(LedgerError::ChainMismatch { index: 1, .. })
        ),
        "the refusal must name the row, not just fail: {text}"
    );
    assert!(
        text.contains(&victim),
        "the refusal must name the item id an operator can look up: {text}"
    );
}

#[test]
fn rows_and_items_that_do_not_pair_are_refused_rather_than_zipped() {
    let Some((_dir, store)) = real_store_copy() else {
        return letibot_tokencore::apparatus::absent(
            "a session store (LETIBOT_STORE, or ~/.local/share/letibot/sessions.db)",
        );
    };
    let id = transcripts(&store)
        .into_iter()
        .find(|id| store.item_count(id).unwrap_or(0) >= 2)
        .expect("a transcript with rows");
    let loaded = store.load_transcript(&id).expect("loading");

    // `persist` indexes `session.items[i]` with the *ledger row's* index and
    // `append_items` mints `"{transcript}.{items.len()}"`. Both are wrong the moment
    // the two vectors are different lengths, and both are silently wrong: the first
    // writes the wrong item next to a row, the second re-uses an item id. So a
    // restore that could not pair them must not hand back a session at all.
    let mut short = loaded.clone();
    short.items.pop();
    // Rebuild only the *item* side of the pairing gap: the ledger rows still all
    // there, one transcript item missing.
    let err = resume::restore_parts(
        &short.transcript_id,
        &short.prefix_tokens,
        short.h_init,
        &loaded.ledger_input(),
        short.items.iter().map(|(i, _, _)| i.clone()).collect(),
    )
    .err()
    .expect("a row with no item must be refused");
    assert!(matches!(err, RestoreError::Unpaired { .. }), "{err}");
    assert!(err.to_string().contains("row"), "{err}");
}
