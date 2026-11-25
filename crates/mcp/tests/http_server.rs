//! The streamable HTTP transport against a minimal in-process server: JSON and SSE answers, the
//! session id carried from `initialize` onto every later request, a session the server ends, a
//! tool list that changes, an https URL that is spoken to in TLS, and the GET stream a server sends
//! its own notices and requests on.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
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
/// live, whether a `grow` call added the second tool, how many sockets carried a POST, how often
/// it was asked for a GET stream, the streams it is holding, and the JSON-RPC answers the client
/// POSTed back to it.
#[derive(Default)]
struct State {
    minted: usize,
    live: BTreeSet<String>,
    grown: bool,
    connections: usize,
    gets: usize,
    streams: Vec<mpsc::Sender<String>>,
    replies: Vec<Value>,
}

/// A thread per connection, which is what a real server does: the socket a client pooled for its
/// next message must not wait behind one nobody will write to again. `stream_ok` is whether this
/// server offers the GET stream; one that does not answers it 405, as the spec allows.
fn serve(listener: TcpListener, stream_ok: bool) -> Arc<Mutex<State>> {
    let state = Arc::new(Mutex::new(State::default()));
    let accepted = Arc::clone(&state);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            let state = Arc::clone(&accepted);
            std::thread::spawn(move || connection(stream, &state, stream_ok));
        }
    });
    state
}

struct Request {
    method: String,
    body: Value,
    session: Option<String>,
}

/// One connection, kept open for as many messages as the client sends on it. An answer that says
/// nothing about closing promises exactly that, and the client's pool keeps the socket for its
/// next message: a server that closed after one answer would meet a later write with RST.
fn connection(mut stream: TcpStream, state: &Mutex<State>, stream_ok: bool) {
    let Ok(clone) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(clone);
    // A GET is the stream's own socket and is not counted: the pooling test below is about POSTs.
    let mut counted = false;
    while let Some(req) = next_request(&mut reader) {
        if req.method == "GET" {
            state.lock().unwrap_or_else(|e| e.into_inner()).gets += 1;
            if !stream_ok {
                let _ = stream
                    .write_all(b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\n\r\n");
                continue;
            }
            serve_stream(stream, state);
            return;
        }
        if !counted {
            state.lock().unwrap_or_else(|e| e.into_inner()).connections += 1;
            counted = true;
        }
        let answer = answer(&req.body, req.session.as_deref(), state);
        if stream.write_all(answer.as_bytes()).is_err() {
            return;
        }
    }
}

/// One request off the socket: its method, its body (none for a GET) and the `Mcp-Session-Id` it
/// carried. `None` is the client closing the connection, or a request this server will not read.
fn next_request(reader: &mut impl BufRead) -> Option<Request> {
    let mut line = String::new();
    if reader.read_line(&mut line).ok()? == 0 {
        return None;
    }
    let method = line.split(' ').next()?.to_string();
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
    let body = if length == 0 {
        Value::Null
    } else {
        reader.read_exact(&mut body).ok()?;
        serde_json::from_slice(&body).ok()?
    };
    Some(Request {
        method,
        body,
        session,
    })
}

/// The GET stream: chunked, because a stream has no length, and open until the test stops it or the
/// client goes away. Each `data:` event is written as one chunk as the test pushes it.
fn serve_stream(mut stream: TcpStream, state: &Mutex<State>) {
    let _ = stream.set_write_timeout(Some(Duration::from_secs(20)));
    if stream
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: \
              chunked\r\n\r\n",
        )
        .is_err()
    {
        return;
    }
    let (tx, rx) = mpsc::channel::<String>();
    state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .streams
        .push(tx);
    while let Ok(text) = rx.recv() {
        let chunk = format!("{:x}\r\n{text}\r\n", text.len());
        if stream.write_all(chunk.as_bytes()).is_err() {
            return;
        }
    }
}

/// Push one SSE event to every stream this server holds. The senders of streams the client stopped
/// reading are dropped, so `false` is the client having given the stream up.
fn push(state: &Mutex<State>, text: &str) -> bool {
    let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
    st.streams.retain(|tx| tx.send(text.to_string()).is_ok());
    !st.streams.is_empty()
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
    // A POST with no method is the client's answer to one of this server's own requests.
    if msg.get("method").is_none() && id.is_some() {
        st.replies.push(msg.clone());
        return "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\r\n".to_string();
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
    boot(false)
}

fn boot(stream_ok: bool) -> (Catalog, Arc<Mutex<State>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let state = serve(listener, stream_ok);
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

/// A server that does offer the GET stream, once the client has opened it.
fn streamed() -> (Catalog, Arc<Mutex<State>>) {
    let (cat, state) = boot(true);
    wait_until("the GET stream to be open", || {
        !state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .streams
            .is_empty()
    });
    (cat, state)
}

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while !cond() {
        assert!(
            std::time::Instant::now() < deadline,
            "never happened: {what}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn tools(cat: &Catalog) -> Vec<String> {
    cat.servers()[0]
        .tools
        .iter()
        .map(|t| t.name.clone())
        .collect()
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
fn a_notice_on_the_get_stream_lists_the_tools_again() {
    let (cat, state) = streamed();
    assert_eq!(tools(&cat), ["whoami"]);
    state.lock().unwrap_or_else(|e| e.into_inner()).grown = true;
    let note = json!({"jsonrpc":"2.0","method":"notifications/tools/list_changed"});
    push(&state, &format!("data: {note}\n\n"));
    wait_until("the tools to be listed again", || {
        tools(&cat).contains(&"extra".to_string())
    });
}

/// A server's own request has no channel back on the stream, so the client POSTs the answer: a
/// `ping` left unanswered would hold up whatever the server was doing.
#[test]
fn a_ping_on_the_get_stream_is_answered_by_a_post() {
    let (cat, state) = streamed();
    let ping = json!({"jsonrpc":"2.0","id":"srv-1","method":"ping"});
    push(&state, &format!("data: {ping}\n\n"));
    wait_until("the ping to be answered", || {
        replied(&state, "srv-1").is_some()
    });
    let answer = replied(&state, "srv-1").unwrap();
    assert_eq!(answer["result"], json!({}), "{answer}");
    assert_eq!(cat.call("web", "whoami", &json!({})).unwrap().text, "s-1");
}

/// A method this client cannot serve is refused by code rather than not at all, which is what
/// leaves a server waiting for ever on a client that only reads.
#[test]
fn a_request_this_client_cannot_serve_is_answered_with_an_error() {
    let (_, state) = streamed();
    let ask = json!({"jsonrpc":"2.0","id":"srv-2","method":"sampling/createMessage",
                     "params":{}});
    push(&state, &format!("data: {ask}\n\n"));
    wait_until("the refusal to be sent", || {
        replied(&state, "srv-2").is_some()
    });
    let answer = replied(&state, "srv-2").unwrap();
    assert_eq!(answer["error"]["code"], json!(-32601), "{answer}");
}

/// A server that offers no GET stream (405) is not a broken server, and the client asks once rather
/// than on every message.
#[test]
fn a_server_without_a_get_stream_serves_and_is_asked_once() {
    let (cat, state) = boot(false);
    wait_until("the GET to be asked for", || {
        state.lock().unwrap_or_else(|e| e.into_inner()).gets >= 1
    });
    for _ in 0..3 {
        assert_eq!(cat.call("web", "whoami", &json!({})).unwrap().text, "s-1");
    }
    let gets = state.lock().unwrap_or_else(|e| e.into_inner()).gets;
    assert_eq!(gets, 1, "asked once, never on a message");
}

/// The new stream keeps I65's caps: an event past 8 MiB ends it rather than growing the daemon's
/// memory, and the server's calls go on without it.
#[test]
fn an_event_past_the_cap_on_the_get_stream_ends_it_and_calls_go_on() {
    let (cat, state) = streamed();
    let line = format!("data: {}", "y".repeat(100_000));
    let huge = format!("{}\n\n", vec![line; 90].join("\n"));
    push(&state, &huge);
    // The reader stops at the cap and drops the stream, so the next event reaches nobody.
    wait_until("the client to give the stream up", || {
        !push(&state, "data: {}\n\n")
    });
    assert_eq!(cat.call("web", "whoami", &json!({})).unwrap().text, "s-1");
}

/// A line with no end on the stream is dropped by the same cap a POST's stream has.
#[test]
fn an_endless_line_on_the_get_stream_ends_it() {
    let (cat, state) = streamed();
    let huge = format!("data: {}", "z".repeat(9 << 20));
    push(&state, &huge);
    wait_until("the client to give the stream up", || {
        !push(&state, "data: {}\n\n")
    });
    assert_eq!(cat.call("web", "whoami", &json!({})).unwrap().text, "s-1");
}

fn replied(state: &Mutex<State>, id: &str) -> Option<Value> {
    let st = state.lock().unwrap_or_else(|e| e.into_inner());
    st.replies.iter().find(|r| r["id"] == json!(id)).cloned()
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
