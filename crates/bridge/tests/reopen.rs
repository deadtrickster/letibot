//! A session whose daemon was stopped is reopened by id: `leticode_attach` starts the folder's
//! daemon on that session, and refuses an id the store does not have for the folder (R241).
//! Drives a fake `harnessd` that writes down its arguments and exits.

use std::path::{Path, PathBuf};

use letibot_bridge::tools::Bridge;
use letibot_tokencore::migrate_split::project_key;
use letibot_tokencore::store::{SessionRecord, Store};

fn scratch() -> PathBuf {
    let dir = letibot_sessionlog::testing::scratch_root().join(format!(
        "letibot-reopen-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("a temp dir");
    dir
}

#[test]
fn attach_on_a_stored_session_with_no_daemon_starts_one_on_that_id() {
    let dir = scratch();
    let (home, rundir, bin, data) = (
        dir.join("home"),
        dir.join("run"),
        dir.join("bin"),
        dir.join("data"),
    );
    let worktree = dir.join("worktree");
    for d in [&home, &rundir, &bin, &data, &worktree] {
        std::fs::create_dir_all(d).expect("a fixture directory");
    }
    let worktree = std::fs::canonicalize(&worktree).expect("canonical");
    let args = dir.join("args");
    letibot_tools::testing::write_stub(
        &bin.join("harnessd"),
        &format!(
            "#!/bin/sh\n[ $# -eq 0 ] && exit 0\necho \"$@\" > {}\nexit 1\n",
            args.display()
        ),
    );
    let room = bin.join("room");
    letibot_tools::testing::write_stub(&room, "#!/bin/sh\nexit 0\n");
    let store = Store::open_set(&data, &project_key(&worktree.to_string_lossy())).expect("a set");
    store
        .put_session(&SessionRecord {
            id: "s-1".into(),
            title: None,
            model_id: "m".into(),
            dialect_sha: "d".into(),
            workspace_root: worktree.display().to_string(),
            owner: "op".into(),
            approvers: vec![],
            role: None,
            parent_session_id: None,
        })
        .expect("the stored session");
    // SAFETY: this binary holds one test and no thread reading the environment has started.
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::remove_var("XDG_CONFIG_HOME");
        std::env::set_var("XDG_RUNTIME_DIR", &rundir);
        std::env::set_var("LETIBOT_BIN", &bin);
        std::env::set_var("LETIBOT_ROOM_SUPERVISOR", &room);
        std::env::set_var("LETIBOT_STORE", &data);
        std::env::set_var("LETIBOT_LOG", dir.join("daemon.log"));
        std::env::remove_var("LETIBOT_SOCKET");
    }
    let bridge = Bridge::new();
    let wt = worktree.display().to_string();

    let refused = bridge
        .attach(&wt, "s-never", true)
        .expect_err("an id the store does not have");
    assert!(refused.contains("no session `s-never`"), "{refused}");
    assert!(!args.exists(), "a refused attach started a daemon");

    // The fake exits, so the attach fails after the start; what is measured is the start.
    let _ = bridge.attach(&wt, "s-1", true);
    let said = std::fs::read_to_string(&args).expect("the daemon was started");
    assert!(said.contains("--session s-1"), "{said}");
    assert!(
        said.contains(&format!("--workspace {}", worktree.display())),
        "{said}"
    );
    let _ = std::fs::remove_dir_all(Path::new(&dir));
}
