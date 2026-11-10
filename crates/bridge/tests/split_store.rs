//! A bridge over a split store (`docs/store-split.md`): the daemon is handed a DIRECTORY whose
//! `shared.db` and `projects/<key>.db` the split made, and every read here has to land in the
//! file the table's rows are in rather than opening the directory as one sqlite file.

use std::path::{Path, PathBuf};
use std::sync::Once;

use letibot_bridge::tools::{self, Bridge};
use letibot_bridge::{names, queue};
use letibot_tokencore::migrate_split::project_key;
use letibot_tokencore::store::split::{PROJECTS_DIR, SHARED_FILE};
use letibot_tokencore::store::{MergePriority, NewReview, ReviewKind, SessionRecord, Store};
use serde_json::json;

/// The handle every test in this file writes under: the bridge's room name, process-wide.
fn seat_handle() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SAFETY: called before any test reads the handle, and only once in this process.
        unsafe {
            std::env::set_var(letibot_bridge::rooms::NAME_ENV, "cc@split-store");
            std::env::remove_var("LETIBOT_STORE");
            std::env::remove_var("LETIBOT_SOCKET");
        }
    });
}

/// A directory of its own per test, removed when it ends.
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A split data directory for `folder`, made through the daemon's own door so the fixture cannot
/// disagree with the split about what a set is. The folder is absolute and need not exist: the
/// key is its path's hash, which is all a project file is named by.
fn split_store(name: &str, folder: &Path) -> (TempDir, PathBuf) {
    let dir = TempDir(std::env::temp_dir().join(format!(
        "letibot-bridge-split-{name}-{}",
        std::process::id()
    )));
    let _ = std::fs::remove_dir_all(&dir.0);
    std::fs::create_dir_all(&dir.0).expect("a scratch dir");
    Store::open_set(&dir.0, &key_of(folder)).expect("a split set, as a daemon's first start makes");
    let path = dir.0.clone();
    (dir, path)
}

fn key_of(folder: &Path) -> String {
    project_key(&folder.to_string_lossy())
}

fn put_session(s: &Store, id: &str, workspace: &str, title: &str) {
    s.put_session(&SessionRecord {
        id: id.into(),
        title: Some(title.into()),
        model_id: "m".into(),
        dialect_sha: "d".into(),
        workspace_root: workspace.into(),
        owner: "op".into(),
        approvers: vec![],
        role: None,
        parent_session_id: None,
    })
    .expect("session");
}

/// The board is a seat's, not a session's, and every head reads it: the row goes to `shared.db`,
/// and the project file does not even hold the table the board lives in.
#[test]
fn a_seat_board_writes_to_a_split_store() {
    seat_handle();
    let folder = Path::new("/w/split/here");
    let (_scratch, dir) = split_store("board", folder);
    let bridge = Bridge::new().with_names(dir.clone(), folder.to_path_buf());
    let wrote = tools::call(
        &bridge,
        "todo_write",
        &json!({"todos": [{"content": "read the split", "status": "in_progress"}]}),
    )
    .expect("the write is taken");
    assert_eq!(wrote["stored"], json!(true), "{wrote}");

    let shared = Store::open(&dir.join(SHARED_FILE)).expect("the shared file");
    let rows = shared.seat_todos("cc@split-store").expect("the board");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].content, "read the split");

    let project = Store::open(
        &dir.join(PROJECTS_DIR)
            .join(format!("{}.db", key_of(folder))),
    )
    .expect("the project file");
    assert!(
        project.seat_todos("cc@split-store").is_err(),
        "a board is in the shared file and the project file has no such table"
    );
}

/// The shape the daemon is handed while the store has not been split yet: a directory whose one
/// file is `sessions.db`. The board lands in it, and no `shared.db` is made beside it.
#[test]
fn an_unsplit_directory_reads_its_sessions_db() {
    seat_handle();
    let dir = TempDir(
        std::env::temp_dir().join(format!("letibot-bridge-unsplit-{}", std::process::id())),
    );
    let _ = std::fs::remove_dir_all(&dir.0);
    std::fs::create_dir_all(&dir.0).expect("a scratch dir");
    Store::open(&dir.0.join("sessions.db")).expect("the file a daemon's first start makes");
    let folder = PathBuf::from("/w/unsplit/here");
    let bridge = Bridge::new().with_names(dir.0.clone(), folder);
    let wrote = tools::call(
        &bridge,
        "todo_write",
        &json!({"todos": [{"content": "one file, still", "status": "in_progress"}]}),
    )
    .expect("the write is taken");
    assert_eq!(wrote["stored"], json!(true), "{wrote}");

    let one = Store::open(&dir.0.join("sessions.db")).expect("the single file");
    assert_eq!(
        one.seat_todos("cc@split-store").expect("the board").len(),
        1
    );
    assert!(
        !dir.0.join(SHARED_FILE).exists(),
        "a bridge never splits a store, and never makes one"
    );
}

/// The session index is the shared file's, so a name resolves without opening a project file.
#[test]
fn a_session_name_resolves_from_a_split_stores_shared_file() {
    let folder = PathBuf::from("/w/split/names");
    let (_scratch, dir) = split_store("names", &folder);
    {
        let shared = Store::open(&dir.join(SHARED_FILE)).expect("the shared file");
        put_session(&shared, "s-100", "/w/split/names", "Stroppy Conformity");
        put_session(&shared, "s-200", "/w/split/other", "Stroppy Conformity");
    }
    let here = vec![folder.clone()];
    assert_eq!(
        names::resolve(&dir, &here, "stroppy-conformity").expect("a name reads"),
        "s-100"
    );
}

/// The queue view follows `queue_transition`, which the split keeps in the shared file: a folder
/// whose daemon serves the queue is not always the folder the row is keyed by.
#[test]
fn the_queue_view_says_what_moved_in_a_split_store() {
    const REPO: &str = "/w/letibot";
    let folder = PathBuf::from("/w/split/queue");
    let (_scratch, dir) = split_store("queue", &folder);
    let mover = queue::Mover::new(dir.clone(), REPO, dir.join("cursor"));
    // Followed before anything moved: a follower says what happens from here, so the enqueue
    // below is the first thing it can be told about.
    assert!(
        mover.step().expect("a first pass").is_empty(),
        "nothing has moved yet"
    );
    {
        let set = Store::open_set(&dir, &key_of(&folder)).expect("the set");
        let enqueued = set
            .enqueue_review(
                &NewReview {
                    id: "r-1".into(),
                    repository: REPO.into(),
                    target: "main".into(),
                    kind: ReviewKind::Branch,
                    session_id: "s-child".into(),
                    branch: "agent/r-1".into(),
                    worktree: None,
                    base_sha: "base".into(),
                    head_sha: "head1".into(),
                    brief: "do the thing".into(),
                    priority: MergePriority::Subagent,
                    fixes: None,
                    evidence: "the child finished".into(),
                },
                "me",
                1,
            )
            .expect("the enqueue");
        assert!(enqueued, "the row was new");
    }

    let notes = mover.step().expect("a step over a split store");
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert_eq!(notes[0]["params"]["meta"]["event"], "enqueued");
    assert_eq!(notes[0]["params"]["meta"]["repository"], REPO);
}
