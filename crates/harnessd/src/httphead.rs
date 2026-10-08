//! **A head that speaks HTTP**, so a model behind harnessd can be used the way a
//! bare llama-server is.
//!
//! # Why this exists
//!
//! The gatekeeper on this box is a llama-server: the oracle posts a brief to
//! `/v1/chat/completions` and reads one verdict back. That works, and it means
//! the guard has **no base prompt of its own** — every call has to carry
//! everything it needs, because nothing else is holding any context for it.
//!
//! The operator, 2026-09-20: *"I want to be able to start harnessd against a
//! model with my custom base prompt. Think of our oracle qwen. right now it is a
//! bare llama endpoint. I want it to be a harnessd and another simple http
//! head."*
//!
//! So: `--http ADDR` puts an OpenAI-shaped endpoint in front of a harnessd whose
//! `--system` is whatever the operator wrote. The oracle's client does not
//! change — same path, same request shape, same reply shape — and the guard
//! gains a base prompt that the server has already prefilled.
//!
//! # What it is not
//!
//! **Stateless.** Every request opens a scratch transcript under the daemon's
//! stable prefix, appends the request's messages, runs one turn and throws the
//! transcript away. That is the right shape for a guard — each brief is judged
//! on its own, and a verdict must not depend on the one before it — and it is
//! the shape `compaction::summarise_one` already uses for the same reason.
//!
//! Nothing here is seated: no tools, no store, no gate. A request cannot make
//! this daemon run a command, because there is no command to run. The turn is a
//! prompt and its answer.
//!
//! # Concurrency is the server's, not this head's
//!
//! llama.cpp's slots ARE its batching unit: N slots means N sequences decoded in
//! the same batch, each with its own KV cache. This head first served one
//! request at a time, on the reasoning that concurrent turns would "queue at the
//! server and thrash the prefix cache" — which has the relationship backwards,
//! and the operator said so in four words: *"four slots means batching"*. The
//! caches are per slot and do not evict one another, so four callers together
//! are cheaper than four callers in a row.
//!
//! So: a thread per connection, bounded by what the endpoint says it can decode
//! at once (`/props`'s `total_slots`), because past that the requests queue
//! inside the server where this head can neither see them nor time them out.
//! A server that does not say gets [`DEFAULT_PERMITS`], which is a bound rather
//! than a guess about hardware.
//!
//! # The prefix is the point
//!
//! `engine.open` renders the prefix once per request, and the SERVER keeps its
//! KV cache across requests because every request begins with the same bytes.
//! That is the thing a bare endpoint cannot do for a base prompt of any size:
//! the guard's instructions are prefilled once and reused, and only the brief is
//! new.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};

use letibot_dialect::StablePrefix;
use letibot_transcript::{SystemOrigin, TranscriptItem, UserPart};
use letibot_turn::TurnEngine;

use crate::config::Config;
use crate::harness::{Parts, engine_for};

/// How much of a request body this will read. A brief is a few kilobytes; a
/// megabyte is somebody pointing the wrong thing at this port, and reading it
/// whole to then refuse it is how a listener becomes a memory bug.
const MAX_BODY: usize = 1 << 20;

/// In-flight requests when the endpoint does not report its slot count. One is
/// wrong (it wastes a batching server) and a large number is wrong (it queues
/// inside a server that cannot say how deep its queue is); four is llama.cpp's
/// own default `-np`.
const DEFAULT_PERMITS: usize = 4;

/// A counting semaphore. `std` has none, and a channel of permits would make a
/// dropped handler leak one — a guard that returns its permit on the way out,
/// panic included, cannot.
struct Permits {
    free: std::sync::Mutex<usize>,
    woken: std::sync::Condvar,
}

impl Permits {
    fn new(n: usize) -> Self {
        Permits {
            free: std::sync::Mutex::new(n.max(1)),
            woken: std::sync::Condvar::new(),
        }
    }

    fn take(self: &std::sync::Arc<Self>) -> Permit {
        let mut free = self.free.lock().unwrap_or_else(|e| e.into_inner());
        while *free == 0 {
            free = self.woken.wait(free).unwrap_or_else(|e| e.into_inner());
        }
        *free -= 1;
        Permit(std::sync::Arc::clone(self))
    }
}

struct Permit(std::sync::Arc<Permits>);

impl Drop for Permit {
    fn drop(&mut self) {
        *self.0.free.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        self.0.woken.notify_one();
    }
}

/// One message as the OpenAI shape carries it.
#[derive(Debug)]
struct Msg {
    role: String,
    content: String,
}

/// Run the listener until the process ends.
pub fn serve(listener: TcpListener, parts: Parts, cfg: Config) {
    let prefix = StablePrefix {
        system: cfg.system.clone(),
        // **No tools.** This head cannot call any, so announcing them would be a
        // prompt that lies about what the model can do — and tool schemas are
        // the bulk of a prefix, which is what this exists to keep small and warm.
        tools_json: Vec::new(),
    };
    // What the endpoint says it decodes at once. Asked here, once, rather than
    // assumed: a metered provider reports nothing and a bare llama-server
    // reports its `-np`.
    let reported = letibot_turn::serving::served_slots(&cfg.endpoint);
    let n = reported.unwrap_or(DEFAULT_PERMITS);
    let permits = std::sync::Arc::new(Permits::new(n));
    eprintln!(
        "  http head on {} — POST /v1/chat/completions, model `{}`, {} byte(s) of system prompt, \
         {n} at a time ({})",
        listener
            .local_addr()
            .map(|a| a.to_string())
            .unwrap_or_else(|_| "?".into()),
        cfg.model,
        cfg.system.len(),
        match reported {
            Some(_) => "the endpoint's slot count",
            None => "the endpoint did not say; this build's default",
        }
    );
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        // Taken on the ACCEPT thread, so a caller past the bound waits in the
        // kernel's backlog rather than in a thread of its own. Threads that exist
        // to block are the shape this avoids.
        let permit = permits.take();
        let (parts, cfg, prefix) = (parts.clone(), cfg.clone(), prefix.clone());
        let spawned = std::thread::Builder::new()
            .name("http-turn".into())
            .spawn(move || {
                // One engine per request. It borrows the vocabulary out of this
                // thread's own `Parts` clone — an `Arc` bump, not a second load —
                // which is what lets the turns actually run side by side.
                let mut engine = match engine_for(&parts, &cfg) {
                    Ok(e) => e,
                    Err(e) => {
                        let _ = respond(&mut stream, 502, &err_json(&format!("engine: {e}")));
                        return;
                    }
                };
                if let Err(e) = handle(&mut stream, &mut engine, &prefix)
                    && e.kind() != std::io::ErrorKind::BrokenPipe
                {
                    eprintln!("  http head: {e}");
                }
                drop(permit);
            });
        if spawned.is_err() {
            eprintln!("  http head: could not spawn a thread for a request");
        }
    }
}

fn handle(
    stream: &mut TcpStream,
    engine: &mut TurnEngine,
    prefix: &StablePrefix,
) -> std::io::Result<()> {
    let Some((method, path, body)) = read_request(stream)? else {
        return respond(stream, 400, &err_json("the request line could not be read"));
    };
    if method != "POST" || !path.starts_with("/v1/chat/completions") {
        return respond(
            stream,
            404,
            &err_json(&format!(
                "this head serves POST /v1/chat/completions and nothing else; got {method} {path}"
            )),
        );
    }
    let msgs = match parse_messages(&body) {
        Ok(m) if !m.is_empty() => m,
        Ok(_) => return respond(stream, 400, &err_json("`messages` is empty")),
        Err(e) => return respond(stream, 400, &err_json(&e)),
    };

    match run_once(engine, prefix, &msgs) {
        Ok(text) => respond(stream, 200, &completion_json(&text)),
        // The turn's own words. A guard that gets `{"error": ...}` back reads it
        // as an unparseable verdict and answers `Unsure`, which is the honest
        // outcome of "the model did not answer" and is what `Oracle::ask`
        // already does with a body it cannot read.
        Err(e) => respond(stream, 502, &err_json(&e)),
    }
}

/// One turn on a scratch transcript, discarded afterwards.
fn run_once(
    engine: &mut TurnEngine,
    prefix: &StablePrefix,
    msgs: &[Msg],
) -> Result<String, String> {
    let items: Vec<TranscriptItem> = msgs
        .iter()
        .map(|m| match m.role.as_str() {
            // A `system` message from the CLIENT is not this head's base prompt —
            // that is `--system` and it is already in the prefix. It lands as an
            // update inside the conversation, which is what it is: something the
            // caller said, after the daemon's own instructions.
            "system" => TranscriptItem::System {
                text: m.content.clone(),
                origin: SystemOrigin::Update,
            },
            "assistant" => TranscriptItem::Assistant {
                text: m.content.clone(),
                tool_calls: Vec::new(),
                truncated: false,
            },
            _ => TranscriptItem::User {
                // The HTTP client's `user` role is whoever is driving that client, so it is
                // the operator's; this head injects nothing of its own into a session.
                speaker: letibot_transcript::Speaker::Operator,
                parts: vec![UserPart::Text {
                    text: m.content.clone(),
                }],
            },
        })
        .collect();

    let id = format!("http-{}", letibot_sessionlog::registry::now_ms());
    let mut scratch = engine
        .open(&id, prefix)
        .map_err(|e| format!("opening a scratch transcript: {e}"))?;
    let mut quiet = letibot_turn::NullSink;
    scratch
        .append_items(engine, &items, &mut quiet)
        .map_err(|e| format!("appending the request: {e}"))?;
    let ok = engine
        .run_turn(&mut scratch, &mut quiet)
        .map_err(|e| format!("the turn failed: {e}"))?;
    // The visible text, the same way a summary turn reads its own: the
    // assistant rows concatenated, reasoning left out. `harvest` is that rule in
    // one place, so this head and compaction cannot disagree about what a turn
    // said.
    Ok(letibot_turn::compaction::harvest(&ok.items).summary)
}

/// `(method, path, body)`.
fn read_request(stream: &mut TcpStream) -> std::io::Result<Option<(String, String, String)>> {
    let mut r = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    if r.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    let mut parts = line.split_whitespace();
    let (Some(method), Some(path)) = (parts.next(), parts.next()) else {
        return Ok(None);
    };
    let (method, path) = (method.to_string(), path.to_string());

    let mut len = 0usize;
    loop {
        let mut h = String::new();
        if r.read_line(&mut h)? == 0 {
            break;
        }
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':')
            && k.eq_ignore_ascii_case("content-length")
        {
            len = v.trim().parse().unwrap_or(0);
        }
    }
    if len > MAX_BODY {
        return Ok(Some((method, path, String::new())));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    Ok(Some((
        method,
        path,
        String::from_utf8_lossy(&body).into_owned(),
    )))
}

fn parse_messages(body: &str) -> Result<Vec<Msg>, String> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("the body is not JSON: {e}"))?;
    let Some(arr) = v.get("messages").and_then(|m| m.as_array()) else {
        return Err("no `messages` array in the body".into());
    };
    Ok(arr
        .iter()
        .filter_map(|m| {
            Some(Msg {
                role: m.get("role")?.as_str()?.to_string(),
                // A content array (the multipart shape) is flattened to its text
                // parts; anything else is skipped rather than rendered as JSON.
                content: match m.get("content")? {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Array(ps) => ps
                        .iter()
                        .filter_map(|p| p.get("text")?.as_str())
                        .collect::<Vec<_>>()
                        .join(""),
                    _ => return None,
                },
            })
        })
        .collect())
}

fn completion_json(text: &str) -> String {
    serde_json::json!({
        "object": "chat.completion",
        "choices": [{
            "index": 0,
            "finish_reason": "stop",
            "message": { "role": "assistant", "content": text },
        }],
    })
    .to_string()
}

fn err_json(why: &str) -> String {
    serde_json::json!({ "error": { "message": why } }).to_string()
}

fn respond(stream: &mut TcpStream, status: u16, body: &str) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Bad Gateway",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The bound is a bound, and a permit comes back however the handler ends.**
    ///
    /// A channel of permits leaks one when a handler panics; a guard cannot.
    #[test]
    fn permits_bound_the_in_flight_requests_and_return_themselves() {
        let p = std::sync::Arc::new(Permits::new(2));
        let a = p.take();
        let b = p.take();
        assert_eq!(*p.free.lock().unwrap(), 0, "both permits are out");

        // A third caller blocks until one comes back.
        let p2 = std::sync::Arc::clone(&p);
        let waiter = std::thread::spawn(move || {
            let _c = p2.take();
            true
        });
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(!waiter.is_finished(), "the bound did not hold");

        drop(a);
        assert!(
            waiter.join().expect("the waiter"),
            "a returned permit woke it"
        );
        drop(b);
        assert_eq!(*p.free.lock().unwrap(), 2, "every permit came back");
    }

    /// A panicking handler must not eat a permit for the life of the process.
    #[test]
    fn a_panicking_handler_returns_its_permit() {
        let p = std::sync::Arc::new(Permits::new(1));
        let p2 = std::sync::Arc::clone(&p);
        let _ = std::thread::spawn(move || {
            let _permit = p2.take();
            panic!("the turn blew up");
        })
        .join();
        assert_eq!(
            *p.free.lock().unwrap_or_else(|e| e.into_inner()),
            1,
            "the permit was lost with the thread"
        );
    }

    #[test]
    fn a_body_becomes_messages_in_order_and_a_content_array_is_flattened() {
        let msgs = parse_messages(
            r#"{"model":"guard","messages":[
                 {"role":"system","content":"you judge"},
                 {"role":"user","content":[{"type":"text","text":"did they "},
                                           {"type":"text","text":"ask for this?"}]},
                 {"role":"assistant","content":"ALLOW"}]}"#,
        )
        .expect("a body");
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].role, "system");
        assert_eq!(msgs[1].content, "did they ask for this?");
        assert_eq!(msgs[2].role, "assistant");
    }

    #[test]
    fn a_body_that_is_not_a_request_is_refused_by_name() {
        assert!(parse_messages("not json").unwrap_err().contains("not JSON"));
        assert!(
            parse_messages(r#"{"prompt":"hi"}"#)
                .unwrap_err()
                .contains("no `messages`")
        );
    }

    /// The reply is the shape `Oracle::ask` already reads: it walks
    /// `choices[0].message.content` and treats anything it cannot walk as
    /// `Unsure`. A head that answered a different shape would make every verdict
    /// unsure without saying so.
    #[test]
    fn the_reply_is_the_shape_the_oracle_client_reads() {
        let body = completion_json("DENY");
        let v: serde_json::Value = serde_json::from_str(&body).expect("json");
        assert_eq!(
            v.get("choices")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("message"))
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str()),
            Some("DENY")
        );
    }
}
