//! **One `letibot gc --now` ends what a killed daemon left, and `letibot hold` stops it**
//! (`docs/lifecycle.md` G2's third and fourth done-whens).
//!
//! The verb is driven as a person runs it - the real binary, its own `$HOME`, store, cache and
//! runtime dir in a fixture - because the claim is about what a process that never talks to a
//! daemon does to a box nobody is looking after. Everything it could kill is a fixture's own: a
//! `sleep` standing in for a job, a script named `firecracker` standing in for a VM.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use letibot_tokencore::migrate_split::project_key;
use letibot_tokencore::store::{OwnerKind, OwnerRow, OwnerState, ProcState, ProcessRecord, Store};

/// A temp directory of this test's own.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "letibot-gc-now-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("a temp dir");
    dir
}

const OWNER: &str = "s-1794509750373521593";
const REPO: &str = "/w/a-package";
const RUN: &str = "firecode-20261113-101010-4242";

/// A box in a directory: the split store, the owner root, firecode's records, the cache and the
/// runtime dir.
struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Fixture {
        let f = Fixture { root: scratch(tag) };
        // A checkout's layout, which is what firecode's own `runs_dir` reads: `<dir>/bin/<cli>`
        // with `<dir>/runs/` beside it.
        for d in ["home", "run", "cache", "bin", "runs"] {
            std::fs::create_dir_all(f.at(d)).expect("a fixture dir");
        }
        f
    }

    fn at(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// `$XDG_RUNTIME_DIR/letibot`: where the launcher writes a daemon's record, and the only place
    /// a reaper looks for one.
    fn rundir(&self) -> PathBuf {
        self.at("run/letibot")
    }

    /// The store: a set, opened the way the daemon opens one, with the owner's project file
    /// attached so the `process` rows are where a reaper looks for them.
    fn store(&self) -> Store {
        Store::open_set(&self.at("data"), &project_key(REPO)).expect("a split store")
    }

    /// A live package whose daemon is gone, with a job running and a VM up.
    fn a_dead_daemons_session(&self) -> Reaped {
        let now = letibot_metrics::now_ms() as i64;
        let store = self.store();
        let mut owner = OwnerRow::live(OWNER, OwnerKind::Package, Some(REPO.into()), now);
        owner.idle_ttl_ms = Some(30 * 60 * 1000);
        owner.hard_ttl_ms = Some(24 * 60 * 60 * 1000);
        store.put_owner(&owner).expect("the owner");

        // The job: a real process, recorded with its `(pid, start)` identity - the only thing a
        // reaper is allowed to signal.
        let job = std::process::Command::new("sleep")
            .arg("120")
            .spawn()
            .expect("a sleep standing in for a job");
        let info = letibot_tools::exec::procs::read(job.id()).expect("its /proc entry");
        store
            .put_process(&ProcessRecord {
                identity: format!("start:{}", info.start),
                state: ProcState::Running,
                ..ProcessRecord::starting(OWNER, "j1", None, job.id())
            })
            .expect("the job row");
        drop(store);

        // The daemon whose record is here and which is not: a pid nothing will ever have.
        std::fs::create_dir_all(self.rundir()).expect("the runtime dir");
        std::fs::write(
            self.rundir().join("key.json"),
            format!(
                r#"{{"workspace":"{REPO}","socket":"{}","pid":4000000000,"role":"head","model":"m","answers":"m","dialect":"d","bash":true,"started":"now"}}"#,
                self.rundir().join("key.sock").display()
            ),
        )
        .expect("the daemon record");

        // firecode's own records: a copy named for the owner under the cache, and the run's files.
        let cache = self.cache();
        std::fs::create_dir_all(cache.join(OWNER)).expect("the booted copy");
        let run = self.at("runs").join(RUN);
        std::fs::create_dir_all(&run).expect("the run dir");
        std::fs::write(run.join("project"), cache.join(OWNER).display().to_string())
            .expect("the run's project");
        std::fs::write(run.join("owner"), format!("{OWNER}\n")).expect("the run's owner stamp");
        Reaped(job)
    }

    /// Where letibot boots a copy: `$XDG_CACHE_HOME/letibot/firecode`.
    fn cache(&self) -> PathBuf {
        self.at("cache/letibot/firecode")
    }

    /// A stand-in firecracker: a script NAMED `firecracker`, run with `--id RUN`, that ignores
    /// SIGTERM the way a jailed VM does.
    fn firecracker(&self) -> std::process::Child {
        let script = self.at("bin/firecracker");
        let body = "#!/bin/sh\n[ \"$1\" = \"--id\" ] || exit 0\ntrap '' TERM\nwhile :; do sleep 0.2; done\n";
        std::fs::write(&script, body).expect("the stand-in");
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
                .expect("its mode");
        }
        let child = std::process::Command::new(&script)
            .args(["--id", RUN])
            .spawn()
            .expect("the stand-in firecracker starts");
        let began = Instant::now();
        while letibot_tools::firecode::held::firecrackers(Path::new("/proc"))
            .iter()
            .all(|(_, p)| *p != child.id())
            && began.elapsed() < Duration::from_secs(5)
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        child
    }

    /// firecode's CLI, whose `down` does nothing: the ladder under it is what takes the VM.
    fn firecode_cli(&self) {
        let bin = self.at("bin/firecode");
        std::fs::write(&bin, "#!/bin/sh\nexit 0\n").expect("the stub");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("its mode");
        // `held::runs_dir` reads `<bin>/../../runs`, so the CLI's own directory is the checkout.
        std::fs::create_dir_all(self.at("bin")).unwrap();
    }

    /// The verb, run the way a person's shell runs it: an environment that names only this
    /// fixture, so nothing here can reach the operator's own store, cache or runtime directory.
    fn run(&self, args: &[&str]) -> (bool, String, String) {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_letibot"))
            .args(args)
            .current_dir(&self.root)
            .env_clear()
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.at("bin").display()),
            )
            .env("HOME", self.at("home"))
            .env("XDG_DATA_HOME", self.at("home/.local/share"))
            .env("XDG_CACHE_HOME", self.at("cache"))
            .env("XDG_RUNTIME_DIR", self.at("run"))
            .env("LETIBOT_STORE", self.at("data"))
            .env("LETIBOT_GC_ROOT", self.at("gc"))
            .output()
            .expect("the verb runs");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn owner_state(&self) -> OwnerState {
        self.store()
            .owner(OWNER)
            .expect("a read")
            .expect("the owner row")
            .state
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Wait, briefly and without blocking for ever, for a pid to leave `/proc`. A fixture that waited
/// on the process itself would hang the suite on the very failure it is looking for.
fn wait_dead(pid: u32) -> bool {
    let began = Instant::now();
    while began.elapsed() < Duration::from_secs(20) {
        if letibot_tools::exec::procs::read(pid).is_none_or(|p| p.state == 'Z') {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

/// **Done when** a daemon SIGKILLed with a job and a VM running leaves neither after one
/// `letibot gc --now`, run by a process that never talks to a daemon.
///
/// Read as: the job's cgroup is what says it is running - here the `process` row's `(pid, start)`,
/// which is the hand a box with no cgroup uses - and the VM is brought down with firecode's own
/// ladder. The owner's row goes to `closed`, and the two are proved gone by `/proc` and by
/// firecode's own records rather than by what the verb printed.
#[test]
fn a_dead_daemons_job_and_vm_are_ended_by_one_gc_now() {
    let f = Fixture::new("dead");
    let job = f.a_dead_daemons_session();
    f.firecode_cli();
    let mut fc = f.firecracker();
    let job_pid = job.id();

    // `--now` names the owner so the reaper opens the owner's own project file: the `process`
    // rows are per project, and a pass that read only the index would find no job at all.
    let (ok, out, err) = f.run(&["gc", OWNER, "--now"]);
    let died = wait_dead(job_pid);
    let _ = fc.kill();
    let _ = fc.wait();
    assert!(ok, "`letibot gc --now` failed. stderr:\n{err}");
    assert!(
        died,
        "the job is still running after the pass:\n{out}\n{err}"
    );
    assert!(
        letibot_tools::firecode::held::firecrackers(Path::new("/proc"))
            .iter()
            .all(|(run, _)| run != RUN),
        "the VM is still up:\n{out}\n{err}"
    );
    assert_eq!(f.owner_state(), OwnerState::Closed, "{out}\n{err}");
    assert!(
        out.contains("closed with no compute left") || out.contains("close"),
        "the close is not in the answer:\n{out}"
    );
    let log = f.store().reap_events_of(OWNER).expect("the journal");
    assert!(
        log.iter().any(|e| e.act == "close"),
        "the close is not journaled: {log:?}"
    );
    assert!(
        log.iter()
            .any(|e| e.act == "kill" && e.kind.as_deref() == Some("process")),
        "the job's end is not journaled: {log:?}"
    );

    // And a second pass has nothing left: the reap is idempotent end to end.
    let (ok, out, err) = f.run(&["gc", OWNER, "--now"]);
    assert!(ok, "a second pass failed:\n{err}");
    assert!(
        out.contains("nothing to reap"),
        "a second pass over a closed owner did something:\n{out}"
    );
}

/// **Done when** `letibot hold` on that owner makes the same pass keep everything and say why.
#[test]
fn a_hold_stops_the_pass_and_says_why() {
    let f = Fixture::new("held");
    let job = f.a_dead_daemons_session();
    f.firecode_cli();
    let mut fc = f.firecracker();

    let (ok, out, err) = f.run(&["hold", OWNER]);
    assert!(ok, "`letibot hold` failed:\n{err}");
    assert!(out.contains("held for ever"), "{out}");
    assert_eq!(f.owner_state(), OwnerState::Live);

    let (ok, out, err) = f.run(&["gc", OWNER, "--now"]);
    assert!(ok, "`letibot gc --now` failed:\n{err}");
    assert!(
        out.contains("the operator holds it"),
        "the pass kept it without saying why:\n{out}"
    );
    assert_eq!(f.owner_state(), OwnerState::Live, "a held owner was closed");
    assert!(
        letibot_tools::exec::procs::read(job.id()).is_some_and(|p| p.state != 'Z'),
        "a held owner's job was killed:\n{out}"
    );
    assert!(
        letibot_tools::firecode::held::firecrackers(Path::new("/proc"))
            .iter()
            .any(|(run, _)| run == RUN),
        "a held owner's VM was brought down:\n{out}"
    );

    // Released, the same pass applies again - the hold is an opt-out and not a lock.
    let (ok, _, err) = f.run(&["release", OWNER]);
    assert!(ok, "`letibot release` failed:\n{err}");
    let (ok, out, err) = f.run(&["gc", OWNER, "--now"]);
    assert!(ok, "{err}");
    let died = wait_dead(job.id());
    let _ = fc.kill();
    let _ = fc.wait();
    assert!(died, "the released job is still running:\n{out}");
    assert_eq!(f.owner_state(), OwnerState::Closed, "{out}");
}

/// A verb that acts says what it will not do: `gc --now` reaps compute and leaves the work alone,
/// and the plan is still a plan.
#[test]
fn the_plan_removes_nothing_and_the_verb_says_so() {
    let f = Fixture::new("plan");
    let mut job = f.a_dead_daemons_session();
    let before = f.owner_state();
    let (ok, out, err) = f.run(&["gc", OWNER]);
    assert!(ok, "`letibot gc` failed:\n{err}");
    assert!(out.contains("this is a plan: nothing was removed"), "{out}");
    assert_eq!(f.owner_state(), before, "the plan changed the owner");
    assert!(
        letibot_tools::exec::procs::read(job.id()).is_some_and(|p| p.state != 'Z'),
        "the plan killed a job:\n{out}"
    );
    let _ = job.kill();
    let _ = job.wait();
}

/// A hold the operator asked for a week is a clock, and an owner the store does not know is a
/// refusal by name rather than a silent no-op.
#[test]
fn a_hold_is_a_clock_and_an_unknown_owner_is_refused() {
    let f = Fixture::new("clock");
    let _job = f.a_dead_daemons_session();
    let (ok, out, err) = f.run(&["hold", OWNER, "--for", "2d"]);
    assert!(ok, "`letibot hold --for 2d` failed:\n{err}");
    assert!(out.contains("held for 2d"), "{out}");
    let row = f.store().owner(OWNER).unwrap().unwrap();
    let left = row.held_until_ms.expect("a hold") - letibot_metrics::now_ms() as i64;
    assert!(
        (left - 2 * 24 * 60 * 60 * 1000).abs() < 60_000,
        "a two-day hold is {left}ms away"
    );

    let (ok, _, err) = f.run(&["hold", "s-nobody"]);
    assert!(
        !ok,
        "a hold on an owner the store never wrote answered success"
    );
    assert!(err.contains("no owner `s-nobody`"), "{err}");
}

/// A test's stand-in job: killed and reaped when the test is done with it, whether or not the
/// reaper under test got to it first, so no test leaves a zombie behind.
struct Reaped(std::process::Child);

impl std::ops::Deref for Reaped {
    type Target = std::process::Child;
    fn deref(&self) -> &std::process::Child {
        &self.0
    }
}

impl std::ops::DerefMut for Reaped {
    fn deref_mut(&mut self) -> &mut std::process::Child {
        &mut self.0
    }
}

impl Drop for Reaped {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
