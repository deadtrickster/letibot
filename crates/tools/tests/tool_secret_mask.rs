//! A secret the session holds must not come back out of a tool. The payload of a result is the
//! bytes the model is sent and the bytes the transcript keeps, so a command that puts a token in a
//! file - `env > x`, then `read x` - is the shape this refuses. It is not a boundary: the file on
//! disk still holds the token, and an encoding walks past an exact match.

use std::sync::Arc;

use letibot_tools::backend::{HostBackend, tempdir::TempDir};
use letibot_tools::builtins::retrieval::Unavailable;
use letibot_tools::events::RecordingToolSink;
use letibot_tools::exec::seat::SecretEnv;
use letibot_tools::runtime::ToolRuntime;
use letibot_transcript::ToolCall;

/// Long enough for the mask to consider it, and unmistakable in a failure message.
const TOKEN: &str = "gho_fixtureSECRET0000000000000000000000";

/// A directory holding one file whose whole content is the token, as `env > leaked.txt` would.
fn tree_with_a_leaked_token() -> TempDir {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("leaked.txt"), format!("{TOKEN}\n")).expect("a leaked file");
    dir
}

/// One `read` call answered through the runtime, and the transcript row it would store. The
/// secret goes on the backend BEFORE the runtime is built, which is the order production opens a
/// session in: the seat is settled with the backend, and the runtime asks it once.
fn read_the_leak(dir: &TempDir, secrets: Option<SecretEnv>) -> (String, String) {
    let mut backend = HostBackend::writable(dir.path()).expect("a writable backend");
    if let Some(s) = secrets {
        backend = backend.with_secret_env(s);
    }
    let registry = letibot_tools::read_only_tools(Arc::new(Unavailable)).expect("built-ins");
    let rt = ToolRuntime::new(registry, Box::new(backend));
    let mut sink = RecordingToolSink::new();
    let call = ToolCall {
        id: "call_1".into(),
        name: "read".into(),
        arguments: serde_json::json!({ "path": "leaked.txt" }).to_string(),
    };
    let result = rt.invoke("turn_1", &call, &mut sink);
    let row = serde_json::to_string(&ToolRuntime::transcript_item(&result))
        .expect("the row is serialisable");
    (result.payload, row)
}

#[test]
fn a_tool_result_carrying_a_secret_keeps_no_token_in_its_row() {
    let dir = tree_with_a_leaked_token();
    let secrets = SecretEnv(vec![("FIXTURE_TOKEN".to_string(), TOKEN.to_string())]);
    let (payload, row) = read_the_leak(&dir, Some(secrets));
    assert!(
        !payload.contains(TOKEN) && !row.contains(TOKEN),
        "the model was sent, and the transcript would keep, a token this session holds:\n{row}"
    );
    // The same length, so an offset into the result still means what it said: the mask is a
    // substitution and not a rewrite of the shape.
    assert!(
        payload.contains(&"*".repeat(TOKEN.len())),
        "the mask keeps the length so offsets still hold: {payload}"
    );
    // And it is not a boundary: what the tool read off the disk is still there.
    let on_disk = std::fs::read_to_string(dir.path().join("leaked.txt")).expect("the file");
    assert!(on_disk.contains(TOKEN), "the mask is not a boundary");
}

/// The control: the same read, with no secret installed, does show the token. Without it a test
/// that found no token would be measuring a `read` that returned nothing.
#[test]
fn without_the_secret_the_same_read_shows_the_token() {
    let dir = tree_with_a_leaked_token();
    let (payload, row) = read_the_leak(&dir, None);
    assert!(
        payload.contains(TOKEN) && row.contains(TOKEN),
        "the fixture has to leak when nothing masks it, or the test beside it proves nothing"
    );
}
