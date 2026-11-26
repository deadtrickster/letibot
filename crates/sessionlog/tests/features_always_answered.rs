//! A head that asks for features waits for the `features` frame, so the daemon answers every ask:
//! an empty `accepted` is how "none of those" is said. A head that asked for nothing gets none.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_sessionlog::server::serve_registry;
use serde_json::json;

const SESSION: &str = "s-features";

fn socket_path(tag: &str) -> std::path::PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    letibot_sessionlog::testing::scratch_root().join(format!(
        "letibot-feat-{tag}-{}-{n}.sock",
        std::process::id()
    ))
}

/// The frame tags the daemon writes after an attach, until it has been quiet for a moment.
fn frames_after_attach(
    socket: &std::path::Path,
    features: serde_json::Value,
) -> Vec<serde_json::Value> {
    let mut out = UnixStream::connect(socket).expect("the daemon's socket");
    out.set_read_timeout(Some(Duration::from_millis(1500)))
        .expect("a read timeout");
    let mut lines = BufReader::new(out.try_clone().expect("a second handle"));
    let mut line = serde_json::to_string(&json!({
        "frame": "attach",
        "protocol_version": 79,
        "session_id": SESSION,
        "since_seq": 0,
        "kind": "tui",
        "identity": "rano",
        "caps": { "queue": 64, "can_decide": false, "features": features },
    }))
    .expect("a frame serialises");
    line.push('\n');
    out.write_all(line.as_bytes()).expect("the head writes");
    let mut said = Vec::new();
    loop {
        let mut l = String::new();
        match lines.read_line(&mut l) {
            Ok(0) | Err(_) => break,
            Ok(_) if l.trim().is_empty() => continue,
            Ok(_) => said.push(serde_json::from_str(l.trim()).expect("the daemon writes a frame")),
        }
    }
    said
}

#[test]
fn a_head_that_asked_for_features_is_answered_even_when_none_is_accepted() {
    let r = Registry::new();
    r.create(SESSION, "", SessionWiring::default())
        .expect("a fresh registry takes the session");
    let server = serve_registry(r.clone(), socket_path("ask")).expect("the socket binds");

    let asked = frames_after_attach(server.path(), json!(["no_such_feature_v9"]));
    let answer = asked
        .iter()
        .find(|f| f["frame"] == "features")
        .unwrap_or_else(|| panic!("a head that asked waits for the answer: {asked:#?}"));
    assert_eq!(answer["accepted"], json!([]), "{answer}");

    let quiet = frames_after_attach(server.path(), json!([]));
    assert!(
        quiet.iter().any(|f| f["frame"] == "hello")
            && !quiet.iter().any(|f| f["frame"] == "features"),
        "a head that asked for nothing is sent no features frame: {quiet:#?}"
    );
}
