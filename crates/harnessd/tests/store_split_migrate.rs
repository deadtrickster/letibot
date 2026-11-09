//! P2's done-test (`docs/store-split.md` § Build plan): the offline migration to the split store,
//! run on a fixture store with two projects that hold rows, a child working in its parent's
//! worktree, and a project whose session wrote nothing.
//!
//! Every path here is under `$TMPDIR`: the pass renames the file it splits, and it is only ever
//! pointed at a store a test built.

use std::path::{Path, PathBuf};

use letibot_tokencore::ledger::TokenLedger;
use letibot_tokencore::migrate_split::{self, ProjectFile, Report};
use letibot_tokencore::rusqlite::{Connection, params};
use letibot_tokencore::store::split::{PROJECTS_DIR, SHARED_FILE};
use letibot_tokencore::store::{SessionRecord, StablePrefixRecord, Store};
use letibot_transcript::TranscriptItem;
use sha2::{Digest, Sha256};

/// The three folders, as a session records them: two projects, and the worktree a child works in.
const ROOT_A: &str = "/proj/a";
const ROOT_B: &str = "/proj/b";
const ROOT_C: &str = "/proj/c";
const WORKTREE: &str = "/proj/a-wt";

const S_A: &str = "s-root-a";
const S_CHILD: &str = "s-child";
const S_B: &str = "s-root-b";
const S_C: &str = "s-root-c";

/// A directory of this test's own: the store to split, and the data directory it splits into.
struct Fixture {
    dir: PathBuf,
    data: PathBuf,
}

impl Fixture {
    fn store(&self) -> PathBuf {
        self.dir.join("sessions.db")
    }

    fn kept(&self) -> PathBuf {
        self.dir.join("sessions.db.pre-split")
    }

    fn shared(&self) -> PathBuf {
        self.data.join(SHARED_FILE)
    }

    fn projects(&self) -> PathBuf {
        self.data.join(PROJECTS_DIR)
    }
}

/// A project's file, by the folder its sessions record.
fn project_of<'a>(report: &'a Report, root: &str) -> &'a ProjectFile {
    let key = twelve_hex(root);
    report
        .projects
        .iter()
        .find(|file| file.key == key)
        .unwrap_or_else(|| panic!("no project file for {root} ({key}): {report:#?}"))
}

/// The folder hash the daemon's socket wears, computed here rather than asked for: the migration's
/// key is worth nothing if it is only compared with itself.
fn twelve_hex(root: &str) -> String {
    let mut h = Sha256::new();
    h.update(root.as_bytes());
    let digest = h.finalize();
    digest.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

fn count(file: &Path, table: &str) -> i64 {
    let conn = Connection::open(file).expect("a connection to the file");
    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap_or_else(|e| panic!("{}: counting {table}: {e}", file.display()))
}

fn tables(file: &Path) -> Vec<String> {
    let conn = Connection::open(file).expect("a connection to the file");
    let mut st = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .expect("a statement");
    st.query_map([], |r| r.get::<_, String>(0))
        .expect("the table list")
        .collect::<std::result::Result<Vec<_>, _>>()
        .expect("the table names")
}

fn files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .collect()
}

fn temp(name: &str) -> Fixture {
    let dir =
        std::env::temp_dir().join(format!("store-split-migrate-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    Fixture {
        data: dir.join("data"),
        dir,
    }
}

/// A store built as a daemon would leave it: four sessions in three folders - one of them with a
/// child in a worktree - one stable prefix both projects use, and a row in each of the shapes the
/// migration's four row filters have to find.
fn seed(path: &Path) {
    let store = Store::open(path).expect("a store");
    let prefix_tokens = [1u32, 2, 3, 4];
    let prefix = StablePrefixRecord {
        dialect_sha: "ab".repeat(32),
        system: "you are a harness".into(),
        tools_json: vec![r#"{"name":"read"}"#.into()],
        tokens: prefix_tokens.to_vec(),
        h_init: letibot_tokencore::ledger::hash_tokens(&prefix_tokens),
        vocab_source: "/models/x.gguf".into(),
    };
    let prefix_id = store.put_stable_prefix(&prefix).expect("a prefix");
    for (id, folder, parent) in [
        (S_A, ROOT_A, None),
        (S_CHILD, WORKTREE, Some(S_A)),
        (S_B, ROOT_B, None),
        (S_C, ROOT_C, None),
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
    // A's conversation and the child's, both seated on the one prefix; B's too, so the assignment
    // has a prefix to duplicate per project. C writes nothing at all.
    append(&store, "a#t0", S_A, &prefix_id, &prefix_tokens, 2);
    append(&store, "ac#t0", S_CHILD, &prefix_id, &prefix_tokens, 1);
    append(&store, "b#t0", S_B, &prefix_id, &prefix_tokens, 1);
    let conn = store.connection();
    conn.execute(
        "INSERT INTO todo (session_id, todos_json, updated_ms) VALUES (?1, '[]', 1)",
        params![S_A],
    )
    .expect("a todo row");
    // The child's own `workspace_root` is the worktree: its run and its transcript land in A's
    // file anyway. The second run's child never got a session row, and its parent did.
    for child in [S_CHILD, "s-never-opened"] {
        conn.execute(
            "INSERT INTO subagent_run
               (child_id, parent_id, state, prompt, spec_json, instance, resumes, outcome,
                later_answer, delivered, started_ms, updated_ms)
             VALUES (?1, ?2, 'done', 'look', '{}', 'box/1/1', 0, 'seen', NULL, 0, 1, 1)",
            params![child, S_A],
        )
        .expect("a child run");
    }
    for (request, session) in [("req-a", S_A), ("req-b", S_B)] {
        conn.execute(
            "INSERT INTO adjudication
               (request_id, session_id, turn_id, decided_ms, action, baseline, tier, trail_json,
                effect)
             VALUES (?1, ?2, 't0', 1, 'read a file', 'read', 'may_approve', '[]', 'admit')",
            params![request, session],
        )
        .expect("a decision");
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

#[test]
fn every_row_lands_in_exactly_one_file_and_the_keys_hash() {
    let fixture = temp("once");
    seed(&fixture.store());
    let report = migrate_split::migrate(&fixture.store(), &fixture.data).expect("the split");
    assert_eq!(report.sessions, 4, "every session, the child included");
    assert_eq!(
        report.projects.len(),
        3,
        "one file per ROOT folder - the worktree is not a fourth: {report:#?}"
    );
    let (a, b, c) = (
        project_of(&report, ROOT_A),
        project_of(&report, ROOT_B),
        project_of(&report, ROOT_C),
    );
    assert_eq!(a.key, twelve_hex(ROOT_A), "the key is the folder hash");
    assert_eq!(b.key, twelve_hex(ROOT_B));
    assert_eq!(c.key, twelve_hex(ROOT_C));
    for file in [&a.path, &b.path, &c.path] {
        assert!(file.is_file(), "{} was not written", file.display());
    }
    assert_eq!(files(&fixture.projects()).len(), 3, "no fourth file");
    assert!(
        !fixture
            .projects()
            .join(format!("{}.db", twelve_hex(WORKTREE)))
            .exists(),
        "the child's own workspace_root is its worktree, and hashing it would be a fourth project"
    );

    // The history: the child's transcript is in its ROOT's project file, and the project whose
    // session wrote nothing holds no row of any project table.
    assert_eq!(count(&a.path, "transcript"), 2);
    assert_eq!(count(&a.path, "transcript_item"), 3);
    assert_eq!(count(&b.path, "transcript"), 1);
    assert_eq!(count(&b.path, "transcript_item"), 1);
    for table in [
        "transcript",
        "transcript_item",
        "stable_prefix",
        "todo",
        "subagent_run",
        "adjudication",
    ] {
        assert_eq!(
            count(&c.path, table),
            0,
            "{table} in the empty project's file"
        );
    }

    // Per-session rows, the child's run, and the decisions: one file each, by the column that
    // names them.
    assert_eq!(count(&a.path, "todo"), 1);
    assert_eq!(
        count(&a.path, "subagent_run"),
        2,
        "the child's run, and the run whose child has no session row"
    );
    assert_eq!(count(&a.path, "adjudication"), 1);
    assert_eq!(count(&b.path, "adjudication"), 1);
    // § Table assignment duplicates a content-addressed prefix per project rather than sharing it,
    // so this is the one row in two files - and both transcripts read their own copy.
    assert_eq!(count(&a.path, "stable_prefix"), 1);
    assert_eq!(count(&b.path, "stable_prefix"), 1);

    // The index: every session, and the key that says which file its rows are in.
    assert_eq!(count(&fixture.shared(), "session"), 4);
    for (session, root) in [
        (S_A, ROOT_A),
        (S_CHILD, ROOT_A),
        (S_B, ROOT_B),
        (S_C, ROOT_C),
    ] {
        let key: String = Connection::open(fixture.shared())
            .expect("the shared file")
            .query_row(
                "SELECT project FROM session WHERE id = ?1",
                params![session],
                |r| r.get(0),
            )
            .expect("a session's project");
        assert_eq!(key, twelve_hex(root), "session {session}");
    }
    // No project table is in `shared.db`, and no decision is anywhere but a project file.
    let shared = tables(&fixture.shared());
    for table in [
        "adjudication",
        "transcript",
        "transcript_item",
        "stable_prefix",
    ] {
        assert!(
            !shared.iter().any(|name| name == table),
            "shared.db holds {table}: {shared:?}"
        );
    }
    assert!(fixture.kept().is_file(), "the old file is kept, renamed");
    assert!(
        !fixture.store().exists(),
        "and is not still at its old name"
    );
}

#[test]
fn a_second_run_refuses() {
    let fixture = temp("second");
    seed(&fixture.store());
    migrate_split::migrate(&fixture.store(), &fixture.data).expect("the first pass");
    let refused = migrate_split::migrate(&fixture.store(), &fixture.data)
        .expect_err("a second pass is refused");
    let said = refused.to_string();
    assert!(said.contains("ONCE"), "{said}");
    // Put the old file back and ask again: the pass refuses on the data directory it filled,
    // which is the half that still holds when the daemon has been writing since.
    std::fs::copy(fixture.kept(), fixture.store()).expect("the old file back");
    let refused =
        migrate_split::migrate(&fixture.store(), &fixture.data).expect_err("still a second pass");
    assert!(refused.to_string().contains("ONCE"), "{refused}");
}

#[test]
fn a_broken_verification_leaves_sessions_db_in_place_under_its_own_name() {
    let fixture = temp("broken");
    seed(&fixture.store());
    // A row naming no session lands in no file, so the count check refuses after files are written.
    Connection::open(fixture.store())
        .expect("the store")
        .execute(
            "INSERT INTO todo (session_id, todos_json, updated_ms) VALUES ('s-nobody', '[]', 1)",
            [],
        )
        .expect("an orphan row");
    let refused = migrate_split::migrate(&fixture.store(), &fixture.data)
        .expect_err("a row in no file is a refusal");
    let said = refused.to_string();
    assert!(said.contains("todo"), "{said}");
    assert!(said.contains("renamed"), "{said}");
    assert!(
        fixture.store().is_file(),
        "nothing was renamed: the old file is still the whole store"
    );
    assert!(
        !fixture.kept().exists(),
        "no kept copy of a file nobody split"
    );
    assert!(
        !fixture.shared().exists(),
        "the files the pass wrote went with the refusal"
    );
    assert!(
        files(&fixture.projects()).is_empty(),
        "and so did the project files"
    );
}

/// A chain that does not replay in the old store either is copied as stored and named: the copy
/// is faithful, and a resume repairs the shapes it knows.
#[test]
fn a_chain_broken_in_the_old_store_is_carried_and_named() {
    let fixture = temp("carried");
    seed(&fixture.store());
    break_one_chain(&fixture.store());
    let report = migrate_split::migrate(&fixture.store(), &fixture.data)
        .expect("a faithful copy of a broken chain is not a refusal");
    assert_eq!(report.unreplayed.len(), 1, "{:?}", report.unreplayed);
    assert!(
        report.unreplayed[0].starts_with("a#t0 "),
        "{:?}",
        report.unreplayed
    );
}

/// The append-only trigger refuses an UPDATE, so the break is written the way a damaged file would
/// have it: one row's chain value replaced under the trigger's nose.
fn break_one_chain(path: &Path) {
    let conn = Connection::open(path).expect("the store");
    conn.execute_batch("DROP TRIGGER transcript_item_no_update")
        .expect("the trigger out of the way");
    conn.execute(
        "UPDATE transcript_item SET h_k = ?1 WHERE transcript_id = 'a#t0' AND seq = 1",
        params![vec![0u8; 32]],
    )
    .expect("the broken row");
}

/// The command's own refusal: the lease is the `session_writer` row, and its liveness is the
/// daemon's rule - this host's pid running, or a claim no pid here can answer for.
#[test]
fn a_live_writer_claim_is_a_refusal_and_a_stale_one_is_not() {
    let fixture = temp("claim");
    seed(&fixture.store());
    let store = Store::open(&fixture.store()).expect("the store");
    assert!(
        migrate_split::writer_claims(&store)
            .expect("no claims yet")
            .is_empty()
    );
    store
        .claim_session_writer(S_A, "box/1/1", std::process::id(), "box", 1, 0)
        .expect("a claim");
    let claims = migrate_split::writer_claims(&store).expect("the claim");
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].session, S_A);
    assert_eq!(
        migrate_split::live_claims(&claims, "box", |pid| pid == std::process::id()).len(),
        1,
        "this process is running, so the claim is live"
    );
    assert!(
        migrate_split::live_claims(&claims, "box", |_| false).is_empty(),
        "a claim whose pid is gone is stale and does not stop a pass"
    );
    assert_eq!(
        migrate_split::live_claims(&claims, "another-box", |_| false).len(),
        1,
        "a claim this box cannot ask about pids for is not taken for stale"
    );
}
