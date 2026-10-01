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

/// The session in the copied store with the most rows **that this build could
/// actually resume**, and how many.
///
/// The filter is not tidiness. A session's tokens came out of one renderer, and
/// `Harness::open` refuses to append another's bytes to them — correctly, and by
/// design. This file configures [`Dialect::Qwen`], so picking the biggest session
/// outright measures nothing the moment the operator's newest sessions are GLM:
/// the run fails on the guard rather than on resume, and the failure names a
/// dialect mismatch that is the test's own doing.
///
/// Selecting by model keeps the vocabulary right too — `Config::for_this_box`
/// names the qwen GGUF, and a GLM session resumed against it would be the same
/// defect one layer down.
///
/// **And the model id is not enough.** What `Harness::open` compares is the
/// TEMPLATE hash, and a template is edited from time to time — so the operator's
/// store holds qwen sessions this binary renders differently, and the biggest one
/// was exactly that (measured 2026-09-15: the run failed on the template guard,
/// naming a mismatch the selector had chosen). The guard was right; the fixture
/// was stale. So the sha this build produces is passed in and candidates are
/// checked against what their own stable prefix was recorded under.
fn biggest(path: &std::path::Path, renders: &str) -> (String, u32, String) {
    let store = Store::open(path).expect("opening the copy");
    let all = store.list_sessions().expect("listing");
    let total = all.len();
    let named: Vec<_> = all
        .into_iter()
        .filter(|s| Dialect::parse(&s.model_id) == Some(WANTED))
        .collect();
    let named_count = named.len();
    let mut same_template: Vec<_> = named
        .into_iter()
        .filter(|s| recorded_under(&store, s).as_deref() == Some(renders))
        .collect();
    same_template.sort_by_key(|s| std::cmp::Reverse(s.items));
    let s = same_template.into_iter().next().unwrap_or_else(|| {
        panic!(
            "of the {total} stored session(s), {named_count} were recorded under `{}` \
             and none of those under the template this build renders ({renders}). That \
             is a fact about the store and not a pass: run a session on this build, or \
             set LETIBOT_STORE to a store that has one.",
            WANTED.name()
        )
    });
    assert!(
        s.items > 0,
        "every stored `{}` session is empty, so this measured nothing",
        WANTED.name()
    );
    (s.id, s.items, s.workspace_root)
}

/// The template hash a stored session's tokens were produced under, or `None`
/// when it has no transcript to have been produced under one.
fn recorded_under(store: &Store, s: &letibot_tokencore::store::StoredSession) -> Option<String> {
    let t = store.load_transcript(s.transcript_id.as_ref()?).ok()?;
    let meta = store.stable_prefix_meta(&t.stable_prefix_id).ok()??;
    Some(meta.dialect_sha)
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

/// The dialect this file configures, named once so the selector and the config
/// cannot drift apart — which is exactly how the selector came to pick a session
/// the config could not open.
const WANTED: Dialect = Dialect::Qwen;

/// **The operator's project modes are not this test's business.**
///
/// A resumed session takes its workspace from the STORE, so on this box that is
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
/// These two tests are about the resume path, so they supply an empty store and
/// let the mode be the daemon's. Same discipline as `wired.rs`: state the
/// condition as a value rather than inheriting whatever this laptop happens to
/// be configured for. Not by pinning $XDG_CONFIG_HOME — `set_var` beside a
/// running thread is undefined behaviour this edition aborts for.
fn empty_modes(parts: Parts) -> Parts {
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    parts
}

fn config(store: &std::path::Path, session_id: &str, workspace: &str) -> Config {
    let mut cfg = Config::for_this_box(workspace);
    cfg.dialect = WANTED;
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
        // Absent apparatus rather than a failed assertion; the fixture argument in
        // the message above still stands (see `letibot_tokencore::apparatus`).
        return letibot_tokencore::apparatus::absent(
            "a session store (LETIBOT_STORE, or ~/.local/share/letibot/sessions.db)",
        );
    };
    // Loaded before the pick, because which session this build can resume depends
    // on the template it renders — and that is a property of the loaded wiring, not
    // of a name. The session id in this first config is a placeholder; `Parts::load`
    // reads the vocabulary and the dialect and nothing else.
    let probe = Parts::load(&config(&path, "", "/tmp")).expect("the vocabulary must load");
    let (session_id, items, workspace) = biggest(&path, &renders_template(&probe));

    // Deliberately NOT the session's own workspace: the daemon is started somewhere
    // else, which is the normal case for `--continue` and the one that used to seat
    // a conversation's tools in the wrong tree.
    let elsewhere = "/tmp";
    assert_ne!(workspace, elsewhere);
    let cfg = config(&path, &session_id, elsewhere);
    let parts = empty_modes(Parts::load(&cfg).expect("the vocabulary must load"));
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
fn a_session_recorded_under_another_dialect_is_re_rendered_not_refused() {
    // The failure the chain cannot catch. Every row in that transcript is correctly
    // hashed by whoever wrote it, so `verify_chain` passes and the tokens are still
    // another renderer's — appending this one's bytes to them builds a prompt no
    // model was ever trained on, and nothing downstream can see it.
    let Some((_dir, path)) = store_copy("harnessd-dialect") else {
        return letibot_tokencore::apparatus::absent(
            "a session store (LETIBOT_STORE, or ~/.local/share/letibot/sessions.db)",
        );
    };
    let probe = Parts::load(&config(&path, "", "/tmp")).expect("the vocabulary must load");
    let (session_id, _, workspace) = biggest(&path, &renders_template(&probe));

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
    let parts = empty_modes(Parts::load(&cfg).expect("the vocabulary must load"));
    let hub = Hub::new(&session_id);

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
    // this safe to do without asking.
    assert_ne!(
        report.transcript_id, session_id,
        "the re-render lands in a fork, not on top of the recorded tokens"
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
