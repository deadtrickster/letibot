//! `queue` through the bridge: an agent enqueues a pushed branch and reads its row back from the
//! store the queue serves, with no daemon and no session.

use std::path::{Path, PathBuf};
use std::process::Command;

use letibot_bridge::tools::{self, Bridge};
use letibot_tokencore::store::{ReviewState, Store};
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

#[test]
fn a_pushed_branch_is_enqueued_and_its_row_reads_back() {
    let dir =
        Dir(std::env::temp_dir().join(format!("letibot-bridge-queue-{}", std::process::id())));
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
