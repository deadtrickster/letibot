//! A one-shot HTTP server that replays a fixed list of SSE frames.
//!
//! Three of the engine's decisions cannot be provoked against the real server: `length` needs an
//! `n_predict` cap §5.7 removed, repetition collapse needs a model that is collapsing, and a
//! prefix violation needs the harness to be broken. Every one is a decision about bytes the
//! engine received, so replaying the bytes is the same test with the model replaced by the case.
//!
//! The frames are in the server's own wire shape, including the empty `tokens` array on the
//! terminal frame and the fabricated id 0 on a progress frame.

// A fixture, not a library: nineteen test binaries include this file by path and each uses the
// part of it that binary's subject needs, so the rest is dead in that compilation and alive in
// another. Per-item allows would be nineteen copies of this sentence.
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::JoinHandle;

use letibot_turn::Endpoint;

/// A frame the canned server will send.
pub enum Frame {
    /// A progress frame — carries `tokens: [0]`, exactly as llama.cpp does.
    Progress { total: u64, processed: u64 },
    /// One generated token.
    Token { id: u32, text: &'static str },
    /// A token the server counted but sent no frame for: unpatched llama.cpp skips the frame
    /// when the generated text ends mid-character, so the counter advances by one with nothing
    /// on the wire. Kept because a server built from upstream is unpatched.
    Suppressed,
    /// The terminal frame. `tokens` is empty, as it is in stream mode.
    Final {
        stop_type: &'static str,
        n_decoded: u64,
        n_prompt: u64,
        cache_n: u64,
    },
}

impl Frame {
    fn to_json(&self, n_decoded_so_far: u64) -> String {
        match self {
            Frame::Progress { total, processed } => format!(
                r#"{{"index":0,"content":"","tokens":[0],"stop":false,"id_slot":-1,"tokens_predicted":0,"tokens_evaluated":{total},"prompt_progress":{{"total":{total},"cache":0,"processed":{processed},"time_ms":1}}}}"#
            ),
            Frame::Suppressed => String::new(),
            Frame::Token { id, text } => format!(
                r#"{{"index":0,"content":{},"tokens":[{id}],"stop":false,"id_slot":3,"tokens_predicted":{n_decoded_so_far},"tokens_evaluated":0}}"#,
                serde_json::to_string(text).unwrap()
            ),
            Frame::Final {
                stop_type,
                n_decoded,
                n_prompt,
                cache_n,
            } => format!(
                r#"{{"index":0,"content":"","tokens":[],"stop":true,"id_slot":3,"model":"canned","tokens_predicted":{n_decoded},"tokens_evaluated":{n_prompt},"tokens_cached":{n_prompt},"stop_type":"{stop_type}","stopping_word":"","truncated":false,"timings":{{"cache_n":{cache_n},"prompt_n":{n_prompt},"prompt_ms":1.0,"predicted_n":{n_decoded},"predicted_ms":1.0,"draft_n":0,"draft_n_accepted":0}}}}"#
            ),
        }
    }
}

/// A server that answers exactly `n` requests with `frames`, then stops.
pub struct Canned {
    pub endpoint: Endpoint,
    handle: Option<JoinHandle<()>>,
}

/// What the server answers ONE request with: the SSE frames of a turn, or an HTTP error status
/// carrying a body verbatim. The status half exists for the refusals a provider answers an
/// over-long prompt with, and the body is kept exactly as measured.
pub enum Reply {
    Frames(Vec<Frame>),
    Status { code: u16, body: String },
}

impl Canned {
    pub fn serve(frames: Vec<Frame>, requests: usize) -> Canned {
        Canned::serve_each(vec![frames], requests)
    }

    /// A different frame list per request, cycling if there are fewer lists than
    /// requests.
    pub fn serve_each(scripts: Vec<Vec<Frame>>, requests: usize) -> Canned {
        Canned::serve_replies(scripts.into_iter().map(Reply::Frames).collect(), requests)
    }

    /// A different REPLY per request — frames for the turns that answer, a
    /// status for the ones the provider refuses.
    pub fn serve_replies(scripts: Vec<Reply>, requests: usize) -> Canned {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            for i in 0..requests {
                let Ok((stream, _)) = listener.accept() else {
                    return;
                };
                let script = &scripts[i % scripts.len()];
                // A client that closes early — a guard trip, an urgent steering
                // message — makes these writes fail. That is the abort working, not
                // an error.
                let _ = answer(stream, script);
            }
        });
        Canned {
            endpoint: Endpoint::new(addr.ip().to_string(), addr.port()),
            handle: Some(handle),
        }
    }
}

impl Drop for Canned {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            // Detach rather than join: a test that provoked an abort has left an
            // unaccepted connection behind, and blocking here would hang the suite
            // on the very case it was written to check.
            drop(h);
        }
    }
}

fn answer(mut stream: TcpStream, reply: &Reply) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body)?;

    // A refusal is a whole response, not a frame list: a status line, the body with a length,
    // and nothing after it. Content-length framing, because there is exactly one body and the
    // client reads to the length and closes.
    let Reply::Status {
        code,
        body: payload,
    } = reply
    else {
        return answer_frames(stream, reply);
    };
    let reason = match code {
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        _ => "Status",
    };
    write!(
        stream,
        "HTTP/1.1 {code} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        payload.len()
    )?;
    stream.write_all(payload.as_bytes())?;
    stream.flush()
}

fn answer_frames(mut stream: TcpStream, reply: &Reply) -> std::io::Result<()> {
    let Reply::Frames(frames) = reply else {
        unreachable!("the caller routes status replies to `answer`");
    };
    stream.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
          Transfer-Encoding: chunked\r\n\r\n",
    )?;
    stream.write_all(&sse_body(frames))?;
    stream.flush()
}

/// The chunked SSE body a frame list becomes - one implementation of the wire shape and of the
/// decoded-token counter, so a caller writing its own response does not fork them.
pub fn sse_body(frames: &[Frame]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut decoded = 0u64;
    for frame in frames {
        if matches!(frame, Frame::Token { .. } | Frame::Suppressed) {
            decoded += 1;
        }
        // The counter moved and nothing goes on the wire. This `continue` is the
        // defect, reproduced rather than described.
        if matches!(frame, Frame::Suppressed) {
            continue;
        }
        let payload = format!("data: {}\n\n", frame.to_json(decoded));
        out.extend_from_slice(format!("{:x}\r\n", payload.len()).as_bytes());
        out.extend_from_slice(payload.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"0\r\n\r\n");
    out
}
