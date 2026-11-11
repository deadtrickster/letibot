//! P1's done-tests (`docs/store-split.md` § Build plan): every table in exactly one file, a file from
//! a newer build refused by name, and a second open of a set that is not a second connection. Every
//! one of them runs on a directory of its own under `$TMPDIR`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::{Connection, params};

use super::split::{
    self, LEGACY_SHARED_TABLES, Place, StoreFile, StoreSet, holds, place_of, tables_in,
};
use super::{SCHEMA_VERSION, Store};
use crate::apparatus;

/// A project key, shaped like the folder hash `scripts/letibot` computes.
const KEY: &str = "42ce9f1aae08";

/// A directory of this test's own. The name carries the test's, so eight threads of this binary
/// cannot collide, and a killed run's leftovers are removed before anything opens them.
fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("store-split-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temp directory");
    dir
}

/// The tables a connection holds. `sqlite_%` is SQLite's own bookkeeping, not the schema.
fn table_names(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare(
            "SELECT name FROM sqlite_master
              WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .expect("a statement");
    stmt.query_map([], |row| row.get::<_, String>(0))
        .expect("the table list")
        .filter_map(std::result::Result::ok)
        .collect()
}

/// The tables a FILE holds, on a connection of the test's own: `Store::open` migrates what it opens,
/// and this has to see the file as the set left it.
fn tables_in_file(path: &Path) -> Vec<String> {
    table_names(&Connection::open(path).expect("a connection to the file"))
}

fn file_label(file: StoreFile) -> &'static str {
    match file {
        StoreFile::Shared => "shared.db",
        StoreFile::Project => "projects/<key>.db",
    }
}

/// This process's descriptors on anything under `dir`, from `/proc/self/fd`: the honest count of
/// connections, where `Store::live` is a process-wide number another test can move under this one.
fn descriptors_under(dir: &Path) -> usize {
    let Some(name) = dir.file_name().map(|n| n.to_string_lossy().into_owned()) else {
        return 0;
    };
    let Ok(entries) = std::fs::read_dir("/proc/self/fd") else {
        return 0;
    };
    entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .parse::<i32>()
                .is_ok_and(|fd| fd > 2)
        })
        .filter(|entry| {
            std::fs::read_link(entry.path())
                .map(|target| target.to_string_lossy().contains(&name))
                .unwrap_or(false)
        })
        .count()
}

/// Every table the current schema creates is named by the assignment, and the two legacy archive
/// names are routed without being created: a table added with no assignment fails here BY NAME.
#[test]
fn every_table_the_schema_creates_is_assigned() {
    let fresh = Store::open_in_memory().expect("an in-memory store");
    let created = table_names(&fresh.connection());
    let unnamed: Vec<&String> = created
        .iter()
        .filter(|table| place_of(table).is_none())
        .collect();
    assert!(
        unnamed.is_empty(),
        "tables the schema creates with no file in the assignment: {unnamed:#?}"
    );
    for table in LEGACY_SHARED_TABLES {
        assert_eq!(place_of(table), Some(Place::Shared), "{table}");
        assert!(
            !created.iter().any(|name| name == table),
            "a fresh store created {table}, which only the v24 arm's archive has"
        );
    }
}

/// § Table assignment both ways round: every table in the two lists is in the file the assignment
/// gives it and in neither other, and neither file holds a table that is not its own.
#[test]
fn every_table_is_in_exactly_one_file() {
    let dir = temp("partition");
    let set = StoreSet::open(&dir, KEY).expect("a set");
    let in_files = [
        (
            StoreFile::Shared,
            tables_in_file(&set.path(StoreFile::Shared)),
        ),
        (
            StoreFile::Project,
            tables_in_file(&set.path(StoreFile::Project)),
        ),
    ];
    let mut wrong = Vec::new();
    for (file, names) in &in_files {
        for table in tables_in(*file) {
            if !names.iter().any(|name| name == table) {
                wrong.push(format!("{table} is missing from {}", file_label(*file)));
            }
        }
        for table in names {
            if !holds(*file, table) {
                wrong.push(format!(
                    "{} holds {table}, which is not its table",
                    file_label(*file)
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A P1 done-test: a project file written by a newer build refuses to open, and the refusal says
/// which file and which two versions - all a person staring at a daemon that will not start gets.
#[test]
fn a_newer_project_file_is_refused_by_name() {
    let dir = temp("newer");
    let path = {
        let set = StoreSet::open(&dir, KEY).expect("a set");
        set.path(StoreFile::Project)
    };
    let newer = SCHEMA_VERSION + 1;
    let conn = Connection::open(&path).expect("the project file");
    conn.execute("UPDATE schema_version SET version = ?1", params![newer])
        .expect("a newer stamp");
    drop(conn);
    let refused = StoreSet::open(&dir, KEY)
        .err()
        .expect("a project file from a newer build is refused");
    let said = refused.to_string();
    assert!(said.contains(&path.display().to_string()), "{said}");
    assert!(said.contains(&newer.to_string()), "{said}");
    assert!(said.contains(&SCHEMA_VERSION.to_string()), "{said}");
}

/// A P1 done-test: opening the set twice in one process is a no-op, not a second connection. The
/// count is this directory's own descriptors, which no other test in the binary can move.
#[test]
fn opening_the_set_twice_is_not_a_second_connection() {
    let Some(_) = apparatus::applies(
        "a /proc/self/fd descriptor census",
        cfg!(target_os = "linux"),
    ) else {
        return;
    };
    let dir = temp("twice");
    let first = StoreSet::open(&dir, KEY).expect("a set");
    let descriptors = descriptors_under(&dir);
    assert!(
        descriptors > 0,
        "the census found no descriptor under {}",
        dir.display()
    );
    let second = StoreSet::open(&dir, KEY).expect("the same set");
    assert!(
        Arc::ptr_eq(&first, &second),
        "the second open built a second set"
    );
    assert_eq!(
        descriptors_under(&dir),
        descriptors,
        "the second open took another descriptor"
    );
}

/// The project file's schema is usable: a per-session row needs no `session` row, which is
/// `shared.db`'s, while the keys between the project's own tables still refuse a missing parent.
#[test]
fn a_project_file_takes_its_own_rows_and_keeps_its_own_keys() {
    let dir = temp("usable");
    let set = StoreSet::open(&dir, KEY).expect("a set");
    let writer = set.writer(StoreFile::Project);
    writer
        .run(|conn| {
            conn.execute(
                "INSERT INTO todo (session_id, todos_json, updated_ms) VALUES ('s1', '[]', 1)",
                [],
            )?;
            Ok(())
        })
        .expect("a per-session row in the project file");
    let seen: i64 = set
        .project()
        .connection()
        .query_row("SELECT COUNT(*) FROM todo", [], |row| row.get(0))
        .expect("a read on the store's own connection");
    assert_eq!(
        seen, 1,
        "the writer's row is not in the file the reader reads"
    );

    let orphan = writer
        .run(|conn| {
            conn.execute(
                "INSERT INTO transcript_item
                   (transcript_id, seq, item_id, kind, item_json, tok_offset, tok_len, h_k, tokens,
                    created_at)
                 VALUES ('no-such-transcript', 0, 'i1', 'user', '{}', 0, 1, ?1, ?2, 1)",
                params![vec![0u8; 32], vec![0u8; 4]],
            )?;
            Ok(())
        })
        .expect_err("a transcript_item under a transcript this file does not have");
    assert!(orphan.to_string().contains("FOREIGN KEY"), "{orphan}");
}

/// The splitter sees every statement the schema is built from. One form it could not name would be
/// dropped from both files by `schema_sql`, and a table would be missing with nothing said.
#[test]
fn every_statement_the_schema_is_built_from_names_its_table() {
    let mut unnamed = Vec::new();
    let mut statements = 0;
    for batch in split::batches() {
        for statement in split::statements(batch) {
            statements += 1;
            if split::table_of(statement).is_none() {
                unnamed.push(statement.lines().next().unwrap_or_default().to_string());
            }
        }
    }
    assert!(statements > 30, "only {statements} statements were split");
    assert!(unnamed.is_empty(), "{unnamed:#?}");
}

/// A key names a file under `projects/`, so one that could walk out of it is refused, and refused
/// before anything is created.
#[test]
fn a_key_that_is_not_a_name_is_refused() {
    let dir = temp("keys");
    for key in ["", "..", "../x", "a/b", "not-hex"] {
        let refused = StoreSet::open(&dir, key)
            .err()
            .unwrap_or_else(|| panic!("{key:?} was taken as a project key"));
        assert!(
            refused.to_string().contains("project key"),
            "{key}: {refused}"
        );
    }
    assert_eq!(
        std::fs::read_dir(&dir).expect("the directory").count(),
        0,
        "a refused key created something"
    );
}
/// P3's half of the assignment: one handle over both files routes each call by `place_of`, so an
/// index row lands in `shared.db` and a session's own state in the project file without the caller
/// saying which. A single-file store routes nothing - it has one connection - which is why every
/// call site kept its meaning.
#[test]
fn a_handles_call_lands_in_the_file_its_table_is_assigned() {
    let dir = temp("routes");
    let set = Store::open_set(&dir, KEY).expect("a set's handle");
    set.put_session(&super::SessionRecord {
        id: "s1".into(),
        title: Some("one".into()),
        model_id: "m".into(),
        dialect_sha: "d".into(),
        workspace_root: "/tmp".into(),
        owner: "op".into(),
        approvers: Vec::new(),
        role: None,
        parent_session_id: None,
    })
    .expect("a session row");
    // A foreign key to `session` would refuse this row in a file that has no index; the project
    // file drops those keys for exactly this reason (P1), so the row lands.
    set.put_todos("s1", &[]).expect("a todo row");
    let count = |table: &str, path: &Path| -> i64 {
        Connection::open(path)
            .expect("a connection of the test's own")
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap_or_else(|e| panic!("counting {table} in {}: {e}", path.display()))
    };
    let shared = dir.join(split::SHARED_FILE);
    let project = dir.join(split::PROJECTS_DIR).join(format!("{KEY}.db"));
    assert_eq!(
        count("session", &shared),
        1,
        "the index is the shared file's"
    );
    assert_eq!(
        count("todo", &project),
        1,
        "a session's board is its project file's"
    );
    assert!(
        !tables_in_file(&shared).iter().any(|t| t == "todo"),
        "shared.db holds no project table at all"
    );
    assert!(
        !tables_in_file(&project).iter().any(|t| t == "session"),
        "and the project file holds no index"
    );
    assert!(set.is_set(), "the handle knows it is a set's");

    // And a single-file store has one connection for everything: the routing decides nothing.
    let one = Store::open(&dir.join("one.db")).expect("one file");
    one.put_session(&super::SessionRecord {
        id: "s2".into(),
        title: None,
        model_id: "m".into(),
        dialect_sha: "d".into(),
        workspace_root: "/tmp".into(),
        owner: "op".into(),
        approvers: Vec::new(),
        role: None,
        parent_session_id: None,
    })
    .expect("a session row");
    one.put_todos("s2", &[]).expect("a todo row");
    assert!(
        !one.is_set() && count("todo", &dir.join("one.db")) == 1,
        "a single-file store holds every table, so nothing is routed"
    );
}

/// No door creates a set's file beside `sessions.db`: a daemon that opened `shared.db` by path
/// made a full-schema file there, and every door after it took the box for split.
#[test]
fn no_set_file_is_created_beside_the_legacy_store() {
    let dir = temp("beside-legacy");
    Store::open(&dir.join(split::LEGACY_FILE)).expect("the pre-split store");
    let shared = dir.join(split::SHARED_FILE);
    let project = dir.join(split::PROJECTS_DIR).join(format!("{KEY}.db"));
    std::fs::create_dir_all(project.parent().unwrap()).unwrap();
    for path in [&shared, &project] {
        let refused = Store::open(path).err().expect("refused");
        assert!(
            refused.to_string().contains("letibot-migrate-split"),
            "the refusal names the command that splits it: {refused}"
        );
        assert!(
            !path.exists(),
            "the refused open created {}",
            path.display()
        );
    }
    // A file already there opens: the migration reads its own copy back before the rename.
    Connection::open(&shared).expect("made by hand");
    Store::open(&shared).expect("an existing file opens");
}

/// A split directory gains a `sessions.db` when a process from before the split opens its old
/// path; a new folder's project file is still created beside it.
#[test]
fn a_stray_sessions_db_in_a_split_directory_does_not_stop_a_new_project() {
    let dir = temp("stray-legacy");
    Store::open_set(&dir, "aaaaaaaaaaaa").expect("the split set");
    Store::open(&dir.join(split::LEGACY_FILE)).expect("a stray made after the split");
    let fresh = Store::open_set(&dir, KEY).expect("a new folder's project file");
    assert!(fresh.is_set());
    assert!(
        dir.join(split::PROJECTS_DIR)
            .join(format!("{KEY}.db"))
            .is_file()
    );
}
