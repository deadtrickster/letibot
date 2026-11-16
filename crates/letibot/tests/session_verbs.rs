//! `letibot session VERB` against a temp store: each verb does its job, and prints what the
//! daemon's old flag printed for the same store.

use letibot_tokencore::store::{SessionRecord, Store};
use std::path::{Path, PathBuf};

const ID: &str = "s-verbs-1";

fn store() -> (PathBuf, PathBuf) {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("letibot-session-verbs-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sessions.db");
    Store::open(&path)
        .unwrap()
        .put_session(&SessionRecord {
            id: ID.into(),
            title: Some("first".into()),
            model_id: "qwen".into(),
            dialect_sha: "aa".repeat(32),
            workspace_root: dir.display().to_string(),
            owner: "dead".into(),
            role: None,
            approvers: vec![],
            parent_session_id: None,
        })
        .unwrap();
    (dir, path)
}

/// The binary beside this test's own: sccache drops the hyphenated `CARGO_BIN_EXE_` variable,
/// so `env!` alone fails to compile under the shared cache.
fn bin() -> PathBuf {
    match option_env!("CARGO_BIN_EXE_letibot-multi") {
        Some(p) => PathBuf::from(p),
        None => {
            let exe = std::env::current_exe().unwrap();
            exe.parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("letibot-multi")
        }
    }
}

fn run(store: &Path, args: &[&str]) -> (i32, String, String) {
    let home = store.parent().unwrap().join("home");
    let out = letibot_testenv::command(bin())
        .args(args)
        .arg("--store")
        .arg(store)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env_remove("LETIBOT_SOCKET")
        .env_remove("LETIBOT_SESSION")
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn title(path: &Path) -> Option<String> {
    Store::open(path)
        .unwrap()
        .session(ID)
        .unwrap()
        .unwrap()
        .title
}

fn archived(path: &Path) -> bool {
    Store::open(path)
        .unwrap()
        .session(ID)
        .unwrap()
        .unwrap()
        .archived_ms
        .is_some()
}

#[test]
fn each_verb_works_and_matches_the_old_flag() {
    let (dir, p) = store();

    let new = run(&p, &["session", "list", "--tsv"]);
    let old = run(&p, &["daemon", "--list-sessions", "--tsv"]);
    assert_eq!(new.0, 0, "{new:?}");
    assert!(new.1.contains(ID), "the listing names the session: {new:?}");
    assert_eq!(new, old);

    let new = run(&p, &["session", "rename", ID, "second"]);
    assert_eq!(new.0, 0, "{new:?}");
    assert_eq!(title(&p).as_deref(), Some("second"));
    let old = run(&p, &["daemon", "--rename", ID, "second"]);
    assert_eq!(new, old);

    let new = run(&p, &["session", "archive", ID, "done"]);
    assert_eq!(new.0, 0, "{new:?}");
    assert!(archived(&p));
    let listed = run(&p, &["session", "list", "--tsv"]);
    assert!(
        !listed.1.contains(ID),
        "archived is out of the list: {listed:?}"
    );
    let only = run(&p, &["session", "list", "--tsv", "--archived-only"]);
    assert!(only.1.contains(ID), "{only:?}");

    let show = run(&p, &["session", "show", ID]);
    assert_eq!(show.0, 0, "{show:?}");
    assert!(show.1.contains(&format!("id        {ID}")), "{show:?}");
    assert!(show.1.contains("title     second"), "{show:?}");
    assert!(show.1.contains("ago: done"), "{show:?}");

    let new = run(&p, &["session", "unarchive", ID]);
    assert_eq!(new.0, 0, "{new:?}");
    assert!(!archived(&p));
    let old = run(&p, &["daemon", "--unarchive", ID]);
    assert_eq!(new, old);

    let missing = run(&p, &["session", "show", "s-nope"]);
    assert_ne!(missing.0, 0);
    assert!(missing.2.contains("no such session: s-nope"), "{missing:?}");

    let bad = run(&p, &["session", "rename"]);
    assert_eq!(bad.0, 2);
    assert!(bad.2.contains("rename needs an ID"), "{bad:?}");

    let _ = std::fs::remove_dir_all(dir);
}
