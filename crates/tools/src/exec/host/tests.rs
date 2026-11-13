#[test]
fn a_netstat_listen_row_names_its_port_and_pid() {
    let t = "Proto Recv-Q Send-Q  Local Address  Foreign Address  (state) rxbytes txbytes rhiwat shiwat process:pid state\n\
tcp6 0 0 2003:c8:a714:850.50213 2600:1901:0:9e23.443 ESTABLISHED 1 2 3 4 claude:3068 00102\n\
tcp46 0 0 *.18765 *.* LISTEN 0 0 131072 131072 Python:34982 00000\n\
tcp4 0 0 127.0.0.1.8080 *.* LISTEN 0 0 131072 131072 llama-server:77 00000\n";
    assert_eq!(super::parse_netstat_listeners(t, 18765), vec![Some(34982)]);
    assert_eq!(super::parse_netstat_listeners(t, 8080), vec![Some(77)]);
    assert!(super::parse_netstat_listeners(t, 443).is_empty());
}

use super::*;

/// The dead check's rule, both sides of both edges: a job that is printing or burning CPU is
/// never ended, a silent one is warned about once at four fifths of the window and ended only
/// when the whole window has gone. Pure, so a window can be a number rather than a wait.
#[test]
fn a_job_is_ended_only_when_neither_output_nor_cpu_has_moved_for_the_whole_window() {
    let w = Duration::from_secs(1800);
    assert_eq!(step(true, Duration::from_secs(1799), w, false), Step::Alive);
    assert_eq!(step(true, w, w, true), Step::Alive);
    // The warning, once: at four fifths exactly, and not before it.
    assert_eq!(
        step(false, Duration::from_secs(1439), w, false),
        Step::Alive
    );
    assert_eq!(step(false, Duration::from_secs(1440), w, false), Step::Warn);
    assert_eq!(step(false, Duration::from_secs(1700), w, true), Step::Alive);
    // And the end, at the window and not a millisecond before it.
    assert_eq!(
        step(false, w - Duration::from_millis(1), w, true),
        Step::Alive
    );
    assert_eq!(step(false, w, w, true), Step::Dead);
}

/// A tree of directories this test owns. The dead check reads `cpu.stat` off a scope's PATH, so
/// a directory with a number in it is a cgroup as far as the rule is concerned, and `end` only
/// records that it was asked. This is how the mechanism is measured on a box whose own cgroup
/// subtree is not delegated: real `Job`s, the real tick thread, real files.
struct FakeTree {
    root: PathBuf,
    ended: Mutex<Vec<String>>,
}

impl ScopeTree for FakeTree {
    fn describe(&self) -> String {
        "a test's directories".into()
    }

    fn open(
        &self,
        kind: ScopeKind,
        name: &str,
        _parent: Option<&ScopeId>,
    ) -> Result<ScopeId, ExecError> {
        Ok(ScopeId {
            kind,
            name: name.into(),
            path: self.root.join(name),
        })
    }

    fn members(&self, _scope: &ScopeId) -> Result<Vec<u32>, ExecError> {
        Ok(Vec::new())
    }

    fn end(&self, scope: &ScopeId) -> Reaping {
        self.ended
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(scope.name.clone());
        Reaping {
            scope: scope.clone(),
            at: SystemTime::now(),
            mechanism: "a test's directories",
            observed: Vec::new(),
            survivors: Vec::new(),
            waited: Duration::ZERO,
            removed: false,
            note: None,
        }
    }

    fn migrate(&self, _from: &ScopeId, _to: &ScopeId) -> Result<Migration, ExecError> {
        Err(ExecError::NoScopes("a test tree moves nothing".into()))
    }

    fn list(&self) -> Vec<ScopeId> {
        Vec::new()
    }
}

/// A directory named after a scope, with this many microseconds of CPU in it.
fn a_scope(tree: &Arc<FakeTree>, name: &str, usec: u64) -> ScopeId {
    let path = tree.root.join(name);
    std::fs::create_dir_all(&path).expect("a directory");
    std::fs::write(path.join("cpu.stat"), format!("usage_usec {usec}\n")).expect("a clock");
    ScopeId {
        kind: ScopeKind::Session,
        name: name.into(),
        path,
    }
}

/// A job on a scope, with no process behind it: the dead check reads the job's clock off the
/// scope and the job's output off its capture, and those are the only two things it looks at.
fn a_job(scope: &ScopeId) -> Arc<Job> {
    Arc::new(Job::new(
        JobId::next(),
        "sleep 300".into(),
        None,
        Lifetime {
            scope: scope.clone(),
            owner: scope.clone(),
            background: Some(Backgrounding::Asked),
        },
        ".".into(),
        0,
        DEFAULT_CAPTURE_BYTES,
        super::super::jobs::Stdin::none(),
    ))
}

/// A clock that never moves is a dead job: the check ends it, says so, and records why.
#[test]
fn a_job_whose_cpu_and_output_never_move_is_ended_as_dead() {
    let root = std::env::temp_dir().join(format!("letibot-dead-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let tree = Arc::new(FakeTree {
        root: root.clone(),
        ended: Mutex::new(Vec::new()),
    });
    let liveness = Liveness::new(Arc::clone(&tree) as Arc<dyn ScopeTree>);
    liveness.set_window(Some(Duration::from_millis(300)));
    let scope = a_scope(&tree, "j-dead", 100);
    let job = a_job(&scope);
    liveness.watch(Arc::clone(&job));
    let until = Instant::now() + Duration::from_secs(10);
    while job.state().is_running() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!job.state().is_running(), "the dead check never fired");
    match job.state() {
        JobState::Killed { by } => assert_eq!(by, DEAD_KILL),
        other => panic!("a dead job must say so: {}", other.word()),
    }
    let ended = liveness.ended();
    assert_eq!(ended.len(), 1, "one record, one job: {ended:?}");
    assert!(
        ended[0].silent_for >= Duration::from_millis(300),
        "ended after {:?}, before the window it was measured against",
        ended[0].silent_for
    );
    assert_eq!(
        tree.ended.lock().unwrap().as_slice(),
        ["j-dead".to_string()],
        "the job's scope was the one ended"
    );
    // And nothing is left watching it: a job that ended is not a job to keep looking at.
    liveness.scan(tree.as_ref(), Duration::from_millis(300));
    assert_eq!(liveness.ended().len(), 1);
    let _ = std::fs::remove_dir_all(&root);
}

/// The case the rule exists for: a job that is burning CPU or printing is WORKING, and neither
/// a window nor the end of one may end it. Two jobs, one signal each.
#[test]
fn a_job_that_burns_cpu_or_keeps_printing_is_never_ended() {
    let root = std::env::temp_dir().join(format!("letibot-alive-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let tree = Arc::new(FakeTree {
        root: root.clone(),
        ended: Mutex::new(Vec::new()),
    });
    let liveness = Liveness::new(Arc::clone(&tree) as Arc<dyn ScopeTree>);
    liveness.set_window(Some(Duration::from_millis(300)));
    // One job whose CPU climbs, and one that says something now and then.
    let busy = a_scope(&tree, "j-busy", 0);
    let burning = a_job(&busy);
    let talking = a_job(&a_scope(&tree, "j-talking", 100));
    liveness.watch(Arc::clone(&burning));
    liveness.watch(Arc::clone(&talking));
    let clock = busy.path.join("cpu.stat");
    let until = Instant::now() + Duration::from_millis(1200);
    let mut usec = 0u64;
    while Instant::now() < until {
        usec += 1_000;
        std::fs::write(&clock, format!("usage_usec {usec}\n")).unwrap();
        talking.capture.lock().expect("capture").push(b"tick\n");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        burning.state().is_running(),
        "a job burning CPU was ended as dead: {}",
        burning.state().word()
    );
    assert!(
        talking.state().is_running(),
        "a job that is printing was ended as dead: {}",
        talking.state().word()
    );
    assert!(liveness.ended().is_empty(), "nothing was ended");
    assert!(tree.ended.lock().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

/// The warning is owed once, at four fifths of the window, and it is TAKEN: a watcher that polls
/// says it one time.
#[test]
fn the_dead_checks_warning_comes_once_and_is_taken_once() {
    let root = std::env::temp_dir().join(format!("letibot-warn-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let tree = Arc::new(FakeTree {
        root: root.clone(),
        ended: Mutex::new(Vec::new()),
    });
    let liveness = Liveness::new(Arc::clone(&tree) as Arc<dyn ScopeTree>);
    let window = Duration::from_millis(1500);
    liveness.set_window(Some(window));
    let job = a_job(&a_scope(&tree, "j-warn", 100));
    liveness.watch(Arc::clone(&job));
    let until = Instant::now() + Duration::from_secs(10);
    while liveness.warnings().is_empty() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
    }
    let warnings = liveness.warnings();
    assert_eq!(warnings.len(), 1, "one warning per job: {warnings:?}");
    assert_eq!(warnings[0].job, job.id);
    assert_eq!(warnings[0].window, window);
    assert!(
        warnings[0].silent_for * 5 >= window * 4 && warnings[0].silent_for < window,
        "warned at {:?}, which is not between four fifths and the window",
        warnings[0].silent_for
    );
    assert!(liveness.take_warning(&job.id).is_some(), "it is said once");
    assert!(
        liveness.take_warning(&job.id).is_none(),
        "and never said twice for one job"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A job somebody has bounded is not the dead check's business, and one the operator turned the
/// check off for is nobody's: two ways for a silent-but-working job to be left alone.
#[test]
fn a_bounded_job_and_a_host_with_the_check_off_are_left_alone() {
    let root = std::env::temp_dir().join(format!("letibot-spared-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let tree = Arc::new(FakeTree {
        root: root.clone(),
        ended: Mutex::new(Vec::new()),
    });
    let liveness = Liveness::new(Arc::clone(&tree) as Arc<dyn ScopeTree>);
    liveness.set_window(Some(Duration::from_millis(300)));
    let bounded = a_job(&a_scope(&tree, "j-bounded", 100));
    liveness.watch(Arc::clone(&bounded));
    liveness.spare(&bounded.id);
    // And the check off altogether.
    liveness.set_window(None);
    let unwatched = a_job(&a_scope(&tree, "j-nobody", 100));
    liveness.watch(Arc::clone(&unwatched));
    std::thread::sleep(Duration::from_millis(1200));
    assert!(
        bounded.state().is_running(),
        "a bounded job was ended as dead"
    );
    assert!(unwatched.state().is_running(), "the check is off");
    assert!(liveness.ended().is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_parent_chain_is_read_past_a_comm_containing_spaces() {
    // The parse everybody gets wrong once: field 2 is `(name)` and the name may
    // hold spaces and parentheses.
    let me = std::process::id();
    let p = parent_of(me);
    assert!(p.is_some(), "this process has a parent");
    assert_ne!(p, Some(me));
}

/// The `letibot-deadline` threads alive in this process, by their kernel name (truncated
/// to 15 bytes, so `letibot-deadlin`).
fn deadline_threads() -> std::collections::BTreeSet<String> {
    std::fs::read_dir("/proc/self/task")
        .map(|d| {
            d.flatten()
                .filter(|t| {
                    std::fs::read_to_string(t.path().join("comm"))
                        .is_ok_and(|c| c.trim_end().starts_with("letibot-deadl"))
                })
                .map(|t| t.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// A dropped host takes its deadline watchdog with it. The watchdog was an endless loop
/// holding its own `Arc<Deadlines>`, so every host ever built left a thread behind for the life
/// of the process. The `Weak` frees exactly when that thread leaves `run`; the tid is checked too.
#[test]
fn a_dropped_host_takes_its_deadline_thread_with_it() {
    let before = deadline_threads();
    let h = HostProcesses::with_tree("/tmp", Box::new(super::super::scope::NoScopes::new("test")));
    let weak = Arc::downgrade(&h.deadlines);
    // Ours is among the threads that appeared; other tests may have added their own. A thread
    // names itself once it runs, so a loaded box can show it a moment after the spawn.
    let mut started: Vec<String> = Vec::new();
    let named_by = Instant::now() + Duration::from_secs(5);
    while started.is_empty() && Instant::now() < named_by {
        started = deadline_threads().difference(&before).cloned().collect();
        if started.is_empty() {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    assert!(
        !started.is_empty() || cfg!(not(target_os = "linux")),
        "a host starts a watchdog"
    );
    drop(h);
    let until = Instant::now() + Duration::from_secs(10);
    while weak.upgrade().is_some() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        weak.upgrade().is_none(),
        "**the watchdog outlived its host** — the thread still holds the deadlines, so it is \
             still running, and one is left behind per HostProcesses for the process's life"
    );
    // And by tid: the thread is gone from the process. (Threads other tests started in the
    // same window may still be alive, so this asserts that not every new one survived.)
    let until = Instant::now() + Duration::from_secs(10);
    let alive = |started: &[String]| {
        let now = deadline_threads();
        started.iter().filter(|t| now.contains(*t)).count()
    };
    while !started.is_empty() && alive(&started) == started.len() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        started.is_empty() || alive(&started) < started.len(),
        "no deadline thread ended after its host was dropped: {started:?}"
    );
}

/// The threads a kernel name prefixes, by tid. The dead check's name is 15 bytes in `comm`,
/// like the watchdog's, so the prefix is the whole of it that can be read back.
fn threads_named(prefix: &str) -> std::collections::BTreeSet<String> {
    std::fs::read_dir("/proc/self/task")
        .map(|d| {
            d.flatten()
                .filter(|t| {
                    std::fs::read_to_string(t.path().join("comm"))
                        .is_ok_and(|c| c.trim_end().starts_with(prefix))
                })
                .map(|t| t.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// A dropped host takes its dead check with it. It is a tick loop holding an `Arc` of its own, so
/// a leaked one would be a thread per `HostProcesses` for the life of the process, waking every
/// minute to look at jobs that no longer exist.
#[test]
fn a_dropped_host_takes_its_dead_check_with_it() {
    let before = threads_named("letibot-liven");
    let h = HostProcesses::with_tree("/tmp", Box::new(super::super::scope::NoScopes::new("test")));
    let weak = Arc::downgrade(&h.liveness);
    let mut started: Vec<String> = Vec::new();
    let named_by = Instant::now() + Duration::from_secs(5);
    while started.is_empty() && Instant::now() < named_by {
        started = threads_named("letibot-liven")
            .difference(&before)
            .cloned()
            .collect();
        if started.is_empty() {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    assert!(
        !started.is_empty() || cfg!(not(target_os = "linux")),
        "a host starts a dead check"
    );
    drop(h);
    let until = Instant::now() + Duration::from_secs(10);
    while weak.upgrade().is_some() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        weak.upgrade().is_none(),
        "**the dead check outlived its host** — the thread still holds it, so it keeps waking \
             for jobs nobody has any more"
    );
    let until = Instant::now() + Duration::from_secs(10);
    let alive = |started: &[String]| {
        let now = threads_named("letibot-liven");
        started.iter().filter(|t| now.contains(*t)).count()
    };
    while !started.is_empty() && alive(&started) == started.len() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        started.is_empty() || alive(&started) < started.len(),
        "no dead-check thread ended after its host was dropped: {started:?}"
    );
}

#[test]
fn the_harness_protects_itself_without_being_asked() {
    let h = HostProcesses::with_tree("/tmp", Box::new(super::super::scope::NoScopes::new("test")));
    let p = h.protected();
    assert!(
        p.iter().any(|e| e.pid == std::process::id()),
        "the process evaluating the command must be in the protected set"
    );
    // And the reason is a sentence a refusal can quote, not a category.
    let me = p.iter().find(|e| e.pid == std::process::id()).unwrap();
    assert!(me.why.contains("evaluating"), "{}", me.why);
    assert!(
        p.len() > 1,
        "the parent chain is protected too, not just self"
    );
}

#[test]
fn a_listener_resolves_to_the_pid_holding_it() {
    // Bind a real socket so the test depends on nothing running on this box. Guard the fact,
    // not the proxy: the assertion is that the pid found is OURS.
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = l.local_addr().unwrap().port();
    match listener_pid(port) {
        Some(pid) => assert_eq!(pid, std::process::id()),
        // /proc may be unreadable in some sandboxes; that is a different fact from "nothing is
        // listening", and the function says so by returning None rather than a wrong pid.
        None => eprintln!("listener_pid: /proc/<pid>/fd not readable here"),
    }
}

#[test]
fn a_host_with_no_scope_tree_refuses_to_spawn() {
    // Fail closed: a process with no owner is the leak, so there is no path from "no cgroups"
    // to "run it anyway".
    let h = HostProcesses::with_tree(
        "/tmp",
        Box::new(super::super::scope::NoScopes::new("no cgroup v2 here")),
    );
    let e = h
        .spawn(&SpawnRequest {
            command: "true".into(),
            // Nobody named this run: the substrate's own test, not an agent's work.
            slug: None,
            cwd: ".".into(),
            scope: ScopeKind::Turn,
            scope_name: None,
            background: false,
            env: vec![],
            // No terminal: these are the substrate's own tests, not an operator's run.
            tty: false,
            memory: None,
            env_trailer: false,
        })
        .unwrap_err();
    assert!(format!("{e}").contains("no cgroup v2 here"), "{e}");
    assert!(format!("{e}").contains("nothing would reap it"), "{e}");
}

/// A scope a settled job left a process in is looked at again. The waiter thread returns
/// after its one look, and nothing else in the tree revisits a job's scope - so without this a
/// session that started a background server per checkout keeps one cgroup directory each.
#[test]
fn a_scope_a_job_left_a_process_in_is_released_by_the_next_spawn() {
    let root = std::env::temp_dir().join(format!("letibot-kept-scope-{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("a root");
    let h = HostProcesses::with_tree(&root, PlainTree::over(root.join("tree")));
    let tree = Arc::clone(&h.tree);
    let scope = tree
        .open(ScopeKind::Session, "job-kept", None)
        .expect("a scope");
    // A command that left something behind: this test's own pid stands in for the child, and
    // the process has to be a live one, or the scope is empty and goes at once.
    std::fs::write(
        scope.path.join("cgroup.procs"),
        std::process::id().to_string(),
    )
    .expect("a member");
    settle_scope(tree.as_ref(), &h.kept_scopes, &scope);
    assert!(
        scope.path.exists(),
        "a scope still holding a process is somebody's live work and must not be ended"
    );
    assert_eq!(
        h.kept_scopes.lock().expect("kept").len(),
        1,
        "the scope that stayed is kept for the next look"
    );

    // The process goes, and the next spawn is what notices.
    std::fs::write(scope.path.join("cgroup.procs"), "").expect("an empty scope");
    h.spawn(&SpawnRequest {
        command: "true".into(),
        slug: None,
        cwd: "/".into(),
        scope: ScopeKind::Session,
        scope_name: None,
        background: true,
        env: vec![],
        tty: false,
        memory: None,
        env_trailer: false,
    })
    .expect("a job runs");
    assert!(
        !scope.path.exists(),
        "a kept scope nothing revisits is one cgroup directory per command a session ever ran"
    );
    assert!(
        h.kept_scopes.lock().expect("kept").is_empty(),
        "the kept list does not grow for ever"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A directory per scope and a `cgroup.procs` a test writes into: what a kernel would answer,
/// on a box with no cgroup v2 to delegate. The join script writes a real pid, so the spawn path
/// under test is the host's own rather than a fake of it.
struct PlainTree {
    root: PathBuf,
}

impl PlainTree {
    fn over(root: PathBuf) -> Box<dyn ScopeTree> {
        let _ = std::fs::create_dir_all(&root);
        Box::new(PlainTree { root })
    }
}

impl ScopeTree for PlainTree {
    fn describe(&self) -> String {
        format!("plain directories under {}", self.root.display())
    }

    fn root(&self) -> Option<PathBuf> {
        Some(self.root.clone())
    }

    fn open(
        &self,
        kind: ScopeKind,
        name: &str,
        parent: Option<&ScopeId>,
    ) -> Result<ScopeId, ExecError> {
        let base = parent
            .map(|p| p.path.clone())
            .unwrap_or_else(|| self.root.clone());
        let path = base.join(format!("{}.{name}", kind.as_str()));
        std::fs::create_dir_all(&path)
            .map_err(|e| ExecError::Spawn(format!("{}: {e}", path.display())))?;
        std::fs::write(path.join("cgroup.procs"), b"")
            .map_err(|e| ExecError::Spawn(format!("{}: {e}", path.display())))?;
        Ok(ScopeId {
            kind,
            name: name.to_string(),
            path,
        })
    }

    /// What the kernel would answer: the pids the join script wrote, less the ones that have
    /// exited - the kernel drops a pid from the file when its process goes.
    fn members(&self, scope: &ScopeId) -> Result<Vec<u32>, ExecError> {
        let body = std::fs::read_to_string(scope.path.join("cgroup.procs"))
            .map_err(|e| ExecError::NoSuchScope(format!("{}: {e}", scope.path.display())))?;
        Ok(body
            .lines()
            .filter_map(|l| l.trim().parse().ok())
            .filter(|pid| super::super::procs::read(*pid).is_some())
            .collect())
    }

    /// Nothing is signalled here, on purpose: a test stands its own pid in for the process a
    /// command left behind, and a fake that killed it would take the test with it.
    fn end(&self, scope: &ScopeId) -> Reaping {
        let observed = self
            .members(scope)
            .unwrap_or_default()
            .into_iter()
            .map(Reaped::observe)
            .collect();
        let removed = std::fs::remove_dir_all(&scope.path).is_ok();
        Reaping {
            scope: scope.clone(),
            at: SystemTime::now(),
            mechanism: "nothing to kill",
            observed,
            survivors: Vec::new(),
            waited: Duration::ZERO,
            removed,
            note: None,
        }
    }

    fn migrate(&self, _from: &ScopeId, _to: &ScopeId) -> Result<Migration, ExecError> {
        Err(ExecError::Spawn("this tree cannot move a process".into()))
    }

    fn list(&self) -> Vec<ScopeId> {
        Vec::new()
    }
}

/// The record of what a host did is bounded, and the oldest go first: a reaping, a promotion
/// and a deadline fired are read back by a caller, so they cannot keep the pids of every kill the
/// daemon ever made.
#[test]
fn the_records_of_what_a_host_did_are_bounded_and_the_oldest_go_first() {
    // The rule, on its own: past the limit the FRONT goes, because the newest record is the one a
    // caller asking *what just happened* wants.
    let mut v: Vec<u32> = (0..10).collect();
    bound_records(&mut v, 4);
    assert_eq!(v, vec![6, 7, 8, 9]);
    let mut short: Vec<u32> = vec![1, 2];
    bound_records(&mut short, 4);
    assert_eq!(short, vec![1, 2], "under the limit nothing is dropped");

    // And through the door a caller uses: a real host, one more promotion than it keeps.
    let Some(_) = letibot_tokencore::apparatus::present(
        "a process-lifetime tree (cgroup v2; process groups on macOS)",
        letibot_tools::host_tree().is_ok(),
    ) else {
        return;
    };
    let root = std::env::temp_dir().join(format!("letibot-records-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let h = HostProcesses::new(&root).expect("this box has a cgroup v2 tree");
    let scope = |name: &str| super::ScopeId {
        kind: ScopeKind::Session,
        name: name.into(),
        path: root.join(name),
    };
    for n in 0..RECORD_LIMIT + 3 {
        h.record_promotion(Promotion {
            job: JobId(format!("job-{n}")),
            command: "sleep 1".into(),
            slug: None,
            from: scope("before"),
            to: scope("after"),
            ran_for: Duration::ZERO,
            how: Backgrounding::Asked,
            migration: None,
            at: std::time::SystemTime::now(),
            note: None,
            output_file: None,
        });
    }
    let kept = h.promotions();
    assert_eq!(kept.len(), RECORD_LIMIT, "the record is held to its bound");
    assert_eq!(
        kept.first().map(|p| p.job.0.as_str()),
        Some("job-3"),
        "the three oldest went: {}",
        kept.first().map(|p| p.job.0.clone()).unwrap_or_default()
    );
    assert_eq!(kept.last().map(|p| p.job.0.as_str()), Some("job-514"));
    let _ = std::fs::remove_dir_all(&root);
}

/// Settled output is held to a budget, oldest released first, and a running job's never.
/// Without it a host kept every job's whole capture for its life, so a session's job table grew
/// with every `bash` it ever ran.
#[test]
fn settled_captures_are_held_to_a_budget_and_the_oldest_go_first() {
    let Some(_) = letibot_tokencore::apparatus::present(
        "a process-lifetime tree (cgroup v2; process groups on macOS)",
        letibot_tools::host_tree().is_ok(),
    ) else {
        return;
    };
    let root = std::env::temp_dir().join(format!("letibot-budget-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let h = HostProcesses::new(&root).expect("this box has a cgroup v2 tree");
    let spawn = |command: &str| {
        h.spawn(&SpawnRequest {
            command: command.into(),
            slug: None,
            cwd: "/".into(),
            scope: ScopeKind::Session,
            scope_name: None,
            background: true,
            env: vec![],
            tty: false,
            memory: None,
            env_trailer: false,
        })
        .expect("a cgroup v2 tree was measured above")
    };
    // Three settled jobs of 1000 bytes each, and one still running.
    let settled: Vec<JobId> = (0..3)
        .map(|_| {
            let id = spawn("head -c 1000 /dev/zero");
            let _ = h.wait_job(&id, Duration::from_secs(30));
            id
        })
        .collect();
    let running = spawn("head -c 1000 /dev/zero; sleep 30");
    let until = Instant::now() + Duration::from_secs(10);
    while h.output(&running, 0, 4096).map(|o| o.retained).unwrap_or(0) < 1000
        && Instant::now() < until
    {
        std::thread::sleep(Duration::from_millis(10));
    }

    h.release_settled_captures(2500);
    let held = |id: &JobId| h.output(id, 0, 4096).expect("the row stays").retained;
    assert_eq!(
        held(&settled[0]),
        0,
        "the oldest settled capture is released"
    );
    assert_eq!(held(&settled[1]), 1000);
    assert_eq!(held(&settled[2]), 1000);
    assert_eq!(
        held(&running),
        1000,
        "a running job's output is never released"
    );
    let gone = h.output(&settled[0], 0, 4096).unwrap();
    assert_eq!(
        (gone.produced, gone.dropped),
        (1000, 1000),
        "a released capture still says what was written, and that it was dropped"
    );
    let _ = h.kill_job_as(&running, "test");
    std::fs::remove_dir_all(&root).ok();
}

/// A row is about 1.1 KB and nothing ever removed one. What is kept is the running jobs and
/// the newest window of finished ones, and a finish younger than the grace whatever the window
/// says: a watcher polls every five seconds and would lose the news of a row that went.
#[test]
fn finished_jobs_are_kept_to_a_window_and_a_running_one_is_never_dropped() {
    let h = HostProcesses::with_tree(
        "/tmp",
        Box::new(crate::exec::scope::NoScopes::new(
            "this test starts no process: the rows are made by hand",
        )),
    );
    let add = |id: &str, settle: Option<JobState>| {
        let scope = ScopeId {
            kind: ScopeKind::Session,
            name: format!("s-{id}"),
            path: PathBuf::from(format!("/nonexistent/{id}")),
        };
        let job = Arc::new(Job::new(
            JobId(id.into()),
            "true".into(),
            None,
            Lifetime {
                scope: scope.clone(),
                owner: scope,
                background: None,
            },
            "/".into(),
            0,
            1024,
            crate::exec::jobs::Stdin::none(),
        ));
        if let Some(state) = settle {
            job.settle(state);
        }
        h.jobs.lock().expect("jobs").push(Arc::clone(&job));
        h.by_id
            .lock()
            .expect("jobs by id")
            .insert(job.id.clone(), job);
    };
    // Three hundred commands a session ran, and five still going.
    for i in 0..300 {
        add(&format!("done-{i}"), Some(JobState::Exited { code: 0 }));
    }
    for i in 0..5 {
        add(&format!("running-{i}"), None);
    }

    // A job that has only just finished is kept whatever the window says.
    assert_eq!(
        h.prune_finished(KEPT_FINISHED_JOBS, FINISHED_JOB_GRACE, SystemTime::now()),
        0,
        "a fresh finish is kept: a watcher has not read its settlement yet"
    );
    assert_eq!(h.jobs().len(), 305);

    // An hour later the window is the rule, and it is counted from the newest.
    let later = SystemTime::now() + Duration::from_secs(3600);
    let dropped = h.prune_finished(KEPT_FINISHED_JOBS, FINISHED_JOB_GRACE, later);
    assert_eq!(
        dropped,
        300 - KEPT_FINISHED_JOBS,
        "the finished rows past the window"
    );
    assert_eq!(
        h.jobs().len(),
        KEPT_FINISHED_JOBS + 5,
        "the window and the running five"
    );
    assert_eq!(
        h.by_id.lock().expect("jobs by id").len(),
        h.jobs().len(),
        "the two tables name the same jobs, or `find` answers about rows `jobs()` has not got"
    );
    for i in 0..5 {
        assert!(
            h.find(&JobId(format!("running-{i}"))).is_some(),
            "a running job is never dropped"
        );
    }
    for i in 299 - KEPT_FINISHED_JOBS + 1..300 {
        assert!(
            h.find(&JobId(format!("done-{i}"))).is_some(),
            "the newest finished rows are the window"
        );
    }
    assert!(
        h.find(&JobId("done-0".into())).is_none(),
        "the oldest finished row went"
    );
    // And a second pass at the same clock drops nothing: the bound is the state, not a count.
    assert_eq!(
        h.prune_finished(KEPT_FINISHED_JOBS, FINISHED_JOB_GRACE, later),
        0
    );
}

/// A run with no deadline has its file from the spawn, and `from` is 0 because nothing had
/// been printed yet, not because a constant said so. A run that still has a deadline gets none:
/// the 8 MiB window is what bounds a runaway `yes`. The ring here is sixteen bytes.
#[test]
fn a_background_job_has_a_file_from_its_spawn_and_a_foreground_one_does_not() {
    // The cgroup is apparatus, the same skip the tests around this one make.
    let Some(_) = letibot_tokencore::apparatus::present(
        "a process-lifetime tree (cgroup v2; process groups on macOS)",
        letibot_tools::host_tree().is_ok(),
    ) else {
        return;
    };
    let root = std::env::temp_dir().join(format!("letibot-spawn-spill-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    // A fixture scratch directory, never the box's own: the same rule the promotion tests
    // follow, and the reason `set_scratch_dir` takes one at all.
    let scratch = std::env::temp_dir().join(format!("letibot-spill-dir-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    let h = HostProcesses::new(&root)
        .expect("this box has a cgroup v2 tree")
        .with_capture_bytes(16);
    h.set_scratch_dir(&scratch);

    let spawn = |command: &str, background: bool| {
        h.spawn(&SpawnRequest {
            command: command.into(),
            slug: None,
            cwd: "/".into(),
            scope: ScopeKind::Session,
            scope_name: None,
            background,
            env: vec![],
            tty: false,
            memory: None,
            env_trailer: false,
        })
        .expect("a cgroup v2 tree was measured above")
    };

    // Four bytes, then twenty: twenty-four into a sixteen-byte ring, so the ring drops its
    // first eight and the file must not.
    let id = spawn("printf aaaa; printf bbbbbbbbbbbbbbbbbbbb", true);
    let _ = h.wait_job(&id, Duration::from_secs(30));

    let view = h.job(&id).expect("the row stays");
    let f = view
        .output_file
        .expect("a job with no deadline has a file from its spawn");
    assert!(
        f.path.ends_with(&format!("job-{id}.out")),
        "the file is named after the job: {}",
        f.path
    );
    assert!(
        f.path.starts_with(&scratch.display().to_string()),
        "the file belongs in the session's scratch directory: {}",
        f.path
    );
    assert_eq!(
        f.from, 0,
        "the file's first byte is the job's first byte — nothing had been dropped when it \
             was opened"
    );

    // Read from the disk, not through the harness: a test that asked the tool whether it
    // wrote is a test of the tool's opinion of itself.
    let on_disk = std::fs::read(&f.path).expect("the scratch file exists");
    assert_eq!(
        on_disk,
        b"aaaabbbbbbbbbbbbbbbbbbbb".to_vec(),
        "the file holds every byte from the job's first, and the ring did not bound it"
    );
    assert_eq!(
        f.bytes, 24,
        "the count is the file's own, and it is final once the drains have joined"
    );

    // The window still dropped the front, and says so: the two are different facts, and the
    // file is the answer to the first one.
    let slice = h.output(&id, 0, 1024).expect("the row stays");
    assert_eq!((slice.dropped, slice.retained), (8, 16));
    assert_eq!(
        slice.text(),
        "bbbbbbbbbbbbbbbb",
        "the window is the last sixteen bytes and nothing else"
    );

    // A run that still has a deadline keeps the window as its only copy: an unbounded file
    // behind every `bash` call would remove the bound the 8 MiB window exists to be.
    let foreground = spawn("true", false);
    let _ = h.wait_job(&foreground, Duration::from_secs(30));
    assert!(
        h.job(&foreground)
            .expect("the row stays")
            .output_file
            .is_none(),
        "a deadline-bounded run gets its file from the promotion, not from the spawn"
    );

    std::fs::remove_dir_all(&root).ok();
    std::fs::remove_dir_all(&scratch).ok();
}

/// A promotion of a run that already has a file keeps it, byte for byte. `spill_to`
/// truncates, so a second promotion that opened the file again would cut a run's output at the
/// moment somebody asked where the rest of it goes. This job is in the "already there" arm.
#[test]
fn a_promotion_does_not_truncate_the_file_a_background_job_already_has() {
    let Some(_) = letibot_tokencore::apparatus::present(
        "a process-lifetime tree (cgroup v2; process groups on macOS)",
        letibot_tools::host_tree().is_ok(),
    ) else {
        return;
    };
    let root = std::env::temp_dir().join(format!("letibot-repromote-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let scratch =
        std::env::temp_dir().join(format!("letibot-repromote-out-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    let h = HostProcesses::new(&root).expect("this box has a cgroup v2 tree");
    h.set_scratch_dir(&scratch);

    // It prints, then waits: the file has bytes in it and a descriptor still open, which is
    // the state a truncation would be invisible in until somebody read the file.
    let id = h
        .spawn(&SpawnRequest {
            command: "printf 'kept'; sleep 30".into(),
            slug: None,
            cwd: "/".into(),
            scope: ScopeKind::Session,
            scope_name: None,
            background: true,
            env: vec![],
            tty: false,
            memory: None,
            env_trailer: false,
        })
        .expect("a cgroup v2 tree was measured above");
    let until = Instant::now() + Duration::from_secs(10);
    while h.output(&id, 0, 64).map(|o| o.retained).unwrap_or(0) == 0 && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
    let before = h
        .job(&id)
        .and_then(|v| v.output_file)
        .expect("a run with no deadline has its file from the spawn");
    assert_eq!(before.bytes, 4, "the four bytes it has printed so far");

    let p = h
        .promote(
            &id,
            ScopeKind::Session,
            None,
            Backgrounding::Operator {
                identity: "test".into(),
            },
        )
        .expect("a promotion of a job this host owns");
    let after = p
        .output_file
        .expect("the promotion still answers with the file");
    assert_eq!(
        after.path, before.path,
        "the same file, not a second one beside it"
    );
    assert_eq!(
        after.bytes, 4,
        "the promotion did not truncate a file the run is writing"
    );
    assert_eq!(
        std::fs::read(&after.path).expect("the file is still there"),
        b"kept".to_vec()
    );

    let _ = h.kill_job_as(&id, "test");
    std::fs::remove_dir_all(&root).ok();
    std::fs::remove_dir_all(&scratch).ok();
}

/// The wrapper writes `$TMPDIR/letibot-joined-<id>` on joining, and that file is the evidence
/// `joined` is read from. A job killed before it ends never reached the arm that consumes it, so
/// the file stayed for the life of the machine: one per killed job.
#[test]
fn a_job_killed_before_it_ends_leaves_no_join_token_behind() {
    let root = std::env::temp_dir().join(format!("letibot-joined-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    // The plain tree, and a command that ends by itself a second later: the kill marks the
    // job `Killed` while the process still runs, which is the state the token leaked in.
    let h = HostProcesses::with_tree(&root, PlainTree::over(root.join("tree")));
    let id = h
        .spawn(&SpawnRequest {
            command: "sleep 1".into(),
            slug: None,
            cwd: "/".into(),
            scope: ScopeKind::Session,
            scope_name: None,
            background: true,
            env: vec![],
            tty: false,
            memory: None,
            env_trailer: false,
        })
        .expect("a run starts");
    let token = std::env::temp_dir().join(format!("letibot-joined-{}", id.0));
    let until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < until && !token.exists() {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        token.exists(),
        "the wrapper must have written its token, or this measures nothing: {}",
        token.display()
    );

    let _ = h.kill_job_as(&id, "the test asking");
    // The waiter joins the drains, finds the job already settled and returns on that arm.
    let until = Instant::now() + Duration::from_secs(10);
    while Instant::now() < until && token.exists() {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !token.exists(),
        "a killed job left its evidence in $TMPDIR: {}",
        token.display()
    );
    std::fs::remove_dir_all(&root).ok();
}

/// The file a backgrounded run is spilled to is read by the model with the file tools, so the
/// token must already be gone from it. Read from the disk, not through `output`.
#[test]
fn a_spill_file_holds_no_secret() {
    let token = "gho_fixtureFIXTUREfixture0000000000000000";
    let root = std::env::temp_dir().join(format!("letibot-mask-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let scratch = std::env::temp_dir().join(format!("letibot-mask-out-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    // The plain tree: this measures what reaches the file, not the kernel, and a fixture that
    // skipped on a box without a delegated cgroup would be a green that measured nothing.
    let h = HostProcesses::with_tree(&root, PlainTree::over(root.join("tree")));
    h.set_scratch_dir(&scratch);
    h.set_secret_env(crate::exec::seat::SecretEnv(vec![(
        "GH_TOKEN".into(),
        token.into(),
    )]));

    let id = h
        .spawn(&SpawnRequest {
            command: "printf 't=%s' \"$GH_TOKEN\"".into(),
            slug: None,
            cwd: "/".into(),
            scope: ScopeKind::Session,
            scope_name: None,
            background: true,
            env: vec![],
            tty: false,
            memory: None,
            env_trailer: false,
        })
        .expect("a run starts");
    let _ = h.wait_job(&id, Duration::from_secs(30));

    let f = h
        .job(&id)
        .and_then(|v| v.output_file)
        .expect("a run with no deadline has its file from its spawn");
    let on_disk = std::fs::read_to_string(&f.path).expect("the file is there");
    assert!(
        !on_disk.contains(token),
        "the spill file the model is pointed at holds the token: {on_disk}"
    );
    assert!(
        on_disk.starts_with("t=***"),
        "and the mask is where the token was: {on_disk}"
    );
    let window = h.output(&id, 0, 4096).expect("the row stays").text();
    assert!(!window.contains(token), "the window holds it too: {window}");

    std::fs::remove_dir_all(&root).ok();
    std::fs::remove_dir_all(&scratch).ok();
}

/// A job of jobs reads a journal other machines wrote, and that journal never passes this
/// host's capture; the branch a reader takes must still be masked.
#[test]
fn a_job_of_jobs_output_is_masked() {
    let token = "gho_fixtureFIXTUREfixture0000000000000000";
    let h = HostProcesses::with_tree("/tmp", Box::new(super::super::scope::NoScopes::new("test")));
    h.set_secret_env(crate::exec::seat::SecretEnv(vec![(
        "GH_TOKEN".into(),
        token.into(),
    )]));
    let id = JobId("fanout-1".into());
    h.hold(
        &id,
        Arc::new(OneLine(format!("branch 1: gh auth says {token}\n"))),
    )
    .expect("an empty host holds the handle");

    let slice = h.output(&id, 0, 4096).expect("the handle is a job");
    let text = slice.text();
    assert!(
        !text.contains(token),
        "the journal carried the token out: {text}"
    );
    assert!(
        text.starts_with("branch 1: gh auth says ***"),
        "masked where it stood: {text}"
    );
    assert_eq!(
        slice.produced as usize,
        "branch 1: gh auth says ".len() + token.len() + 1,
        "masking keeps the length, so an offset still points at the same byte"
    );
}

/// A job of jobs that answers one fixed line: the branch of `output` above is what is under
/// test, so the aggregate itself must not be able to say anything else.
struct OneLine(String);

impl Aggregate for OneLine {
    fn command(&self) -> String {
        "fanout".into()
    }
    fn slug(&self) -> Option<String> {
        None
    }
    fn state(&self) -> JobState {
        JobState::Exited { code: 0 }
    }
    fn elapsed(&self) -> Duration {
        Duration::ZERO
    }
    fn ran_for(&self) -> Option<Duration> {
        Some(Duration::ZERO)
    }
    fn produced(&self) -> u64 {
        self.0.len() as u64
    }
    fn since_last_output(&self) -> Option<Duration> {
        None
    }
    fn output(&self, from: u64, limit: usize) -> OutputSlice {
        let bytes: Vec<u8> = self.0.bytes().skip(from as usize).take(limit).collect();
        OutputSlice {
            from,
            to: from + bytes.len() as u64,
            bytes,
            produced: self.0.len() as u64,
            dropped: 0,
            retained: self.0.len() as u64,
        }
    }
    fn wait(&self, _within: Duration) -> Option<JobState> {
        Some(JobState::Exited { code: 0 })
    }
    fn kill(&self, _by: &str) -> Vec<Reaped> {
        Vec::new()
    }
    fn report(&self) -> String {
        self.0.clone()
    }
}

/// A name is unique within the session, and it never moves: two jobs the caller named
/// `tests` are `tests` and `tests-2`. And a request that named nothing gets nothing: no
/// derivation, no fallback word.
#[test]
fn a_slug_is_made_unique_within_the_session_and_never_invented() {
    // The cgroup is apparatus: a container with no delegatable cgroup v2 subtree is a fact
    // about the machine and not a failure of this claim.
    let Some(_) = letibot_tokencore::apparatus::present(
        "a process-lifetime tree (cgroup v2; process groups on macOS)",
        letibot_tools::host_tree().is_ok(),
    ) else {
        return;
    };
    let root = std::env::temp_dir().join(format!("letibot-slug-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let h = HostProcesses::new(&root).expect("this box has a cgroup v2 tree");
    let spawn = |slug: Option<&str>| {
        h.spawn(&SpawnRequest {
            command: "true".into(),
            slug: slug.map(|s| s.to_string()),
            cwd: "/".into(),
            scope: ScopeKind::Session,
            scope_name: None,
            background: true,
            env: vec![],
            tty: false,
            memory: None,
            env_trailer: false,
        })
        .expect("a cgroup v2 tree was measured above")
    };
    let slug_of = |id: &JobId| h.job(id).expect("a job this host just made").slug;

    let first = spawn(Some("tests"));
    // Settled before the next one is named: a name that came free when its job ended
    // would point at two jobs the moment a reader looked the first one up again.
    let _ = h.wait_job(&first, Duration::from_secs(30));
    let second = spawn(Some("tests"));
    let third = spawn(Some("tests"));
    let unnamed = spawn(None);

    assert_eq!(slug_of(&first).as_deref(), Some("tests"));
    assert_eq!(slug_of(&second).as_deref(), Some("tests-2"));
    assert_eq!(slug_of(&third).as_deref(), Some("tests-3"));
    assert_ne!(first, second, "two jobs, two handles");
    assert_eq!(
        slug_of(&unnamed),
        None,
        "nobody named this run, so it has no name — not `tests-4` and not a guess"
    );
    // Stable for the job's life: nothing above moved a name that was already taken.
    assert_eq!(slug_of(&first).as_deref(), Some("tests"));
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn a_bash_env_planted_in_the_parent_never_reaches_the_child() {
    // The spawn clears the environment before setting its own pairs, so nothing planted in
    // this process, `BASH_ENV` included, can reach the child, and `PATH` is the pinned one.
    // The cgroup is apparatus: a container without a delegatable subtree has none to give.
    let Some(_) = letibot_tokencore::apparatus::present(
        "a process-lifetime tree (cgroup v2; process groups on macOS)",
        letibot_tools::host_tree().is_ok(),
    ) else {
        return;
    };
    let root = std::env::temp_dir().join(format!("letibot-r10-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    // Edition 2024: touching the process environment is unsafe, and this is the one test that
    // needs to. `env_clear()` in the spawn path undoes it for every child this host starts.
    unsafe {
        std::env::set_var("BASH_ENV", "/tmp/letibot-absent.rc");
        std::env::set_var("SHELLOPTS", "xtrace");
    }
    let h = HostProcesses::new(&root).expect("this box has a cgroup v2 tree");
    let id = h
        .spawn(&SpawnRequest {
            // Both printf and the `${var-}` expansion are builtins, so the assertion does not
            // depend on anything outside the pinned PATH.
            command: "printf 'be=%s\\n' \"${BASH_ENV-}\"; printf 'path=%s\\n' \"$PATH\"".into(),
            // Nobody named this run: the substrate's own test.
            slug: None,
            cwd: "/".into(),
            scope: ScopeKind::Turn,
            scope_name: None,
            background: false,
            // A session's env configuration must not re-arm what the clear removed: the four
            // names are filtered even when asked for by name.
            env: vec![("BASH_ENV".into(), "/tmp/letibot-absent.rc".into())],
            // No terminal: the substrate's own test, not an operator's run.
            tty: false,
            memory: None,
            env_trailer: false,
        })
        .unwrap();
    let waited = h.wait_job(&id, std::time::Duration::from_secs(30)).unwrap();
    let Waited::Happened { state, .. } = waited else {
        panic!("the job did not finish inside 30s: {waited:?}");
    };
    assert_eq!(
        state,
        Some(JobState::Exited { code: 0 }),
        "the child exited nonzero"
    );
    let out = h.output(&id, 0, 4096).unwrap();
    let text = String::from_utf8(out.bytes).unwrap();
    // Restored before anything can fail, because the variables are the whole process's:
    // every other test that starts a shell without clearing its environment inherited
    // `SHELLOPTS=xtrace` from here, where `/bin/sh` is bash and honours it.
    unsafe {
        std::env::remove_var("BASH_ENV");
        std::env::remove_var("SHELLOPTS");
    }
    let pinned = std::env::var("PATH").unwrap_or_default();
    assert_eq!(
        text,
        format!("be=\npath={pinned}\n"),
        "the child runs on the pinned PATH"
    );
    std::fs::remove_dir_all(&root).ok();
}

/// The operator's own run can be ANSWERED: the command reads its stdin, the handle comes out
/// of the job the spawn returned, and a line goes in. The assertion is the exact capture, so
/// the typed `Y` must not be echoed, and with `tty: false` the send must be refused instead.
#[test]
fn the_operators_own_run_has_a_stdin_a_line_can_be_sent_to() {
    // The cgroup is apparatus: a container without a delegatable subtree has none to give,
    // which is a fact about the machine and not a failure of this claim.
    let Some(_) = letibot_tokencore::apparatus::present(
        "a process-lifetime tree (cgroup v2; process groups on macOS)",
        letibot_tools::host_tree().is_ok(),
    ) else {
        return;
    };
    let Some(_) = letibot_tokencore::apparatus::present(
        "/bin/bash, the shell the operator's own run is handed to",
        std::path::Path::new("/bin/bash").exists(),
    ) else {
        return;
    };
    let root = std::env::temp_dir().join(format!("letibot-stdin-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let h = HostProcesses::new(&root)
        .expect("this box has a cgroup v2 tree")
        .with_console_shell(vec![
            "/bin/bash".to_string(),
            "--norc".to_string(),
            "--noprofile".to_string(),
            "-ic".to_string(),
        ]);
    // `read` is a builtin in every shell this host can be configured with, so the assertion does
    // not depend on anything outside the pinned PATH, the same rule the test above keeps.
    let command = "read answer; printf 'answered=%s\\n' \"$answer\"";
    let spawn = |tty: bool| {
        h.spawn(&SpawnRequest {
            command: command.into(),
            // Nobody named this run: the substrate's own test.
            slug: None,
            cwd: "/".into(),
            scope: ScopeKind::Turn,
            scope_name: None,
            background: false,
            env: vec![],
            tty,
            memory: None,
            env_trailer: false,
        })
        .unwrap()
    };

    // ---- The operator's own run: its own terminal, and the line reaches the command.
    let id = spawn(true);
    // And the host can NAME the run's processes, the other half of the detection: `ask`
    // reads each one's `/proc/<pid>/fd/0` and its `wchan`, so a host that cannot list them would
    // make every operator run look like one that is not asking (`ask::Waiting::Unreadable`).
    let pids = h.job_pids(&id);
    assert!(
        !pids.is_empty(),
        "a spawned job's own cgroup must name its processes"
    );
    let handle = h.job_handle(&id).expect("the job the spawn returned");
    let stdin = handle.stdin();
    assert!(
        stdin.is_open(),
        "the operator's own run must have a stdin the daemon holds"
    );
    stdin.send_line("Y").expect("the line reaches the command");
    let waited = h.wait_job(&id, std::time::Duration::from_secs(30)).unwrap();
    let Waited::Happened { state, .. } = waited else {
        panic!("the answered command did not finish inside 30s: {waited:?}");
    };
    assert_eq!(
        state,
        Some(JobState::Exited { code: 0 }),
        "the command read the line and exited"
    );
    let text = h.output(&id, 0, 4096).unwrap().text();
    assert_eq!(
        text, "answered=Y\n",
        "the answer must reach the command's own stdin and must NOT be echoed back into \
             the run's output: {text:?}"
    );

    // ---- The control: a model's run is unchanged, and the send says so.
    let id = spawn(false);
    let handle = h.job_handle(&id).expect("the job the spawn returned");
    let stdin = handle.stdin();
    assert!(
        !stdin.is_open(),
        "a run that is not the operator's must keep /dev/null on stdin"
    );
    let refused = stdin.send_line("Y").unwrap_err();
    assert!(
        refused.contains("/dev/null"),
        "the refusal must name what is missing: {refused}"
    );
    // And it got EOF rather than waiting, which is the older rule intact: the command
    // ran `read` against `/dev/null` and exited at once.
    let waited = h.wait_job(&id, std::time::Duration::from_secs(30)).unwrap();
    assert!(
        matches!(waited, Waited::Happened { .. }),
        "a model's run must not wait for a person who is not there: {waited:?}"
    );
    std::fs::remove_dir_all(&root).ok();
}

/// A tree that opens a scope as a plain directory and kills nothing: the by-id map is what
/// this test is about, and a real cgroup is apparatus it does not need.
struct DirScopes(std::path::PathBuf);

impl super::super::scope::ScopeTree for DirScopes {
    fn describe(&self) -> String {
        "a directory per scope".into()
    }
    fn open(
        &self,
        kind: ScopeKind,
        name: &str,
        _parent: Option<&ScopeId>,
    ) -> Result<ScopeId, ExecError> {
        let path = self.0.join(format!("{}.{}", kind.as_str(), name));
        std::fs::create_dir_all(&path)
            .map_err(|e| ExecError::Spawn(format!("{}: {e}", path.display())))?;
        Ok(ScopeId {
            kind,
            name: name.to_string(),
            path,
        })
    }
    fn members(&self, _scope: &ScopeId) -> Result<Vec<u32>, ExecError> {
        Ok(Vec::new())
    }
    fn end(&self, scope: &ScopeId) -> Reaping {
        Reaping {
            scope: scope.clone(),
            at: SystemTime::now(),
            mechanism: "nothing (a directory per scope)",
            observed: Vec::new(),
            survivors: Vec::new(),
            waited: Duration::ZERO,
            removed: false,
            note: None,
        }
    }
    fn migrate(&self, from: &ScopeId, _to: &ScopeId) -> Result<Migration, ExecError> {
        Err(ExecError::Spawn(format!(
            "a directory per scope cannot migrate out of {from}"
        )))
    }
    fn list(&self) -> Vec<ScopeId> {
        Vec::new()
    }
}

/// A job is found by its id, at every door it comes in by. The host adopted a record
/// another daemon left, and its row has to be where a read looks: a handle the table does not
/// hold is one every wait and kill fails to find.
#[test]
fn an_adopted_job_is_found_by_its_id() {
    let dir = std::env::temp_dir().join(format!("letibot-by-id-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the scope root");
    let h = HostProcesses::with_tree("/tmp", Box::new(DirScopes(dir.clone())));
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .stdout(Stdio::null())
        .spawn()
        .expect("a child to adopt");
    let pid = child.id();
    let start = super::super::procs::read(pid)
        .expect("the child is in /proc")
        .start;
    let adopted = h
        .adopt(&Adopted {
            handle: "h-adopted".into(),
            command: "sleep 30".into(),
            slug: Some("adopted".into()),
            cwd: "/tmp".into(),
            pid,
            identity: format!("start:{start}"),
            scope: "session.test".into(),
            // What a daemon's record can say: the cgroup died with that process, so the
            // directory is re-derived from the scope's name, and the job is found by id.
            scope_dir: None,
            owner_dir: None,
        })
        .expect("a recorded job is adopted");
    let row = h
        .job(&adopted)
        .expect("the job this host adopted is found by its id");
    assert_eq!(row.command, "sleep 30");
    assert_eq!(row.pid, pid);
    assert!(
        h.job_handle(&adopted).is_some(),
        "and the job itself, not only a row built from it"
    );
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A root for one test, and a host over it: a directory per scope, because a real cgroup is
/// apparatus these three facts do not need.
fn host_and_root(tag: &str) -> (HostProcesses, PathBuf) {
    let dir = std::env::temp_dir().join(format!("letibot-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the scope root");
    (
        HostProcesses::with_tree("/tmp", Box::new(DirScopes(dir.clone()))),
        dir,
    )
}

/// A live process to hand over, and the identity a record would carry for it.
fn a_live_child() -> (std::process::Child, u32, String) {
    let child = std::process::Command::new("sleep")
        .arg("30")
        .stdout(Stdio::null())
        .spawn()
        .expect("a child to adopt");
    let pid = child.id();
    let start = super::super::procs::read(pid)
        .expect("the child is in /proc")
        .start;
    (child, pid, format!("start:{start}"))
}

/// A hand-over must not take back a job nothing can identify. `survivors` invents an empty
/// identity where `/proc` could not be read, and `""` never matches `start:<n>` - so the
/// heir's watcher settled a LIVE process `Dead` on its first look, 500 ms later.
#[test]
fn a_job_with_no_readable_identity_is_refused_rather_than_settled_dead() {
    let (h, dir) = host_and_root("by-no-identity");
    let (mut child, pid, _) = a_live_child();
    let refused = h.adopt(&Adopted {
        handle: "h-blind".into(),
        command: "sleep 30".into(),
        slug: Some("blind".into()),
        cwd: "/tmp".into(),
        pid,
        identity: String::new(),
        scope: "explicit.blind".into(),
        scope_dir: None,
        owner_dir: None,
    });
    assert!(
        refused.is_err(),
        "a job nothing can identify was taken back anyway, so its watcher will settle a live \
             process Dead: {refused:?}"
    );
    // And nothing is left behind wearing its handle, for a reader to believe.
    std::thread::sleep(Duration::from_millis(700));
    assert!(
        h.job(&JobId("h-blind".into())).is_none(),
        "a refused adoption still put a row in the host's table"
    );
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);
}

/// An adopted job is a job of this host like any other, so it gets the same output ring.
/// `adopt` built it with a zero-length one, and every byte that arrived was dropped.
#[test]
fn an_adopted_job_keeps_the_output_it_is_given() {
    let (h, dir) = host_and_root("adopted-ring");
    let (mut child, pid, identity) = a_live_child();
    let id = h
        .adopt(&Adopted {
            handle: "h-ring".into(),
            command: "make".into(),
            slug: None,
            cwd: "/tmp".into(),
            pid,
            identity,
            scope: "session.test".into(),
            scope_dir: None,
            owner_dir: None,
        })
        .expect("a recorded job is adopted");
    let job = h.job_handle(&id).expect("the job this host adopted");
    job.capture
        .lock()
        .expect("capture")
        .push(b"the build says what it did\n");
    let window = h.output(&id, 0, 100).expect("the window onto it");
    assert_eq!(
        window.text(),
        "the build says what it did\n",
        "an adopted job dropped its own output: {window:?}"
    );
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);
}

/// One host is not two: `adopt` answers the id it already holds, and `hand_over` then forgets
/// the only record there is, leaving a running process in no table at all.
#[test]
fn a_hand_over_to_the_same_host_leaves_the_job_where_it_is() {
    let (h, dir) = host_and_root("hand-over-to-itself");
    let (mut child, pid, identity) = a_live_child();
    let id = h
        .adopt(&Adopted {
            handle: "h-explicit".into(),
            command: "sleep 30".into(),
            slug: Some("cache-warm".into()),
            cwd: "/tmp".into(),
            pid,
            identity,
            scope: "explicit.cache-warm".into(),
            scope_dir: Some(dir.join("job-h-explicit").display().to_string()),
            owner_dir: Some(dir.join("explicit.cache-warm").display().to_string()),
        })
        .expect("a recorded job is adopted");
    assert_eq!(
        h.survivors().len(),
        1,
        "an explicit-scope job is a survivor"
    );
    let moved = hand_over(&h, &h);
    assert_eq!(moved.len(), 1, "{moved:?}");
    assert!(
        moved[0].refused.is_some(),
        "a hand-over with one host as both ends is not a hand-over: {moved:?}"
    );
    assert!(
        h.job(&id).is_some(),
        "the only record of a running process was forgotten, and the process runs on"
    );
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);
}

/// One `bash` call's export reaches the next call's environment: the appended trailer reports it,
/// the capture gives the trailer up, `SessionEnv::observe` holds it, and those pairs are what the
/// next spawn is given. The plain tree, because this chain is not about the boundary.
#[test]
fn an_export_in_one_call_reaches_the_next_spawn() {
    let Some(_) = letibot_tokencore::apparatus::present(
        "a bash with `compgen -e` (the child the one-shot trailer runs)",
        std::path::Path::new("/bin/bash").exists(),
    ) else {
        return;
    };
    let root = std::env::temp_dir().join(format!("letibot-env-chain-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a root");
    let h = HostProcesses::with_tree(&root, PlainTree::over(root.join("tree")));
    let spawn = |command: &str, env: Vec<(String, String)>, trailer: bool| {
        h.spawn(&SpawnRequest {
            command: command.into(),
            slug: None,
            cwd: "/".into(),
            scope: ScopeKind::Session,
            scope_name: None,
            background: true,
            env,
            tty: false,
            memory: None,
            env_trailer: trailer,
        })
        .expect("a run starts")
    };

    // The first call exports, and its trailer reports what it left behind.
    let first = spawn("echo before\nexport LT_CHAIN=/w/chain", Vec::new(), true);
    let _ = h.wait_job(&first, Duration::from_secs(30));
    let view = h.job(&first).expect("the row stays");
    let Some(Ok(report)) = view.env_report.clone() else {
        panic!(
            "no believable report from the first call: {:?}",
            view.env_report
        );
    };
    assert!(
        report.contains("LT_CHAIN=/w/chain;"),
        "the export: {report}"
    );
    let window = h.output(&first, 0, usize::MAX).expect("the capture").text();
    assert_eq!(
        window, "before\n",
        "the trailer stayed in the window: {window:?}"
    );
    assert_eq!(
        view.produced, 7,
        "the trailer is not output the command made"
    );

    // The gate's own two steps: observe, then merge what the shell said into the next spawn.
    let mut session =
        crate::intent::env::SessionEnv::seeded([("PATH".to_string(), "/usr/bin:/bin".to_string())]);
    session
        .observe(&report)
        .expect("a report this build believes");
    let second = spawn(
        "test -n \"$LT_CHAIN\" && echo chained",
        session.shell_pairs(),
        false,
    );
    let _ = h.wait_job(&second, Duration::from_secs(30));
    let window = h
        .output(&second, 0, usize::MAX)
        .expect("the capture")
        .text();
    assert_eq!(
        window, "chained\n",
        "the export did not reach the next call: {window:?}"
    );
    assert!(
        h.job(&second).expect("the row").env_report.is_none(),
        "a call nobody armed carries no trailer"
    );

    // And the record keeps the model's own text: the append is argv, not the row.
    assert_eq!(
        h.job(&first).expect("the row").command,
        "echo before\nexport LT_CHAIN=/w/chain",
        "the job's row must carry the command as it was adjudicated"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A command that is not a whole program gets no trailer: text appended after an
/// unterminated construct would become part of the command, so the honest answer is no
/// report rather than a different command. The run is exactly the one the model wrote.
#[test]
fn a_command_that_is_not_a_whole_program_gets_no_trailer_and_runs_as_written() {
    let root = std::env::temp_dir().join(format!("letibot-env-partial-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a root");
    let h = HostProcesses::with_tree(&root, PlainTree::over(root.join("tree")));
    let id = h
        .spawn(&SpawnRequest {
            // A dangling pipeline: a syntax error in any shell, and the shape the trailer
            // must not be appended to.
            command: "echo hi |".into(),
            slug: None,
            cwd: "/".into(),
            scope: ScopeKind::Session,
            scope_name: None,
            background: true,
            env: vec![],
            tty: false,
            memory: None,
            env_trailer: true,
        })
        .expect("a run starts");
    let waited = h
        .wait_job(&id, Duration::from_secs(30))
        .expect("it settles");
    assert!(
        matches!(waited, Waited::Happened { state: Some(_), .. }),
        "a syntax error still settles: {waited:?}"
    );
    let view = h.job(&id).expect("the row stays");
    assert_eq!(
        view.command, "echo hi |",
        "the row carries the command as it was written"
    );
    assert!(
        view.env_report.is_none(),
        "a partial command must not have its text extended by machinery"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A tree whose kill leaves a process behind: the daemon cannot make a stubborn one, so the
/// tree lies about membership the way a failed kill would find it.
struct Stubborn {
    inner: DirScopes,
    pid: u32,
}

impl super::super::scope::ScopeTree for Stubborn {
    fn describe(&self) -> String {
        "a tree that cannot kill".into()
    }
    fn open(
        &self,
        kind: ScopeKind,
        name: &str,
        parent: Option<&ScopeId>,
    ) -> Result<ScopeId, ExecError> {
        self.inner.open(kind, name, parent)
    }
    fn members(&self, _scope: &ScopeId) -> Result<Vec<u32>, ExecError> {
        Ok(vec![self.pid])
    }
    fn end(&self, scope: &ScopeId) -> Reaping {
        let mut r = self.inner.end(scope);
        r.survivors = vec![self.pid];
        r
    }
    fn migrate(&self, from: &ScopeId, to: &ScopeId) -> Result<Migration, ExecError> {
        self.inner.migrate(from, to)
    }
    fn list(&self) -> Vec<ScopeId> {
        Vec::new()
    }
}

fn adopt_sleep(h: &HostProcesses, handle: &str) -> (std::process::Child, JobId) {
    let child = std::process::Command::new("sleep")
        .arg("30")
        .stdout(Stdio::null())
        .spawn()
        .expect("a child to adopt");
    let start = super::super::procs::read(child.id())
        .expect("the child is visible")
        .start;
    let id = h
        .adopt(&Adopted {
            handle: handle.into(),
            command: "sleep 30".into(),
            slug: None,
            cwd: "/tmp".into(),
            pid: child.id(),
            identity: format!("start:{start}"),
            scope: "session.test".into(),
            scope_dir: None,
            owner_dir: None,
        })
        .expect("adopted");
    (child, id)
}

/// A kill that left a process must not be reported as a kill, by `job_kill` or by a scope's end.
#[test]
fn a_kill_that_left_a_process_says_so_and_the_job_stays_running() {
    let dir = std::env::temp_dir().join(format!("letibot-stubborn-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("root");
    let mut probe = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("probe");
    let h = HostProcesses::with_tree(
        "/tmp",
        Box::new(Stubborn {
            inner: DirScopes(dir.clone()),
            pid: probe.id(),
        }),
    );
    let (mut child, id) = adopt_sleep(&h, "h-stubborn");
    let r = h.kill_job(&id).expect("a kill is recorded");
    let note = r.note.expect("the survivor is named");
    assert!(
        note.contains("NOT killed") && note.contains(&probe.id().to_string()),
        "{note}"
    );
    assert!(h.job(&id).unwrap().state.is_running(), "reported killed");
    let own_scope = h.job(&id).unwrap().scope;
    let r = h.end_scope(&own_scope).expect("a scope end is recorded");
    assert!(r.note.unwrap_or_default().contains("NOT killed"));
    assert!(
        h.job(&id).unwrap().state.is_running(),
        "a scope ending was announced over a live process"
    );
    let _ = child.kill();
    let _ = child.wait();
    let _ = probe.kill();
    let _ = probe.wait();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The daemon restarted: a new tree root knows nothing of the old scope's group files, and
/// the job is re-found from its pid and start time.
#[cfg(target_os = "macos")]
#[test]
fn a_job_kill_reaches_a_process_a_restarted_daemon_never_saw() {
    use std::os::unix::process::CommandExt;
    let parent = std::env::temp_dir().join(format!("letibot-restart-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&parent);
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .process_group(0)
        .stdout(Stdio::null())
        .spawn()
        .expect("a job to outlive its daemon");
    let h = HostProcesses::with_tree(
        "/tmp",
        Box::new(super::super::scope::ProcessGroups::probe_under(&parent).unwrap()),
    );
    let start = super::super::procs::read(child.id()).unwrap().start;
    let id = h
        .adopt(&Adopted {
            handle: "h-restarted".into(),
            command: "sleep 30".into(),
            slug: None,
            cwd: "/tmp".into(),
            pid: child.id(),
            identity: format!("start:{start}"),
            scope: "explicit.site-server".into(),
            scope_dir: None,
            owner_dir: None,
        })
        .expect("adopted");
    let r = h.kill_job(&id).expect("killed");
    assert_eq!(r.observed.len(), 1, "observed 0: {}", r.summary());
    assert!(r.survivors.is_empty(), "{}", r.summary());
    let _ = child.wait();
    assert!(!super::super::darwin::alive(child.id()));
    let _ = std::fs::remove_dir_all(&parent);
}

#[test]
fn only_a_model_servers_own_command_is_one() {
    assert!(model_server_command("llama-server -m x.gguf --port 8080"));
    assert!(!model_server_command(
        "python3 -m http.server 8080 --bind 127.0.0.1"
    ));
}

fn listener(tag: &str) -> (std::process::Child, u16) {
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("a free port");
        l.local_addr().unwrap().port()
    };
    let child = std::process::Command::new("python3")
        .args([
            "-c",
            "import socket,sys,time\ns=socket.socket();s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1)\ns.bind(('127.0.0.1',int(sys.argv[1])));s.listen(1);time.sleep(60)",
        ])
        .arg(port.to_string())
        .arg(tag)
        .stdout(Stdio::null())
        .spawn()
        .expect("python3");
    for _ in 0..100 {
        if listener_pid(port).is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    (child, port)
}

/// A port is not an identity: a session's own server on the model's port is not refused, a
/// real model server is, and a job is never one even when its command says so.
#[test]
fn the_model_server_is_found_by_what_it_is_and_not_by_its_port() {
    let (h, dir) = host_and_root("model-server");
    let (mut http, port) = listener("http.server");
    if listener_pid(port).is_none() {
        eprintln!("listener not resolvable here");
        let _ = http.kill();
        return;
    }
    assert_eq!(h.protect_model_server(port, "model", true), None);
    assert!(h.protected().iter().all(|p| p.pid != http.id()));
    let _ = http.kill();
    let _ = http.wait();

    let (mut real, port) = listener("llama-server");
    assert_eq!(h.protect_model_server(port, "model", true), Some(real.id()));
    let _ = real.kill();
    let _ = real.wait();

    let (mut job, port) = listener("llama-server");
    let start = super::super::procs::read(job.id()).unwrap().start;
    h.adopt(&Adopted {
        handle: "h-port".into(),
        command: "serve".into(),
        slug: None,
        cwd: "/tmp".into(),
        pid: job.id(),
        identity: format!("start:{start}"),
        scope: "session.test".into(),
        scope_dir: None,
        owner_dir: None,
    })
    .expect("adopted");
    assert_eq!(h.protect_model_server(port, "model", true), None);
    let _ = job.kill();
    let _ = job.wait();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A command carried into a VM by a host-side client: the client is what runs here.
struct Carried;

impl Confinement for Carried {
    fn describe(&self) -> String {
        "a VM the client below carries the command into".into()
    }

    fn boundary(&self) -> Option<&super::super::confine::Boundary> {
        None
    }

    fn wrap(&self, _: &ConfinePlan<'_>) -> Result<Vec<String>, ExecError> {
        Ok(vec!["env".into()])
    }

    fn memory_is_elsewhere(&self) -> bool {
        true
    }
}

/// A VM job's memory is the guest's: a host cap on the client said UNCAPPED for every call in a
/// firecode session, and the model read that as running on the host.
#[test]
fn a_command_carried_into_a_vm_is_not_capped_on_the_host() {
    let root = std::env::temp_dir().join(format!("letibot-carried-{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("a root");
    let h = HostProcesses::with_tree(&root, PlainTree::over(root.join("tree")))
        .with_confinement(Box::new(Carried));
    let id = h
        .spawn(&SpawnRequest {
            command: "true".into(),
            slug: None,
            cwd: "/".into(),
            scope: ScopeKind::Turn,
            scope_name: None,
            background: false,
            env: vec![],
            tty: false,
            memory: Some(64 << 20),
            env_trailer: false,
        })
        .expect("a job runs");
    let cap = h.job(&id).expect("the job").memory_cap;
    assert_eq!(cap, MemoryCap::NotAsked, "{cap:?}");
    let _ = std::fs::remove_dir_all(&root);
}

/// The owner's share is `memory.high` on the cgroup its own commands join, written where that
/// cgroup is made - and only when somebody asked. A job's `memory.max` ends the job; this is the
/// bound that makes the kernel slow an owner down instead of letting it starve the box first.
#[test]
fn the_owner_is_throttled_at_the_cgroup_its_commands_join() {
    let root = std::env::temp_dir().join(format!("letibot-owner-high-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let session_dir = |tree: &std::path::Path| {
        std::fs::read_dir(tree)
            .expect("the tree")
            .flatten()
            .map(|e| e.path())
            .find(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("session."))
            })
            .expect("the session scope was opened")
    };
    let run = |h: &HostProcesses| {
        h.spawn(&SpawnRequest {
            command: "true".into(),
            slug: None,
            cwd: "/".into(),
            scope: ScopeKind::Session,
            scope_name: None,
            background: true,
            env: vec![],
            tty: false,
            memory: None,
            env_trailer: false,
        })
        .expect("a job runs");
    };

    let share: u64 = 8 << 30;
    let h = HostProcesses::with_tree(&root, PlainTree::over(root.join("asked")))
        .with_owner_memory_high(Some(share));
    run(&h);
    assert_eq!(
        std::fs::read_to_string(session_dir(&root.join("asked")).join("memory.high"))
            .expect("memory.high on the owner's cgroup"),
        share.to_string()
    );

    // A host nobody gave a share writes nothing: a missing file is *no bound*, and a file holding
    // zero would freeze every command the session ran.
    let plain = HostProcesses::with_tree(&root, PlainTree::over(root.join("plain")));
    run(&plain);
    assert!(
        !session_dir(&root.join("plain"))
            .join("memory.high")
            .exists(),
        "nobody asked, so the owner is unbounded rather than throttled at zero"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A job keeps no core dump, and neither does anything it starts: a run the harness ends for size
/// (SIGXFSZ) is an expected ending, and its dump is what filled the operator's desktop and then
/// the user manager's unit table (R191, G5). The rlimit is inherited, so the shell under the
/// wrapper - where a test's own program runs - reports it.
#[test]
fn a_job_and_everything_under_it_keep_no_core_dump() {
    let root = std::env::temp_dir().join(format!("letibot-no-core-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let h = HostProcesses::with_tree(&root, PlainTree::over(root.join("tree")));
    let id = h
        .spawn(&SpawnRequest {
            command: "sh -c 'ulimit -c'".into(),
            slug: None,
            cwd: "/".into(),
            scope: ScopeKind::Turn,
            scope_name: None,
            background: false,
            env: vec![],
            tty: false,
            memory: None,
            env_trailer: false,
        })
        .expect("a job runs");
    let waited = h
        .wait_job(&id, std::time::Duration::from_secs(30))
        .expect("the job answers");
    assert!(
        matches!(waited, Waited::Happened { .. }),
        "the job did not finish: {waited:?}"
    );
    let text = h.output(&id, 0, 4096).expect("the row stays").text();
    assert_eq!(
        text.trim(),
        "0",
        "the core limit a command reports must be 0, not the box's own: {text:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A job that allocates past its cap is killed WHOLE, and the job beside it survives: the cap is the
/// cgroup's, and `memory.oom.group` takes every process in it - the difference between one runaway
/// test and half a test run left behind (G5, `docs/lifecycle.md` d.7). The line the session gets is
/// the job's own memory clause, asserted where it is built
/// (`builtins::jobs::tests::a_job_killed_at_its_cap_says_the_cap_and_not_a_finish`).
///
/// Skips where this box has no cgroup v2 subtree to delegate: a test cannot make one, and a cap that
/// could not be written would be measured as a cap that never fired.
#[test]
fn a_job_past_its_cap_is_killed_whole_while_its_neighbour_survives() {
    let root = std::env::temp_dir().join(format!("letibot-cap-whole-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    // The jobs run with the root as their cwd, and a spawn into a missing cwd is ENOENT.
    std::fs::create_dir_all(&root).unwrap();
    let h = match HostProcesses::new(&root) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("no cgroup v2 subtree here, so the cap cannot be measured: {e}");
            letibot_tokencore::apparatus::absent("a cgroup v2 subtree this user may write");
            return;
        }
    };
    let cap: u64 = 48 << 20;
    let ask = |command: &str, memory: Option<u64>| SpawnRequest {
        command: command.into(),
        slug: None,
        cwd: "/".into(),
        scope: ScopeKind::Session,
        scope_name: None,
        background: true,
        env: vec![],
        tty: false,
        memory,
        env_trailer: false,
    };
    // The backgrounded child is the point: it is in the job's cgroup, so a group kill takes it too.
    let hog = h
        .spawn(&ask(
            "sh -c 'sleep 300 & echo child=$!; python3 -c \"b = bytearray(256 << 20)\"'",
            Some(cap),
        ))
        .expect("the hog runs");
    // A daemon outside a scope of its own runs every job uncapped, and there is no cap to measure.
    if let Some(MemoryCap::Uncapped { why, .. }) = h.job(&hog).map(|j| j.memory_cap) {
        let _ = h.kill_job(&hog);
        let _ = std::fs::remove_dir_all(&root);
        eprintln!("the cap could not be set here: {why}");
        letibot_tokencore::apparatus::absent("a cgroup v2 memory controller this test may cap");
        return;
    }
    let neighbour = h
        .spawn(&ask("sleep 30", Some(cap)))
        .expect("the neighbour runs");
    let waited = h
        .wait_job(&hog, std::time::Duration::from_secs(180))
        .expect("the job answers");
    assert!(
        matches!(waited, Waited::Happened { .. }),
        "the hog was not ended by its cap inside the bound: {waited:?}"
    );
    let out = h.output(&hog, 0, 8192).expect("the row stays").text();
    let child: u32 = out
        .lines()
        .find_map(|l| l.strip_prefix("child="))
        .and_then(|p| p.trim().parse().ok())
        .unwrap_or_else(|| panic!("the command never named its child: {out:?}"));
    assert!(
        crate::exec::procs::read(child).is_none(),
        "the dead job's child {child} is still running: the cap killed the process, not the job"
    );
    let ended = h.job(&hog).expect("the ended job's row");
    let said = ended
        .memory_clause()
        .expect("a job ended at its cap says so");
    assert!(said.contains("KILLED at the cap"), "{said}");
    let beside = h.job(&neighbour).expect("the neighbour's row");
    assert!(
        beside.state.is_running(),
        "a neighbour's cap must not end this job: {beside:?}"
    );
    let _ = h.kill_job(&neighbour);
    let _ = std::fs::remove_dir_all(&root);
}
