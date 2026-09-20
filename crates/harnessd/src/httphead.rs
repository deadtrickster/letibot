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

/// One message as the OpenAI shape carries it.
#[derive(Debug)]
struct Msg {
    role: String,
    content: String,
}

/// Run the listener until the process ends.
///
/// Sequential on purpose: one connection at a time, one turn at a time. The
/// endpoint this fronts is a single model with a handful of slots, and a head
/// that accepted twenty concurrent turns would queue them there instead — with
/// the prefix cache thrashing between them, which is the one thing this is for.
pub fn serve(listener: TcpListener, parts: Parts, cfg: Config) {
    let prefix = StablePrefix {
        system: cfg.system.clone(),
        // **No tools.** This head cannot call any, so announcing them would be a
        // prompt that lies about what the model can do — and tool schemas are
        // the bulk of a prefix, which is what this exists to keep small and warm.
        tools_json: Vec::new(),
    };
    let mut engine = match engine_for(&parts, &cfg) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("  http head: cannot build an engine: {e}");
            return;
        }
    };
    eprintln!(
        "  http head on {} — POST /v1/chat/completions, model `{}`, {} byte(s) of system prompt",
        listener
            .local_addr()
            .map(|a| a.to_string())
            .unwrap_or_else(|_| "?".into()),
        cfg.model,
        cfg.system.len()
    );
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        if let Err(e) = handle(&mut stream, &mut engine, &prefix) {
            // A broken pipe is a client that went away mid-answer and is not an
            // event; anything else is worth a line, because this listener has no
            // other way to say it.
            if e.kind() != std::io::ErrorKind::BrokenPipe {
                eprintln!("  http head: {e}");
            }
        }
    }
}

fn handle(
    stream: &mut TcpStream,
    engine: &mut TurnEngine<'_>,
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
    engine: &mut TurnEngine<'_>,
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
