//! The streamable HTTP transport against a minimal in-process server: JSON and SSE answers, the
//! session id carried from `initialize` onto every later request, a session the server ends, a
//! tool list that changes, and an https URL that is spoken to in TLS.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::time::Duration;

use letibot_mcp_client::{Catalog, ServerSpec, Transport};
use letibot_tools::builtins::external::mcp::McpCatalog;
use serde_json::{Value, json};

fn reply(content_type: &str, extra: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\n{extra}Content-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

fn serve(listener: TcpListener) {
    let mut minted = 0;
    let mut live: BTreeSet<String> = BTreeSet::new();
    let mut grown = false;
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { return };
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut length = 0usize;
        let mut session = None;
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        loop {
            line.clear();
            reader.read_line(&mut line).unwrap();
            let l = line.trim_end();
            if l.is_empty() {
                break;
            }
            let (k, v) = l.split_once(':').unwrap();
            match k.to_ascii_lowercase().as_str() {
                "content-length" => length = v.trim().parse().unwrap(),
                "mcp-session-id" => session = Some(v.trim().to_string()),
                _ => {}
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        let msg: Value = serde_json::from_slice(&body).unwrap();
        let id = msg.get("id").cloned();
        let method = msg["method"].as_str().unwrap_or("");
        let tool = msg.pointer("/params/name").and_then(Value::as_str);
        if let Some(s) = &session
            && !live.contains(s)
        {
            let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
            continue;
        }
        let answer = match (method, id) {
            (_, None) => "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\r\n".to_string(),
            ("initialize", Some(id)) => {
                minted += 1;
                let s = format!("s-{minted}");
                live.insert(s.clone());
                let b = json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":"2025-06-18","capabilities":{}}}).to_string();
                reply("application/json", &format!("Mcp-Session-Id: {s}\r\n"), &b)
            }
            ("tools/list", Some(id)) => {
                let note = json!({"jsonrpc":"2.0","method":"notifications/progress","params":{}});
                let mut tools = vec![
                    json!({"name":"whoami","description":"Say the session.","inputSchema":{"type":"object"}}),
                ];
                if grown {
                    tools.push(json!({"name":"extra","inputSchema":{"type":"object"}}));
                }
                let b = json!({"jsonrpc":"2.0","id":id,"result":{"tools":tools}});
                reply(
                    "text/event-stream",
                    "",
                    &format!("data: {note}\n\ndata: {b}\n\n"),
                )
            }
            ("tools/call", Some(id)) if tool == Some("grow") => {
                grown = true;
                let note = json!({"jsonrpc":"2.0","method":"notifications/tools/list_changed"});
                let b = json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":"grown"}]}});
                // The last event ends without its blank line.
                reply(
                    "text/event-stream",
                    "",
                    &format!("data: {note}\n\ndata: {b}\n"),
                )
            }
            ("tools/call", Some(id)) if tool == Some("expire") => {
                live.clear();
                let b = json!({"jsonrpc":"2.0","id":id,"result":{"content":[]}}).to_string();
                reply("application/json", "", &b)
            }
            (_, Some(id)) => {
                let text = session.clone().unwrap_or_else(|| "no session".into());
                let b = json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":text}]}}).to_string();
                reply("application/json", "", &b)
            }
        };
        let _ = stream.write_all(answer.as_bytes());
    }
}

fn web() -> Catalog {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || serve(listener));
    let cat = Catalog::start(&[ServerSpec {
        name: "web".into(),
        transport: Transport::Http {
            url: format!("http://127.0.0.1:{port}/mcp"),
            headers: BTreeMap::new(),
        },
        timeout: Duration::from_secs(10),
    }]);
    assert!(cat.failures().is_empty(), "{:?}", cat.failures());
    cat
}

#[test]
fn an_http_server_is_listed_and_called_with_its_session_id() {
    let cat = web();
    assert_eq!(cat.servers()[0].tools[0].name, "whoami");
    let r = cat.call("web", "whoami", &json!({})).unwrap();
    assert_eq!(r.text, "s-1");
}

#[test]
fn a_session_the_server_ended_is_initialized_again() {
    let cat = web();
    cat.call("web", "expire", &json!({})).unwrap();
    let e = cat.call("web", "whoami", &json!({})).unwrap_err();
    let said = format!("{e:?}");
    assert!(said.contains("ended session s-1 (404)"), "{said}");
    let r = cat.call("web", "whoami", &json!({})).unwrap();
    assert_eq!(r.text, "s-2", "a new initialize, a new session");
}

#[test]
fn a_list_changed_event_on_the_stream_lists_the_tools_again() {
    let cat = web();
    assert_eq!(cat.servers()[0].tools.len(), 1);
    let r = cat.call("web", "grow", &json!({})).unwrap();
    assert_eq!(r.text, "grown");
    let tools: Vec<String> = cat.servers()[0]
        .tools
        .iter()
        .map(|t| t.name.clone())
        .collect();
    assert_eq!(tools, ["whoami", "extra"]);
}

#[test]
fn an_https_url_is_spoken_to_in_tls() {
    // A plain listener: the first byte the client sends is a TLS handshake record (0x16).
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let mut first = [0u8; 1];
        s.read_exact(&mut first).unwrap();
        tx.send(first[0]).unwrap();
    });
    let cat = Catalog::start(&[ServerSpec {
        name: "tls".into(),
        transport: Transport::Http {
            url: format!("https://127.0.0.1:{port}/mcp"),
            headers: BTreeMap::new(),
        },
        timeout: Duration::from_secs(3),
    }]);
    assert_eq!(cat.failures().len(), 1, "no TLS server answered");
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        0x16,
        "a ClientHello"
    );
}

#[test]
fn a_401_names_the_header_to_set() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in listener.incoming() {
            let mut s = s.unwrap();
            let mut buf = [0u8; 4096];
            let _ = s.read(&mut buf);
            let _ = s.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n");
        }
    });
    let cat = Catalog::start(&[ServerSpec {
        name: "auth".into(),
        transport: Transport::Http {
            url: format!("http://127.0.0.1:{port}/mcp"),
            headers: BTreeMap::new(),
        },
        timeout: Duration::from_secs(3),
    }]);
    let why = &cat.failures()[0].why;
    assert!(why.contains("HTTP 401"), "{why}");
    assert!(why.contains("Authorization"), "{why}");
}
