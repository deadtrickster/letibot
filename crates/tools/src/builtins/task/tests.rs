use super::*;

/// A runner that answers immediately: `start` records what it was asked and
/// `collect` hands the same sentence back, so the tests are about the tool's
/// own shape and not about threads.
#[derive(Default)]
struct Echo {
    answers: std::sync::Mutex<Vec<(String, String)>>,
}

impl TaskRunner for Echo {
    fn start(&self, prompt: &str, spec: &TaskSpec) -> Result<String, String> {
        let mut a = self.answers.lock().unwrap();
        let handle = format!("sub-{}", a.len() + 1);
        a.push((
            handle.clone(),
            format!(
                "{} [{}] @{}: {prompt}",
                spec.role,
                spec.downgrade.describe(),
                spec.placement.as_str()
            ),
        ));
        Ok(handle)
    }

    fn collect(&self, handle: &str, _timeout: std::time::Duration) -> TaskStatus {
        match self
            .answers
            .lock()
            .unwrap()
            .iter()
            .find(|(h, _)| h == handle)
        {
            Some((_, answer)) => TaskStatus::Done {
                answer: answer.clone(),
                child: None,
            },
            None => TaskStatus::Unknown,
        }
    }

    fn started(&self) -> Vec<String> {
        self.answers
            .lock()
            .unwrap()
            .iter()
            .map(|(h, _)| h.clone())
            .collect()
    }
}

fn spec(role: &str) -> TaskSpec {
    TaskSpec {
        role: role.into(),
        ..Default::default()
    }
}

/// **A correction reaches the runner, and a refusal is not an `Ok`.**
///
/// The whole value of `task_message` is that a parent cannot be told its child was
/// corrected when it was not, so this pins both directions: what the runner is asked to
/// say, and what the caller sees when the runner refuses.
#[test]
fn a_message_reaches_the_runner_and_a_refusal_is_not_an_ok() {
    use crate::runtime::{Registry, ToolRuntime};
    use letibot_transcript::ToolCall;

    /// Records what it was asked to say; refuses one handle by name.
    struct Steerable(std::sync::Mutex<Vec<(String, String)>>);

    impl TaskRunner for Steerable {
        fn start(&self, _p: &str, _s: &TaskSpec) -> Result<String, String> {
            Ok("sub-1".into())
        }
        fn collect(&self, _h: &str, _t: std::time::Duration) -> TaskStatus {
            TaskStatus::Unknown
        }
        fn send(&self, handle: &str, text: &str) -> Result<String, String> {
            if handle == "sub-finished" {
                return Err("`sub-finished` is not running a turn".into());
            }
            self.0
                .lock()
                .unwrap()
                .push((handle.to_string(), text.to_string()));
            Ok(format!("`{handle}` was told"))
        }
    }

    let runner = Arc::new(Steerable(std::sync::Mutex::new(Vec::new())));
    let mut reg = Registry::new();
    reg.register(Box::new(TaskMessageTool::new(runner.clone())))
        .unwrap();
    let d = crate::backend::tempdir::TempDir::new();
    let backend = crate::backend::HostBackend::new(d.path()).unwrap();
    let mut rt = ToolRuntime::new(reg, Box::new(backend));
    let mut sink = crate::NullToolSink;
    let call = |rt: &mut ToolRuntime, sink: &mut crate::NullToolSink, args: &str| {
        rt.invoke(
            "t1",
            &ToolCall {
                id: "c1".into(),
                name: "task_message".into(),
                arguments: args.into(),
            },
            sink,
        )
    };

    let said = call(
        &mut rt,
        &mut sink,
        r#"{"task": "sub-1", "text": "stop and report what you have"}"#,
    );
    assert!(
        matches!(said.outcome, letibot_transcript::ToolOutcome::Ok),
        "{:?}",
        said.outcome
    );
    assert_eq!(
        runner.0.lock().unwrap().as_slice(),
        [(
            "sub-1".to_string(),
            "stop and report what you have".to_string()
        )]
    );

    // **A child that has already answered is a failure, not a quiet success.** This is
    // the defect class the kill already paid for: a tool reporting a state change that
    // nothing made.
    let missed = call(
        &mut rt,
        &mut sink,
        r#"{"task": "sub-finished", "text": "hello"}"#,
    );
    assert!(
        matches!(
            missed.outcome,
            letibot_transcript::ToolOutcome::Failed { .. }
        ),
        "{:?}",
        missed.outcome
    );
    assert!(
        missed.payload.contains("not running a turn"),
        "{}",
        missed.payload
    );

    // Both arguments are required, and the miss says which one.
    let missing = call(&mut rt, &mut sink, r#"{"task": "sub-1"}"#);
    assert!(missing.payload.contains("`text`"), "{}", missing.payload);

    // And the default a session with no runner gets refuses by name.
    let why = NoTaskRunner.send("sub-1", "x").unwrap_err();
    assert!(why.contains("cannot message"), "{why}");
}

/// **The call returns while the child is still working.** This is the whole
/// point of the change: the daemon runs a round's calls in order on one
/// thread, so a `task` that waited held every call behind it for as long as
/// the child lived. Measured 2026-09-17 in the operator's session — a block
/// of `todo_write`, `task` and three `web_search` calls, all unfinished for
/// the fifteen minutes the subagent ran.
/// **R23 for subagents: a watched child is looked at, not waited on.** The stroppy session
/// spent 240 s in `task_result` on a child whose answer the daemon was already going to
/// deliver as a turn — *"why it decided to wait for it explicitly if we have completion
/// events"*. With the watch question wired, a `timeout_ms` on a child still working returns
/// at once and says the answer is on its way; an answer already in is returned as ever; and
/// a child nobody is watching keeps the wait it asked for.
#[test]
fn a_watched_subagent_is_not_waited_on() {
    use std::sync::atomic::{AtomicBool, Ordering};
    /// A child that finishes when told, and that writes down the longest wait anybody asked
    /// of it — the fact "held the floor" is about, read without a clock.
    struct Gate {
        done: Arc<AtomicBool>,
        longest_wait: Arc<std::sync::Mutex<std::time::Duration>>,
    }
    impl TaskRunner for Gate {
        fn start(&self, _p: &str, _s: &TaskSpec) -> Result<String, String> {
            Ok("sub-w".into())
        }
        fn collect(&self, _h: &str, timeout: std::time::Duration) -> TaskStatus {
            {
                let mut l = self.longest_wait.lock().unwrap();
                *l = (*l).max(timeout);
            }
            let deadline = std::time::Instant::now() + timeout;
            loop {
                if self.done.load(Ordering::SeqCst) {
                    return TaskStatus::Done {
                        answer: "the child's answer".into(),
                        child: None,
                    };
                }
                if std::time::Instant::now() >= deadline {
                    return TaskStatus::Running {
                        note: None,
                        child: None,
                    };
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        fn started(&self) -> Vec<String> {
            vec!["sub-w".into()]
        }
    }
    let run = |watched: bool, done: bool| {
        let longest_wait = Arc::new(std::sync::Mutex::new(std::time::Duration::ZERO));
        let runner: Arc<dyn TaskRunner> = Arc::new(Gate {
            done: Arc::new(AtomicBool::new(done)),
            longest_wait: longest_wait.clone(),
        });
        let d = crate::backend::tempdir::TempDir::new();
        let backend = crate::backend::HostBackend::new(d.path()).unwrap();
        let mut reg = crate::runtime::Registry::new();
        reg.register(Box::new(TaskResultTool::new(runner))).unwrap();
        let mut rt = crate::runtime::ToolRuntime::new(reg, Box::new(backend));
        let wanted = Arc::new(std::sync::Mutex::new(Vec::new()));
        if watched {
            rt = rt.with_completion_delivered(Arc::new(|j: &str| j == "sub-w"));
            let w = wanted.clone();
            rt = rt.with_completion_wanted(Arc::new(
                move |j: &str, why: crate::runtime::WantedNow| {
                    w.lock().unwrap().push((j.to_string(), why));
                },
            ));
        }
        let started = std::time::Instant::now();
        let r = rt.invoke(
            "t1",
            &letibot_transcript::ToolCall {
                id: "c0".into(),
                name: "task_result".into(),
                arguments: r#"{"task": "sub-w", "timeout_ms": 400}"#.into(),
            },
            &mut crate::NullToolSink,
        );
        let asked = *longest_wait.lock().unwrap();
        let wanted = wanted.lock().unwrap().clone();
        (r, started.elapsed(), asked, wanted)
    };

    // **The watched child was asked for no wait at all** — the cause, rather than
    // `elapsed() < 200ms`, which a loaded box trips with the code right (R67 item 19).
    let (r, _, asked, wanted) = run(true, false);
    assert_eq!(
        asked,
        std::time::Duration::ZERO,
        "a watched child was waited on for {asked:?}"
    );
    // **And the wait told the daemon it wants this one now** (R79), so the answer is not held
    // for the next break — which is what "reaches you on its own" promises a waiter.
    assert_eq!(
        wanted,
        [("sub-w".to_string(), crate::runtime::WantedNow::Awaited)]
    );
    assert!(r.payload.contains("nothing to wait for"), "{}", r.payload);
    assert!(r.payload.contains("on its own"), "{}", r.payload);

    let (r, _, _, _) = run(true, true);
    assert!(r.payload.contains("the child's answer"), "{}", r.payload);

    // Unwatched: the wait it asked for, to the deadline.
    // (A LOWER bound is safe on any box: a slow machine only makes it truer.)
    let (r, took, asked, _) = run(false, false);
    assert_eq!(asked, std::time::Duration::from_millis(400));
    assert!(
        took >= std::time::Duration::from_millis(400),
        "an unwatched wait returned early: {took:?}"
    );
    assert!(r.payload.contains("has not answered yet"), "{}", r.payload);
}

/// **THE THREE STATES, THROUGH THE DOOR THE PARENT HAS** — a working child, an idle one and a
/// wedged one must be tellable apart by what `task_result` answers, and the idle one is the
/// case this change is for.
///
/// The operator's measurement, in their words: three asks about one child produced the
/// identical sentence, *"still working"* — *"which tells me it has not died and nothing at all
/// about whether it is stuck"*. And the state that had no sentence anywhere: a child whose turn
/// ended and which sits parked, *"between turns"*, which the parent learned only by sending a
/// message nothing said to send.
///
/// **Constructed, not waited for**: the reading is a value the runner hands back
/// ([`RunningChild`]), so the three cases are three literals and no child, no model and no
/// clock. The one thing this cannot construct is a runner that keeps no reading — that is the
/// last case, and it must read exactly as it did before this field existed.
#[test]
fn task_result_tells_a_working_child_from_an_idle_one_and_a_wedged_one() {
    use crate::runtime::{Registry, ToolRuntime};
    use std::time::Duration;

    /// A runner that answers with one reading for as long as it is asked. `None` is the
    /// runner that keeps no record at all, which is every fake and `NoTaskRunner`.
    struct OneReading(Option<RunningChild>);
    impl TaskRunner for OneReading {
        fn start(&self, _p: &str, _s: &TaskSpec) -> Result<String, String> {
            Ok("sub-r".into())
        }
        fn collect(&self, _h: &str, _t: Duration) -> TaskStatus {
            TaskStatus::Running {
                note: None,
                child: self.0,
            }
        }
        fn started(&self) -> Vec<String> {
            vec!["sub-r".into()]
        }
    }

    let ask = |child: Option<RunningChild>| {
        let runner: Arc<dyn TaskRunner> = Arc::new(OneReading(child));
        let d = crate::backend::tempdir::TempDir::new();
        let backend = crate::backend::HostBackend::new(d.path()).unwrap();
        let mut reg = Registry::new();
        reg.register(Box::new(TaskResultTool::new(runner))).unwrap();
        let mut rt = ToolRuntime::new(reg, Box::new(backend));
        rt.invoke(
            "t1",
            &letibot_transcript::ToolCall {
                id: "c0".into(),
                name: "task_result".into(),
                arguments: r#"{"task": "sub-r"}"#.into(),
            },
            &mut crate::NullToolSink,
        )
    };
    // Forty-five minutes old in every case, so the AGE decides nothing and the difference
    // between the answers is only what the child last showed — the whole point.
    let at = |quiet: u64, working: bool| RunningChild {
        age: Duration::from_secs(45 * 60),
        quiet_for: Some(Duration::from_secs(quiet)),
        working,
    };
    let summary = |r: &crate::ToolResult| match &r.outcome {
        letibot_transcript::ToolOutcome::Abstained { reason } => reason.clone(),
        other => panic!("a child whose turn has not ended abstains: {other:?}"),
    };

    // **1. WORKING** — a turn in flight and the child's own log moved eight seconds ago.
    let working = ask(Some(at(8, true)));
    assert_eq!(summary(&working), "subagent `sub-r` is working");
    assert!(
        working.payload.contains("a turn is in flight"),
        "{}",
        working.payload
    );
    assert!(
        !working.payload.contains("check whether it is stuck"),
        "a child that is moving is not sent to be looked at: {}",
        working.payload
    );

    // **2. IDLE, which is the case that had no sentence.** No turn is in flight and the child
    // is still there: the one door that reaches it is its parent's message, and the answer must
    // say so instead of the sentence that was printed about every child.
    let idle = ask(Some(at(12 * 60, false)));
    assert_eq!(
        summary(&idle),
        "subagent `sub-r` is between turns",
        "the state is named, and it is not *still working*: the whole defect is here"
    );
    assert!(
        idle.payload.contains("between turns") && idle.payload.contains("`task_message`"),
        "and the move a parked child needs is named: {}",
        idle.payload
    );
    assert!(
        !idle.payload.contains("still working"),
        "the sentence this replaces, and the one the operator got three times: {}",
        idle.payload
    );
    assert!(
        !idle.payload.contains("`job_kill`"),
        "a child waiting to be asked is not stuck, and stopping it is not the move: {}",
        idle.payload
    );

    // **3. WEDGED** — a turn in flight and nothing out of it for twelve minutes. The same
    // silence as the idle child, and the opposite move: this is the one `job_kill` answers.
    let wedged = ask(Some(at(12 * 60, true)));
    assert_eq!(summary(&wedged), "subagent `sub-r` is quiet");
    assert!(
        wedged.payload.contains("check whether it is stuck")
            && wedged.payload.contains("`job_kill`"),
        "a turn in flight with nothing coming out of it is what a reader is sent to look at: {}",
        wedged.payload
    );
    assert_ne!(
        idle.payload, wedged.payload,
        "idle and wedged must not read the same — they are the two states this whole change is \
             about"
    );

    // **And a runner that keeps no reading reads exactly as it did before.** This is the tree's
    // rule for a defaulted field: every existing fake, and `NoTaskRunner`, is this case.
    let none = ask(None);
    assert_eq!(summary(&none), "subagent `sub-r` is still working");
    assert!(
        !none.payload.contains("between turns") && !none.payload.contains("`task_message`"),
        "a runner with no reading must not be made to claim one: {}",
        none.payload
    );
}

#[test]
fn starting_a_subagent_does_not_wait_for_it() {
    use std::sync::Arc as StdArc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// A child that never finishes unless this test lets it, and that counts every time
    /// anybody waits on it.
    struct Slow {
        release: StdArc<AtomicBool>,
        collects: StdArc<AtomicUsize>,
    }
    impl TaskRunner for Slow {
        fn start(&self, _p: &str, _s: &TaskSpec) -> Result<String, String> {
            Ok("sub-slow".into())
        }
        fn collect(&self, handle: &str, timeout: std::time::Duration) -> TaskStatus {
            if handle != "sub-slow" {
                return TaskStatus::Unknown;
            }
            self.collects.fetch_add(1, Ordering::SeqCst);
            let deadline = std::time::Instant::now() + timeout;
            loop {
                if self.release.load(Ordering::SeqCst) {
                    return TaskStatus::Done {
                        answer: "the child answered".into(),
                        child: None,
                    };
                }
                if std::time::Instant::now() >= deadline {
                    return TaskStatus::Running {
                        note: Some("still going".into()),
                        child: None,
                    };
                }
                std::thread::yield_now();
            }
        }
        fn started(&self) -> Vec<String> {
            vec!["sub-slow".into()]
        }
    }

    let release = StdArc::new(AtomicBool::new(false));
    let collects = StdArc::new(AtomicUsize::new(0));
    let runner: Arc<dyn TaskRunner> = Arc::new(Slow {
        release: release.clone(),
        collects: collects.clone(),
    });
    let tool = TaskTool::new(runner.clone());
    let result = TaskResultTool::new(runner);

    let d = crate::backend::tempdir::TempDir::new();
    let backend = crate::backend::HostBackend::new(d.path()).unwrap();
    let mut reg = crate::runtime::Registry::new();
    reg.register(Box::new(tool)).unwrap();
    reg.register(Box::new(result)).unwrap();
    let mut rt = crate::runtime::ToolRuntime::new(reg, Box::new(backend));
    let mut sink = crate::NullToolSink;
    let call = |rt: &mut crate::runtime::ToolRuntime,
                sink: &mut crate::NullToolSink,
                name: &str,
                args: &str| {
        rt.invoke(
            "t1",
            &letibot_transcript::ToolCall {
                id: "c0".into(),
                name: name.into(),
                arguments: args.into(),
            },
            sink,
        )
    };

    // The child is nowhere near done, and `task` comes back anyway —
    // `Backgrounded`, not `Ok`: it is running and has not answered yet.
    //
    // "Did not wait" is asserted as a CAUSE, not a duration: `task` never asked the child
    // anything (`collect` is the only way to wait on it, and it ran zero times). This was
    // `elapsed() < 1s`, which a loaded box or a parallel `cargo test` can trip without the
    // code being wrong (R67 item 19) — and which a `task` that waited 900 ms would pass.
    let r = call(&mut rt, &mut sink, "task", r#"{"prompt": "a long job"}"#);
    assert_eq!(
        collects.load(Ordering::SeqCst),
        0,
        "`task` waited on the child it started"
    );
    assert!(
        matches!(
            r.outcome,
            letibot_transcript::ToolOutcome::Backgrounded { .. }
        ),
        "{:?}",
        r.outcome
    );
    assert!(r.payload.contains("task_result"), "{}", r.payload);

    // Collecting before it is done is the WAIT ending, not the child ending:
    // an abstention, which `is_grounded` will not treat as an answer.
    let waited = call(
        &mut rt,
        &mut sink,
        "task_result",
        r#"{"task": "sub-slow", "timeout_ms": 5}"#,
    );
    assert!(
        matches!(
            waited.outcome,
            letibot_transcript::ToolOutcome::Abstained { .. }
        ),
        "{:?}",
        waited.outcome
    );
    assert!(waited.payload.contains("still going"), "{}", waited.payload);

    release.store(true, Ordering::SeqCst);
    let done = call(&mut rt, &mut sink, "task_result", r#"{"task": "sub-slow"}"#);
    assert_eq!(done.payload, "the child answered");
    assert!(
        matches!(done.outcome, letibot_transcript::ToolOutcome::Ok),
        "{:?}",
        done.outcome
    );

    // A handle nobody started is a failure that says how to find the ones
    // that were.
    let miss = call(&mut rt, &mut sink, "task_result", r#"{"task": "sub-nope"}"#);
    assert!(matches!(
        miss.outcome,
        letibot_transcript::ToolOutcome::Failed { .. }
    ));
    let listed = call(&mut rt, &mut sink, "task_result", "{}");
    assert!(listed.payload.contains("sub-slow"), "{}", listed.payload);
}

/// **A child finishing enqueues its branch** — the seam the merge queue's enqueue hangs on,
/// driven through the tool with a fake runner.
///
/// The claim is the tool's half of `task_start`'s door: when a `task_start` child is seen
/// to finish, the tool asks its runner to enqueue the branch and reports what the runner
/// answered. The runner's own half — the row, the store, the brief — is
/// `harnessd::mergequeue::entry_for_finished`'s and is tested there; what this test holds is
/// that the ask happens at all, and that a plain `task` child does not trigger it.
#[test]
fn a_child_finishing_enqueues_its_branch() {
    use crate::runtime::{Registry, ToolRuntime};
    use letibot_transcript::ToolCall;

    /// A runner that answers `Done` and records the enqueue it was asked for.
    struct FinishedRunner {
        finished: Arc<std::sync::Mutex<Vec<String>>>,
        /// What `finished` answers with, so both arms can be driven.
        answer: Option<Result<Finished, String>>,
    }
    impl TaskRunner for FinishedRunner {
        fn start(&self, _p: &str, _s: &TaskSpec) -> Result<String, String> {
            Ok("sub-1".into())
        }
        fn collect(&self, _h: &str, _t: std::time::Duration) -> TaskStatus {
            TaskStatus::Done {
                answer: "the child answered".into(),
                child: None,
            }
        }
        fn start_worktree(
            &self,
            _p: &str,
            _s: &TaskSpec,
            w: &WorktreeSpec,
        ) -> Result<WorktreeHandle, String> {
            Ok(WorktreeHandle {
                handle: "sub-1".into(),
                placement: WorktreePlacement {
                    path: worktree_path("/repo", &w.slug),
                    branch: worktree_branch(&w.slug),
                    base_sha: "deadbeef".into(),
                    main_tree: false,
                },
            })
        }
        fn finished(&self, handle: &str) -> Result<Finished, String> {
            self.finished.lock().unwrap().push(handle.to_string());
            match &self.answer {
                Some(Ok(f)) => Ok(f.clone()),
                Some(Err(e)) => Err(e.clone()),
                None => Ok(Finished::NoBranch {
                    why: "not a task_start child".into(),
                }),
            }
        }
    }

    let d = crate::backend::tempdir::TempDir::new();
    let backend = crate::backend::HostBackend::new(d.path()).unwrap();
    let call = |runner: Arc<dyn TaskRunner>, args: &str| {
        let mut reg = Registry::new();
        reg.register(Box::new(TaskStartTool::new(runner.clone())))
            .unwrap();
        reg.register(Box::new(TaskResultTool::new(runner))).unwrap();
        let mut rt = ToolRuntime::new(reg, Box::new(backend.clone()));
        let mut sink = crate::NullToolSink;
        let _ = rt.invoke(
            "t1",
            &ToolCall {
                id: "c0".into(),
                name: "task_start".into(),
                arguments: r#"{"prompt": "do the work", "slug": "do-the-work"}"#.into(),
            },
            &mut sink,
        );
        rt.invoke(
            "t1",
            &ToolCall {
                id: "c1".into(),
                name: "task_result".into(),
                arguments: args.into(),
            },
            &mut sink,
        )
    };

    // **A `task_start` child's finish is reported.** The child answered, the runner was
    // asked to enqueue its branch, and the answer says where the branch is going.
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let r = call(
        Arc::new(FinishedRunner {
            finished: seen.clone(),
            answer: Some(Ok(Finished::Queued {
                id: "sub-1".into(),
                branch: "agent/do-the-work".into(),
            })),
        }),
        r#"{"task": "sub-1"}"#,
    );
    assert_eq!(
        *seen.lock().unwrap(),
        vec!["sub-1"],
        "the tool must ask its runner to enqueue the finished child's branch"
    );
    assert!(r.payload.starts_with("the child answered"), "{}", r.payload);
    assert!(r.payload.contains("agent/do-the-work"), "{}", r.payload);
    assert!(r.payload.contains("merge queue"), "{}", r.payload);

    // **A runner with no branch to land says nothing extra.** The ordinary case for a plain
    // `task` child, and the reason every existing `task_result` answer reads as it did.
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let r = call(
        Arc::new(FinishedRunner {
            finished: seen.clone(),
            answer: None,
        }),
        r#"{"task": "sub-1"}"#,
    );
    assert_eq!(*seen.lock().unwrap(), vec!["sub-1"]);
    assert_eq!(r.payload, "the child answered");

    // **A refusal is SAID, not swallowed.** The branch is not pushed and nobody else carries
    // it, so a parent told nothing would believe the work was on its way to main.
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let r = call(
        Arc::new(FinishedRunner {
            finished: seen.clone(),
            answer: Some(Err("the store could not be opened".into())),
        }),
        r#"{"task": "sub-1"}"#,
    );
    assert!(r.payload.contains("was NOT enqueued"), "{}", r.payload);
    assert!(r.payload.contains("could not be opened"), "{}", r.payload);
}

/// **A runner that arranges nothing answers `NoBranch`, and that is not a failure.**
///
/// `NoTaskRunner` is the default seat, and a `task_result` against it must not grow a line
/// about a merge queue the session has no door to.
#[test]
fn a_runner_with_no_worktree_leaves_no_branch() {
    match NoTaskRunner.finished("anything").unwrap() {
        Finished::NoBranch { why } => assert!(why.contains("no worktree"), "{why}"),
        other => panic!("a runner that arranges nothing must answer NoBranch: {other:?}"),
    }
}

#[test]
fn a_runner_is_delegated_to_and_no_runner_refuses() {
    let e = Echo::default();
    let h = e.start("find the bug", &spec("coder")).unwrap();
    assert_eq!(
        e.collect(&h, std::time::Duration::ZERO),
        TaskStatus::Done {
            answer: "coder [none] @host: find the bug".into(),
            child: None,
        }
    );
    assert!(
        NoTaskRunner
            .start("x", &spec("coder"))
            .unwrap_err()
            .contains("no subagent runner")
    );
    assert_eq!(
        NoTaskRunner.collect("anything", std::time::Duration::ZERO),
        TaskStatus::Unknown
    );
}

#[test]
fn a_downgrade_only_removes_and_the_union_keeps_every_denial() {
    let survey = Downgrade::parse("read-only").unwrap();
    assert!(
        survey.denies(Access::Write)
            && survey.denies(Access::Exec)
            && survey.denies(Access::Network)
    );
    assert!(!survey.denies(Access::Read) && !survey.denies(Access::Session));
    assert_eq!(survey.describe(), "no write, no exec, no network");
    let nw = Downgrade::parse("no-write, no-network").unwrap();
    assert_eq!(nw.describe(), "no write, no network");
    // A parent that cannot write cannot hand a child writes back by asking for
    // less: the union is what the child gets.
    assert_eq!(nw.and(&Downgrade::parse("no-exec").unwrap()), survey);
    assert!(
        Downgrade::parse("no-read")
            .unwrap_err()
            .contains("not a subagent")
    );
    assert!(
        Downgrade::parse("sudo")
            .unwrap_err()
            .contains("not a downgrade")
    );
    assert!(Downgrade::parse("").unwrap().is_none());
}

/// **A placement round-trips through the string a recorded spec carries.**
///
/// `RecordedSpec` writes `placement.as_str()` and reads it back with `Placement::parse` when a
/// child is resumed, so the two spellings are one fact: a name that changed on the way through
/// would resume a child somewhere its record does not say.
#[test]
fn a_placement_round_trips_and_a_host_keeps_its_case() {
    for p in [
        Placement::Host,
        Placement::Firecode,
        Placement::Remote("lab2x1".into()),
    ] {
        assert_eq!(
            Placement::parse(&p.as_str()),
            Ok(p.clone()),
            "{}",
            p.as_str()
        );
    }
    // The keyword is matched case-insensitively (`where: Firecode` is what a model writes);
    // the NAME is not — a hostname keeps the case it was declared with.
    assert_eq!(
        Placement::parse("Host:Lab2X1"),
        Ok(Placement::Remote("Lab2X1".into()))
    );
    assert!(
        Placement::parse("host:")
            .unwrap_err()
            .contains("needs a name")
    );
    assert_eq!(
        Placement::Remote("lab2x1".into()).host(),
        Some("lab2x1"),
        "the name is readable without matching the variant"
    );
    assert_eq!(Placement::Host.host(), None);
}

#[test]
fn the_tool_parses_access_and_where_and_refuses_the_unknown() {
    use crate::runtime::{Registry, ToolRuntime};
    use letibot_transcript::ToolCall;
    // A host that has firecode: the tool is told the CLI is a program that is there (the
    // refusal for a host that has none is the end of this test). Named to the tool, not
    // put in `$FIRECODE_BIN`: a `set_var` here was read by every other test in this binary
    // that looked at firecode while this one ran (R67 item 17).
    let stub = std::env::temp_dir().join(format!("letibot-firecode-stub-{}", std::process::id()));
    crate::testing::write_stub(&stub, "#!/bin/sh\n");
    let runner: Arc<dyn TaskRunner> = Arc::new(Echo::default());
    let d = crate::backend::tempdir::TempDir::new();
    let runtime = |firecode: &std::path::Path| {
        let mut reg = Registry::new();
        reg.register(Box::new(
            TaskTool::new(runner.clone()).with_firecode_bin(firecode),
        ))
        .unwrap();
        reg.register(Box::new(TaskResultTool::new(runner.clone())))
            .unwrap();
        let backend = crate::backend::HostBackend::new(d.path()).unwrap();
        ToolRuntime::new(reg, Box::new(backend))
    };
    let mut rt = runtime(&stub);
    let mut sink = crate::NullToolSink;
    let run = |rt: &mut ToolRuntime, sink: &mut crate::NullToolSink, args: &str| {
        rt.invoke(
            "t1",
            &ToolCall {
                id: "c0".into(),
                name: "task".into(),
                arguments: args.into(),
            },
            sink,
        )
        .payload
    };
    // `task` hands back a handle; the spec it parsed is what the subagent was
    // started with, and `task_result` is where the answer comes from.
    let started = run(
        &mut rt,
        &mut sink,
        r#"{"prompt": "survey the crates", "role": "researcher", "access": "read-only", "where": "firecode"}"#,
    );
    assert!(started.contains("started subagent `sub-1`"), "{started}");
    assert!(started.contains("survey the crates"), "{started}");
    assert!(started.contains("task_result"), "{started}");
    let collected = rt
        .invoke(
            "t1",
            &ToolCall {
                id: "c1".into(),
                name: "task_result".into(),
                arguments: r#"{"task": "sub-1"}"#.into(),
            },
            &mut sink,
        )
        .payload;
    assert_eq!(
        collected, "researcher [no write, no exec, no network] @firecode: survey the crates",
        "the spec `task` parsed is the spec the subagent ran under"
    );
    // An acceptance check is the harness's to run, after the child, in the child's VM - so
    // it is taken with `where: firecode` and refused without one, never silently dropped.
    let refused = run(
        &mut rt,
        &mut sink,
        r#"{"prompt": "run the suite", "verify": "make e2e"}"#,
    );
    assert!(refused.contains("Add `where: firecode`"), "{refused}");
    assert!(refused.contains("Nothing was spawned"), "{refused}");
    let gated = run(
        &mut rt,
        &mut sink,
        r#"{"prompt": "run the suite", "where": "firecode", "verify": "make e2e"}"#,
    );
    assert!(gated.contains("started subagent"), "{gated}");
    // **`repo` names a VM's copy**, and is refused for a child on the host — the operator: *"VMs
    // are per repo"*. With `where: firecode` the spec carries it, resolved by the spawn.
    let refused = run(
        &mut rt,
        &mut sink,
        r#"{"prompt": "run the suite", "repo": "stroppy"}"#,
    );
    assert!(refused.contains("drop `repo`"), "{refused}");
    let placed = run(
        &mut rt,
        &mut sink,
        r#"{"prompt": "run the suite", "where": "firecode", "repo": "stroppy"}"#,
    );
    assert!(placed.contains("started subagent"), "{placed}");
    let p = run(&mut rt, &mut sink, r#"{"prompt": "x", "access": "root"}"#);
    assert!(p.contains("not a downgrade"), "{p}");
    let p = run(&mut rt, &mut sink, r#"{"prompt": "x", "where": "moon"}"#);
    assert!(p.contains("not a placement"), "{p}");
    // **A host with no firecode refuses the VM at the call**, with nothing spawned and
    // `host` named — not a child that dies at its start. The operator's Mac, 2026-10-08.
    let mut rt = runtime(std::path::Path::new("/nonexistent/firecode"));
    let p = run(&mut rt, &mut sink, r#"{"prompt": "x", "where": "vm"}"#);
    let _ = std::fs::remove_file(&stub);
    assert!(p.contains("cannot be honoured"), "{p}");
    assert!(p.contains("Nothing was spawned"), "{p}");
    assert!(p.contains("`host`"), "{p}");
    assert!(!p.contains("started subagent `sub-2`"), "{p}");
}

/// **The seam line for a VM placement says where the work comes back** — FIRECODE-NOTES:
/// *"It says … 'the work comes back as the branch, not as the directory.' There is no branch
/// on this path — the work comes back as a sibling directory. A confidently wrong line in a
/// tool's own output is worse than no line."*
#[test]
fn a_vm_placement_does_not_promise_a_branch() {
    let line = placement_seam(&Placement::Firecode);
    // And the remote seam is the other way round: a branch, and it says which one.
    let remote = placement_seam(&Placement::Remote("lab2x1".into()));
    assert!(remote.contains("remote/lab2x1/"), "{remote}");
    assert!(remote.contains("checkout"), "{remote}");
    assert!(!remote.contains("directory beside"), "{remote}");
    assert!(!line.contains("comes back as the branch"), "{line}");
    assert!(line.contains("directory beside"), "{line}");
}

/// **The slug is short, kebab, and deterministic in the prompt.**
///
/// The derivation is the default, not a lock: two different subtasks get two
/// different slugs, and the same subtask asked twice gets the same slug — which
/// is what makes the refusal of an existing path meaningful. Pure, so it is
/// tested without a repo.
#[test]
fn the_slug_is_short_kebab_and_deterministic() {
    assert_eq!(
        slug_from_prompt("add a new tool task_start"),
        "add-a-new-tool"
    );
    assert_eq!(
        slug_from_prompt("Fix the bug in the parser"),
        "fix-the-bug-in"
    );
    // Non-alphanumerics collapse to a single hyphen.
    assert_eq!(
        slug_from_prompt("hello, world! foo_bar"),
        "hello-world-foo-bar"
    );
    // The same prompt gives the same slug.
    assert_eq!(
        slug_from_prompt("a long prompt that goes on and on"),
        slug_from_prompt("a long prompt that goes on and on")
    );
    // A different prompt gives a different slug.
    assert_ne!(
        slug_from_prompt("a long prompt that goes on and on"),
        slug_from_prompt("a different prompt that goes elsewhere")
    );
    // Capped at a length that keeps the path legible.
    let long =
        slug_from_prompt("one two three four five six seven eight nine ten eleven twelve thirteen");
    assert!(long.chars().count() <= 40, "{long}");
    assert!(!long.ends_with('-'), "{long}");
    // An empty prompt gets a default rather than an empty slug.
    assert_eq!(slug_from_prompt(""), "task");
    assert_eq!(slug_from_prompt("   "), "task");
}

/// **The worktree path and branch are pure functions of the workspace and slug.**
#[test]
fn the_worktree_path_and_branch_are_pure() {
    assert_eq!(
        worktree_path("/repo", "task-start"),
        "/repo/.claude/worktrees/agent-task-start"
    );
    assert_eq!(worktree_branch("task-start"), "agent/task-start");
}

/// **The brief names the tree, the branch, and the rule** — the report of 2026-10-12: *"the
/// pattern is now so routine that it belongs in the brief rather than in the salvage."*
#[test]
fn the_worktree_brief_says_the_branch_is_the_deliverable() {
    let b = worktree_brief("/repo/.claude/worktrees/agent-x", "agent/x");
    assert!(b.contains("`/repo/.claude/worktrees/agent-x`"), "{b}");
    assert!(b.contains("`agent/x`"), "{b}");
    assert!(b.contains("the branch is the deliverable"), "{b}");
    assert!(
        b.contains("commit your work to it as you go and before you finish"),
        "{b}"
    );
    assert!(b.contains("`git status --porcelain` prints nothing"), "{b}");
    assert!(b.contains("names the commit(s)"), "{b}");
}

/// **A `task_start` whose path already exists is refused by name, and nothing is
/// spawned.**
///
/// The default is a fresh worktree, and a path that already exists — a directory,
/// or a worktree a previous `task_start` made — is refused here rather than by a
/// child that was never started. The refusal names the path and says nothing was
/// spawned.
#[test]
fn an_existing_path_is_refused_by_name_and_nothing_is_spawned() {
    use crate::runtime::{Registry, ToolRuntime};
    use letibot_transcript::ToolCall;

    /// A runner that records whether it was asked to arrange a worktree.
    struct PlacedRunner {
        asked: Arc<std::sync::atomic::AtomicBool>,
    }
    impl TaskRunner for PlacedRunner {
        fn start(&self, _p: &str, _s: &TaskSpec) -> Result<String, String> {
            Ok("sub-1".into())
        }
        fn collect(&self, _h: &str, _t: std::time::Duration) -> TaskStatus {
            TaskStatus::Unknown
        }
        fn start_worktree(
            &self,
            _p: &str,
            _s: &TaskSpec,
            _w: &WorktreeSpec,
        ) -> Result<WorktreeHandle, String> {
            self.asked.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(WorktreeHandle {
                handle: "sub-1".into(),
                placement: WorktreePlacement {
                    path: "/nowhere".into(),
                    branch: "agent/x".into(),
                    base_sha: "abc".into(),
                    main_tree: false,
                },
            })
        }
    }

    let d = crate::backend::tempdir::TempDir::new();
    // The workspace in a folder of its own: where worktrees go depends on the
    // workspace's siblings (`worktree_home`), and the temp dir's siblings are the
    // other tests' repositories, made and removed while this one runs.
    let ws = d.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    // The backend canonicalises its root, so the workspace the tool sees is the
    // canonicalised path — use the same one to compute the path the tool checks.
    let workspace = ws.canonicalize().unwrap().to_string_lossy().to_string();
    // Create the path the tool would try to use, so the refusal fires.
    let slug = "existing";
    let path = worktree_path(&workspace, slug);
    std::fs::create_dir_all(&path).unwrap();

    let asked = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let runner: Arc<dyn TaskRunner> = Arc::new(PlacedRunner {
        asked: asked.clone(),
    });
    let mut reg = Registry::new();
    reg.register(Box::new(TaskStartTool::new(runner))).unwrap();
    let backend = crate::backend::HostBackend::new(&ws).unwrap();
    let mut rt = ToolRuntime::new(reg, Box::new(backend));
    let mut sink = crate::NullToolSink;
    let r = rt.invoke(
        "t1",
        &ToolCall {
            id: "c1".into(),
            name: "task_start".into(),
            arguments: format!(r#"{{"prompt": "do the work", "slug": "{slug}"}}"#).into(),
        },
        &mut sink,
    );
    assert!(
        matches!(r.outcome, letibot_transcript::ToolOutcome::Failed { .. }),
        "{:?}",
        r.outcome
    );
    assert!(r.payload.contains(&path), "{}", r.payload);
    assert!(
        r.payload.contains("already a directory or a worktree"),
        "{}",
        r.payload
    );
    // The runner was never asked to arrange a worktree: the refusal happened
    // before the spawn.
    assert!(
        !asked.load(std::sync::atomic::Ordering::SeqCst),
        "the runner was asked to arrange a worktree for a path that already exists"
    );
}

/// **The placement is in the answer**: the handle names the path, the branch and
/// the base SHA, so the caller and the operator see where the child is working
/// without re-reading the brief.
#[test]
fn the_placement_is_in_the_answer() {
    use crate::runtime::{Registry, ToolRuntime};
    use letibot_transcript::ToolCall;

    /// A runner that returns a known placement, so the test is about the tool's
    /// answer and not about git.
    struct KnownPlaced;
    impl TaskRunner for KnownPlaced {
        fn start(&self, _p: &str, _s: &TaskSpec) -> Result<String, String> {
            Ok("sub-1".into())
        }
        fn collect(&self, _h: &str, _t: std::time::Duration) -> TaskStatus {
            TaskStatus::Unknown
        }
        fn start_worktree(
            &self,
            _p: &str,
            _s: &TaskSpec,
            w: &WorktreeSpec,
        ) -> Result<WorktreeHandle, String> {
            let path = worktree_path("/repo", &w.slug);
            Ok(WorktreeHandle {
                handle: "sub-1".into(),
                placement: WorktreePlacement {
                    path: path.clone(),
                    branch: worktree_branch(&w.slug),
                    base_sha: "deadbeef".into(),
                    main_tree: w.main_tree,
                },
            })
        }
    }

    let d = crate::backend::tempdir::TempDir::new();
    let runner: Arc<dyn TaskRunner> = Arc::new(KnownPlaced);
    let mut reg = Registry::new();
    reg.register(Box::new(TaskStartTool::new(runner))).unwrap();
    let backend = crate::backend::HostBackend::new(d.path()).unwrap();
    let mut rt = ToolRuntime::new(reg, Box::new(backend));
    let mut sink = crate::NullToolSink;
    let r = rt.invoke(
        "t1",
        &ToolCall {
            id: "c1".into(),
            name: "task_start".into(),
            arguments: r#"{"prompt": "do the work", "slug": "task-start"}"#.into(),
        },
        &mut sink,
    );
    assert!(
        matches!(
            r.outcome,
            letibot_transcript::ToolOutcome::Backgrounded { .. }
        ),
        "{:?}",
        r.outcome
    );
    // The path, branch and base SHA are all in the answer.
    assert!(
        r.payload
            .contains("/repo/.claude/worktrees/agent-task-start"),
        "{}",
        r.payload
    );
    assert!(r.payload.contains("agent/task-start"), "{}", r.payload);
    assert!(r.payload.contains("deadbeef"), "{}", r.payload);
    // The branch is the deliverable and is not pushed.
    assert!(r.payload.contains("NOT pushed"), "{}", r.payload);
    // A child in the wrong place is corrected, not killed.
    assert!(r.payload.contains("task_message"), "{}", r.payload);
}

/// **The main tree is the exception, and it is named as such in the answer.**
#[test]
fn the_main_tree_is_named_as_the_exception() {
    use crate::runtime::{Registry, ToolRuntime};
    use letibot_transcript::ToolCall;

    struct MainTreePlaced;
    impl TaskRunner for MainTreePlaced {
        fn start(&self, _p: &str, _s: &TaskSpec) -> Result<String, String> {
            Ok("sub-1".into())
        }
        fn collect(&self, _h: &str, _t: std::time::Duration) -> TaskStatus {
            TaskStatus::Unknown
        }
        fn start_worktree(
            &self,
            _p: &str,
            _s: &TaskSpec,
            w: &WorktreeSpec,
        ) -> Result<WorktreeHandle, String> {
            Ok(WorktreeHandle {
                handle: "sub-1".into(),
                placement: WorktreePlacement {
                    path: "/repo".into(),
                    branch: "main".into(),
                    base_sha: "cafe0000".into(),
                    main_tree: w.main_tree,
                },
            })
        }
    }

    let d = crate::backend::tempdir::TempDir::new();
    let runner: Arc<dyn TaskRunner> = Arc::new(MainTreePlaced);
    let mut reg = Registry::new();
    reg.register(Box::new(TaskStartTool::new(runner))).unwrap();
    let backend = crate::backend::HostBackend::new(d.path()).unwrap();
    let mut rt = ToolRuntime::new(reg, Box::new(backend));
    let mut sink = crate::NullToolSink;
    let r = rt.invoke(
        "t1",
        &ToolCall {
            id: "c1".into(),
            name: "task_start".into(),
            arguments: r#"{"prompt": "do the work", "main_tree": true}"#.into(),
        },
        &mut sink,
    );
    assert!(
        matches!(
            r.outcome,
            letibot_transcript::ToolOutcome::Backgrounded { .. }
        ),
        "{:?}",
        r.outcome
    );
    // The main tree is named as the exception.
    assert!(
        r.payload.contains("the main checkout (the exception"),
        "{}",
        r.payload
    );
}

/// **R93.F21/F22: the schemas take what the tools read.** `task` read `model` with no property
/// for it, and `task_start` promised a delivery `wake` decides with no `wake` to pass.
#[test]
fn task_and_task_start_schemas_name_model_and_wake() {
    let runner: Arc<dyn TaskRunner> = Arc::new(NoTaskRunner);
    for s in [
        TaskTool::new(runner.clone()).schema(),
        TaskStartTool::new(runner.clone()).schema(),
    ] {
        let names = s.param_names();
        assert!(names.contains(&"model"), "{}: {names:?}", s.name);
        assert!(names.contains(&"wake"), "{}: {names:?}", s.name);
        assert!(
            s.description.contains(crate::builtins::DELIVERED),
            "{} says how the answer arrives the one way: {}",
            s.name,
            s.description
        );
    }
}

/// **A named slug that cannot name a directory and a branch is refused** (TODO R92.17): `/`,
/// `..` and whitespace were only trimmed.
#[test]
fn a_slug_with_a_slash_dots_or_spaces_is_refused() {
    for bad in [
        "a/b",
        "../escape",
        "has space",
        "-x",
        "x.lock",
        "a~b",
        "tab\there",
    ] {
        assert!(slug_refusal(bad).is_some(), "{bad:?} should be refused");
    }
    for good in ["fix-the-gate", "r92", "agent-refresh-2"] {
        assert_eq!(slug_refusal(good), None, "{good:?}");
    }
    let why = slug_refusal("../x").unwrap();
    assert!(why.contains("`..`"), "{why}");
}

/// **A derived slug that is taken becomes the next free one** (TODO R92.17) — `-2`, `-3`.
#[test]
fn a_taken_derived_slug_gets_the_next_free_suffix() {
    let taken = ["fix-the-gate", "fix-the-gate-2"];
    assert_eq!(
        unique_slug("fix-the-gate", |s| taken.contains(&s)),
        "fix-the-gate-3"
    );
    assert_eq!(unique_slug("free", |_| false), "free");
}

/// **A runner that keeps names** — three children of this session (two of them sharing a name),
/// one grandchild elsewhere in the tree, and a log of every handle the tools asked it about.
/// `start` names its child the way the daemon does: the spec's slug, else the prompt's, made
/// unique among the names it holds.
#[derive(Default)]
struct Named {
    kids: std::sync::Mutex<Vec<ChildName>>,
    asked: std::sync::Mutex<Vec<String>>,
}

impl Named {
    fn family() -> Named {
        let kid = |handle: &str, slug: Option<&str>, own: bool| ChildName {
            handle: handle.into(),
            slug: slug.map(str::to_string),
            own,
        };
        Named {
            kids: std::sync::Mutex::new(vec![
                kid("s-1-sub-1", Some("fix-login"), true),
                kid("s-1-sub-2", Some("bench"), true),
                kid("s-1-sub-3", Some("bench"), true),
                kid("s-1-sub-1-sub-1", Some("fix-login"), false),
                kid("s-1-sub-1-sub-2", Some("survey"), false),
                kid("s-1-sub-4", None, true),
            ]),
            asked: Default::default(),
        }
    }
}

impl TaskRunner for Named {
    fn start(&self, prompt: &str, spec: &TaskSpec) -> Result<String, String> {
        let mut kids = self.kids.lock().unwrap();
        let handle = format!("s-1-sub-{}", kids.len() + 1);
        let taken: Vec<String> = kids.iter().filter_map(|c| c.slug.clone()).collect();
        let slug = child_slug(spec.slug.as_deref(), prompt, |s| {
            taken.iter().any(|t| t == s)
        });
        kids.push(ChildName {
            handle: handle.clone(),
            slug: Some(slug),
            own: true,
        });
        Ok(handle)
    }
    fn collect(&self, handle: &str, _t: std::time::Duration) -> TaskStatus {
        self.asked.lock().unwrap().push(handle.to_string());
        if self.kids.lock().unwrap().iter().any(|c| c.handle == handle) {
            TaskStatus::Done {
                answer: format!("answer of {handle}"),
                child: None,
            }
        } else {
            TaskStatus::Unknown
        }
    }
    fn started(&self) -> Vec<String> {
        self.kids
            .lock()
            .unwrap()
            .iter()
            .map(|c| c.handle.clone())
            .collect()
    }
    fn children(&self) -> Vec<ChildName> {
        self.kids.lock().unwrap().clone()
    }
    fn send(&self, handle: &str, _text: &str) -> Result<String, String> {
        self.asked.lock().unwrap().push(handle.to_string());
        Ok(format!("`{handle}` was told"))
    }
    fn resume(&self, handle: &str) -> Result<String, String> {
        self.asked.lock().unwrap().push(handle.to_string());
        Ok(format!("`{handle}` is resumed"))
    }
    fn kill(&self, handle: &str) -> Result<String, String> {
        self.asked.lock().unwrap().push(handle.to_string());
        Ok(format!("stopped `{handle}`"))
    }
}

/// **A child is named by its handle or its slug — the job rule, case by case.** The handle
/// always wins; a name one child carries is that child; a name two of this session's children
/// carry is refused naming both; a name a grandchild shares with a child is the child's; a name
/// only a grandchild carries is the grandchild's; and anything else is handed on unchanged, so the
/// runner's own refusal (or a peer session's id) reaches it as it was written.
#[test]
fn a_child_resolves_by_id_or_slug_and_an_ambiguous_name_is_refused() {
    let kids = Named::family().children();
    // The id.
    assert_eq!(
        pick_child(&kids, "s-1-sub-2"),
        PickedChild::One("s-1-sub-2".into())
    );
    // The slug, among this session's own children first: the grandchild's `fix-login` does not
    // make the child's ambiguous.
    assert_eq!(
        pick_child(&kids, "fix-login"),
        PickedChild::One("s-1-sub-1".into())
    );
    // A name only somewhere else in the tree still names that one.
    assert_eq!(
        pick_child(&kids, " survey "),
        PickedChild::One("s-1-sub-1-sub-2".into())
    );
    // Two of this session's children answer to `bench`: neither is guessed.
    let PickedChild::Ambiguous(two) = pick_child(&kids, "bench") else {
        panic!("a shared name must not resolve");
    };
    let handles: Vec<&str> = two.iter().map(|c| c.handle.as_str()).collect();
    assert_eq!(handles, ["s-1-sub-2", "s-1-sub-3"]);
    // Unknown, and empty.
    assert_eq!(pick_child(&kids, "no-such"), PickedChild::Unknown);
    assert_eq!(pick_child(&kids, "  "), PickedChild::Unknown);

    // The resolver over it: the refusal names the name as it was asked and lists both by label.
    let refused = resolve_child_in(&kids, "bench").expect_err("two children carry it");
    let letibot_transcript::ToolOutcome::Failed { reason } = &refused.outcome else {
        panic!("a refusal is a failure: {:?}", refused.outcome);
    };
    assert!(
        reason.contains("`bench` is the name of 2 subagents"),
        "{reason}"
    );
    assert!(
        refused.payload.contains("bench · s-1-sub-2"),
        "{}",
        refused.payload
    );
    assert!(
        refused.payload.contains("bench · s-1-sub-3"),
        "{}",
        refused.payload
    );
    // An unknown string goes on unchanged — the runner answers for it.
    assert_eq!(resolve_child_in(&kids, "s-peer").unwrap(), "s-peer");
}

/// **A child's label is the job's spelling, and it reads back.** `slug · handle`, the bare handle
/// for a child nobody named, and the split recovers both — and refuses to read a `·` that has no
/// child's handle after it.
#[test]
fn a_childs_label_is_its_slug_and_handle_and_splits_back() {
    assert_eq!(
        subagent_label(Some("fix-login"), "s-1-sub-3"),
        "fix-login · s-1-sub-3"
    );
    assert_eq!(subagent_label(None, "s-1-sub-3"), "s-1-sub-3");
    assert_eq!(subagent_label(Some("  "), "s-1-sub-3"), "s-1-sub-3");
    assert_eq!(
        split_subagent_label("fix-login · s-1-sub-3"),
        (Some("fix-login"), "s-1-sub-3")
    );
    assert_eq!(split_subagent_label("s-1-sub-3"), (None, "s-1-sub-3"));
    assert_eq!(
        split_subagent_label("release-build · j65"),
        (None, "release-build · j65")
    );
    // A default runner keeps no names and labels by the handle.
    assert_eq!(Echo::default().label_of("sub-1"), "sub-1");
}

/// **A plain `task` child's name**: the one given, else the prompt's, unique among its siblings.
#[test]
fn a_task_childs_slug_is_given_or_derived_and_made_unique() {
    let taken = ["fix-the-login".to_string(), "bench".to_string()];
    let is_taken = |s: &str| taken.iter().any(|t| t == s);
    assert_eq!(
        child_slug(None, "Fix the login page.\nMore.", is_taken),
        "fix-the-login-page"
    );
    assert_eq!(child_slug(Some("bench"), "whatever", is_taken), "bench-2");
    assert_eq!(child_slug(Some(" "), "Bench it", is_taken), "bench-it");
}

/// **The tools take the slug where they took the handle, and say the name back.** `task` names
/// the child it started by its label; `task_result`, `task_message` and `task_resume` reach the
/// runner with the HANDLE the slug stands for; a shared name reaches nothing; and `task_result`'s
/// listing names every child by its label.
#[test]
fn the_task_tools_take_a_slug_and_name_the_child_by_it() {
    use crate::runtime::{Registry, ToolRuntime};
    use letibot_transcript::{ToolCall, ToolOutcome};

    let runner = Arc::new(Named::family());
    let mut reg = Registry::new();
    reg.register(Box::new(TaskTool::new(runner.clone())))
        .unwrap();
    reg.register(Box::new(TaskResultTool::new(runner.clone())))
        .unwrap();
    reg.register(Box::new(TaskMessageTool::new(runner.clone())))
        .unwrap();
    reg.register(Box::new(TaskResumeTool::new(runner.clone())))
        .unwrap();
    let d = crate::backend::tempdir::TempDir::new();
    let backend = crate::backend::HostBackend::new(d.path()).unwrap();
    let mut rt = ToolRuntime::new(reg, Box::new(backend));
    let mut sink = crate::NullToolSink;
    let mut call = |name: &str, args: &str| {
        rt.invoke(
            "t1",
            &ToolCall {
                id: "c1".into(),
                name: name.into(),
                arguments: args.into(),
            },
            &mut sink,
        )
    };

    // `task` with a name already taken: the next free one, and the result says it.
    let started = call(
        "task",
        r#"{"prompt": "run the benchmarks", "slug": "bench"}"#,
    );
    assert!(
        started
            .payload
            .contains("started subagent `bench-2 · s-1-sub-7`"),
        "{}",
        started.payload
    );
    // `task` with no name: the prompt's.
    let started = call("task", r#"{"prompt": "Audit the store schema"}"#);
    assert!(
        started
            .payload
            .contains("started subagent `audit-the-store-schema · s-1-sub-8`"),
        "{}",
        started.payload
    );
    // A name that cannot be one is refused before anything spawns.
    let bad = call("task", r#"{"prompt": "x", "slug": "a/b"}"#);
    assert!(
        matches!(bad.outcome, ToolOutcome::Failed { .. }),
        "{:?}",
        bad.outcome
    );
    assert_eq!(runner.kids.lock().unwrap().len(), 8, "nothing was spawned");

    runner.asked.lock().unwrap().clear();
    let got = call("task_result", r#"{"task": "fix-login"}"#);
    assert!(
        got.payload.contains("answer of s-1-sub-1"),
        "{}",
        got.payload
    );
    let told = call("task_message", r#"{"task": "bench-2", "text": "hurry"}"#);
    assert!(
        told.payload.contains("`s-1-sub-7` was told"),
        "{}",
        told.payload
    );
    let back = call("task_resume", r#"{"task": "survey"}"#);
    assert!(
        matches!(back.outcome, ToolOutcome::Backgrounded { .. }),
        "{:?}",
        back.outcome
    );
    assert_eq!(
        runner.asked.lock().unwrap().as_slice(),
        ["s-1-sub-1", "s-1-sub-7", "s-1-sub-1-sub-2"],
        "each tool reached the runner with the handle the name stands for"
    );

    // A shared name reaches nothing, and says which children carry it.
    runner.asked.lock().unwrap().clear();
    for (tool, args) in [
        ("task_result", r#"{"task": "bench"}"#),
        ("task_message", r#"{"task": "bench", "text": "x"}"#),
        ("task_resume", r#"{"task": "bench"}"#),
    ] {
        let refused = call(tool, args);
        assert!(
            matches!(refused.outcome, ToolOutcome::Failed { .. }),
            "{tool}: {:?}",
            refused.outcome
        );
        assert!(
            refused.payload.contains("bench · s-1-sub-2")
                && refused.payload.contains("bench · s-1-sub-3"),
            "{tool}: {}",
            refused.payload
        );
    }
    assert!(
        runner.asked.lock().unwrap().is_empty(),
        "no child was touched"
    );

    // The listing names each child by its label; one nobody named by its handle alone.
    let list = call("task_result", "{}");
    assert!(
        list.payload.contains("  fix-login · s-1-sub-1 — answered"),
        "{}",
        list.payload
    );
    assert!(
        list.payload.contains("  s-1-sub-4 — answered"),
        "{}",
        list.payload
    );
}

/// **A worktree lives beside its repository when the layout says where** — the operator's rule:
/// `~/Projects/<name>/<name>` keeps them in `~/Projects/<name>/worktrees`, an org folder of several
/// repositories in its own `worktrees/<repo>`, anything else in `.claude/worktrees` as before.
#[test]
fn a_worktree_goes_where_the_layout_says() {
    let root = std::env::temp_dir().join(format!("letibot-wthome-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let named = root.join("letibot/letibot");
    std::fs::create_dir_all(named.join(".git")).unwrap();
    assert_eq!(
        worktree_path(named.to_str().unwrap(), "fix"),
        format!("{}/letibot/worktrees/agent-fix", root.display())
    );
    let org = root.join("stroppy-io");
    for r in ["stroppy", "stroppy-io.github.io"] {
        std::fs::create_dir_all(org.join(r).join(".git")).unwrap();
    }
    assert_eq!(
        worktree_path(org.join("stroppy").to_str().unwrap(), "fix"),
        format!("{}/stroppy-io/worktrees/stroppy/agent-fix", root.display())
    );
    let lone = root.join("solo/tool");
    std::fs::create_dir_all(lone.join(".git")).unwrap();
    assert_eq!(
        worktree_path(lone.to_str().unwrap(), "fix"),
        format!("{}/.claude/worktrees/agent-fix", lone.display()),
        "any other layout keeps them inside the repository"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// **`where: "host:NAME"` names a machine the operator declared, or it names nothing.**
///
/// The refusal is the interesting half: a name that is not in the file must spawn nothing
/// and must say which names the file *does* have, because "no host `lab2`" when the file
/// says `lab2x1` is a refusal the caller cannot act on.
#[test]
fn a_remote_placement_needs_a_host_the_operator_declared() {
    use crate::runtime::{Registry, ToolRuntime};
    use letibot_transcript::ToolCall;

    let hosts = crate::remote::Hosts::parse(
        "[host.lab2x1]\nhostname = \"lab2x1\"\nuser = \"dead\"\nroot = \"~/letibot-remote\"\n",
    )
    .expect("it parses");
    let runner: Arc<dyn TaskRunner> = Arc::new(Echo::default());
    let mut reg = Registry::new();
    reg.register(Box::new(TaskTool::new(runner.clone()).with_hosts(hosts)))
        .unwrap();
    reg.register(Box::new(TaskResultTool::new(runner.clone())))
        .unwrap();
    let d = crate::backend::tempdir::TempDir::new();
    let backend = crate::backend::HostBackend::new(d.path()).unwrap();
    let mut rt = ToolRuntime::new(reg, Box::new(backend));
    let mut sink = crate::NullToolSink;
    let run = |rt: &mut ToolRuntime, sink: &mut crate::NullToolSink, tool: &str, args: &str| {
        rt.invoke(
            "t1",
            &ToolCall {
                id: "c0".into(),
                name: tool.into(),
                arguments: args.into(),
            },
            sink,
        )
        .payload
    };

    // A host that is there: the child is started, and the spec carries which one.
    let placed = run(
        &mut rt,
        &mut sink,
        "task",
        r#"{"prompt": "run the suite", "where": "host:lab2x1"}"#,
    );
    assert!(placed.contains("started subagent"), "{placed}");
    // What the child was actually started with, read back from the runner: the name is
    // in the spec, so a remote child is a child on that machine and not a host one.
    let collected = run(&mut rt, &mut sink, "task_result", r#"{"task": "sub-1"}"#);
    assert!(collected.contains("@host:lab2x1"), "{collected}");

    // A host that is not: nothing spawns, and the ones that are there are listed.
    let refused = run(
        &mut rt,
        &mut sink,
        "task",
        r#"{"prompt": "x", "where": "host:lab2"}"#,
    );
    assert!(refused.contains("lab2x1"), "{refused}");
    assert!(refused.contains("Nothing was spawned"), "{refused}");
    assert!(!refused.contains("started subagent `sub-2`"), "{refused}");

    // An empty name is its own sentence rather than a lookup of "".
    let empty = run(
        &mut rt,
        &mut sink,
        "task",
        r#"{"prompt": "x", "where": "host:"}"#,
    );
    assert!(empty.contains("needs a name"), "{empty}");
}

/// **`parent` is a HANDLE, not a name** — the reserved word a child answers its parent with.
///
/// Three facts, and each is a way this could be got wrong:
///
/// * a session that has a parent reaches it by the word, and what the runner is asked to say is
///   addressed to the parent's ID — the model never carries a thirty-character id;
/// * a session at the top of its tree is REFUSED by name, because there is nobody above it. The
///   alternative — resolving `parent` to nothing and letting the refusal come from `send` — would
///   say *"no session `parent` is live"*, which is a sentence about a session rather than about
///   the tree;
/// * **a child actually called `parent` does not shadow it.** The word is read before
///   [`resolve_child`]'s handles and slugs, so a child whose slug is `parent` is not reachable by
///   it: `parent` means the session that started this one, and nothing else.
#[test]
fn parent_is_a_reserved_handle_and_a_root_is_refused_by_name() {
    use crate::runtime::{Registry, ToolRuntime};
    use letibot_transcript::ToolCall;

    /// A runner with one child whose SLUG is `parent`, and a parent of its own.
    struct Named(Option<String>, std::sync::Mutex<Vec<(String, String)>>);

    impl TaskRunner for Named {
        fn start(&self, _p: &str, _s: &TaskSpec) -> Result<String, String> {
            Ok("sub-1".into())
        }
        fn collect(&self, _h: &str, _t: std::time::Duration) -> TaskStatus {
            TaskStatus::Unknown
        }
        fn children(&self) -> Vec<ChildName> {
            vec![ChildName {
                handle: "sub-a-child-called-parent".into(),
                slug: Some(PARENT_HANDLE.into()),
                own: true,
            }]
        }
        fn parent(&self) -> Option<String> {
            self.0.clone()
        }
        fn send(&self, handle: &str, text: &str) -> Result<String, String> {
            self.1
                .lock()
                .unwrap()
                .push((handle.to_string(), text.to_string()));
            Ok(format!("`{handle}` was told"))
        }
    }

    let d = crate::backend::tempdir::TempDir::new();
    let call = |runner: Arc<Named>, args: &str| {
        let mut reg = Registry::new();
        reg.register(Box::new(TaskMessageTool::new(runner.clone())))
            .unwrap();
        let backend = crate::backend::HostBackend::new(d.path()).unwrap();
        let mut rt = ToolRuntime::new(reg, Box::new(backend));
        let mut sink = crate::NullToolSink;
        rt.invoke(
            "t1",
            &ToolCall {
                id: "c1".into(),
                name: "task_message".into(),
                arguments: args.into(),
            },
            &mut sink,
        )
    };

    // A child of a session: the word resolves to the parent's id, not to the child named `parent`.
    let child = Arc::new(Named(
        Some("s-the-father".into()),
        std::sync::Mutex::new(Vec::new()),
    ));
    let said = call(child.clone(), r#"{"task": "parent", "text": "it is done"}"#);
    assert!(
        matches!(said.outcome, letibot_transcript::ToolOutcome::Ok),
        "{:?} — {}",
        said.outcome,
        said.payload
    );
    assert_eq!(
        child.1.lock().unwrap().as_slice(),
        [("s-the-father".to_string(), "it is done".to_string())],
        "`parent` addressed the parent's id, and the child called `parent` was not touched"
    );

    // A session at the top of its tree: refused, and the sentence is about the tree.
    let root = Arc::new(Named(None, std::sync::Mutex::new(Vec::new())));
    let refused = call(root.clone(), r#"{"task": "parent", "text": "hello"}"#);
    match &refused.outcome {
        // `Invocation::failed(reason, payload)` — the reason is the tool's one-line refusal and
        // the payload is the sentence the model reads, so both are checked where they live.
        letibot_transcript::ToolOutcome::Failed { reason } => assert!(
            reason.contains("no parent to speak to"),
            "the refusal names the tree: {reason}"
        ),
        other => panic!("a root was not refused: {other:?} — {}", refused.payload),
    }
    assert!(
        refused.payload.contains("nothing started this one"),
        "{}",
        refused.payload
    );
    assert!(
        root.1.lock().unwrap().is_empty(),
        "nothing was sent from a session with no parent"
    );

    // And any other handle still goes through the ordinary resolution: the slug `parent` is
    // reachable by its HANDLE, which is what a name that is reserved must not take away.
    let by_handle = call(
        child.clone(),
        r#"{"task": "sub-a-child-called-parent", "text": "and you"}"#,
    );
    assert!(
        matches!(by_handle.outcome, letibot_transcript::ToolOutcome::Ok),
        "{:?} — {}",
        by_handle.outcome,
        by_handle.payload
    );
    assert_eq!(
        child.1.lock().unwrap().last().cloned(),
        Some((
            "sub-a-child-called-parent".to_string(),
            "and you".to_string()
        ))
    );
}
