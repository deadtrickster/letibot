//! `letibot-secret`, through the real binary: the value goes to the caller and nowhere else.
//!
//! Every test here has the same shape - a daemon served in this process, a head that answers the
//! card, and the helper spawned with the session's environment in `LETIBOT_SOCKET` - so what is
//! being proved is the door, not the programs that knock on it. No `sudo`, no `ssh` and no other
//! machine appears anywhere in this file.

use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use letibot_sessionlog::Registry;
use letibot_sessionlog::client::{HeadClient, Inbound, pump};
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::protocol::{Caps, ServerFrame};
use letibot_sessionlog::server::{ServerHandle, serve_registry};

/// How long a helper may take before the test calls it a hang.
const PATIENCE: Duration = Duration::from_secs(60);

/// The multicall binary, which is every role: `sudo` reaches the askpass one through a symlink
/// named `letibot-askpass`, and `install.sh` makes the same link.
const MULTI: &str = env!("CARGO_BIN_EXE_letibot-multi");

fn socket_path(tag: &str) -> std::path::PathBuf {
    letibot_sessionlog::testing::scratch(&format!("letibot-secret-{tag}")).with_extension("sock")
}

/// A daemon with one session, one head, and the session's environment a command would carry.
struct Rig {
    dir: std::path::PathBuf,
    socket: std::path::PathBuf,
    head: HeadClient,
    events: std::sync::mpsc::Receiver<Inbound>,
    registry: std::sync::Arc<Registry>,
    server: Option<ServerHandle>,
    /// Every card this head was shown, in order: `for_what`, and how many asks it settled.
    asked: Vec<(String, u32)>,
}

impl Rig {
    fn new(tag: &str) -> Rig {
        let socket = socket_path(tag);
        let dir = socket.with_extension("run");
        std::fs::create_dir_all(&dir).expect("the runtime dir");
        let registry = Registry::of(Hub::new("s"));
        let server = serve_registry(registry.clone(), &socket).expect("bind");
        let caps = Caps {
            can_decide: true,
            ..Caps::default()
        };
        let (head, _hello, reader) =
            HeadClient::attach(&socket, "s", 0, "tui", "dead", caps).expect("head attach");
        let (tx, events) = channel();
        std::thread::spawn(move || pump(reader, tx));
        // The name `sudo`, `ssh` and `git` reach the helper by: a link to the multicall binary,
        // which is what `install.sh` puts on an operator's PATH.
        let link = dir.join("letibot-askpass");
        std::os::unix::fs::symlink(MULTI, &link).expect("the askpass link");
        Rig {
            dir,
            socket,
            head,
            events,
            registry,
            server: Some(server),
            asked: Vec::new(),
        }
    }

    /// The helper, as a command runs it.
    fn helper(&self, args: &[&str]) -> std::process::Child {
        let mut cmd = letibot_testenv::command(MULTI);
        cmd.arg("secret").args(args);
        self.session_env(&mut cmd);
        cmd.spawn().expect("the helper starts")
    }

    fn session_env(&self, cmd: &mut std::process::Command) {
        cmd.env("LETIBOT_SOCKET", &self.socket)
            .env("LETIBOT_SESSION", "s")
            .env("LETIBOT_COMMAND", "sudo apt-get -y upgrade")
            .env("XDG_RUNTIME_DIR", &self.dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
    }

    /// Wait for a card, answer it, and say what the card showed.
    fn answer_next(&mut self, answer: Option<&str>) -> (String, u32, Option<u64>) {
        let deadline = Instant::now() + PATIENCE;
        loop {
            if let Ok(inbound) = self.events.recv_timeout(Duration::from_millis(10))
                && let ServerFrame::Event(envelope) = inbound.frame()
                && let SessionEvent::SecretRequested {
                    req_id,
                    for_what,
                    waiting,
                    keep_ms,
                    ..
                } = envelope.event
            {
                self.asked.push((for_what.clone(), waiting));
                self.head
                    .secret_keeping(&req_id, answer.map(str::to_string), keep_ms)
                    .expect("the card is answered");
                return (for_what, waiting, keep_ms);
            }
            assert!(Instant::now() < deadline, "no card was raised");
        }
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        if let Some(server) = self.server.take() {
            server.shutdown();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Collect a spawned helper: its status, stdout and stderr.
fn finish(child: std::process::Child) -> (std::process::ExitStatus, String, String) {
    let out = child.wait_with_output().expect("the helper ends");
    (
        out.status,
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The whole of the helper: it blocks, raises one card naming what will use the value, and prints
/// the value - and nothing else - on stdout.
#[test]
fn the_helper_prints_the_value_and_the_card_says_what_uses_it() {
    let mut rig = Rig::new("round-trip");
    let child = rig.helper(&["ask", "--for", "sudo on lubuntu1-3", "--host", "lubuntu1-3"]);
    let (for_what, waiting, keep) = rig.answer_next(Some("hunter2hunter2"));
    assert_eq!(for_what, "sudo on lubuntu1-3", "the card's first line");
    assert_eq!(waiting, 1, "one ask, one answer");
    assert_eq!(keep, None, "no --key means no window to offer");
    let (status, stdout, stderr) = finish(child);
    assert!(status.success(), "the helper failed: {stderr}");
    assert_eq!(stdout, "hunter2hunter2\n", "the value and nothing else");
    // And the value is nowhere in the daemon's own record.
    let snapshot = format!("{:?}", rig.registry.get("s").expect("the hub").snapshot());
    assert!(
        !snapshot.contains("hunter2hunter2"),
        "the value reached the snapshot"
    );
}

/// A refusal is exit 1 with an EMPTY stdout: a caller that reads a half-sentence as a password is
/// the bug this is worth a test for.
#[test]
fn a_refused_card_is_exit_one_with_nothing_on_stdout() {
    let mut rig = Rig::new("refused");
    let child = rig.helper(&["ask", "--for", "a token"]);
    let (_, _, _) = rig.answer_next(None);
    let (status, stdout, stderr) = finish(child);
    assert_eq!(status.code(), Some(1), "a refusal is exit 1");
    assert_eq!(stdout, "", "a refusal prints no value at all");
    assert!(stderr.contains("no secret was given"), "{stderr}");
}

/// `--key` + `--keep`: the person chooses the window, the second ask is answered without a card,
/// and a different key is a different question.
#[test]
fn a_kept_value_answers_the_next_ask_without_a_card() {
    let mut rig = Rig::new("keep");
    let child = rig.helper(&[
        "ask",
        "--for",
        "sudo on lubuntu1-3",
        "--key",
        "fleet-sudo",
        "--keep",
        "15m",
    ]);
    let (_, _, keep) = rig.answer_next(Some("hunter2hunter2"));
    assert_eq!(
        keep,
        Some(15 * 60 * 1000),
        "the card offers the window the asker allowed"
    );
    let (status, stdout, _) = finish(child);
    assert!(status.success());
    assert_eq!(stdout, "hunter2hunter2\n");
    assert_eq!(
        rig.registry.vault().windows("s").len(),
        1,
        "one value is held, in the daemon's memory only"
    );

    // The second ask costs no card: the value is answered from the vault.
    let child = rig.helper(&[
        "ask",
        "--for",
        "sudo on lubuntu1-3",
        "--key",
        "fleet-sudo",
        "--keep",
        "15m",
    ]);
    let (status, stdout, stderr) = finish(child);
    assert!(status.success(), "{stderr}");
    assert_eq!(stdout, "hunter2hunter2\n");
    assert_eq!(rig.asked.len(), 1, "the second ask raised no card");

    // A different key is a different question, and it is asked.
    let child = rig.helper(&[
        "ask",
        "--for",
        "another host",
        "--key",
        "other",
        "--keep",
        "5m",
    ]);
    rig.answer_next(Some("secondsecret"));
    let (status, stdout, _) = finish(child);
    assert!(status.success());
    assert_eq!(stdout, "secondsecret\n");
    assert_eq!(rig.asked.len(), 2);
}

/// A merged card is one card even when the person has not answered: the second helper joins it
/// and the head is told, and both are answered by one keystroke. This is what makes a fleet
/// upgrade ONE question rather than three.
#[test]
fn a_second_ask_joins_the_card_that_is_up() {
    let mut rig = Rig::new("join");
    let one = rig.helper(&[
        "ask",
        "--for",
        "sudo on lubuntu1",
        "--key",
        "fleet-sudo",
        "--keep",
        "15m",
    ]);
    // The card as the daemon raised it, read and NOT answered, so the second helper arrives
    // while the person is still typing.
    let deadline = Instant::now() + PATIENCE;
    let mut first = None;
    while first.is_none() {
        if let Ok(inbound) = rig.events.recv_timeout(Duration::from_millis(10))
            && let ServerFrame::Event(envelope) = inbound.frame()
            && let SessionEvent::SecretRequested {
                req_id,
                waiting,
                keep_ms,
                ..
            } = envelope.event
        {
            first = Some((req_id, waiting, keep_ms));
        }
        assert!(Instant::now() < deadline, "the first card was never raised");
    }
    let two = rig.helper(&[
        "ask",
        "--for",
        "sudo on lubuntu2",
        "--key",
        "fleet-sudo",
        "--keep",
        "15m",
    ]);
    // The joined ask re-says the SAME card with one more wait on it.
    let mut merged = None;
    let deadline = Instant::now() + PATIENCE;
    while merged.is_none() {
        if let Ok(inbound) = rig.events.recv_timeout(Duration::from_millis(10))
            && let ServerFrame::Event(envelope) = inbound.frame()
            && let SessionEvent::SecretRequested {
                req_id,
                waiting,
                keep_ms,
                ..
            } = envelope.event
            && waiting > 1
        {
            merged = Some((req_id, waiting, keep_ms));
        }
        assert!(
            Instant::now() < deadline,
            "the second ask raised its own card"
        );
    }
    let (req_id, waiting, keep) = merged.expect("the merged card");
    assert_eq!(waiting, 2, "one card, two waits");
    assert_eq!(
        req_id,
        first.expect("the first card").0,
        "the card keeps its id, so a head replaces the fact and not the typing"
    );
    rig.head
        .secret_keeping(&req_id, Some("hunter2hunter2".into()), keep)
        .expect("one answer");
    for child in [one, two] {
        let (status, stdout, stderr) = finish(child);
        assert!(status.success(), "{stderr}");
        assert_eq!(stdout, "hunter2hunter2\n", "both got the one value");
    }
}

/// `SSH_ASKPASS` is the same door with another program's name on it: a fake `ssh` that runs
/// `$SSH_ASKPASS <prompt>` gets the value exactly as `sudo` would, through the same link.
#[test]
fn the_ssh_hook_is_the_same_helper() {
    let mut rig = Rig::new("ssh");
    let link = rig.dir.join("letibot-askpass");
    let fake_ssh = rig.dir.join("fake-ssh");
    letibot_tools::testing::write_stub(
        &fake_ssh,
        "#!/bin/sh\n\
         value=$(\"$SSH_ASKPASS\" \"dead@lubuntu1's password: \")\n\
         printf '%s\\n' \"$value\"\n",
    );
    let mut cmd = letibot_testenv::command(&fake_ssh);
    rig.session_env(&mut cmd);
    cmd.env("SSH_ASKPASS", &link)
        .env("SSH_ASKPASS_REQUIRE", "force");
    let child = cmd.spawn().expect("the fake ssh starts");
    let (for_what, _, _) = rig.answer_next(Some("hunter2hunter2"));
    assert!(
        for_what.contains("lubuntu1"),
        "the card names the prompt ssh used: {for_what}"
    );
    let (status, stdout, stderr) = finish(child);
    assert!(status.success(), "{stderr}");
    assert_eq!(stdout, "hunter2hunter2\n", "the password ssh would send");
}
