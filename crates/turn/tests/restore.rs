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
use letibot_tokencore::store::LoadedTranscript;
use letibot_tokencore::store::live_view::LiveView;
use letibot_turn::resume::RestoreError;
use letibot_turn::{Session, resume};

/// **The operator's store, read in place through a read-only view — never copied.**
///
/// This used to `std::fs::copy` the whole file, `-wal` and `-shm` beside it, into the temp
/// dir, once per test. MEASURED 2026-10-14: a 5.2 GB store with a 346 MB WAL, three tests
/// running in parallel, so ~16.7 GB in a `/tmp` that is a tmpfs; a failed or killed run left
/// its copy behind (two 9.3 GB leftovers found that day); and a file copy of a database a
/// daemon is checkpointing is not a snapshot of it. No test here ever wrote to its copy, and
/// each reads one transcript at a time, so [`LiveView`] attaches the file `mode=ro` and
/// copies one transcript into memory per load, in one read transaction. See its module doc.
fn real_store() -> Option<LiveView> {
    sweep_stale_copies();
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
    Some(LiveView::open(&src).unwrap_or_else(|e| panic!("opening {}: {e}", src.display())))
}

/// **The copies older builds of this file left behind**, removed once their owner is gone.
///
/// Builds before the [`LiveView`] wrote `letibot-restore-<pid>-<nanos>-<n>/sessions.db`, a
/// full copy of the store, and lost it whenever the test panicked past its guard or was
/// killed. Those builds still exist in other checkouts, so this sweeps their leftovers: only
/// that exact name shape, and only when no process `<pid>` exists — a live pid, even a reused
/// one, keeps its directory. Linux only, because `/proc` is the probe; elsewhere it does
/// nothing rather than guess.
fn sweep_stale_copies() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        if !std::path::Path::new("/proc/self").exists() {
            return;
        }
        let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(rest) = name
                .to_str()
                .and_then(|n| n.strip_prefix("letibot-restore-"))
            else {
                continue;
            };
            let parts: Vec<&str> = rest.split('-').collect();
            let [pid, nanos, n] = parts.as_slice() else {
                continue;
            };
            if ![pid, nanos, n]
                .iter()
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
            {
                continue;
            }
            if std::path::Path::new("/proc").join(pid).exists() {
                continue;
            }
            match std::fs::remove_dir_all(entry.path()) {
                Ok(()) => eprintln!("  removed a stale store copy: {}", entry.path().display()),
                Err(e) => eprintln!(
                    "  could not remove a stale store copy {}: {e}",
                    entry.path().display()
                ),
            }
        }
    });
}

/// The first transcript with at least two rows, which is what the tamper tests need.
fn a_transcript_with_rows(view: &LiveView) -> LoadedTranscript {
    let id: String = view
        .connection()
        .query_row(
            "SELECT transcript_id FROM live.transcript_item
              GROUP BY transcript_id HAVING COUNT(*) >= 2
              ORDER BY transcript_id LIMIT 1",
            [],
            |r| r.get(0),
        )
        .expect("a transcript with rows to tamper with");
    view.load(&id).expect("loading")
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
    let Some(view) = real_store() else {
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

    let ids = view.transcripts().expect("listing transcripts");
    assert!(
        !ids.is_empty(),
        "the store has no transcripts; there is nothing to restore"
    );

    let mut with_rows = 0;
    for id in &ids {
        let loaded = view
            .load(id)
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
    let Some(view) = real_store() else {
        return letibot_tokencore::apparatus::absent(
            "a session store (LETIBOT_STORE, or ~/.local/share/letibot/sessions.db)",
        );
    };
    let mut loaded = a_transcript_with_rows(&view);

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
    let Some(view) = real_store() else {
        return letibot_tokencore::apparatus::absent(
            "a session store (LETIBOT_STORE, or ~/.local/share/letibot/sessions.db)",
        );
    };
    let loaded = a_transcript_with_rows(&view);

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
