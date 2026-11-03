//! A settings row that changes is PUSHED to every head attached to the session, not only
//! answered to the one that asked: the rows a head draws are live rather than a photograph
//! taken at attach.

use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use letibot_sessionlog::protocol::{Caps, ClientFrame, PROTOCOL_VERSION, ServerFrame, SettingRow};
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_sessionlog::server::serve_conn;
use letibot_sessionlog::wire::{FrameReader, FrameWriter, WireError};

/// How long a frame that should arrive may take. Generous: the push is a condvar wake, so a
/// frame that is coming arrives in microseconds and this only has to outlast a loaded box.
const PATIENCE: Duration = Duration::from_secs(5);

fn row(key: &str, value: &str) -> SettingRow {
    SettingRow {
        key: key.into(),
        value: value.into(),
        source: "this session".into(),
        editable: String::new(),
        choices: Vec::new(),
        tools: Vec::new(),
    }
}

/// One head on a socket pair, attached and past its `Hello`. The test reads its own frames, so
/// nothing here swallows what the daemon sends.
fn head(
    registry: &Arc<Registry>,
    session: &str,
) -> (FrameWriter<UnixStream>, FrameReader<UnixStream>) {
    let (mine, theirs) = UnixStream::pair().expect("a socket pair");
    // A head that is owed nothing must not block the test for ever.
    mine.set_read_timeout(Some(PATIENCE)).expect("a read clock");
    let reg = registry.clone();
    std::thread::spawn(move || {
        let _ = serve_conn(reg, theirs);
    });
    let mut w = FrameWriter::new(mine.try_clone().expect("clone"));
    let mut r = FrameReader::new(mine);
    w.write(&ClientFrame::Attach {
        protocol_version: PROTOCOL_VERSION,
        session_id: session.into(),
        since_seq: 0,
        kind: "tui".into(),
        identity: "test".into(),
        caps: Caps::default(),
    })
    .expect("attach");
    match r.read::<ServerFrame>().expect("hello") {
        ServerFrame::Hello { .. } => {}
        other => panic!("expected a Hello, got {other:?}"),
    }
    (w, r)
}

/// The settings frame this reader is owed, and its rows. The `HeadAttached` of a second head
/// rides the same stream, so everything that is not settings is passed over.
fn settings(r: &mut FrameReader<UnixStream>) -> Vec<SettingRow> {
    let began = Instant::now();
    while began.elapsed() < PATIENCE {
        match r.read::<ServerFrame>() {
            Ok(ServerFrame::Settings { rows }) => return rows,
            Ok(_) => {}
            Err(WireError::Io(e))
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(e) => panic!("the head lost its connection before it was told: {e:?}"),
        }
    }
    panic!("no settings frame arrived in {PATIENCE:?}");
}

/// Nothing arrives: the reader's clock expires. Anything else - a frame, a closed socket - is a
/// fact this test did not ask for.
fn silent(r: &mut FrameReader<UnixStream>) {
    match r.read::<ServerFrame>() {
        Err(WireError::Io(e))
            if matches!(
                e.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ) => {}
        Err(e) => panic!("the head was owed nothing and the connection said {e:?}"),
        Ok(frame) => panic!("the head was owed nothing and was sent {frame:?}"),
    }
}

/// The scene one layer down: two heads on one session, one of them asks, and the other is told
/// anyway. The rows are the harness's answer, so the connection that did not ask has no request
/// to correlate an unprompted answer with - it takes it as the new truth.
#[test]
fn a_settings_change_reaches_the_head_that_did_not_ask() {
    let registry = Registry::new();
    registry
        .create("s-1", "one", SessionWiring::default())
        .expect("a fresh registry has no session by that name");

    let (mut asker, mut asker_r) = head(&registry, "s-1");
    let (_quiet, mut quiet_r) = head(&registry, "s-1");

    // The premise: the asker has read the rows once, and they are not what is about to be
    // published - so the push below is a CHANGE and not a repeat.
    asker.write(&ClientFrame::Settings).expect("settings");
    assert!(settings(&mut asker_r).is_empty(), "nothing published yet");

    registry.set_settings("s-1", vec![row("mode", "read-only")]);

    // Both of this session's heads, and the one that never asked is the subject.
    let asked = settings(&mut asker_r);
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].key, "mode");
    let unasked = settings(&mut quiet_r);
    assert_eq!(
        unasked.len(),
        1,
        "the head that did not ask was not told: {unasked:?}"
    );
    assert_eq!(unasked[0].key, "mode");
    assert_eq!(unasked[0].value, "read-only");
}

/// The same rows published again are not a change, and a head is not woken by a photograph of
/// what it already draws. Without this a flush that republishes unchanged rows redraws every
/// attached screen for nothing.
#[test]
fn the_same_rows_again_are_not_pushed() {
    let registry = Registry::new();
    registry
        .create("s-1", "one", SessionWiring::default())
        .expect("create");
    let (_w, mut r) = head(&registry, "s-1");

    registry.set_settings("s-1", vec![row("mode", "read-only")]);
    assert_eq!(settings(&mut r)[0].value, "read-only");

    registry.set_settings("s-1", vec![row("mode", "read-only")]);
    silent(&mut r);
}

/// The push belongs to ONE session's hub. A head attached elsewhere is not told about a session
/// it cannot see, which is what keeps a picker's other rows out of this head's settings.
#[test]
fn another_sessions_head_is_not_told() {
    let registry = Registry::new();
    registry
        .create("s-1", "one", SessionWiring::default())
        .expect("create");
    registry
        .create("s-2", "two", SessionWiring::default())
        .expect("create");
    let (_w, mut other_r) = head(&registry, "s-2");

    registry.set_settings("s-1", vec![row("mode", "read-only")]);
    silent(&mut other_r);
}
