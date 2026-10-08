//! `sudo` wants a password: the helper asks on its own connection, every head is
//! shown the request, one head answers, the helper gets the secret and nothing
//! else does — not the log, not the view, not the other head.

use std::sync::Arc;
use std::time::Duration;

use letibot_sessionlog::client::HeadClient;
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::protocol::{Caps, ServerFrame};
use letibot_sessionlog::server::{ServerHandle, serve};

fn socket_path(tag: &str) -> std::path::PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "letibot-askpass-{tag}-{}-{n}.sock",
        std::process::id()
    ))
}

fn start(tag: &str) -> (Arc<Hub>, ServerHandle) {
    let hub = Hub::new("s");
    let h = serve(hub.clone(), socket_path(tag)).expect("bind");
    (hub, h)
}

/// Read frames until one satisfies `pick`, or give up.
fn wait_for<T>(
    reader: &mut letibot_sessionlog::wire::FrameReader<std::os::unix::net::UnixStream>,
    mut pick: impl FnMut(&ServerFrame) -> Option<T>,
) -> T {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "nothing arrived in time"
        );
        let f: ServerFrame = reader.read().expect("frame");
        if let Some(t) = pick(&f) {
            return t;
        }
    }
}

#[test]
fn the_secret_reaches_the_helper_and_nothing_else() {
    let (hub, server) = start("ok");
    let path = server.path().to_path_buf();

    // A head, attached first, that can decide.
    let caps = Caps {
        can_decide: true,
        ..Caps::default()
    };
    let (mut head, _hello, mut head_reader) =
        HeadClient::attach(&path, "s", 0, "tui", "dead", caps.clone()).expect("head");

    // The helper, on its own thread: it asks and blocks for the answer.
    let helper_path = path.clone();
    let helper = std::thread::spawn(move || {
        let caps = Caps {
            can_decide: false,
            ..Caps::default()
        };
        let (mut c, _h, mut r) =
            HeadClient::attach(&helper_path, "s", u64::MAX, "askpass", "sudo", caps)
                .expect("helper");
        c.askpass("[sudo] password for dead: ", "sudo apt install x")
            .expect("ask");
        wait_for(&mut r, |f| match f {
            ServerFrame::Secret { secret, .. } => Some(secret.clone()),
            _ => None,
        })
    });

    // The head sees the request — with the command and the prompt, without any
    // secret — and answers.
    let req_id = wait_for(&mut head_reader, |f| match f {
        ServerFrame::Event(env) => match &env.event {
            SessionEvent::SecretRequested {
                req_id,
                prompt,
                command,
                deadline,
            } => {
                assert!(prompt.contains("password for dead"));
                assert_eq!(command, "sudo apt install x");
                assert!(*deadline > 0);
                Some(req_id.clone())
            }
            _ => None,
        },
        _ => None,
    });
    head.secret(&req_id, Some("hunter2".into()))
        .expect("answer");

    let got = helper.join().expect("helper thread");
    assert_eq!(got.as_deref(), Some("hunter2"));

    // The settlement is on the log; the secret is not, anywhere.
    let settled = wait_for(&mut head_reader, |f| match f {
        ServerFrame::Event(env) => match &env.event {
            SessionEvent::SecretSettled {
                req_id: r,
                given,
                by,
            } if r == &req_id => Some((*given, by.clone())),
            _ => None,
        },
        _ => None,
    });
    assert_eq!(settled, (true, "dead".to_string()));
    let everything = format!("{:?}", hub.snapshot());
    assert!(
        !everything.contains("hunter2"),
        "the secret leaked into the hub's state"
    );
    for env in hub.retained() {
        assert!(
            !format!("{env:?}").contains("hunter2"),
            "the secret leaked into the log"
        );
    }
    server.shutdown();
}

#[test]
fn a_refusal_and_a_late_answer_are_both_honest() {
    let (_hub, server) = start("refuse");
    let path = server.path().to_path_buf();
    let caps = Caps {
        can_decide: true,
        ..Caps::default()
    };
    let (mut head, _hello, mut head_reader) =
        HeadClient::attach(&path, "s", 0, "tui", "dead", caps).expect("head");
    let helper_path = path.clone();
    let helper = std::thread::spawn(move || {
        let (mut c, _h, mut r) = HeadClient::attach(
            &helper_path,
            "s",
            u64::MAX,
            "askpass",
            "sudo",
            Caps::default(),
        )
        .expect("helper");
        c.askpass("[sudo] password: ", "sudo rm -rf /")
            .expect("ask");
        wait_for(&mut r, |f| match f {
            ServerFrame::Secret { secret, .. } => Some(secret.clone()),
            _ => None,
        })
    });
    let req_id = wait_for(&mut head_reader, |f| match f {
        ServerFrame::Event(env) => match &env.event {
            SessionEvent::SecretRequested { req_id, .. } => Some(req_id.clone()),
            _ => None,
        },
        _ => None,
    });
    // Esc: no password.
    head.secret(&req_id, None).expect("refuse");
    assert_eq!(helper.join().expect("helper"), None);
    // A second answer to the same request has nothing to land on, and says so.
    head.secret(&req_id, Some("late".into())).expect("late");
    let warned = wait_for(&mut head_reader, |f| match f {
        ServerFrame::Event(env) => match &env.event {
            SessionEvent::Warning { code, .. } if code == "secret_late" => Some(true),
            _ => None,
        },
        _ => None,
    });
    assert!(warned);
    server.shutdown();
}
