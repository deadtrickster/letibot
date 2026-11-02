//! `SuggestShell` to `ShellSuggestions`: the smart `!`'s one round trip. A head has no HTTP client
//! and no transcript-wide context while the daemon has both, so the ask is a frame - and the daemon
//! always answers, because an empty list and a silent daemon are the same fact to a head.

use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::protocol::{Caps, ClientFrame, PROTOCOL_VERSION, ServerFrame};
use letibot_sessionlog::registry::{Registry, SessionWiring, ShellSuggester};
use letibot_sessionlog::server::serve_conn;
use letibot_sessionlog::wire::{FrameReader, FrameWriter};

/// A suggester that records what it was asked and answers with canned lines. A `ShellSuggester`
/// and not the daemon's own, because this file is about the seam: the model call is `harnessd`'s
/// half and has its own tests.
struct Canned {
    lines: Vec<String>,
    asked: Mutex<Vec<(String, String, String)>>,
}

impl Canned {
    fn new(lines: &[&str]) -> Arc<Self> {
        Arc::new(Canned {
            lines: lines.iter().map(|l| l.to_string()).collect(),
            asked: Mutex::new(Vec::new()),
        })
    }

    /// What it was asked, in order: (session, workspace, prefix).
    fn asked(&self) -> Vec<(String, String, String)> {
        self.asked.lock().unwrap().clone()
    }
}

impl ShellSuggester for Canned {
    fn suggest(&self, hub: &Hub, workspace: &str, prefix: &str) -> Vec<String> {
        self.asked.lock().unwrap().push((
            hub.session_id(),
            workspace.to_string(),
            prefix.to_string(),
        ));
        self.lines.clone()
    }
}

fn start(suggester: Option<Arc<dyn ShellSuggester>>) -> Arc<Registry> {
    let registry = Registry::new();
    if let Some(s) = suggester {
        registry.set_suggester(s);
    }
    registry
        .create(
            "s-1",
            "one",
            SessionWiring {
                model: "qwen3-4b".into(),
                dialect: "qwen".into(),
                endpoint: "127.0.0.1:8080".into(),
                workspace: "/tmp/ws".into(),
            },
        )
        .expect("create");
    registry
}

/// Attach a head to `s-1` and hand back the wire plus the `Hello` it was given.
fn attach(registry: &Arc<Registry>) -> (FrameWriter<UnixStream>, FrameReader<UnixStream>) {
    let (a, b) = UnixStream::pair().expect("pair");
    let reg = registry.clone();
    let _server = std::thread::spawn(move || {
        let _ = serve_conn(reg, a);
    });
    let mut w = FrameWriter::new(b.try_clone().expect("clone"));
    let mut r = FrameReader::new(b);
    w.write(&ClientFrame::Attach {
        protocol_version: PROTOCOL_VERSION,
        session_id: "s-1".into(),
        since_seq: 0,
        kind: "tui".into(),
        identity: "test".into(),
        caps: Caps::default(),
    })
    .expect("attach");
    assert!(matches!(
        r.read::<ServerFrame>().expect("hello"),
        ServerFrame::Hello { .. }
    ));
    (w, r)
}

/// Ask, and get the answer, skipping the session's own live events on the way as a head's pump
/// does: they share the stream with the answer, so one frame read is not the answer.
fn ask_frame(
    w: &mut FrameWriter<UnixStream>,
    r: &mut FrameReader<UnixStream>,
    id: &str,
    prefix: &str,
) -> ServerFrame {
    w.write(&ClientFrame::SuggestShell {
        client_request_id: id.into(),
        expected_seq: 0,
        prefix: prefix.into(),
    })
    .expect("ask");
    loop {
        match r.read::<ServerFrame>().expect("an answer") {
            f @ ServerFrame::ShellSuggestions { .. } => return f,
            ServerFrame::Event(_) => continue,
            other => panic!("expected ShellSuggestions, got {other:?}"),
        }
    }
}

/// The same ask, for a test that is about what did *not* happen rather than the answer.
fn ask(w: &mut FrameWriter<UnixStream>, r: &mut FrameReader<UnixStream>, id: &str, prefix: &str) {
    let _ = ask_frame(w, r, id, prefix);
}

/// The frames round-trip, and the daemon hands the suggester the session's own context: the head
/// has the prefix, the daemon has the session and its workspace. The answer carries the id and the
/// prefix back, so the head can key its cache without a request table.
#[test]
fn the_daemon_asks_the_suggester_for_this_session_and_answers_the_head() {
    let suggester = Canned::new(&["! git status", "! git log"]);
    let registry = start(Some(suggester.clone()));
    let (mut w, mut r) = attach(&registry);

    let f = ask_frame(&mut w, &mut r, "head-1-s1", "! git");
    match f {
        ServerFrame::ShellSuggestions {
            client_request_id,
            prefix,
            lines,
        } => {
            assert_eq!(client_request_id, "head-1-s1", "the id it was asked under");
            assert_eq!(
                prefix, "! git",
                "the prefix, echoed back for the head's cache"
            );
            assert_eq!(
                lines,
                vec!["! git status".to_string(), "! git log".to_string()],
                "the suggester's lines, in the order it offered them"
            );
        }
        other => panic!("not ShellSuggestions: {other:?}"),
    }
    assert_eq!(
        suggester.asked(),
        vec![(
            "s-1".to_string(),
            "/tmp/ws".to_string(),
            "! git".to_string()
        )],
        "the session's own hub and the workspace from the wiring"
    );
}

/// A daemon with no suggester answers an empty list rather than nothing, because *no local model*
/// and *the model had nothing* are the same fact to a head that waits. It also keeps a suggestion
/// from costing money per keystroke on a box with no local endpoint.
#[test]
fn a_daemon_with_no_suggester_answers_an_empty_list() {
    let registry = start(None);
    let (mut w, mut r) = attach(&registry);
    let f = ask_frame(&mut w, &mut r, "head-1-s1", "! git");
    match f {
        ServerFrame::ShellSuggestions { lines, prefix, .. } => {
            assert!(lines.is_empty(), "no suggester, no suggestion: {lines:?}");
            assert_eq!(prefix, "! git", "and the ask is still answered by name");
        }
        other => panic!("not ShellSuggestions: {other:?}"),
    }
}

/// Nothing in the round trip is submitted: no command queued, no row appended, no seq moved. A
/// head that read the answer as a run would run a model's guess on a keystroke, and a frame that
/// moved the log would be one the other heads have to be told about.
#[test]
fn the_round_trip_writes_no_row_and_moves_no_seq() {
    let registry = start(Some(Canned::new(&["! git status"])));
    let hub = registry.get("s-1").expect("the session");
    let (mut w, mut r) = attach(&registry);
    let before = hub.head_seq();

    ask(&mut w, &mut r, "head-1-s1", "! git");

    assert_eq!(hub.head_seq(), before, "the log did not move");
    assert!(
        hub.snapshot().items.is_empty(),
        "no row was appended: a suggestion is not a run"
    );
    assert!(
        hub.try_command().is_none(),
        "and nothing was queued for a turn to pick up"
    );
}
