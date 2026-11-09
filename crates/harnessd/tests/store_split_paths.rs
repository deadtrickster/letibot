//! The daemon opens two paths (`docs/store-split.md` § Build plan, P3). `--store` is a data
//! directory: a daemon writes a session to `shared.db` and its transcript to
//! `projects/<key>.db`; a listing still answers with the project file renamed away; and a data
//! directory that still holds the single `sessions.db` is refused with the migration's name.

use std::path::PathBuf;
use std::process::Command;

use letibot_harnessd::config::{Config, ProviderConfig, Seat};
use letibot_harnessd::sessions::{Sessions, StoreSessions};
use letibot_harnessd::{Dialect, Parts};
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_tokencore::migrate_split::project_key;
use letibot_tokencore::store::Store;
use letibot_tokencore::store::split::{PROJECTS_DIR, SHARED_FILE};
use letibot_turn::Endpoint;

const ROOT: &str = "s-split-paths";

/// A data directory and the folder the daemon serves, in a temp directory of this test's own.
struct Fixture {
    dir: PathBuf,
    workspace: PathBuf,
    data: PathBuf,
}

impl Fixture {
    fn new() -> Fixture {
        // A counter as well as the pid: two tests of one binary run in parallel, and a name
        // that only says the millisecond is one directory both of them open - and one of them
        // deletes while the other is writing.
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("letibot-split-paths-{}-{n}", std::process::id()));
        let workspace = dir.join("ws");
        let data = dir.join("data");
        std::fs::create_dir_all(&workspace).expect("the folder the daemon serves");
        std::fs::create_dir_all(&data).expect("the data directory");
        Fixture {
            dir,
            workspace,
            data,
        }
    }

    /// The daemon's own configuration: the byte vocabulary and a provider nothing is sent to,
    /// because no turn runs - the store is what this file is about.
    fn config(&self) -> Config {
        let mut cfg = Config::for_this_box(&self.workspace);
        cfg.dialect = Dialect::Qwen;
        cfg.seat = Seat::Leticode;
        cfg.endpoint = Endpoint::new("127.0.0.1", 1);
        cfg.http_retries = 0;
        cfg.context_window = Some(32_768);
        cfg.store = Some(self.data.clone());
        cfg.session_id = ROOT.to_string();
        cfg.web_search = None;
        cfg.permission = Vec::new();
        cfg.vocab_gguf = None;
        cfg.provider = Some(ProviderConfig {
            name: "deepseek".into(),
            model: None,
            api_key: Some("sk-not-used".into()),
            thinking: false,
        });
        cfg
    }

    fn project_file(&self) -> PathBuf {
        self.data.join(PROJECTS_DIR).join(format!(
            "{}.db",
            project_key(&self.workspace.to_string_lossy())
        ))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A count read straight out of one file, through the store's own door - so a row that landed
/// in the wrong file is the failure and not a file this test could not open.
fn rows(path: &std::path::Path, table: &str) -> i64 {
    let store = Store::open(path).unwrap_or_else(|e| panic!("opening {}: {e}", path.display()));
    store
        .connection_of(table)
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap_or_else(|e| panic!("counting {table} in {}: {e}", path.display()))
}

/// Is `table` one of the tables in this file at all? The assignment is a file's SCHEMA too: a
/// table the other file holds is not here with no rows, it is not here.
fn has_table(path: &std::path::Path, table: &str) -> bool {
    let store = Store::open(path).unwrap_or_else(|e| panic!("opening {}: {e}", path.display()));
    let n: i64 = store
        .connection_of("schema_version")
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |r| r.get(0),
        )
        .expect("sqlite_master");
    n > 0
}

/// `harnessd --store DIR --list-sessions`, as the launcher runs it: the real binary, so the
/// answer a person gets is what this asserts.
fn listing(fixture: &Fixture) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_harnessd"))
        .arg("--store")
        .arg(&fixture.data)
        .arg("--workspace")
        .arg(&fixture.workspace)
        .arg("--list-sessions")
        .output()
        .expect("the listing ran")
}

/// A daemon on a data directory writes the index to `shared.db` and the history to its project
/// file, and a listing reads the index alone: with the project file renamed away the session is
/// still there, because that is the file `--sessions` was split off from.
#[test]
fn a_daemon_writes_the_index_and_the_history_to_two_files() {
    let fixture = Fixture::new();
    let cfg = fixture.config();
    let parts = Parts::load(&cfg).expect("the byte vocabulary needs no model file");
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    let registry = Registry::new();
    registry.set_source(StoreSessions::open(&cfg).expect("the index opens"));
    registry
        .create(
            ROOT,
            "a split session",
            SessionWiring {
                model: cfg.model.clone(),
                dialect: "qwen".into(),
                endpoint: "http://127.0.0.1:1".into(),
                workspace: fixture.workspace.display().to_string(),
            },
        )
        .expect("the session is created");
    let mut sessions = Sessions::open_first(&parts, cfg, registry).expect("the daemon opens");
    sessions.open(ROOT).expect("the session opens");

    let shared = fixture.data.join(SHARED_FILE);
    let project = fixture.project_file();
    assert!(
        project.is_file(),
        "the daemon opened no project file at {}",
        project.display()
    );
    assert_eq!(
        rows(&shared, "session"),
        1,
        "the index is the shared file's, and it holds this session"
    );
    assert_eq!(
        rows(&project, "transcript"),
        1,
        "a session's conversation is its project file's"
    );
    assert!(
        !has_table(&shared, "transcript_item") && !has_table(&shared, "adjudication"),
        "shared.db holds no history and no decisions: a project file is where both are written"
    );

    // The listing, with the history renamed out of the way: the index is what it reads.
    let moved = project.with_extension("db.away");
    std::fs::rename(&project, &moved).expect("the project file is renamed away");
    let out = listing(&fixture);
    let listed = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        listed.contains(ROOT),
        "a listing that read a project file cannot answer without one:\n{listed}"
    );
    std::fs::rename(&moved, &project).expect("and renamed back");
}

/// A data directory that still holds the single `sessions.db` is refused BY NAME, with the
/// command that splits it: this daemon never migrates on its own.
#[test]
fn a_directory_that_still_holds_sessions_db_is_refused_with_the_migrations_name() {
    let fixture = Fixture::new();
    // The pre-split store, with nothing in it: the shape of a box nobody has migrated yet.
    Store::open(&fixture.data.join("sessions.db")).expect("a single-file store");
    let out = listing(&fixture);
    assert!(
        !out.status.success(),
        "a daemon started on an un-split store"
    );
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(
        said.contains("sessions.db") && said.contains("letibot-migrate-split"),
        "the refusal has to name the file and the command that splits it:\n{said}"
    );
    assert!(
        !fixture.data.join(SHARED_FILE).exists(),
        "nothing is migrated implicitly, so the refusal wrote no shared file"
    );
}

/// The daemon's own door refuses it too: a daemon that created the set beside `sessions.db` would
/// hide every session that file holds, and a resume would open each one a second time.
#[test]
fn a_daemon_does_not_create_a_set_beside_sessions_db() {
    let fixture = Fixture::new();
    let cfg = fixture.config();
    Store::open(&fixture.data.join("sessions.db")).expect("a single-file store");
    let refused = cfg
        .store_at()
        .expect("a store is configured")
        .open()
        .err()
        .expect("the daemon's open is refused");
    assert!(
        refused.to_string().contains("letibot-migrate-split"),
        "the refusal names the command that splits it: {refused}"
    );
    assert!(
        !fixture.data.join(SHARED_FILE).exists(),
        "the refused open created no shared file"
    );
}

/// The other half of that rule: once a data directory IS a split store, a `--store` that names
/// the old `sessions.db` inside it is refused BY NAME too - a daemon that went on writing that
/// file would be writing a store nothing reads.
#[test]
fn the_legacy_file_inside_a_split_directory_is_refused_by_name() {
    let fixture = Fixture::new();
    let cfg = fixture.config();
    // The daemon's own first open, which creates the set.
    StoreSessions::open(&cfg).expect("the set is created");
    assert!(fixture.data.join(SHARED_FILE).is_file());
    Store::open(&fixture.data.join("sessions.db")).expect("an old store beside it");
    let out = Command::new(env!("CARGO_BIN_EXE_harnessd"))
        .arg("--store")
        .arg(fixture.data.join("sessions.db"))
        .arg("--workspace")
        .arg(&fixture.workspace)
        .arg("--list-sessions")
        .output()
        .expect("the query ran");
    assert!(
        !out.status.success(),
        "a `--store` pointing at the old file was taken"
    );
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(
        said.contains("sessions.db") && said.contains("point --store at the DIRECTORY"),
        "the refusal has to say which file and where the store is:\n{said}"
    );
}

/// `Store::connection` is the index's file on a split set, so a raw read of the history through it
/// fails with `no such table` there and nowhere else. The history goes through `connection_of`.
#[test]
fn no_source_reads_the_history_through_the_index_connection() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = Vec::new();
    let mut stack = vec![src];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("the source tree") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("a source file");
            let lines: Vec<&str> = text.lines().collect();
            for (at, line) in lines.iter().enumerate() {
                if !line.contains(".connection()") {
                    continue;
                }
                let near = lines[at..lines.len().min(at + 6)].join("\n");
                if near.contains("FROM transcript") || near.contains("INTO transcript") {
                    found.push(format!("{}:{}", path.display(), at + 1));
                }
            }
        }
    }
    assert!(
        found.is_empty(),
        "these read the history through the index's connection: {found:#?}"
    );
}
