//! **R79 at the round boundary and the turn's start** — what a running turn is handed, and what
//! waits for the next break. The decision itself is `crate::delivery`'s and asserted there; these
//! drive it through the steering source a turn actually polls, with a watcher set, a board and a
//! record on disk.

use super::*;

use crate::delivery::{DeliveryLog, LOG};
use letibot_tokencore::store::{TodoBy, TodoCondition, TodoNeed, TodoStatus};
use letibot_tools::runtime::WantedNow;
use std::time::{Duration, SystemTime};

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
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
    }
}

/// A session's watcher set — a ROOT's when `child` is false, a child's (joined to a parent's
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
        held: VecDeque::new(),
    }
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

/// **A routine completion during a long turn is held, and the batch arrives as ONE note at the
/// boundary** — the operator's point 1. A CHILD is the session that used to hear every
/// settlement at its next round boundary; now neither of two routine ones reaches it mid-turn,
/// and the next turn it takes opens with both under one `[meanwhile]` line, recorded as a batch
/// of two with the times they arrived.
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
        s.try_next().is_none(),
        "a routine completion interrupted a running turn"
    );
    w.queue_for_test(settled("j2", "exited 0", SystemTime::now()));
    assert!(s.try_next().is_none(), "the second one did too");
    assert_eq!(w.held_count(), 2, "both are held, not dropped");
    assert!(dir.records().is_empty(), "nothing has been delivered yet");

    let note = d.at_turn_start().expect("the next turn carries the batch");
    assert!(
        note.starts_with("[meanwhile] 2 results arrived while you were busy"),
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

/// **A `when: job` row's job is delivered at once — to a ROOT too**, at the round boundary of the
/// turn that is running. (Before R79 a root heard nothing mid-turn: its every settlement waited
/// for the turn to end and then cost a wake turn.) A routine one beside it stays held.
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
    let got = s.try_next().expect("the plan waits on j12");
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

/// **The failure of a needed job is immediate** — named by the slug the row used — and recorded
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
    let got = s.try_next().expect("a broken dependency stops the work");
    assert!(
        got.text.contains("`release-build · j40` exited 101"),
        "{}",
        got.text
    );
    assert_eq!(dir.records()[0]["rule"], "immediate:dependency_failed");
}

/// **`wake: "now"` is immediate**, and so is a result the model asked for with a wait after it
/// had been held — the sort re-reads what is held against what is wanted now.
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
    let got = s.try_next().expect("wake: now");
    assert!(got.text.contains("`j5`"), "{}", got.text);

    w.queue_for_test(settled("j6", "exited 0", SystemTime::now()));
    assert!(s.try_next().is_none(), "j6 is routine and held");
    w.want_now("j6", WantedNow::Awaited);
    let got = s
        .try_next()
        .expect("a job_wait on a held result promotes it");
    assert!(got.text.contains("`j6`"), "{}", got.text);
    assert_eq!(w.held_count(), 0);

    let rules: Vec<String> = dir
        .records()
        .iter()
        .map(|r| r["rule"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(rules, ["immediate:wake_now", "immediate:awaited"]);
}

/// **The starvation cap**: a turn running with a result held past the (here shortened) cap gets
/// the batch at its next round boundary anyway, as one note — and not before.
#[test]
fn the_starvation_cap_releases_the_batch_mid_turn() {
    let dir = Scratch::new("r79-cap");
    let hub = Hub::new("s-r79");
    let w = watchers(&hub, false);
    let cap = Duration::from_millis(300);
    let mut s = steering(&hub, deliveries(&w, Vec::new(), &dir, cap));

    w.queue_for_test(settled("j7", "exited 0", SystemTime::now()));
    w.queue_for_test(settled("j8", "exited 1", SystemTime::now()));
    assert!(s.try_next().is_none(), "inside the cap, nothing comes in");
    std::thread::sleep(cap + Duration::from_millis(50));
    let got = s.try_next().expect("past the cap, the batch is released");
    assert!(
        got.text
            .starts_with("[meanwhile] 2 results held for over 300ms while this turn ran"),
        "{}",
        got.text
    );
    assert!(
        got.text.contains("`j7`") && got.text.contains("`j8`"),
        "{}",
        got.text
    );
    assert!(s.try_next().is_none(), "one note, once");
    let rec = dir.records();
    assert_eq!(rec.len(), 2);
    assert!(
        rec.iter()
            .all(|r| r["rule"] == "starvation" && r["batch"] == 2),
        "{rec:?}"
    );
}

/// **The batch's own words are pinned for the head**: `[meanwhile] …` and then the `[job]` group
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
    let text = meanwhile_notice(
        batch,
        "s-r79",
        crate::delivery::Rule::Batch,
        crate::delivery::HELD_RESULTS_CAP,
    );
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../tui/src/app/tests/fixtures/job_notice_meanwhile.txt");
    if std::env::var_os("LETIBOT_BLESS_JOB_NOTICE").is_some() {
        std::fs::write(&path, &text).unwrap();
    }
    let pinned = std::fs::read_to_string(&path).expect("the head's fixture exists");
    assert_eq!(
        text, pinned,
        "the [meanwhile] notice changed; regenerate the head's fixture with \
         LETIBOT_BLESS_JOB_NOTICE=1 and update the head's parser with it"
    );
}
