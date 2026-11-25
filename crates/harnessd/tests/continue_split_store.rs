//! `--continue` and `--sessions` over a split data directory: two projects, rows in each, and the
//! store queries the launcher runs against them as the real binary. A listing that reads the one
//! project file it happens to have open answers `0` for every other folder's sessions, which is
//! what made `letibot --continue` refuse a folder whose session has thousands of rows.

use std::path::PathBuf;

use letibot_tokencore::migrate_split::project_key;
use letibot_tokencore::store::{SessionRecord, StablePrefixRecord, Store};

/// A data directory and the two folders it holds sessions for, in a temp directory of this test's
/// own. Two tests of one binary run in parallel, so the name carries a counter as well as the pid.
struct Fixture {
    dir: PathBuf,
    data: PathBuf,
    /// The data home the query resolves when `--store` is not what it is reading: a scratch, so
    /// nothing a test spawns can fall back to the operator's own data directory.
    xdg: PathBuf,
    a: String,
    b: String,
}

impl Fixture {
    fn new() -> Fixture {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("letibot-continue-split-{}-{n}", std::process::id()));
        let data = dir.join("data");
        std::fs::create_dir_all(&data).expect("the data directory");
        let xdg = dir.join("xdg-data");
        std::fs::create_dir_all(&xdg).expect("the data home");
        let (a, b) = (dir.join("ws-a"), dir.join("ws-b"));
        std::fs::create_dir_all(&a).expect("the first folder");
        std::fs::create_dir_all(&b).expect("the second folder");
        Fixture {
            dir,
            data,
            xdg,
            a: a.display().to_string(),
            b: b.display().to_string(),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// `harnessd` as the launcher runs it: the real binary, and a runtime directory of this test's own
/// so it never reads the box's live daemons.
fn harnessd(fixture: &Fixture, args: &[&str]) -> std::process::Output {
    let mut cmd = letibot_testenv::command(env!("CARGO_BIN_EXE_harnessd"));
    cmd.arg("--store").arg(&fixture.data);
    for a in args {
        cmd.arg(a);
    }
    cmd.env("XDG_RUNTIME_DIR", &fixture.dir)
        // The data home too: `--store` names the store, and nothing else may be resolved from the
        // operator's data directory by a process under test.
        .env("XDG_DATA_HOME", &fixture.xdg)
        .output()
        .expect("the query ran")
}

/// One named session of a project, with `rows` rows in that project's file - so the index records
/// the key and the rows are in `projects/<key>.db`.
fn seed(set: &Store, id: &str, workspace: &str, title: &str, rows: u32) {
    let prefix = StablePrefixRecord {
        dialect_sha: "d".into(),
        system: format!("system of {id}"),
        tools_json: vec![],
        tokens: vec![1, 2, 3],
        h_init: [0u8; 32],
        vocab_source: "test".into(),
        effort: None,
    };
    let prefix_id = set.put_stable_prefix(&prefix).expect("prefix");
    set.put_session(&SessionRecord {
        id: id.into(),
        title: Some(title.into()),
        model_id: "m".into(),
        dialect_sha: "d".into(),
        workspace_root: workspace.into(),
        owner: "dead".into(),
        approvers: vec![],
        role: None,
        parent_session_id: None,
    })
    .expect("session");
    let transcript = format!("{id}#t0");
    set.put_transcript(&transcript, id, &prefix_id)
        .expect("transcript");
    for seq in 0..rows {
        set.append_item(
            &transcript,
            seq,
            &letibot_transcript::TranscriptItem::User {
                speaker: Default::default(),
                parts: vec![letibot_transcript::UserPart::Text {
                    text: format!("row {seq} of {id}"),
                }],
            },
            &letibot_tokencore::ledger::LedgerRow {
                item_id: format!("{transcript}.{seq}"),
                // The prefix above is three tokens long, so the first row starts after them.
                tok_offset: 3 + seq * 3,
                tok_len: 3,
                h_k: [0u8; 32],
            },
            &[7, 8, 9],
        )
        .expect("a row");
    }
}

/// A store waited past a millisecond, so two sessions written in a row differ in activity: the
/// resolution's order is the question, and a tie would answer it by accident.
fn wait_past(after_ms: i64) {
    let now = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    };
    while now() <= after_ms {
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

/// The whole query the launcher runs for `letibot --continue`: the newest session under a folder
/// whose rows are in ANOTHER project's file than the one this command line's cwd names.
#[test]
fn latest_session_finds_a_folder_whose_rows_are_in_another_project_file() {
    let fixture = Fixture::new();
    let (ka, kb) = (project_key(&fixture.a), project_key(&fixture.b));
    let older;
    {
        // Two daemons' worth of store, each opened with its own folder's key.
        let a = Store::open_set(&fixture.data, &ka).expect("the first project's handle");
        seed(&a, "s-a", &fixture.a, "the first folder", 3);
        older = {
            let s = a.session("s-a").expect("held").expect("there");
            s.last_activity_ms
        };
        wait_past(older);
        let b = Store::open_set(&fixture.data, &kb).expect("the second project's handle");
        seed(&b, "s-b", &fixture.b, "the second folder", 5);
    }

    // The command line the launcher runs: the data directory, and the folder the person is in.
    // Its cwd is neither folder, which is what every `--store DIR` query from elsewhere is.
    let out = harnessd(&fixture, &["--latest-session", "--scope", &fixture.b]);
    let said = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "the query refused: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        said.trim(),
        "s-b",
        "the newest session under the folder, not nothing: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let note = String::from_utf8_lossy(&out.stderr);
    assert!(
        note.contains("5 row(s)"),
        "the pick says how many rows it found: {note}"
    );
}

/// The listing a person reads: `--sessions` (the TSV form) carries the real row count of every
/// project's sessions, not the index's zero.
#[test]
fn the_listing_carries_each_projects_real_row_count() {
    let fixture = Fixture::new();
    let (ka, kb) = (project_key(&fixture.a), project_key(&fixture.b));
    {
        let a = Store::open_set(&fixture.data, &ka).expect("the first project's handle");
        seed(&a, "s-a", &fixture.a, "the first folder", 3);
        let b = Store::open_set(&fixture.data, &kb).expect("the second project's handle");
        seed(&b, "s-b", &fixture.b, "the second folder", 5);
    }
    let out = harnessd(&fixture, &["--list-sessions", "--tsv"]);
    assert!(
        out.status.success(),
        "the listing refused: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let listed = String::from_utf8_lossy(&out.stdout).to_string();
    // Five tab-separated fields, and the count is the fourth.
    let count = |id: &str| {
        listed
            .lines()
            .find(|l| l.starts_with(&format!("{id}\t")))
            .and_then(|l| l.split('\t').nth(3))
            .map(str::to_string)
    };
    assert_eq!(count("s-a"), Some("3".to_string()), "in {listed}");
    assert_eq!(count("s-b"), Some("5".to_string()), "in {listed}");
}

/// The plain listing is the pick list - this folder's block first, then one block per other
/// folder - and `--all` is the census behind it, grouped by what each session is.
#[test]
fn the_plain_listing_is_this_folder_first_and_all_is_the_census() {
    let fixture = Fixture::new();
    let (ka, kb) = (project_key(&fixture.a), project_key(&fixture.b));
    {
        let a = Store::open_set(&fixture.data, &ka).expect("the first project's handle");
        seed(&a, "s-a", &fixture.a, "the first folder", 3);
        let b = Store::open_set(&fixture.data, &kb).expect("the second project's handle");
        seed(&b, "s-b", &fixture.b, "the second folder", 5);
    }
    // The daemon's own `--workspace`, which is the folder this command was run in.
    let out = harnessd(&fixture, &["--list-sessions", "--workspace", &fixture.a]);
    assert!(
        out.status.success(),
        "the listing refused: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let listed = String::from_utf8_lossy(&out.stdout).to_string();
    let here = listed
        .find(&format!("{} (1)", fixture.a))
        .unwrap_or_else(|| panic!("this folder's block is first:\n{listed}"));
    let there = listed
        .find(&format!("{} (1)", fixture.b))
        .unwrap_or_else(|| panic!("the other folder has a block of its own:\n{listed}"));
    assert!(here < there, "this folder comes first:\n{listed}");
    assert!(
        listed
            .lines()
            .any(|l| l.split_whitespace().last() == Some("s-a"))
            && listed
                .lines()
                .any(|l| l.split_whitespace().last() == Some("s-b")),
        "both conversations are in it, one line each:\n{listed}"
    );
    // `--all` is the census: grouped by what each session is, which is where a conversation that
    // knows nothing about folders goes.
    let census = harnessd(
        &fixture,
        &["--list-sessions", "--all", "--workspace", &fixture.a],
    );
    let said = String::from_utf8_lossy(&census.stdout).to_string();
    assert!(
        said.contains("mine (2)"),
        "the census groups every session by what it is:\n{said}"
    );
}
