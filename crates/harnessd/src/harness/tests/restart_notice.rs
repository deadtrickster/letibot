//! A restart is said even when there is nothing to restore, and it starts a turn when the session still owes something.
//! Idle and owing nothing gets a row and no turn, idle and owing work gets a notice the ring spends a turn on, and a
//! turn already running hears it at its step boundary.

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
    let row = letibot_tokencore::ledger::LedgerRow {
        item_id: format!("{transcript}.0"),
        tok_offset: prefix_tokens.len() as u32,
        tok_len: tokens.len() as u32,
        h_k: letibot_tokencore::ledger::chain(&h_init, &tokens),
    };
    store
        .append_item(&transcript, 0, &item, &row, &tokens)
        .expect("the row is appended");
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
    h.say_without_a_turn(text).expect("the sentence is filed");
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
        text.contains("2 todo rows still owed"),
        "the two rows that are owed, and not the three that are not: {text}"
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
    let second = again
        .restore_after_restart()
        .expect("the next daemon says it");
    assert!(
        again.restart_notices.is_empty(),
        "the row it filed is not work it owes: {:?}",
        again.restart_notices
    );
    assert!(
        second.ends_with(". Nothing was running."),
        "no clause, because nothing is owed: {second}"
    );
}

/// **A session active just before the stop is woken though it owes nothing recorded** - the model that told the
/// operator "after you restart I will verify X" has to run again without being typed to. The notice says what to do.
#[test]
fn a_recently_active_session_owing_nothing_gets_a_wake_turn() {
    let dir = Scratch::new("restart-recent");
    let id = "s-restart-recent";
    let Some(h) = opened_by_a_later_daemon(&dir, id) else {
        return;
    };
    stage_a_last_row(
        &dir,
        id,
        TranscriptItem::Assistant {
            text: "the build is done".into(),
            tool_calls: Vec::new(),
            truncated: false,
        },
    );
    age_session(
        &Store::open(&dir.store()).unwrap(),
        id,
        STOPPED_AGO_MS + 600_000,
    );
    drop(h);
    let mut h = reopened(&dir, id);
    let text = h.restore_after_restart().expect("a restart is said");
    assert!(text.contains("The daemon restarted (down "), "{text}");
    assert!(text.contains("continue now"), "{text}");
    assert!(text.contains("end this turn with no reply"), "{text}");
    assert_eq!(h.restart_notices, vec![text], "{:?}", h.restart_notices);
    assert!(restart_rows(&h).is_empty(), "{:?}", h.items());
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
    assert_eq!(h.restart_notices, vec![text], "{:?}", h.restart_notices);
}

/// A session idle beyond the window with nothing promised keeps today's row and no turn.
#[test]
fn a_session_idle_beyond_the_window_gets_only_the_row() {
    let dir = Scratch::new("restart-idle");
    let Some(mut h) = opened_by_a_later_daemon(&dir, "s-restart-idle") else {
        return;
    };
    let text = h.restore_after_restart().expect("a restart is said");
    assert!(!text.contains("continue now"), "{text}");
    assert!(h.restart_notices.is_empty(), "{:?}", h.restart_notices);
    assert_eq!(restart_rows(&h), vec![text], "{:?}", h.items());
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
    h.say_without_a_turn("an earlier row").unwrap();
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
