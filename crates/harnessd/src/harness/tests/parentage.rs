//! A session a session started has that session as its parent: the one place the rule is decided,
//! and the doors it has to hold for.
//!
//! The rule lives in `Registry::parent_for` and is applied in `Harness::open_with_registry`, which
//! every open in this crate goes through. These tests are about the open, not about a spawner: the
//! open has to find a parent anyway, from the registry's entry for the id or from the session's own
//! row, so a door that forgot does not leave a child indistinguishable from a top-level session.
//!
//! It matters because the head's `ctrl-s` tree nests by exactly this field, so a child with no
//! parent is drawn at the top of its project for ever, and the only thing left naming the relation
//! is the `-sub-` in a minted id, which is a convention and not a record.
//!
//! The corollary is a test here too: a rule that fills in a parent is one edit away from inventing
//! one, so a session a person started has no parent in any of the three sources and must come back
//! with none.

use super::*;
use crate::sessions::Sessions;
use letibot_sessionlog::registry::SessionWiring;
use letibot_tokencore::store::Store;

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

/// The parts, the config for `id`, and the store's path — or `None` when this box has no
/// vocabulary GGUF (the open renders a stable prefix, so the file is the price of any open).
fn parts_for(dir: &Scratch, id: &str) -> Option<(Parts, Config, std::path::PathBuf)> {
    let gguf = crate::vocab_gguf()?;
    let path = dir.0.join("sessions.db");
    let mut cfg = Config::for_this_box(&dir.0);
    cfg.dialect = crate::Dialect::Qwen;
    // **`leticode` is the seat that names `task`** (and `task_result`/`task_message`), and it is
    // unconfined, so it opens on a Mac as well as here. The default seat names no delegation at
    // all, and a spawn test on it would measure the seat table instead of the parentage.
    cfg.seat = crate::config::Seat::Leticode;
    cfg.store = Some(path.clone());
    cfg.session_id = id.into();
    cfg.vocab_gguf = Some(gguf);
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    Some((parts, cfg, path))
}

/// **Open `cfg.session_id` through a registry that already knows whose it is** — the shape of
/// every door that registers first and opens second (a head's `ResumeSession`, a parked child
/// reopened by its parent, the daemon's `--session`), with the config's own field left empty, as
/// a door that forgot to copy it would.
fn open_registered(
    parts: &Parts,
    mut cfg: Config,
    parent: Option<&str>,
) -> (Harness, Arc<letibot_sessionlog::registry::Registry>) {
    let registry = letibot_sessionlog::registry::Registry::new();
    let hub = registry.new_hub(cfg.session_id.clone());
    registry
        .adopt(
            hub.clone(),
            cfg.session_id.clone(),
            wiring(&cfg),
            parent.map(str::to_string),
        )
        .expect("the session is registered");
    cfg.parent_session_id = None;
    let h = Harness::open_with_registry(parts, cfg, hub, None, None, registry.clone())
        .expect("the session opens");
    (h, registry)
}

fn wiring(cfg: &Config) -> SessionWiring {
    SessionWiring {
        model: cfg.model.clone(),
        dialect: "qwen".into(),
        endpoint: "127.0.0.1:1".into(),
        workspace: cfg.workspace.display().to_string(),
    }
}

/// The session's row, as the store holds it — the fact that outlives the daemon that wrote it.
fn stored_parent(path: &std::path::Path, id: &str) -> Option<String> {
    Store::open(path)
        .expect("the store")
        .session(id)
        .expect("the row reads")
        .expect("the row is there")
        .parent_session_id
}

/// **A child opened by a door that left the config empty is still a child** — the registry's
/// entry is the second source, and it is the one every "register, then open" door has.
///
/// This is the test the rule exists for: before it, the parent was written to the store row from
/// `cfg` alone, so this session's row said `NULL` and the head's tree drew it at the top of its
/// project for ever.
#[test]
fn a_child_a_door_opened_without_its_parent_still_has_one() {
    let dir = Scratch::new("parentage-registered");
    let Some((parts, cfg, path)) = parts_for(&dir, "s-parentage-child") else {
        return;
    };
    let (h, _registry) = open_registered(&parts, cfg, Some("s-parentage-root"));
    assert_eq!(
        h.parent_session(),
        Some("s-parentage-root"),
        "the open settles it from the registry's entry"
    );
    assert_eq!(
        stored_parent(&path, "s-parentage-child").as_deref(),
        Some("s-parentage-root"),
        "and the row the open writes carries it, so the tree is right after a restart too"
    );
}

/// **A session a person started has no parent, and the rule must not invent one.**
///
/// The three sources are all empty here — no caller's word, an entry created with none, and a row
/// this open is about to write — which is exactly the state of a head's `NewSession` and of a
/// fresh launcher start.
#[test]
fn a_session_a_person_started_has_no_parent() {
    let dir = Scratch::new("parentage-person");
    let Some((parts, cfg, path)) = parts_for(&dir, "s-parentage-mine") else {
        return;
    };
    let (h, _registry) = open_registered(&parts, cfg, None);
    assert_eq!(h.parent_session(), None);
    assert_eq!(stored_parent(&path, "s-parentage-mine"), None);
}

/// **A child of a child is parented on the child, not on the root** — the tree is a chain, and a
/// rule that flattened it would put every grandchild directly under the operator's conversation.
#[test]
fn a_child_of_a_child_is_parented_on_the_child() {
    let dir = Scratch::new("parentage-depth");
    let Some((parts, cfg, path)) = parts_for(&dir, "s-depth-sub-1") else {
        return;
    };
    let (h, _registry) = open_registered(&parts, cfg, Some("s-depth"));
    assert_eq!(h.parent_session(), Some("s-depth"));
    assert_eq!(
        stored_parent(&path, "s-depth-sub-1").as_deref(),
        Some("s-depth")
    );

    let Some((parts, cfg, grandchild_path)) = parts_for(&dir, "s-depth-sub-1-sub-2") else {
        return;
    };
    let (g, _registry) = open_registered(&parts, cfg, Some("s-depth-sub-1"));
    assert_eq!(
        g.parent_session(),
        Some("s-depth-sub-1"),
        "the grandchild's parent is the child, and its id says so — but the id is not the reason"
    );
    assert_eq!(
        stored_parent(&grandchild_path, "s-depth-sub-1-sub-2").as_deref(),
        Some("s-depth-sub-1")
    );
}

/// **A session whose entry forgot reads the parent off its own row** — the third source, and the
/// repair for a daemon that registered a child with no parent at all.
///
/// The row is seeded first and the open then finds it (`put_session` writes only a session it has
/// no row for), so the only thing that can answer is the row itself.
#[test]
fn a_daemon_whose_entry_forgot_reads_the_parent_off_the_row() {
    let dir = Scratch::new("parentage-row");
    let Some((parts, cfg, path)) = parts_for(&dir, "s-rowed-sub-1") else {
        return;
    };
    {
        let s = Store::open(&path).expect("the store");
        s.put_session(&letibot_tokencore::store::SessionRecord {
            id: "s-rowed-sub-1".into(),
            title: None,
            model_id: "m".into(),
            dialect_sha: "sha".into(),
            workspace_root: dir.0.display().to_string(),
            owner: "dead".into(),
            role: None,
            approvers: vec![],
            parent_session_id: Some("s-rowed".into()),
        })
        .expect("the row");
    }
    let (h, _registry) = open_registered(&parts, cfg, None);
    assert_eq!(
        h.parent_session(),
        Some("s-rowed"),
        "the row is the fact that survived the daemon that spawned this child"
    );
}

/// **The caller's word is what a spawn relies on, because at the child's open there is nothing
/// else** — the `task`/`task_start` shape, exactly: the id is minted, the config is built with the
/// parent, and the session is adopted only *after* it opens.
#[test]
fn the_callers_word_is_written_when_nothing_else_knows() {
    let dir = Scratch::new("parentage-spawn");
    let Some((parts, mut cfg, path)) = parts_for(&dir, "s-spawn-sub-9") else {
        return;
    };
    cfg.parent_session_id = Some("s-spawn".into());
    let registry = letibot_sessionlog::registry::Registry::new();
    let hub = registry.new_hub(cfg.session_id.clone());
    let h = Harness::open_with_registry(&parts, cfg, hub, None, None, registry)
        .expect("the session opens");
    assert_eq!(h.parent_session(), Some("s-spawn"));
    assert_eq!(
        stored_parent(&path, "s-spawn-sub-9").as_deref(),
        Some("s-spawn"),
        "the row the spawn's child writes is the one the head's tree nests by"
    );
}

/// **The `--session` door, measured** — the daemon's own first session, reopened by id from a
/// launcher.
///
/// `harnessd --session <id>` does not set `Config::parent_session_id` (it is a command line, and
/// the flag that carries a parent is a spawn's, not a person's), so the config arrives empty and
/// the *only* record of the relation is the row the id was born with. Before
/// `Registry::parent_for` was applied at the open, this session was a root to its own harness:
/// no `parent` row in its settings, the root's paragraph in its prompt, and — the one with teeth —
/// no parent id to address, so a child reopened this way could not message its parent at all.
///
/// `Sessions` is driven rather than `Harness` because this is the door: the launcher's flag goes
/// through `Sessions::open_first_with_seat` and nothing else.
#[test]
fn a_session_the_launcher_reopened_with_session_keeps_the_parent_it_was_born_with() {
    let dir = Scratch::new("parentage-session-flag");
    let Some((parts, mut cfg, path)) = parts_for(&dir, "s-flag-sub-1") else {
        return;
    };
    {
        let s = Store::open(&path).expect("the store");
        s.put_session(&letibot_tokencore::store::SessionRecord {
            id: "s-flag-sub-1".into(),
            title: None,
            model_id: "m".into(),
            dialect_sha: "sha".into(),
            workspace_root: dir.0.display().to_string(),
            owner: "dead".into(),
            role: None,
            approvers: vec![],
            parent_session_id: Some("s-flag".into()),
        })
        .expect("the row the daemon reads at startup");
    }
    // The launcher's shape: `--session` sets the id and nothing else.
    cfg.parent_session_id = None;
    let registry = letibot_sessionlog::registry::Registry::new();
    // The store, so the startup door's `resumable` can read the row — exactly what
    // `harnessd`'s own startup does (`StoreSessions::open` + `Registry::set_source`).
    registry.set_source(crate::sessions::StoreSessions::open(&cfg).expect("the store is readable"));
    let on_disk = registry
        .resumable(&cfg.session_id)
        .and_then(|b| b.parent_session_id);
    let parent = registry.parent_for(
        &cfg.session_id,
        cfg.parent_session_id.clone(),
        on_disk.as_deref(),
    );
    registry
        .create_under(cfg.session_id.clone(), "", wiring(&cfg), parent)
        .expect("the entry the startup door makes");
    let mut sessions = Sessions::open_first_with_seat(&parts, cfg, registry.clone(), None)
        .expect("the first session opens");
    let h = sessions.held("s-flag-sub-1").expect("the daemon holds it");
    assert_eq!(
        h.parent_session(),
        Some("s-flag"),
        "a child reopened by id is still a child: the row said so and the open asks the tree"
    );
}
