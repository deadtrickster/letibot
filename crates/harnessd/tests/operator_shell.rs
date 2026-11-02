//! The operator's own shell line (`!`), run by the daemon - the run half of the feature
//! whose wire half is `letibot-sessionlog`'s `operator_shell.rs`. Three properties

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use letibot_harnessd::config::{Config, Seat};
use letibot_harnessd::{Dialect, Harness, Parts};
use letibot_sessionlog::client::{HeadClient, pump};
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::protocol::{Caps, ClientFrame, ServerFrame};
use letibot_sessionlog::server::serve_registry;
use letibot_sessionlog::{Registry, SessionWiring, SnapshotItem};
use letibot_tools::{AdjudicationDecision, AdjudicationRequest, Adjudicator};
use letibot_transcript::{CallOrigin, Speaker, TranscriptItem};

mod support;

fn config(session: &str, socket: &std::path::Path) -> Config {
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("the workspace root is two levels above this crate")
        .to_path_buf();
    let mut cfg = Config::for_this_box(repo);
    // Not this box's configuration. These tests assert what the fixture supplies, not what the
    // operator's `~/.config/letibot` happens to hold.
    cfg.web_search = None;
    cfg.permission = Vec::new();
    cfg.dialect = Dialect::Qwen;
    cfg.session_id = session.into();
    cfg.socket = socket.to_path_buf();
    // A seat that can run commands: unconfined (`leticode`), `bash` seated, and a
    // mode that needs no oracle - the point here is the exec path, not the mode.
    cfg.seat = Seat::Leticode;
    cfg.allow_bash = true;
    // `Config::for_this_box` carries no vocabulary default any more, so a harness built here is
    // handed one: the same path this file's gate consults.
    cfg.vocab_gguf = support::vocab::gguf();
    cfg
}

fn parts(cfg: &Config) -> Parts {
    let p = Parts::load(cfg).expect(
        "the vocabulary must load; set LETIBOT_VOCAB_GGUF if this box is not the one \
         this repository is developed on",
    );
    // The operator's per-project mode is not this test's business (`wired.rs`'s rule).
    *p.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    p
}

/// An adjudicator that counts its calls and would answer *nobody decided*. It exists so
/// that "the gate was not consulted" is a measurement rather than an inference from the
/// call having succeeded
struct NeverAsked(Arc<AtomicUsize>);

impl Adjudicator for NeverAsked {
    fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
        self.0.fetch_add(1, Ordering::SeqCst);
        AdjudicationDecision::unavailable(
            req,
            "test",
            "this adjudicator exists to prove it is never asked",
        )
    }

    fn describe(&self) -> String {
        "test adjudicator (refuses, and counts)".into()
    }
}

fn socket_path(tag: &str) -> std::path::PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    letibot_sessionlog::testing::scratch_root().join(format!(
        "letibot-bang-{tag}-{}-{n}.sock",
        std::process::id()
    ))
}

/// The whole run: a `!` line becomes two rows, the command actually ran, and no
/// permission decision was made anywhere on the way.
#[test]
fn a_bang_line_runs_in_the_workspace_and_lands_as_two_rows_with_no_decision() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let socket = socket_path("rows");
    let cfg = config("bang-rows", &socket);
    let p = parts(&cfg);
    let hub = Hub::new(&cfg.session_id);
    let asked = Arc::new(AtomicUsize::new(0));
    let opened = Harness::open_with(
        &p,
        cfg,
        hub.clone(),
        Some(Box::new(NeverAsked(asked.clone()))),
        None,
    );
    let mut h = match opened {
        Ok(h) => h,
        // An exec host needs a delegated cgroup v2 subtree; a host without one is a real host, and
        // the refusal is asserted rather than skipped - a green test that measured nothing is bad.
        Err(e) => {
            let msg = format!("{e}");
            assert!(
                msg.contains("cgroup") || msg.contains("reap") || msg.contains("exec"),
                "a refusal must name what was missing: {msg}"
            );
            eprintln!("bang-rows: no exec host on this box, refusing open: {msg}");
            return;
        }
    };

    // The marker is a file the command writes in the session's workspace, so the
    // assertion is on the disk rather than on the tool's opinion of itself.
    let marker = format!("bang-marker-{}", std::process::id());
    let line = format!("! printf 'the-operator-ran-this' > {marker} && cat {marker}");
    h.run_operator_shell(&line, "dead")
        .expect("the operator's own command runs and is recorded");

    // What a head would receive, read the way a head reads it: the snapshot.
    let snap = hub.snapshot();
    let items: Vec<&TranscriptItem> = snap.items.iter().filter_map(|i| i.item.as_ref()).collect();
    // The operator's own row first: their words, verbatim, in their own voice.
    let user = items.iter().position(|i| {
        matches!(
            i,
            TranscriptItem::User { speaker: Speaker::Operator, parts }
                if parts.iter().any(|p| matches!(p, letibot_transcript::UserPart::Text { text } if text == &line))
        )
    });
    let at = user.unwrap_or_else(|| {
        panic!("the typed line never landed as the operator's own row: {items:?}")
    });
    // Then the result, AFTER it, named `bash` and attributed to the operator.
    match &items[at + 1] {
        TranscriptItem::ToolResult {
            name,
            outcome,
            payload,
            origin: Some(CallOrigin::Operator { who }),
            ..
        } => {
            assert_eq!(name, "bash");
            assert_eq!(who, "dead");
            assert!(
                matches!(outcome, letibot_transcript::ToolOutcome::Ok),
                "the command ran: {payload}"
            );
            assert!(
                payload.contains("the-operator-ran-this"),
                "the row carries the command's output: {payload}"
            );
        }
        other => panic!("the row after the line is not the command's result: {other:?}"),
    }
    // And the command really did run, where it was told to.
    let wrote = std::fs::read_to_string(h.workspace().join(&marker)).unwrap_or_default();
    assert_eq!(wrote, "the-operator-ran-this");
    let _ = std::fs::remove_file(h.workspace().join(&marker));

    // No permission decision anywhere on the way - three half-checks, because the decision has
    // three places it could show up: the adjudicator, the session log, and the outcome.
    assert_eq!(
        asked.load(Ordering::SeqCst),
        0,
        "the `!` path consulted the adjudicator"
    );
    for env in hub.retained() {
        assert!(
            !matches!(
                env.event,
                SessionEvent::DecisionRequested { .. } | SessionEvent::OperatorCallAllowed { .. }
            ),
            "a permission decision appeared for the operator's own command: {:?}",
            env.event
        );
    }
    // The size disclosure the door's runs make, on this path too.
    assert!(
        hub.retained().iter().any(|env| matches!(
            &env.event,
            SessionEvent::Warning { code, .. } if code == "operator_shell_ran"
        )),
        "the run must disclose what it put in the conversation"
    );
}

/// The turn that starts reads the payload - the half the two rows do not prove by
/// themselves.
#[test]
fn the_turn_the_line_starts_hands_the_model_the_commands_output() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let socket = socket_path("model-reads");
    let cfg = config("bang-model-reads", &socket);
    let p = parts(&cfg);
    let hub = Hub::new(&cfg.session_id);
    let mut h = match Harness::open_with(&p, cfg, hub.clone(), None, None) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("bang-model-reads: no exec host on this box: {e}");
            return;
        }
    };
    let marker = format!("bang-reads-marker-{}", std::process::id());
    // The output is not in the command line, and that is what makes this a test: the first
    // draft `printf`ed its own marker, so the assertion passed against the LINE while the result
    // was being dropped. The file is seeded here and the command only names it.
    let output = format!("the-model-must-read-{}", std::process::id());
    std::fs::write(h.workspace().join(&marker), &output).expect("seed the file the command reads");
    let line = format!("! cat {marker}");
    assert!(
        !line.contains(&output),
        "the typed line must not carry the output, or this test measures the wrong row: {line}"
    );
    h.run_operator_shell(&line, "dead")
        .expect("the operator's own command runs and is recorded");

    // The rows the turn reads, in the order the turn reads them: everything the
    // deposit left, exactly as the transcript holds it.
    let items: Vec<TranscriptItem> = hub
        .snapshot()
        .items
        .iter()
        .filter_map(|i| i.item.clone())
        .collect();
    assert!(
        items
            .iter()
            .any(|i| matches!(i, TranscriptItem::User { .. })),
        "the operator's own line is not in what the model would read: {items:?}"
    );

    // What the model is handed. A model that takes pictures: this test is about which ROWS reach
    // it, and a text-only model would answer a `read` image with a placeholder instead.
    let messages = letibot_provider::messages::convert(
        "be terse",
        &items,
        false,
        letibot_provider::messages::Eyes::Sees,
        letibot_provider::messages::Withheld::nothing(),
    );
    let said: Vec<&str> = messages
        .iter()
        .filter_map(|m| m["content"].as_str())
        .collect();
    assert!(
        said.iter().any(|c| c.contains(&output)),
        "the command's output never reaches the model \u{2014} this is the defect: {messages:?}"
    );
    // And it is the person's, not the model's. No assistant `tool_calls` entry was invented
    // to own it and no `tool` message answers one: the model is handed the output as the
    // operator's own turn, which is the shape the local dialects already render for this row.
    assert!(
        !messages.iter().any(|m| m["role"] == "tool"),
        "the result was sent as a tool message answering nothing: {messages:?}"
    );
    let carried = messages
        .iter()
        .find(|m| m["content"].as_str().is_some_and(|c| c.contains(&output)))
        .expect("just asserted");
    assert_eq!(
        carried["role"], "user",
        "the operator's own run is the person's turn: {carried:?}"
    );
    let _ = std::fs::remove_file(h.workspace().join(&marker));
}

/// A command that cannot start is reported, not swallowed. `bash`'s own refusal lands as the
/// `ToolResult` row with the outcome the tool gave, so the operator reads it where they read every
/// other result.
#[test]
fn a_command_that_cannot_start_lands_as_a_failed_row() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let socket = socket_path("nostart");
    let cfg = config("bang-nostart", &socket);
    let p = parts(&cfg);
    let hub = Hub::new(&cfg.session_id);
    let mut h = match Harness::open_with(&p, cfg, hub.clone(), None, None) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("bang-nostart: no exec host on this box: {e}");
            return;
        }
    };
    // A command that cannot start: the shell is asked to `exec` a binary that does not exist.
    h.run_operator_shell("! this-binary-does-not-exist-anywhere", "dead")
        .expect("the run itself records a row");
    let snap = hub.snapshot();
    let said = snap
        .items
        .iter()
        .filter_map(|i| i.item.as_ref())
        .find_map(|i| match i {
            TranscriptItem::ToolResult { payload, .. } => Some(payload.clone()),
            _ => None,
        })
        .expect("the failure landed as a row");
    assert!(
        said.contains("this-binary-does-not-exist-anywhere") && said.contains("not found"),
        "the row names the command and what happened to it: {said}"
    );
    assert!(
        said.contains("[exit ") || said.contains("exit"),
        "a non-zero exit is the command's answer, not a harness failure: {said}"
    );
}

// ---------------------------------------------------------------------------
// sudo: the password ask
// ---------------------------------------------------------------------------

/// The `!` path's command can ask the head for a password. The link this test adds: the
/// command runs with the standing environment (`LETIBOT_SOCKET` `LETIBOT_SESSION`
/// `SUDO_ASKPASS`) and the `sudo` shim first on `PATH`, wired at open.
#[test]
fn a_sudo_inside_a_bang_line_asks_the_head_for_the_password_and_gets_it() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let socket = socket_path("sudo");
    let cfg = config("bang-sudo", &socket);
    // Leaked on purpose. The harness borrows `Parts` for its life and the run must happen on
    // another thread (the worker blocks inside the command while this thread answers the ask).
    let p: &'static Parts = Box::leak(Box::new(parts(&cfg)));
    let hub = Hub::new(&cfg.session_id);
    let h = match Harness::open_with(p, cfg.clone(), hub.clone(), None, None) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("bang-sudo: no exec host on this box: {e}");
            return;
        }
    };
    // The registry that serves the socket, so the helper's attach reaches THIS hub.
    let registry = Registry::of(hub.clone());
    registry
        .create(&cfg.session_id, "", SessionWiring::default())
        .ok();
    let server = serve_registry(registry, &socket).expect("bind");
    // The harness borrows `Parts` for its life, and the runner thread below takes
    // the harness while this thread keeps `p` - both alive to the end of the test.
    let h = h;

    // The two frames the double sends, serialized here so the double cannot drift.
    let attach = serde_json::to_string(&ClientFrame::Attach {
        protocol_version: letibot_sessionlog::PROTOCOL_VERSION,
        session_id: String::new(), // the double fills it from $LETIBOT_SESSION
        since_seq: u64::MAX,
        kind: "askpass".into(),
        identity: "sudo".into(),
        caps: Caps {
            can_decide: false,
            ..Caps::default()
        },
    })
    .expect("attach line");
    let askpass = serde_json::to_string(&ClientFrame::Askpass {
        prompt: String::new(),
        command: String::new(),
    })
    .expect("askpass line");

    // The double. `#!/usr/bin/python3` because the exec host clears the environment
    // and pins PATH - an absolute interpreter is the one spelling that survives.
    let dir = letibot_sessionlog::testing::scratch_root()
        .join(format!("letibot-bang-askpass-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let double = dir.join("askpass-double");
    letibot_tools::testing::write_stub(
        &double,
        &format!(
            r#"#!/usr/bin/python3
# letibot test double for `sudo -A` + `letibot-askpass`: attach as an askpass
# head, ask for the password for `command`, read the answer. Prints a MARKER
# rather than the password, so the transcript can never hold the secret even
# by accident of this test.
import json, os, socket, sys
if len(sys.argv) < 3:
    # `letibot_tools::testing::write_stub`'s probe: this double needs a prompt and
    # a command, and a no-argument run must prove nothing but that the file can
    # be exec'd.
    sys.exit(0)
prompt, command = sys.argv[1], sys.argv[2]
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect(os.environ["LETIBOT_SOCKET"])
f = s.makefile("rw")
def send(line):
    f.write(json.dumps(line) + "\n")
    f.flush()
attach = json.loads({attach:?})
attach["session_id"] = os.environ["LETIBOT_SESSION"]
send(attach)
ask = json.loads({askpass:?})
ask["prompt"] = prompt
ask["command"] = command
send(ask)
for line in f:
    m = json.loads(line)
    if isinstance(m, dict) and m.get("frame") == "secret":
        got = m["secret"].get("secret") if isinstance(m["secret"], dict) else m["secret"]
        print("askpass-double: the head answered" if got else "askpass-double: no password came")
        sys.exit(0 if got else 1)
print("askpass-double: no answer")
sys.exit(1)
"#,
            attach = attach,
            askpass = askpass,
        ),
    );

    // The head, attached before the command runs, that will answer the ask.
    let (mut head, _hello, reader) =
        HeadClient::attach(&socket, &cfg.session_id, 0, "tui", "dead", Caps::default())
            .expect("head attach");
    let (tx, rx) = std::sync::mpsc::channel();
    let _pump = std::thread::spawn(move || pump(reader, tx));

    // The `!` line, run on the worker's own thread - the operator's command blocks
    // inside it while the password is outstanding, exactly as a real `sudo` would.
    let line = format!(
        "! {double} '[sudo] password for dead: ' 'sudo true'",
        double = double.display()
    );
    let runner = std::thread::spawn(move || {
        let (line, mut h) = (line, h);
        h.run_operator_shell(&line, "dead")
            .expect("the run records")
    });

    // The ask arrives at the head with the command on it, and the answer goes back.
    let deadline = Instant::now() + Duration::from_secs(20);
    let req_id = loop {
        assert!(
            Instant::now() < deadline,
            "no SecretRequested reached the head"
        );
        let Ok(inbound) = rx.recv_timeout(Duration::from_millis(200)) else {
            continue;
        };
        if let ServerFrame::Event(env) = inbound.frame()
            && let SessionEvent::SecretRequested {
                req_id,
                prompt,
                command,
                ..
            } = &env.event
        {
            assert!(prompt.contains("password for dead"), "{prompt}");
            assert_eq!(command, "sudo true");
            break req_id.clone();
        }
    };
    head.secret(&req_id, Some("hunter2".into()))
        .expect("answer the ask");
    runner.join().expect("the run finished");

    // The helper's own report is the command's output, so it is the row's payload -
    // and the password is nowhere the transcript or the log can reach.
    let snap = hub.snapshot();
    let payload = item_payloads(&snap).join("\n");
    assert!(
        payload.contains("askpass-double: the head answered"),
        "the helper got the password the head typed: {payload}"
    );
    let everything = format!("{:?} {:?}", hub.snapshot(), hub.retained());
    assert!(
        !everything.contains("hunter2"),
        "the secret leaked into the session's state"
    );
    server.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

fn item_payloads(snap: &letibot_sessionlog::Snapshot) -> Vec<String> {
    snap.items
        .iter()
        .filter_map(|i: &SnapshotItem| i.item.as_ref())
        .filter_map(|i| match i {
            TranscriptItem::ToolResult { payload, .. } => Some(payload.clone()),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The prompt card: the operator's own run can be ANSWERED
// ---------------------------------------------------------------------------

/// The whole thing, end to end, through the real path. The daemon holds a pipe on the run's
/// stdin; the run is blocked reading it, which `exec::ask` reads out of `/proc` - the
/// process, not its words - and which is what raises the card
#[test]
fn a_bang_line_that_asks_is_answered_and_finishes() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // Linux's reading, asserted. The card carries the program's own last line only where the
    // daemon can see that the program is blocked on its terminal, and Darwin exposes neither
    // `wchan` nor `/proc/<tid>/syscall`: a Mac raises the unreadable offer instead.
    let Some(_) = letibot_tokencore::apparatus::applies(
        "reading that a program waits on its terminal (wchan or /proc/<tid>/syscall)",
        cfg!(target_os = "linux"),
    ) else {
        return;
    };
    let socket = socket_path("answered");
    let cfg = config("bang-answered", &socket);
    // Leaked on purpose. The harness borrows `Parts` for its life and the run must happen on
    // another thread (the worker blocks inside the command while this thread answers), so the
    // borrow has to be `'static`.
    let p: &'static Parts = Box::leak(Box::new(parts(&cfg)));
    let hub = Hub::new(&cfg.session_id);
    // The registry the harness is opened WITH, and not a second one built afterwards: the
    // daemon installs this session's stdin driver on the registry it was handed, so serving a
    // different one would be testing a socket with no way in.
    let registry = Registry::of(hub.clone());
    let mut h = match Harness::open_with_registry(
        p,
        cfg.clone(),
        hub.clone(),
        None,
        None,
        registry.clone(),
    ) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("bang-answered: no exec host on this box: {e}");
            return;
        }
    };
    let server = serve_registry(registry, &socket).expect("bind");

    let (mut head, _hello, reader) =
        HeadClient::attach(&socket, &cfg.session_id, 0, "tui", "dead", Caps::default())
            .expect("head attach");
    let (tx, rx) = std::sync::mpsc::channel();
    let _pump = std::thread::spawn(move || pump(reader, tx));

    // A program that prints a prompt and then BLOCKS on its stdin. `read` is a builtin in
    // every shell this host can be configured with.
    let line = "! printf 'Continue? [Y/n] '; read x; printf 'answered=%s\\n' \"$x\"";
    let runner = std::thread::spawn({
        let line = line.to_string();
        move || h.run_operator_shell(&line, "dead")
    });

    // The card, as a head receives it.
    let deadline = Instant::now() + Duration::from_secs(30);
    let req_id = loop {
        assert!(
            Instant::now() < deadline,
            "no PromptRequested reached the head — the run is blocked on a pipe and nothing \
             said so"
        );
        let Ok(inbound) = rx.recv_timeout(Duration::from_millis(200)) else {
            continue;
        };
        if let ServerFrame::Event(env) = inbound.frame()
            && let SessionEvent::PromptRequested {
                req_id,
                command,
                question,
                ..
            } = &env.event
        {
            assert!(
                command.contains("read x"),
                "the card names the operator's own command: {command}"
            );
            assert_eq!(
                question.as_deref(),
                Some("Continue? [Y/n]"),
                "the card SHOWS the program's own last line"
            );
            break req_id.clone();
        }
    };
    head.prompt_answer(&req_id, "Y")
        .expect("the answer is written");
    runner
        .join()
        .expect("the run thread")
        .expect("the run records");

    // The command read the answer and finished with it in its own output. This is the
    // assertion the branch exists for.
    let payload = item_payloads(&hub.snapshot()).join("\n");
    assert!(
        payload.contains("answered=Y"),
        "the answer must reach the command's stdin and come back out of it: {payload}"
    );
    // And the ending the `!` path already added is still there - the program's last words and
    // its status. An answered prompt must not cost the ending that was there before it.
    let outcome = hub
        .snapshot()
        .items
        .iter()
        .filter_map(|i| i.item.as_ref())
        .find_map(|i| match i {
            TranscriptItem::ToolResult { outcome, .. } => Some(outcome.clone()),
            _ => None,
        })
        .expect("the result row");
    assert!(
        matches!(outcome, letibot_transcript::ToolOutcome::Ok),
        "the answered command exited cleanly and the row says so: {outcome:?}"
    );
    // And the settlement is on the log - who answered, and never the line.
    let settled = hub.retained().into_iter().find_map(|env| match env.event {
        SessionEvent::PromptSettled { req_id, sent, by } => Some((req_id, sent, by)),
        _ => None,
    });
    match settled {
        Some((id, sent, by)) => {
            assert_eq!(id, req_id);
            assert!(sent, "a line was sent");
            assert_eq!(by, "dead");
        }
        None => panic!("the card was never settled"),
    }
    let everything = format!("{:?}", hub.retained());
    assert!(
        !everything.contains("\"Y\""),
        "the line itself must not be on the log"
    );
    server.shutdown();
}

/// The manual way in reaches a running command's stdin - and it needs no card at all. The
/// floor under the heuristic: the card is raised by a reading of `/proc` that has misses it
/// names, and a person watching the stream can always answer.
#[test]
fn the_manual_send_reaches_a_running_commands_stdin() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let socket = socket_path("manual");
    let cfg = config("bang-manual", &socket);
    let p: &'static Parts = Box::leak(Box::new(parts(&cfg)));
    let hub = Hub::new(&cfg.session_id);
    let registry = Registry::of(hub.clone());
    let mut h = match Harness::open_with_registry(
        p,
        cfg.clone(),
        hub.clone(),
        None,
        None,
        registry.clone(),
    ) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("bang-manual: no exec host on this box: {e}");
            return;
        }
    };
    let server = serve_registry(registry, &socket).expect("bind");
    let (mut head, _hello, reader) =
        HeadClient::attach(&socket, &cfg.session_id, 0, "tui", "dead", Caps::default())
            .expect("head attach");
    let (tx, rx) = std::sync::mpsc::channel();
    let _pump = std::thread::spawn(move || pump(reader, tx));

    let line = "! read x; printf 'sent=%s\\n' \"$x\"";
    let runner = std::thread::spawn({
        let line = line.to_string();
        move || h.run_operator_shell(&line, "dead")
    });
    // No card is waited for. The verb is the way in whether or not anything looked like a
    // question, which is the whole point of it being a verb. Sent until it is read, not once
    // after a fixed sleep
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while !runner.is_finished() {
        assert!(
            std::time::Instant::now() < deadline,
            "`read x` never received the verb's line in 60 s"
        );
        let _ = head.send_line("hello from the verb");
        std::thread::sleep(Duration::from_millis(500));
    }
    runner
        .join()
        .expect("the run thread")
        .expect("the run records");

    let payload = item_payloads(&hub.snapshot()).join("\n");
    assert!(
        payload.contains("sent=hello from the verb"),
        "`!send` must reach the running command's stdin: {payload}"
    );
    let _ = rx;
    server.shutdown();
}

/// A program that prints a question and EXITS raises no card. The control, and the one that
/// would have failed before the detection was corrected
#[test]
fn a_program_that_prints_a_question_and_exits_raises_no_card() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // Linux's reading, asserted. The card carries the program's own last line only where the
    // daemon can see that the program is blocked on its terminal, and Darwin exposes neither
    // `wchan` nor `/proc/<tid>/syscall`: a Mac raises the unreadable offer instead.
    let Some(_) = letibot_tokencore::apparatus::applies(
        "reading that a program waits on its terminal (wchan or /proc/<tid>/syscall)",
        cfg!(target_os = "linux"),
    ) else {
        return;
    };
    // ---- 1. A question, and a clean exit.
    {
        let socket = socket_path("noquestion");
        let cfg = config("bang-noquestion", &socket);
        let p: &'static Parts = Box::leak(Box::new(parts(&cfg)));
        let hub = Hub::new(&cfg.session_id);
        let registry = Registry::of(hub.clone());
        let mut h = match Harness::open_with_registry(
            p,
            cfg.clone(),
            hub.clone(),
            None,
            None,
            registry.clone(),
        ) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("bang-noquestion: no exec host on this box: {e}");
                return;
            }
        };
        let server = serve_registry(registry, &socket).expect("bind");
        let (mut head, _hello, reader) =
            HeadClient::attach(&socket, &cfg.session_id, 0, "tui", "dead", Caps::default())
                .expect("head attach");
        let (tx, rx) = std::sync::mpsc::channel();
        let _pump = std::thread::spawn(move || pump(reader, tx));
        h.run_operator_shell("! printf 'Are you sure? [Y/n] '", "dead")
            .expect("the run records");
        assert!(
            !hub.retained()
                .into_iter()
                .any(|e| matches!(e.event, SessionEvent::PromptRequested { .. })),
            "a program that printed a question and EXITED must raise no card: there is \
             nobody left to answer it and nothing is blocked"
        );
        // And the ending the `!` path adds is still there - an answered prompt, or a run that
        // ends, still leaves the row with the program's last words and its status.
        let payload = item_payloads(&hub.snapshot()).join("\n");
        assert!(
            payload.contains("Are you sure? [Y/n]"),
            "the program's last words are on the row: {payload}"
        );
        let _ = (&mut head, &rx);
        server.shutdown();
    }
    // ---- 2. No question at all, and a block: the card is raised anyway.
    {
        let socket = socket_path("noquestion-blocked");
        let cfg = config("bang-blocked", &socket);
        let p: &'static Parts = Box::leak(Box::new(parts(&cfg)));
        let hub = Hub::new(&cfg.session_id);
        let registry = Registry::of(hub.clone());
        let mut h = match Harness::open_with_registry(
            p,
            cfg.clone(),
            hub.clone(),
            None,
            None,
            registry.clone(),
        ) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("bang-blocked: no exec host on this box: {e}");
                return;
            }
        };
        let server = serve_registry(registry, &socket).expect("bind");
        let (mut head, _hello, reader) =
            HeadClient::attach(&socket, &cfg.session_id, 0, "tui", "dead", Caps::default())
                .expect("head attach");
        let (tx, rx) = std::sync::mpsc::channel();
        let _pump = std::thread::spawn(move || pump(reader, tx));
        let line = "! printf 'working...'; read x; printf 'ok\\n'";
        let runner = std::thread::spawn({
            let line = line.to_string();
            move || h.run_operator_shell(&line, "dead")
        });
        let deadline = Instant::now() + Duration::from_secs(30);
        let (req_id, question) = loop {
            assert!(
                Instant::now() < deadline,
                "a run blocked on the pipe must raise a card whatever it printed"
            );
            let Ok(inbound) = rx.recv_timeout(Duration::from_millis(200)) else {
                continue;
            };
            if let ServerFrame::Event(env) = inbound.frame()
                && let SessionEvent::PromptRequested {
                    req_id, question, ..
                } = &env.event
            {
                break (req_id.clone(), question.clone());
            }
        };
        assert_eq!(
            question.as_deref(),
            Some("working..."),
            "the card shows what the program said, and decides nothing by it"
        );
        head.prompt_answer(&req_id, "")
            .expect("a bare Enter is a real answer");
        runner
            .join()
            .expect("the run thread")
            .expect("the run records");
        let payload = item_payloads(&hub.snapshot()).join("\n");
        assert!(
            payload.contains("ok"),
            "the empty line released the read: {payload}"
        );
        server.shutdown();
    }
}
