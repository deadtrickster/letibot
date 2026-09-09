//! A one-shot HTTP server that replays a fixed list of SSE frames.
//!
//! # Why the engine needs one
//!
//! Three of the engine's decisions cannot be provoked against the real server
//! without doing something the design forbids or the operator forbids:
//!
//! * **`finish_reason: length`** needs an `n_predict` cap — §5.7 deliberately
//!   removed that field from the request — or a 1.3 M-token prompt.
//! * **Repetition collapse** needs a model that is actually collapsing.
//! * **A prefix violation** needs the harness to be broken.
//!
//! Every one of those is a decision the engine makes about bytes it received, so
//! replaying the bytes is not a weaker test than a live one; it is the same test
//! with the model replaced by the case we care about. The live file covers what
//! only a real server can answer: that the frames look like this at all, that the
//! ids round-trip through a real vocabulary, and what the cache actually does.
//!
//! The frames are written in the server's own wire shape, including the empty
//! `tokens` array on the terminal frame and the fabricated id 0 on a progress
//! frame, because those are the two behaviours the accumulator exists to survive.

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

impl Canned {
    pub fn serve(frames: Vec<Frame>, requests: usize) -> Canned {
        Canned::serve_each(vec![frames], requests)
    }

    /// A different frame list per request, cycling if there are fewer lists than
    /// requests.
    pub fn serve_each(scripts: Vec<Vec<Frame>>, requests: usize) -> Canned {
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

fn answer(mut stream: TcpStream, frames: &[Frame]) -> std::io::Result<()> {
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

    stream.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
          Transfer-Encoding: chunked\r\n\r\n",
    )?;
    let mut decoded = 0u64;
    for frame in frames {
        if matches!(frame, Frame::Token { .. }) {
            decoded += 1;
        }
        let payload = format!("data: {}\n\n", frame.to_json(decoded));
        write!(stream, "{:x}\r\n", payload.len())?;
        stream.write_all(payload.as_bytes())?;
        stream.write_all(b"\r\n")?;
        stream.flush()?;
    }
    stream.write_all(b"0\r\n\r\n")?;
    stream.flush()
}
