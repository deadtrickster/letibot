//! The streamable HTTP transport against a minimal in-process server: JSON and SSE answers, the
//! session id carried from `initialize` onto every later request, a session the server ends, a
//! tool list that changes, and an https URL that is spoken to in TLS.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
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

/// What the fake server remembers across connections: the sessions it minted, the ones still
/// live, whether a `grow` call added the second tool, and how many sockets it has accepted.
#[derive(Default)]
struct State {
    minted: usize,
    live: BTreeSet<String>,
    grown: bool,
    connections: usize,
}

/// A thread per connection, which is what a real server does: the socket a client pooled for its
/// next message must not wait behind one nobody will write to again.
fn serve(listener: TcpListener) -> Arc<Mutex<State>> {
    let state = Arc::new(Mutex::new(State::default()));
    let accepted = Arc::clone(&state);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            accepted
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .connections += 1;
            let state = Arc::clone(&accepted);
            std::thread::spawn(move || connection(stream, &state));
        }
    });
    state
}

/// One connection, kept open for as many messages as the client sends on it. An answer that says
/// nothing about closing promises exactly that, and the client's pool keeps the socket for its
/// next message: a server that closed after one answer would meet a later write with RST.
fn connection(mut stream: TcpStream, state: &Mutex<State>) {
    let Ok(clone) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(clone);
    while let Some((msg, session)) = next_message(&mut reader) {
        let answer = answer(&msg, session.as_deref(), state);
        if stream.write_all(answer.as_bytes()).is_err() {
            return;
        }
    }
}

/// One message off the socket: its body, and the `Mcp-Session-Id` it carried. `None` is the
/// client closing the connection, or a request this server will not read.
fn next_message(reader: &mut impl BufRead) -> Option<(Value, Option<String>)> {
    let mut line = String::new();
    if reader.read_line(&mut line).ok()? == 0 {
        return None;
    }
    let mut length = 0usize;
    let mut session = None;
    loop {
        line.clear();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let l = line.trim_end();
        if l.is_empty() {
            break;
        }
        let (k, v) = l.split_once(':')?;
        match k.to_ascii_lowercase().as_str() {
            "content-length" => length = v.trim().parse().ok()?,
            "mcp-session-id" => session = Some(v.trim().to_string()),
            _ => {}
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).ok()?;
    Some((serde_json::from_slice(&body).ok()?, session))
}

fn answer(msg: &Value, session: Option<&str>, state: &Mutex<State>) -> String {
    let id = msg.get("id").cloned();
    let method = msg["method"].as_str().unwrap_or("");
    let tool = msg.pointer("/params/name").and_then(Value::as_str);
    let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = session
        && !st.live.contains(s)
    {
        return "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_string();
    }
    match (method, id) {
        (_, None) => "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\r\n".to_string(),
        ("initialize", Some(id)) => {
            st.minted += 1;
            let s = format!("s-{}", st.minted);
            st.live.insert(s.clone());
            let b = json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":"2025-06-18","capabilities":{}}}).to_string();
            reply("application/json", &format!("Mcp-Session-Id: {s}\r\n"), &b)
        }
        ("tools/list", Some(id)) => {
            let note = json!({"jsonrpc":"2.0","method":"notifications/progress","params":{}});
            let mut tools = vec![
                json!({"name":"whoami","description":"Say the session.","inputSchema":{"type":"object"}}),
            ];
            if st.grown {
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
            st.grown = true;
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
            st.live.clear();
            let b = json!({"jsonrpc":"2.0","id":id,"result":{"content":[]}}).to_string();
            reply("application/json", "", &b)
        }
        (_, Some(id)) => {
            let text = session.unwrap_or("no session");
            let b =
                json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":text}]}})
                    .to_string();
            reply("application/json", "", &b)
        }
    }
}

fn web() -> (Catalog, Arc<Mutex<State>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let state = serve(listener);
    let cat = Catalog::start(&[ServerSpec {
        name: "web".into(),
        transport: Transport::Http {
            url: format!("http://127.0.0.1:{port}/mcp"),
            headers: BTreeMap::new(),
        },
        timeout: Duration::from_secs(10),
    }]);
    assert!(cat.failures().is_empty(), "{:?}", cat.failures());
    (cat, state)
}

#[test]
fn an_http_server_is_listed_and_called_with_its_session_id() {
    let (cat, _) = web();
    assert_eq!(cat.servers()[0].tools[0].name, "whoami");
    let r = cat.call("web", "whoami", &json!({})).unwrap();
    assert_eq!(r.text, "s-1");
}

/// An HTTP/1.1 answer that does not say `Connection: close` leaves the socket usable, so the whole
/// handshake - `initialize`, its notification, `tools/list` - rides the one connection. This is the
/// test that was flaky: a server answering one message per socket and then closing made the
/// client's next message a write to a peer that had gone, which the kernel answers with RST.
#[test]
fn the_handshake_rides_the_connection_the_first_message_opened() {
    let (cat, state) = web();
    cat.call("web", "whoami", &json!({})).unwrap();
    let accepted = state.lock().unwrap_or_else(|e| e.into_inner()).connections;
    assert_eq!(accepted, 1, "one socket: the handshake and one call");
}

#[test]
fn a_session_the_server_ended_is_initialized_again() {
    let (cat, _) = web();
    cat.call("web", "expire", &json!({})).unwrap();
    let e = cat.call("web", "whoami", &json!({})).unwrap_err();
    let said = format!("{e:?}");
    assert!(said.contains("ended session s-1 (404)"), "{said}");
    let r = cat.call("web", "whoami", &json!({})).unwrap();
    assert_eq!(r.text, "s-2", "a new initialize, a new session");
}

#[test]
fn a_list_changed_event_on_the_stream_lists_the_tools_again() {
    let (cat, _) = web();
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
