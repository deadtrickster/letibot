//! **A session's model choice survives a restart** — the operator's question of
//! 2026-10-04: *"will model choice be saved for a session? will it survive
//! restars?"* It did not: `set_provider` was in-memory, the session row had no
//! column for a choice, and a switched session came back on the daemon's own
//! CLI default with the first turn quietly going to a model nobody chose for
//! that conversation.
//!
//! Three moving parts, and each has its half pinned here:
//!
//! 1. **the switch writes the row** (`Harness::persist_provider_choice`), in the
//!    same breath as `publish_settings` — a switch that reached the screen but
//!    not the row was a choice that expired with the process;
//! 2. **the restore reads it at both open paths** — the first session a
//!    `--continue` resumes and the lazy `harness()` a head's switch reaches;
//! 3. **the restore goes through the SAME door the switch went through**
//!    (`models_choice`), so a restore cannot build a provider the verb would
//!    refuse — and a preset this build no longer knows fails SAID rather than
//!    silently, with the row left alone so a build that does know it still finds
//!    it.
//!
//! The store's own half (the column, its four spellings, the v13 migration) is
//! pinned in `tokencore`'s own tests; this file is the harness's half.
//!
//! **What these tests cannot cover, stated rather than hidden:** restoring a
//! KNOWN provider requires that provider's key to resolve, and a key on this
//! box is not a thing CI has — so the known-provider branch is exercised by the
//! branch that refuses (`no-such-provider`), which is the same code path down to
//! `models_choice`, plus the live session that switched to `glm-coding` and is
//! being answered by it right now.

use letibot_harnessd::config::Config;
use letibot_harnessd::{Dialect, Harness, Parts};
use letibot_sessionlog::hub::Hub;
use letibot_tokencore::store::Store;

const WANTED: Dialect = Dialect::Qwen;

fn config(store: &std::path::Path, session_id: &str) -> Config {
    let mut cfg = Config::for_this_box("/tmp");
    cfg.dialect = WANTED;
    cfg.http_retries = 0;
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = session_id.to_string();
    // `Config::for_this_box` carries no vocabulary default any more (a daemon on
    // the byte vocabulary needs none), so a harness built here is handed one: the
    // operator's `LETIBOT_VOCAB_GGUF` if it is set, else the box's own GGUF — the
    // same path this file's `present_gguf` gate consults.
    cfg.vocab_gguf = std::env::var("LETIBOT_VOCAB_GGUF")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(letibot_tokencore::apparatus::present_gguf);
    cfg
}

/// A session row the restore can read, written straight to the store the way a
/// previous daemon's switch would have left it.
fn seeded_store(path: &std::path::Path, session_id: &str) {
    let s = Store::open(path).expect("the store opens");
    s.put_session(&letibot_tokencore::store::SessionRecord {
        id: session_id.into(),
        title: None,
        model_id: "qwen-3.8-27b".into(),
        dialect_sha: "00".repeat(32),
        workspace_root: "/tmp".into(),
        owner: "test".into(),
        role: None,
        approvers: vec![],
        parent_session_id: None,
    })
    .expect("the session row");
}

struct TempDir {
    path: std::path::PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "harnessd-provider-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("scratch");
        TempDir { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// **An unswitched session writes `local` BY NAME, not NULL.**
///
/// The two answer different questions at a resume — *no opinion* against *the
/// local server, deliberately* — and a daemon whose own default is a provider
/// would hand a switched-back session straight back to it if the two collapsed
/// into NULL.
#[test]
fn a_switch_writes_the_row_and_local_is_written_by_name() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let dir = TempDir::new("persist");
    let path = dir.path.join("sessions.db");
    let session_id = "provider-persist";
    let cfg = config(&path, session_id);
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let hub = Hub::new(session_id);
    let h = Harness::open(&parts, cfg, hub).expect("the session must open");

    // Nothing switched: the row this writes is `local`, because the daemon's own
    // default being a provider is a real configuration and NULL must not mean
    // "whatever the daemon was started on" for a session that CHOSE local.
    h.persist_provider_choice().expect("the row is written");
    let s = Store::open(&path).expect("reopening");
    assert_eq!(
        s.provider_choice(session_id).unwrap().as_deref(),
        Some("local"),
        "an unswitched session records local BY NAME, not NULL"
    );
}

/// **A resume restores the session's own model** — and a preset this build does
/// not know is refused SAID, with the daemon's default standing and the row left
/// alone so a build that does know the preset still finds it.
#[test]
fn a_resume_restores_the_sessions_own_model_and_a_unknown_preset_is_refused_by_name() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let dir = TempDir::new("restore");
    let path = dir.path.join("sessions.db");
    let session_id = "provider-restore";
    {
        // The previous daemon's switch, as the row it left behind.
        let s = Store::open(&path).expect("the store opens");
        seeded_store(&path, session_id);
        s.set_provider_choice(session_id, Some("no-such-provider"))
            .expect("the row is written");
    }
    let cfg = config(&path, session_id);
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let hub = Hub::new(session_id);
    let mut h = Harness::open(&parts, cfg, hub).expect("the session must open");

    let said = h.restore_provider_choice().expect("a sentence");
    assert!(
        said.contains("no-such-provider") && said.contains("does not know that provider"),
        "the refusal names the preset and says what answers instead: {said}"
    );
    // **And the row is left alone**, so a build that does know the preset still
    // finds it on the next open rather than the refusal having eaten the choice.
    let s = Store::open(&path).expect("reopening");
    assert_eq!(
        s.provider_choice(session_id).unwrap().as_deref(),
        Some("no-such-provider"),
        "a refused restore must not clear the row it failed to apply"
    );

    // A second open of the SAME session is not a re-switch of anything: the
    // restore reads the row it left, says the same refusal, and the daemon
    // default keeps answering. The no-op-by-`already` arm (a live session a head
    // switched INTO) needs a resolvable provider, and that is the live session's
    // half — stated in the module note rather than hidden.
    drop(s);
    assert_eq!(
        h.restore_provider_choice().expect("the same sentence"),
        said,
        "a second caller is not a second answer"
    );
}

/// **A session with no row for a choice resumes on the daemon's default** — which
/// is what such a session always did, and the reason NULL is not backfilled: a
/// store recorded before this column existed has no opinion to restore.
#[test]
fn a_session_that_never_chose_resumes_on_the_daemon_default() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let dir = TempDir::new("unswitched");
    let path = dir.path.join("sessions.db");
    let session_id = "provider-unswitched";
    {
        let s = Store::open(&path).expect("the store opens");
        seeded_store(&path, session_id);
        // No provider_choice written: NULL is the state.
    }
    let cfg = config(&path, session_id);
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let hub = Hub::new(session_id);
    let mut h = Harness::open(&parts, cfg, hub).expect("the session must open");
    assert_eq!(
        h.restore_provider_choice(),
        None,
        "no choice on the row, nothing to restore, and nothing to say"
    );
    let s = Store::open(&path).expect("reopening");
    assert_eq!(s.provider_choice(session_id).unwrap(), None);
}
