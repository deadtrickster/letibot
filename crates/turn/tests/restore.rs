//! The acceptance test for resume: the hash chain reproduces from stored tokens, or the restore
//! refuses by row. The real-store test is opt-in (`LETIBOT_CHECK_REAL_STORE=1`) and announces its
//! skip, because a red from the box's own data is one no change to this tree can fix.

use letibot_tokencore::ledger::{LedgerError, hex};
use letibot_tokencore::store::LoadedTranscript;
use letibot_tokencore::store::live_view::LiveView;
use letibot_turn::resume::RestoreError;
use letibot_turn::{Session, resume};

/// The operator's store, read in place through a read-only view, never copied: a copy of a
/// database a daemon is checkpointing is not a snapshot of it, and a 5 GB store copied once per
/// test filled a tmpfs. `LiveView` attaches `mode=ro` and copies one transcript per load.
fn real_store() -> Option<LiveView> {
    // Opt in, and asked for separately from the apparatus - see the module doc. A run that
    // insists on its apparatus is insisting about the CODE's prerequisites; it cannot insist
    // that the operator's history be intact.
    if !letibot_tokencore::apparatus::check_real_store() {
        return None;
    }
    sweep_stale_copies();
    let src = std::env::var("LETIBOT_STORE").unwrap_or_else(|_| {
        format!(
            "{}/.local/share/letibot/sessions.db",
            std::env::var("HOME").unwrap_or_default()
        )
    });
    let src = std::path::PathBuf::from(src);
    if !src.is_file() {
        eprintln!(
            "SKIPPED: LETIBOT_CHECK_REAL_STORE is set, but there is no session store at {} — \
             THIS IS NOT A PASS.",
            src.display()
        );
        return None;
    }
    Some(LiveView::open(&src).unwrap_or_else(|e| panic!("opening {}: {e}", src.display())))
}

/// Is the process `pid` still there? `kill(pid, 0)`, which every POSIX system answers - a
/// `/proc` probe is a fact on Linux and an absence on macOS, so the sweep below did nothing
/// there.
fn process_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 is not delivered, so this asks the kernel only whether `pid` exists  -
    // no memory is shared with it and no handler can run in response.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    // `EPERM` is the other "it is there": a process this one may not signal still exists.
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// The copies older builds of this file left behind, removed once their owner is gone. Only
/// that exact name shape, and only when no process `<pid>` exists, so a live pid keeps its
/// directory.
fn sweep_stale_copies() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
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
            if pid.parse::<u32>().is_ok_and(process_alive) {
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
        // Not a failed assertion: a synthetic store holds tokens this harness produced, not
        // tokens a model did, so it cannot stand in. `real_store` has already said why it did
        // not, and it never panics, so an absent store cannot become a failure.
        return;
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
        return;
    };
    let mut loaded = a_transcript_with_rows(&view);

    // Somebody edited the database - or a renderer changed under us, which produces
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
        return;
    };
    let loaded = a_transcript_with_rows(&view);

    // `persist` indexes `session.items[i]` with the ledger row's index and `append_items` mints
    // `"{transcript}.{items.len()}"`. Both are silently wrong the moment the two vectors differ,
    // so a restore that cannot pair them must not hand back a session at all.
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
