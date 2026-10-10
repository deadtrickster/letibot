//! **A child is a session, so it gets the idle plan-check** — the proof, with a live child
//! harness and a store row behind it.
//!
//! The operator's ruling, verbatim: *"childs being just a session should get nags"*. What that
//! rules on is the limitation the previous branch (`agent/child-todos`) left standing: a
//! parent's rows reach the child's board, its store row and its pane, and are nag-worthy by
//! construction (`the_check_may_ask_about` filters by status and never by author) — but
//! `Sessions::rearm_todo_nag` reads `Sessions::open`, a child's harness is not in it, and so
//! nothing ever armed. The rows sat there, persisted and published, named only if the child
//! happened to run a turn for some other reason.
//!
//! # What is proved here, and through what
//!
//! The real chain, not a unit on the text: a child `Harness` opened the way
//! `HarnessTaskRunner::run_to_completion` opens it (registry-adopted with a parent, board
//! registered in `ChildBoards`), its first turn run on this thread, then parked in
//! `serve_child_under` on its own thread — and a parent's write through the REAL resolver
//! (`ParentTodos::upsert_child`), which persists the row, publishes `TodosUpdated` and wakes
//! the parked reader. What the test waits for is the child's own log holding the
//! `[todo check]` item as `Speaker::Agent` — the same item `nag_turn` puts in for a session
//! the daemon holds.
//!
//! # Why the parent writes TWICE, and the measurement that forced it
//!
//! **A single write cannot prove the wake.** The fixture spawns the child's serving thread
//! and the write follows it, so the write may land before that thread reaches its park — and
//! then `serve_child_under`'s own entry re-arm answers the test, and the wake this branch
//! adds is never exercised at all. MEASURED 2026-10-09: with `hub.wake_its_own_reader()`
//! deleted from `ParentTodos::upsert_child`, the one-write version of
//! `a_row_a_parent_writes_nags_the_parked_child` PASSED. The second write cannot be answered
//! that way — the entry arm is spent by then and the reader is provably back in
//! `take_own_work_until` — so the check that names the second row is the one that proves the
//! wake. See `SECOND_ROW`.
//!
//! # The clock is driven, not slept through
//!
//! `TODO_NAG_AFTER` is a minute. A test that waited a real minute would not be a test, so
//! `serve_child_under` takes the clock as its parameter and this file injects a 300ms window
//! (`TodoNagClock::with_window`) — the schedule is exercised end to end at a deadline the
//! test can afford. The DECISION the window guards (what may be asked about, the
//! silence-then-check rule, the postponed row) is pinned separately, as predicates, in
//! `sessions.rs`'s tests — for the reason that file gives: *silent because the row is
//! postponed* and *silent because nothing armed the clock* are the same observation from
//! outside.
//!
//! # What this needs, and what it does not
//!
//! The vocabulary GGUF (a `Harness` renders its stable prefix at open) and **not** a model:
//! every turn is answered by the canned server the turn crate's tests replay, shared by path
//! rather than copied. No process tree, so no apparatus gate beyond the GGUF.

use std::time::{Duration, Instant};

use letibot_harnessd::child_todos::ParentTodos;
use letibot_harnessd::config::Config;
use letibot_harnessd::harness::serve_child_under;
use letibot_harnessd::sessions::TodoNagClock;
use letibot_harnessd::{Dialect, Harness, Parts};
use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_tokencore::Vocab;
use letibot_tokencore::store::{Store, TodoBy, TodoStatus};
use letibot_tools::builtins::todo::ChildTodos;
use letibot_transcript::{Speaker, TranscriptItem};

/// The canned server the turn crate's tests replay, shared by path rather than
/// copied — a second copy of the wire shape would drift from the first.
#[path = "../../turn/tests/support/canned.rs"]
mod canned;

use canned::Frame;

// Qwen's own ids, the same values the turn crate's canned tests pin.
const THINK_CLOSE: u32 = 248069;
const IM_END: u32 = 248046;

/// The injected window — the one difference between a child this daemon parks and one this
/// test parks. Small enough that the whole schedule runs in well under a second of the
/// clock; large enough that a loaded runner's scheduling jitter is not a flake.
const WINDOW: Duration = Duration::from_millis(300);

/// How long the driver will wait for something that should arrive within one window —
/// generous on purpose: the assertion is the ORDER of events, not their speed.
const PATIENCE: Duration = Duration::from_secs(20);

/// The row the parent writes — the work the child is to be ASKED FOR, not told about.
const PARENTS_ROW: &str = "migrate the ledger rows to the new schema";

/// **The parent's SECOND row, and the one that proves the wake.**
///
/// A single write cannot: the fixture spawns the child's serving thread and the write
/// follows it, so the write may land before the thread reaches its park — and then the
/// clock's own entry re-arm answers the test and the wake this branch adds is never
/// exercised. MEASURED 2026-10-09: with `hub.wake_its_own_reader()` removed from
/// `ParentTodos::upsert_child`, the one-write version of this test PASSED. A second write
/// issued after the first check has been seen cannot be answered by the entry arm — that
/// arm is spent and the reader is provably back in its wait — so the check that names this
/// row can only come from the parent's wake re-arming the clock.
const SECOND_ROW: &str = "and then tell me what the old schema did";

fn config(store: &std::path::Path, session_id: &str) -> Config {
    let mut cfg = Config::for_this_box("/tmp");
    cfg.dialect = Dialect::Qwen;
    // No retry ladder: nothing here should fail, and a failure that retried would spend
    // the suite's time hiding behind doubling waits.
    cfg.http_retries = 0;
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = session_id.to_string();
    cfg.vocab_gguf = std::env::var("LETIBOT_VOCAB_GGUF")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(letibot_tokencore::apparatus::present_gguf);
    cfg
}

fn ids(vocab: &Vocab, text: &str) -> Vec<u32> {
    vocab.tokenize_text(text).expect("text tokenizes")
}

fn spoken(vocab: &Vocab, ids: &[u32]) -> Vec<Frame> {
    ids.iter()
        .map(|id| Frame::Token {
            id: *id,
            text: vocab
                .detokenize(&[*id], true)
                .expect("one token decodes")
                .leak(),
        })
        .collect()
}

/// A complete turn that answers in plain text: the model thinks, closes the
/// block, answers, ends — `wall_refusal.rs`'s shape, kept identical so the two
/// files' fixtures cannot drift apart on the wire bytes that matter.
fn a_plain_answer_turn(vocab: &Vocab, thought: &str, answer: &str, n_prompt: u64) -> Vec<Frame> {
    let thought_ids = ids(vocab, thought);
    let answer_ids = ids(vocab, answer);
    let mut frames = vec![Frame::Progress {
        total: n_prompt,
        processed: n_prompt,
    }];
    frames.extend(spoken(vocab, &thought_ids));
    frames.extend(spoken(vocab, &[THINK_CLOSE]));
    frames.extend(spoken(vocab, &answer_ids));
    frames.extend(spoken(vocab, &[IM_END]));
    frames.push(Frame::Final {
        stop_type: "eos",
        n_decoded: (thought_ids.len() + answer_ids.len() + 2) as u64,
        n_prompt,
        cache_n: 0,
    });
    frames
}

/// The `[todo check]` items on the child's own log, as their text — the harness's
/// self-addressed prompt, which is what a delivered check IS: a turn, started by nobody
/// outside the session.
fn checks_on(hub: &Hub) -> Vec<String> {
    hub.retained()
        .iter()
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
                    text.contains("[todo check]").then_some(text)
                }
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// Wait until `what` is true on the child's hub, or panic with everything the log holds —
/// the log IS the diagnosis for a schedule failure, and a bare timeout would hide it.
fn wait_for(hub: &Hub, what: impl Fn(&Hub) -> Option<String>) -> String {
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

/// **A parked child and the parent that can write to it** — the fixture both tests share,
/// mirroring `run_to_completion`'s spawn order: hub, harness (which registers the board in
/// `ChildBoards`), registry adoption naming the parent, first turn, then `serve_child_under`
/// on the child's own thread.
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
    let mut cfg = config(path, child);
    // Cloned rather than moved: `Canned` implements `Drop` (it owns the server thread's
    // handle), so no field of it can be moved out.
    cfg.endpoint = canned.endpoint.clone();
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let registry = Registry::new();
    let hub = registry.new_hub(child.to_string());
    // The child's harness, opened the way the runner opens it — this is what registers
    // its board in `parts.child_boards`, which is the map the parent's write resolves
    // through.
    let mut sub =
        Harness::open_with_registry(&parts, cfg, hub.clone(), None, None, registry.clone())
            .expect("the child opens");
    // Adopted AFTER the open, naming the parent — the one fact `ParentTodos`' predicate
    // reads (`parent_session_id == caller`), and what makes this a child rather than a
    // second root.
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
    // **The task it was spawned for** — a real turn against the canned server, on this
    // thread, exactly as `run_to_completion` runs it before parking.
    sub.submit_as_a_normal_session("do the task, then stop and wait")
        .expect("the first turn answers");
    // Parked. The clock's window is the test's; everything else about it is the
    // production schedule.
    let hub_for_the_struct = hub.clone();
    // The thread outlives this frame, so it cannot borrow the caller's `&str` — the name
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
            )
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
    /// The parent's resolver — the same `ParentTodos` a parent session's `todo_write`
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

/// **THE ONE THIS BRANCH EXISTS FOR**: a parent writes a row on its parked child's board,
/// and the child is asked for it on the clock — a real turn, on the child's own thread,
/// with nobody typing into the child.
#[test]
fn a_row_a_parent_writes_nags_the_parked_child() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-child-nag");
    let path = dir.path().join("sessions.db");
    let parent = "s-parent-of-the-nagged-child";
    let child = "s-child-due-a-check";

    // The vocabulary first — the canned frames must speak the ids the harness will send,
    // and `Parts::load` is what reads the GGUF the gate above approved of.
    let parts = Parts::load(&config(&path, child)).expect("the vocabulary must load");
    let first = a_plain_answer_turn(&parts.vocab, "planning", "the task is done", 30);
    let check = a_plain_answer_turn(&parts.vocab, "checking", "on it now", 30);
    let check_again = a_plain_answer_turn(&parts.vocab, "checking", "on that one too", 30);
    // **A repeat is another TURN, so it needs its own frames** — `Frame` is not `Clone`, which is
    // the tripwire's other half: the schedule is not allowed to reuse an answer.
    let repeat = a_plain_answer_turn(&parts.vocab, "checking", "still on the first one", 30);
    let repeat_again = a_plain_answer_turn(&parts.vocab, "checking", "and the second", 30);
    // **A generous pantry, and the RATE is the tripwire.** The child's first turn, then the checks
    // and their repeats (the operator's ruling — *"hmm, yes obviously it should be a repeated nag"*
    // — which a child shares because a child is a session). The ladder doubles the gap per
    // delivery, so the number of turns this test's few seconds can hold is small; the assertions
    // below bound it, and a metronome would blow past both the bound and the pantry.
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

    // **The parent writes, through the real resolver.** Before this branch the row
    // landed and nothing else happened; the assertion below is what changed.
    kid.parent(parent)
        .upsert_child(
            child,
            &[(PARENTS_ROW.into(), TodoStatus::Pending, Vec::new())],
        )
        .expect("the parent's write is accepted");

    // **The store row first** — the write is durable, authored by the parent, which is
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

    // **And then the nag** — the check names the parent's row, in the child's own voice
    // to itself (`Speaker::Agent`), naming the parent by id.
    let found = wait_for(&kid.hub, |hub| {
        checks_on(hub)
            .into_iter()
            .find(|t| t.contains(PARENTS_ROW) && t.contains(parent))
    });
    assert!(
        found.contains("[todo check]"),
        "the item is a plan-check, not a prompt somebody typed: {found}"
    );

    // **And the repeat, at the interval the schedule owes rather than on a metronome.** The same
    // notice comes back — the operator's ruling, and a child is a session — but it comes back at
    // TWICE the window: `TodoNagClock` doubles the gap per delivery, so three windows hold a check
    // and its repeat and not three checks. The clock's own unit tests pin the ladder with the times
    // injected; what is asserted here is that the child's thread runs the same schedule.
    sleep_for(WINDOW * 3);
    let said = checks_on(&kid.hub);
    assert!(
        (2..=3).contains(&said.len()),
        "a check and its repeat inside three windows — not one per window: {said:#?}"
    );
    assert_eq!(
        said[0], said[1],
        "the repeat is the same sentence, because the plan has not moved"
    );

    // **THE WAKE ITSELF, which one write cannot prove.** By now the first check has been
    // delivered, so the clock is disarmed and the child's serving thread is back in
    // `Hub::take_own_work_until(None)` — the entry re-arm is spent and nothing but this
    // write's own wake can re-arm it. See `SECOND_ROW`.
    kid.parent(parent)
        .upsert_child(
            child,
            &[(SECOND_ROW.into(), TodoStatus::Pending, Vec::new())],
        )
        .expect("the parent's second write is accepted");
    // The second notice is found by its OWN words and not by the second row's: a check names the
    // head of the plan and COUNTS the rest (`unfinished_plan`), so the row the parent added is the
    // "(1 more open)" and not a line — which is also what tells the two notices apart.
    let second = wait_for(&kid.hub, |hub| {
        checks_on(hub)
            .into_iter()
            .find(|t| t.contains("(1 more open)"))
    });
    assert_ne!(
        second, found,
        "the plan moved, so the second check is a new thing to say rather than the first \
         one over again"
    );
    // **The plan is bigger than the row it names, and the check says so.** A check serves the
    // queue's head one at a time (`unfinished_plan`), so the second row is the COUNT and not
    // a second line — which is the fact that makes this a check about the moved plan rather
    // than a re-rendering of the first one.
    assert!(
        second.contains("(1 more open)") && second.contains(PARENTS_ROW),
        "the second check knows about both rows: {second}"
    );

    // And the second plan's saying is as repeated-not-metronomic as the first's: its own check and
    // its own repeats, and the first plan's ladder was reset by the move — so the last two entries
    // are two deliveries of ONE sentence rather than a fresh sentence each time.
    sleep_for(WINDOW * 4);
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

/// **The operator's `[p]` stays theirs, on a child too**: a parent's POSTPONED row
/// persists on the child's board and is never asked about.
///
/// The observational half only — the predicate (`nag_should_arm` on a postponed-only
/// plan) is pinned in `sessions.rs`'s tests, for the reason that file gives: from outside,
/// this silence and an unarmed clock are the same thing to look at. What ONLY this test
/// can say is that the row is on the parked child's board through the whole of it, so the
/// silence is about the ROW and not about the write never landing.
#[test]
fn a_postponed_row_a_parent_writes_persists_and_never_nags() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-child-nag-postponed");
    let path = dir.path().join("sessions.db");
    let parent = "s-parent-who-set-it-aside";
    let child = "s-child-with-a-later-row";

    let parts = Parts::load(&config(&path, child)).expect("the vocabulary must load");
    let first = a_plain_answer_turn(&parts.vocab, "planning", "the task is done", 30);
    drop(parts);
    // One answer: the first turn. Nothing else should ever be asked — the canned server
    // serving exactly one is itself a tripwire: a nag that fired would be a request
    // nobody answers, and the test's log dump would say so.
    let canned = canned::Canned::serve_each(vec![first], 1);

    let kid = park_a_child(parent, child, canned, &path);
    kid.parent(parent)
        .upsert_child(
            child,
            &[(PARENTS_ROW.into(), TodoStatus::Postponed, Vec::new())],
        )
        .expect("the parent's write is accepted");

    // **Persisted**: the row is real and durable, set aside rather than dropped.
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

    // **And never asked**: ten windows pass — an armed clock would have fired in one —
    // and the child's log holds no check.
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

/// Wait a fixed span, checking nothing — the *silence* half of a schedule. A sleep and not
/// a poll because there is no fact to wait FOR: the assertion after it is that nothing
/// arrived, and a poll would only be a slower way of not looking.
fn sleep_for(span: Duration) {
    let until = Instant::now() + span;
    while Instant::now() < until {
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A directory that removes itself, because a test that leaks a store per run
/// eventually fills the disk and the run that finds out is not this one.
struct TempDir {
    path: std::path::PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
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
