//! The streamable HTTP transport against a minimal in-process server: JSON and SSE answers, and
//! the session id carried from `initialize` onto every later request.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::time::Duration;

use letibot_mcp::{Catalog, ServerSpec, Transport};
use letibot_tools::builtins::external::mcp::McpCatalog;
use serde_json::{Value, json};

fn serve(listener: TcpListener) {
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
        let reply = match (method, id) {
            (_, None) => "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\r\n".to_string(),
            ("initialize", Some(id)) => {
                let b = json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":"2025-06-18","capabilities":{}}}).to_string();
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nMcp-Session-Id: s-42\r\nContent-Length: {}\r\n\r\n{b}",
                    b.len()
                )
            }
            ("tools/list", Some(id)) => {
                let note = json!({"jsonrpc":"2.0","method":"notifications/progress","params":{}});
                let b = json!({"jsonrpc":"2.0","id":id,"result":{"tools":[{"name":"whoami","description":"Say the session.","inputSchema":{"type":"object"}}]}});
                let sse = format!("data: {note}\n\ndata: {b}\n\n");
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{sse}",
                    sse.len()
                )
            }
            (_, Some(id)) => {
                let text = session.clone().unwrap_or_else(|| "no session".into());
                let b = json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":text}]}}).to_string();
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{b}",
                    b.len()
                )
            }
        };
        let _ = stream.write_all(reply.as_bytes());
    }
}

#[test]
fn an_http_server_is_listed_and_called_with_its_session_id() {
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
    assert_eq!(cat.servers()[0].tools[0].name, "whoami");
    let r = cat.call("web", "whoami", &json!({})).unwrap();
    assert_eq!(r.text, "s-42");
}

#[test]
fn an_https_url_fails_to_start_by_name() {
    let cat = Catalog::start(&[ServerSpec {
        name: "tls".into(),
        transport: Transport::Http {
            url: "https://example.invalid/mcp".into(),
            headers: BTreeMap::new(),
        },
        timeout: Duration::from_secs(1),
    }]);
    assert!(
        cat.failures()[0].why.contains("TLS"),
        "{:?}",
        cat.failures()
    );
}
