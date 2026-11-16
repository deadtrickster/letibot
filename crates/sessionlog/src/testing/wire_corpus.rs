//! The wire corpus: one fixed exchange, as this build's daemon sends it to a head at a given
//! protocol and as this build's head encodes its own frames. `tests/wire_golden/v<N>/` keeps it
//! per protocol, and `scripts/wire-skew.sh` feeds it to an older build's decoder.

use std::io;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use letibot_transcript::{Speaker, ToolOutcome, TranscriptItem, UserPart};
use serde_json::Value;

use crate::event::{SessionEvent, TodoBy, TodoEntry, TodoState, TodoStatus, TurnPark};
use crate::hub::Hub;
use crate::protocol::{
    Ack, AnswerMode, Caps, ClientFrame, ContextAsk, ContextPos, ContextRange, ContextRef,
    EditorContext, OpenBuffer, PromptDelivery,
};
use crate::testing::{scratch, socket_path, turn_finished_round, turn_started, warn};
use crate::wire::{FrameReader, FrameWriter};

/// The last event of the exchange: a head that has read it has read everything.
pub const END: &str = "wire-corpus-end";

/// Keys whose values differ between two runs of the same exchange: clocks and minted ids.
const VOLATILE: &[&str] = &["ts", "created_ms", "last_ms", "moved_ms"];

fn user_row(id: &str, text: &str, speaker: Speaker) -> SessionEvent {
    SessionEvent::TranscriptAppended {
        item_id: id.into(),
        kind: "user".into(),
        ledger_head: "beef".into(),
        item: Some(Box::new(TranscriptItem::User {
            parts: vec![UserPart::Text { text: text.into() }],
            speaker,
        })),
        body_bytes: 0,
    }
}

fn peer_todo(content: &str) -> TodoEntry {
    TodoEntry {
        content: content.into(),
        status: TodoStatus::InProgress,
        by: TodoBy::Peer("cc@rano".into()),
        when: None,
        needs: Vec::new(),
    }
}

/// What the session holds before the head attaches: its snapshot.
fn before_attach(hub: &Hub) {
    hub.publish(user_row("u1", "fix the build", Speaker::Operator));
    hub.publish(user_row(
        "u2",
        "from a peer",
        Speaker::Peer {
            from: "cc@rano".into(),
        },
    ));
    hub.publish(SessionEvent::TranscriptAppended {
        item_id: "r1".into(),
        kind: "tool_result".into(),
        ledger_head: "beef".into(),
        item: Some(Box::new(TranscriptItem::ToolResult {
            call_id: "c1".into(),
            name: "bash".into(),
            outcome: ToolOutcome::Ok,
            payload: "done".into(),
            edits: Vec::new(),
            origin: None,
            media: None,
            ran_ms: Some(1_234),
            decision: Some(serde_json::json!({"req_id": "r1", "decision": "allow_once"})),
        })),
        body_bytes: 0,
    });
    hub.publish(SessionEvent::TodosUpdated {
        todos: vec![peer_todo("land the corpus")],
    });
    hub.publish(turn_started("t1"));
    hub.publish(SessionEvent::TurnInterrupted {
        turn_id: "t1".into(),
        reason: "usage limit".into(),
        partial_kept: true,
        park: Some(TurnPark {
            model: "glm".into(),
            until_ms: 1_700_000_000_000,
            retry_at_ms: None,
        }),
    });
}

/// What the head reads live after its Hello.
fn after_attach(hub: &Hub) {
    hub.publish(SessionEvent::TranscriptContent {
        item_id: "u3".into(),
        item: Box::new(TranscriptItem::User {
            parts: vec![UserPart::Text {
                text: "a second peer line".into(),
            }],
            speaker: Speaker::Peer {
                from: "cc@rano".into(),
            },
        }),
    });
    hub.publish(SessionEvent::TodosUpdated {
        todos: vec![peer_todo("check the old head")],
    });
    hub.publish(turn_started("t2"));
    hub.publish(turn_finished_round("t2"));
    hub.end_the_prompt();
    hub.publish(warn(END));
}

/// Every frame this build's daemon sends a head that attaches at `head`, as raw lines: the Hello
/// and the live events, or the Bye below the floor.
pub fn server_frames(head: u32) -> io::Result<Vec<String>> {
    let dir = scratch("wire-corpus");
    std::fs::create_dir_all(&dir)?;
    let hub = Hub::new("s");
    before_attach(&hub);
    let server = crate::server::serve(hub.clone(), socket_path(&dir, "d.sock"))?;
    let stream = UnixStream::connect(server.path())?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut w = FrameWriter::new(stream.try_clone()?);
    w.write(&ClientFrame::Attach {
        protocol_version: head,
        session_id: "s".into(),
        since_seq: 0,
        kind: "tui".into(),
        identity: "corpus".into(),
        caps: Caps {
            queue: 1 << 12,
            // Asked for, so the corpus carries the daemon's echo as well as the frames it gates.
            features: vec![crate::protocol::FEATURE_QUEUE_PROMPT.to_string()],
            ..Caps::default()
        },
    })
    .map_err(io::Error::other)?;
    let mut r = FrameReader::new(stream);
    let mut lines = Vec::new();
    let mut published = false;
    loop {
        let v: Value = match r.read() {
            Ok(v) => v,
            Err(e) => {
                server.shutdown();
                let _ = std::fs::remove_dir_all(&dir);
                return Err(io::Error::other(format!(
                    "the corpus stopped after {} frames: {e}",
                    lines.len()
                )));
            }
        };
        let line = serde_json::to_string(&v).map_err(io::Error::other)?;
        let frame = v["frame"].as_str().unwrap_or_default().to_string();
        let end = frame == "bye" || v["detail"].as_str() == Some(END);
        lines.push(line);
        if end {
            break;
        }
        if frame == "hello" && !published {
            published = true;
            after_attach(&hub);
        }
    }
    let _ = w.write(&ClientFrame::Detach);
    server.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
    Ok(lines)
}

/// Head frames as this build encodes them, one per line.
pub fn client_frames() -> Vec<String> {
    let context = EditorContext {
        origin: "rano".into(),
        refs: vec![ContextRef {
            file: "/w/src/lib.rs".into(),
            range: Some(ContextRange {
                start: ContextPos { line: 3, column: 1 },
                end: ContextPos { line: 9, column: 2 },
            }),
            cursor: Some(ContextPos { line: 4, column: 5 }),
            text: "fn main() {}".into(),
            unsaved: true,
            lang: "rust".into(),
            title: String::new(),
        }],
        open: vec![OpenBuffer {
            file: "/w/Cargo.toml".into(),
            unsaved: false,
            visible: true,
        }],
        ask: Some(ContextAsk {
            preset: "explain".into(),
            answer: AnswerMode::Notes,
        }),
        outcomes: Vec::new(),
        captured: String::new(),
    };
    let frames = [
        ClientFrame::Attach {
            protocol_version: crate::protocol::PROTOCOL_VERSION,
            session_id: "s".into(),
            since_seq: 7,
            kind: "tui".into(),
            identity: "corpus".into(),
            caps: Caps {
                queue: 64,
                can_decide: true,
                features: vec!["queue".into()],
                row_bodies: true,
            },
        },
        ClientFrame::Ack(Ack {
            seq: 12,
            rendered: 10,
            filtered: 2,
        }),
        ClientFrame::Resync,
        ClientFrame::Prompt {
            client_request_id: "p1".into(),
            expected_seq: 12,
            text: "explain this".into(),
            attachments: Vec::new(),
            context: Some(context),
            from: Some("cc@rano".into()),
            delivery: PromptDelivery::Queue,
        },
        ClientFrame::DriverTodos {
            client_request_id: "d1".into(),
            expected_seq: 12,
            from: "cc@rano".into(),
            rows: vec![TodoState {
                content: "land the corpus".into(),
                status: TodoStatus::Completed,
            }],
        },
        ClientFrame::Answer {
            client_request_id: "a1".into(),
            req_id: "r1".into(),
            option_id: "deny_and_tell".into(),
            pattern: None,
            note: Some("not on main".into()),
        },
        ClientFrame::Interrupt {
            client_request_id: "i1".into(),
            expected_seq: 12,
            reason: "operator".into(),
        },
        ClientFrame::Detach,
    ];
    frames
        .iter()
        .map(|f| serde_json::to_string(f).expect("a head frame encodes"))
        .collect()
}

/// `line` with the [`VOLATILE`] keys removed at every depth, for comparing two captures.
pub fn normalized(line: &str) -> Value {
    fn strip(v: &mut Value) {
        match v {
            Value::Object(m) => {
                m.retain(|k, _| !VOLATILE.contains(&k.as_str()));
                m.values_mut().for_each(strip);
            }
            Value::Array(a) => a.iter_mut().for_each(strip),
            _ => {}
        }
    }
    let mut v: Value = serde_json::from_str(line).expect("a corpus line is JSON");
    strip(&mut v);
    v
}

/// Writes `server.jsonl` (the frames sent to a head at `head`) and `client.jsonl` into `dir`.
pub fn write(dir: &Path, head: u32) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let join = |lines: Vec<String>| lines.join("\n") + "\n";
    std::fs::write(dir.join("server.jsonl"), join(server_frames(head)?))?;
    std::fs::write(dir.join("client.jsonl"), join(client_frames()))
}
