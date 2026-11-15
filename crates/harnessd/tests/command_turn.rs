//! A command file's `model` and `allowed-tools` hold for the one turn the file starts: the turn
//! goes to the named model, a call outside the list is refused, and the next turn is back on the
//! session's own model with every tool.

use std::sync::{Arc, Mutex};

use letibot_harnessd::config::{Config, Seat};
use letibot_harnessd::{Dialect, Parts, Sessions};
use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::hub::{CommandKind, Hub, QueuedCommand};
use letibot_sessionlog::registry::Registry;
use letibot_tokencore::store::{SessionRecord, Store};
use letibot_turn::Endpoint;

mod support;

#[test]
fn a_command_turn_runs_on_its_model_with_its_tools_and_the_next_turn_does_not() {
    let root = letibot_sessionlog::testing::scratch("command-turn");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("config/letibot")).unwrap();
    // SAFETY: this binary has ONE test, and these are set before any thread that reads config exists.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
        std::env::set_var("XDG_DATA_HOME", root.join("data"));
        std::env::remove_var("LETIBOT_MODE");
    }
    let fake = Fake::serve();
    let providers = root.join("providers.toml");
    std::fs::write(
        &providers,
        format!(
            "[provider.probe]\nprotocol = \"chat-completions\"\nurl = \"{}\"\nkey = \"sk-not-used\"\nmodel = \"m1\"\n",
            fake.url
        ),
    )
    .unwrap();

    let workspace = root.join("ws");
    let commands = workspace.join(".letibot/commands");
    std::fs::create_dir_all(commands.join("ops")).unwrap();
    std::fs::write(
        commands.join("ops/tidy.md"),
        "---\nmodel: probe/m2\nallowed-tools: Read, Bash(git status:*)\n---\nTidy $ARGUMENTS.\n",
    )
    .unwrap();
    std::fs::write(commands.join("plain.md"), "Plain.").unwrap();

    let store = root.join("sessions.db");
    let id = "command-turn".to_string();
    Store::open(&store)
        .unwrap()
        .put_session(&SessionRecord {
            id: id.clone(),
            title: None,
            model_id: "m".into(),
            dialect_sha: "sha".into(),
            workspace_root: workspace.display().to_string(),
            owner: "dead".into(),
            role: None,
            approvers: vec![],
            parent_session_id: None,
        })
        .unwrap();
    let mut cfg = Config::for_this_box(workspace.to_str().unwrap());
    cfg.dialect = Dialect::Qwen;
    cfg.seat = Seat::Leticode;
    cfg.store = Some(store);
    cfg.session_id = id.clone();
    cfg.web_search = None;
    cfg.permission = Vec::new();
    // The session's own model answers nothing, so a round that reaches the fake went to the
    // command's model.
    cfg.endpoint = Endpoint::new("127.0.0.1", 1);
    cfg.http_retries = 0;
    cfg.vocab_gguf = support::vocab::gguf();
    cfg.providers_file = Some(providers);

    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let registry = Registry::new();
    registry.create(&id, "", Sessions::wiring(&cfg)).unwrap();
    let mut sessions = Sessions::open_first(&parts, cfg.clone(), registry.clone()).unwrap();
    let hub = registry.get(&id).expect("the hub");

    sessions.dispatch(&id, &command("ops/tidy src"));
    let bodies = fake.bodies();
    assert_eq!(
        bodies.len(),
        2,
        "a refused call, then the answer: {bodies:?}"
    );
    assert!(
        bodies[0].contains("\"m2\""),
        "the file's model: {}",
        bodies[0]
    );
    assert!(
        bodies[0].contains("This command allows only these tools: read, bash(git status *)."),
        "the model is told the list: {}",
        bodies[0]
    );
    assert!(
        bodies[1].contains("`write` is not one of them"),
        "the call outside the list is refused: {}",
        bodies[1]
    );
    assert!(!workspace.join("out.txt").exists(), "and did not run");
    let said = warnings(&hub, "command_file");
    assert!(
        said.iter()
            .any(|w| w.contains("this turn runs on `probe/m2`")
                && w.contains("this turn's tools: read, bash(git status *)")),
        "{said:?}"
    );

    sessions.dispatch(&id, &command("plain"));
    assert_eq!(
        fake.bodies().len(),
        2,
        "the next turn went to the session's own model, not the command's"
    );
    let _ = std::fs::remove_dir_all(&root);
}

fn command(line: &str) -> QueuedCommand {
    QueuedCommand {
        head_id: "test-head".into(),
        identity: "dead".into(),
        client_request_id: "req-1".into(),
        at_seq: 0,
        kind: CommandKind::Slash { line: line.into() },
        enqueued_ms: 0,
    }
}

fn warnings(hub: &Hub, code: &str) -> Vec<String> {
    hub.retained()
        .iter()
        .filter_map(|e| match &e.event {
            SessionEvent::Warning {
                code: c, detail, ..
            } if c == code => Some(detail.clone()),
            _ => None,
        })
        .collect()
}

/// A provider on loopback: round one calls `write`, round two answers. Keeps every round's body.
struct Fake {
    url: String,
    bodies: Arc<Mutex<Vec<String>>>,
}

impl Fake {
    fn serve() -> Fake {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://127.0.0.1:{}/v1/chat/completions",
            listener.local_addr().unwrap().port()
        );
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&bodies);
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { continue };
                let (line, body) = support::stub::read_request(conn.try_clone().unwrap());
                // The provider's model listing is asked before the first round; it is no round.
                if !line.starts_with("POST") {
                    let _ = write_json(conn, "{\"data\":[]}");
                    continue;
                }
                let round = {
                    let mut b = seen.lock().unwrap_or_else(|e| e.into_inner());
                    b.push(body);
                    b.len()
                };
                let _ = write_sse(conn, round);
            }
        });
        Fake { url, bodies }
    }

    fn bodies(&self) -> Vec<String> {
        self.bodies
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

fn write_json(mut stream: std::net::TcpStream, body: &str) -> std::io::Result<()> {
    use std::io::Write;
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

fn write_sse(mut stream: std::net::TcpStream, round: usize) -> std::io::Result<()> {
    use std::io::Write;
    let mut body = String::new();
    let frame = |v: serde_json::Value| format!("data: {v}\n\n");
    if round == 1 {
        let arguments = serde_json::json!({ "path": "out.txt", "content": "x" }).to_string();
        body.push_str(&frame(
            serde_json::json!({"choices":[{"delta":{"tool_calls":[{
                "index": 0,
                "id": "call_command_turn_1",
                "function": { "name": "write", "arguments": arguments },
            }]}}]}),
        ));
        body.push_str(&frame(
            serde_json::json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
        ));
    } else {
        body.push_str(&frame(
            serde_json::json!({"choices":[{"delta":{"content":"tidied"}}]}),
        ));
        body.push_str(&frame(
            serde_json::json!({"choices":[{"delta":{},"finish_reason":"stop"}]}),
        ));
    }
    body.push_str(&frame(serde_json::json!({"choices":[],"usage":{
        "prompt_tokens": 50,
        "completion_tokens": 9,
    }})));
    body.push_str("data: [DONE]\n\n");
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}
