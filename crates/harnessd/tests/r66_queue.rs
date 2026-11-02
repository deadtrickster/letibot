//! R66: what the daemon's per-session worker and its job table retain, per turn and per job.
//! Counts and bytes held, never wall-clock RSS: the subject is what is kept, not how fast.

mod support;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use letibot_harnessd::Parts;
use letibot_harnessd::config::Config;
use letibot_harnessd::jobwatch::{BackgroundKind, JobCompletion, JobWatchers};
use letibot_harnessd::memwatch::{self, Counting};
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::registry::Registry;
use letibot_tools::exec::scope::{Migration, Reaping, ScopeId, ScopeTree};
use letibot_tools::exec::{ExecError, HostProcesses, JobId, ProcessHost, ScopeKind, SpawnRequest};

/// The allocator, counted - the same one `memwatch` prints the daemon's census from.
#[global_allocator]
static ALLOC: Counting = Counting;

/// Jobs a fixture runs. A session that has served one project for two days runs thousands; three
/// hundred is enough for the per-job cost to be read off a difference.
const JOBS: usize = 300;

fn mb(bytes: f64) -> String {
    format!("{:.2} MiB", bytes / (1024.0 * 1024.0))
}

/// A directory that removes itself.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "{tag}-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock before epoch")
                .as_nanos(),
            {
                static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            }
        ));
        std::fs::create_dir_all(&path).expect("creating the temp dir");
        Self { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Real directories standing in for cgroups. The subject is `HostProcesses`' own table, and the
/// join script writes `$$` into `cgroup.procs` - so a plain file where the kernel would put a
/// cgroup makes the whole spawn path run unchanged on a box with no cgroup v2 to delegate.
struct PlainDirectories {
    root: PathBuf,
    open: Mutex<Vec<ScopeId>>,
}

impl PlainDirectories {
    fn over(root: PathBuf) -> Box<dyn ScopeTree> {
        let _ = std::fs::create_dir_all(&root);
        Box::new(PlainDirectories {
            root,
            open: Mutex::new(Vec::new()),
        })
    }
}

impl ScopeTree for PlainDirectories {
    fn describe(&self) -> String {
        format!(
            "plain directories under {} (lifetime only, and no cgroup behind it)",
            self.root.display()
        )
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
        let path = base.join(format!("{}.{}", kind.as_str(), name));
        std::fs::create_dir_all(&path)
            .map_err(|e| ExecError::Spawn(format!("{}: {e}", path.display())))?;
        std::fs::write(path.join("cgroup.procs"), b"")
            .map_err(|e| ExecError::Spawn(format!("{}: {e}", path.display())))?;
        let id = ScopeId {
            kind,
            name: name.to_string(),
            path,
        };
        self.open.lock().expect("open scopes").push(id.clone());
        Ok(id)
    }

    /// What the kernel would answer: the pids the join script wrote into this scope's
    /// `cgroup.procs`. A tree that answered `empty` would fail every spawn that outlives the
    /// five-second membership check, which is the check that makes an unowned process impossible.
    fn members(&self, scope: &ScopeId) -> Result<Vec<u32>, ExecError> {
        let body = std::fs::read_to_string(scope.path.join("cgroup.procs")).unwrap_or_default();
        Ok(body.lines().filter_map(|l| l.trim().parse().ok()).collect())
    }

    fn end(&self, scope: &ScopeId) -> Reaping {
        let observed = self
            .members(scope)
            .unwrap_or_default()
            .into_iter()
            .map(letibot_tools::exec::scope::Reaped::observe)
            .collect();
        // A scope that ended is gone from a listing, as a removed cgroup directory is.
        self.open
            .lock()
            .expect("open scopes")
            .retain(|s| s.path != scope.path);
        Reaping {
            scope: scope.clone(),
            at: SystemTime::now(),
            mechanism: "nothing to kill",
            observed,
            survivors: Vec::new(),
            waited: Duration::ZERO,
            removed: true,
            note: None,
        }
    }

    fn migrate(&self, _from: &ScopeId, _to: &ScopeId) -> Result<Migration, ExecError> {
        Err(ExecError::Spawn("this tree cannot move a process".into()))
    }

    fn list(&self) -> Vec<ScopeId> {
        self.open.lock().expect("open scopes").clone()
    }
}

fn a_host(dir: &TempDir) -> Arc<HostProcesses> {
    Arc::new(HostProcesses::with_tree(
        &dir.path,
        PlainDirectories::over(dir.path.join("scopes")),
    ))
}

fn spawn(host: &dyn ProcessHost, command: &str) -> JobId {
    host.spawn(&SpawnRequest {
        command: command.into(),
        slug: None,
        cwd: "/".into(),
        scope: ScopeKind::Session,
        scope_name: None,
        background: true,
        env: vec![],
        tty: false,
        memory: None,
    })
    .expect("spawn")
}

/// What the host's own table holds: one row per job it ever ran, and the bytes its captures
/// still hold.
fn table(host: &HostProcesses) -> (usize, usize) {
    let jobs = host.jobs();
    let held = jobs
        .iter()
        .map(|j| {
            host.output(&j.id, 0, 1)
                .map(|o| o.retained as usize)
                .unwrap_or(0)
        })
        .sum::<usize>();
    (jobs.len(), held)
}

/// Three hundred tiny jobs, and what the host keeps of them afterwards.
#[test]
fn a_job_row_outlives_its_job_and_the_captures_do_not() {
    let _serial = serial();
    let dir = TempDir::new("harnessd-r66-jobs");
    let host = a_host(&dir);
    let (live0, _, _) = memwatch::heap();
    let mut ids: Vec<JobId> = Vec::new();
    let mut marks: Vec<(usize, usize)> = Vec::new();
    for i in 0..JOBS {
        let id = spawn(host.as_ref(), &format!("echo job number {i}"));
        let _ = host.wait_job(&id, Duration::from_secs(30));
        ids.push(id);
        if (i + 1) % 100 == 0 {
            marks.push((i + 1, memwatch::heap().0));
        }
    }
    let (rows, held) = table(&host);
    let (live1, _, _) = memwatch::heap();
    println!(
        "{JOBS} jobs: {rows} row(s) in the host's table, {held} capture byte(s) held, heap live {} -> {} (+{})",
        mb(live0 as f64),
        mb(live1 as f64),
        mb(live1.saturating_sub(live0) as f64)
    );
    for (n, live) in &marks {
        println!(
            "  after {n:>3} jobs: +{} ({} a job)",
            mb(live.saturating_sub(live0) as f64),
            live.saturating_sub(live0) / n
        );
    }
    assert_eq!(rows, JOBS, "a job's row stays for the host's life");
    assert!(
        held <= 64 * 1024 * 1024,
        "the settled captures are over the budget: {held} bytes held"
    );
    // And the scopes: one per job, and a job that ended by itself is never `end`ed, so on a real
    // cgroup tree each of these is a directory until the session's own scope goes.
    let scopes = host.scopes().len();
    println!("  {scopes} scope(s) open for {rows} row(s)");
    assert_eq!(
        scopes,
        JOBS + 1,
        "one session scope and one per job, and nothing removed a job's"
    );
    // And what one publish of that table costs. `Harness::publish_jobs` runs several times a turn
    // and calls `jobs()`, which clones every view - so a session's per-turn allocation grows with
    // the number of commands it has EVER run, whatever it is doing now.
    let views = host.jobs();
    let published = views.len() * std::mem::size_of::<letibot_tools::exec::JobView>()
        + views
            .iter()
            .map(|j| j.id.0.len() + j.command.len() + j.slug.as_ref().map_or(0, String::len))
            .sum::<usize>();
    println!(
        "  one `publish_jobs` clones {published} byte(s) of {rows} row(s) - {} a row, and it runs \
         several times a turn",
        published / rows
    );
    let _ = ids;
    host.end_running("test");
}

/// A hundred jobs that each write a megabyte: the capture is the term R66 guessed at, and the
/// host holds a budget of it rather than a window per job.
#[test]
fn a_hundred_big_jobs_hold_the_capture_budget_and_not_a_window_each() {
    let _serial = serial();
    const BIG: usize = 100;
    const EACH: usize = 1 << 20;
    let dir = TempDir::new("harnessd-r66-big");
    let host = a_host(&dir);
    for i in 0..BIG {
        let id = spawn(host.as_ref(), &format!("head -c {EACH} /dev/zero"));
        let _ = host.wait_job(&id, Duration::from_secs(30));
        if i == 0 {
            // The first job's own window, before any budget can have released it: 8 MiB per job
            // is what R66's estimate multiplied by the number of jobs.
            let held = host.output(&id, 0, 1).map(|o| o.retained).unwrap_or(0);
            println!("one job's window holds {held} of the {EACH} bytes it wrote");
        }
    }
    let (rows, held) = table(&host);
    println!(
        "{BIG} jobs of {EACH} bytes each: {rows} row(s), {held} capture byte(s) held - a per-job \
         window would be {}",
        BIG * EACH
    );
    assert_eq!(rows, BIG, "every job keeps its row");
    // The budget is applied at the NEXT spawn, so the host may hold it plus the last job's window.
    assert!(
        held <= 64 * 1024 * 1024 + EACH,
        "the settled captures held {held} bytes, over the 64 MiB budget and one job's window - the \
         estimate of a window per job would be {}",
        BIG * EACH
    );
    // And a released capture still says what the job wrote: the counts are the record's, and only
    // the bytes are gone.
    let oldest = host
        .jobs()
        .first()
        .map(|j| j.id.clone())
        .expect("the first job's row");
    let slice = host
        .output(&oldest, 0, 1)
        .expect("a released row still answers");
    assert_eq!(
        (slice.produced, slice.retained),
        (EACH as u64, 0),
        "the oldest job's capture was released and its counts kept"
    );
    host.end_running("test");
}

/// A watcher set, and the two maps a session's backgrounded work leaves in it.
#[test]
fn the_settled_set_is_bounded_and_still_refuses_a_repeat_watch() {
    let _serial = serial();
    // Past the bound on purpose: a guard that was never asked to evict is not a guard.
    const WATCHED: usize = 1200;
    let dir = TempDir::new("harnessd-r66-watch");
    let host: Arc<dyn ProcessHost> = a_host(&dir);
    let hub = Hub::new("s-r66-watch");
    let watch = JobWatchers::new(&host, &hub, None);
    let (live0, _, _) = memwatch::heap();
    let mut handles: Vec<String> = Vec::new();
    // In rounds, so the fixture is not a thousand watcher threads at once.
    for round in 0..(WATCHED / 200) {
        for i in 0..200 {
            let id = spawn(host.as_ref(), &format!("echo watched {round}.{i}"));
            watch.watch(id.0.clone());
            handles.push(id.0);
        }
        let until = Instant::now() + Duration::from_secs(120);
        while watch.retained().watching > 0 && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    let kept = watch.retained();
    let (live1, _, _) = memwatch::heap();
    println!(
        "{WATCHED} watched jobs: retained {kept:?}, heap live {} -> {} (+{}), {} settlement(s) queued",
        mb(live0 as f64),
        mb(live1 as f64),
        mb(live1.saturating_sub(live0) as f64),
        watch.take_completions().len()
    );
    assert_eq!(
        kept.watching, 0,
        "every watcher left its handle behind in `watching`: {kept:?}"
    );
    assert_eq!(
        kept.wanted, 0,
        "nothing was asked for, so nothing may stay in `wanted`: {kept:?}"
    );
    assert_eq!(
        kept.settled, 1024,
        "the duplicate guard kept {} of {WATCHED} handles - it is a guard and not a history: \
         {kept:?}",
        kept.settled
    );
    // The guard's whole job: a second `watch` for a handle that just settled starts no watcher.
    let recent = handles.last().expect("a handle").clone();
    watch.watch(recent.clone());
    assert!(
        !watch.delivering(&recent),
        "a settled handle was watched a second time, which is a second settlement for one job"
    );
    watch.stop();
}

/// A result delivered on every turn, and the record of what the model has been told.
#[test]
fn a_result_delivered_every_turn_does_not_grow_the_read_record() {
    let _serial = serial();
    let hub = Hub::new("s-r66-read");
    let watch = JobWatchers::watching_tasks(&hub, None);
    let mut most = 0usize;
    for turn in 0..200 {
        let done = vec![completion(
            &format!("j{turn}"),
            Some(&format!("slug-{turn}")),
        )];
        watch.read_now(&done);
        watch.turn_ended();
        most = most.max(watch.retained().read);
    }
    let kept = watch.retained();
    println!(
        "200 turns, one result delivered each: the read record held at most {most} name(s), {} at the end",
        kept.read
    );
    assert_eq!(kept.wanted, 0, "every want was spent by its delivery");
    assert!(
        most <= 4,
        "the read record kept {most} names: what may still be graced is a turn and a half's \
         worth, so a session that delivers a result every turn must not grow it for its life"
    );
    // And a want for a result the model already has is not recorded: nothing would ever spend it,
    // so it would sit in `wanted` for the session's life.
    watch.want_now("j199", letibot_tools::runtime::WantedNow::WakeNow);
    assert_eq!(
        watch.retained().wanted,
        0,
        "a want was recorded for a handle whose result was already delivered"
    );
}

/// One settlement as a watcher would queue it.
fn completion(job: &str, slug: Option<&str>) -> JobCompletion {
    JobCompletion {
        memory: None,
        kind: BackgroundKind::Job,
        owner: "s-r66-read".into(),
        job: job.into(),
        command: "echo hi".into(),
        slug: slug.map(str::to_string),
        state: "exited 0".into(),
        produced: 3,
        elapsed_ms: 4,
        detail: String::new(),
        arrived: SystemTime::now(),
    }
}

// ---------------------------------------------------------------------------
// The worker and the queue: many turns through a real `Sessions`
// ---------------------------------------------------------------------------

/// One test at a time in this binary. Two reasons, and either alone is enough: `XDG_CONFIG_HOME` is
/// the process's and the provider this file points at is written there, and every measurement here
/// is `memwatch::heap()`, which is the WHOLE process's live heap - a sibling test allocating
/// through the same window is a delta that is not this test's.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner())
}

/// Turns the fixture runs. A 47-hour session runs thousands; forty is where a per-turn entry has
/// to show, because forty of anything is a number a reader can see.
const TURNS: usize = 40;

/// A model that answers every request with one word, in the frames a provider parses.
fn a_model_that_says_ok() -> String {
    a_model_that_says("ok")
}

/// A model that answers every request with `answer`, in the frames a provider parses. The answer
/// is the size knob: what a child says is what the tree keeps of it.
fn a_model_that_says(answer: &str) -> String {
    use std::io::{Read, Write};
    let answer = answer.to_string();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { return };
            let mut acc: Vec<u8> = Vec::new();
            let mut buf = [0u8; 8192];
            let mut want: Option<usize> = None;
            loop {
                match s.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => acc.extend_from_slice(&buf[..n]),
                }
                if want.is_none()
                    && let Some(at) = acc.windows(4).position(|w| w == b"\r\n\r\n")
                {
                    let len = acc[..at]
                        .split(|b| *b == b'\n')
                        .find_map(|line| {
                            let line = String::from_utf8_lossy(line);
                            let (name, value) = line.split_once(':')?;
                            name.trim()
                                .eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())?
                        })
                        .unwrap_or(0);
                    want = Some(at + 4 + len);
                }
                if let Some(total) = want
                    && acc.len() >= total
                {
                    break;
                }
            }
            let mut body = String::new();
            let said = serde_json::json!({ "choices": [{ "delta": { "content": answer } }] });
            body.push_str(&format!("data: {said}\n\n"));
            body.push_str("data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n");
            body.push_str("data: [DONE]\n\n");
            let _ = write!(
                s,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = s.flush();
        }
    });
    format!("http://127.0.0.1:{port}/v1/chat/completions")
}

/// A session opened by the daemon's own `Sessions`, on the byte vocabulary.
fn a_daemon(dir: &TempDir, url: &str) -> (Parts, Config, Arc<Registry>) {
    use letibot_harnessd::config::{ProviderConfig, Seat};
    use letibot_harnessd::{Dialect, Parts};
    use letibot_sessionlog::registry::{Registry, SessionWiring};

    let config = dir.path.join("config");
    std::fs::create_dir_all(config.join("letibot")).expect("the config dir");
    std::fs::write(
        config.join("letibot/providers.toml"),
        format!(
            "# Written by crates/harnessd/tests/r66_queue.rs\n[deepseek]\nkey = \"sk-test\"\nurl = \"{url}\"\n"
        ),
    )
    .expect("providers.toml");
    // SAFETY: one test in this binary, and the value is only ever read by this fixture.
    unsafe { std::env::set_var("XDG_CONFIG_HOME", &config) };

    let mut cfg = Config::for_this_box(&dir.path);
    cfg.dialect = Dialect::Qwen;
    cfg.seat = Seat::Leticode;
    // No `--bash`: an exec backend wants a cgroup a test inside a boundary has not got, and the
    // job table is measured by its own test above.
    cfg.allow_bash = false;
    cfg.vocab_gguf = None;
    cfg.provider = Some(ProviderConfig {
        name: "deepseek".into(),
        model: Some("deepseek-chat".into()),
        api_key: None,
        thinking: false,
    });
    cfg.web_search = None;
    cfg.permission = Vec::new();
    cfg.http_retries = 0;
    // No wall: the byte vocabulary makes message zero tens of thousands of tokens.
    cfg.context_window = Some(4_000_000);
    cfg.store = Some(dir.path.join("sessions.db"));
    cfg.session_id = "s-r66-turns".into();

    let parts = Parts::load(&cfg).expect("a provider session opens on the byte vocabulary");
    let registry = Registry::new();
    registry
        .create(
            &cfg.session_id,
            "r66 turns",
            SessionWiring {
                model: cfg.model.clone(),
                dialect: "qwen".into(),
                endpoint: url.to_string(),
                workspace: cfg.workspace.display().to_string(),
            },
        )
        .expect("the session registers");
    (parts, cfg, registry)
}

/// Forty turns through the daemon's own worker, and everything the queue path holds afterwards.
#[test]
fn turns_through_the_worker_leave_no_entry_behind() {
    use letibot_harnessd::sessions::Sessions;

    let _serial = serial();
    let url = a_model_that_says_ok();
    let dir = TempDir::new("harnessd-r66-turns");
    let (parts, cfg, registry) = a_daemon(&dir, &url);
    let id = cfg.session_id.clone();
    let mut sessions =
        Sessions::open_first(&parts, cfg, registry.clone()).expect("the daemon opens");
    // One warm-up turn, before the baseline: a session's first turn pays costs it never pays again.
    sessions
        .submit(&id, "warm-up")
        .expect("the warm-up answers");

    let (live0, _, _) = memwatch::heap();
    let mut per_turn: Vec<usize> = Vec::new();
    let mut most = (0usize, 0usize, 0usize);
    for turn in 0..TURNS {
        sessions
            .submit(&id, &format!("turn {turn} of the session"))
            .expect("every turn answers");
        per_turn.push(memwatch::heap().0);
        let held = sessions
            .harness_of(&id)
            .and_then(|h| h.job_watchers())
            .map(|w| w.retained())
            .unwrap_or_default();
        most = (
            most.0.max(held.watching),
            most.1.max(held.settled),
            most.2.max(held.read),
        );
    }
    let (live1, _, _) = memwatch::heap();
    let watchers = sessions
        .harness_of(&id)
        .and_then(|h| h.job_watchers())
        .map(|w| w.retained())
        .unwrap_or_default();
    println!(
        "{TURNS} turns on one session: opened {} · registry {} · heap live {} -> {} (+{}) · \
         watchers {watchers:?} · most seen (watching, settled, read) {most:?}",
        sessions.opened(),
        registry.ids().len(),
        mb(live0 as f64),
        mb(live1 as f64),
        mb(live1.saturating_sub(live0) as f64),
    );
    println!(
        "  per turn: {} · first half {} · second half {}",
        mb((live1.saturating_sub(live0)) as f64 / TURNS as f64),
        mb(per_turn[TURNS / 2].saturating_sub(live0) as f64 / (TURNS / 2) as f64),
        mb(live1.saturating_sub(per_turn[TURNS / 2]) as f64 / (TURNS / 2) as f64),
    );

    // Nothing the queue path holds may have gained an entry: one session, one harness, and a
    // watcher set holding nothing because nothing was backgrounded.
    assert_eq!(sessions.opened(), 1, "a turn built a second harness");
    assert_eq!(
        registry.ids().len(),
        1,
        "a turn registered a second session: {:?}",
        registry.ids()
    );
    assert_eq!(
        watchers,
        letibot_harnessd::jobwatch::Retained::default(),
        "the watcher set kept something across {TURNS} turns"
    );
    registry.close();
    let _ = sessions.drain_subagents(Duration::from_secs(5));
}

// ---------------------------------------------------------------------------
// The children a session has run
// ---------------------------------------------------------------------------

/// Children the fixture starts. A session that has served one project for two days starts dozens
/// to hundreds; twenty is where a per-child retention has to show.
const CHILDREN: usize = 6;

/// One tool call on a session's own runtime, as the model's call would arrive.
fn call(
    sessions: &letibot_harnessd::sessions::Sessions<'_>,
    root: &str,
    name: &str,
    args: serde_json::Value,
) -> letibot_tools::ToolResult {
    let call = letibot_transcript::ToolCall {
        id: format!("call_{name}"),
        name: name.to_string(),
        arguments: args.to_string(),
    };
    sessions
        .harness_of(root)
        .expect("the root is open")
        .runtime_handle()
        .invoke("t1", &call, &mut letibot_tools::NullToolSink)
}

/// What a child says. The size is the knob: a child's answer is what the tree keeps of it, and a
/// real child's answer is kilobytes.
const CHILD_ANSWER: usize = 16 << 10;

/// A session that has run ten children, three ways: with a big answer and a small one, each child
/// parked as soon as it is quiet, and once with the children left open. The parked runs separate
/// the answer's cost from the row's, and the open one is what a child still running costs on top.
#[test]
fn children_a_session_has_run_leave_their_answers_behind() {
    use letibot_harnessd::sessions::Sessions;

    let _serial = serial();
    let big = format!("the child's own answer. {}", "a".repeat(CHILD_ANSWER));
    let small = "ok".to_string();
    let dir = TempDir::new("harnessd-r66-children");

    let cost = |url: &str, park: Option<Duration>, what: &str| -> usize {
        let (parts, mut cfg, registry) = a_daemon(&dir, url);
        cfg.child_park_after = park;
        let id = cfg.session_id.clone();
        let sessions =
            Sessions::open_first(&parts, cfg, registry.clone()).expect("the daemon opens");
        let (live0, _, _) = memwatch::heap();
        let mut marks: Vec<(usize, usize)> = Vec::new();
        for n in 0..CHILDREN {
            let r = call(
                &sessions,
                &id,
                "task",
                serde_json::json!({"prompt": format!("child {n}: answer and stop")}),
            );
            let handle = match r.outcome {
                letibot_transcript::ToolOutcome::Backgrounded { handle, .. } => handle,
                other => panic!("`task` did not start a child: {other:?} - {}", r.payload),
            };
            // And the child's own answer, which is what its slot keeps: nothing here may run on
            // before the child has said something, or the heap measured is a child still opening.
            let until = Instant::now() + Duration::from_secs(60);
            loop {
                let asked = call(
                    &sessions,
                    &id,
                    "task_result",
                    serde_json::json!({"task": handle}),
                );
                if asked.outcome == letibot_transcript::ToolOutcome::Ok || Instant::now() > until {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            if (n + 1) % 3 == 0 {
                marks.push((n + 1, memwatch::heap().0));
            }
        }
        let (live1, _, _) = memwatch::heap();
        let grew = live1.saturating_sub(live0);
        println!(
            "{what}: {} session(s) live · heap +{} ({} a child)",
            registry.ids().len(),
            mb(grew as f64),
            grew / CHILDREN
        );
        for (n, live) in &marks {
            println!(
                "  after {n:>3} children: +{} ({} a child)",
                mb(live.saturating_sub(live0) as f64),
                live.saturating_sub(live0) / n
            );
        }
        registry.close();
        let _ = sessions.drain_subagents(Duration::from_secs(10));
        grew
    };

    let big_url = a_model_that_says(&big);
    let small_url = a_model_that_says(&small);
    let closed_big = cost(&big_url, Some(Duration::ZERO), "parked, 16 KiB answer");
    let closed_small = cost(&small_url, Some(Duration::ZERO), "parked, 2 byte answer");
    let open_big = cost(&big_url, None, "not parked, 16 KiB answer");
    let each = |total: usize| total / CHILDREN;
    let per_answer_byte = (each(closed_big).saturating_sub(each(closed_small)) as f64)
        / (big.len().saturating_sub(small.len())) as f64;
    println!(
        "one closed child: {} with a {} byte answer, {} with {} · one still open: {} · the answer \
         costs {per_answer_byte:.1}x its own bytes",
        each(closed_big),
        big.len(),
        each(closed_small),
        small.len(),
        each(open_big),
    );
    assert!(
        closed_big >= CHILDREN * big.len(),
        "the {CHILDREN} closed children hold {} between them, less than their {} of answers: the \
         slot is supposed to keep what the child said",
        mb(closed_big as f64),
        mb((CHILDREN * big.len()) as f64)
    );
    assert!(
        open_big > closed_big,
        "a child still open holds no more than a closed one ({} vs {}), so the two costs are one",
        mb(open_big as f64),
        mb(closed_big as f64)
    );
}
