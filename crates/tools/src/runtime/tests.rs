use super::*;
use crate::events::RecordingToolSink;

struct Probe {
    access: Access,
}

impl Tool for Probe {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "probe",
            "Do a thing. Takes a path.",
            serde_json::json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"],
            }),
            self.access,
        )
    }
    fn invoke(&self, _ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        Invocation::ok(format!("probed {}", args["path"]))
    }
}

/// A declared skill's `allowed-tools` narrows the seat and cannot widen it: the tools it
/// does not name leave the registry, so the prompt does not announce them.
#[test]
fn only_tools_narrows_to_the_declared_names() {
    let mut reg = Registry::new();
    reg.register(Box::new(Probe {
        access: Access::Read,
    }))
    .unwrap();
    reg.register(Box::new(crate::builtins::bash::Bash::default()))
        .unwrap();
    let allowed: std::collections::BTreeSet<String> = ["probe".to_string()].into_iter().collect();
    let narrowed = reg.only_tools(&allowed);
    let names: Vec<String> = narrowed.schemas().iter().map(|s| s.name.clone()).collect();
    assert_eq!(names, vec!["probe".to_string()]);
}

/// A worker holds the runtime and asks its gate from its own thread, so the gate has to
/// cross a thread boundary and `Box<dyn Gate>` is only `Send` when the trait says so.
#[test]
fn a_runtime_and_its_gate_cross_a_thread_boundary() {
    fn movable<T: Send + ?Sized>() {}
    fn shareable<T: Sync + ?Sized>() {}
    movable::<Box<dyn Gate>>();
    shareable::<Box<dyn Gate>>();
    movable::<ToolRuntime>();
    shareable::<ToolRuntime>();
}

/// A tool that runs a command line, for the changed-file check: the real `scripts_of`
/// reads any call whose arguments carry a `command`, so the check is exercised through the
/// same path a `bash` call takes without needing a process host.
struct CommandProbe {
    ran: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Tool for CommandProbe {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "run",
            "Run a command line.",
            serde_json::json!({
                "type": "object",
                "properties": {"command": {"type": "string"}},
                "required": ["command"],
            }),
            Access::Exec,
        )
    }
    fn invoke(&self, _ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        self.ran.store(true, std::sync::atomic::Ordering::SeqCst);
        Invocation::ok(format!("ran {}", args["command"]))
    }
}

/// A gate that answers by changing the file it was asked about. That is exactly the
/// window the changed-file check exists for: the file is read, an answer is waited for,
/// and the answer takes time, reproduced in one process rather than described.
struct RewritingGate {
    path: std::path::PathBuf,
    to: String,
}

impl Gate for RewritingGate {
    fn admit(&mut self, _call: &GateCall<'_>) -> GateDecision {
        std::fs::write(&self.path, &self.to).unwrap();
        GateDecision::Admit
    }
}

/// A file the gate judged that changed before the command ran is refused, not run. A
/// here-document cannot have this window: its body is in the command text. A path can, and
/// the window is the operator's own thinking time.
#[test]
fn a_script_that_changed_while_the_gate_was_answering_is_refused() {
    let d = crate::backend::tempdir::TempDir::new();
    let script = d.path().join("deploy.py");
    std::fs::write(&script, "print('judged')\n").unwrap();

    let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut reg = Registry::new();
    reg.register(Box::new(CommandProbe { ran: ran.clone() }))
        .unwrap();
    let backend = crate::backend::HostBackend::new(d.path()).unwrap();
    let rt = ToolRuntime::new(reg, Box::new(backend)).with_gate(Box::new(RewritingGate {
        path: script.clone(),
        to: "print('swapped')\n".into(),
    }));
    let mut sink = RecordingToolSink::default();
    let r = rt.invoke(
        "t1",
        &call("run", r#"{"command":"python3 deploy.py"}"#),
        &mut sink,
    );
    assert!(
        !ran.load(std::sync::atomic::Ordering::SeqCst),
        "the command ran with a body nobody judged: {r:?}"
    );
    let payload = format!("{r:?}");
    assert!(
        payload.contains("changed between being classified and being run")
            && payload.contains("deploy.py")
            && payload.contains("Ask again"),
        "the refusal does not say what happened or what to do: {payload}"
    );
}

/// And the ordinary case is untouched: a file that did not change runs. Without this, the
/// test above would pass on a runtime that refused everything.
#[test]
fn a_script_that_did_not_change_is_not_refused() {
    let d = crate::backend::tempdir::TempDir::new();
    std::fs::write(d.path().join("deploy.py"), "print('judged')\n").unwrap();

    let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut reg = Registry::new();
    reg.register(Box::new(CommandProbe { ran: ran.clone() }))
        .unwrap();
    let backend = crate::backend::HostBackend::new(d.path()).unwrap();
    let rt = ToolRuntime::new(reg, Box::new(backend)).with_gate(Box::new(AdmitGate));
    let mut sink = RecordingToolSink::default();
    let r = rt.invoke(
        "t1",
        &call("run", r#"{"command":"python3 deploy.py"}"#),
        &mut sink,
    );
    assert!(
        ran.load(std::sync::atomic::Ordering::SeqCst),
        "an unchanged script was refused: {r:?}"
    );
}

struct AdmitGate;

impl Gate for AdmitGate {
    fn admit(&mut self, _call: &GateCall<'_>) -> GateDecision {
        GateDecision::Admit
    }
}

struct ExplodingGate;

impl Gate for ExplodingGate {
    fn admit(&mut self, call: &GateCall<'_>) -> GateDecision {
        panic!("a read-only tool must never reach the gate: {}", call.name);
    }
}

fn runtime(access: Access) -> ToolRuntime {
    let mut reg = Registry::new();
    reg.register(Box::new(Probe { access })).unwrap();
    let d = crate::backend::tempdir::TempDir::new();
    let backend = crate::backend::HostBackend::new(d.path()).unwrap();
    // The temp dir outlives the backend only within one test; leak it there
    // rather than complicate every caller.
    std::mem::forget(d);
    ToolRuntime::new(reg, Box::new(backend)).with_gate(Box::new(ExplodingGate))
}

// -----------------------------------------------------------------------
// The view ask: a path the boundary hid, and the one card a tree gets for it
// -----------------------------------------------------------------------

/// A tool that could not see the path it was asked for, as `bash` reports one it read off
/// its own output. What the runtime does with that finding is the whole of these tests.
struct Hidden {
    access: crate::view::ViewAccess,
}

impl Tool for Hidden {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "hidden",
            "Read something the boundary hid.",
            serde_json::json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"],
            }),
            Access::Read,
        )
    }
    fn invoke(&self, _ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let mut inv = Invocation::ok("the boundary hid it");
        inv.needs_in_view = vec![crate::view::ViewAsk {
            path: std::path::PathBuf::from(args["path"].as_str().unwrap_or_default()),
            access: self.access,
        }];
        inv
    }
}

/// Answers every view card by granting read and write, and counts the cards: "no card" is
/// the claim most of these make, and it cannot be read off an answer.
struct GrantsReadWrite {
    cards: std::sync::Arc<std::sync::Mutex<Vec<crate::view::ViewAccess>>>,
}

impl crate::adjudicate::Adjudicator for GrantsReadWrite {
    fn decide(
        &self,
        req: &crate::adjudicate::AdjudicationRequest,
    ) -> crate::adjudicate::AdjudicationDecision {
        let access = if req.class.access == Access::Write {
            crate::view::ViewAccess::Write
        } else {
            crate::view::ViewAccess::Read
        };
        self.cards
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(access);
        crate::adjudicate::AdjudicationDecision::selected(req, "grant_rw", "human:test", "because")
    }
    fn describe(&self) -> String {
        "asked at the head: a test".into()
    }
}

/// A runtime over a backend with no view of its own, and a gate over one tree's record.
/// The backend cannot bind what it grants here - it starts no processes - so these tests
/// read the ASK, which is the question R162 is about, and not the mount.
fn view_runtime(
    access: crate::view::ViewAccess,
) -> (
    ToolRuntime,
    std::sync::Arc<std::sync::Mutex<Vec<crate::view::ViewAccess>>>,
) {
    let cards = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let view = std::sync::Arc::new(crate::view::ViewGrants::new());
    let gate = crate::adjudicate::AdjudicatedGate::new(Box::new(GrantsReadWrite {
        cards: std::sync::Arc::clone(&cards),
    }))
    .with_identity("s-test", "agent")
    .with_view_grants(view);
    let mut reg = Registry::new();
    reg.register(Box::new(Hidden { access })).unwrap();
    let d = crate::backend::tempdir::TempDir::new();
    let backend = crate::backend::HostBackend::new(d.path()).unwrap();
    std::mem::forget(d);
    (
        ToolRuntime::new(reg, Box::new(backend)).with_gate(Box::new(gate)),
        cards,
    )
}

/// R162: a grant on a directory covers every path under it, and a name that only starts the
/// same way is a different directory. This is the whole path from a tool's finding to the
/// one card a tree gets for it.
#[test]
fn a_path_a_grant_covers_raises_no_card_and_a_lookalike_name_still_does() {
    let (rt, cards) = view_runtime(crate::view::ViewAccess::Read);
    let mut sink = RecordingToolSink::default();
    let asked = |path: &str, sink: &mut RecordingToolSink| {
        let _ = rt.invoke(
            "t1",
            &call("hidden", &format!(r#"{{"path":"{path}"}}"#)),
            sink,
        );
        cards.lock().unwrap().len()
    };
    assert_eq!(asked("/build/letibot-r114/review/", &mut sink), 1);
    assert_eq!(
        asked("/build/letibot-r114/review/chunk02.jsonl", &mut sink),
        1,
        "a grant on the directory covers the file under it: no second card"
    );
    assert_eq!(
        asked("/build-other/y", &mut sink),
        2,
        "`/build-other` is not under `/build/...`, however its name starts"
    );
}

/// The access a tool reports reaches the gate: a write the view refused is a WRITE ask, and
/// a read-only grant is not an answer to it.
#[test]
fn the_access_a_tool_reported_is_the_access_the_gate_is_asked_about() {
    let (rt, cards) = view_runtime(crate::view::ViewAccess::Write);
    let mut sink = RecordingToolSink::default();
    let _ = rt.invoke("t1", &call("hidden", r#"{"path":"/build/x"}"#), &mut sink);
    assert_eq!(
        *cards.lock().unwrap(),
        vec![crate::view::ViewAccess::Write],
        "a write ask must not reach the gate as a read"
    );
}

/// An envelope repair is said to the model once per session, and still counted.
#[test]
fn an_envelope_repair_is_said_once_and_still_counted() {
    let rt = runtime(Access::Read);
    let mut sink = RecordingToolSink::default();
    let fenced = "```json\n{\"path\": \"a\"}\n```";
    let first = rt.invoke("t1", &call("probe", fenced), &mut sink);
    let second = rt.invoke("t1", &call("probe", fenced), &mut sink);
    assert!(first.render().contains("[repaired]"), "{}", first.render());
    assert!(
        !second.render().contains("[repaired]"),
        "{}",
        second.render()
    );
    assert!(
        second.repairs.iter().any(|r| r.code == "envelope.fence"),
        "the head and the metrics still see it: {:?}",
        second.repairs
    );
}

/// Every kind, not only the envelope's: a repair that is a renamed key here is said once
/// per session too, and still counted.
#[test]
fn every_repair_kind_is_said_once_and_still_counted() {
    let rt = runtime(Access::Read);
    let mut sink = RecordingToolSink::default();
    let renamed = "{\"file_path\": \"a\"}";
    let first = rt.invoke("t1", &call("probe", renamed), &mut sink);
    let second = rt.invoke("t1", &call("probe", renamed), &mut sink);
    assert!(
        first.repairs.iter().any(|r| r.code == "key.renamed"),
        "{:?}",
        first.repairs
    );
    assert!(first.render().contains("[repaired]"), "{}", first.render());
    assert!(
        !second.render().contains("[repaired]"),
        "{}",
        second.render()
    );
    assert!(second.repairs.iter().any(|r| r.code == "key.renamed"));
}

fn call(name: &str, args: &str) -> ToolCall {
    ToolCall {
        id: "c0".into(),
        name: name.into(),
        arguments: args.into(),
    }
}

#[test]
fn without_access_drops_a_class_and_leaves_the_rest_in_role_order() {
    let mut reg = Registry::new();
    reg.register(Box::new(Probe {
        access: Access::Read,
    }))
    .unwrap();
    let mut denied = std::collections::BTreeSet::new();
    denied.insert(Access::Read);
    assert!(reg.schemas().iter().any(|s| s.name == "probe"));
    let reg = reg.without_access(&denied);
    assert!(reg.schemas().is_empty());
    let mut reg = Registry::new();
    reg.register(Box::new(Probe {
        access: Access::Read,
    }))
    .unwrap();
    let mut other = std::collections::BTreeSet::new();
    other.insert(Access::Exec);
    assert_eq!(reg.without_access(&other).schemas().len(), 1);
}

#[test]
fn a_read_only_tool_never_reaches_the_gate() {
    // Clause 4, as a control-flow fact rather than a promise: the gate panics
    // if it is consulted, and this call must not consult it.
    let rt = runtime(Access::Read);
    let mut sink = RecordingToolSink::new();
    let r = rt.invoke("t1", &call("probe", r#"{"path":"a"}"#), &mut sink);
    assert_eq!(r.outcome, ToolOutcome::Ok);
}

#[test]
fn a_write_tool_does_and_m1_has_nobody_to_ask() {
    let mut reg = Registry::new();
    reg.register(Box::new(Probe {
        access: Access::Write,
    }))
    .unwrap();
    let d = crate::backend::tempdir::TempDir::new();
    let backend = crate::backend::HostBackend::new(d.path()).unwrap();
    let rt = ToolRuntime::new(reg, Box::new(backend));
    let mut sink = RecordingToolSink::new();
    let r = rt.invoke("t1", &call("probe", r#"{"path":"a"}"#), &mut sink);
    match r.outcome {
        // `NotRun`, not `Denied`: nobody decided anything.
        ToolOutcome::NotRun { why } => assert!(why.contains("fails closed"), "{why}"),
        other => panic!("a write tool must not run unattended: {other:?}"),
    }
    assert!(
        !sink.kinds().contains(&"ToolStarted"),
        "a refused call never started"
    );
}

#[test]
fn an_unknown_tool_comes_back_with_the_list_and_the_nearest_name() {
    let rt = runtime(Access::Read);
    let mut sink = RecordingToolSink::new();
    let r = rt.invoke("t1", &call("probes", "{}"), &mut sink);
    assert!(r.payload.contains("probe"), "{}", r.payload);
    assert!(r.payload.contains("nearest"), "{}", r.payload);
}

#[test]
fn a_description_that_names_data_is_refused_at_registration() {
    struct Stale;
    impl Tool for Stale {
        fn schema(&self) -> ToolSchema {
            ToolSchema::new(
                "stale",
                "Search the corpus. The corpus contains the Rust book.",
                serde_json::json!({"type": "object"}),
                Access::Read,
            )
        }
        fn invoke(&self, _c: &mut InvokeCtx<'_>, _a: &Value) -> Invocation {
            Invocation::ok("")
        }
    }
    let mut reg = Registry::new();
    let e = reg.register(Box::new(Stale)).unwrap_err();
    assert!(format!("{e}").contains("clause 6"), "{e}");
    // A foreign tool is seated, and its findings are visible.
    assert!(reg.register_foreign(Box::new(Stale)).is_ok());
    assert_eq!(reg.foreign_findings.len(), 1);
}

#[test]
fn a_role_over_the_ceiling_is_refused_with_the_overflow_named() {
    let mut reg = Registry::new();
    reg.register(Box::new(Probe {
        access: Access::Read,
    }))
    .unwrap();
    let mut role = Role::new("greedy", &["probe"]);
    role.max_tools = 0;
    let e = reg.resolve_role(&role).unwrap_err();
    assert!(format!("{e}").contains("probe"), "{e}");
}

/// `m2_coder` seats the delegation pair. `task` and `task_result` are listed on the seat
/// rather than stripped at the depth limit, because the cap is enforced where the call is
/// made: the seat must name them at every depth, or a child starts work it cannot collect.
#[test]
fn the_coder_seat_names_the_delegation_pair() {
    let seat = roles::m2_coder();
    assert!(
        seat.tools.iter().any(|t| t == "task"),
        "a subagent must be able to delegate: {:?}",
        seat.tools
    );
    assert!(
        seat.tools.iter().any(|t| t == "task_result"),
        "a seat that can start work and not collect it is worse than one that \
             cannot: {:?}",
        seat.tools
    );
    assert!(
        seat.tools.iter().any(|t| t == "task_message"),
        "a seat that can start work and not CORRECT it has only the wrong answer to \
             wait for: {:?}",
        seat.tools
    );
    // The pair is seated, not pushed: a role over its ceiling is refused at resolve,
    // so an overflow here would be a prompt the build cannot seat at all.
    assert!(
        seat.tools.len() <= seat.max_tools,
        "the seat is over its own ceiling: {} > {}",
        seat.tools.len(),
        seat.max_tools
    );
}

/// The gatekeeper seat is read-only by construction: it may read, grep, glob and run `bash`
/// for `git diff`, and it has no `write`, no `edit`, no `task`. A reviewer that can author
/// code, directly or through a child, becomes a second author and then nobody reviewed.
#[test]
fn the_gatekeeper_seat_has_no_writing_tool() {
    let seat = roles::gatekeeper();
    // The reading and the diff are there.
    for want in ["read", "grep", "glob", "bash"] {
        assert!(
            seat.tools.iter().any(|t| t == want),
            "the gatekeeper seat must name `{want}`: {:?}",
            seat.tools
        );
    }
    // And nothing that can author code: no write, no edit, no delegation.
    for forbidden in ["write", "edit", "task", "task_result", "task_message"] {
        assert!(
            !seat.tools.iter().any(|t| t == forbidden),
            "the gatekeeper seat must not name `{forbidden}` — a reviewer that \
                 can edit becomes a second author and then nobody reviewed: {:?}",
            seat.tools
        );
    }
    // And the one tool that is not a read: the door its verdict goes through. A seat without
    // it is a reviewer that can judge and cannot say so, which is the state this tree was in
    // while the parent parsed its answer instead.
    assert!(
        seat.tools
            .iter()
            .any(|t| t == crate::builtins::verdict::TOOL),
        "the gatekeeper seat must carry its own verdict door: {:?}",
        seat.tools
    );
    // The seat is under its own ceiling, so it is seatable.
    assert!(
        seat.tools.len() <= seat.max_tools,
        "the seat is over its own ceiling: {} > {}",
        seat.tools.len(),
        seat.max_tools
    );
}

/// The notes keeper seat: the read tools and `notes`, and nothing that authors code. `notes`
/// is the one writer, and it is how a repair keeps authorship: a bare name, so no spelling of
/// a write leaves the notes directory, and the operator's own files are refused.
#[test]
fn the_notes_keeper_seat_reads_and_repairs_memory_but_never_code() {
    let seat = roles::notes_keeper();
    for want in ["read", "grep", "glob", "read_spill", "notes"] {
        assert!(
            seat.tools.iter().any(|t| t == want),
            "the notes keeper seat must name `{want}`: {:?}",
            seat.tools
        );
    }
    // Nothing that can author code, and nothing that can run it: the keeper inspects a note
    // and repairs memory. `bash` in particular is not here even behind a flag, because the
    // commits a note cites were resolved for it before it was asked.
    for forbidden in [
        "write",
        "edit",
        "bash",
        "task",
        "task_start",
        "job_kill",
        "monitor",
    ] {
        assert!(
            !seat.tools.iter().any(|t| t == forbidden),
            "the notes keeper seat must not name `{forbidden}`: {:?}",
            seat.tools
        );
    }
    assert!(
        seat.tools.len() <= seat.max_tools,
        "the seat is over its own ceiling: {} > {}",
        seat.tools.len(),
        seat.max_tools
    );
}

/// One plan tool per seat: a role naming both `todo` and `todo_write` resolves to
/// `todo_write` alone.
#[test]
fn a_seat_with_todo_write_does_not_also_get_todo() {
    let mut reg = Registry::new();
    let board = std::sync::Arc::new(crate::builtins::todo::TodoBoard::new(Vec::new()));
    reg.register(Box::new(crate::builtins::todo::TodoWriteTool::new(board)))
        .unwrap();
    crate::builtins::intent::register_into(
        &mut reg,
        &crate::builtins::intent::Wiring::standalone(),
    )
    .unwrap();
    let both = Role::new("both", &["todo", "todo_write"]);
    let seated = reg.resolve_role(&both).unwrap().names();
    assert!(seated.contains(&"todo_write".to_string()), "{seated:?}");
    assert!(!seated.contains(&"todo".to_string()), "{seated:?}");
}

/// The room is on the two seats a person's own session runs on, and on nothing else. It was
/// once written into a table no seat resolves, so the arbiter was woken, told to answer with
/// a tool it did not have, and a raised item froze. The seats that must not are subagents'.
#[test]
fn the_room_is_seated_on_the_seats_a_top_level_session_actually_runs_on() {
    for seat in [
        roles::m1_orchestrator(),
        roles::leticode(),
        roles::review_queue(),
    ] {
        assert!(
            seat.tools.iter().any(|t| t == "room"),
            "`{}` is a seat a top-level session runs on, so it must carry `room`: {:?}",
            seat.name,
            seat.tools
        );
    }
    for seat in [
        roles::m2_coder(),
        roles::m2_runner(),
        roles::gatekeeper(),
        roles::notes_keeper(),
    ] {
        assert!(
            !seat.tools.iter().any(|t| t == "room"),
            "`{}` is a subagent's or a hidden child's seat, and the room is top-level only: \
                 {:?}",
            seat.name,
            seat.tools
        );
    }
    // And the seat the coordinator resolves is `m1_orchestrator`, not the table above it.
    // `base_role_for_seat` lives in `harnessd`; what this crate can pin is that the two are
    // not the same list, which is the fact the defect turned on.
    assert_ne!(
        roles::m1_orchestrator().tools,
        roles::orchestrator().tools,
        "the live orchestrator seat and §8.4's table are different lists; if they ever \
             collapse into one, `harnessd`'s `base_role_for_seat` has to be re-read before \
             anybody believes a tool added here is seated"
    );
}

/// Both seats the review queue runs on, and the delta between them printed when either is
/// wrong: the queue's own session reads and corrects the reviewers the queue starts, and the
/// reviewer holds the shell and the verdict and can collect nothing.
#[test]
fn the_queue_seat_reads_and_corrects_and_the_reviewer_seat_judges() {
    let queue = roles::review_queue();
    let reviewer = roles::gatekeeper();
    let delta = || {
        format!(
            "\n  queue {}: {:?}\n  reviewer {}: {:?}\n  m1 {}: {:?}",
            queue.name,
            queue.tools,
            reviewer.name,
            reviewer.tools,
            roles::m1_orchestrator().name,
            roles::m1_orchestrator().tools
        )
    };

    // The pair its prompt prescribes, and the whole of the difference from M1's seat: a tool
    // added to either list without the other is the defect this test was written for.
    let mut added: Vec<&String> = queue
        .tools
        .iter()
        .filter(|t| !roles::m1_orchestrator().tools.contains(t))
        .collect();
    added.sort();
    assert_eq!(
        added,
        ["task_message", "task_result"],
        "the queue's seat is M1's plus exactly the verbs its prompt prescribes{}",
        delta()
    );
    for tool in ["todo_write", "read", "recall", "room"] {
        assert!(
            queue.tools.iter().any(|t| t == tool),
            "the queue's prompt prescribes `{tool}` and its seat lacks it{}",
            delta()
        );
    }

    // None of the four, on the queue's seat: it starts no child, runs no command and hands in
    // no verdict - the reviewer does the last one, and the queue's clock starts the child.
    let forbidden = ["task", "task_start", "review_verdict", "bash"];
    let held: Vec<&&str> = forbidden
        .iter()
        .filter(|t| queue.tools.iter().any(|x| x == *t))
        .collect();
    assert!(
        held.is_empty(),
        "the queue's seat must hold none of {forbidden:?}, and holds {held:?}{}",
        delta()
    );

    // And the reviewer's seat is the other half: a shell to `git diff` with, its own verdict to
    // hand in, and no way to collect or correct anything - a reader that could become a second
    // author is the thing its seat exists to prevent.
    for tool in ["bash", "review_verdict"] {
        assert!(
            reviewer.tools.iter().any(|t| t == tool),
            "the reviewer's seat must keep `{tool}`{}",
            delta()
        );
    }
    let leaked: Vec<&&str> = ["task_result", "task_message", "task", "task_start"]
        .iter()
        .filter(|t| reviewer.tools.iter().any(|x| x == *t))
        .collect();
    assert!(
        leaked.is_empty(),
        "the reviewer's seat must hold none of [\"task_result\", \"task_message\", \"task\", \"task_start\"], and holds {leaked:?}{}",
        delta()
    );

    // Both fit the ceiling that binds what a session is actually given.
    for seat in [&queue, &reviewer] {
        assert!(
            seat.tools.len() <= seat.max_tools,
            "`{}` is over its own ceiling: {} > {}",
            seat.name,
            seat.tools.len(),
            seat.max_tools
        );
    }
}

/// `task_start` is seated wherever `task` is: placement lives in the tool, not in the
/// caller's memory. A seat that can start a child but not arrange its placement spawns it
/// into the main tree and then kills it for being in the wrong place.
#[test]
fn task_start_is_seated_wherever_task_is() {
    for (name, seat) in [
        ("orchestrator", roles::orchestrator()),
        ("leticode", roles::leticode()),
        ("m2_coder", roles::m2_coder()),
    ] {
        let has_task = seat.tools.iter().any(|t| t == "task");
        let has_task_start = seat.tools.iter().any(|t| t == "task_start");
        assert!(
            has_task,
            "the {name} seat is expected to name `task` in this test: {:?}",
            seat.tools
        );
        assert!(
            has_task_start,
            "a seat that can start a child but not arrange its placement spawns it \
                 into the main tree and then kills it for being in the wrong place: {:?}",
            seat.tools
        );
        // Seated, not pushed: a role over its ceiling is refused at resolve.
        assert!(
            seat.tools.len() <= seat.max_tools,
            "the {name} seat is over its own ceiling: {} > {}",
            seat.tools.len(),
            seat.max_tools
        );
    }
}

/// The leticode seat can ask the person a question. The tool lives in the `intent` bundle,
/// which is the planner's, so without this entry a leticode session has no such verb. The
/// seat names tools this crate does not register, so only its own list is pinned here.
#[test]
fn the_leticode_seat_carries_ask_user_question() {
    let seat = roles::leticode();
    assert!(
        seat.tools.iter().any(|t| t == "ask_user_question"),
        "the leticode seat does not name `ask_user_question`: {:?}",
        seat.tools
    );
    // It took its own seat rather than trading for one: nothing left the list when it
    // arrived, which is the property `max_tools` is declared for. The count is pinned so a
    // tool added without a decision fails here. 38 since `job_extend` (R160): a background job
    // has no deadline of its own, and the warning before a dead check names the way out.
    // 39 with `screen`.
    assert_eq!(seat.tools.len(), 39, "{:?}", seat.tools);
    assert!(
        seat.tools.len() <= seat.max_tools,
        "the seat is over its own ceiling: {} > {}",
        seat.tools.len(),
        seat.max_tools
    );
    // And the declaration is the one `intent/mod.rs` asserts for it: it changes
    // nothing the operator owns and reads nothing off disk; it interrupts a person.
    let mut reg =
        crate::coder_tools(std::sync::Arc::new(crate::builtins::retrieval::Unavailable)).unwrap();
    crate::builtins::intent::register_into(
        &mut reg,
        &crate::builtins::intent::Wiring::standalone(),
    )
    .unwrap();
    let access = reg
        .schemas()
        .into_iter()
        .find(|s| s.name == "ask_user_question")
        .expect("the tool is registered")
        .access;
    assert_eq!(access, Access::Session);
}

#[test]
fn a_role_naming_a_tool_this_build_lacks_is_refused_not_shrunk() {
    let mut reg = Registry::new();
    reg.register(Box::new(Probe {
        access: Access::Read,
    }))
    .unwrap();
    let e = reg.resolve_role(&roles::coder()).unwrap_err();
    let msg = format!("{e}");
    assert!(msg.contains("write") && msg.contains("bash"), "{msg}");
}

#[test]
fn the_events_bracket_the_call() {
    let rt = runtime(Access::Read);
    let mut sink = RecordingToolSink::new();
    rt.invoke("t1", &call("probe", r#"{"path":"a"}"#), &mut sink);
    assert_eq!(sink.kinds(), vec!["ToolStarted", "ToolFinished"]);
}

#[test]
fn the_row_and_the_event_carry_the_same_bounded_excerpt() {
    // One bound, two carriers: the event reaches the heads watching live, the row reaches
    // every head after a restart. A row whose panels disagree with the card the operator
    // watched draw would be a defect, not a choice.
    let before: String = (1..=50).map(|i| format!("line {i}\n")).collect();
    let after = before.replacen("line 25", "line 25 changed", 1);
    let r = ToolResult {
        call_id: "c1".into(),
        name: "edit".into(),
        outcome: ToolOutcome::Ok,
        payload: "done".into(),
        repairs: Vec::new(),
        notes: Vec::new(),
        spill: None,
        media: None,
        repairs_told: Vec::new(),
        edits: vec![crate::edit::FileEdit {
            path: "f".into(),
            before: before.clone(),
            after: after.clone(),
            created: false,
            before_digest: String::new(),
            after_digest: String::new(),
            replacements: 1,
            changed: crate::edit::changed_span(&before, &after),
        }],
    };
    let item = ToolRuntime::transcript_item(&r);
    let TranscriptItem::ToolResult { edits: row, .. } = &item else {
        panic!("transcript_item built a {item:?}");
    };
    let expected = r.edits.first().unwrap().excerpt(3, 400);
    assert_eq!(row.first(), Some(&expected), "same bound, same excerpt");
}

/// A path in the secret store is not opened to find out what it contains. The refusal is at
/// the reader, before the open: a filter after the read has already read it. The closure
/// here counts its own calls, which is the only way to assert not opened.
#[test]
fn a_secret_store_script_is_never_opened() {
    use std::cell::Cell;
    let opened = Cell::new(0);
    let found = crate::runtime::scripts_for("python3 ~/.ssh/id_rsa", Some("/home/dead"), |p| {
        opened.set(opened.get() + 1);
        Ok(format!("the contents of {p}").into_bytes())
    });
    assert_eq!(opened.get(), 0, "the reader opened a secret-store file");
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].path, "~/.ssh/id_rsa");
    let ScriptBody::Unreadable(why) = &found[0].body else {
        panic!(
            "a secret-store path came back readable: {:?}",
            found[0].body
        );
    };
    assert!(why.contains(".ssh"), "{why}");
    assert!(
        why.contains("not opened to find out what it contains"),
        "the refusal does not say what is being refused: {why}"
    );

    // An ordinary path is read, so the guard is not a blanket refusal: the assertion above
    // would pass on a reader that never opened anything.
    let opened = Cell::new(0);
    let found = crate::runtime::scripts_for("python3 deploy.py", Some("/home/dead"), |p| {
        opened.set(opened.get() + 1);
        Ok(format!("print('{p}')").into_bytes())
    });
    assert_eq!(
        opened.get(),
        1,
        "an ordinary script was not read: {found:?}"
    );
    assert!(
        matches!(&found[0].body, ScriptBody::Read(t) if t.contains("deploy.py")),
        "{:?}",
        found[0].body
    );
}

/// The reader and the classifier agree about which file is the program: a wrapped
/// interpreter is unwrapped on both sides, so `sudo python3 foo.py` does not have its file
/// read by neither half.
#[test]
fn a_wrapped_interpreter_has_its_file_read() {
    let found =
        crate::runtime::scripts_for("sudo -u dead python3 deploy.py", Some("/home/dead"), |_| {
            Ok(b"print(1)".to_vec())
        });
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].path, "deploy.py");
}

/// An inline program names no file, so nothing is read and nothing is claimed, and a
/// command that names no script at all stays empty.
#[test]
fn a_command_with_no_script_file_reads_nothing() {
    let read = |_: &str| -> Result<Vec<u8>, String> { panic!("opened a file") };
    assert!(crate::runtime::scripts_for("python3 -c 'print(1)'", None, read).is_empty());
    let read = |_: &str| -> Result<Vec<u8>, String> { panic!("opened a file") };
    assert!(crate::runtime::scripts_for("ls -la", None, read).is_empty());
}

// A heredoc through `bash` is a write (see `crate::heredoc`)

/// A `bash` stand-in. It records that it ran, so a test can tell a converted call, which
/// must never reach the shell, from one left alone.
struct FakeBash {
    ran: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Tool for FakeBash {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "bash",
            "Run a shell command in the session's workspace and return its output.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"},
                    "cwd": {"type": "string"}
                },
                "required": ["command"]
            }),
            Access::Exec,
        )
    }
    fn invoke(&self, _ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        self.ran.store(true, std::sync::atomic::Ordering::SeqCst);
        Invocation::ok(format!("ran {}", args["command"]))
    }
}

/// What the gate was shown, which is the half of the rewrite that is not the file on
/// disk: a `write` to a path, adjudicated as one.
#[derive(Clone, Default)]
struct Seen(std::sync::Arc<std::sync::Mutex<Vec<(String, Access, Value)>>>);

impl Seen {
    fn calls(&self) -> Vec<(String, Access, Value)> {
        self.0.lock().unwrap().clone()
    }
}

impl Gate for Seen {
    fn admit(&mut self, call: &GateCall<'_>) -> GateDecision {
        self.0
            .lock()
            .unwrap()
            .push((call.name.to_string(), call.access, call.args.clone()));
        GateDecision::Admit
    }
}

/// A writable temp tree with a fake `bash`, the real `write`, and a gate that says
/// what it was asked about. Returns the directory too, so a test can read what
/// landed.
fn heredoc_runtime(
    with_write: bool,
) -> (
    ToolRuntime,
    Seen,
    std::sync::Arc<std::sync::atomic::AtomicBool>,
    crate::backend::tempdir::TempDir,
) {
    let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut reg = Registry::new();
    reg.register(Box::new(FakeBash { ran: ran.clone() }))
        .unwrap();
    if with_write {
        reg.register(Box::new(crate::builtins::write::Write))
            .unwrap();
    }
    let d = crate::backend::tempdir::TempDir::new();
    let backend = crate::backend::HostBackend::writable(d.path()).unwrap();
    let seen = Seen::default();
    let rt = ToolRuntime::new(reg, Box::new(backend)).with_gate(Box::new(seen.clone()));
    (rt, seen, ran, d)
}

fn bash_call(command: &str) -> ToolCall {
    call("bash", &serde_json::json!({"command": command}).to_string())
}

/// The operator's own command, through the runtime. It is a `write` by the time the gate
/// sees it: the access is `Write`, the file lands with the heredoc's body, and the result
/// carries the diff a write draws.
#[test]
fn a_heredoc_bash_call_is_adjudicated_recorded_and_drawn_as_a_write() {
    let (rt, seen, ran, d) = heredoc_runtime(true);
    let mut sink = RecordingToolSink::default();
    let r = rt.invoke(
        "t1",
        &bash_call(
            "cd crates && cat > workloads/src/banking.rs <<'RUST'\n\
                 pub fn balance() -> i64 { 0 }\nRUST",
        ),
        &mut sink,
    );

    assert!(
        !ran.load(std::sync::atomic::Ordering::SeqCst),
        "the heredoc reached the shell instead of being recorded as a write: {r:?}"
    );
    let landed = std::fs::read_to_string(d.path().join("crates/workloads/src/banking.rs"))
        .expect("the file the heredoc wrote");
    assert_eq!(landed, "pub fn balance() -> i64 { 0 }\n");

    let seen = seen.calls();
    assert_eq!(seen.len(), 1, "the gate was asked {seen:?}");
    assert_eq!(seen[0].0, "write", "the gate judged an exec, not a write");
    assert_eq!(seen[0].1, Access::Write);
    assert_eq!(
        seen[0].2["path"], "crates/workloads/src/banking.rs",
        "the gate was given the command rather than the path: {:?}",
        seen[0].2
    );

    assert_eq!(r.name, "write", "the transcript row still says bash");
    let edit = r.edits.first().expect("a write draws a diff");
    assert_eq!(edit.path, "crates/workloads/src/banking.rs");
    assert!(edit.created);
    assert_eq!(edit.after, "pub fn balance() -> i64 { 0 }\n");

    let rendered = r.render();
    assert!(
        rendered.contains(
            "recorded as a write to `crates/workloads/src/banking.rs` \
                 (heredoc through bash is converted; use `write` directly)"
        ),
        "{rendered}"
    );
    assert!(
        !rendered.contains("call `write` with the contents"),
        "a converted call also got the near-miss nudge: {rendered}"
    );
}

/// The `cwd` argument is where the shell would have been, so a relative path is
/// resolved against it.
#[test]
fn a_cwd_argument_is_where_the_heredoc_writes() {
    let (rt, seen, _, d) = heredoc_runtime(true);
    let mut sink = RecordingToolSink::default();
    let r = rt.invoke(
        "t1",
        &call(
            "bash",
            &serde_json::json!({
                "command": "cat > f.rs <<'EOF'\nfn a() {}\nEOF",
                "cwd": "crates/tools",
            })
            .to_string(),
        ),
        &mut sink,
    );
    assert_eq!(r.name, "write");
    assert_eq!(seen.calls()[0].2["path"], "crates/tools/f.rs");
    assert_eq!(
        std::fs::read_to_string(d.path().join("crates/tools/f.rs")).unwrap(),
        "fn a() {}\n"
    );
}

/// An append is the file that is there plus the body: `write` replaces whole contents,
/// and a `>>` that replaced would be a deletion.
#[test]
fn an_appending_heredoc_keeps_what_the_file_already_held() {
    let (rt, seen, _, d) = heredoc_runtime(true);
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    std::fs::write(d.path().join("src/lib.rs"), "// the head\n").unwrap();
    let mut sink = RecordingToolSink::default();
    let r = rt.invoke(
        "t1",
        &bash_call("cat >> src/lib.rs <<'EOF'\npub fn a() {}\nEOF"),
        &mut sink,
    );
    assert_eq!(r.name, "write");
    assert_eq!(seen.calls()[0].2["path"], "src/lib.rs");
    assert_eq!(
        std::fs::read_to_string(d.path().join("src/lib.rs")).unwrap(),
        "// the head\npub fn a() {}\n"
    );
    let edit = r.edits.first().expect("a diff");
    assert!(!edit.created);
    assert_eq!(edit.before, "// the head\n");
}

/// A near miss is left to the shell and told what it cost. `mkdir -p ... &&` is a second
/// command, so this is not the one shape: the call still runs, and the result carries the
/// one line naming the tools that would have recorded it.
#[test]
fn an_unconverted_heredoc_write_carries_the_nudge() {
    let (rt, seen, ran, _d) = heredoc_runtime(true);
    let mut sink = RecordingToolSink::default();
    let r = rt.invoke(
        "t1",
        &bash_call("mkdir -p d && cat > d/f <<'EOF'\nx\nEOF"),
        &mut sink,
    );
    assert!(
        ran.load(std::sync::atomic::Ordering::SeqCst),
        "a command that is not this shape was taken away from the shell"
    );
    assert_eq!(r.name, "bash");
    assert_eq!(seen.calls()[0].0, "bash");
    assert!(r.edits.is_empty());
    assert!(
        r.render().contains("call `write` with the contents"),
        "{}",
        r.render()
    );
}

/// A seat with no `write` keeps its shell. The gatekeeper holds `bash` and no writing
/// tool, and a rewrite there would turn a working command into "no tool called `write`
/// is available here".
#[test]
fn a_seat_without_write_is_not_rewritten_and_not_nudged() {
    let (rt, seen, ran, d) = heredoc_runtime(false);
    let mut sink = RecordingToolSink::default();
    let r = rt.invoke("t1", &bash_call("cat > f <<'EOF'\nx\nEOF"), &mut sink);
    assert!(ran.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(seen.calls()[0].0, "bash");
    assert_eq!(r.name, "bash");
    assert!(
        !r.render().contains("`write`"),
        "a seat that cannot write was told to write: {}",
        r.render()
    );
    assert!(!d.path().join("f").exists());
}

/// The operator's own line is not rewritten. `invoke_operator` is the `!` line and the
/// head-run door: what a person typed is what runs.
#[test]
fn the_operators_own_heredoc_is_left_as_they_typed_it() {
    let (rt, seen, ran, _d) = heredoc_runtime(true);
    let mut sink = RecordingToolSink::default();
    let r = rt.invoke_operator("t1", &bash_call("cat > f <<'EOF'\nx\nEOF"), &mut sink);
    assert!(ran.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(r.name, "bash");
    assert!(seen.calls().is_empty(), "the ungated path asked the gate");
}

// `on_tool_call`: a plugin layer sees the call before the gate

/// A gate that records what it was asked about and admits, so a test can read the
/// arguments the gate adjudicated rather than trusting that it saw the right ones.
struct RecordingGate {
    seen: std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
}

impl Gate for RecordingGate {
    fn admit(&mut self, call: &GateCall<'_>) -> GateDecision {
        self.seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(call.args.clone());
        GateDecision::Admit
    }
}

/// A tool that says whether it ran: the difference between a call a layer refused and a
/// call whose result a layer replaced, which no outcome alone can tell apart.
struct RanProbe {
    ran: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Tool for RanProbe {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "probe",
            "Do a thing. Takes a path.",
            serde_json::json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"],
            }),
            Access::Exec,
        )
    }
    fn invoke(&self, _ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        self.ran.store(true, std::sync::atomic::Ordering::SeqCst);
        Invocation::ok(format!("probed {}", args["path"]))
    }
}

/// A plugin layer, answered from a fixed verdict, and recording every call it was shown.
struct FixedHook {
    answer: Option<letibot_plugin::tool_call::ToolCallOut>,
    seen: std::sync::Arc<std::sync::Mutex<Vec<letibot_plugin::Call>>>,
    /// What a host that knows the layer's NAME would put above the result, see
    /// [`Patched::note`]. `None` is the `p1` id with nothing said about it.
    note: Option<String>,
}

impl CallHook for FixedHook {
    fn before(&self, call: &letibot_plugin::Call, _standing: &Standing) -> Option<Patched> {
        self.seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(call.clone());
        self.answer.clone().map(|out| Patched {
            out,
            by: "p1".to_string(),
            note: self.note.clone(),
        })
    }
}

/// What `hooked` hands back: the runtime plus the registers its gate and hook record into.
type Hooked = (
    ToolRuntime,
    std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
    std::sync::Arc<std::sync::Mutex<Vec<letibot_plugin::Call>>>,
    std::sync::Arc<std::sync::atomic::AtomicBool>,
    crate::backend::tempdir::TempDir,
);

/// A runtime over one `probe` tool, a recording gate, and a hook that answers `answer`.
fn hooked(answer: Option<letibot_plugin::tool_call::ToolCallOut>) -> Hooked {
    let d = crate::backend::tempdir::TempDir::new();
    let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut reg = Registry::new();
    reg.register(Box::new(RanProbe { ran: ran.clone() }))
        .unwrap();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let rt = ToolRuntime::new(
        reg,
        Box::new(crate::backend::HostBackend::new(d.path()).unwrap()),
    )
    .with_gate(Box::new(RecordingGate { seen: seen.clone() }))
    .with_call_hook(std::sync::Arc::new(FixedHook {
        answer,
        seen: calls.clone(),
        note: None,
    }));
    (rt, seen, calls, ran, d)
}

/// The rewritten call is the call the gate adjudicates, which is the whole of the safety: a
/// layer can change what a call asks for, and what it asks for then meets the same gate. The
/// model asked for one path, the layer asked for another, and the gate was asked about that.
#[test]
fn a_rewritten_call_is_the_call_the_gate_adjudicates() {
    let (rt, seen, _calls, ran, _d) =
        hooked(Some(letibot_plugin::tool_call::ToolCallOut::rewrite(
            serde_json::json!({"path": "/tmp/the-rewritten-one"}),
        )));
    let mut sink = RecordingToolSink::default();
    let r = rt.invoke(
        "t1",
        &call("probe", r#"{"path":"/tmp/as-the-model-asked"}"#),
        &mut sink,
    );
    assert!(ran.load(std::sync::atomic::Ordering::SeqCst));
    let judged = seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
    assert_eq!(judged.len(), 1, "the gate was asked once: {judged:?}");
    assert_eq!(
        judged[0]["path"],
        serde_json::json!("/tmp/the-rewritten-one"),
        "the gate judged the layer's arguments, not the model's"
    );
    assert!(
        r.render().contains("the-rewritten-one"),
        "and the tool ran with them: {}",
        r.render()
    );
    assert!(
        r.render().contains("rewrote this call's arguments"),
        "the result says the call was rewritten, so the row is not a mystery: {}",
        r.render()
    );
}

/// A layer can refuse a call, and then nothing runs. The outcome says who refused and the
/// payload is the layer's own sentence, which is what the model acts on.
#[test]
fn a_refused_call_does_not_run() {
    let (rt, seen, _calls, ran, _d) = hooked(Some(letibot_plugin::tool_call::ToolCallOut::refuse(
        "already watched as pr-24; use that",
    )));
    let mut sink = RecordingToolSink::default();
    let r = rt.invoke("t1", &call("probe", r#"{"path":"/tmp/x"}"#), &mut sink);
    assert!(
        !ran.load(std::sync::atomic::Ordering::SeqCst),
        "it ran anyway"
    );
    assert!(
        seen.lock().unwrap_or_else(|e| e.into_inner()).is_empty(),
        "a refused call never reached the gate"
    );
    assert!(
        matches!(&r.outcome, ToolOutcome::NotRun { why } if why.contains("p1")),
        "{:?}",
        r.outcome
    );
    assert!(r.render().contains("already watched as pr-24"));
}

/// A layer can answer a call without letting it run, which is the model asking something the
/// layer already knows.
#[test]
fn a_layer_can_answer_a_call_without_running_it() {
    let (rt, _seen, _calls, ran, _d) = hooked(Some(
        letibot_plugin::tool_call::ToolCallOut::answer("pr-24 is already watched for its checks"),
    ));
    let mut sink = RecordingToolSink::default();
    let r = rt.invoke("t1", &call("probe", r#"{"path":"/tmp/x"}"#), &mut sink);
    assert!(
        !ran.load(std::sync::atomic::Ordering::SeqCst),
        "it ran anyway"
    );
    assert_eq!(r.outcome, ToolOutcome::Ok);
    assert!(r.render().contains("already watched for its checks"));
}

/// An answered call says whose words these are, and that the tool did not run. Without this
/// line a model reads an `answer` exactly as it reads a tool's output, so a `monitor` that
/// never armed is a fact about the world nobody stated. The line goes in `[note]`.
#[test]
fn an_answered_call_is_attributed_and_the_payload_is_otherwise_unchanged() {
    let d = crate::backend::tempdir::TempDir::new();
    let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut reg = Registry::new();
    reg.register(Box::new(RanProbe { ran: ran.clone() }))
        .unwrap();
    let rt = ToolRuntime::new(
        reg,
        Box::new(crate::backend::HostBackend::new(d.path()).unwrap()),
    )
    .with_gate(Box::new(AdmitGate))
    .with_call_hook(std::sync::Arc::new(FixedHook {
        answer: Some(letibot_plugin::tool_call::ToolCallOut::answer(
            "pr-24 is watched for its checks",
        )),
        seen: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        note: Some("answered by plugin pr-watch (project); the tool did not run".to_string()),
    }));
    let mut sink = RecordingToolSink::default();
    let r = rt.invoke("t1", &call("probe", r#"{"path":"/tmp/x"}"#), &mut sink);
    assert!(!ran.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(
        r.payload, "pr-24 is watched for its checks",
        "the payload is the layer's own words, unchanged"
    );
    let rendered = r.render();
    assert_eq!(
        r.notes,
        vec!["answered by plugin pr-watch (project); the tool did not run".to_string()]
    );
    assert!(
        rendered.contains("[note] answered by plugin pr-watch (project); the tool did not run"),
        "the model is not told whose words it is reading: {rendered}"
    );
    assert!(
        rendered.contains("pr-24 is watched for its checks"),
        "{rendered}"
    );
}

/// `pass`, and a layer that says nothing, both leave the call exactly as it was.
#[test]
fn a_pass_runs_the_call_as_the_model_proposed_it() {
    for answer in [Some(letibot_plugin::tool_call::ToolCallOut::pass()), None] {
        let (rt, seen, _calls, ran, _d) = hooked(answer);
        let mut sink = RecordingToolSink::default();
        let r = rt.invoke(
            "t1",
            &call("probe", r#"{"path":"/tmp/as-asked"}"#),
            &mut sink,
        );
        assert!(ran.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(r.outcome, ToolOutcome::Ok);
        let judged = seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert_eq!(judged[0]["path"], serde_json::json!("/tmp/as-asked"));
        assert!(!r.render().contains("rewrote this call"));
    }
}

/// The operator's own run is not shown to the layer. `invoke_operator` is the `!` line and
/// the head-run door, a person's own act adjudicated at the door, and a layer rewriting it
/// would be policy over the person.
#[test]
fn the_operators_own_call_is_not_shown_to_the_layer() {
    let (rt, _seen, calls, ran, _d) = hooked(Some(letibot_plugin::tool_call::ToolCallOut::refuse(
        "not on my watch",
    )));
    let mut sink = RecordingToolSink::default();
    let _ = rt.invoke_operator("t1", &call("probe", r#"{"path":"/tmp/x"}"#), &mut sink);
    assert!(
        ran.load(std::sync::atomic::Ordering::SeqCst),
        "the operator's call was stopped"
    );
    assert!(
        calls.lock().unwrap_or_else(|e| e.into_inner()).is_empty(),
        "the ungated path showed the call to the layer"
    );
}

/// A rewrite the tool would refuse is reported, not run: a layer's arguments go through
/// the same `salvage` the model's do, so a layer cannot put a shape past the schema.
#[test]
fn a_rewrite_the_tool_refuses_does_not_run() {
    let (rt, seen, _calls, ran, _d) =
        hooked(Some(letibot_plugin::tool_call::ToolCallOut::rewrite(
            serde_json::json!({"nothing": "the schema wants"}),
        )));
    let mut sink = RecordingToolSink::default();
    let r = rt.invoke("t1", &call("probe", r#"{"path":"/tmp/x"}"#), &mut sink);
    // `salvage` repairs what it can; a `required` path it cannot invent is a refusal.
    if ran.load(std::sync::atomic::Ordering::SeqCst) {
        // A repair was possible: then the gate must have judged the repaired arguments.
        let judged = seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert!(
            !judged.is_empty(),
            "a repaired rewrite ran without being adjudicated"
        );
    } else {
        assert!(
            matches!(r.outcome, ToolOutcome::NotRun { .. }),
            "{:?}",
            r.outcome
        );
        assert!(
            r.render().contains("p1"),
            "the layer is named: {}",
            r.render()
        );
    }
}
