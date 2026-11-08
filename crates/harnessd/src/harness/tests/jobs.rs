//! Background jobs: their records, windows, completions and settlements.

use super::*;

/// The firing names no count: it fired for one row or for five, and the head is the same - the
/// number that had to agree with the rows behind it, and could not, is gone.
#[test]
fn a_firing_names_its_rows_and_no_count() {
    let due = vec![
        (
            "ship the notes".to_string(),
            "j12".to_string(),
            JobFate::Ended {
                state: "exited 0".into(),
            },
        ),
        (
            "cut the release".to_string(),
            "j13".to_string(),
            JobFate::Unknown,
        ),
    ];
    let said = due_notice(&due);
    assert!(
        said.starts_with("[todo] Due (its condition is met):\n"),
        "{said}"
    );
    assert!(
        said.contains("  - ship the notes — `j12` exited 0\n"),
        "one line per row, its own word first: {said}"
    );
    assert!(said.contains("  - cut the release — `j13`"), "{said}");
    assert!(!said.contains("2 row"), "no count: {said}");
    assert!(
        !said.contains("is due"),
        "no singular verb to disagree: {said}"
    );
}

/// A condition is met by a job that is GONE, including one this daemon never knew: the restart case. Three fates and
/// three sentences, since a handle that ended here can be read with `job_output` and one never heard of cannot, so the
/// fate is carried rather than a boolean.
#[test]
fn a_conditions_sentence_says_which_kind_of_gone_it_was() {
    assert_eq!(
        fate_line(
            "j121",
            &JobFate::Ended {
                state: "exited 0".into()
            }
        ),
        "`j121` exited 0",
        "the job's own word, and nothing invented around it"
    );
    // the sentence says what the handle is and that its result is gone, and sends the
    // reader nowhere - "read it up yourself" named no place to read.
    let unknown = fate_line("j121", &JobFate::Unknown);
    assert_eq!(
        unknown,
        "`j121`: unknown job (lost in a restart, or a wrong handle); its result is unavailable."
    );
    // A running job is not due, and the sentence exists anyway: a function that panics on the arm
    // nobody reaches is a function whose contract is a rumour, and this one is reachable the moment
    // anything but `due_todo_lines` asks.
    assert!(
        fate_line("j7", &JobFate::Running).contains("still running"),
        "the running arm must answer rather than be unreachable"
    );
}

/// The evaluator: what the world says about a handle, and which rows that makes due. Reaching it through a `Harness`
/// needs a backend, a registry, a gate and a store, so before it was a free function only `fate_line`'s wording was
/// pinned. `Unknown` must meet the condition, since after a restart the process table is empty.
#[test]
fn the_evaluator_fires_a_row_whose_handle_is_gone() {
    use letibot_tokencore::store::{TodoBy, TodoCondition, TodoItem, TodoStatus};
    /// A stored row for one handle, as a restart would find it - the ending written down
    /// before the daemon died.
    fn record(handle: &str, state: &str) -> JobRecord {
        JobRecord {
            handle: handle.into(),
            command: "cargo test".into(),
            how: "asked".into(),
            state: state.into(),
            produced: 12,
            elapsed_ms: 4_000,
            redirect: None,
        }
    }
    let row = |content: &str, when: Option<TodoCondition>| TodoItem {
        content: content.into(),
        status: TodoStatus::Pending,
        by: TodoBy::Operator,
        when,
        needs: Vec::new(),
    };
    let waiting_on = |handle: &str| {
        Some(TodoCondition::Job {
            handle: handle.into(),
        })
    };
    // The same row, set aside. Nothing else about it changes, so what the filter below is asked
    // about is the STATE and not some other difference between two fixtures.
    let set_aside = |content: &str, when: Option<TodoCondition>| TodoItem {
        status: TodoStatus::Postponed,
        ..row(content, when)
    };
    // The same row again, finished: the fourth skip, and the one `due_rows` widens rather than
    // adds, because the predicate is the idle check's and a row already done is not work it may
    // speak about.
    let finished = |content: &str, when: Option<TodoCondition>| TodoItem {
        status: TodoStatus::Completed,
        ..row(content, when)
    };

    // The order of the two looks, which is the whole of the honesty: only the process host can say
    // a job runs NOW, so a record saying `running` must not override a host that says otherwise.
    assert_eq!(
        fate_of(
            Some(letibot_tools::exec::JobState::Running),
            Some(&record("j7", "running")),
        ),
        JobFate::Running,
        "the live table is the first look, so the record cannot say otherwise"
    );
    assert_eq!(
        fate_of(
            Some(letibot_tools::exec::JobState::Exited { code: 0 }),
            Some(&record("j7", "running")),
        ),
        JobFate::Ended {
            state: "exited 0".into()
        },
        "the host's own word for it, and not the stale row's"
    );
    // The restart, which is why the record is the second look: no process table at all, and the
    // ending this daemon wrote down before it went away.
    assert_eq!(
        fate_of(None, Some(&record("j121", "exited 3"))),
        JobFate::Ended {
            state: "exited 3".into()
        },
        "the record is what survives a restart"
    );
    // And a handle nobody has run: met, and not dressed up as an ending - the distinction is
    // the operator's, since the result can be read up for one and not for the other.
    assert_eq!(
        fate_of(None, None),
        JobFate::Unknown,
        "a job this daemon never heard of has ended; that is what a restart looks like"
    );

    // The filter, over a board the world answers for. Four skips, a row with no condition, a running
    // job, a row the operator set aside and a row already done, and two firings, one of each kind of
    // gone.
    let rows = vec![
        row("ship the parity row", None),
        row("push once CI lands", waiting_on("j121")),
        row("wait for the build", waiting_on("j7")),
        row("after the restart", waiting_on("j900")),
        // A POSTPONED row does not fire, and it keeps its handle: the handle is the same one the row above is due on, so
        // a firing here could only come from the state. Lifting the postponement puts the row back in front of this same
        // filter, which is what makes the state reversible.
        set_aside("push once CI lands", waiting_on("j121")),
        // And a FINISHED row does not fire either, on the same handle: the condition is not
        // cleared, so the row keeps it, and the skip buys silence about work that is over.
        finished("push once CI lands", waiting_on("j121")),
    ];
    let due = due_rows(
        &rows,
        &letibot_tools::builtins::todo::ChildSessions::none(),
        &letibot_tools::builtins::todo::RunningJobs::none(),
        |handle| match handle {
            "j7" => JobFate::Running,
            "j121" => JobFate::Ended {
                state: "exited 0".into(),
            },
            _ => JobFate::Unknown,
        },
    );
    assert_eq!(
        due,
        vec![
            (
                "push once CI lands".to_string(),
                "j121".to_string(),
                JobFate::Ended {
                    state: "exited 0".into()
                }
            ),
            (
                "after the restart".to_string(),
                "j900".to_string(),
                JobFate::Unknown
            ),
        ],
        "a row with no condition is not due, a running job is not due, a row the operator \
             postponed is not due, a row already done is not due, and both kinds of gone are — with \
             the handle the caller needs to consume the condition: {due:?}"
    );
}

/// A subagent's ask is addressed to the tree's ROOT, never to its parent: the grandchild case decides it, since at
/// depth 1 the parent is itself a child and addressing it would post the card on a session no head can be attached to.
/// The fixture is a root, a child and a grandchild.
#[test]
fn a_subagents_ask_is_addressed_to_the_tree_root_and_not_to_its_parent() {
    use letibot_sessionlog::registry::{Registry, SessionWiring};

    let registry = Registry::new();
    // The root is the session a daemon drives; the child and the grandchild are the ones
    // it does not. All three are registered, which is what `adopt` does at a spawn.
    let root_hub = registry
        .create("s-root", "", SessionWiring::default())
        .expect("the root registers");
    let _child_hub = registry
        .create_under(
            "s-child",
            "",
            SessionWiring::default(),
            Some("s-root".into()),
        )
        .expect("the child registers");
    let grandchild_hub = registry
        .create_under(
            "s-child-sub-1",
            "",
            SessionWiring::default(),
            Some("s-child".into()),
        )
        .expect("the grandchild registers");

    // The answers the whole tree reaches: the root's own, handed down through `Parts`.
    let answers = Some(Arc::new(crate::answers::Answers::new()));
    // The grandchild's set is built FROM the root's, which is what gives it the ROOT  -
    // the same set `Parts` carries for a grandchild.
    let root_watch = crate::jobwatch::JobWatchers::watching_tasks(&root_hub, None);
    let grandchild_watch = crate::jobwatch::JobWatchers::watching_tasks(&grandchild_hub, None)
        .shares_tree(&root_watch);

    let (root, head) = subagent_ask_target(
        &registry,
        Some(&grandchild_watch),
        "s-child",
        answers.as_ref(),
    );
    assert_eq!(
        root, "s-root",
        "the card was addressed to the parent, not to the tree's root"
    );
    assert!(
        head.is_some(),
        "the root's answers did not reach the child's ask"
    );

    // A tree with no head-wired adjudicator: no card is posted, and the child's own
    // refusal names the root instead (see `answers::SubagentAdjudicator`).
    let (root, head) = subagent_ask_target(&registry, Some(&grandchild_watch), "s-child", None);
    assert_eq!(root, "s-root");
    assert!(head.is_none());

    // A root this daemon does not hold - a harness with no daemon behind it, or a
    // registry that has lost the session - is the same refusal rather than a card on a
    // hub nothing will read. With no watcher set at all, the parent IS the tree's root.
    let (root, head) = subagent_ask_target(&registry, None, "s-absent", answers.as_ref());
    assert_eq!(root, "s-absent");
    assert!(head.is_none(), "a hub nobody holds cannot answer");
}

/// The window of a REDIRECTED job is the file's tail: `offset == 0` is the tail, because that is what a fresh read of
/// an output pane means, and any other offset is a forward window so a walk to the end of a long log still works.
#[test]
fn a_redirected_jobs_window_is_the_tail_of_its_file() {
    let dir = std::env::temp_dir().join(format!("letibot-tail-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch");
    let path = dir.join("build.log");
    let body: String = (0..40).map(|i| format!("line {i}\n")).collect();
    std::fs::write(&path, &body).expect("write");
    let path = path.to_str().expect("utf-8 path");

    // A fresh read is the tail: the last `limit` bytes, and the pane is told the
    // FILE's length so its `of N` is the file and not the window.
    let w = tail_file(path, 0, 20).expect("a window");
    assert_eq!(w.produced, body.len() as u64);
    assert_eq!(
        w.to,
        body.len() as u64,
        "the tail ends at the end of the file"
    );
    assert_eq!(w.from, body.len() as u64 - 20);
    assert_eq!(w.text.len(), 20);
    assert!(w.text.ends_with("line 39\n"), "{:?}", w.text);

    // An explicit offset is a forward window, which is what `next` hands to `→`.
    let w2 = tail_file(path, w.from, 20).expect("a window");
    assert_eq!(w2.from, w.from);
    assert_eq!(w2.to, w.from + 20);

    // A limit larger than the file is the whole file, not an error and not padding.
    let all = tail_file(path, 0, 4096).expect("a window");
    assert_eq!(all.from, 0);
    assert_eq!(all.to, body.len() as u64);

    // A file that is not there is `None` - the job has not created it yet - which is
    // what the caller answers with the sentence that names the path.
    assert!(tail_file(&dir.join("nope.log").to_string_lossy(), 0, 10).is_none());

    let _ = std::fs::remove_dir_all(&dir);
}

// The fixtures for the window tests below: real jobs, really run. The decision they hold to is `job_window_for`, and
// the honest way to exercise it is with a command a real `/bin/sh` ran, a capture ring that holds what the preamble
// wrote, and a file the stage created.
mod job_tail {
    use super::*;
    use letibot_tools::exec::{HostProcesses, ScopeKind, SpawnRequest};
    use std::time::Duration;

    /// Run `command` to its end and hand back the three things the window is decided on: the command, its state and its
    /// capture. A job that does not exit 0 fails here, so a broken fixture never reaches the assertions as a mystery.
    fn a_settled_job(
        host: &HostProcesses,
        command: &str,
    ) -> (
        String,
        letibot_tools::exec::JobState,
        letibot_tools::exec::OutputSlice,
    ) {
        let id = host
            .spawn(&SpawnRequest {
                command: command.to_string(),
                // Nobody named this run: the test's own fixture.
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
            .expect("spawn");
        match host.wait_job(&id, Duration::from_secs(30)) {
            Ok(letibot_tools::exec::Waited::Happened {
                state: Some(letibot_tools::exec::JobState::Exited { code: 0 }),
                ..
            }) => {}
            other => panic!("the fixture did not run clean: {other:?}"),
        }
        let view = host.job(&id).expect("the job is still in the table");
        let slice = host
            .output(&id, 0, JOB_OUTPUT_WINDOW)
            .expect("the capture reads");
        (view.command, view.state, slice)
    }

    /// A host over a temp root, or nothing when this box cannot delegate a cgroup v2 subtree: CI's runner is such a box,
    /// where the harness's own cgroup is not writable. A `#[ignore]` would be a green suite that measured nothing, so the
    /// absence is SAID and `LETIBOT_REQUIRE_APPARATUS=1` refuses it.
    fn a_host(name: &str) -> Option<(std::path::PathBuf, std::sync::Arc<HostProcesses>)> {
        letibot_tokencore::apparatus::present(
            "a process-lifetime tree (cgroup v2; process groups on macOS)",
            letibot_tools::host_tree().is_ok(),
        )?;
        let root = std::env::temp_dir().join(format!(
            "letibot-jobtail-{name}-{pid}",
            pid = std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("scratch");
        let host = HostProcesses::new(&root)
            .expect("the tree probed writable above, so this root builds one");
        Some((root, std::sync::Arc::new(host)))
    }

    /// The measured defect: a compound command whose preamble writes to the capture and whose work redirects to a file.
    /// The window must answer with the FILE's tail whatever the capture holds, and the capture's bytes must be counted,
    /// not hidden: the fresh read says how many are outside the window and which file the window is.
    #[test]
    fn a_preamble_in_the_capture_does_not_stop_the_window_being_the_file() {
        let Some((root, host)) = a_host("compound") else {
            return;
        };
        let log = root.join("work.log");
        let command = format!(
            "sleep 0.1; echo '=== MIX (load + validate + mix) ==='; \
                 printf 'work-a\\nwork-b\\n' > {} 2>&1",
            log.display()
        );
        let (command, state, slice) = a_settled_job(&host, &command);

        // The fixture is the measurement or it is nothing: the capture holds the
        // preamble - bytes, produced > 0 - and not one byte of the work.
        assert!(slice.produced > 0, "j87's shape needs a non-empty capture");
        assert!(slice.text().contains("=== MIX"));
        assert!(
            !slice.text().contains("work-a"),
            "the work must be in the file, not the capture: {:?}",
            slice.text()
        );

        let w = job_window_for(&command, &state, &slice, 0, JOB_OUTPUT_WINDOW);
        let file_len = std::fs::metadata(&log).expect("the work ran").len();
        assert_eq!(w.produced, file_len, "the denominator is the FILE's length");
        assert_eq!(w.from, 0);
        assert_eq!(w.to, file_len);
        let drawn = w.lines.join("\n");
        assert!(
            drawn.contains("work-b"),
            "the file's tail is the window: {drawn}"
        );
        assert!(
            !drawn.contains("=== MIX"),
            "the preamble is not the window any more: {drawn}"
        );
        // The preamble is disclosed, not silently dropped. The line names the
        // count that stayed in the capture and the file the window is over.
        assert!(
            drawn.contains(&slice.produced.to_string()) && drawn.contains("work.log"),
            "the fresh read must count what it is not showing: {drawn}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// j70's case must not regress: a redirect whose capture is empty - the
    /// `produced == 0` the old condition required - still tails the file, and the
    /// window gains no phantom disclosure about bytes that were never there.
    #[test]
    fn a_redirect_with_an_empty_capture_still_tails_and_claims_no_preamble() {
        let Some((root, host)) = a_host("empty-capture") else {
            return;
        };
        let log = root.join("solo.log");
        let command = format!("printf 'solo\\n' > {} 2>&1", log.display());
        let (command, state, slice) = a_settled_job(&host, &command);

        assert_eq!(slice.produced, 0, "the fixture needs the empty capture");
        let w = job_window_for(&command, &state, &slice, 0, JOB_OUTPUT_WINDOW);
        assert_eq!(w.produced, 5);
        // The file's own lines, and nothing bolted on: no phantom disclosure
        // about a preamble that was never there.
        let body = std::fs::read_to_string(&log).expect("the work ran");
        assert_eq!(
            w.lines,
            progress_lines(&body),
            "the file, and only the file"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A `2>&1` merge is not a redirect - it is what makes the capture complete,
    /// so the window is the capture. The parser guarantees it; this holds the
    /// window path to the same guarantee end to end.
    #[test]
    fn a_merged_descriptor_keeps_the_capture_as_the_window() {
        let Some((root, host)) = a_host("merge") else {
            return;
        };
        let (command, state, slice) = a_settled_job(&host, "echo merged 2>&1");
        assert!(slice.produced > 0);

        let w = job_window_for(&command, &state, &slice, 0, JOB_OUTPUT_WINDOW);
        assert_eq!(
            w.produced, slice.produced,
            "the denominator is the capture's"
        );
        assert_eq!(w.lines, progress_lines(&slice.text()));
        assert!(w.lines.join("\n").contains("merged"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A running job's output is readable by the off-worker source, and grows as it does: the operator's session showed
    /// 1.8 KB written and Enter drew nothing, because the read was queued behind the turn. It must read a job that has
    /// not settled, and a handle the host never held is `None`, which sends the read to the worker and the store.
    #[test]
    fn a_running_jobs_window_is_read_live_and_grows() {
        let Some((root, host)) = a_host("running") else {
            return;
        };
        let gate = root.join("go");
        let command = format!(
            "echo first; while [ ! -e {g} ]; do sleep 0.05; done; echo second; sleep 30",
            g = gate.display()
        );
        let id = host
            .spawn(&SpawnRequest {
                command,
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
            .expect("spawn");
        let read = |want: &str| -> SessionEvent {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            loop {
                let e = live_job_output(host.as_ref(), &id.0, 0, JOB_OUTPUT_WINDOW)
                    .expect("the host holds a running job");
                if let SessionEvent::JobOutput { lines, .. } = &e
                    && lines.iter().any(|l| l.contains(want))
                {
                    return e;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "never saw {want:?}: {e:?}"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        let SessionEvent::JobOutput {
            job, state, lines, ..
        } = read("first")
        else {
            unreachable!()
        };
        assert_eq!(job, id.0);
        assert_eq!(state, "running", "read while the job runs");
        assert!(!lines.iter().any(|l| l.contains("second")));
        std::fs::write(&gate, b"").expect("open the gate");
        let SessionEvent::JobOutput { state, .. } = read("second") else {
            unreachable!()
        };
        assert_eq!(state, "running", "still running, and the window grew");
        assert!(
            live_job_output(host.as_ref(), "j-never", 0, JOB_OUTPUT_WINDOW).is_none(),
            "a handle the host does not hold is the worker's to answer"
        );
        let _ = host.kill_job(&id);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A job with no redirect is unaffected: the capture is the window, exactly
    /// as before - the redirect branch must not have widened into this case.
    #[test]
    fn a_job_with_no_redirect_keeps_the_capture_as_the_window() {
        let Some((root, host)) = a_host("plain") else {
            return;
        };
        let (command, state, slice) = a_settled_job(&host, "echo plain");
        assert!(slice.produced > 0);

        let w = job_window_for(&command, &state, &slice, 0, JOB_OUTPUT_WINDOW);
        assert_eq!(w.produced, slice.produced);
        assert_eq!(w.lines, progress_lines(&slice.text()));
        assert!(w.lines.join("\n").contains("plain"));
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// The name reaches the transcript, and a job nobody named reads exactly as it did. The bullet carries `job_label`'s
/// spelling, the same string `bash` printed when the job was started, and the unnamed half is a regression test rather
/// than a courtesy: the label's fallback IS the id, so those notices differ in the name and nowhere else.
#[test]
fn a_settlement_carries_the_jobs_name_and_an_unnamed_one_is_unchanged() {
    let settled = |job: &str, slug: Option<&str>| JobCompletion {
        arrived: std::time::SystemTime::UNIX_EPOCH,
        later: false,
        kind: BackgroundKind::Job,
        owner: "s-me".into(),
        job: job.into(),
        command: "cargo test".into(),
        slug: slug.map(str::to_string),
        state: "exited 0".into(),
        produced: 1_030,
        elapsed_ms: 54_900,
        detail: String::new(),
        memory: None,
    };

    let named = completion_notice(&[settled("j65", Some("release-build"))], true);
    assert!(
        named.contains("  - `release-build · j65` exited 0 after 54.9s\n"),
        "the settlement must name the job the way the bash result did: {named}"
    );

    // The handed-up variant carries the same name. One bullet builder, so this is the
    // same sentence under the other heading - and the owner is still named beside it.
    let handed = completion_notice(&[settled("j65", Some("release-build"))], false);
    assert!(
        handed
            .contains("`release-build · j65` exited 0 after 54.9s: cargo test (started by `s-me`)"),
        "{handed}"
    );
    // the closing is one clause naming the starter, not a paragraph about the ending.
    assert!(
        handed.ends_with("(Started by `s-me`, which has stopped; its output is gone.)"),
        "{handed}"
    );

    let bare = completion_notice(&[settled("j65", None)], true);
    assert!(
        bare.contains("  - `j65` exited 0 after 54.9s\n") && !bare.contains("cargo test"),
        "{bare}"
    );
    // The name is the only difference. Strip it from the named notice and what is left
    // is the unnamed one, byte for byte - so "nothing regresses for those who do not use it"
    // is asserted rather than argued.
    assert_eq!(named.replace("release-build · ", ""), bare);
}

/// A settlement that carries more than a row: a `fanout {background: true}` settles as a job and what the model waits
/// for is the per-host table, which `detail` carries. The rows are indented under the bullet that names the job, so a
/// reader cannot mistake them for a second settlement. An ordinary job's `detail` is empty.
#[test]
fn a_settlement_carries_what_a_job_of_jobs_says_for_itself() {
    let table = "fanout · 2 planned, 2 answered · 1.2s · work dir `fanout-c1` on each host\n  \
                 fake1 on fake1 · exited 0 · 0.3s\n  fake2 on fake2 · exited 0 · 0.9s\n\
                 every cell exited 0";
    let one = JobCompletion {
        arrived: std::time::SystemTime::UNIX_EPOCH,
        later: false,
        kind: BackgroundKind::Job,
        owner: "s-me".into(),
        job: "j90".into(),
        command: "fanout: 2 branch(es) on fake1, fake2".into(),
        slug: Some("fanout".into()),
        state: "exited 0".into(),
        produced: 128,
        elapsed_ms: 1_200,
        detail: table.to_string(),
        memory: None,
    };
    let text = completion_notice(&[one], true);
    assert!(
        text.contains("  - `fanout · j90` exited 0 after 1.2s\n"),
        "the row is still one row: {text}"
    );
    for line in table.lines() {
        assert!(
            text.contains(&format!("    {line}\n")),
            "`{line}` did not reach the notice, indented under its row: {text}"
        );
    }
    // The closing is still the door, and it is still where a reader goes for more.
    assert!(text.contains(JOB_CLOSING), "{text}");
}

/// A killed job's result is what it wrote before the harness ended it. A settlement that said only
/// `killed by …` reads as a finish, which is the report R160 filed: the notice carries the partial
/// sentence, and a job that finished carries nothing.
#[test]
fn a_killed_jobs_settlement_says_its_output_is_partial() {
    let settled = |state: &str| JobCompletion {
        arrived: std::time::SystemTime::UNIX_EPOCH,
        later: false,
        kind: BackgroundKind::Job,
        owner: "s-me".into(),
        job: "j83627".into(),
        command: "cargo test".into(),
        slug: None,
        state: state.into(),
        produced: 4_096,
        elapsed_ms: 600_000,
        detail: String::new(),
        memory: None,
    };
    for (state, why) in [
        ("killed by its deadline", "a deadline"),
        (
            "killed by the dead check — no output and no CPU progress",
            "the dead check",
        ),
    ] {
        let text = completion_notice(&[settled(state)], true);
        assert!(
            text.contains(&format!("  - `j83627` {state} after 10m00s\n")),
            "the row still says how it ended: {text}"
        );
        assert!(
            text.contains("its output is partial"),
            "{why} ended this job, and the model is told the window is not a finish: {text}"
        );
        assert!(text.contains("job_extend"), "{text}");
    }
    // And the sentence is absent from every settlement that is a finish: a notice that said it
    // over an `exited 0` would teach a model to distrust a whole class of results.
    for finish in [
        "exited 0",
        "exited 1",
        "signalled 15",
        "gone (no exit was recorded)",
    ] {
        let text = completion_notice(&[settled(finish)], true);
        assert!(
            !text.contains("its output is partial"),
            "`{finish}` is a finish: {text}"
        );
    }
}

/// The notice, byte for byte, as the TUI's compact card parses it: the notices are written to fixtures its tests parse
/// and compared here, so a change to the sentence fails HERE, naming the fixture. Re-bless with
/// `LETIBOT_BLESS_JOB_NOTICE=1`, then teach the TUI's parser the new shape.
#[test]
fn the_tuis_job_notice_fixtures_are_what_completion_notice_writes() {
    let job = |id: &str,
               slug: Option<&str>,
               command: &str,
               state: &str,
               produced: u64,
               ms: u64,
               memory: Option<&str>,
               owner: &str| JobCompletion {
        arrived: std::time::SystemTime::UNIX_EPOCH,
        later: false,
        kind: BackgroundKind::Job,
        owner: owner.into(),
        job: id.into(),
        command: command.into(),
        slug: slug.map(str::to_string),
        state: state.into(),
        produced,
        elapsed_ms: ms,
        detail: String::new(),
        memory: memory.map(str::to_string),
    };
    let mine = completion_notice(
        &[
            job(
                "j1042",
                Some("build-and-test-null"),
                "bash -c 'cd /home/dead/Projects/dbtest/pg-noop; echo \"=== building release\"\n  cargo build --release 2>&1 | tail -3\n\n  echo \"=== testing\"; cargo test 2>&1 | tail -5'",
                "exited 0",
                1008,
                174_000,
                Some("peak 245 MiB of a 8 GiB cap"),
                "s-me",
            ),
            job(
                "j1043",
                None,
                "cargo test -p letibot-tui",
                "exited 101",
                0,
                4_400,
                None,
                "s-me",
            ),
        ],
        true,
    );
    let theirs = completion_notice(
        &[job(
            "j7",
            Some("release-build"),
            "sleep 3; echo done",
            "killed by job_kill",
            13,
            3_000,
            None,
            "s-child",
        )],
        false,
    );
    let dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tui/src/app/tests/fixtures");
    for (name, text) in [
        ("job_notice_mine.txt", &mine),
        ("job_notice_theirs.txt", &theirs),
    ] {
        let path = dir.join(name);
        if std::env::var_os("LETIBOT_BLESS_JOB_NOTICE").is_some() {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(&path, text).unwrap();
        }
        let pinned =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(
            text,
            &pinned,
            "`completion_notice` no longer writes what the TUI's compact job card parses ({}); \
             re-bless the fixture and teach `job_card::ended_jobs` the new shape",
            path.display()
        );
    }
}

/// A job whose view was already reaped still produces a usable notice: the command
/// is the one field the durable settlement does not carry, so it is the one that can
/// be missing, and the notice says so rather than inventing one.
#[test]
fn a_reaped_jobs_completion_says_the_command_was_not_recorded() {
    let text = completion_notice(
        &[JobCompletion {
            arrived: std::time::SystemTime::UNIX_EPOCH,
            later: false,
            kind: BackgroundKind::Job,
            owner: "s-me".into(),
            job: "j9".into(),
            command: String::new(),
            // And the name is gone with it. A reaped job has no view left to read a
            // slug off, so this is the other field that can be missing - and the notice
            // draws the id, which is the label's own fallback rather than a second rule.
            slug: None,
            state: "gone".into(),
            produced: 0,
            elapsed_ms: 0,
            detail: String::new(),
            memory: None,
        }],
        // the command is echoed only for a job another session started, so that is
        // where a missing one is said.
        false,
    );
    assert!(text.contains("command not recorded"), "{text}");
    assert!(!text.contains("``"), "an empty pair of backticks: {text}");
}

/// The child's sentence, which had no test, and it is the one the model reads: same channel and same wake as a job's,
/// and the text is the deliverable. Two functions rather than a flag, since the verb a child is read with is
/// `task_result`, and a child's completion carries an answer where a job's carries output.
#[test]
fn a_childs_completion_names_the_child_and_points_at_task_result() {
    let one = JobCompletion {
        arrived: std::time::SystemTime::UNIX_EPOCH,
        later: false,
        kind: BackgroundKind::Subagent,
        owner: "s-me".into(),
        job: "s-1-sub-1".into(),
        // A subagent runs no command, and the field must not quietly hold an answer.
        command: String::new(),
        // A child from a runner that keeps no names: its handle alone, as every notice from
        // before names said it.
        slug: None,
        state: "done".into(),
        produced: 0,
        elapsed_ms: 0,
        detail: "two facts off a fixture".into(),
        memory: None,
    };
    let text = subagent_notice(std::slice::from_ref(&one), true);
    // The label, and it is what tells this row from a job's: one channel, two nouns.
    assert!(
        text.starts_with("[task]"),
        "labelled like a monitor: {text}"
    );
    assert!(
        text.contains("a subagent you started has finished"),
        "{text}"
    );
    assert!(text.contains("`s-1-sub-1`"), "{text}");
    assert!(text.contains("done"), "{text}");
    // What the child SAID. Without this the notice is a doorbell with no answer behind it:
    // the parent is told something happened and must still go and read.
    assert!(text.contains("two facts off a fixture"), "{text}");
    // one instruction, with the real handle - not "don't call `task_result` … call it".
    assert!(
        text.ends_with("Full answer: `task_result` task=\"s-1-sub-1\"."),
        "{text}"
    );
    assert!(
        !text.contains("job_output"),
        "a subagent is read with `task_result`; sending the parent to `job_output` is a door \
             that does not open: {text}"
    );

    // A child that said nothing gets no empty field. The job side guards an empty command
    // (`` `` ``); the field that can be empty here is `detail`, and the shape it would leave is
    // a line ending in a colon with nothing after it.
    let bare = subagent_notice(
        &[JobCompletion {
            detail: String::new(),
            ..one.clone()
        }],
        true,
    );
    assert!(
        !bare.contains("done:"),
        "an empty detail drew a colon and nothing after it: {bare}"
    );
    assert!(bare.contains("`s-1-sub-1` done"), "{bare}");

    // Two children are one notice, two lines, and the plural sentence - a round that spawned
    // a pair reports a pair rather than making the reader count the list themselves.
    let pair = subagent_notice(
        &[
            one.clone(),
            JobCompletion {
                job: "s-1-sub-2".into(),
                ..one.clone()
            },
        ],
        true,
    );
    assert!(
        pair.contains("2 subagents you started have finished"),
        "{pair}"
    );
    assert!(
        pair.contains("s-1-sub-1") && pair.contains("s-1-sub-2"),
        "{pair}"
    );

    // A named child is named - `fix-login · s-1-sub-1`, the job's spelling - while the
    // closing still hands the model the real handle to collect it by.
    let named = subagent_notice(
        &[JobCompletion {
            slug: Some("fix-login".into()),
            ..one.clone()
        }],
        true,
    );
    assert!(
        named.contains("  - `fix-login · s-1-sub-1` done: two facts off a fixture\n"),
        "{named}"
    );
    assert!(
        named.ends_with("Full answer: `task_result` task=\"s-1-sub-1\"."),
        "{named}"
    );
}

/// The wake's one decision, asserted: which sentence for which kind, and none for nothing. Everything else `wake` does
/// is take the queue and submit the text, and it cannot be asserted itself, since it needs a live `Harness` that this
/// crate's tests build only where a vocabulary and a store are present.
#[test]
fn a_settlement_is_two_nouns_out_of_one_channel() {
    let job = |id: &str| JobCompletion {
        arrived: std::time::SystemTime::UNIX_EPOCH,
        later: false,
        kind: BackgroundKind::Job,
        owner: "s-me".into(),
        job: id.into(),
        command: "cargo test".into(),
        slug: None,
        state: "exited 0".into(),
        produced: 12,
        elapsed_ms: 900,
        detail: String::new(),
        memory: None,
    };
    let child = |id: &str| JobCompletion {
        arrived: std::time::SystemTime::UNIX_EPOCH,
        later: false,
        kind: BackgroundKind::Subagent,
        owner: "s-me".into(),
        job: id.into(),
        command: String::new(),
        slug: None,
        state: "done".into(),
        produced: 0,
        elapsed_ms: 0,
        detail: "off a fixture".into(),
        memory: None,
    };

    // Nothing settled says nothing, which is what makes a spurious wake cost no generation.
    assert!(completion_notices(Vec::new(), "s-me", &[]).is_empty());

    // The two nouns are the point. A reader told `[job]` about a child goes looking for it
    // with `job_output` - which is the wrong door, and is what a hand measurement had to find
    // because nothing asserted this.
    let only_child = completion_notices(vec![child("s-1-sub-1")], "s-me", &[]);
    assert_eq!(only_child.len(), 1, "{only_child:?}");
    assert!(only_child[0].starts_with("[task]"), "{:?}", only_child[0]);
    assert!(only_child[0].contains("task_result"), "{:?}", only_child[0]);
    let only_job = completion_notices(vec![job("j7")], "s-me", &[]);
    assert_eq!(only_job.len(), 1, "{only_job:?}");
    assert!(only_job[0].starts_with("[job]"), "{:?}", only_job[0]);
    assert!(only_job[0].contains("job_output"), "{:?}", only_job[0]);

    // Both kinds are one wake and two sentences, job first - the order the old inline code
    // produced, kept so the record of a session reads the same way it did.
    let both = completion_notices(vec![job("j7"), child("s-1-sub-1")], "s-me", &[]);
    assert_eq!(both.len(), 2, "{both:?}");
    assert!(both[0].starts_with("[job]"), "{:?}", both[0]);
    assert!(both[1].starts_with("[task]"), "{:?}", both[1]);

    // Two children are one sentence, not two - the plural arm of the child's own wording,
    // asserted through the partition so the two functions cannot drift apart.
    let pair = completion_notices(vec![child("s-1-sub-1"), child("s-1-sub-2")], "s-me", &[]);
    assert_eq!(pair.len(), 1, "{pair:?}");
    assert!(
        pair[0].contains("2 subagents you started have finished"),
        "{:?}",
        pair[0]
    );
}

/// A later turn of a child the model already has news of is its own sentence - the same child,
/// settling a second time. A group headed *has finished* over it would be false: it finished once.
#[test]
fn a_later_turn_is_its_own_sentence_and_not_a_second_finish() {
    let one = JobCompletion {
        arrived: std::time::SystemTime::UNIX_EPOCH,
        later: true,
        kind: BackgroundKind::Subagent,
        owner: "s-me".into(),
        job: "s-1-sub-1".into(),
        command: String::new(),
        slug: Some("fix-login".into()),
        state: "done".into(),
        produced: 0,
        elapsed_ms: 0,
        detail: "the second half".into(),
        memory: None,
    };
    let said = completion_notices(vec![one.clone()], "s-me", &[]);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(
        said[0].starts_with("[task] a subagent you started has answered again on a later turn:"),
        "{:?}",
        said[0]
    );
    assert!(
        said[0].contains("  - `fix-login · s-1-sub-1` answered again: the second half\n"),
        "the line names the child and carries the new answer: {:?}",
        said[0]
    );
    assert!(
        !said[0].contains("has finished"),
        "the child finished once, at its first answer: {:?}",
        said[0]
    );
    assert!(
        said[0].ends_with("Full answer: `task_result` task=\"s-1-sub-1\"."),
        "{:?}",
        said[0]
    );

    // Two of them are one sentence and the plural, like every other group here.
    let pair = completion_notices(
        vec![
            one.clone(),
            JobCompletion {
                job: "s-1-sub-2".into(),
                ..one
            },
        ],
        "s-me",
        &[],
    );
    assert_eq!(pair.len(), 1, "{pair:?}");
    assert!(
        pair[0].contains("2 subagents you started have answered again on later turns:"),
        "{:?}",
        pair[0]
    );

    // And a later turn handed UP from a session below is not one this session started - the
    // same rule the finish sentence keeps, which this one must keep by accounting for
    // whose batch it is reading.
    let theirs = JobCompletion {
        arrived: std::time::SystemTime::UNIX_EPOCH,
        later: true,
        kind: BackgroundKind::Subagent,
        owner: "s-child".into(),
        job: "s-1-sub-1".into(),
        command: String::new(),
        slug: Some("fix-login".into()),
        state: "done".into(),
        produced: 0,
        elapsed_ms: 0,
        detail: "the second half".into(),
        memory: None,
    };
    let said = completion_notices(vec![theirs], "s-me", &[]);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(
        !said[0].contains("you started"),
        "a child of a session below was announced as this session's own: {:?}",
        said[0]
    );
    assert!(
        said[0].contains("a session below this one started"),
        "{:?}",
        said[0]
    );
}

/// A settlement handed up says WHOSE it was, and points at no door: `stop` hands a stopped session's undrained
/// settlements to the level above, and a notice saying you backgrounded about a job the reader cannot open is false
/// twice, and one promising `job_output` sends a model to a door that does not open.
#[test]
fn a_handed_up_settlement_names_its_owner_and_sends_nobody_to_a_shut_door() {
    let job = JobCompletion {
        arrived: std::time::SystemTime::UNIX_EPOCH,
        later: false,
        kind: BackgroundKind::Job,
        owner: "s-child".into(),
        job: "j17".into(),
        command: "sleep 25; echo done".into(),
        slug: None,
        state: "exited 0".into(),
        produced: 5,
        elapsed_ms: 25_000,
        detail: String::new(),
        memory: None,
    };

    // The level it was handed up to - the ROOT - is not the session that started it.
    let theirs = completion_notices(vec![job.clone()], "s-root", &[]);
    assert_eq!(theirs.len(), 1, "{theirs:?}");
    assert!(
        !theirs[0].contains("you backgrounded"),
        "the one sentence that must never be said about somebody else's job: {:?}",
        theirs[0]
    );
    assert!(
        theirs[0].contains("a job a session below this one started has ended"),
        "{:?}",
        theirs[0]
    );
    assert!(
        theirs[0].contains("started by `s-child`"),
        "the owner is named on the fact line: {:?}",
        theirs[0]
    );
    assert!(
        !theirs[0].contains("job_output"),
        "its host went with the session that started it, so `job_output` here is a door \
             that does not open: {:?}",
        theirs[0]
    );

    // The same settlement, read by the session that started it - which is the
    // sentence that was right all along and must not have moved.
    let mine = completion_notices(vec![job.clone()], "s-child", &[]);
    assert!(
        mine[0].contains("a job you backgrounded has ended"),
        "{:?}",
        mine[0]
    );
    assert!(mine[0].contains("job_output"), "{:?}", mine[0]);
    assert!(!mine[0].contains("started by"), "{:?}", mine[0]);

    // Its own come first, so one drain reads as this session's own news before what
    // it is being told on somebody else's behalf - two owners, two notices, that order.
    let mine_job = JobCompletion {
        owner: "s-root".into(),
        job: "j3".into(),
        ..job.clone()
    };
    let both = completion_notices(vec![job, mine_job], "s-root", &[]);
    assert_eq!(both.len(), 2, "{both:?}");
    assert!(
        both[0].contains("a job you backgrounded has ended") && both[0].contains("`j3`"),
        "{:?}",
        both[0]
    );
    assert!(
        both[1].contains("started by `s-child`") && both[1].contains("`j17`"),
        "{:?}",
        both[1]
    );

    // A handed-up GRANDCHILD keeps its own door: `task_result` reads the tree's handle
    // list (`Parts::tree_slots`), so unlike a dead job's output the answer is still there.
    let child = JobCompletion {
        arrived: std::time::SystemTime::UNIX_EPOCH,
        later: false,
        kind: BackgroundKind::Subagent,
        owner: "s-child".into(),
        job: "s-child-sub-1".into(),
        command: String::new(),
        slug: None,
        state: "done".into(),
        produced: 0,
        elapsed_ms: 0,
        detail: "the grandchild answered".into(),
        memory: None,
    };
    let handed = completion_notices(vec![child], "s-root", &[]);
    assert_eq!(handed.len(), 1, "{handed:?}");
    assert!(
        handed[0].contains("a subagent a session below this one started has finished"),
        "{:?}",
        handed[0]
    );
    assert!(
        handed[0].contains("started by `s-child`"),
        "{:?}",
        handed[0]
    );
    assert!(
        handed[0].contains("the grandchild answered"),
        "{:?}",
        handed[0]
    );
    assert!(
        handed[0].contains("task_result"),
        "the handle is in this tree, so this door does open: {:?}",
        handed[0]
    );
    // one clause naming the starter, and the door to the answer in it.
    assert!(
        handed[0].ends_with(
            "(Started by `s-child`, which has stopped; its answer is still here: \
             `task_result` task=\"s-child-sub-1\".)"
        ),
        "{:?}",
        handed[0]
    );
}

/// A finished foreground command is not a job anybody acts on: the host gives every command a job id, so `/job` after
/// a turn listed one line per grep, cat and sed of the whole session. Measured in the operator's store: of 1,210 tool
/// results, exactly 2 were backgrounded.
#[test]
fn only_background_and_still_running_jobs_are_listed() {
    use letibot_tools::exec::{JobId, JobState, JobView};
    use letibot_tools::exec::{ScopeId, ScopeKind};
    let scope = || ScopeId {
        kind: ScopeKind::Session,
        name: "s".into(),
        path: std::path::PathBuf::from("/sys/fs/cgroup/x"),
    };
    let job = |background, state| JobView {
        id: JobId("j1".into()),
        command: "grep -n x y".into(),
        // Nobody named this one - which is the premise of every case below.
        slug: None,
        scope: scope(),
        owner: scope(),
        cwd: "/".into(),
        pid: 1,
        background,
        state,
        elapsed: std::time::Duration::from_secs(1),
        ran_for: Some(std::time::Duration::from_secs(1)),
        produced: 91,
        since_last_output: None,
        memory_cap: letibot_tools::exec::MemoryCap::NotAsked,
        memory: None,
        output_file: None,
        env_report: None,
    };

    // The case that flooded the screen: finished, never backgrounded.
    assert!(!Harness::worth_listing(&job(
        None,
        JobState::Exited { code: 0 }
    )));
    // A failure is still not a job - the tool result already carried it.
    assert!(!Harness::worth_listing(&job(
        None,
        JobState::Exited { code: 1 }
    )));
    // Still running, never backgrounded: ctrl-o moves this one, so it stays.
    assert!(Harness::worth_listing(&job(None, JobState::Running)));
    // Backgrounded, however it got there, finished or not.
    for how in [
        letibot_transcript::Backgrounding::Asked,
        letibot_transcript::Backgrounding::Promoted,
    ] {
        assert!(Harness::worth_listing(&job(
            Some(how.clone()),
            JobState::Running
        )));
        assert!(Harness::worth_listing(&job(
            Some(how),
            JobState::Exited { code: 0 }
        )));
    }
}

/// A row is one line, whatever the model wrote: a heredoc carries newlines in the command text, and a cut to 60
/// characters kept them, so one row drew as two and a screen of them was unreadable.
#[test]
fn a_job_row_is_one_line_however_many_the_command_had() {
    let heredoc = "cd /home/dead/Projects/leticl && python3 - <<'PY'\ns = open(\"x\")\nPY";
    let line = Harness::one_line(heredoc, 60);
    assert!(!line.contains('\n'), "a row that draws as two: {line:?}");
    // 60 characters plus the ellipsis that says something was cut.
    assert_eq!(line.chars().count(), 61, "{line:?}");
    assert!(line.ends_with('…'), "{line:?}");
    assert!(
        line.starts_with("cd /home/dead/Projects/leticl && python3"),
        "{line:?}"
    );

    // A short command is left exactly as it is - no ellipsis promising more.
    assert_eq!(Harness::one_line("git status", 60), "git status");
    // Internal runs of whitespace collapse, so the width asked for is the
    // width drawn.
    assert_eq!(Harness::one_line("ls   -la\t-h", 60), "ls -la -h");
}

/// A named job draws its name beside its id, and an unnamed one draws what it drew before: `j57` is a counter, so the
/// operator watching the pane could not tell which running job was the release build. The name is the agent's own
/// stated intent, so the absence of one has to be exactly the old row.
#[test]
fn a_named_job_draws_its_name_beside_its_id_and_an_unnamed_one_does_not() {
    use letibot_tools::exec::{JobId, JobState, JobView, ScopeId, ScopeKind};
    let scope = || ScopeId {
        kind: ScopeKind::Session,
        name: "s".into(),
        path: std::path::PathBuf::from("/sys/fs/cgroup/x"),
    };
    let view = |slug: Option<&str>| JobView {
        id: JobId("j65".into()),
        command: "cargo build --release".into(),
        slug: slug.map(|s| s.to_string()),
        scope: scope(),
        owner: scope(),
        cwd: "/".into(),
        pid: 1,
        background: Some(letibot_transcript::Backgrounding::Asked),
        state: JobState::Running,
        elapsed: std::time::Duration::from_secs(2),
        ran_for: None,
        produced: 12_288,
        since_last_output: None,
        memory_cap: letibot_tools::exec::MemoryCap::NotAsked,
        memory: None,
        output_file: None,
        env_report: None,
    };

    let named = Harness::job_row(&view(Some("release-build")));
    assert_eq!(
        named,
        "  release-build · j65  running  12288 bytes  cargo build --release"
    );
    // The name is flattened like everything else on the row - a name is one line
    // however it was typed, and the listing is a listing.
    assert_eq!(
        Harness::job_name(&view(Some("release\nbuild"))),
        "release build · j65",
        "a row that draws as two"
    );
    // No name, no decoration: byte-for-byte what this listing printed before slugs
    // existed, because that is what "absent" has to mean.
    let unnamed = Harness::job_row(&view(None));
    assert_eq!(
        unnamed,
        "  j65  running  12288 bytes  cargo build --release"
    );
    assert!(
        !unnamed.contains('·'),
        "a separator with nothing before it: {unnamed:?}"
    );
    // And an empty string is the same claim as no name, for the same reason the wire
    // spells absence as empty: *nobody named this* is one fact with one rendering.
    assert_eq!(Harness::job_name(&view(Some(""))), "j65");
}

/// The name reaches the wire, and a job nobody named sends an empty one: the head draws `JobEntry`, so this is the
/// half that makes the pane possible, and the empty half is the row that existed before the field did.
#[test]
fn a_job_entry_carries_the_name_it_was_given_and_nothing_when_it_was_not() {
    use letibot_tools::exec::{JobId, JobState, JobView, ScopeId, ScopeKind};
    let scope = || ScopeId {
        kind: ScopeKind::Session,
        name: "s".into(),
        path: std::path::PathBuf::from("/sys/fs/cgroup/x"),
    };
    let view = |slug: Option<&str>| JobView {
        id: JobId("j65".into()),
        command: "cargo test -p letibot-tools".into(),
        slug: slug.map(|s| s.to_string()),
        scope: scope(),
        owner: scope(),
        cwd: "/".into(),
        pid: 1,
        background: Some(letibot_transcript::Backgrounding::Asked),
        state: JobState::Running,
        elapsed: std::time::Duration::from_secs(2),
        ran_for: None,
        produced: 0,
        since_last_output: None,
        memory_cap: letibot_tools::exec::MemoryCap::NotAsked,
        memory: None,
        output_file: None,
        env_report: None,
    };

    assert_eq!(
        Harness::job_entry(&view(Some("fold-tests"))).slug,
        "fold-tests"
    );
    assert_eq!(Harness::job_entry(&view(None)).slug, "");
    assert_eq!(Harness::job_entry(&view(Some(""))).slug, "");
    // The command is untouched by any of it: the name is not the command's doing.
    assert_eq!(
        Harness::job_entry(&view(Some("fold-tests"))).command,
        "cargo test -p letibot-tools"
    );
}

/// The url is written the way it is curled: a scheme and the chat-completions path both appear in what the operator
/// pastes, and both come off, since the client appends its own route and a kept path would post to a doubled one.
#[test]
fn a_fleet_url_is_read_the_way_the_operator_writes_it() {
    let e = super::parse_local_url("http://192.168.1.78:8082/v1/chat/completions")
        .expect("a curl's url");
    assert_eq!(e.authority(), "192.168.1.78:8082");
    assert_eq!(
        super::parse_local_url("192.168.1.78:8082")
            .unwrap()
            .authority(),
        "192.168.1.78:8082",
        "a bare authority is the same address"
    );
    assert_eq!(
        super::parse_local_url("https://box.lan:443/")
            .unwrap()
            .authority(),
        "box.lan:443"
    );
}

/// The parent is told which jobs died with a VM child - by
/// label and command - and nothing is said when none did.
#[test]
fn the_jobs_a_vm_child_left_running_are_named_to_its_parent() {
    use letibot_tools::exec::{JobId, JobState, JobView, ScopeId, ScopeKind};
    let scope = || ScopeId {
        kind: ScopeKind::Session,
        name: "s".into(),
        path: std::path::PathBuf::from("/sys/fs/cgroup/x"),
    };
    let job = JobView {
        id: JobId("j41".into()),
        command: "cargo test --release\necho done".into(),
        slug: Some("tests".into()),
        scope: scope(),
        owner: scope(),
        cwd: "/".into(),
        pid: 1,
        background: Some(letibot_transcript::Backgrounding::Asked),
        state: JobState::Running,
        elapsed: std::time::Duration::from_secs(5),
        ran_for: None,
        produced: 0,
        since_last_output: None,
        memory_cap: letibot_tools::exec::MemoryCap::NotAsked,
        memory: None,
        output_file: None,
        env_report: None,
    };
    assert_eq!(vm_jobs_lost_line(&[]), None);
    let said = vm_jobs_lost_line(&[job]).expect("a line");
    assert!(
        said.starts_with("[subagent jobs] 1 background job(s)"),
        "{said}"
    );
    assert!(
        said.contains("`tests · j41` (`cargo test --release`)"),
        "{said}"
    );
    assert!(said.contains("died with it"), "{said}");
}
