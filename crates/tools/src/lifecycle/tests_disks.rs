//! G4's done-whens, each against a fixture: caches gone one pass after a close with the worktree
//! still there, a shared target under its lock, a firecode run retired by whichever door this CLI
//! has, a copy-out archived before it is removed, a stale remote lease swept and bundled, and a host
//! this box could not reach marked `lost` and retried. Plus the one the box found for us: a session's
//! `/tmp` is not on the runtime tmpfs.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use letibot_tokencore::store::{
    HoldingClass, HoldingRow, HoldingState, OwnerKind, OwnerRow, OwnerState, Store,
};

use super::*;

/// A directory of its own per test, removed when it ends.
struct Fixture(PathBuf);

impl Fixture {
    fn new(tag: &str) -> Fixture {
        let p = std::env::temp_dir().join(format!(
            "letibot-disks-{tag}-{}-{}",
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

    fn file(&self, rel: &str, bytes: usize) -> PathBuf {
        let p = self.0.join(rel);
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(&p, "x".repeat(bytes)).unwrap();
        p
    }

    /// This box's places, pointed at the fixture: no firecode, no cgroup, no real `/proc`.
    fn places(&self) -> Places {
        Places {
            gc_root: self.at("gc"),
            box_id: "lab2x1".into(),
            settings: Settings::default(),
            data_dir: self.at("data"),
            rundir: self.at("run"),
            daemon_logs: self.at("logs"),
            firecode_runs: Some(self.at("runs")),
            firecode_cache: Some(self.at("cache")),
            proc_root: self.at("proc"),
            cgroup_fs: self.at("cg"),
            docker_engine: None,
        }
    }

    fn store(&self) -> Store {
        Store::open_in_memory().expect("an in-memory store")
    }

    fn owner(&self, _places: &Places, id: &str, state: OwnerState) -> OwnerRow {
        let repo = self.at("repo");
        OwnerRow {
            state,
            closed_ms: Some(now_ms() - 1000),
            ..OwnerRow::live(
                id,
                OwnerKind::Package,
                Some(repo.display().to_string()),
                now_ms(),
            )
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The hands, as a fixture: the firecode verb is a verdict the test sets, the lock is the REAL
/// `/proc/locks` read (`BoxHands::lock_held`), and a "host" is this machine's own shell - so a
/// script the production pass would send over ssh runs here against a fixture root.
type FirecodeFake = Box<dyn Fn(&[String]) -> Result<String, String> + Send + Sync>;

struct StubHands {
    real: BoxHands,
    firecode: FirecodeFake,
    calls: Mutex<Vec<Vec<String>>>,
    /// `None`: a host that answers. `Some(why)`: a host this box cannot ask.
    unreachable: Mutex<Option<String>>,
}

impl StubHands {
    fn new(firecode: impl Fn(&[String]) -> Result<String, String> + Send + Sync + 'static) -> Self {
        StubHands {
            real: BoxHands::new(),
            firecode: Box::new(firecode),
            calls: Mutex::new(Vec::new()),
            unreachable: Mutex::new(None),
        }
    }
}

impl Hands for StubHands {
    fn firecode(&self, args: &[String]) -> Result<String, String> {
        self.calls.lock().unwrap().push(args.to_vec());
        (self.firecode)(args)
    }

    fn lock_held(&self, target: &Path) -> Result<bool, String> {
        self.real.lock_held(target)
    }

    fn host_command(&self, _host: &str, script: &str) -> Result<String, String> {
        if let Some(why) = self.unreachable.lock().unwrap().clone() {
            return Err(why);
        }
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(script)
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }

    fn host_read(&self, _host: &str, path: &str) -> Result<Vec<u8>, String> {
        std::fs::read(path).map_err(|e| format!("{path}: {e}"))
    }
}

/// The one pass, with the fixture's hands and no firecode for the compute half: every owner here
/// is past its compute already, so the disk half is what runs.
fn pass(
    store: &Store,
    places: &Places,
    hands: &Arc<StubHands>,
    now: i64,
    only: Option<&str>,
) -> StoreResult<Pass> {
    let ops = Ops {
        by: "test".into(),
        firecode: None,
        hands: Arc::clone(hands) as Arc<dyn Hands>,
        ..Ops::here("test")
    };
    super::pass(store, places, &ops, only, now)
}

/// `git` with this box's config held out of it, so a fixture commits the same way everywhere.
fn git(root: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .current_dir(root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "letibot fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@letibot.invalid")
        .env("GIT_COMMITTER_NAME", "letibot fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@letibot.invalid")
        .args(args)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// **Done when** a package that builds leaves no directory under its owner root's `target/` and
/// `tmp/` one pass after close, and its worktree (a `work` holding) is still there.
#[test]
fn a_closed_owner_loses_its_caches_and_keeps_its_worktree() {
    let f = Fixture::new("caches");
    let places = f.places();
    let store = f.store();
    let owner = f.owner(&places, "s-1", OwnerState::Closed);
    register(&store, &places, &owner).expect("the owner and its root");
    let root = places.owner_root("s-1");
    f.file("gc/lab2x1/s-1/target/debug/deps/libx.rlib", 8192);
    f.file("gc/lab2x1/s-1/tmp/session/scratch.tmp", 1024);
    f.at("gc/lab2x1/s-1/wt");
    f.file("gc/lab2x1/s-1/wt/Cargo.toml", 32);
    refresh(&store, &owner, &places).expect("the pass that writes them down");

    let hands = Arc::new(StubHands::new(|_| Err("no firecode here".into())));
    let report = pass(&store, &places, &hands, now_ms(), Some("s-1")).expect("a pass");

    assert!(!root.join("target").exists(), "a build's target is a cache");
    assert!(!root.join("tmp").exists(), "a session's /tmp is a cache");
    assert!(root.join("wt").exists(), "a worktree is work, and it stays");
    assert!(
        report.kept.iter().any(|k| k.contains("wt")),
        "the pass says why the worktree stays: {:#?}",
        report.kept
    );
    let log = store.reap_events_of("s-1").expect("the journal");
    assert!(
        log.iter()
            .any(|e| e.act == "remove" && e.kind.as_deref() == Some("target")),
        "every removal is journaled: {log:#?}"
    );
    let rows = store.holdings_of("s-1").expect("a read");
    let state = |kind: &str| {
        rows.iter()
            .find(|r| r.kind == kind)
            .map(|r| r.state)
            .expect(kind)
    };
    assert_eq!(state("target"), HoldingState::Gone);
    assert_eq!(state("worktree"), HoldingState::Held);
    // And a second pass has nothing to do: an act is done when the thing is ABSENT.
    let again = pass(&store, &places, &hands, now_ms(), Some("s-1")).expect("a second pass");
    assert!(again.acts.is_empty(), "{:#?}", again.acts);
}

/// One reaper: the pass that closes an idle owner also removes its caches, and an owner held by the
/// operator keeps both its compute and its caches.
#[test]
fn the_pass_that_closes_an_owner_frees_its_caches_and_a_hold_keeps_both() {
    let f = Fixture::new("one-pass");
    let places = f.places();
    let store = f.store();
    let mut owner = f.owner(&places, "s-7", OwnerState::Live);
    owner.closed_ms = None;
    owner.idle_ttl_ms = Some(60_000);
    owner.hard_ttl_ms = None;
    owner.lease_ms = now_ms() - 120_000;
    register(&store, &places, &owner).expect("the owner and its root");
    let root = places.owner_root("s-7");
    f.file("gc/lab2x1/s-7/target/debug/deps/libx.rlib", 4096);
    refresh(&store, &owner, &places).expect("discovered");

    store.hold_owner("s-7", Some(0)).expect("a hold");
    let hands = Arc::new(StubHands::new(|_| Err("no firecode".into())));
    let held = pass(&store, &places, &hands, now_ms(), Some("s-7")).expect("a pass");
    let state = || store.owner("s-7").unwrap().unwrap().state;
    assert_eq!(state(), OwnerState::Live, "{held:#?}");
    assert!(root.join("target").exists(), "a hold keeps the caches too");
    assert!(
        held.kept.iter().any(|k| k.contains("holds it")),
        "{:#?}",
        held.kept
    );

    store.hold_owner("s-7", None).expect("a release");
    let report = pass(&store, &places, &hands, now_ms(), Some("s-7")).expect("a pass");
    assert_eq!(state(), OwnerState::Closed, "{report:#?}");
    assert!(
        !root.join("target").exists(),
        "closed and freed in one pass: {report:#?}"
    );
}

/// **Done when** a shared target over its quota with no lock held is removed, and one with the lock
/// held is kept and said. The lock is a real `flock` and the answer is a real `/proc/locks` read.
#[test]
fn a_shared_target_over_its_quota_goes_only_under_a_free_lock() {
    let f = Fixture::new("shared");
    let mut places = f.places();
    let target = f.at("target-landing");
    f.file("target-landing/debug/deps/big.rlib", 64 * 1024);
    places.settings.shared = vec![SharedTarget {
        path: target.clone(),
        quota: Some(4096),
        idle_days: None,
    }];
    let store = f.store();
    let hands = Arc::new(StubHands::new(|_| Err("no firecode".into())));

    let report = pass(&store, &places, &hands, now_ms(), None).expect("a pass");
    assert!(!target.exists(), "over quota and unlocked: removed");
    assert!(
        report.acts.iter().any(|a| a.kind == "target"),
        "{:#?}",
        report.acts
    );

    // The same target, rebuilt, with the lock `rrun-runner` takes BESIDE it held by this process.
    f.file("target-landing/debug/deps/big.rlib", 64 * 1024);
    let lock = std::fs::File::create(format!("{}.rrun.lock", target.display())).expect("the lock");
    use std::os::unix::io::AsRawFd;
    let took = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    if !took {
        println!("could not take the lock here — THIS IS NOT A PASS");
        return;
    }
    let report = pass(&store, &places, &hands, now_ms(), None).expect("a pass");
    assert!(target.exists(), "a build holds it: nothing is removed");
    assert!(
        report
            .kept
            .iter()
            .any(|k| k.contains("holds its lock") && k.contains("over its quota")),
        "{:#?}",
        report.kept
    );
}

/// A firecode without `gc --run` still retires a run: the design's fallback move into
/// `runs-archive/`, and it says which door it used. With the verb, firecode's own removal is used
/// and nothing is moved.
#[test]
fn a_run_directory_is_retired_by_the_verb_or_by_the_move() {
    let f = Fixture::new("runs");
    let places = f.places();
    let runs = places.firecode_runs.clone().expect("the runs dir");
    let run = "firecode-20261112-111454-2218966";
    f.file(&format!("runs/{run}/project"), 16);
    f.file(&format!("runs/{run}/owner"), 8);
    std::fs::write(f.0.join(format!("runs/{run}/owner")), "s-2").unwrap();
    let store = f.store();
    let mut owner = f.owner(&places, "s-2", OwnerState::Settled);
    owner.state = OwnerState::Settled;
    store.put_owner(&owner).unwrap();
    refresh(&store, &owner, &places).expect("the run is discovered");
    assert!(
        store
            .holdings_of("s-2")
            .unwrap()
            .iter()
            .any(|r| r.kind == super::VM && r.class == HoldingClass::Record),
        "a run whose firecracker is gone is a record"
    );

    let hands = Arc::new(StubHands::new(|_| {
        Err("`gc --help` names no `--run`".into())
    }));
    let report = pass(&store, &places, &hands, now_ms(), Some("s-2")).expect("a pass");
    assert!(!runs.join(run).exists(), "the run is out of `runs/`");
    let moved = runs
        .parent()
        .unwrap()
        .join("runs-archive")
        .join(stamp_of(now_ms()).get(..7).unwrap())
        .join(run);
    assert!(moved.is_dir(), "moved into {}", moved.display());
    assert!(
        report.acts.iter().any(|a| a.said.contains("runs-archive")),
        "{:#?}",
        report.acts
    );

    // And the other door: a firecode that has the verb removes it itself and nothing is moved.
    let run2 = "firecode-20261112-120000-99";
    f.file(&format!("runs/{run2}/project"), 16);
    std::fs::write(f.0.join(format!("runs/{run2}/owner")), "s-2").unwrap();
    let mut owner = owner.clone();
    owner.id = "s-2".into();
    store.put_owner(&owner).unwrap();
    refresh(&store, &owner, &places).expect("discovered");
    let runs2 = runs.clone();
    let path = runs.join(run2);
    let hands = Arc::new(StubHands::new(move |args| {
        assert!(
            args.starts_with(&["gc".to_string(), "--run".to_string()]),
            "{args:?}"
        );
        std::fs::remove_dir_all(&path).map_err(|e| e.to_string())?;
        Ok(String::new())
    }));
    pass(&store, &places, &hands, now_ms(), Some("s-2")).expect("a pass");
    assert!(!runs2.join(run2).exists());
    assert!(!moved.with_file_name(run2).exists(), "nothing was moved");
}

/// **A copy-out is work**: its commits are fetched into `refs/letibot/archive/<owner>/` and read
/// back before the directory goes.
#[test]
fn a_copy_out_is_in_the_archive_namespace_before_it_is_removed() {
    let f = Fixture::new("copyout");
    let places = f.places();
    let repo = f.at("repo");
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "one"]);

    // A copy-out: a clone with a commit the source does not have, and one changed file.
    let copy = f.at("cache/s-3-20261112-115500");
    git(
        &f.0,
        &[
            "clone",
            "-q",
            repo.to_str().unwrap(),
            copy.to_str().unwrap(),
        ],
    );
    std::fs::write(copy.join("b.txt"), "the guest's work\n").unwrap();
    git(&copy, &["add", "-A"]);
    git(&copy, &["commit", "-q", "-m", "the guest's own commit"]);
    std::fs::write(copy.join("a.txt"), "one\ntwo (uncommitted)\n").unwrap();
    let tip = git(&copy, &["rev-parse", "HEAD"]).trim().to_string();

    let store = f.store();
    let owner = f.owner(&places, "s-3", OwnerState::Settled);
    store.put_owner(&owner).unwrap();
    refresh(&store, &owner, &places).expect("the copy-out is discovered");
    let hands = Arc::new(StubHands::new(|_| Err("no firecode".into())));
    let report = pass(&store, &places, &hands, now_ms(), Some("s-3")).expect("a pass");

    assert!(!copy.exists(), "the copy-out is gone");
    let heads = git(
        &repo,
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/letibot/archive/s-3",
        ],
    );
    assert!(
        heads.contains(&tip),
        "the guest's commit is reachable from the archive: {heads}"
    );
    assert!(
        heads.contains("copy-out/20261112-115500/wip"),
        "and so is the uncommitted half: {heads}"
    );
    assert!(
        report
            .acts
            .iter()
            .any(|a| a.said.contains("refs/letibot/archive")),
        "{:#?}",
        report.acts
    );
}

/// **Done when** a remote owner whose lease is not renewed has its processes ended and its caches
/// removed by the host's own pass, a bundle holds its unfetched commit, and the home box later
/// fetches that bundle into the archive namespace - after which the bundle goes.
#[test]
fn a_stale_remote_lease_is_swept_bundled_and_fetched_home() {
    let f = Fixture::new("remote");
    let mut places = f.places();
    places.box_id = "lab2x1".into();
    let root = f.at("host");
    let work = f.at("host/repo/abc");
    git(&work, &["init", "-q", "-b", "main"]);
    std::fs::write(work.join("unfetched.txt"), "only here\n").unwrap();
    git(&work, &["add", "-A"]);
    git(
        &work,
        &["commit", "-q", "-m", "a commit the home box does not have"],
    );
    let tip = git(&work, &["rev-parse", "HEAD"]).trim().to_string();

    // The stamp the placement writes, with a lease two hours older than its own idle TTL.
    let owner = remote::RemoteOwner {
        owner: "s-4".into(),
        box_id: "lab2x1".into(),
        kind: "package".into(),
        created_ms: now_ms() - 7_200_000,
        lease_ms: now_ms() - 7_200_000,
        idle_ttl_ms: Some(super::PACKAGE_IDLE_TTL_MS),
        work: Some(work.display().to_string()),
        ledger: Some(f.at("host/.letibot-jobs/abc").display().to_string()),
    };
    let home = remote::owner_root(&root, "s-4");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join(OWNER_JSON), owner.line()).unwrap();
    f.file("host/.letibot-home/s-4/tmp/worker.tmp", 4096);

    let store = f.store();
    let mut row = OwnerRow::live(
        "s-4",
        OwnerKind::Package,
        Some(f.at("repo").display().to_string()),
        now_ms(),
    );
    row.state = OwnerState::Closed;
    store.put_owner(&row).unwrap();
    store
        .put_holding(
            &HoldingRow::new(
                "s-4",
                REMOTE_HOME,
                HoldingClass::Cache,
                home.display().to_string(),
                "owner.json there names the owner",
                now_ms(),
            )
            .on_host(Some("lubuntu2".into())),
        )
        .unwrap();
    // The home box's copy of the work is a repository the bundle can be fetched into.
    let repo = f.at("repo");
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "one"]);
    let mut row = row.clone();
    row.repo = Some(repo.display().to_string());
    store.put_owner(&row).unwrap();

    let hands = Arc::new(StubHands::new(|_| Err("no firecode".into())));
    let report = pass(&store, &places, &hands, now_ms(), None).expect("a pass");

    assert!(
        !home.join("tmp").exists(),
        "the stale owner's caches are gone from that host"
    );
    assert!(work.exists(), "and its tree is NOT removed");
    let bundle = root.join(".letibot-archive/s-4.bundle");
    assert!(!bundle.exists(), "the bundle came home and was removed");
    let heads = git(
        &repo,
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/letibot/archive/s-4",
        ],
    );
    assert!(
        heads.contains(&tip),
        "the unfetched commit is in the archive namespace: {heads}"
    );
    assert!(
        report.acts.iter().any(|a| a.kind == "bundle"),
        "{:#?}",
        report.acts
    );
    let row = store
        .holdings_of("s-4")
        .unwrap()
        .into_iter()
        .find(|r| r.host.as_deref() == Some("lubuntu2"))
        .expect("its row");
    assert_eq!(row.state, HoldingState::Gone, "the host's pass touched it");
}

/// **Done when** a pass with the remote unreachable marks its holdings `lost`, removes nothing local
/// that belongs to it, and succeeds on the next pass.
#[test]
fn an_unreachable_host_is_lost_and_the_next_pass_succeeds() {
    let f = Fixture::new("lost");
    let mut places = f.places();
    places.box_id = "lab2x1".into();
    let root = f.at("host");
    let home = remote::owner_root(&root, "s-5");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join(OWNER_JSON),
        remote::RemoteOwner {
            owner: "s-5".into(),
            box_id: "lab2x1".into(),
            kind: "package".into(),
            created_ms: 1,
            lease_ms: 1,
            idle_ttl_ms: Some(super::PACKAGE_IDLE_TTL_MS),
            work: None,
            ledger: None,
        }
        .line(),
    )
    .unwrap();

    let store = f.store();
    let mut owner = OwnerRow::live(
        "s-5",
        OwnerKind::Package,
        Some(f.at("repo").display().to_string()),
        now_ms(),
    );
    owner.state = OwnerState::Closed;
    store.put_owner(&owner).unwrap();
    let row = HoldingRow::new(
        "s-5",
        REMOTE_HOME,
        HoldingClass::Cache,
        home.display().to_string(),
        "owner.json there names the owner",
        now_ms(),
    )
    .on_host(Some("lubuntu2".into()));
    store.put_holding(&row).unwrap();

    let hands = Arc::new(StubHands::new(|_| Err("no firecode".into())));
    *hands.unreachable.lock().unwrap() = Some("ssh: no route to host".into());
    let report = pass(&store, &places, &hands, now_ms(), None).expect("a pass");
    assert_eq!(report.lost.len(), 1, "{:#?}", report.lost);
    assert!(report.lost[0].2.contains("no route"), "{:#?}", report.lost);
    assert!(
        home.exists(),
        "nothing local was removed for an unreachable host"
    );
    let back = store
        .holdings_of("s-5")
        .unwrap()
        .into_iter()
        .find(|r| r.host.as_deref() == Some("lubuntu2"))
        .unwrap();
    assert_eq!(back.state, HoldingState::Lost);
    assert_eq!(back.attempts, 1);

    // The host answers on the next pass, and the pass succeeds.
    *hands.unreachable.lock().unwrap() = None;
    let report = pass(&store, &places, &hands, now_ms(), None).expect("the next pass");
    assert!(report.lost.is_empty(), "{:#?}", report.lost);
    let back = store
        .holdings_of("s-5")
        .unwrap()
        .into_iter()
        .find(|r| r.host.as_deref() == Some("lubuntu2"))
        .unwrap();
    assert_ne!(back.state, HoldingState::Lost);
}

/// A session's `/tmp` and its spill directory are under its owner root, on disk - never the runtime
/// directory, which is RAM. On 2026-11-13 a fuzz job's temp dirs there filled a 19 GiB tmpfs and
/// broke every other session's gate.
#[test]
fn a_sessions_tmp_is_not_under_the_runtime_directory() {
    let f = Fixture::new("tmpdir");
    let runtime = f.at("run-user");
    // SAFETY: this test's only reader of the variable is `session_dirs`, which does not read it;
    // the value is here so the assertion can name what it must not be under.
    unsafe { std::env::set_var("XDG_RUNTIME_DIR", &runtime) };
    let places = f.places();
    let (spill, session) = session_dirs(&places, "s-6").expect("the session's temp directories");
    for p in [&spill, &session] {
        assert!(
            !p.starts_with(&runtime),
            "{} is on the runtime tmpfs",
            p.display()
        );
        assert!(
            p.starts_with(places.owner_root("s-6")),
            "{} is not under the owner root",
            p.display()
        );
    }
    assert!(spill.is_dir() && session.is_dir());
}

/// The home box's lease renewal, over a fixture host: the stamp's lease moves, and a second renewal
/// inside the same minute costs nothing - a supervisor ticks every second.
#[test]
fn a_lease_is_renewed_on_the_host_and_only_once_a_minute() {
    let f = Fixture::new("renew");
    let root = f.at("host");
    let home = remote::owner_root(&root, "s-7");
    std::fs::create_dir_all(&home).unwrap();
    let stamp = remote::RemoteOwner {
        owner: "s-7".into(),
        box_id: "lab2x1".into(),
        kind: "package".into(),
        created_ms: 1,
        lease_ms: 1,
        idle_ttl_ms: Some(super::PACKAGE_IDLE_TTL_MS),
        work: None,
        ledger: None,
    };
    std::fs::write(home.join(OWNER_JSON), stamp.line()).unwrap();

    let mut roots = std::collections::BTreeMap::new();
    roots.insert("lubuntu2".to_string(), root.display().to_string());
    let renewer = LeaseRenewer::with_roots(
        std::sync::Arc::new(StubHands::new(|_| Err("no firecode".into()))),
        roots,
    );
    let sessions = vec!["s-7".to_string()];
    assert!(renewer.renew("lubuntu2", &sessions, 50_000).is_empty());
    let back = remote::parse_stamps(&std::fs::read_to_string(home.join(OWNER_JSON)).unwrap());
    assert_eq!(back[0].lease_ms, 50_000, "the lease moved");
    assert_eq!(back[0].created_ms, 1, "and nothing else did");

    // Inside the minute: no call, so the stamp stays where it was.
    assert!(renewer.renew("lubuntu2", &sessions, 50_100).is_empty());
    let back = remote::parse_stamps(&std::fs::read_to_string(home.join(OWNER_JSON)).unwrap());
    assert_eq!(back[0].lease_ms, 50_000);
    // A minute later it is due again.
    assert!(
        renewer
            .renew("lubuntu2", &sessions, 50_000 + RENEW_MS)
            .is_empty()
    );
    let back = remote::parse_stamps(&std::fs::read_to_string(home.join(OWNER_JSON)).unwrap());
    assert_eq!(back[0].lease_ms, 50_000 + RENEW_MS);
}

/// A closed owner's run, its booted copy and a rescue bundle beside it, with `copy_out` saying
/// `state` - the shape firecode leaves while `down` is still copying the project out.
fn copy_out_fixture(f: &Fixture, id: &str, run: &str, state: &str) -> (PathBuf, PathBuf, PathBuf) {
    f.file(&format!("runs/{run}/project"), 16);
    std::fs::write(f.0.join(format!("runs/{run}/owner")), id).unwrap();
    std::fs::write(
        f.0.join(format!("runs/{run}/copy_out")),
        format!("{state}\n"),
    )
    .unwrap();
    f.file(&format!("cache/{id}/src/lib.rs"), 64);
    let bundle = f.file(&format!("cache/{id}.rescue.bundle"), 128);
    (
        f.0.join("runs").join(run),
        f.0.join("cache").join(id),
        bundle,
    )
}

/// **The incident of 2026-11-13**: a sweep one second after close retired the run and removed the
/// copy while firecode was still copying it out. A `pending` copy-out keeps all of it.
#[test]
fn a_pending_copy_out_keeps_the_run_and_the_copy() {
    let f = Fixture::new("copyout-pending");
    let places = f.places();
    let run = "firecode-20261113-083420-2574138";
    let (dir, copy, bundle) = copy_out_fixture(&f, "s-7", run, "pending");
    let dest = f.at("cache/s-7-20261113-083420");
    let store = f.store();
    let owner = f.owner(&places, "s-7", OwnerState::Closed);
    store.put_owner(&owner).unwrap();
    refresh(&store, &owner, &places).expect("discovered");
    let hands = Arc::new(StubHands::new(|_| Err("no `gc --run` here".into())));
    let report = pass(&store, &places, &hands, now_ms(), Some("s-7")).expect("a pass");
    assert!(dir.is_dir(), "the run dir stays: {:#?}", report.acts);
    assert!(copy.is_dir(), "the copy stays: {:#?}", report.acts);
    assert!(dest.is_dir(), "the copy-out destination stays");
    assert!(bundle.is_file(), "the rescue bundle stays");
    assert!(
        report
            .kept
            .iter()
            .any(|k| k.contains("kept: copy-out still running")),
        "{:#?}",
        report.kept
    );
    assert!(
        !report
            .acts
            .iter()
            .any(|a| a.kind == "copy" || a.kind == "vm"),
        "{:#?}",
        report.acts
    );
}

/// The same with the launcher alive and the deadline long past: a live process is still copying.
#[test]
fn a_live_launcher_keeps_the_copy_past_the_deadline() {
    let f = Fixture::new("copyout-live");
    let places = f.places();
    let run = "firecode-20261113-083420-77";
    let (dir, copy, _) = copy_out_fixture(&f, "s-8", run, "pending");
    std::fs::write(f.0.join(format!("runs/{run}/launcher")), "4242\n").unwrap();
    let stat = f.file("proc/4242/stat", 0);
    std::fs::write(stat, "4242 (firecode) S 1 1 1").unwrap();
    let store = f.store();
    let owner = OwnerRow {
        closed_ms: Some(now_ms() - 2 * COPY_OUT_DEADLINE_MS),
        ..f.owner(&places, "s-8", OwnerState::Closed)
    };
    store.put_owner(&owner).unwrap();
    refresh(&store, &owner, &places).expect("discovered");
    let hands = Arc::new(StubHands::new(|_| Err("no `gc --run` here".into())));
    let report = pass(&store, &places, &hands, now_ms(), Some("s-8")).expect("a pass");
    assert!(dir.is_dir() && copy.is_dir(), "{:#?}", report.acts);
    assert!(
        report.kept.iter().any(|k| k.contains("pid 4242 alive")),
        "{:#?}",
        report.kept
    );
}

/// A finished copy-out lets the run be retired and the copy go; the rescue bundle beside the copy
/// is not the disk half's to remove.
#[test]
fn a_done_copy_out_lets_the_run_and_the_copy_go() {
    let f = Fixture::new("copyout-done");
    let places = f.places();
    let run = "firecode-20261113-083420-78";
    let (dir, copy, bundle) =
        copy_out_fixture(&f, "s-9", run, "done /tmp/elsewhere/s-9-20261113-083420");
    let store = f.store();
    let owner = f.owner(&places, "s-9", OwnerState::Closed);
    store.put_owner(&owner).unwrap();
    refresh(&store, &owner, &places).expect("discovered");
    let hands = Arc::new(StubHands::new(|_| Err("no `gc --run` here".into())));
    let report = pass(&store, &places, &hands, now_ms(), Some("s-9")).expect("a pass");
    assert!(!dir.exists(), "the run is retired: {:#?}", report.kept);
    assert!(!copy.exists(), "the copy is gone: {:#?}", report.kept);
    assert!(bundle.is_file(), "the rescue bundle stays");
}

/// `pending` past the deadline with nothing alive is a failed copy-out: the copy stays, both rows
/// are `stuck`, and the journal says so once.
#[test]
fn a_pending_copy_out_past_its_deadline_is_stuck_and_keeps_the_copy() {
    let f = Fixture::new("copyout-stuck");
    let places = f.places();
    let run = "firecode-20261113-083420-79";
    let (dir, copy, bundle) = copy_out_fixture(&f, "s-10", run, "pending");
    let store = f.store();
    let owner = OwnerRow {
        closed_ms: Some(now_ms() - COPY_OUT_DEADLINE_MS - 1000),
        ..f.owner(&places, "s-10", OwnerState::Closed)
    };
    store.put_owner(&owner).unwrap();
    refresh(&store, &owner, &places).expect("discovered");
    let hands = Arc::new(StubHands::new(|_| Err("no `gc --run` here".into())));
    for _ in 0..2 {
        let report = pass(&store, &places, &hands, now_ms(), Some("s-10")).expect("a pass");
        assert!(
            dir.is_dir() && copy.is_dir() && bundle.is_file(),
            "{:#?}",
            report.acts
        );
        assert!(
            report
                .kept
                .iter()
                .any(|k| k.contains("copy-out did not finish")),
            "{:#?}",
            report.kept
        );
    }
    for kind in ["copy", "vm"] {
        let row = holding(&store, "s-10", kind).expect("a row");
        assert_eq!(row.state, HoldingState::Stuck, "{kind}: {row:?}");
    }
    let stuck = store
        .reap_events_of("s-10")
        .unwrap()
        .into_iter()
        .filter(|e| e.act == "stuck")
        .count();
    assert_eq!(stuck, 2, "one `stuck` per holding, not one per pass");
}

/// Every holding of one owner in the store, for a test that wants to look at one.
#[allow(dead_code)]
fn holding(store: &Store, owner: &str, kind: &str) -> Option<HoldingRow> {
    store
        .holdings_of(owner)
        .ok()?
        .into_iter()
        .find(|r| r.kind == kind)
}

/// A placed session's home (with a read-only Go module cache in it), its ledger, and a fanout
/// call's work dir and ledger, all recorded on one host, under one owner in `state`.
fn remote_fixture(f: &Fixture, store: &Store, id: &str, state: OwnerState) -> Vec<PathBuf> {
    let root = f.at("host");
    let home = remote::owner_root(&root, id);
    std::fs::create_dir_all(&home).unwrap();
    let stamp = remote::RemoteOwner {
        owner: id.into(),
        box_id: "lab2x1".into(),
        kind: "session".into(),
        created_ms: now_ms(),
        lease_ms: now_ms(),
        idle_ttl_ms: Some(super::PACKAGE_IDLE_TTL_MS),
        work: None,
        ledger: None,
    };
    std::fs::write(home.join(OWNER_JSON), stamp.line()).unwrap();
    let module = f.file(
        &format!("host/.letibot-home/{id}/go/pkg/mod/x@v1/x.go"),
        2048,
    );
    let ledger = f.at(&format!("host/.letibot-jobs/{id}"));
    let fanout = f.at(&format!("host/fanout-call_{id}"));
    f.file(&format!("host/fanout-call_{id}/out.log"), 4096);
    let fanout_ledger = f.at(&format!("host/.letibot-jobs/fanout-call_{id}"));
    use std::os::unix::fs::PermissionsExt;
    let ro = |p: &Path, mode| std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode));
    ro(&module, 0o444).unwrap();
    for d in module.ancestors().skip(1).take(4) {
        ro(d, 0o555).unwrap();
    }

    let mut owner = f.owner(&f.places(), id, state);
    owner.lease_ms = now_ms();
    store.put_owner(&owner).unwrap();
    let paths = vec![home, ledger, fanout, fanout_ledger];
    for (kind, p) in [REMOTE_HOME, REMOTE_JOBS, REMOTE_FANOUT, REMOTE_JOBS]
        .iter()
        .zip(&paths)
    {
        store
            .put_holding(
                &HoldingRow::new(
                    id,
                    *kind,
                    HoldingClass::Cache,
                    p.display().to_string(),
                    "recorded when it was made",
                    now_ms(),
                )
                .on_host(Some("lubuntu2".into())),
            )
            .unwrap();
    }
    paths
}

fn remote_rows(store: &Store, id: &str) -> Vec<HoldingRow> {
    store
        .holdings_of(id)
        .unwrap()
        .into_iter()
        .filter(|r| r.host.as_deref() == Some("lubuntu2"))
        .collect()
}

/// **Done when** a settled owner's home, ledgers and fanout dirs on a host are gone after one pass,
/// a read-only module cache included, and its rows say so.
#[test]
fn a_settled_owners_remote_dirs_are_removed() {
    let f = Fixture::new("remote-settled");
    let places = f.places();
    let store = f.store();
    let paths = remote_fixture(&f, &store, "s-8", OwnerState::Settled);
    let hands = Arc::new(StubHands::new(|_| Err("no firecode".into())));
    let report = pass(&store, &places, &hands, now_ms(), None).expect("a pass");

    for p in &paths {
        assert!(!p.exists(), "{} is still on the host", p.display());
    }
    let rows = remote_rows(&store, "s-8");
    assert_eq!(rows.len(), 4);
    assert!(
        rows.iter().all(|r| r.state == HoldingState::Gone),
        "{rows:#?}"
    );
    let freed: u64 = report
        .acts
        .iter()
        .filter(|a| a.owner == "s-8" && a.act == letibot_tokencore::store::ReapAct::Remove)
        .map(|a| a.bytes)
        .sum();
    assert!(freed >= 4096, "{:#?}", report.acts);
    assert!(
        remote::remote_holdings(&store, "lab2x1")
            .unwrap()
            .is_empty(),
        "nothing left to list"
    );
}

/// A held owner, a closed one and one with a live process there keep every dir, and `holds` lists
/// them per host with the size the pass measured.
#[test]
fn held_live_and_unsettled_owners_keep_their_remote_dirs() {
    let f = Fixture::new("remote-kept");
    let places = f.places();
    let store = f.store();
    let held = remote_fixture(&f, &store, "s-9", OwnerState::Settled);
    let mut row = store.owner("s-9").unwrap().unwrap();
    row.held_until_ms = Some(0);
    store.put_owner(&row).unwrap();
    let closed = remote_fixture(&f, &store, "s-10", OwnerState::Closed);
    let live = remote_fixture(&f, &store, "s-11", OwnerState::Settled);
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .env(crate::remote::SESSION_MARK, "s-11")
        .spawn()
        .expect("sleep");

    let hands = Arc::new(StubHands::new(|_| Err("no firecode".into())));
    let report = pass(&store, &places, &hands, now_ms(), None).expect("a pass");
    let _ = child.kill();
    let _ = child.wait();

    for p in held.iter().chain(&closed).chain(&live) {
        assert!(p.exists(), "{} was removed", p.display());
    }
    let kept = report.kept.join("\n");
    assert!(
        kept.contains("s-9 on lubuntu2: its dirs stay, the operator holds it"),
        "{kept}"
    );
    assert!(
        kept.contains("s-10 on lubuntu2: its dirs stay, its owner is `closed`"),
        "{kept}"
    );
    assert!(
        kept.contains("s-11 on lubuntu2: a process of it still runs"),
        "{kept}"
    );

    let by_host = remote::remote_holdings(&store, "lab2x1").unwrap();
    let rows = &by_host["lubuntu2"];
    assert_eq!(rows.len(), 12, "{rows:#?}");
    let fanout = rows
        .iter()
        .find(|r| r.owner_id == "s-10" && r.kind == REMOTE_FANOUT)
        .expect("the fanout row");
    assert!(fanout.bytes.is_some_and(|b| b >= 4096), "{fanout:#?}");
    let lines = remote::remote_holdings_lines(&by_host);
    assert!(
        lines[0].starts_with("lubuntu2: 12 holding(s), "),
        "{lines:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("s-10 fanout") && l.contains("fanout-call_s-10")),
        "{lines:#?}"
    );

    // The read-only cache would otherwise stop the fixture's own cleanup.
    let _ = std::process::Command::new("chmod")
        .args(["-R", "u+w"])
        .arg(f.at("host"))
        .status();
}

/// The removal refuses a handle that is not one of the three shapes, so a bad row cannot aim
/// `rm -rf` at a host's root or home.
#[test]
fn only_homes_ledgers_and_fanout_dirs_are_removable() {
    assert!(remote::removable_shape(
        "~/letibot-remote/.letibot-home/abc"
    ));
    assert!(remote::removable_shape("/r/.letibot-jobs/fanout-call_1"));
    assert!(remote::removable_shape("/r/fanout-call_1"));
    assert!(!remote::removable_shape("/r"));
    assert!(!remote::removable_shape("/r/.letibot-home"));
    assert!(!remote::removable_shape("/r/.letibot-home/.."));
    assert!(!remote::removable_shape("/r/fanout-"));
    assert!(!remote::removable_shape("/home/dead"));
}

/// The cargo home every session on a host shares is the MACHINE's, so it is not one of the three
/// shapes a reaper removes and no holding row is ever written for it. The one spelling that would be
/// a removable shape is the spelling the placement refuses, which is why it refuses it.
#[test]
fn a_shared_cargo_home_is_never_a_sessions_own_data() {
    assert!(
        !remote::removable_shape("/home/dead/.cargo"),
        "a shared cargo home outside the owner homes is not a shape the reaper removes"
    );
    assert!(
        remote::removable_shape("/r/.letibot-home/shared-cargo"),
        "and one INSIDE the owner homes is, which is why `cargo_home` there is refused at the \
         placement: the home would be counted and removed as one session's data"
    );
    // What a reaper does to a session's own home: `tmp` and `.cache`, and nothing else in it.
    let owner = remote::RemoteOwner {
        owner: "s-1".into(),
        box_id: "lab2x1".into(),
        kind: "session".into(),
        created_ms: 1,
        lease_ms: 2,
        idle_ttl_ms: Some(1_800_000),
        work: None,
        ledger: None,
    };
    let script = remote::reap_script(Path::new("/r"), "/r/.letibot-home/s-1", &owner, 10);
    assert!(
        !script.contains(".cargo") && !script.contains("registry"),
        "a reaper that named a cargo home would be reaping the one every session shares:\n{script}"
    );
}

/// The gate build caches a box's roots hold are in the plan a person and a bridge agent read: a
/// gate leaves no owner row, so without this the 223 GB of 2026-11-24 is in nobody's list.
#[test]
fn the_plan_lists_the_gate_build_caches_with_their_size_and_age() {
    let f = Fixture::new("gatecaches");
    let target = f.0.join("landing/letibot-0123abcdef/target/debug");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("blob"), vec![b'x'; 8192]).unwrap();
    let roots = gate_cache::Roots {
        landing: Some(f.0.join("landing")),
        ..Default::default()
    };
    let here = autoarchive::Here::this_process();
    let mut plan = Plan {
        now_ms: now_ms(),
        ..Default::default()
    };
    plan.read_gate_caches(&roots, &here);
    assert_eq!(plan.gate_caches.len(), 1, "{plan:#?}");
    let line = plan.render();
    assert!(
        line.contains("gate build caches") && line.contains("letibot-0123abcdef"),
        "{line}"
    );
    assert!(line.contains("8 KiB"), "the size is in the line: {line}");
    let json = plan.to_json();
    assert_eq!(json["gate_caches"][0]["bytes"], 8192);
    assert_eq!(
        json["gate_caches"][0]["dir"],
        f.0.join("landing/letibot-0123abcdef").display().to_string()
    );
}
