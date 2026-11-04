//! Two daemons, one store, one session - the guard [`crate::writer`] exists for, driven with
//! real `Sessions` and a real store file. It lives here because a test that appends a row needs the
//! harness `Sessions` holds, which is this module's field.

use std::path::PathBuf;

use letibot_sessionlog::registry::Registry;
use letibot_tokencore::store::Store;

use crate::config::Config;
use crate::harness::Parts;
use crate::sessions::Sessions;

/// A directory that removes itself.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("letibot-writer-{tag}-{}-{}", std::process::id(), {
                static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            }));
        std::fs::create_dir_all(&path).expect("creating the temp dir");
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A config for one daemon seated on `dir`, with the store the two of them share.
fn a_daemon_on(dir: &std::path::Path, db: &std::path::Path) -> Config {
    let mut cfg = Config::for_this_box(dir);
    cfg.vocab_gguf = crate::vocab_gguf();
    // A temp store, never the daemon's own.
    cfg.store = Some(db.to_path_buf());
    cfg.session_id = "s-contended".into();
    cfg.http_retries = 0;
    cfg
}

/// A registry holding the session, which `Sessions::open_first` requires: a daemon's first session
/// is registered by its startup, and one registry per daemon is what makes two of them two.
fn a_daemon(cfg: &Config) -> std::sync::Arc<Registry> {
    let registry = Registry::new();
    registry
        .create(&cfg.session_id, "", Sessions::wiring(cfg))
        .expect("the daemon's own session registers");
    registry
}

/// A second `Sessions` on one store file is REFUSED, by name, before a row is written.
/// Two daemons both appended to `#t0`, the store
/// refused the second's rows, and the transcript kept a hole no restart could repair.
#[test]
fn a_second_session_cannot_write_one_transcript() {
    let Some(_) = crate::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("refused");
    let db = dir.0.join("sessions.db");
    let cfg = a_daemon_on(&dir.0, &db);
    let parts = Parts::load(&cfg).expect("the vocabulary must load");

    // A registry of its own, as every daemon has.
    let mut first = Sessions::open_first(&parts, cfg.clone(), a_daemon(&cfg))
        .expect("the first daemon opens its session");

    // A second daemon's open of the SAME id on the same file: two `Sessions`, two registries, one
    // `sessions.db` - the shape of two daemons without the second process.
    let refusal = Sessions::open_first(&parts, cfg.clone(), a_daemon(&cfg))
        .err()
        .expect("two processes may not write one transcript")
        .to_string();
    assert!(
        refusal.contains("is being written by another process"),
        "the refusal names the reason, not just a failure: {refusal}"
    );
    assert!(
        refusal.contains("s-contended"),
        "and the session it is about: {refusal}"
    );
    assert!(
        refusal.contains(&std::process::id().to_string()),
        "and the process holding it, by pid, so a person can find it: {refusal}"
    );

    // The refused open wrote nothing: the row is still the first daemon's, and there is one.
    let store = Store::open(&db).expect("the store reopens");
    let holder = store
        .session_writer("s-contended")
        .expect("the writer row reads")
        .expect("the first daemon still holds it");
    assert_ne!(holder.owner, "", "the claim names its owner: {holder:?}");

    // The transcript reads back as a chain - what interleaving would break, silently.
    first
        .open
        .get_mut("s-contended")
        .expect("the first daemon holds the harness")
        .note_to_model("a row written while another daemon was refused")
        .expect("the append goes through");
    let loaded = store
        .load_transcript("s-contended#t0")
        .expect("the transcript is a chain, not a hole");
    assert_eq!(loaded.items.len(), 1, "one row, written by one writer");

    // The claim goes back with the harness, so a restart is not refused by its predecessor.
    drop(first);
    assert!(
        store
            .session_writer("s-contended")
            .expect("the writer row reads")
            .is_none(),
        "the claim is dropped with the harness"
    );
    Sessions::open_first(&parts, cfg.clone(), a_daemon(&cfg))
        .expect("a restart opens the session it left");
}
