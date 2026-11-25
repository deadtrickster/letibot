//! What a provider refused, kept verbatim: the status, the response's headers and every byte of its
//! body. The message a refusal is projected to (`BackendError::Refused.body`) is the part of the body
//! a person reads, and it is exactly what loses the diagnosis: GLM's `1210` names no field, so the
//! answer to "which parameter" is only in the bytes it came with.

use std::time::{SystemTime, UNIX_EPOCH};

/// The most of a refusal's body one record keeps. A provider that answers a 5 MB conversation with a
/// page of prose is far under this; one that echoes the request back is cut, and the record says how
/// much it cut, so a truncated copy is never read as the whole answer.
pub const MAX_BODY: usize = 1024 * 1024;

/// One refusal as the wire gave it. `headers` are the RESPONSE's and never the request's: this record
/// lands in a log, and the request's `Authorization` is the one header that must not go with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub provider: String,
    pub model: String,
    pub status: u16,
    /// Every header the answer carried, in the order the client read them.
    pub headers: Vec<(String, String)>,
    /// The body verbatim, cut at [`MAX_BODY`].
    pub body: String,
    /// How many bytes the body was before the cut, so `body.len() < body_bytes` is a silent absence
    /// no longer: the record says how much of the answer is here.
    pub body_bytes: usize,
    /// The refused request's length. A limit on size is read against what was actually sent.
    pub request_bytes: usize,
    /// What was sent as its SHAPE - roles in order, which fields each message carries, and how long
    /// they are - where the client composes one. Never the content: this is not a second transcript.
    pub shape: Option<String>,
}

impl Refusal {
    /// The record as one line of JSON, which is how the session's log keeps it.
    pub fn to_json(&self) -> serde_json::Value {
        let headers: serde_json::Map<String, serde_json::Value> = self
            .headers
            .iter()
            .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
            .collect();
        serde_json::json!({
            "at_ms": now_ms(),
            "provider": self.provider,
            "model": self.model,
            "status": self.status,
            "headers": headers,
            "body": self.body,
            "body_bytes": self.body_bytes,
            "body_truncated": self.body_bytes > self.body.len(),
            "request_bytes": self.request_bytes,
            "request_shape": self.shape,
        })
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Where a refusal is recorded: the daemon appends it to the session's own log. A closure for the
/// reason [`Notes`](crate::openai::Notes) is one - this crate stays below the session log, and who
/// keeps the record is the caller's business.
pub type RefusalLog = std::sync::Arc<dyn Fn(&Refusal) + Send + Sync>;

/// The most of a body the daemon's own log (its stderr) is given: a session with no sink wired still
/// says what a provider refused, and a megabyte of it on a terminal is a log nobody reads.
pub const SAID_MAX: usize = 2000;

/// Record one refusal: the sink gets it whole, and the daemon's stderr gets the line a person reads
/// plus as much of the body as one screen holds.
pub fn record(sink: Option<&RefusalLog>, r: &Refusal) {
    eprintln!(
        "  refused by {} ({} {}), {} header(s), {} byte(s) of request, {} byte(s) of answer:\n  {}",
        r.provider,
        r.model,
        r.status,
        r.headers.len(),
        r.request_bytes,
        r.body_bytes,
        cut(&r.body, SAID_MAX)
    );
    // What was sent, when what was sent was refused: the shape of it and never the content, so the
    // next unexplained refusal is read off this record rather than guessed at from documentation.
    if let Some(shape) = &r.shape {
        eprintln!("  refused request shape:\n{shape}");
    }
    if let Some(sink) = sink {
        sink(r);
    }
}

/// Every header an answer carried, in the order the client read them. One door for the four
/// clients, because a record that kept a different set per protocol is four records.
pub fn headers_of(map: &ureq::http::HeaderMap) -> Vec<(String, String)> {
    map.iter()
        .filter_map(|(k, v)| {
            v.to_str()
                .ok()
                .map(|v| (k.as_str().to_string(), v.to_string()))
        })
        .collect()
}

/// `s` cut at `max` bytes on a character boundary: a body is UTF-8 and a log line that is not is a
/// log line nothing can parse.
pub fn cut(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Every byte of a body, cut at [`MAX_BODY`], and how long it was.
pub fn body_of(text: String) -> (String, usize) {
    let bytes = text.len();
    (cut(&text, MAX_BODY).to_string(), bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(body: &str) -> Refusal {
        Refusal {
            provider: "glm-coding".into(),
            model: "glm-5.4".into(),
            status: 400,
            headers: vec![
                ("content-type".into(), "application/json".into()),
                ("x-request-id".into(), "req_9".into()),
            ],
            body: body.into(),
            body_bytes: body.len(),
            request_bytes: 5_900_000,
            shape: Some("508 message(s)".into()),
        }
    }

    /// The record is the body and the answer's own headers, and the request's key is nowhere in it.
    #[test]
    fn a_record_is_the_body_and_the_answers_headers_verbatim() {
        let said = "{\"error\":{\"code\":\"1210\",\"message\":\"Invalid API parameter\"}}";
        let json = sample(said).to_json();
        assert_eq!(json["status"], 400);
        assert_eq!(json["body"], said, "the body is kept byte for byte");
        assert_eq!(json["body_truncated"], false);
        assert_eq!(json["headers"]["x-request-id"], "req_9");
        assert_eq!(json["request_shape"], "508 message(s)");
        assert!(json.get("authorization").is_none());
    }

    /// A body over the cap is cut on a character boundary and the record says it was.
    #[test]
    fn a_body_over_the_cap_is_cut_and_says_how_long_it_was() {
        let long = "é".repeat(MAX_BODY);
        let (body, bytes) = body_of(long.clone());
        assert_eq!(bytes, long.len());
        assert!(body.len() <= MAX_BODY, "{} bytes", body.len());
        assert!(body.is_char_boundary(body.len()));
        let mut r = sample("");
        (r.body, r.body_bytes) = (body, bytes);
        let json = r.to_json();
        assert_eq!(json["body_truncated"], true);
        assert_eq!(json["body_bytes"], long.len());
    }

    /// The stderr copy is one screen, and cutting never splits a character.
    #[test]
    fn the_daemons_copy_of_a_body_is_bounded_and_valid_utf8() {
        let long = "é".repeat(SAID_MAX);
        let said = cut(&long, SAID_MAX);
        assert!(said.len() < long.len());
        assert!(said.is_char_boundary(said.len()));
        assert_eq!(cut("short", SAID_MAX), "short");
    }
}
