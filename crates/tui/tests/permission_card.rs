//! The answer half of a permission card, end to end: a real `letibot-tui` on a pty, a real daemon
//! socket, and the gate's own ask (`harnessd::answers::HeadAdjudicator`) between them. Nothing here
//! is a fixture of the card - the card the head draws is the one the gate posted, and the answer the
//! gate acts on is the one a person pressed.
//!
//! Three answers, and each is one of the item's bars: a digit answers, a typed refusal reaches the
//! model as its reason, and an always-allow shows the rule it is about to write and then writes it
//! to `permission.json`. A fourth call proves the written rule admits without asking.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use letibot_harnessd::answers::{Answers, HeadAdjudicator};
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::registry::Registry;
use letibot_sessionlog::server::serve_registry_rooted;
use letibot_tools::adjudicate::AdjudicatedGate;
use letibot_tools::mode::Mode;
use letibot_tools::schema::Access;
use letibot_tools::{Gate, GateCall, GateDecision};
use letibot_vt::Screen;

const SIZE: (usize, usize) = (100, 32);

/// A scratch directory that survives the test's own failure, so a stuck run leaves its state.
fn scratch(tag: &str) -> PathBuf {
    let mut p = letibot_sessionlog::testing::scratch_root();
    p.push(format!("letibot-permcard-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).expect("a scratch dir");
    p
}

/// The head on a pty, sized, with the screen its bytes draw kept current by a reader thread.
struct Head {
    child: std::process::Child,
    writer: std::fs::File,
    screen: Arc<Mutex<Screen>>,
}

impl Head {
    fn start(socket: &Path, home: &Path) -> Head {
        let pty = letibot_tools::exec::pty::Pty::open().expect("a pty");
        set_size(pty.slave_fd(), SIZE);
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_letibot-tui"));
        cmd.arg("--socket")
            .arg(socket)
            .env("HOME", home)
            .env("TERM", "xterm-256color")
            .env("USER", "deadtrickster")
            .stdin(pty.stdio().expect("stdin"))
            .stdout(pty.stdio().expect("stdout"))
            .stderr(pty.stdio().expect("stderr"));
        letibot_tools::exec::pty::controlling_terminal(&mut cmd, pty.slave_fd());
        let child = cmd.spawn().expect("the head starts");
        let writer = pty.input().expect("a way to type at it");
        let master = pty.into_master();
        let screen = Arc::new(Mutex::new(Screen::new(SIZE.1, SIZE.0)));
        let sink = screen.clone();
        std::thread::spawn(move || {
            let mut master = master;
            let mut buf = [0u8; 8192];
            // Until the head closes its terminal: this thread owns the only handle, so the loop's
            // end is the process's.
            while let Ok(n) = master.read(&mut buf) {
                if n == 0 {
                    break;
                }
                sink.lock().expect("screen").feed(&buf[..n]);
            }
        });
        Head {
            child,
            writer,
            screen,
        }
    }

    /// What the head is drawing, one string.
    fn drawn(&self) -> String {
        let s = self.screen.lock().expect("screen");
        (0..SIZE.1)
            .map(|r| s.line(r))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Wait for the head to draw something, or say what it was drawing instead. The card is read
    /// before the keys are pressed, and a card a frame opened ignores the key that would answer it
    /// for its first half second - see `DEC_ARM_MS`.
    fn wait_for(&self, what: &str, needle: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let drawn = self.drawn();
            if drawn.contains(needle) {
                std::thread::sleep(Duration::from_millis(700));
                return self.drawn();
            }
            if Instant::now() > deadline {
                panic!("timed out waiting for {what}. The head drew:\n{drawn}");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn press(&mut self, keys: &str) {
        self.writer.write_all(keys.as_bytes()).expect("the keys");
        self.writer.flush().expect("flush");
        // A pause between strokes keeps two keys out of one read, which is how a person types.
        std::thread::sleep(Duration::from_millis(150));
    }
}

impl Drop for Head {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The digit that marks the rung whose label says `label`, read off the card the head drew: the
/// ladder's order is the gate's to choose, and a test that hard-coded it would break on a rung the
/// gate started offering.
fn rung(drawn: &str, matches: impl Fn(&str) -> bool) -> char {
    for line in drawn.lines() {
        let t = line.trim().trim_start_matches('▸').trim();
        let Some((num, label)) = t.split_once("  ") else {
            continue;
        };
        let Ok(n) = num.trim().parse::<u8>() else {
            continue;
        };
        if (1..=9).contains(&n) && matches(label.trim()) {
            return char::from(b'0' + n);
        }
    }
    panic!("no rung matching on the card:\n{drawn}");
}

fn set_size(fd: std::os::fd::RawFd, (cols, rows): (usize, usize)) {
    let ws = libc::winsize {
        ws_row: rows as u16,
        ws_col: cols as u16,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let rc = unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &ws) };
    assert_eq!(rc, 0, "the pty takes a window size");
}

/// One call the gate was asked about, and what it did with it.
struct Verdict {
    decision: GateDecision,
    /// The gate's own row for the ask: the option a person selected and the basis the model is
    /// handed, which is the fact "the reason reached the model" is measured on.
    option: Option<String>,
    basis: String,
}

/// What the gate knows about where it is standing: a pinned shell in the scratch workspace, and a
/// home that is not the box's own.
fn surroundings(workspace: &Path) -> letibot_tools::intent::Surroundings {
    letibot_tools::intent::Surroundings {
        home: Some("/home/deadtrickster".into()),
        workspace: Some(workspace.display().to_string()),
        shell: letibot_tools::intent::ShellTrust::Pinned {
            how: "the test's own shell".into(),
        },
        ..Default::default()
    }
}

/// The gate, on its own thread: `admit` waits for a person, and this test's main thread is that
/// person, pressing keys at the head.
struct Driver {
    jobs: Sender<(String, String)>,
    verdicts: Mutex<Receiver<Verdict>>,
}

impl Driver {
    fn start(
        hub: Arc<Hub>,
        answers: Arc<Answers>,
        workspace: &Path,
        permission_file: &Path,
    ) -> Driver {
        let (jobs, job_rx) = std::sync::mpsc::channel::<(String, String)>();
        let (verdict_tx, verdicts) = std::sync::mpsc::channel::<Verdict>();
        let ws = workspace.display().to_string();
        let around = surroundings(workspace);
        let file = permission_file.to_path_buf();
        std::thread::spawn(move || {
            let mut gate = AdjudicatedGate::new(Box::new(HeadAdjudicator::new(hub, answers)))
                .with_mode(Mode::WRITES_ALLOWED)
                .with_permission(letibot_tools::permission::seed())
                .with_permission_sink(Arc::new(move |r: &letibot_tools::permission::Rule| {
                    letibot_tools::permission::append_to_file(&file, r)
                }))
                .with_surroundings(around)
                .with_trail_source(|_| {
                    letibot_tools::authorise::AuthorisationTrail::from_messages(vec![], 1)
                });
            while let Ok((call_id, command)) = job_rx.recv() {
                let args = serde_json::json!({ "command": command });
                let call = GateCall {
                    name: "bash",
                    access: Access::Exec,
                    args: &args,
                    turn_id: "t1",
                    call_id: &call_id,
                    workspace: &ws,
                    target_exists: None,
                    scripts: &[],
                };
                let decision = gate.admit(&call);
                let row = gate.log.last().expect("a row per ask");
                let option = match &row.decision.outcome {
                    letibot_tools::adjudicate::DecisionOutcome::Selected { option_id } => {
                        Some(option_id.clone())
                    }
                    _ => None,
                };
                let _ = verdict_tx.send(Verdict {
                    decision,
                    option,
                    basis: row.decision.basis.clone(),
                });
            }
        });
        Driver {
            jobs,
            verdicts: Mutex::new(verdicts),
        }
    }

    /// Ask the gate about a command and wait for the verdict - the caller has already pressed the
    /// keys, or is about to on another thread.
    fn ask(&self, call_id: &str, command: &str) -> Verdict {
        self.jobs
            .send((call_id.to_string(), command.to_string()))
            .expect("the gate's thread");
        self.verdicts
            .lock()
            .expect("verdicts")
            .recv_timeout(Duration::from_secs(30))
            .expect("a verdict")
    }
}

#[test]
fn a_digit_a_typed_reason_and_an_always_allow_answer_a_real_ask() {
    let dir = scratch("card");
    let workspace = dir.join("work");
    std::fs::create_dir_all(&workspace).expect("a workspace");
    std::fs::write(workspace.join("notes.txt"), "a\n").expect("a file to edit");
    let permission_file = dir.join("config").join("letibot").join("permission.json");

    // The daemon: one session on a socket, with the rendezvous the gate asks through.
    let hub = Hub::new("s-perm");
    let answers = Arc::new(Answers::new());
    hub.set_answer_sink(answers.clone());
    let socket = dir.join("d.sock");
    let registry = Registry::of(hub.clone());
    // `FOREIGN_TREE`: the head is this process's child, and a tree root no real process descends
    // from is what says so - serving in-process must not judge our own head a tool call.
    let server =
        serve_registry_rooted(registry, &socket, letibot_sessionlog::testing::FOREIGN_TREE)
            .expect("the daemon serves");

    let mut head = Head::start(&socket, &dir);
    // A card has somewhere to land only once a head that may decide is attached, and that is a fact
    // about the hub rather than about a screen.
    let deadline = Instant::now() + Duration::from_secs(30);
    while hub.deciding_heads().is_empty() {
        assert!(
            Instant::now() < deadline,
            "the head never attached. It drew:\n{}",
            head.drawn()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let gate = Driver::start(hub.clone(), answers.clone(), &workspace, &permission_file);

    // ── 1. The digit is the answer ────────────────────────────────────────────────────────────
    let first = std::thread::scope(|s| {
        let ask = s.spawn(|| gate.ask("c1", "cp notes.txt copy.txt"));
        let drawn = head.wait_for("the first card", "Allow this one");
        // What the card says about its keys, in this head's words: digits and arrows mark, and only
        // they select.
        assert!(
            drawn.contains("↑↓ or a row number marks · enter answers"),
            "the ladder's own sentence is not on the card:\n{drawn}"
        );
        assert!(
            !drawn.contains("type the id"),
            "the card still tells people to type an id:\n{drawn}"
        );
        let one = rung(&drawn, |l| l == "Allow this one");
        head.press(&one.to_string());
        head.press("\r");
        ask.join().expect("the ask")
    });
    assert!(
        matches!(first.decision, GateDecision::Admit),
        "a digit did not answer the ask: {:?}",
        first.decision
    );
    assert_eq!(
        first.option.as_deref(),
        Some("allow_once"),
        "the marked rung"
    );

    // ── 2. A typed refusal is the reason the model is handed ─────────────────────────────────
    let second = std::thread::scope(|s| {
        // A different program from the first act's: the gate remembers the SHAPE a person approved,
        // so a second `cp` would be admitted without a card and prove nothing.
        let ask = s.spawn(|| gate.ask("c2", "sed -i s/b/c/ notes.txt"));
        let drawn = head.wait_for("the second card", "Allow this one");
        // Mark the plain refusal, then type at it: the words are its reason, and the card says
        // where they go before Enter spends them.
        let deny = rung(&drawn, |l| l == "Deny");
        head.press(&deny.to_string());
        head.press("use the scratch dir");
        let drawn = head.wait_for("the preview", "tells the model: use the scratch dir");
        assert!(
            !drawn.contains("type the id"),
            "a typed line is the rung's material, not a selector:\n{drawn}"
        );
        assert!(
            drawn.contains("type the reason"),
            "the hint names what the marked rung takes:\n{drawn}"
        );
        head.press("\r");
        ask.join().expect("the ask")
    });
    match &second.decision {
        GateDecision::Refuse { tell, .. } => assert!(
            tell.contains("use the scratch dir"),
            "the reason did not reach the model:\n{tell}"
        ),
        other => panic!("a refusal was expected, got {other:?}"),
    }
    assert_eq!(second.option.as_deref(), Some("deny"), "the plain refusal");
    assert!(
        second.basis.contains("use the scratch dir"),
        "the answer's own basis: {}",
        second.basis
    );

    // ── 3. An always-allow shows the rule, then writes it ────────────────────────────────────
    let third = std::thread::scope(|s| {
        let ask = s.spawn(|| gate.ask("c3", "mv notes.txt notes2.txt"));
        let drawn = head.wait_for("the third card", "Allow this one");
        // The always-allow is the rung whose label begins `Always allow`; the glob typed on it is
        // the rule, and the card says which file it lands in before Enter writes it.
        let always = rung(&drawn, |l| l.starts_with("Always allow"));
        head.press(&always.to_string());
        head.press("mv*");
        let drawn = head.wait_for(
            "the rule's preview",
            "writes the rule `mv*` to permission.json",
        );
        assert!(
            drawn.contains("type the rule"),
            "the hint names what the marked rung takes:\n{drawn}"
        );
        head.press("\r");
        ask.join().expect("the ask")
    });
    assert!(
        matches!(third.decision, GateDecision::Admit),
        "the always-allow did not admit: {:?}",
        third.decision
    );
    assert_eq!(third.option.as_deref(), Some("allow_always"));
    let written = std::fs::read_to_string(&permission_file).unwrap_or_else(|e| {
        panic!("{} was not written ({e})", permission_file.display());
    });
    assert!(
        written.contains("\"mv*\""),
        "the typed glob is not the rule that was saved:\n{written}"
    );
    assert!(written.contains("\"bash\""), "{written}");
    // And the file the gate would read back holds a rule that covers the next call.
    let rules = letibot_tools::permission::load_file(&permission_file).expect("the saved ruleset");
    assert!(
        rules
            .iter()
            .any(|r| r.pattern == "mv*" && r.action == letibot_tools::permission::Action::Allow),
        "the rule the file holds is not `mv*`: {rules:?}"
    );

    // ── 4. And the rule the file holds is what admits the next call ─────────────────────────
    // A second gate over the saved file alone, with an adjudicator that refuses anything that
    // asks: `mv -f …` is a call the rule was not typed for and the same rule covers, so admitting
    // it here is the rule's doing and nobody's answer.
    let loaded = letibot_tools::permission::load_file(&permission_file).expect("the saved ruleset");
    let mut fresh = AdjudicatedGate::new(Box::new(letibot_tools::adjudicate::AskAdjudicator::new(
        "human",
        |req: &letibot_tools::adjudicate::AdjudicationRequest| {
            Some(letibot_tools::adjudicate::AdjudicationDecision::selected(
                req,
                "deny",
                "human:test",
                "nothing should have asked",
            ))
        },
    )))
    .with_mode(Mode::WRITES_ALLOWED)
    .with_permission(loaded)
    .with_surroundings(surroundings(&workspace))
    .with_trail_source(|_| letibot_tools::authorise::AuthorisationTrail::from_messages(vec![], 1));
    let args = serde_json::json!({ "command": "mv -f notes2.txt notes3.txt" });
    let ws4 = workspace.display().to_string();
    let again = GateCall {
        name: "bash",
        access: Access::Exec,
        args: &args,
        turn_id: "t1",
        call_id: "c4",
        workspace: &ws4,
        target_exists: None,
        scripts: &[],
    };
    assert!(
        matches!(fresh.admit(&again), GateDecision::Admit),
        "the saved rule did not admit the next call"
    );

    drop(server);
    let _ = std::fs::remove_dir_all(&dir);
}
