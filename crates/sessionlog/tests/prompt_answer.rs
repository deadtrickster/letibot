//! **The operator's own run can be ANSWERED** - the two frames that write to a command's
//! stdin, and the verb that is the floor under the card.
//!
//! What is pinned HERE is the wire half: that a `PromptAnswer` and a `!send` reach the
//! session's [`PromptDriver`] with the right fields, that neither goes through the command
//! queue, that a late answer is said rather than refused, and that a secret cannot be
//! routed through the prompt card. The driver is a recorder rather than a pipe, because
//! this file is about what the server hands over.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use letibot_sessionlog::client::{HeadClient, Inbound, pump};
use letibot_sessionlog::protocol::{Caps, ServerFrame};
use letibot_sessionlog::registry::{PromptDriver, Registry, SessionWiring};
use letibot_sessionlog::server::{ServerHandle, serve_registry};
use letibot_sessionlog::{SessionEvent, send_line};

fn socket_path(tag: &str) -> std::path::PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    letibot_sessionlog::testing::scratch_root().join(format!(
        "letibot-prompt-{tag}-{}-{n}.sock",
        std::process::id()
    ))
}

/// **What the daemon was asked to write, in order.** `req` is `None` for a `!send` and
/// `Some(req_id)` for a card's own answer, which is the whole of the stale-card rule.
#[derive(Default)]
struct Recorder {
    sent: Mutex<Vec<(Option<String>, String)>>,
    /// The sentence every `send` answers with, so a test can drive the late case without a
    /// real pipe.
    refuse: Mutex<Option<String>>,
    /// What a successful `send` says it settled, if anything.
    settles: Mutex<Option<String>>,
}

impl PromptDriver for Recorder {
    fn send(
        &self,
        session_id: &str,
        req: Option<&str>,
        line: &str,
    ) -> Result<Option<String>, String> {
        assert_eq!(
            session_id, "a",
            "the driver is looked up by the session it is for"
        );
        if let Some(why) = self.refuse.lock().unwrap().clone() {
            return Err(why);
        }
        self.sent
            .lock()
            .unwrap()
            .push((req.map(str::to_string), line.to_string()));
        Ok(self.settles.lock().unwrap().clone())
    }
}

fn start(tag: &str) -> (Arc<Registry>, ServerHandle, Arc<Recorder>) {
    let r = Registry::new();
    r.create("a", "", SessionWiring::default()).unwrap();
    let driver = Arc::new(Recorder::default());
    r.set_prompt("a", driver.clone());
    let h = serve_registry(r.clone(), socket_path(tag)).expect("bind");
    (r, h, driver)
}

/// **Every warning the session published, by code.**
fn warnings(hub: &Arc<letibot_sessionlog::hub::Hub>) -> Vec<(String, String)> {
    hub.retained()
        .into_iter()
        .filter_map(|env| match env.event {
            SessionEvent::Warning { code, detail, .. } => Some((code, detail)),
            _ => None,
        })
        .collect()
}

/// **A card's answer reaches the command, addressed to the card.** The `req_id` travels
/// because it is what stops a stale card answering a later command, so the driver is
/// handed it and not a job id.
#[test]
fn an_answer_to_a_card_reaches_the_runs_stdin_addressed_to_the_card() {
    let (r, handle, driver) = start("answer");
    let hub = r.get("a").expect("session");
    hub.attach("tui", "dead", Caps::default(), 0);
    let (mut client, _hello, reader) =
        HeadClient::attach(handle.path(), "a", 0, "tui", "dead", Caps::default()).expect("attach");
    let (tx, rx) = std::sync::mpsc::channel();
    let _t = std::thread::spawn(move || pump(reader, tx));

    // The card the daemon raised. `settles` is what a real driver returns when the request
    // it was given is the one still open.
    *driver.settles.lock().unwrap() = Some("prompt-a-1".into());
    client
        .prompt_answer("prompt-a-1", "Y")
        .expect("the answer is written");

    // The driver got the line, with the request id — and NOTHING was queued: this frame is
    // never a command, because the run's own thread is blocked inside the very command that asked.
    let sent = wait_for_sent(&driver, 1);
    assert_eq!(
        sent,
        vec![(Some("prompt-a-1".to_string()), "Y".to_string())],
        "the answer must arrive addressed to the card it answers"
    );
    assert!(
        hub.try_command().is_none(),
        "a prompt answer must not go through the command queue"
    );
    // And the settlement is on the log, with who answered and never the line.
    let settled = wait_for_event(&rx, |e| matches!(e, SessionEvent::PromptSettled { .. }));
    match settled {
        SessionEvent::PromptSettled { req_id, sent, by } => {
            assert_eq!(req_id, "prompt-a-1");
            assert!(sent, "a line was sent");
            assert_eq!(by, "dead", "the settlement names who answered");
        }
        other => panic!("expected a settlement, got {other:?}"),
    }
    let everything = format!("{:?}", hub.retained());
    assert!(
        !everything.contains("\"Y\""),
        "the line itself must not be on the log — the settlement is the record"
    );
    handle.shutdown();
}

/// **The manual way in needs no card and no request id.** `!send` is the floor under the
/// heuristic: the frame addresses *whatever this session's operator run is*, which the
/// daemon knows and the head does not - so the driver is handed `None`.
#[test]
fn the_manual_send_addresses_whatever_is_running() {
    let (r, handle, driver) = start("manual");
    let hub = r.get("a").expect("session");
    hub.attach("tui", "dead", Caps::default(), 0);
    let (mut client, _hello, reader) =
        HeadClient::attach(handle.path(), "a", 0, "tui", "dead", Caps::default()).expect("attach");
    let (tx, rx) = std::sync::mpsc::channel();
    let _t = std::thread::spawn(move || pump(reader, tx));

    // A bare `!send` is a bare Enter, which is a real answer — `Continue? [Y/n]` takes it as
    // its default — and `!send yes please` is one line and not three words.
    client.send_line("").expect("write");
    client.send_line("yes please").expect("write");

    let sent = wait_for_sent(&driver, 2);
    assert_eq!(
        sent,
        vec![(None, String::new()), (None, "yes please".to_string())],
        "a `!send` carries no request id and the text is not re-split"
    );
    assert!(
        hub.try_command().is_none(),
        "`!send` must not go through the command queue either"
    );
    let _ = rx;
    handle.shutdown();
}

/// **A card that is no longer open is SAID, not silently dropped.** What lands is a
/// `prompt_late` warning rather than a `Rejected`: a late answer is not a refused one.
#[test]
fn an_answer_with_nothing_waiting_is_a_warning_and_not_a_refusal() {
    let (r, handle, driver) = start("late");
    let hub = r.get("a").expect("session");
    hub.attach("tui", "dead", Caps::default(), 0);
    let (mut client, _hello, reader) =
        HeadClient::attach(handle.path(), "a", 0, "tui", "dead", Caps::default()).expect("attach");
    let (tx, rx) = std::sync::mpsc::channel();
    let _t = std::thread::spawn(move || pump(reader, tx));
    *driver.refuse.lock().unwrap() =
        Some("nothing was waiting on `prompt-a-9` — the command had ended".into());

    client.prompt_answer("prompt-a-9", "Y").expect("write");
    let w = wait_for_event(
        &rx,
        |e| matches!(e, SessionEvent::Warning { code, .. } if code == "prompt_late"),
    );
    match w {
        SessionEvent::Warning { detail, .. } => {
            assert!(detail.contains("prompt-a-9"), "{detail}");
            assert!(
                detail.contains("dead"),
                "the warning names who answered: {detail}"
            );
        }
        other => panic!("expected a prompt_late warning, got {other:?}"),
    }
    assert!(
        driver.sent.lock().unwrap().is_empty(),
        "nothing was written to any command's stdin"
    );
    // And the connection is still up: a late answer is not a protocol error.
    assert!(
        client.send_line("still here").is_ok(),
        "the socket must survive a late answer"
    );
    handle.shutdown();
}

/// **`!send` with nothing of yours running is a sentence, not silence.** The act is well
/// formed and the thing it names is not there, so it is a `Refused` and not a red line.
#[test]
fn a_send_with_nothing_running_says_so() {
    let (r, handle, driver) = start("nothing");
    let hub = r.get("a").expect("session");
    hub.attach("tui", "dead", Caps::default(), 0);
    let (mut client, _hello, reader) =
        HeadClient::attach(handle.path(), "a", 0, "tui", "dead", Caps::default()).expect("attach");
    let (tx, rx) = std::sync::mpsc::channel();
    let _t = std::thread::spawn(move || pump(reader, tx));
    *driver.refuse.lock().unwrap() = Some("no command of yours is running in this session".into());

    client.send_line("Y").expect("write");
    let w = wait_for_event(
        &rx,
        |e| matches!(e, SessionEvent::Warning { code, .. } if code == "nothing_to_send_to"),
    );
    match w {
        SessionEvent::Warning { detail, .. } => {
            assert!(detail.contains("no command of yours"), "{detail}");
        }
        other => panic!("expected a nothing_to_send_to warning, got {other:?}"),
    }
    let classes = warnings(&hub);
    assert!(
        classes.iter().any(|(c, _)| c == "nothing_to_send_to"),
        "the warning must be on the session's log, not only on this socket: {classes:?}"
    );
    handle.shutdown();
}

/// **A SECRET CANNOT BE ROUTED THROUGH THE PROMPT CARD.** A password has its own path
/// (`SUDO_ASKPASS`, an `askpass` head, the one [`ServerFrame::Secret`] written back on that
/// connection); the card's field is drawn in the open and its line is a program's stdin.
#[test]
fn a_secret_cannot_be_routed_through_the_prompt_card() {
    let (r, handle, driver) = start("nosecret");
    let hub = r.get("a").expect("session");
    hub.attach("tui", "dead", Caps::default(), 0);
    let (mut client, _hello, reader) =
        HeadClient::attach(handle.path(), "a", 0, "tui", "dead", Caps::default()).expect("attach");
    let (tx, rx) = std::sync::mpsc::channel();
    let _t = std::thread::spawn(move || pump(reader, tx));
    // The helper's own connection, attached and waiting — the only socket a password may
    // ever be written to.
    let (mut helper, _hhello, hreader) =
        HeadClient::attach(handle.path(), "a", 0, "askpass", "sudo", Caps::default())
            .expect("attach");
    let (htx, hrx) = std::sync::mpsc::channel();
    let _h = std::thread::spawn(move || pump(hreader, htx));

    // What a person might type into the card for a program that is asking for a password.
    *driver.settles.lock().unwrap() = Some("prompt-a-1".into());
    client
        .prompt_answer("prompt-a-1", "hunter2")
        .expect("write");

    // It reached the command's stdin — which is what the prompt card is for.
    let sent = wait_for_sent(&driver, 1);
    assert_eq!(
        sent,
        vec![(Some("prompt-a-1".to_string()), "hunter2".to_string())],
        "the card's line goes to the run, and the card is the channel it went down"
    );
    // **And it is not a secret on the wire.** No `Secret` frame was produced for the
    // helper, and none can be: the prompt answer is not an `Askpass` and the daemon has no
    // path from one to the other.
    let mut saw_a_secret = false;
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        let Ok(inbound) = hrx.recv_timeout(Duration::from_millis(100)) else {
            continue;
        };
        if matches!(inbound.frame(), ServerFrame::Secret { .. }) {
            saw_a_secret = true;
        }
    }
    assert!(
        !saw_a_secret,
        "a line typed into the prompt card must never become a `Secret` frame — a password \
         has its own path, and the prompt card is not it"
    );
    // The control: the helper's channel DOES carry a secret when a head answers an askpass
    // the way that path is answered. `askpass` only writes the frame - the server's reader
    // thread is the one that blocks - so this needs no thread of its own.
    helper
        .askpass("[sudo] password for dead: ", "sudo true")
        .expect("the helper asks");
    let req_id = {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(
                Instant::now() < deadline,
                "no SecretRequested reached the head"
            );
            let Ok(inbound) = rx.recv_timeout(Duration::from_millis(200)) else {
                continue;
            };
            if let ServerFrame::Event(env) = inbound.frame()
                && let SessionEvent::SecretRequested { req_id, .. } = &env.event
            {
                break req_id.clone();
            }
        }
    };
    // **The answer comes from the HEAD and not from the helper**: the helper's own
    // connection is the one blocked inside the `Askpass` arm, so the other end sends the
    // `Secret`.
    client
        .secret(&req_id, Some("hunter2".into()))
        .expect("the head answers the askpass");
    let mut carried: Option<Option<String>> = None;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && carried.is_none() {
        let Ok(inbound) = hrx.recv_timeout(Duration::from_millis(200)) else {
            continue;
        };
        if let ServerFrame::Secret { secret, .. } = inbound.frame() {
            carried = Some(secret);
        }
    }
    assert_eq!(
        carried,
        Some(Some("hunter2".to_string())),
        "the askpass connection is the one channel a password travels on, and it must \
         actually carry one — otherwise the assertion above measured a dead socket"
    );
    handle.shutdown();
}

// ---------------------------------------------------------------------------
// The verb
// ---------------------------------------------------------------------------

/// **`!send` is a whole word, and the parse is the one both halves share.** The head
/// recognises the verb at the composer and the daemon re-checks it at the socket, and the
/// two cannot disagree because there is one function.
#[test]
fn the_verb_is_a_whole_word_and_the_bare_verb_is_an_enter() {
    assert_eq!(send_line("!send Y"), Some("Y"));
    assert_eq!(send_line("!send   yes please"), Some("yes please"));
    assert_eq!(send_line("!send\tY"), Some("Y"));
    // **The verb with nothing after it is a bare Enter**, and that is a real answer rather
    // than an empty mistake: `Continue? [Y/n]` takes Enter as its default.
    assert_eq!(send_line("!send"), Some(""));
    assert_eq!(send_line("!send   "), Some(""));
    // Not the verb.
    assert_eq!(send_line("!sender x"), None);
    assert_eq!(send_line("!sends x"), None);
    assert_eq!(send_line("!send-mail"), None);
    assert_eq!(send_line("send Y"), None);
    assert_eq!(send_line(" !send Y"), None);
    assert_eq!(send_line(""), None);
    // And the text is one line, whole and unsplit — the program decides what to do with the
    // spaces in it, exactly as the shell decides what to do with a `!` line's.
    assert_eq!(send_line("!send one  two\tthree"), Some("one  two\tthree"));
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Wait for `n` lines to reach the recorder, and hand back what it got.
fn wait_for_sent(driver: &Arc<Recorder>, n: usize) -> Vec<(Option<String>, String)> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let got = driver.sent.lock().unwrap().clone();
        if got.len() >= n || Instant::now() >= deadline {
            return got;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Wait for the first event on this connection matching `want`.
fn wait_for_event(
    rx: &std::sync::mpsc::Receiver<Inbound>,
    want: impl Fn(&SessionEvent) -> bool,
) -> SessionEvent {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let Ok(inbound) = rx.recv_timeout(Duration::from_millis(200)) else {
            continue;
        };
        if let ServerFrame::Event(env) = inbound.frame()
            && want(&env.event)
        {
            return env.event;
        }
    }
    panic!("the event never arrived");
}
