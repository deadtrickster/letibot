//! Delivery at the round boundary and the turn's start: what a running turn is handed, and what waits for the next break.
//! The decision is `crate::delivery`'s and asserted there; these drive it through the steering source a turn polls.

use super::*;

use crate::delivery::{DeliveryLog, LOG};
use crate::jobwatch::TurnEnd;
use letibot_tokencore::store::{TodoBy, TodoCondition, TodoNeed, TodoStatus};
use letibot_tools::runtime::WantedNow;
use std::time::{Duration, SystemTime};

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "{tag}-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            {
                static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            }
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    /// The record's lines, parsed.
    fn records(&self) -> Vec<serde_json::Value> {
        std::fs::read_to_string(self.0.join(LOG))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).expect("a record line is JSON"))
            .collect()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn settled(job: &str, state: &str, arrived: SystemTime) -> JobCompletion {
    JobCompletion {
        memory: None,
        kind: BackgroundKind::Job,
        owner: "s-r79".into(),
        job: job.into(),
        command: format!("make {job}"),
        slug: None,
        state: state.into(),
        produced: 12,
        elapsed_ms: 1500,
        detail: String::new(),
        arrived,
        later: false,
    }
}

/// A session's watcher set - a ROOT's when `child` is false, a child's (joined to a parent's
/// tree, which is what gives it a level above) when true.
fn watchers(hub: &Arc<Hub>, child: bool) -> Arc<JobWatchers> {
    let own = JobWatchers::watching_tasks(hub, None);
    if child {
        let parent = JobWatchers::watching_tasks(&Hub::new("s-parent"), None);
        own.shares_tree(&parent)
    } else {
        own
    }
}

fn deliveries(
    w: &Arc<JobWatchers>,
    board: Vec<TodoItem>,
    dir: &Scratch,
    cap: Duration,
) -> Deliveries {
    Deliveries::new(
        Arc::clone(w),
        Some(Arc::new(TodoBoard::new(board))),
        DeliveryLog::under(&dir.0, "s-r79"),
        cap,
        "s-r79",
    )
}

fn steering(hub: &Arc<Hub>, d: Deliveries) -> HubSteering {
    HubSteering {
        hub: hub.clone(),
        trail: None,
        injected: Arc::new(Mutex::new(VecDeque::new())),
        monitors: None,
        monitor_cursor: Arc::new(AtomicUsize::new(0)),
        deliveries: Some(d),
        subagents: None,
        held: Default::default(),
        stopped: Default::default(),
        todos: None,
        // These fixtures are about deliveries, not about the tree: a session with no parent
        // words a message `message from session `<id>`` - see `message_from_a_session`.
        parent: None,
        at_boundary: false,
    }
}

/// One poll at a step boundary, which is the only poll a turn's results are handed over at
/// - a poll between tokens answers nothing, which is its own test.
fn boundary(s: &mut HubSteering) -> Option<SteeringMessage> {
    s.at_a_step_boundary();
    s.try_next()
}

fn row(when: Option<TodoCondition>, needs: Vec<TodoNeed>) -> TodoItem {
    TodoItem {
        content: "land the release".into(),
        status: TodoStatus::InProgress,
        by: TodoBy::Model,
        when,
        needs,
    }
}

/// A routine completion during a long turn is held, and the batch arrives as ONE note at the boundary: a child does not
/// hear every settlement at its next round: neither of two routine ones reaches it mid-turn, with both under one
/// `[meanwhile]` line and recorded as a batch of two with the times they arrived.
#[test]
fn a_routine_completion_during_a_long_turn_is_held_and_arrives_as_one_batch() {
    let dir = Scratch::new("r79-batch");
    let hub = Hub::new("s-r79");
    let w = watchers(&hub, true);
    let d = deliveries(&w, Vec::new(), &dir, Duration::from_secs(600));
    let mut s = steering(&hub, d.clone());

    let first = SystemTime::now() - Duration::from_secs(30);
    w.queue_for_test(settled("j1", "exited 0", first));
    assert!(
        boundary(&mut s).is_none(),
        "a routine completion interrupted a running turn"
    );
    w.queue_for_test(settled("j2", "exited 0", SystemTime::now()));
    assert!(boundary(&mut s).is_none(), "the second one did too");
    assert_eq!(w.held_count(), 2, "both are held, not dropped");
    assert!(dir.records().is_empty(), "nothing has been delivered yet");

    // The long turn ends; the next turn's start (a prompt that got there before the turn-end
    // wake) carries what was held through it.
    w.turn_ended(TurnEnd::Done);
    let note = d.at_turn_start().expect("the next turn carries the batch");
    assert!(
        note.starts_with("[meanwhile] Finished while you were working:"),
        "{note}"
    );
    assert!(
        note.contains("[job] 2 jobs you backgrounded have ended:")
            && note.contains("`j1` exited 0")
            && note.contains("`j2` exited 0"),
        "one batch, in the [job] group's own words: {note}"
    );
    assert_eq!(w.held_count(), 0, "the batch is taken once");
    assert!(d.at_turn_start().is_none(), "and not delivered twice");

    let rec = dir.records();
    assert_eq!(rec.len(), 2, "{rec:?}");
    for r in &rec {
        assert_eq!(r["rule"], "batch");
        assert_eq!(r["batch"], 2);
        assert_eq!(r["kind"], "job");
        assert_eq!(r["session"], "s-r79");
    }
    let j1 = rec.iter().find(|r| r["handle"] == "j1").unwrap();
    let first_ms = first
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    assert_eq!(j1["arrived_at_ms"], first_ms);
    assert!(
        j1["waited_ms"].as_u64().unwrap() >= 30_000,
        "the wait is measured from the arrival: {j1}"
    );
}

/// A ROOT's watcher set rung through a real registry's bell, with its hub adopted - so a test can
/// ask the worker's own question, *is there a wake for this session?*
fn rung_watchers() -> (
    Arc<Hub>,
    Arc<JobWatchers>,
    Arc<letibot_sessionlog::registry::Registry>,
) {
    let registry = letibot_sessionlog::registry::Registry::new();
    let hub = registry.new_hub("s-r79".to_string());
    registry
        .adopt(
            hub.clone(),
            "s-r79".to_string(),
            letibot_sessionlog::registry::SessionWiring {
                model: "m".into(),
                dialect: "qwen".into(),
                endpoint: "http://127.0.0.1:1".into(),
                workspace: "/".into(),
            },
            None,
        )
        .expect("adopted");
    let w = JobWatchers::watching_tasks(&hub, Some(Arc::clone(registry.bell())));
    (hub, w, registry)
}

fn woken(registry: &letibot_sessionlog::registry::Registry) -> bool {
    matches!(
        registry.next_work_until(Some(std::time::Instant::now())),
        letibot_sessionlog::registry::WorkOrIdle::Work(
            letibot_sessionlog::registry::Work::Woken(id)
        ) if id == "s-r79"
    )
}

/// Mid-turn a routine result is still held; the turn's END wakes the session for it, and the wake hands it over as one
/// batch under the turn-end heading. Holding is for a RUNNING turn only: the round boundary still holds without
/// interrupting, the turn end rings the wake when something is held, and `between_turns` delivers it as `turn_end`.
#[test]
fn the_end_of_a_turn_with_results_held_wakes_the_session_for_them() {
    let dir = Scratch::new("r79-turn-end");
    let (hub, w, registry) = rung_watchers();
    let d = deliveries(&w, Vec::new(), &dir, Duration::from_secs(600));
    let mut s = steering(&hub, d.clone());

    w.turn_ended(TurnEnd::Done);
    assert!(!woken(&registry), "a turn that held nothing wakes nothing");

    w.queue_for_test(settled("j1", "exited 0", SystemTime::now()));
    assert!(boundary(&mut s).is_none(), "mid-turn it is still held");
    assert_eq!(w.held_count(), 1);
    assert!(!woken(&registry), "and holding it rang nothing");

    w.turn_ended(TurnEnd::Done);
    assert!(woken(&registry), "the turn ended with a result held");

    let b = d.between_turns(|_| false, crate::delivery::Rule::TurnEnd);
    assert!(b.now.is_empty(), "nothing arrived while idle: {:?}", b.now);
    let batch = b.batch.expect("the held result is delivered");
    assert!(
        batch.starts_with("[meanwhile] Finished while you were working:")
            && batch.contains("`j1` exited 0"),
        "{batch}"
    );
    assert_eq!(w.held_count(), 0);
    let rec = dir.records();
    assert_eq!(rec.len(), 1, "{rec:?}");
    assert_eq!(rec[0]["rule"], "turn_end");
}

/// Between turns the line is the turn's end: what arrived before it is the batch under its heading, and what arrived
/// after it while the session was idle is an ordinary notice with no heading, recorded as `idle`. Several held results
/// are ONE batch.
#[test]
fn an_idle_arrival_is_an_ordinary_notice_and_the_held_ones_are_one_batch() {
    let dir = Scratch::new("r79-idle");
    let (_hub, w, _registry) = rung_watchers();
    let d = deliveries(&w, Vec::new(), &dir, Duration::from_secs(600));

    w.hold(vec![
        settled("j1", "exited 0", SystemTime::now()),
        settled("j2", "exited 0", SystemTime::now()),
    ]);
    w.turn_ended(TurnEnd::Done);
    std::thread::sleep(Duration::from_millis(5));
    w.queue_for_test(settled("j3", "exited 0", SystemTime::now()));

    let b = d.between_turns(|_| false, crate::delivery::Rule::TurnEnd);
    assert_eq!(b.now.len(), 1, "{:?}", b.now);
    assert!(
        b.now[0].starts_with("[job] ") && b.now[0].contains("`j3`"),
        "{}",
        b.now[0]
    );
    assert!(!b.now[0].contains("`j1`") && !b.now[0].contains("[meanwhile]"));
    let batch = b.batch.expect("the two held");
    assert!(
        batch.starts_with("[meanwhile] Finished while you were working:")
            && batch.contains("`j1`")
            && batch.contains("`j2`")
            && !batch.contains("`j3`"),
        "{batch}"
    );
    let rules: Vec<(String, String, u64)> = dir
        .records()
        .iter()
        .map(|r| {
            (
                r["handle"].as_str().unwrap().into(),
                r["rule"].as_str().unwrap().into(),
                r["batch"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        rules,
        [
            ("j3".into(), "idle".into(), 1),
            ("j1".into(), "turn_end".into(), 2),
            ("j2".into(), "turn_end".into(), 2),
        ]
    );
}

/// The `HeldResults` events the hub has been handed, in order, as `(jobs, subagents, failed)`.
fn held_said(hub: &Hub) -> Vec<(u64, u64, u64)> {
    hub.retained()
        .into_iter()
        .filter_map(|e| match e.event {
            letibot_sessionlog::SessionEvent::HeldResults {
                jobs,
                subagents,
                failed,
            } => Some((jobs, subagents, failed)),
            _ => None,
        })
        .collect()
}

/// The held set is said on the hub every time it changes, by kind and failures, and only then: a job and a failed
/// subagent held at a round boundary say `(1, 1, 1)`, a boundary that changes nothing says nothing, and delivering it
/// says `(0, 0, 0)`.
#[test]
fn the_held_counts_are_said_on_the_hub_when_they_change() {
    let dir = Scratch::new("r79-held-said");
    let hub = Hub::new("s-r79");
    let w = watchers(&hub, false);
    let d = deliveries(&w, Vec::new(), &dir, Duration::from_secs(600));
    let mut s = steering(&hub, d.clone());

    w.queue_for_test(settled("j1", "exited 0", SystemTime::now()));
    w.queue_for_test(JobCompletion {
        kind: BackgroundKind::Subagent,
        ..settled("s-child", "failed", SystemTime::now())
    });
    assert!(
        boundary(&mut s).is_none(),
        "both are routine, so both are held"
    );
    assert_eq!(held_said(&hub), [(1, 1, 1)]);

    assert!(boundary(&mut s).is_none());
    assert_eq!(
        held_said(&hub),
        [(1, 1, 1)],
        "a boundary that changed nothing said nothing"
    );

    w.turn_ended(TurnEnd::Done);
    let b = d.between_turns(|_| false, crate::delivery::Rule::TurnEnd);
    assert!(b.batch.is_some());
    assert_eq!(held_said(&hub), [(1, 1, 1), (0, 0, 0)]);
}

/// A `when: job` row's job is delivered at once - to a ROOT too, at the round boundary of the
/// turn that is running. A routine one beside it stays held.
#[test]
fn a_when_job_rows_job_is_delivered_at_once() {
    let dir = Scratch::new("r79-when");
    let hub = Hub::new("s-r79");
    let w = watchers(&hub, false);
    let board = vec![row(
        Some(TodoCondition::Job {
            handle: "j12".into(),
        }),
        Vec::new(),
    )];
    let mut s = steering(&hub, deliveries(&w, board, &dir, Duration::from_secs(600)));

    w.queue_for_test(settled("j12", "exited 0", SystemTime::now()));
    w.queue_for_test(settled("j13", "exited 0", SystemTime::now()));
    let got = boundary(&mut s).expect("the plan waits on j12");
    assert!(
        got.text
            .starts_with("[job] a job you backgrounded has ended:")
            && got.text.contains("`j12`")
            && !got.text.contains("`j13`"),
        "{}",
        got.text
    );
    assert!(!got.from_operator && !got.urgent);
    assert_eq!(w.held_count(), 1, "j13 is nobody's dependency and waits");
    let rec = dir.records();
    assert_eq!(rec.len(), 1);
    assert_eq!(rec[0]["rule"], "immediate:when_job");
    assert_eq!(rec[0]["batch"], 1);
}

/// The failure of a needed job is immediate - named by the slug the row used - and recorded
/// as a broken dependency rather than as a need met.
#[test]
fn the_failure_of_a_needed_job_is_immediate() {
    let dir = Scratch::new("r79-failed");
    let hub = Hub::new("s-r79");
    let w = watchers(&hub, false);
    let board = vec![row(
        None,
        vec![TodoNeed::Job {
            handle: "release-build".into(),
        }],
    )];
    let mut s = steering(&hub, deliveries(&w, board, &dir, Duration::from_secs(600)));

    let mut c = settled("j40", "exited 101", SystemTime::now());
    c.slug = Some("release-build".into());
    w.queue_for_test(c);
    let got = boundary(&mut s).expect("a broken dependency stops the work");
    assert!(
        got.text.contains("`release-build · j40` exited 101"),
        "{}",
        got.text
    );
    assert_eq!(dir.records()[0]["rule"], "immediate:dependency_failed");
}

/// `wake: "now"` is immediate, and so is a result the model asked for with a wait after it
/// had been held - the sort re-reads what is held against what is wanted now.
#[test]
fn wake_now_and_a_wait_are_immediate() {
    let dir = Scratch::new("r79-now");
    let hub = Hub::new("s-r79");
    let w = watchers(&hub, false);
    let mut s = steering(
        &hub,
        deliveries(&w, Vec::new(), &dir, Duration::from_secs(600)),
    );

    w.want_now("j5", WantedNow::WakeNow);
    w.queue_for_test(settled("j5", "exited 0", SystemTime::now()));
    let got = boundary(&mut s).expect("wake: now");
    assert!(got.text.contains("`j5`"), "{}", got.text);

    w.queue_for_test(settled("j6", "exited 0", SystemTime::now()));
    assert!(boundary(&mut s).is_none(), "j6 is routine and held");
    w.want_now("j6", WantedNow::Awaited);
    let got = boundary(&mut s).expect("a job_wait on a held result promotes it");
    assert!(got.text.contains("`j6`"), "{}", got.text);
    assert_eq!(w.held_count(), 0);

    let rules: Vec<String> = dir
        .records()
        .iter()
        .map(|r| r["rule"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(rules, ["immediate:wake_now", "immediate:awaited"]);
}

/// The sort runs at a step boundary, not on every streamed delta: what a turn may
/// deliver is read from the whole board and taken out of the held set, so a poll between tokens
/// answers nothing and the boundary the engine marks is where the result comes in - once.
#[test]
fn a_result_waits_for_the_step_boundary_and_not_for_a_token() {
    let dir = Scratch::new("r79-boundary");
    let hub = Hub::new("s-r79");
    let w = watchers(&hub, false);
    let mut s = steering(
        &hub,
        deliveries(&w, Vec::new(), &dir, Duration::from_secs(600)),
    );

    w.want_now("j9", WantedNow::WakeNow);
    w.queue_for_test(settled("j9", "exited 0", SystemTime::now()));
    assert!(
        s.try_next().is_none(),
        "a poll between tokens delivered a result, which is the whole board read per token"
    );
    assert!(w.pending(), "it is still the queue's: nothing was taken");
    assert!(dir.records().is_empty(), "and nothing was recorded");

    s.at_a_step_boundary();
    let got = s.try_next().expect("the boundary hands over an immediate");
    assert!(got.text.contains("`j9`"), "{}", got.text);
    assert!(s.try_next().is_none(), "one boundary, one handover");
    assert_eq!(dir.records().len(), 1, "{:?}", dir.records());
}

/// The starvation cap: a turn running with a result held past the (here shortened) cap gets
/// the batch at its next round boundary anyway, as one note - and not before.
#[test]
fn the_starvation_cap_releases_the_batch_mid_turn() {
    let dir = Scratch::new("r79-cap");
    let hub = Hub::new("s-r79");
    let w = watchers(&hub, false);
    let cap = Duration::from_millis(300);
    let mut s = steering(&hub, deliveries(&w, Vec::new(), &dir, cap));

    w.queue_for_test(settled("j7", "exited 0", SystemTime::now()));
    w.queue_for_test(settled("j8", "exited 1", SystemTime::now()));
    assert!(
        boundary(&mut s).is_none(),
        "inside the cap, nothing comes in"
    );
    std::thread::sleep(cap + Duration::from_millis(50));
    let got = boundary(&mut s).expect("past the cap, the batch is released");
    assert!(
        got.text
            .starts_with("[meanwhile] Finished while you were working:"),
        "{}",
        got.text
    );
    assert!(
        got.text.contains("`j7`") && got.text.contains("`j8`"),
        "{}",
        got.text
    );
    assert!(boundary(&mut s).is_none(), "one note, once");
    let rec = dir.records();
    assert_eq!(rec.len(), 2);
    assert!(
        rec.iter()
            .all(|r| r["rule"] == "starvation" && r["batch"] == 2),
        "{rec:?}"
    );
}

/// A held batch with a row waiting on it says the plan line and a head that does not deny
/// it, so it never says "nothing in your plan was waiting on it" over a line naming the row
/// that was.
#[test]
fn a_held_batch_never_denies_the_plan_line_under_it() {
    let board = waiting_board();
    let text = meanwhile_notice(
        vec![settled("j12", "exited 0", SystemTime::UNIX_EPOCH)],
        "s-r79",
        crate::delivery::Rule::Batch,
        crate::delivery::HELD_RESULTS_CAP,
        &board,
    );
    assert!(
        text.contains("Waiting on this: \"make tier 1 green\""),
        "the row that waited on it is named: {text}"
    );
    assert!(
        !text.to_lowercase().contains("nothing in your plan"),
        "a head that says the plan was not waiting contradicts the line under it: {text}"
    );
}

/// The batch's own words are pinned for the head: `[meanwhile] …` and then the `[job]` group
/// exactly as `completion_notice` writes it, so the head's compact job rows parse it. Written to
/// the head's fixture and compared byte for byte, as `job_notice_*.txt` are.
#[test]
fn the_meanwhile_notice_is_the_heads_fixture() {
    let batch = vec![
        JobCompletion {
            slug: Some("release-build".into()),
            command: "cargo build --release".into(),
            produced: 1008,
            elapsed_ms: 174_000,
            ..settled("j1042", "exited 0", SystemTime::UNIX_EPOCH)
        },
        JobCompletion {
            command: "cargo test -p letibot-tui".into(),
            produced: 0,
            elapsed_ms: 4_400,
            ..settled("j1043", "exited 101", SystemTime::UNIX_EPOCH)
        },
    ];
    // Both headings the head is handed: the batch that rode a turn started for another reason, and the batch delivered
    // the moment the turn it was held through ended.
    for (rule, fixture) in [
        (crate::delivery::Rule::Batch, "job_notice_meanwhile.txt"),
        (
            crate::delivery::Rule::TurnEnd,
            "job_notice_meanwhile_turn_end.txt",
        ),
    ] {
        let text = meanwhile_notice(
            batch.clone(),
            "s-r79",
            rule,
            crate::delivery::HELD_RESULTS_CAP,
            &[],
        );
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tui/src/app/tests/fixtures")
            .join(fixture);
        if std::env::var_os("LETIBOT_BLESS_JOB_NOTICE").is_some() {
            std::fs::write(&path, &text).unwrap();
        }
        let pinned = std::fs::read_to_string(&path).expect("the head's fixture exists");
        assert_eq!(
            text, pinned,
            "the [meanwhile] notice changed; regenerate the head's fixture {fixture} with \
             LETIBOT_BLESS_JOB_NOTICE=1 and update the head's parser with it"
        );
    }
}

/// The pg-noop board for the grace tests: the operator's row and the model's CRITERION row, both
/// attached with `wait_on` to the gate job `j12`, and one row that waits on nothing.
fn waiting_board() -> Vec<TodoItem> {
    let wait = || {
        vec![TodoNeed::Job {
            handle: "j12".into(),
        }]
    };
    vec![
        TodoItem {
            content: "make tier 1 green".into(),
            status: TodoStatus::InProgress,
            by: TodoBy::Operator,
            when: None,
            needs: wait(),
        },
        TodoItem {
            content: "CRITERION, NOT A TASK — stays open until the gate is green".into(),
            status: TodoStatus::InProgress,
            by: TodoBy::Model,
            when: None,
            needs: wait(),
        },
        TodoItem {
            content: "write the notes".into(),
            status: TodoStatus::Completed,
            by: TodoBy::Model,
            when: None,
            needs: Vec::new(),
        },
    ]
}

/// What the end-of-turn check may ask about, read the way `Harness::running_jobs` reads it.
fn askable(board: &[TodoItem], w: &JobWatchers, cap: Duration, now: SystemTime) -> Vec<String> {
    let jobs = graced_endings(
        letibot_tools::builtins::todo::RunningJobs::none(),
        board,
        w,
        cap,
        now,
    );
    crate::sessions::the_plan_as_checked(
        board,
        &letibot_tools::builtins::todo::ChildSessions::none(),
        &jobs,
    )
    .into_iter()
    .map(|r| r.content)
    .collect()
}

/// The grace after a wait ends, step by step: quiet until the model has READ the completion and finished a turn after
/// the one it arrived in. Ended and still queued is quiet, delivered at a round boundary and then that turn ends is
/// quiet, and the NEXT turn ends is askable. The fact guarded is the delivery, not a timer.
#[test]
fn a_wait_ons_grace_lasts_until_the_turn_after_the_completion_was_read() {
    let dir = Scratch::new("grace-steps");
    let hub = Hub::new("s-r79");
    let w = watchers(&hub, false);
    let board = waiting_board();
    let cap = Duration::from_secs(600);
    let mut s = steering(&hub, deliveries(&w, board.clone(), &dir, cap));
    assert_eq!(
        askable(&board, &w, cap, SystemTime::now()).len(),
        2,
        "before the job ends nothing is graced (the running-job quiet is the harness's other read)"
    );

    // 1. The job ends; its completion is queued and not yet read.
    w.queue_for_test(settled("j12", "exited 0", SystemTime::now()));
    assert!(
        askable(&board, &w, cap, SystemTime::now()).is_empty(),
        "ended but undelivered: both waiting rows are quiet"
    );

    // 2. Delivered at the running turn's round boundary, and that turn ends.
    let got = boundary(&mut s).expect("a needed job is immediate");
    assert!(got.text.contains("`j12` exited 0"), "{}", got.text);
    assert!(
        askable(&board, &w, cap, SystemTime::now()).is_empty(),
        "delivered mid-turn: quiet"
    );
    w.turn_ended(TurnEnd::Done);
    assert!(
        askable(&board, &w, cap, SystemTime::now()).is_empty(),
        "at the end of the delivering turn the model has only just read it: still quiet"
    );

    // 3. The next turn ends without the rows being moved: they are the check's again.
    w.turn_ended(TurnEnd::Done);
    assert_eq!(
        askable(&board, &w, cap, SystemTime::now()),
        vec![
            "make tier 1 green".to_string(),
            "CRITERION, NOT A TASK — stays open until the gate is green".to_string()
        ],
        "a whole turn after the read, the rows are askable"
    );
}

/// A result HELD for the boundary is undelivered too, and quiet - and so is a completion delivered as
/// a wake's own turn start, until the turn after it. The held queue is the other place an ended job
/// can sit unread.
#[test]
fn a_held_or_wake_delivered_completion_is_graced_the_same_way() {
    let dir = Scratch::new("grace-held");
    let hub = Hub::new("s-r79");
    let w = watchers(&hub, false);
    let board = waiting_board();
    let cap = Duration::from_secs(600);
    let d = deliveries(&w, board.clone(), &dir, cap);
    w.hold(vec![settled("j12", "exited 0", SystemTime::now())]);
    assert!(
        askable(&board, &w, cap, SystemTime::now()).is_empty(),
        "held, not delivered: quiet"
    );
    // Delivered as the opening of the next turn the model takes (the batch rides in with it).
    let note = d.at_turn_start().expect("the batch");
    assert!(note.contains("`j12` exited 0"), "{note}");
    w.turn_ended(TurnEnd::Done);
    assert!(
        askable(&board, &w, cap, SystemTime::now()).is_empty(),
        "the turn it opened has ended: still quiet"
    );
    w.turn_ended(TurnEnd::Done);
    assert_eq!(askable(&board, &w, cap, SystemTime::now()).len(), 2);
}

/// The backstop: a grace cannot outlive the starvation cap, so a completion never delivered cannot silence a row for
/// ever. Measured from the ending, and the idle clock is armed for the moment it runs out, since a model that went idle
/// takes no turn that would re-arm it.
#[test]
fn the_grace_has_a_backstop_and_the_clock_is_armed_for_it() {
    let hub = Hub::new("s-r79");
    let w = watchers(&hub, false);
    let board = waiting_board();
    let cap = crate::delivery::HELD_RESULTS_CAP;
    let ended = SystemTime::now() - Duration::from_secs(4 * 60);
    w.queue_for_test(settled("j12", "exited 0", ended));
    let now = SystemTime::now();
    assert!(askable(&board, &w, cap, now).is_empty(), "four minutes in");

    // The clock: nothing askable, so it is armed for the backstop's end - about six minutes out.
    let jobs = graced_endings(
        letibot_tools::builtins::todo::RunningJobs::none(),
        &board,
        &w,
        cap,
        now,
    );
    let t0 = std::time::Instant::now();
    let mut clock = crate::sessions::TodoNagClock::with_window(Duration::from_secs(60));
    clock.rearm(
        &board,
        t0,
        &letibot_tools::builtins::todo::ChildSessions::none(),
        &jobs,
    );
    let due = clock.due_at().expect("a deadline for the grace's end");
    let left = due - t0;
    assert!(
        left > Duration::from_secs(5 * 60) && left <= Duration::from_secs(6 * 60),
        "armed for the backstop: {left:?}"
    );

    // Past the cap, still undelivered: askable.
    assert_eq!(
        askable(&board, &w, cap, ended + cap + Duration::from_secs(1)).len(),
        2,
        "the backstop releases an undelivered ending"
    );

    // And a read completion is released by it too, without a second turn end.
    let w2 = watchers(&hub, false);
    w2.read_now(&[settled("j12", "exited 0", ended)]);
    assert!(askable(&board, &w2, cap, now).is_empty());
    assert_eq!(
        askable(&board, &w2, cap, ended + cap + Duration::from_secs(1)).len(),
        2
    );
}

/// The completion names the rows that wait on it, on both halves of the board, and says nothing when none do: the
/// gate's completion does not arrive while the row attached to it stays `in_progress`. A DONE row is not named, since
/// it owes nothing.
#[test]
fn the_completion_names_the_rows_waiting_on_it() {
    let board = waiting_board();
    let job = settled("j12", "exited 0", SystemTime::UNIX_EPOCH);
    let said = completion_notices(vec![job.clone()], "s-r79", &board).join("\n\n");
    assert!(
        said.ends_with(
            "\nWaiting on this: \"make tier 1 green\" (the operator's), \"CRITERION, NOT A TASK — \
             stays open until the gate is green\" (yours) — update them with `todo_write` now that \
             it has ended."
        ),
        "{said}"
    );
    // The head still reads it: the job card parses, and the line survives the fold.
    assert!(said.starts_with("[job] a job you backgrounded has ended:"));

    // Nobody waits on j13: not a word.
    let other = settled("j13", "exited 0", SystemTime::UNIX_EPOCH);
    let plain = completion_notices(vec![other], "s-r79", &board).join("\n\n");
    assert!(!plain.contains("Waiting on"), "{plain}");

    // A subagent, by its session id, with a long row cut like the check cuts it.
    let long = "x".repeat(120);
    let rows = vec![TodoItem {
        content: long.clone(),
        status: TodoStatus::Pending,
        by: TodoBy::Model,
        when: None,
        needs: vec![TodoNeed::Child {
            id: "s-r79-sub-2".into(),
        }],
    }];
    let child = JobCompletion {
        kind: BackgroundKind::Subagent,
        job: "s-r79-sub-2".into(),
        command: String::new(),
        state: "done".into(),
        detail: "all green".into(),
        ..settled("s-r79-sub-2", "done", SystemTime::UNIX_EPOCH)
    };
    let said = completion_notices(vec![child], "s-r79", &rows).join("\n\n");
    let want = format!(
        "\nWaiting on this: \"{}…\" (yours) — update it with `todo_write` now that it has \
         finished.",
        "x".repeat(79)
    );
    assert!(said.ends_with(&want), "{said}");

    // Two completions in one group: each row says which handle it waits on.
    let two = completion_notices(
        vec![job, settled("j14", "exited 0", SystemTime::UNIX_EPOCH)],
        "s-r79",
        &board,
    )
    .join("\n\n");
    assert!(
        two.contains("\nWaiting on these: \"make tier 1 green\" (the operator's, on `j12`)"),
        "{two}"
    );
    assert!(two.ends_with("now that they have ended."), "{two}");
}

/// A `when: job` firing is not delayed by the grace: the grace is for the CHECK, which must not ask about a row before
/// the model has digested the ending, while a condition firing is the news itself. A row that both waits on `j12` and
/// fires on it would otherwise hear its condition a turn late.
#[test]
fn a_when_job_firing_is_not_held_back_by_the_grace() {
    let mut row = waiting_board().remove(1);
    row.when = Some(TodoCondition::Job {
        handle: "j12".into(),
    });
    let rows = vec![row];
    let graced = letibot_tools::builtins::todo::RunningJobs::none()
        .digesting("j12", Duration::from_secs(300));
    let due = due_rows(
        &rows,
        &letibot_tools::builtins::todo::ChildSessions::none(),
        &graced,
        |_| JobFate::Unknown,
    );
    assert_eq!(due.len(), 1, "the condition fires with the completion");
}

/// A want does not outlive the delivery of its result, by any door: the `wanted` map was cleared only on IMMEDIATE
/// delivery, so a want recorded while the result was held, and then handed over in the batch, stayed for the session's
/// life.
#[test]
fn a_want_is_forgotten_when_its_result_is_delivered_in_a_batch_or_already_was() {
    let dir = Scratch::new("r79-wanted");
    let hub = Hub::new("s-r79");
    let w = watchers(&hub, true);
    let d = deliveries(&w, Vec::new(), &dir, Duration::from_secs(600));
    let mut s = steering(&hub, d.clone());

    w.queue_for_test(settled("j1", "exited 0", SystemTime::now()));
    assert!(boundary(&mut s).is_none(), "a routine completion is held");
    assert_eq!(w.held_count(), 1);
    // Wanted only now, while it is held - and then handed over with the batch.
    w.want_now("j1", WantedNow::Awaited);
    assert!(d.batch(crate::delivery::Rule::Batch).is_some());
    assert_eq!(
        w.wanted("j1"),
        None,
        "the batch delivered it and the want stayed"
    );

    // A want for a result the model has already been handed is not recorded at all.
    w.want_now("j1", WantedNow::WakeNow);
    assert_eq!(
        w.wanted("j1"),
        None,
        "a want for a delivered result was kept"
    );
}

/// A batch put back is results the model was never told, and nothing rings for it again: its ring was spent by the turn that could not say it, and a stopped or failed turn takes its wake back.
/// Putting it back must ring, or the session sits on `waiting on: delivery`.
#[test]
fn a_batch_put_back_rings_the_session_that_owes_it() {
    use letibot_sessionlog::registry::{Registry, SessionWiring, Work, WorkOrIdle};

    let dir = Scratch::new("r79-put-back");
    let registry = Registry::new();
    let hub = registry
        .create("s-r79", "", SessionWiring::default())
        .expect("the session is in the registry");
    // The create rang an Open; the next ring is the one this test is about.
    assert!(matches!(registry.next_work(), Some(Work::Open(_))));
    let w = JobWatchers::watching_tasks(&hub, Some(Arc::clone(registry.bell())));
    let d = Deliveries::new(
        Arc::clone(&w),
        None,
        DeliveryLog::under(&dir.0, "s-r79"),
        Duration::from_secs(600),
        "s-r79",
    );

    d.put_back(vec![settled("j1", "exited 0", SystemTime::now())]);
    assert_eq!(w.held_count(), 1, "the result was not held again");
    // And the door's turn ends as the refused one did: a stopped turn takes back a SETTLEMENT's
    // wake, and this is not one - nothing queues a second copy of what is held.
    w.turn_ended(TurnEnd::Interrupted);

    assert!(
        matches!(
            registry.next_work_until(Some(std::time::Instant::now())),
            WorkOrIdle::Work(Work::Woken(id)) if id == "s-r79"
        ),
        "nothing rings for the results this session still owes"
    );
}
