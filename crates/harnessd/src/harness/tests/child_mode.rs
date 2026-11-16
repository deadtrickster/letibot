//! A child's point only narrows: every door that moves it is held under the parent's live point,
//! and a parent that narrows narrows its children.

use super::*;
use letibot_tools::mode::Mode;

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

/// A child session at `at`, held under a parent cell at the same point.
fn child_at(
    dir: &Scratch,
    id: &str,
    at: Mode,
) -> Option<(Harness, Arc<std::sync::Mutex<Mode>>, std::path::PathBuf)> {
    child_with(dir, id, at, |_| {})
}

/// [`child_at`], with the config touched before the open.
fn child_with(
    dir: &Scratch,
    id: &str,
    at: Mode,
    touch: impl FnOnce(&mut Config),
) -> Option<(Harness, Arc<std::sync::Mutex<Mode>>, std::path::PathBuf)> {
    let gguf = crate::vocab_gguf()?;
    let path = dir.0.join("sessions.db");
    let mut cfg = Config::for_this_box(&dir.0);
    cfg.dialect = crate::Dialect::Qwen;
    cfg.store = Some(path.clone());
    cfg.session_id = id.into();
    cfg.vocab_gguf = Some(gguf);
    cfg.parent_session_id = Some("s-parent".into());
    cfg.mode = at;
    touch(&mut cfg);
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    let mut h = Harness::open(&parts, cfg, Hub::new(id)).expect("the session opens");
    let parent = Arc::new(std::sync::Mutex::new(at));
    h.follow_parent(parent.clone());
    Some((h, parent, path))
}

fn warnings(hub: &Arc<Hub>, code: &str) -> Vec<String> {
    hub.retained()
        .iter()
        .filter_map(|e| match &e.event {
            SessionEvent::Warning {
                code: c, detail, ..
            } if c == code => Some(detail.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_child_cannot_widen_past_its_parent_by_mode_or_by_the_bridge() {
    let dir = Scratch::new("child-mode-widen");
    let Some((mut h, _parent, path)) = child_at(&dir, "s-parent-sub-1", Mode::WRITES_ALLOWED)
    else {
        return;
    };
    assert_eq!(h.parent_mode(), Some(Mode::WRITES_ALLOWED));

    // `/mode`, taken by the session's own turn.
    let head = h.hub.attach("tui", "dead", Default::default(), 0);
    h.hub.submit(
        &head.head_id,
        "m1",
        0,
        letibot_sessionlog::hub::CommandKind::Mode {
            name: "allow-all".into(),
            consented: true,
        },
    );
    h.apply_queued_mode();
    assert_eq!(
        h.config().mode,
        Mode::WRITES_ALLOWED,
        "the child stayed put"
    );
    let refused = warnings(&h.hub, "mode_set_refused");
    assert!(
        refused
            .iter()
            .any(|w| w.contains("wider than this session's parent")
                && w.contains(&format!("`{}`", Mode::WRITES_ALLOWED.name))),
        "the refusal names the parent's point: {refused:?}"
    );
    let rows = crate::modes::ModeStore::load(&path);
    assert!(
        rows.session("s-parent-sub-1").is_none(),
        "a refused point writes no session row"
    );
    assert_ne!(
        rows.for_project(h.workspace()).name,
        Mode::ALLOW_ALL.name,
        "and no project row"
    );

    // `leticode_mode`, which the bridge sends as a confirmation request.
    let e = h
        .ask_mode_confirm("allow-all", "head-bridge", "bridge", "r1")
        .expect_err("the bridge's door is refused too");
    assert!(e.contains("wider than this session's parent"), "{e}");
    assert_eq!(h.config().mode, Mode::WRITES_ALLOWED);

    // The direct door, and narrowing, which is always allowed.
    assert!(h.set_mode(Mode::ALLOW_ALL).is_err());
    h.set_mode(Mode::READ_ONLY).expect("a child may narrow");
    h.set_mode(Mode::WRITES_ALLOWED)
        .expect("and come back up to its parent's point");
}

#[test]
fn a_parent_that_narrows_narrows_its_child() {
    let dir = Scratch::new("child-mode-narrow");
    let Some((mut h, parent, _path)) = child_at(&dir, "s-parent-sub-2", Mode::WRITES_ALLOWED)
    else {
        return;
    };
    *parent.lock().unwrap() = Mode::READ_ONLY;
    h.narrow_to_parent();
    assert!(
        h.config().mode.no_wider_than(Mode::READ_ONLY),
        "the child follows its parent down, at {}",
        h.config().mode.name
    );
    let said = warnings(&h.hub, "mode_set");
    assert!(
        said.iter()
            .any(|w| w.contains(&format!("the parent moved to `{}`", Mode::READ_ONLY.name))),
        "the child says why it moved: {said:?}"
    );
    // And it cannot go back up past the parent.
    assert!(h.set_mode(Mode::WRITES_ALLOWED).is_err());
}

#[test]
fn child_cannot_move_human_to_model_decider() {
    let dir = Scratch::new("child-mode-decider");
    let Some((mut h, _parent, _path)) =
        child_with(&dir, "s-parent-sub-3", Mode::WRITES_ALLOWED, |cfg| {
            cfg.oracle = Some(letibot_turn::Endpoint::parse("127.0.0.1:9").unwrap());
        })
    else {
        return;
    };
    let e = h
        .set_mode_consented(Mode::AUTO_EDITS, false)
        .expect_err("a person answers the parent's asks, so the child's may not go to a model");
    assert!(e.contains("wider than this session's parent"), "{e}");
    assert!(h.set_mode(Mode::AUTO).is_err());
    assert!(
        h.ask_mode_confirm("automode-edits", "head-bridge", "bridge", "r1")
            .is_err(),
        "nor through the bridge's door"
    );
    assert!(
        h.set_supervision(true).is_err(),
        "nor by turning the guard on"
    );
    assert_eq!(h.config().mode, Mode::WRITES_ALLOWED);
    assert!(!h.supervising());
}

/// A daemon's `Sessions` at `base`, holding one unrelated root, with the store at `path`.
fn daemon_at(
    dir: &Scratch,
    base: Mode,
) -> Option<(
    Parts,
    Config,
    Arc<letibot_sessionlog::registry::Registry>,
    std::path::PathBuf,
)> {
    let gguf = crate::vocab_gguf()?;
    let path = dir.0.join("sessions.db");
    let mut cfg = Config::for_this_box(&dir.0);
    cfg.dialect = crate::Dialect::Qwen;
    cfg.store = Some(path.clone());
    cfg.session_id = "s-first".into();
    cfg.vocab_gguf = Some(gguf);
    cfg.mode = base;
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    let registry = letibot_sessionlog::registry::Registry::new();
    for (id, parent) in [
        ("s-first", None),
        ("s-root", None),
        ("s-root-sub-1", Some("s-root".to_string())),
    ] {
        registry
            .create_under(
                id,
                "",
                letibot_sessionlog::registry::SessionWiring {
                    model: cfg.model.clone(),
                    dialect: "qwen".into(),
                    endpoint: "127.0.0.1:1".into(),
                    workspace: cfg.workspace.display().to_string(),
                },
                parent,
            )
            .expect("the entry a door makes");
    }
    Some((parts, cfg, registry, path))
}

/// R306: after a restart the parent is not open, and the child is reopened by a door that is not
/// the spawn. It comes back at its own narrowed row, held under its parent's stored point.
#[test]
fn a_reopened_child_keeps_its_narrowed_point_and_ceiling() {
    let dir = Scratch::new("child-mode-reopen");
    let Some((parts, cfg, registry, path)) = daemon_at(&dir, Mode::WRITES_ALLOWED) else {
        return;
    };
    let rows = crate::modes::ModeStore::load(&path);
    rows.set_session("s-root", Mode::READ_ONLY, None).unwrap();
    rows.set_session("s-root-sub-1", Mode::READ_ONLY, None)
        .unwrap();
    let mut sessions = crate::sessions::Sessions::open_first(&parts, cfg, registry)
        .expect("the first session opens");
    sessions.open("s-root-sub-1").expect("the child reopens");
    let h = sessions.held("s-root-sub-1").expect("held");
    assert_eq!(
        h.config().mode,
        Mode::READ_ONLY,
        "its own row, not the base"
    );
    assert_eq!(h.parent_mode(), Some(Mode::READ_ONLY), "the parent's row");
    let e = h
        .set_mode(Mode::WRITES_ALLOWED)
        .expect_err("the ceiling is the parent's point");
    assert!(e.contains("wider than this session's parent"), "{e}");
}

/// A child reopened while its parent was closed follows the parent once it opens and narrows.
#[test]
fn a_reopened_child_follows_its_parent_once_the_parent_opens() {
    let dir = Scratch::new("child-mode-follow");
    let Some((parts, cfg, registry, _path)) = daemon_at(&dir, Mode::WRITES_ALLOWED) else {
        return;
    };
    let mut sessions = crate::sessions::Sessions::open_first(&parts, cfg, registry)
        .expect("the first session opens");
    sessions.open("s-root-sub-1").expect("the child reopens");
    assert_eq!(
        sessions.held("s-root-sub-1").unwrap().config().mode,
        Mode::WRITES_ALLOWED
    );
    sessions.open("s-root").expect("the parent opens");
    sessions
        .held("s-root")
        .unwrap()
        .set_mode(Mode::READ_ONLY)
        .expect("a root narrows");
    let h = sessions.held("s-root-sub-1").unwrap();
    h.narrow_to_parent();
    assert!(
        h.config().mode.no_wider_than(Mode::READ_ONLY),
        "the child follows the live parent down, at {}",
        h.config().mode.name
    );
}
