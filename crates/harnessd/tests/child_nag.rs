//! A child is a session, so it gets the idle plan-check - with a live child harness and a
//! store row behind it. The chain is the real one:

use std::time::{Duration, Instant};

use letibot_harnessd::child_todos::ParentTodos;
use letibot_harnessd::config::Config;
use letibot_harnessd::harness::serve_child_under;
use letibot_harnessd::sessions::TodoNagClock;
use letibot_harnessd::{Dialect, Harness, Parts};
use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_tokencore::store::{Store, TodoBy, TodoStatus};
use letibot_tools::builtins::todo::ChildTodos;
use letibot_transcript::{Speaker, TranscriptItem};

mod support;

use support::canned;
use support::qwen::a_plain_answer_turn;

/// The injected window - the one difference between a child this daemon parks and one this test
/// parks. Small enough that the whole schedule runs in well under a second of the clock; large
/// enough that a loaded runner's scheduling jitter is not a flake.
const WINDOW: Duration = Duration::from_millis(300);

/// How long the driver will wait for something that should arrive within one window -
/// generous on purpose: the assertion is the ORDER of events, not their speed.
const PATIENCE: Duration = Duration::from_secs(20);

/// The row the parent writes - the work the child is to be ASKED FOR, not told about.
const PARENTS_ROW: &str = "migrate the ledger rows to the new schema";

/// The parent's SECOND row, and the one that proves the wake. A single write cannot: the
/// write follows the serving thread's spawn, so it may land before that thread reaches its
/// park - and then the clock's own entry re-arm answers the test.
const SECOND_ROW: &str = "and then tell me what the old schema did";

fn config(store: &std::path::Path, session_id: &str) -> Config {
    let mut cfg = Config::for_this_box("/tmp");
    cfg.dialect = Dialect::Qwen;
    // No retry ladder: nothing here should fail, and a failure that retried would spend
    // the suite's time hiding behind doubling waits.
    cfg.http_retries = 0;
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = session_id.to_string();
    cfg.vocab_gguf = support::vocab::gguf();
    cfg
}

/// The `[todo check]` items on the child's own log, as their text - the harness's self-addressed
/// prompt, which is what a delivered check IS: a turn, started by nobody outside the session.
fn checks_on(hub: &Hub) -> Vec<String> {
    checks_timed(hub)
        .into_iter()
        .map(|(_, text)| text)
        .collect()
}

/// The same, each with the moment the hub PUBLISHED it - the daemon's own `ts` and not the
/// driver's reading of the log. A schedule has to be timed on these.
fn checks_timed(hub: &Hub) -> Vec<(u64, String)> {
    hub.retained()
        .into_iter()
        .filter_map(|e| match &e.event {
            SessionEvent::TranscriptContent { item, .. } => match &**item {
                TranscriptItem::User { speaker, parts } if *speaker == Speaker::Agent => {
                    let text = parts
                        .iter()
                        .filter_map(|p| match p {
                            letibot_transcript::UserPart::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    text.contains("[todo check]").then_some((e.ts, text))
                }
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// Wait until `what` is true on the child's hub, or panic with everything the log holds -
/// the log IS the diagnosis for a schedule failure, and a bare timeout would hide it.
fn wait_for<T>(hub: &Hub, what: impl Fn(&Hub) -> Option<T>) -> T {
    let began = Instant::now();
    while began.elapsed() < PATIENCE {
        if let Some(found) = what(hub) {
            return found;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "nothing the test waited for arrived within {PATIENCE:?} — the child's log:\n{:#?}",
        hub.retained().iter().map(|e| &e.event).collect::<Vec<_>>()
    );
}

/// A parked child and the parent that can write to it - the fixture both tests share, mirroring
/// `run_to_completion`'s spawn order: hub, harness (which registers the board in `ChildBoards`)
/// registry adoption naming the parent, first turn, then `serve_child_under` on its own thread.
struct ParkedChild {
    hub: std::sync::Arc<Hub>,
    registry: std::sync::Arc<Registry>,
    boards: std::sync::Arc<letibot_harnessd::child_todos::ChildBoards>,
    joined: Option<std::thread::JoinHandle<()>>,
}

#[allow(clippy::type_complexity)]
fn park_a_child(
    parent: &str,
    child: &str,
    canned: canned::Canned,
    path: &std::path::Path,
) -> ParkedChild {
    park_a_child_with(parent, child, canned, path, |_, _| {})
}

/// [`park_a_child`], with the board written BEFORE the serving thread starts - `before` is handed
/// the child's harness and the parent's resolver, so rows of every author can go on the board
/// without racing the clock's first window.
fn park_a_child_with(
    parent: &str,
    child: &str,
    canned: canned::Canned,
    path: &std::path::Path,
    before: impl FnOnce(&mut Harness, &ParentTodos),
) -> ParkedChild {
    let mut cfg = config(path, child);
    // Cloned rather than moved: `Canned` implements `Drop` (it owns the server thread's
    // handle), so no field of it can be moved out.
    cfg.endpoint = canned.endpoint.clone();
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let registry = Registry::new();
    let hub = registry.new_hub(child.to_string());
    // The child's harness, opened the way the runner opens it - this is what registers its
    // board in `parts.child_boards`, the map the parent's write resolves through.
    let mut sub =
        Harness::open_with_registry(&parts, cfg, hub.clone(), None, None, registry.clone())
            .expect("the child opens");
    // Adopted AFTER the open, naming the parent - the one fact `ParentTodos`' predicate reads
    // (`parent_session_id == caller`), and what makes this a child rather than a second root.
    registry
        .adopt(
            hub.clone(),
            child.to_string(),
            SessionWiring {
                model: "m".into(),
                dialect: "qwen".into(),
                endpoint: "http://127.0.0.1:1".into(),
                workspace: "/tmp".into(),
            },
            Some(parent.to_string()),
        )
        .expect("the child is in the registry");
    // The task it was spawned for - a real turn against the canned server, on this
    // thread, exactly as `run_to_completion` runs it before parking.
    sub.submit_as_a_normal_session("do the task, then stop and wait")
        .expect("the first turn answers");
    before(
        &mut sub,
        &ParentTodos::new(
            parent.to_string(),
            registry.clone(),
            parts.child_boards.clone(),
        ),
    );
    // Parked. The clock's window is the test's; everything else about it is the
    // production schedule.
    let hub_for_the_struct = hub.clone();
    // The thread outlives this frame, so it cannot borrow the caller's `&str` - the name
    // it serves under is its own.
    let served_as = child.to_string();
    let parked = std::thread::Builder::new()
        .name(format!("serve-{child}"))
        .spawn(move || {
            serve_child_under(
                &mut sub,
                &hub,
                &served_as,
                TodoNagClock::with_window(WINDOW),
            );
        })
        .expect("the child's serving thread starts");
    ParkedChild {
        hub: hub_for_the_struct,
        registry,
        boards: parts.child_boards.clone(),
        joined: Some(parked),
    }
}

impl ParkedChild {
    /// The parent's resolver - the same `ParentTodos` a parent session's `todo_write`
    /// target=` goes through, bound to the same caller id the registry recorded.
    fn parent(&self, caller: &str) -> ParentTodos {
        ParentTodos::new(
            caller.to_string(),
            self.registry.clone(),
            self.boards.clone(),
        )
    }

    /// End the serving thread the way the daemon would: close its hub, then join.
    fn release(mut self) {
        self.hub.close();
        if let Some(j) = self.joined.take() {
            let _ = j.join();
        }
    }
}

/// THE ONE THIS BRANCH EXISTS FOR: a parent writes a row on its parked child's board, and the
/// child is asked for it on the clock - a real turn, on the child's own thread, with nobody typing
/// into the child.
#[test]
fn a_row_a_parent_writes_nags_the_parked_child() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-child-nag");
    let path = dir.path().join("sessions.db");
    let parent = "s-parent-of-the-nagged-child";
    let child = "s-child-due-a-check";

    // The vocabulary first - the canned frames must speak the ids the harness will send
    // and `Parts::load` is what reads the GGUF the gate above approved of.
    let parts = Parts::load(&config(&path, child)).expect("the vocabulary must load");
    let first = a_plain_answer_turn(&parts.vocab, "planning", "the task is done", 30);
    let check = a_plain_answer_turn(&parts.vocab, "checking", "on it now", 30);
    let check_again = a_plain_answer_turn(&parts.vocab, "checking", "on that one too", 30);
    // A repeat is another TURN, so it needs its own frames - `Frame` is not `Clone`, which is
    // the tripwire's other half: the schedule is not allowed to reuse an answer.
    let repeat = a_plain_answer_turn(&parts.vocab, "checking", "still on the first one", 30);
    let repeat_again = a_plain_answer_turn(&parts.vocab, "checking", "and the second", 30);
    // A generous pantry, and the RATE is the tripwire. The child's first turn, then the checks
    // and their repeats (a child shares the repeat, because a child is a session).
    let mut scripts = vec![first, check, check_again, repeat, repeat_again];
    for i in 0..35 {
        scripts.push(a_plain_answer_turn(
            &parts.vocab,
            "checking",
            &format!("and again {i}"),
            30,
        ));
    }
    let canned = canned::Canned::serve_each(scripts, 40);
    drop(parts);

    let kid = park_a_child(parent, child, canned, &path);

    // The parent writes, through the real resolver. Before this branch the row
    // landed and nothing else happened; the assertion below is what changed.
    kid.parent(parent)
        .upsert_child(
            child,
            &[(PARENTS_ROW.into(), TodoStatus::Pending, Vec::new())],
        )
        .expect("the parent's write is accepted");

    // The store row first - the write is durable, authored by the parent, which is
    // the half the previous branch built and must still be true.
    let rows = Store::open(&path)
        .expect("the store")
        .todos(child)
        .expect("the row");
    let row = rows
        .iter()
        .find(|t| t.content == PARENTS_ROW)
        .unwrap_or_else(|| panic!("the parent's row is on the child's board: {rows:#?}"));
    assert_eq!(
        row.by,
        TodoBy::parent_of(parent),
        "the author travels: `Parent <the caller's full id>`"
    );
    assert_eq!(row.status, TodoStatus::Pending);

    // And then the nag - the check names the parent's row, in the child's own voice
    // to itself (`Speaker::Agent`), naming the parent by id.
    let (first_at, found) = wait_for(&kid.hub, |hub| {
        checks_timed(hub)
            .into_iter()
            .find(|(_, t)| t.contains(PARENTS_ROW) && t.contains(parent))
    });
    assert!(
        found.contains("[todo check]"),
        "the item is a plan-check, not a prompt somebody typed: {found}"
    );

    // And the repeat, at the interval the schedule owes rather than on a metronome.
    // `TodoNagClock` doubles the gap per delivery, so the repeat is due TWO windows after the
    // first delivery and the one after that is four.
    let (repeat_at, repeat) = wait_for(&kid.hub, |hub| {
        let said = checks_timed(hub);
        (said.len() >= 2).then(|| said[1].clone())
    });
    // The repeat names the same row, in the same words: it used to say *"you were asked about
    // this one already"*, which the model answered instead of the row.
    assert!(
        found.contains(&format!("{PARENTS_ROW} — Parent {parent}")),
        "the check names the parent's row and its author: {found}"
    );
    assert_eq!(
        repeat, found,
        "the repeat is the same notice, because the plan has not moved"
    );
    // Half again the window: more than a metronome's gap, comfortably under the ladder's two
    // so the millisecond the two `ts` values are stamped in cannot decide the answer.
    let floor = (WINDOW * 3) / 2;
    assert!(
        repeat_at.saturating_sub(first_at) >= floor.as_millis() as u64,
        "the repeat is due a DOUBLED window ({:?}) after the first delivery and these two are \
         {}ms apart — one per window would be {:?}: {found}",
        WINDOW * 2,
        repeat_at.saturating_sub(first_at),
        WINDOW
    );

    // THE WAKE ITSELF, which one write cannot prove. By now the first check has been delivered
    // so the clock is disarmed and the serving thread is back in `take_own_work_until(None)` - the
    // entry re-arm is spent and nothing but this write's own wake can re-arm it. See `SECOND_ROW`.
    kid.parent(parent)
        .upsert_child(
            child,
            // The first row done and a second one open: the QUESTION moves (a row added beside an
            // unchanged question no longer makes a new notice).
            &[
                (PARENTS_ROW.into(), TodoStatus::Completed, Vec::new()),
                (SECOND_ROW.into(), TodoStatus::Pending, Vec::new()),
            ],
        )
        .expect("the parent's second write is accepted");
    // The second notice names the second row, which is what tells the two notices apart.
    let (second_at, second) = wait_for(&kid.hub, |hub| {
        checks_timed(hub)
            .into_iter()
            .find(|(_, t)| t.contains(SECOND_ROW))
    });
    assert_ne!(
        second, found,
        "the plan moved, so the second check is a new thing to say rather than the first \
         one over again"
    );
    assert!(
        !second.contains(PARENTS_ROW),
        "the done row is not asked about: {second}"
    );

    // And the second plan's saying is as repeated-not-metronomic as the first's: its own check and
    // its own repeat, the ladder restarted by the move - a different notice is one base window
    // again, not whatever gap the old one had grown to - and the pair timed the same way.
    let (second_repeat_at, said_again) = wait_for(&kid.hub, |hub| {
        let mine: Vec<(u64, String)> = checks_timed(hub)
            .into_iter()
            .filter(|(_, t)| *t == second)
            .collect();
        (mine.len() >= 2).then(|| mine[1].clone())
    });
    assert_eq!(
        said_again, second,
        "the repeat is the second plan's own notice, not the first one over again"
    );
    assert!(
        second_repeat_at.saturating_sub(second_at) >= floor.as_millis() as u64,
        "the second plan's repeat is a doubled window after ITS first delivery and these two are \
         {}ms apart",
        second_repeat_at.saturating_sub(second_at)
    );
    let said = checks_on(&kid.hub);
    assert!(
        said.len() >= 4,
        "the second plan was said and repeated: {said:#?}"
    );
    assert_eq!(
        said[said.len() - 1],
        said[said.len() - 2],
        "the last two are repeats of one notice"
    );

    kid.release();
}

/// The operator's `[p]` stays theirs, on a child too: a parent's POSTPONED row persists on
/// the child's board and is never asked about.
#[test]
fn a_postponed_row_a_parent_writes_persists_and_never_nags() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-child-nag-postponed");
    let path = dir.path().join("sessions.db");
    let parent = "s-parent-who-set-it-aside";
    let child = "s-child-with-a-later-row";

    let parts = Parts::load(&config(&path, child)).expect("the vocabulary must load");
    let first = a_plain_answer_turn(&parts.vocab, "planning", "the task is done", 30);
    drop(parts);
    // One answer: the first turn. Nothing else should ever be asked - the canned server serving
    // exactly one is itself a tripwire: a nag that fired would be a request nobody answers, and the
    // test's log dump would say so.
    let canned = canned::Canned::serve_each(vec![first], 1);

    let kid = park_a_child(parent, child, canned, &path);
    kid.parent(parent)
        .upsert_child(
            child,
            &[(PARENTS_ROW.into(), TodoStatus::Postponed, Vec::new())],
        )
        .expect("the parent's write is accepted");

    // Persisted: the row is real and durable, set aside rather than dropped.
    let rows = Store::open(&path)
        .expect("the store")
        .todos(child)
        .expect("the row");
    let row = rows
        .iter()
        .find(|t| t.content == PARENTS_ROW)
        .unwrap_or_else(|| panic!("the postponed row is on the child's board: {rows:#?}"));
    assert_eq!(row.status, TodoStatus::Postponed);
    assert_eq!(row.by, TodoBy::parent_of(parent));

    // And never asked: ten windows pass - an armed clock would have fired in one - and the
    // child's log holds no check.
    let until = Instant::now() + WINDOW * 10;
    while Instant::now() < until {
        assert!(
            checks_on(&kid.hub).is_empty(),
            "a postponed row is not work the check may speak about, whoever wrote it"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    kid.release();
}

/// A directory that removes itself, because a test that leaks a store per run eventually fills the
/// disk and the run that finds out is not this one.
struct TempDir {
    path: std::path::PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = letibot_sessionlog::testing::scratch_root().join(format!(
            "{tag}-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock before epoch")
                .as_nanos(),
            {
                static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            }
        ));
        std::fs::create_dir_all(&path).expect("creating the temp dir");
        Self { path }
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// The operator's row is named first, and the first check says they added it. The child's
/// board holds the parent's row and then the operator's - theirs LAST, the way the snapshot
/// orders its halves - and the check the child's own thread delivers names the operator's.
#[test]
fn a_childs_check_names_the_operators_row_first() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-child-nag-operator");
    let path = dir.path().join("sessions.db");
    let parent = "s-parent-beside-the-operator";
    let child = "s-child-with-an-operator-row";
    const THEIRS: &str = "make tier 1 green";

    let parts = Parts::load(&config(&path, child)).expect("the vocabulary must load");
    let mut scripts = vec![a_plain_answer_turn(
        &parts.vocab,
        "planning",
        "the task is done",
        30,
    )];
    for i in 0..10 {
        scripts.push(a_plain_answer_turn(
            &parts.vocab,
            "checking",
            &format!("on it {i}"),
            30,
        ));
    }
    let canned = canned::Canned::serve_each(scripts, 20);
    drop(parts);

    let kid = park_a_child_with(parent, child, canned, &path, |sub, theirs| {
        theirs
            .upsert_child(
                child,
                &[(PARENTS_ROW.into(), TodoStatus::Pending, Vec::new())],
            )
            .expect("the parent's write is accepted");
        sub.set_operator_todos(
            vec![letibot_tokencore::store::TodoItem {
                content: THEIRS.into(),
                status: TodoStatus::Pending,
                by: TodoBy::Operator,
                when: None,
                needs: Vec::new(),
            }],
            &[],
        );
    });

    let first = wait_for(&kid.hub, |hub| checks_on(hub).into_iter().next());
    assert!(
        first.starts_with("[todo check] The operator added this, and it is not done:")
            && first.contains(&format!("  - {THEIRS} — the operator's")),
        "the child's first check names the operator's row, and says they added it: {first}"
    );
    assert!(
        !first.contains(&format!("  - {PARENTS_ROW}")),
        "the parent's row is not the line: {first}"
    );

    kid.release();
}

/// The warning rows on the child's hub with this code, as their text.
fn warnings_on(hub: &Hub, code: &str) -> Vec<String> {
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

/// `/nag …` sent to the child as a head sends it - on the child's own queue, where its serving
/// thread takes it (the worker hands a `/nag` it wins back to that thread).
fn nag_verb_to(hub: &Hub, line: &str) {
    assert!(
        hub.submit_daemon(
            "dead",
            format!("nag-{line}"),
            letibot_sessionlog::CommandKind::Slash { line: line.into() },
        ),
        "the child's hub takes the command"
    );
}

/// `/nag pause` on a CHILD pauses that child - its clock is on its own thread, and the verb is
/// answered there: a parent's row lands on the paused child's board and is not asked about, and
/// `/nag resume` brings the check. The pause is per session, and a child is a session.
#[test]
fn a_paused_child_is_not_checked_until_it_is_resumed() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-child-nag-paused");
    let path = dir.path().join("sessions.db");
    let parent = "s-parent-of-the-paused-child";
    let child = "s-child-paused-by-the-operator";

    let parts = Parts::load(&config(&path, child)).expect("the vocabulary must load");
    let mut scripts = vec![a_plain_answer_turn(
        &parts.vocab,
        "planning",
        "the task is done",
        30,
    )];
    for i in 0..10 {
        scripts.push(a_plain_answer_turn(
            &parts.vocab,
            "checking",
            &format!("on it {i}"),
            30,
        ));
    }
    let canned = canned::Canned::serve_each(scripts, 11);
    drop(parts);

    let kid = park_a_child(parent, child, canned, &path);
    nag_verb_to(&kid.hub, "nag pause");
    // Answered on the child's own thread, and loud: the verb's reply and the hub row.
    wait_for(&kid.hub, |hub| warnings_on(hub, "nags_paused").pop());
    assert!(
        warnings_on(&kid.hub, "child_door_refused").is_empty(),
        "`/nag` is the one verb a child answers itself"
    );

    kid.parent(parent)
        .upsert_child(
            child,
            &[(PARENTS_ROW.into(), TodoStatus::Pending, Vec::new())],
        )
        .expect("the parent's write is accepted");
    // Ten windows - an armed clock would have fired in one.
    let until = Instant::now() + WINDOW * 10;
    while Instant::now() < until {
        assert!(
            checks_on(&kid.hub).is_empty(),
            "a paused child is not asked about the row its parent wrote"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        Store::open(&path)
            .expect("the store")
            .session(child)
            .expect("read")
            .is_some_and(|s| s.nags_paused_until == Some(0)),
        "the child's pause is on the child's own row"
    );

    nag_verb_to(&kid.hub, "nag resume");
    wait_for(&kid.hub, |hub| warnings_on(hub, "nags_resumed").pop());
    let found = wait_for(&kid.hub, |hub| {
        checks_on(hub).into_iter().find(|t| t.contains(PARENTS_ROW))
    });
    assert!(
        found.contains("[todo check]"),
        "resumed, and asked: {found}"
    );

    kid.release();
}
