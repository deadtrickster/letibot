//! **What the operator is looking at, fetched from the head that drew it.**
//!
//! The daemon holds the log and the view and never a rendered cell. Width,
//! scroll position, theme and which folds are open live in the head, so a
//! daemon-side re-render would be a plausible picture of a different screen —
//! and the whole reason this exists is that a reconstruction is not what the
//! person is looking at.
//!
//! So the request goes out as an event and comes back as a frame, and this
//! covers that round trip over a real socket, plus the two ways it does not
//! answer: nobody attached, and attached but not drawing.

use std::sync::Arc;
use std::time::{Duration, Instant};

use letibot_sessionlog::client::{HeadClient, Inbound, pump};
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::protocol::{Caps, ServerFrame};
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_sessionlog::server::{ServerHandle, serve_registry};

fn socket_path(tag: &str) -> std::path::PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "letibot-screen-{tag}-{}-{n}.sock",
        std::process::id()
    ))
}

fn start(tag: &str) -> (Arc<Registry>, ServerHandle) {
    let r = Registry::new();
    r.create(
        "a",
        "",
        SessionWiring {
            model: "qwen-3.8-flash-next".into(),
            dialect: "qwen3.8".into(),
            endpoint: "127.0.0.1:8080".into(),
            workspace: "/home/dead/Projects/letibot".into(),
        },
    )
    .unwrap();
    let h = serve_registry(r.clone(), socket_path(tag)).expect("bind");
    (r, h)
}

/// The rows a head would answer with: escape codes included, because the colour
/// IS the thing being asked about.
fn drawn() -> Vec<String> {
    vec![
        "\u{1b}[1;32m● letibot\u{1b}[0m  s-17894 · qwen-3.8-flash-next".into(),
        "  \u{1b}[31m! cache_reuse_shortfall\u{1b}[0m — 812 tokens".into(),
        "> ".into(),
    ]
}

#[test]
fn the_head_answers_with_the_exact_rows_it_drew() {
    let (reg, server) = start("round-trip");
    let (mut client, _hello, reader) =
        HeadClient::attach(server.path(), "a", 0, "tui", "dead", Caps::default()).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let pumping = std::thread::spawn(move || pump(reader, tx));
    let hub = reg.get("a").unwrap();

    // A head that draws: it watches for the request and answers with its rows.
    // This is the driver's job in the real head, done here by hand so the test
    // is about the wire and not about the renderer.
    let answering = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Ok(ServerFrame::Event(env)) =
                rx.recv_timeout(Duration::from_millis(200)).map(Inbound::frame)
                && let SessionEvent::ScreenRequested { req_id } = &env.event
            {
                client.screen(req_id, 96, 3, drawn()).unwrap();
                return;
            }
        }
        panic!("the head was never asked to draw");
    });

    let (_req, answer) = hub.request_screen();
    let (cols, rows_n, rows) = answer
        .recv_timeout(Duration::from_secs(10))
        .expect("no head answered");
    answering.join().unwrap();
    drop(pumping);

    // The head's size, not the daemon's idea of one.
    assert_eq!((cols, rows_n), (96, 3));
    // Byte for byte. A test that compared the visible text would pass on a
    // version that stripped the colour, which is the one thing being asked for.
    assert_eq!(rows, drawn());
    assert!(
        rows[0].contains("\u{1b}[1;32m"),
        "the SGR survived the wire"
    );
}

#[test]
fn nobody_attached_is_not_an_empty_screen() {
    let (reg, _server) = start("nobody");
    let hub = reg.get("a").unwrap();

    // The event is still published — the hub does not know who is listening, and
    // pretending to know is how a head that attaches a millisecond later gets
    // skipped. What it does not do is answer.
    let (req, answer) = hub.request_screen();
    let verdict = answer.recv_timeout(Duration::from_millis(300));
    assert!(
        verdict.is_err(),
        "a screen came back with no head to draw it: {verdict:?}"
    );

    // And a head that wakes up late finds nothing waiting, rather than parking a
    // row for a caller that has already given up.
    hub.abandon_screen(&req);
    assert!(
        !hub.give_screen(&req, 80, 1, vec!["too late".into()]),
        "a late answer was accepted into a request nobody holds"
    );
}

#[test]
fn a_head_that_never_draws_times_out_and_the_next_request_still_works() {
    let (reg, server) = start("wedged");
    // Attached, pumping, never answering — a wedged head, which is the case the
    // deadline exists for. Without the pump the socket would back up and the
    // test would be about flow control instead.
    let (_client, _hello, reader) =
        HeadClient::attach(server.path(), "a", 0, "tui", "dead", Caps::default()).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let pumping = std::thread::spawn(move || pump(reader, tx));
    let hub = reg.get("a").unwrap();
    let drain =
        std::thread::spawn(move || while rx.recv_timeout(Duration::from_secs(2)).is_ok() {});

    let (first, answer) = hub.request_screen();
    assert!(answer.recv_timeout(Duration::from_millis(300)).is_err());
    hub.abandon_screen(&first);

    // The second request gets its own id. A hub that reused one would hand the
    // first request's late answer to the second caller.
    let (second, _) = hub.request_screen();
    assert_ne!(first, second);
    drop(drain);
    drop(pumping);
}
