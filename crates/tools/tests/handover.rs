//! A session's survivors, handed to its heir: the half of T24 that is not reaping.
//!
//! A job in an `explicit` scope outlives the session that started it, which is right for a
//! long-lived checkout on a box that stays up and is an orphan the moment that session is a
//! subagent's. So it moves, and these tests are about the move being real: listed by the heir,
//! killable by the heir, and gone from the giver's table.

use letibot_tools::exec::{HostProcesses, ProcessHost, ScopeKind, SpawnRequest};

/// A root of its own, so two hosts in one process are two trees - which is the fact that makes
/// a re-derived cgroup directory useless and a handed-over one necessary.
fn root(tag: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!(
        "letibot-handover-{}-{tag}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&p).expect("temp root");
    p
}

/// `None`, with the apparatus says so, when this box has no cgroup v2 subtree to own a process
/// with. The skip is loud and `LETIBOT_REQUIRE_APPARATUS=1` turns it back into a failure.
fn host(tag: &str) -> Option<HostProcesses> {
    let root = root(tag);
    match HostProcesses::new(&root) {
        Ok(h) => Some(h),
        Err(e) => {
            eprintln!("{tag}: no cgroup v2 subtree here, exercising the refusal path instead: {e}");
            let msg = format!("{e}");
            assert!(
                msg.contains("nothing would reap it") || msg.contains("cgroup"),
                "a refusal must name what was missing: {msg}"
            );
            letibot_tokencore::apparatus::absent("a cgroup v2 subtree this user may write");
            None
        }
    }
}

fn spawn_sleep(host: &dyn ProcessHost, scope: ScopeKind, name: Option<&str>, slug: &str) -> String {
    let id = host
        .spawn(&SpawnRequest {
            command: "sleep 300".into(),
            slug: Some(slug.into()),
            cwd: ".".into(),
            scope,
            scope_name: name.map(str::to_string),
            background: true,
            env: vec![],
            tty: false,
            memory: None,
        })
        .expect("a sleep starts");
    id.0
}

/// A job in an `explicit` scope is what a session cannot take with it, and the heir gets it:
/// listed there, killable there, and gone from the giver's table so two tables cannot both
/// claim one process.
#[test]
fn a_session_hands_its_explicit_scope_job_to_its_heir_and_the_heir_can_kill_it() {
    let Some(child) = host("giver") else { return };
    let Some(parent) = host("heir") else { return };
    let handle = spawn_sleep(
        &child,
        ScopeKind::Explicit,
        Some("cache-warm"),
        "cache-warm",
    );

    let survivors = child.survivors();
    assert_eq!(
        survivors.len(),
        1,
        "a running job in an explicit scope is a survivor: {survivors:?}"
    );
    assert_eq!(survivors[0].handle, handle);
    assert_eq!(survivors[0].scope, "explicit.cache-warm");
    assert!(
        survivors[0].identity.starts_with("start:"),
        "the heir's watcher compares against a real identity; without one the job settles \
         `Dead` on its first look and the process runs on unwatched: {survivors:?}"
    );
    assert!(
        survivors[0].scope_dir.is_some() && survivors[0].owner_dir.is_some(),
        "the directories are the record: the heir's tree has a root of its own, so a \
         re-derived name would be a new empty cgroup: {survivors:?}"
    );

    let moved = letibot_tools::exec::hand_over(&child, &parent);
    assert_eq!(moved.len(), 1, "{moved:?}");
    assert!(moved[0].refused.is_none(), "{moved:?}");

    let seen = parent.job(&letibot_tools::exec::JobId(handle.clone()));
    let seen = seen.expect("the heir lists what it was handed");
    assert!(seen.state.is_running(), "{seen:?}");
    assert_eq!(seen.slug.as_deref(), Some("cache-warm"));
    assert!(
        child
            .job(&letibot_tools::exec::JobId(handle.clone()))
            .is_none(),
        "a job in two tables is two owners, which is the leak this exists to stop"
    );

    // And the kill reaches the PROCESS, not a directory named after it.
    let pid = seen.pid;
    parent
        .kill_job(&letibot_tools::exec::JobId(handle.clone()))
        .expect("the heir can end what it was handed");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut gone = false;
    while std::time::Instant::now() < deadline {
        if letibot_tools::exec::procs::read(pid).is_none() {
            gone = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        gone,
        "the kill must reach the process wherever its cgroup is; pid {pid} is still there, \
         so the hand-over moved a record and not a job"
    );
}

/// The other half of the rule: what does NOT survive is not offered. A background job in the
/// session's own scope dies with the session, so handing it on would be inventing work for the
/// heir and keeping a process alive past the only thing that was reaping it.
#[test]
fn a_session_scope_job_is_not_a_survivor_and_is_never_offered() {
    let Some(child) = host("giver-ss") else {
        return;
    };
    let Some(parent) = host("heir-ss") else {
        return;
    };
    let handle = spawn_sleep(&child, ScopeKind::Session, None, "build");
    assert!(
        child.survivors().is_empty(),
        "a session-scope job is reaped by the session, so it is not a survivor: {:?}",
        child.survivors()
    );
    let moved = letibot_tools::exec::hand_over(&child, &parent);
    assert!(moved.is_empty(), "{moved:?}");
    assert!(
        parent.job(&letibot_tools::exec::JobId(handle)).is_none(),
        "the heir must not be handed a job the giver's own end is about to reap"
    );
}
