//! `queue` through the bridge: an agent enqueues a pushed branch and reads its row back from the
//! store the queue serves, moves it with the operator's verbs and waits on it, with no daemon and
//! no session.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use letibot_bridge::queue_desk;
use letibot_bridge::tools::{self, Bridge};
use letibot_tokencore::store::{MergeState, Move, ReviewState, Store};
use serde_json::json;

fn sh(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

struct Dir(PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A repository with a pushed `main`, an unpushed branch `ik/work`, and the daemon's store.
fn fixture(tag: &str) -> (Dir, PathBuf, PathBuf, Bridge) {
    let dir = Dir(
        std::env::temp_dir().join(format!("letibot-bridge-queue-{tag}-{}", std::process::id()))
    );
    let _ = std::fs::remove_dir_all(&dir.0);
    let repo = dir.0.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    sh(&repo, &["init", "-q", "-b", "main"]);
    letibot_tools::testing::repo_config(&repo);
    std::fs::write(
        repo.join("AGENTS.md"),
        "# agents\n\n## Merge gate\n\n```sh\ntrue\n```\n",
    )
    .unwrap();
    sh(&repo, &["add", "."]);
    sh(&repo, &["commit", "-q", "-m", "base"]);
    let remote = dir.0.join("remote.git");
    sh(&dir.0, &["init", "-q", "--bare", &remote.to_string_lossy()]);
    sh(
        &repo,
        &["remote", "add", "origin", &remote.to_string_lossy()],
    );
    sh(&repo, &["push", "-q", "origin", "main"]);
    sh(&repo, &["checkout", "-q", "-b", "ik/work"]);
    std::fs::write(repo.join("work.txt"), "work\n").unwrap();
    sh(&repo, &["add", "."]);
    sh(&repo, &["commit", "-q", "-m", "work"]);
    sh(&repo, &["checkout", "-q", "main"]);
    let repo = std::fs::canonicalize(&repo).unwrap();

    let store_at = dir.0.join("sessions.db");
    drop(Store::open(&store_at).expect("the daemon's store"));
    let bridge = Bridge::new().with_names(&store_at, &repo);

    (dir, repo, store_at, bridge)
}

fn enqueued(tag: &str) -> (Dir, PathBuf, Bridge, String) {
    let (dir, repo, store_at, bridge) = fixture(tag);
    sh(&repo, &["push", "-q", "origin", "ik/work"]);
    let put = tools::call(
        &bridge,
        "queue",
        &json!({"action": "enqueue", "branch": "ik/work", "why": "the work"}),
    )
    .expect("enqueued");
    let id = put["id"].as_str().expect("an id").to_string();
    (dir, store_at, bridge, id)
}

#[test]
fn a_pushed_branch_is_enqueued_and_its_row_reads_back() {
    let (_dir, repo, store_at, bridge) = fixture("enqueue");
    let unpushed = tools::call(
        &bridge,
        "queue",
        &json!({"action": "enqueue", "branch": "ik/work", "why": "the work"}),
    )
    .expect_err("an unpushed branch is refused");
    assert!(unpushed.contains("not pushed"), "{unpushed}");

    sh(&repo, &["push", "-q", "origin", "ik/work"]);
    let put = tools::call(
        &bridge,
        "queue",
        &json!({"action": "enqueue", "branch": "ik/work", "why": "the work"}),
    )
    .expect("enqueued");
    let id = put["id"].as_str().expect("an id").to_string();
    assert_eq!(put["target"], "main", "{put}");
    assert_eq!(put["state"], "waiting", "{put}");

    let row = Store::open(&store_at)
        .unwrap()
        .review_row(&id)
        .unwrap()
        .expect("the row is in the queue's store");
    assert_eq!(row.state, ReviewState::Waiting);
    assert_eq!(row.brief, "the work");
    assert!(row.session_id.starts_with("bridge:"), "{row:?}");

    let read = tools::call(&bridge, "queue", &json!({"action": "status", "id": id}))
        .expect("the row reads back");
    assert_eq!(read["review"]["state"], "waiting", "{read}");
    assert_eq!(read["review"]["branch"], "ik/work", "{read}");
    assert!(read["landing"].is_null(), "not approved yet: {read}");

    let all = tools::call(&bridge, "queue", &json!({"action": "status"})).expect("the queue");
    assert!(
        all["status"].as_str().unwrap_or_default().contains(&id),
        "{all}"
    );

    let none = tools::call(&bridge, "queue", &json!({"action": "status", "id": "nope"}))
        .expect_err("an unknown id");
    assert!(none.contains("`nope`"), "{none}");
}

fn verb(bridge: &Bridge, args: serde_json::Value) -> Result<serde_json::Value, String> {
    tools::call(bridge, "leticode_queue_verb", &args)
}

#[test]
fn the_queue_verbs_move_a_row_as_the_bridge() {
    let (_dir, store_at, bridge, id) = enqueued("verbs");
    let e = verb(&bridge, json!({"verb": "approve"})).expect_err("no id");
    assert!(e.contains("`approve` needs `id`"), "{e}");
    let e = verb(&bridge, json!({"verb": "merge", "id": id})).expect_err("no such verb");
    assert!(e.contains("`merge`"), "{e}");

    let said = verb(&bridge, json!({"verb": "veto", "id": id, "why": "too big"})).expect("vetoed");
    assert!(said["said"].as_str().unwrap().contains("vetoed"), "{said}");
    let store = Store::open(&store_at).unwrap();
    let row = store.review_row(&id).unwrap().unwrap();
    assert_eq!(row.state, ReviewState::Rejected);
    assert!(
        row.evidence.contains("vetoed by the bridge: too big"),
        "{row:?}"
    );
    assert!(
        row.verdict_by
            .as_deref()
            .unwrap_or_default()
            .starts_with("bridge:"),
        "{row:?}"
    );

    verb(&bridge, json!({"verb": "restart", "id": id})).expect("restarted");
    assert_eq!(
        store.review_row(&id).unwrap().unwrap().state,
        ReviewState::Waiting
    );

    // The bridge enqueued it, so the operator approves it.
    operator_approves(&store, &id);
    let landing = store.merge_rows(None).unwrap();
    assert_eq!(landing.len(), 1, "{landing:?}");
    assert!(
        landing[0].approved_by.starts_with("operator:"),
        "{landing:?}"
    );

    verb(&bridge, json!({"verb": "rm", "id": landing[0].id})).expect("withdrawn");
    assert_eq!(
        store.merge_rows(None).unwrap()[0].state,
        MergeState::Withdrawn
    );
    let said = verb(&bridge, json!({"verb": "clean"})).expect("nothing parked");
    assert!(
        said["said"].as_str().unwrap().starts_with("0 parked"),
        "{said}"
    );
}

/// The operator's approval of a row the bridge enqueued: the bridge may not give it.
fn operator_approves(store: &Store, id: &str) {
    letibot_harnessd::mergequeue::verbs::approve(store, id, "s-op", &|_| {}).expect("approved");
}

/// The seat that enqueued a branch cannot approve it over the reviewer, nor can another bridge
/// seat: a row the bridge enqueued is the operator's to approve.
#[test]
fn bridge_cannot_approve_own_enqueue() {
    let (_dir, store_at, bridge, id) = enqueued("self-approve");
    let e = verb(&bridge, json!({"verb": "approve", "id": id})).expect_err("its own branch");
    assert!(e.contains("approved by the operator"), "{e}");
    let store = Store::open(&store_at).unwrap();
    assert_eq!(
        store.review_row(&id).unwrap().unwrap().state,
        ReviewState::Waiting
    );
    assert!(store.merge_rows(None).unwrap().is_empty());
}

#[test]
fn a_row_of_another_repository_is_refused() {
    let (_a, store_at, _bridge, id) = enqueued("scope-a");
    let (_b, other, _, _) = fixture("scope-b");
    let elsewhere = Bridge::new().with_names(&store_at, &other);
    let e = verb(&elsewhere, json!({"verb": "veto", "id": id})).expect_err("not this repository's");
    assert!(e.contains("is a row of"), "{e}");
}

const FAST: Duration = Duration::from_millis(20);

fn wait(bridge: &Bridge, id: &str, within: Duration) -> (i32, serde_json::Value) {
    let (code, said) =
        queue_desk::wait(bridge, &bridge.folder(), id, Instant::now() + within, FAST);
    let state = serde_json::from_str(&said).unwrap_or(json!(said));
    (code, state)
}

#[test]
fn wait_queue_times_out_on_a_row_still_in_line() {
    let (_dir, _, bridge, id) = enqueued("wait-timeout");
    let started = Instant::now();
    let (code, state) = wait(&bridge, &id, Duration::from_millis(150));
    assert_eq!(code, queue_desk::exit::TIMEOUT, "{state}");
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(state["review"]["state"], "waiting", "{state}");

    let (code, said) = queue_desk::wait(&bridge, &bridge.folder(), "nope", Instant::now(), FAST);
    assert_eq!(code, queue_desk::exit::UNKNOWN);
    assert!(said.contains("`nope`"), "{said}");
}

#[test]
fn wait_queue_exits_10_on_a_veto_with_the_words() {
    let (_dir, store_at, bridge, id) = enqueued("wait-veto");
    let waiter = {
        let bridge = Bridge::new().with_names(&store_at, bridge.folder());
        let id = id.clone();
        std::thread::spawn(move || wait(&bridge, &id, Duration::from_secs(20)))
    };
    std::thread::sleep(Duration::from_millis(100));
    verb(
        &bridge,
        json!({"verb": "veto", "id": id, "why": "split it"}),
    )
    .expect("vetoed");
    let (code, state) = waiter.join().unwrap();
    assert_eq!(code, queue_desk::exit::REJECTED, "{state}");
    assert_eq!(state["review"]["state"], "rejected", "{state}");
    assert!(
        state["review"]["reasons"].to_string().contains("split it"),
        "{state}"
    );
}

#[test]
fn wait_queue_follows_an_approval_to_its_landing() {
    let (_dir, store_at, bridge, id) = enqueued("wait-land");
    let store = Store::open(&store_at).unwrap();
    operator_approves(&store, &id);
    let m = store.merge_rows(None).unwrap().remove(0);
    let (code, state) = wait(&bridge, &m.id, Duration::from_millis(100));
    assert_eq!(
        code,
        queue_desk::exit::TIMEOUT,
        "queued is in line: {state}"
    );
    assert_eq!(state["landing"]["state"], "queued", "{state}");

    assert!(
        store
            .move_merge(
                &m.id,
                MergeState::Queued,
                MergeState::Landed,
                &Move::new("test", "landed"),
                2
            )
            .unwrap()
    );
    let (code, state) = wait(&bridge, &id, Duration::from_secs(5));
    assert_eq!(code, queue_desk::exit::LANDED, "{state}");
    assert_eq!(state["landing"]["state"], "landed", "{state}");
}

#[test]
fn wait_queue_exits_20_on_a_red_gate() {
    let (_dir, store_at, bridge, id) = enqueued("wait-red");
    let store = Store::open(&store_at).unwrap();
    operator_approves(&store, &id);
    let m = store.merge_rows(None).unwrap().remove(0);
    assert!(
        store
            .move_merge(
                &m.id,
                MergeState::Queued,
                MergeState::Failed,
                &Move::new("test", "red"),
                2
            )
            .unwrap()
    );
    let (code, state) = wait(&bridge, &id, Duration::from_secs(5));
    assert_eq!(code, queue_desk::exit::FAILED, "{state}");
    assert_eq!(state["landing"]["state"], "failed", "{state}");
}
