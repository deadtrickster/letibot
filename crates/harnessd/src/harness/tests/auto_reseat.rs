//! A session no person answers moves onto the current prompt before its turn; the operator's own
//! session keeps the card. Byte vocabulary, temp stores, a provider that answers without a server.

use super::*;

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

#[derive(Clone, Default)]
struct Asked(Arc<std::sync::atomic::AtomicUsize>);

struct Answers(Asked);

impl letibot_backend::MessagesBackend for Answers {
    fn caps(&self) -> letibot_backend::BackendCaps {
        letibot_backend::BackendCaps::METERED_API
    }
    fn name(&self) -> &str {
        "deepseek"
    }
    fn model(&self) -> &str {
        "not-called"
    }
    fn authority(&self) -> String {
        "a fake provider".into()
    }
    fn complete(
        &self,
        _req: &letibot_backend::TurnRequest<'_>,
        _on_delta: &mut dyn FnMut(&letibot_backend::Delta) -> letibot_backend::StreamFlow,
    ) -> Result<letibot_backend::Completion, letibot_backend::BackendError> {
        self.0.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(letibot_backend::Completion {
            text: "done".into(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish: letibot_backend::Finish::Stop,
            cost: letibot_backend::TurnCost {
                meter: letibot_backend::Meter::Metered,
                prompt_tokens: 10,
                cached_tokens: 0,
                generated_tokens: 1,
                wall_ms: 1,
                micros_usd: None,
            },
            raw_usage: None,
            thoughts: Vec::new(),
        })
    }
}

/// The review queue's own session id for some workspace key.
const QUEUE: &str = "s-gatekeeper-0123456789ab";

/// A session on `dir`'s store, opened with `system` as the base prompt when given.
fn open(dir: &Scratch, id: &str, system: Option<&str>) -> Harness {
    let mut cfg = Config::for_this_box(&dir.0);
    cfg.dialect = crate::Dialect::Qwen;
    cfg.store = Some(dir.0.join("sessions.db"));
    cfg.session_id = id.into();
    cfg.vocab_gguf = None;
    if let Some(system) = system {
        cfg.system = system.into();
    }
    let parts = Parts::load(&cfg).expect("the byte vocabulary loads");
    *parts.mode_store.write().unwrap() = crate::modes::ModeStore::default();
    // A registry that knows the session, so the card it is handed can be read back.
    let hub = Hub::new(id);
    let registry = letibot_sessionlog::registry::Registry::of(hub.clone());
    let mut h = Harness::open_with_registry(&parts, cfg, hub, None, None, registry)
        .expect("the session opens");
    h.provider = Some(Box::new(Answers(Asked::default())));
    h
}

/// A session whose stored prompt an older build composed: one turn under `old`, then reopened
/// on this build's default prompt.
fn stale_session(dir: &Scratch, id: &str) -> Harness {
    let mut h = open(
        dir,
        id,
        Some("You are an assistant an older build described."),
    );
    h.submit("first").expect("the first turn runs");
    drop(h);
    open(dir, id, None)
}

fn reseat_rows(h: &Harness) -> Vec<String> {
    h.items()
        .iter()
        .filter_map(|i| match i {
            TranscriptItem::User { parts, .. } => parts.iter().find_map(|p| match p {
                UserPart::Text { text } if text.starts_with("[reseat] ") => Some(text.clone()),
                _ => None,
            }),
            _ => None,
        })
        .collect()
}

fn warned(h: &Harness, code: &str) -> Vec<String> {
    h.hub
        .retained()
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
fn every_seat_states_what_a_stale_prompt_does() {
    use crate::config::{OnStale, Seat};
    for seat in Seat::ALL {
        let want = match seat {
            Seat::Gatekeeper | Seat::NotesKeeper => OnStale::Reseat,
            Seat::Shadow => OnStale::Exempt,
            _ => OnStale::Ask,
        };
        assert_eq!(seat.on_stale(), want, "{}", seat.as_str());
        assert_eq!(
            Seat::parse(seat.as_str()),
            Ok(seat),
            "ALL lists a real seat"
        );
    }
}

#[test]
fn the_queue_session_on_a_stale_prompt_reseats_before_its_turn() {
    let dir = Scratch::new("auto-reseat-queue");
    let mut h = stale_session(&dir, QUEUE);
    assert_eq!(h.config().on_stale(), crate::config::OnStale::Reseat);
    assert!(
        h.session_registry.stale_for_model(QUEUE).is_none(),
        "a session that reseats is not told the update is the person's"
    );
    h.submit("review this").expect("the turn runs");
    assert_eq!(reseat_rows(&h).len(), 1, "{:#?}", h.items());
    let said = warned(&h, "reseated");
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(
        said[0].starts_with("reseated: the system instructions changed"),
        "{said:?}"
    );
    // The model's line opens the turn, before the words that started it.
    let last_user: Vec<String> = h
        .items()
        .iter()
        .filter_map(|i| match i {
            TranscriptItem::User { parts, .. } => parts.iter().find_map(|p| match p {
                UserPart::Text { text } => Some(text.clone()),
                _ => None,
            }),
            _ => None,
        })
        .collect();
    let n = last_user.len();
    assert!(last_user[n - 2].starts_with("[reseat] ") && last_user[n - 1] == "review this");
    // The first turn's row came across with the conversation.
    assert!(last_user.iter().any(|t| t == "first"), "{last_user:?}");

    h.submit("again").expect("the next turn runs");
    assert_eq!(
        reseat_rows(&h).len(),
        1,
        "a current prompt is not reseated again"
    );
}

#[test]
fn an_operator_session_keeps_the_card_and_its_prompt() {
    let dir = Scratch::new("auto-reseat-operator");
    let id = "s-operator-stale";
    let mut h = stale_session(&dir, id);
    assert_eq!(h.config().on_stale(), crate::config::OnStale::Ask);
    assert!(
        h.session_registry.stale_prefix(id).is_some(),
        "the operator is shown the card"
    );
    h.submit("carry on").expect("the turn runs");
    assert!(reseat_rows(&h).is_empty(), "{:#?}", h.items());
    assert!(warned(&h, "reseated").is_empty());
    assert!(h.session_registry.stale_prefix(id).is_some());
}

#[test]
fn a_prompt_that_moves_while_the_session_runs_is_caught_at_the_next_turn() {
    let dir = Scratch::new("auto-reseat-live");
    let mut h = open(&dir, QUEUE, None);
    h.submit("one").expect("the first turn runs");
    assert!(reseat_rows(&h).is_empty(), "a fresh session is current");
    h.cfg.system = format!("{}\nA rule read live.", h.cfg.system);
    h.submit("two").expect("the second turn runs");
    assert_eq!(reseat_rows(&h).len(), 1, "{:#?}", h.items());
    assert!(h.prefix.system.contains("A rule read live."));
}

#[test]
fn the_switch_turns_the_reseat_back_into_the_card() {
    let dir = Scratch::new("auto-reseat-off");
    let mut h = open(&dir, QUEUE, None);
    h.cfg.leticode.sessions.auto_reseat = Some(false);
    assert_eq!(h.config().on_stale(), crate::config::OnStale::Ask);
    h.cfg.system = format!("{}\nA rule read live.", h.cfg.system);
    h.submit("one").expect("the turn runs");
    assert!(reseat_rows(&h).is_empty());
}

#[test]
fn a_refused_reseat_runs_on_the_old_prompt_and_is_said_once() {
    let dir = Scratch::new("auto-reseat-refused");
    let mut h = open(&dir, QUEUE, None);
    h.submit("one").expect("the first turn runs");
    // The new prompt cannot be recorded: its table is moved away under the harness.
    let other = Store::open_private(&dir.0.join("sessions.db")).expect("a connection of its own");
    other
        .connection_of("stable_prefix")
        .execute_batch("ALTER TABLE stable_prefix RENAME TO stable_prefix_away")
        .expect("the table moves away");
    h.cfg.system = format!("{}\nA rule read live.", h.cfg.system);
    let old = h.prefix.clone();
    assert!(
        h.auto_reseat_before_turn().is_none(),
        "nothing to open with"
    );
    assert!(h.auto_reseat_before_turn().is_none(), "and no retry");
    // The table is back, so a turn can be stored; the same target is still not retried.
    other
        .connection_of("stable_prefix")
        .execute_batch("ALTER TABLE stable_prefix_away RENAME TO stable_prefix")
        .expect("the table comes back");
    h.submit("two").expect("the turn runs on the old prompt");
    assert!(reseat_rows(&h).is_empty());
    assert_eq!(h.prefix, old);
    assert_eq!(warned(&h, "auto_reseat_failed").len(), 1);
}
