//! The corpus sweep (`docs/store-split.md` § Build plan, P4). The decisions live in one file per
//! project: a scoped read resolves through the shared index to ONE project file, `session=any`,
//! `counts` and `/gate` sweep every file read-only, and a file the sweep cannot read is named in
//! the answer while the rest is still returned. The start-up count stays the daemon's own file.

use std::path::PathBuf;

use letibot_harnessd::config::StoreAt;
use letibot_harnessd::corpus::StoreCorpus;
use letibot_harnessd::decision_source::StoreDecisions;
use letibot_harnessd::slash::{GateVerb, gate};
use letibot_tokencore::migrate_split::project_key;
use letibot_tokencore::store::split::{PROJECTS_DIR, SHARED_FILE};
use letibot_tokencore::store::{NewAdjudication, SessionRecord, Store};
use letibot_tools::builtins::decisions::{DecisionQuery, DecisionSource};

/// A data directory of this test's own. A counter as well as the pid: two tests of one binary run
/// in parallel, and a name that only says the pid is one directory both of them open.
struct Fixture {
    dir: PathBuf,
    data: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "letibot-split-corpus-{name}-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let data = dir.join("data");
        std::fs::create_dir_all(&data).expect("the data directory");
        Fixture { dir, data }
    }

    fn workspace(project: &str) -> String {
        format!("/w/{project}")
    }

    fn path(&self, project: &str) -> PathBuf {
        self.data
            .join(PROJECTS_DIR)
            .join(format!("{}.db", project_key(&Fixture::workspace(project))))
    }

    /// One project file with one session and `decisions` rows, written the way that folder's
    /// daemon writes them: a set opened under the key its own workspace hashes to.
    fn write(&self, project: &str, session: &str, decisions: usize) {
        let store = Store::open_set(&self.data, &project_key(&Fixture::workspace(project)))
            .expect("a project's two files");
        store
            .put_session(&SessionRecord {
                id: session.into(),
                title: Some(project.into()),
                model_id: "m".into(),
                dialect_sha: "d".into(),
                workspace_root: Fixture::workspace(project),
                owner: "dead".into(),
                approvers: vec![],
                role: None,
                parent_session_id: None,
            })
            .expect("the session row, in the index");
        for n in 0..decisions {
            store
                .record_adjudication(&decision(&format!("adj-{session}-{n:03}"), session))
                .expect("a decision, in this project's file");
        }
    }

    /// A file that will not be read as a database: what a half-migrated or damaged project file
    /// looks like to the sweep.
    fn corrupt(&self, project: &str) {
        let path = self.path(project);
        for suffix in ["-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
        std::fs::write(&path, b"this is not a database").expect("a file that will not open");
    }

    fn at(&self) -> StoreAt {
        StoreAt::Set {
            dir: self.data.clone(),
            key: project_key(&Fixture::workspace("a")),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn decision(request_id: &str, session_id: &str) -> NewAdjudication {
    NewAdjudication {
        request_id: request_id.into(),
        session_id: session_id.into(),
        turn_id: "t1".into(),
        action: "read a file under the project".into(),
        baseline: "read".into(),
        tier: "may_approve".into(),
        trail_json: "[]".into(),
        shown: Some("brief — a file was read".into()),
        reply: Some("ALLOW 0".into()),
        consulted: Some(true),
        tool: "bash".into(),
        arguments_json: "{}".into(),
        mode: "automode-edits".into(),
        options_json: "[]".into(),
        agent: "leticode".into(),
        verdict: Some("allow".into()),
        verdict_by: Some("oracle:glm".into()),
        verdict_basis: Some("the operator asked for it".into()),
        effect: "admit".into(),
        asked: false,
        ..Default::default()
    }
}

fn any() -> DecisionQuery {
    DecisionQuery {
        session: Some("any".into()),
        limit: 12,
        ..Default::default()
    }
}

fn one(session: &str) -> DecisionQuery {
    DecisionQuery {
        session: Some(session.into()),
        limit: 12,
        ..Default::default()
    }
}

/// The done-test. Three project files: `session=any` returns rows from all three, a session-scoped
/// read returns only its own, and with one file unreadable the sweep names it and still returns
/// the other two.
#[test]
fn a_corpus_sweep_reads_every_project_file_and_a_scoped_read_opens_one() {
    let f = Fixture::new("three");
    f.write("a", "s-a", 1);
    f.write("b", "s-b", 1);
    f.write("c", "s-c", 1);
    assert!(f.data.join(SHARED_FILE).is_file(), "the index is shared.db");
    assert!(
        f.path("b").is_file() && f.path("c").is_file(),
        "one file per project"
    );

    let src = StoreDecisions::open(f.at(), "s-a".into()).expect("a corpus reader");

    // `session=any` is a sweep: every project file, read-only, in key order.
    let swept = src.find(&any()).expect("the whole corpus");
    assert_eq!(swept.matched, 3, "three decisions, one in each file");
    assert!(swept.unreadable.is_empty(), "{:?}", swept.unreadable);
    let mut sessions: Vec<&str> = swept.rows.iter().map(|r| r.session_id.as_str()).collect();
    sessions.sort();
    assert_eq!(sessions, ["s-a", "s-b", "s-c"]);
    assert_eq!(swept.session_id, "any");

    // A scoped read resolves through the index to that session's ONE file - here another
    // project's, which this daemon does not write.
    let theirs = src.find(&one("s-b")).expect("another project's file");
    assert_eq!(theirs.rows.len(), 1);
    assert_eq!(theirs.rows[0].session_id, "s-b");
    assert_eq!(
        theirs.matched, 1,
        "the count is that file's, not the corpus's"
    );

    // No `session` at all means this session, whose rows are in this daemon's own file.
    let mine = src
        .find(&DecisionQuery {
            limit: 12,
            ..Default::default()
        })
        .expect("this session's own file");
    assert_eq!(mine.rows.len(), 1);
    assert_eq!(mine.rows[0].session_id, "s-a");

    // `counts` follows the same two rules.
    let corpus = src.counts(Some("any")).expect("the corpus's counts");
    assert_eq!(corpus.total, 3);
    assert_eq!(corpus.measured, 3);
    assert!(corpus.unreadable.is_empty());
    let scoped = src.counts(Some("s-c")).expect("one project's counts");
    assert_eq!(scoped.total, 1);

    // The start-up count stays the daemon's own file: it is not a sweep and must not become one.
    let sink = StoreCorpus::open(f.at()).expect("the gate's sink");
    let (counted, lost, orphans) = sink.counts();
    assert_eq!(counted.total, 1, "the daemon's own project file, only");
    assert_eq!((lost, orphans), (0, 0));

    // A `request=` is an id, which is unique: the sweep finds it in whichever file holds it.
    let by_id = src
        .find(&DecisionQuery {
            request: Some("adj-s-c-000".into()),
            session: Some("any".into()),
            limit: 12,
            ..Default::default()
        })
        .expect("an id across the corpus");
    assert_eq!(by_id.rows.len(), 1);
    assert_eq!(by_id.rows[0].session_id, "s-c");

    // One file unreadable: the sweep NAMES it, and the other two still answer.
    drop(src);
    f.corrupt("c");
    let src = StoreDecisions::open(f.at(), "s-a".into()).expect("a corpus reader");
    let swept = src.find(&any()).expect("the rest of the corpus");
    assert_eq!(swept.matched, 2, "a and b: c could not be read");
    assert_eq!(swept.unreadable.len(), 1, "{:?}", swept.unreadable);
    assert!(
        swept.unreadable[0].contains(&project_key(&Fixture::workspace("c"))),
        "the answer names the file it could not read: {:?}",
        swept.unreadable
    );
    let counts = src.counts(Some("any")).expect("the rest of the corpus");
    assert_eq!(counts.total, 2);
    assert_eq!(counts.unreadable.len(), 1);

    // `/gate` is the same sweep: `counts` and `recent` are whole-corpus questions.
    let replied = gate(Some(&f.at()), &GateVerb::Counts);
    assert!(replied.ok, "{:?}", replied.lines);
    assert!(
        replied.lines[0].starts_with("2 decisions recorded"),
        "{:?}",
        replied.lines
    );
    assert!(
        replied
            .lines
            .iter()
            .any(|l| l.contains("could not be read")),
        "a count that leaves a file out says so: {:?}",
        replied.lines
    );
    let replied = gate(
        Some(&f.at()),
        &GateVerb::Recent {
            limit: 20,
            only_unlabelled: false,
        },
    );
    assert!(replied.ok, "{:?}", replied.lines);
    assert!(
        replied.lines.iter().any(|l| l.contains("adj-s-a-000"))
            && replied.lines.iter().any(|l| l.contains("adj-s-b-000")),
        "both readable files are listed: {:?}",
        replied.lines
    );
    assert!(
        !replied.lines.iter().any(|l| l.contains("adj-s-c-000")),
        "the unreadable file contributes nothing: {:?}",
        replied.lines
    );

    // And a scoped read of the damaged file is an error that names it, not an empty answer.
    let broken = src
        .find(&one("s-c"))
        .expect_err("a file that will not open");
    assert!(
        broken.contains(&project_key(&Fixture::workspace("c"))),
        "{broken}"
    );
}

/// A project file this build cannot read is one the sweep says it skipped, and the walk goes on:
/// the read-only door leaves the file exactly as it was.
#[test]
fn a_sweep_takes_no_write_lock_and_renames_nothing() {
    let f = Fixture::new("readonly");
    f.write("a", "s-a", 2);
    f.write("b", "s-b", 2);
    let before = std::fs::read(f.path("b")).expect("the file the other daemon writes");
    let src = StoreDecisions::open(f.at(), "s-a".into()).expect("a corpus reader");
    let swept = src.find(&any()).expect("the whole corpus");
    assert_eq!(swept.matched, 4);
    // Read-only is a fact about the door, not a promise: the file's bytes are untouched, and a
    // journal it left behind carries no frames of the sweep's.
    assert_eq!(
        std::fs::read(f.path("b")).expect("still there"),
        before,
        "the sweep wrote to a file it only read"
    );
    let wal = std::fs::metadata(format!("{}-wal", f.path("b").display()))
        .map(|m| m.len())
        .unwrap_or(0);
    assert_eq!(wal, 0, "the sweep wrote frames into a file it only read");
}

/// The measurement the doc's open question asks for: 29 project files, the open + count + the
/// newest-200 draw. A sweep that took a second would push `decisions` into the slow-call log and
/// read as a store problem (§ Open questions).
#[test]
fn a_sweep_of_twenty_nine_project_files_is_measured() {
    let f = Fixture::new("cost");
    const PROJECTS: usize = 29;
    const EACH: usize = 8;
    for i in 0..PROJECTS {
        f.write(&format!("p{i}"), &format!("s-p{i}"), EACH);
    }
    let at = f.at();
    let files = letibot_harnessd::corpus::project_files(&at);
    assert_eq!(files.len(), PROJECTS, "a project file each");

    let started = std::time::Instant::now();
    let (counted, unreadable) = letibot_harnessd::corpus::sweep_counts(&at);
    let (rows, _) = letibot_harnessd::corpus::sweep_recent(&at, 200, 200);
    let took = started.elapsed();

    assert!(unreadable.is_empty(), "{unreadable:?}");
    assert_eq!(counted.total, (PROJECTS * EACH) as u64);
    assert_eq!(rows.len(), 200, "one newest-200 over the whole corpus");
    // Measured, not guessed: the number the doc's open question now carries.
    eprintln!(
        "corpus sweep: {PROJECTS} project files, open + count + newest-200 in {} ms",
        took.as_millis()
    );
    assert!(
        took.as_millis() < 1000,
        "the sweep took {} ms, which is the second the doc says would read as a store problem",
        took.as_millis()
    );
}
