//! The daemon's model call for the smart `!`: the local endpoint, bounded.
//!
//! The prompt and the defensive parse live in `letibot_sessionlog::suggest`, where
//! they are testable without standing up inference. This is the half that talks to a
//! model, and it is the daemon's because the daemon is the one with an endpoint and
//! an HTTP client. The operator's ask, in their words: *"i want smart ! when a model
//! suggest completions."*
//!
//! # Local only, and bounded
//!
//! The endpoint is the one the daemon already resolved into `cfg.oracle` — the
//! `[gatekeeper]` endpoint in the operator's providers.toml, which on this box is the
//! LOCAL model. It is never a metered provider, because a suggestion must not cost
//! money per keystroke: a daemon with no local endpoint installs no suggester, and a
//! `SuggestShell` then answers with an empty list, which the head reads as *no
//! suggestion*.
//!
//! The call is bounded two ways. The output is capped small — the answer is at most
//! five short lines, and a cap that could hold a paragraph would let a chatty model
//! spend the operator's time on prose. And the read is cut off at a timeout, because
//! **a suggestion that does not arrive is nothing**: it is never a reason to wait, and
//! a model that overruns the deadline is abandoned and the answer is the empty list.

use std::time::Duration;

use letibot_sessionlog::ShellSuggester;
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::suggest::{self, RECENT_ROWS};
use letibot_transcript::TranscriptItem;
use letibot_turn::http::{self, Endpoint};

/// **How long a suggestion may take the local model.**
///
/// The gatekeeper's own budget is for a verdict that fails closed on a timeout, and it
/// is tight (400 ms by default) because it is on the tool path. A suggestion is off the
/// tool path — it fills a composer, it runs nothing — so it may take a little longer,
/// but it is still a completion and not a turn: three seconds is the patience of a
/// person waiting for a word to appear, and past it the answer is *nothing* rather than
/// a stall.
pub const SUGGEST_TIMEOUT: Duration = Duration::from_secs(3);

/// **How many output tokens a suggestion may spend.**
///
/// Five shell lines, one per line, is a handful of tokens. The cap is generous enough
/// for five real commands and small enough that a model that starts explaining itself
/// is cut off before the explanation costs the operator anything.
pub const SUGGEST_MAX_TOKENS: usize = 128;

/// The local model, asked for `!` completions.
pub struct LocalSuggester {
    endpoint: Endpoint,
    model: String,
}

impl LocalSuggester {
    /// `endpoint` is the local model (the `[gatekeeper]` endpoint), and `model` is the
    /// name that server serves. Both come from the daemon's own config, so a suggester
    /// and the guard never disagree about which model is local.
    pub fn new(endpoint: Endpoint, model: String) -> Self {
        LocalSuggester { endpoint, model }
    }

    /// One round trip to the local model, or `None` when it did not answer in time or
    /// at all. `None` is the whole of the failure mode: a suggestion that does not
    /// arrive is nothing, and the caller answers with an empty list.
    fn ask(&self, prompt: &str) -> Option<String> {
        let body = serde_json::json!({
            "model": self.model,
            "messages": [{ "role": "user", "content": prompt }],
            "max_tokens": SUGGEST_MAX_TOKENS,
            "temperature": 0.0,
            // Both spellings, for the reason the oracle's own call sends both: which
            // one a build honours depends on its template, and a suggestion that spends
            // its budget in a thinking block is a suggestion that never arrives.
            "reasoning_effort": "none",
            "chat_template_kwargs": { "enable_thinking": false },
        })
        .to_string();

        // The timeout, made real: the read timeout is the bound that exists on this
        // transport, and a deadline the caller merely promises is not a deadline.
        let mut endpoint = self.endpoint.clone();
        endpoint.read_timeout = SUGGEST_TIMEOUT;
        let res = http::post_json(&endpoint, "/v1/chat/completions", &body).ok()?;
        let text = res.read_to_string().ok()?;
        let v: serde_json::Value = serde_json::from_str(&text).ok()?;
        // The chat shape nests it. `?` on each step: a reply we cannot read becomes
        // `None` -> an empty list, never an empty string the parser would then have to
        // guess at.
        v.get("choices")?
            .get(0)?
            .get("message")?
            .get("content")?
            .as_str()?
            .to_string()
            .into()
    }
}

impl ShellSuggester for LocalSuggester {
    fn suggest(&self, hub: &Hub, workspace: &str, prefix: &str) -> Vec<String> {
        // The conversation, from the session's own log: the last handful of rows
        // condensed and the commands already run, the two context halves the prompt
        // carries. A row whose body has not landed is `None` and is skipped — a
        // candidate built on a body nobody has is a candidate built on nothing.
        let snap = hub.snapshot();
        let items: Vec<TranscriptItem> = snap.items.iter().filter_map(|s| s.item.clone()).collect();
        let recent = suggest::recent_rows(&items, RECENT_ROWS);
        let commands = suggest::commands_run(&items);
        let prompt = suggest::suggestion_prompt(&recent, &commands, workspace, prefix);
        // The model's half, and the defensive parse of whatever it said. A model that
        // did not answer, or answered with prose, is an empty list — the head reads
        // that as *no suggestion* and the operator's Tab does what it did before.
        match self.ask(&prompt) {
            Some(reply) => suggest::parse_suggestions(&reply),
            None => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc::{Receiver, channel};
    use std::time::Instant;

    use letibot_sessionlog::event::SessionEvent;
    use letibot_transcript::{Speaker, UserPart};

    use super::*;

    /// A one-shot local model: it records the request body and answers `reply` as a
    /// chat completion. The endpoint comes back as an `Endpoint`, not a URL, because
    /// `Endpoint::parse` takes `HOST:PORT` and a `http://…` string is a hostname it
    /// would try to resolve.
    fn canned_model(reply: &str) -> (Endpoint, Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
        let addr = listener.local_addr().expect("the bound address");
        let (tx, rx) = channel();
        let reply = reply.to_string();
        std::thread::spawn(move || {
            let Ok((mut conn, _)) = listener.accept() else {
                return;
            };
            let body = read_request(&mut conn);
            let _ = tx.send(body);
            let payload = serde_json::json!({
                "choices": [{ "message": { "role": "assistant", "content": reply } }]
            })
            .to_string();
            let _ = conn.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    payload.len(),
                    payload
                )
                .as_bytes(),
            );
            let _ = conn.flush();
        });
        (Endpoint::new("127.0.0.1", addr.port()), rx)
    }

    /// A local model that accepts the connection and then says nothing at all — the
    /// one shape that tests the deadline, because there is no byte for a read to
    /// return and no close for it to notice.
    fn silent_model() -> Endpoint {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
        let addr = listener.local_addr().expect("the bound address");
        std::thread::spawn(move || {
            let Ok((conn, _)) = listener.accept() else {
                return;
            };
            // Held open, unread and unanswered, for longer than the suggester's own
            // deadline — the thread is detached and the test is done with it by then.
            std::thread::sleep(Duration::from_secs(30));
            drop(conn);
        });
        Endpoint::new("127.0.0.1", addr.port())
    }

    /// The request head, then exactly `Content-Length` bytes of body.
    fn read_request(conn: &mut TcpStream) -> String {
        let mut reader = BufReader::new(conn.try_clone().expect("a clone"));
        let mut len = 0usize;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                return String::new();
            }
            if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                len = v.trim().parse().unwrap_or(0);
            }
            if line.trim_end().is_empty() {
                break;
            }
        }
        let mut buf = vec![0u8; len];
        let _ = reader.read_exact(&mut buf);
        String::from_utf8_lossy(&buf).to_string()
    }

    /// A session that has run one `!` line and been told one thing, which is the
    /// conversation the prompt is built from.
    fn hub_with_a_conversation() -> std::sync::Arc<Hub> {
        let hub = Hub::new("s");
        let row = |id: &str, kind: &str, item: TranscriptItem| {
            hub.publish(SessionEvent::TranscriptAppended {
                item_id: id.into(),
                kind: kind.into(),
                ledger_head: "0000".into(),
            });
            hub.record_item(id, item);
        };
        row(
            "t.0",
            "user",
            TranscriptItem::User {
                speaker: Speaker::Operator,
                parts: vec![UserPart::Text {
                    text: "! cargo test".into(),
                }],
            },
        );
        row(
            "t.1",
            "assistant",
            TranscriptItem::Assistant {
                text: "the tests are red".into(),
                tool_calls: Vec::new(),
                truncated: false,
            },
        );
        hub
    }

    /// **The prompt that goes on the wire is the conversation, and the answer is the
    /// parsed reply.**
    ///
    /// This is the seam nothing else covers: `sessionlog::suggest` tests the prompt
    /// and the parse as pure functions, and this is what proves the daemon half hands
    /// them the session's own rows, names the local model, caps the output — and turns
    /// a decorated reply into candidates rather than prose.
    #[test]
    fn the_local_model_is_asked_about_this_session_and_its_reply_is_parsed() {
        let (endpoint, asked) = canned_model(
            "```bash\n! git status\n! git log\nHere are some commands:\n! git status\n```",
        );
        let s = LocalSuggester::new(endpoint, "local".into());
        let hub = hub_with_a_conversation();

        let lines = s.suggest(&hub, "/tmp/ws", "! git");

        assert_eq!(
            lines,
            vec!["! git status".to_string(), "! git log".to_string()],
            "the fences, the prose and the duplicate are gone: {lines:?}"
        );
        let body = asked
            .recv_timeout(Duration::from_secs(5))
            .expect("the request reached the model");
        // The context halves: the workspace, the rows, the commands already run.
        assert!(body.contains("/tmp/ws"), "the workspace: {body}");
        assert!(body.contains("the tests are red"), "a recent row: {body}");
        assert!(
            body.contains("Commands already run in this session"),
            "the commands already run: {body}"
        );
        assert!(body.contains("! cargo test"), "a command run: {body}");
        // The prefix, and the rule that makes an empty answer acceptable.
        assert!(body.contains("! git"), "the prefix: {body}");
        assert!(
            body.contains("a wrong suggestion is worse than none"),
            "the rule: {body}"
        );
        // The two bounds: the local model's name, and a small output cap.
        assert!(
            body.contains(r#""model":"local""#),
            "the local model: {body}"
        );
        assert!(
            body.contains(&format!(r#""max_tokens":{SUGGEST_MAX_TOKENS}"#)),
            "the output cap: {body}"
        );
    }

    /// **A model that never answers is nothing, and it is nothing in three seconds.**
    ///
    /// The endpoint's own default read timeout is 180 s, which is right for a turn and
    /// absurd for a completion: the operator would sit with a composer that says
    /// *asking the model* for three minutes. So the suggester's own deadline is the
    /// bound, and this is the test that would catch it being dropped — the call would
    /// still answer `None`, three minutes later.
    #[test]
    fn a_model_that_never_answers_is_nothing_within_the_deadline() {
        let s = LocalSuggester::new(silent_model(), "local".into());
        let hub = hub_with_a_conversation();

        let began = Instant::now();
        let lines = s.suggest(&hub, "/tmp/ws", "! git");
        let took = began.elapsed();

        assert!(lines.is_empty(), "silence is not a suggestion: {lines:?}");
        assert!(
            took < SUGGEST_TIMEOUT + Duration::from_secs(5),
            "bounded by the suggester's deadline and not the endpoint's 180 s: {took:?}"
        );
    }

    /// **No endpoint at all is nothing**, which is what a daemon with no local model
    /// answers: the head reads it as *no suggestion* and the operator's Tab does what
    /// it did before the feature.
    #[test]
    fn a_dead_endpoint_suggests_nothing() {
        let s = LocalSuggester::new(
            Endpoint::parse("127.0.0.1:1").expect("a port nothing listens on"),
            "local".into(),
        );
        let hub = hub_with_a_conversation();
        assert!(s.suggest(&hub, "/tmp/ws", "! git").is_empty());
    }
}
