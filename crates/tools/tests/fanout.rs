//! **`fanout` against a fake ssh** — one command on many hosts, one row per host, and the five
//! things the contract asks to be measured: they all succeed, one host fails and the rest still
//! report, an unknown host is a row, a deadline stops a slow one, and **every host has a row**.
//!
//! The rig is `support/fake_remote.rs`: a script standing in for `ssh` that runs the command
//! locally against a temporary "remote home", so `~/letibot-remote` is expanded on the far side
//! and the job files, the exit file and the process group are the production ones. No test here
//! contacts another machine.

#[path = "support/fake_remote.rs"]
mod fake_remote;

use std::sync::Arc;
use std::time::{Duration, Instant};

use letibot_tools::builtins::fanout::Fanout;
use letibot_tools::exec::{HostProcesses, JobId, NoScopes, ProcessHost};
use letibot_tools::fleet::{NoBook, ProcessBook, ProcessNote};
use letibot_transcript::ToolOutcome;
use serde_json::json;

use fake_remote::Rig;

/// A fanout wired to one rig.
fn tool(rig: &Rig) -> Fanout {
    Fanout::new()
        .with_file(rig.hosts_file())
        .with_paths(rig.paths())
        .with_ssh(rig.ssh_program())
        // **A fake answers in milliseconds**, so a bound of five seconds is the same measurement a
        // production bound of thirty would make, in a test that cannot take thirty.
        .with_bound(Duration::from_secs(5))
}

/// **A command that lives long enough to have a pid.** A job that exits in five milliseconds has
/// already written its exit file by the time the launch's pid poll asks, and `RemoteProcesses`
/// then reports pid `0` — harmless (the exit file is the settlement, and nothing signals pid 0) but
/// useless to a test that wants to look at the process.
const BRIEFLY: &str = "sleep 0.3";

/// The row lines of an answer — the header is not one.
fn rows(payload: &str) -> Vec<&str> {
    payload.lines().filter(|l| l.starts_with("  ")).collect()
}

/// **The same work on three hosts, and three rows that all say it worked.**
#[test]
fn every_host_runs_and_answers_a_row() {
    let rig = Rig::new("all-ok", &["fake1", "fake2", "fake3"]);
    let answer = tool(&rig).answer(
        &json!({"hosts": ["fake1", "fake2", "fake3"], "command": "echo one; echo two"}),
        None,
        "fanout-ok",
    );
    assert_eq!(answer.outcome, ToolOutcome::Ok);
    let text = answer.payload.clone();
    let lines = rows(&text);
    assert_eq!(lines.len(), 3, "one row per host:\n{text}");
    for host in ["fake1", "fake2", "fake3"] {
        assert!(
            text.contains(&format!("{host} on {host} · exited 0")),
            "`{host}`'s row says it exited 0:\n{text}"
        );
    }
    assert!(text.contains("3 planned, 3 answered"), "{text}");
    assert!(text.contains("every cell exited 0"), "{text}");
    // **The output tail is the host's own bytes** — not a summary this tool invented.
    assert!(text.contains("tail: one ⏎ two"), "{text}");
    // **And the work really happened on the "remote"**: the fake's home, not this tree.
    assert!(
        rig.remote_root()
            .join("fanout-ok")
            .join(".letibot-remote")
            .join("jobs")
            .is_dir(),
        "the job directory is in the remote root, not here"
    );
}

/// **One host fails and the other two still report.** The whole call is one answer, and a non-zero
/// exit is a ROW's business, not the call's.
#[test]
fn one_failing_host_does_not_swallow_the_others() {
    let rig = Rig::new("one-fails", &["fake1", "fake2", "fake3"]);
    let answer = tool(&rig).answer(
        &json!({"hosts": {
            "fake1": "true",
            "fake2": "echo it broke >&2; exit 3",
            "fake3": "true"
        }}),
        None,
        "fanout-one",
    );
    assert_eq!(answer.outcome, ToolOutcome::Ok, "{}", answer.payload);
    let text = answer.payload.clone();
    assert_eq!(rows(&text).len(), 3, "{text}");
    assert!(text.contains("fake2 on fake2 · exited 3"), "{text}");
    assert!(
        text.contains("it broke"),
        "the failing host's stderr is in its tail:\n{text}"
    );
    assert!(text.contains("fake1 on fake1 · exited 0"), "{text}");
    assert!(text.contains("fake3 on fake3 · exited 0"), "{text}");
    assert!(text.contains("1 of 3 cells did not exit 0"), "{text}");
}

/// **A host nobody declared is a row, and the other hosts still run.** This is the contract's
/// sentence — *an unreachable host is a row, not a failure of the whole* — in its cheapest form.
#[test]
fn an_unknown_host_is_a_row_with_the_names_the_file_has() {
    let rig = Rig::new("unknown", &["fake1"]);
    let answer = tool(&rig).answer(
        &json!({"hosts": ["fake1", "nope"], "command": "true"}),
        None,
        "fanout-unknown",
    );
    assert_eq!(answer.outcome, ToolOutcome::Ok);
    let text = answer.payload.clone();
    assert_eq!(rows(&text).len(), 2, "two hosts, two rows:\n{text}");
    assert!(text.contains("fake1 on fake1 · exited 0"), "{text}");
    assert!(text.contains("nope on nope · never ran"), "{text}");
    assert!(
        text.contains("there is no host `nope`"),
        "the refusal says which names exist:\n{text}"
    );
    assert!(text.contains("this file declares fake1"), "{text}");
}

/// **A host that cannot be reached is a row too** — ssh says the name does not resolve, and the row
/// carries that sentence rather than the fanout failing.
#[test]
fn an_unreachable_host_is_a_row_and_the_rest_still_report() {
    let mut rig = Rig::new("unreachable", &["fake1"]);
    rig.unreachable();
    let answer = tool(&rig).answer(
        &json!({"hosts": ["fake1", "gone"], "command": "true"}),
        None,
        "fanout-gone",
    );
    let text = answer.payload.clone();
    assert_eq!(rows(&text).len(), 2, "{text}");
    assert!(text.contains("fake1 on fake1 · exited 0"), "{text}");
    assert!(text.contains("gone on gone · never ran"), "{text}");
    assert!(
        text.contains("could not be reached"),
        "the row says the host was not reached:\n{text}"
    );
    assert!(text.contains("Could not resolve hostname"), "{text}");
}

/// **The deadline ends a slow branch, and its row says stopped.** Not `exited`, not missing: a
/// third fate, with the reason.
#[test]
fn the_deadline_stops_a_slow_host_and_its_row_says_stopped() {
    let rig = Rig::new("deadline", &["fake1", "fake2"]);
    let started = std::time::Instant::now();
    let answer = tool(&rig).answer(
        &json!({
            "hosts": {"fake1": "sleep 30", "fake2": "true"},
            "deadline_s": 2
        }),
        None,
        "fanout-deadline",
    );
    let took = started.elapsed();
    let text = answer.payload.clone();
    assert_eq!(rows(&text).len(), 2, "{text}");
    assert!(
        text.contains("fake1 on fake1 · stopped · ") && text.contains("its deadline passed"),
        "the slow host's row says what stopped it:\n{text}"
    );
    assert!(text.contains("fake2 on fake2 · exited 0"), "{text}");
    // **The call is bounded by the deadline it was given**, not by the slow branch.
    assert!(
        took < Duration::from_secs(20),
        "a 2-second deadline returned in {took:?}"
    );
    // **And the process is really gone**, not just reported so: the pid in the job directory is
    // no longer there. A row that says `stopped` about a live process is the claim-versus-fact
    // failure this whole feature is against.
    let jobs = rig
        .remote_root()
        .join("fanout-deadline")
        .join(".letibot-remote")
        .join("jobs");
    let mut checked = 0;
    for e in std::fs::read_dir(&jobs)
        .expect("the job directory")
        .flatten()
    {
        if e.path().extension().is_some_and(|x| x == "pid")
            && let Ok(pid) = std::fs::read_to_string(e.path())
                .unwrap_or_default()
                .trim()
                .parse::<u32>()
        {
            checked += 1;
            if let Some(command) = pid_command(pid) {
                assert!(
                    !command.contains("sleep 30"),
                    "the slow branch is still running: pid {pid} `{command}`"
                );
            }
        }
    }
    assert_eq!(checked, 2, "both branches wrote a pid file");
}

/// The command line of a live pid, or `None` when nothing is there.
fn pid_command(pid: u32) -> Option<String> {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .ok()
        .map(|b| String::from_utf8_lossy(&b).replace('\0', " "))
}

/// **A fanout is one gated call, and its arguments name every host and every command** — the card's
/// whole content. Asserted on the schema, because that is what the gate is handed: `Access::Exec`
/// (so it asks) and the two arguments (so the person reading the card reads the plan).
#[test]
fn the_call_is_one_exec_that_names_every_host() {
    let schema = letibot_tools::runtime::Tool::schema(&Fanout::new());
    assert_eq!(schema.name, "fanout");
    assert_eq!(schema.access, letibot_tools::schema::Access::Exec);
    let props = &schema.parameters["properties"];
    assert!(props.get("hosts").is_some(), "the hosts are on the wire");
    assert!(props.get("command").is_some(), "so is the command");
    // A description that says what a row means, and what never running looks like.
    assert!(schema.description.contains("one row per host"));
    assert!(schema.description.contains("never ran"));
    assert!(schema.description.contains("deadline_s"));
}

/// **What a book is told is what the supervisor needs** — the start, then the ending. This is the
/// daemon's half measured here because it is the contract between the two crates: a record with no
/// identity is a record the supervisor must not act on, and a record whose host is empty would be
/// checked on the wrong machine.
#[test]
fn every_started_branch_is_written_down_with_its_host() {
    #[derive(Default)]
    struct Book {
        started: std::sync::Mutex<Vec<ProcessNote>>,
        ended: std::sync::Mutex<Vec<(String, String)>>,
    }
    impl ProcessBook for Book {
        fn started(&self, p: &ProcessNote) {
            self.started.lock().expect("book").push(p.clone());
        }
        fn ended(
            &self,
            handle: &str,
            state: letibot_tokencore::store::ProcState,
            _exit: Option<i32>,
        ) {
            self.ended
                .lock()
                .expect("book")
                .push((handle.to_string(), state.word().to_string()));
        }
    }
    let rig = Rig::new("book", &["fake1", "fake2"]);
    let book = std::sync::Arc::new(Book::default());
    let answer = Fanout::new()
        .with_file(rig.hosts_file())
        .with_paths(rig.paths())
        .with_ssh(rig.ssh_program())
        .with_bound(Duration::from_secs(5))
        .with_book(book.clone())
        .answer(
            &json!({"hosts": ["fake1", "fake2"], "command": BRIEFLY}),
            None,
            "fanout-book",
        );
    assert_eq!(answer.outcome, ToolOutcome::Ok);
    let started = book.started.lock().expect("book").clone();
    assert_eq!(started.len(), 2, "one record per branch: {started:?}");
    for p in &started {
        assert!(
            p.host.is_some(),
            "a branch knows which host it is on: {p:?}"
        );
        assert!(
            p.identity.starts_with("cmd:"),
            "a host's identity is the command marker the far side greps for: {p:?}"
        );
        assert!(p.pid > 0);
        assert!(
            p.log.as_deref().is_some_and(|l| l.ends_with(".out")),
            "the record names the output file the supervisor reads: {p:?}"
        );
    }
    let ended = book.ended.lock().expect("book").clone();
    assert_eq!(ended.len(), 2, "{ended:?}");
    assert!(ended.iter().all(|(_, state)| state == "done"), "{ended:?}");
}

/// A book that writes nothing is the default, and says so rather than pretending.
#[test]
fn a_run_with_no_daemon_writes_no_records_and_still_works() {
    let rig = Rig::new("nobook", &["fake1"]);
    let answer = Fanout::new()
        .with_file(rig.hosts_file())
        .with_paths(rig.paths())
        .with_ssh(rig.ssh_program())
        .with_book(std::sync::Arc::new(NoBook))
        .answer(
            &json!({"hosts": ["fake1"], "command": "true"}),
            None,
            "fanout-nobook",
        );
    assert_eq!(answer.outcome, ToolOutcome::Ok, "{}", answer.payload);
}

// ------------------------------------------------- `background: true` (FB)

/// **The session's own job table, on a box with no cgroup to delegate.**
///
/// A backgrounded fanout needs a host to *hold* its handle — one map — and nothing else: the
/// branches are jobs of the fake remotes' own tables, so no process of this box is ever started.
/// `NoScopes` says exactly that, which is why this rig runs on a container as happily as on a
/// desktop: the absence of a process-lifetime tree is a fact about the host, not about the test.
fn session_host(tag: &str) -> Arc<HostProcesses> {
    let root = std::env::temp_dir().join(format!(
        "letibot-fanout-bg-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&root).expect("root");
    Arc::new(HostProcesses::with_tree(
        &root,
        Box::new(NoScopes::new(
            "a fanout branch is a job of the host it runs on, so this test's own host needs no \
             process-lifetime tree at all",
        )),
    ))
}

/// The handle a backgrounded fanout answered with, or the failed invocation.
fn handle_of(answer: &letibot_tools::runtime::Invocation) -> String {
    match &answer.outcome {
        ToolOutcome::Backgrounded { handle, how, .. } => {
            assert_eq!(
                *how,
                letibot_transcript::Backgrounding::Asked,
                "the model asked for this one"
            );
            handle.clone()
        }
        other => panic!(
            "a backgrounded fanout hands back a handle: {other:?} — {}",
            answer.payload
        ),
    }
}

/// The journal `job_output` would read, as the model would read it.
fn journal(host: &HostProcesses, handle: &str) -> String {
    host.output(&JobId(handle.to_string()), 0, 64 * 1024)
        .expect("the journal")
        .text()
}

/// **A backgrounded fanout answers ONE handle at once**, and that handle is a job of THIS session's
/// table — the whole of what `jobwatch` needs to settle it.
#[test]
fn a_background_fanout_answers_one_handle_at_once() {
    let rig = Rig::new("bg-at-once", &["fake1", "fake2"]);
    let host = session_host("at-once");
    let began = Instant::now();
    let answer = tool(&rig).answer(
        &json!({"hosts": ["fake1", "fake2"], "command": "sleep 20", "background": true}),
        Some(host.clone() as Arc<dyn ProcessHost>),
        "fanout-bg",
    );
    let took = began.elapsed();
    let handle = handle_of(&answer);
    // **At once is the claim**, and it is a measurement: the branches sleep twenty seconds and
    // the call came back while they were still running.
    assert!(
        took < Duration::from_secs(10),
        "a backgrounded fanout took {took:?} to answer"
    );
    let view = host
        .job(&JobId(handle.clone()))
        .expect("the handle is a job of this session's table");
    assert!(view.state.is_running(), "{view:?}");
    assert_eq!(
        view.slug.as_deref(),
        Some("fanout"),
        "by name as well as id"
    );
    assert_eq!(view.pid, 0, "a job of jobs has no process of its own");
    assert!(
        view.command.contains("2 branch(es) on fake1, fake2"),
        "the row says what it is: {}",
        view.command
    );
    let _ = host.kill_job(&JobId(handle));
}

/// **`job_output` mid-run reads the per-host progress**: a row per branch that has ended, and a
/// `started` line for every branch that is still running.
#[test]
fn job_output_mid_run_names_the_rows_that_ended_and_the_ones_still_running() {
    let rig = Rig::new("bg-progress", &["fake1", "fake2"]);
    let host = session_host("progress");
    let answer = tool(&rig).answer(
        &json!({
            "hosts": {"fake1": "true", "fake2": "sleep 20"},
            "background": true
        }),
        Some(host.clone() as Arc<dyn ProcessHost>),
        "fanout-progress",
    );
    let handle = handle_of(&answer);
    let id = JobId(handle.clone());
    let arrived = fake_remote::until(Duration::from_secs(20), || {
        journal(&host, &handle).contains("fake1 on fake1 · exited 0")
    });
    let text = journal(&host, &handle);
    assert!(
        arrived,
        "the branch that exited never reached the journal: {text}"
    );
    assert!(
        text.contains("fake2 on fake2 · started"),
        "the branch still running is a row too: {text}"
    );
    assert!(
        !text.contains("fake2 on fake2 · exited"),
        "a branch that has not ended has no ending: {text}"
    );
    // **And the handle is still running** — the journal's `started` line is not a verdict.
    assert!(
        host.job(&id).map(|v| v.state.is_running()).unwrap_or(false),
        "{text}"
    );
    let _ = host.kill_job(&id);
}

/// **`job_kill` on the handle ends every branch, on every host, and each branch's record says
/// `stopped`** — not `done`, because nothing about that ending is the command's own.
#[test]
fn job_kill_stops_every_branch_and_every_record_says_stopped() {
    #[derive(Default)]
    struct Book {
        ended: std::sync::Mutex<Vec<(String, String)>>,
    }
    impl ProcessBook for Book {
        fn started(&self, _p: &ProcessNote) {}
        fn ended(
            &self,
            handle: &str,
            state: letibot_tokencore::store::ProcState,
            _exit: Option<i32>,
        ) {
            self.ended
                .lock()
                .expect("book")
                .push((handle.to_string(), state.word().to_string()));
        }
    }
    let rig = Rig::new("bg-kill", &["fake1", "fake2"]);
    let host = session_host("kill");
    let book = Arc::new(Book::default());
    let answer = Fanout::new()
        .with_file(rig.hosts_file())
        .with_paths(rig.paths())
        .with_ssh(rig.ssh_program())
        .with_bound(Duration::from_secs(5))
        .with_book(book.clone())
        .answer(
            &json!({"hosts": ["fake1", "fake2"], "command": "sleep 30", "background": true}),
            Some(host.clone() as Arc<dyn ProcessHost>),
            "fanout-kill",
        );
    let handle = handle_of(&answer);
    let id = JobId(handle.clone());
    // **The pids before the kill**, so the assertion afterwards is about the processes and not
    // about the rows: a row that says `stopped` over a live branch is the claim-versus-fact
    // failure this whole feature is against.
    let jobs = rig
        .remote_root()
        .join("fanout-kill")
        .join(".letibot-remote")
        .join("jobs");
    let pids: Vec<u32> = std::fs::read_dir(&jobs)
        .expect("the job directory")
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "pid"))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    assert_eq!(pids.len(), 2, "two branches wrote a pid file: {pids:?}");
    host.kill_job(&id).expect("the handle is killable");
    // The tracker writes each branch's ending one pass later, so the settlement is the thing to
    // wait for — it is the same wait the daemon's watcher makes.
    let settled = fake_remote::until(Duration::from_secs(20), || {
        host.job(&id)
            .map(|v| !v.state.is_running())
            .unwrap_or(false)
    });
    assert!(
        settled,
        "the fanout did not settle after the kill: {}",
        journal(&host, &handle)
    );
    assert!(
        journal(&host, &handle).contains("stopped"),
        "a killed branch is a row saying so: {}",
        journal(&host, &handle)
    );
    for pid in &pids {
        if let Some(command) = pid_command(*pid) {
            assert!(
                !command.contains("sleep 30"),
                "branch pid {pid} is still running: {command}"
            );
        }
    }
    let ended = book.ended.lock().expect("book").clone();
    assert_eq!(ended.len(), 2, "one record per branch: {ended:?}");
    assert!(
        ended.iter().all(|(_, state)| state == "stopped"),
        "every branch's record says something ended it: {ended:?}"
    );
    let state = host.job(&id).expect("still listed").state;
    assert!(
        matches!(state, letibot_tools::exec::JobState::Killed { .. }),
        "the parent says who ended it, not `exited`: {state:?}"
    );
}

/// **The settlement is the table the blocking form answers with** — an unreachable host is a row
/// there too, and the rows that ran still report. This is what `jobwatch` delivers to the model.
#[test]
fn the_settlement_is_the_per_host_table_including_a_host_that_could_not_be_reached() {
    let mut rig = Rig::new("bg-settle", &["fake1"]);
    rig.unreachable();
    let host = session_host("settle");
    let answer = tool(&rig).answer(
        &json!({"hosts": ["fake1", "gone"], "command": "true", "background": true}),
        Some(host.clone() as Arc<dyn ProcessHost>),
        "fanout-settle",
    );
    let handle = handle_of(&answer);
    let id = JobId(handle.clone());
    let settled = fake_remote::until(Duration::from_secs(20), || {
        host.job(&id)
            .map(|v| !v.state.is_running())
            .unwrap_or(false)
    });
    assert!(
        settled,
        "the fanout never settled: {}",
        journal(&host, &handle)
    );
    let table = host
        .settlement_note(&id)
        .expect("a job of jobs says what it did");
    assert!(table.contains("2 planned, 2 answered"), "{table}");
    assert!(table.contains("fake1 on fake1 · exited 0"), "{table}");
    assert!(
        table.contains("gone on gone · never ran"),
        "an unreachable host is a row and not a failure of the whole: {table}"
    );
    assert!(table.contains("Could not resolve hostname"), "{table}");
    assert!(table.contains("1 of 2 cells did not exit 0"), "{table}");
    // **And the journal ends with the same table**, so a reader of `job_output` after the end sees
    // what the settlement said rather than only the lines it was built from.
    assert!(
        journal(&host, &handle).contains("fake1 on fake1 · exited 0"),
        "{}",
        journal(&host, &handle)
    );
}

/// **A session with no process host is refused before a branch starts** — the handle would settle
/// nowhere, and branches running with nobody able to reach them is worse than a refusal.
#[test]
fn a_background_fanout_with_no_process_host_is_refused_and_starts_nothing() {
    let rig = Rig::new("bg-nohost", &["fake1"]);
    let answer = tool(&rig).answer(
        &json!({"hosts": ["fake1"], "command": "sleep 30", "background": true}),
        None,
        "fanout-nohost",
    );
    // **The refusal names the missing thing in its SUBJECT** — the reason a reader acts on — and the
    // body says what to do instead.
    match &answer.outcome {
        ToolOutcome::Failed { reason } => {
            assert!(reason.contains("no process host"), "{reason}");
        }
        other => panic!("expected a refusal: {other:?} {}", answer.payload),
    }
    assert!(
        answer.payload.contains("blocks for its table"),
        "the refusal says what to do instead: {}",
        answer.payload
    );
    // **Nothing ran**: the work directory was never made on the fake remote, so there is no job
    // directory to have a pid in.
    assert!(
        !rig.remote_root().join("fanout-nohost").exists(),
        "a refused call started nothing on the host"
    );
}
