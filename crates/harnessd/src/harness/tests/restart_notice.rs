//! A restart is said even when there is nothing to restore, and it starts a turn only when the
//! session owes something. Idle and owing nothing gets a row and no turn - being active before the
//! stop is not owed work - and owed work gets one turn whose sentence names it, ends in one
//! instruction, and defines the empty reply that ends it without a row to read.

use super::*;
use letibot_tokencore::store::{SessionRecord, Store, TodoBy, TodoItem, TodoStatus};
use letibot_transcript::{ToolCall, TranscriptItem};

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

/// How long before the test the earlier daemon stopped.
const STOPPED_AGO_MS: u64 = 120_000;

/// Date everything the session holds `ago_ms` before now: its row and every transcript row.
fn age_session(store: &Store, id: &str, ago_ms: u64) {
    let at = (letibot_sessionlog::event::now_ms() - ago_ms) as i64;
    let conn = store.connection();
    conn.execute("UPDATE session SET created_at = ?2 WHERE id = ?1", (id, at))
        .expect("the session is dated");
    // Rows are append-only in SQL; the next open recreates the trigger.
    conn.execute("DROP TRIGGER IF EXISTS transcript_item_no_update", ())
        .expect("the trigger is lifted");
    conn.execute(
        "UPDATE transcript_item SET created_at = ?2 WHERE transcript_id IN \
         (SELECT id FROM transcript WHERE session_id = ?1)",
        (id, at),
    )
    .expect("its rows are dated");
}

/// A session an earlier daemon had already created: the row is written first and the vocabulary loaded, and only then
/// does `Harness::open` mint this daemon's registry, so the row's `created_at` is behind `Registry::started_ms`. `None`
/// when this box has no vocabulary GGUF.
fn opened_by_a_later_daemon(dir: &Scratch, id: &str) -> Option<Harness> {
    opened_by_a_later_daemon_with(dir, id, |_| {})
}

/// The same, with rows written into the earlier daemon's store before this one opens it - the
/// board and the transcript are read at `Harness::open`, so a fixture that seeds them afterwards
/// seeds a harness that never saw them.
fn opened_by_a_later_daemon_with(
    dir: &Scratch,
    id: &str,
    seed: impl FnOnce(&Store),
) -> Option<Harness> {
    let gguf = crate::vocab_gguf()?;
    let path = dir.store();
    let store = Store::open(&path).expect("a store");
    // A daemon has to have stopped here. The guard is a fact about the box rather than an
    // inference from a session's creation time (a seeded session is older than this daemon while
    // nothing restarted), so a fixture that wants the restart said must leave a stopped life.
    let now = letibot_sessionlog::event::now_ms();
    store
        .life_started("d-the-earlier-one", now - 3_600_000)
        .expect("the earlier daemon began");
    store
        .life_stopped("d-the-earlier-one", now - STOPPED_AGO_MS)
        .expect("the earlier daemon stopped");
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
    seed(&store);
    // Idle by default: the quiet cases below are sessions nobody touched for hours before the stop.
    age_session(&store, id, 5 * 3_600_000);
    drop(store);
    let mut cfg = Config::for_this_box(&dir.0);
    cfg.dialect = crate::Dialect::Qwen;
    cfg.store = Some(path);
    cfg.session_id = id.into();
    cfg.vocab_gguf = Some(gguf);
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    Some(Harness::open(&parts, cfg, Hub::new(id)).expect("the session opens"))
}

/// One row of the board, as the model writes it.
fn a_row(what: &str, status: TodoStatus) -> TodoItem {
    TodoItem {
        content: what.into(),
        status,
        by: TodoBy::Model,
        when: None,
        needs: Vec::new(),
    }
}

/// One transcript row, written into the store behind the open harness's back: nothing resumes over this, and it is the
/// store's own last-row fact that `mid_turn` reads, so staging it stages what a daemon that died mid-turn leaves.
fn stage_a_last_row(dir: &Scratch, id: &str, item: TranscriptItem) {
    stage_last_rows(dir, id, &[item]);
}

/// [`stage_a_last_row`] for several rows, in order.
fn stage_last_rows(dir: &Scratch, id: &str, items: &[TranscriptItem]) {
    let store = Store::open(&dir.store()).expect("the store");
    let transcript = store
        .current_transcript_id(id)
        .expect("the store is readable")
        .expect("the open wrote this session's transcript");
    let prefix_id = store
        .transcript_prefix_id(&transcript)
        .expect("readable")
        .expect("a transcript has a prefix");
    let (prefix_tokens, h_init) = store
        .get_stable_prefix(&prefix_id)
        .expect("the prefix is readable");
    let tokens: Vec<u32> = vec![7, 8, 9];
    let mut h = h_init;
    for (seq, item) in items.iter().enumerate() {
        h = letibot_tokencore::ledger::chain(&h, &tokens);
        let row = letibot_tokencore::ledger::LedgerRow {
            item_id: format!("{transcript}.{seq}"),
            tok_offset: (prefix_tokens.len() + seq * tokens.len()) as u32,
            tok_len: tokens.len() as u32,
            h_k: h,
        };
        store
            .append_item(&transcript, seq as u32, item, &row, &tokens)
            .expect("the row is appended");
    }
}

/// The daemon that comes back to that store - a new `Harness` over the same file, which is what
/// a restart is here: the previous one is dropped first, because a session has ONE writer and the
/// store refuses a second open while the first still holds its claim (`crate::writer`).
fn reopened(dir: &Scratch, id: &str) -> Harness {
    let mut cfg = Config::for_this_box(&dir.0);
    cfg.dialect = crate::Dialect::Qwen;
    cfg.store = Some(dir.store());
    cfg.session_id = id.into();
    cfg.vocab_gguf = crate::vocab_gguf();
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    Harness::open(&parts, cfg, Hub::new(id)).expect("the session opens again")
}

// ---------------------------------------------------------------------------
// The door a restart turn takes, and the one it ends on - on the BYTE vocabulary,
// because these tests also run a turn and this box has no GGUF
// ---------------------------------------------------------------------------

/// How many completions the fake model was asked for - the fact that says whether a generation
/// was spent, which is the whole of what a restart's door decides.
#[derive(Clone, Default)]
struct Asked(std::sync::Arc<std::sync::atomic::AtomicUsize>);

impl Asked {
    fn count(&self) -> usize {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// A model that answers `text` to every request, on every round.
struct Answers {
    text: &'static str,
    asked: Asked,
}

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
        self.asked
            .0
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(letibot_backend::Completion {
            text: self.text.into(),
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

/// A config on the BYTE vocabulary, so nothing below needs a GGUF. The provider is injected on the
/// open harness, so nothing here dials a server either.
fn on_bytes(dir: &Scratch, id: &str) -> Config {
    let mut cfg = Config::for_this_box(&dir.0);
    cfg.dialect = crate::Dialect::Qwen;
    cfg.store = Some(dir.store());
    cfg.session_id = id.into();
    cfg.vocab_gguf = None;
    cfg
}

/// The parts, with this box's project modes replaced by none: a resumed session takes its mode from
/// the store, and none of these tests is about this laptop.
fn parts_of(cfg: &Config) -> Parts {
    let parts = Parts::load(cfg).expect("the byte vocabulary loads");
    *parts.mode_store.write().unwrap() = crate::modes::ModeStore::default();
    parts
}

/// The store an earlier daemon left behind - a stopped life, the session's own row, whatever `seed`
/// puts on the board, every row dated `ago_ms` before now - and this daemon opening it with a model
/// that answers `text`.
fn a_restart_with(
    dir: &Scratch,
    id: &str,
    ago_ms: u64,
    seed: impl FnOnce(&Store),
    text: &'static str,
) -> (Harness, Asked) {
    {
        let store = Store::open(&dir.store()).expect("a store");
        let now = letibot_sessionlog::event::now_ms();
        store
            .life_started("d-the-earlier-one", now - 3_600_000)
            .expect("the earlier daemon began");
        store
            .life_stopped("d-the-earlier-one", now - STOPPED_AGO_MS)
            .expect("the earlier daemon stopped");
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
        seed(&store);
        age_session(&store, id, ago_ms);
    }
    let cfg = on_bytes(dir, id);
    let parts = parts_of(&cfg);
    let mut h = Harness::open(&parts, cfg, Hub::new(id)).expect("the session opens again");
    let asked = Asked::default();
    h.provider = Some(Box::new(Answers {
        text,
        asked: asked.clone(),
    }));
    (h, asked)
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

/// A restart with nothing running is said, once, as a row, and it starts no turn: the sentence is in the transcript so
/// the model reads it at its next turn, `restart_notices` is empty so the ring finds nothing to say, and the second open
/// of the same session says nothing at all.
#[test]
fn an_empty_restart_is_one_row_and_never_a_turn() {
    let dir = Scratch::new("restart-empty");
    let Some(mut h) = opened_by_a_later_daemon(&dir, "s-restart-empty") else {
        return;
    };
    let text = h
        .restore_after_restart()
        .expect("a restart with nothing running is still said");
    // The fixture stages a stopped life row, so `latest_box_stop` finds it: the stop is dated from that row even though
    // the session's own rows name no daemon, since the down time is a fact about the box and every stopped life counts.
    assert!(
        text.starts_with("[restart] The daemon restarted (down "),
        "the stop is dated from a stopped life row, not invented: {text}"
    );
    assert!(text.ends_with(". Nothing was running."), "{text}");
    // The model has it, as a row of the transcript.
    assert_eq!(restart_rows(&h), vec![text.clone()], "{:?}", h.items());
    // And nothing is waiting for a turn to be spent on it.
    assert!(
        h.restart_notices.is_empty(),
        "a notice left for a wake is a turn: {:?}",
        h.restart_notices
    );
    assert!(
        h.injected.lock().unwrap().is_empty(),
        "nothing was injected: a boundary would only be taken by a turn"
    );
    // And the wake the daemon rings at open finds nothing to say, so it spends no generation.
    assert!(
        h.wake().expect("the wake runs").is_none(),
        "a restart owing nothing started a turn"
    );
    // Once per (session, restart) - the key names the daemon.
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

/// A session the daemon itself created is not a restart, and is told nothing - a head
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

/// A turn already running reads the sentence at its next step boundary - filed, not spoken,
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
    let text = "[restart] The daemon restarted (down 1s). Nothing was running.";
    h.say_without_a_turn(&letibot_plugin::notice::Facts::Restart { lines: 1 }, text)
        .expect("the sentence is filed");
    // Not a row yet: it waits for the boundary of the turn that is already running.
    assert!(restart_rows(&h).is_empty(), "{:?}", h.items());
    // And the boundary hands it to the engine, as context in that turn.
    let said = h.steering().try_next().expect("the step boundary has it");
    assert!(said.text.contains(text), "{}", said.text);
    // Nothing is left over for a wake to turn into a turn of its own.
    assert!(h.restart_notices.is_empty(), "{:?}", h.restart_notices);
    h.hold_a_turn_open_for_test(false);
}

// ---------------------------------------------------------------------------
// What the session still OWES - the door the operator's 2026-11-01 ruling turns on
// ---------------------------------------------------------------------------

/// A session whose plan still has rows owed is WOKEN, and the sentence says why: the operator expected the work to
/// continue after a restart, and a plan is the durable half of that promise, so the row it left is why the sentence is
/// a turn rather than a row nobody reads.
#[test]
fn a_plan_with_rows_owed_makes_the_restart_a_turn() {
    let dir = Scratch::new("restart-owed-plan");
    let id = "s-restart-owed-plan";
    let Some(mut h) = opened_by_a_later_daemon_with(&dir, id, |store| {
        store
            .put_todos(
                id,
                &[
                    a_row("finish the migration and report", TodoStatus::Pending),
                    a_row("then update the notes", TodoStatus::InProgress),
                    // The operator's own acts: neither is work the session owes.
                    a_row("later, maybe the changelog", TodoStatus::Postponed),
                    a_row("struck off", TodoStatus::Cancelled),
                    a_row("already answered", TodoStatus::Completed),
                ],
            )
            .expect("the earlier daemon's board");
    }) else {
        return;
    };
    let text = h.restore_after_restart().expect("a restart is said");
    assert!(
        text.starts_with("[restart] The daemon restarted (down "),
        "{text}"
    );
    assert!(
        text.contains(
            "2 todo rows still owed: finish the migration and report; then update the \
                      notes"
        ),
        "the two rows that are owed, by their own words, and not the three that are not: {text}"
    );
    assert!(
        text.contains("Carry on with that work now") && text.contains("and report it."),
        "one instruction: {text}"
    );
    assert!(
        !text.contains("If you were") && !text.contains("If nothing was waiting"),
        "no conditional the model has to resolve: {text}"
    );
    assert!(
        text.contains(crate::restart::QUIET_END),
        "the quiet end is defined in the sentence: {text}"
    );
    // A turn is owed, and the ring at open is what spends it - `restart_notices` is the door
    // `Harness::wake` drains, and a non-empty list is a turn.
    assert_eq!(
        h.restart_notices,
        vec![text.clone()],
        "a session that owes work keeps the turn: {:?}",
        h.restart_notices
    );
    // And it is NOT also filed as a row - one restart, one notification, and this one is the
    // turn's opening line.
    assert!(restart_rows(&h).is_empty(), "{:?}", h.items());
    // Once per (session, restart): a second open of the same session by the same daemon says
    // nothing at all, and does not queue a second turn.
    assert!(h.restore_after_restart().is_none(), "said twice");
    assert_eq!(h.restart_notices.len(), 1, "{:?}", h.restart_notices);
}

/// A session that was mid-turn when the rows stopped is woken - the arm the operator's own
/// case is: a model that restarts the daemon from inside a tool call leaves its call and no
/// result, and nothing else in the store says a turn was going.
#[test]
fn a_turn_cut_off_mid_work_makes_the_restart_a_turn() {
    let dir = Scratch::new("restart-owed-midturn");
    let id = "s-restart-owed-midturn";
    let Some(h) = opened_by_a_later_daemon(&dir, id) else {
        return;
    };
    // The shape a daemon that died during a tool call leaves: the call was issued, its result was
    // never written.
    stage_a_last_row(
        &dir,
        id,
        TranscriptItem::Assistant {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: "call_restart".into(),
                name: "bash".into(),
                arguments: r#"{"command":"letibot --stop"}"#.into(),
            }],
            truncated: false,
        },
    );
    drop(h);
    let mut h = reopened(&dir, id);
    let text = h.restore_after_restart().expect("a restart is said");
    assert!(
        text.contains("a turn was in flight when the rows stopped"),
        "{text}"
    );
    assert_eq!(
        h.restart_notices,
        vec![text.clone()],
        "{:?}",
        h.restart_notices
    );
    assert!(restart_rows(&h).is_empty(), "{:?}", h.items());
}

/// A prompt nobody answered is the same arm - the person typed, the daemon stopped before the
/// answer, and the session is woken to carry on rather than told into a row they must type again
/// to see read.
#[test]
fn a_prompt_nobody_answered_makes_the_restart_a_turn() {
    let dir = Scratch::new("restart-owed-prompt");
    let id = "s-restart-owed-prompt";
    let Some(h) = opened_by_a_later_daemon(&dir, id) else {
        return;
    };
    stage_a_last_row(
        &dir,
        id,
        TranscriptItem::User {
            speaker: letibot_transcript::Speaker::Operator,
            parts: vec![UserPart::Text {
                text: "carry on with the second half".into(),
            }],
        },
    );
    drop(h);
    let mut h = reopened(&dir, id);
    let text = h.restore_after_restart().expect("a restart is said");
    assert!(text.contains("a turn was in flight"), "{text}");
    assert_eq!(
        h.restart_notices,
        vec![text.clone()],
        "{:?}",
        h.restart_notices
    );
}

/// A session whose last row is a finished answer owes nothing, and keeps the row - the cost
/// argument still holds where it is true, and this is the session it is true about.
#[test]
fn a_session_that_answered_last_is_told_in_a_row_and_not_woken() {
    let dir = Scratch::new("restart-owed-answered");
    let id = "s-restart-owed-answered";
    let Some(h) = opened_by_a_later_daemon(&dir, id) else {
        return;
    };
    stage_a_last_row(
        &dir,
        id,
        TranscriptItem::Assistant {
            text: "that is the migration done; nothing left on my side".into(),
            tool_calls: Vec::new(),
            truncated: false,
        },
    );
    age_session(&Store::open(&dir.store()).unwrap(), id, 5 * 3_600_000);
    drop(h);
    let mut h = reopened(&dir, id);
    let text = h.restore_after_restart().expect("a restart is said");
    assert!(text.ends_with(". Nothing was running."), "{text}");
    assert!(
        h.restart_notices.is_empty(),
        "a session with nothing owed must not spend a call: {:?}",
        h.restart_notices
    );
    assert_eq!(restart_rows(&h), vec![text], "{:?}", h.items());
}

/// A sentence the harness filed itself is not a turn in flight, which is what keeps an idle session from being woken on
/// every second restart: the empty restart's own row has exactly the shape a wake's notice has, so the store cannot tell
/// them apart and the class is left out.
#[test]
fn the_restart_row_it_filed_is_not_work_it_owes() {
    let dir = Scratch::new("restart-owed-filed");
    let id = "s-restart-owed-filed";
    let Some(mut h) = opened_by_a_later_daemon(&dir, id) else {
        return;
    };
    // The first restart files its sentence - nothing owed, so no turn.
    let first = h.restore_after_restart().expect("a restart is said");
    assert!(h.restart_notices.is_empty(), "{:?}", h.restart_notices);
    assert_eq!(restart_rows(&h), vec![first.clone()], "{:?}", h.items());
    // The next daemon, opening the same session against the row that is now its last.
    drop(h);
    let mut again = reopened(&dir, id);
    // Nothing owed, and the unanswered row already says it: no second row.
    assert_eq!(again.restore_after_restart(), None);
    assert!(
        again.restart_notices.is_empty(),
        "the row it filed is not work it owes: {:?}",
        again.restart_notices
    );
    assert_eq!(restart_rows(&again), vec![first], "{:?}", again.items());
}

/// Restarts with no turn between them leave one bare row, not one per daemon; a turn in between
/// makes the next restart news again.
#[test]
fn restarts_with_no_turn_between_them_leave_one_bare_row() {
    let dir = Scratch::new("restart-stacked");
    let id = "s-restart-stacked";
    let (mut h, _) = a_restart_with(&dir, id, 5 * 3_600_000, |_| {}, "ok");
    let first = h
        .restore_after_restart()
        .expect("the first restart is said");
    let reopen = |h: Harness| {
        drop(h);
        let cfg = on_bytes(&dir, id);
        let parts = parts_of(&cfg);
        Harness::open(&parts, cfg, Hub::new(id)).expect("the session opens again")
    };
    let mut h = reopen(h);
    assert_eq!(h.restore_after_restart(), None, "a second bare row");
    let mut h = reopen(h);
    assert_eq!(h.restore_after_restart(), None, "a third bare row");
    assert_eq!(restart_rows(&h), vec![first.clone()], "{:?}", h.items());
    // A turn answers it, so the next restart is said again.
    h.provider = Some(Box::new(Answers {
        text: "noted",
        asked: Asked::default(),
    }));
    h.submit("carry on").expect("a turn runs");
    let mut h = reopen(h);
    let next = h.restore_after_restart().expect("a restart after a turn");
    assert!(next.ends_with(". Nothing was running."), "{next}");
    assert_eq!(restart_rows(&h).len(), 2, "{:?}", h.items());
}

/// **A session active just before the stop is NOT woken on that alone.** It owes nothing a turn
/// could continue, and a generation spent reading a note to conclude there is nothing to do is
/// exactly what the operator called out: the note is a row, and the session stays asleep until
/// somebody types to it.
#[test]
fn a_recently_active_session_owing_nothing_gets_the_row_and_no_turn() {
    let dir = Scratch::new("restart-recent");
    let id = "s-restart-recent";
    let (mut h, asked) = a_restart_with(
        &dir,
        id,
        STOPPED_AGO_MS + 600_000,
        |_| {},
        "nothing should have been asked of me",
    );
    let text = h.restore_after_restart().expect("a restart is said");
    assert!(text.contains("The daemon restarted (down "), "{text}");
    assert!(
        text.ends_with(". Nothing was running."),
        "nothing was owed, so there is nothing to add: {text}"
    );
    assert!(h.restart_notices.is_empty(), "{:?}", h.restart_notices);
    assert_eq!(restart_rows(&h), vec![text], "{:?}", h.items());
    // The test's whole point: the wake the daemon rings at open finds nothing to say.
    assert!(h.wake().expect("the wake runs").is_none(), "a turn ran");
    assert_eq!(asked.count(), 0, "a generation was spent on nothing");
}

/// **A session that owes work spends ONE turn, and the sentence it opens with names the work by
/// the row's own words.** A model reading *2 todo rows* has to go and look; a model reading what it
/// wrote on the board does not, and the turn after it has nothing left to say.
#[test]
fn a_restart_that_owes_work_spends_one_turn_that_names_it() {
    let dir = Scratch::new("restart-turn-names");
    let id = "s-restart-turn-names";
    let (mut h, asked) = a_restart_with(
        &dir,
        id,
        5 * 3_600_000,
        |store| {
            store
                .put_todos(
                    id,
                    &[a_row(
                        "finish the migration and report",
                        TodoStatus::Pending,
                    )],
                )
                .expect("the earlier daemon's board");
        },
        "Done: the migration is applied and the notes are updated.",
    );
    let text = h.restore_after_restart().expect("a restart is said");
    assert!(
        text.contains("a todo row still owed: finish the migration and report"),
        "the row is named: {text}"
    );
    let reply = h
        .wake()
        .expect("the wake runs")
        .expect("the owed work bought a turn");
    assert!(!reply.text.trim().is_empty(), "the model answered");
    assert_eq!(asked.count(), 1, "one model call, for one turn");
    // The turn opened with the restart's own sentence, so the model read what it owes.
    assert_eq!(restart_rows(&h), vec![text], "{:?}", h.items());
    assert!(h.restart_notices.is_empty(), "{:?}", h.restart_notices);
    // And the next pass has nothing left: the restart was one turn, not one per wake.
    assert!(h.wake().expect("the wake runs").is_none(), "a second turn");
    assert_eq!(asked.count(), 1, "the restart was said twice");
}

/// **The quiet end leaves nothing to read and nothing to wait on.** A turn that finds the work
/// already done answers with the empty reply the sentence defines: the row it leaves is an
/// assistant row with no words and no calls, which renders no lines, and the session afterwards is
/// idle - no pending delivery, no `waiting on:` line.
#[test]
fn a_quiet_end_leaves_no_drawn_row_and_an_idle_session() {
    let dir = Scratch::new("restart-quiet-end");
    let id = "s-restart-quiet-end";
    let (mut h, asked) = a_restart_with(
        &dir,
        id,
        5 * 3_600_000,
        |store| {
            store
                .put_todos(
                    id,
                    &[a_row(
                        "finish the migration and report",
                        TodoStatus::Pending,
                    )],
                )
                .expect("the earlier daemon's board");
        },
        "",
    );
    let text = h.restore_after_restart().expect("a restart is said");
    assert!(
        text.contains(crate::restart::QUIET_END),
        "the model is told which reply ends it: {text}"
    );
    let reply = h.wake().expect("the wake runs").expect("a turn ran");
    assert_eq!(asked.count(), 1, "one model call");
    assert_eq!(reply.text, "", "the empty reply is the turn's whole answer");
    // Nothing of it is drawn: the turn's own row carries no words and no calls, and a row that
    // renders no lines is not drawn (`crates/tui/src/ui/transcript/window.rs`).
    let spoke = h.items().iter().any(|i| match i {
        TranscriptItem::Assistant {
            text, tool_calls, ..
        } => !text.trim().is_empty() || !tool_calls.is_empty(),
        _ => false,
    });
    assert!(!spoke, "a row was left to read: {:?}", h.items());
    // And the session is idle: the notice was spent, and nothing is queued behind it.
    assert!(h.restart_notices.is_empty(), "{:?}", h.restart_notices);
    assert!(
        !h.hub.work_pending(),
        "a delivery nobody will run is the `waiting on: delivery` line"
    );
    h.publish_activity();
    assert!(
        h.hub.activity().is_quiet(),
        "the head is left with a state to draw: {:?}",
        h.hub.activity()
    );
}

/// **An old session is woken only when its last words wait on the restart**, and one whose last message is
/// unrelated keeps the row (`a_session_that_answered_last_is_told_in_a_row_and_not_woken`).
#[test]
fn an_old_session_that_promised_to_continue_after_a_restart_is_woken() {
    let dir = Scratch::new("restart-promise");
    let id = "s-restart-promise";
    let Some(h) = opened_by_a_later_daemon(&dir, id) else {
        return;
    };
    stage_a_last_row(
        &dir,
        id,
        TranscriptItem::Assistant {
            text: "Patch is in. After you restart the daemon I'll verify the migration landed."
                .into(),
            tool_calls: Vec::new(),
            truncated: false,
        },
    );
    age_session(&Store::open(&dir.store()).unwrap(), id, 5 * 3_600_000);
    drop(h);
    let mut h = reopened(&dir, id);
    let text = h.restore_after_restart().expect("a restart is said");
    assert_eq!(
        h.restart_notices,
        vec![text.clone()],
        "{:?}",
        h.restart_notices
    );
    assert!(
        text.contains("This turn is what you told the operator you would do once they restarted"),
        "the sentence names the promise as the reason for the turn: {text}"
    );
    assert!(
        text.contains("Do that one thing now, and report it.")
            && text.contains(crate::restart::QUIET_END),
        "one instruction and the quiet end: {text}"
    );
}

/// A turn that ended on a question owes nothing, with an offer card after it or not: the card is
/// not a turn in flight and the question is not a promise.
#[test]
fn a_question_with_an_offer_card_after_it_is_not_woken() {
    let dir = Scratch::new("restart-question-offer");
    let id = "s-restart-question-offer";
    let Some(h) = opened_by_a_later_daemon(&dir, id) else {
        return;
    };
    stage_last_rows(
        &dir,
        id,
        &[
            TranscriptItem::Assistant {
                text: "Can you paste the error you saw, or tell me which provider it was? The \
                       model_endpoint_retry warnings we built would have named the provider and \
                       the attempt count if the daemon was running when it happened."
                    .into(),
                tool_calls: Vec::new(),
                truncated: false,
            },
            TranscriptItem::Offer {
                kind: letibot_transcript::OfferKind::Note,
                title: "note offered: provider-retries.md (0.41)".into(),
                body: String::new(),
            },
        ],
    );
    age_session(&Store::open(&dir.store()).unwrap(), id, 5 * 3_600_000);
    drop(h);
    let mut h = reopened(&dir, id);
    let text = h.restore_after_restart().expect("a restart is said");
    assert!(text.ends_with(". Nothing was running."), "{text}");
    assert!(h.restart_notices.is_empty(), "{:?}", h.restart_notices);
}

/// A session idle beyond the window with nothing promised keeps today's row and no turn.
#[test]
fn a_session_idle_beyond_the_window_gets_only_the_row() {
    let dir = Scratch::new("restart-idle");
    let Some(mut h) = opened_by_a_later_daemon(&dir, "s-restart-idle") else {
        return;
    };
    let text = h.restore_after_restart().expect("a restart is said");
    assert!(!text.contains("Carry on with that work now"), "{text}");
    assert!(!text.contains("Do that one thing now"), "{text}");
    assert!(h.restart_notices.is_empty(), "{:?}", h.restart_notices);
    assert_eq!(restart_rows(&h), vec![text], "{:?}", h.items());
    assert!(h.wake().expect("the wake runs").is_none(), "a turn ran");
}

/// A session with history and no store cannot restore anything, and says so in one row naming the
/// restore window.
#[test]
fn a_session_with_no_store_says_restart_restore_is_off() {
    let dir = Scratch::new("restart-nostore");
    let Some(gguf) = crate::vocab_gguf() else {
        letibot_tokencore::apparatus::absent("a vocabulary GGUF");
        return;
    };
    let mut cfg = Config::for_this_box(&dir.0);
    cfg.dialect = crate::Dialect::Qwen;
    cfg.store = None;
    cfg.session_id = "s-nostore".into();
    cfg.vocab_gguf = Some(gguf);
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    let mut h = Harness::open(&parts, cfg, Hub::new("s-nostore")).expect("the session opens");
    assert!(
        h.restore_after_restart().is_none(),
        "a new session has no history a stop could have cut"
    );
    h.say_without_a_turn(
        &letibot_plugin::notice::Facts::Restart { lines: 1 },
        "an earlier row",
    )
    .unwrap();
    let text = h.restore_after_restart().expect("said");
    assert!(text.contains("this session has no store"), "{text}");
    assert!(text.contains("1h"), "names the restore window: {text}");
}

/// Opened past the restore window with nothing running, the row names the window.
#[test]
fn an_empty_restart_past_the_restore_window_names_it() {
    let dir = Scratch::new("restart-past");
    let Some(mut h) = opened_by_a_later_daemon(&dir, "s-restart-past") else {
        letibot_tokencore::apparatus::absent("a vocabulary GGUF");
        return;
    };
    h.cfg.restore_within = std::time::Duration::from_secs(30);
    let text = h.restore_after_restart().expect("said");
    assert!(text.contains("older than the 30s restore window"), "{text}");
}
