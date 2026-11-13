//! G1's done-whens, each against a fixture rather than the box: a package owner with a stub VM,
//! a scope and a daemon; a row deleted by hand; a stranger beside an owner root; the firecode
//! copies accounted for by owner. The one box-dependent case (the real firecode cache) says so and
//! skips loudly, which is the tree's rule for apparatus.

use std::path::{Path, PathBuf};

use letibot_tokencore::store::{HoldingClass, OwnerKind, OwnerRow, OwnerState, Store};

use super::*;

/// A directory of its own per test, removed when it ends.
struct Fixture(PathBuf);

impl Fixture {
    fn new(tag: &str) -> Fixture {
        let p = std::env::temp_dir().join(format!(
            "letibot-lifecycle-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Fixture(p)
    }

    fn at(&self, rel: &str) -> PathBuf {
        let p = self.0.join(rel);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn file(&self, rel: &str, body: &str) -> PathBuf {
        let p = self.0.join(rel);
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(&p, body).unwrap();
        p
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A stub box: this process's own places, pointed at a fixture. Every path a discovery pass reads
/// is here, so the walk is proved with no cgroup, no firecode and no daemon of its own.
struct Stub {
    fixture: Fixture,
    places: Places,
    repo: PathBuf,
    sid: String,
    run: String,
    cache: PathBuf,
    /// The pid the fixture's daemon record names.
    daemon_pid: u32,
}

impl Stub {
    fn new(tag: &str) -> Stub {
        let fixture = Fixture::new(tag);
        let sid = "s-1794509750373521593".to_string();
        let run = "firecode-20261112-111454-2218966".to_string();
        let repo = fixture.at("work");
        let cache = fixture.at("cache");
        // The daemon's own cgroup, and the leaf it steps into for a memory delegation: the scope
        // lives under the leaf, which is where `Cgroup2::probe` puts it.
        fixture.file("proc/4242/cgroup", "0::/user.slice/session-test.scope\n");
        fixture.file(
            "proc/4242/stat",
            "4242 (harnessd) S 1 4242 4242 0 -1 4194560 100 0 0 0 1 0 0 0 20 0 1 0 9 0 0\n",
        );
        fixture.file("proc/5151/cmdline", &format!("firecracker\0--id\0{run}\0"));
        fixture.file(
            "proc/5151/stat",
            "5151 (firecracker) S 1 5151 5151 0 -1 4194560 100 0 0 0 1 0 0 0 20 0 1 0 9 0 0\n",
        );
        // The scope: two jobs in one turn, its memory at the cap, and 591 OOM kills - the case a
        // model's `ps` calls "a stale handle".
        let scope = format!("cg/user.slice/session-test.scope/daemon/letibot.4242.0/session.{sid}");
        fixture.file(&format!("{scope}/cgroup.events"), "populated 1\nfrozen 0\n");
        fixture.file(&format!("{scope}/cgroup.procs"), "6001\n6002\n");
        fixture.file(&format!("{scope}/memory.current"), "18253611008\n");
        fixture.file(&format!("{scope}/memory.peak"), "34359738368\n");
        fixture.file(&format!("{scope}/memory.max"), "34359738368\n");
        fixture.file(
            &format!("{scope}/memory.events"),
            "low 0\nhigh 0\nmax 12\noom 40\noom_kill 591\noom_group_kill 0\n",
        );
        // The VM: firecode's own run directory, with the owner file letibot writes at `up`.
        fixture.file(
            &format!("runs/{run}/project"),
            &format!("{}/{sid}", cache.display()),
        );
        fixture.file(&format!("runs/{run}/name"), "lb-1794509750");
        fixture.file(&format!("runs/{run}/owner"), &sid);
        // The copy the VM was booted on, and a copy-out beside it - the only copy of that work.
        fixture.file(
            &format!("cache/{sid}/Cargo.toml"),
            "x".repeat(4096).as_str(),
        );
        fixture.file(
            &format!("cache/{sid}-20261112-115500/manifest"),
            "y".repeat(2048).as_str(),
        );
        // The daemon's record, which names the owner's repo and a pid that is really there.
        let key = "0123456789ab";
        fixture.file(
            &format!("run/{key}.json"),
            &format!(
                r#"{{"workspace":"{}","socket":"/run/user/1000/letibot/{key}.sock","pid":4242}}"#,
                repo.display()
            ),
        );
        let places = Places {
            gc_root: fixture.at("gc"),
            box_id: "lab2x1".into(),
            settings: Settings::default(),
            data_dir: fixture.at("data"),
            rundir: fixture.at("run"),
            firecode_runs: Some(fixture.at("runs")),
            firecode_cache: Some(cache.clone()),
            proc_root: fixture.at("proc"),
            cgroup_fs: fixture.at("cg"),
            docker_engine: None,
        };
        Stub {
            fixture,
            places,
            repo,
            sid,
            run,
            cache,
            daemon_pid: 4242,
        }
    }

    fn owner(&self) -> OwnerRow {
        OwnerRow {
            kind: OwnerKind::Package,
            idle_ttl_ms: Some(30 * 60 * 1000),
            hard_ttl_ms: Some(24 * 60 * 60 * 1000),
            ..OwnerRow::live(
                self.sid.clone(),
                OwnerKind::Package,
                Some(self.repo.display().to_string()),
                now_ms(),
            )
        }
    }

    fn store(&self) -> Store {
        Store::open_in_memory().expect("an in-memory store")
    }
}

/// **Done when** a test opens a package session with a stub VM and two jobs and finds one owner
/// row, and holdings for the scope, the VM and the daemon, each with a stamp the discovery walk
/// re-derives from the filesystem alone.
#[test]
fn a_package_with_a_vm_two_jobs_and_a_daemon_is_one_owner_and_its_holdings() {
    let stub = Stub::new("package");
    let store = stub.store();
    let owner = stub.owner();
    register(&store, &stub.places, &owner).expect("the owner is written before anything it holds");

    let owners = store.owners().expect("a read");
    assert_eq!(owners.len(), 1, "one owner row: {owners:#?}");
    assert_eq!(owners[0].id, stub.sid);
    assert_eq!(owners[0].kind, OwnerKind::Package);
    assert_eq!(owners[0].state, OwnerState::Live);

    let rows = store.holdings_of(&stub.sid).expect("a read");
    let kind = |k: &str| rows.iter().find(|r| r.kind == k).cloned();
    let scope = kind("scope").expect("a scope holding");
    let vm = kind("vm").expect("a VM holding");
    let daemon = kind("daemon").expect("a daemon holding");
    // Two jobs are not two holdings: the compute holding is the scope, and the processes in it are
    // the count the kernel gives, not rows of their own.
    assert_eq!(
        rows.iter()
            .filter(|r| r.class == HoldingClass::Compute)
            .count(),
        3,
        "compute is the scope, the VM and the daemon: {rows:#?}"
    );
    assert!(
        scope.stamp.contains(&format!("session.{}", stub.sid)),
        "the scope's stamp names the kernel's own directory: {}",
        scope.stamp
    );
    assert!(
        scope
            .why
            .as_deref()
            .unwrap_or_default()
            .contains("2 processes"),
        "the scope says how many processes it holds: {:?}",
        scope.why
    );
    assert!(
        scope
            .why
            .as_deref()
            .unwrap_or_default()
            .contains("591 OOM kills"),
        "and what the kernel killed at the cap: {:?}",
        scope.why
    );
    assert_eq!(
        scope.bytes,
        Some(18_253_611_008),
        "memory.current is the size"
    );
    assert_eq!(vm.handle, stub.run, "the VM's handle is firecode's run id");
    assert!(
        vm.stamp.contains("owner"),
        "the stamp is the run's owner file"
    );
    assert!(
        daemon.handle.contains(&stub.daemon_pid.to_string()),
        "the daemon's handle is the pid its record names: {}",
        daemon.handle
    );
    // And the copy the VM was booted on, and its copy-out, are the owner's too.
    assert_eq!(kind("copy").expect("a booted copy").bytes, Some(4096));
    assert_eq!(kind("copy-out").expect("a copy-out").bytes, Some(2048));
    assert_eq!(
        kind("copy-out").unwrap().class,
        HoldingClass::Work,
        "a copy-out is the only copy of that work until it is fetched"
    );

    // Re-derived from the filesystem alone: the same walk finds the same handles with no row.
    let found = discover(&owner, &stub.places);
    let handles: Vec<&str> = found.iter().map(|f| f.handle.as_str()).collect();
    assert!(handles.contains(&scope.handle.as_str()), "{handles:#?}");
    assert!(handles.contains(&stub.run.as_str()), "{handles:#?}");
    assert!(
        handles
            .iter()
            .any(|h| h.contains(&stub.daemon_pid.to_string())),
        "{handles:#?}"
    );
    assert!(
        found
            .iter()
            .filter(|f| f.kind == "scope")
            .all(|f| f.use_.is_some_and(|u| u.alive() && u.procs == 2)),
        "the scope's own facts come with it"
    );
}

/// **Done when** a holding row deleted by hand is re-inserted by the next discovery pass.
#[test]
fn a_holding_deleted_by_hand_comes_back_on_the_next_pass() {
    let stub = Stub::new("reinsert");
    let store = stub.store();
    let owner = stub.owner();
    register(&store, &stub.places, &owner).expect("registered");
    let before = store.holdings_of(&stub.sid).expect("a read");
    assert!(before.len() >= 4, "{before:#?}");
    store
        .connection()
        .execute("DELETE FROM holding", [])
        .expect("a hand edit");
    assert!(store.holdings_of(&stub.sid).unwrap().is_empty());

    let after = refresh(&store, &owner, &stub.places).expect("the next pass");
    let rows = store.holdings_of(&stub.sid).expect("a read");
    let mut was: Vec<&str> = before.iter().map(|r| r.handle.as_str()).collect();
    let mut is: Vec<&str> = rows.iter().map(|r| r.handle.as_str()).collect();
    was.sort_unstable();
    is.sort_unstable();
    assert_eq!(was, is, "the pass wrote back every resource it found");
    assert_eq!(after.len(), rows.len());
}

/// A row whose resource is gone is marked `gone` rather than deleted, and the plan says so instead
/// of counting it as something to reap.
#[test]
fn a_resource_that_vanished_is_gone_and_not_reaped() {
    let stub = Stub::new("gone");
    let store = stub.store();
    let owner = stub.owner();
    register(&store, &stub.places, &owner).expect("registered");
    std::fs::remove_dir_all(stub.cache.join(&stub.sid)).expect("the copy goes");
    refresh(&store, &owner, &stub.places).expect("the next pass");
    let copy = store
        .holdings_of(&stub.sid)
        .unwrap()
        .into_iter()
        .find(|r| r.kind == "copy")
        .expect("its row is still here");
    assert_eq!(copy.state, HoldingState::Gone);
}

/// **Done when** a directory with no `owner.json` beside an owner root is listed under "looks like
/// ours" and has no verdict that reaps.
#[test]
fn a_directory_with_no_stamp_is_reported_and_never_reaped() {
    let stub = Stub::new("stranger");
    let store = stub.store();
    let owner = stub.owner();
    register(&store, &stub.places, &owner).expect("registered");
    // Somebody's leftovers, beside a real owner root, with no owner.json.
    let stray = stub.places.box_dir().join("pk-old-thing");
    std::fs::create_dir_all(stray.join("target")).unwrap();
    std::fs::write(stray.join("target/obj"), "z".repeat(1024)).unwrap();

    let plan = plan(&store, &stub.places, None, now_ms()).expect("a plan");
    let listed = plan
        .looks_like_ours
        .iter()
        .find(|s| s.handle == stray.display().to_string())
        .expect("the stray is reported");
    assert!(listed.why.contains("nothing"), "{listed:?}");
    assert_eq!(listed.bytes, Some(1024), "and with its size");
    // It belongs to no owner, so it holds no verdict at all - and the plan reaps nothing.
    assert!(
        !plan
            .owners
            .iter()
            .any(|o| o.id == "pk-old-thing" || o.holds.iter().any(|h| h.handle == listed.handle)),
        "a stranger is not an owner's holding"
    );
    assert_eq!(
        plan.would_reap(),
        (0, 0),
        "a live owner's plan reaps nothing"
    );
    let rendered = plan.render();
    assert!(rendered.contains("unrecorded"), "{rendered}");
    assert!(rendered.contains("pk-old-thing"), "{rendered}");
    assert!(
        rendered.contains("this is a plan: nothing was removed"),
        "{rendered}"
    );
}

/// A root that carries a stamp for an owner the store does not know is reported too, by the name
/// the stamp gives: the file is the proof, and it disagrees with the index.
#[test]
fn a_stamped_root_for_an_unknown_owner_is_reported_by_its_stamp() {
    let stub = Stub::new("unrecorded");
    let store = stub.store();
    let dir = stub.places.box_dir().join("s-999");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(OWNER_JSON),
        r#"{"owner":"s-999","box_id":"lab2x1","kind":"package","repo":null,"created_ms":1,"lease_ms":1}"#,
    )
    .unwrap();
    let plan = plan(&store, &stub.places, None, now_ms()).expect("a plan");
    let listed = plan
        .looks_like_ours
        .iter()
        .find(|s| s.handle == dir.display().to_string())
        .expect("the root is reported");
    assert!(listed.why.contains("s-999"), "{listed:?}");
    assert!(plan.would_reap().0 == 0);
}

/// **Done when** the plan accounts for the VM copy-outs and booted copies under the firecode cache
/// by owner, with sizes - read off a fixture here, and off `~/.cache/letibot/firecode` on a box
/// that has one.
#[test]
fn the_firecode_copies_are_accounted_for_by_owner_with_sizes() {
    let stub = Stub::new("copies");
    let store = stub.store();
    let owner = stub.owner();
    register(&store, &stub.places, &owner).expect("registered");
    let plan = plan(&store, &stub.places, None, now_ms()).expect("a plan");
    let mine = plan
        .owners
        .iter()
        .find(|o| o.id == stub.sid)
        .expect("its owner block");
    let copy = mine
        .holds
        .iter()
        .find(|h| h.kind == "copy")
        .expect("a copy");
    let out = mine
        .holds
        .iter()
        .find(|h| h.kind == "copy-out")
        .expect("a copy-out");
    assert_eq!(copy.bytes, Some(4096));
    assert_eq!(out.bytes, Some(2048));
    assert!(copy.handle.ends_with(&stub.sid), "{}", copy.handle);
    assert!(
        out.handle.contains(&format!("{}-", stub.sid)),
        "a copy-out is named for its owner and a stamp: {}",
        out.handle
    );
    let totals = plan.totals();
    assert_eq!(totals.get("work").copied().unwrap_or(0), 2048);
    assert!(totals.get("cache").copied().unwrap_or(0) >= 4096);
}

/// The real box's own cache, when it has one: the same walk, unrun on a box with no firecode. The
/// skip is loud and says `THIS IS NOT A PASS`, as every apparatus skip here does.
#[test]
fn this_boxs_real_firecode_cache_is_walked() {
    let places = Places::here();
    let Some(cache) = places.firecode_cache.as_deref() else {
        return;
    };
    if !cache.is_dir() {
        println!(
            "no firecode cache at {} — THIS IS NOT A PASS: this box has nothing to walk",
            cache.display()
        );
        return;
    }
    let known = std::collections::BTreeSet::new();
    let found = unowned(&places, &known);
    println!(
        "{} entries under {} answered as candidates by name",
        found.len(),
        cache.display()
    );
    // Whatever the cache holds, every row is named by an owner id this walk derived from the name
    // alone. Rows under the gc root are other tests' owners, walked in the same data directory.
    let under = cache.display().to_string();
    for f in found.iter().filter(|f| f.handle.starts_with(&under)) {
        assert!(
            f.owner.starts_with("unrecorded:") || !f.owner.is_empty(),
            "{f:?}"
        );
    }
}

/// A closed owner's plan is a plan and nothing else: the file it was built from is byte-identical
/// afterwards, and no directory under the owner root changed.
#[test]
fn a_plan_writes_nothing() {
    let stub = Stub::new("readonly");
    let path = stub.fixture.0.join("store.db");
    let store = Store::open(&path).expect("a store");
    let mut owner = stub.owner();
    owner.state = OwnerState::Closed;
    owner.closed_ms = Some(now_ms() - UNLANDED_GRACE_MS - 1000);
    register(&store, &stub.places, &owner).expect("registered");
    let before = std::fs::read(&path).expect("the file");
    let root_before = snapshot(&stub.places.owner_root(&owner.id));

    let plan = plan(&store, &stub.places, Some(&owner.id), now_ms()).expect("a plan");
    assert!(
        plan.would_reap().0 > 0,
        "a closed owner has something to reap"
    );
    let after = std::fs::read(&path).expect("the file");
    assert_eq!(before, after, "the store changed under a plan");
    assert_eq!(
        root_before,
        snapshot(&stub.places.owner_root(&owner.id)),
        "the owner root changed under a plan"
    );
}

/// Every file under `root` with its length and modification time.
fn snapshot(root: &Path) -> Vec<(String, u64, std::time::SystemTime)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            let name = entry.path().display().to_string();
            if meta.is_dir() {
                stack.push(entry.path());
                out.push((name, 0, meta.modified().unwrap_or(std::time::UNIX_EPOCH)));
            } else {
                out.push((
                    name,
                    meta.len(),
                    meta.modified().unwrap_or(std::time::UNIX_EPOCH),
                ));
            }
        }
    }
    out.sort();
    out
}

/// The owner roots default outside the store: a session's `TMPDIR` lives under them, and a
/// command's view hides the store, so a root inside it is a temp dir no command can write.
#[test]
fn the_default_owner_root_is_not_inside_the_store() {
    let root = super::default_gc_root();
    assert!(
        !root.starts_with(super::data_dir()),
        "{} is inside the store {}",
        root.display(),
        super::data_dir().display()
    );
}

/// A job a restart kept is a holding of its owner while no daemon runs it: `holds` names it
/// detached, the owner is busy, and a live owner keeps it.
#[test]
fn a_job_kept_across_a_restart_is_a_detached_holding_of_its_owner() {
    use letibot_tokencore::store::HoldingRow;
    let stub = Stub::new("detached");
    let store = stub.store();
    let owner = stub.owner();
    register(&store, &stub.places, &owner).expect("the owner");
    let mut row = HoldingRow::new(
        &stub.sid,
        DETACHED_JOB,
        HoldingClass::Compute,
        "j7",
        "pid 4343 start:42 in /sys/fs/cgroup/x/job-j7",
        now_ms(),
    );
    row.why = Some("detached across a restart: `./fuzz.sh`".into());
    store.put_holding(&row).expect("the kept job's holding");
    let plan = plan(&store, &stub.places, None, now_ms()).expect("a plan");
    let o = &plan.owners[0];
    let line = o.holds_line(now_ms());
    assert!(line.contains("job j7 detached"), "{line}");
    let job = o.holds.iter().find(|h| h.handle == "j7").expect("listed");
    assert!(!job.verdict.reaps(), "a live owner's kept job was reaped");
    assert!(job.stamp.contains("pid 4343 start:42"), "{}", job.stamp);
    assert!(reap::activity(&store, &owner, &stub.places).kept);
}
