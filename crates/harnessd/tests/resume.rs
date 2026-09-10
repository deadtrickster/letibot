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
//! 4. **A dialect that does not match refuses.** This is the case the hash chain
//!    *cannot* catch — every stored row is correctly hashed by whoever wrote it — so
//!    it is the one that has to be checked explicitly.
//!
//! # The store is copied, never touched
//!
//! This reads the operator's real sessions, and it copies the file (with its `-wal`,
//! which a daemon may be writing right now) before opening it. Nothing here can reach
//! the original, including the test that deliberately corrupts a `dialect_sha`.
//!
//! It needs the vocabulary GGUF and **not** the model server: a resume is a store
//! read and a re-hash, and if it needed a generation to prove itself it would be the
//! wrong shape. `LETIBOT_VOCAB_GGUF` overrides the path.

use letibot_harnessd::config::Config;
use letibot_harnessd::{Dialect, Harness, Parts};
use letibot_sessionlog::hub::Hub;
use letibot_tokencore::store::Store;

/// The operator's store, copied. `None` when there is not one.
fn store_copy(tag: &str) -> Option<(TempDir, std::path::PathBuf)> {
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
    let dir = TempDir::new(tag);
    let dst = dir.path().join("sessions.db");
    std::fs::copy(&src, &dst).ok()?;
    for suffix in ["-wal", "-shm"] {
        let from = src.with_file_name(format!(
            "{}{suffix}",
            src.file_name().unwrap().to_string_lossy()
        ));
        if from.is_file() {
            let _ = std::fs::copy(&from, dir.path().join(format!("sessions.db{suffix}")));
        }
    }
    Some((dir, dst))
}

/// The session in the copied store with the most rows, and how many.
fn biggest(path: &std::path::Path) -> (String, u32, String) {
    let store = Store::open(path).expect("opening the copy");
    let s = store
        .list_sessions()
        .expect("listing")
        .into_iter()
        .max_by_key(|s| s.items)
        .expect("the store has sessions");
    assert!(
        s.items > 0,
        "every stored session is empty, so this measured nothing"
    );
    (s.id, s.items, s.workspace_root)
}

fn config(store: &std::path::Path, session_id: &str, workspace: &str) -> Config {
    let mut cfg = Config::for_this_box(workspace);
    cfg.dialect = Dialect::Qwen;
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = session_id.to_string();
    if let Ok(g) = std::env::var("LETIBOT_VOCAB_GGUF") {
        cfg.vocab_gguf = g.into();
    }
    cfg
}

#[test]
fn a_resumed_session_is_put_back_on_the_log_where_a_head_can_see_it() {
    let Some((_dir, path)) = store_copy("harnessd-resume") else {
        panic!(
            "no store to resume. A fixture cannot stand in for this: the rows an \
             operator will type `--continue` against hold tokens a real model \
             produced. Set LETIBOT_STORE, or run this on the box that has one."
        );
    };
    let (session_id, items, workspace) = biggest(&path);

    // Deliberately NOT the session's own workspace: the daemon is started somewhere
    // else, which is the normal case for `--continue` and the one that used to seat
    // a conversation's tools in the wrong tree.
    let elsewhere = "/tmp";
    assert_ne!(workspace, elsewhere);
    let cfg = config(&path, &session_id, elsewhere);
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let hub = Hub::new(&session_id);
    let h = Harness::open(&parts, cfg, hub.clone()).expect("the session must resume");

    let report = h
        .resumed()
        .unwrap_or_else(|| panic!("{session_id} has {items} rows and was not resumed"));
    assert_eq!(report.rows as u32, items, "every stored row came back");
    assert_eq!(report.head, h.ledger_head());
    assert_eq!(h.items().len(), items as usize);

    // (2) The session's own tree, and it said so rather than moving quietly.
    assert_eq!(report.workspace, workspace);
    assert!(
        report.notes.iter().any(|n| n.contains(&workspace)),
        "a resume that relocated the workspace must say which one it used: {:?}",
        report.notes
    );

    // (1) The head's view. A snapshot is what a head attaching now would be handed,
    // and `item` being `None` on a row is not a loading state — it is the final
    // state, which is how the operator's own prompt rendered as
    // `[user … — content not loaded]` forever.
    let snap = hub.snapshot();
    assert_eq!(
        snap.items.len(),
        items as usize,
        "the transcript did not reach the log, so a head would see an empty session"
    );
    assert!(
        snap.items.iter().all(|r| r.item.is_some()),
        "{} row(s) announced with no body",
        snap.items.iter().filter(|r| r.item.is_none()).count()
    );
    // Ids come off the ledger rows, which is what pairs an announcement with its
    // body. A second minting rule here would go stale against `append_items`.
    for (row, ledger) in snap.items.iter().zip(h.row_ids()) {
        assert_eq!(row.item_id, ledger, "the announced id is the ledger row's");
    }

    // (3) Nothing to write: every row is already stored. Asserted through the store
    // rather than through a private counter — a second insert at seq 0 is what the
    // append-only trigger would refuse, and this is the state that avoids it.
    let store = Store::open(&path).expect("reopening");
    assert_eq!(
        store.item_count(h.transcript_id()).expect("counting"),
        items,
        "the store grew or shrank during a resume, which reads nothing and writes \
         nothing"
    );
}

#[test]
fn a_session_recorded_under_another_dialect_is_refused_by_name() {
    // The failure the chain cannot catch. Every row in that transcript is correctly
    // hashed by whoever wrote it, so `verify_chain` passes and the tokens are still
    // another renderer's — appending this one's bytes to them builds a prompt no
    // model was ever trained on, and nothing downstream can see it.
    let Some((_dir, path)) = store_copy("harnessd-dialect") else {
        panic!("no store; see the other test in this file for why this is not skipped");
    };
    let (session_id, _, workspace) = biggest(&path);

    {
        // `stable_prefix` carries no append-only trigger — it is content-addressed
        // and the chain does not run through it — so this is the one edit that can
        // stage the case at all.
        let store = Store::open(&path).expect("opening the copy");
        store
            .connection()
            .execute(
                "UPDATE stable_prefix SET dialect_sha = 'ff' || substr(dialect_sha, 3)",
                [],
            )
            .expect("staging a dialect mismatch");
    }

    let cfg = config(&path, &session_id, &workspace);
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let hub = Hub::new(&session_id);
    let err = Harness::open(&parts, cfg, hub.clone())
        .err()
        .expect("a dialect mismatch must refuse");
    let text = err.to_string();
    assert!(
        text.contains("was NOT resumed") || text.contains("NOT resumed"),
        "the refusal must say nothing happened: {text}"
    );
    assert!(
        text.contains(&session_id),
        "the refusal must name the session: {text}"
    );
    assert_eq!(
        hub.snapshot().items.len(),
        0,
        "a refused resume must not have published half a transcript first"
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
