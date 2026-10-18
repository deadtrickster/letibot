//! **A message to a model that cannot be reached opens the chooser** — `app::reach`.
//!
//! The operator, 2026-10-18: *"idk why i even have to do --provider. horrible ux - we already
//! have models chooser and keys asker"*. The launcher stopped asking; these are the head's half:
//! the chooser opens with the reason when the session's local model cannot be reached, does not
//! when it can, asks again on every send (nothing is latched), and a cloud row with no key goes
//! through the key card and takes the held message with it.

use super::*;
use letibot_sessionlog::protocol::{MODEL_KEYLESS_KEY, MODEL_KEYS_KEY, SettingRow};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

fn row(k: &str, v: &str, choices: &[&str]) -> SettingRow {
    SettingRow {
        key: k.into(),
        value: v.into(),
        source: String::new(),
        editable: String::new(),
        choices: choices.iter().map(|s| (*s).to_string()).collect(),
        tools: Vec::new(),
    }
}

const CHOICES: &[&str] = &["local", "deepseek/deepseek-flash"];
const EP: &str = "127.0.0.1:59999";

/// A head attached to a session on the LOCAL model at [`EP`], with the rows a daemon of this
/// build publishes; `keyed` is the `models.keys` row, `vocab` the `vocab` row.
fn on_local(keyed: &str, vocab: &str) -> App {
    let mut a = App::new(RenderConfig {
        width: 110,
        color: false,
        ..RenderConfig::default()
    });
    a.session_id = "s1".into();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Settings {
        rows: vec![
            row("model", "local (qwen-3.8-flash-next)", CHOICES),
            row(MODEL_KEYS_KEY, keyed, &[]),
            row(MODEL_KEYLESS_KEY, "local", &[]),
            row("endpoint", EP, &[]),
            row("vocab", vocab, &[]),
        ],
    });
    // The attach's own asks (`Settings`, `ListJobs`) are not what these tests are about.
    a.take_actions();
    a
}

/// A probe that answers `up` and counts its calls — no network.
fn stub(a: &mut App, up: Arc<AtomicBool>, calls: Arc<AtomicUsize>) {
    a.reach.probe = Arc::new(move |ep: &str| {
        calls.fetch_add(1, Ordering::SeqCst);
        if up.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(format!("nothing is serving the local model on {ep}"))
        }
    });
}

/// Run the loop's half until the probe has answered — `Driver::tick` calls `poll_reach`.
fn settle(a: &mut App) {
    for _ in 0..500 {
        a.poll_reach();
        if a.reach.job.is_none() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("the probe never answered");
}

fn send(a: &mut App, text: &str) -> Option<Action> {
    typed(a, text);
    a.key(Key::Enter)
}

fn prompts(actions: &[Action]) -> Vec<String> {
    actions
        .iter()
        .filter_map(|x| match x {
            Action::Prompt(t) => Some(t.clone()),
            _ => None,
        })
        .collect()
}

/// **Nothing serving the local model: the chooser, with the reason, and the message held** — and
/// esc gives the words back to the composer rather than sending them to nothing.
#[test]
fn a_message_to_a_local_model_nobody_serves_opens_the_chooser_with_the_reason() {
    let mut a = on_local("", "/models/qwen.gguf");
    let calls = Arc::new(AtomicUsize::new(0));
    stub(&mut a, Arc::new(AtomicBool::new(false)), calls.clone());

    assert_eq!(
        send(&mut a, "hello"),
        None,
        "nothing leaves before the check"
    );
    settle(&mut a);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the endpoint row was probed"
    );
    assert!(a.pick == Some(Pick::Model), "the chooser opened");
    let screen = a.screen(110, 30).join("\n");
    assert!(
        screen.contains("nothing is serving the local model on 127.0.0.1:59999 — pick a model"),
        "the card carries the reason:\n{screen}"
    );
    let acts = a.take_actions();
    assert!(prompts(&acts).is_empty(), "the message was held: {acts:?}");

    // Esc on the card: the message comes back, nothing was sent.
    a.key(Key::Esc);
    a.poll_reach();
    assert_eq!(
        a.input(),
        "hello",
        "the words are back where they were typed"
    );
    assert!(prompts(&a.take_actions()).is_empty());
    assert!(a.reach.reason.is_none() && a.reach.held.is_none());
}

/// **A local model that answers is not interrupted** — the message goes, no card — **and the check
/// is per attempt**: the same session, the server gone, the next message opens the chooser; the
/// server back, the one after that is sent. Nothing is latched either way.
#[test]
fn a_message_to_a_local_model_that_answers_is_sent_and_the_check_is_per_attempt() {
    let mut a = on_local("", "/models/qwen.gguf");
    let up = Arc::new(AtomicBool::new(true));
    let calls = Arc::new(AtomicUsize::new(0));
    stub(&mut a, up.clone(), calls.clone());

    assert_eq!(send(&mut a, "first"), None);
    settle(&mut a);
    assert!(a.pick.is_none(), "no chooser for a model that answers");
    assert_eq!(prompts(&a.take_actions()), vec!["first".to_string()]);

    up.store(false, Ordering::SeqCst);
    send(&mut a, "second");
    settle(&mut a);
    assert!(a.pick == Some(Pick::Model), "the server went: asked again");
    a.key(Key::Esc);
    a.poll_reach();
    assert_eq!(a.input(), "second");

    up.store(true, Ordering::SeqCst);
    assert_eq!(a.key(Key::Enter), None);
    settle(&mut a);
    assert!(a.pick.is_none());
    assert_eq!(prompts(&a.take_actions()), vec!["second".to_string()]);
    assert_eq!(calls.load(Ordering::SeqCst), 3, "one probe per send");
}

/// **No local model on the box at all** (the daemon's `vocab` row is `none` and the session is on
/// the local route — started with no GGUF and no provider): the chooser at once, no probe.
#[test]
fn no_local_model_on_the_box_opens_the_chooser_without_a_probe() {
    let mut a = on_local(
        "",
        "none — the byte vocabulary: a cloud provider's, or no model chosen yet",
    );
    let calls = Arc::new(AtomicUsize::new(0));
    stub(&mut a, Arc::new(AtomicBool::new(true)), calls.clone());

    assert_eq!(send(&mut a, "hello"), None);
    assert!(
        a.pick == Some(Pick::Model),
        "the chooser is up on the send itself"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "the rows said it; nothing probed"
    );
    let screen = a.screen(110, 30).join("\n");
    assert!(
        screen.contains("no local model is set up on this box — pick a model"),
        "{screen}"
    );
}

/// **A cloud row with no key goes through the key card, and the held message follows it** — the
/// switch with the key, then the re-read, the default (`/default-model`, so it is asked once per
/// box), and the message, in that order.
#[test]
fn choosing_a_provider_with_no_key_leads_to_the_key_card_and_the_message_follows() {
    let mut a = on_local("", "/models/qwen.gguf");
    stub(
        &mut a,
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicUsize::new(0)),
    );
    send(&mut a, "hello");
    settle(&mut a);
    a.take_actions();

    a.key(Key::Down);
    assert_eq!(a.key(Key::Enter), None);
    assert!(
        a.key_ask.is_some(),
        "no key held for deepseek: the key card"
    );
    a.poll_reach();
    assert!(
        a.reach.held.is_some(),
        "the message waits behind the key card"
    );

    for c in "sk-test".chars() {
        a.key(Key::Char(c));
    }
    match a.key(Key::Enter) {
        Some(Action::Slash { line }) => {
            assert_eq!(line, "models deepseek/deepseek-flash --key sk-test")
        }
        other => panic!("the key card stores and switches: {other:?}"),
    }
    let after = a.take_actions();
    let lines: Vec<String> = after
        .iter()
        .map(|x| match x {
            Action::Settings => "settings".to_string(),
            Action::Slash { line } => line.clone(),
            Action::Prompt(t) => format!("prompt {t}"),
            other => format!("{other:?}"),
        })
        .collect();
    assert_eq!(
        lines,
        vec![
            "settings".to_string(),
            "default-model deepseek/deepseek-flash".to_string(),
            "prompt hello".to_string(),
        ],
        "re-read, made the default, then the message"
    );
    a.poll_reach();
    assert_eq!(a.input(), "", "the message was sent, not given back");
}

/// **A cloud row the box holds a key for switches at once**, and the default and message follow;
/// a session already on a provider is never checked here — its missing key is the daemon's card.
#[test]
fn a_keyed_cloud_row_switches_and_a_provider_session_is_not_checked() {
    let mut a = on_local("deepseek", "/models/qwen.gguf");
    stub(
        &mut a,
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicUsize::new(0)),
    );
    send(&mut a, "hello");
    settle(&mut a);
    a.take_actions();
    a.key(Key::Down);
    match a.key(Key::Enter) {
        Some(Action::Slash { line }) => assert_eq!(line, "models deepseek/deepseek-flash"),
        other => panic!("a keyed row switches: {other:?}"),
    }
    assert!(a.key_ask.is_none());
    let after = a.take_actions();
    assert!(
        after.iter().any(|x| matches!(x, Action::Slash { line } if line == "default-model deepseek/deepseek-flash")),
        "{after:?}"
    );
    assert_eq!(prompts(&after), vec!["hello".to_string()]);

    // On a provider: no check, the message goes as typed.
    let mut b = on_local(
        "",
        "none — the byte vocabulary: a cloud provider's, or no model chosen yet",
    );
    let calls = Arc::new(AtomicUsize::new(0));
    stub(&mut b, Arc::new(AtomicBool::new(false)), calls.clone());
    b.apply(ServerFrame::Settings {
        rows: vec![
            row("model", "deepseek/deepseek-flash", CHOICES),
            row(MODEL_KEYS_KEY, "", &[]),
            row(MODEL_KEYLESS_KEY, "local", &[]),
            row("endpoint", EP, &[]),
            row(
                "vocab",
                "none — the byte vocabulary: a cloud provider's, or no model chosen yet",
                &[],
            ),
        ],
    });
    assert_eq!(send(&mut b, "hi"), Some(Action::Prompt("hi".into())));
    assert!(b.pick.is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

/// **The probe itself**, against real sockets: a port nobody listens on is *nothing serving*, a
/// 503 (llama-server loading) names the status, a 200 is up.
#[test]
fn the_health_probe_reads_refused_loading_and_up() {
    use std::io::{Read, Write};
    let gone = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = gone.local_addr().unwrap().port();
    drop(gone);
    let why = crate::app::reach::health(&format!("127.0.0.1:{port}")).unwrap_err();
    assert!(
        why.starts_with("nothing is serving the local model on"),
        "{why}"
    );

    for (status, want_ok) in [("503 Service Unavailable", false), ("200 OK", true)] {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let t = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf);
            let body = "{}";
            let _ = write!(
                s,
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        });
        let got = crate::app::reach::health(&addr.to_string());
        t.join().unwrap();
        assert_eq!(got.is_ok(), want_ok, "{status}: {got:?}");
        if !want_ok {
            assert!(got.unwrap_err().contains("answered /health with 503"));
        }
    }
}
