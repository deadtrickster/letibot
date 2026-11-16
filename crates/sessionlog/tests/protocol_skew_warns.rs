//! A head on another protocol number is a fact about the install, and the daemon says it in the
//! conversation: once per head, not into a log file an operator never opens.

use std::os::unix::net::UnixStream;
use std::time::Duration;

use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::protocol::{Caps, ClientFrame, PROTOCOL_VERSION, ServerFrame};
use letibot_sessionlog::server::{ServerHandle, serve};
use letibot_sessionlog::testing::{scratch_root, warn};
use letibot_sessionlog::wire::{FrameReader, FrameWriter};

const END: &str = "skew-warning-end";
const PATIENCE: Duration = Duration::from_secs(60);

fn a_server(tag: &str) -> (std::sync::Arc<Hub>, ServerHandle) {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let socket = scratch_root().join(format!(
        "letibot-skew-warn-{tag}-{}-{n}.sock",
        std::process::id()
    ));
    let hub = Hub::new("s");
    let server = serve(hub.clone(), socket).expect("bind");
    (hub, server)
}

/// A head at `protocol` attached by hand, because `HeadClient::attach` always speaks this build's.
fn attach_at(
    path: &std::path::Path,
    protocol: u32,
    identity: &str,
) -> (FrameWriter<UnixStream>, FrameReader<UnixStream>) {
    let stream = UnixStream::connect(path).expect("connect");
    stream
        .set_read_timeout(Some(PATIENCE))
        .expect("a read bound");
    let mut w = FrameWriter::new(stream.try_clone().expect("clone"));
    w.write(&ClientFrame::Attach {
        protocol_version: protocol,
        session_id: "s".into(),
        since_seq: 0,
        kind: "tui".into(),
        identity: identity.into(),
        caps: Caps::default(),
    })
    .expect("the attach is written");
    (w, FrameReader::new(stream))
}

/// Read to the sentinel `END`, counting the skew warnings on the way, and publish the sentinel the
/// moment the `Hello` lands so the daemon's own publish (which happens before it) is ordered first.
fn read_to_the_sentinel(hub: &Hub, r: &mut FrameReader<UnixStream>) -> usize {
    let mut skew = 0;
    loop {
        let frame: ServerFrame = r.read().expect("a frame from the daemon");
        match &frame {
            ServerFrame::Hello { .. } => {
                hub.publish(warn(END));
            }
            ServerFrame::Event(env) => match &env.event {
                SessionEvent::Warning { code, .. } if code == "protocol_skew" => skew += 1,
                SessionEvent::Warning { detail, .. } if detail == END => return skew,
                _ => {}
            },
            _ => {}
        }
    }
}

#[test]
fn a_skewed_attach_raises_a_protocol_skew_warning() {
    let (hub, server) = a_server("once");
    // One number behind is the ordinary skew: accepted, served, and said.
    let (_w, mut r) = attach_at(server.path(), PROTOCOL_VERSION - 1, "old-head");
    assert_eq!(
        read_to_the_sentinel(&hub, &mut r),
        1,
        "the daemon says the skew in the conversation, once"
    );

    // The same head again: the fact has been said to it, so nothing new is written.
    let (_w2, mut r2) = attach_at(server.path(), PROTOCOL_VERSION - 1, "old-head");
    assert_eq!(
        read_to_the_sentinel(&hub, &mut r2),
        0,
        "one head is told once, however often it reconnects"
    );

    // A different head on the same skew is told in its own turn: the dedupe is per head, and a
    // second person must not be left to work it out from a log.
    let (_w3, mut r3) = attach_at(server.path(), PROTOCOL_VERSION - 1, "another-old-head");
    assert_eq!(read_to_the_sentinel(&hub, &mut r3), 1);

    // And a head on this build is told nothing at all.
    let (_w4, mut r4) = attach_at(server.path(), PROTOCOL_VERSION, "this-head");
    assert_eq!(
        read_to_the_sentinel(&hub, &mut r4),
        0,
        "no skew, no warning"
    );
    server.shutdown();
}
