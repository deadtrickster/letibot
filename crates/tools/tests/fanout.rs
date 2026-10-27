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

use std::time::Duration;

use letibot_tools::builtins::fanout::Fanout;
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
