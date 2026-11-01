//! **A restart is said even when there is nothing to restore** — the operator, 2026-10-30: *"mb
//! always tell agent about restart? with the current time lol"* — **and it starts a turn when the
//! session still owes something** — the operator, 2026-11-01: *"it is not ok that restart notice
//! arrived with my continue … so i expected that after restart you will continue"*.
//!
//! The condition `restore_after_restart` was written under was *what was running*: a session with
//! no subagent, monitor or job had nothing restored and nothing reminded, so it was told nothing
//! and the model went on believing no time had passed. What closes that gap is the time, and the
//! daemon is the only thing that knows it — so the sentence is said on every restart that reopens
//! a session, and this is where its three doors are pinned:
//!
//! * **idle, owing nothing** → a transcript row, and no turn of its own (a wake is a model call);
//! * **idle, owing work** → a restart notice, which the daemon's own ring at open spends a turn on
//!   ([`crate::restart::Owed`]: a turn in flight, a settlement nobody collected, a due row, a plan
//!   with rows still owed);
//! * **a turn already running** → the step boundary of that turn, in the same turn.
//!
//! The restore case keeps the door it has always had, and is pinned by `restart_restore.rs`.

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

/// **A session an earlier daemon had already created** — the row is written first, the vocabulary
/// is loaded, and only then does `Harness::open` mint this daemon's registry: so the row's
/// `created_at` is behind `Registry::started_ms`, which is the whole of what *a restart happened
/// for this session* means when the session left no other record.
///
/// `None` when this box has no vocabulary GGUF, the way every harness test here answers that.
fn opened_by_a_later_daemon(dir: &Scratch, id: &str) -> Option<Harness> {
    opened_by_a_later_daemon_with(dir, id, |_| {})
}

/// The same, with rows written into the earlier daemon's store **before** this one opens it — the
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
    // **A daemon has to have stopped here.** The guard is a fact about the box rather than an
    // inference from a session's creation time (a seeded session is older than this daemon while
    // nothing restarted), so a fixture that wants the restart said must leave a stopped life.
    store
        .life_started("d-the-earlier-one", 1)
        .expect("the earlier daemon began");
    store
        .life_stopped("d-the-earlier-one", 2)
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
    drop(store);
    let mut cfg = Config::for_this_box(&dir.0);
    cfg.dialect = crate::Dialect::Qwen;
    cfg.store = Some(path);
    cfg.session_id = id.into();
    cfg.vocab_gguf = Some(gguf);
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    Some(Harness::open(&parts, cfg, Hub::new(id)).expect("the session opens"))
}

/// **One row of the board**, as the model writes it.
fn a_row(what: &str, status: TodoStatus) -> TodoItem {
    TodoItem {
        content: what.into(),
        status,
        by: TodoBy::Model,
        when: None,
        needs: Vec::new(),
    }
}

/// **One transcript row, written into the store behind the open harness's back.**
///
/// The harness is already open, so nothing resumes over this: it is the store's own last-row fact
/// that [`crate::restart::mid_turn`] reads, and staging it here is staging exactly what a daemon
/// that died mid-turn leaves. `seq` 0 needs the offset the transcript's stable prefix ends at, and
/// the trigger's other two checks follow from the token blob.
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

/// **The daemon that comes back to that store** — a new `Harness` over the same file, which is what
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
    // The fixture stages a stopped life row, so `latest_box_stop` finds it: the stop is
    // dated from that row, not Unknown, even though the session's own rows name no daemon.
    // The down time is a fact about the box (three restarts in an hour measured 1 h 11 m
    // when the notice asked only the session's daemons), so every stopped life row counts.
    assert!(
        text.contains("stopped at "),
        "the stop is dated from a stopped life row, not invented: {text}"
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

// ---------------------------------------------------------------------------
// What the session still OWES — the door the operator's 2026-11-01 ruling turns on
// ---------------------------------------------------------------------------

/// **A session whose plan still has rows owed is WOKEN, and the sentence says why.**
///
/// The operator: *"it is not ok that restart notice arrived with my continue. because just before
/// restart you said 'after restart i owe you blabla' so i expected that after restart you will
/// continue"*. A plan is the durable half of that promise — the model wrote it down — so the row
/// it left is the reason the sentence is a turn and not a row nobody reads.
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
        text.starts_with("[restart] the daemon came back at "),
        "{text}"
    );
    assert!(
        text.contains("2 todo rows still owed"),
        "the two rows that are owed, and not the three that are not: {text}"
    );
    // **A turn is owed, and the ring at open is what spends it** — `restart_notices` is the door
    // `Harness::wake` drains, and a non-empty list is a turn.
    assert_eq!(
        h.restart_notices,
        vec![text.clone()],
        "a session that owes work keeps the turn: {:?}",
        h.restart_notices
    );
    // **And it is NOT also filed as a row** — one restart, one notification, and this one is the
    // turn's opening line.
    assert!(restart_rows(&h).is_empty(), "{:?}", h.items());
    // **Once per (session, restart)**: a second open of the same session by the same daemon says
    // nothing at all, and does not queue a second turn.
    assert!(h.restore_after_restart().is_none(), "said twice");
    assert_eq!(h.restart_notices.len(), 1, "{:?}", h.restart_notices);
}

/// **A session that was mid-turn when the rows stopped is woken** — the arm the operator's own
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

/// **A prompt nobody answered is the same arm** — the person typed, the daemon stopped before the
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

/// **A session whose last row is a finished answer owes nothing, and keeps the row** — the cost
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
    drop(h);
    let mut h = reopened(&dir, id);
    let text = h.restore_after_restart().expect("a restart is said");
    assert!(text.ends_with(": nothing was running"), "{text}");
    assert!(
        h.restart_notices.is_empty(),
        "a session with nothing owed must not spend a call: {:?}",
        h.restart_notices
    );
    assert_eq!(restart_rows(&h), vec![text], "{:?}", h.items());
}

/// **A sentence the harness filed itself is not a turn in flight** — and that is what keeps an
/// idle session from being woken on every second restart.
///
/// The empty restart's own row is a `User` row with `Speaker::Agent`, exactly the shape a wake's
/// notice has; the store cannot tell them apart, so the class is left out ([`crate::restart::mid_turn`]).
/// Without that, restart N+1 would read restart N's row as work owed, wake the session, and
/// alternate a model call into every idle restart after the first.
#[test]
fn the_restart_row_it_filed_is_not_work_it_owes() {
    let dir = Scratch::new("restart-owed-filed");
    let id = "s-restart-owed-filed";
    let Some(mut h) = opened_by_a_later_daemon(&dir, id) else {
        return;
    };
    // The first restart files its sentence — nothing owed, so no turn.
    let first = h.restore_after_restart().expect("a restart is said");
    assert!(h.restart_notices.is_empty(), "{:?}", h.restart_notices);
    assert_eq!(restart_rows(&h), vec![first.clone()], "{:?}", h.items());
    // **The next daemon**, opening the same session against the row that is now its last.
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
        second.ends_with(": nothing was running"),
        "no clause, because nothing is owed: {second}"
    );
}
