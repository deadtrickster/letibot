//! The guard: a process under test cannot open a store in the default data directory, because a
//! row written there outlives the test and every later reader counts it as real.
//!
//! A refusal ENDS the process, so a refusal is proved by running this binary once more as a child:
//! an abort's only observable is how that process went. The doors that open are proved here.

use letibot_tokencore::Store;

/// How a child was asked to open the store, and where its data directory is.
const CHILD: &str = "LETIBOT_GUARD_CHILD";

#[test]
fn a_store_in_the_default_data_directory_ends_the_process_and_creates_nothing() {
    let root = std::env::temp_dir().join(format!("letibot-store-guard-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("data/letibot")).unwrap();

    // `open`: the file's own door. `set`: the data DIRECTORY a daemon is handed.
    for door in ["open", "set"] {
        let out = run_child(Some(door), Some("1"), &root);
        let said = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(
            !out.status.success() && out.status.code().is_none(),
            "{door} was refused by ending the process, not by an exit status: {:?} {said}",
            out.status
        );
        assert!(
            said.contains("must not open the store in")
                && said.contains("LETIBOT_REFUSE_DEFAULT_STORE"),
            "{door}: the refusal names the directory and the way out: {said}"
        );
        assert!(
            !root.join("data/letibot/sessions.db").exists()
                && !root.join("data/letibot/shared.db").exists(),
            "{door}: nothing was created - the refusal comes before the open"
        );
    }

    // The test's own directory is the whole point: it opens, and its file is there.
    std::fs::create_dir_all(root.join("scratch")).unwrap();
    let own = Store::open(&root.join("scratch/sessions.db")).expect("a store of the test's own");
    assert_eq!(
        own.path().map(|p| p.file_name().unwrap().to_owned()),
        Some("sessions.db".into())
    );
    drop(own);

    let _ = std::fs::remove_dir_all(&root);
}

/// A run that MEANS the default store opens it: the same path, with the word that says so - which
/// is how the operator's own daemon opens the directory it was installed for.
#[test]
fn a_run_that_means_the_default_store_opens_it() {
    let root = std::env::temp_dir().join(format!("letibot-store-meant-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("data/letibot")).unwrap();
    let out = run_child(Some("set"), Some("0"), &root);
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{said}");
    assert!(
        root.join("data/letibot/shared.db").is_file(),
        "the set the run meant was opened: {said}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// This binary as a child: it opens the store through `door` and must not come back. Run here in
/// the parent too, where nothing is asked of it, so the name is one test and not a second program.
#[test]
fn a_refused_door_comes_back_only_if_it_did_not_refuse() {
    let Ok(door) = std::env::var(CHILD) else {
        return;
    };
    let dir = letibot_paths::data_dir().expect("a data directory in this child");
    match door.as_str() {
        "open" => {
            drop(Store::open(&dir.join("sessions.db")));
        }
        "set" => {
            drop(Store::open_set(&dir, "abc"));
        }
        other => panic!("this child was asked to open through {other}"),
    }
}

/// The child, with its data directory and the word the guard reads.
fn run_child(
    door: Option<&str>,
    refuse: Option<&str>,
    root: &std::path::Path,
) -> std::process::Output {
    std::process::Command::new(std::env::current_exe().expect("this test binary"))
        .args([
            "--exact",
            "a_refused_door_comes_back_only_if_it_did_not_refuse",
            "--nocapture",
        ])
        .env(CHILD, door.unwrap_or_default())
        .env("XDG_DATA_HOME", root.join("data"))
        .env(
            letibot_paths::REFUSE_DEFAULT_STORE,
            refuse.unwrap_or_default(),
        )
        .output()
        .expect("the child runs")
}
