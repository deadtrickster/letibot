//! Carving one project at a time out of `sessions.db` (`docs/store-split.md` § Carving one
//! project): one key's file is written and verified, the key is recorded, and only then are its
//! rows deleted - so every other project's daemon keeps running. Then `--finish` renames what is
//! left, once nothing but the shared tables is in it.
//!
//! Every path here is under `$TMPDIR`: the carve deletes rows, and it is only ever pointed at a
//! store a test built.

use std::path::{Path, PathBuf};
use std::process::Command;

use letibot_harnessd::config::{Config, StoreAt};
use letibot_tokencore::ledger::TokenLedger;
use letibot_tokencore::migrate_split::{self, Carved};
use letibot_tokencore::rusqlite::{Connection, params};
use letibot_tokencore::store::split::{LEGACY_FILE, PROJECTS_DIR, SHARED_FILE};
use letibot_tokencore::store::{NewAdjudication, SessionRecord, StablePrefixRecord, Store};
use letibot_transcript::TranscriptItem;
use sha2::{Digest, Sha256};

/// Two folders, one session each, and a worktree a child of A's works in.
const ROOT_A: &str = "/proj/a";
const ROOT_B: &str = "/proj/b";
const WORKTREE: &str = "/proj/a-wt";

const S_A: &str = "s-root-a";
const S_CHILD: &str = "s-child";
const S_B: &str = "s-root-b";

/// A data directory of this test's own: the store every daemon wrote, and the directory the carve
/// writes into - one directory, the way the box has it.
struct Fixture {
    dir: PathBuf,
    data: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        // A counter as well as the pid: two tests of one binary run in parallel, and a name that
        // only says the pid is one directory both of them carve.
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "letibot-split-carve-{name}-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let data = dir.join("data");
        std::fs::create_dir_all(&data).expect("the data directory");
        Fixture { dir, data }
    }

    fn store(&self) -> PathBuf {
        self.data.join(LEGACY_FILE)
    }

    fn shared(&self) -> PathBuf {
        self.data.join(SHARED_FILE)
    }

    fn projects(&self) -> PathBuf {
        self.data.join(PROJECTS_DIR)
    }

    fn project(&self, root: &str) -> PathBuf {
        self.projects().join(format!("{}.db", twelve_hex(root)))
    }

    /// A daemon of one folder, as the bridge starts it: the data directory, and its own key.
    fn config(&self, root: &str) -> Config {
        let mut cfg = Config::for_this_box(root);
        cfg.store = Some(self.data.clone());
        cfg.store_key = Some(twelve_hex(root));
        cfg
    }

    fn at(&self, root: &str) -> StoreAt {
        StoreAt::Set {
            dir: self.data.clone(),
            key: twelve_hex(root),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The folder hash the daemon's socket wears, computed here rather than asked for: the carve's key
/// is worth nothing if it is only compared with itself.
fn twelve_hex(root: &str) -> String {
    let mut h = Sha256::new();
    h.update(root.as_bytes());
    let digest = h.finalize();
    digest.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

fn count(file: &Path, table: &str) -> i64 {
    let conn = Connection::open(file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap_or_else(|e| panic!("{}: counting {table}: {e}", file.display()))
}

fn has_table(file: &Path, table: &str) -> bool {
    let conn = Connection::open(file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |r| r.get(0),
        )
        .expect("sqlite_master");
    n > 0
}

/// The keys the store records as carved, read straight out of the file.
fn carved_rows(fixture: &Fixture) -> Vec<String> {
    if !has_table(&fixture.store(), "carved_project") {
        return Vec::new();
    }
    let conn = Connection::open(fixture.store()).expect("the store");
    let mut st = conn
        .prepare("SELECT key FROM carved_project ORDER BY key")
        .expect("a statement");
    st.query_map([], |r| r.get::<_, String>(0))
        .expect("the rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("the keys")
}

/// `harnessd --store DATA --list-sessions`, as the launcher runs it: the real binary, so the answer
/// a person gets is what this asserts.
fn listing(fixture: &Fixture) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_harnessd"))
        .arg("--store")
        .arg(&fixture.data)
        .arg("--workspace")
        .arg(ROOT_A)
        .arg("--list-sessions")
        .output()
        .expect("the listing ran");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A store built as the daemons left it: two folders' sessions - one with a child in a worktree -
/// one prefix both projects use, and a row in each shape the carve's row filters have to find. The
/// prefix's id comes back, because a transcript written after the carve names it.
fn seed(path: &Path) -> String {
    let store = Store::open(path).expect("a store");
    let prefix_tokens = [1u32, 2, 3, 4];
    let prefix = StablePrefixRecord {
        dialect_sha: "ab".repeat(32),
        system: "you are a harness".into(),
        tools_json: vec![r#"{"name":"read"}"#.into()],
        tokens: prefix_tokens.to_vec(),
        h_init: letibot_tokencore::ledger::hash_tokens(&prefix_tokens),
        vocab_source: "/models/x.gguf".into(),
        effort: None,
    };
    let prefix_id = store.put_stable_prefix(&prefix).expect("a prefix");
    for (id, folder, parent) in [
        (S_A, ROOT_A, None),
        (S_CHILD, WORKTREE, Some(S_A)),
        (S_B, ROOT_B, None),
    ] {
        store
            .put_session(&SessionRecord {
                id: id.into(),
                title: Some(id.into()),
                model_id: "qwen".into(),
                dialect_sha: prefix.dialect_sha.clone(),
                workspace_root: folder.into(),
                owner: "dead".into(),
                role: Some("coder".into()),
                approvers: vec![],
                parent_session_id: parent.map(str::to_string),
            })
            .expect("a session row");
    }
    append(&store, "a#t0", S_A, &prefix_id, &prefix_tokens, 2);
    append(&store, "ac#t0", S_CHILD, &prefix_id, &prefix_tokens, 1);
    append(&store, "b#t0", S_B, &prefix_id, &prefix_tokens, 1);
    // The guard is dropped before the next store call: `connection()` holds the connection's own
    // mutex, and every store method takes it again - so a call inside this scope deadlocks.
    {
        let conn = store.connection();
        for session in [S_A, S_B] {
            conn.execute(
                "INSERT INTO todo (session_id, todos_json, updated_ms) VALUES (?1, '[]', 1)",
                params![session],
            )
            .expect("a todo row");
        }
    }
    for session in [S_A, S_B] {
        store
            .record_adjudication(&decision(session))
            .expect("a decision");
    }
    prefix_id
}

fn decision(session: &str) -> NewAdjudication {
    NewAdjudication {
        request_id: format!("req-{session}"),
        session_id: session.into(),
        turn_id: "t0".into(),
        action: "read a file".into(),
        baseline: "read".into(),
        tier: "may_approve".into(),
        trail_json: "[]".into(),
        tool: "read".into(),
        arguments_json: "{}".into(),
        mode: "automode-edits".into(),
        options_json: "[]".into(),
        agent: "leticode".into(),
        effect: "admit".into(),
        ..Default::default()
    }
}

/// One transcript seated on `prefix_id`, with `rows` rows whose hash chain replays - the copy is
/// verified the way a resume reads it, so a fixture that does not replay would be a false refusal.
fn append(
    store: &Store,
    transcript: &str,
    session: &str,
    prefix_id: &str,
    prefix: &[u32],
    rows: usize,
) {
    store
        .put_transcript(transcript, session, prefix_id)
        .expect("a transcript");
    let mut ledger = TokenLedger::new(transcript, prefix).expect("a ledger");
    for seq in 0..rows {
        let tokens = vec![10 + seq as u32];
        let row = ledger
            .append(&format!("{transcript}#{seq}"), &tokens)
            .expect("a ledger row")
            .clone();
        let item = TranscriptItem::Assistant {
            text: format!("{transcript} row {seq}"),
            tool_calls: vec![],
            truncated: false,
        };
        store
            .append_item(transcript, seq as u32, &item, &row, &tokens)
            .expect("a transcript row");
    }
}

/// This process's own pid is a live claim: the rule the daemon takes a claim's liveness by.
fn this_pid(pid: u32) -> bool {
    pid == std::process::id()
}

fn carve(fixture: &Fixture, root: &str) -> Result<Carved, String> {
    migrate_split::carve(
        &fixture.store(),
        &fixture.data,
        &twelve_hex(root),
        "box",
        this_pid,
    )
    .map_err(|e| e.to_string())
}

/// A writer claim on one project stops that project's carve and nothing else: the other key's is
/// live and its carve still runs, which is the whole point of taking one key at a time.
#[test]
fn a_live_claim_on_one_project_stops_that_project_only() {
    let fixture = Fixture::new("claim");
    seed(&fixture.store());
    let store = Store::open(&fixture.store()).expect("the store");
    store
        .claim_session_writer(S_B, "box/1/1", std::process::id(), "box", 1, 0)
        .expect("a claim on B, held by this process");

    let carved = carve(&fixture, ROOT_A).expect("A is carved while B's daemon is claimed");
    assert_eq!(carved.key, twelve_hex(ROOT_A));
    assert!(
        carved.uncarved.iter().any(|k| k == &twelve_hex(ROOT_B)),
        "the answer names what is left: {carved:#?}"
    );
    assert!(fixture.project(ROOT_A).is_file(), "A's own file");

    // And B, claimed, is refused - by name, and with nothing written.
    let refused = carve(&fixture, ROOT_B).expect_err("B's daemon is holding it");
    assert!(
        refused.contains(S_B) && refused.contains("claimed"),
        "the refusal names the claimed session: {refused}"
    );
    assert!(
        !fixture.project(ROOT_B).exists(),
        "a refused carve writes no project file"
    );
    assert_eq!(
        carved_rows(&fixture),
        vec![twelve_hex(ROOT_A)],
        "and records no key"
    );
    assert_eq!(
        count(&fixture.store(), "todo"),
        1,
        "B's row is still where B's daemon writes it"
    );
}

/// The two shapes a daemon opens after a carve: the carved key routes its project rows to its file
/// and its index rows to `sessions.db`, and the uncarved key still opens `sessions.db` alone.
#[test]
fn a_carved_key_opens_its_own_file_and_an_uncarved_key_opens_sessions_db() {
    let fixture = Fixture::new("open");
    let prefix = seed(&fixture.store());
    carve(&fixture, ROOT_A).expect("A is carved");
    let a = fixture.project(ROOT_A);

    // The carved key: the daemon's own door, opened by the config a folder's daemon has.
    let store = fixture
        .config(ROOT_A)
        .store_at()
        .expect("a store")
        .open()
        .expect("A's daemon opens the transition's two files");
    assert!(store.is_set(), "a carved key is a set's handle");
    assert_eq!(
        store.path().map(Path::to_path_buf),
        Some(fixture.store()),
        "the shared half of the set is sessions.db while the carve is running"
    );
    // A transcript row this daemon writes lands in A's file, and its session row in the index.
    store
        .put_transcript("a#t1", S_A, &prefix)
        .expect("a transcript, in A's own file");
    assert_eq!(
        count(&a, "transcript"),
        3,
        "A's history is its own file's: two carved, one written since"
    );
    assert_eq!(
        count(&fixture.store(), "transcript"),
        1,
        "and B's transcript is the only one left in sessions.db"
    );
    assert_eq!(
        count(&fixture.store(), "session"),
        3,
        "the index never leaves sessions.db: every session is listed from it"
    );
    // The file a path question names is where the rows are.
    assert_eq!(fixture.at(ROOT_A).path("transcript"), a);
    assert_eq!(fixture.at(ROOT_B).path("transcript"), fixture.store());

    // The bridge's shape: `--store .../sessions.db` for a folder that IS carved. Handed the file,
    // the daemon must still open the set - its project rows belong in its own file.
    let mut cfg = fixture.config(ROOT_A);
    cfg.store = Some(fixture.store());
    let at = cfg.store_at().expect("a store");
    assert_eq!(
        at,
        StoreAt::Set {
            dir: fixture.data.clone(),
            key: twelve_hex(ROOT_A)
        },
        "a sessions.db beside a project file is the data directory"
    );
    let from_file = at.open().expect("the daemon opens the set");
    assert!(
        from_file
            .load_transcript("a#t0")
            .map(|t| !t.items.is_empty())
            .unwrap_or(false),
        "and reads A's history out of A's file"
    );

    // The uncarved key: one file, and its rows are still in it.
    let b = fixture
        .config(ROOT_B)
        .store_at()
        .expect("a store")
        .open()
        .expect("B's daemon opens sessions.db");
    assert!(
        b.is_set(),
        "a listing must not join a history that is elsewhere"
    );
    assert_eq!(
        b.path().map(Path::to_path_buf),
        Some(fixture.store()),
        "B's only file is sessions.db"
    );
    b.put_transcript("b#t1", S_B, &prefix)
        .expect("B writes into sessions.db");
    assert_eq!(count(&fixture.store(), "transcript"), 2, "B's two now");
    assert!(
        !fixture.project(ROOT_B).exists(),
        "an uncarved key opens no file of its own"
    );
    assert_eq!(
        b.load_transcript("b#t0")
            .map(|t| t.items.len())
            .unwrap_or(0),
        1,
        "and reads its own history out of sessions.db"
    );
}

/// A check that refuses after the copy is a refusal with nothing left behind: no project file, no
/// carved row, and every one of that key's rows still in `sessions.db`.
#[test]
fn a_refused_check_leaves_no_file_and_no_carved_row() {
    let fixture = Fixture::new("broken");
    let prefix = seed(&fixture.store());
    // B's transcript is a fork of A's: a reference no file of the split layout can carry, and the
    // one shape the carve refuses after it has already written its copy.
    Store::open(&fixture.store())
        .expect("the store")
        .put_fork("b#fork", S_B, &prefix, "a#t0", 1)
        .expect("a fork across the two projects");
    let refused = carve(&fixture, ROOT_A).expect_err("a fork across projects cannot be carried");
    assert!(
        refused.contains("b#fork") && refused.contains("forks"),
        "the refusal names the transcript that forks it: {refused}"
    );
    assert!(
        !fixture.project(ROOT_A).exists(),
        "the file the refused carve wrote was taken back"
    );
    assert!(
        carved_rows(&fixture).is_empty(),
        "and no key was recorded as carved"
    );
    assert_eq!(
        count(&fixture.store(), "transcript"),
        4,
        "sessions.db is untouched: every transcript is still in it"
    );
    assert_eq!(count(&fixture.store(), "todo"), 2);
    assert_eq!(
        count(&fixture.store(), "session_writer"),
        0,
        "and no lease was taken or left behind"
    );
    // The store is still a whole store, in the shape it had before the carve began: a daemon handed
    // the FILE opens the one store and reads its own history. A DIRECTORY is still the refusal P3
    // gives, because nothing has been carved out of it - which is the discriminator `store_at`
    // reads, so both halves are asserted rather than the one the test happens to want.
    let refused = fixture
        .config(ROOT_A)
        .store_at()
        .expect("a store")
        .open()
        .err()
        .expect("a directory holding only sessions.db is not a store yet");
    assert!(refused.to_string().contains("not split yet"), "{refused}");
    for (root, transcript, items) in [(ROOT_A, "a#t0", 2), (ROOT_B, "b#t0", 1)] {
        let mut cfg = fixture.config(root);
        cfg.store = Some(fixture.store());
        let store = cfg
            .store_at()
            .expect("a store")
            .open()
            .expect("the one file still opens");
        assert_eq!(
            store
                .load_transcript(transcript)
                .ok()
                .map(|t| t.items.len()),
            Some(items),
            "{root} reads its own history out of the one file"
        );
    }
}

/// `--finish` is the last step and says so: it refuses while a key is uncarved, and once every key
/// is out it produces exactly the layout P3 opens - `shared.db` and the project files, no project
/// table in the shared one.
#[test]
fn finish_refuses_until_every_key_is_carved_and_then_makes_the_split_layout() {
    let fixture = Fixture::new("finish");
    seed(&fixture.store());
    carve(&fixture, ROOT_A).expect("A is carved");

    let refused = migrate_split::finish(&fixture.store(), &fixture.data, "box", this_pid)
        .expect_err("B is still in sessions.db");
    let said = refused.to_string();
    assert!(
        said.contains(&twelve_hex(ROOT_B)) && said.contains("not carved"),
        "the refusal names the key that is left: {said}"
    );
    assert!(
        fixture.store().is_file() && !fixture.shared().exists(),
        "nothing was renamed"
    );

    carve(&fixture, ROOT_B).expect("B is carved");
    let finished = migrate_split::finish(&fixture.store(), &fixture.data, "box", this_pid)
        .expect("every key is carved, so the last step runs");
    assert_eq!(finished.shared, fixture.shared());
    assert!(fixture.shared().is_file(), "the shared file is the old one");
    assert!(!fixture.store().exists(), "and not still at its old name");
    assert_eq!(
        finished.carved,
        vec![twelve_hex(ROOT_A), twelve_hex(ROOT_B)],
        "both keys, in order"
    );

    // Exactly P3's layout: no project table in the shared file at all, and no transition record.
    for table in [
        "transcript",
        "transcript_item",
        "stable_prefix",
        "adjudication",
        "todo",
        "carved_project",
    ] {
        assert!(
            !has_table(&fixture.shared(), table),
            "shared.db still holds {table}"
        );
    }
    assert_eq!(count(&fixture.shared(), "session"), 3, "the index, whole");
    // The history is in the project files, one each.
    assert_eq!(count(&fixture.project(ROOT_A), "transcript"), 2);
    assert_eq!(count(&fixture.project(ROOT_A), "transcript_item"), 3);
    assert_eq!(count(&fixture.project(ROOT_B), "transcript"), 1);
    assert_eq!(count(&fixture.project(ROOT_A), "adjudication"), 1);

    // And a daemon opens it the way P3 always has, the file now being called shared.db.
    let store = fixture
        .config(ROOT_A)
        .store_at()
        .expect("a store")
        .open()
        .expect("a daemon opens the finished set");
    assert_eq!(
        store.path().map(Path::to_path_buf),
        Some(fixture.shared()),
        "the shared half is shared.db"
    );
    assert_eq!(
        store
            .load_transcript("a#t0")
            .map(|t| t.items.len())
            .unwrap_or(0),
        2,
        "A's history reads out of A's file"
    );
    assert_eq!(
        fixture
            .config(ROOT_A)
            .store_at()
            .unwrap()
            .path("transcript"),
        fixture.project(ROOT_A)
    );

    // A second finish is refused: there would be two shared files.
    let again = migrate_split::finish(&fixture.store(), &fixture.data, "box", this_pid)
        .expect_err("the store is finished");
    assert!(again.to_string().contains("already there"), "{again}");
}

/// The listing is the index's, so it answers for a carved project and an uncarved one alike: with A
/// out of `sessions.db` and B still in it, both are in `--sessions`.
#[test]
fn the_listing_sees_a_carved_project_and_an_uncarved_one() {
    let fixture = Fixture::new("listing");
    seed(&fixture.store());
    carve(&fixture, ROOT_A).expect("A is carved");
    let listed = listing(&fixture);
    for session in [S_A, S_CHILD, S_B] {
        assert!(
            listed.contains(session),
            "{session} is missing from a listing of a store being carved:\n{listed}"
        );
    }
    // And after the last step, the same answer from `shared.db`.
    carve(&fixture, ROOT_B).expect("B is carved");
    migrate_split::finish(&fixture.store(), &fixture.data, "box", this_pid).expect("the last step");
    let listed = listing(&fixture);
    for session in [S_A, S_CHILD, S_B] {
        assert!(
            listed.contains(session),
            "{session} is missing from the finished store's listing:\n{listed}"
        );
    }
}
