//! **A restart is said even when there is nothing to restore, and it is never a turn** — the
//! operator, 2026-10-30: *"mb always tell agent about restart? with the current time lol"*.
//!
//! The condition `restore_after_restart` was written under was *what was running*: a session with
//! no subagent, monitor or job had nothing restored and nothing reminded, so it was told nothing
//! and the model went on believing no time had passed. What closes that gap is the time, and the
//! daemon is the only thing that knows it — so the sentence is said on every restart that reopens
//! a session, and this is where its two doors are pinned:
//!
//! * **idle** → a transcript row, and no turn of its own (a wake is a model call);
//! * **a turn already running** → the step boundary of that turn, in the same turn.
//!
//! The restore case keeps the door it has always had, and is pinned by `restart_restore.rs`.

use super::*;
use letibot_tokencore::store::{SessionRecord, Store};

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

    fn store(&self) -> std::path::PathBuf {
        self.0.join("sessions.db")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// **A session an earlier daemon had already created** — the row is written first, the vocabulary
/// is loaded, and only then does `Harness::open` mint this daemon's registry: so the row's
/// `created_at` is behind `Registry::started_ms`, which is the whole of what *a restart happened
/// for this session* means when the session left no other record.
///
/// `None` when this box has no vocabulary GGUF, the way every harness test here answers that.
fn opened_by_a_later_daemon(dir: &Scratch, id: &str) -> Option<Harness> {
    let gguf = crate::vocab_gguf()?;
    let path = dir.store();
    let store = Store::open(&path).expect("a store");
    store
        .put_session(&SessionRecord {
            id: id.into(),
            title: None,
            model_id: "a model".into(),
            dialect_sha: "a dialect".into(),
            workspace_root: dir.0.display().to_string(),
            owner: "dead".into(),
            approvers: Vec::new(),
            role: None,
            parent_session_id: None,
        })
        .expect("the earlier daemon's row");
    let mut cfg = Config::for_this_box(&dir.0);
    cfg.dialect = crate::Dialect::Qwen;
    cfg.store = Some(path);
    cfg.session_id = id.into();
    cfg.vocab_gguf = Some(gguf);
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    Some(Harness::open(&parts, cfg, Hub::new(id)).expect("the session opens"))
}

/// Every `[restart] …` row the session's transcript holds.
fn restart_rows(h: &Harness) -> Vec<String> {
    h.items()
        .iter()
        .filter_map(|i| match i {
            TranscriptItem::User { parts, .. } => {
                let text = parts
                    .iter()
                    .filter_map(|p| match p {
                        UserPart::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                text.starts_with("[restart] ").then_some(text)
            }
            _ => None,
        })
        .collect()
}

/// **A restart with nothing running is said, once, as a row — and it starts no turn.**
///
/// The three halves are one decision: the sentence is in the transcript (so the model reads it at
/// its next turn), `restart_notices` is empty (so the daemon's own ring at open finds nothing to
/// say and runs no turn — a wake is a model call), and the second open of the same session says
/// nothing at all (the one-shot key is per (session, restart)).
#[test]
fn an_empty_restart_is_one_row_and_never_a_turn() {
    let dir = Scratch::new("restart-empty");
    let Some(mut h) = opened_by_a_later_daemon(&dir, "s-restart-empty") else {
        return;
    };
    let text = h
        .restore_after_restart()
        .expect("a restart with nothing running is still said");
    assert_eq!(
        text,
        crate::restart::nothing_running(
            &crate::restart::Stopped::Unknown { seen_ms: 0 },
            h.session_registry.started_ms()
        ),
        "a session that names no daemon says the stop is unknown rather than inventing one"
    );
    assert!(text.contains("the daemon came back at "), "{text}");
    assert!(text.ends_with(": nothing was running"), "{text}");
    // **The model has it**, as a row of the transcript.
    assert_eq!(restart_rows(&h), vec![text.clone()], "{:?}", h.items());
    // **And nothing is waiting for a turn to be spent on it.**
    assert!(
        h.restart_notices.is_empty(),
        "a notice left for a wake is a turn: {:?}",
        h.restart_notices
    );
    assert!(
        h.injected.lock().unwrap().is_empty(),
        "nothing was injected: a boundary would only be taken by a turn"
    );
    // **Once per (session, restart)** — the key names the daemon.
    assert!(
        h.restore_after_restart().is_none(),
        "the same daemon said it twice"
    );
    assert_eq!(restart_rows(&h).len(), 1, "{:?}", h.items());
    let key = format!("restart:s-restart-empty:{}", h.session_registry.instance());
    let store = Store::open(&dir.store()).expect("the store");
    assert_eq!(
        store.told(&key).expect("the key").map(|t| t.count),
        Some(1),
        "the one-shot is a row the store holds"
    );
}

/// **A session the daemon itself created is not a restart, and is told nothing** — a head
/// attaching to a session this daemon started, a `/resume` of it, and a second open: there is no
/// stop behind any of them.
#[test]
fn a_session_this_daemon_created_is_never_told_of_a_restart() {
    let dir = Scratch::new("restart-not-mine");
    let gguf = match crate::vocab_gguf() {
        Some(g) => g,
        None => return,
    };
    let mut cfg = Config::for_this_box(&dir.0);
    cfg.dialect = crate::Dialect::Qwen;
    cfg.store = Some(dir.store());
    cfg.session_id = "s-fresh".into();
    cfg.vocab_gguf = Some(gguf);
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    let mut h = Harness::open(&parts, cfg, Hub::new("s-fresh")).expect("the session opens");
    assert!(h.restore_after_restart().is_none(), "a fresh session");
    assert!(restart_rows(&h).is_empty(), "{:?}", h.items());
    assert!(h.restore_after_restart().is_none(), "and again");
    let store = Store::open(&dir.store()).expect("the store");
    assert!(
        store
            .told(&format!(
                "restart:s-fresh:{}",
                h.session_registry.instance()
            ))
            .expect("the table is readable")
            .is_none(),
        "a session that was never lost spends no key"
    );
}

/// **A turn already running reads the sentence at its next step boundary** — filed, not spoken,
/// and not a second turn opened under the one in flight.
#[test]
fn a_session_mid_turn_reads_the_line_at_its_next_round_boundary() {
    let dir = Scratch::new("restart-mid-turn");
    let Some(mut h) = opened_by_a_later_daemon(&dir, "s-restart-mid-turn") else {
        return;
    };
    // The one state a single-threaded test cannot reach by running a turn: a turn holds the
    // harness for its whole life, so nothing else can call in while one runs.
    h.hold_a_turn_open_for_test(true);
    let text = "[restart] the daemon came back at 2026-10-12 12:00:01 UTC (down 1s, stopped at \
                2026-10-12 12:00:00 UTC): nothing was running";
    h.say_without_a_turn(text).expect("the sentence is filed");
    // **Not a row yet**: it waits for the boundary of the turn that is already running.
    assert!(restart_rows(&h).is_empty(), "{:?}", h.items());
    // **And the boundary hands it to the engine**, as context in that turn.
    let said = h.steering().try_next().expect("the step boundary has it");
    assert!(said.text.contains(text), "{}", said.text);
    // Nothing is left over for a wake to turn into a turn of its own.
    assert!(h.restart_notices.is_empty(), "{:?}", h.restart_notices);
    h.hold_a_turn_open_for_test(false);
}
