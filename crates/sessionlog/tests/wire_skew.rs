//! Frames another build sent, read by this one. `tests/wire_golden/v<N>/` is the corpus the
//! protocol-N daemon and head emitted (`examples/wire_capture.rs`), kept once N is superseded, so
//! a later build is checked against the bytes, not against its own types at both ends.

use std::path::{Path, PathBuf};

use letibot_sessionlog::event::{SessionEvent, TodoBy};
use letibot_sessionlog::protocol::{ClientFrame, PROTOCOL_VERSION, PromptDelivery, ServerFrame};
use letibot_sessionlog::testing::{wait_until, wire_corpus};
use letibot_sessionlog::view::TurnState;
use letibot_transcript::{Speaker, TranscriptItem};

fn golden(version: u32) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/wire_golden/v{version}"))
}

fn lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

fn server(version: u32) -> Vec<ServerFrame> {
    lines(&golden(version).join("server.jsonl"))
        .iter()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("v{version} server: {e}: {l}")))
        .collect()
}

fn client(version: u32) -> Vec<ClientFrame> {
    lines(&golden(version).join("client.jsonl"))
        .iter()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("v{version} client: {e}: {l}")))
        .collect()
}

/// What the 78 daemon and head said, as this build reads it: each value the corpus carries, not
/// only that it parses.
#[test]
fn wire_golden_frames_from_v78_still_decode_with_same_meaning() {
    let frames = server(78);
    let Some(ServerFrame::Hello {
        protocol_version,
        snapshot: Some(snap),
        ..
    }) = frames.first()
    else {
        panic!(
            "the corpus opens with a snapshot Hello: {:?}",
            frames.first()
        );
    };
    assert_eq!(*protocol_version, 78);
    let items: Vec<&TranscriptItem> = snap.items.iter().filter_map(|r| r.item.as_ref()).collect();
    assert!(
        matches!(
            items[0],
            TranscriptItem::User {
                speaker: Speaker::Operator,
                ..
            }
        ),
        "{:?}",
        items[0]
    );
    assert!(
        matches!(items[1], TranscriptItem::User { speaker: Speaker::Peer { from }, .. } if from == "cc@rano"),
        "a peer's row reads as the peer's: {:?}",
        items[1]
    );
    let TranscriptItem::ToolResult {
        ran_ms, decision, ..
    } = items[2]
    else {
        panic!("{:?}", items[2]);
    };
    assert_eq!(*ran_ms, Some(1_234));
    assert_eq!(
        decision.as_ref().and_then(|d| d["decision"].as_str()),
        Some("allow_once")
    );
    let turn = snap.turn.as_ref().expect("the parked turn");
    assert!(
        matches!(&turn.state, TurnState::Parked { model, until_ms: 1_700_000_000_000, .. } if model == "glm"),
        "a park stays a park: {:?}",
        turn.state
    );

    let events: Vec<&SessionEvent> = frames[1..]
        .iter()
        .map(|f| match f {
            ServerFrame::Event(e) => &e.event,
            other => panic!("a live event, not {other:?}"),
        })
        .collect();
    assert!(
        matches!(events[0], SessionEvent::TranscriptContent { item, .. }
        if matches!(&**item, TranscriptItem::User { speaker: Speaker::Peer { from }, .. } if from == "cc@rano"))
    );
    assert!(matches!(events[1], SessionEvent::TodosUpdated { todos }
        if matches!(&todos[0].by, TodoBy::Peer(h) if h == "cc@rano")));
    assert!(matches!(
        events[3],
        SessionEvent::TurnFinished {
            ends_turn: false,
            ..
        }
    ));
    assert!(
        matches!(events[4], SessionEvent::TurnEnded { turn_id } if turn_id == "t2"),
        "the prompt's end is its own frame at 78: {:?}",
        events[4]
    );

    let heads = client(78);
    let Some(ClientFrame::Prompt {
        text,
        context: Some(context),
        from,
        delivery,
        ..
    }) = heads
        .iter()
        .find(|f| matches!(f, ClientFrame::Prompt { .. }))
    else {
        panic!("the corpus holds a prompt with context");
    };
    assert_eq!(text, "explain this");
    assert_eq!(
        *delivery,
        PromptDelivery::Queue,
        "a queued prompt must not steer"
    );
    assert_eq!(
        from.as_deref(),
        Some("cc@rano"),
        "a peer's prompt is not the operator's"
    );
    assert_eq!(context.refs[0].file, "/w/src/lib.rs");
    assert_eq!(context.refs[0].range.map(|r| r.end.line), Some(9));
    assert!(context.refs[0].unsaved);
    assert!(heads.iter().any(|f| matches!(f,
        ClientFrame::DriverTodos { from, rows, .. } if from == "cc@rano" && rows.len() == 1)));
    assert!(heads.iter().any(|f| matches!(f,
        ClientFrame::Answer { option_id, note: Some(n), .. }
            if option_id == "deny_and_tell" && n == "not on main")));
}

/// A field renamed or retyped, or a value spelled differently, without moving the version. The
/// corpus at this version is what the build sends today, so a change to it is a bump and a row.
#[test]
fn this_build_sends_its_own_versions_corpus() {
    let dir = golden(PROTOCOL_VERSION);
    assert!(
        dir.is_dir(),
        "protocol {PROTOCOL_VERSION} has no corpus: a bump adds one with \
         `cargo run -p letibot-sessionlog --example wire_capture -- {} {PROTOCOL_VERSION}`",
        dir.display()
    );
    let now = wire_corpus::server_frames(PROTOCOL_VERSION).expect("the capture");
    let kept = lines(&dir.join("server.jsonl"));
    let heads = wire_corpus::client_frames();
    let kept_heads = lines(&dir.join("client.jsonl"));
    for (sent, kept) in now.iter().zip(&kept).chain(heads.iter().zip(&kept_heads)) {
        assert_eq!(
            wire_corpus::normalized(sent),
            wire_corpus::normalized(kept),
            "the wire changed under protocol {PROTOCOL_VERSION}: bump it, add its compat row, and \
             capture its corpus"
        );
    }
    assert_eq!((now.len(), heads.len()), (kept.len(), kept_heads.len()));
}

/// Every kept corpus still parses here. The two-build job (`scripts/wire-skew.sh`) is the other
/// direction: an older build's decoder over this build's frames.
#[test]
fn every_kept_corpus_decodes() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/wire_golden");
    let mut seen = 0;
    for entry in std::fs::read_dir(&root).unwrap() {
        let name = entry.unwrap().file_name().into_string().unwrap();
        let version: u32 = name
            .strip_prefix('v')
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| panic!("{name} is not v<protocol>"));
        assert!(
            version <= PROTOCOL_VERSION,
            "{name} is newer than this build"
        );
        assert!(!server(version).is_empty() && !client(version).is_empty());
        seen += 1;
    }
    assert!(seen > 0);
}

/// The live half of the skew: a head's own `HeadClient` attaches to the corpus server and reads
/// its snapshot. `examples/wire_head.rs` is the same attach, built from another revision, which is
/// what `scripts/wire-skew.sh` runs against this build.
#[test]
fn a_head_attaches_to_the_corpus_server_and_reads_the_snapshot() {
    use letibot_sessionlog::client::HeadClient;
    use letibot_sessionlog::protocol::Caps;
    let dir = letibot_sessionlog::testing::scratch("wire-head-attach");
    std::fs::create_dir_all(&dir).unwrap();
    let sock = dir.join("d.sock");
    let serving = {
        let sock = sock.clone();
        std::thread::spawn(move || wire_corpus::serve_for_an_older_head(&sock, 3_000))
    };
    wait_until("the corpus server's socket", || sock.exists());
    let (_head, hello, _reader) =
        HeadClient::attach(&sock, "s", 0, "tui", "wire-head", Caps::default())
            .expect("the corpus server takes an attach");
    let ServerFrame::Hello { snapshot, .. } = &hello else {
        panic!("the answer to an attach was not a Hello: {hello:?}");
    };
    let snapshot = snapshot
        .as_deref()
        .expect("a snapshot attach is a snapshot");
    assert_eq!(snapshot.session_id, "s");
    assert!(!snapshot.items.is_empty(), "the exchange's rows are in it");
    let _ = serving.join();
    let _ = std::fs::remove_dir_all(&dir);
}
