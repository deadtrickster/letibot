//! Background jobs: their records, windows, completions and settlements.

use super::*;

/// **A condition is met by a job that is GONE — including one this daemon never knew** — the
/// operator's restart case, as the words the model is handed.
///
/// Three fates, three sentences, and the split is the operator's own: a handle that ended here
/// can be read with `job_output`, and one this daemon has never heard of cannot — so that line
/// says the result is not here rather than leaving the reader to find out by asking. His
/// words, which are the whole reason the fate is carried and not just the boolean:
/// *"you either follow the job result up manually (which is more robust) if next steps depend
/// on it or just do your things if it was just a timeline"*.
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
    let unknown = fate_line("j121", &JobFate::Unknown);
    assert!(
        unknown.contains("not a job this session knows"),
        "the restart case must say the result is not here: {unknown}"
    );
    assert!(
        unknown.contains("read it up yourself"),
        "and say what the reader can do about it, since following it up is the robust half: \
             {unknown}"
    );
    // **A running job is not due, and the sentence exists anyway.** A function that panics on
    // the arm nobody reaches is a function whose contract is a rumour — and this one is
    // reachable the moment anything but `due_todo_lines` asks.
    assert!(
        fate_line("j7", &JobFate::Running).contains("still running"),
        "the running arm must answer rather than be unreachable"
    );
}

/// **THE EVALUATOR: what the world says about a handle, and which rows that makes due.**
///
/// The decision a conditioned row turns on, and it had **no assertion at all** before this:
/// reaching it through a `Harness` needs a backend, a registry, a gate and a store, so the only
/// thing pinned was [`fate_line`]'s wording — the sentence, and not the choice that picks it.
///
/// The operator's acceptance case is the one that decides the design: *"a todo conditioned on
/// job end, and then i restart head and harnessd. once server is back it should fire - job is
/// gone"*. After that restart the process table is empty and the handle is a job this daemon
/// never ran, so **`Unknown` must meet the condition** — a decision that read *no row, no fate,
/// skip it* would leave the row waiting for ever on work that is already done, which is the one
/// failure this whole mechanism exists to prevent.
#[test]
fn the_evaluator_fires_a_row_whose_handle_is_gone() {
    use letibot_tokencore::store::{TodoBy, TodoCondition, TodoItem, TodoStatus};
    /// A stored row for one handle, as a restart would find it — the ending written down
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
    // **The same row, set aside.** Nothing else about it changes — the words, the author and
    // the handle are `row`'s — so what the filter below is being asked about is the STATE and
    // not some other difference between two fixtures.
    let set_aside = |content: &str, when: Option<TodoCondition>| TodoItem {
        status: TodoStatus::Postponed,
        ..row(content, when)
    };
    // **The same row again, finished.** The fourth skip below, and it is the one this change
    // widens rather than adds — see `due_rows`: the predicate is the idle check's, and a row
    // that is already done is not work it may speak about.
    let finished = |content: &str, when: Option<TodoCondition>| TodoItem {
        status: TodoStatus::Completed,
        ..row(content, when)
    };

    // **The order of the two looks, which is the whole of the honesty.** The live table answers
    // first: only the process host can say a job runs *now*, so a record saying `running` — a
    // job the daemon was watching when it died — must not override a host that says otherwise.
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
    // **The restart, which is why the record is the second look**: no process table at all, and
    // the ending this daemon wrote down before it went away.
    assert_eq!(
        fate_of(None, Some(&record("j121", "exited 3"))),
        JobFate::Ended {
            state: "exited 3".into()
        },
        "the record is what survives a restart"
    );
    // And a handle nobody has run: met, and not dressed up as an ending — the distinction is
    // the operator's, since the result can be read up for one and not for the other.
    assert_eq!(
        fate_of(None, None),
        JobFate::Unknown,
        "a job this daemon never heard of has ended; that is what a restart looks like"
    );

    // **The filter, over a board the world answers for.** Four skips — a row with no
    // condition, a running job, a row the operator has set aside, and a row already done — and
    // two firings, one of each kind of gone.
    let rows = vec![
        row("ship the parity row", None),
        row("push once CI lands", waiting_on("j121")),
        row("wait for the build", waiting_on("j7")),
        row("after the restart", waiting_on("j900")),
        // **A POSTPONED row does not fire, and it keeps its handle.** The handle is the same
        // `j121` the row above is due on, so a firing here could only come from the state —
        // and the absence of one is the whole of what *"persists but without nag"* means for
        // a conditioned row. Nothing in `due_rows` clears the condition: lifting the
        // postponement puts the row back in front of this same filter, which is what makes
        // the state reversible rather than a quiet way to drop a firing.
        set_aside("push once CI lands", waiting_on("j121")),
        // **And a FINISHED row does not fire either**, again on `j121`, which is the same
        // handle the open row above is due on: a firing here could only come from the state.
        // The condition is not cleared by any of this, so the row keeps its handle — what the
        // skip buys is silence about work that is over.
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

/// **A subagent's ask is addressed to the tree's ROOT, never to its parent** — R58's ask
/// route, and the one sentence in the spawn that has to be right for a card to reach a
/// head at all.
///
/// The GRANDCHILD case is what decides it: at depth ≥ 1 the parent is itself a child, so
/// addressing the parent would post the card on a session no head can be attached to and
/// no worker drives — a card nobody would ever see, which is the defect this whole change
/// is against. The fixture is therefore a root, a child and a grandchild, built the way
/// `jobwatch`'s settlement test builds them.
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
    // The grandchild's set is built FROM the root's, which is what gives it the ROOT —
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

    // **A tree with no head-wired adjudicator**: no card is posted, and the child's own
    // refusal names the root instead (see `answers::SubagentAdjudicator`).
    let (root, head) = subagent_ask_target(&registry, Some(&grandchild_watch), "s-child", None);
    assert_eq!(root, "s-root");
    assert!(head.is_none());

    // **A root this daemon does not hold** — a harness with no daemon behind it, or a
    // registry that has lost the session — is the same refusal rather than a card on a
    // hub nothing will read. With no watcher set at all, the parent IS the tree's root.
    let (root, head) = subagent_ask_target(&registry, None, "s-absent", answers.as_ref());
    assert_eq!(root, "s-absent");
    assert!(head.is_none(), "a hub nobody holds cannot answer");
}

/// **THE WINDOW OF A REDIRECTED JOB IS THE FILE'S TAIL** — R41's other half, and the
/// operator's own next ask after the pane learned to name the file: *"job pane can tail
/// the file"*.
///
/// `offset == 0` is the TAIL, because that is what a fresh read of an output pane means;
/// any other offset is a forward window from there, so `→` still walks to the end of a
/// long log the way it does for the ring.
#[test]
fn a_redirected_jobs_window_is_the_tail_of_its_file() {
    let dir = std::env::temp_dir().join(format!("letibot-tail-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch");
    let path = dir.join("build.log");
    let body: String = (0..40).map(|i| format!("line {i}\n")).collect();
    std::fs::write(&path, &body).expect("write");
    let path = path.to_str().expect("utf-8 path");

    // **A fresh read is the tail**: the last `limit` bytes, and the pane is told the
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

    // **An explicit offset is a forward window**, which is what `next` hands to `→`.
    let w2 = tail_file(path, w.from, 20).expect("a window");
    assert_eq!(w2.from, w.from);
    assert_eq!(w2.to, w.from + 20);

    // A limit larger than the file is the whole file, not an error and not padding.
    let all = tail_file(path, 0, 4096).expect("a window");
    assert_eq!(all.from, 0);
    assert_eq!(all.to, body.len() as u64);

    // **A file that is not there is `None`** — the job has not created it yet — which is
    // what the caller answers with the sentence that names the path.
    assert!(tail_file(&dir.join("nope.log").to_string_lossy(), 0, 10).is_none());

    let _ = std::fs::remove_dir_all(&dir);
}

// **The fixtures for the window tests below: real jobs, really run.** The decision
// those tests hold to is `job_window_for`, which `Harness::job_output_window` hands
// its two reads; the honest way to exercise it is with a command a real `/bin/sh`
// ran, a capture ring that really holds what the preamble wrote, and a file the
// work's stage really created — the same substrate `jobwatch`'s tests spawn on,
// and the only one that can prove the fixture is the operator's measurement.
mod job_tail {
    use super::*;
    use letibot_tools::ProcessHost as _;
    use letibot_tools::exec::{HostProcesses, ScopeKind, SpawnRequest};
    use std::time::Duration;

    /// Run `command` to its end and hand back the three things the window is
    /// decided on: the command, its state, and its capture. A job that does not
    /// exit 0 fails here, so a broken fixture never reaches the assertions as a
    /// mystery.
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

    /// **A host over a temp root — or nothing, when this box cannot delegate a
    /// cgroup v2 subtree.**
    ///
    /// A job needs a process-lifetime tree, and a box without one is a real thing:
    /// CI's runner is one, MEASURED on run 37925350568 — *"mkdir
    /// /sys/fs/cgroup/system.slice/hosted-compute-agent.service/letibot.7443.2:
    /// Permission denied (os error 13) — a delegated cgroup v2 subtree is needed;
    /// …/hosted-compute-agent.service is the harness's own cgroup and it is not
    /// writable"*. A `#[ignore]` here would be a green suite that measured nothing,
    /// so the absence is SAID and `LETIBOT_REQUIRE_APPARATUS=1` refuses it — the same
    /// gate over the same predicate this crate's other host-spawning tests take (see
    /// `jobwatch.rs`).
    fn a_host(name: &str) -> Option<(std::path::PathBuf, std::sync::Arc<HostProcesses>)> {
        let Some(_) = letibot_tokencore::apparatus::present(
            "a process-lifetime tree (cgroup v2; process groups on macOS)",
            letibot_tools::host_tree().is_ok(),
        ) else {
            return None;
        };
        let root = std::env::temp_dir().join(format!(
            "letibot-jobtail-{name}-{pid}",
            pid = std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("scratch");
        let host = HostProcesses::new(&root)
            .expect("the tree probed writable above, so this root builds one");
        Some((root, std::sync::Arc::new(host)))
    }

    /// **THE MEASURED DEFECT, j87's own shape**: a compound command whose preamble
    /// writes to the capture and whose work redirects to a file. Read from the
    /// operator's store — `exited 0`, produced 2426, redirect `/tmp/tpcc-trace2.log`,
    /// the file NOT read — with j70 (`produced 0`, same redirect, worked) proving
    /// the empty capture was the only case the pane tailed.
    ///
    /// The window must answer with the FILE's tail whatever the capture holds, and
    /// the capture's bytes must be counted, not hidden: the fresh read says how many
    /// are outside the window and which file the window is.
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

        // **The fixture is the measurement or it is nothing**: the capture holds the
        // preamble — bytes, produced > 0 — and not one byte of the work.
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
        // **The preamble is disclosed, not silently dropped.** The line names the
        // count that stayed in the capture and the file the window is over.
        assert!(
            drawn.contains(&slice.produced.to_string()) && drawn.contains("work.log"),
            "the fresh read must count what it is not showing: {drawn}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **j70's case must not regress**: a redirect whose capture is empty — the
    /// `produced == 0` the old condition required — still tails the file, and the
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

    /// **A `2>&1` merge is not a redirect** — it is what makes the capture complete,
    /// so the window is the capture. R41's parser guarantees it; this holds the
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

    /// **A job with no redirect is unaffected**: the capture is the window, exactly
    /// as before — the redirect branch must not have widened into this case.
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

/// **The name reaches the transcript, and a job nobody named reads exactly as it did.**
///
/// The operator's own correction, verbatim: *"yes, change rano, and i was look at the
/// transcript actually, not jobs pane"* — their complaint (*"j57 doesnt ringe the bell for
/// me"*) was about THIS line, the one a settlement puts in the conversation, and a counter
/// is not a name. So the bullet carries [`letibot_tools::exec::job_label`]'s spelling: the
/// same string `bash`'s backgrounded result printed when the job was started, and the same
/// one `/job` draws.
///
/// **And the unnamed half is a regression test rather than a courtesy.** A job nobody named
/// must read byte for byte as it did before a name existed — the tree's rule for a defaulted
/// field — and it does, because the label's fallback IS the id. The two notices below differ
/// in the name and nowhere else, which is what makes that a property instead of a hope.
#[test]
fn a_settlement_carries_the_jobs_name_and_an_unnamed_one_is_unchanged() {
    let settled = |job: &str, slug: Option<&str>| JobCompletion {
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
        named.contains(
            "  - `release-build · j65` exited 0 after 54.9s, wrote 1030 bytes: cargo test\n"
        ),
        "the settlement must name the job the way the bash result did: {named}"
    );

    // **The handed-up variant carries the same name.** One bullet builder, so this is the
    // same sentence under the other heading — and the owner is still named beside it.
    let handed = completion_notice(&[settled("j65", Some("release-build"))], false);
    assert!(
        handed.contains("`release-build · j65` exited 0") && handed.contains("started by `s-me`"),
        "{handed}"
    );

    let bare = completion_notice(&[settled("j65", None)], true);
    assert!(
        bare.contains("  - `j65` exited 0 after 54.9s, wrote 1030 bytes: cargo test\n"),
        "{bare}"
    );
    // **The name is the only difference.** Strip it from the named notice and what is left
    // is the unnamed one, byte for byte — so "nothing regresses for those who do not use it"
    // is asserted rather than argued.
    assert_eq!(named.replace("release-build · ", ""), bare);
}

/// **The notice, byte for byte, as the TUI's compact card parses it.**
///
/// The TUI draws this notice as two lines a job — the operator: *"i dont need job completion
/// card that huge - first line is j<id> - slug - truncated cmd and second line is truncated
/// output"* — and to do that it parses the sentence (`crates/tui/src/ui/transcript/job_card.rs`).
/// A parser of another crate's prose is only as good as the test that ties them, so the two
/// notices below are written out to fixtures the TUI's tests parse, and compared here byte for
/// byte. **A change to the sentence fails HERE**, naming the fixture, rather than turning the
/// operator's compact rows back into six rows of raw notice; re-bless with
/// `LETIBOT_BLESS_JOB_NOTICE=1 cargo test -p letibot-harnessd job_notice_fixtures` and then make
/// the TUI's parser read the new shape.
///
/// The cases are the hard ones: a command written over several lines (a blank line among them),
/// the `memory:` clause, a named and an unnamed job in one notice, and the handed-up notice with
/// its owner on the end of the command.
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
            kind: BackgroundKind::Job,
            owner: "s-me".into(),
            job: "j9".into(),
            command: String::new(),
            // **And the name is gone with it.** A reaped job has no view left to read a
            // slug off, so this is the other field that can be missing — and the notice
            // draws the id, which is the label's own fallback rather than a second rule.
            slug: None,
            state: "gone".into(),
            produced: 0,
            elapsed_ms: 0,
            detail: String::new(),
            memory: None,
        }],
        true,
    );
    assert!(text.contains("command not recorded"), "{text}");
    assert!(!text.contains("``"), "an empty pair of backticks: {text}");
}

/// **The child's sentence, which had no test, and it is the one the model reads.**
///
/// `completion_notice`'s twin on the subagent side: same channel, same wake, and the text is
/// again the deliverable. It is the *whole* of what the parent is handed when a child finishes
/// between turns, so what it names and what it promises are asserted rather than left to the
/// docstring — which is exactly the gap that let *"nothing fired"* be reported without anything
/// in CI disagreeing about whether the notice could fire at all.
///
/// **Two functions rather than a flag on one**, and the first assertion is why: the verb a child
/// is read with is `task_result`, not `job_output`, so a model that gets the wrong one is being
/// sent to a door that does not open. The second is that a child's completion carries an ANSWER
/// where a job's carries a command's output, and the parent that is told only *it stopped* has to
/// go and ask — which is the poll this whole path exists to end.
#[test]
fn a_childs_completion_names_the_child_and_points_at_task_result() {
    let one = JobCompletion {
        kind: BackgroundKind::Subagent,
        owner: "s-me".into(),
        job: "s-1-sub-1".into(),
        // A subagent runs no command, and the field must not quietly hold an answer.
        command: String::new(),
        // Nor does it have a name: a `task` child's handle is its identity.
        slug: None,
        state: "done".into(),
        produced: 0,
        elapsed_ms: 0,
        detail: "two facts off a fixture".into(),
        memory: None,
    };
    let text = subagent_notice(&[one.clone()], true);
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
    // **What the child SAID.** Without this the notice is a doorbell with no answer behind it:
    // the parent is told something happened and must still go and read.
    assert!(text.contains("two facts off a fixture"), "{text}");
    // The half that is the fix and not the footnote, in the job side's own words.
    assert!(text.contains("do not need to wait"), "{text}");
    assert!(text.contains("task_result"), "{text}");
    assert!(
        !text.contains("job_output"),
        "a subagent is read with `task_result`; sending the parent to `job_output` is a door \
             that does not open: {text}"
    );

    // **A child that said nothing gets no empty field.** The job side guards an empty command
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

    // **Two children are one notice, two lines, and the plural sentence** — a round that spawned
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
}

/// **The wake's one decision, asserted: which sentence for which kind, and none for nothing.**
///
/// The partition is the whole of what [`Harness::wake`] decides about a settlement — everything
/// else it does is take the queue and submit the text — and it is asserted here because `wake()`
/// itself cannot be: it needs a live `Harness`, which this crate's tests build only where a
/// vocabulary and a store are present. See [`completion_notices`] for why that is not a reason to
/// leave the decision unasserted.
#[test]
fn a_settlement_is_two_nouns_out_of_one_channel() {
    let job = |id: &str| JobCompletion {
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

    // **Nothing settled says nothing**, which is what makes a spurious wake cost no generation.
    assert!(completion_notices(Vec::new(), "s-me").is_empty());

    // **The two nouns are the point.** A reader told `[job]` about a child goes looking for it
    // with `job_output` — which is the wrong door, and is what a hand measurement had to find
    // because nothing asserted this.
    let only_child = completion_notices(vec![child("s-1-sub-1")], "s-me");
    assert_eq!(only_child.len(), 1, "{only_child:?}");
    assert!(only_child[0].starts_with("[task]"), "{:?}", only_child[0]);
    assert!(only_child[0].contains("task_result"), "{:?}", only_child[0]);
    let only_job = completion_notices(vec![job("j7")], "s-me");
    assert_eq!(only_job.len(), 1, "{only_job:?}");
    assert!(only_job[0].starts_with("[job]"), "{:?}", only_job[0]);
    assert!(only_job[0].contains("job_output"), "{:?}", only_job[0]);

    // **Both kinds are one wake and two sentences**, job first — the order the old inline code
    // produced, kept so the record of a session reads the same way it did.
    let both = completion_notices(vec![job("j7"), child("s-1-sub-1")], "s-me");
    assert_eq!(both.len(), 2, "{both:?}");
    assert!(both[0].starts_with("[job]"), "{:?}", both[0]);
    assert!(both[1].starts_with("[task]"), "{:?}", both[1]);

    // **Two children are one sentence, not two** — the plural arm of the child's own wording,
    // asserted through the partition so the two functions cannot drift apart.
    let pair = completion_notices(vec![child("s-1-sub-1"), child("s-1-sub-2")], "s-me");
    assert_eq!(pair.len(), 1, "{pair:?}");
    assert!(
        pair[0].contains("2 subagents you started have finished"),
        "{:?}",
        pair[0]
    );
}

/// **A settlement handed up says WHOSE it was, and points at no door.**
///
/// The other half of the ownership split, and the sentence the measured defect of
/// 2026-10-05 needed: [`JobWatchers::stop`] hands a stopped session's undrained
/// settlements to the level above, and this is what keeps that hand-over honest.
///
/// The measurement, from the operator's own conversation: a subagent backgrounded
/// `sleep 25; echo done`, and the notice arrived in the MAIN session's transcript
/// reading *"a job you backgrounded has ended"* — while that session's own
/// `job_output` answered *no job called `j17`*. A notice that says *you backgrounded*
/// about a job you cannot read is not misplaced, it is false twice; and a notice that
/// promised `job_output` to the session that cannot open it sends a model to a door
/// that does not open, which is the rule this file keeps for `task_result` vs
/// `job_output` one layer down.
#[test]
fn a_handed_up_settlement_names_its_owner_and_sends_nobody_to_a_shut_door() {
    let job = JobCompletion {
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

    // The level it was handed up to — the ROOT — is not the session that started it.
    let theirs = completion_notices(vec![job.clone()], "s-root");
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

    // **The same settlement, read by the session that started it** — which is the
    // sentence that was right all along and must not have moved.
    let mine = completion_notices(vec![job.clone()], "s-child");
    assert!(
        mine[0].contains("a job you backgrounded has ended"),
        "{:?}",
        mine[0]
    );
    assert!(mine[0].contains("job_output"), "{:?}", mine[0]);
    assert!(!mine[0].contains("started by"), "{:?}", mine[0]);

    // **Its own come first**, so one drain reads as this session's own news before what
    // it is being told on somebody else's behalf — two owners, two notices, that order.
    let mine_job = JobCompletion {
        owner: "s-root".into(),
        job: "j3".into(),
        ..job.clone()
    };
    let both = completion_notices(vec![job, mine_job], "s-root");
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
    let handed = completion_notices(vec![child], "s-root");
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
}

/// **A finished foreground command is not a job anybody acts on.**
///
/// The operator ran `/job` after a turn and got hundreds of lines — one per
/// `grep`, `cat` and `sed` the model had run all session — because the host
/// gives every command a job id and the listing showed all of them. Measured
/// in their own store the same day: of 1,210 tool results in that session,
/// exactly **2** were `backgrounded`.
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
        // Nobody named this one — which is the premise of every case below.
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
    };

    // The case that flooded the screen: finished, never backgrounded.
    assert!(!Harness::worth_listing(&job(
        None,
        JobState::Exited { code: 0 }
    )));
    // A failure is still not a job — the tool result already carried it.
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

/// **A row is one line, whatever the model wrote.**
///
/// A heredoc carries newlines inside the command text, and the listing cut it
/// to 60 *characters* — keeping the newline, so one row drew as two and a
/// screen of them was unreadable. The operator, 2026-09-20, looking at it:
/// *"some fuckery with jobs"*.
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

    // A short command is left exactly as it is — no ellipsis promising more.
    assert_eq!(Harness::one_line("git status", 60), "git status");
    // Internal runs of whitespace collapse, so the width asked for is the
    // width drawn.
    assert_eq!(Harness::one_line("ls   -la\t-h", 60), "ls -la -h");
}

/// **A named job draws its name beside its id, and an unnamed one draws what it drew
/// before.**
///
/// `j57` is a counter: the operator, watching the pane, could not tell which running job
/// was the release build and which was the fold's tests — *"do we want to give jobs
/// slugs? j57 doesnt ringe the bell for me"*. The name is the agent's own stated intent,
/// so the row has to carry it and the absence of one has to be exactly the old row.
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
    };

    let named = Harness::job_row(&view(Some("release-build")));
    assert_eq!(
        named,
        "  release-build · j65  running  12288 bytes  cargo build --release"
    );
    // **The name is flattened like everything else on the row** — a name is one line
    // however it was typed, and the listing is a listing.
    assert_eq!(
        Harness::job_name(&view(Some("release\nbuild"))),
        "release build · j65",
        "a row that draws as two"
    );
    // **No name, no decoration**: byte-for-byte what this listing printed before slugs
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

/// **The name reaches the wire, and a job nobody named sends an empty one.**
///
/// The head draws `JobEntry`, so this is the half that makes the pane possible at all;
/// the empty half is the one an older daemon's row takes, and it must be the row that
/// existed before the field did.
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

/// **The url is written the way it is curled.** A scheme and the chat-completions
/// path both appear in what the operator pastes, and both have to come off: the
/// client appends its own route, so a kept path would post to
/// `/v1/chat/completions/completion`.
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
